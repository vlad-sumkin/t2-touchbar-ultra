// SPDX-License-Identifier: GPL-3.0-or-later

//! Configuration loader, validator, and serializer for T2 Touchbar Ultra.

use std::{
    env, fs,
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::mechpad::SwitchProfile;

pub const DEFAULT_MIN_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_MAX_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub minimum_timeout_ms: u64,
    pub maximum_timeout_ms: u64,
    pub continuation_window_ms: u64,
    pub fn_long_press_ms: u64,
    pub active_brightness: u32,
    pub haptic_feedback: bool,
    pub haptic_touch_up: bool,
    pub haptic_release_gap_ms: u64,
    pub haptic_mute_with_audio: bool,
    pub haptic_keyboard: bool,
    pub haptic_keyboard_profile: String,
    pub haptic_keyboard_intensity: f32,
    pub haptic_keyboard_key_up: bool,
    pub haptic_keyboard_acoustics: bool,
    pub key_color: String,
    pub two_finger_window_ms: u64,
    pub idle_gesture_window_ms: u64,
    pub two_finger_brightness: bool,
    pub volume_swipe_step_px: u32,
    pub brightness_swipe_step_px: u32,
    pub mode_swipe_px: u32,
    pub follow_keyboard_backlight: bool,
    pub minimum_brightness: u32,
    pub idle_status_bar: bool,
    pub activity_backlight: bool,
    pub activity_timeout_ms: u64,
    pub show_track_changes: bool,
    pub predictive_typing: bool,
    pub clipboard_history: bool,
    pub clipboard_timeout_sec: u64,
    pub word_lookup: bool,
    pub word_lookup_delay_ms: u64,
    pub word_lookup_timeout_sec: u64,
    #[serde(alias = "font_name", alias = "font_path")]
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub status_font_size: Option<f32>,
    pub physical_escape: Option<bool>,
    pub pixel_shift: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            minimum_timeout_ms: DEFAULT_MIN_TIMEOUT_MS,
            maximum_timeout_ms: DEFAULT_MAX_TIMEOUT_MS,
            continuation_window_ms: 15_000,
            fn_long_press_ms: 600,
            active_brightness: 128,
            haptic_feedback: true,
            haptic_touch_up: true,
            haptic_release_gap_ms: 140,
            haptic_mute_with_audio: true,
            haptic_keyboard: true,
            haptic_keyboard_profile: "cherry-mx-blue".into(),
            haptic_keyboard_intensity: 0.85,
            haptic_keyboard_key_up: true,
            haptic_keyboard_acoustics: true,
            key_color: "#dce6ff".into(),
            two_finger_window_ms: 100,
            idle_gesture_window_ms: 2000,
            two_finger_brightness: true,
            volume_swipe_step_px: 10,
            brightness_swipe_step_px: 10,
            mode_swipe_px: 1000,
            follow_keyboard_backlight: true,
            minimum_brightness: 64,
            idle_status_bar: true,
            activity_backlight: true,
            activity_timeout_ms: 30_000,
            show_track_changes: true,
            predictive_typing: true,
            clipboard_history: true,
            clipboard_timeout_sec: 30,
            word_lookup: true,
            word_lookup_delay_ms: 3000,
            word_lookup_timeout_sec: 60,
            font_family: None,
            font_size: Some(32.0),
            status_font_size: Some(36.0),
            physical_escape: Some(true),
            pixel_shift: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let mut config = match fs::read_to_string(&path) {
            Ok(body) => toml::from_str(&body)
                .with_context(|| format!("invalid configuration {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.minimum_timeout_ms >= DEFAULT_MIN_TIMEOUT_MS,
            "minimum_timeout_ms must be at least {DEFAULT_MIN_TIMEOUT_MS}"
        );
        anyhow::ensure!(
            self.maximum_timeout_ms >= self.minimum_timeout_ms,
            "maximum_timeout_ms must not be below minimum_timeout_ms"
        );
        anyhow::ensure!(
            self.fn_long_press_ms >= 300,
            "fn_long_press_ms must be at least 300"
        );
        anyhow::ensure!(
            self.active_brightness <= 255,
            "active_brightness must be 0..255"
        );
        anyhow::ensure!(
            (40..=1000).contains(&self.haptic_release_gap_ms),
            "haptic_release_gap_ms must be 40..1000"
        );
        anyhow::ensure!(
            self.haptic_keyboard_intensity.is_finite()
                && (0.0..=1.0).contains(&self.haptic_keyboard_intensity),
            "haptic_keyboard_intensity must be 0.0..=1.0"
        );
        let parsed_profile = SwitchProfile::parse(&self.haptic_keyboard_profile).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown haptic_keyboard_profile {:?}; expected one of: {}",
                self.haptic_keyboard_profile,
                SwitchProfile::ALL_IDS.join(", ")
            )
        })?;
        self.haptic_keyboard_profile = parsed_profile.id().to_owned();
        anyhow::ensure!(
            (30..=300).contains(&self.two_finger_window_ms),
            "two_finger_window_ms must be 30..300"
        );
        anyhow::ensure!(
            (100..=5000).contains(&self.idle_gesture_window_ms),
            "idle_gesture_window_ms must be 100..5000"
        );
        anyhow::ensure!(
            self.volume_swipe_step_px >= 10
                && self.brightness_swipe_step_px >= 10
                && self.mode_swipe_px >= 40,
            "swipe distances are too short"
        );
        anyhow::ensure!(
            (5_000..=300_000).contains(&self.activity_timeout_ms),
            "activity_timeout_ms must be 5000..300000"
        );
        anyhow::ensure!(
            self.minimum_brightness <= 100,
            "minimum_brightness must be 0..100"
        );
        anyhow::ensure!(
            (5..=300).contains(&self.clipboard_timeout_sec),
            "clipboard_timeout_sec must be 5..300"
        );
        anyhow::ensure!(
            (300..=15_000).contains(&self.word_lookup_delay_ms),
            "word_lookup_delay_ms must be 300..15000"
        );
        anyhow::ensure!(
            (5..=300).contains(&self.word_lookup_timeout_sec),
            "word_lookup_timeout_sec must be 5..300"
        );
        if let Some(ref f) = self.font_family
            && f.trim().is_empty()
        {
            self.font_family = None;
        }
        parse_color(&self.key_color)
            .with_context(|| format!("key_color {:?} must be #rrggbb", self.key_color))?;
        self.continuation_window_ms = self.continuation_window_ms.max(1_000);
        Ok(())
    }

    pub fn key_color(&self) -> u32 {
        parse_color(&self.key_color).unwrap_or(0x00dce6ff)
    }

    pub fn to_commented_toml(&self) -> String {
        let mut out = String::from("# T2 Touchbar Ultra configuration\n\n");
        out.push_str("# Illumination & timeout learning bounds (ms)\n");
        out.push_str(&format!("minimum_timeout_ms = {}\n", self.minimum_timeout_ms));
        out.push_str(&format!("maximum_timeout_ms = {}\n", self.maximum_timeout_ms));
        out.push_str(&format!("continuation_window_ms = {}\n", self.continuation_window_ms));
        out.push_str(&format!("fn_long_press_ms = {}\n\n", self.fn_long_press_ms));

        out.push_str("# Panel brightness, status bar & anti-burn-in\n");
        out.push_str(&format!("active_brightness = {}\n", self.active_brightness));
        out.push_str(&format!("idle_status_bar = {}\n", self.idle_status_bar));
        out.push_str(&format!("pixel_shift = {}\n", self.pixel_shift));
        if let Some(esc) = self.physical_escape {
            out.push_str(&format!("physical_escape = {esc}\n"));
        } else {
            out.push_str("# physical_escape = true\n");
        }
        out.push('\n');

        out.push_str("# Touch Bar & mechanical keyboard haptics (MechPad profiles)\n");
        out.push_str(&format!("haptic_feedback = {}\n", self.haptic_feedback));
        out.push_str(&format!("haptic_touch_up = {}\n", self.haptic_touch_up));
        out.push_str(&format!("haptic_release_gap_ms = {}\n", self.haptic_release_gap_ms));
        out.push_str(&format!("haptic_mute_with_audio = {}\n", self.haptic_mute_with_audio));
        out.push_str(&format!("haptic_keyboard = {}\n", self.haptic_keyboard));
        out.push_str(&format!(
            "haptic_keyboard_profile = {:?}\n",
            self.haptic_keyboard_profile
        ));
        out.push_str(&format!(
            "haptic_keyboard_intensity = {:.2}\n",
            self.haptic_keyboard_intensity
        ));
        out.push_str(&format!("haptic_keyboard_key_up = {}\n", self.haptic_keyboard_key_up));
        out.push_str(&format!("haptic_keyboard_acoustics = {}\n\n", self.haptic_keyboard_acoustics));

        out.push_str("# Appearance & typography\n");
        out.push_str(&format!("key_color = {:?}\n", self.key_color));
        if let Some(ref family) = self.font_family {
            out.push_str(&format!("font_family = {:?}\n", family));
        } else {
            out.push_str("# font_family = \"Adwaita Sans\"\n");
        }
        if let Some(sz) = self.font_size {
            out.push_str(&format!("font_size = {:.1}\n", sz));
        }
        if let Some(sz) = self.status_font_size {
            out.push_str(&format!("status_font_size = {:.1}\n", sz));
        }
        out.push('\n');

        out.push_str("# Multi-finger gestures\n");
        out.push_str(&format!("two_finger_window_ms = {}\n", self.two_finger_window_ms));
        out.push_str(&format!("idle_gesture_window_ms = {}\n", self.idle_gesture_window_ms));
        out.push_str(&format!("two_finger_brightness = {}\n", self.two_finger_brightness));
        out.push_str(&format!("volume_swipe_step_px = {}\n", self.volume_swipe_step_px));
        out.push_str(&format!("brightness_swipe_step_px = {}\n", self.brightness_swipe_step_px));
        out.push_str(&format!("mode_swipe_px = {}\n\n", self.mode_swipe_px));

        out.push_str("# Keyboard backlight synchronization\n");
        out.push_str(&format!("follow_keyboard_backlight = {}\n", self.follow_keyboard_backlight));
        out.push_str(&format!("minimum_brightness = {}\n", self.minimum_brightness));
        out.push_str(&format!("activity_backlight = {}\n", self.activity_backlight));
        out.push_str(&format!("activity_timeout_ms = {}\n\n", self.activity_timeout_ms));

        out.push_str("# Smart features: MPRIS track toast, predictive typing, clipboard, dictionary\n");
        out.push_str(&format!("show_track_changes = {}\n", self.show_track_changes));
        out.push_str(&format!("predictive_typing = {}\n", self.predictive_typing));
        out.push_str(&format!("clipboard_history = {}\n", self.clipboard_history));
        out.push_str(&format!("clipboard_timeout_sec = {}\n", self.clipboard_timeout_sec));
        out.push_str(&format!("word_lookup = {}\n", self.word_lookup));
        out.push_str(&format!("word_lookup_delay_ms = {}\n", self.word_lookup_delay_ms));
        out.push_str(&format!("word_lookup_timeout_sec = {}\n", self.word_lookup_timeout_sec));
        out
    }

    pub fn save(&mut self) -> Result<PathBuf> {
        self.validate()?;
        let path = user_config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let body = self.to_commented_toml();
        fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    }
}

