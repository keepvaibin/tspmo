//! `org.tspmo.Power1`: power limits per mode and power source.

use super::thermal::{Thermal, lock, mode_from_index};
use super::{PATH, authorize};
use std::sync::Arc;
use tspmo_core::power::Profile;
use zbus::message::Header;

pub struct Power1(pub Arc<Thermal>);

fn source_arg(s: &str) -> zbus::fdo::Result<bool> {
    match s {
        "ac" => Ok(false),
        "battery" => Ok(true),
        other => Err(zbus::fdo::Error::InvalidArgs(format!(
            "unknown power source '{other}' (ac, battery)"
        ))),
    }
}

fn mode_arg(s: &str) -> zbus::fdo::Result<Profile> {
    Profile::parse(s).ok_or_else(|| {
        zbus::fdo::Error::InvalidArgs(format!("unknown mode '{s}' (power-saver, balanced, performance)"))
    })
}

#[zbus::interface(name = "org.tspmo.Power1")]
impl Power1 {
    /// `(name, label, unit, min, max, default, current)` for each limit the firmware has.
    /// Bounds are those of the power source in use. `current` is the last value written.
    #[zbus(property)]
    fn limits(&self) -> Vec<(String, String, String, i64, i64, i64, i64)> {
        lock(&self.0.state)
            .limits
            .iter()
            .map(|l| {
                (
                    l.name.into(),
                    l.label.into(),
                    l.unit.into(),
                    l.min,
                    l.max,
                    l.default,
                    l.current,
                )
            })
            .collect()
    }

    /// `(mode, source, name, value)` for every custom limit. Anything absent is the firmware default.
    #[zbus(property)]
    fn custom(&self) -> Vec<(String, String, String, i64)> {
        lock(&self.0.state)
            .custom
            .iter()
            .map(|((m, bat, name), v)| {
                (
                    mode_from_index(*m).as_str().to_owned(),
                    super::thermal::source_name(*bat).to_owned(),
                    name.clone(),
                    *v,
                )
            })
            .collect()
    }

    #[zbus(property)]
    fn mode(&self) -> String {
        lock(&self.0.state).mode.as_str().to_owned()
    }

    #[zbus(property)]
    fn source(&self) -> String {
        super::thermal::source_name(lock(&self.0.state).on_battery).to_owned()
    }

    /// Custom limits need a custom fan curve to be active on this model (applied automatically).
    #[zbus(property)]
    fn needs_fan_curve(&self) -> bool {
        self.0.quirks.power_needs_fan_curve
    }

    /// Set limit `name` for `mode` on `source` (`ac` or `battery`). A negative value restores the
    /// firmware default.
    async fn set_limit(
        &self,
        mode: String,
        source: String,
        name: String,
        value: i64,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let (mode, bat) = (mode_arg(&mode)?, source_arg(&source)?);
        authorize(conn, &hdr, super::SET_POWER).await?;
        self.0
            .set_limit(mode, bat, &name, (value >= 0).then_some(value))
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        super::fans::changed(conn).await;
        Ok(())
    }

    /// Firmware defaults for every limit of `mode`.
    async fn reset(
        &self,
        mode: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let mode = mode_arg(&mode)?;
        authorize(conn, &hdr, super::SET_POWER).await?;
        self.0.reset_limits(mode);
        changed(conn).await;
        super::fans::changed(conn).await;
        Ok(())
    }
}

pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Power1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.limits_changed(e).await;
    let _ = i.custom_changed(e).await;
    let _ = i.mode_changed(e).await;
    let _ = i.source_changed(e).await;
}
