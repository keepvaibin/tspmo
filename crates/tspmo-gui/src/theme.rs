//! Light or dark: follow the desktop without asking it anything over D-Bus.

/// Parse the window background from a `kdeglobals` file: dark if its luminance is low.
pub fn kde_is_dark(kdeglobals: &str) -> Option<bool> {
    let mut in_window = false;
    for line in kdeglobals.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_window = line == "[Colors:Window]";
        } else if in_window {
            if let Some(rgb) = line.strip_prefix("BackgroundNormal=") {
                let c: Vec<f64> = rgb.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                if c.len() >= 3 {
                    let lum = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                    return Some(lum < 128.0);
                }
            }
        }
    }
    None
}

pub fn is_dark() -> bool {
    match std::env::var("TSPMO_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")));
    if let Some(text) = cfg.and_then(|c| std::fs::read_to_string(c.join("kdeglobals")).ok()) {
        if let Some(d) = kde_is_dark(&text) {
            return d;
        }
    }
    if let Ok(t) = std::env::var("GTK_THEME") {
        return !t.to_lowercase().contains("light");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_window_colour() {
        let dark = "[General]\nColorScheme=BreezeDark\n[Colors:Window]\nBackgroundNormal=35,38,41\n";
        let light = "[Colors:Button]\nBackgroundNormal=0,0,0\n[Colors:Window]\nBackgroundNormal=239,240,241\n";
        assert_eq!(kde_is_dark(dark), Some(true));
        assert_eq!(kde_is_dark(light), Some(false));
        assert_eq!(kde_is_dark("[General]\n"), None);
    }
}
