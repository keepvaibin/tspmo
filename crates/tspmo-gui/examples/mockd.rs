//! A stand-in for tspmod on the *session* bus, shaped like docs/dbus.md, for trying every page of
//! the GUI on a machine without the hardware:
//!
//! ```text
//! cargo run -p tspmo-gui --example mockd &
//! TSPMOD_BUS=session cargo run -p tspmo-gui
//! ```
//!
//! Setters change the matching property, emit `PropertiesChanged` and log the call.

use futures_lite::StreamExt;
use std::collections::HashMap;
use zbus::message::Type;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};
use zbus::{MessageStream, connection::Builder};

type Props = Vec<(&'static str, Value<'static>)>;

fn s(v: &str) -> Value<'static> {
    Value::from(v.to_owned())
}
fn strs(v: &[&str]) -> Value<'static> {
    Value::from(v.iter().map(|x| (*x).to_owned()).collect::<Vec<String>>())
}
fn tup(fields: Vec<Value<'static>>) -> Value<'static> {
    let mut b = StructureBuilder::new();
    for f in fields {
        b = b.append_field(f);
    }
    Value::Structure(b.build().unwrap())
}
fn arr(items: Vec<Value<'static>>) -> Value<'static> {
    let sig = items.first().map(Value::value_signature).cloned();
    let mut a = zbus::zvariant::Array::new(&sig.unwrap_or(zbus::zvariant::Signature::Str));
    for i in items {
        a.append(i).unwrap();
    }
    Value::Array(a)
}
fn pts(v: &[(u8, u8)]) -> Value<'static> {
    arr(v
        .iter()
        .map(|(t, p)| tup(vec![Value::U8(*t), Value::U8(*p)]))
        .collect())
}

fn curve(base: u8) -> Vec<(u8, u8)> {
    (0..8u32)
        .map(|i| {
            (
                (30 + i * 9) as u8,
                (u32::from(base) + i * (100 - u32::from(base)) / 9) as u8,
            )
        })
        .collect()
}

fn tables() -> Vec<(&'static str, Props)> {
    let mut settings = Vec::new();
    let mut factory = Vec::new();
    for (m, base) in [("power-saver", 0u8), ("balanced", 10), ("performance", 25)] {
        for fan in 1u32..=3 {
            settings.push(tup(vec![s(m), Value::U32(fan), s("auto"), pts(&curve(base))]));
            factory.push(tup(vec![s(m), Value::U32(fan), pts(&curve(base))]));
        }
    }
    let limit = |n: &str, l: &str, u: &str, min: i64, max: i64, def: i64| {
        tup(vec![
            s(n),
            s(l),
            s(u),
            Value::I64(min),
            Value::I64(max),
            Value::I64(def),
            Value::I64(def),
        ])
    };
    let item = |n: &str, l: &str, k: &str, cur: &str, vals: &[&str], w: bool| {
        tup(vec![
            s(n),
            s(l),
            s(k),
            s(cur),
            strs(vals),
            Value::I64(0),
            Value::I64(1),
            Value::Bool(w),
        ])
    };
    vec![
        (
            "Daemon1",
            vec![
                ("Profile", s("balanced")),
                ("ProfileAc", s("balanced")),
                ("ProfileBattery", s("power-saver")),
                ("OnBattery", Value::Bool(false)),
                ("GpuMode", s("hybrid")),
                ("GpuPolicy", s("optimized")),
                ("GpuPending", s("")),
                ("DgpuAsleep", Value::Bool(true)),
                ("GpuMessage", s("")),
                ("GpuSwitching", s("")),
            ],
        ),
        (
            "Display1",
            vec![
                ("Refresh", Value::F64(240.0)),
                ("Rates", Value::from(vec![60u32, 240])),
                ("RefreshPolicy", s("auto")),
                ("FlickerFree", Value::Bool(true)),
                ("FlickerSetting", s("auto")),
                ("Brightness", Value::U32(40)),
                ("ColorMode", s("native")),
                ("ColorModes", strs(&["native", "srgb", "p3"])),
                ("EyeCare", Value::U32(0)),
                ("Oled", Value::Bool(true)),
            ],
        ),
        (
            "Fans1",
            vec![
                (
                    "Fans",
                    arr(vec![
                        tup(vec![Value::U32(1), s("CPU")]),
                        tup(vec![Value::U32(2), s("GPU")]),
                        tup(vec![Value::U32(3), s("Mid")]),
                    ]),
                ),
                ("Mode", s("balanced")),
                ("Settings", arr(settings)),
                ("Factory", arr(factory)),
            ],
        ),
        (
            "Power1",
            vec![
                (
                    "Limits",
                    arr(vec![
                        limit("ppt_pl1_spl", "Sustained", "W", 5, 65, 30),
                        limit("ppt_pl2_sppt", "Boost", "W", 5, 80, 40),
                        limit("ppt_pl3_fppt", "Fast", "W", 5, 80, 45),
                        limit("nv_dynamic_boost", "GPU boost", "W", 5, 25, 15),
                        limit("nv_temp_target", "GPU temp", "C", 75, 87, 87),
                    ]),
                ),
                (
                    "Custom",
                    arr(vec![tup(vec![
                        s("balanced"),
                        s("ac"),
                        s("ppt_pl1_spl"),
                        Value::I64(45),
                    ])]),
                ),
                ("Mode", s("balanced")),
                ("Source", s("ac")),
                ("NeedsFanCurve", Value::Bool(true)),
            ],
        ),
        (
            "Battery1",
            vec![
                ("Limit", Value::U8(80)),
                ("UserLimit", Value::U8(80)),
                ("Steps", Value::from(vec![60u8, 65, 70, 75, 80, 100])),
                ("ChargeOnce", Value::Bool(false)),
                ("Status", s("Discharging")),
                ("Percent", Value::U8(87)),
                ("Health", Value::F64(93.0)),
                ("Cycles", Value::U32(112)),
                ("Power", Value::F64(14.2)),
                ("TimeLeft", Value::I64(15300)),
            ],
        ),
        (
            "Hardware1",
            vec![
                (
                    "Items",
                    arr(vec![
                        item("boot_sound", "Boot sound", "toggle", "1", &[], true),
                        item("panel_overdrive", "Overdrive", "toggle", "1", &[], true),
                        item("mini_led_mode", "Mini-LED", "toggle", "0", &[], true),
                        item(
                            "charge_mode",
                            "Charger",
                            "choice",
                            "USB-C",
                            &["none", "barrel", "USB-C"],
                            false,
                        ),
                    ]),
                ),
                ("RebootPending", Value::Bool(false)),
            ],
        ),
        (
            "Lighting1",
            vec![
                ("MaxBrightness", Value::U8(3)),
                ("Brightness", Value::U8(2)),
                ("Modes", strs(&["static", "breathe", "rainbow", "pulse"])),
                (
                    "Effect",
                    tup(vec![
                        s("static"),
                        tup(vec![Value::U8(255), Value::U8(255), Value::U8(255)]),
                        tup(vec![Value::U8(0), Value::U8(0), Value::U8(0)]),
                        s("med"),
                        s("right"),
                    ]),
                ),
                ("Source", s("ac")),
                (
                    "PowerStates",
                    tup(vec![
                        Value::Bool(true),
                        Value::Bool(true),
                        Value::Bool(false),
                        Value::Bool(false),
                    ]),
                ),
                ("IdleTimeout", Value::U32(60)),
            ],
        ),
        (
            "Lid1",
            vec![
                ("Devices", strs(&["slash"])),
                ("SlashEnabled", Value::Bool(true)),
                ("SlashBrightness", Value::U8(120)),
                ("SlashInterval", Value::U8(5)),
                ("SlashMode", s("Bounce")),
                ("SlashModes", strs(&["Bounce", "Slash", "Spectrum", "Flow"])),
                (
                    "SlashShow",
                    arr(["boot", "sleep", "shutdown", "battery", "lid", "warning"]
                        .iter()
                        .enumerate()
                        .map(|(i, n)| tup(vec![s(n), Value::Bool(i != 2 && i != 3)]))
                        .collect()),
                ),
            ],
        ),
    ]
}

/// (interface, method) → (input signature, property to set from argument `arg`).
fn setters() -> Vec<(&'static str, &'static str, &'static str, &'static str, usize)> {
    vec![
        ("Daemon1", "SetProfile", "s", "Profile", 0),
        ("Daemon1", "SetGpuMode", "s", "GpuPolicy", 0),
        ("Daemon1", "GpuUsers", "", "", 0),
        ("Display1", "SetRefreshPolicy", "s", "RefreshPolicy", 0),
        ("Display1", "SetFlickerFree", "s", "FlickerSetting", 0),
        ("Display1", "SetBrightness", "u", "Brightness", 0),
        ("Display1", "SetColorMode", "s", "ColorMode", 0),
        ("Display1", "SetEyeCare", "u", "EyeCare", 0),
        ("Fans1", "SetCurve", "sua(yy)", "", 0),
        ("Fans1", "SetAuto", "su", "", 0),
        ("Fans1", "SetFull", "su", "", 0),
        ("Fans1", "Reset", "su", "", 0),
        ("Power1", "SetLimit", "sssx", "", 0),
        ("Power1", "Reset", "s", "", 0),
        ("Battery1", "SetLimit", "y", "Limit", 0),
        ("Battery1", "SetChargeOnce", "b", "ChargeOnce", 0),
        ("Hardware1", "Set", "ss", "", 0),
        ("Lighting1", "SetBrightness", "sy", "Brightness", 1),
        ("Lighting1", "SetEffect", "s(s(yyy)(yyy)ss)", "Effect", 1),
        ("Lighting1", "SetPowerStates", "bbbb", "", 0),
        ("Lighting1", "SetIdleTimeout", "u", "IdleTimeout", 0),
        ("Lid1", "SetSlashEnabled", "b", "SlashEnabled", 0),
        ("Lid1", "SetSlashBrightness", "y", "SlashBrightness", 0),
        ("Lid1", "SetSlashMode", "s", "SlashMode", 0),
        ("Lid1", "SetSlashShow", "sb", "", 0),
    ]
}

fn introspect(tables: &[(&str, Props)]) -> String {
    let mut x = String::from("<node>\n");
    for (name, _) in tables {
        x.push_str(&format!(" <interface name=\"org.tspmo.{name}\">\n"));
        for (i, m, sig, _, _) in setters() {
            if i == *name {
                x.push_str(&format!("  <method name=\"{m}\">\n"));
                // One <arg> per complete type.
                let mut rest = sig;
                while !rest.is_empty() {
                    let n = complete_type_len(rest);
                    x.push_str(&format!("   <arg type=\"{}\" direction=\"in\"/>\n", &rest[..n]));
                    rest = &rest[n..];
                }
                x.push_str("  </method>\n");
            }
        }
        x.push_str(" </interface>\n");
    }
    x.push_str("</node>");
    x
}

fn complete_type_len(s: &str) -> usize {
    let b = s.as_bytes();
    match b[0] {
        b'a' => 1 + complete_type_len(&s[1..]),
        b'(' => {
            let mut depth = 0;
            for (i, c) in b.iter().enumerate() {
                match c {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return i + 1;
                        }
                    }
                    _ => {}
                }
            }
            b.len()
        }
        _ => 1,
    }
}

