//! `tspmo-gui --install-shortcut`: bind the ROG key for this user.
//!
//! The packaged files already do it system-wide (`/usr/share/applications` and
//! `/usr/share/kglobalaccel` carry `X-KDE-Shortcuts`). This writes the same entry under
//! `~/.local/share` for a build that is not installed, and has KDE pick it up.

use crate::{APP_ICON, APP_ID};
use std::path::PathBuf;

/// The ROG / Armoury key: `KEY_PROG1` (148) from hid-asus, xkb `<I156>` = `XF86Launch1`, Qt
/// `Launch (1)`. The WMI-device variant (`KEY_PROG3`) is the same physical key on other models
/// but is also the Aura key of the USB keyboard here, so it is not bound.
pub const KEY: &str = "Launch (1)";

/// The application entry: a launcher in the menu. No startup notification: the flyout is a
/// layer surface and never maps a toplevel, so KDE's launch feedback would never be satisfied
/// (bouncing cursor, a task manager placeholder that lingers).
pub fn launcher_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=tspmo\n\
         GenericName=Power and hardware control\n\
         Comment=Modes, GPU, screen, battery, fans, lighting\n\
         Exec={exec}\n\
         Icon={APP_ICON}\n\
         Terminal=false\n\
         Categories=System;Settings;HardwareSettings;\n\
         StartupWMClass={APP_ID}\n\
         StartupNotify=false\n\
         X-KDE-StartupNotify=false\n"
    )
}

/// The helper that carries the shortcut: hidden from menus, so the key is not an application
/// launch, and silent for the same reason.
pub fn toggle_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=tspmo (ROG key)\n\
         Comment=Open or close the tspmo window\n\
         Exec={exec} --toggle\n\
         Icon={APP_ICON}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         StartupNotify=false\n\
         X-KDE-StartupNotify=false\n\
         X-KDE-Shortcuts={KEY}\n"
    )
}

fn data_home() -> Result<PathBuf, String> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| "HOME is not set".to_owned())
}

pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exec = exe.to_string_lossy().into_owned();
    let base = data_home()?;
    let files = [
        (format!("{APP_ID}.desktop"), launcher_entry(&exec)),
        (format!("{APP_ID}-toggle.desktop"), toggle_entry(&exec)),
    ];
    for dir in ["applications", "kglobalaccel"] {
        let d = base.join(dir);
        std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        for (name, text) in &files {
            let f = d.join(name);
            std::fs::write(&f, text).map_err(|e| format!("{}: {e}", f.display()))?;
            println!("{}", f.display());
        }
    }
    // KDE's shortcut service reloads its components when the desktop database changes, so a
    // running session picks the key up at once. Best effort: a new login always works.
    let _ = std::process::Command::new("kbuildsycoca6")
        .arg("--noincremental")
        .stderr(std::process::Stdio::null())
        .status();
    println!("{KEY}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_runs_a_silent_hidden_helper() {
        let e = toggle_entry("/usr/bin/tspmo-gui");
        assert!(e.contains("Exec=/usr/bin/tspmo-gui --toggle\n"));
        assert!(e.contains(&format!("X-KDE-Shortcuts={KEY}\n")));
        assert!(e.contains("NoDisplay=true\n"));
        assert!(e.contains("StartupNotify=false\n") && e.contains("X-KDE-StartupNotify=false\n"));
        // The launcher carries no shortcut: only the helper does.
        let l = launcher_entry("/usr/bin/tspmo-gui");
        assert!(!l.contains("X-KDE-Shortcuts"));
        assert!(l.contains("StartupNotify=false\n") && l.contains("X-KDE-StartupNotify=false\n"));
        assert!(!l.contains("NoDisplay"));
    }

    fn packaged(name: &str) -> String {
        std::fs::read_to_string(format!("{}/../../data/applications/{name}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    #[test]
    fn packaged_entries_match() {
        let t = packaged("org.tspmo.gui-toggle.desktop");
        for line in [
            "Exec=tspmo-gui --toggle",
            "NoDisplay=true",
            "StartupNotify=false",
            "X-KDE-StartupNotify=false",
        ] {
            assert!(t.lines().any(|l| l == line), "{line}");
        }
        assert!(t.contains(&format!("X-KDE-Shortcuts={KEY}")));
        let l = packaged("org.tspmo.gui.desktop");
        assert!(!l.contains("X-KDE-Shortcuts") && !l.contains("NoDisplay"));
        assert!(l.contains("StartupNotify=false") && l.contains("X-KDE-StartupNotify=false"));
        assert!(l.contains(&format!("Icon={APP_ICON}")));
    }
}
