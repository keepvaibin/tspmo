//! `tspmo-gui --osd <icon> <text> [value]`: the on-screen note (bottom centre, 10 % of the screen
//! height up, click-through, about 1.2 s). The daemon calls it instead of the desktop's own OSD.

use crate::{Osd, Th, theme};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
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

/// Wire format of one update: `icon \t text \t value-or-empty \n`. Tabs and line breaks inside
/// the fields become spaces.
pub fn encode(args: &[String]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let f = |i: usize| args.get(i).map_or(String::new(), |s| clean(s));
    format!("{}\t{}\t{}\n", f(0), f(1), f(2))
}

/// Inverse of [`encode`]; `None` for an empty or icon-less line.
pub fn decode(line: &str) -> Option<Args> {
    let mut it = line.trim_end_matches(['\n', '\r']).splitn(3, '\t');
    let icon = it.next().filter(|s| !s.is_empty())?;
    let text = it.next().unwrap_or("");
    let mut v = vec![icon.to_owned(), text.to_owned()];
    v.extend(it.next().filter(|s| !s.is_empty()).map(str::to_owned));
    parse(&v).ok()
}

/// Hand the update to a running OSD. True once it acknowledged (it is alive and will show it).
fn send(args: &[String]) -> bool {
    let Ok(a) = crate::instance::addr_named("tspmo-osd") else {
        return false;
    };
    let Ok(mut s) = UnixStream::connect_addr(&a) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
    if s.write_all(encode(args).as_bytes()).is_err() {
        return false;
    }
    let mut ack = String::new();
    BufReader::new(s).read_line(&mut ack).is_ok_and(|n| n > 0)
}

/// Bind the OSD socket, or `None` when another instance holds it.
fn bind() -> Option<UnixListener> {
    let a = crate::instance::addr_named("tspmo-osd").ok()?;
    UnixListener::bind_addr(&a).ok()
}

/// The daemon passes theme icon names (or mode ids); the profile ones pick the matching accent.
fn mode_of(icon: &str) -> &str {
    if icon.contains("performance") {
        "performance"
    } else if icon.contains("powersave") {
        "eco-plus"
    } else if icon.contains("night") {
        "silent"
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
    } else if has(&["silent", "night", "quiet"]) {
        "silent"
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

thread_local! {
    static STATE: RefCell<Option<(slint::Weak<Osd>, slint::Timer, slint::Timer)>> =
        const { RefCell::new(None) };
}

/// Put `a` on the (already visible) window and restart the hide timers; a fade in progress
/// turns back to full opacity.
fn apply(win: &Osd, a: &Args) {
    let dark = theme::is_dark();
    win.global::<Th>()
        .set_accent_target(crate::app::mode_color(mode_of(&a.icon), dark));
    win.set_text(a.text.as_str().into());
    win.set_glyph(icon_of(&a.icon).into());
    win.set_value(a.value.unwrap_or(-1.0));
    win.set_shown(true);
    STATE.with(|s| {
        if let Some((weak, fade, quit)) = s.borrow().as_ref() {
            let w = weak.clone();
            fade.start(slint::TimerMode::SingleShot, FADE_OUT_AT, move || {
                if let Some(w) = w.upgrade() {
                    w.set_shown(false);
                }
            });
            quit.start(slint::TimerMode::SingleShot, TOTAL, || {
                let _ = slint::quit_event_loop();
            });
        }
    });
}

/// Block in the kernel for updates; each one is applied on the UI thread and acknowledged only
/// when the event loop took it (a loop that is already quitting leaves it unacknowledged, and the
/// sender then shows its own).
fn serve(l: UnixListener) {
    std::thread::Builder::new()
        .name("osd-instance".into())
        .spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(&s).read_line(&mut line).is_err() {
                    continue;
                }
                let Some(a) = decode(&line) else { continue };
                let ok = slint::invoke_from_event_loop(move || {
                    STATE.with(|st| {
                        let w = st.borrow().as_ref().and_then(|(w, ..)| w.upgrade());
                        if let Some(w) = w {
                            apply(&w, &a);
                        }
                    });
                })
                .is_ok();
                if ok {
                    let _ = s.write_all(b"ok\n");
                }
            }
        })
        .ok();
}

pub fn run(args: &[String]) -> Result<(), String> {
    let a = parse(args)?;
    // A running OSD takes the update; otherwise become it.
    let mut listener = None;
    for _ in 0..3 {
        if send(args) {
            return Ok(());
        }
        listener = bind();
        if listener.is_some() {
            break;
        }
    }
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
    let th = win.global::<Th>();
    let f = theme::fonts();
    th.set_dark(theme::is_dark());
    th.set_font_size(f.px);
    th.set_font_family(f.family.into());
    th.set_mono(f.mono.into());
    STATE.with(|s| {
        *s.borrow_mut() = Some((win.as_weak(), slint::Timer::default(), slint::Timer::default()));
    });
    win.show().map_err(|e| e.to_string())?;
    apply(&win, &a);
    if let Some(l) = listener {
        serve(l);
    }
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
    fn wire_roundtrip() {
        let line = encode(&v(&["icon", "Eco+", "40"]));
        assert_eq!(line, "icon\tEco+\t40\n");
        let a = decode(&line).unwrap();
        assert_eq!(
            (a.icon.as_str(), a.text.as_str(), a.value),
            ("icon", "Eco+", Some(0.4))
        );
        let a = decode(&encode(&v(&["x", "a\tb\nc"]))).unwrap();
        assert_eq!((a.text.as_str(), a.value), ("a b c", None));
        assert!(decode("\n").is_none());
        assert!(decode("").is_none());
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
