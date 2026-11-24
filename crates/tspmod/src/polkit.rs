//! Authorisation through polkit, as power-profiles-daemon does it.

use std::collections::HashMap;
use zbus::zvariant::Value;

/// Polkit action ids. The power-profiles ones are the standard ids KDE/GNOME expect.
pub const SWITCH_PROFILE: &str = "org.freedesktop.UPower.PowerProfiles.switch-profile";
pub const HOLD_PROFILE: &str = "org.freedesktop.UPower.PowerProfiles.hold-profile";
pub const SET_GPU: &str = "org.tspmo.set-gpu";
pub const SET_DISPLAY: &str = "org.tspmo.set-display";

/// `Ok(())` if the caller may perform `action`. Root is always allowed.
pub async fn check(conn: &zbus::Connection, sender: Option<&str>, action: &str) -> zbus::fdo::Result<()> {
    let Some(sender) = sender else {
        return Err(zbus::fdo::Error::AccessDenied("no sender".into()));
    };
    // Development mode (session bus, fake sysroot): there is no polkit on the session bus.
    if std::env::var("TSPMOD_BUS").as_deref() == Ok("session") {
        return Ok(());
    }
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    if let Ok(name) = zbus::names::BusName::try_from(sender) {
        if dbus
            .get_connection_unix_user(name)
            .await
            .is_ok_and(|uid| uid == 0)
        {
            return Ok(());
        }
    }
    let authority = zbus::Proxy::new(
        conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await?;
    let mut subject_details: HashMap<&str, Value<'_>> = HashMap::new();
    subject_details.insert("name", Value::from(sender));
    let subject = ("system-bus-name", subject_details);
    let details: HashMap<&str, &str> = HashMap::new();
    const ALLOW_USER_INTERACTION: u32 = 1;
    let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) = authority
        .call(
            "CheckAuthorization",
            &(subject, action, details, ALLOW_USER_INTERACTION, ""),
        )
        .await
        .map_err(|e| zbus::fdo::Error::AccessDenied(format!("polkit: {e}")))?;
    if authorized {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(format!(
            "not authorized for {action}"
        )))
    }
}
