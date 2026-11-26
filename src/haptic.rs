// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::mechpad::{
    KeyKind, SwitchProfile, WaveformReport, encode_report_53, should_click_evdev,
};

const PLAY: &str = "/sys/module/t2_trackpad_actuator/parameters/play";
const ACTUATOR_DRIVER: &str = "t2_trackpad_actuator";
const MIN_INTERVAL: Duration = Duration::from_millis(40);
const STALE_DROP: Duration = Duration::from_millis(60);
const HIDRAW_SLOW_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const HIDRAW_FAST_PROBE_INTERVAL: Duration = Duration::from_millis(350);
const RESUME_RETRY_WINDOW: Duration = Duration::from_secs(30);

/// Raw captured DTrace click waveforms, prefixed with Report ID `0x53`.
const REPORT_CLICK_LIGHT: &[u8] = &[
    0x53, 0x01, 0x1c, 0x8f, 0x01, 0x03, 0x00, 0x53, 0x37, 0x02, 0x02, 0x00, 0x18, 0x4d,
];
const REPORT_CLICK_FIRM: &[u8] = &[
    0x53, 0x01, 0x2d, 0x8f, 0x01, 0x05, 0x00, 0x53, 0x37, 0x02, 0x02, 0x00, 0x30, 0x4d,
];

#[derive(Clone, Copy, Debug)]
pub struct KeyboardHapticConfig {
    pub enabled: bool,
    pub profile: SwitchProfile,
    pub intensity: f32,
    pub key_up: bool,
    pub acoustics: bool,
    pub mute_with_audio: bool,
}

impl KeyboardHapticConfig {
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            profile: SwitchProfile::CherryMxBlue,
            intensity: 0.85,
            key_up: true,
            acoustics: true,
            mute_with_audio: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PresetKind {
    Light,
    Firm,
}

enum HapticJob {
    Preset {
        preset: PresetKind,
        due: Instant,
    },
    Keyboard {
        primary: WaveformReport,
        secondary: Option<(WaveformReport, u64)>,
        fallback_firm: bool,
        due: Instant,
    },
    Suspend,
    Resume,
}

/// Plays Touch Bar preset clicks and mechanical keyboard switch waveforms
/// on a dedicated worker thread with automatic sleep/resume reconnection
/// and system mute gating.
pub struct Haptic {
    worker: Option<Sender<HapticJob>>,
    touch_enabled: bool,
    touch_up_enabled: bool,
    release_gap: Duration,
    last: Option<Instant>,
    last_press: Option<Instant>,
    keyboard: KeyboardHapticConfig,
    system_muted: Arc<AtomicBool>,
    rng_state: u64,
}

impl Haptic {
    #[allow(dead_code)]
    pub fn open(enabled: bool, touch_up_enabled: bool, release_gap_ms: u64) -> Self {
        Self::open_with_keyboard(
            enabled,
            touch_up_enabled,
            release_gap_ms,
            KeyboardHapticConfig::disabled(),
        )
    }

    pub fn open_with_keyboard(
        touch_enabled: bool,
        touch_up_enabled: bool,
        release_gap_ms: u64,
        keyboard: KeyboardHapticConfig,
    ) -> Self {
        let want_actuator = touch_enabled || (keyboard.enabled && keyboard.intensity > 0.001);
        let system_muted = Arc::new(AtomicBool::new(false));
        let play_file = want_actuator.then(open_play_actuator).flatten();
        let hidraw_file = want_actuator.then(open_hidraw_actuator).flatten();

        if want_actuator && play_file.is_none() && hidraw_file.is_none() {
            eprintln!("t2-touchbar-ultra: haptic actuator not yet available, will retry");
        }
        let worker = if want_actuator {
            spawn(
                play_file,
                hidraw_file,
                keyboard.enabled,
                keyboard.mute_with_audio,
                Arc::clone(&system_muted),
            )
            .ok()
        } else {
            None
        };
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);

        Self {
            worker,
            touch_enabled,
            touch_up_enabled,
            release_gap: Duration::from_millis(release_gap_ms.clamp(40, 1000)),
            last: None,
            last_press: None,
            keyboard,
            system_muted,
            rng_state: seed ^ 0x517c_c1b7_2722_0a95,
        }
    }

    /// Updates whether the desktop audio output is currently muted (or at 0%).
    pub fn set_system_muted(&self, muted: bool) {
        self.system_muted.store(muted, Ordering::Relaxed);
    }

    pub fn is_silenced_by_mute(&self) -> bool {
        self.keyboard.mute_with_audio && self.system_muted.load(Ordering::Relaxed)
    }

