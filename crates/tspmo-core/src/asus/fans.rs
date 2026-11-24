//! Fan curves through the `asus_custom_fan_curve` hwmon device and fan speeds through `asus`.
//!
//! How the kernel (`asus-wmi`) behaves, which shapes everything here:
//! * `pwmN_autoK_{temp,pwm}` only edit the driver's copy and mark the fan "not enabled".
//! * `pwmN_enable = 1` writes the whole curve to the EC; `2` returns the fan to the firmware
//!   curve; `3` reloads the *factory* curve of the current thermal mode from the EC (and
//!   returns to the firmware curve).
//! * The EC drops custom curves whenever the thermal mode changes, so they must be written
//!   again after every mode change and after resume.
//!
//! Curves are in °C and **percent** (the EC works in whole percent; the kernel's 0..255 PWM is
//! only a scaling of it).

use std::io;
use tspmo_sys::{SysRoot, hwmon};

pub const CURVE_HWMON: &str = "asus_custom_fan_curve";
pub const SENSOR_HWMON: &str = "asus";
pub const POINTS: usize = 8;

/// Valid curve temperatures, °C.
pub const TEMP_MIN: u8 = 20;
pub const TEMP_MAX: u8 = 100;
/// Above this temperature a curve may not run the fan slower than the firmware does.
pub const HOT: u8 = 75;

/// Kernel PWM (0..=255) to whole percent.
pub fn pct_from_pwm(pwm: u8) -> u8 {
    ((u16::from(pwm) * 100 + 127) / 255) as u8
}

/// Percent to the smallest PWM the kernel converts back to exactly that percent.
pub fn pwm_from_pct(pct: u8) -> u8 {
    (u16::from(pct.min(100)) * 255).div_ceil(100) as u8
}

/// Eight `(°C, percent)` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Curve(pub [(u8, u8); POINTS]);

impl Curve {
    pub fn from_points(p: &[(u8, u8)]) -> Option<Self> {
        Some(Self(p.try_into().ok()?))
    }

    /// Fan speed (percent, fractional) at `temp`: linear between points, flat outside them.
    pub fn at(&self, temp: u8) -> f32 {
        let p = &self.0;
        if temp <= p[0].0 {
            return f32::from(p[0].1);
        }
        for w in p.windows(2) {
            let ((t0, s0), (t1, s1)) = (w[0], w[1]);
            if temp <= t1 {
                if t1 == t0 {
                    return f32::from(s1);
                }
                let f = f32::from(temp - t0) / f32::from(t1 - t0);
                return f32::from(s0) + f * (f32::from(s1) - f32::from(s0));
            }
        }
        f32::from(p[POINTS - 1].1)
    }

    /// Every fan at 100 % from 30 °C up.
    pub fn full() -> Self {
        Self([30, 40, 50, 60, 70, 80, 90, 100].map(|t| (t, 100)))
    }
}

/// What a fan does in one thermal mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    /// The firmware curve.
    Auto,
    Custom(Curve),
    /// 100 %.
    Full,
}

impl Setting {
    /// The curve to write, if the setting needs one.
    pub fn curve(&self) -> Option<Curve> {
        match self {
            Self::Auto => None,
            Self::Custom(c) => Some(c.clone()),
            Self::Full => Some(Curve::full()),
        }
    }
}

/// Why a curve is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurveError {
    Temp,
    Rising,
    Speed,
    TooSlow { temp: u8, min: u8 },
}

impl std::fmt::Display for CurveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Temp => write!(f, "temperature must be {TEMP_MIN}-{TEMP_MAX} °C"),
            Self::Rising => f.write_str("temperature must not fall along the curve"),
            Self::Speed => f.write_str("speed must be 0-100% and must not fall along the curve"),
            Self::TooSlow { temp, min } => write!(f, "at {temp} °C the fan must run at least {min}%"),
        }
    }
}

impl std::error::Error for CurveError {}

