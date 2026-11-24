//! Acting inside the user's desktop session from the root daemon: on-screen display (OSD),
//! refresh rate, brightness/dimming, colour.
//!
//! There is no resident helper. When something has to happen in the session, the daemon finds
//! the active graphical session (logind), and runs one short command as that user with the
//! session's environment (`XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, the user bus). Nothing runs
//! while nothing changes.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tspmo_sys::SysRoot;

/// The user's graphical session on the seat the keyboard and panel belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: String,
    /// `KDE`, `GNOME`, ... as the display manager reported it (may be empty).
    pub desktop: String,
    pub runtime_dir: String,
    pub wayland_display: Option<String>,
}

impl Session {
    pub fn is_kde(&self) -> bool {
        self.desktop.eq_ignore_ascii_case("kde")
    }
}

/// One line of logind's `ListSessions`, with the properties tspmo filters on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub uid: u32,
    pub user: String,
    pub seat: String,
    pub class: String,
    pub kind: String,
    pub active: bool,
    pub desktop: String,
}

/// The session that owns the screen: active, a real user session on the first seat, graphical.
pub fn pick(sessions: &[Candidate]) -> Option<&Candidate> {
    sessions.iter().find(|s| {
        s.active
            && s.class == "user"
            && s.seat == "seat0"
            && matches!(s.kind.as_str(), "wayland" | "x11" | "mir")
    })
}

/// `/run/user/<uid>/wayland-N` sockets (not the `.lock` files), lowest first.
pub fn wayland_socket(root: &SysRoot, runtime_dir: &str) -> Option<String> {
    root.list(runtime_dir)
        .into_iter()
        .find(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
}

/// `(uid, gid, home)` of a user from `/etc/passwd`.
pub fn passwd_entry(root: &SysRoot, user: &str) -> Option<(u32, u32, String)> {
    let text = root.read_opt("/etc/passwd")?;
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 6 && f[0] == user)
            .then(|| Some((f[2].parse().ok()?, f[3].parse().ok()?, f[5].to_owned())))?
    })
}

/// Ask logind for the active graphical session.
pub fn active(root: &SysRoot) -> Option<Session> {
    let conn = zbus::blocking::Connection::system().ok()?;
    let manager = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .ok()?;
    #[allow(clippy::type_complexity)]
    let listed: Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)> =
        manager.call("ListSessions", &()).ok()?;
    let mut candidates = Vec::new();
    for (_id, uid, user, seat, path) in listed {
        let Ok(s) = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.login1",
            path,
            "org.freedesktop.login1.Session",
        ) else {
            continue;
        };
        candidates.push(Candidate {
            uid,
            user,
            seat,
            class: s.get_property("Class").unwrap_or_default(),
            kind: s.get_property("Type").unwrap_or_default(),
            active: s.get_property("Active").unwrap_or(false),
            desktop: s.get_property("Desktop").unwrap_or_default(),
        });
    }
    let c = pick(&candidates)?;
    let (_, gid, home) = passwd_entry(root, &c.user)?;
    let runtime_dir = format!("/run/user/{}", c.uid);
    Some(Session {
        uid: c.uid,
        gid,
        user: c.user.clone(),
        home,
        desktop: c.desktop.clone(),
        wayland_display: wayland_socket(root, &runtime_dir),
        runtime_dir,
    })
}

/// Read a pipe to the end on its own thread, so a chatty child can't block on a full pipe.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_string(&mut s);
        }
        s
    })
}

