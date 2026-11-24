//! `tspmo fans | limits | battery | hw | light | lid | fnlock`: command line for the ASUS
//! interfaces of tspmod.

use std::process::ExitCode;
use zbus::blocking::{Connection, Proxy};

const NAME: &str = "org.tspmo.Daemon1";
const PATH: &str = "/org/tspmo";

pub const USAGE: &str = "\
ASUS COMMANDS:
    fans                         Fan speeds and curves per mode
    fans <mode> <fan> <what>     mode: power-saver|balanced|performance|now; fan: cpu|gpu|mid|all|<n>
                                 what: auto | full | reset | <temp:percent,...> (8 points)
    limits                       Power limits per mode and power source
    limits <mode> <ac|battery> <name> <value|default>
    limits <mode> reset          Firmware defaults for a mode
    battery                      Charge limit, health, power, time left
    battery limit <percent>      Set the charge limit
    battery once [off]           Charge to 100% once
    hw                           Firmware toggles (panel overdrive, boot sound, ...)
    hw <name> <value>
";

pub fn handles(cmd: &str) -> bool {
    matches!(cmd, "fans" | "limits" | "battery" | "hw")
}

pub fn run(cmd: &str, args: &[String]) -> ExitCode {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match cmd {
        "fans" => fans(&a),
        "limits" => limits(&a),
        "battery" => battery(&a),
        "hw" => hw(&a),
        _ => Err(format!("unknown command '{cmd}'")),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tspmo: {e}");
            ExitCode::FAILURE
        }
    }
}

// --- bus ---------------------------------------------------------------------------------------

fn connect() -> Result<Connection, String> {
    match std::env::var("TSPMOD_BUS").as_deref() {
        Ok("session") => Connection::session(),
        _ => Connection::system(),
    }
    .map_err(|e| format!("can't reach the system bus: {e}"))
}

fn explain(e: zbus::Error, iface: &str) -> String {
    let s = e.to_string();
    if s.contains("ServiceUnknown") || s.contains("NameHasNoOwner") || s.contains("was not provided") {
        "tspmod isn't running (start it with: sudo systemctl enable --now tspmod)".into()
    } else if s.contains("UnknownInterface") || s.contains("UnknownObject") || s.contains("UnknownProperty") {
        format!("this laptop has no {iface}")
    } else {
        match s.split_once(": ") {
            Some((kind, msg)) if kind.contains(".Error.") => msg.to_owned(),
            _ => s,
        }
    }
}

struct Iface {
    iface: &'static str,
    // Keeps the connection alive for the proxy.
    conn: Connection,
}

impl Iface {
    fn new(iface: &'static str) -> Result<Self, String> {
        Ok(Self {
            iface,
            conn: connect()?,
        })
    }

    fn proxy(&self) -> Result<Proxy<'_>, String> {
        Proxy::new(&self.conn, NAME, PATH, self.iface).map_err(|e| e.to_string())
    }

    fn get<T>(&self, prop: &str) -> Result<T, String>
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
        T::Error: Into<zbus::Error>,
    {
        self.proxy()?
            .get_property(prop)
            .map_err(|e| explain(e, self.iface))
    }

    fn call<B>(&self, method: &str, body: &B) -> Result<(), String>
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.proxy()?
            .call::<_, _, ()>(method, body)
            .map_err(|e| explain(e, self.iface))
    }
}

// --- fans --------------------------------------------------------------------------------------

type Settings = Vec<(String, u32, String, Vec<(u8, u8)>)>;

