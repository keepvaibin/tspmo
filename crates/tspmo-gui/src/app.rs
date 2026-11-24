//! The popup: owns the store, turns daemon state into Slint properties and UI actions into calls.
//!
//! Everything here runs on the UI thread. D-Bus threads hand events over with
//! `invoke_from_event_loop`; no polling except the live readouts of the Fans and Battery pages,
//! which tick only while one of them is open.

use crate::bindings;
use crate::bus::{self, Call, Event};
use crate::live;
use crate::model::{self, Arg, Curve, MODES, SOURCES, Store, THERMAL};
use crate::value::V;
use crate::{Bk, FanRow, GpuUser, Item, LimitRow, Main, Opt, Pt, Th};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tspmo_sys::SysRoot;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct Models {
    sections: Rc<VecModel<Opt>>,
    gpu_users: Rc<VecModel<GpuUser>>,
    color_modes: Rc<VecModel<Opt>>,
    screen_items: Rc<VecModel<Item>>,
    bat_opts: Rc<VecModel<Opt>>,
    bat_values: Rc<VecModel<i32>>,
    fans: Rc<VecModel<FanRow>>,
    curve: Rc<VecModel<Pt>>,
    limits: Rc<VecModel<LimitRow>>,
    kb_effects: Rc<VecModel<Opt>>,
    kb_states: Rc<VecModel<bool>>,
    slash_modes: Rc<VecModel<SharedString>>,
    slash_show: Rc<VecModel<Item>>,
    system_items: Rc<VecModel<Item>>,
}

/// A curve being edited: shown instead of the daemon's until the daemon reports it back.
struct Edit {
    mode: &'static str,
    fan: u32,
    points: Curve,
    sent: bool,
}

struct App {
    win: slint::Weak<Main>,
    store: Store,
    m: Models,
    section: String,
    fan_mode: usize,
    fan_sel: usize,
    pw_mode: usize,
    pw_source: usize,
    fan_init: bool,
    pw_init: bool,
    edit: Option<Edit>,
    eco_missing: bool,
    loaded: bool,
    offline: bool,
    error: String,
    error_timer: Timer,
    live_timer: Timer,
    live_w: Option<f32>,
    cpu_temp: Option<i32>,
    root: SysRoot,
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

fn opt(id: &str, label: &str, icon: &str) -> Opt {
    Opt {
        id: ss(id),
        label: ss(label),
        icon: ss(icon),
    }
}

fn item_of(i: &model::HwItem) -> Item {
    Item {
        name: ss(&i.name),
        label: ss(&i.label),
        kind: ss(&i.kind),
        on: i.on(),
        text: ss(&i.current),
        choices: ModelRc::default(),
        idx: 0,
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
        if let Ok(mut g) = cell.try_borrow_mut() {
            if let Some(a) = g.as_mut() {
                f(a);
            }
        }
    });
}

/// Wire the window to the daemon and start reading. The window is already visible (or about
/// to be): nothing here waits for D-Bus.
pub fn start(win: &Main, section_hint: Option<String>) {
    let bk = win.global::<Bk>();
    let m = Models {
        sections: Rc::new(VecModel::default()),
        gpu_users: Rc::new(VecModel::default()),
        color_modes: Rc::new(VecModel::default()),
        screen_items: Rc::new(VecModel::default()),
        bat_opts: Rc::new(VecModel::default()),
        bat_values: Rc::new(VecModel::default()),
        fans: Rc::new(VecModel::default()),
        curve: Rc::new(VecModel::default()),
        limits: Rc::new(VecModel::default()),
        kb_effects: Rc::new(VecModel::default()),
        kb_states: Rc::new(VecModel::default()),
        slash_modes: Rc::new(VecModel::default()),
        slash_show: Rc::new(VecModel::default()),
        system_items: Rc::new(VecModel::default()),
    };
    bk.set_sections(m.sections.clone().into());
    bk.set_gpu_users(m.gpu_users.clone().into());
    bk.set_color_modes(m.color_modes.clone().into());
    bk.set_screen_items(m.screen_items.clone().into());
    bk.set_bat_opts(m.bat_opts.clone().into());
    bk.set_bat_values(m.bat_values.clone().into());
    bk.set_fans(m.fans.clone().into());
    bk.set_curve(m.curve.clone().into());
    bk.set_limits(m.limits.clone().into());
    bk.set_kb_effects(m.kb_effects.clone().into());
    bk.set_kb_states(m.kb_states.clone().into());
    bk.set_slash_modes(m.slash_modes.clone().into());
    bk.set_slash_show(m.slash_show.clone().into());
    bk.set_system_items(m.system_items.clone().into());

    bk.on_act(|key, arg| with_app(|a| a.act(&key, &arg)));
    bk.on_acti(|key, n| with_app(|a| a.acti(&key, i64::from(n))));
    bk.on_curve_move(|i, t, p| with_app(|a| a.curve_move(i as usize, t, p)));
    bk.on_curve_commit(|| with_app(App::curve_commit));
    bk.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    let app = App {
        win: win.as_weak(),
        store: Store::default(),
        m,
        section: section_hint.unwrap_or_default(),
        fan_mode: 1,
        fan_sel: 0,
        pw_mode: 1,
        pw_source: 0,
        fan_init: false,
        pw_init: false,
        edit: None,
        eco_missing: false,
        loaded: false,
        offline: false,
        error: String::new(),
        error_timer: Timer::default(),
        live_timer: Timer::default(),
        live_w: None,
        cpu_temp: None,
        root: SysRoot::from_env(),
    };
    APP.with(|c| *c.borrow_mut() = Some(app));
    bus::start(emit);
}

