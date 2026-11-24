//! `tspmo-gui --osd <icon> <text> [value]`: the on-screen note (bottom centre, 10 % of the screen
//! height up, click-through, about 1.2 s). The daemon calls it instead of the desktop's own OSD.

use crate::{Osd, Th, theme};
use slint::ComponentHandle;
use std::time::Duration;

pub struct Args {
    /// A mode id (`eco-plus`, `performance`, ...) picks the accent; anything else keeps the
    /// default.
    pub icon: String,
    pub text: String,
    /// 0.0 to 1.0.
    pub value: Option<f32>,
}

/// `[value]` is a percentage; anything unparsable shows no bar.
pub fn parse(args: &[String]) -> Result<Args, String> {
    let icon = args.first().ok_or("usage: --osd <icon> <text> [value]")?.clone();
    let text = args.get(1).cloned().unwrap_or_default();
    let value = args
        .get(2)
        .and_then(|v| v.trim_end_matches('%').parse::<f32>().ok())
        .map(|v| (v / 100.0).clamp(0.0, 1.0));
    Ok(Args { icon, text, value })
}

/// The daemon passes theme icon names (or mode ids); the profile ones pick the matching accent.
fn mode_of(icon: &str) -> &str {
    if icon.contains("performance") {
        "performance"
    } else if icon.contains("powersave") {
        "eco-plus"
    } else if icon.contains("night") {
        "power-saver"
    } else {
        icon
    }
}

/// One of the UI's own line icons for a theme icon name.
pub fn icon_of(icon: &str) -> &'static str {
    let i = icon.to_ascii_lowercase();
    let has = |w: &[&str]| w.iter().any(|w| i.contains(w));
    if has(&["performance", "turbo"]) {
        "performance"
    } else if has(&["powersave", "eco-plus", "eco+"]) {
        "eco-plus"
    } else if has(&["power-saver", "night", "silent"]) {
        "power-saver"
    } else if has(&["balanced", "profile"]) {
        "balanced"
    } else if has(&["keyboard", "kbd", "aura", "light"]) {
        "keyboard"
    } else if has(&["battery", "charge"]) {
        "battery"
    } else if has(&["brightness", "display-brightness", "sun"]) {
        "sun"
    } else if has(&["gpu", "chip", "graphics"]) {
        "chip"
    } else if has(&["screen", "display", "monitor", "refresh"]) {
        "screen"
    } else if has(&["fan", "tune"]) {
        "tune"
    } else if has(&["slash"]) {
        "slash"
    } else {
        "gauge"
    }
}

/// Fade in, hold, fade out: gone after this long.
const TOTAL: Duration = Duration::from_millis(1200);
const FADE_OUT_AT: Duration = Duration::from_millis(950);

pub fn run(args: &[String]) -> Result<(), String> {
    let a = parse(args)?;
    let layer = crate::use_layer_shell();
    if !layer && !cfg!(feature = "winit") {
        return Err("no Wayland compositor".into());
    }
    crate::wl::next_kind(crate::wl::Kind::Osd);
    let win = Osd::new().map_err(|e| e.to_string())?;
    #[cfg(feature = "winit")]
    if !layer {
        slint::set_xdg_app_id("org.tspmo.osd").map_err(|e| e.to_string())?;
    }
    let dark = theme::is_dark();
    let f = theme::fonts();
    let th = win.global::<Th>();
    th.set_dark(dark);
    th.set_font_size(f.px);
    th.set_font_family(f.family.into());
    th.set_mono(f.mono.into());
    th.set_accent_target(crate::app::mode_color(mode_of(&a.icon), dark));
    win.set_text(a.text.into());
    win.set_glyph(icon_of(&a.icon).into());
    win.set_value(a.value.unwrap_or(-1.0));
    win.show().map_err(|e| e.to_string())?;
    win.set_shown(true);
    let weak = win.as_weak();
    slint::Timer::single_shot(FADE_OUT_AT, move || {
        if let Some(w) = weak.upgrade() {
            w.set_shown(false);
        }
    });
    slint::Timer::single_shot(TOTAL, || {
        let _ = slint::quit_event_loop();
    });
    slint::run_event_loop().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn theme_icons_become_line_icons() {
        assert_eq!(icon_of("battery-profile-performance"), "performance");
        assert_eq!(icon_of("battery-profile-powersave"), "eco-plus");
        assert_eq!(icon_of("input-keyboard"), "keyboard");
        assert_eq!(icon_of("display-brightness"), "sun");
        assert_eq!(icon_of("whatever"), "gauge");
    }

    #[test]
    fn icons_pick_accents() {
        assert_eq!(mode_of("battery-profile-performance"), "performance");
        assert_eq!(mode_of("battery-profile-powersave"), "eco-plus");
        assert_eq!(mode_of("input-keyboard"), "input-keyboard");
    }

    #[test]
    fn parses_value_as_percent() {
        let a = parse(&v(&["battery", "80%", "80"])).unwrap();
        assert_eq!(
            (a.icon.as_str(), a.text.as_str(), a.value),
            ("battery", "80%", Some(0.8))
        );
        assert_eq!(parse(&v(&["light", "Aura"])).unwrap().value, None);
        assert_eq!(parse(&v(&["light", "x", "250"])).unwrap().value, Some(1.0));
        assert!(parse(&[]).is_err());
    }
}
