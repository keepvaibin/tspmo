//! Light or dark: follow the desktop without asking it anything over D-Bus.

/// Parse the window background from a `kdeglobals` file: dark if its luminance is low.
pub fn kde_is_dark(kdeglobals: &str) -> Option<bool> {
    let mut in_window = false;
    for line in kdeglobals.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_window = line == "[Colors:Window]";
        } else if in_window && let Some(rgb) = line.strip_prefix("BackgroundNormal=") {
            let c: Vec<f64> = rgb.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            if c.len() >= 3 {
                let lum = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                return Some(lum < 128.0);
            }
        }
    }
    None
}

/// The system text: family, size in logical px (Qt's 96 dpi: 10 pt is 13.33 px) and the fixed
/// family for numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    pub family: String,
    pub px: f32,
    pub mono: String,
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts {
            family: String::new(),
            px: 10.0 * 96.0 / 72.0,
            mono: "monospace".into(),
        }
    }
}

/// `"Noto Sans,10,-1,5,400,0,..."` (a Qt font string) to family and points.
pub fn parse_font(spec: &str) -> Option<(String, f32)> {
    let mut f = spec.split(',');
    let family = f.next()?.trim();
    let pt: f32 = f.next()?.trim().parse().ok()?;
    (!family.is_empty() && pt > 0.0).then(|| (family.to_owned(), pt))
}

fn general_key(text: &str, key: &str) -> Option<String> {
    let mut in_general = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_general = line == "[General]";
        } else if in_general && let Some(v) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            return Some(v.to_owned());
        }
    }
    None
}

pub fn fonts_from(kdeglobals: &str) -> Fonts {
    let mut f = Fonts::default();
    if let Some((family, pt)) = general_key(kdeglobals, "font").and_then(|v| parse_font(&v)) {
        f.family = family;
        f.px = pt * 96.0 / 72.0;
    }
    if let Some((family, _)) = general_key(kdeglobals, "fixed").and_then(|v| parse_font(&v)) {
        f.mono = family;
    }
    f
}

/// `TSPMO_FONT="Family,pt"` overrides, for testing text scaling.
pub fn fonts() -> Fonts {
    let text = kdeglobals_text().unwrap_or_default();
    let mut f = fonts_from(&text);
    if let Ok(v) = std::env::var("TSPMO_FONT")
        && let Some((family, pt)) = parse_font(&v)
    {
        if !family.is_empty() {
            f.family = family;
        }
        f.px = pt * 96.0 / 72.0;
    }
    f
}

fn kdeglobals_text() -> Option<String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")))?;
    std::fs::read_to_string(cfg.join("kdeglobals")).ok()
}

pub fn is_dark() -> bool {
    match std::env::var("TSPMO_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    if let Some(text) = kdeglobals_text()
        && let Some(d) = kde_is_dark(&text)
    {
        return d;
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
    fn reads_fonts() {
        let g = "[General]\nfont=Inter,11,-1,5,400,0,0,0,0,0,0,0,0,0,0,1\nfixed=JetBrains Mono,10,-1,5,400,0\n[Other]\nfont=X,9\n";
        let f = fonts_from(g);
        assert_eq!((f.family.as_str(), f.mono.as_str()), ("Inter", "JetBrains Mono"));
        assert!((f.px - 14.667).abs() < 0.01);
        assert_eq!(fonts_from("[General]\n"), Fonts::default());
    }

    #[test]
    fn reads_the_window_colour() {
        let dark = "[General]\nColorScheme=BreezeDark\n[Colors:Window]\nBackgroundNormal=35,38,41\n";
        let light =
            "[Colors:Button]\nBackgroundNormal=0,0,0\n[Colors:Window]\nBackgroundNormal=239,240,241\n";
        assert_eq!(kde_is_dark(dark), Some(true));
        assert_eq!(kde_is_dark(light), Some(false));
        assert_eq!(kde_is_dark("[General]\n"), None);
    }
}
