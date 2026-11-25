//! One window at a time. The first instance owns the abstract socket `@tspmo-gui-<uid>`; a second
//! launch hands it a message and exits, so the ROG key opens and closes the popup.

use std::io::{BufRead, BufReader, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

fn addr() -> std::io::Result<SocketAddr> {
    addr_named("tspmo-gui")
}

/// `@<name>-<uid>`; TSPMO_GUI_ID lets tests run a second, independent instance.
pub fn addr_named(name: &str) -> std::io::Result<SocketAddr> {
    let uid = rustix::process::getuid().as_raw();
    let id = std::env::var("TSPMO_GUI_ID").unwrap_or_default();
    SocketAddr::from_abstract_name(format!("{name}-{uid}{id}"))
}

pub enum Role {
    /// We are the window. Keep the listener for the second press.
    Window(UnixListener),
    /// Another instance was told to quit.
    Toggled,
}

/// Become the window, or hand `msg` (`toggle`, `open fans`) to the one that exists.
pub fn claim(msg: &str) -> Role {
    let Ok(a) = addr() else {
        return Role::Toggled;
    };
    match UnixListener::bind_addr(&a) {
        Ok(l) => Role::Window(l),
        Err(_) => {
            if let Ok(mut s) = UnixStream::connect_addr(&a) {
                let _ = s.write_all(format!("{msg}\n").as_bytes());
                Role::Toggled
            } else {
                // Unreachable owner (it just exited): become the window without a socket.
                Role::Window(UnixListener::bind_addr(&a).unwrap_or_else(|_| {
                    UnixListener::bind_addr(
                        &SocketAddr::from_abstract_name("tspmo-gui-fallback").expect("name"),
                    )
                    .expect("fallback socket")
                }))
            }
        }
    }
}

/// Wait for messages on a thread that blocks in the kernel (no wakeups while idle).
pub fn serve(l: UnixListener, on_msg: fn(&str)) {
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for s in l.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(s).read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    on_msg(line.trim());
                }
            }
        })
        .ok();
}
