//! Colour modes: the panel's gamut and an eye-care white point.
//!
//! KWin does the work: a per-output ICC profile (generated here from the panel's EDID
//! primaries) for the gamut modes, and Night Light in constant mode for eye care.

use super::{Shared, State, kscreen_doctor, lock};
use crate::session;
use std::path::PathBuf;
use std::time::Duration;
use tspmo_core::display::{icc, ini, kscreen};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The panel's own gamut: sRGB content is shown as is, saturated on a wide-gamut panel.
    #[default]
    Native,
    /// Colour-accurate sRGB: a profile built from the panel's primaries.
    Srgb,
    /// Colour-accurate for Display-P3 sources.
    P3,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Srgb => "srgb",
            Self::P3 => "p3",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "native" | "vivid" | "default" => Some(Self::Native),
            "srgb" | "accurate" => Some(Self::Srgb),
            "p3" | "dci-p3" | "display-p3" | "cinema" => Some(Self::P3),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub mode: Mode,
    /// Eye-care white point in kelvin; 0 = off.
    pub eyecare_k: u32,
    /// Night Light settings from before eye care took over, to put back when it ends.
    pub saved_nightlight: Option<[String; 3]>,
    /// A dimming profile is loaded in the compositor (so it must be replaced to undim).
    pub dimmed: bool,
    /// The compositor's colour source for the panel is ICC (set by us since the last
    /// [`Settings::forget_source`]). Setting it again makes KWin re-apply the output and write
    /// the backlight to 100 % for a moment: a flash, on every brightness step.
    pub icc_loaded: bool,
}

impl Settings {
    /// The compositor may have reset the output (login, resume, a mode change): the next
    /// profile load sets the source again.
    pub fn forget_source(&mut self) {
        self.icc_loaded = false;
    }

    pub fn load(&mut self, key: &str, v: &str) {
        match key {
            "color_mode" => self.mode = Mode::parse(v).unwrap_or(self.mode),
            "eyecare_k" => self.eyecare_k = v.parse().unwrap_or(0),
            "nightlight_saved" => {
                let p: Vec<&str> = v.split(',').collect();
                if let [a, b, c] = p[..] {
                    self.saved_nightlight = Some([a.into(), b.into(), c.into()]);
                }
            }
            _ => {}
        }
    }

    pub fn save(&self) -> String {
        let mut t = format!(
            "color_mode={}\neyecare_k={}\n",
            self.mode.as_str(),
            self.eyecare_k
        );
        if let Some([a, b, c]) = &self.saved_nightlight {
            t.push_str(&format!("nightlight_saved={a},{b},{c}\n"));
        }
        t
    }
}

/// Modes this panel can do.
pub fn available(s: &State) -> Vec<String> {
    let have_primaries = s
        .panel
        .as_ref()
        .and_then(|p| p.edid.as_ref())
        .is_some_and(|e| e.primaries.is_some());
    let mut v = vec![Mode::Native.as_str().to_owned()];
    if have_primaries {
        v.push(Mode::Srgb.as_str().to_owned());
        v.push(Mode::P3.as_str().to_owned());
    }
    v
}

fn icc_dir(s: &State) -> PathBuf {
    s.state_dir.join("icc")
}

/// The profile for a colour mode with the image dimmed to `dim_pct` percent of the light. Native
/// is sRGB primaries (what the compositor assumes without a profile), so a native profile with
/// no dimming is never needed.
fn build_profile(s: &State, mode: Mode, dim_pct: u32) -> Result<Vec<u8>, String> {
    let edid = s.panel.as_ref().and_then(|p| p.edid.as_ref());
    let (name, primaries) = match mode {
        Mode::Native => ("native", icc::SRGB),
        Mode::Srgb => (
            "sRGB",
            edid.and_then(|e| e.primaries.clone())
                .ok_or("panel colours unknown")?,
        ),
        Mode::P3 => ("Display P3", icc::DISPLAY_P3),
    };
    let panel = edid.map_or("panel", |e| e.name.as_str());
    Ok(icc::build_dimmed(
        &format!("tspmo {name}, {dim_pct}% on {panel}"),
        &primaries,
        icc::Trc::Gamma(2.2),
        f64::from(dim_pct) / 100.0,
    ))
}

