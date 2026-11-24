//! A Slint platform on Wayland: the popup is a `wlr-layer-shell` surface anchored bottom-right,
//! the other windows are ordinary toplevels, all drawn by the software renderer into `wl_shm`
//! buffers. Fractional scale (`wp_fractional_scale_v1` + `wp_viewporter`) keeps text sharp.
//!
//! The loop is single threaded. Wayland callbacks only queue work (`State::out`); the loop
//! then hands it to Slint with no Wayland state borrowed, so Slint callbacks may call back into
//! the adapters (show, hide, resize) freely.

mod input;
mod state;

use crate::px::Px;
use slint::platform::software_renderer::{RepaintBufferType, SoftwareRenderer};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent, WindowProperties};
use slint::{LogicalSize, PhysicalSize, PlatformError};
use smithay_client_toolkit::reexports::calloop::channel;
use state::State;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The tray-style flyout: layer surface, bottom-right, takes the keyboard.
    Popup,
    /// A short note: layer surface, bottom-centre, never takes input.
    Osd,
    /// A normal window.
    Window,
}

thread_local! {
    static NEXT: Cell<Kind> = const { Cell::new(Kind::Window) };
    static APP_ID: RefCell<String> = const { RefCell::new(String::new()) };
}

/// What kind of window the next `Component::new()` creates.
pub fn next_kind(k: Kind) {
    NEXT.set(k);
}

type Job = Box<dyn FnOnce() + Send>;

enum Msg {
    Job(Job),
    Quit,
}

struct Proxy(Mutex<channel::Sender<Msg>>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.send(Msg::Quit)
    }
    fn invoke_from_event_loop(&self, event: Job) -> Result<(), slint::EventLoopError> {
        self.send(Msg::Job(event))
    }
}

impl Proxy {
    fn send(&self, m: Msg) -> Result<(), slint::EventLoopError> {
        self.0
            .lock()
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)?
            .send(m)
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
}

/// Everything the adapters and the loop share.
struct Wl {
    st: RefCell<State>,
    wins: RefCell<Vec<Weak<Win>>>,
    next_id: Cell<usize>,
    quit_on_last_closed: Cell<bool>,
}

pub struct WlPlatform {
    wl: Rc<Wl>,
    tx: channel::Sender<Msg>,
    rx: RefCell<Option<channel::Channel<Msg>>>,
    ev: RefCell<Option<smithay_client_toolkit::reexports::calloop::EventLoop<'static, State>>>,
}

/// Connect and bind the globals. `None` when there is no compositor or no layer-shell.
pub fn connect(app_id: &str) -> Option<WlPlatform> {
    APP_ID.with_borrow_mut(|a| *a = app_id.to_owned());
    let (tx, rx) = channel::channel::<Msg>();
    let (st, ev) = State::connect()?;
    Some(WlPlatform {
        wl: Rc::new(Wl {
            st: RefCell::new(st),
            wins: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            quit_on_last_closed: Cell::new(true),
        }),
        tx,
        rx: RefCell::new(Some(rx)),
        ev: RefCell::new(Some(ev)),
    })
}

impl Platform for WlPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let kind = NEXT.replace(Kind::Window);
        let id = self.wl.next_id.get();
        self.wl.next_id.set(id + 1);
        let win = Rc::new_cyclic(|w: &Weak<Win>| Win {
            window: slint::Window::new(w.clone()),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(
                RepaintBufferType::ReusedBuffer,
            ),
            kind,
            id,
            wl: self.wl.clone(),
            visible: Cell::new(false),
            redraw: Cell::new(true),
            phys: Cell::new(PhysicalSize::new(0, 0)),
            want: Cell::new(None),
            title: RefCell::new(String::new()),
            canvas: RefCell::new(Vec::new()),
        });
        self.wl.wins.borrow_mut().push(Rc::downgrade(&win));
        Ok(win)
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(Mutex::new(self.tx.clone()))))
    }

    fn set_event_loop_quit_on_last_window_closed(&self, quit: bool) {
        self.wl.quit_on_last_closed.set(quit);
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut ev = self.ev.borrow_mut().take().ok_or(PlatformError::NoEventLoopProvider)?;
        let rx = self.rx.borrow_mut().take().ok_or(PlatformError::NoEventLoopProvider)?;
        ev.handle()
            .insert_source(rx, |e, (), st: &mut State| match e {
                channel::Event::Msg(Msg::Job(j)) => st.jobs.push(j),
                channel::Event::Msg(Msg::Quit) | channel::Event::Closed => st.quit = true,
            })
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        let mut seen_window = false;
        loop {
            slint::platform::update_timers_and_animations();
            // Take the jobs out first: a job may queue another.
            let jobs = std::mem::take(&mut self.wl.st.borrow_mut().jobs);
            for j in jobs {
                j();
            }
            self.wl.process();
            let visible = self.wl.visible_count();
            seen_window |= visible > 0;
            let quit = self.wl.st.borrow().quit
                || (seen_window && visible == 0 && self.wl.quit_on_last_closed.get());
            if quit {
                break;
            }
            // Sleep until the next Wayland event, job or Slint timer. Animations are paced by
            // frame callbacks, so they need no timeout.
            let timeout = slint::platform::duration_until_next_timer_update();
            let mut st = self.wl.st.borrow_mut();
            ev.dispatch(timeout.map(|d| d.max(Duration::from_millis(1))), &mut st)
                .map_err(|e| PlatformError::Other(e.to_string()))?;
        }
        // Drop surfaces before the connection goes away.
        self.wl.st.borrow_mut().destroy_all();
        Ok(())
    }
}

impl Wl {
    fn windows(&self) -> Vec<Rc<Win>> {
        let mut w = self.wins.borrow_mut();
        w.retain(|x| x.strong_count() > 0);
        w.iter().filter_map(Weak::upgrade).collect()
    }

