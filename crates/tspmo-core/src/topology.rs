//! CPU core classes: performance, efficiency and low-power-efficiency cores.
//!
//! Intel hybrid parts list them under `/sys/devices/cpu_core` and `cpu_atom`; the low-power
//! E-cores (Meteor Lake and later) are the atom cores without an L3 cache. AMD and others fall
//! back to the maximum clock of each core (dense/compact cores clock lower).

use tspmo_sys::SysRoot;

const CPU: &str = "/sys/devices/system/cpu";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Topology {
    pub performance: Vec<u32>,
    pub efficiency: Vec<u32>,
    /// Efficiency cores on the low-power island (no L3), e.g. Meteor Lake's two LP-E cores.
    pub low_power: Vec<u32>,
}

impl Topology {
    pub fn is_hybrid(&self) -> bool {
        !self.efficiency.is_empty() || !self.low_power.is_empty()
    }

    pub fn all(&self) -> Vec<u32> {
        let mut v: Vec<u32> = [&self.performance, &self.efficiency, &self.low_power]
            .into_iter()
            .flatten()
            .copied()
            .collect();
        v.sort_unstable();
        v
    }
}

/// Parse a kernel CPU list such as `0-11,13`.
pub fn parse_cpulist(s: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => out.extend(part.trim().parse::<u32>()),
        }
    }
    out
}

/// Format CPU numbers as a kernel/systemd list (`0-11,20`).
pub fn format_cpulist(cpus: &[u32]) -> String {
    let mut sorted = cpus.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[j] + 1 {
            j += 1;
        }
        if !out.is_empty() {
            out.push(',');
        }
        if j > i {
            out.push_str(&format!("{}-{}", sorted[i], sorted[j]));
        } else {
            out.push_str(&sorted[i].to_string());
        }
        i = j + 1;
    }
    out
}

fn has_l3(root: &SysRoot, cpu: u32) -> bool {
    root.exists(format!("{CPU}/cpu{cpu}/cache/index3"))
}

pub fn detect(root: &SysRoot) -> Topology {
    let online = root
        .read_opt(format!("{CPU}/online"))
        .or_else(|| root.read_opt(format!("{CPU}/present")))
        .map(|s| parse_cpulist(&s))
        .unwrap_or_default();

    let core = root.read_opt("/sys/devices/cpu_core/cpus");
    let atom = root.read_opt("/sys/devices/cpu_atom/cpus");
    if let (Some(core), Some(atom)) = (core, atom) {
        let mut t = Topology {
            performance: parse_cpulist(&core),
            ..Topology::default()
        };
        for cpu in parse_cpulist(&atom) {
            if has_l3(root, cpu) {
                t.efficiency.push(cpu);
            } else {
                t.low_power.push(cpu);
            }
        }
        return t;
    }

    // No hybrid description: classify by maximum clock (AMD compact cores, Arm big.LITTLE).
    let freq =
        |cpu: u32| -> Option<u64> { root.read_parse(format!("{CPU}/cpu{cpu}/cpufreq/cpuinfo_max_freq")) };
    let best = online.iter().filter_map(|&c| freq(c)).max().unwrap_or(0);
    let mut t = Topology::default();
    for cpu in online {
        match freq(cpu) {
            Some(f) if best > 0 && f * 100 < best * 80 => t.efficiency.push(cpu),
            _ => t.performance.push(cpu),
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeRoot(std::path::PathBuf);
    impl FakeRoot {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("tspmo-topo-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            Self(d)
        }
        fn file(&self, p: &str, c: &str) -> &Self {
            let p = self.0.join(p.trim_start_matches('/'));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
            self
        }
        fn root(&self) -> SysRoot {
            SysRoot::new(&self.0)
        }
    }
    impl Drop for FakeRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cpulists() {
        assert_eq!(parse_cpulist("0-3,8,10-11\n"), [0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(format_cpulist(&[0, 1, 2, 3, 8, 10, 11]), "0-3,8,10-11");
        assert_eq!(format_cpulist(&[]), "");
        assert_eq!(parse_cpulist("junk,5"), [5]);
    }

    #[test]
    fn meteor_lake_split() {
        let f = FakeRoot::new("topo-mtl");
        f.file("/sys/devices/system/cpu/online", "0-21\n")
            .file("/sys/devices/cpu_core/cpus", "0-11\n")
            .file("/sys/devices/cpu_atom/cpus", "12-21\n");
        for c in 0..20 {
            f.file(
                &format!("/sys/devices/system/cpu/cpu{c}/cache/index3/level"),
                "3\n",
            );
        }
        let t = detect(&f.root());
        assert_eq!(t.performance.len(), 12);
        assert_eq!(t.efficiency, (12..20).collect::<Vec<_>>());
        assert_eq!(t.low_power, [20, 21]);
        assert!(t.is_hybrid());
        assert_eq!(t.all().len(), 22);
    }

    #[test]
    fn gu605_fixture() {
        let root = SysRoot::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/asus-gu605mi"
        ));
        let t = detect(&root);
        assert_eq!(t.performance, (0..12).collect::<Vec<_>>());
        assert_eq!(t.efficiency, (12..20).collect::<Vec<_>>());
        assert_eq!(t.low_power, [20, 21]);
    }

    #[test]
    fn clock_based_fallback_and_homogeneous() {
        let f = FakeRoot::new("topo-amd");
        f.file("/sys/devices/system/cpu/online", "0-3\n");
        for (c, khz) in [(0, 5000000), (1, 5000000), (2, 3000000), (3, 3000000)] {
            f.file(
                &format!("/sys/devices/system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"),
                &format!("{khz}\n"),
            );
        }
        let t = detect(&f.root());
        assert_eq!(t.performance, [0, 1]);
        assert_eq!(t.efficiency, [2, 3]);

        let g = FakeRoot::new("topo-flat");
        g.file("/sys/devices/system/cpu/online", "0-1\n");
        let t = detect(&g.root());
        assert_eq!(t.performance, [0, 1]);
        assert!(!t.is_hybrid());
    }
}
