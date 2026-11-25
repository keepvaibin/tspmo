//! `tspmo` — Thermal & System Power Management Optimizer, command line.

mod asus_cli;
mod bench;
mod client;
mod display;
mod dump;
mod info;
mod session_env;

use std::process::ExitCode;
use tspmo_core::{SysRoot, doctor, machine::Machine};

const USAGE: &str = "\
tspmo — Thermal & System Power Management Optimizer

USAGE:
    tspmo <command> [options]

COMMANDS:
    info              What this laptop is and which controls it exposes
    status            Current profile, power source and GPU mode (from tspmod)
    profile [<name>]  Show or set the power mode: eco-plus | power-saver | balanced | performance
    profile cycle     Next mode
    display ...       Screen: refresh rate, brightness, dimming, colour (tspmo display help)
    bench ...         Measure what a setting costs or saves (tspmo bench help)
    ping [N]          Time N (default 50) round trips to tspmod
    gpu [<mode>]      Show or set the GPU mode: eco | hybrid | ultimate | optimized
    gpu users         Apps keeping the NVIDIA GPU awake
    doctor            Find what breaks features or costs battery, with exact fixes
    doctor --apply    ...and apply those fixes (asks for your password via sudo)
    dump <dir>        Capture an anonymised snapshot of this machine's kernel interfaces
                      (attach it to a hardware-support issue)
    help              Show this message

ENVIRONMENT:
    TSPMO_SYSROOT     Read kernel interfaces from this directory instead of /
                      (point it at a `tspmo dump` to inspect another machine)
";