    fn visible_count(&self) -> usize {
        self.windows().iter().filter(|w| w.visible.get()).count()
    }

    fn find(&self, id: usize) -> Option<Rc<Win>> {
        self.windows().into_iter().find(|w| w.id == id)
    }

    /// Hand queued Wayland input to Slint, sync surfaces with window state, draw.
    fn process(&self) {
        let outs = std::mem::take(&mut self.st.borrow_mut().out);
        for o in outs {
            match o {
                state::Out::Event(id, e) => {
                    if let Some(w) = self.find(id) {
                        w.window.dispatch_event(e);
                    }
                }
                state::Out::Scale(id) => {
                    if let Some(w) = self.find(id) {
                        w.rescale();
                    }
                }
                state::Out::Close(id) | state::Out::FocusLost(id) => {
                    if let Some(w) = self.find(id) {
                        // A popup that loses focus (a click elsewhere) goes away.
                        w.window.dispatch_event(WindowEvent::CloseRequested);
                    }
                }
            }
        }
        for w in self.windows() {
            w.sync();
        }
    }
}

/// One Slint window: a layer surface or a toplevel.
pub struct Win {
    window: slint::Window,
    renderer: SoftwareRenderer,
    kind: Kind,
    id: usize,
    wl: Rc<Wl>,
    visible: Cell<bool>,
    redraw: Cell<bool>,
    /// Buffer size in physical pixels.
    phys: Cell<PhysicalSize>,
    /// Logical size the layout asks for, applied by the loop.
    want: Cell<Option<(u32, u32)>>,
    title: RefCell<String>,
    canvas: RefCell<Vec<Px>>,
}

impl Win {
    fn scale120(&self) -> u32 {
        self.wl.st.borrow().scale120(self.id)
    }

    fn physical(&self, (w, h): (u32, u32)) -> PhysicalSize {
        let s = self.scale120();
        PhysicalSize::new((w * s + 60) / 120, (h * s + 60) / 120)
    }

    fn rescale(&self) {
        let sf = self.scale120() as f32 / 120.0;
        self.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: sf });
        if let Some(l) = self.wl.st.borrow().logical(self.id) {
            self.phys.set(self.physical(l));
            self.window.dispatch_event(WindowEvent::Resized {
                size: LogicalSize::new(l.0 as f32, l.1 as f32),
            });
        }
        self.redraw.set(true);
    }

    fn sync(&self) {
        let exists = self.wl.st.borrow().has_surface(self.id);
        if self.visible.get() && !exists {
            // Wait for the layout to say how big the window wants to be.
            let Some(sz) = self.want.get() else { return };
            let title = self.title.borrow().clone();
            let app = APP_ID.with_borrow(Clone::clone);
            self.wl.st.borrow_mut().create(self.id, self.kind, sz, &title, &app);
            self.rescale();
        } else if !self.visible.get() && exists {
            self.wl.st.borrow_mut().destroy(self.id);
            return;
        }
        if !self.visible.get() {
            return;
        }
        let Some(want) = self.want.get() else { return };
        if self.wl.st.borrow().logical(self.id) != Some(want) {
            self.wl.st.borrow_mut().resize(self.id, want);
            self.rescale();
        }
        let animating = self.window.has_active_animations();
        if !(self.redraw.get() || animating) || !self.wl.st.borrow().can_draw(self.id) {
            return;
        }
        self.redraw.set(false);
        self.draw();
    }

    fn draw(&self) {
        let size = self.phys.get();
        let (w, h) = (size.width as usize, size.height as usize);
        if w == 0 || h == 0 {
            return;
        }
        let mut canvas = self.canvas.borrow_mut();
        if canvas.len() != w * h {
            canvas.clear();
            canvas.resize(w * h, Px::default());
            self.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        }
        let region = self.renderer.render(&mut canvas, w);
        let bb = (
            region.bounding_box_origin(),
            region.bounding_box_size(),
        );
        self.wl.st.borrow_mut().present(self.id, &canvas, w as u32, h as u32, bb);
    }
}

impl WindowAdapter for Win {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }

    fn size(&self) -> PhysicalSize {
        self.phys.get()
    }

    fn set_size(&self, size: slint::WindowSize) {
        let sf = self.window.scale_factor();
        let l = size.to_logical(sf);
        self.want.set(Some((l.width.ceil() as u32, l.height.ceil() as u32)));
    }

    fn request_redraw(&self) {
        self.redraw.set(true);
    }

    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        self.visible.set(visible);
        self.redraw.set(true);
        Ok(())
    }

    fn update_window_properties(&self, p: WindowProperties<'_>) {
        let c = p.layout_constraints();
        let mut w = c.preferred.width;
        let mut h = c.preferred.height;
        if let Some(m) = c.min {
            w = w.max(m.width);
            h = h.max(m.height);
        }
        if let Some(m) = c.max {
            w = w.min(m.width);
            h = h.min(m.height);
        }
        let new = (w.ceil().max(1.0) as u32, h.ceil().max(1.0) as u32);
        if std::env::var_os("TSPMO_DEBUG").is_some() { eprintln!("props {new:?}"); }
        if self.want.get() != Some(new) {
            self.want.set(Some(new));
            self.redraw.set(true);
        }
        let t = p.title().to_string();
        if *self.title.borrow() != t {
            *self.title.borrow_mut() = t.clone();
            self.wl.st.borrow_mut().set_title(self.id, &t);
        }
    }
}

/// Test hook: where to dump each frame as a PPM (`TSPMO_DUMP`).
pub fn dump_path() -> Option<String> {
    std::env::var("TSPMO_DUMP").ok()
}
