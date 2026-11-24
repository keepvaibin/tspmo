//! `org.tspmo.Fans1`: per-mode fan curves.

use super::thermal::{Thermal, lock, mode_from_index};
use super::{PATH, authorize};
use std::sync::Arc;
use tspmo_core::asus::fans::{Curve, Setting};
use tspmo_core::power::Profile;
use zbus::message::Header;

pub struct Fans1(pub Arc<Thermal>);

type Points = Vec<(u8, u8)>;

fn mode_arg(s: &str) -> zbus::fdo::Result<Profile> {
    Profile::parse(s).ok_or_else(|| {
        zbus::fdo::Error::InvalidArgs(format!("unknown mode '{s}' (power-saver, balanced, performance)"))
    })
}

fn invalid(e: String) -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs(e)
}

impl Fans1 {
    async fn change(
        &self,
        conn: &zbus::Connection,
        hdr: &Header<'_>,
        f: impl FnOnce(&Thermal) -> Result<(), String>,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, hdr, super::SET_FANS).await?;
        f(&self.0).map_err(invalid)?;
        changed(conn).await;
        super::power::changed(conn).await;
        Ok(())
    }
}

#[zbus::interface(name = "org.tspmo.Fans1")]
impl Fans1 {
    /// `(index, label)` of every fan with a custom curve.
    #[zbus(property)]
    fn fans(&self) -> Vec<(u32, String)> {
        self.0
            .fans
            .as_ref()
            .map(|f| f.fans.iter().map(|f| (f.index, f.label.clone())).collect())
            .unwrap_or_default()
    }

    /// The thermal mode in force.
    #[zbus(property)]
    fn mode(&self) -> String {
        lock(&self.0.state).mode.as_str().to_owned()
    }

    /// `(mode, fan, kind, points)`; kind is `auto`, `custom` or `full`. An `auto` fan lists the
    /// firmware's curve when it is known. Points are `(°C, percent)`.
    #[zbus(property)]
    fn settings(&self) -> Vec<(String, u32, String, Points)> {
        let st = lock(&self.0.state);
        let mut out = Vec::new();
        for m in 0..3 {
            for f in self.0.fans.iter().flat_map(|f| f.fans.iter()) {
                let key = (m, f.index);
                let (kind, curve) = match st.settings.get(&key) {
                    Some(Setting::Custom(c)) => ("custom", Some(c.clone())),
                    Some(Setting::Full) => ("full", Some(Curve::full())),
                    _ => ("auto", st.factory.get(&key).cloned()),
                };
                out.push((
                    mode_from_index(m).as_str().to_owned(),
                    f.index,
                    kind.to_owned(),
                    curve.map(|c| c.0.to_vec()).unwrap_or_default(),
                ));
            }
        }
        out
    }

    /// The firmware's own curves, per mode and fan, once read.
    #[zbus(property)]
    fn factory(&self) -> Vec<(String, u32, Points)> {
        lock(&self.0.state)
            .factory
            .iter()
            .map(|((m, f), c)| (mode_from_index(*m).as_str().to_owned(), *f, c.0.to_vec()))
            .collect()
    }

    /// `(fan, rpm)`, read from the firmware on each query (no change signal).
    #[zbus(property(emits_changed_signal = "false"))]
    fn rpm(&self) -> Vec<(u32, u32)> {
        let Some(fans) = &self.0.fans else {
            return Vec::new();
        };
        fans.fans
            .iter()
            .filter_map(|f| Some((f.index, fans.rpm(&self.0.root, f.index)?)))
            .collect()
    }

    /// `(°C, label)` of the CPU package, read on each query (no change signal).
    /// Never the NVIDIA GPU: reading its sensors wakes it.
    #[zbus(property(emits_changed_signal = "false"))]
    fn temps(&self) -> Vec<(u32, String)> {
        tspmo_sys::hwmon::find(&self.0.root, "coretemp")
            .and_then(|h| self.0.root.read_parse::<i32>(format!("{}/temp1_input", h.dir)))
            .map(|t| vec![((t.max(0) as u32 + 500) / 1000, "CPU".to_owned())])
            .unwrap_or_default()
    }

    /// Run a custom curve (`(°C, percent)`, eight points) on `fan` (0 = all) in `mode`.
    async fn set_curve(
        &self,
        mode: String,
        fan: u32,
        points: Points,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let mode = mode_arg(&mode)?;
        let curve = Curve::from_points(&points).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs(format!("a curve has {} points", tspmo_core::asus::fans::POINTS))
        })?;
        self.change(conn, &hdr, |t| t.set_fan(mode, fan, Setting::Custom(curve)))
            .await
    }

    /// The firmware's curve on `fan` (0 = all) in `mode`.
    async fn set_auto(
        &self,
        mode: String,
        fan: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let mode = mode_arg(&mode)?;
        self.change(conn, &hdr, |t| t.set_fan(mode, fan, Setting::Auto))
            .await
    }

    /// 100 % on `fan` (0 = all) in `mode`.
    async fn set_full(
        &self,
        mode: String,
        fan: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let mode = mode_arg(&mode)?;
        self.change(conn, &hdr, |t| t.set_fan(mode, fan, Setting::Full))
            .await
    }

    /// Firmware defaults: back to the firmware curve and re-read it.
    async fn reset(
        &self,
        mode: String,
        fan: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let mode = mode_arg(&mode)?;
        self.change(conn, &hdr, |t| t.reset_fan(mode, fan)).await
    }
}

/// Emit `PropertiesChanged` for what applying a mode can change.
pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Fans1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.mode_changed(e).await;
    let _ = i.settings_changed(e).await;
    let _ = i.factory_changed(e).await;
}