fn main() -> ExitCode {
    // `tspmo status | head -1`: a closed pipe ends the program quietly, as other tools do.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if msg.contains("Broken pipe") {
            std::process::exit(0);
        }
        default_hook(info);
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = SysRoot::from_env();
    // `tspmo <command> -h`: the lines about that command.
    if let [cmd, flag] = &args[..] {
        if matches!(flag.as_str(), "-h" | "--help") && !matches!(cmd.as_str(), "display" | "bench") {
            return command_help(cmd);
        }
    }
    match args.first().map(String::as_str) {
        Some("info") => {
            info::print(&Machine::probe(&root));
            ExitCode::SUCCESS
        }
        Some("doctor") => match &args[1..] {
            [] => run_doctor(&root, false),
            [a] if a == "--apply" => run_doctor(&root, true),
            [a, ..] => usage_error(&format!("doctor: unexpected '{a}'")),
        },
        Some("status") => status(),
        // systemd-sleep hook: keep the internal USB chips powered across sleep (some keyboard
        // firmware corrupts its USB state when resumed from autosuspend after a long sleep).
        Some("sleep-hook") => {
            match args.get(1).map(String::as_str) {
                Some("pre") => {
                    tspmo_core::usbpm::set_control(&root, false);
                }
                Some("post") => {
                    tspmo_core::usbpm::apply(&root);
                }
                _ => {}
            }
            ExitCode::SUCCESS
        }
        Some("session-env") => {
            session_env::print(&root);
            ExitCode::SUCCESS
        }
        Some("profile") => profile(args.get(1).map(String::as_str)),
        Some("gpu") => gpu(args.get(1).map(String::as_str)),
        Some(c) if asus_cli::handles(c) => asus_cli::run(c, &args[1..]),
        Some("display") => display::run(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("ping") => match args.get(1).map(|n| n.parse::<usize>()) {
            None => ping(50),
            Some(Ok(n)) if n > 0 => ping(n),
            Some(_) => usage_error("ping: N is a positive number"),
        },
        Some("dump") => match args.get(1) {
            Some(dir) => match dump::dump(&root, dir.as_ref()) {
                Ok(n) => {
                    println!("Wrote {n} files to {dir}");
                    println!("Share it with:  tar -C {dir} -czf tspmo-dump.tar.gz .");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("tspmo dump: {e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                eprintln!("tspmo dump: missing output directory");
                ExitCode::from(2)
            }
        },
        None | Some("help" | "-h" | "--help") => {
            print!("{USAGE}\n{}", asus_cli::USAGE);
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("tspmo {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("unknown command '{other}'")),
    }
}

/// One line and exit 2: the command line was wrong.
fn usage_error(what: &str) -> ExitCode {
    eprintln!("tspmo: {what} (tspmo help)");
    ExitCode::from(2)
}

/// The lines of the usage text that belong to `cmd` (a command, then its indented continuations).
fn help_lines(cmd: &str) -> String {
    let text = format!("{USAGE}\n{}", asus_cli::USAGE);
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        match line.len() - line.trim_start().len() {
            0 => inside = false,
            4 => inside = line.split_whitespace().next() == Some(cmd),
            _ => {}
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn command_help(cmd: &str) -> ExitCode {
    let out = help_lines(cmd);
    if out.is_empty() {
        return usage_error(&format!("unknown command '{cmd}'"));
    }
    print!("{out}");
    ExitCode::SUCCESS
}

fn fail(e: String) -> ExitCode {
    eprintln!("tspmo: {e}");
    ExitCode::FAILURE
}

fn status() -> ExitCode {
    let st = match client::state() {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let s = |k| client::get_str(&st, k);
    let on_batt = client::get_bool(&st, "OnBattery");
    println!(
        "profile:  {}  (on AC: {}, on battery: {})",
        s("Profile"),
        s("ProfileAc"),
        s("ProfileBattery")
    );
    println!("power:    {}", if on_batt { "battery" } else { "AC" });
    display::status_line();
    let pending = s("GpuPending");
    println!(
        "gpu:      {} (policy {}){}{}",
        s("GpuMode"),
        s("GpuPolicy"),
        if client::get_bool(&st, "DgpuAsleep") {
            ", dGPU asleep"
        } else {
            ""
        },
        if pending.is_empty() {
            String::new()
        } else {
            format!(", {pending} after reboot")
        }
    );
    let msg = s("GpuMessage");
    if !msg.is_empty() {
        println!("          {msg}");
    }
    ExitCode::SUCCESS
}

/// Round-trip times to the daemon, over one connection (the cost the desktop sees).
fn ping(n: usize) -> ExitCode {
    let mut ms = Vec::with_capacity(n);
    for _ in 0..n {
        let t = std::time::Instant::now();
        if let Err(e) = client::state() {
            return fail(e);
        }
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{n} calls: median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        bench::median(&ms),
        bench::percentile(&ms, 95.0),
        bench::percentile(&ms, 100.0)
    );
    ExitCode::SUCCESS
}

fn profile(arg: Option<&str>) -> ExitCode {
    match arg {
        None => match client::state() {
            Ok(st) => {
                println!("{}", client::get_str(&st, "Profile"));
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        Some("cycle" | "next") => match client::cycle_profile() {
            Ok(p) => {
                println!("{p}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        Some(p) => match tspmo_core::power::Profile::parse(p) {
            Some(p) => match client::set_profile(p.as_str()) {
                Ok(()) => {
                    println!("{}", p.as_str());
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            },
            None => fail(format!(
                "unknown profile '{p}' (eco-plus, power-saver, balanced, performance)"
            )),
        },
    }
}

fn gpu(arg: Option<&str>) -> ExitCode {
    match arg {
        None | Some("status") => status(),
        Some("users") => match client::gpu_users() {
            Ok(u) if u.is_empty() => {
                println!("Nothing uses the NVIDIA GPU.");
                ExitCode::SUCCESS
            }
            Ok(u) => {
                for (pid, name, kind) in u {
                    let what = if kind == "render" {
                        "app using the GPU (blocks Eco)"
                    } else {
                        "desktop watching its outputs (released on switch)"
                    };
                    println!("{pid:>7}  {name:<18} {what}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        Some(m) => match client::set_gpu(m) {
            Ok(msg) if msg.starts_with("Switching") => {
                println!("{msg}");
                match client::wait_gpu_switch(std::time::Duration::from_secs(120)) {
                    Ok(done) if done.starts_with("Couldn't") => fail(done),
                    Ok(done) => {
                        println!("{done}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => fail(e),
                }
            }
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
    }
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .map(|l| l.split_whitespace().nth(2) == Some("0"))
        })
        .unwrap_or(false)
}

/// Run a fix command. As root, `sudo ` prefixes are dropped; makepkg refuses to run as root.
fn run_fix(cmd: &str) -> bool {
    let cmd = cmd.split("   #").next().unwrap_or(cmd).trim();
    if cmd.starts_with('#') {
        return false;
    }
    let root = is_root();
    if root && cmd.contains("makepkg") {
        eprintln!("         (skipped: makepkg must run as your user)");
        return false;
    }
    let cmd = if root {
        cmd.replace("sudo ", "")
    } else {
        cmd.to_owned()
    };
    std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status()
        .is_ok_and(|s| s.success())
}

fn run_doctor(root: &SysRoot, apply: bool) -> ExitCode {
    let m = Machine::probe(root);
    let findings = doctor::run(root, &m);
    if findings.is_empty() {
        println!("✔ No problems found on {}.", m.dmi.model());
        return ExitCode::SUCCESS;
    }
    for f in &findings {
        let tag = match f.severity {
            doctor::Severity::Error => "\x1b[1;31mERROR\x1b[0m",
            doctor::Severity::Warn => "\x1b[1;33m WARN\x1b[0m",
            doctor::Severity::Info => "\x1b[1;34m INFO\x1b[0m",
        };
        println!("{tag}  {}  \x1b[2m[{}]\x1b[0m", f.title, f.id);
        println!("       {}", wrap(&f.detail, 7, 88));
        if let Some(fix) = &f.fix {
            let reboot = if fix.needs_reboot { " (then reboot)" } else { "" };
            println!("       \x1b[1mFix:\x1b[0m {}{reboot}", fix.summary);
            for c in &fix.commands {
                println!("         $ {c}");
            }
        }
        println!();
    }
    if apply {
        let mut reboot = false;
        for f in findings
            .iter()
            .filter(|f| f.fix.as_ref().is_some_and(|x| !x.commands.is_empty()))
        {
            let fix = f.fix.as_ref().unwrap();
            println!("\x1b[1m==> {}\x1b[0m", fix.summary);
            let mut ok = true;
            for c in &fix.commands {
                println!("    $ {c}");
                if !run_fix(c) {
                    ok = false;
                    break;
                }
            }
            println!(
                "    {}",
                if ok {
                    "\x1b[32mdone\x1b[0m"
                } else {
                    "\x1b[31mfailed\x1b[0m"
                }
            );
            reboot |= ok && fix.needs_reboot;
        }
        if reboot {
            println!("\nReboot to finish applying the fixes.");
        }
        return ExitCode::SUCCESS;
    }
    if findings.iter().any(|f| f.severity == doctor::Severity::Error) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Greedy word wrap with a hanging indent.
fn wrap(text: &str, indent: usize, width: usize) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut col = indent;
    for word in text.split_whitespace() {
        if col > indent && col + 1 + word.len() > width {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', indent));
            col = indent;
        } else if col > indent {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += word.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::help_lines;

    #[test]
    fn every_command_has_its_own_help() {
        for cmd in [
            "info", "status", "profile", "gpu", "doctor", "dump", "ping", "fans", "limits", "battery", "hw",
            "light", "lid",
        ] {
            let h = help_lines(cmd);
            assert!(
                h.lines().next().is_some_and(|l| l.trim_start().starts_with(cmd)),
                "{cmd}"
            );
            // Only that command's lines.
            assert!(
                h.lines()
                    .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
                    .all(|l| l.split_whitespace().next() == Some(cmd)),
                "{cmd}"
            );
        }
        assert!(help_lines("nothing").is_empty());
        assert!(help_lines("fans").contains("temp:percent"));
    }
}