#[derive(Debug)]
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `program args...` as the session's user and wait for it (killing it after `timeout`).
pub fn run(s: &Session, program: &str, args: &[&str], timeout: Duration) -> std::io::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin")
        .env("HOME", &s.home)
        .env("USER", &s.user)
        .env("XDG_RUNTIME_DIR", &s.runtime_dir)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/bus", s.runtime_dir),
        )
        .env("LANG", "C.UTF-8")
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", &s.desktop)
        // Qt tools would otherwise look for an X display.
        .env("QT_QPA_PLATFORM", "wayland")
        .current_dir("/")
        .uid(s.uid)
        .gid(s.gid)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(w) = &s.wayland_display {
        cmd.env("WAYLAND_DISPLAY", w);
    }
    let mut child = cmd.spawn()?;
    let (t_out, t_err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Ok(Output {
        ok: status.is_some_and(|s| s.success()),
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

/// Run a command in the active session; `None` when nobody is logged in graphically or the
/// command couldn't be started.
pub fn run_in_session(root: &SysRoot, program: &str, args: &[&str]) -> Option<Output> {
    let s = active(root)?;
    run(&s, program, args, Duration::from_secs(10)).ok()
}

/// Show a transient on-screen indicator (like the brightness OSD). `value`: 0–100 for a bar,
/// or `None` for text only. Never blocks the caller.
pub fn osd(icon: &str, text: &str, value: Option<u8>) {
    let (icon, text) = (icon.to_owned(), text.to_owned());
    let _ = std::thread::Builder::new().name("osd".into()).spawn(move || {
        let root = SysRoot::from_env();
        if !root.is_real() || std::env::var("TSPMOD_BUS").is_ok() {
            eprintln!("tspmod: osd [{icon}] {text} {value:?}");
            return;
        }
        let Some(s) = active(&root) else { return };
        // tspmo's own OSD (bottom centre, about 40 ms): `tspmo-gui --osd <icon> <text> [value]`.
        let mut args = vec!["--osd", icon.as_str(), text.as_str()];
        let v = value.map(|v| v.min(100).to_string());
        if let Some(v) = &v {
            args.push(v);
        }
        let out = match run(&s, "tspmo-gui", &args, Duration::from_secs(3)) {
            Ok(o) if o.ok => Ok(o),
            // The binary is missing or failed: Plasma's own OSD, where there is one.
            first if s.is_kde() => {
                let line = match value {
                    Some(v) => format!("{text} {}%", v.min(100)),
                    None => text.clone(),
                };
                let fallback = run(
                    &s,
                    "busctl",
                    &[
                        "--user",
                        "--timeout=2",
                        "call",
                        "org.kde.plasmashell",
                        "/org/kde/osdService",
                        "org.kde.osdService",
                        "showText",
                        "ss",
                        &icon,
                        &line,
                    ],
                    Duration::from_secs(3),
                );
                if fallback.as_ref().is_ok_and(|o| o.ok) {
                    fallback
                } else {
                    first
                }
            }
            other => other,
        };
        match out {
            Ok(o) if o.ok => {}
            Ok(o) => eprintln!("tspmod: osd failed: {}", o.stderr.trim()),
            Err(e) => eprintln!("tspmod: osd: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn cand(uid: u32, seat: &str, class: &str, kind: &str, active: bool) -> Candidate {
        Candidate {
            uid,
            user: format!("u{uid}"),
            seat: seat.into(),
            class: class.into(),
            kind: kind.into(),
            active,
            desktop: "KDE".into(),
        }
    }

    #[test]
    fn picks_the_active_graphical_seat0_session() {
        let list = [
            cand(1000, "", "manager", "unspecified", true),
            cand(1001, "seat0", "user", "wayland", false), // fast-user-switched away
            cand(1000, "seat0", "user", "tty", true),
            cand(1000, "seat0", "user", "wayland", true),
        ];
        assert_eq!(pick(&list), Some(&list[3]));
        assert_eq!(pick(&list[..3]), None);
    }

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn wayland_socket_ignores_lock_files() {
        let d = std::env::temp_dir().join(format!("tspmo-sess-{}", std::process::id()));
        let _g = Tmp(d.clone());
        std::fs::create_dir_all(d.join("run/user/1000")).unwrap();
        for f in ["wayland-1.lock", "wayland-1", "bus"] {
            std::fs::write(d.join("run/user/1000").join(f), "").unwrap();
        }
        let root = SysRoot::new(&d);
        assert_eq!(
            wayland_socket(&root, "/run/user/1000").as_deref(),
            Some("wayland-1")
        );
        assert_eq!(wayland_socket(&root, "/run/user/42"), None);
    }

    #[test]
    fn passwd_lookup() {
        let d = std::env::temp_dir().join(format!("tspmo-pw-{}", std::process::id()));
        let _g = Tmp(d.clone());
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(
            d.join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\nana:x:1000:1000:Ana:/home/ana:/bin/zsh\n",
        )
        .unwrap();
        let root = SysRoot::new(&d);
        assert_eq!(passwd_entry(&root, "ana"), Some((1000, 1000, "/home/ana".into())));
        assert_eq!(passwd_entry(&root, "bob"), None);
    }

    #[test]
    fn run_times_out_and_captures() {
        let me = Session {
            uid: std::fs::metadata("/proc/self").unwrap().uid(),
            gid: std::fs::metadata("/proc/self").unwrap().gid(),
            user: "t".into(),
            home: "/tmp".into(),
            desktop: String::new(),
            runtime_dir: "/tmp".into(),
            wayland_display: None,
        };
        let o = run(&me, "echo", &["hi"], Duration::from_secs(2)).unwrap();
        assert!(o.ok && o.stdout.trim() == "hi");
        let t = Instant::now();
        let o = run(&me, "sleep", &["5"], Duration::from_millis(100)).unwrap();
        assert!(!o.ok && t.elapsed() < Duration::from_secs(2));
    }
}
