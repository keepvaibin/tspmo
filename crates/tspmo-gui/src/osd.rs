//! `tspmo-gui --osd <icon> <text> [value]`: a small overlay for desktops without their own OSD.

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

/// The daemon passes theme icon names; the profile ones pick the matching accent.
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
    win.set_value(a.value.unwrap_or(-1.0));
    win.show().map_err(|e| e.to_string())?;
    slint::Timer::single_shot(Duration::from_millis(1500), || {
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