/// The slowest speed allowed at `temp`: the firmware's own at high temperatures, or a
/// conservative default when its curve isn't known yet.
pub fn min_speed(factory: Option<&Curve>, temp: u8) -> u8 {
    if temp < HOT {
        return 0;
    }
    match factory {
        Some(f) => f.at(temp).floor() as u8,
        None if temp >= 85 => 50,
        None => 30,
    }
}

/// The CPU's throttle temperature, °C (coretemp's critical limit; 100 when unknown).
pub fn tjmax(root: &SysRoot) -> u8 {
    hwmon::find(root, "coretemp")
        .and_then(|h| root.read_parse::<u32>(format!("{}/temp1_crit", h.dir)))
        .map_or(100, |m| (m / 1000).clamp(80, 110) as u8)
}

/// The Eco+ curve for one fan: stopped while it is safe to be, then a steep ramp.
///
/// * Off up to `off` °C: four degrees past where the firmware's quiet curve starts its first
///   step (`factory`), kept within 58-66 °C. At the 5 W Eco+ draws, an idle laptop settles
///   around 57 °C with the fans stopped.
/// * Above it the fan runs at once (the EC's lowest non-zero speed is already about 1800 rpm)
///   and climbs faster than the firmware: 100 % from `tjmax - 20` °C, and never below the
///   firmware's own speed from 75 °C ([`validate`] enforces that).
pub fn eco_curve(factory: Option<&Curve>, tjmax: u8) -> Curve {
    let off = factory.map_or(62, |f| f.0[0].0 + 4).clamp(58, 66);
    let top = tjmax.saturating_sub(20).clamp(75, 95);
    let mut p = [
        (TEMP_MIN, 0),
        (off, 0),
        (off + 3, 12),
        (off + 7, 28),
        (72.max(off + 8), 40),
        (76.max(off + 12), 55),
        (82.max(off + 16), 75),
        (top.max(off + 24).min(TEMP_MAX), 100),
    ];
    // Never falling along the curve, and never slower than the firmware from `HOT` up.
    if let Some(f) = factory {
        for (t, s) in p.iter_mut().filter(|(t, _)| *t >= HOT) {
            *s = (*s).max(f.at(*t).ceil() as u8);
        }
    }
    for i in 1..POINTS {
        p[i].0 = p[i].0.max(p[i - 1].0);
        p[i].1 = p[i].1.max(p[i - 1].1);
    }
    // Straight pieces between points can still dip under a kinked firmware curve: lift the
    // next point until none does.
    for _ in 0..POINTS {
        let c = Curve(p);
        let Some(t) = (HOT..=TEMP_MAX).find(|t| c.at(*t).round() < f32::from(min_speed(factory, *t))) else {
            break;
        };
        let Some(i) = p.iter().position(|(pt, _)| *pt >= t) else {
            break;
        };
        p[i].1 = p[i].1.max(min_speed(factory, t)).max(min_speed(factory, p[i].0));
        for j in i + 1..POINTS {
            p[j].1 = p[j].1.max(p[j - 1].1);
        }
    }
    Curve(p)
}

