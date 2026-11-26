// SPDX-License-Identifier: GPL-3.0-or-later

use std::{cell::{Cell, RefCell}, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub fn detect_default() -> Self {
        let state_file = lang_pref_path();
        if let Ok(saved) = std::fs::read_to_string(&state_file) {
            match saved.trim() {
                "ru" => return Self::Ru,
                "en" => return Self::En,
                _ => {}
            }
        }
        let lc = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if lc.starts_with("ru") { Self::Ru } else { Self::En }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Ru => Self::En,
            Self::En => Self::Ru,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }

    pub fn save_pref(self) {
        let path = lang_pref_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.code());
    }
}

fn lang_pref_path() -> std::path::PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".local/state")
        })
        .join("t2-touchbar-ultra/settings-lang")
}

pub fn tr(lang: Lang, ru: &'static str, en: &'static str) -> &'static str {
    match lang {
        Lang::Ru => ru,
        Lang::En => en,
    }
}

#[derive(Clone)]
pub struct I18n {
    lang: Rc<Cell<Lang>>,
    updaters: Rc<RefCell<Vec<Box<dyn Fn(Lang)>>>>,
}

impl I18n {
    pub fn new(initial: Lang) -> Self {
        Self {
            lang: Rc::new(Cell::new(initial)),
            updaters: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn lang(&self) -> Lang {
        self.lang.get()
    }

    pub fn toggle(&self) -> Lang {
        let next = self.lang.get().toggle();
        self.lang.set(next);
        next.save_pref();
        let list = self.updaters.borrow();
        for f in list.iter() {
            f(next);
        }
        next
    }

    pub fn bind(&self, f: impl Fn(Lang) + 'static) {
        f(self.lang.get());
        self.updaters.borrow_mut().push(Box::new(f));
    }
}
