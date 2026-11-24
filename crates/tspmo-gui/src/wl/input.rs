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
