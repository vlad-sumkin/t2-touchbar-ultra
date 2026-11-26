// SPDX-License-Identifier: GPL-3.0-or-later

//! Mechanical keyboard switch haptic synthesis for the Apple T2 Force Touch actuator.
//!
//! Ported from `asadm/mechpad` switch acoustic profiles and encoded into HID Report `0x53`
//! custom actuator waveforms matching the MacBook trackpad actuator hardware limits
//! (`AmplitudeMax = 70`).

use input_linux::Key;

/// HID Report ID used by Apple T2 Force Touch actuators for custom waveform synthesis.
pub const ACTUATOR_REPORT_ID: u8 = 0x53;

/// Maximum amplitude unit advertised by `t2_trackpad_actuator` (`ActuatorLimits AmplitudeMax`).
pub const ACTUATOR_AMP_MAX: u8 = 70;

const WAVE_NONE: u8 = 0;
const WAVE_GAUSSIAN: u8 = 1;
const TONE_SINE: u8 = 1;

const MIN_TONE_HZ: f64 = 95.0;
const MAX_TONE_HZ: f64 = 4000.0;
const MIN_DURATION_MS: f64 = 1.5;

/// Supported mechanical switch profiles matching `asadm/mechpad`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchProfile {
    CherryMxBlue,
    CherryMxBrown,
    CherryMxRed,
    HolyPanda,
    Topre45g,
    IbmModelM,
    GateronOilKing,
}

/// Acoustic category of a physical key, used for keycap/stabilizer resonance shaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Alpha,
    Spacebar,
    Enter,
    Backspace,
    Modifier,
}

#[derive(Clone, Copy, Debug)]
struct HapticTone {
    hz: f64,
    amp: f64,
    duration_ms: f64,
    delay_ms: f64,
}

#[derive(Clone, Debug)]
pub struct HapticBurst {
    pub base_gaussian_amp: f64,
    pub base_duration_ms: f64,
    pub level: f32,
    tones: Vec<HapticTone>,
}

#[derive(Clone, Debug)]
pub struct SynthesizedKey {
    pub primary: HapticBurst,
    pub secondary: Option<(HapticBurst, u64)>,
}

/// Encoded HID Report `0x53` payload ready to write to `/dev/hidraw*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveformReport {
    pub bytes: [u8; 64],
    pub len: usize,
}