    /// Releases hardware handles cleanly before system suspend.
    pub fn on_suspend(&self) {
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.send(HapticJob::Suspend);
        }
    }

    /// Re-acquires the actuator hidraw device and sysfs preset node after wake.
    pub fn on_resume(&self) {
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.send(HapticJob::Resume);
        }
    }

    /// Auditions a switch profile immediately (used by the settings UI test button).
    pub fn preview_switch(&mut self, profile: SwitchProfile, intensity: f32, key_up: bool, acoustics: bool) {
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let mut rng = self.rng_state;
        let down = profile.synthesize(KeyKind::Alpha, true, acoustics, &mut rng);
        self.rng_state = rng;
        if let Some(primary) = encode_report_53(&down.primary, intensity) {
            let secondary = down.secondary.and_then(|(b, d)| {
                encode_report_53(&b, intensity).map(|r| (r, d))
            });
            let _ = worker.send(HapticJob::Keyboard {
                primary,
                secondary,
                fallback_firm: (down.primary.level * intensity) >= 0.72,
                due: Instant::now(),
            });
        }
        if key_up {
            let up = profile.synthesize(KeyKind::Alpha, false, acoustics, &mut self.rng_state);
            if let Some(primary) = encode_report_53(&up.primary, intensity) {
                let sender = worker.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(85));
                    let _ = sender.send(HapticJob::Keyboard {
                        primary,
                        secondary: None,
                        fallback_firm: false,
                        due: Instant::now(),
                    });
                });
            }
        }
    }

    pub fn click(&mut self) {
        if self.last.is_some_and(|last| last.elapsed() < MIN_INTERVAL) {
            return;
        }
        self.play_preset(PresetKind::Light, Instant::now());
    }

    /// The strongest captured click, for running into a stop. It is never
    /// rate limited, so it also lands right after a light click.
    pub fn firm(&mut self) {
        self.play_preset(PresetKind::Firm, Instant::now());
    }

    /// Immediate strong crisp click for word suggestion insertion.
    pub fn suggestion_tap(&mut self) {
        if self.last.is_some_and(|last| last.elapsed() < MIN_INTERVAL) {
            return;
        }
        self.last_press = None;
        self.play_preset(PresetKind::Firm, Instant::now());
    }

    /// Rate-limited tick for continuous ribbon/slider dragging.
    pub fn slider_tick(&mut self) {
        let min_interval = Duration::from_millis(50);
        if self.last.is_some_and(|last| last.elapsed() < min_interval) {
            return;
        }
        self.play_preset(PresetKind::Light, Instant::now());
    }

    /// A Touch Bar key going down: the light click.
    pub fn key_press(&mut self) {
        self.last_press = Some(Instant::now());
        self.play_preset(PresetKind::Light, Instant::now());
    }

    /// A Touch Bar key coming back up: the firm click, never sooner than
    /// `release_gap` after the press.
    pub fn key_release(&mut self) {
        if !self.touch_up_enabled {
            return;
        }
        let due = self
            .last_press
            .take()
            .map_or_else(Instant::now, |press| press + self.release_gap)
            .max(Instant::now());
        self.play_preset(PresetKind::Firm, due);
    }

    /// Synthesizes and queues a mechanical-switch haptic pulse for a physical keyboard event.
    pub fn keyboard_key(&mut self, key_code: u32, pressed: bool) {
        if !self.keyboard.enabled || self.keyboard.intensity <= 0.001 {
            return;
        }
        if self.is_silenced_by_mute() {
            return;
        }
        if !should_click_evdev(key_code) {
            return;
        }
        if !pressed && !self.keyboard.key_up {
            return;
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };

        let kind = KeyKind::from_evdev(key_code);
        let synth = self.keyboard.profile.synthesize(
            kind,
            pressed,
            self.keyboard.acoustics,
            &mut self.rng_state,
        );
        let Some(primary) = encode_report_53(&synth.primary, self.keyboard.intensity) else {
            return;
        };
        let secondary = synth.secondary.and_then(|(burst, delay_us)| {
            encode_report_53(&burst, self.keyboard.intensity).map(|rep| (rep, delay_us))
        });
        let fallback_firm =
            pressed && (synth.primary.level * self.keyboard.intensity) >= 0.72;

        let _ = worker.send(HapticJob::Keyboard {
            primary,
            secondary,
            fallback_firm,
            due: Instant::now(),
        });
    }

    fn play_preset(&mut self, preset: PresetKind, due: Instant) {
        self.last = Some(due);
        if !self.touch_enabled || self.is_silenced_by_mute() {
            return;
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let _ = worker.send(HapticJob::Preset { preset, due });
    }
}

