//! D-Bus: one connection to the system bus, all work off the UI thread.
//!
//! Start-up: the window is already on screen. A thread connects, subscribes to
//! `PropertiesChanged` (before reading anything, so no change is missed), reads every interface
//! in parallel with `GetAll` and introspects the object for method signatures. Interfaces the
//! machine lacks simply fail and stay hidden. After that the thread only waits for signals.
//! Commands run on their own short threads; nothing polls.

use crate::value::V;
use futures_lite::StreamExt;
use std::collections::HashMap;
use std::sync::OnceLock;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};
use zbus::{Connection, MatchRule, MessageStream};

pub const SERVICE: &str = "org.tspmo.Daemon1";
pub const PATH: &str = "/org/tspmo";
const PREFIX: &str = "org.tspmo.";

/// Every interface the GUI knows, by short name.
pub const IFACES: [&str; 8] = [
    "Daemon1",
    "Display1",
    "Fans1",
    "Power1",
    "Battery1",
    "Hardware1",
    "Lighting1",
    "Lid1",
];

pub enum Event {
    /// Properties of one interface (all of them on first read, the changed ones after).
    Props(String, Vec<(String, V)>),
    /// Input signatures of every method: `"Display1.SetFlickerFree"` → `["s"]`.
    Methods(HashMap<String, Vec<String>>),
    /// The first read of every interface is over.
    Ready,
    Offline(String),
    GpuUsers(Vec<(u32, String, String)>),
    Error(String),
}

pub struct Call {
    pub iface: &'static str,
    pub method: String,
    pub args: Vec<Value<'static>>,
}

impl Call {
    pub fn new(iface: &'static str, method: &str, args: Vec<Value<'static>>) -> Call {
        Call {
            iface,
            method: method.to_owned(),
            args,
        }
    }
}

static CONN: OnceLock<Connection> = OnceLock::new();

fn connect() -> zbus::Result<Connection> {
    if let Some(c) = CONN.get() {
        return Ok(c.clone());
    }
    let c = match std::env::var("TSPMOD_BUS").as_deref() {
        Ok("session") => zbus::block_on(zbus::connection::Builder::session()?.build())?,
        _ => zbus::block_on(zbus::connection::Builder::system()?.build())?,
    };
    Ok(CONN.get_or_init(|| c).clone())
}

/// Turn a D-Bus error into the one short line shown to the user.
pub fn explain(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, desc, _) => {
            let name = name.as_str();
            if name.ends_with("AccessDenied") || name.ends_with("NotAuthorized") {
                "Not authorized".into()
            } else if name.ends_with("ServiceUnknown") || name.ends_with("NameHasNoOwner") {
                "tspmod is not running".into()
            } else if let Some(d) = desc.as_deref().filter(|d| !d.is_empty()) {
                d.to_owned()
            } else {
                name.rsplit('.').next().unwrap_or(name).to_owned()
            }
        }
        zbus::Error::InputOutput(_) => "System bus unavailable".into(),
        other => other.to_string(),
    }
}

/// Start the bus thread. `emit` delivers events to the UI thread.
pub fn start(emit: impl Fn(Event) + Send + Sync + 'static) {
    std::thread::Builder::new()
        .name("bus".into())
        .spawn(move || {
            if let Err(e) = zbus::block_on(run(&emit)) {
                emit(Event::Offline(explain(&e)));
            }
        })
        .ok();
}