/// Make the compositor show the colour mode and dimming in force now. Profiles are files named
/// by mode and dimming, written once and kept (the compositor identifies them by path).
pub fn apply_profile(shared: &Shared) -> Result<(), String> {
    let (root, connector, dim_pct, path, bytes, icc_loaded) = {
        let s = lock(shared);
        let dim_pct = s.flicker_state.dimming_pct.round().clamp(1.0, 100.0) as u32;
        let mode = s.color.mode;
        if !s.compositor {
            drop(s);
            lock(shared).color.dimmed = dim_pct < 100;
            return Ok(());
        }
        let connector = s.connector().map(str::to_owned).ok_or("no internal panel")?;
        let (path, bytes) = if mode == Mode::Native && dim_pct >= 100 {
            (None, Vec::new())
        } else {
            let path = icc_dir(&s).join(format!("{}-{dim_pct}.icc", mode.as_str()));
            let bytes = if path.exists() {
                Vec::new()
            } else {
                build_profile(&s, mode, dim_pct)?
            };
            (Some(path), bytes)
        };
        (
            s.root.clone(),
            connector,
            dim_pct,
            path,
            bytes,
            s.color.icc_loaded,
        )
    };
    let mut args: Vec<String> = Vec::new();
    match &path {
        Some(p) => {
            if !bytes.is_empty() {
                write_profile_file(p, &bytes)?;
            }
            args.push(kscreen::set_icc_profile(&connector, &p.to_string_lossy()));
            if !icc_loaded {
                args.push(kscreen::set_color_source(&connector, "ICC"));
            }
        }
        None => args.push(kscreen::set_color_source(&connector, "sRGB")),
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    kscreen_doctor(&root, &refs)?;
    {
        let mut s = lock(shared);
        s.color.dimmed = dim_pct < 100;
        s.color.icc_loaded = path.is_some();
    }
    if let Some(p) = &path {
        prune(p.parent(), p);
    }
    Ok(())
}

fn write_profile_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().ok_or("bad path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // The user's compositor reads it.
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).map_err(|e| e.to_string())
}

