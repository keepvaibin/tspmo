//! One window at a time. The first instance owns the abstract socket `@tspmo-gui-<uid>`; a second
//! launch tells it to quit and exits, so the ROG key opens and closes the popup.

use std::io::{BufRead, BufReader, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

fn addr() -> std::io::Result<SocketAddr> {
    let uid = rustix::process::getuid().as_raw();
    // TSPMO_GUI_ID lets tests run a second, independent popup.
    let id = std::env::var("TSPMO_GUI_ID").unwrap_or_default();
    SocketAddr::from_abstract_name(format!("tspmo-gui-{uid}{id}"))
}

pub enum Role {
    /// We are the window. Keep the listener for the second press.
    Window(UnixListener),
    /// Another instance was told to quit.
    Toggled,
}

pub fn claim() -> Role {
    let Ok(a) = addr() else {
        return Role::Toggled;
    };
    match UnixListener::bind_addr(&a) {
        Ok(l) => Role::Window(l),
        Err(_) => {
            if let Ok(mut s) = UnixStream::connect_addr(&a) {
                let _ = s.write_all(b"quit\n");
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

/// Wait for "quit" on a thread that blocks in the kernel (no wakeups while idle).
pub fn serve(l: UnixListener, on_quit: fn()) {
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for s in l.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(s).read_line(&mut line).is_ok() && line.trim() == "quit" {
                    on_quit();
                }
            }
        })
        .ok();
}
