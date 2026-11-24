#![allow(dead_code)] // wired up by the work packages in docs/plan.md
//! Acting inside the user's desktop session from the root daemon: on-screen display (OSD),
//! refresh rate, brightness/dimming, colour. Owned by the display work package (see docs/plan.md).

/// Show a transient on-screen indicator (like the brightness OSD). `value`: 0–100 for a bar,
/// or `None` for text only. Never blocks the caller.
pub fn osd(icon: &str, text: &str, value: Option<u8>) {
    let _ = (icon, text, value); // implemented by the display work package
}
