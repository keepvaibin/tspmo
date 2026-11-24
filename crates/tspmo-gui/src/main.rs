//! `tspmo-gui`: the popup. Opens on the ROG key, closes on the second press, leaves nothing
//! resident.
//!
//! ```text
//! tspmo-gui [--toggle] [--section <id>]   open, or close the one that is open
//! tspmo-gui --osd <icon> <text> [value]    1.5 s overlay with an optional 0-100 bar
//! tspmo-gui --install-shortcut             bind the ROG key for this user
//! ```

mod app;
mod bindings;
mod bus;
mod env;
mod instance;
mod model;
mod osd;
mod shortcut;
mod theme;
mod value;

slint::include_modules!();

use slint::ComponentHandle;

/// Placeholder icon (a theme icon name), used by the desktop entry and the shortcut installer.
pub const APP_ICON: &str = "preferences-system-power-management";
/// Desktop entry and Wayland app id.
pub const APP_ID: &str = "org.tspmo.gui";

fn main() -> std::process::ExitCode {
    env::force_mesa();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("--osd") => osd::run(&args[1..]),
        Some("--install-shortcut") => shortcut::install(),
        Some("--help" | "-h") => {
            println!(
                "tspmo-gui [--toggle] [--section <id>] | --osd <icon> <text> [0-100] | --install-shortcut"
            );
            Ok(())
        }
        _ => popup(&args),
    };
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tspmo-gui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn popup(args: &[String]) -> Result<(), String> {
    let listener = match instance::claim() {
        instance::Role::Toggled => return Ok(()),
        instance::Role::Window(l) => l,
    };
    let section = args
        .iter()
        .position(|a| a == "--section")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let win = Main::new().map_err(|e| e.to_string())?;
    // After the first component exists (that creates the platform), before it is shown.
    slint::set_xdg_app_id(APP_ID).map_err(|e| e.to_string())?;
    win.global::<Th>().set_dark(theme::is_dark());
    app::start(&win, section);
    instance::serve(listener, || {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });
    win.show().map_err(|e| e.to_string())?;
    slint::run_event_loop().map_err(|e| e.to_string())
}
