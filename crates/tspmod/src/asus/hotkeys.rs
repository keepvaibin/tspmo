//! Vendor hotkeys and keyboard-light idle timing, on one thread that sleeps in `poll(2)`.
//!
//! It watches the ASUS input devices read-only (never grabbing them), publishes every key press
//! as `events::Key`, and acts on the Aura key. The same `poll` carries the idle timeout of the
//! keyboard backlight: its deadline is the poll timeout, so nothing wakes while the keyboard is
//! idle or dark.

use super::Asus;
use crate::events;
use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
use rustix::fs::{Mode, OFlags, open};
use rustix::io::{Errno, read, write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tspmo_core::asus::hotkeys::{self, EVENT_SIZE, Hotkey, Source};

static WAKE: OnceLock<OwnedFd> = OnceLock::new();

/// Make the thread look at its devices and timers again.
pub fn rearm() {
    if let Some(fd) = WAKE.get() {
        let _ = write(fd, &1u64.to_ne_bytes());
    }
}

struct Dev {
    fd: OwnedFd,
    /// `None` for devices that only count as activity (touchpad).
    source: Option<Source>,
}

fn open_nonblock(path: &str) -> Option<OwnedFd> {
    open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()
}

fn open_devices(a: &Asus) -> Vec<Dev> {
    let mut v: Vec<Dev> = hotkeys::input_devices(&a.root)
        .into_iter()
        .filter_map(|d| {
            Some(Dev {
                fd: open_nonblock(&d.node)?,
                source: Some(d.source),
            })
        })
        .collect();
    // Without a backlight there is no idle timeout: no reason to watch the touchpad.
    if a.lighting.is_some() {
        v.extend(hotkeys::touchpads(&a.root).iter().filter_map(|p| {
            Some(Dev {
                fd: open_nonblock(p)?,
                source: None,
            })
        }));
    }
    v
}

pub fn start(a: Arc<Asus>) {
    let Ok(fd) = eventfd(0, EventfdFlags::NONBLOCK | EventfdFlags::CLOEXEC) else {
        return;
    };
    let _ = WAKE.set(fd);
    super::spawn("asus-input", move || run(&a));
}

fn run(a: &Asus) {
    let Some(wake) = WAKE.get() else { return };
    let mut last_input = Instant::now();
    'outer: loop {
        let mut devs = open_devices(a);
        // sysfs signals a hardware brightness change with POLLPRI.
        let hw = a
            .lighting
            .as_ref()
            .and_then(|l| l.hw_changed_path())
            .and_then(|p| std::fs::File::open(p).ok());
        loop {
            // (index into devs) for each polled descriptor after the first two.
            let mut fds = vec![PollFd::new(wake, PollFlags::IN)];
            if let Some(f) = &hw {
                fds.push(PollFd::new(f, PollFlags::PRI | PollFlags::ERR));
            }
            let hw_slots = fds.len();
            fds.extend(devs.iter().map(|d| PollFd::new(&d.fd, PollFlags::IN)));

            let timeout = a
                .lighting
                .as_ref()
                .and_then(|l| l.idle_armed())
                .map(|secs| Duration::from_secs(u64::from(secs)).saturating_sub(last_input.elapsed()))
                .map(|d| Timespec {
                    tv_sec: d.as_secs() as i64,
                    tv_nsec: i64::from(d.subsec_nanos()),
                });
            match poll(&mut fds, timeout.as_ref()) {
                Ok(0) => {
                    if let Some(l) = &a.lighting {
                        l.idle_dim();
                        a.emit(super::lighting::changed);
                    }
                    continue;
                }
                Err(Errno::INTR) => continue,
                Err(_) => return,
                Ok(_) => {}
            }

            if fds[0].revents().intersects(PollFlags::IN) {
                let mut b = [0u8; 8];
                let _ = read(wake, &mut b);
                continue 'outer;
            }
            if hw_slots == 2 && !fds[1].revents().is_empty() {
                use std::io::{Read, Seek, SeekFrom};
                if let Some(mut f) = hw.as_ref() {
                    // Reading re-arms the notification.
                    let mut s = String::new();
                    let _ = f.seek(SeekFrom::Start(0));
                    let _ = f.read_to_string(&mut s);
                }
                if a.lighting.as_ref().is_some_and(|l| l.brightness_changed()) {
                    a.emit(super::lighting::changed);
                }
            }
            let ready: Vec<usize> = fds[hw_slots..]
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.revents().is_empty())
                .map(|(i, _)| i)
                .collect();
            drop(fds);

            let mut dead = Vec::new();
            for i in ready {
                if !drain(a, &devs[i], &mut last_input) {
                    dead.push(i);
                }
            }
            // A device that went away (unplugged, re-enumerated): forget it; resume rescans.
            for i in dead.into_iter().rev() {
                devs.remove(i);
            }
        }
    }
}

/// Read every pending event of one device. `false` if the device is gone.
fn drain(a: &Asus, d: &Dev, last_input: &mut Instant) -> bool {
    let mut buf = [0u8; EVENT_SIZE];
    loop {
        match read(d.fd.as_fd(), &mut buf) {
            Ok(n) if n == EVENT_SIZE => {}
            Ok(_) => return false,
            Err(Errno::AGAIN) => return true,
            Err(Errno::INTR) => continue,
            Err(_) => return false,
        }
        *last_input = Instant::now();
        if let Some(l) = &a.lighting {
            if l.idle_wake() {
                a.emit(super::lighting::changed);
            }
        }
        if let (Some(code), Some(source)) = (hotkeys::parse_event(&buf).press(), d.source) {
            pressed(a, source, code);
        }
    }
}

fn pressed(a: &Asus, source: Source, code: u16) {
    events::publish(&events::Event::Key(code));
    match hotkeys::decode(source, code) {
        Some(Hotkey::AuraNext) => a.aura_key(true),
        Some(Hotkey::AuraPrev) => a.aura_key(false),
        // The kernel cycles the thermal mode itself and the daemon follows it; the OSD comes with that.
        Some(Hotkey::Fan | Hotkey::Rog) | None => {}
    }
}
