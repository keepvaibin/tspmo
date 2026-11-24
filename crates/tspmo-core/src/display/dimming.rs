//! Flicker-free dimming for OLED panels.
//!
//! Many OLED panels dim with pulse-width modulation at low backlight levels, which flickers.
//! Above a threshold they dim by current, which doesn't. So the panel's own brightness never
//! goes below that threshold; the rest of the way down, the image itself is darkened in the
//! compositor.
//!
//! A *level* is what the user asked for (0..=100, the value of the brightness slider). It is
//! split into a hardware backlight value and a software dimming factor whose product is the
//! level.

/// Lowest panel brightness (percent of the panel's range) that is still flicker-free, until the
/// user says otherwise. Samsung publishes no number for its ATNA panels; this is the point
/// where the OLED panels measured in reviews stop modulating, rounded up.
pub const DEFAULT_FLOOR_PCT: u32 = 30;

/// The lowest level offered: at 0 the screen would be black with no way to see the slider.
pub const MIN_LEVEL_PCT: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Split {
    /// Hardware backlight, percent of the panel's range.
    pub backlight_pct: f64,
    /// Software dimming, percent of full image brightness (100 = untouched).
    pub dimming_pct: f64,
}

/// Split a level into hardware backlight and software dimming.
pub fn split(level_pct: f64, floor_pct: u32) -> Split {
    let level = level_pct.clamp(MIN_LEVEL_PCT, 100.0);
    let floor = f64::from(floor_pct.clamp(1, 100));
    if level >= floor {
        Split {
            backlight_pct: level,
            dimming_pct: 100.0,
        }
    } else {
        Split {
            backlight_pct: floor,
            dimming_pct: level / floor * 100.0,
        }
    }
}

/// The inverse: the level a (backlight, dimming) pair produces.
pub fn level(backlight_pct: f64, dimming_pct: f64) -> f64 {
    backlight_pct * dimming_pct / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn above_the_floor_only_the_backlight_moves() {
        let s = split(60.0, 30);
        assert_eq!((s.backlight_pct, s.dimming_pct), (60.0, 100.0));
    }

    #[test]
    fn below_the_floor_the_backlight_stays_put() {
        let s = split(15.0, 30);
        assert_eq!(s.backlight_pct, 30.0);
        assert!((s.dimming_pct - 50.0).abs() < 1e-9);
        assert!((level(s.backlight_pct, s.dimming_pct) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn product_is_always_the_level_and_backlight_never_drops() {
        for l in (1..=100).map(f64::from) {
            let s = split(l, 30);
            assert!(s.backlight_pct >= 30.0);
            assert!((level(s.backlight_pct, s.dimming_pct) - l).abs() < 1e-9);
            assert!(s.dimming_pct <= 100.0 && s.dimming_pct > 0.0);
        }
    }

    #[test]
    fn extremes_are_clamped() {
        assert_eq!(split(0.0, 30).dimming_pct, 1.0 / 30.0 * 100.0);
        assert_eq!(split(500.0, 30).backlight_pct, 100.0);
        assert_eq!(split(10.0, 0).backlight_pct, 10.0); // a floor of 0 means no floor
    }
}
