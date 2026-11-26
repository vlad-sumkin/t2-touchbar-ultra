// SPDX-License-Identifier: GPL-3.0-or-later

use std::{ffi::c_char, fs::OpenOptions};

use anyhow::{Context, Result};
use input_linux::{EventKind, Key, SynchronizeKind, uinput::UInputHandle};
use input_linux_sys::{input_event, input_id, timeval, uinput_setup};

const ALLOWED_KEYS: &[Key] = &[
    Key::Esc,
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::BrightnessDown,
    Key::BrightnessUp,
    Key::MicMute,
    Key::IllumDown,
    Key::IllumUp,
    Key::PreviousSong,
    Key::PlayPause,
    Key::NextSong,
    Key::Sysrq,
    Key::Insert,
    Key::Delete,
    Key::Home,
    Key::End,
    Key::PageUp,
    Key::PageDown,
    Key::Rewind,
    Key::FastForward,
    Key::Mute,
    Key::VolumeDown,
    Key::VolumeUp,
    // Typing and editing keys for predictive text insertion
    Key::Backspace,
    Key::Space,
    Key::Enter,
    Key::Tab,
    Key::LeftShift,
    Key::RightShift,
    Key::LeftCtrl,
    Key::RightCtrl,
    Key::LeftAlt,
    Key::RightAlt,
    Key::A, Key::B, Key::C, Key::D, Key::E, Key::F, Key::G, Key::H, Key::I, Key::J,
    Key::K, Key::L, Key::M, Key::N, Key::O, Key::P, Key::Q, Key::R, Key::S, Key::T,
    Key::U, Key::V, Key::W, Key::X, Key::Y, Key::Z,
    Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4,
    Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9,
    Key::Minus, Key::Equal, Key::LeftBrace, Key::RightBrace,
    Key::Semicolon, Key::Apostrophe, Key::Grave, Key::Backslash,
    Key::Comma, Key::Dot, Key::Slash,
];

pub struct VirtualKeyboard {
    handle: UInputHandle<std::fs::File>,
    pressed: Option<Key>,
}

