//! Talking to tspmod over the system bus.

use std::collections::HashMap;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const NAME: &str = "org.tspmo.Daemon1";
const PATH: &str = "/org/tspmo";

fn connect() -> Result<Connection, String> {
    let conn = match std::env::var("TSPMOD_BUS").as_deref() {
        Ok("session") => Connection::session(),
        _ => Connection::system(),
    };
    conn.map_err(|e| format!("can't reach the system bus: {e}"))
}

fn proxy(conn: &Connection) -> Result<Proxy<'_>, String> {
    Proxy::new(conn, NAME, PATH, NAME).map_err(|e| e.to_string())
}

fn explain(e: zbus::Error) -> String {
    let s = e.to_string();
    if s.contains("ServiceUnknown") || s.contains("NameHasNoOwner") || s.contains("was not provided") {
        "tspmod isn't running (start it with: sudo systemctl enable --now tspmod)".into()
    } else {
        // "org.freedesktop.DBus.Error.Failed: message" -> "message"
        match s.split_once(": ") {
            Some((kind, msg)) if kind.contains(".Error.") => msg.to_owned(),
            _ => s,
        }
    }
}

pub fn state() -> Result<HashMap<String, OwnedValue>, String> {
    let conn = connect()?;
    proxy(&conn)?.call("GetState", &()).map_err(explain)
}

pub fn get_str(state: &HashMap<String, OwnedValue>, key: &str) -> String {
    state
        .get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

pub fn get_bool(state: &HashMap<String, OwnedValue>, key: &str) -> bool {
    state
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

pub fn set_profile(p: &str) -> Result<(), String> {
    let conn = connect()?;
    proxy(&conn)?
        .call::<_, _, ()>("SetProfile", &(p,))
        .map_err(explain)
}

pub fn cycle_profile() -> Result<String, String> {
    let conn = connect()?;
    proxy(&conn)?.call("CycleProfile", &()).map_err(explain)
}

pub fn set_gpu(mode: &str) -> Result<String, String> {
    let conn = connect()?;
    proxy(&conn)?.call("SetGpuMode", &(mode,)).map_err(explain)
}

pub fn gpu_users() -> Result<Vec<(u32, String, String)>, String> {
    let conn = connect()?;
    proxy(&conn)?.call("GpuUsers", &()).map_err(explain)
}