impl WaveformReport {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

struct ProfileSpec {
    profile: SwitchProfile,
    id: &'static str,
    name: &'static str,
    down_base_amp: f64,
    down_base_dur_ms: f64,
    down_tones: &'static [HapticTone],
    down_level: f32,
    down_stage2_delay_ms: Option<f64>,
    down_stage2_tones: &'static [HapticTone],
    down_stage2_level: f32,
    up_base_amp: f64,
    up_base_dur_ms: f64,
    up_tones: &'static [HapticTone],
    up_level: f32,
}

const fn tone(hz: f64, amp: f64, duration_ms: f64) -> HapticTone {
    HapticTone {
        hz,
        amp,
        duration_ms,
        delay_ms: 0.0,
    }
}

const PROFILES: &[ProfileSpec] = &[
    // 1. Cherry MX Blue — sharp click-jacket snap + crisp clack
    ProfileSpec {
        profile: SwitchProfile::CherryMxBlue,
        id: "cherry-mx-blue",
        name: "Cherry MX Blue",
        down_base_amp: 0.16,
        down_base_dur_ms: 3.0,
        down_tones: &[
            tone(2680.0, 0.52, 5.5),
            tone(3850.0, 0.36, 4.5),
            tone(1240.0, 0.12, 6.5),
        ],
        down_level: 0.84,
        down_stage2_delay_ms: Some(5.0),
        down_stage2_tones: &[tone(560.0, 0.50, 6.5), tone(1480.0, 0.50, 5.0)],
        down_stage2_level: 0.48,
        up_base_amp: 0.06,
        up_base_dur_ms: 2.5,
        up_tones: &[tone(3180.0, 0.62, 4.0), tone(1680.0, 0.38, 4.5)],
        up_level: 0.42,
    },
    // 2. Cherry MX Brown — dry tactile notch + balanced bottom-out
    ProfileSpec {
        profile: SwitchProfile::CherryMxBrown,
        id: "cherry-mx-brown",
        name: "Cherry MX Brown",
        down_base_amp: 0.14,
        down_base_dur_ms: 4.5,
        down_tones: &[
            tone(1220.0, 0.46, 6.0),
            tone(1680.0, 0.28, 4.5),
            tone(480.0, 0.26, 8.5),
        ],
        down_level: 0.84,
        down_stage2_delay_ms: Some(4.5),
        down_stage2_tones: &[tone(680.0, 0.60, 6.5), tone(1120.0, 0.40, 4.5)],
        down_stage2_level: 0.40,
        up_base_amp: 0.05,
        up_base_dur_ms: 3.0,
        up_tones: &[tone(1240.0, 0.58, 4.0), tone(620.0, 0.42, 4.5)],
        up_level: 0.34,
    },
    // 3. Cherry MX Red — smooth linear travel with soft low-mid bottom-out
    ProfileSpec {
        profile: SwitchProfile::CherryMxRed,
        id: "cherry-mx-red",
        name: "Cherry MX Red",
        down_base_amp: 0.12,
        down_base_dur_ms: 5.5,
        down_tones: &[
            tone(520.0, 0.54, 9.0),
            tone(780.0, 0.36, 6.5),
            tone(1080.0, 0.10, 4.0),
        ],
        down_level: 0.62,
        down_stage2_delay_ms: None,
        down_stage2_tones: &[],
        down_stage2_level: 0.0,
        up_base_amp: 0.04,
        up_base_dur_ms: 3.0,
        up_tones: &[tone(740.0, 0.65, 4.0), tone(410.0, 0.35, 4.5)],
        up_level: 0.26,
    },
    // 4. Holy Panda — punchy, hollow woody pop + warm thock
    ProfileSpec {
        profile: SwitchProfile::HolyPanda,
        id: "holy-panda",
        name: "Holy Panda",
        down_base_amp: 0.18,
        down_base_dur_ms: 6.0,
        down_tones: &[
            tone(660.0, 0.48, 10.0),
            tone(920.0, 0.32, 7.0),
            tone(310.0, 0.20, 12.0),
        ],
        down_level: 0.98,
        down_stage2_delay_ms: Some(5.5),
        down_stage2_tones: &[tone(520.0, 0.62, 9.0), tone(780.0, 0.38, 6.0)],
        down_stage2_level: 0.56,
        up_base_amp: 0.10,
        up_base_dur_ms: 4.0,
        up_tones: &[tone(820.0, 0.58, 5.5), tone(440.0, 0.42, 6.5)],
        up_level: 0.44,
    },
    // 5. Topre 45g — deepest muted rubber-dome electrostatic thock
    ProfileSpec {
        profile: SwitchProfile::Topre45g,
        id: "topre-45g",
        name: "Topre 45g",
        down_base_amp: 0.26,
        down_base_dur_ms: 9.5,
        down_tones: &[
            tone(150.0, 0.52, 16.5),
            tone(265.0, 0.38, 13.5),
            tone(390.0, 0.10, 8.0),
        ],
        down_level: 0.98,
        down_stage2_delay_ms: None,
        down_stage2_tones: &[],
        down_stage2_level: 0.0,
        up_base_amp: 0.08,
        up_base_dur_ms: 5.0,
        up_tones: &[tone(260.0, 0.65, 7.5), tone(380.0, 0.35, 6.0)],
        up_level: 0.34,
    },
    // 6. IBM Model M — loud buckling-spring snap + metallic spring ping
    ProfileSpec {
        profile: SwitchProfile::IbmModelM,
        id: "ibm-model-m",
        name: "IBM Model M",
        down_base_amp: 0.18,
        down_base_dur_ms: 4.0,
        down_tones: &[
            tone(1420.0, 0.42, 6.5),
            tone(2640.0, 0.38, 6.0),
            tone(3920.0, 0.20, 5.5),
        ],
        down_level: 1.00,
        down_stage2_delay_ms: Some(6.5),
        down_stage2_tones: &[tone(2740.0, 0.55, 22.0), tone(3951.0, 0.45, 18.0)],
        down_stage2_level: 0.72,
        up_base_amp: 0.10,
        up_base_dur_ms: 3.0,
        up_tones: &[tone(2520.0, 0.58, 5.5), tone(1320.0, 0.42, 5.5)],
        up_level: 0.52,
    },
    // 7. Gateron Oil King — factory-lubed creamy, marbly deep linear
    ProfileSpec {
        profile: SwitchProfile::GateronOilKing,
        id: "gateron-oil-king",
        name: "Gateron Oil King",
        down_base_amp: 0.20,
        down_base_dur_ms: 7.0,
        down_tones: &[
            tone(310.0, 0.44, 12.5),
            tone(580.0, 0.38, 10.0),
            tone(860.0, 0.18, 6.5),
        ],
        down_level: 0.92,
        down_stage2_delay_ms: None,
        down_stage2_tones: &[],
        down_stage2_level: 0.0,
        up_base_amp: 0.07,
        up_base_dur_ms: 4.0,
        up_tones: &[tone(510.0, 0.58, 5.5), tone(320.0, 0.42, 6.0)],
        up_level: 0.32,
    },
];

impl KeyKind {
    pub fn from_evdev(key_code: u32) -> Self {
        if key_code == Key::Space as u32 {
            Self::Spacebar
        } else if key_code == Key::Enter as u32 || key_code == Key::KpEnter as u32 {
            Self::Enter
        } else if key_code == Key::Backspace as u32 || key_code == Key::Delete as u32 {
            Self::Backspace
        } else if key_code == Key::Tab as u32
            || key_code == Key::Esc as u32
            || key_code == Key::CapsLock as u32
            || key_code == Key::LeftShift as u32
            || key_code == Key::RightShift as u32
            || key_code == Key::LeftCtrl as u32
            || key_code == Key::RightCtrl as u32
            || key_code == Key::LeftAlt as u32
            || key_code == Key::RightAlt as u32
            || key_code == Key::LeftMeta as u32
            || key_code == Key::RightMeta as u32
            || key_code == Key::Fn as u32
        {
            Self::Modifier
        } else {
            Self::Alpha
        }
    }

