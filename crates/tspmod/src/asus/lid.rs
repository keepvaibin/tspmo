//! `org.tspmo.Lid1`: the Slash light bar (and, on models that have one, the AniMe Matrix).
//!
//! Like the keyboard, the device is opened per write and closed again. Settings the firmware
//! keeps itself (show on boot, sleep, shutdown, lid closed) are written once; "show on
//! battery" is also enforced here, because it follows the power source.

use super::thermal::lock;
use super::{PATH, authorize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tspmo_core::asus::anime::{self, Anime, Grid};
use tspmo_core::asus::slash::{self, Mode, Slash, When};
use tspmo_sys::{SysRoot, power_supply};
use zbus::message::Header;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlashCfg {
    enabled: bool,
    brightness: u8,
    interval: u8,
    mode: Mode,
    boot: bool,
    sleep: bool,
    shutdown: bool,
    /// Lit while running on battery. Off by default: it costs battery.
    battery: bool,
    lid: bool,
    warning: bool,
}

impl Default for SlashCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: 255,
            interval: 0,
            mode: Mode::Bounce,
            boot: true,
            sleep: true,
            shutdown: true,
            battery: false,
            lid: true,
            warning: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnimeCfg {
    enabled: bool,
    /// 0-3.
    brightness: u8,
    /// The firmware's own animations (boot, awake, sleep, shutdown).
    builtin: bool,
    /// Which of two animations plays in each of those states.
    choice: [u8; 4],
}

impl Default for AnimeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: 2,
            builtin: true,
            choice: [0; 4],
        }
    }
}

const OPTIONS: [&str; 6] = ["boot", "sleep", "shutdown", "battery", "lid", "warning"];

impl SlashCfg {
    fn option(&self, name: &str) -> Option<bool> {
        Some(match name {
            "boot" => self.boot,
            "sleep" => self.sleep,
            "shutdown" => self.shutdown,
            "battery" => self.battery,
            "lid" => self.lid,
            "warning" => self.warning,
            _ => return None,
        })
    }

    /// The firmware-side "show when" flags and their values.
    fn flags(&self) -> [(When, bool); 4] {
        [
            (When::Boot, self.boot),
            (When::Sleep, self.sleep),
            (When::Shutdown, self.shutdown),
            (When::LowBattery, self.warning),
        ]
    }

    fn set_option(&mut self, name: &str, v: bool) -> bool {
        let slot = match name {
            "boot" => &mut self.boot,
            "sleep" => &mut self.sleep,
            "shutdown" => &mut self.shutdown,
            "battery" => &mut self.battery,
            "lid" => &mut self.lid,
            "warning" => &mut self.warning,
            _ => return false,
        };
        *slot = v;
        true
    }
}

struct State {
    cfg: SlashCfg,
    anime: AnimeCfg,
    on_battery: bool,
}

pub struct Lid {
    root: SysRoot,
    slash: Option<Slash>,
    anime: Option<Anime>,
    file: PathBuf,
    st: Mutex<State>,
    /// Eco+ is active: the light bar stays dark.
    eco: std::sync::atomic::AtomicBool,
}

impl Lid {
    /// `None` if the machine has no lid display.
    pub fn new(root: SysRoot, dir: PathBuf) -> Option<Self> {
        let slash = slash::detect(&root);
        let anime = anime::detect(&root);
        if slash.is_none() && anime.is_none() {
            return None;
        }
        let lid = Self {
            file: dir.join("lid"),
            st: Mutex::new(State {
                cfg: SlashCfg::default(),
                anime: AnimeCfg::default(),
                on_battery: power_supply::on_ac(&root) == Some(false),
            }),
            slash,
            anime,
            root,
            eco: false.into(),
        };
        lid.load();
        Some(lid)
    }

