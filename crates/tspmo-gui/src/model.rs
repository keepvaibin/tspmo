//! What the daemon says, as the UI needs it. Pure functions over [`Store`]; no Slint, no D-Bus.

use crate::bindings;
use crate::value::V;
use std::collections::HashMap;
use zbus::zvariant::Value;

/// Thermal modes (Eco+ is a power-saver variant for fans and power limits).
pub const THERMAL: [&str; 3] = ["power-saver", "balanced", "performance"];
pub const SOURCES: [&str; 2] = ["ac", "battery"];

#[derive(Default)]
pub struct Store {
    pub p: HashMap<String, HashMap<String, V>>,
    /// `"Display1.SetFlickerFree"` → input signatures.
    pub methods: HashMap<String, Vec<String>>,
    pub gpu_users: Vec<(u32, String, String)>,
}

impl Store {
    pub fn has(&self, iface: &str) -> bool {
        self.p.contains_key(iface)
    }

    pub fn get(&self, iface: &str, prop: &str) -> &V {
        self.p.get(iface).and_then(|m| m.get(prop)).unwrap_or(&V::None)
    }

    pub fn has_prop(&self, iface: &str, prop: &str) -> bool {
        !self.get(iface, prop).is_none()
    }

    pub fn merge(&mut self, iface: &str, props: Vec<(String, V)>) {
        let m = self.p.entry(iface.to_owned()).or_default();
        for (k, v) in props {
            m.insert(k, v);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// sections

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
}

/// Items of Hardware1 shown on the Screen page instead of System.
pub const SCREEN_ITEMS: [&str; 2] = ["panel_overdrive", "mini_led_mode"];

/// Which pages this machine has. Absent hardware hides the page.
pub fn sections(s: &Store) -> Vec<Section> {
    let hw = hw_items(s);
    let mut v = Vec::new();
    let sec = |id, label, icon| Section { id, label, icon };
    let gpu = s.get("Daemon1", "GpuMode").str();
    if !gpu.is_empty() && gpu != "none" && gpu != "unknown" {
        v.push(sec("gpu", "GPU", "gpu"));
    }
    if s.has("Display1") || hw.iter().any(|i| SCREEN_ITEMS.contains(&i.name.as_str())) {
        v.push(sec("screen", "Screen", "screen"));
    }
    if s.has("Battery1") {
        v.push(sec("battery", "Battery", "battery"));
    }
    if s.has("Fans1") {
        v.push(sec("fans", "Fans", "fan"));
    }
    if s.has("Power1") {
        v.push(sec("power", "Power", "power"));
    }
    if s.has("Lighting1") {
        v.push(sec("light", "Light", "light"));
    }
    if s.has("Lid1") && !s.get("Lid1", "Devices").list().is_empty() {
        v.push(sec("lid", "Lid", "lid"));
    }
    if hw.iter().any(|i| !SCREEN_ITEMS.contains(&i.name.as_str()))
        || s.get("Hardware1", "RebootPending").bool()
    {
        v.push(sec("system", "System", "system"));
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Hardware1 items

#[derive(Debug, Clone, PartialEq)]
pub struct HwItem {
    pub name: String,
    pub label: String,
    pub kind: String,
    pub current: String,
    pub values: Vec<String>,
    pub min: i64,
    pub max: i64,
    pub writable: bool,
}

pub fn hw_items(s: &Store) -> Vec<HwItem> {
    s.get("Hardware1", "Items")
        .list()
        .iter()
        .map(|i| HwItem {
            name: i.at(0).str().to_owned(),
            label: i.at(1).str().to_owned(),
            kind: i.at(2).str().to_owned(),
            current: i.at(3).str().to_owned(),
            values: i.at(4).strings(),
            min: i.at(5).int(),
            max: i.at(6).int(),
            writable: i.at(7).bool(),
        })
        .collect()
}

impl HwItem {
    pub fn on(&self) -> bool {
        self.current == "1" || self.current == "on"
    }

    /// The value one step away (`dir` ±1), for choices and numbers. `None` at the ends.
    pub fn step(&self, dir: i64) -> Option<String> {
        match self.kind.as_str() {
            "choice" => {
                let i = self.values.iter().position(|v| *v == self.current)? as i64 + dir;
                let n = self.values.len() as i64;
                (0..n).contains(&i).then(|| self.values[i as usize].clone())
            }
            "number" => {
                let cur: i64 = self.current.parse().ok()?;
                let step = ((self.max - self.min) / 20).max(1);
                let n = (cur + dir * step).clamp(self.min, self.max);
                (n != cur).then(|| n.to_string())
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Fans

pub type Curve = Vec<(u8, u8)>;

pub struct FanSetting {
    pub kind: String,
    pub points: Curve,
}

fn points(v: &V) -> Curve {
    v.list()
        .iter()
        .map(|p| (p.at(0).int() as u8, p.at(1).int() as u8))
        .collect()
}

/// Kind and curve for (mode, fan). `auto` shows the firmware curve when it is known.
pub fn fan_setting(s: &Store, mode: &str, fan: u32) -> FanSetting {
    let found = s
        .get("Fans1", "Settings")
        .list()
        .iter()
        .find(|e| e.at(0).str() == mode && e.at(1).int() == i64::from(fan));
    let (kind, mut pts) = found.map_or((String::from("auto"), Vec::new()), |e| {
        (e.at(2).str().to_owned(), points(e.at(3)))
    });
    if pts.is_empty()
        && let Some(f) = s
            .get("Fans1", "Factory")
            .list()
            .iter()
            .find(|e| e.at(0).str() == mode && e.at(1).int() == i64::from(fan))
    {
        pts = points(f.at(2));
    }
    FanSetting { kind, points: pts }
}

/// A gentle default for editing when nothing is known.
pub fn default_curve() -> Curve {
    [0u8, 10, 25, 40, 55, 70, 85, 100]
        .iter()
        .enumerate()
        .map(|(i, p)| (30 + 10 * i as u8, *p))
        .collect()
}

/// Move point `i`, keeping temperatures and speeds non-decreasing and inside 20-100.
pub fn move_point(pts: &mut [(u8, u8)], i: usize, t: i32, p: i32) {
    if i >= pts.len() {
        return;
    }
    let lo_t = if i == 0 { 20 } else { i32::from(pts[i - 1].0) };
    let hi_t = pts.get(i + 1).map_or(100, |n| i32::from(n.0));
    let lo_p = if i == 0 { 0 } else { i32::from(pts[i - 1].1) };
    let hi_p = pts.get(i + 1).map_or(100, |n| i32::from(n.1));
    pts[i] = (t.clamp(lo_t, hi_t) as u8, p.clamp(lo_p, hi_p) as u8);
}

pub fn rpm_of(s: &Store, fan: u32) -> u32 {
    s.get("Fans1", "Rpm")
        .list()
        .iter()
        .find(|e| e.at(0).int() == i64::from(fan))
        .map_or(0, |e| e.at(1).int() as u32)
}

// ---------------------------------------------------------------------------------------------
// Power limits

#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub name: String,
    pub label: String,
    pub unit: String,
    pub min: i64,
    pub max: i64,
    pub def: i64,
    pub cur: i64,
    pub custom: bool,
}

pub fn limits(s: &Store, mode: &str, source: &str) -> Vec<Limit> {
    let custom = s.get("Power1", "Custom").list();
    s.get("Power1", "Limits")
        .list()
        .iter()
        .map(|l| {
            let name = l.at(0).str();
            let set = custom
                .iter()
                .find(|c| c.at(0).str() == mode && c.at(1).str() == source && c.at(2).str() == name)
                .map(|c| c.at(3).int());
            let (min, max, def) = (l.at(3).int(), l.at(4).int(), l.at(5).int());
            Limit {
                name: name.to_owned(),
                label: l.at(1).str().to_owned(),
                unit: l.at(2).str().to_owned(),
                min,
                max,
                def,
                cur: set.unwrap_or(def).clamp(min, max.max(min)),
                custom: set.is_some(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Battery

pub fn fmt_duration(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let m = (secs + 30) / 60;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

/// Watts, or nothing when unknown or too small to mean anything (0.0 W while "Not charging").
pub fn fmt_watts(w: f64) -> String {
    if w < 0.5 {
        String::new()
    } else {
        format!("{w:.1} W")
    }
}

/// `82%  Charging 41.9 W`. The rate only appears while energy actually flows.
pub fn battery_line(percent: i64, status: &str, watts: f64) -> String {
    if percent < 0 {
        return String::new();
    }
    let rate = if matches!(status, "Charging" | "Discharging") {
        fmt_watts(watts)
    } else {
        String::new()
    };
    [format!("{percent}%"), status.to_owned(), rate]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("  ")
}

/// What the firmware reports as the charger in use.
pub fn charger_label(code: &str) -> &'static str {
    match code {
        "0" => "none",
        "1" => "barrel",
        "2" => "USB-C",
        _ => "",
    }
}

/// Power limits that must keep an order: sustained <= boost <= fast boost.
pub const LIMIT_ORDER: [&str; 3] = ["ppt_pl1_spl", "ppt_pl2_sppt", "ppt_pl3_fppt"];

/// After `name` was set to `value`, the neighbours that would break the order, with their new
/// values (clamped to their own range). Empty when nothing needs to move.
pub fn limit_fixes(ls: &[Limit], name: &str, value: i64) -> Vec<(String, i64)> {
    let Some(at) = LIMIT_ORDER.iter().position(|n| *n == name) else {
        return Vec::new();
    };
    let get = |n: &str| ls.iter().find(|l| l.name == n);
    let mut out: Vec<(String, i64)> = Vec::new();
    let mut prev = value;
    for n in &LIMIT_ORDER[at + 1..] {
        if let Some(l) = get(n) {
            if l.cur < prev {
                let v = prev.clamp(l.min, l.max.max(l.min));
                out.push(((*n).to_owned(), v));
                prev = v;
            } else {
                prev = l.cur;
            }
        }
    }
    let mut next = value;
    for n in LIMIT_ORDER[..at].iter().rev() {
        if let Some(l) = get(n) {
            if l.cur > next {
                let v = next.clamp(l.min, l.max.max(l.min));
                out.push(((*n).to_owned(), v));
                next = v;
            } else {
                next = l.cur;
            }
        }
    }
    out
}

/// Index of the step matching `limit` (the nearest below if the exact one is absent).
pub fn step_index(steps: &[i64], limit: i64) -> usize {
    steps
        .iter()
        .rposition(|s| *s <= limit)
        .unwrap_or(0)
        .min(steps.len().saturating_sub(1))
}

// ---------------------------------------------------------------------------------------------
// Lighting

pub const KB_COLORS: [(u8, u8, u8); 8] = [
    (255, 255, 255),
    (255, 59, 48),
    (255, 149, 0),
    (255, 214, 10),
    (52, 199, 89),
    (10, 132, 255),
    (191, 90, 242),
    (255, 45, 146),
];
pub const KB_SPEEDS: [&str; 3] = ["low", "med", "high"];
pub const KB_IDLE: [i64; 7] = [0, 15, 30, 60, 120, 300, 600];

#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub mode: String,
    pub color: (u8, u8, u8),
    pub color2: (u8, u8, u8),
    pub speed: String,
    pub direction: String,
}

pub fn effect(s: &Store) -> Effect {
    let e = s.get("Lighting1", "Effect");
    let rgb = |v: &V| (v.at(0).int() as u8, v.at(1).int() as u8, v.at(2).int() as u8);
    Effect {
        mode: e.at(0).str().to_owned(),
        color: rgb(e.at(1)),
        color2: rgb(e.at(2)),
        speed: e.at(3).str().to_owned(),
        direction: e.at(4).str().to_owned(),
    }
}

impl Effect {
    /// The D-Bus argument `(s(yyy)(yyy)ss)`.
    pub fn to_value(&self) -> Value<'static> {
        use zbus::zvariant::StructureBuilder;
        let rgb = |c: (u8, u8, u8)| {
            StructureBuilder::new()
                .add_field(c.0)
                .add_field(c.1)
                .add_field(c.2)
                .build()
                .expect("three fields")
        };
        Value::Structure(
            StructureBuilder::new()
                .add_field(self.mode.clone())
                .add_field(rgb(self.color))
                .add_field(rgb(self.color2))
                .add_field(self.speed.clone())
                .add_field(self.direction.clone())
                .build()
                .expect("five fields"),
        )
    }
}

/// The entry after/before `cur` in `list`, wrapping.
pub fn cycle<'a>(list: &'a [String], cur: &str, dir: i64) -> Option<&'a String> {
    if list.is_empty() {
        return None;
    }
    let n = list.len() as i64;
    // Nothing chosen yet: forward starts at the first entry, backward at the last.
    let i = list
        .iter()
        .position(|m| m == cur)
        .map_or(if dir > 0 { -1 } else { 0 }, |i| i as i64);
    list.get((i + dir).rem_euclid(n) as usize)
}

// ---------------------------------------------------------------------------------------------
// Call arguments

/// A value the UI wants to send, coerced to whatever type the daemon's method declares.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    B(bool),
    N(i64),
    S(String),
}

impl Arg {
    pub fn s(v: &str) -> Arg {
        Arg::S(v.to_owned())
    }

    pub fn coerce(&self, sig: Option<&str>) -> Value<'static> {
        let num = |a: &Arg| match a {
            Arg::B(b) => i64::from(*b),
            Arg::N(n) => *n,
            Arg::S(s) => i64::from(matches!(s.as_str(), "on" | "true" | "1")),
        };
        match (sig, self) {
            (Some("b"), a) => Value::Bool(num(a) != 0),
            (Some("y"), a) => Value::U8(num(a).clamp(0, 255) as u8),
            (Some("u"), a) => Value::U32(num(a).max(0) as u32),
            (Some("i"), a) => Value::I32(num(a) as i32),
            (Some("x"), a) => Value::I64(num(a)),
            (Some("d"), a) => Value::F64(num(a) as f64),
            (Some("s"), Arg::B(b)) => Value::from(if *b { "on" } else { "off" }),
            (Some("s"), Arg::N(n)) => Value::from(n.to_string()),
            (_, Arg::S(s)) => Value::from(s.clone()),
            (_, Arg::B(b)) => Value::Bool(*b),
            (_, Arg::N(n)) => Value::I64(*n),
        }
    }
}

pub fn call(s: &Store, iface: &'static str, method: &str, args: &[Arg]) -> crate::bus::Call {
    let sigs = s.methods.get(&format!("{iface}.{method}"));
    let vals = args
        .iter()
        .enumerate()
        .map(|(i, a)| a.coerce(sigs.and_then(|v| v.get(i)).map(String::as_str)))
        .collect();
    crate::bus::Call::new(iface, method, vals)
}

/// The Display1 property names the GUI relies on live in [`bindings`]; this reads them.
pub struct Screen {
    pub has_refresh: bool,
    pub policy: String,
    pub now: String,
    pub has_flicker: bool,
    pub flicker: bool,
    pub brightness: i64,
    pub color_modes: Vec<String>,
    pub color_mode: String,
    pub has_eye: bool,
    pub eye_kelvin: i64,
}

pub fn screen(s: &Store) -> Screen {
    let d = "Display1";
    let hz = bindings::current_hz(s);
    Screen {
        has_refresh: s.has_prop(d, bindings::REFRESH_POLICY),
        policy: s.get(d, bindings::REFRESH_POLICY).str().to_owned(),
        now: if hz > 0.0 {
            format!("{} Hz", hz.round())
        } else {
            String::new()
        },
        has_flicker: s.has_prop(d, bindings::FLICKER_FREE) || s.has_prop(d, bindings::FLICKER_SETTING),
        flicker: bindings::flicker_on(s),
        brightness: if s.has_prop(d, bindings::BRIGHTNESS) {
            s.get(d, bindings::BRIGHTNESS).int()
        } else {
            -1
        },
        color_modes: s.get(d, bindings::COLOR_MODES).strings(),
        color_mode: s.get(d, bindings::COLOR_MODE).str().to_owned(),
        has_eye: s.has_prop(d, bindings::EYE_CARE),
        eye_kelvin: bindings::eye_kelvin(s),
    }
}

pub fn color_label(id: &str) -> String {
    match id {
        "srgb" => "sRGB".into(),
        "p3" => "P3".into(),
        "" => String::new(),
        _ => {
            let mut c = id.chars();
            c.next()
                .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(iface: &str, props: Vec<(&str, V)>) -> Store {
        let mut s = Store::default();
        s.merge(iface, props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
        s
    }

    fn item(name: &str, kind: &str, cur: &str, values: &[&str], w: bool) -> V {
        V::L(vec![
            V::S(name.into()),
            V::S(name.into()),
            V::S(kind.into()),
            V::S(cur.into()),
            V::L(values.iter().map(|v| V::S((*v).into())).collect()),
            V::I(0),
            V::I(10),
            V::B(w),
        ])
    }

    #[test]
    fn hides_what_the_machine_lacks() {
        let mut s = st("Daemon1", vec![("GpuMode", V::S("hybrid".into()))]);
        assert_eq!(sections(&s).iter().map(|x| x.id).collect::<Vec<_>>(), ["gpu"]);
        s.merge("Battery1", vec![("Limit".into(), V::I(80))]);
        s.merge("Fans1", vec![]);
        s.merge(
            "Hardware1",
            vec![(
                "Items".into(),
                V::L(vec![
                    item("boot_sound", "toggle", "1", &[], true),
                    item("panel_overdrive", "toggle", "0", &[], true),
                ]),
            )],
        );
        let ids: Vec<_> = sections(&s).iter().map(|x| x.id).collect();
        assert_eq!(ids, ["gpu", "screen", "battery", "fans", "system"]);
        // Only a screen item: no System page.
        let mut s2 = Store::default();
        s2.merge(
            "Hardware1",
            vec![(
                "Items".into(),
                V::L(vec![item("panel_overdrive", "toggle", "0", &[], true)]),
            )],
        );
        let ids: Vec<_> = sections(&s2).iter().map(|x| x.id).collect();
        assert_eq!(ids, ["screen"]);
        // Lid with no devices is hidden.
        s2.merge("Lid1", vec![("Devices".into(), V::L(vec![]))]);
        assert!(!sections(&s2).iter().any(|x| x.id == "lid"));
    }

    #[test]
    fn steps_choices_and_numbers() {
        let c = HwItem {
            name: "x".into(),
            label: "x".into(),
            kind: "choice".into(),
            current: "b".into(),
            values: vec!["a".into(), "b".into(), "c".into()],
            min: 0,
            max: 0,
            writable: true,
        };
        assert_eq!(c.step(1).as_deref(), Some("c"));
        assert_eq!(c.step(-1).as_deref(), Some("a"));
        let end = HwItem {
            current: "c".into(),
            ..c.clone()
        };
        assert_eq!(end.step(1), None);
        let n = HwItem {
            kind: "number".into(),
            current: "9".into(),
            min: 0,
            max: 10,
            ..c
        };
        assert_eq!(n.step(1).as_deref(), Some("10"));
        assert_eq!(n.step(-1).as_deref(), Some("8"));
    }

    #[test]
    fn fan_setting_falls_back_to_factory_curve() {
        let pts = |v: &[(i64, i64)]| V::L(v.iter().map(|(t, p)| V::L(vec![V::I(*t), V::I(*p)])).collect());
        let s = st(
            "Fans1",
            vec![
                (
                    "Settings",
                    V::L(vec![
                        V::L(vec![
                            V::S("balanced".into()),
                            V::I(1),
                            V::S("custom".into()),
                            pts(&[(30, 10), (60, 50)]),
                        ]),
                        V::L(vec![
                            V::S("balanced".into()),
                            V::I(2),
                            V::S("auto".into()),
                            pts(&[]),
                        ]),
                    ]),
                ),
                (
                    "Factory",
                    V::L(vec![V::L(vec![
                        V::S("balanced".into()),
                        V::I(2),
                        pts(&[(40, 20)]),
                    ])]),
                ),
            ],
        );
        let a = fan_setting(&s, "balanced", 1);
        assert_eq!(
            (a.kind.as_str(), a.points.clone()),
            ("custom", vec![(30, 10), (60, 50)])
        );
        let b = fan_setting(&s, "balanced", 2);
        assert_eq!((b.kind.as_str(), b.points), ("auto", vec![(40, 20)]));
        assert_eq!(fan_setting(&s, "performance", 1).kind, "auto");
    }

    #[test]
    fn points_stay_ordered() {
        let mut p = default_curve();
        move_point(&mut p, 3, 0, 100);
        assert_eq!(p[3], (p[2].0, p[4].1));
        move_point(&mut p, 0, 0, -5);
        assert_eq!(p[0], (20, 0));
        move_point(&mut p, 7, 500, 500);
        assert_eq!(p[7], (100, 100));
        assert!(p.windows(2).all(|w| w[0].0 <= w[1].0 && w[0].1 <= w[1].1));
    }

    #[test]
    fn limits_use_custom_values_per_mode_and_source() {
        let row = |n: &str, min, max, def| {
            V::L(vec![
                V::S(n.into()),
                V::S(n.into()),
                V::S("W".into()),
                V::I(min),
                V::I(max),
                V::I(def),
                V::I(def),
            ])
        };
        let s = st(
            "Power1",
            vec![
                (
                    "Limits",
                    V::L(vec![
                        row("ppt_pl1_spl", 5, 65, 30),
                        row("ppt_pl2_sppt", 5, 80, 40),
                    ]),
                ),
                (
                    "Custom",
                    V::L(vec![V::L(vec![
                        V::S("balanced".into()),
                        V::S("ac".into()),
                        V::S("ppt_pl1_spl".into()),
                        V::I(45),
                    ])]),
                ),
            ],
        );
        let l = limits(&s, "balanced", "ac");
        assert_eq!(
            (l[0].cur, l[0].custom, l[1].cur, l[1].custom),
            (45, true, 40, false)
        );
        assert!(!limits(&s, "balanced", "battery")[0].custom);
    }

    #[test]
    fn limits_keep_their_order() {
        let l = |n: &str, min, max, cur| Limit {
            name: n.into(),
            label: n.into(),
            unit: "W".into(),
            min,
            max,
            def: cur,
            cur,
            custom: false,
        };
        let ls = [l("ppt_pl1_spl", 28, 90, 60), l("ppt_pl2_sppt", 28, 135, 80), l("nv_tgp", 5, 20, 10)];
        // Sustained above boost drags boost along.
        assert_eq!(limit_fixes(&ls, "ppt_pl1_spl", 100), vec![("ppt_pl2_sppt".to_owned(), 100)]);
        // Boost below sustained drags sustained down.
        assert_eq!(limit_fixes(&ls, "ppt_pl2_sppt", 40), vec![("ppt_pl1_spl".to_owned(), 40)]);
        assert!(limit_fixes(&ls, "ppt_pl1_spl", 70).is_empty());
        assert!(limit_fixes(&ls, "nv_tgp", 15).is_empty());
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_duration(3600 + 25 * 60), "1h 25m");
        assert_eq!(fmt_duration(90), "2m");
        assert_eq!(fmt_duration(-1), "");
        assert_eq!(fmt_watts(12.34), "12.3 W");
        assert_eq!(fmt_watts(-1.0), "");
        assert_eq!(fmt_watts(0.0), "");
        assert_eq!(battery_line(82, "Not charging", 0.0), "82%  Not charging");
        assert_eq!(battery_line(75, "Charging", 41.94), "75%  Charging 41.9 W");
        assert_eq!(battery_line(75, "Full", 3.0), "75%  Full");
        assert_eq!(battery_line(-1, "Full", 3.0), "");
        assert_eq!(step_index(&[60, 65, 70, 75, 80, 100], 80), 4);
        assert_eq!(step_index(&[60, 65, 70, 75, 80, 100], 77), 3);
        assert_eq!(step_index(&[], 77), 0);
        assert_eq!(color_label("srgb"), "sRGB");
        assert_eq!(color_label("native"), "Native");
    }

    #[test]
    fn arguments_follow_the_declared_signature() {
        let mut s = Store::default();
        s.methods
            .insert("Display1.SetFlickerFree".into(), vec!["s".into()]);
        s.methods.insert("Display1.SetEyeCare".into(), vec!["b".into()]);
        let c = call(&s, "Display1", "SetFlickerFree", &[Arg::B(true)]);
        assert_eq!(c.args, vec![Value::from("on")]);
        let c = call(&s, "Display1", "SetEyeCare", &[Arg::N(4500)]);
        assert_eq!(c.args, vec![Value::Bool(true)]);
        let c = call(&s, "Display1", "SetUnknown", &[Arg::N(3)]);
        assert_eq!(c.args, vec![Value::I64(3)]);
        assert_eq!(Arg::N(300).coerce(Some("y")), Value::U8(255));
    }

    #[test]
    fn cycles_with_wrap() {
        let l = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(cycle(&l, "c", 1).map(String::as_str), Some("a"));
        assert_eq!(cycle(&l, "a", -1).map(String::as_str), Some("c"));
        assert_eq!(cycle(&[], "a", 1), None);
        assert_eq!(cycle(&l, "", 1).map(String::as_str), Some("a"));
        assert_eq!(cycle(&l, "", -1).map(String::as_str), Some("c"));
    }
}