    pub fn pitch_multiplier(self) -> f64 {
        match self {
            Self::Alpha => 1.00,
            Self::Spacebar => 0.72,
            Self::Enter => 0.80,
            Self::Backspace => 0.85,
            Self::Modifier => 0.90,
        }
    }

    pub fn level_multiplier(self) -> f32 {
        match self {
            Self::Alpha => 1.00,
            Self::Spacebar => 1.15,
            Self::Enter => 1.10,
            Self::Backspace => 1.06,
            Self::Modifier => 0.92,
        }
    }

    pub fn duration_multiplier(self) -> f64 {
        match self {
            Self::Alpha => 1.00,
            Self::Spacebar => 1.32,
            Self::Enter => 1.18,
            Self::Backspace => 1.12,
            Self::Modifier => 1.08,
        }
    }
}

/// Returns `false` for non-typing ACPI/power keys so closing lid or pressing power doesn't click.
pub fn should_click_evdev(key_code: u32) -> bool {
    key_code != Key::Power as u32
        && key_code != Key::Sleep as u32
        && key_code != Key::Wakeup as u32
        && key_code != Key::Suspend as u32
        && key_code != Key::Battery as u32
        && key_code != Key::Rfkill as u32
}

impl SwitchProfile {
    pub const ALL_IDS: &'static [&'static str] = &[
        "cherry-mx-blue",
        "cherry-mx-brown",
        "cherry-mx-red",
        "holy-panda",
        "topre-45g",
        "ibm-model-m",
        "gateron-oil-king",
    ];