async fn run(emit: &(impl Fn(Event) + Send + Sync + 'static)) -> zbus::Result<()> {
    let conn = connect()?;
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SERVICE)?
        .path(PATH)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &conn, Some(64)).await?;

    // Read everything at once: one task per interface plus the introspection.
    let (tx, rx) = std::sync::mpsc::channel::<(String, Result<Vec<(String, V)>, zbus::Error>)>();
    for name in IFACES {
        let conn = conn.clone();
        let tx = tx.clone();
        conn.clone()
            .executor()
            .spawn(
                async move {
                    let r = get_all(&conn, name).await;
                    let _ = tx.send((name.to_owned(), r));
                },
                "getall",
            )
            .detach();
    }
    let intro = {
        let conn = conn.clone();
        conn.clone()
            .executor()
            .spawn(async move { introspect(&conn).await }, "introspect")
    };
    drop(tx);

    // Deliver each interface as soon as it arrives, from this thread, while the others finish.
    let mut any = false;
    let mut daemon_err = None;
    while let Ok((name, r)) = rx.recv() {
        match r {
            Ok(props) => {
                any = true;
                emit(Event::Props(name, props));
            }
            Err(e) if name == "Daemon1" => daemon_err = Some(e),
            Err(_) => {}
        }
    }
    if let Ok(Ok(m)) = intro.await {
        emit(Event::Methods(m));
    }
    if !any {
        let msg = daemon_err.map_or_else(|| "tspmod is not running".into(), |e| explain(&e));
        emit(Event::Offline(msg));
    } else {
        emit(Event::Ready);
    }

    while let Some(Ok(msg)) = stream.next().await {
        let Ok((iface, changed, invalid)) = msg
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            continue;
        };
        let Some(short) = iface.strip_prefix(PREFIX) else {
            continue;
        };
        let short = short.to_owned();
        let mut props: Vec<(String, V)> = changed
            .iter()
            .map(|(k, v)| (k.clone(), V::from_value(v)))
            .collect();
        if !invalid.is_empty()
            && let Ok(all) = get_all(&conn, IFACES.iter().find(|n| **n == short).unwrap_or(&"")).await
        {
            props = all;
        }
        emit(Event::Props(short, props));
    }
    Ok(())
}

async fn get_all(conn: &Connection, short: &str) -> zbus::Result<Vec<(String, V)>> {
    let iface = format!("{PREFIX}{short}");
    let reply = conn
        .call_method(
            Some(SERVICE),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(iface.as_str(),),
        )
        .await?;
    let map: HashMap<String, OwnedValue> = reply.body().deserialize()?;
    Ok(map.iter().map(|(k, v)| (k.clone(), V::from_value(v))).collect())
}

async fn introspect(conn: &Connection) -> zbus::Result<HashMap<String, Vec<String>>> {
    let reply = conn
        .call_method(
            Some(SERVICE),
            PATH,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .await?;
    let xml: String = reply.body().deserialize()?;
    Ok(parse_methods(&xml))
}

/// `"Iface.Method"` → input signatures, from introspection XML. Only `org.tspmo.*` interfaces.
pub fn parse_methods(xml: &str) -> HashMap<String, Vec<String>> {
    fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let key = format!("{name}=\"");
        let start = tag.find(&key)? + key.len();
        let end = tag[start..].find('"')? + start;
        Some(&tag[start..end])
    }
    let mut out = HashMap::new();
    let mut iface = String::new();
    let mut method = String::new();
    for tag in xml.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or("");
        if tag.starts_with("interface ") {
            iface = attr(tag, "name")
                .and_then(|n| n.strip_prefix(PREFIX))
                .unwrap_or("")
                .to_owned();
        } else if tag.starts_with("method ") && !iface.is_empty() {
            method = format!("{iface}.{}", attr(tag, "name").unwrap_or(""));
            out.entry(method.clone()).or_insert_with(Vec::new);
        } else if tag.starts_with("/method") {
            method.clear();
        } else if tag.starts_with("arg ") && !method.is_empty() {
            let dir = attr(tag, "direction").unwrap_or("in");
            if dir == "in"
                && let (Some(sig), Some(v)) = (attr(tag, "type"), out.get_mut(&method))
            {
                v.push(sig.to_owned());
            }
        }
    }
    out
}