fn main() -> zbus::Result<()> {
    zbus::block_on(async {
        let conn = Builder::session()?.name("org.tspmo.Daemon1")?.build().await?;
        let mut state: HashMap<String, HashMap<String, Value<'static>>> = tables()
            .into_iter()
            .map(|(n, p)| {
                (
                    n.to_owned(),
                    p.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
                )
            })
            .collect();
        let tbl = tables();
        let xml = introspect(&tbl);
        eprintln!("mockd on the session bus as org.tspmo.Daemon1");
        let mut stream = MessageStream::from(&conn);
        let mut tick = 0u32;
        while let Some(Ok(msg)) = stream.next().await {
            let h = msg.header();
            if h.message_type() != Type::MethodCall {
                continue;
            }
            let iface = h.interface().map(|i| i.to_string()).unwrap_or_default();
            let member = h.member().map(|m| m.to_string()).unwrap_or_default();
            let short = iface.strip_prefix("org.tspmo.").unwrap_or("").to_owned();
            match (iface.as_str(), member.as_str()) {
                ("org.freedesktop.DBus.Introspectable", "Introspect") => {
                    conn.reply(&h, &xml.as_str()).await?;
                }
                ("org.freedesktop.DBus.Properties", "GetAll") => {
                    let name: String = msg.body().deserialize()?;
                    let short = name.strip_prefix("org.tspmo.").unwrap_or("");
                    match state.get(short) {
                        Some(p) => {
                            let m: HashMap<&str, Value<'_>> =
                                p.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                            conn.reply(&h, &m).await?;
                        }
                        None => {
                            conn.reply_error(
                                &h,
                                "org.freedesktop.DBus.Error.UnknownInterface",
                                &"no such interface",
                            )
                            .await?;
                        }
                    }
                }
                ("org.freedesktop.DBus.Properties", "Get") => {
                    tick += 1;
                    let (_, prop): (String, String) = msg.body().deserialize()?;
                    let v = if prop == "Rpm" {
                        arr((1u32..=3)
                            .map(|f| {
                                tup(vec![
                                    Value::U32(f),
                                    Value::U32(if f == 3 {
                                        0
                                    } else {
                                        2000 + 40 * f + (tick % 7) * 20
                                    }),
                                ])
                            })
                            .collect())
                    } else {
                        Value::from(0u8)
                    };
                    conn.reply(&h, &Value::Value(Box::new(v))).await?;
                }
                _ if state.contains_key(&short) => {
                    let body = msg.body();
                    let args: Vec<Value<'static>> = match body.deserialize::<zbus::zvariant::Structure>() {
                        Ok(st) => st
                            .fields()
                            .iter()
                            .map(|f| f.try_to_owned().map(Value::from).unwrap_or(Value::U8(0)))
                            .collect(),
                        Err(_) => body
                            .deserialize::<OwnedValue>()
                            .map(|o| vec![Value::from(o)])
                            .unwrap_or_default(),
                    };
                    eprintln!("{short}.{member} {args:?}");
                    if member == "GpuUsers" {
                        let v = Value::from(vec![(4242u32, "blender".to_owned(), "render".to_owned())]);
                        conn.reply(&h, &v).await?;
                        continue;
                    }
                    let mut changed: HashMap<String, Value<'static>> = HashMap::new();
                    if let Some((_, _, _, prop, arg)) = setters()
                        .into_iter()
                        .find(|(i, m, ..)| *i == short && *m == member)
                    {
                        if !prop.is_empty()
                            && let Some(v) = args.get(arg)
                        {
                            let v = match (prop, v) {
                                ("FlickerSetting", Value::Str(x)) => {
                                    state
                                        .get_mut(&short)
                                        .unwrap()
                                        .insert("FlickerFree".into(), Value::Bool(x.as_str() != "off"));
                                    changed.insert("FlickerFree".into(), Value::Bool(x.as_str() != "off"));
                                    v.clone()
                                }
                                _ => v.clone(),
                            };
                            state.get_mut(&short).unwrap().insert(prop.to_owned(), v.clone());
                            changed.insert(prop.to_owned(), v);
                        }
                        if short == "Hardware1" && member == "Set" {
                            // Rebuild Items with the new value.
                            if let (Some(Value::Str(n)), Some(Value::Str(val))) = (args.first(), args.get(1))
                            {
                                let items = state["Hardware1"]["Items"].clone();
                                if let Value::Array(a) = items {
                                    let new: Vec<Value<'static>> = a
                                        .iter()
                                        .map(|it| {
                                            let Value::Structure(st) = it else {
                                                return it.try_to_owned().map(Value::from).unwrap();
                                            };
                                            let f = st.fields();
                                            if matches!(&f[0], Value::Str(x) if x.as_str() == n.as_str()) {
                                                let mut v: Vec<Value<'static>> = f
                                                    .iter()
                                                    .map(|x| x.try_to_owned().map(Value::from).unwrap())
                                                    .collect();
                                                v[3] = s(val.as_str());
                                                tup(v)
                                            } else {
                                                it.try_to_owned().map(Value::from).unwrap()
                                            }
                                        })
                                        .collect();
                                    let na = arr(new);
                                    state
                                        .get_mut("Hardware1")
                                        .unwrap()
                                        .insert("Items".into(), na.clone());
                                    changed.insert("Items".into(), na);
                                }
                            }
                        }
                    }
                    conn.reply(&h, &()).await?;
                    if !changed.is_empty() {
                        conn.emit_signal(
                            None::<&str>,
                            "/org/tspmo",
                            "org.freedesktop.DBus.Properties",
                            "PropertiesChanged",
                            &(iface.as_str(), changed, Vec::<String>::new()),
                        )
                        .await?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })
}