impl App {
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
                if switching && self.section == "gpu" {
                    self.fetch_gpu_users();
                }
            }
            Event::Methods(m) => self.store.methods = m,
            Event::Ready => {
                self.loaded = true;
                self.select_default_section();
                self.after_section_change();
            }
            Event::Offline(msg) => {
                self.loaded = true;
                self.offline = true;
                self.error = msg;
            }
            Event::Rpm(r) => self.store.rpm = r,
            Event::GpuUsers(u) => self.store.gpu_users = u,
            Event::Error(msg) => {
                if msg.contains("unknown profile") {
                    self.eco_missing = true;
                }
                self.set_error(msg);
            }
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
            self.pw_mode = THERMAL
                .iter()
                .position(|m| *m == self.store.get("Power1", "Mode").str())
                .unwrap_or(1);
            self.pw_source = SOURCES
                .iter()
                .position(|m| *m == self.store.get("Power1", "Source").str())
                .unwrap_or(0);
        }
    }

    fn select_default_section(&mut self) {
        let secs = model::sections(&self.store);
        if !secs.iter().any(|s| s.id == self.section) {
            self.section = secs.first().map_or(String::new(), |s| s.id.to_owned());
        }
    }

    fn set_error(&mut self, msg: String) {
        self.error = msg;
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

    /// Live readouts run only while the page that shows them is open.
    fn after_section_change(&mut self) {
        if self.section == "gpu" {
            self.fetch_gpu_users();
        }
        if matches!(self.section.as_str(), "fans" | "battery") {
            self.tick();
            self.live_timer
                .start(TimerMode::Repeated, Duration::from_secs(1), || {
                    with_app(|a| a.tick_and_push())
                });
        } else {
            self.live_timer.stop();
        }
    }

    fn tick_and_push(&mut self) {
        self.tick();
        self.push();
    }

    fn tick(&mut self) {
        match self.section.as_str() {
            "fans" => {
                self.cpu_temp = live::cpu_temp(&self.root);
                bus::read_rpm(emit);
            }
            "battery" => self.live_w = live::battery_watts(&self.root),
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // actions

    fn mode_list(&self) -> Vec<&'static str> {
        MODES
            .iter()
            .copied()
            .filter(|m| *m != "eco-plus" || !self.eco_missing)
            .collect()
    }

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
            "section" => {
                self.section = arg.to_owned();
                self.after_section_change();
            }
            "gpu" => self.send("Daemon1", "SetGpuMode", &[Arg::s(arg)]),
            "reboot" => bus::reboot(emit),
            "refresh" => self.send(bindings::IFACE, bindings::SET_REFRESH_POLICY, &[Arg::s(arg)]),
            "flicker" => self.send(
                bindings::IFACE,
                bindings::SET_FLICKER_FREE,
                &[Arg::B(arg == "on")],
            ),
            "color" => self.send(bindings::IFACE, bindings::SET_COLOR_MODE, &[Arg::s(arg)]),
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
                let mode = THERMAL[self.pw_mode.min(2)];
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
            "mode-step" => {
                let list = self.mode_list();
                let cur = s.get("Daemon1", "Profile").str();
                let i = list.iter().position(|m| *m == cur).unwrap_or(0) as i64;
                let next = list[(i + n).rem_euclid(list.len() as i64) as usize];
                self.send("Daemon1", "SetProfile", &[Arg::s(next)]);
            }
            "section-step" => {
                let secs = model::sections(s);
                if let Some(i) = secs.iter().position(|x| x.id == self.section) {
                    let j = (i as i64 + n).rem_euclid(secs.len() as i64) as usize;
                    self.section = secs[j].id.to_owned();
                    self.after_section_change();
                }
            }
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
            "pw-mode" => self.pw_mode = n.clamp(0, 2) as usize,
            "pw-source" => self.pw_source = n.clamp(0, 1) as usize,
            "kb-level" => self.send("Lighting1", "SetBrightness", &[Arg::s(""), Arg::N(n)]),
            "kb-effect-step" => {
                let modes = s.get("Lighting1", "Modes").strings();
                let mut e = model::effect(s);
                if let Some(next) = model::cycle(&modes, &e.mode, n) {
                    e.mode = next.clone();
                    self.call(Call::new(
                        "Lighting1",
                        "SetEffect",
                        vec![zbus::zvariant::Value::from(""), e.to_value()],
                    ));
                }
            }
            "kb-color" => {
                let mut e = model::effect(s);
                e.color = model::KB_COLORS[(n as usize).min(7)];
                self.call(Call::new(
                    "Lighting1",
                    "SetEffect",
                    vec![zbus::zvariant::Value::from(""), e.to_value()],
                ));
            }
            "kb-speed" => {
                let mut e = model::effect(s);
                e.speed = model::KB_SPEEDS[(n as usize).min(2)].to_owned();
                self.call(Call::new(
                    "Lighting1",
                    "SetEffect",
                    vec![zbus::zvariant::Value::from(""), e.to_value()],
                ));
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
                self.send(
                    "Power1",
                    "SetLimit",
                    &[
                        Arg::s(THERMAL[self.pw_mode.min(2)]),
                        Arg::s(SOURCES[self.pw_source.min(1)]),
                        Arg::s(name),
                        Arg::N(n),
                    ],
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
        if let Some(w) = self.win.upgrade() {
            let kind = if self.edit.is_some() {
                "custom".to_owned()
            } else {
                model::fan_setting(&self.store, self.thermal(), self.fan_number().unwrap_or(1)).kind
            };
            w.global::<Bk>().set_fan_kind(ss(&kind));
        }
    }

    // -----------------------------------------------------------------------------------------
    // state → properties

    fn push(&self) {
        let Some(w) = self.win.upgrade() else { return };
        let bk = w.global::<Bk>();
        let s = &self.store;
        let secs = model::sections(s);
        bk.set_loaded(self.loaded);
        bk.set_error(ss(&self.error));
        bk.set_section(ss(&self.section));
        sync(
            &self.m.sections,
            secs.iter().map(|x| opt(x.id, x.label, x.icon)).collect(),
        );

        let mode = s.get("Daemon1", "Profile").str();
        bk.set_mode(ss(mode));
        bk.set_eco_available(!self.eco_missing);
        bk.set_on_battery(s.get("Daemon1", "OnBattery").bool());
        w.global::<Th>().set_accent_target(if mode.is_empty() {
            slint::Color::from_rgb_u8(0x3d, 0xdc, 0x97)
        } else {
            mode_color(mode)
        });
        bk.set_head_text(ss(&head_text(s, self.live_w)));

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
        bk.set_gpu_message(ss(s.get("Daemon1", "GpuMessage").str()));
        bk.set_dgpu_asleep(s.get("Daemon1", "DgpuAsleep").bool());
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
        bk.set_color_mode(ss(&sc.color_mode));
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
                .map(|m| opt(m, &model::color_label(m), ""))
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

        // Battery
        let b = "Battery1";
        bk.set_bat_percent(if s.has_prop(b, "Percent") {
            s.get(b, "Percent").int() as i32
        } else {
            -1
        });
        bk.set_bat_status(ss(s.get(b, "Status").str()));
        let watts = self.live_w.map_or_else(|| s.get(b, "Power").float(), f64::from);
        bk.set_bat_power(ss(&model::fmt_watts(
            if s.has_prop(b, "Power") || self.live_w.is_some() {
                watts
            } else {
                -1.0
            },
        )));
        bk.set_bat_left(ss(&model::fmt_duration(s.get(b, "TimeLeft").int())));
        bk.set_bat_limit(s.get(b, "Limit").int() as i32);
        let steps: Vec<i64> = s.get(b, "Steps").list().iter().map(V::int).collect();
        bk.set_bat_idx(model::step_index(&steps, s.get(b, "Limit").int()) as i32);
        sync(
            &self.m.bat_opts,
            steps
                .iter()
                .map(|v| opt(&v.to_string(), &v.to_string(), ""))
                .collect(),
        );
        sync(&self.m.bat_values, steps.iter().map(|v| *v as i32).collect());
        bk.set_charge_once(s.get(b, "ChargeOnce").bool());
        let health = s.get(b, "Health").float();
        bk.set_bat_health(ss(&if health >= 0.0 && s.has_prop(b, "Health") {
            format!("{health:.0}%")
        } else {
            "-".into()
        }));
        bk.set_bat_cycles(ss(&if s.has_prop(b, "Cycles") {
            s.get(b, "Cycles").int().to_string()
        } else {
            "-".into()
        }));

        // Fans
        let fan_list: Vec<FanRow> = s
            .get("Fans1", "Fans")
            .list()
            .iter()
            .map(|f| {
                let n = f.at(0).int() as u32;
                FanRow {
                    label: ss(f.at(1).str()),
                    rpm: model::rpm_of(s, n) as i32,
                    kind: ss(&model::fan_setting(s, self.thermal(), n).kind),
                }
            })
            .collect();
        bk.set_fan_mode(self.fan_mode as i32);
        bk.set_fan_sel(self.fan_sel.min(fan_list.len().saturating_sub(1)) as i32);
        sync(&self.m.fans, fan_list);
        self.push_curve();
        bk.set_cpu_temp(ss(&self.cpu_temp.map_or(String::new(), |t| format!("{t}°"))));

        // Power limits
        bk.set_pw_mode(self.pw_mode as i32);
        bk.set_pw_source(self.pw_source as i32);
        bk.set_pw_needs_fan(s.get("Power1", "NeedsFanCurve").bool());
        sync(
            &self.m.limits,
            model::limits(s, THERMAL[self.pw_mode.min(2)], SOURCES[self.pw_source.min(1)])
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
            s.get(k, "Modes")
                .strings()
                .iter()
                .map(|m| opt(m, m, ""))
                .collect(),
        );
        bk.set_kb_effect(ss(&ef.mode));
        bk.set_kb_color(slint::Color::from_rgb_u8(ef.color.0, ef.color.1, ef.color.2));
        bk.set_kb_speed(model::KB_SPEEDS.iter().position(|v| *v == ef.speed).unwrap_or(1) as i32);
        sync(
            &self.m.kb_states,
            s.get(k, "PowerStates").list().iter().map(V::bool).collect(),
        );
        bk.set_kb_idle(s.get(k, "IdleTimeout").int() as i32);

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
        sync(&self.m.slash_modes, modes.iter().map(|m| ss(m)).collect());
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
                    choices: ModelRc::default(),
                    idx: 0,
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
}

pub fn mode_color(mode: &str) -> slint::Color {
    match mode {
        "eco-plus" => slint::Color::from_rgb_u8(0x3d, 0xdc, 0x97),
        "power-saver" => slint::Color::from_rgb_u8(0x4e, 0xa8, 0xff),
        "performance" => slint::Color::from_rgb_u8(0xff, 0x6a, 0x4d),
        _ => slint::Color::from_rgb_u8(0xf5, 0xb9, 0x42),
    }
}

fn head_text(s: &Store, live_w: Option<f32>) -> String {
    let b = "Battery1";
    if !s.has(b) {
        return String::new();
    }
    let mut t = format!("{}%", s.get(b, "Percent").int());
    let w = live_w.map_or_else(|| s.get(b, "Power").float(), f64::from);
    if w >= 0.0 && (live_w.is_some() || s.has_prop(b, "Power")) {
        t.push_str(&format!(" · {w:.1} W"));
    }
    t
}
