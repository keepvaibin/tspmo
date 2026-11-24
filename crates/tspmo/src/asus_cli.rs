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
    light                        Keyboard lighting
    light brightness <0-3> [ac|battery]
    light <effect> [rrggbb] [rrggbb] [low|med|high] [right|left|up|down]
    light next | prev            Cycle effects (what the Aura key does)
    light power <boot> <awake> <sleep> <shutdown>   on|off for each
    light idle <seconds>         Dark after this long without input (0: never)
    lid                          Slash light bar
    lid on | off | brightness <0-255> | interval <0-255> | mode <name>
    lid show <boot|sleep|shutdown|battery|lid|warning> on|off
    lid anime on|off | brightness <0-3> | builtin on|off | text <text> | image <file.pgm>
";

pub fn handles(cmd: &str) -> bool {
    matches!(cmd, "fans" | "limits" | "battery" | "hw" | "light" | "lid")
}

pub fn run(cmd: &str, args: &[String]) -> ExitCode {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match cmd {
        "fans" => fans(&a),
        "limits" => limits(&a),
        "battery" => battery(&a),
        "hw" => hw(&a),
        "light" => light(&a),
        "lid" => lid(&a),
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

    fn call_str<B>(&self, method: &str, body: &B) -> Result<String, String>
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.proxy()?
            .call::<_, _, String>(method, body)
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

// --- keyboard lighting -------------------------------------------------------------------------

type Effect = (String, (u8, u8, u8), (u8, u8, u8), String, String);

pub fn parse_colour(s: &str) -> Result<(u8, u8, u8), String> {
    let h = s.trim_start_matches('#');
    match (h.len(), u32::from_str_radix(h, 16)) {
        (6, Ok(v)) => Ok(((v >> 16) as u8, (v >> 8) as u8, v as u8)),
        _ => Err(format!("'{s}': colour is rrggbb, e.g. ff8000")),
    }
}

fn on_off(s: &str) -> Result<bool, String> {
    match s {
        "on" | "1" | "yes" => Ok(true),
        "off" | "0" | "no" => Ok(false),
        _ => Err(format!("'{s}': use on or off")),
    }
}

fn light(a: &[&str]) -> Result<(), String> {
    let l = Iface::new("org.tspmo.Lighting1")?;
    match a {
        [] => {
            let max: u8 = l.get("MaxBrightness")?;
            let level: u8 = l.get("Brightness")?;
            let modes: Vec<String> = l.get("Modes")?;
            let source: String = l.get("Source")?;
            println!("brightness {level}/{max} ({source})");
            if modes.is_empty() {
                return Ok(());
            }
            let e: Effect = l.get("Effect")?;
            let kind: String = l.get("Kind")?;
            let params: Vec<(String, bool, bool, bool, bool)> = l.get("ModeParams")?;
            // Only what the current effect takes: a rainbow has no colour, static no speed.
            let mut shown = e.0.clone();
            if let Some((_, c1, c2, speed, dir)) = params.iter().find(|p| p.0 == e.0) {
                if *c1 {
                    shown.push_str(&format!(" {:02x}{:02x}{:02x}", e.1.0, e.1.1, e.1.2));
                }
                if *c2 {
                    shown.push_str(&format!(" {:02x}{:02x}{:02x}", e.2.0, e.2.1, e.2.2));
                }
                if *speed {
                    shown.push_str(&format!(" {}", e.3));
                }
                if *dir {
                    shown.push_str(&format!(" {}", e.4));
                }
            }
            println!("backlight  {kind}");
            println!("effect     {shown}");
            println!("effects    {}", modes.join(" "));
            let (b, w, s, d): (bool, bool, bool, bool) = l.get("PowerStates")?;
            let f = |v: bool| if v { "on" } else { "off" };
            println!(
                "lit in     boot {} awake {} sleep {} shutdown {}",
                f(b),
                f(w),
                f(s),
                f(d)
            );
            let idle: u32 = l.get("IdleTimeout")?;
            println!(
                "idle       {}",
                if idle == 0 {
                    "never".to_owned()
                } else {
                    format!("{idle} s")
                }
            );
            Ok(())
        }
        ["brightness", level, rest @ ..] => {
            let level: u8 = level.parse().map_err(|_| format!("'{level}': not a level"))?;
            l.call("SetBrightness", &(rest.first().copied().unwrap_or(""), level))
        }
        ["next"] | ["prev"] => {
            println!("{}", l.call_str("CycleEffect", &(a[0] == "next",))?);
            Ok(())
        }
        ["power", b, w, s, d] => l.call(
            "SetPowerStates",
            &(on_off(b)?, on_off(w)?, on_off(s)?, on_off(d)?),
        ),
        ["idle", n] => l.call(
            "SetIdleTimeout",
            &(n.parse::<u32>().map_err(|_| format!("'{n}': not a number"))?,),
        ),
        [effect, rest @ ..] => {
            // Colours are hex; speed and direction are words.
            let (mut colours, mut speed, mut dir) = (Vec::new(), "med", "right");
            for r in rest {
                match *r {
                    "low" | "med" | "high" => speed = r,
                    "right" | "left" | "up" | "down" => dir = r,
                    c => colours.push(parse_colour(c)?),
                }
            }
            let c1 = colours.first().copied().unwrap_or((255, 255, 255));
            let c2 = colours.get(1).copied().unwrap_or((0, 0, 0));
            l.call("SetEffect", &("", (*effect, c1, c2, speed, dir)))
        }
    }
}

// --- lid display -------------------------------------------------------------------------------

fn lid(a: &[&str]) -> Result<(), String> {
    let l = Iface::new("org.tspmo.Lid1")?;
    match a {
        ["anime", rest @ ..] => anime(&l, rest),
        [] => {
            let devices: Vec<String> = l.get("Devices")?;
            if devices.iter().any(|d| d == "anime") {
                let on: bool = l.get("AnimeEnabled")?;
                let level: u8 = l.get("AnimeBrightness")?;
                let builtin: bool = l.get("AnimeBuiltin")?;
                println!(
                    "anime {}  brightness {level}  built-in animations {}",
                    if on { "on" } else { "off" },
                    if builtin { "on" } else { "off" }
                );
            }
            if !devices.iter().any(|d| d == "slash") {
                return Ok(());
            }
            let enabled: bool = l.get("SlashEnabled")?;
            let brightness: u8 = l.get("SlashBrightness")?;
            let interval: u8 = l.get("SlashInterval")?;
            let mode: String = l.get("SlashMode")?;
            let show: Vec<(String, bool)> = l.get("SlashShow")?;
            let lit: bool = l.get("SlashLit")?;
            let max: u8 = l.get("SlashIntervalMax")?;
            let levels: Vec<(String, u8)> = l.get("SlashBrightnessLevels")?;
            println!(
                "slash {}{}  brightness {brightness}  interval {interval} s (0-{max})  mode {mode}",
                if enabled { "on" } else { "off" },
                if enabled && !lit { " (dark on battery)" } else { "" }
            );
            let steps: Vec<String> = levels.iter().map(|(n, v)| format!("{n}={v}")).collect();
            println!("levels {}", steps.join(" "));
            let s: Vec<String> = show
                .iter()
                .map(|(n, on)| format!("{n} {}", if *on { "on" } else { "off" }))
                .collect();
            println!("show  {}", s.join(", "));
            let modes: Vec<String> = l.get("SlashModes")?;
            println!("modes {}", modes.join(" "));
            Ok(())
        }
        ["on"] => l.call("SetSlashEnabled", &(true,)),
        ["off"] => l.call("SetSlashEnabled", &(false,)),
        ["brightness", v] => {
            let levels: Vec<(String, u8)> = l.get("SlashBrightnessLevels")?;
            let n = levels
                .iter()
                .find(|(name, _)| name == v)
                .map(|(_, n)| *n)
                .or_else(|| v.parse::<u8>().ok())
                .ok_or_else(|| format!("'{v}': 0-255 or off, low, medium, high"))?;
            l.call("SetSlashBrightness", &(n,))
        }
        ["interval", v] => l.call(
            "SetSlashInterval",
            &(v.parse::<u8>().map_err(|_| format!("'{v}': 0-5 seconds"))?,),
        ),
        ["mode", m] => l.call("SetSlashMode", &(*m,)),
        ["show", what, v] => l.call("SetSlashShow", &(*what, on_off(v)?)),
        _ => {
            Err("usage: tspmo lid [on|off|brightness <n>|interval <n>|mode <name>|show <what> on|off]".into())
        }
    }
}

fn anime(l: &Iface, a: &[&str]) -> Result<(), String> {
    match a {
        ["on"] => l.call("SetAnimeEnabled", &(true,)),
        ["off"] => l.call("SetAnimeEnabled", &(false,)),
        ["brightness", v] => l.call(
            "SetAnimeBrightness",
            &(v.parse::<u8>().map_err(|_| format!("'{v}': 0-3"))?,),
        ),
        ["builtin", v] => l.call("SetAnimeBuiltin", &(on_off(v)?,)),
        ["text", words @ ..] if !words.is_empty() => l.call("ShowAnimeText", &(words.join(" "),)),
        ["image", file] => {
            let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            l.call("ShowAnimeImage", &(data,))
        }
        _ => Err(
            "usage: tspmo lid anime on|off|brightness <0-3>|builtin on|off|text <text>|image <file.pgm>"
                .into(),
        ),
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
    fn colours_parse() {
        assert_eq!(parse_colour("ff8000"), Ok((255, 128, 0)));
        assert_eq!(parse_colour("#00ff00"), Ok((0, 255, 0)));
        assert!(parse_colour("fff").is_err());
        assert!(parse_colour("gg0000").is_err());
        assert_eq!(on_off("on"), Ok(true));
        assert!(on_off("maybe").is_err());
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