    pub fn parse(raw: &str) -> Option<Self> {
        let norm: String = raw
            .trim()
            .chars()
            .map(|ch| {
                if ch == '_' || ch == ' ' {
                    '-'
                } else {
                    ch.to_ascii_lowercase()
                }
            })
            .collect();
        match norm.as_str() {
            "cherry-mx-blue" | "mx-blue" | "blue" => Some(Self::CherryMxBlue),
            "cherry-mx-brown" | "mx-brown" | "brown" => Some(Self::CherryMxBrown),
            "cherry-mx-red" | "mx-red" | "red" => Some(Self::CherryMxRed),
            "holy-panda" | "panda" => Some(Self::HolyPanda),
            "topre-45g" | "topre" => Some(Self::Topre45g),
            "ibm-model-m" | "model-m" | "buckling-spring" => Some(Self::IbmModelM),
            "gateron-oil-king" | "oil-king" | "gateron" => Some(Self::GateronOilKing),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        self.spec().id
    }

    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    fn spec(self) -> &'static ProfileSpec {
        PROFILES
            .iter()
            .find(|spec| spec.profile == self)
            .expect("all SwitchProfile variants have specs")
    }

    /// Synthesizes primary (and optional delayed stage-2) bursts with organic jitter.
    /// `rng_state` is updated in-place via splitmix/xorshift to avoid external dependencies.
    pub fn synthesize(
        self,
        key_kind: KeyKind,
        is_key_down: bool,
        use_key_acoustics: bool,
        rng_state: &mut u64,
    ) -> SynthesizedKey {
        let spec = self.spec();
        let pitch_jitter = next_unit(rng_state) * 0.08 + 0.96; // 0.96..=1.04
        let level_jitter = (next_unit(rng_state) as f32) * 0.10 + 0.95; // 0.95..=1.05

        let pitch_mult = if use_key_acoustics {
            key_kind.pitch_multiplier()
        } else {
            1.0
        } * pitch_jitter;
        let level_mult = if use_key_acoustics {
            key_kind.level_multiplier()
        } else {
            1.0
        } * level_jitter;
        let dur_mult = if use_key_acoustics {
            key_kind.duration_multiplier()
        } else {
            1.0
        };

        if is_key_down {
            let scaled_tones = spec
                .down_tones
                .iter()
                .map(|t| HapticTone {
                    hz: t.hz * pitch_mult,
                    amp: t.amp,
                    duration_ms: t.duration_ms * dur_mult,
                    delay_ms: t.delay_ms,
                })
                .collect();
            let primary = HapticBurst {
                base_gaussian_amp: spec.down_base_amp,
                base_duration_ms: spec.down_base_dur_ms * dur_mult,
                level: (spec.down_level * level_mult).min(1.0),
                tones: scaled_tones,
            };

            let secondary = spec
                .down_stage2_delay_ms
                .filter(|_| !spec.down_stage2_tones.is_empty())
                .map(|delay_ms| {
                    let s2_tones = spec
                        .down_stage2_tones
                        .iter()
                        .map(|t| HapticTone {
                            hz: t.hz * pitch_mult,
                            amp: t.amp,
                            duration_ms: t.duration_ms * dur_mult,
                            delay_ms: t.delay_ms,
                        })
                        .collect();
                    let s2_burst = HapticBurst {
                        base_gaussian_amp: 0.0,
                        base_duration_ms: 4.0,
                        level: (spec.down_stage2_level * level_mult).min(1.0),
                        tones: s2_tones,
                    };
                    (s2_burst, (delay_ms * 1000.0).round() as u64)
                });

            SynthesizedKey { primary, secondary }
        } else {
            let scaled_up = spec
                .up_tones
                .iter()
                .map(|t| HapticTone {
                    hz: t.hz * pitch_mult,
                    amp: t.amp,
                    duration_ms: t.duration_ms * dur_mult,
                    delay_ms: t.delay_ms,
                })
                .collect();
            let primary = HapticBurst {
                base_gaussian_amp: spec.up_base_amp,
                base_duration_ms: spec.up_base_dur_ms * dur_mult,
                level: (spec.up_level * level_mult).min(1.0),
                tones: scaled_up,
            };
            SynthesizedKey {
                primary,
                secondary: None,
            }
        }
    }
}