/// Keep the profile directory small: the newest few files stay (the compositor may still
/// hold the previous one).
fn prune(dir: Option<&std::path::Path>, keep: &std::path::Path) {
    const KEEP: usize = 12;
    let Some(dir) = dir else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| {
            let e = e.ok()?;
            Some((e.metadata().ok()?.modified().ok()?, e.path()))
        })
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(KEEP);
    for (_, p) in files.into_iter().take(excess) {
        if p != keep {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub fn set_mode(shared: &Shared, name: &str) -> Result<String, String> {
    let mode = Mode::parse(name).ok_or_else(|| format!("unknown colour mode '{name}'"))?;
    let previous = {
        let mut s = lock(shared);
        if !available(&s).iter().any(|m| m == mode.as_str()) {
            return Err(format!("{} isn't available on this panel", mode.as_str()));
        }
        std::mem::replace(&mut s.color.mode, mode)
    };
    if let Err(e) = apply_profile(shared) {
        lock(shared).color.mode = previous;
        return Err(e);
    }
    lock(shared).save();
    session::osd(
        "preferences-desktop-display-color",
        &format!("Colour: {}", mode.as_str()),
        None,
    );
    Ok(format!("Colour {}", mode.as_str()))
}

// --- eye care (Night Light, constant temperature) -------------------------------------------

const GROUP: &str = "NightColor";
const KEYS: [&str; 3] = ["Active", "Mode", "NightTemperature"];

/// The Night Light settings that turn constant-temperature mode on at `kelvin`, or, when
/// `kelvin` is 0, the `saved` ones (an empty saved value means the key was not set).
pub fn eyecare_values(kelvin: u32, saved: &[String; 3]) -> [Option<String>; 3] {
    if kelvin == 0 {
        saved.clone().map(|v| (!v.is_empty()).then_some(v))
    } else {
        [
            Some("true".into()),
            Some("Constant".into()),
            Some(kelvin.to_string()),
        ]
    }
}

/// `kwriteconfig6` arguments setting (or, for `None`, deleting) one Night Light key. Going
/// through KDE's own tool with `--notify` matters: that tells the running KWin the file changed.
fn write_args(key: &str, value: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = ["--notify", "--file", "kwinrc", "--group", GROUP, "--key", key]
        .into_iter()
        .map(String::from)
        .collect();
    match value {
        Some(v) => a.push(v.to_owned()),
        None => a.push("--delete".to_owned()),
    }
    a
}

pub fn set_eyecare(shared: &Shared, kelvin: u32) -> Result<String, String> {
    let root = lock(shared).root.clone();
    let Some(sess) = session::active(&root) else {
        return Err("no desktop session".into());
    };
    let config =
        std::fs::read_to_string(PathBuf::from(&sess.home).join(".config/kwinrc")).unwrap_or_default();
    // Remember what Night Light was before eye care first took it over.
    let saved = {
        let mut s = lock(shared);
        if kelvin != 0 && s.color.saved_nightlight.is_none() {
            s.color.saved_nightlight = Some(KEYS.map(|k| ini::get(&config, GROUP, k).unwrap_or_default()));
        }
        s.color
            .saved_nightlight
            .clone()
            .unwrap_or_else(|| KEYS.map(|_| String::new()))
    };
    for (key, v) in KEYS.iter().zip(eyecare_values(kelvin, &saved)) {
        let args = write_args(key, v.as_deref());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let o =
            session::run(&sess, "kwriteconfig6", &refs, Duration::from_secs(5)).map_err(|e| e.to_string())?;
        if !o.ok {
            return Err(format!("kwriteconfig6: {}", o.stderr.trim()));
        }
    }
    let mut s = lock(shared);
    s.color.eyecare_k = kelvin;
    if kelvin == 0 {
        s.color.saved_nightlight = None;
    }
    s.save();
    drop(s);
    let text = if kelvin == 0 {
        "Eye care off".to_owned()
    } else {
        format!("Eye care {kelvin} K")
    };
    session::osd("redshift-status-on", &text, None);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_names() {
        assert_eq!(Mode::parse("Vivid"), Some(Mode::Native));
        assert_eq!(Mode::parse("display-p3"), Some(Mode::P3));
        assert_eq!(Mode::parse("x"), None);
    }

    #[test]
    fn settings_round_trip() {
        let mut s = Settings {
            mode: Mode::Srgb,
            eyecare_k: 4500,
            saved_nightlight: Some(["".into(), "Automatic".into(), "4000".into()]),
            dimmed: false,
            icc_loaded: false,
        };
        let text = s.save();
        let mut t = Settings::default();
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            t.load(k, v);
        }
        assert_eq!((t.mode, t.eyecare_k), (Mode::Srgb, 4500));
        assert_eq!(t.saved_nightlight, s.saved_nightlight.take());
    }

    #[test]
    fn eyecare_turns_on_and_restores_the_users_settings() {
        let config = "[NightColor]\nMode=Automatic\nNightTemperature=4000\n\n[Other]\nx=1\n";
        let saved = KEYS.map(|k| ini::get(config, GROUP, k).unwrap_or_default());
        assert_eq!(saved, ["".to_owned(), "Automatic".to_owned(), "4000".to_owned()]);

        let on = eyecare_values(4500, &saved);
        assert_eq!(
            on,
            [Some("true".into()), Some("Constant".into()), Some("4500".into())]
        );
        let off = eyecare_values(0, &saved);
        assert_eq!(off, [None, Some("Automatic".into()), Some("4000".into())]);
        // Unset keys are deleted, set ones written back.
        assert_eq!(
            write_args("Active", None).last().map(String::as_str),
            Some("--delete")
        );
        assert_eq!(write_args("Mode", Some("Automatic"))[6..], ["Mode", "Automatic"]);
    }
}
