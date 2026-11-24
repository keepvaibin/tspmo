//! Wayland key and button codes to Slint events.

use slint::SharedString;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers};

#[derive(Clone, Copy)]
pub enum Press {
    Down,
    Repeat,
    Up,
}

/// Linux `BTN_*` to Slint buttons.
pub fn button(code: u32) -> Option<PointerEventButton> {
    match code {
        0x110 => Some(PointerEventButton::Left),
        0x111 => Some(PointerEventButton::Right),
        0x112 => Some(PointerEventButton::Middle),
        _ => None,
    }
}

fn special(sym: Keysym, shift: bool) -> Option<Key> {
    Some(match sym {
        Keysym::Escape => Key::Escape,
        Keysym::Return | Keysym::KP_Enter => Key::Return,
        Keysym::Tab if shift => Key::Backtab,
        Keysym::Tab => Key::Tab,
        Keysym::ISO_Left_Tab => Key::Backtab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Delete => Key::Delete,
        Keysym::Up => Key::UpArrow,
        Keysym::Down => Key::DownArrow,
        Keysym::Left => Key::LeftArrow,
        Keysym::Right => Key::RightArrow,
        Keysym::Page_Up => Key::PageUp,
        Keysym::Page_Down => Key::PageDown,
        Keysym::Home => Key::Home,
        Keysym::End => Key::End,
        _ => return None,
    })
}

pub fn key_event(e: &KeyEvent, kind: Press, shift: bool) -> Option<WindowEvent> {
    let text: SharedString = if let Some(k) = special(e.keysym, shift) {
        char::from(k).to_string().into()
    } else {
        let t = e.utf8.as_deref().filter(|t| t.chars().all(|c| !c.is_control()))?;
        if t.is_empty() {
            return None;
        }
        t.into()
    };
    Some(match kind {
        Press::Down => WindowEvent::KeyPressed { text },
        Press::Repeat => WindowEvent::KeyPressRepeated { text },
        Press::Up => WindowEvent::KeyReleased { text },
    })
}

/// Slint tracks Shift, Control, Alt and Meta from key events, so changes are sent as such.
pub fn modifier_events(old: &Modifiers, new: &Modifiers) -> Vec<WindowEvent> {
    let mut v = Vec::new();
    let mut one = |was: bool, is: bool, k: Key| {
        if was != is {
            let text: SharedString = char::from(k).to_string().into();
            v.push(if is {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            });
        }
    };
    one(old.shift, new.shift, Key::Shift);
    one(old.ctrl, new.ctrl, Key::Control);
    one(old.alt, new.alt, Key::Alt);
    one(old.logo, new.logo, Key::Meta);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(sym: Keysym, utf8: Option<&str>) -> KeyEvent {
        KeyEvent {
            time: 0,
            raw_code: 0,
            keysym: sym,
            utf8: utf8.map(str::to_owned),
        }
    }

    fn text(e: Option<WindowEvent>) -> Option<String> {
        match e? {
            WindowEvent::KeyPressed { text }
            | WindowEvent::KeyPressRepeated { text }
            | WindowEvent::KeyReleased { text } => Some(text.to_string()),
            _ => None,
        }
    }

    #[test]
    fn named_keys_become_slint_keys() {
        let esc = text(key_event(&ev(Keysym::Escape, Some("\u{1b}")), Press::Down, false));
        assert_eq!(esc, Some(char::from(Key::Escape).to_string()));
        let tab = text(key_event(&ev(Keysym::Tab, Some("\t")), Press::Down, false));
        assert_eq!(tab, Some(char::from(Key::Tab).to_string()));
        let back = text(key_event(&ev(Keysym::Tab, Some("\t")), Press::Down, true));
        assert_eq!(back, Some(char::from(Key::Backtab).to_string()));
        let left = text(key_event(&ev(Keysym::Left, None), Press::Repeat, false));
        assert_eq!(left, Some(char::from(Key::LeftArrow).to_string()));
    }

    #[test]
    fn printable_keys_pass_their_text() {
        assert_eq!(
            text(key_event(&ev(Keysym::_3, Some("3")), Press::Down, false)),
            Some("3".into())
        );
        assert_eq!(
            text(key_event(&ev(Keysym::f, Some("f")), Press::Up, false)),
            Some("f".into())
        );
        // Control characters and keys without text (a bare modifier) are dropped.
        assert!(key_event(&ev(Keysym::a, Some("\u{1}")), Press::Down, false).is_none());
        assert!(key_event(&ev(Keysym::Shift_L, None), Press::Down, false).is_none());
    }

    #[test]
    fn modifier_changes_are_key_events() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let down = modifier_events(&none, &shift);
        assert_eq!(down.len(), 1);
        assert!(matches!(down[0], WindowEvent::KeyPressed { .. }));
        let up = modifier_events(&shift, &none);
        assert!(matches!(up[0], WindowEvent::KeyReleased { .. }));
        assert!(modifier_events(&shift, &shift).is_empty());
    }

    #[test]
    fn buttons() {
        assert_eq!(button(0x110), Some(PointerEventButton::Left));
        assert_eq!(button(0x111), Some(PointerEventButton::Right));
        assert_eq!(button(0x999), None);
    }
}