/// Encodes a `HapticBurst` into HID Report `0x53` bytes for the T2 trackpad actuator.
/// Returns `None` if `master_intensity` / effective amplitudes are effectively silent.
pub fn encode_report_53(burst: &HapticBurst, master_intensity: f32) -> Option<WaveformReport> {
    let clamped_level = (burst.level * master_intensity).clamp(0.0, 1.0) as f64;
    if clamped_level <= 0.005 || (burst.tones.is_empty() && burst.base_gaussian_amp <= 0.0) {
        return None;
    }

    let base_norm = burst.base_gaussian_amp.clamp(0.0, 0.6);
    let tone_sum: f64 = burst.tones.iter().map(|t| t.amp.max(0.0)).sum();
    let total_norm = (tone_sum + base_norm).max(0.0001);
    let tone_scale = if total_norm > 1.0 {
        (1.0 - base_norm) / tone_sum.max(0.0001)
    } else {
        1.0
    };

    let scale_amp = f64::from(ACTUATOR_AMP_MAX) * clamped_level;
    let mut base_amp = (base_norm * scale_amp).round().clamp(0.0, f64::from(ACTUATOR_AMP_MAX)) as u8;
    let mut wave_type = if base_amp > 0 { WAVE_GAUSSIAN } else { WAVE_NONE };
    let base_dur = encode_duration_ms(burst.base_duration_ms.max(MIN_DURATION_MS));

    let mut bytes = [0u8; 64];
    bytes[0] = ACTUATOR_REPORT_ID;

    let mut tone_count = 0usize;
    let mut remaining_amp = ACTUATOR_AMP_MAX.saturating_sub(base_amp);

    for tone in burst.tones.iter().take(12) {
        let raw_amp = (tone.amp.max(0.0) * tone_scale * scale_amp).round();
        let mut t_amp = raw_amp.clamp(0.0, f64::from(ACTUATOR_AMP_MAX)) as u8;
        if t_amp == 0 {
            continue;
        }
        if t_amp > remaining_amp {
            t_amp = remaining_amp;
        }
        if t_amp == 0 {
            continue;
        }
        remaining_amp = remaining_amp.saturating_sub(t_amp);

        let offset = 4 + tone_count * 5;
        bytes[offset] = TONE_SINE;
        bytes[offset + 1] = t_amp;
        bytes[offset + 2] = encode_duration_ms(tone.delay_ms.max(0.0));
        bytes[offset + 3] = encode_duration_ms(tone.duration_ms.max(MIN_DURATION_MS));
        bytes[offset + 4] = encode_freq_hz(tone.hz);
        tone_count += 1;
    }

    if base_amp == 0 && tone_count == 0 {
        return None;
    }

    // Keep base fields consistent when base Gaussian is muted (e.g. stage-2 spring ping).
    if base_amp == 0 {
        wave_type = WAVE_NONE;
        base_amp = 0;
    }
    bytes[1] = wave_type;
    bytes[2] = base_amp;
    bytes[3] = base_dur;

    Some(WaveformReport {
        bytes,
        len: 4 + tone_count * 5,
    })
}

/// Duration/delay byte in Q5.3 milliseconds (1 unit = 0.125 ms, range 0..31.875 ms).
fn encode_duration_ms(ms: f64) -> u8 {
    if ms <= 0.0 {
        return 0;
    }
    (ms * 8.0).round().clamp(1.0, 255.0) as u8
}

/// Frequency byte in Q2.6 kilohertz (1 unit = 15.625 Hz, 4000 Hz -> 255).
fn encode_freq_hz(hz: f64) -> u8 {
    let clamped_hz = hz.clamp(MIN_TONE_HZ, MAX_TONE_HZ);
    ((clamped_hz / 1000.0) * 64.0).round().clamp(1.0, 255.0) as u8
}

