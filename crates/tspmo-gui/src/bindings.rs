//! `org.tspmo.Display1` names in one place, so a rename on the daemon side is a one-line change here.
//!
//! The daemon's flicker-free and eye-care types are still settling (`b`/`s`, `b`/`u` kelvin), so
//! values are read leniently and written in whatever type the introspected method asks for
//! (see `model::Arg::coerce`).

use crate::model::Store;
use crate::value::V;

pub const IFACE: &str = "Display1";

// Properties.
pub const REFRESH_POLICY: &str = "RefreshPolicy"; // s: max | 60 | auto
pub const REFRESH_NOW: &str = "Refresh"; // d: Hz in use
pub const REFRESH_MODES: &str = "RefreshModes"; // a(uud): width, height, Hz
pub const REFRESH_RATES: &str = "Rates"; // au: Hz, older spelling
pub const FLICKER_FREE: &str = "FlickerFree"; // b
pub const FLICKER_SETTING: &str = "FlickerSetting"; // s: auto | on | off
pub const BRIGHTNESS: &str = "Brightness"; // u: 1-100
pub const COLOR_MODE: &str = "ColorMode"; // s
pub const COLOR_MODES: &str = "ColorModes"; // as
pub const EYE_CARE: &str = "EyeCare"; // u kelvin (0 off) or b

// Methods.
pub const SET_REFRESH_POLICY: &str = "SetRefreshPolicy";
pub const SET_FLICKER_FREE: &str = "SetFlickerFree";
pub const SET_BRIGHTNESS: &str = "SetBrightness";
pub const SET_COLOR_MODE: &str = "SetColorMode";
pub const SET_EYE_CARE: &str = "SetEyeCare";

/// Eye care's default warm point when it is switched on.
pub const EYE_DEFAULT_KELVIN: i64 = 4500;

pub fn current_hz(s: &Store) -> f64 {
    let hz = s.get(IFACE, REFRESH_NOW).float();
    if hz > 0.0 {
        return hz;
    }
    // No live rate: the highest of the known modes is the best guess for "Max".
    let modes = s.get(IFACE, REFRESH_MODES).list();
    let from_modes = modes.iter().map(|m| m.at(2).float());
    let rates = s.get(IFACE, REFRESH_RATES).list().iter().map(V::float);
    from_modes.chain(rates).fold(0.0, f64::max)
}

pub fn flicker_on(s: &Store) -> bool {
    match s.get(IFACE, FLICKER_FREE) {
        V::None => s.get(IFACE, FLICKER_SETTING).bool(),
        v => v.bool(),
    }
}

pub fn eye_kelvin(s: &Store) -> i64 {
    match s.get(IFACE, EYE_CARE) {
        V::B(true) => EYE_DEFAULT_KELVIN,
        V::I(n) => *n,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(props: Vec<(&str, V)>) -> Store {
        let mut s = Store::default();
        s.merge(IFACE, props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
        s
    }

    #[test]
    fn reads_both_spellings() {
        let s = store(vec![(FLICKER_SETTING, V::S("on".into())), (EYE_CARE, V::B(true))]);
        assert!(flicker_on(&s));
        assert_eq!(eye_kelvin(&s), EYE_DEFAULT_KELVIN);
        let s = store(vec![(FLICKER_FREE, V::B(false)), (FLICKER_SETTING, V::S("on".into())), (EYE_CARE, V::I(3500))]);
        assert!(!flicker_on(&s));
        assert_eq!(eye_kelvin(&s), 3500);
    }

    #[test]
    fn refresh_falls_back_to_the_highest_mode() {
        let s = store(vec![(
            REFRESH_MODES,
            V::L(vec![
                V::L(vec![V::I(2880), V::I(1800), V::F(60.0)]),
                V::L(vec![V::I(2880), V::I(1800), V::F(240.0)]),
            ]),
        )]);
        assert_eq!(current_hz(&s), 240.0);
        let s = store(vec![(REFRESH_NOW, V::F(60.0)), (REFRESH_RATES, V::L(vec![V::I(240)]))]);
        assert_eq!(current_hz(&s), 60.0);
    }
}
