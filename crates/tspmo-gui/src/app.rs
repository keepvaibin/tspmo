//! The windows: own the store, turn daemon state into Slint properties and UI actions into calls.
//!
//! Everything here runs on the UI thread. D-Bus threads hand events over with
//! `invoke_from_event_loop`. The popup and each secondary window are separate Slint components,
//! each with its own `Bk` global; every state change is pushed into all that are open. One
//! one-second timer reads the live numbers (fan speed, battery power) while a window exists.

use crate::bindings;
use crate::bus::{self, Call, Event};
use crate::model::{self, Arg, Curve, SOURCES, Store, THERMAL};
use crate::value::V;
use crate::{Bk, FansWin, GpuUser, Item, KeysWin, LimitRow, Opt, Popup, Pt, ScreenWin, SystemWin, Th};
use slint::{ComponentHandle, Model, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Closed windows wait here until their callback has returned.
    static GRAVE: RefCell<Vec<AnyWin>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Popup,
    Fans,
    Screen,
    Keys,
    System,
}

impl Kind {
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "popup" | "main" => Kind::Popup,
            "fans" => Kind::Fans,
            "screen" => Kind::Screen,
            "keys" | "keyboard" => Kind::Keys,
            "system" => Kind::System,
            _ => return None,
        })
    }
}

enum AnyWin {
    Popup(Popup),
    Fans(FansWin),
    Screen(ScreenWin),
    Keys(KeysWin),
    System(SystemWin),
}

/// Run `$body` with the `Bk` and `Th` globals of whichever window `$w` is.
macro_rules! with_globals {
    ($w:expr, |$bk:ident, $th:ident| $body:expr) => {
        match $w {
            AnyWin::Popup(w) => {
                let ($bk, $th) = (w.global::<Bk>(), w.global::<Th>());
                $body
            }
            AnyWin::Fans(w) => {
                let ($bk, $th) = (w.global::<Bk>(), w.global::<Th>());
                $body
            }
            AnyWin::Screen(w) => {
                let ($bk, $th) = (w.global::<Bk>(), w.global::<Th>());
                $body
            }
            AnyWin::Keys(w) => {
                let ($bk, $th) = (w.global::<Bk>(), w.global::<Th>());
                $body
            }
            AnyWin::System(w) => {
                let ($bk, $th) = (w.global::<Bk>(), w.global::<Th>());
                $body
            }
        }
    };
}

impl AnyWin {
    fn window(&self) -> &slint::Window {
        match self {
            AnyWin::Popup(w) => w.window(),
            AnyWin::Fans(w) => w.window(),
            AnyWin::Screen(w) => w.window(),
            AnyWin::Keys(w) => w.window(),
            AnyWin::System(w) => w.window(),
        }
    }
}

struct Models {
    gpu_users: Rc<VecModel<GpuUser>>,
    color_modes: Rc<VecModel<Opt>>,
    screen_items: Rc<VecModel<Item>>,
    bat_opts: Rc<VecModel<Opt>>,
    bat_values: Rc<VecModel<i32>>,
    fan_opts: Rc<VecModel<Opt>>,
    curve: Rc<VecModel<Pt>>,
    limits: Rc<VecModel<LimitRow>>,
    kb_effects: Rc<VecModel<Opt>>,
    kb_states: Rc<VecModel<bool>>,
    slash_modes: Rc<VecModel<SharedString>>,
    slash_show: Rc<VecModel<Item>>,
    system_items: Rc<VecModel<Item>>,
}

impl Models {
    fn new() -> Models {
        Models {
            gpu_users: Rc::default(),
            color_modes: Rc::default(),
            screen_items: Rc::default(),
            bat_opts: Rc::default(),
            bat_values: Rc::default(),
            fan_opts: Rc::default(),
            curve: Rc::default(),
            limits: Rc::default(),
            kb_effects: Rc::default(),
            kb_states: Rc::default(),
            slash_modes: Rc::default(),
            slash_show: Rc::default(),
            system_items: Rc::default(),
        }
    }

    fn bind(&self, bk: &Bk) {
        bk.set_gpu_users(self.gpu_users.clone().into());
        bk.set_color_modes(self.color_modes.clone().into());
        bk.set_screen_items(self.screen_items.clone().into());
        bk.set_bat_opts(self.bat_opts.clone().into());
        bk.set_bat_values(self.bat_values.clone().into());
        bk.set_fan_opts(self.fan_opts.clone().into());
        bk.set_curve(self.curve.clone().into());
        bk.set_limits(self.limits.clone().into());
        bk.set_kb_effects(self.kb_effects.clone().into());
        bk.set_kb_states(self.kb_states.clone().into());
        bk.set_slash_modes(self.slash_modes.clone().into());
        bk.set_slash_show(self.slash_show.clone().into());
        bk.set_system_items(self.system_items.clone().into());
    }
}