fn next_unit(state: &mut u64) -> f64 {
    if *state == 0 {
        *state = 0x9e37_79b9_7f4a_7c15;
    }
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    ((x >> 11) as f64) / ((1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_seven_profiles_by_id_and_display_name() {
        for &id in SwitchProfile::ALL_IDS {
            let p = SwitchProfile::parse(id).expect("id should parse");
            assert_eq!(p.id(), id);
            assert_eq!(SwitchProfile::parse(p.name()), Some(p));
        }
        assert!(SwitchProfile::parse("cherry-mx-green").is_none());
    }

    #[test]
    fn key_kind_matches_evdev_categories() {
        assert_eq!(KeyKind::from_evdev(Key::Space as u32), KeyKind::Spacebar);
        assert_eq!(KeyKind::from_evdev(Key::Enter as u32), KeyKind::Enter);
        assert_eq!(KeyKind::from_evdev(Key::KpEnter as u32), KeyKind::Enter);
        assert_eq!(KeyKind::from_evdev(Key::Backspace as u32), KeyKind::Backspace);
        assert_eq!(KeyKind::from_evdev(Key::Delete as u32), KeyKind::Backspace);
        assert_eq!(KeyKind::from_evdev(Key::LeftShift as u32), KeyKind::Modifier);
        assert_eq!(KeyKind::from_evdev(Key::Fn as u32), KeyKind::Modifier);
        assert_eq!(KeyKind::from_evdev(Key::A as u32), KeyKind::Alpha);
        assert!(!should_click_evdev(Key::Power as u32));
        assert!(should_click_evdev(Key::A as u32));
    }

    #[test]
    fn synthesized_reports_stay_within_hardware_amplitude_limit() {
        let mut rng = 0x1234_5678_9abc_def0;
        for &id in SwitchProfile::ALL_IDS {
            let profile = SwitchProfile::parse(id).unwrap();
            for kind in [
                KeyKind::Alpha,
                KeyKind::Spacebar,
                KeyKind::Enter,
                KeyKind::Backspace,
                KeyKind::Modifier,
            ] {
                for down in [true, false] {
                    let synth = profile.synthesize(kind, down, true, &mut rng);
                    let rep1 = encode_report_53(&synth.primary, 1.0)
                        .expect("primary waveform at full intensity should be non-empty");
                    assert_eq!(rep1.bytes[0], ACTUATOR_REPORT_ID);
                    assert!(rep1.len <= 64);

                    let total_amp: u16 = u16::from(rep1.bytes[2])
                        + rep1.as_bytes()[4..]
                            .chunks_exact(5)
                            .map(|c| u16::from(c[1]))
                            .sum::<u16>();
                    assert!(
                        total_amp <= u16::from(ACTUATOR_AMP_MAX),
                        "profile {id} total amp {total_amp} exceeds {}",
                        ACTUATOR_AMP_MAX
                    );

                    if let Some((sec, _delay_us)) = synth.secondary {
                        let rep2 = encode_report_53(&sec, 1.0)
                            .expect("secondary waveform should be non-empty");
                        assert_eq!(rep2.bytes[0], ACTUATOR_REPORT_ID);
                        let s2_amp: u16 = u16::from(rep2.bytes[2])
                            + rep2.as_bytes()[4..]
                                .chunks_exact(5)
                                .map(|c| u16::from(c[1]))
                                .sum::<u16>();
                        assert!(s2_amp <= u16::from(ACTUATOR_AMP_MAX));
                    }
                }
            }
        }
    }

    #[test]
    fn zero_intensity_produces_no_report() {
        let mut rng = 42;
        let synth = SwitchProfile::CherryMxBlue.synthesize(KeyKind::Alpha, true, true, &mut rng);
        assert!(encode_report_53(&synth.primary, 0.0).is_none());
    }
}