    /// First start: the settings are whatever the firmware has, not guesses.
    fn adopt_firmware(&self) {
        let Some(s) = &self.slash else { return };
        let Some(live) = s.open(&self.root).ok().and_then(|mut d| d.read()) else {
            return;
        };
        let mut st = lock(&self.st);
        let c = &mut st.cfg;
        c.enabled = live.enabled;
        c.brightness = live.brightness;
        c.interval = live.interval;
        c.mode = live.mode.unwrap_or(c.mode);
        c.boot = live.boot.unwrap_or(c.boot);
        c.sleep = live.sleep.unwrap_or(c.sleep);
        c.shutdown = live.shutdown.unwrap_or(c.shutdown);
        c.warning = live.low_battery.unwrap_or(c.warning);
    }

    fn load(&self) {
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            self.adopt_firmware();
            return;
        };
        let mut st = lock(&self.st);
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let (k, v) = (k.trim(), v.trim());
            let st = &mut *st;
            let c = &mut st.cfg;
            match k {
                "enabled" => c.enabled = v == "1",
                "brightness" => c.brightness = v.parse().unwrap_or(c.brightness),
                "interval" => {
                    c.interval = v.parse::<u8>().unwrap_or(c.interval).min(slash::MAX_INTERVAL);
                }
                "mode" => c.mode = Mode::parse(v).unwrap_or(c.mode),
                "anime_enabled" => st.anime.enabled = v == "1",
                "anime_brightness" => st.anime.brightness = v.parse::<u8>().unwrap_or(2).min(3),
                "anime_builtin" => st.anime.builtin = v == "1",
                "anime_choice" => {
                    let n: Vec<u8> = v
                        .split(',')
                        .filter_map(|x| x.parse::<u8>().ok())
                        .map(|x| x & 1)
                        .collect();
                    if let Ok(a) = <[u8; 4]>::try_from(n) {
                        st.anime.choice = a;
                    }
                }
                other => {
                    c.set_option(other, v == "1");
                }
            }
        }
    }

    fn save(&self, st: &State) {
        let c = &st.cfg;
        let mut t = String::from("# tspmod lid display (managed by tspmod)\n");
        t.push_str(&format!(
            "enabled={}\nbrightness={}\ninterval={}\nmode={}\n",
            u8::from(c.enabled),
            c.brightness,
            c.interval,
            c.mode.name()
        ));
        for o in OPTIONS {
            t.push_str(&format!("{o}={}\n", u8::from(c.option(o).unwrap_or(false))));
        }
        let a = &st.anime;
        t.push_str(&format!(
            "anime_enabled={}\nanime_brightness={}\nanime_builtin={}\nanime_choice={}\n",
            u8::from(a.enabled),
            a.brightness,
            u8::from(a.builtin),
            a.choice.map(|c| c.to_string()).join(",")
        ));
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    fn send(&self, packets: &[Vec<u8>]) {
        if packets.is_empty() {
            return;
        }
        if let Some(s) = &self.slash {
            if let Err(e) = s.send(&self.root, packets) {
                eprintln!("tspmod: slash: {e}");
            }
        }
    }

    /// Lit now: enabled, never in Eco+, and either on AC or allowed on battery.
    fn lit(c: &SlashCfg, on_battery: bool, eco: bool) -> bool {
        c.enabled && !eco && (!on_battery || c.battery)
    }

    fn display_packets(&self, c: &SlashCfg, on_battery: bool) -> [Vec<u8>; 2] {
        let id = self.report();
        let lit = Self::lit(c, on_battery, self.eco.load(std::sync::atomic::Ordering::Relaxed));
        [
            slash::enable(id, lit),
            slash::options(id, lit, c.brightness, c.interval),
        ]
    }

    fn report(&self) -> u8 {
        self.slash.as_ref().map_or(0, |s| s.report)
    }

    /// Everything, in the order the firmware wants it (start-up, resume).
    pub fn apply(&self) {
        let (c, on_battery) = {
            let st = lock(&self.st);
            (st.cfg.clone(), st.on_battery)
        };
        self.apply_anime();
        if self.slash.is_none() {
            return;
        }
        let id = self.report();
        let [i0, i1] = slash::init(id);
        let [en, op] = self.display_packets(&c, on_battery);
        if let Some(s) = &self.slash {
            let r = s.open(&self.root).and_then(|mut d| {
                d.write(&[i0, i1, en, op])?;
                d.set_mode(c.mode)?;
                d.write(&[slash::save(id)])?;
                for (w, on) in c.flags() {
                    d.set_flag(w, on)?;
                }
                d.write(&[
                    slash::show_on_battery(id, c.battery),
                    slash::show_on_lid_closed(id, c.lid),
                    slash::save(id),
                ])?;
                // The firmware says what it took; a difference is worth a log line, not a retry.
                if let Some(live) = d.read() {
                    let lit = Self::lit(
                        &c,
                        on_battery,
                        self.eco.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    if (live.enabled, live.brightness, live.interval, live.mode)
                        != (lit, c.brightness, c.interval, Some(c.mode))
                    {
                        eprintln!("tspmod: slash: wrote {c:?} (lit {lit}) but the firmware reports {live:?}");
                    }
                }
                Ok(())
            });
            if let Err(e) = r {
                eprintln!("tspmod: slash: {e}");
            }
        }
        // Remember the defaults on first start.
        self.save(&lock(&self.st));
    }

    fn send_anime(&self, packets: &[Vec<u8>]) {
        if let Some(a) = &self.anime {
            if let Err(e) = a.send(&self.root, packets) {
                eprintln!("tspmod: anime matrix: {e}");
            }
        }
    }

    fn anime_state_packets(c: &AnimeCfg) -> Vec<Vec<u8>> {
        let [b, a, s, d] = c.choice;
        vec![
            anime::enable(c.enabled).to_vec(),
            anime::brightness(if c.enabled { c.brightness } else { 0 }).to_vec(),
            anime::builtin(c.builtin).to_vec(),
            anime::builtin_choice(b, a, s, d).to_vec(),
        ]
    }

    fn apply_anime(&self) {
        if self.anime.is_none() {
            return;
        }
        let c = lock(&self.st).anime.clone();
        let mut p: Vec<Vec<u8>> = anime::init().iter().map(|x| x.to_vec()).collect();
        p.extend(Self::anime_state_packets(&c));
        self.send_anime(&p);
    }

    fn change_anime(&self, f: impl FnOnce(&mut AnimeCfg)) -> Result<(), String> {
        if self.anime.is_none() {
            return Err("no AniMe Matrix on this laptop".into());
        }
        let mut st = lock(&self.st);
        f(&mut st.anime);
        self.save(&st);
        let p = Self::anime_state_packets(&st.anime);
        drop(st);
        self.send_anime(&p);
        Ok(())
    }

    pub fn set_anime_enabled(&self, on: bool) -> Result<(), String> {
        self.change_anime(|c| c.enabled = on)
    }

    pub fn set_anime_brightness(&self, level: u8) -> Result<(), String> {
        if level > 3 {
            return Err("brightness must be 0-3".into());
        }
        self.change_anime(|c| c.brightness = level)
    }

    pub fn set_anime_builtin(&self, on: bool) -> Result<(), String> {
        self.change_anime(|c| c.builtin = on)
    }

    pub fn set_anime_choice(&self, choice: [u8; 4]) -> Result<(), String> {
        if choice.iter().any(|c| *c > 1) {
            return Err("each animation is 0 or 1".into());
        }
        self.change_anime(|c| c.choice = choice)
    }

    fn show_grid(&self, g: &Grid) -> Result<(), String> {
        let a = self.anime.as_ref().ok_or("no AniMe Matrix on this laptop")?;
        let (leds, bright) = (g.leds(a.kind)?, lock(&self.st).anime.brightness);
        let mut p: Vec<Vec<u8>> = vec![anime::brightness(bright.max(1)).to_vec()];
        p.extend(anime::frame(a.kind, &leds)?.iter().map(|x| x.to_vec()));
        p.push(anime::flush().to_vec());
        self.send_anime(&p);
        Ok(())
    }

    pub fn show_text(&self, text: &str) -> Result<(), String> {
        let a = self.anime.as_ref().ok_or("no AniMe Matrix on this laptop")?;
        let (w, h) = a.kind.size();
        self.show_grid(&Grid::text(w, h, text))
    }

    /// A PGM (P2 or P5) image, scaled to the display.
    pub fn show_image(&self, pgm: &[u8]) -> Result<(), String> {
        let a = self.anime.as_ref().ok_or("no AniMe Matrix on this laptop")?;
        let (w, h) = a.kind.size();
        self.show_grid(&Grid::from_pgm(pgm, w, h)?)
    }

    /// Eco+ starts or ends: the light bar goes dark, or back to what the settings say.
    pub fn set_eco(&self, eco: bool) {
        if self.eco.swap(eco, std::sync::atomic::Ordering::Relaxed) == eco || self.slash.is_none() {
            return;
        }
        let (c, on_battery) = {
            let st = lock(&self.st);
            (st.cfg.clone(), st.on_battery)
        };
        self.send(&self.display_packets(&c, on_battery));
    }

    pub fn on_power_source(&self, on_battery: bool) {
        let c = {
            let mut st = lock(&self.st);
            if st.on_battery == on_battery {
                return;
            }
            st.on_battery = on_battery;
            st.cfg.clone()
        };
        self.send(&self.display_packets(&c, on_battery));
    }

    fn change(&self, f: impl FnOnce(&mut SlashCfg) -> Result<Vec<Vec<u8>>, String>) -> Result<(), String> {
        let mut st = lock(&self.st);
        let on_battery = st.on_battery;
        let packets = f(&mut st.cfg)?;
        let _ = on_battery;
        self.save(&st);
        drop(st);
        self.send(&packets);
        Ok(())
    }

    pub fn set_enabled(&self, on: bool) -> Result<(), String> {
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            c.enabled = on;
            Ok(self.display_packets(c, on_battery).to_vec())
        })
    }

    pub fn set_brightness(&self, v: u8) -> Result<(), String> {
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            c.brightness = v;
            Ok(self.display_packets(c, on_battery).to_vec())
        })
    }

    pub fn set_interval(&self, v: u8) -> Result<(), String> {
        if v > slash::MAX_INTERVAL {
            return Err(format!(
                "interval must be 0-{} seconds (0: no pause between animations)",
                slash::MAX_INTERVAL
            ));
        }
        let on_battery = lock(&self.st).on_battery;
        self.change(|c| {
            c.interval = v;
            Ok(self.display_packets(c, on_battery).to_vec())
        })
    }

    pub fn set_mode(&self, name: &str) -> Result<(), String> {
        let m = Mode::parse(name).ok_or_else(|| {
            format!(
                "unknown mode '{name}' (use {})",
                Mode::ALL.iter().map(|m| m.name()).collect::<Vec<_>>().join(", ")
            )
        })?;
        self.change(|c| {
            c.mode = m;
            Ok(Vec::new())
        })?;
        let id = self.report();
        self.with_session(|d| {
            d.set_mode(m)?;
            d.write(&[slash::save(id)])
        });
        Ok(())
    }

    pub fn set_show(&self, what: &str, on: bool) -> Result<(), String> {
        let id = self.report();
        let on_battery = lock(&self.st).on_battery;
        let mut flag = None;
        self.change(|c| {
            if !c.set_option(what, on) {
                return Err(format!("unknown option '{what}' (use {})", OPTIONS.join(", ")));
            }
            flag = match what {
                "boot" => Some(When::Boot),
                "sleep" => Some(When::Sleep),
                "shutdown" => Some(When::Shutdown),
                "warning" => Some(When::LowBattery),
                _ => None,
            };
            Ok(match what {
                "battery" => {
                    let mut v = vec![slash::show_on_battery(id, on)];
                    v.extend(self.display_packets(c, on_battery));
                    v
                }
                "lid" => vec![slash::show_on_lid_closed(id, on), slash::save(id)],
                _ => Vec::new(),
            })
        })?;
        if let Some(w) = flag {
            self.with_session(|d| d.set_flag(w, on));
        }
        Ok(())
    }

    /// An exchange with the Slash that reads before it writes.
    fn with_session(&self, f: impl FnOnce(&mut slash::Session) -> std::io::Result<()>) {
        let Some(s) = &self.slash else { return };
        if let Err(e) = s.open(&self.root).and_then(|mut d| f(&mut d)) {
            eprintln!("tspmod: slash: {e}");
        }
    }
}

