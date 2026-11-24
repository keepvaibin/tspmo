//! `tspmo` — Thermal & System Power Management Optimizer, command line.

mod dump;
mod info;

use std::process::ExitCode;
use tspmo_core::{SysRoot, doctor, machine::Machine};

const USAGE: &str = "\
tspmo — Thermal & System Power Management Optimizer

USAGE:
    tspmo <command> [options]

COMMANDS:
    info              What this laptop is and which controls it exposes
    doctor            Find what breaks features or costs battery, with exact fixes
    dump <dir>        Capture an anonymised snapshot of this machine's kernel interfaces
                      (attach it to a hardware-support issue)
    help              Show this message

ENVIRONMENT:
    TSPMO_SYSROOT     Read kernel interfaces from this directory instead of /
                      (point it at a `tspmo dump` to inspect another machine)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = SysRoot::from_env();
    match args.first().map(String::as_str) {
        Some("info") => {
            info::print(&Machine::probe(&root));
            ExitCode::SUCCESS
        }
        Some("doctor") => run_doctor(&root),
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
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("tspmo {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("tspmo: unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_doctor(root: &SysRoot) -> ExitCode {
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
