//! `org.tspmo.Display1` at `/org/tspmo`.

use super::{Shared, State, Tri, color, flicker, lock};
use crate::polkit;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tspmo_core::display::refresh::Policy;
use zbus::message::Header;
use zbus::zvariant::{OwnedValue, Value};

pub const NAME: &str = "org.tspmo.Display1";
pub const PATH: &str = "/org/tspmo";

fn ov(v: impl Into<Value<'static>>) -> OwnedValue {
    v.into().try_into().expect("plain values always convert")
}

/// Every property, for `GetState` and `PropertiesChanged`.
pub fn props(s: &State) -> Vec<(&'static str, OwnedValue)> {
    let (backlight, dimming) = flicker::current_split(s);
    vec![
        ("Connector", ov(s.connector().unwrap_or_default().to_owned())),
        ("Oled", ov(s.oled)),
        ("Rates", ov(s.rates.clone())),
        ("Refresh", ov(s.hz)),
        ("RefreshPolicy", ov(s.policy.as_str())),
        ("FlickerFree", ov(s.flicker_free())),
        ("FlickerSetting", ov(s.flicker.as_str())),
        ("FlickerFloor", ov(s.floor_pct)),
        ("Brightness", ov(s.flicker_state.level.round() as u32)),
        ("Backlight", ov(backlight.round() as u32)),
        ("Dimming", ov(dimming.round() as u32)),
        ("ColorMode", ov(s.color.mode.as_str())),
        ("ColorModes", ov(color::available(s))),
        ("EyeCare", ov(s.color.eyecare_k)),
        ("Message", ov(s.message.clone())),
    ]
}

pub async fn emit_changed(conn: &zbus::Connection, shared: &Shared) {
    let p = props(&lock(shared));
    let Ok(emitter) = zbus::object_server::SignalEmitter::new(conn, PATH) else {
        return;
    };
    let changed: HashMap<&str, Value<'_>> = p.iter().map(|(k, v)| (*k, Value::from(v.clone()))).collect();
    let _ = zbus::fdo::Properties::properties_changed(
        &emitter,
        zbus::names::InterfaceName::from_static_str_unchecked(NAME),
        changed,
        Cow::Borrowed(&[]),
    )
    .await;
}

pub fn emit_changed_blocking(conn: &zbus::Connection, shared: &Shared) {
    zbus::block_on(emit_changed(conn, shared));
}

pub struct Display1(pub Shared);

async fn authorize(hdr: &Header<'_>, conn: &zbus::Connection) -> zbus::fdo::Result<()> {
    let sender = hdr.sender().map(|s| s.to_string());
    polkit::check(conn, sender.as_deref(), polkit::SET_DISPLAY).await
}

fn failed(msg: String) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(msg)
}