fn fmt_curve(p: &[(u8, u8)]) -> String {
    p.iter()
        .map(|(t, s)| format!("{t}:{s}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn fan_arg(fans: &[(u32, String)], s: &str) -> Result<u32, String> {
    if s == "all" {
        return Ok(0);
    }
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }
    fans.iter()
        .find(|(_, l)| l.eq_ignore_ascii_case(s))
        .map(|(i, _)| *i)
        .ok_or_else(|| {
            let names: Vec<_> = fans.iter().map(|(_, l)| l.to_lowercase()).collect();
            format!("unknown fan '{s}' ({}, all)", names.join(", "))
        })
}

pub fn parse_curve(s: &str) -> Result<Vec<(u8, u8)>, String> {
    s.split([',', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (t, v) = p
                .split_once(':')
                .ok_or_else(|| format!("'{p}': use temp:percent"))?;
            Ok((
                t.parse().map_err(|_| format!("'{t}': not a temperature"))?,
                v.parse().map_err(|_| format!("'{v}': not a percentage"))?,
            ))
        })
        .collect()
}

fn fans(a: &[&str]) -> Result<(), String> {
    let f = Iface::new("org.tspmo.Fans1")?;
    let fans: Vec<(u32, String)> = f.get("Fans")?;
    let mode: String = f.get("Mode")?;
    match a {
        [] => {
            let rpm: Vec<(u32, u32)> = f.get("Rpm")?;
            println!("mode: {mode}");
            for (i, label) in &fans {
                let r = rpm.iter().find(|(n, _)| n == i).map_or(0, |(_, r)| *r);
                println!("{label:<4} {r:>5} rpm");
            }
            let settings: Settings = f.get("Settings")?;
            for m in ["power-saver", "balanced", "performance"] {
                println!("\n{m}{}", if m == mode { " (active)" } else { "" });
                for (sm, i, kind, pts) in &settings {
                    if sm == m {
                        let label = fans.iter().find(|(n, _)| n == i).map_or("?", |(_, l)| l.as_str());
                        println!("  {label:<4} {kind:<7} {}", fmt_curve(pts));
                    }
                }
            }
            Ok(())
        }
        [m, fan, what] => {
            let m = if *m == "now" { mode.as_str() } else { *m };
            let n = fan_arg(&fans, fan)?;
            match *what {
                "auto" => f.call("SetAuto", &(m, n)),
                "full" => f.call("SetFull", &(m, n)),
                "reset" => f.call("Reset", &(m, n)),
                curve => f.call("SetCurve", &(m, n, parse_curve(curve)?)),
            }
        }
        _ => Err("usage: tspmo fans [<mode> <fan> auto|full|reset|<temp:percent,...>]".into()),
    }
}

// --- power limits ------------------------------------------------------------------------------

fn limits(a: &[&str]) -> Result<(), String> {
    let p = Iface::new("org.tspmo.Power1")?;
    match a {
        [] => {
            let limits: Vec<(String, String, String, i64, i64, i64, i64)> = p.get("Limits")?;
            let custom: Vec<(String, String, String, i64)> = p.get("Custom")?;
            let (mode, source): (String, String) = (p.get("Mode")?, p.get("Source")?);
            println!("mode: {mode}, power: {source}");
            for (name, label, unit, min, max, default, current) in &limits {
                let range = if min == max {
                    "fixed".to_owned()
                } else {
                    format!("{min}-{max}")
                };
                println!("{name:<18} {label:<16} {current:>4} {unit:<2}  range {range}, default {default}");
            }
            if !custom.is_empty() {
                println!("\ncustom");
                for (m, s, n, v) in &custom {
                    println!("  {m:<12} {s:<8} {n} = {v}");
                }
            }
            Ok(())
        }
        [m, "reset"] => p.call("Reset", &(*m,)),
        [m, src, name, value] => {
            let v: i64 = if *value == "default" {
                -1
            } else {
                value
                    .parse()
                    .map_err(|_| format!("'{value}': not a number (or 'default')"))?
            };
            p.call("SetLimit", &(*m, *src, *name, v))
        }
        _ => Err("usage: tspmo limits [<mode> <ac|battery> <name> <value|default>]".into()),
    }
}

// --- battery -----------------------------------------------------------------------------------

fn battery(a: &[&str]) -> Result<(), String> {
    let b = Iface::new("org.tspmo.Battery1")?;
    match a {
        [] => {
            let limit: u8 = b.get("Limit")?;
            let user: u8 = b.get("UserLimit")?;
            let steps: Vec<u8> = b.get("Steps")?;
            let once: bool = b.get("ChargeOnce")?;
            let status: String = b.get("Status")?;
            let percent: u8 = b.get("Percent")?;
            let health: f64 = b.get("Health")?;
            let cycles: u32 = b.get("Cycles")?;
            let power: f64 = b.get("Power")?;
            let left: i64 = b.get("TimeLeft")?;
            println!("{percent}% {status}");
            if limit > 0 {
                println!(
                    "limit    {limit}%{}",
                    if once {
                        format!(
                            " (charging to 100% once, then {})",
                            if user > 0 { user } else { 100 }
                        )
                    } else {
                        String::new()
                    }
                );
                let s: Vec<String> = steps.iter().map(u8::to_string).collect();
                println!("allowed  {}", s.join(" "));
            }
            if health >= 0.0 {
                println!("health   {health:.0}%");
            }
            println!("cycles   {cycles}");
            if power >= 0.0 {
                println!("power    {power:.1} W");
            }
            if left >= 0 {
                println!("time     {}h {:02}m", left / 3600, left % 3600 / 60);
            }
            Ok(())
        }
        ["limit", v] => {
            let v: u8 = v
                .trim_end_matches('%')
                .parse()
                .map_err(|_| format!("'{v}': not a percentage"))?;
            b.call("SetLimit", &(v,))
        }
        ["once"] => b.call("SetChargeOnce", &(true,)),
        ["once", "off"] => b.call("SetChargeOnce", &(false,)),
        _ => Err("usage: tspmo battery [limit <percent> | once [off]]".into()),
    }
}

// --- hardware toggles --------------------------------------------------------------------------

type Row = (String, String, String, String, Vec<String>, i64, i64, bool);

fn hw(a: &[&str]) -> Result<(), String> {
    let h = Iface::new("org.tspmo.Hardware1")?;
    match a {
        [] => {
            let items: Vec<Row> = h.get("Items")?;
            for (name, label, kind, current, values, min, max, writable) in &items {
                let allowed = match kind.as_str() {
                    "number" => format!("{min}-{max}"),
                    _ => values.join("|"),
                };
                println!(
                    "{name:<16} {label:<16} {current:<4} {allowed}{}",
                    if *writable { "" } else { "  (read-only)" }
                );
            }
            if h.get::<bool>("RebootPending")? {
                println!("\nreboot to apply");
            }
            Ok(())
        }
        [name, value] => {
            let v = match *value {
                "on" => "1",
                "off" => "0",
                v => v,
            };
            h.call("Set", &(*name, v))
        }
        _ => Err("usage: tspmo hw [<name> <value>]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_parse() {
        assert_eq!(parse_curve("40:0,50:10").unwrap(), vec![(40, 0), (50, 10)]);
        assert_eq!(parse_curve("40:0 50:10").unwrap(), vec![(40, 0), (50, 10)]);
        assert!(parse_curve("40").is_err());
        assert!(parse_curve("x:1").is_err());
        assert!(parse_curve("300:1").is_err());
    }

    #[test]
    fn fan_names() {
        let fans = vec![(1, "CPU".to_owned()), (2, "GPU".to_owned())];
        assert_eq!(fan_arg(&fans, "gpu"), Ok(2));
        assert_eq!(fan_arg(&fans, "all"), Ok(0));
        assert_eq!(fan_arg(&fans, "3"), Ok(3));
        assert!(fan_arg(&fans, "side").unwrap_err().contains("cpu, gpu"));
    }
}