/// Run a method call on its own thread and report failures. The daemon's own
/// `PropertiesChanged` brings the new state back; nothing is applied optimistically here.
pub fn call(c: Call, emit: impl Fn(Event) + Send + 'static) {
    std::thread::Builder::new()
        .name("call".into())
        .spawn(move || match run_call(&c) {
            Ok(Some(ev)) => emit(ev),
            Ok(None) => {}
            Err(e) => emit(Event::Error(explain(&e))),
        })
        .ok();
}

fn run_call(c: &Call) -> zbus::Result<Option<Event>> {
    let conn = connect()?;
    let iface = format!("{PREFIX}{}", c.iface);
    let reply = zbus::block_on(async {
        if c.args.is_empty() {
            conn.call_method(Some(SERVICE), PATH, Some(iface.as_str()), c.method.as_str(), &())
                .await
        } else {
            let mut b = StructureBuilder::new();
            for a in &c.args {
                b = b.append_field(a.clone());
            }
            let body = b.build()?;
            conn.call_method(
                Some(SERVICE),
                PATH,
                Some(iface.as_str()),
                c.method.as_str(),
                &body,
            )
            .await
        }
    })?;
    let body = reply.body();
    Ok(match (c.iface, c.method.as_str()) {
        ("Fans1", _) if body.signature().to_string() == "a(uu)" => None,
        ("Daemon1", "GpuUsers") => body
            .deserialize::<Vec<(u32, String, String)>>()
            .ok()
            .map(Event::GpuUsers),
        _ => None,
    })
}

/// Live readings that the daemon computes on each query (no signal): fan speeds and battery
/// power. Everything comes from tspmod; the GUI reads no sysfs. One thread reads them all.
pub fn read_live(what: Vec<(&'static str, &'static str)>, emit: impl Fn(Event) + Send + 'static) {
    std::thread::Builder::new()
        .name("live".into())
        .spawn(move || {
            for (iface, prop) in what {
                let r = (|| -> zbus::Result<V> {
                    let conn = connect()?;
                    let reply = zbus::block_on(conn.call_method(
                        Some(SERVICE),
                        PATH,
                        Some("org.freedesktop.DBus.Properties"),
                        "Get",
                        &(format!("{PREFIX}{iface}"), prop),
                    ))?;
                    let v: OwnedValue = reply.body().deserialize()?;
                    Ok(V::from_value(&v))
                })();
                if let Ok(v) = r {
                    emit(Event::Props(iface.to_owned(), vec![(prop.to_owned(), v)]));
                }
            }
        })
        .ok();
}

/// Ask logind to reboot (the GPU's Ultimate mode needs it).
pub fn reboot(emit: impl Fn(Event) + Send + 'static) {
    std::thread::Builder::new()
        .name("reboot".into())
        .spawn(move || {
            let r = connect().and_then(|conn| {
                zbus::block_on(conn.call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    "Reboot",
                    &(true,),
                ))
            });
            if let Err(e) = r {
                emit(Event::Error(explain(&e)));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_method_signatures() {
        let xml = r#"<node>
 <interface name="org.freedesktop.DBus.Peer"><method name="Ping"/></interface>
 <interface name="org.tspmo.Display1">
  <method name="SetFlickerFree">
   <arg name="setting" type="s" direction="in"/>
   <arg type="s" direction="out"/>
  </method>
  <method name="GetState"><arg type="a{sv}" direction="out"/></method>
  <property name="FlickerFree" type="b" access="read"/>
 </interface>
 <interface name="org.tspmo.Fans1">
  <method name="SetCurve">
   <arg name="mode" type="s" direction="in"/>
   <arg name="fan" type="u" direction="in"/>
   <arg name="points" type="a(yy)" direction="in"/>
  </method>
 </interface>
</node>"#;
        let m = parse_methods(xml);
        assert_eq!(m["Display1.SetFlickerFree"], ["s"]);
        assert!(m["Display1.GetState"].is_empty());
        assert_eq!(m["Fans1.SetCurve"], ["s", "u", "a(yy)"]);
        assert!(!m.contains_key("Peer.Ping"));
    }
}