pub struct Lid1(pub Arc<Lid>);

#[zbus::interface(name = "org.tspmo.Lid1")]
impl Lid1 {
    /// Lid displays this laptop has: `slash`, `anime`.
    #[zbus(property)]
    fn devices(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.0.slash.is_some() {
            v.push("slash".to_owned());
        }
        if self.0.anime.is_some() {
            v.push("anime".to_owned());
        }
        v
    }

    #[zbus(property)]
    fn anime_enabled(&self) -> bool {
        lock(&self.0.st).anime.enabled
    }

    /// 0-3.
    #[zbus(property)]
    fn anime_brightness(&self) -> u8 {
        lock(&self.0.st).anime.brightness
    }

    /// The firmware's own animations are on.
    #[zbus(property)]
    fn anime_builtin(&self) -> bool {
        lock(&self.0.st).anime.builtin
    }

    /// Which animation (0 or 1) plays at boot, awake, sleep, shutdown.
    #[zbus(property)]
    fn anime_builtin_choice(&self) -> (u8, u8, u8, u8) {
        let c = lock(&self.0.st).anime.choice;
        (c[0], c[1], c[2], c[3])
    }

    async fn set_anime_enabled(
        &self,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_anime_enabled(on)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_anime_brightness(
        &self,
        level: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_anime_brightness(level)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_anime_builtin(
        &self,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_anime_builtin(on)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_anime_builtin_choice(
        &self,
        boot: u8,
        awake: u8,
        sleep: u8,
        shutdown: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_anime_choice([boot, awake, sleep, shutdown])
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    /// Show text (5x7 font, upper case) on the AniMe Matrix.
    async fn show_anime_text(
        &self,
        text: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0.show_text(&text).map_err(zbus::fdo::Error::InvalidArgs)
    }

    /// Show a PGM image (P2 or P5), scaled to the display.
    async fn show_anime_image(
        &self,
        pgm: Vec<u8>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0.show_image(&pgm).map_err(zbus::fdo::Error::InvalidArgs)
    }

    #[zbus(property)]
    fn slash_enabled(&self) -> bool {
        lock(&self.0.st).cfg.enabled
    }

    /// 0-255.
    #[zbus(property)]
    fn slash_brightness(&self) -> u8 {
        lock(&self.0.st).cfg.brightness
    }

    /// Whether the Slash is lighting right now: enabled, and either on AC or allowed on
    /// battery (`SlashShow` "battery").
    #[zbus(property)]
    fn slash_lit(&self) -> bool {
        let st = lock(&self.0.st);
        Lid::lit(
            &st.cfg,
            st.on_battery,
            self.0.eco.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Pause between animations in seconds, 0 to `SlashIntervalMax` (0: none).
    #[zbus(property)]
    fn slash_interval(&self) -> u8 {
        lock(&self.0.st).cfg.interval
    }

    #[zbus(property)]
    fn slash_interval_max(&self) -> u8 {
        slash::MAX_INTERVAL
    }

    /// The brightness steps of the firmware's own apps, `(name, value)`; any value 0-255 works.
    #[zbus(property)]
    fn slash_brightness_levels(&self) -> Vec<(String, u8)> {
        slash::BRIGHTNESS_LEVELS
            .iter()
            .map(|(n, v)| ((*n).to_owned(), *v))
            .collect()
    }

    #[zbus(property)]
    fn slash_mode(&self) -> String {
        lock(&self.0.st).cfg.mode.name().to_owned()
    }

    #[zbus(property)]
    fn slash_modes(&self) -> Vec<String> {
        Mode::ALL.iter().map(|m| m.name().to_owned()).collect()
    }

    /// `(name, on)`: boot, sleep, shutdown, battery, lid, warning.
    #[zbus(property)]
    fn slash_show(&self) -> Vec<(String, bool)> {
        let st = lock(&self.0.st);
        OPTIONS
            .iter()
            .map(|o| ((*o).to_owned(), st.cfg.option(o).unwrap_or(false)))
            .collect()
    }

    async fn set_slash_enabled(
        &self,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0.set_enabled(on).map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_brightness(
        &self,
        level: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_brightness(level)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_interval(
        &self,
        seconds: u8,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_interval(seconds)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_mode(
        &self,
        mode: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0.set_mode(&mode).map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }

    async fn set_slash_show(
        &self,
        what: String,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        authorize(conn, &hdr, super::SET_LID).await?;
        self.0
            .set_show(&what, on)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        changed(conn).await;
        Ok(())
    }
}

pub async fn changed(conn: &zbus::Connection) {
    let Ok(r) = conn.object_server().interface::<_, Lid1>(PATH).await else {
        return;
    };
    let i = r.get().await;
    let e = r.signal_emitter();
    let _ = i.slash_enabled_changed(e).await;
    let _ = i.slash_lit_changed(e).await;
    let _ = i.slash_brightness_changed(e).await;
    let _ = i.slash_interval_changed(e).await;
    let _ = i.slash_mode_changed(e).await;
    let _ = i.slash_show_changed(e).await;
    let _ = i.anime_enabled_changed(e).await;
    let _ = i.anime_brightness_changed(e).await;
    let _ = i.anime_builtin_changed(e).await;
    let _ = i.anime_builtin_choice_changed(e).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tspmo_core::asus::testfix::Fix;

    fn lid(fx: &Fix) -> Lid {
        let root = fx.root();
        std::fs::create_dir_all(root.path("/dev")).unwrap();
        std::fs::write(root.path("/dev/hidraw1"), b"").unwrap();
        Lid::new(root, fx.dir().join("state")).unwrap()
    }

    fn sent(fx: &Fix) -> Vec<u8> {
        std::fs::read(fx.root().path("/dev/hidraw1")).unwrap()
    }

    fn clear(fx: &Fix) {
        std::fs::write(fx.root().path("/dev/hidraw1"), b"").unwrap();
    }

    #[test]
    fn dark_on_battery_by_default() {
        let fx = Fix::new("lid-battery");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        let l = lid(&fx);
        clear(&fx);
        l.on_power_source(true);
        // enable off + options off
        let b = sent(&fx);
        assert_eq!(&b[..6], &[0x5e, 0xd8, 2, 0, 1, 0x80]);
        assert_eq!(b[6 + 8], 0, "options: disabled");
        clear(&fx);
        l.on_power_source(false);
        let b = sent(&fx);
        assert_eq!(b[5], 0x00);
        assert_eq!(b[6 + 8], 1);
        // Brightness is carried.
        assert_eq!(b[6 + 10], 255);
    }

    #[test]
    fn eco_plus_keeps_the_light_bar_dark_and_leaving_it_lights_it_again() {
        let fx = Fix::new("lid-eco");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        let l = lid(&fx);
        clear(&fx);
        l.set_eco(true);
        assert_eq!(sent(&fx)[6 + 8], 0, "options: disabled");
        // A settings change in Eco+ stays dark.
        clear(&fx);
        l.set_brightness(100).unwrap();
        assert_eq!(sent(&fx)[6 + 8], 0);
        clear(&fx);
        l.set_eco(false);
        assert_eq!(sent(&fx)[6 + 8], 1);
        // No change, no packet.
        clear(&fx);
        l.set_eco(false);
        assert!(sent(&fx).is_empty());
    }

    #[test]
    fn battery_option_keeps_it_lit() {
        let fx = Fix::new("lid-show-battery");
        let l = lid(&fx);
        l.on_power_source(true);
        clear(&fx);
        l.set_show("battery", true).unwrap();
        let b = sent(&fx);
        // show_on_battery packet first (6 bytes), then enable + options.
        assert_eq!(&b[..6], &[0x5e, 0xd8, 1, 0, 1, 0x00]);
        assert_eq!(b[6 + 5], 0x00);
    }

    #[test]
    fn settings_persist_and_validate() {
        let fx = Fix::new("lid-persist");
        {
            let l = lid(&fx);
            l.set_mode("flux").unwrap();
            l.set_brightness(100).unwrap();
            l.set_show("lid", false).unwrap();
            assert!(l.set_mode("nope").is_err());
            assert!(l.set_show("nope", true).is_err());
        }
        let l = lid(&fx);
        let st = lock(&l.st);
        assert_eq!(st.cfg.mode, Mode::Flux);
        assert_eq!(st.cfg.brightness, 100);
        assert!(!st.cfg.lid);
        assert!(!st.cfg.battery);
    }

    #[test]
    fn nothing_without_the_device() {
        let fx = Fix::new("lid-none");
        fx.set("/sys/class/dmi/id/board_name", "FX507ZC4\n");
        assert!(Lid::new(fx.root(), fx.dir().join("state")).is_none());
    }

    #[test]
    fn anime_matrix_is_driven_on_models_that_have_one() {
        let fx = Fix::new("lid-anime");
        // A GA402 uses the same USB device as the Slash does on the GU605.
        fx.set("/sys/class/dmi/id/board_name", "GA402XV\n");
        let l = lid(&fx);
        assert!(l.anime.is_some() && l.slash.is_none());
        clear(&fx);
        l.set_anime_brightness(3).unwrap();
        let b = sent(&fx);
        assert_eq!(b.len(), 4 * 640);
        assert_eq!(&b[..3], &[0x5e, 0xc3, 0x01]); // enable
        assert_eq!(&b[640..644], &[0x5e, 0xc0, 0x04, 3]); // brightness
        assert!(l.set_anime_brightness(4).is_err());
        assert!(l.set_anime_choice([0, 2, 0, 0]).is_err());

        clear(&fx);
        l.show_text("HI").unwrap();
        let b = sent(&fx);
        // brightness, three panes, flush
        assert_eq!(b.len(), 5 * 640);
        assert_eq!(&b[4 * 640..4 * 640 + 3], &[0x5e, 0xc0, 0x03]);
        assert!(b[640..4 * 640].contains(&255), "dots are lit");
        assert!(l.show_image(b"junk").is_err());
        // The Slash settings stay inert without a Slash.
        l.on_power_source(true);
    }

    #[test]
    fn no_anime_on_the_slash_model() {
        let fx = Fix::new("lid-no-anime");
        let l = lid(&fx);
        assert!(l.anime.is_none());
        assert!(l.set_anime_enabled(true).is_err());
        assert!(l.show_text("x").is_err());
    }

    #[test]
    fn full_apply_order() {
        let fx = Fix::new("lid-apply");
        fx.set("/sys/class/power_supply/ACAD/online", "1\n");
        let l = lid(&fx);
        clear(&fx);
        l.apply();
        let b = sent(&fx);
        assert_eq!(&b[..6], &[0x5e, 0xd7, 0, 0, 1, 0xac]); // init
        assert!(b.windows(6).any(|w| w == [0x5e, 0xd4, 0, 0, 1, 0xab])); // save
        assert!(b.ends_with(&[0x5e, 0xd4, 0, 0, 1, 0xab]));
    }
}