fn parse_color(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

pub fn user_config_path() -> PathBuf {
    if let Some(path) = env::var_os("T2_TOUCHBAR_ULTRA_CONFIG") {
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
        .join("t2-touchbar-ultra/config.toml")
}

pub fn config_path() -> PathBuf {
    if let Some(path) = env::var_os("T2_TOUCHBAR_ULTRA_CONFIG") {
        return path.into();
    }
    let user_path = user_config_path();
    if user_path.exists() {
        user_path
    } else {
        PathBuf::from("/etc/t2-touchbar-ultra/touchbar.toml")
    }
}

pub fn state_path() -> PathBuf {
    if let Some(path) = env::var_os("T2_TOUCHBAR_ULTRA_STATE") {
        return path.into();
    }
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state")
        })
        .join("t2-touchbar-ultra/state.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_haptic_config_defaults_and_validation() {
        let mut cfg = Config::default();
        assert!(cfg.haptic_keyboard);
        assert!(cfg.haptic_mute_with_audio);
        assert_eq!(cfg.haptic_keyboard_profile, "cherry-mx-blue");
        assert!((cfg.haptic_keyboard_intensity - 0.85).abs() < 1e-6);
        assert!(cfg.haptic_keyboard_key_up);
        assert!(cfg.haptic_keyboard_acoustics);
        cfg.validate().expect("default config should validate");

        cfg.haptic_keyboard_profile = "Holy Panda".into();
        cfg.validate().expect("display profile name should normalize");
        assert_eq!(cfg.haptic_keyboard_profile, "holy-panda");

        cfg.haptic_keyboard_profile = "nonexistent-switch".into();
        assert!(cfg.validate().is_err());

        cfg.haptic_keyboard_profile = "topre-45g".into();
        cfg.haptic_keyboard_intensity = 1.5;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn commented_toml_roundtrips_cleanly() {
        let mut orig = Config::default();
        orig.haptic_keyboard_profile = "ibm-model-m".into();
        orig.haptic_mute_with_audio = true;
        orig.font_family = Some("Inter".into());
        let text = orig.to_commented_toml();
        let mut parsed: Config = toml::from_str(&text).unwrap();
        parsed.validate().unwrap();
        assert_eq!(orig, parsed);
    }

    #[test]
    fn bundled_touchbar_toml_parses_and_validates() {
        let raw = include_str!("../config/touchbar.toml");
        let mut cfg: Config = toml::from_str(raw).expect("bundled touchbar.toml must parse");
        cfg.validate().expect("bundled touchbar.toml must validate");
    }
}
