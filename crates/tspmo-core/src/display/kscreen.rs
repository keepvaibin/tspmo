//! Reading and changing KWin's outputs through `kscreen-doctor` (KDE Plasma 6).
//!
//! This module only parses its text and builds its arguments; the daemon runs the commands in
//! the user's session.

#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    /// KWin's mode id (what `output.<name>.mode.<id>` takes).
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub hz: f64,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Output {
    pub name: String,
    pub enabled: bool,
    pub modes: Vec<Mode>,
    pub scale: f64,
    /// KWin's brightness setting, 0..100, if the output has brightness control.
    pub brightness: Option<u32>,
    /// KWin's software dimming, 0..100.
    pub dimming: Option<u32>,
    /// `sRGB`, `ICC` or `EDID`.
    pub color_source: String,
    pub icc_profile: String,
}

impl Output {
    pub fn current_mode(&self) -> Option<&Mode> {
        self.modes.iter().find(|m| m.current)
    }

    /// Is this the laptop's own panel (`eDP-*`, `LVDS-*`, `DSI-*`)?
    pub fn is_internal(&self) -> bool {
        ["eDP", "LVDS", "DSI"].iter().any(|p| self.name.starts_with(p))
    }
}

/// Remove ANSI colour escapes (`kscreen-doctor` colours its output even when piped).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for n in it.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_mode(tok: &str) -> Option<Mode> {
    // `1:2560x1600@240.00*!`
    let (id, rest) = tok.split_once(':')?;
    let current = rest.contains('*');
    let rest = rest.trim_end_matches(['*', '!']);
    let (res, hz) = rest.split_once('@')?;
    let (w, h) = res.split_once('x')?;
    Some(Mode {
        id: id.to_owned(),
        width: w.parse().ok()?,
        height: h.parse().ok()?,
        hz: hz.parse().ok()?,
        current,
    })
}

fn percent_after(line: &str, key: &str) -> Option<u32> {
    let rest = &line[line.find(key)? + key.len()..];
    rest.trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Parse the output of `kscreen-doctor -o`.
pub fn parse_outputs(text: &str) -> Vec<Output> {
    let mut out: Vec<Output> = Vec::new();
    for raw in strip_ansi(text).lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Output:") {
            let mut parts = rest.split_whitespace();
            let _id = parts.next();
            out.push(Output {
                name: parts.next().unwrap_or_default().to_owned(),
                scale: 1.0,
                ..Output::default()
            });
            continue;
        }
        let Some(o) = out.last_mut() else { continue };
        if line == "enabled" {
            o.enabled = true;
        } else if let Some(rest) = line.strip_prefix("Modes:") {
            o.modes = rest.split_whitespace().filter_map(parse_mode).collect();
        } else if let Some(v) = line.strip_prefix("Scale:") {
            o.scale = v.trim().parse().unwrap_or(1.0);
        } else if line.starts_with("Brightness control:") && line.contains("supported,") {
            o.brightness = percent_after(line, "set to");
            o.dimming = percent_after(line, "dimming to");
        } else if let Some(v) = line.strip_prefix("Color profile source:") {
            o.color_source = v.trim().to_owned();
        } else if let Some(v) = line.strip_prefix("ICC profile:") {
            o.icc_profile = v.trim().to_owned();
        }
    }
    out
}

/// `kscreen-doctor` argument selecting a mode.
pub fn set_mode(output: &str, mode_id: &str) -> String {
    format!("output.{output}.mode.{mode_id}")
}

pub fn set_brightness(output: &str, percent: u32) -> String {
    format!("output.{output}.brightness.{}", percent.min(100))
}

pub fn set_color_source(output: &str, source: &str) -> String {
    format!("output.{output}.colorProfileSource.{source}")
}

pub fn set_icc_profile(output: &str, path: &str) -> String {
    format!("output.{output}.iccprofile.{path}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const SAMPLE: &str = "\x1b[01;32mOutput: \x1b[0;0m1 eDP-1 ecbc3bbd-dbaf\n\
\t\x1b[01;32menabled\x1b[0;0m\n\
\t\x1b[01;32mconnected\x1b[0;0m\n\
\t\x1b[01;34mModes: \x1b[0;0m 1:\x1b[01;32m2560x1600@240.00*\x1b[0;0m!  2:2560x1600@60.00  3:1600x1200@59.87  4:1600x1200@239.94\n\
\t\x1b[01;33mScale: \x1b[0;0m1.5\n\
\t\x1b[01;33mColor profile source: \x1b[0;0msRGB\n\
\t\x1b[01;33mICC profile: \x1b[0;0mnone\n\
\t\x1b[01;33mBrightness control: \x1b[0;0msupported, set to 15% and dimming to 100%\n\
Output: 2 DP-3 aaaa\n\
\tenabled\n\
\tModes:  1:1920x1080@60.00*!  2:1920x1080@144.00\n\
\tBrightness control: unsupported\n";

    #[test]
    fn parses_the_laptop_panel() {
        let o = parse_outputs(SAMPLE);
        assert_eq!(o.len(), 2);
        let p = &o[0];
        assert_eq!(p.name, "eDP-1");
        assert!(p.enabled && p.is_internal());
        assert_eq!(p.modes.len(), 4);
        let cur = p.current_mode().unwrap();
        assert_eq!((cur.id.as_str(), cur.width, cur.hz), ("1", 2560, 240.0));
        assert_eq!(p.scale, 1.5);
        assert_eq!((p.brightness, p.dimming), (Some(15), Some(100)));
        assert_eq!(p.color_source, "sRGB");
        assert_eq!(p.icc_profile, "none");
    }

    #[test]
    fn external_output_without_brightness() {
        let o = parse_outputs(SAMPLE);
        let e = &o[1];
        assert!(!e.is_internal());
        assert_eq!(e.brightness, None);
        assert_eq!(e.current_mode().unwrap().hz, 60.0);
    }

    #[test]
    fn argument_builders() {
        assert_eq!(set_mode("eDP-1", "2"), "output.eDP-1.mode.2");
        assert_eq!(set_brightness("eDP-1", 140), "output.eDP-1.brightness.100");
        assert_eq!(
            set_color_source("eDP-1", "ICC"),
            "output.eDP-1.colorProfileSource.ICC"
        );
    }
}