impl VirtualKeyboard {
    /// `extra` adds the keys of the personal keys file to the fixed set.
    pub fn open(extra: &[Key]) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("open /dev/uinput")?;
        let handle = UInputHandle::new(file);
        handle.set_evbit(EventKind::Key)?;
        for key in ALLOWED_KEYS.iter().chain(extra) {
            handle.set_keybit(*key)?;
        }
        let mut name = [0 as c_char; 80];
        for (target, byte) in name.iter_mut().zip(b"T2 Touch Bar".iter().copied()) {
            *target = byte as c_char;
        }
        handle.dev_setup(&uinput_setup {
            id: input_id {
                bustype: 0x19,
                vendor: 0x1209,
                product: 0x4b32,
                version: 1,
            },
            ff_effects_max: 0,
            name,
        })?;
        handle.dev_create()?;
        Ok(Self {
            handle,
            pressed: None,
        })
    }

    pub fn press(&mut self, key: Key) -> Result<()> {
        self.release()?;
        emit(&mut self.handle, key, 1)?;
        self.pressed = Some(key);
        Ok(())
    }

    /// Taps a combination: modifiers and key down in order, up in reverse.
    pub fn tap_combination(&mut self, keys: &[Key]) -> Result<()> {
        self.release()?;
        for key in keys {
            emit(&mut self.handle, *key, 1)?;
        }
        for key in keys.iter().rev() {
            emit(&mut self.handle, *key, 0)?;
        }
        Ok(())
    }

    pub fn release(&mut self) -> Result<()> {
        if let Some(key) = self.pressed.take() {
            emit(&mut self.handle, key, 0)?;
        }
        Ok(())
    }

    pub fn tap_key(&mut self, key: Key) -> Result<()> {
        emit(&mut self.handle, key, 1)?;
        emit(&mut self.handle, key, 0)?;
        Ok(())
    }

    pub fn tap_shift_key(&mut self, key: Key) -> Result<()> {
        emit(&mut self.handle, Key::LeftShift, 1)?;
        emit(&mut self.handle, key, 1)?;
        emit(&mut self.handle, key, 0)?;
        emit(&mut self.handle, Key::LeftShift, 0)?;
        Ok(())
    }

    pub fn tap_backspaces(&mut self, count: usize) -> Result<()> {
        for _ in 0..count {
            emit(&mut self.handle, Key::Backspace, 1)?;
            emit(&mut self.handle, Key::Backspace, 0)?;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(())
    }

    pub fn tap_deletes(&mut self, count: usize) -> Result<()> {
        for _ in 0..count {
            emit(&mut self.handle, Key::Delete, 1)?;
            emit(&mut self.handle, Key::Delete, 0)?;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(())
    }

    pub fn type_char(&mut self, ch: char, layout: &str) -> Result<()> {
        if ch == ' ' {
            return self.tap_key(Key::Space);
        }
        if let Some((key, shift)) = char_to_key(ch, layout) {
            if shift {
                self.tap_shift_key(key)?;
            } else {
                self.tap_key(key)?;
            }
        }
        Ok(())
    }

    pub fn type_text(&mut self, text: &str, layout: &str) -> Result<()> {
        for ch in text.chars() {
            self.type_char(ch, layout)?;
        }
        Ok(())
    }

    pub fn paste_clipboard(&mut self) -> Result<()> {
        self.tap_combination(&[Key::LeftShift, Key::Insert])
    }

    pub fn paste_ctrl_v(&mut self) -> Result<()> {
        self.tap_combination(&[Key::LeftCtrl, Key::V])
    }
}

pub fn char_to_key(ch: char, layout: &str) -> Option<(Key, bool)> {
    let is_ru = layout.trim().eq_ignore_ascii_case("RU");
    let is_upper = ch.is_uppercase();
    let lower = ch.to_lowercase().next().unwrap_or(ch);

    if is_ru {
        match ch {
            '.' => return Some((Key::Num7, true)),
            ',' => return Some((Key::Num6, true)),
            ':' => return Some((Key::Num5, true)),
            ';' => return Some((Key::Num8, true)),
            '(' => return Some((Key::Num9, true)),
            ')' => return Some((Key::Num0, true)),
            '!' => return Some((Key::Num1, true)),
            '?' => return Some((Key::Slash, true)),
            '"' => return Some((Key::Num2, true)),
            '%' => return Some((Key::Num4, true)),
            '*' => return Some((Key::Num8, true)),
            '_' => return Some((Key::Minus, true)),
            '-' => return Some((Key::Minus, false)),
            '+' => return Some((Key::Equal, true)),
            '=' => return Some((Key::Equal, false)),
            '/' => return Some((Key::Slash, false)),
            '\\' => return Some((Key::Backslash, false)),
            'ё' => return Some((Key::Backslash, false)),
            'Ё' => return Some((Key::Backslash, true)),
            '<' => return Some((Key::Grave, false)),
            '>' => return Some((Key::Grave, true)),
            _ => {}
        }

        let key = match lower {
            'й' => Key::Q,
            'ц' => Key::W,
            'у' => Key::E,
            'к' => Key::R,
            'е' => Key::T,
            'н' => Key::Y,
            'г' => Key::U,
            'ш' => Key::I,
            'щ' => Key::O,
            'з' => Key::P,
            'х' => Key::LeftBrace,
            'ъ' => Key::RightBrace,
            'ф' => Key::A,
            'ы' => Key::S,
            'в' => Key::D,
            'а' => Key::F,
            'п' => Key::G,
            'р' => Key::H,
            'о' => Key::J,
            'л' => Key::K,
            'д' => Key::L,
            'ж' => Key::Semicolon,
            'э' => Key::Apostrophe,
            'я' => Key::Z,
            'ч' => Key::X,
            'с' => Key::C,
            'м' => Key::V,
            'и' => Key::B,
            'т' => Key::N,
            'ь' => Key::M,
            'б' => Key::Comma,
            'ю' => Key::Dot,
            'ё' => Key::Backslash,
            '-' => Key::Minus,
            ' ' => Key::Space,
            '0' => Key::Num0,
            '1' => Key::Num1,
            '2' => Key::Num2,
            '3' => Key::Num3,
            '4' => Key::Num4,
            '5' => Key::Num5,
            '6' => Key::Num6,
            '7' => Key::Num7,
            '8' => Key::Num8,
            '9' => Key::Num9,
            _ => return char_to_key_en(lower, is_upper),
        };
        return Some((key, is_upper));
    }

    char_to_key_en(lower, is_upper)
}

fn char_to_key_en(lower: char, is_upper: bool) -> Option<(Key, bool)> {
    match lower {
        '.' => return Some((Key::Dot, false)),
        ',' => return Some((Key::Comma, false)),
        ':' => return Some((Key::Semicolon, true)),
        ';' => return Some((Key::Semicolon, false)),
        '(' => return Some((Key::Num9, true)),
        ')' => return Some((Key::Num0, true)),
        '!' => return Some((Key::Num1, true)),
        '?' => return Some((Key::Slash, true)),
        '"' => return Some((Key::Apostrophe, true)),
        '\'' => return Some((Key::Apostrophe, false)),
        '%' => return Some((Key::Num5, true)),
        '*' => return Some((Key::Num8, true)),
        '_' => return Some((Key::Minus, true)),
        '-' => return Some((Key::Minus, false)),
        '+' => return Some((Key::Equal, true)),
        '=' => return Some((Key::Equal, false)),
        '/' => return Some((Key::Slash, false)),
        '\\' => return Some((Key::Backslash, false)),
        '<' => return Some((Key::Comma, true)),
        '>' => return Some((Key::Dot, true)),
        '[' => return Some((Key::LeftBrace, false)),
        ']' => return Some((Key::RightBrace, false)),
        '{' => return Some((Key::LeftBrace, true)),
        '}' => return Some((Key::RightBrace, true)),
        '`' => return Some((Key::Grave, false)),
        '~' => return Some((Key::Grave, true)),
        '@' => return Some((Key::Num2, true)),
        '#' => return Some((Key::Num3, true)),
        '$' => return Some((Key::Num4, true)),
        '^' => return Some((Key::Num6, true)),
        '&' => return Some((Key::Num7, true)),
        '|' => return Some((Key::Backslash, true)),
        _ => {}
    }

    let key = match lower {
        'a' => Key::A,
        'b' => Key::B,
        'c' => Key::C,
        'd' => Key::D,
        'e' => Key::E,
        'f' => Key::F,
        'g' => Key::G,
        'h' => Key::H,
        'i' => Key::I,
        'j' => Key::J,
        'k' => Key::K,
        'l' => Key::L,
        'm' => Key::M,
        'n' => Key::N,
        'o' => Key::O,
        'p' => Key::P,
        'q' => Key::Q,
        'r' => Key::R,
        's' => Key::S,
        't' => Key::T,
        'u' => Key::U,
        'v' => Key::V,
        'w' => Key::W,
        'x' => Key::X,
        'y' => Key::Y,
        'z' => Key::Z,
        '0' => Key::Num0,
        '1' => Key::Num1,
        '2' => Key::Num2,
        '3' => Key::Num3,
        '4' => Key::Num4,
        '5' => Key::Num5,
        '6' => Key::Num6,
        '7' => Key::Num7,
        '8' => Key::Num8,
        '9' => Key::Num9,
        '-' => Key::Minus,
        ' ' => Key::Space,
        '\'' => Key::Apostrophe,
        ',' => Key::Comma,
        '.' => Key::Dot,
        _ => return None,
    };
    Some((key, is_upper))
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn emit(handle: &mut UInputHandle<std::fs::File>, key: Key, value: i32) -> Result<()> {
    let event = |kind: EventKind, code: u16, value| input_event {
        value,
        type_: kind as u16,
        code,
        time: timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
    };
    handle.write(&[
        event(EventKind::Key, key as u16, value),
        event(EventKind::Synchronize, SynchronizeKind::Report as u16, 0),
    ])?;
    Ok(())
}
