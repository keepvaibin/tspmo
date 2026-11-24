//! Wayland side: globals, surfaces, buffers, input. Handlers queue into `out`.

use super::{Job, Kind, input};
use crate::px::Px;
use slint::platform::WindowEvent;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface,
};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window as XdgWindow, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    registry_handlers,
};
use std::collections::HashMap;

/// Margin between the popup and the screen edge, logical px.
const MARGIN: i32 = 8;

pub enum Out {
    Event(usize, WindowEvent),
    Scale(usize),
    Close(usize),
    FocusLost(usize),
}

struct Surf {
    kind: Kind,
    wl: wl_surface::WlSurface,
    layer: Option<LayerSurface>,
    xdg: Option<XdgWindow>,
    viewport: Option<WpViewport>,
    frac: Option<WpFractionalScaleV1>,
    scale120: u32,
    configured: bool,
    frame_pending: bool,
    logical: (u32, u32),
}

pub struct State {
    qh: QueueHandle<State>,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    compositor: CompositorState,
    layer_shell: LayerShell,
    xdg_shell: Option<XdgShell>,
    viewporter: Option<WpViewporter>,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    cursor_mgr: Option<WpCursorShapeManagerV1>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    loop_handle: LoopHandle<'static, State>,
    surfs: HashMap<usize, Surf>,
    kb_focus: Option<usize>,
    ptr_focus: Option<usize>,
    pos: (f32, f32),
    mods: Modifiers,
    pub out: Vec<Out>,
    pub jobs: Vec<Job>,
    pub quit: bool,
    dump: Option<String>,
}

