//! `tspmo-gui`: the popup. Opens on the ROG key, closes on the second press, leaves nothing
//! resident: the process ends with its last window.
//!
//! ```text
//! tspmo-gui [--toggle]                     open the popup, or close it if it is open
//! tspmo-gui --window fans|screen|keys|system   open that window
//! tspmo-gui --osd <icon> <text> [value]    1.5 s overlay with an optional 0-100 bar
//! tspmo-gui --install-shortcut             bind the ROG key for this user
//! tspmo-gui --snapshot <file.ppm> [--window w] [--scale 1.5]   render once, headless
//! ```

mod app;
mod bindings;
mod bus;
mod env;
mod instance;
mod model;
mod osd;
mod px;
mod shortcut;
mod snapshot;
mod theme;
mod value;
mod wl;

slint::include_modules!();

use app::Kind;

/// A theme icon name, used by the desktop entry and the shortcut installer.
pub const APP_ICON: &str = "preferences-system-power-management";
/// Desktop entry and Wayland app id.
pub const APP_ID: &str = "org.tspmo.gui";

/// Process start, for `TSPMO_TRACE=1` timings.
pub static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// `TSPMO_TRACE=1`: print a timing line to stderr.
pub fn trace(what: &str) {
    if std::env::var_os("TSPMO_TRACE").is_some()
        && let Some(t) = T0.get()
    {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        eprintln!(
            "trace {:>7.1} ms  {what}  (epoch ns {wall})",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}

fn main() -> std::process::ExitCode {
    let _ = T0.set(std::time::Instant::now());
    env::force_mesa();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("--osd") => osd::run(&args[1..]),
        Some("--install-shortcut") => shortcut::install(),
        Some("--snapshot") => snapshot::run(&args[1..]),
        Some("--help" | "-h") => {
            println!(
                "tspmo-gui [--toggle] | --window fans|screen|keys|system | --osd <icon> <text> [0-100] | --install-shortcut"
            );
            Ok(())
        }
        _ => gui(&args),
    };
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tspmo-gui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn gui(args: &[String]) -> Result<(), String> {
    let window = args
        .iter()
        .position(|a| a == "--window" || a == "--section")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    let kind = match window {
        Some(n) => Kind::parse(n).ok_or_else(|| format!("unknown window {n}"))?,
        None => Kind::Popup,
    };
    let msg = match window {
        Some(n) if kind != Kind::Popup => format!("open {n}"),
        _ => "toggle".to_owned(),
    };
    let listener = match instance::claim(&msg) {
        instance::Role::Toggled => return Ok(()),
        instance::Role::Window(l) => l,
    };
    let layer = use_layer_shell();
    if !layer && !cfg!(feature = "winit") {
        return Err("no Wayland compositor (X11 needs a build with --features winit)".into());
    }
    app::start(&[kind], theme::is_dark(), theme::fonts())?;
    instance::serve(listener, app::message);
    slint::run_event_loop().map_err(|e| e.to_string())
}

/// Install the Wayland platform (layer-shell popup, or a plain window where that is missing).
/// Without a Wayland compositor the winit backend takes over when it is built in.
pub fn use_layer_shell() -> bool {
    if let Some(p) = wl::connect(APP_ID)
        && slint::platform::set_platform(Box::new(p)).is_ok()
    {
        LAYER.store(true, std::sync::atomic::Ordering::Relaxed);
        return true;
    }
    false
}

static LAYER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Slint's winit backend needs the application id after the first component exists and before
/// it is shown. The Wayland platform takes it at connect time.
pub fn name_window() {
    #[cfg(feature = "winit")]
    if !LAYER.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = slint::set_xdg_app_id(APP_ID);
    }
}