/// Run blocking compositor work on its own thread and await it, so the D-Bus executor keeps
/// answering other callers (power-profile queries must stay fast while a mode switch runs).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    struct Slot<T>(Mutex<(Option<T>, Option<Waker>)>);
    struct Wait<T>(Arc<Slot<T>>);
    impl<T> Future for Wait<T> {
        type Output = T;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            let mut g = (self.0).0.lock().unwrap_or_else(|p| p.into_inner());
            match g.0.take() {
                Some(v) => Poll::Ready(v),
                None => {
                    g.1 = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }

    let slot = Arc::new(Slot(Mutex::new((None, None))));
    let theirs = Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name("display-op".into())
        .spawn(move || {
            let v = f();
            let mut g = theirs.0.lock().unwrap_or_else(|p| p.into_inner());
            g.0 = Some(v);
            if let Some(w) = g.1.take() {
                w.wake();
            }
        });
    if spawned.is_err() {
        return Err("busy; try again".into());
    }
    Wait(slot).await
}

#[zbus::interface(name = "org.tspmo.Display1")]
impl Display1 {
    fn get_state(&self) -> HashMap<String, OwnedValue> {
        props(&lock(&self.0))
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect()
    }

    #[zbus(property)]
    fn connector(&self) -> String {
        lock(&self.0).connector().unwrap_or_default().to_owned()
    }
    #[zbus(property)]
    fn oled(&self) -> bool {
        lock(&self.0).oled
    }
    #[zbus(property)]
    fn rates(&self) -> Vec<u32> {
        lock(&self.0).rates.clone()
    }
    #[zbus(property)]
    fn refresh(&self) -> f64 {
        lock(&self.0).hz
    }
    #[zbus(property)]
    fn refresh_policy(&self) -> String {
        lock(&self.0).policy.as_str().to_owned()
    }
    #[zbus(property)]
    fn flicker_free(&self) -> bool {
        lock(&self.0).flicker_free()
    }
    #[zbus(property)]
    fn flicker_setting(&self) -> String {
        lock(&self.0).flicker.as_str().to_owned()
    }
    #[zbus(property)]
    fn flicker_floor(&self) -> u32 {
        lock(&self.0).floor_pct
    }
    #[zbus(property)]
    fn brightness(&self) -> u32 {
        lock(&self.0).flicker_state.level.round() as u32
    }
    #[zbus(property)]
    fn backlight(&self) -> u32 {
        flicker::current_split(&lock(&self.0)).0.round() as u32
    }
    #[zbus(property)]
    fn dimming(&self) -> u32 {
        flicker::current_split(&lock(&self.0)).1.round() as u32
    }
    #[zbus(property)]
    fn color_mode(&self) -> String {
        lock(&self.0).color.mode.as_str().to_owned()
    }
    #[zbus(property)]
    fn color_modes(&self) -> Vec<String> {
        color::available(&lock(&self.0))
    }
    #[zbus(property)]
    fn eye_care(&self) -> u32 {
        lock(&self.0).color.eyecare_k
    }
    #[zbus(property)]
    fn message(&self) -> String {
        lock(&self.0).message.clone()
    }

    /// `max`, `60` or `auto` (60 Hz on battery, the highest rate on AC; Eco+ is always 60).
    async fn set_refresh_policy(
        &self,
        policy: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        let p = Policy::parse(&policy).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs(format!("unknown refresh policy '{policy}' (max, 60, auto)"))
        })?;
        authorize(&hdr, conn).await?;
        let shared = self.0.clone();
        let r = blocking(move || {
            {
                let mut s = lock(&shared);
                s.policy = p;
                s.save();
            }
            super::events::exclusive(|| super::apply_refresh(&shared))
        })
        .await;
        emit_changed(conn, &self.0).await;
        r.map_err(failed)?;
        let s = lock(&self.0);
        Ok(format!("Refresh {} ({} Hz)", s.policy.as_str(), s.hz.round()))
    }

    /// `auto` (on for OLED), `on` or `off`.
    async fn set_flicker_free(
        &self,
        setting: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        let t = Tri::parse(&setting).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs(format!("unknown setting '{setting}' (auto, on, off)"))
        })?;
        authorize(&hdr, conn).await?;
        let shared = self.0.clone();
        let r = blocking(move || super::events::exclusive(|| flicker::set_mode(&shared, t))).await;
        emit_changed(conn, &self.0).await;
        r.map_err(failed)
    }

    /// Lowest panel brightness in percent that stays flicker-free.
    async fn set_flicker_floor(
        &self,
        percent: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        if !(1..=100).contains(&percent) {
            return Err(zbus::fdo::Error::InvalidArgs("floor is 1 to 100".into()));
        }
        authorize(&hdr, conn).await?;
        let shared = self.0.clone();
        let r = blocking(move || super::events::exclusive(|| flicker::set_floor(&shared, percent))).await;
        emit_changed(conn, &self.0).await;
        r.map_err(failed)
    }

    /// Screen brightness 1..=100, split into backlight and software dimming when flicker-free
    /// dimming is on.
    async fn set_brightness(
        &self,
        percent: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(&hdr, conn).await?;
        if !(1..=100).contains(&percent) {
            return Err(zbus::fdo::Error::InvalidArgs("brightness is 1-100".into()));
        }
        let shared = self.0.clone();
        let r =
            blocking(move || super::events::exclusive(|| flicker::set_level(&shared, f64::from(percent))))
                .await;
        emit_changed(conn, &self.0).await;
        r.map_err(failed)
    }

    /// One of `ColorModes`.
    async fn set_color_mode(
        &self,
        mode: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        authorize(&hdr, conn).await?;
        let shared = self.0.clone();
        let r = blocking(move || super::events::exclusive(|| color::set_mode(&shared, &mode))).await;
        emit_changed(conn, &self.0).await;
        r.map_err(failed)
    }

    /// Warm white point in kelvin (1000..=6500); 0 turns eye care off.
    async fn set_eye_care(
        &self,
        kelvin: u32,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<String> {
        if kelvin != 0 && !(1000..=6500).contains(&kelvin) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "temperature is 1000 to 6500 K, or 0 for off".into(),
            ));
        }
        authorize(&hdr, conn).await?;
        let shared = self.0.clone();
        let r = blocking(move || super::events::exclusive(|| color::set_eyecare(&shared, kelvin))).await;
        emit_changed(conn, &self.0).await;
        r.map_err(failed)
    }
}
