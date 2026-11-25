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

/// Kernel uevents (no udev daemon needed), to notice input devices appearing after start.
fn uevent_socket() -> Option<OwnedFd> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, bind, netlink, socket_with};
    let fd = socket_with(
        AddressFamily::NETLINK,
        SocketType::RAW,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        Some(netlink::KOBJECT_UEVENT),
    )
    .ok()?;
    bind(&fd, &netlink::SocketAddrNetlink::new(0, 1)).ok()?;
    Some(fd)
}

/// Drain the socket; true if an input device was added or removed.
fn input_changed(fd: &OwnedFd) -> bool {
    let mut buf = [0u8; 4096];
    let mut hit = false;
    while let Ok(n) = read(fd, &mut buf) {
        if n == 0 {
            break;
        }
        hit |= is_input_hotplug(&buf[..n]);
    }
    hit
}

fn is_input_hotplug(msg: &[u8]) -> bool {
    let mut change = false;
    let mut input = false;
    for f in msg.split(|b| *b == 0) {
        match f {
            b"ACTION=add" | b"ACTION=remove" => change = true,
            b"SUBSYSTEM=input" => input = true,
            _ => {}
        }
    }
    change && input
}

fn run(a: &Asus) {
    let Some(wake) = WAKE.get() else { return };
    let uevents = uevent_socket();
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
            if let Some(u) = &uevents {
                fds.push(PollFd::new(u, PollFlags::IN));
            }
            let has_u = uevents.is_some();
            let hw_slot = fds.len();
            if let Some(f) = &hw {
                fds.push(PollFd::new(f, PollFlags::PRI | PollFlags::ERR));
            }
            let hw_slots = fds.len();
            // The touchpad only matters as activity for the idle timeout (and to undo it): while
            // that timer is neither running nor holding the light dark, its events are not read.
            let pads = a
                .lighting
                .as_ref()
                .is_some_and(|l| l.idle_armed().is_some() || l.is_dimmed());
            let polled: Vec<usize> = (0..devs.len())
                .filter(|i| pads || devs[*i].source.is_some())
                .collect();
            fds.extend(polled.iter().map(|i| PollFd::new(&devs[*i].fd, PollFlags::IN)));

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
            if has_u && !fds[1].revents().is_empty() {
                if let Some(u) = &uevents {
                    if input_changed(u) {
                        continue 'outer;
                    }
                }
            }
            if hw.is_some() && !fds[hw_slot].revents().is_empty() {
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
                .map(|(i, _)| polled[i])
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
        // Fn+F5, or its Fn-lock variant: tspmod owns it (the kernel's own step of the thermal
        // mode is not followed, see `fankey.rs`).
        Some(Hotkey::Fan) => events::publish(&events::Event::FanKey(match source {
            Source::Keyboard => "keyboard",
            Source::Wmi => "wmi",
        })),
        Some(Hotkey::Rog) | None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tspmo_core::asus::hotkeys::{EV_KEY, KEY_PROG3};
    use tspmo_core::asus::testfix::Fix;

    fn event(code: u16, value: i32) -> [u8; EVENT_SIZE] {
        let mut b = [0u8; EVENT_SIZE];
        b[16..18].copy_from_slice(&EV_KEY.to_ne_bytes());
        b[18..20].copy_from_slice(&code.to_ne_bytes());
        b[20..24].copy_from_slice(&value.to_ne_bytes());
        b
    }

    #[test]
    fn uevents_filter_input_hotplug() {
        let add = b"add@/devices/x/input/input9/event9\0ACTION=add\0DEVPATH=/devices/x\0SUBSYSTEM=input\0";
        assert!(is_input_hotplug(add));
        let usb = b"add@/devices/x\0ACTION=add\0SUBSYSTEM=usb\0";
        assert!(!is_input_hotplug(usb));
        let bind = b"bind@/devices/x\0ACTION=bind\0SUBSYSTEM=input\0";
        assert!(!is_input_hotplug(bind));
    }

    #[test]
    fn aura_key_cycles_the_effect_and_activity_wakes_the_keyboard() {
        let fx = Fix::new("hotkeys-aura");
        let root = fx.root();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw2"), b"").unwrap();
        let a = Asus::build(&root, fx.dir().join("state"));
        let l = a.lighting.as_ref().unwrap();

        // press, release, press: two cycles
        let path = fx.dir().join("event");
        let mut f = std::fs::File::create(&path).unwrap();
        for (code, v) in [(KEY_PROG3, 1), (KEY_PROG3, 0), (KEY_PROG3, 1)] {
            f.write_all(&event(code, v)).unwrap();
        }
        drop(f);
        let dev = Dev {
            fd: open_nonblock(path.to_str().unwrap()).unwrap(),
            source: Some(Source::Keyboard),
        };
        let mut last = Instant::now() - Duration::from_secs(60);
        let alive = drain(&a, &dev, &mut last);
        assert!(!alive, "end of file counts as a vanished device");
        assert!(last.elapsed() < Duration::from_secs(5));
        // Cycled twice: static -> breathe -> rainbow-cycle.
        assert_eq!(l.cycle_effect(false).unwrap(), "breathe");
    }

    #[test]
    fn input_wakes_a_dimmed_keyboard() {
        let fx = Fix::new("hotkeys-idle");
        fx.set("/sys/class/leds/asus::kbd_backlight/brightness", "2\n");
        let a = Asus::build(&fx.root(), fx.dir().join("state"));
        let l = a.lighting.as_ref().unwrap();
        l.set_idle(10);
        l.idle_dim();
        assert_eq!(fx.get("/sys/class/leds/asus::kbd_backlight/brightness"), "0");
        let path = fx.dir().join("event");
        std::fs::write(&path, event(30, 1)).unwrap();
        let dev = Dev {
            fd: open_nonblock(path.to_str().unwrap()).unwrap(),
            source: None,
        };
        drain(&a, &dev, &mut Instant::now());
        assert_eq!(fx.get("/sys/class/leds/asus::kbd_backlight/brightness"), "2");
    }
}
