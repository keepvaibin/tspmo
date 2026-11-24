//! `tspmo-gui --snapshot <file.ppm> [--section main|fans]`: render the popup once, headless
//! (no compositor, no window), and write it as a binary PPM. For design review and tests; it
//! never touches the desktop.

use crate::{Main, Th, app, theme};
use slint::ComponentHandle;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
}

struct Proxy(Arc<Queue>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }
    fn invoke_from_event_loop(&self, event: Job) -> Result<(), slint::EventLoopError> {
        self.0
            .jobs
            .lock()
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)?
            .push_back(event);
        self.0.wake.notify_all();
        Ok(())
    }
}

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    queue: Arc<Queue>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

/// Straight BGRA pixel for the buffer; transparent where nothing is drawn.
#[derive(Clone, Copy, Default)]
struct Px {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl TargetPixel for Px {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let ia = u16::from(255 - c.alpha);
        self.r = (u16::from(self.r) * ia / 255) as u8 + c.red;
        self.g = (u16::from(self.g) * ia / 255) as u8 + c.green;
        self.b = (u16::from(self.b) * ia / 255) as u8 + c.blue;
        self.a =
            (u16::from(self.a) + u16::from(c.alpha) - u16::from(self.a) * u16::from(c.alpha) / 255) as u8;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Px { r, g, b, a: 255 }
    }
    fn background() -> Self {
        Px::default()
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let path = args
        .first()
        .ok_or("usage: --snapshot <file.ppm> [--section main|fans]")?;
    let section = args
        .iter()
        .position(|a| a == "--section")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let queue = Arc::new(Queue::default());
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        queue: queue.clone(),
    }))
    .map_err(|e| e.to_string())?;
    let win = Main::new().map_err(|e| e.to_string())?;
    win.global::<Th>().set_dark(theme::is_dark());
    app::start(&win, section);
    let scale = 1.0_f32;
    window.set_size(slint::PhysicalSize::new(
        (468.0 * scale) as u32,
        (640.0 * scale) as u32,
    ));
    win.show().map_err(|e| e.to_string())?;

    // Let the daemon's answers arrive and the animations settle.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        slint::platform::update_timers_and_animations();
        loop {
            let job = queue.jobs.lock().map_err(|_| "poisoned")?.pop_front();
            match job {
                Some(j) => j(),
                None => break,
            }
        }
        let guard = queue.jobs.lock().map_err(|_| "poisoned")?;
        let _ = queue.wake.wait_timeout(guard, Duration::from_millis(20));
    }
    slint::platform::update_timers_and_animations();

    let (w, h) = (window.size().width as usize, window.size().height as usize);
    let mut buf = vec![Px::default(); w * h];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, w);
    });
    // Over a checkerboard, so transparency shows.
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for (i, p) in buf.iter().enumerate() {
        let (x, y) = (i % w, i / w);
        let bg: u16 = if (x / 8 + y / 8) % 2 == 0 { 255 } else { 200 };
        let ia = u16::from(255 - p.a);
        for c in [p.r, p.g, p.b] {
            out.push((u16::from(c) + bg * ia / 255).min(255) as u8);
        }
    }
    std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))
}