/// Check a curve: temperatures within range and non-decreasing, speeds within 0-100 and
/// non-decreasing, and never slower than the firmware at high temperature.
pub fn validate(curve: &Curve, factory: Option<&Curve>) -> Result<(), CurveError> {
    let p = &curve.0;
    if p.iter().any(|(t, _)| !(TEMP_MIN..=TEMP_MAX).contains(t)) {
        return Err(CurveError::Temp);
    }
    if p.windows(2).any(|w| w[1].0 < w[0].0) {
        return Err(CurveError::Rising);
    }
    if p.iter().any(|(_, s)| *s > 100) || p.windows(2).any(|w| w[1].1 < w[0].1) {
        return Err(CurveError::Speed);
    }
    for t in HOT..=TEMP_MAX {
        let min = min_speed(factory, t);
        if curve.at(t).round() < f32::from(min) {
            return Err(CurveError::TooSlow { temp: t, min });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fan {
    /// `N` of `pwmN_*` / `fanN_*`.
    pub index: u32,
    /// Short name: `CPU`, `GPU`, `Mid`.
    pub label: String,
}

fn pretty(label: &str) -> String {
    match label.trim_end_matches("_fan") {
        "cpu" => "CPU".into(),
        "gpu" => "GPU".into(),
        "mid" => "Mid".into(),
        other => {
            let mut c = other.chars();
            c.next()
                .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fans {
    dir: String,
    sensors: Option<String>,
    pub fans: Vec<Fan>,
}

/// The fans that have a custom-curve interface, or `None` if the machine has none.
pub fn discover(root: &SysRoot) -> Option<Fans> {
    let curve = hwmon::find(root, CURVE_HWMON)?;
    let sensors = hwmon::find(root, SENSOR_HWMON).map(|h| h.dir);
    let fans: Vec<Fan> = curve
        .indices(root, "pwm", "enable")
        .into_iter()
        .filter(|n| root.exists(format!("{}/pwm{n}_auto_point1_pwm", curve.dir)))
        .map(|index| {
            let label = sensors
                .as_ref()
                .and_then(|s| root.read_opt(format!("{s}/fan{index}_label")))
                .map_or_else(|| format!("Fan {index}"), |l| pretty(&l));
            Fan { index, label }
        })
        .collect();
    (!fans.is_empty()).then_some(Fans {
        dir: curve.dir,
        sensors,
        fans,
    })
}

impl Fans {
    pub fn has(&self, index: u32) -> bool {
        self.fans.iter().any(|f| f.index == index)
    }

    /// Live speed in RPM.
    pub fn rpm(&self, root: &SysRoot, index: u32) -> Option<u32> {
        root.read_parse(format!("{}/fan{index}_input", self.sensors.as_ref()?))
    }

    /// The curve the driver currently holds (the EC's, until something edits it).
    pub fn read(&self, root: &SysRoot, index: u32) -> Option<Curve> {
        let mut pts = [(0u8, 0u8); POINTS];
        for (k, p) in pts.iter_mut().enumerate() {
            let n = k + 1;
            p.0 = root.read_parse(format!("{}/pwm{index}_auto_point{n}_temp", self.dir))?;
            p.1 = pct_from_pwm(root.read_parse(format!("{}/pwm{index}_auto_point{n}_pwm", self.dir))?);
        }
        Some(Curve(pts))
    }

    /// Whether the custom curve is the one in force (`pwmN_enable` = 1).
    pub fn custom_active(&self, root: &SysRoot, index: u32) -> bool {
        root.read_parse::<u8>(format!("{}/pwm{index}_enable", self.dir)) == Some(1)
    }

    fn enable(&self, root: &SysRoot, index: u32, value: u8) -> io::Result<()> {
        root.write(format!("{}/pwm{index}_enable", self.dir), &value.to_string())
    }

    /// Put `curve` in force. Skips the EC write when it already is, unless `force` (after
    /// resume the EC may have dropped a curve the driver still believes is active).
    pub fn write(&self, root: &SysRoot, index: u32, curve: &Curve, force: bool) -> io::Result<()> {
        if !force && self.custom_active(root, index) && self.read(root, index).as_ref() == Some(curve) {
            return Ok(());
        }
        for (k, (temp, pct)) in curve.0.iter().enumerate() {
            let n = k + 1;
            root.write(
                format!("{}/pwm{index}_auto_point{n}_temp", self.dir),
                &temp.to_string(),
            )?;
            root.write(
                format!("{}/pwm{index}_auto_point{n}_pwm", self.dir),
                &pwm_from_pct(*pct).to_string(),
            )?;
        }
        self.enable(root, index, 1)
    }

    /// Back to the firmware curve. Only touches the EC if a custom curve is in force: each
    /// such write makes the EC notify the NVIDIA driver.
    pub fn auto(&self, root: &SysRoot, index: u32) -> io::Result<()> {
        if self.custom_active(root, index) {
            self.enable(root, index, 2)?;
        }
        Ok(())
    }

    /// Make the EC reload the current thermal mode, which drops custom curves and, on
    /// firmware that ties them to it, custom power limits. Writes `pwm1_enable = 2`: the
    /// driver answers with a thermal-policy write.
    pub fn reset_mode(&self, root: &SysRoot) -> io::Result<()> {
        match self.fans.first() {
            Some(f) => self.enable(root, f.index, 2),
            None => Ok(()),
        }
    }

    /// Reload the factory curve of the current thermal mode and return it.
    pub fn factory(&self, root: &SysRoot, index: u32) -> io::Result<Curve> {
        self.enable(root, index, 3)?;
        self.read(root, index)
            .ok_or_else(|| io::Error::other("curve unreadable"))
    }

    /// Whether any fan runs a custom curve.
    pub fn any_custom(&self, root: &SysRoot) -> bool {
        self.fans.iter().any(|f| self.custom_active(root, f.index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asus::testfix::Fix;

    fn factory() -> Curve {
        Curve([
            (62, 7),
            (67, 12),
            (72, 16),
            (74, 28),
            (76, 32),
            (80, 51),
            (80, 51),
            (80, 51),
        ])
    }

    #[test]
    fn percent_round_trips() {
        for pct in 0..=100u8 {
            assert_eq!(pct_from_pwm(pwm_from_pct(pct)), pct, "{pct}");
            // What the kernel sends the EC: 100 * pwm / 255 (integer).
            assert_eq!(100 * u16::from(pwm_from_pct(pct)) / 255, u16::from(pct), "{pct}");
        }
        assert_eq!(pct_from_pwm(130), 51);
        assert_eq!(pwm_from_pct(100), 255);
    }

    #[test]
    fn interpolation() {
        let c = factory();
        assert_eq!(c.at(10), 7.0);
        assert_eq!(c.at(62), 7.0);
        assert_eq!(c.at(100), 51.0);
        assert!((c.at(64) - 9.0).abs() < 0.01);
        assert_eq!(c.at(80), 51.0);
    }

    #[test]
    fn validation() {
        let f = factory();
        assert_eq!(validate(&f, Some(&f)), Ok(()));
        assert_eq!(validate(&Curve::full(), Some(&f)), Ok(()));

        let mut c = factory();
        c.0[3].0 = 60; // falls after 72
        assert_eq!(validate(&c, Some(&f)), Err(CurveError::Rising));

        let mut c = factory();
        c.0[4].1 = 5; // speed falls
        assert_eq!(validate(&c, Some(&f)), Err(CurveError::Speed));

        let mut c = factory();
        c.0[0].0 = 10;
        assert_eq!(validate(&c, Some(&f)), Err(CurveError::Temp));
        let mut c = factory();
        c.0[7].0 = 101;
        assert_eq!(validate(&c, Some(&f)), Err(CurveError::Temp));

        let mut c = factory();
        c.0[7].1 = 101;
        assert_eq!(validate(&c, Some(&f)), Err(CurveError::Speed));
    }

    #[test]
    fn eco_curve_is_off_while_cool_and_never_below_the_firmware_when_hot() {
        let quiet_gpu = Curve([
            (54, 1),
            (58, 5),
            (62, 5),
            (65, 10),
            (68, 12),
            (71, 19),
            (74, 32),
            (74, 32),
        ]);
        let quiet_mid = Curve([
            (62, 1),
            (67, 8),
            (71, 19),
            (76, 19),
            (78, 34),
            (80, 49),
            (82, 56),
            (82, 56),
        ]);
        for f in [Some(&quiet_gpu), Some(&quiet_mid), Some(&factory()), None] {
            let c = eco_curve(f, 110);
            assert_eq!(validate(&c, f), Ok(()), "{c:?} against {f:?}");
            assert_eq!(c.at(20), 0.0);
            assert_eq!(c.at(55), 0.0, "{f:?}");
            assert!(c.at(90) >= 100.0 - f32::EPSILON, "{c:?}");
            assert!(c.at(70) > 0.0);
        }
        // The off range follows the firmware's first step, within bounds.
        assert_eq!(eco_curve(Some(&quiet_gpu), 110).0[1].0, 58);
        assert_eq!(eco_curve(Some(&quiet_mid), 110).0[1].0, 66);
        // A lower Tjmax brings 100 % in sooner.
        assert!(eco_curve(None, 95).at(80) >= eco_curve(None, 110).at(80));
    }

    #[test]
    fn hot_floor() {
        let f = factory();
        // Silent until 90 °C, then jumps: slower than the firmware at 80 °C.
        let c = Curve([
            (30, 0),
            (40, 0),
            (50, 0),
            (60, 0),
            (70, 0),
            (80, 0),
            (90, 0),
            (100, 100),
        ]);
        assert!(matches!(validate(&c, Some(&f)), Err(CurveError::TooSlow { .. })));
        // Same, without a known factory curve: the conservative default applies.
        assert!(matches!(validate(&c, None), Err(CurveError::TooSlow { .. })));
        // Quiet below the hot threshold is fine.
        let c = Curve([
            (30, 0),
            (40, 0),
            (50, 0),
            (60, 10),
            (70, 20),
            (75, 30),
            (80, 51),
            (90, 70),
        ]);
        assert_eq!(validate(&c, Some(&f)), Ok(()));
        assert_eq!(min_speed(Some(&f), 60), 0);
        assert_eq!(min_speed(None, 90), 50);
    }

    #[test]
    fn discovers_fixture_fans() {
        let fx = Fix::new("fans-discover");
        let root = fx.root();
        let fans = discover(&root).unwrap();
        let names: Vec<_> = fans.fans.iter().map(|f| (f.index, f.label.as_str())).collect();
        assert_eq!(names, vec![(1, "CPU"), (2, "GPU"), (3, "Mid")]);
        assert!(fans.rpm(&root, 1).is_some());
        let c = fans.read(&root, 1).unwrap();
        assert!(c.0.windows(2).all(|w| w[1].0 >= w[0].0));
    }

    #[test]
    fn no_fans_no_interface() {
        let fx = Fix::new("fans-none");
        std::fs::remove_dir_all(fx.root().path("/sys/class/hwmon/hwmon8")).unwrap();
        assert!(discover(&fx.root()).is_none());
    }

    #[test]
    fn write_and_read_back() {
        let fx = Fix::new("fans-write");
        let root = fx.root();
        let fans = discover(&root).unwrap();
        fans.write(&root, 2, &Curve::full(), false).unwrap();
        let d = "/sys/class/hwmon/hwmon8";
        assert_eq!(fx.get(&format!("{d}/pwm2_enable")), "1");
        assert_eq!(fx.get(&format!("{d}/pwm2_auto_point1_pwm")), "255");
        assert_eq!(fx.get(&format!("{d}/pwm2_auto_point8_temp")), "100");
        assert_eq!(fans.read(&root, 2), Some(Curve::full()));
        assert!(fans.custom_active(&root, 2));
        assert!(fans.any_custom(&root));
        // Other fans untouched.
        assert!(!fans.custom_active(&root, 1));
        fans.auto(&root, 2).unwrap();
        assert_eq!(fx.get(&format!("{d}/pwm2_enable")), "2");
        // Auto on an already-automatic fan writes nothing.
        fx.set(&format!("{d}/pwm3_enable"), "2");
        fans.auto(&root, 3).unwrap();
        assert_eq!(fx.get(&format!("{d}/pwm3_enable")), "2");
    }

    #[test]
    fn labels() {
        assert_eq!(pretty("cpu_fan"), "CPU");
        assert_eq!(pretty("mid_fan"), "Mid");
        assert_eq!(pretty("water_fan"), "Water");
    }
}