fn find_actuator_hidraw_path() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/hidraw").ok()?;
    for entry in entries.flatten() {
        let driver_link = entry.path().join("device/driver");
        let Ok(target) = fs::read_link(driver_link) else {
            continue;
        };
        if target.file_name().and_then(|s| s.to_str()) == Some(ACTUATOR_DRIVER) {
            return Some(PathBuf::from("/dev").join(entry.file_name()));
        }
    }
    None
}

pub fn actuator_hidraw_available() -> bool {
    open_hidraw_actuator().is_some()
}

fn open_hidraw_actuator() -> Option<File> {
    let path = find_actuator_hidraw_path()?;
    OpenOptions::new().write(true).open(path).ok()
}

fn open_play_actuator() -> Option<File> {
    OpenOptions::new().write(true).open(Path::new(PLAY)).ok()
}

fn write_play_raw(file: &mut File, preset: PresetKind) -> std::io::Result<()> {
    let bytes: &'static [u8] = match preset {
        PresetKind::Light => b"light\n",
        PresetKind::Firm => b"firm\n",
    };
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)
}

fn send_play_with_reopen(play_file: &mut Option<File>, preset: PresetKind) -> bool {
    if let Some(file) = play_file.as_mut()
        && write_play_raw(file, preset).is_ok()
    {
        return true;
    }
    *play_file = open_play_actuator();
    if let Some(file) = play_file.as_mut()
        && write_play_raw(file, preset).is_ok()
    {
        return true;
    }
    *play_file = None;
    false
}

fn write_hidraw_raw(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    match file.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(libc::EPIPE) => file.write_all(bytes),
        Err(err) => Err(err),
    }
}

fn send_hidraw_with_reopen(
    hidraw_file: &mut Option<File>,
    bytes: &[u8],
    fast_retry_until: &mut Option<Instant>,
) -> bool {
    if let Some(file) = hidraw_file.as_mut() {
        match write_hidraw_raw(file, bytes) {
            Ok(()) => {
                *fast_retry_until = None;
                return true;
            }
            Err(_) => {
                // Device node was likely replaced/re-bound (e.g. after suspend/resume).
                *hidraw_file = None;
            }
        }
    }
    *hidraw_file = open_hidraw_actuator();
    if let Some(file) = hidraw_file.as_mut()
        && write_hidraw_raw(file, bytes).is_ok()
    {
        *fast_retry_until = None;
        return true;
    }
    *hidraw_file = None;
    *fast_retry_until = Some(Instant::now() + RESUME_RETRY_WINDOW);
    false
}