impl State {
    pub fn connect() -> Option<(State, EventLoop<'static, State>)> {
        let conn = Connection::connect_to_env().ok()?;
        let (globals, queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).ok()?;
        let layer_shell = LayerShell::bind(&globals, &qh).ok()?;
        let shm = Shm::bind(&globals, &qh).ok()?;
        let pool = SlotPool::new(1 << 20, &shm).ok()?;
        let ev: EventLoop<'static, State> = EventLoop::try_new().ok()?;
        let loop_handle = ev.handle();
        WaylandSource::new(conn.clone(), queue).insert(loop_handle.clone()).ok()?;
        let bind = |g: &GlobalList| {
            (
                g.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok(),
                g.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ()).ok(),
                g.bind::<WpCursorShapeManagerV1, _, _>(&qh, 1..=2, ()).ok(),
            )
        };
        let (viewporter, frac_mgr, cursor_mgr) = bind(&globals);
        let st = State {
            registry: RegistryState::new(&globals),
            seats: SeatState::new(&globals, &qh),
            outputs: OutputState::new(&globals, &qh),
            xdg_shell: XdgShell::bind(&globals, &qh).ok(),
            qh,
            shm,
            pool,
            compositor,
            layer_shell,
            viewporter,
            frac_mgr,
            cursor_mgr,
            cursor_dev: None,
            keyboard: None,
            pointer: None,
            loop_handle,
            surfs: HashMap::new(),
            kb_focus: None,
            ptr_focus: None,
            pos: (0.0, 0.0),
            mods: Modifiers::default(),
            out: Vec::new(),
            jobs: Vec::new(),
            quit: false,
            dump: super::dump_path(),
        };
        Some((st, ev))
    }

    pub fn has_surface(&self, id: usize) -> bool {
        self.surfs.contains_key(&id)
    }

    pub fn scale120(&self, id: usize) -> u32 {
        self.surfs.get(&id).map_or(120, |s| s.scale120)
    }

    pub fn logical(&self, id: usize) -> Option<(u32, u32)> {
        self.surfs.get(&id).map(|s| s.logical)
    }

    pub fn can_draw(&self, id: usize) -> bool {
        self.surfs.get(&id).is_some_and(|s| s.configured && !s.frame_pending)
    }

    pub fn set_title(&mut self, id: usize, title: &str) {
        if let Some(x) = self.surfs.get(&id).and_then(|s| s.xdg.as_ref()) {
            x.set_title(title.to_owned());
        }
    }

    pub fn create(&mut self, id: usize, kind: Kind, size: (u32, u32), title: &str, app_id: &str) {
        let wl = self.compositor.create_surface(&self.qh);
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&wl, &self.qh, ()));
        let frac = self
            .frac_mgr
            .as_ref()
            .map(|m| m.get_fractional_scale(&wl, &self.qh, id));
        let mut s = Surf {
            kind,
            wl: wl.clone(),
            layer: None,
            xdg: None,
            viewport,
            frac,
            scale120: 120,
            configured: false,
            frame_pending: false,
            logical: size,
        };
        match kind {
            Kind::Popup | Kind::Osd => {
                let layer = self.layer_shell.create_layer_surface(
                    &self.qh,
                    wl,
                    if kind == Kind::Osd { Layer::Overlay } else { Layer::Top },
                    Some(if kind == Kind::Osd { "tspmo-osd" } else { "tspmo" }),
                    None,
                );
                if kind == Kind::Popup {
                    layer.set_anchor(Anchor::BOTTOM | Anchor::RIGHT);
                    layer.set_margin(0, MARGIN, MARGIN, 0);
                    // On demand: a click elsewhere takes the focus away, which closes the popup.
                    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                } else {
                    layer.set_anchor(Anchor::BOTTOM);
                    layer.set_margin(0, 0, 64, 0);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    if let Ok(region) = smithay_client_toolkit::compositor::Region::new(&self.compositor) {
                        s.wl.set_input_region(Some(region.wl_region()));
                    }
                }
                layer.set_exclusive_zone(0);
                layer.set_size(size.0, size.1);
                layer.commit();
                s.layer = Some(layer);
            }
            Kind::Window => {
                if let Some(x) = &self.xdg_shell {
                    let w = x.create_window(wl, WindowDecorations::RequestServer, &self.qh);
                    w.set_title(title.to_owned());
                    w.set_app_id(app_id.to_owned());
                    w.set_min_size(Some(size));
                    w.set_max_size(Some(size));
                    w.commit();
                    s.xdg = Some(w);
                }
            }
        }
        self.surfs.insert(id, s);
    }

    pub fn resize(&mut self, id: usize, size: (u32, u32)) {
        let Some(s) = self.surfs.get_mut(&id) else { return };
        s.logical = size;
        if let Some(l) = &s.layer {
            l.set_size(size.0, size.1);
            l.commit();
        }
        if let Some(x) = &s.xdg {
            x.set_min_size(Some(size));
            x.set_max_size(Some(size));
            x.commit();
        }
    }

    pub fn destroy(&mut self, id: usize) {
        if self.kb_focus == Some(id) {
            self.kb_focus = None;
        }
        if self.ptr_focus == Some(id) {
            self.ptr_focus = None;
        }
        if let Some(s) = self.surfs.remove(&id) {
            if let Some(f) = &s.frac {
                f.destroy();
            }
            if let Some(v) = &s.viewport {
                v.destroy();
            }
            // Dropping the role objects destroys them; the wl_surface goes last.
            drop(s.layer);
            drop(s.xdg);
            s.wl.destroy();
        }
    }

    pub fn destroy_all(&mut self) {
        let ids: Vec<usize> = self.surfs.keys().copied().collect();
        for id in ids {
            self.destroy(id);
        }
    }

    /// Copy the canvas into a shared-memory buffer and show it.
    pub fn present(
        &mut self,
        id: usize,
        canvas: &[Px],
        w: u32,
        h: u32,
        bb: (slint::PhysicalPosition, slint::PhysicalSize),
    ) {
        let Some(s) = self.surfs.get_mut(&id) else { return };
        crate::trace("present");
        let Ok((buffer, mem)) = self.pool.create_buffer(
            w as i32,
            h as i32,
            (w * 4) as i32,
            smithay_client_toolkit::reexports::client::protocol::wl_shm::Format::Argb8888,
        ) else {
            return;
        };
        for (d, p) in mem.chunks_exact_mut(4).zip(canvas) {
            d.copy_from_slice(&[p.b, p.g, p.r, p.a]);
        }
        if let Some(path) = &self.dump {
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in canvas {
                out.extend_from_slice(&[p.r, p.g, p.b]);
            }
            let _ = std::fs::write(path, out);
        }
        if let Some(v) = &s.viewport {
            v.set_destination(s.logical.0 as i32, s.logical.1 as i32);
        }
        let _ = buffer.attach_to(&s.wl);
        // Whole-window damage keeps every compositor happy; the copy dominates anyway.
        let _ = bb;
        s.wl.damage_buffer(0, 0, w as i32, h as i32);
        s.wl.frame(&self.qh, s.wl.clone());
        s.frame_pending = true;
        s.wl.commit();
    }

    fn id_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfs.iter().find(|(_, s)| &s.wl == surface).map(|(i, _)| *i)
    }

    fn push(&mut self, id: usize, e: WindowEvent) {
        self.out.push(Out::Event(id, e));
    }

    fn key(&mut self, e: &KeyEvent, kind: input::Press) {
        if let Some(id) = self.kb_focus
            && let Some(ev) = input::key_event(e, kind, self.mods.shift)
        {
            self.push(id, ev);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// protocol plumbing

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_keyboard!(State);
delegate_pointer!(State);
delegate_layer!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore WpViewport);
delegate_noop!(State: ignore WpFractionalScaleManagerV1);
delegate_noop!(State: ignore WpCursorShapeManagerV1);
delegate_noop!(State: ignore WpCursorShapeDeviceV1);

impl Dispatch<WpFractionalScaleV1, usize> for State {
    fn event(
        st: &mut Self,
        _: &WpFractionalScaleV1,
        e: wp_fractional_scale_v1::Event,
        id: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e
            && let Some(s) = st.surfs.get_mut(id)
            && s.scale120 != scale
        {
            s.scale120 = scale;
            st.out.push(Out::Scale(*id));
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        // Integer scale only matters when the compositor has no fractional scale.
        if self.frac_mgr.is_none()
            && let Some(id) = self.id_of(surface)
            && let Some(s) = self.surfs.get_mut(&id)
        {
            s.scale120 = (factor.max(1) as u32) * 120;
            self.out.push(Out::Scale(id));
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(id) = self.id_of(surface)
            && let Some(s) = self.surfs.get_mut(&id)
        {
            s.frame_pending = false;
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(id) = self.id_of(layer.wl_surface()) {
            self.out.push(Out::Close(id));
        }
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(id) = self.id_of(layer.wl_surface())
            && let Some(s) = self.surfs.get_mut(&id)
        {
            s.configured = true;
            self.out.push(Out::Scale(id));
        }
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, w: &XdgWindow) {
        if let Some(id) = self.id_of(w.wl_surface()) {
            self.out.push(Out::Close(id));
        }
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        w: &XdgWindow,
        _: WindowConfigure,
        _: u32,
    ) {
        if let Some(id) = self.id_of(w.wl_surface())
            && let Some(s) = self.surfs.get_mut(&id)
        {
            s.configured = true;
            self.out.push(Out::Scale(id));
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let cb = Box::new(|st: &mut State, _: &wl_keyboard::WlKeyboard, e: KeyEvent| {
                st.key(&e, input::Press::Repeat);
            });
            self.keyboard = self
                .seats
                .get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), cb)
                .ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seats.get_pointer(qh, &seat).ok();
            if let (Some(m), Some(p)) = (&self.cursor_mgr, &self.pointer) {
                self.cursor_dev = Some(m.get_pointer(p, qh, ()));
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            self.cursor_dev = None;
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.kb_focus = self.id_of(surface);
        crate::trace("keyboard focus");
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(id) = self.id_of(surface) {
            if self.kb_focus == Some(id) {
                self.kb_focus = None;
            }
            if self.surfs.get(&id).is_some_and(|s| s.kind == Kind::Popup) {
                self.out.push(Out::FocusLost(id));
            }
        }
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        e: KeyEvent,
    ) {
        self.key(&e, input::Press::Down);
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        e: KeyEvent,
    ) {
        self.key(&e, input::Press::Up);
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        m: Modifiers,
        _: u32,
    ) {
        if let Some(id) = self.kb_focus {
            for e in input::modifier_events(&self.mods, &m) {
                self.push(id, e);
            }
        }
        self.mods = m;
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for e in events {
            let Some(id) = self.id_of(&e.surface) else { continue };
            let position = slint::LogicalPosition::new(e.position.0 as f32, e.position.1 as f32);
            match e.kind {
                PointerEventKind::Enter { serial } => {
                    self.ptr_focus = Some(id);
                    if let Some(d) = &self.cursor_dev {
                        d.set_shape(serial, Shape::Default);
                    }
                    self.pos = (position.x, position.y);
                    self.push(id, WindowEvent::PointerMoved { position });
                }
                PointerEventKind::Leave { .. } => {
                    if self.ptr_focus == Some(id) {
                        self.ptr_focus = None;
                    }
                    self.push(id, WindowEvent::PointerExited);
                }
                PointerEventKind::Motion { .. } => {
                    self.pos = (position.x, position.y);
                    self.push(id, WindowEvent::PointerMoved { position });
                }
                PointerEventKind::Press { button, .. } => {
                    if let Some(button) = input::button(button) {
                        self.push(id, WindowEvent::PointerPressed { position, button });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(button) = input::button(button) {
                        self.push(id, WindowEvent::PointerReleased { position, button });
                    }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    let d = |a: smithay_client_toolkit::seat::pointer::AxisScroll| {
                        if a.discrete != 0 { -(a.discrete as f32) * 20.0 } else { -(a.absolute as f32) }
                    };
                    let (dx, dy) = (d(horizontal), d(vertical));
                    if dx != 0.0 || dy != 0.0 {
                        self.push(
                            id,
                            WindowEvent::PointerScrolled { position, delta_x: dx, delta_y: dy },
                        );
                    }
                }
            }
        }
    }
}
