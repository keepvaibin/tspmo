//! `tspmo-gui --snapshot <file.ppm> [--window popup|fans|screen|keys|system] [--scale 1.5]`:
//! render one window once, headless (no compositor, no window), and write it as a binary PPM over
//! a checkerboard so transparency shows. `TSPMO_FONT="Family,pt"` and `TSPMO_THEME` apply. For
//! design review and tests; it never touches the desktop.

use crate::app::{self, Kind};
use crate::px::Px;
use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent, WindowProperties};
use slint::{LogicalSize, PhysicalSize};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
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

/// A window that renders into memory and remembers the size its layout asks for.
struct SnapWin {
    window: slint::Window,
    renderer: SoftwareRenderer,
    want: Cell<(f32, f32)>,
    size: Cell<PhysicalSize>,
    scale: f32,
    resized: Cell<bool>,
}

impl WindowAdapter for SnapWin {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn set_size(&self, _: slint::WindowSize) {}
    fn update_window_properties(&self, p: WindowProperties<'_>) {
        let c = p.layout_constraints();
        let (mut w, mut h) = (c.preferred.width, c.preferred.height);
        if let Some(m) = c.min {
            w = w.max(m.width);
            h = h.max(m.height);
        }
        if let Some(m) = c.max {
            w = w.min(m.width);
            h = h.min(m.height);
        }
        self.want.set((w.ceil(), h.ceil()));
        self.size.set(PhysicalSize::new(
            (w.ceil() * self.scale).round() as u32,
            (h.ceil() * self.scale).round() as u32,
        ));
        // Telling the window its size here, inside its own property tracker, would break the tracker.
        self.resized.set(true);
    }
}

thread_local! {
    static SNAP: RefCell<Option<Rc<SnapWin>>> = const { RefCell::new(None) };
}

struct Headless {
    queue: Arc<Queue>,
    scale: f32,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let scale = self.scale;
        let w = Rc::new_cyclic(|me: &Weak<SnapWin>| SnapWin {
            window: slint::Window::new(me.clone()),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::NewBuffer),
            want: Cell::new((0.0, 0.0)),
            size: Cell::new(PhysicalSize::new(0, 0)),
            scale,
            resized: Cell::new(false),
        });
        w.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        SNAP.with(|s| *s.borrow_mut() = Some(w.clone()));
        Ok(w)
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn run(args: &[String]) -> Result<(), String> {
    let path = args
        .first()
        .ok_or("usage: --snapshot <file.ppm> [--window popup|fans|screen|keys|system] [--scale 1.5]")?;
    let kind = arg(args, "--window")
        .or_else(|| arg(args, "--section"))
        .map_or(Some(Kind::Popup), |n| Kind::parse(&n))
        .ok_or("unknown window")?;
    let scale: f32 = arg(args, "--scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let queue = Arc::new(Queue::default());
    slint::platform::set_platform(Box::new(Headless {
        queue: queue.clone(),
        scale,
    }))
    .map_err(|e| e.to_string())?;
    app::start(&[kind], crate::theme::is_dark(), crate::theme::fonts())?;

    // Let the daemon's answers arrive and the animations settle.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        slint::platform::update_timers_and_animations();
        if let Some(w) = SNAP.with(|s| s.borrow().clone())
            && w.resized.replace(false)
        {
            let (lw, lh) = w.want.get();
            w.window.dispatch_event(WindowEvent::Resized { size: LogicalSize::new(lw, lh) });
        }
        // Drawing is what keeps Slint's layout tracking alive, as in a real window.
        if let Some(w) = SNAP.with(|s| s.borrow().clone()) {
            let (pw, ph) = (w.size.get().width as usize, w.size.get().height as usize);
            if pw > 0 && ph > 0 {
                let mut scratch = vec![Px::default(); pw * ph];
                w.renderer.render(&mut scratch, pw);
            }
        }
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

    let win = SNAP.with(|s| s.borrow().clone()).ok_or("no window")?;
    let size = win.size.get();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut buf = vec![Px::default(); w * h];
    win.window.request_redraw();
    win.renderer.render(&mut buf, w);
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
    let (lw, lh) = win.want.get();
    println!("{lw}x{lh} logical, {w}x{h} px");
    std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))
}
