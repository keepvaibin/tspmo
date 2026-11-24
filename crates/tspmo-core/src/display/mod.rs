//! Display control: refresh rate, flicker-free dimming and colour for the laptop's panel.
//!
//! Everything here is pure (parsing, policy, colour maths) so it is tested without a desktop;
//! the daemon (`tspmod`) runs the compositor commands in the user's session.

pub mod dimming;
pub mod edid;
pub mod icc;
pub mod ini;
pub mod kscreen;
pub mod refresh;

use tspmo_sys::SysRoot;

/// The laptop's own panel, from the kernel's DRM connectors.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    /// Connector name as the compositor calls it (`eDP-1`).
    pub connector: String,
    pub edid: Option<edid::Edid>,
}

/// The connected internal connector (`eDP`, `LVDS`, `DSI`) and its EDID.
pub fn internal_panel(root: &SysRoot) -> Option<Panel> {
    for dir in root.glob("/sys/class/drm/card*-*") {
        let name = dir.rsplit('/').next()?;
        let connector = name.split_once('-')?.1;
        if !["eDP", "LVDS", "DSI"].iter().any(|p| connector.starts_with(p)) {
            continue;
        }
        if root.read_opt(format!("{dir}/status")).as_deref() != Some("connected") {
            continue;
        }
        let edid = std::fs::read(root.path(format!("{dir}/edid")))
            .ok()
            .and_then(|b| edid::parse(&b));
        return Some(Panel {
            connector: connector.to_owned(),
            edid,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gu605_fixture_panel_is_an_oled() {
        let root = SysRoot::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/asus-gu605mi"
        ));
        let p = internal_panel(&root).unwrap();
        assert_eq!(p.connector, "eDP-1");
        let e = p.edid.unwrap();
        assert_eq!(e.name, "ATNA60DL01-0");
        assert!(edid::is_oled(Some(&e), ""));
        assert!(e.primaries.is_some());
    }

    #[test]
    fn finds_the_connected_internal_panel() {
        let d = std::env::temp_dir().join(format!("tspmo-drm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let drm = d.join("sys/class/drm");
        for (n, status) in [
            ("card0-eDP-2", "disconnected"),
            ("card1-DP-1", "connected"),
            ("card1-eDP-1", "connected"),
        ] {
            std::fs::create_dir_all(drm.join(n)).unwrap();
            std::fs::write(drm.join(n).join("status"), format!("{status}\n")).unwrap();
        }
        std::fs::write(drm.join("card1-eDP-1/edid"), edid::tests::gu605_edid()).unwrap();
        let p = internal_panel(&SysRoot::new(&d)).unwrap();
        assert_eq!(p.connector, "eDP-1");
        assert_eq!(p.edid.unwrap().name, "ATNA60DL01-0");
        let _ = std::fs::remove_dir_all(&d);
    }
}
