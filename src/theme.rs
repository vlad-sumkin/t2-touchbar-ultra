// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tiny_skia::Paint;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const BLACK: Rgb = Rgb(0, 0, 0);

impl Rgb {
    pub const fn from_u32(color: u32) -> Self {
        Self((color >> 16) as u8, (color >> 8) as u8, color as u8)
    }

    pub fn to_u32(self) -> u32 {
        ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | (self.2 as u32)
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    pub fn scale(self, percent: u32) -> Self {
        let channel = |value: u8| (u32::from(value) * percent / 100).min(255) as u8;
        Self(channel(self.0), channel(self.1), channel(self.2))
    }

    pub fn paint(self) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color_rgba8(self.0, self.1, self.2, 255);
        paint.anti_alias = true;
        paint
    }
}

pub fn parse_color(value: &str) -> Option<Rgb> {
    let s = value.trim().strip_prefix('#')?;
    match s.len() {
        3 => {
            let r = u8::from_str_radix(&s[0..1], 16).ok()?;
            let g = u8::from_str_radix(&s[1..2], 16).ok()?;
            let b = u8::from_str_radix(&s[2..3], 16).ok()?;
            Some(Rgb(r * 17, g * 17, b * 17))
        }
        6 | 8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Rgb(r, g, b))
        }
        _ => None,
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_color(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid hex color: {s}")))
    }
}

impl Serialize for Rgb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Theme {
    pub keys: KeysTheme,
    pub status: StatusTheme,
    pub levels: LevelsTheme,
    pub track_popup: TrackPopupTheme,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct KeysTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esc_color: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_color: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion_match_color: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct StatusTheme {
    pub clock: ClockTheme,
    pub center: CenterTheme,
    pub media_bar: MediaBarTheme,
    pub battery: BatteryTheme,
    pub sysmon: SysmonTheme,
    pub hardware: HardwareTheme,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ClockTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct CenterTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_title: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marquee_separator: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct MediaBarTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capsule_bg: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_fill: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needle: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BatteryTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugged: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charging: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SysmonTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HardwareTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_muted: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyboard_layout: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct LevelsTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_meter: Option<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_text: Option<Rgb>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TrackPopupTheme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgb>,
}

impl Theme {
    pub fn load() -> Self {
        let path = theme_path();
        match fs::read_to_string(&path) {
            Ok(body) => match toml::from_str(&body) {
                Ok(theme) => theme,
                Err(err) => {
                    eprintln!("t2-touchbar-ultra: invalid theme {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                eprintln!("t2-touchbar-ultra: read {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn load_from_str(s: &str) -> Result<Self> {
        toml::from_str(s).context("parse theme toml")
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = user_theme_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(self).context("serialize theme")?;
        let header = "# T2 Touchbar Ultra color theme (live reload supported)\n\n";
        fs::write(&path, format!("{header}{body}")).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}

pub fn user_theme_path() -> PathBuf {
    if let Some(path) = env::var_os("T2_TOUCHBAR_ULTRA_THEME") {
        return path.into();
    }
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("t2-touchbar-ultra/theme.toml")
}

pub fn theme_path() -> PathBuf {
    if let Some(path) = env::var_os("T2_TOUCHBAR_ULTRA_THEME") {
        return path.into();
    }
    let user_path = user_theme_path();
    if user_path.exists() {
        user_path
    } else {
        PathBuf::from("/etc/t2-touchbar-ultra/theme.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_color() {
        assert_eq!(parse_color("#fff"), Some(Rgb(255, 255, 255)));
        assert_eq!(parse_color("#000"), Some(Rgb(0, 0, 0)));
        assert_eq!(parse_color("#ff8c00"), Some(Rgb(255, 140, 0)));
        assert_eq!(parse_color("#dce6ff"), Some(Rgb(0xdc, 0xe6, 0xff)));
        assert_eq!(parse_color("#dce6ff80"), Some(Rgb(0xdc, 0xe6, 0xff)));
        assert_eq!(parse_color("invalid"), None);
    }

    #[test]
    fn test_theme_toml_roundtrip() {
        let toml_str = r##"
[keys]
color = "#112233"
esc_color = "#445566"
suggestion_match_color = "#ffd60a"

[status.clock]
time = "#aabbcc"
date = "#ddeeff"

[status.center]
window_title = "#ff8c00"
workspace = "#00ffcc"
media_title = "#ff007f"
marquee_separator = "#555555"

[status.media_bar]
capsule_bg = "#202020"
progress_fill = "#00ff88"
needle = "#ffffff"

[status.battery]
plugged = "#009fff"
"##;
        let theme = Theme::load_from_str(toml_str).unwrap();
        assert_eq!(theme.keys.color, Some(Rgb(0x11, 0x22, 0x33)));
        assert_eq!(theme.status.battery.plugged, Some(Rgb(0x00, 0x9f, 0xff)));
        let serialized = toml::to_string_pretty(&theme).unwrap();
        let reparsed = Theme::load_from_str(&serialized).unwrap();
        assert_eq!(theme, reparsed);
    }
}