fn spawn(
    mut play_file: Option<File>,
    mut hidraw_file: Option<File>,
    probe_hidraw: bool,
    mute_with_audio: bool,
    system_muted: Arc<AtomicBool>,
) -> std::io::Result<Sender<HapticJob>> {
    let (sender, jobs) = mpsc::channel::<HapticJob>();
    thread::Builder::new()
        .name("haptic".to_owned())
        .spawn(move || {
            let mut pending_stage2: Option<(Instant, WaveformReport)> = None;
            let mut last_hidraw_probe = Instant::now();
            let mut fast_retry_until: Option<Instant> =
                if probe_hidraw && hidraw_file.is_none() {
                    Some(Instant::now() + RESUME_RETRY_WINDOW)
                } else {
                    None
                };

            let is_muted = || mute_with_audio && system_muted.load(Ordering::Relaxed);

            loop {
                let job = if let Some((deadline, _)) = pending_stage2.as_ref() {
                    let now = Instant::now();
                    if *deadline <= now {
                        let (_, report) = pending_stage2.take().expect("checked Some");
                        if !is_muted() {
                            let _ = send_hidraw_with_reopen(
                                &mut hidraw_file,
                                report.as_bytes(),
                                &mut fast_retry_until,
                            );
                        }
                        continue;
                    }
                    match jobs.recv_timeout(*deadline - now) {
                        Ok(job) => job,
                        Err(RecvTimeoutError::Timeout) => {
                            if let Some((_, report)) = pending_stage2.take()
                                && !is_muted()
                            {
                                let _ = send_hidraw_with_reopen(
                                    &mut hidraw_file,
                                    report.as_bytes(),
                                    &mut fast_retry_until,
                                );
                            }
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                } else if probe_hidraw
                    && hidraw_file.is_none()
                    && fast_retry_until.is_some_and(|until| Instant::now() < until)
                {
                    match jobs.recv_timeout(HIDRAW_FAST_PROBE_INTERVAL) {
                        Ok(job) => job,
                        Err(RecvTimeoutError::Timeout) => {
                            last_hidraw_probe = Instant::now();
                            hidraw_file = open_hidraw_actuator();
                            if hidraw_file.is_some() {
                                fast_retry_until = None;
                            }
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match jobs.recv() {
                        Ok(job) => job,
                        Err(_) => break,
                    }
                };

                match job {
                    HapticJob::Suspend => {
                        pending_stage2 = None;
                        hidraw_file = None;
                        fast_retry_until = None;
                    }
                    HapticJob::Resume => {
                        pending_stage2 = None;
                        play_file = open_play_actuator();
                        hidraw_file = open_hidraw_actuator();
                        last_hidraw_probe = Instant::now();
                        fast_retry_until = if probe_hidraw && hidraw_file.is_none() {
                            Some(Instant::now() + RESUME_RETRY_WINDOW)
                        } else {
                            None
                        };
                    }
                    HapticJob::Preset { preset, due } => {
                        let now = Instant::now();
                        if due > now {
                            thread::sleep(due - now);
                        } else if now.saturating_duration_since(due) > STALE_DROP {
                            continue;
                        }
                        if is_muted() {
                            continue;
                        }
                        if !send_play_with_reopen(&mut play_file, preset) {
                            let bytes = match preset {
                                PresetKind::Light => REPORT_CLICK_LIGHT,
                                PresetKind::Firm => REPORT_CLICK_FIRM,
                            };
                            let _ = send_hidraw_with_reopen(
                                &mut hidraw_file,
                                bytes,
                                &mut fast_retry_until,
                            );
                        }
                    }
                    HapticJob::Keyboard {
                        primary,
                        secondary,
                        fallback_firm,
                        due,
                    } => {
                        let now = Instant::now();
                        if now.saturating_duration_since(due) > STALE_DROP {
                            continue;
                        }
                        pending_stage2 = None;
                        if is_muted() {
                            continue;
                        }

                        if probe_hidraw && hidraw_file.is_none() {
                            let min_wait = if fast_retry_until.is_some_and(|u| now < u) {
                                HIDRAW_FAST_PROBE_INTERVAL
                            } else {
                                HIDRAW_SLOW_PROBE_INTERVAL
                            };
                            if last_hidraw_probe.elapsed() >= min_wait {
                                last_hidraw_probe = Instant::now();
                                hidraw_file = open_hidraw_actuator();
                                if hidraw_file.is_some() {
                                    fast_retry_until = None;
                                }
                            }
                        }

                        if hidraw_file.is_some()
                            && send_hidraw_with_reopen(
                                &mut hidraw_file,
                                primary.as_bytes(),
                                &mut fast_retry_until,
                            )
                        {
                            if let Some((sec_report, delay_us)) = secondary {
                                pending_stage2 = Some((
                                    Instant::now() + Duration::from_micros(delay_us),
                                    sec_report,
                                ));
                            }
                        } else {
                            let preset = if fallback_firm {
                                PresetKind::Firm
                            } else {
                                PresetKind::Light
                            };
                            let _ = send_play_with_reopen(&mut play_file, preset);
                        }
                    }
                }
            }
        })?;
    Ok(sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haptic_release_gap_default() {
        let mut haptic = Haptic::open(false, true, 140);
        assert_eq!(haptic.release_gap, Duration::from_millis(140));
        haptic.key_press();
        assert!(haptic.last_press.is_some());
        haptic.key_release();
        assert!(haptic.last_press.is_none());
    }

    #[test]
    fn haptic_slider_tick_rate_limit() {
        let mut haptic = Haptic::open(false, true, 140);
        haptic.slider_tick();
        let first_last = haptic.last;
        assert!(first_last.is_some());
        haptic.slider_tick();
        assert_eq!(haptic.last, first_last);
    }

    #[test]
    fn haptic_keyboard_disabled_no_panic() {
        let mut haptic = Haptic::open_with_keyboard(
            false,
            false,
            140,
            KeyboardHapticConfig::disabled(),
        );
        haptic.keyboard_key(input_linux::Key::A as u32, true);
        haptic.keyboard_key(input_linux::Key::Space as u32, false);
    }

    #[test]
    fn haptic_mute_gating_flag_works() {
        let haptic = Haptic::open_with_keyboard(
            false,
            false,
            140,
            KeyboardHapticConfig::disabled(),
        );
        assert!(!haptic.is_silenced_by_mute());
        haptic.set_system_muted(true);
        assert!(haptic.is_silenced_by_mute());
        haptic.set_system_muted(false);
        assert!(!haptic.is_silenced_by_mute());
    }
}