/// A curve being edited: shown instead of the daemon's until the daemon reports it back.
struct Edit {
    mode: &'static str,
    fan: u32,
    points: Curve,
    sent: bool,
}

struct App {
    wins: Vec<(Kind, AnyWin)>,
    /// Created, not yet shown.
    pending: Vec<Kind>,
    store: Store,
    m: Models,
    fonts: crate::theme::Fonts,
    dark: bool,
    /// Thermal mode being edited in Fans & Power (fans and limits share it).
    fan_mode: usize,
    fan_sel: usize,
    pw_source: usize,
    fan_init: bool,
    pw_init: bool,
    edit: Option<Edit>,
    loaded: bool,
    offline: bool,
    error: String,
    error_timer: Timer,
    live_timer: Timer,
}

fn ss(s: &str) -> SharedString {
    SharedString::from(s)
}

/// Replace a model's contents touching only the rows that differ, so repeaters keep their
/// elements (and animations) while values change.
fn sync<T: Clone + PartialEq + 'static>(m: &VecModel<T>, new: Vec<T>) {
    if m.row_count() == new.len() {
        for (i, n) in new.into_iter().enumerate() {
            if m.row_data(i).as_ref() != Some(&n) {
                m.set_row_data(i, n);
            }
        }
    } else {
        m.set_vec(new);
    }
}

fn opt(id: &str, label: &str) -> Opt {
    Opt {
        id: ss(id),
        label: ss(label),
        tint: slint::Color::default(),
        keyed: false,
    }
}

fn item_of(i: &model::HwItem) -> Item {
    Item {
        name: ss(&i.name),
        label: ss(&i.label),
        kind: ss(&i.kind),
        on: i.on(),
        text: ss(&if i.name == "charge_mode" {
            model::charger_label(&i.current).to_owned()
        } else {
            i.current.clone()
        }),
        min: i.min as i32,
        max: i.max as i32,
        value: i.current.parse().unwrap_or(0),
        writable: i.writable,
    }
}

fn emit(ev: Event) {
    let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_event(ev)));
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|cell| {
        if let Ok(mut g) = cell.try_borrow_mut()
            && let Some(a) = g.as_mut()
        {
            f(a);
        }
    });
}

