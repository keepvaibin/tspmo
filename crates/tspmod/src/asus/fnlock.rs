//! Software Fn-lock: writes the hwdb keymap, rebuilds hwdb.bin, re-applies it to the keyboard.
//! The hwdb file is the state; nothing runs while idle.

use std::process::Command;
use tspmo_core::asus::fnlock::{self, HWDB_FILE};
use tspmo_sys::SysRoot;

const HWDB_BIN: &str = "/etc/udev/hwdb.bin";
const INPUT: &str = "/sys/class/input";

/// The product id of a measured ASUS USB keyboard, and its evdev nodes.
pub fn find(root: &SysRoot) -> Option<(u16, Vec<String>)> {
    let mut product = None;
    let mut nodes = Vec::new();
    for n in root.list(INPUT).into_iter().filter(|n| n.starts_with("event")) {
        let id = |f: &str| root.read_opt(format!("{INPUT}/{n}/device/id/{f}"));
        if id("bustype").as_deref() != Some("0003") || id("vendor").as_deref() != Some("0b05") {
            continue;
        }
        let Some(p) = id("product").and_then(|p| u16::from_str_radix(&p, 16).ok()) else {
            continue;
        };
        if fnlock::KNOWN_PRODUCTS.contains(&p) {
            product = Some(p);
            nodes.push(format!("{INPUT}/{n}"));
        }
    }
    product.map(|p| (p, nodes))
}

/// Fn-lock in force (the media layout is installed).
pub fn state() -> bool {
    std::fs::read_to_string(HWDB_FILE)
        .ok()
        .and_then(|t| fnlock::parse_state(&t))
        .unwrap_or(false)
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn apply_layout(product: u16, nodes: &[String], on: bool) -> Result<(), String> {
    std::fs::write(HWDB_FILE, fnlock::hwdb(product, on)).map_err(|e| format!("{HWDB_FILE}: {e}"))?;
    run("systemd-hwdb", &["update"])?;
    let mut args = vec!["trigger", "--action=change", "--settle"];
    args.extend(nodes.iter().map(String::as_str));
    run("udevadm", &args)
}

/// Switch Fn-lock. Turning it off restores the native keys explicitly (the kernel keeps a
/// keymap until the device re-enumerates), then removes our files.
pub fn set(root: &SysRoot, on: bool) -> Result<(), String> {
    let (product, nodes) = find(root).ok_or("no supported keyboard")?;
    apply_layout(product, &nodes, on)?;
    if !on {
        let _ = std::fs::remove_file(HWDB_FILE);
        // hwdb.bin in /etc shadows the system one (which package updates rebuild): drop it
        // when nothing else needs it.
        let others = std::fs::read_dir("/etc/udev/hwdb.d")
            .map(|d| {
                d.flatten()
                    .any(|e| e.file_name().to_string_lossy().ends_with(".hwdb"))
            })
            .unwrap_or(false);
        if others {
            run("systemd-hwdb", &["update"])?;
        } else {
            let _ = std::fs::remove_file(HWDB_BIN);
        }
    }
    Ok(())
}

/// At start: if Fn-lock is on, make sure hwdb.bin is not older than the system one
/// (a systemd update rebuilds only the system copy).
pub fn refresh_if_stale() {
    if !state() {
        return;
    }
    let m = |p: &str| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let stale = match (m(HWDB_BIN), m("/usr/lib/udev/hwdb.bin")) {
        (None, _) => true,
        (Some(e), Some(u)) => e < u,
        _ => false,
    };
    if stale {
        let _ = run("systemd-hwdb", &["update"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    #[test]
    fn finds_only_measured_keyboards() {
        let fx = Fix::new("fnlock-find");
        let d = fx.dir().join("sys/class/input/event7/device/id");
        std::fs::create_dir_all(&d).unwrap();
        for (f, v) in [("bustype", "0003"), ("vendor", "0b05"), ("product", "19b6")] {
            std::fs::write(d.join(f), format!("{v}\n")).unwrap();
        }
        let (p, nodes) = find(&fx.root()).unwrap();
        assert_eq!(p, 0x19b6);
        assert_eq!(nodes.len(), 1);
        std::fs::write(d.join("product"), "1866\n").unwrap();
        assert!(find(&fx.root()).is_none());
    }
}
