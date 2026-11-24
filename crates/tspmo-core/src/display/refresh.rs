//! Refresh-rate policy: which mode of an output to use for a wanted rate, at the current
//! resolution.

use super::kscreen::{Mode, Output};

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Always the highest rate.
    Max,
    /// Always about 60 Hz.
    Sixty,
    /// 60 Hz on battery (and always in Eco+), the highest rate on AC.
    #[default]
    Auto,
}

impl Policy {
    pub const ALL: [Policy; 3] = [Policy::Max, Policy::Sixty, Policy::Auto];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Sixty => "60",
            Self::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().trim_end_matches("hz").trim() {
            "max" | "high" | "highest" => Some(Self::Max),
            "60" | "low" => Some(Self::Sixty),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// The rate class to use now.
    pub fn target(self, on_battery: bool, eco_plus: bool) -> Target {
        match self {
            Self::Max if !eco_plus => Target::Max,
            Self::Auto if !on_battery && !eco_plus => Target::Max,
            // Eco+ always saves the display power, whatever the setting.
            _ => Target::Sixty,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Max,
    Sixty,
}

/// Modes at the output's current resolution, slowest first.
pub fn at_current_resolution(o: &Output) -> Vec<&Mode> {
    let Some(cur) = o.current_mode() else {
        return Vec::new();
    };
    let mut v: Vec<&Mode> = o
        .modes
        .iter()
        .filter(|m| (m.width, m.height) == (cur.width, cur.height))
        .collect();
    v.sort_by(|a, b| a.hz.total_cmp(&b.hz));
    v
}

/// The distinct rates (rounded) at the current resolution, ascending.
pub fn rates(o: &Output) -> Vec<u32> {
    let mut r: Vec<u32> = at_current_resolution(o)
        .iter()
        .map(|m| m.hz.round() as u32)
        .collect();
    r.dedup();
    r
}

/// The mode to switch to, or `None` when the output is already there (or has no choice).
pub fn pick(o: &Output, target: Target) -> Option<&Mode> {
    let modes = at_current_resolution(o);
    let want = match target {
        Target::Max => modes.last()?,
        // The mode nearest 60 Hz, preferring the lower one on a tie.
        Target::Sixty => modes
            .iter()
            .min_by(|a, b| (a.hz - 60.0).abs().total_cmp(&(b.hz - 60.0).abs()))?,
    };
    (!want.current).then_some(*want)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::kscreen::{parse_outputs, tests::SAMPLE};

    #[test]
    fn policy_targets() {
        assert_eq!(Policy::Auto.target(true, false), Target::Sixty);
        assert_eq!(Policy::Auto.target(false, false), Target::Max);
        assert_eq!(Policy::Auto.target(false, true), Target::Sixty);
        assert_eq!(Policy::Max.target(true, false), Target::Max);
        assert_eq!(Policy::Max.target(false, true), Target::Sixty);
        assert_eq!(Policy::Sixty.target(false, false), Target::Sixty);
        assert_eq!(Policy::parse("60 Hz"), Some(Policy::Sixty));
        assert_eq!(Policy::parse("Max"), Some(Policy::Max));
        assert_eq!(Policy::parse("fast"), None);
    }

    #[test]
    fn picks_at_the_same_resolution() {
        let outs = parse_outputs(SAMPLE);
        let p = &outs[0];
        assert_eq!(rates(p), [60, 240]);
        assert_eq!(pick(p, Target::Sixty).unwrap().id, "2");
        assert!(pick(p, Target::Max).is_none()); // already at 240
        // Never a different resolution: the 1600x1200 modes are ignored.
        let mut p = p.clone();
        for m in &mut p.modes {
            m.current = m.id == "2";
        }
        assert_eq!(pick(&p, Target::Max).unwrap().id, "1");
        assert!(pick(&p, Target::Sixty).is_none());
    }

    #[test]
    fn nearest_to_sixty() {
        let mut o = Output::default();
        for (i, hz) in [48.0, 59.94, 120.0].into_iter().enumerate() {
            o.modes.push(Mode {
                id: i.to_string(),
                width: 1920,
                height: 1080,
                hz,
                current: i == 2,
            });
        }
        assert_eq!(pick(&o, Target::Sixty).unwrap().hz, 59.94);
    }
}