/// Create the first windows and start reading from the daemon. Nothing here waits for D-Bus.
pub fn start(first: &[Kind], dark: bool, fonts: crate::theme::Fonts) -> Result<(), String> {
    let app = App {
        wins: Vec::new(),
        pending: Vec::new(),
        store: Store::default(),
        m: Models::new(),
        fonts,
        dark,
        fan_mode: 1,
        fan_sel: 0,
        pw_source: 0,
        fan_init: false,
        pw_init: false,
        edit: None,
        loaded: false,
        offline: false,
        error: String::new(),
        error_timer: Timer::default(),
        live_timer: Timer::default(),
    };
    APP.with(|c| *c.borrow_mut() = Some(app));
    let mut err = None;
    with_app(|a| {
        for k in first {
            if let Err(e) = a.open(*k) {
                err = Some(e);
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    bus::start(emit);
    Ok(())
}

/// A message from a second launch: `toggle` or `open <window>`.
pub fn message(msg: &str) {
    let msg = msg.to_owned();
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|a| {
            if let Some(name) = msg.strip_prefix("open ") {
                if let Some(k) = Kind::parse(name.trim()) {
                    a.show(k);
                }
            } else if a.wins.iter().any(|(k, _)| *k == Kind::Popup) {
                a.close(Kind::Popup);
            } else {
                a.show(Kind::Popup);
            }
        });
    });
}

impl App {
    fn open(&mut self, k: Kind) -> Result<(), String> {
        let wl = match k {
            Kind::Popup => crate::wl::Kind::Popup,
            _ => crate::wl::Kind::Window,
        };
        crate::wl::next_kind(wl);
        let win = match k {
            Kind::Popup => AnyWin::Popup(Popup::new().map_err(|e| e.to_string())?),
            Kind::Fans => AnyWin::Fans(FansWin::new().map_err(|e| e.to_string())?),
            Kind::Screen => AnyWin::Screen(ScreenWin::new().map_err(|e| e.to_string())?),
            Kind::Keys => AnyWin::Keys(KeysWin::new().map_err(|e| e.to_string())?),
            Kind::System => AnyWin::System(SystemWin::new().map_err(|e| e.to_string())?),
        };
        with_globals!(&win, |bk, th| {
            th.set_dark(self.dark);
            th.set_font_size(self.fonts.px);
            th.set_font_family(ss(&self.fonts.family));
            th.set_mono(ss(&self.fonts.mono));
            self.m.bind(&bk);
            bk.on_act(|key, arg| with_app(|a| a.act(&key, &arg)));
            bk.on_acti(|key, n| with_app(|a| a.acti(&key, i64::from(n))));
            bk.on_curve_move(|i, t, p| with_app(|a| a.curve_move(i as usize, t, p)));
            bk.on_curve_commit(|| with_app(App::curve_commit));
            bk.on_open(|n| {
                if let Some(k) = Kind::parse(&n) {
                    with_app(|a| a.show(k));
                }
            });
            bk.on_close(move || with_app(|a| a.close(k)));
        });
        win.window().on_close_requested(move || {
            with_app(|a| a.forget(k));
            slint::CloseRequestResponse::HideWindow
        });
        with_globals!(&win, |bk, _th| self.push_to(&bk));
        crate::name_window();
        self.wins.push((k, win));
        // Wait for the daemon's first answer (a few ms) so the first frame is the right size and
        // already filled in; a timer shows the window anyway if the daemon is slow or absent.
        if self.loaded {
            self.show_kind(k)?;
        } else {
            self.pending.push(k);
            Timer::single_shot(Duration::from_millis(120), || with_app(App::show_pending));
        }
        self.live_timer
            .start(TimerMode::Repeated, Duration::from_secs(1), || {
                with_app(App::tick);
            });
        if self.loaded {
            self.tick();
        }
        Ok(())
    }

    fn show_kind(&self, k: Kind) -> Result<(), String> {
        let Some((_, win)) = self.wins.iter().find(|(w, _)| *w == k) else {
            return Ok(());
        };
        match win {
            AnyWin::Popup(w) => w.show(),
            AnyWin::Fans(w) => w.show(),
            AnyWin::Screen(w) => w.show(),
            AnyWin::Keys(w) => w.show(),
            AnyWin::System(w) => w.show(),
        }
        .map_err(|e| e.to_string())
    }

    fn show_pending(&mut self) {
        crate::trace("show windows");
        for k in std::mem::take(&mut self.pending) {
            if let Err(e) = self.show_kind(k) {
                self.set_error(e);
            }
        }
    }

    /// Open a window, or raise it by recreating nothing when it already exists.
    fn show(&mut self, k: Kind) {
        if self.wins.iter().any(|(w, _)| *w == k) {
            return;
        }
        if let Err(e) = self.open(k) {
            self.set_error(e);
        }
    }

    /// Hide and drop a window; the process ends with the last one.
    fn close(&mut self, k: Kind) {
        if let Some(i) = self.wins.iter().position(|(w, _)| *w == k) {
            let (_, w) = self.wins.remove(i);
            let _ = w.window().hide();
            self.drop_later(w);
        }
    }

    /// Dropping a component inside one of its own callbacks is unsafe: do it next turn.
    fn drop_later(&mut self, w: AnyWin) {
        GRAVE.with_borrow_mut(|g| g.push(w));
        Timer::single_shot(Duration::ZERO, || {
            let dead = GRAVE.take();
            drop(dead);
            with_app(App::finish_if_empty);
        });
    }

    /// The compositor or a click outside closed it: only drop our handle.
    fn forget(&mut self, k: Kind) {
        if let Some(i) = self.wins.iter().position(|(w, _)| *w == k) {
            let (_, w) = self.wins.remove(i);
            self.drop_later(w);
        }
    }

    fn finish_if_empty(&mut self) {
        if self.wins.is_empty() {
            self.live_timer.stop();
        }
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Props(iface, props) => {
                let settings_changed = iface == "Fans1" && props.iter().any(|(k, _)| k == "Settings");
                let switching =
                    iface == "Daemon1" && props.iter().any(|(k, _)| k == "GpuSwitching" || k == "GpuMode");
                self.store.merge(&iface, props);
                if settings_changed && self.edit.as_ref().is_some_and(|e| e.sent) {
                    self.edit = None;
                }
                self.init_locals();
                if switching {
                    self.fetch_gpu_users();
                }
            }
            Event::Methods(m) => self.store.methods = m,
            Event::Ready => {
                crate::trace("daemon state read");
                self.loaded = true;
                self.push();
                self.show_pending();
                self.fetch_gpu_users();
                self.tick();
            }
            Event::Offline(msg) => {
                self.loaded = true;
                self.offline = true;
                self.error = msg;
                self.push();
                self.show_pending();
            }
            Event::GpuUsers(u) => self.store.gpu_users = u,
            Event::Error(msg) => self.set_error(msg),
        }
        self.push();
    }

    fn init_locals(&mut self) {
        if !self.fan_init && self.store.has_prop("Fans1", "Mode") {
            self.fan_init = true;
            self.fan_mode = THERMAL
                .iter()
                .position(|m| *m == self.store.get("Fans1", "Mode").str())
                .unwrap_or(1);
        }
        if !self.pw_init && self.store.has_prop("Power1", "Mode") {
            self.pw_init = true;
            if !self.fan_init {
                self.fan_mode = THERMAL
                    .iter()
                    .position(|m| *m == self.store.get("Power1", "Mode").str())
                    .unwrap_or(1);
            }
            self.pw_source = SOURCES
                .iter()
                .position(|m| *m == self.store.get("Power1", "Source").str())
                .unwrap_or(0);
        }
    }

    fn set_error(&mut self, msg: String) {
        // One short line: the popup has no room for more.
        self.error = match msg.char_indices().nth(60) {
            Some((i, _)) => format!("{}…", msg[..i].trim_end()),
            None => msg,
        };
        self.error_timer
            .start(TimerMode::SingleShot, Duration::from_secs(6), || {
                with_app(|a| {
                    if !a.offline {
                        a.error.clear();
                        a.push();
                    }
                });
            });
    }

    fn call(&self, c: Call) {
        bus::call(c, emit);
    }

    fn send(&self, iface: &'static str, method: &str, args: &[Arg]) {
        self.call(model::call(&self.store, iface, method, args));
    }

    fn fetch_gpu_users(&self) {
        self.call(Call::new("Daemon1", "GpuUsers", Vec::new()));
    }

    fn tick(&mut self) {
        let mut what = Vec::new();
        if self.store.has("Fans1") {
            what.push(("Fans1", "Rpm"));
        }
        if self.store.has("Battery1") {
            what.push(("Battery1", "Power"));
        }
        if !what.is_empty() {
            bus::read_live(what, emit);
        }
    }

    // -----------------------------------------------------------------------------------------
    // actions

    fn thermal(&self) -> &'static str {
        THERMAL[self.fan_mode.min(2)]
    }

    fn fan_number(&self) -> Option<u32> {
        let f = self.store.get("Fans1", "Fans").list();
        f.get(self.fan_sel)
            .or_else(|| f.first())
            .map(|e| e.at(0).int() as u32)
    }

    fn act(&mut self, key: &str, arg: &str) {
        let s = &self.store;
        match key {
            "mode" => self.send("Daemon1", "SetProfile", &[Arg::s(arg)]),
            "gpu" => self.send("Daemon1", "SetGpuMode", &[Arg::s(arg)]),
            "reboot" => bus::reboot(emit),
            "refresh" => self.send(bindings::IFACE, bindings::SET_REFRESH_POLICY, &[Arg::s(arg)]),
            "flicker" => {
                // On an OLED panel "auto" is "on": send it so the user's default is kept.
                let text = s
                    .methods
                    .get("Display1.SetFlickerFree")
                    .is_some_and(|v| v.first().is_some_and(|t| t == "s"));
                let a = if arg == "on" && text && s.get(bindings::IFACE, "Oled").bool() {
                    Arg::s("auto")
                } else {
                    Arg::B(arg == "on")
                };
                self.send(bindings::IFACE, bindings::SET_FLICKER_FREE, &[a]);
            }
            "color" => self.send(bindings::IFACE, bindings::SET_COLOR_MODE, &[Arg::s(arg)]),
            "fnlock" => self.send("Hardware1", "SetFnLock", &[Arg::B(arg == "on")]),
            "hw-toggle" => {
                if let Some(what) = arg.strip_prefix("slash:") {
                    let on = s
                        .get("Lid1", "SlashShow")
                        .list()
                        .iter()
                        .any(|e| e.at(0).str() == what && e.at(1).bool());
                    self.send("Lid1", "SetSlashShow", &[Arg::s(what), Arg::B(!on)]);
                } else if let Some(it) = model::hw_items(s).iter().find(|i| i.name == arg) {
                    self.send(
                        "Hardware1",
                        "Set",
                        &[Arg::s(arg), Arg::s(if it.on() { "0" } else { "1" })],
                    );
                }
            }
            "charge-once" => self.send("Battery1", "SetChargeOnce", &[Arg::B(arg == "on")]),
            "fan-auto" | "fan-full" | "fan-reset" => {
                let method = match key {
                    "fan-auto" => "SetAuto",
                    "fan-full" => "SetFull",
                    _ => "Reset",
                };
                if let Some(fan) = self.fan_number() {
                    self.edit = None;
                    self.send("Fans1", method, &[Arg::s(self.thermal()), Arg::N(i64::from(fan))]);
                }
            }
            "limit-default" | "limit-reset" => {
                let mode = self.thermal();
                if key == "limit-reset" {
                    self.send("Power1", "Reset", &[Arg::s(mode)]);
                } else {
                    self.send(
                        "Power1",
                        "SetLimit",
                        &[
                            Arg::s(mode),
                            Arg::s(SOURCES[self.pw_source.min(1)]),
                            Arg::s(arg),
                            Arg::N(-1),
                        ],
                    );
                }
            }
            "slash-on" => self.send("Lid1", "SetSlashEnabled", &[Arg::B(arg == "on")]),
            "anime-on" => self.send("Lid1", "SetAnimeEnabled", &[Arg::B(arg == "on")]),
            _ => {}
        }
        self.push();
    }

    fn acti(&mut self, key: &str, n: i64) {
        let s = &self.store;
        match key {
            "brightness" => self.send(bindings::IFACE, bindings::SET_BRIGHTNESS, &[Arg::N(n)]),
            "eye" => self.send(bindings::IFACE, bindings::SET_EYE_CARE, &[Arg::N(n)]),
            "bat-limit" => self.send("Battery1", "SetLimit", &[Arg::N(n)]),
            "fan-mode" => {
                self.fan_mode = n.clamp(0, 2) as usize;
                self.edit = None;
            }
            "fan-sel" => {
                self.fan_sel = n.max(0) as usize;
                self.edit = None;
            }
            "pw-source" => self.pw_source = n.clamp(0, 1) as usize,
            "kb-level" => self.send("Lighting1", "SetBrightness", &[Arg::s(""), Arg::N(n)]),
            "kb-effect-step" => {
                let modes = s.get("Lighting1", "Modes").strings();
                let mut e = model::effect(s);
                if let Some(next) = model::cycle(&modes, &e.mode, n) {
                    e.mode = next.clone();
                    self.set_effect(&e);
                }
            }
            "kb-color" => {
                let mut e = model::effect(s);
                e.color = model::KB_COLORS[(n as usize).min(7)];
                self.set_effect(&e);
            }
            "kb-speed" => {
                let mut e = model::effect(s);
                e.speed = model::KB_SPEEDS[(n as usize).min(2)].to_owned();
                self.set_effect(&e);
            }
            "kb-idle-step" => {
                let cur = s.get("Lighting1", "IdleTimeout").int();
                let i = model::KB_IDLE.iter().rposition(|v| *v <= cur).unwrap_or(0) as i64;
                let j = (i + n).clamp(0, model::KB_IDLE.len() as i64 - 1) as usize;
                self.send("Lighting1", "SetIdleTimeout", &[Arg::N(model::KB_IDLE[j])]);
            }
            "slash-level" => self.send("Lid1", "SetSlashBrightness", &[Arg::N(n)]),
            "slash-mode-step" => {
                let modes = s.get("Lid1", "SlashModes").strings();
                if let Some(m) = model::cycle(&modes, s.get("Lid1", "SlashMode").str(), n) {
                    self.send("Lid1", "SetSlashMode", &[Arg::s(m)]);
                }
            }
            "anime-level" => self.send("Lid1", "SetAnimeBrightness", &[Arg::N(n)]),
            k if k.starts_with("kb-state:") => {
                let i: usize = k["kb-state:".len()..].parse().unwrap_or(0);
                let mut st: Vec<bool> = s
                    .get("Lighting1", "PowerStates")
                    .list()
                    .iter()
                    .map(V::bool)
                    .collect();
                st.resize(4, true);
                if let Some(slot) = st.get_mut(i) {
                    *slot = n != 0;
                }
                let args: Vec<Arg> = st.into_iter().map(Arg::B).collect();
                self.send("Lighting1", "SetPowerStates", &args);
            }
            k if k.starts_with("limit:") => {
                let name = &k["limit:".len()..];
                let (mode, source) = (self.thermal(), SOURCES[self.pw_source.min(1)]);
                // Keep sustained <= boost <= fast boost: move the neighbours along.
                let fixes = model::limit_fixes(&model::limits(s, mode, source), name, n);
                for (other, v) in fixes {
                    self.send(
                        "Power1",
                        "SetLimit",
                        &[Arg::s(mode), Arg::s(source), Arg::S(other), Arg::N(v)],
                    );
                }
                self.send(
                    "Power1",
                    "SetLimit",
                    &[Arg::s(mode), Arg::s(source), Arg::s(name), Arg::N(n)],
                );
            }
            k if k.starts_with("hw-step:") => {
                let name = &k["hw-step:".len()..];
                if let Some(v) = model::hw_items(s)
                    .iter()
                    .find(|i| i.name == name)
                    .and_then(|i| i.step(n))
                {
                    self.send("Hardware1", "Set", &[Arg::s(name), Arg::S(v)]);
                }
            }
            _ => {}
        }
        self.push();
    }

    fn set_effect(&self, e: &model::Effect) {
        self.call(Call::new(
            "Lighting1",
            "SetEffect",
            vec![zbus::zvariant::Value::from(""), e.to_value()],
        ));
    }

    // -----------------------------------------------------------------------------------------
    // fan curve editing

    fn shown_curve(&self) -> Curve {
        if let Some(e) = &self.edit {
            return e.points.clone();
        }
        let fan = self.fan_number().unwrap_or(1);
        let pts = model::fan_setting(&self.store, self.thermal(), fan).points;
        if pts.is_empty() {
            model::default_curve()
        } else {
            pts
        }
    }

    fn curve_move(&mut self, i: usize, t: i32, p: i32) {
        if self.edit.is_none() {
            let Some(fan) = self.fan_number() else { return };
            self.edit = Some(Edit {
                mode: self.thermal(),
                fan,
                points: self.shown_curve(),
                sent: false,
            });
        }
        if let Some(e) = self.edit.as_mut() {
            e.sent = false;
            model::move_point(&mut e.points, i, t, p);
        }
        self.push_curve();
    }

    fn curve_commit(&mut self) {
        let Some(e) = self.edit.as_mut() else { return };
        if e.sent {
            return;
        }
        e.sent = true;
        let pts: zbus::zvariant::Value<'static> = zbus::zvariant::Value::from(e.points.clone());
        let c = Call::new(
            "Fans1",
            "SetCurve",
            vec![
                zbus::zvariant::Value::from(e.mode),
                zbus::zvariant::Value::U32(e.fan),
                pts,
            ],
        );
        self.call(c);
    }

    fn push_curve(&self) {
        let pts: Vec<Pt> = self
            .shown_curve()
            .iter()
            .map(|(t, p)| Pt {
                t: i32::from(*t),
                p: i32::from(*p),
            })
            .collect();
        sync(&self.m.curve, pts);
        let kind = if self.edit.is_some() {
            "custom".to_owned()
        } else {
            model::fan_setting(&self.store, self.thermal(), self.fan_number().unwrap_or(1)).kind
        };
        for (_, w) in &self.wins {
            with_globals!(w, |bk, _th| bk.set_fan_kind(ss(&kind)));
        }
    }

    // -----------------------------------------------------------------------------------------
    // state → properties

    fn push(&self) {
        for (_, w) in &self.wins {
            with_globals!(w, |bk, _th| self.push_to(&bk));
        }
        let mode = self.store.get("Daemon1", "Profile").str();
        let accent = if mode.is_empty() {
            mode_color("balanced", self.dark)
        } else {
            mode_color(mode, self.dark)
        };
        for (_, w) in &self.wins {
            with_globals!(w, |_bk, th| th.set_accent_target(accent));
        }
    }

    fn push_to(&self, bk: &Bk) {
        let s = &self.store;
        let secs = model::sections(s);
        bk.set_loaded(self.loaded);
        bk.set_error(ss(&self.error));
        let has = |id: &str| secs.iter().any(|x| x.id == id);
        bk.set_has_gpu(has("gpu"));
        bk.set_has_battery(has("battery"));
        bk.set_has_fans(has("fans"));
        bk.set_has_power(has("power"));
        bk.set_has_light(has("light"));
        bk.set_win_fans(has("fans") || has("power"));
        bk.set_win_screen(has("screen"));
        bk.set_win_keys(has("light") || has("lid"));
        bk.set_win_system(
            has("battery") || has("system") || has("gpu") || s.get("Hardware1", "FnLockAvailable").bool(),
        );

        let mode = s.get("Daemon1", "Profile").str();
        bk.set_mode(ss(mode));
        let rpm = s
            .get("Fans1", "Rpm")
            .list()
            .iter()
            .map(|e| e.at(1).int())
            .max()
            .unwrap_or(0);
        bk.set_fan_text(ss(&if rpm > 0 {
            format!("{rpm} rpm")
        } else {
            String::new()
        }));

        // GPU
        bk.set_gpu_mode(ss(&model::color_label(s.get("Daemon1", "GpuMode").str())));
        bk.set_gpu_policy(ss(s.get("Daemon1", "GpuPolicy").str()));
        bk.set_gpu_switching(ss(s.get("Daemon1", "GpuSwitching").str()));
        let (mode_raw, pending) = (
            s.get("Daemon1", "GpuMode").str(),
            s.get("Daemon1", "GpuPending").str(),
        );
        bk.set_gpu_pending(ss(&model::color_label(pending)));
        bk.set_gpu_reboot(!pending.is_empty() && pending != mode_raw);
        // One short line: what comes before the first colon of the daemon's explanation.
        let note = s.get("Daemon1", "GpuMessage").str();
        bk.set_gpu_message(ss(note.split(':').next().unwrap_or("").trim()));
        bk.set_gpu_state(ss(gpu_state(
            s.get("Daemon1", "GpuMode").str(),
            s.get("Daemon1", "DgpuAsleep").bool(),
        )));
        sync(
            &self.m.gpu_users,
            s.gpu_users
                .iter()
                .filter(|(_, _, k)| k == "render")
                .map(|(pid, name, _)| GpuUser {
                    pid: *pid as i32,
                    name: ss(name),
                })
                .collect(),
        );

        // Screen
        let sc = model::screen(s);
        bk.set_has_refresh(sc.has_refresh);
        bk.set_refresh_policy(ss(&sc.policy));
        bk.set_refresh_now(ss(&sc.now));
        bk.set_has_flicker(sc.has_flicker);
        bk.set_flicker(sc.flicker);
        bk.set_brightness(sc.brightness as i32);
        bk.set_color_idx(
            sc.color_modes
                .iter()
                .position(|m| *m == sc.color_mode)
                .unwrap_or(0) as i32,
        );
        sync(
            &self.m.color_modes,
            sc.color_modes
                .iter()
                .map(|m| opt(m, &model::color_label(m)))
                .collect(),
        );
        bk.set_has_eye(sc.has_eye);
        bk.set_eye_kelvin(sc.eye_kelvin as i32);
        let hw = model::hw_items(s);
        sync(
            &self.m.screen_items,
            hw.iter()
                .filter(|i| model::SCREEN_ITEMS.contains(&i.name.as_str()))
                .map(item_of)
                .collect(),
        );
        sync(
            &self.m.system_items,
            hw.iter()
                .filter(|i| !model::SCREEN_ITEMS.contains(&i.name.as_str()))
                .map(item_of)
                .collect(),
        );
        bk.set_reboot_pending(s.get("Hardware1", "RebootPending").bool());
        bk.set_has_fnlock(s.get("Hardware1", "FnLockAvailable").bool());
        bk.set_fnlock(s.get("Hardware1", "FnLock").bool());

        // Battery
        let b = "Battery1";
        bk.set_bat_percent(if s.has_prop(b, "Percent") {
            s.get(b, "Percent").int() as i32
        } else {
            -1
        });
        bk.set_bat_status(ss(s.get(b, "Status").str()));
        let watts = if s.has_prop(b, "Power") {
            s.get(b, "Power").float()
        } else {
            -1.0
        };
        bk.set_bat_power(ss(&model::fmt_watts(watts)));
        bk.set_bat_line(ss(&model::battery_line(
            if s.has_prop(b, "Percent") {
                s.get(b, "Percent").int()
            } else {
                -1
            },
            s.get(b, "Status").str(),
            watts,
        )));
        bk.set_bat_left(ss(&model::fmt_duration(s.get(b, "TimeLeft").int())));
        bk.set_bat_limit(s.get(b, "Limit").int() as i32);
        let steps: Vec<i64> = s.get(b, "Steps").list().iter().map(V::int).collect();
        bk.set_bat_idx(model::step_index(&steps, s.get(b, "Limit").int()) as i32);
        sync(
            &self.m.bat_opts,
            steps
                .iter()
                .map(|v| opt(&v.to_string(), &v.to_string()))
                .collect(),
        );
        sync(&self.m.bat_values, steps.iter().map(|v| *v as i32).collect());
        bk.set_charge_once(s.get(b, "ChargeOnce").bool());
        let health = s.get(b, "Health").float();
        bk.set_bat_health(ss(&if health >= 0.0 && s.has_prop(b, "Health") {
            format!("{health:.0}%")
        } else {
            String::new()
        }));
        bk.set_bat_cycles(ss(&if s.has_prop(b, "Cycles") {
            s.get(b, "Cycles").int().to_string()
        } else {
            String::new()
        }));

        // Fans
        let fan_list: Vec<Opt> = s
            .get("Fans1", "Fans")
            .list()
            .iter()
            .map(|f| {
                let rpm = model::rpm_of(s, f.at(0).int() as u32);
                let label = if rpm > 0 {
                    format!("{}  {rpm}", f.at(1).str())
                } else {
                    f.at(1).str().to_owned()
                };
                opt(f.at(1).str(), &label)
            })
            .collect();
        bk.set_has_fans(s.has("Fans1"));
        bk.set_fan_mode(self.fan_mode as i32);
        bk.set_fan_sel(self.fan_sel.min(fan_list.len().saturating_sub(1)) as i32);
        sync(&self.m.fan_opts, fan_list);
        self.push_curve_to(bk);

        // Power limits
        bk.set_pw_source(self.pw_source as i32);
        bk.set_pw_needs_fan(s.get("Power1", "NeedsFanCurve").bool());
        sync(
            &self.m.limits,
            model::limits(s, self.thermal(), SOURCES[self.pw_source.min(1)])
                .iter()
                .map(|l| LimitRow {
                    name: ss(&l.name),
                    label: ss(&l.label),
                    unit: ss(&l.unit),
                    min: l.min as i32,
                    max: l.max as i32,
                    def: l.def as i32,
                    cur: l.cur as i32,
                    custom: l.custom,
                })
                .collect(),
        );

        // Lighting
        let k = "Lighting1";
        bk.set_kb_max(s.get(k, "MaxBrightness").int() as i32);
        bk.set_kb_level(s.get(k, "Brightness").int() as i32);
        let ef = model::effect(s);
        sync(
            &self.m.kb_effects,
            s.get(k, "Modes").strings().iter().map(|m| opt(m, m)).collect(),
        );
        bk.set_kb_effect(ss(&if ef.mode.is_empty() {
            "Firmware".to_owned()
        } else {
            model::color_label(&ef.mode)
        }));
        bk.set_kb_color(slint::Color::from_rgb_u8(ef.color.0, ef.color.1, ef.color.2));
        bk.set_kb_color_idx(
            model::KB_COLORS
                .iter()
                .position(|c| *c == ef.color)
                .map_or(-1, |i| i as i32),
        );
        bk.set_kb_speed(
            model::KB_SPEEDS
                .iter()
                .position(|v| *v == ef.speed)
                .map_or(-1, |i| i as i32),
        );
        sync(
            &self.m.kb_states,
            s.get(k, "PowerStates").list().iter().map(V::bool).collect(),
        );
        bk.set_kb_idle(ss(&idle_label(s.get(k, "IdleTimeout").int())));

        // Lid
        let l = "Lid1";
        let devices = s.get(l, "Devices").strings();
        bk.set_has_slash(devices.iter().any(|d| d == "slash"));
        bk.set_slash_on(s.get(l, "SlashEnabled").bool());
        bk.set_slash_level(s.get(l, "SlashBrightness").int() as i32);
        let modes = s.get(l, "SlashModes").strings();
        bk.set_slash_mode_idx(
            modes
                .iter()
                .position(|m| m == s.get(l, "SlashMode").str())
                .unwrap_or(0) as i32,
        );
        sync(
            &self.m.slash_modes,
            modes.iter().map(|m| ss(&model::color_label(m))).collect(),
        );
        sync(
            &self.m.slash_show,
            s.get(l, "SlashShow")
                .list()
                .iter()
                .map(|e| Item {
                    name: ss(&format!("slash:{}", e.at(0).str())),
                    label: ss(&model::color_label(e.at(0).str())),
                    kind: ss("toggle"),
                    on: e.at(1).bool(),
                    text: ss(""),
                    min: 0,
                    max: 1,
                    value: 0,
                    writable: true,
                })
                .collect(),
        );
        bk.set_has_anime(devices.iter().any(|d| d == "anime"));
        bk.set_anime_on(s.get(l, "AnimeEnabled").bool());
        bk.set_anime_level(s.get(l, "AnimeBrightness").int() as i32);
    }

    fn push_curve_to(&self, bk: &Bk) {
        let kind = if self.edit.is_some() {
            "custom".to_owned()
        } else {
            model::fan_setting(&self.store, self.thermal(), self.fan_number().unwrap_or(1)).kind
        };
        bk.set_fan_kind(ss(&kind));
        let pts: Vec<Pt> = self
            .shown_curve()
            .iter()
            .map(|(t, p)| Pt {
                t: i32::from(*t),
                p: i32::from(*p),
            })
            .collect();
        sync(&self.m.curve, pts);
    }
}

