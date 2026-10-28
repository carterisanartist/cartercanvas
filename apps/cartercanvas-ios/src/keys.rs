//! Hardware keyboard input: `UIKeyboardHIDUsage` codes and `UIKeyModifierFlags` from the host.

use egui::{Key, Modifiers};

/// `UIKeyModifierFlags` bits.
pub const MOD_SHIFT: u32 = 1 << 17;
pub const MOD_CONTROL: u32 = 1 << 18;
pub const MOD_ALTERNATE: u32 = 1 << 19;
pub const MOD_COMMAND: u32 = 1 << 20;

/// egui modifiers for `UIKeyModifierFlags`. ⌘ is egui's `command`, as on macOS.
pub fn modifiers(flags: u32) -> Modifiers {
    let cmd = flags & MOD_COMMAND != 0;
    Modifiers { alt: flags & MOD_ALTERNATE != 0, ctrl: flags & MOD_CONTROL != 0, shift: flags & MOD_SHIFT != 0, mac_cmd: cmd, command: cmd }
}

/// The egui key for a USB HID keyboard usage (page 7), as `UIKey.keyCode` reports it.
pub fn key(usage: u32) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0];
    const F1_12: [Key; 12] = [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
    const F13_20: [Key; 8] = [Key::F13, Key::F14, Key::F15, Key::F16, Key::F17, Key::F18, Key::F19, Key::F20];
    const KEYPAD_DIGITS: [Key; 10] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0];
    let at = |table: &[Key], first: u32| usize::try_from(usage - first).ok().and_then(|i| table.get(i).copied());
    Some(match usage {
        0x04..=0x1D => return at(&LETTERS, 0x04),
        0x1E..=0x27 => return at(&DIGITS, 0x1E),
        0x28 | 0x58 => Key::Enter,
        0x29 => Key::Escape,
        0x2A => Key::Backspace,
        0x2B => Key::Tab,
        0x2C => Key::Space,
        0x2D | 0x56 => Key::Minus,
        0x2E => Key::Equals,
        0x2F => Key::OpenBracket,
        0x30 => Key::CloseBracket,
        0x31 => Key::Backslash,
        0x33 => Key::Semicolon,
        0x34 => Key::Quote,
        0x35 => Key::Backtick,
        0x36 => Key::Comma,
        0x37 | 0x63 => Key::Period,
        0x38 | 0x54 => Key::Slash,
        0x3A..=0x45 => return at(&F1_12, 0x3A),
        0x49 => Key::Insert,
        0x4A => Key::Home,
        0x4B => Key::PageUp,
        0x4C => Key::Delete,
        0x4D => Key::End,
        0x4E => Key::PageDown,
        0x4F => Key::ArrowRight,
        0x50 => Key::ArrowLeft,
        0x51 => Key::ArrowDown,
        0x52 => Key::ArrowUp,
        0x57 => Key::Plus,
        0x59..=0x62 => return at(&KEYPAD_DIGITS, 0x59),
        0x68..=0x6F => return at(&F13_20, 0x68),
        _ => return None,
    })
}

/// Whether the key event should also produce text: printable characters typed without ⌘ or ⌃
/// (those are shortcuts), as egui-winit decides.
pub fn types_text(flags: u32, text: &str) -> bool {
    flags & (MOD_COMMAND | MOD_CONTROL) == 0 && !text.is_empty() && text.chars().all(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letters_digits_and_navigation() {
        assert_eq!(key(0x04), Some(Key::A));
        assert_eq!(key(0x1D), Some(Key::Z));
        assert_eq!(key(0x1E), Some(Key::Num1));
        assert_eq!(key(0x27), Some(Key::Num0));
        assert_eq!(key(0x2C), Some(Key::Space));
        assert_eq!(key(0x52), Some(Key::ArrowUp));
        assert_eq!(key(0x45), Some(Key::F12));
        assert_eq!(key(0xE0), None);
    }

    #[test]
    fn command_is_egui_command() {
        let m = modifiers(MOD_COMMAND | MOD_SHIFT);
        assert!(m.command && m.mac_cmd && m.shift && !m.ctrl && !m.alt);
    }

    #[test]
    fn shortcuts_type_no_text() {
        assert!(types_text(0, "a"));
        assert!(!types_text(MOD_COMMAND, "a"));
        assert!(!types_text(0, "\u{8}"));
    }
}
