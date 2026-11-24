//! `org.tspmo.Hardware1`: firmware toggles (panel overdrive, boot sound, ...), driven by what
//! `asus-armoury` says each attribute is.

use super::thermal::lock;
use super::{PATH, authorize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tspmo_core::asus::hardware::{self, Item};
use tspmo_sys::SysRoot;
use zbus::message::Header;

pub struct Hardware {
    root: SysRoot,
    file: PathBuf,
    st: Mutex<State>,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    reboot: bool,
    /// What the user chose; written again at start and after resume (some firmware forgets).
    chosen: BTreeMap<String, String>,
}

impl Hardware {
    /// `None` if the firmware exposes nothing to toggle.
    pub fn new(root: SysRoot, dir: PathBuf) -> Option<Self> {
        let items = hardware::discover(&root);
        if items.is_empty() {
            return None;
        }
        let h = Self {
            file: dir.join("hardware"),
            st: Mutex::new(State {
                reboot: hardware::reboot_pending(&root),
                items,
                chosen: BTreeMap::new(),
            }),
            root,
        };
        if let Ok(text) = std::fs::read_to_string(&h.file) {
            let mut st = lock(&h.st);
            for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
                st.chosen.insert(k.trim().to_owned(), v.trim().to_owned());
            }
        }
        Some(h)
    }

    fn save(&self, st: &State) {
        let mut t = String::from("# tspmod firmware toggles (managed by tspmod)\n");
        for (k, v) in &st.chosen {
            t.push_str(&format!("{k}={v}\n"));
        }
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    fn refresh(&self) {
        let items = hardware::discover(&self.root);
        let reboot = hardware::reboot_pending(&self.root);
        let mut st = lock(&self.st);
        st.items = items;
        st.reboot = reboot;
    }

    /// Write the user's choices (start-up, resume). Only differing values are written.
    pub fn reapply(&self) {
        let chosen = lock(&self.st).chosen.clone();
        for (k, v) in chosen {
            if let Err(e) = hardware::set(&self.root, &k, &v) {
                eprintln!("tspmod: {e}");
            }
        }
        self.refresh();
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), String> {
        hardware::set(&self.root, name, value)?;
        {
            let mut st = lock(&self.st);
            st.chosen.insert(name.to_owned(), value.trim().to_owned());
            self.save(&st);
        }
        self.refresh();
        Ok(())
    }
}

pub struct Hardware1(pub Arc<Hardware>);

type Row = (String, String, String, String, Vec<String>, i64, i64, bool);

#[zbus::interface(name = "org.tspmo.Hardware1")]
impl Hardware1 {
    /// `(name, label, kind, current, values, min, max, writable)`. Kind is `toggle` (0/1),
    /// `choice` (one of `values`), `number` (`min`..=`max`) or `text`.
    #[zbus(property)]
    fn items(&self) -> Vec<Row> {
        lock(&self.0.st)
            .items
            .iter()
            .map(|i| {
                (
                    i.name.clone(),
                    i.label.clone(),
                    i.kind.as_str().to_owned(),
                    i.current.clone(),
                    i.values.clone(),
                    i.min,
                    i.max,
                    i.writable,
                )
            })
            .collect()
    }

    /// A firmware change only takes effect after a reboot.
    #[zbus(property)]
    fn reboot_pending(&self) -> bool {
        lock(&self.0.st).reboot
    }

    async fn set(
        &self,
        name: String,
        value: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_HARDWARE).await?;
        self.0.set(&name, &value).map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }
}

pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Hardware1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.items_changed(e).await;
    let _ = i.reboot_pending_changed(e).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    const OD: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/panel_overdrive/current_value";

    #[test]
    fn set_is_remembered_and_reapplied() {
        let fx = Fix::new("hw-iface");
        let h = Hardware::new(fx.root(), fx.dir().join("state")).unwrap();
        h.set("panel_overdrive", "0").unwrap();
        assert_eq!(fx.get(OD), "0");
        assert!(h.set("panel_overdrive", "7").is_err());
        assert!(h.set("gpu_mux_mode", "0").is_err());
        // The firmware forgets across a reboot; a new daemon writes the choice again.
        fx.set(OD, "1\n");
        let h = Hardware::new(fx.root(), fx.dir().join("state")).unwrap();
        h.reapply();
        assert_eq!(fx.get(OD), "0");
        let row = lock(&h.st)
            .items
            .iter()
            .find(|i| i.name == "panel_overdrive")
            .unwrap()
            .current
            .clone();
        assert_eq!(row, "0");
    }

    #[test]
    fn nothing_chosen_nothing_written() {
        let fx = Fix::new("hw-untouched");
        let h = Hardware::new(fx.root(), fx.dir().join("state")).unwrap();
        fx.set(OD, "0\n");
        h.reapply();
        assert_eq!(fx.get(OD), "0");
    }
}