/// The accent of a mode, tuned for the theme.
pub fn mode_color(mode: &str, dark: bool) -> slint::Color {
    let (r, g, b) = match (mode, dark) {
        ("eco-plus", true) => (0x8c, 0xd4, 0x4a),
        ("power-saver", true) => (0x35, 0xc9, 0xa6),
        ("performance", true) => (0xf2, 0x67, 0x3b),
        (_, true) => (0x4d, 0x9c, 0xf7),
        ("eco-plus", false) => (0x4f, 0x9a, 0x14),
        ("power-saver", false) => (0x0e, 0x9d, 0x7e),
        ("performance", false) => (0xd6, 0x48, 0x1c),
        (_, false) => (0x1f, 0x74, 0xdc),
    };
    slint::Color::from_rgb_u8(r, g, b)
}

fn idle_label(secs: i64) -> String {
    match secs {
        0 => "never".into(),
        s if s < 60 => format!("{s} s"),
        s => format!("{} min", s / 60),
    }
}

/// What the dGPU is doing: removed (Eco), runtime-suspended, or powered.
fn gpu_state(mode: &str, asleep: bool) -> &'static str {
    match (mode, asleep) {
        ("eco", _) => "dGPU off",
        (_, true) => "dGPU asleep",
        _ => "dGPU active",
    }
}

#[cfg(test)]
mod tests {
    use super::{gpu_state, idle_label};

    #[test]
    fn dgpu_states() {
        assert_eq!(gpu_state("eco", true), "dGPU off");
        assert_eq!(gpu_state("hybrid", true), "dGPU asleep");
        assert_eq!(gpu_state("hybrid", false), "dGPU active");
        assert_eq!(gpu_state("ultimate", false), "dGPU active");
    }

    #[test]
    fn idle_labels() {
        assert_eq!(idle_label(0), "never");
        assert_eq!(idle_label(30), "30 s");
        assert_eq!(idle_label(120), "2 min");
    }
}
