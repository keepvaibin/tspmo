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

pub fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=tspmo\n\
         GenericName=Power and hardware control\n\
         Comment=Modes, GPU, screen, battery, fans, lighting\n\
         Exec={exec} --toggle\n\
         Icon={APP_ICON}\n\
         Terminal=false\n\
         Categories=System;Settings;HardwareSettings;\n\
         StartupWMClass={APP_ID}\n\
         X-KDE-Shortcuts={KEY}\n\
         Actions=Toggle;\n\
         \n\
         [Desktop Action Toggle]\n\
         Name=Toggle\n\
         Exec={exec} --toggle\n"
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
    let text = desktop_entry(&exec);
    let base = data_home()?;
    for dir in ["applications", "kglobalaccel"] {
        let d = base.join(dir);
        std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        let f = d.join(format!("{APP_ID}.desktop"));
        std::fs::write(&f, &text).map_err(|e| format!("{}: {e}", f.display()))?;
        println!("{}", f.display());
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
    fn entry_binds_the_key_and_toggles() {
        let e = desktop_entry("/usr/bin/tspmo-gui");
        assert!(e.contains("Exec=/usr/bin/tspmo-gui --toggle\n"));
        assert!(e.contains(&format!("X-KDE-Shortcuts={KEY}\n")));
        assert!(e.contains(&format!("Icon={APP_ICON}\n")));
    }

    #[test]
    fn packaged_entry_matches() {
        let packaged = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/applications/org.tspmo.gui.desktop"
        ))
        .expect("data/applications/org.tspmo.gui.desktop");
        assert!(packaged.contains(&format!("X-KDE-Shortcuts={KEY}")));
        assert!(packaged.contains(&format!("Icon={APP_ICON}")));
        assert!(packaged.contains("Exec=tspmo-gui --toggle"));
    }
}
