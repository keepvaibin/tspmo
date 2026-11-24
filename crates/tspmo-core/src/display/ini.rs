//! Reading and editing KDE-style INI files (`[Group]` / `key=value`) without disturbing the
//! rest of the file: comments, order and unrelated keys stay as they are.

/// The value of `key` in `[group]`, if set.
pub fn get(text: &str, group: &str, key: &str) -> Option<String> {
    let mut in_group = false;
    for line in text.lines() {
        let l = line.trim();
        if let Some(g) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            in_group = g == group;
        } else if in_group {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim().to_owned());
                }
            }
        }
    }
    None
}

/// `text` with `key` in `[group]` set to `value` (or removed when `None`). The group is
/// created when missing; an emptied group is left in place.
pub fn set(text: &str, group: &str, key: &str, value: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_group = false;
    let mut group_seen = false;
    let mut done = false;
    let mut insert_at: Option<usize> = None; // end of the group, to add the key there
    for line in text.lines() {
        let l = line.trim();
        if let Some(g) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            if in_group && insert_at.is_none() {
                insert_at = Some(out.len());
            }
            in_group = g == group;
            group_seen |= in_group;
            out.push(line.to_owned());
            continue;
        }
        if in_group {
            if let Some((k, _)) = l.split_once('=') {
                if k.trim() == key {
                    if !done {
                        if let Some(v) = value {
                            out.push(format!("{key}={v}"));
                        }
                        done = true;
                    }
                    continue;
                }
            }
        }
        out.push(line.to_owned());
    }
    if !done {
        if let Some(v) = value {
            if group_seen {
                let at = insert_at.unwrap_or(out.len());
                // Keep a trailing blank line after the group.
                let at = if at > 0 && out[at - 1].trim().is_empty() {
                    at - 1
                } else {
                    at
                };
                out.insert(at, format!("{key}={v}"));
            } else {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push(format!("[{group}]"));
                out.push(format!("{key}={v}"));
            }
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[Compositing]\nBackend=OpenGL\n\n[NightColor]\nActive=false\nNightTemperature=4000\n\n[Xwayland]\nScale=1\n";

    #[test]
    fn reads_values_by_group() {
        assert_eq!(get(SAMPLE, "NightColor", "Active").as_deref(), Some("false"));
        assert_eq!(get(SAMPLE, "Compositing", "Active"), None);
        assert_eq!(get(SAMPLE, "Nope", "Active"), None);
    }

    #[test]
    fn replaces_in_place() {
        let t = set(SAMPLE, "NightColor", "Active", Some("true"));
        assert_eq!(get(&t, "NightColor", "Active").as_deref(), Some("true"));
        assert_eq!(get(&t, "NightColor", "NightTemperature").as_deref(), Some("4000"));
        assert!(t.contains("[Xwayland]\nScale=1"));
        assert!(t.contains("Backend=OpenGL"));
    }

    #[test]
    fn adds_a_key_at_the_end_of_its_group() {
        let t = set(SAMPLE, "NightColor", "Mode", Some("Constant"));
        assert!(
            t.contains("NightTemperature=4000\nMode=Constant\n\n[Xwayland]"),
            "{t}"
        );
        // Adding to the last group appends.
        let t = set("[A]\nx=1\n", "A", "y", Some("2"));
        assert_eq!(t, "[A]\nx=1\ny=2\n");
    }

    #[test]
    fn creates_a_missing_group_and_removes_keys() {
        let t = set("[A]\nx=1\n", "NightColor", "Active", Some("true"));
        assert_eq!(t, "[A]\nx=1\n\n[NightColor]\nActive=true\n");
        let t = set(SAMPLE, "NightColor", "Active", None);
        assert_eq!(get(&t, "NightColor", "Active"), None);
        assert_eq!(get(&t, "NightColor", "NightTemperature").as_deref(), Some("4000"));
        // Removing what isn't there changes nothing but the final newline.
        assert_eq!(set(SAMPLE, "NightColor", "Mode", None), SAMPLE);
        assert_eq!(set("", "G", "k", None), "\n");
    }
}
