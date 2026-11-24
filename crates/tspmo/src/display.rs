//! `tspmo display`: the laptop's screen (refresh rate, brightness, flicker-free dimming, colour).

use crate::client;
use std::process::ExitCode;

const USAGE: &str = "\
USAGE:
    tspmo display                       Show the screen settings
    tspmo display refresh [max|60|auto] Refresh rate (auto: 60 Hz on battery, max on AC)
    tspmo display brightness [1-100]    Brightness (flicker-free below the panel's floor on OLED)
    tspmo display dimming [auto|on|off] Flicker-free dimming (auto: on for OLED)
    tspmo display floor [1-100]         Lowest panel brightness that is flicker-free, percent
    tspmo display color [native|srgb|p3] Colour mode
    tspmo display eyecare [<kelvin>|off] Warm white point (1000-6500 K)
";

fn fail(e: String) -> ExitCode {
    eprintln!("tspmo: {e}");
    ExitCode::FAILURE
}

fn report(r: Result<String, String>) -> ExitCode {
    match r {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn number(arg: &str, what: &str) -> Result<u32, String> {
    arg.trim_end_matches('%')
        .parse()
        .map_err(|_| format!("{what}: '{arg}' is not a number"))
}

pub fn run(args: &[String]) -> ExitCode {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a[..] {
        [] | ["status"] => show(),
        ["help" | "-h" | "--help"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["refresh"] => value("RefreshPolicy"),
        ["refresh", p] => report(client::display_set("SetRefreshPolicy", p)),
        ["brightness"] => value("Brightness"),
        ["brightness", n] => {
            report(number(n, "brightness").and_then(|n| client::display_set_num("SetBrightness", n)))
        }
        ["dimming"] => value("FlickerSetting"),
        ["dimming", s] => report(client::display_set("SetFlickerFree", s)),
        ["floor"] => value("FlickerFloor"),
        ["floor", n] => {
            report(number(n, "floor").and_then(|n| client::display_set_num("SetFlickerFloor", n)))
        }
        ["color" | "colour"] => value("ColorMode"),
        ["color" | "colour", m] => report(client::display_set("SetColorMode", m)),
        ["eyecare"] => value("EyeCare"),
        ["eyecare", "off"] => report(client::display_set_num("SetEyeCare", 0)),
        ["eyecare", k] => report(
            number(k.trim_end_matches(['K', 'k']), "eyecare")
                .and_then(|k| client::display_set_num("SetEyeCare", k)),
        ),
        _ => {
            eprintln!("tspmo: display: unknown or incomplete command (tspmo display help)");
            ExitCode::from(2)
        }
    }
}

fn value(key: &str) -> ExitCode {
    match client::display_state() {
        Ok(st) => {
            match st.get(key) {
                Some(v) => {
                    if let Ok(s) = String::try_from(v.try_clone().unwrap_or_else(|_| v.clone())) {
                        println!("{s}");
                    } else if let Ok(n) = u32::try_from(v) {
                        println!("{n}");
                    }
                }
                None => return fail(format!("no such setting: {key}")),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn show() -> ExitCode {
    let st = match client::display_state() {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let s = |k| client::get_str(&st, k);
    println!(
        "panel:       {}{}",
        s("Connector"),
        if client::get_bool(&st, "Oled") {
            " (OLED)"
        } else {
            ""
        }
    );
    let rates = client::get_u32s(&st, "Rates");
    println!(
        "refresh:     {:.0} Hz, policy {}  (available: {})",
        client::get_f64(&st, "Refresh"),
        s("RefreshPolicy"),
        rates.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
    );
    println!(
        "brightness:  {}%  (backlight {}%, dimming {}%)",
        client::get_u32(&st, "Brightness"),
        client::get_u32(&st, "Backlight"),
        client::get_u32(&st, "Dimming")
    );
    println!(
        "flicker-free: {} (setting {}, floor {}%)",
        if client::get_bool(&st, "FlickerFree") {
            "on"
        } else {
            "off"
        },
        s("FlickerSetting"),
        client::get_u32(&st, "FlickerFloor")
    );
    let k = client::get_u32(&st, "EyeCare");
    println!(
        "colour:      {}  (modes: {}){}",
        s("ColorMode"),
        client::get_strs(&st, "ColorModes").join(", "),
        if k > 0 {
            format!(", eye care {k} K")
        } else {
            String::new()
        }
    );
    let msg = s("Message");
    if !msg.is_empty() {
        println!("note:        {msg}");
    }
    ExitCode::SUCCESS
}

/// One line for `tspmo status`; silent when the daemon has no display interface.
pub fn status_line() {
    if let Ok(st) = client::display_state() {
        let hz = client::get_f64(&st, "Refresh");
        if hz > 0.0 {
            println!(
                "screen:   {hz:.0} Hz, {}% brightness, {}",
                client::get_u32(&st, "Brightness"),
                client::get_str(&st, "ColorMode")
            );
        }
    }
}
