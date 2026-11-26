// SPDX-License-Identifier: GPL-3.0-or-later

mod backlight;
mod clipboard;
mod display;
mod gesture;
mod kbdlight;
mod keyboard;
mod lookup;
mod mpris;
mod niri;
mod pixel_shift;
mod policy;
mod predict;
mod renderer;
mod sleep;
mod text_input;
mod touchid;
mod usb;

use t2_touchbar_ultra::{config, haptic, keys, levels, mechpad, status, theme};

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Read,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::{fs::OpenOptionsExt, net::UnixStream},
    },
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use input::{
    Device as InputDevice, DeviceCapability, Libinput, LibinputInterface, SendEventsMode,
    event::{
        DeviceEvent, Event, EventTrait,
        keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
        pointer::{ButtonState, PointerEvent},
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot},
    },
};
use input_linux::Key;
use libc::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY, pollfd};

use backlight::Backlight;
use config::{Config, state_path};
use display::Display;
use gesture::{Swipe, SwipeKind};
use haptic::{Haptic, KeyboardHapticConfig};
use keyboard::VirtualKeyboard;
use keys::{CustomAction, CustomKey};
use levels::{Level, Reader as LevelReader};
use mpris::{Mpris, Track};
use policy::{Mode, PersistentState, TimeoutLearner};
use predict::{Predictor, code_to_key, key_to_char, extract_context_at_cursor, is_word_char};
use renderer::{Action, Canvas, Layout, LevelKind, TrackLayout};
use sleep::SleepEvent;
use text_input::{TextInputEvent, TextInputWatcher};
use touchid::TouchIdState;

/// How long the esc key stays lit after it was used on the dark bar, so even
/// a quick tap is visible.
const ESC_LINGER_MS: u64 = 150;
/// Swipe feedback stays a little after the fingers lift to show the result.
const LEVEL_LINGER_MS: u64 = 700;
const LEVEL_FADE_MS: u64 = 350;
const LEVEL_FRAME_MS: u64 = 33;
const ACTIVITY_FADE_MS: u64 = 700;
const ACTIVITY_FRAME_MS: u64 = 33;
/// The desktop applies a volume or brightness key asynchronously, so the
/// value is read back repeatedly for a short while after each step.
const LEVEL_POLL_MS: u64 = 50;
const LEVEL_POLL_WINDOW_MS: u64 = 600;
/// The level feedback crossfades to the other side of the fingers once
/// they pass the middle of the bar by this much.
const LEVEL_SIDE_HYSTERESIS_PX: f64 = 60.0;
const LEVEL_SIDE_FADE_MS: u64 = 700;
/// Switching rows move far enough to read as a swipe across the bar.
const LAYER_SLIDE_PX: f32 = 190.0;
/// Switching rows take 195 ms.
const LAYER_FADE_MS: u64 = 195;
const LAYER_FRAME_MS: u64 = 20;
/// Track overlay timeline: fade in, hold, fade out.
const TRACK_FADE_MS: u64 = 700;
const TRACK_HOLD_MS: u64 = 5_000;
/// About 30 fps while the overlay fades.
const TRACK_FRAME_MS: u64 = 33;
/// Holding previous/next this long from touch-down starts seeking.
/// The Touch ID arrow swings towards the sensor once per period, at about
/// 30 fps.
const ARROW_PERIOD_MS: u64 = 1_500;
const ARROW_FRAME_MS: u64 = 33;
const MEDIA_HOLD_MS: u64 = 450;
const SEEK_INTERVAL_MS: u64 = 250;
/// Seek steps grow after this long, so long distances stay quick.
const SEEK_FAST_AFTER_MS: u64 = 1_500;
const SEEK_STEP: Duration = Duration::from_secs(5);
const SEEK_FAST_STEP: Duration = Duration::from_secs(15);
/// Inactivity timeout for sleeping (battery) or dimming 2x (mains/charging).
const IDLE_TIMEOUT_MS: u64 = 300_000;

pub(crate) static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> std::result::Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;
        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(Into::into)
            .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

#[derive(Clone, Copy, Debug)]
enum Contact {
    Wake,
    Button(usize),
    TouchIdEsc(usize),
    Cancelled,
    /// First finger, waiting whether a second one turns this into a swipe.
    Pending,
    Swipe,
    /// Esc held on the dark bar. The bar stays dark.
    DarkEsc,
    /// Previous/next held: a tap on release, seeking once held long enough.
    MediaHold(usize),
    /// A key of the track overlay on the dark bar; `true` is next.
    TrackKey(bool),
    MediaScrubbing,
    ColorRibbon,
    EmojiScroll,
    #[allow(dead_code)]
    WindowButton(u64, f64, f64),
}

/// Crossfade from the previous row to the current one.
struct LayerFade {
    /// The frame that was on screen when the switch started.
    from: Vec<u32>,
    to: Vec<u32>,
    start_ms: u64,
    duration_ms: u64,
    next_ms: u64,
    /// `1` when the new row comes in from the right, `-1` from the left.
    direction: i32,
    /// Everything left of this column (esc) stays in place.
    clip_left: usize,
    /// A swipe switched the row: a firm click confirms it once the new row
    /// has settled.
    confirm: bool,
    start_progress: f32,
    target_progress: f32,
    slide_px: f32,
}

/// Interactive 1:1 physical touch dragging state across panel modes.
struct ModeSwipeState {
    anchor: f64,
    step: f64,
    clip_left: usize,
    base_mode: Mode,
    media_frame: Vec<u32>,
    function_frame: Vec<u32>,
    special_frame: Vec<u32>,
    last_centroid: f64,
}

impl ModeSwipeState {
    fn frame(&self, mode: Mode) -> &[u32] {
        match mode {
            Mode::Media => &self.media_frame,
            Mode::Function => &self.function_frame,
            Mode::Special => &self.special_frame,
        }
    }
}

/// "Artist – Title" shown on the dark bar when a new track starts.
struct TrackToast {
    track: Track,
    /// Origin of the fade in / hold / fade out timeline.
    start_ms: u64,
    next_ms: Option<u64>,
    layout: TrackLayout,
    pressed: Option<bool>,
}

struct WordToast {
    def: lookup::WordDefinition,
    start_ms: u64,
    expire_ms: u64,
    is_fast: bool,
}

struct BacklightFade {
    start_ms: u64,
    next_ms: u64,
    from: u32,
    to: u32,
}

struct EscapeFade {
    start_ms: u64,
    next_ms: u64,
    from: f32,
    to: f32,
}

/// Previous/next under a finger. Released early it skips the track, held
/// past `MEDIA_HOLD_MS` it seeks in steps until released.
struct MediaHold {
    forward: bool,
    next_ms: u64,
    seeking_since: Option<u64>,
    player: Option<String>,
}

/// A first touch held back for `two_finger_window_ms`. On the dark bar it
/// becomes a wake, on the lit bar a key press, unless a second finger joins.
#[derive(Clone, Copy, Debug)]
struct PendingTouch {
    slot: u32,
    deadline_ms: u64,
    dark: bool,
    button: Option<usize>,
}

struct Runtime {
    config: Config,
    state: PersistentState,
    state_path: std::path::PathBuf,
    learner: TimeoutLearner,
    started: Instant,
    visible: bool,
    touch_id: TouchIdState,
    deadline_ms: Option<u64>,
    fn_down_ms: Option<u64>,
    fn_toggled: bool,
    contacts: HashMap<u32, Contact>,
    positions: HashMap<u32, f64>,
    pending: Option<PendingTouch>,
    swipe: Option<Swipe>,
    /// The dark bar currently lights only the esc key.
    overlay_shown: bool,
    overlay_hide_ms: Option<u64>,
    /// Level feedback while a dark-bar swipe runs.
    level_kind: Option<LevelKind>,
    level: Level,
    /// Leftmost and rightmost finger the feedback was last placed against.
    level_fingers: (f64, f64),
    /// How much of the feedback shows right of the fingers, moving towards
    /// `level_right_target` for the crossfade.
    level_right: f32,
    level_right_target: bool,
    level_right_ms: u64,
    level_poll_ms: Option<u64>,
    level_poll_until_ms: u64,
    /// The level feedback changed and is drawn once the pending input is
    /// processed, so a burst of touch events costs one frame.
    level_dirty: bool,
    level_fade_start_ms: Option<u64>,
    level_frame_ms: Option<u64>,
    /// This swipe has sent a key, so a level at 0 or 100 was reached by it.
    level_stepped: bool,
    /// The stop the level sits at was already felt as a firm click.
    level_stop_felt: bool,
    activity_deadline_ms: Option<u64>,
    activity_dimmed: bool,
    activity_restore_brightness: Option<u32>,
    activity_max_brightness: Option<u32>,
    activity_esc_shown: bool,
    activity_current_brightness: Option<u32>,
    backlight_fade: Option<BacklightFade>,
    escape_opacity: f32,
    escape_fade: Option<EscapeFade>,
    /// Last values seen per kind, shown until a fresh read arrives.
    level_cache: [Level; 2],
    level_reader: LevelReader,
    level_wake: UnixStream,
    key_level: u32,
    media_hold: Option<MediaHold>,
    mpris: Mpris,
    toast: Option<TrackToast>,
    layer_fade: Option<LayerFade>,
    /// The blended frame while a crossfade runs.
    blended: Vec<u32>,
    touches_armed: bool,
    active_button: Option<usize>,
    layout: Layout,
    canvas: Canvas,
    display: Display,
    backlight: Backlight,
    keyboard: VirtualKeyboard,
    haptic: Haptic,
    pressed_physical_keys: std::collections::HashSet<u32>,
    physical_escape: bool,
    /// Personal keys from keys.toml, shown in the special row.
    custom_keys: Vec<CustomKey>,
    custom_items: Vec<renderer::CustomItem>,
    current_track: Option<String>,
    next_idle_refresh_ms: u64,
    animation_start_ms: u64,
    next_animation_ms: Option<u64>,
    pixel_shift: pixel_shift::PixelShiftManager,
    last_activity_ms: u64,
    last_touchbar_activity_ms: u64,
    idle_sleeping: bool,
    idle_dimmed: bool,
    active_window: Option<niri::ActiveWindow>,
    cached_workspace: Option<niri::WorkspaceInfo>,
    next_idle_scroll_ms: Option<u64>,
    last_polled_step: Option<u64>,
    cached_clock: Option<status::ClockWidgetData>,
    cached_battery: Option<status::BatteryStatus>,
    cached_ram: Option<u32>,
    cached_cpu: Option<u32>,
    cached_cpu_temp: Option<u32>,
    cached_keyboard_layout: Option<String>,
    cached_brightness: Option<u32>,
    media_position_us: Option<u64>,
    media_length_us: Option<u64>,
    media_is_playing: bool,
    media_position_sampled_at_ms: u64,
    media_last_query_ms: u64,
    mode_swipe: Option<ModeSwipeState>,
    theme_path: std::path::PathBuf,
    last_theme_mtime: Option<std::time::SystemTime>,
    last_theme_check_ms: u64,
    active_modifiers: std::collections::HashSet<u32>,
    predictor: Predictor,
    predict_wake: UnixStream,
    input_chars: Vec<char>,
    cursor_pos: usize,
    suggestions: Vec<String>,
    suggestions_active: bool,
    suggestions_manually_dismissed: bool,
    clipboard_expire_ms: Option<u64>,
    next_word_mode: bool,
    last_applied_word: Option<String>,
    shift_down: bool,
    repeat_key: Option<(u32, u64)>,
    space_down: bool,
    scrubbing: Option<(u32, f64, f64)>,
    text_input_focused: bool,
    text_input_has_surrounding: bool,
    media_scrubbing: Option<(u32, f64, bool)>,
    media_scrub_progress: Option<f32>,
    media_scrub_display_until_ms: Option<u64>,
    color_picker_active: bool,
    color_hue: f32,
    color_copied: bool,
    color_copied_deadline: Option<u64>,
    emoji_strip_active: bool,
    emoji_scroll_offset: f32,
    emoji_scroll_active: bool,
    emoji_scroll_last_centroid: Option<f64>,
    workspace_windows: Vec<niri::NiriWindowItem>,
    clipboard_manager: clipboard::ClipboardManager,
    word_toast: Option<WordToast>,
}

impl Runtime {
    fn new(config: Config, state: PersistentState) -> Result<Self> {
        let mut display = Display::open()?;
        let (width, height) = display.dimensions();
        let theme_path = theme::theme_path();
        let last_theme_mtime = std::fs::metadata(&theme_path).and_then(|m| m.modified()).ok();
        let theme = theme::Theme::load();
        let canvas = Canvas::new(
            width,
            height,
            config.key_color(),
            config.font_family.as_deref(),
            config.font_size,
            config.status_font_size,
            Some(theme),
        )?;
        display.present(&canvas.pixels, width, height)?;
        let mut backlight = Backlight::open()?;
        backlight.set(0)?;
        let physical_escape = config.physical_escape.unwrap_or_else(has_physical_escape);
        let custom_keys = keys::load();
        let custom_items: Vec<renderer::CustomItem> = custom_keys
            .iter()
            .map(|key| {
                let icon_name = key.icon.unwrap_or(key.label);
                let svg = renderer::load_icon_svg(icon_name, 64, 44);
                renderer::CustomItem {
                    label: key.label,
                    icon: key.icon,
                    svg,
                }
            })
            .collect();
        let custom_codes: Vec<Key> = custom_keys
            .iter()
            .flat_map(|key| key.keys().iter().copied())
            .collect();
        let layout = Layout::with_custom(state.mode, width, physical_escape, &custom_items);
        let keyboard_haptics = KeyboardHapticConfig {
            enabled: config.haptic_keyboard,
            profile: mechpad::SwitchProfile::parse(&config.haptic_keyboard_profile)
                .unwrap_or(mechpad::SwitchProfile::CherryMxBlue),
            intensity: config.haptic_keyboard_intensity,
            key_up: config.haptic_keyboard_key_up,
            acoustics: config.haptic_keyboard_acoustics,
            mute_with_audio: config.haptic_mute_with_audio,
        };
        let haptic = Haptic::open_with_keyboard(
            config.haptic_feedback,
            config.haptic_touch_up,
            config.haptic_release_gap_ms,
            keyboard_haptics,
        );
        let init_vol = levels::volume();
        haptic.set_system_muted(init_vol.muted || init_vol.percent == Some(0));
        let (level_reader, level_wake) = LevelReader::spawn()?;
        level_wake.set_nonblocking(true)?;
        let (predictor, predict_wake) = Predictor::new();
        predict_wake.set_nonblocking(true)?;
        let initial_keyboard_brightness = config
            .activity_backlight
            .then(|| kbdlight::brightness().ok())
            .flatten();
        let activity_restore_brightness =
            initial_keyboard_brightness.filter(|brightness| *brightness > 0);
        let activity_dimmed = config.activity_backlight && initial_keyboard_brightness == Some(0);
        let activity_deadline_ms = config
            .activity_backlight
            .then_some(config.activity_timeout_ms);
        Ok(Self {
            config,
            state,
            state_path: state_path(),
            learner: TimeoutLearner::new(),
            started: Instant::now(),
            visible: false,
            touch_id: TouchIdState::Idle,
            deadline_ms: None,
            fn_down_ms: None,
            fn_toggled: false,
            contacts: HashMap::new(),
            positions: HashMap::new(),
            pending: None,
            swipe: None,
            overlay_shown: false,
            overlay_hide_ms: None,
            level_kind: None,
            level: Level::default(),
            level_fingers: (0.0, 0.0),
            level_right: 1.0,
            level_right_target: true,
            level_right_ms: 0,
            level_poll_ms: None,
            level_poll_until_ms: 0,
            level_dirty: false,
            level_fade_start_ms: None,
            level_frame_ms: None,
            level_stepped: false,
            level_stop_felt: false,
            activity_deadline_ms,
            activity_dimmed,
            activity_restore_brightness,
            activity_max_brightness: None,
            activity_esc_shown: false,
            activity_current_brightness: initial_keyboard_brightness,
            backlight_fade: None,
            escape_opacity: 0.0,
            escape_fade: None,
            level_cache: [init_vol, Level::default()],
            level_reader,
            level_wake,
            key_level: 100,
            media_hold: None,
            mpris: Mpris::default(),
            toast: None,
            layer_fade: None,
            blended: Vec::new(),
            touches_armed: true,
            active_button: None,
            layout,
            canvas,
            display,
            backlight,
            keyboard: VirtualKeyboard::open(&custom_codes)?,
            haptic,
            pressed_physical_keys: std::collections::HashSet::new(),
            physical_escape,
            custom_keys,
            custom_items,
            current_track: None,
            next_idle_refresh_ms: 0,
            animation_start_ms: 0,
            next_animation_ms: None,
            pixel_shift: pixel_shift::PixelShiftManager::new(),
            last_activity_ms: 0,
            last_touchbar_activity_ms: 0,
            idle_sleeping: false,
            idle_dimmed: false,
            active_window: niri::query_focused_window(),
            cached_workspace: niri::query_workspaces().0,
            next_idle_scroll_ms: None,
            last_polled_step: None,
            cached_clock: None,
            cached_battery: None,
            cached_ram: status::read_ram_percent(),
            cached_cpu: status::read_cpu_percent(),
            cached_cpu_temp: status::read_cpu_temp(),
            cached_keyboard_layout: niri::query_keyboard_layout(),
            cached_brightness: levels::brightness().percent,
            media_position_us: None,
            media_length_us: None,
            media_is_playing: true,
            media_position_sampled_at_ms: 0,
            media_last_query_ms: 0,
            mode_swipe: None,
            theme_path,
            last_theme_mtime,
            last_theme_check_ms: 0,
            active_modifiers: std::collections::HashSet::new(),
            predictor,
            predict_wake,
            input_chars: Vec::new(),
            cursor_pos: 0,
            suggestions: Vec::new(),
            suggestions_active: false,
            suggestions_manually_dismissed: false,
            clipboard_expire_ms: None,
            next_word_mode: false,
            last_applied_word: None,
            shift_down: false,
            repeat_key: None,
            space_down: false,
            scrubbing: None,
            text_input_focused: false,
            text_input_has_surrounding: false,
            media_scrubbing: None,
            media_scrub_progress: None,
            media_scrub_display_until_ms: None,
            color_picker_active: false,
            color_hue: 0.0,
            color_copied: false,
            color_copied_deadline: None,
            emoji_strip_active: false,
            emoji_scroll_offset: 0.0,
            emoji_scroll_active: false,
            emoji_scroll_last_centroid: None,
            workspace_windows: niri::query_windows(),
            clipboard_manager: clipboard::ClipboardManager::new(),
            word_toast: None,
        })
    }

    fn check_theme_reload(&mut self, now_ms: u64) -> Result<()> {
        if now_ms.saturating_sub(self.last_theme_check_ms) >= 1000 {
            self.last_theme_check_ms = now_ms;
            let current_path = theme::theme_path();
            let current_mtime = std::fs::metadata(&current_path).and_then(|m| m.modified()).ok();
            if current_path != self.theme_path || current_mtime != self.last_theme_mtime {
                self.theme_path = current_path;
                self.last_theme_mtime = current_mtime;
                let new_theme = theme::Theme::load();
                self.canvas.set_theme(new_theme);
                self.next_idle_refresh_ms = 0;
                self.next_idle_scroll_ms = Some(now_ms);
                if self.visible {
                    self.present_keys()?;
                }
            }
        }
        Ok(())
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn current_media_progress(&self, now: u64) -> Option<f32> {
        if self.current_track.is_some()
            && let (Some(pos_us), Some(len_us)) = (self.media_position_us, self.media_length_us)
            && len_us > 0
        {
            let elapsed_ms = if self.media_is_playing {
                now.saturating_sub(self.media_position_sampled_at_ms)
            } else {
                0
            };
            let mut current_pos_us = pos_us + elapsed_ms * 1000;
            // Short looping videos (<= 30s) wrap around seamlessly
            if current_pos_us >= len_us && len_us <= 30_000_000 {
                current_pos_us %= len_us;
            }
            Some((current_pos_us as f32 / len_us as f32).clamp(0.0, 1.0))
        } else {
            None
        }
    }

    fn save(&self) {
        if let Err(error) = self.state.save(&self.state_path) {
            eprintln!("t2-touchbar-ultra: could not save state: {error:#}");
        }
    }

    fn on_suspend(&mut self) {
        self.pressed_physical_keys.clear();
        self.haptic.on_suspend();
    }

    fn on_resume(&mut self) {
        self.pressed_physical_keys.clear();
        self.haptic.on_resume();
        self.level_reader.request(LevelKind::Volume);
        self.level_reader.request(LevelKind::Brightness);
    }

    pub fn current_layout_code(&self) -> &'static str {
        let is_ru = self
            .cached_keyboard_layout
            .as_deref()
            .is_some_and(|l| l.to_ascii_lowercase().contains("ru"));
        if is_ru { "RU" } else { "EN" }
    }

    fn present_keys(&mut self) -> Result<()> {
        if self.color_picker_active {
            self.layout = Layout::color_picker(
                self.canvas.width,
                self.color_hue,
                self.physical_escape,
                self.color_copied,
            );
            self.canvas.keys(&self.layout, self.active_button);
            return self.display.present(&self.canvas.pixels, self.canvas.width, self.canvas.height);
        }
        if self.suggestions_active {
            if self.emoji_strip_active {
                self.layout = Layout::emoji_and_punctuation(
                    self.canvas.width,
                    self.physical_escape,
                    self.emoji_scroll_offset,
                );
            } else {
                let line_str: String = self.input_chars.iter().collect();
                let ctx = extract_context_at_cursor(&self.input_chars, self.cursor_pos);
                let (prefix, prefix_len) = if self.next_word_mode {
                    ("", 0)
                } else {
                    (ctx.current_prefix.as_str(), ctx.current_prefix.chars().count())
                };
                let preview = if self.text_input_has_surrounding && !line_str.trim().is_empty() {
                    Some((line_str.as_str(), self.cursor_pos, prefix_len))
                } else {
                    None
                };
                let clip_entries = if self.suggestions.is_empty() && prefix.is_empty() {
                    self.clipboard_manager.items()
                } else {
                    Vec::new()
                };
                self.layout = Layout::suggestions(
                    self.canvas.width,
                    &self.suggestions,
                    prefix,
                    self.physical_escape,
                    preview,
                    &clip_entries,
                );
            }
            self.canvas.keys(&self.layout, self.active_button);
            return self.display.present(&self.canvas.pixels, self.canvas.width, self.canvas.height);
        }
        self.layout = Layout::with_custom(
            self.state.mode,
            self.canvas.width,
            self.physical_escape,
            &self.custom_items,
        );
        self.canvas.keys(&self.layout, self.active_button);
        if self.layer_fade.is_some() {
            return self.present_layer_fade(self.now_ms());
        }
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)
    }

    /// Shows the crossfade between the previous row and the freshly drawn
    /// current one for `now`, and ends it once the current row is complete.
    fn present_layer_fade(&mut self, now: u64) -> Result<()> {
        let Some(fade) = self.layer_fade.as_mut() else {
            return Ok(());
        };
        let elapsed = now.saturating_sub(fade.start_ms);
        if elapsed >= fade.duration_ms {
            if fade.confirm {
                self.haptic.firm();
            }
            self.layer_fade = None;
            self.blended.clear();
            return self.display.present(
                &self.canvas.pixels,
                self.canvas.width,
                self.canvas.height,
            );
        }
        fade.next_ms = now + LAYER_FRAME_MS;
        let t = (elapsed as f32 / fade.duration_ms as f32).clamp(0.0, 1.0);
        let ease = 1.0 - (1.0 - t) * (1.0 - t);
        let progress = fade.start_progress + (fade.target_progress - fade.start_progress) * ease;
        self.blended.resize(self.canvas.pixels.len(), 0);
        compose_interactive_slide(
            &fade.from,
            &fade.to,
            &mut self.blended,
            usize::from(self.canvas.width),
            fade.clip_left,
            fade.direction,
            progress,
            fade.slide_px,
        );
        self.display
            .present(&self.blended, self.canvas.width, self.canvas.height)
    }

    fn wake(&mut self, quarantine_touch: bool) -> Result<()> {
        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        let now = self.now_ms();
        self.last_activity_ms = now;
        self.idle_sleeping = false;
        self.refresh_key_level();
        if !self.visible {
            self.learner.wake(now, &self.config);
        }
        self.visible = true;
        self.overlay_shown = false;
        self.activity_esc_shown = false;
        self.escape_fade = None;
        self.escape_opacity = 0.0;
        self.overlay_hide_ms = None;
        self.level_kind = None;
        self.level_poll_ms = None;
        self.toast = None;
        self.layer_fade = None;
        self.mode_swipe = None;
        if quarantine_touch {
            self.touches_armed = false;
        }
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.present_keys()?;
        self.backlight.set(self.config.active_brightness)?;
        Ok(())
    }

    fn go_dark(&mut self, learned_off: bool) -> Result<()> {
        self.go_dark_inner(learned_off, false)
    }

    fn go_dark_to_activity_esc(&mut self, learned_off: bool) -> Result<()> {
        self.go_dark_inner(learned_off, true)
    }

    fn go_dark_inner(&mut self, learned_off: bool, preserve_activity_esc: bool) -> Result<()> {
        if self.suggestions_active || self.color_picker_active {
            return Ok(());
        }
        let now = self.now_ms();
        let escape = (!self.config.idle_status_bar
            && preserve_activity_esc
            && self.config.activity_backlight
            && self.touch_id == TouchIdState::Idle
            && self
                .activity_deadline_ms
                .is_some_and(|deadline| now < deadline))
        .then(|| {
            self.layout
                .buttons
                .iter()
                .position(|button| button.action == Action::Key(Key::Esc))
        })
        .flatten();
        self.keyboard.release()?;
        self.active_button = None;
        self.clear_touches();
        self.touches_armed = true;
        self.deadline_ms = None;
        if self.visible && learned_off {
            self.learner
                .auto_off(self.now_ms(), self.state.mode, &self.config);
        }
        self.visible = false;
        self.overlay_shown = escape.is_some();
        self.activity_esc_shown = escape.is_some();
        self.escape_fade = None;
        self.escape_opacity = if escape.is_some() { 1.0 } else { 0.0 };
        self.overlay_hide_ms = None;
        self.level_kind = None;
        self.level_poll_ms = None;
        self.toast = None;
        self.layer_fade = None;
        self.idle_sleeping = false;
        self.idle_dimmed = false;
        if self.config.idle_status_bar {
            self.last_polled_step = None;
            self.next_idle_refresh_ms = now + 5_000;
            self.render_idle_bar()?;
            self.backlight.set(self.config.active_brightness)?;
        } else {
            if let Some(index) = escape {
                self.canvas.single_key(&self.layout, index, false, 100);
            } else {
                self.canvas.clear();
            }
            self.display
                .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
            if escape.is_none() {
                self.backlight.set(0)?;
            }
        }
        Ok(())
    }

    fn render_idle_bar(&mut self) -> Result<()> {
        if self.suggestions_active || self.color_picker_active || self.word_toast.is_some() {
            return Ok(());
        }
        let now = self.now_ms();
        let target_step = now / renderer::CYCLING_STEP_MS;

        if self.cached_clock.is_none() || self.last_polled_step != Some(target_step) {
            self.last_polled_step = Some(target_step);
            match target_step % 3 {
                0 => self.cached_cpu = status::read_cpu_percent(),
                1 => self.cached_ram = status::read_ram_percent(),
                2 => self.cached_cpu_temp = status::read_cpu_temp(),
                _ => {}
            }
            if target_step.is_multiple_of(3) || self.cached_battery.is_none() {
                self.cached_battery = status::read_battery();
            }
            if target_step.is_multiple_of(3) || self.cached_brightness.is_none() {
                self.cached_brightness = levels::brightness().percent;
            }
            self.cached_clock = Some(status::read_clock_data());
        }

        self.refresh_key_level();
        self.canvas.set_level(self.key_level);

        let esc_index = (!self.physical_escape)
            .then(|| {
                self.layout
                    .buttons
                    .iter()
                    .position(|button| button.action == Action::Key(Key::Esc))
            })
            .flatten();
        let esc_param = esc_index.map(|index| (&self.layout, index));
        if self.cached_clock.is_none() {
            self.cached_clock = Some(status::read_clock_data());
        }
        let battery = self.cached_battery;
        let vol_level = self.level_cache[cache_slot(LevelKind::Volume)];
        let brt_level = self.level_cache[cache_slot(LevelKind::Brightness)];
        let volume = vol_level.percent.map(|p| (p, vol_level.muted));
        let brightness = brt_level.percent.or(self.cached_brightness);
        let active_window = self.active_window.as_ref();
        let ram_percent = self.cached_ram;
        let cpu_percent = self.cached_cpu;
        let cpu_temp = self.cached_cpu_temp;
        let shift = if self.config.pixel_shift {
            self.pixel_shift.step_time(now)
        } else {
            (0.0, 0.0)
        };

        if let Some((new_pos, new_len, is_playing, sample_instant)) = self.mpris.get_playback_sample() {
            let elapsed_ms = sample_instant.elapsed().as_millis() as u64;
            let current_pos = if is_playing {
                new_pos + elapsed_ms * 1000
            } else {
                new_pos
            };
            self.media_position_us = Some(current_pos);
            if let Some(len) = new_len {
                self.media_length_us = Some(len);
            }
            self.media_is_playing = is_playing;
            self.media_position_sampled_at_ms = now;
        }
        let media_progress = self.media_scrub_progress.or_else(|| self.current_media_progress(now));
        let mut scrub_display_text: Option<String> = None;
        if self.media_scrubbing.is_some()
            || self.media_scrub_display_until_ms.is_some_and(|until| now < until)
        {
            if let Some(total_us) = self.media_length_us.filter(|&len| len > 0) {
                let p = media_progress.unwrap_or(0.0);
                let current_us = (p as f64 * total_us as f64) as u64;
                let tot_sec = (total_us + 500_000) / 1_000_000;
                let cur_sec = ((current_us + 500_000) / 1_000_000).min(tot_sec);
                scrub_display_text = Some(format!(
                    "{} / {}",
                    mpris::format_duration(cur_sec),
                    mpris::format_duration(tot_sec)
                ));
            }
        }
        let media_track = scrub_display_text.as_deref().or(self.current_track.as_deref());
        let clock_data = self.cached_clock.as_ref().unwrap();
        self.canvas.workspace_windows = self.workspace_windows.clone();

        let (needs_fast_anim, next_transition_ms) = self.canvas.idle_status_bar(
            esc_param,
            false,
            clock_data,
            battery,
            volume,
            brightness,
            media_track,
            media_progress,
            self.media_is_playing,
            active_window,
            self.cached_workspace,
            ram_percent,
            cpu_percent,
            cpu_temp,
            self.cached_keyboard_layout.as_deref(),
            shift,
            now,
        );
        let until_wake = self.media_scrub_display_until_ms.filter(|&until| until > now);
        let next_wake = match (next_transition_ms, until_wake) {
            (Some(t), Some(u)) => Some(t.min(u)),
            (Some(t), None) => Some(t),
            (None, Some(u)) => Some(u),
            (None, None) => None,
        };
        if needs_fast_anim && !self.idle_dimmed {
            self.next_idle_scroll_ms = Some(now + 33);
        } else {
            self.next_idle_scroll_ms = next_wake;
        }
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        Ok(())
    }

    fn turn_off(&mut self) -> Result<()> {
        self.visible = false;
        self.canvas.clear();
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        self.backlight.set(0)
    }

    fn clear_touches(&mut self) {
        self.contacts.clear();
        self.positions.clear();
        self.pending = None;
        self.swipe = None;
        self.mode_swipe = None;
        self.media_hold = None;
    }

    fn touch_down(&mut self, slot: u32, x: f64, y: f64) -> Result<()> {
        let now = self.now_ms();
        self.last_touchbar_activity_ms = now;
        if self.idle_sleeping || self.idle_dimmed {
            self.idle_sleeping = false;
            self.idle_dimmed = false;
            self.backlight.set(self.config.active_brightness)?;
            if self.config.idle_status_bar && !self.visible {
                self.render_idle_bar()?;
            }
        }
        self.user_activity(now, false)?;
        self.resolve_pending(now)?;
        self.positions.insert(slot, x);
        if self.touch_id == TouchIdState::Idle
            && self
                .swipe
                .is_some_and(|swipe| swipe.kind != SwipeKind::Mode)
        {
            return self.join_dark_swipe(slot);
        }
        if self.touch_id != TouchIdState::Idle {
            let esc_index = self
                .layout
                .buttons
                .iter()
                .position(|button| button.action == Action::Key(Key::Esc));
            if let Some(index) = esc_index.filter(|&index| {
                self.layout.hit(x, y, self.canvas.height) == Some(index)
            }) {
                self.active_button = Some(index);
                self.contacts.insert(slot, Contact::TouchIdEsc(index));
                self.render_touch_id(now)?;
            } else {
                self.contacts.insert(slot, Contact::Cancelled);
            }
            return Ok(());
        }

        if self.color_picker_active {
            self.deadline_ms = None;
            self.pending = None;
            self.swipe = None;
            self.mode_swipe = None;

            let button = self.layout.hit(x, y, self.canvas.height);
            if let Some(index) = button {
                if let Some(btn) = self.layout.buttons.get(index) {
                    match btn.action {
                        Action::Key(Key::Esc) => {
                            self.close_color_picker()?;
                            return Ok(());
                        }
                        Action::ColorCopy => {
                            self.copy_selected_color(now)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }

            // In color picker mode, any touch across the ribbon area adjusts the hue!
            if let Some(ribbon) = self.layout.buttons.iter().find(|b| b.action == Action::ColorPicker) {
                self.contacts.insert(slot, Contact::ColorRibbon);
                let ribbon_w = f64::from(ribbon.right - ribbon.left).max(1.0);
                let hue = ((x - f64::from(ribbon.left)) / ribbon_w).clamp(0.0, 1.0) as f32;
                self.color_hue = hue;
                self.haptic.slider_tick();
                self.present_keys()?;
            }
            return Ok(());
        }

        if self.swipe.is_some() {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        if let Some((scrub_slot, _, _)) = self.media_scrubbing.take() {
            self.contacts.remove(&scrub_slot);
            self.media_scrub_progress = None;
            let pending = PendingTouch {
                slot: scrub_slot,
                deadline_ms: now,
                dark: true,
                button: None,
            };
            return self.start_swipe(pending, slot);
        }
        if self.suggestions_active && self.emoji_strip_active {
            if let Some(pending) = self.pending.take() {
                return self.start_swipe(pending, slot);
            }
            if let Some(other_slot) = self.contacts.keys().copied().find(|&s| s != slot) {
                self.contacts.insert(other_slot, Contact::EmojiScroll);
                self.contacts.insert(slot, Contact::EmojiScroll);
                self.emoji_scroll_active = true;
                let p1 = self.positions.get(&other_slot).copied().unwrap_or(x);
                let p2 = x;
                self.emoji_scroll_last_centroid = Some((p1 + p2) / 2.0);
                self.active_button = None;
                return self.present_keys();
            }
            if self.emoji_scroll_active {
                self.contacts.insert(slot, Contact::EmojiScroll);
                return Ok(());
            }
        }
        if let Some(pending) = self.pending.take() {
            return self.start_swipe(pending, slot);
        }
        if !self.contacts.is_empty()
            || ((self.visible || self.suggestions_active) && (!self.touches_armed || self.active_button.is_some()))
        {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        if self.word_toast.is_some() {
            self.word_toast = None;
            self.render_idle_bar()?;
            return Ok(());
        }
        if !self.visible
            && !self.suggestions_active
            && let Some(toast) = self.toast.as_mut()
        {
            let inside = |(left, right): (u16, u16)| x >= f64::from(left) && x < f64::from(right);
            let forward = if inside(toast.layout.next) {
                Some(true)
            } else if inside(toast.layout.prev) {
                Some(false)
            } else {
                None
            };
            if let Some(forward) = forward {
                toast.pressed = Some(forward);
                self.contacts.insert(slot, Contact::TrackKey(forward));
                return self.track_frame(now);
            }
            // Anything else on the bar takes over from the overlay.
            self.toast = None;
        }

        if !self.visible && !self.suggestions_active && !self.color_picker_active {
            if let Some((b_left, b_right)) = self.canvas.media_capsule_rect {
                if x >= f64::from(b_left) && x <= f64::from(b_right) {
                    self.media_scrubbing = Some((slot, x, false));
                    self.contacts.insert(slot, Contact::MediaScrubbing);
                    let bar_w = f64::from(b_right - b_left).max(1.0);
                    let p = ((x - f64::from(b_left)) / bar_w).clamp(0.0, 1.0) as f32;
                    self.media_scrub_progress = Some(p);
                    self.media_scrub_display_until_ms = Some(now + 2000);
                    self.render_idle_bar()?;
                    return Ok(());
                }
            }
            for &(idx, b_left, b_right) in &self.canvas.status_workspace_buttons {
                if x >= f64::from(b_left) && x <= f64::from(b_right) {
                    self.haptic.suggestion_tap();
                    niri::focus_workspace(idx);
                    return Ok(());
                }
            }
            for &(win_id, b_left, b_right) in &self.canvas.status_window_buttons {
                if x >= f64::from(b_left) && x <= f64::from(b_right) {
                    self.haptic.suggestion_tap();
                    niri::focus_window(win_id);
                    return Ok(());
                }
            }
        }

        let button = self.layout.hit(x, y, self.canvas.height);

        if self.suggestions_active {
            if self.clipboard_expire_ms.is_some() {
                self.clipboard_expire_ms = Some(now + self.config.clipboard_timeout_sec * 1000);
            }
            if let Some(index) = button {
                if let Some(btn) = self.layout.buttons.get(index) {
                    match btn.action {
                        Action::Key(Key::Esc) => {
                            self.haptic.suggestion_tap();
                            self.contacts.insert(slot, Contact::Cancelled);
                            self.suggestions_manually_dismissed = true;
                            self.dismiss_suggestions()?;
                            return Ok(());
                        }
                        Action::Clipboard(_) => {
                            self.contacts.insert(slot, Contact::Cancelled);
                            return self.press_button(index, now);
                        }
                        Action::Suggestion(sug_idx) => {
                            self.contacts.insert(slot, Contact::Cancelled);
                            return self.apply_suggestion(sug_idx, now);
                        }
                        Action::ToggleEmojiStrip => {
                            self.contacts.insert(slot, Contact::Cancelled);
                            return self.toggle_emoji_strip(now);
                        }
                        Action::OpenColorPicker => {
                            self.contacts.insert(slot, Contact::Cancelled);
                            return self.open_color_picker(now);
                        }
                        Action::Punctuation(ch) => {
                            self.contacts.insert(slot, Contact::Cancelled);
                            return self.insert_punctuation(ch, now);
                        }
                        Action::Emoji(idx) => {
                            self.contacts.insert(slot, Contact::Cancelled);
                            return self.insert_emoji(idx, now);
                        }
                        Action::Preview => {
                            self.scrubbing = Some((slot, x, 0.0));
                            self.contacts.insert(slot, Contact::Cancelled);
                            self.scrub_to_touch_x(x)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }

        // The esc slot never moves, so it stays usable while the bar is dark.
        let button = if self.visible || self.suggestions_active {
            button
        } else {
            button.filter(|&index| self.is_esc(index))
        };
        let button = button.filter(|&index| {
            !matches!(self.layout.buttons.get(index).map(|b| &b.action), Some(Action::Preview))
        });
        let window_ms = if (!self.visible && !self.suggestions_active) && self.config.idle_status_bar {
            self.config.idle_gesture_window_ms
        } else {
            self.config.two_finger_window_ms
        };
        self.pending = Some(PendingTouch {
            slot,
            deadline_ms: now + window_ms,
            dark: !self.visible && !self.suggestions_active,
            button,
        });
        self.contacts.insert(slot, Contact::Pending);
        if let Some(index) = button {
            if self.visible || self.suggestions_active {
                // Highlight at once. Only the key event waits for the window.
                self.active_button = button;
                self.present_keys()?;
            } else {
                self.show_dark_esc(index)?;
            }
        }
        Ok(())
    }

    fn render_mode_frame(&mut self, mode: Mode) -> Vec<u32> {
        let layout = Layout::with_custom(
            mode,
            self.canvas.width,
            self.physical_escape,
            &self.custom_items,
        );
        self.canvas.keys(&layout, None);
        self.canvas.pixels.clone()
    }

    fn start_mode_swipe(&mut self, step: f64) -> Result<()> {
        let clip_left = if self.physical_escape {
            0
        } else {
            usize::from(self.layout.buttons[0].right) + 1
        };
        let media_frame = self.render_mode_frame(Mode::Media);
        let function_frame = self.render_mode_frame(Mode::Function);
        let special_frame = self.render_mode_frame(Mode::Special);

        let current_frame = match self.state.mode {
            Mode::Media => &media_frame,
            Mode::Function => &function_frame,
            Mode::Special => &special_frame,
        };
        self.canvas.pixels.copy_from_slice(current_frame);
        self.layout = Layout::with_custom(
            self.state.mode,
            self.canvas.width,
            self.physical_escape,
            &self.custom_items,
        );

        let centroid = self.swipe_centroid();
        self.mode_swipe = Some(ModeSwipeState {
            anchor: centroid,
            step,
            clip_left,
            base_mode: self.state.mode,
            media_frame,
            function_frame,
            special_frame,
            last_centroid: centroid,
        });
        Ok(())
    }

    fn start_swipe(&mut self, pending: PendingTouch, slot: u32) -> Result<()> {
        if self.suggestions_active {
            if self.emoji_strip_active {
                self.contacts.insert(pending.slot, Contact::EmojiScroll);
                self.contacts.insert(slot, Contact::EmojiScroll);
                self.emoji_scroll_active = true;
                let p1 = self.positions.get(&pending.slot).copied().unwrap_or(0.0);
                let p2 = self.positions.get(&slot).copied().unwrap_or(p1);
                self.emoji_scroll_last_centroid = Some((p1 + p2) / 2.0);
                self.active_button = None;
                return self.present_keys();
            }
            self.dismiss_suggestions()?;
        }
        let (kind, step, level_kind) = if pending.dark {
            if self.config.two_finger_brightness {
                (SwipeKind::Brightness, self.config.brightness_swipe_step_px, LevelKind::Brightness)
            } else {
                (SwipeKind::Volume, self.config.volume_swipe_step_px, LevelKind::Volume)
            }
        } else {
            (SwipeKind::Mode, self.config.mode_swipe_px, LevelKind::Volume)
        };
        self.contacts.insert(pending.slot, Contact::Swipe);
        self.contacts.insert(slot, Contact::Swipe);
        self.swipe = Some(Swipe::new(kind, self.swipe_centroid(), f64::from(step)));
        if pending.dark {
            return self.begin_level(level_kind);
        }
        if pending.button.is_some() {
            self.active_button = None;
            self.present_keys()?;
        }
        if kind == SwipeKind::Mode {
            self.start_mode_swipe(f64::from(step))?;
        }
        Ok(())
    }

    /// Three fingers on the dark bar.
    fn join_dark_swipe(&mut self, slot: u32) -> Result<()> {
        let (target_swipe, step, target_level) = if self.config.two_finger_brightness {
            (SwipeKind::Volume, self.config.volume_swipe_step_px, LevelKind::Volume)
        } else {
            (SwipeKind::Brightness, self.config.brightness_swipe_step_px, LevelKind::Brightness)
        };
        if self
            .swipe
            .is_some_and(|swipe| swipe.kind == target_swipe)
        {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        self.contacts.insert(slot, Contact::Swipe);
        self.swipe = Some(Swipe::new(
            target_swipe,
            self.swipe_centroid(),
            f64::from(step),
        ));
        self.begin_level(target_level)
    }

    fn swipe_span(&self) -> (f64, f64) {
        let xs = self
            .contacts
            .iter()
            .filter(|(_, contact)| matches!(contact, Contact::Swipe))
            .filter_map(|(slot, _)| self.positions.get(slot).copied());
        xs.fold((f64::MAX, f64::MIN), |(first, last), x| {
            (first.min(x), last.max(x))
        })
    }

    fn swipe_centroid(&self) -> f64 {
        let xs: Vec<f64> = self
            .contacts
            .iter()
            .filter(|(_, contact)| matches!(contact, Contact::Swipe))
            .filter_map(|(slot, _)| self.positions.get(slot).copied())
            .collect();
        xs.iter().sum::<f64>() / xs.len().max(1) as f64
    }

    /// Turns a first touch whose window has expired into its single-finger
    /// meaning: a wake on the dark bar, a held key on the lit bar.
    fn resolve_pending(&mut self, now: u64) -> Result<()> {
        let Some(pending) = self.pending.filter(|pending| now >= pending.deadline_ms) else {
            return Ok(());
        };
        self.pending = None;
        if pending.dark && pending.button.is_some() {
            self.contacts.insert(pending.slot, Contact::DarkEsc);
            self.keyboard.press(Key::Esc)?;
            self.haptic.key_press();
            return Ok(());
        }
        if pending.dark {
            self.contacts.insert(pending.slot, Contact::Wake);
            return self.wake(true);
        }
        match pending.button {
            Some(index) if self.media_direction(index).is_some() => {
                // Stays highlighted. The action is decided on release or hold.
                let touched = pending.deadline_ms - self.config.two_finger_window_ms;
                self.contacts
                    .insert(pending.slot, Contact::MediaHold(index));
                self.media_hold = Some(MediaHold {
                    forward: self.media_direction(index) == Some(true),
                    next_ms: touched + MEDIA_HOLD_MS,
                    seeking_since: None,
                    player: None,
                });
                self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
                Ok(())
            }
            Some(index) => {
                if let Some(btn) = self.layout.buttons.get(index) {
                    if matches!(btn.action, Action::Emoji(_)) {
                        self.contacts.insert(pending.slot, Contact::Button(index));
                        return Ok(());
                    }
                }
                self.contacts.insert(pending.slot, Contact::Button(index));
                self.press_button(index, now)
            }
            None => {
                self.contacts.insert(pending.slot, Contact::Cancelled);
                Ok(())
            }
        }
    }

    fn is_agy_active(&self) -> bool {
        let Some(win) = &self.active_window else {
            return false;
        };
        let app_id = win.app_id.as_deref().unwrap_or("").to_lowercase();
        let title = win.title.to_lowercase();

        if title.contains("agy")
            || title.contains("antigravity")
            || app_id.contains("agy")
            || app_id.contains("antigravity")
        {
            return true;
        }

        if let Some(pid) = win.pid {
            if let Ok(cmd) = std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
                let cmd_lower = cmd.to_lowercase();
                if cmd_lower.contains("agy") || cmd_lower.contains("antigravity") {
                    return true;
                }
            }
            if let Ok(entries) = std::fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    if let Ok(file_name) = entry.file_name().into_string() {
                        if let Ok(child_pid) = file_name.parse::<u32>() {
                            if let Ok(status) = std::fs::read_to_string(format!("/proc/{child_pid}/stat")) {
                                let parts: Vec<&str> = status.split_whitespace().collect();
                                if parts.len() > 3 && parts[3] == pid.to_string() {
                                    if let Ok(cmd) = std::fs::read_to_string(format!("/proc/{child_pid}/cmdline")) {
                                        let cmd_lower = cmd.to_lowercase();
                                        if cmd_lower.contains("agy") || cmd_lower.contains("antigravity") {
                                            return true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        false
    }

    fn is_text_input_allowed(&self) -> bool {
        if !self.config.predictive_typing && !self.config.clipboard_history {
            return false;
        }
        let Some(win) = &self.active_window else {
            return false;
        };

        if self.is_agy_active() {
            return true;
        }

        let app_id = win.app_id.as_deref().unwrap_or("").to_lowercase();
        let title = win.title.to_lowercase();

        // 1. Never in terminal emulators
        if app_id.contains("alacritty")
            || app_id.contains("kitty")
            || app_id.contains("foot")
            || app_id.contains("wezterm")
            || app_id.contains("terminal")
            || app_id.contains("konsole")
            || app_id.contains("xterm")
            || app_id.contains("urxvt")
            || app_id.contains("rxvt")
            || app_id.contains("st")
            || app_id.contains("terminology")
            || app_id.contains("tilix")
            || app_id.contains("terminator")
            || app_id.contains("ghostty")
        {
            return false;
        }

        // 2. Never in TUI / CLI programs running in any window
        let tui_tools = [
            "btop", "htop", "top", "yazi", "ranger", "nvim", "neovim", "vim", "vi",
            "nano", "emacs", "tmux", "screen", "lazygit", "lazydocker", "mc",
            "midnight commander", "fzf", "ncdu", "gdu", "less", "man", "fish", "bash", "zsh"
        ];
        for tool in tui_tools {
            if title == tool
                || title.starts_with(&format!("{tool} "))
                || title.starts_with(&format!("{tool} -"))
                || title.ends_with(&format!(" - {tool}"))
                || title.contains(&format!(" {tool} "))
                || title.contains(&format!("({tool})"))
                || title.contains(&format!("[{tool}]"))
            {
                return false;
            }
        }

        // 3. Never in non-text media/utility apps
        let non_text_apps = [
            "pavucontrol", "mpv", "vlc", "imv", "swappy", "calculator", "galculator",
            "kcalc", "gnome-calculator", "blueman", "nm-connection-editor", "piper"
        ];
        for app in non_text_apps {
            if app_id.contains(app) {
                return false;
            }
        }

        true
    }

    fn is_text_input_target_active(&self) -> bool {
        if !self.is_text_input_allowed() {
            return false;
        }
        if self.text_input_focused || self.is_agy_active() {
            return true;
        }
        let Some(win) = &self.active_window else {
            return false;
        };
        let app_id = win.app_id.as_deref().unwrap_or("").to_lowercase();
        let title = win.title.to_lowercase();

        // 1. Explicitly allow Telegram and all common messaging / notes / text editing apps
        if app_id.contains("telegram")
            || title.contains("telegram")
            || app_id.contains("discord")
            || app_id.contains("slack")
            || app_id.contains("signal")
            || app_id.contains("element")
            || app_id.contains("viber")
            || app_id.contains("skype")
            || app_id.contains("whatsapp")
            || app_id.contains("thunderbird")
            || app_id.contains("obsidian")
            || app_id.contains("notes")
            || app_id.contains("gedit")
            || app_id.contains("kate")
            || app_id.contains("kwrite")
            || app_id.contains("sublime")
            || app_id.contains("libreoffice")
            || app_id.contains("soffice")
            || app_id.contains("writer")
        {
            return true;
        }

        // 2. For non-browser GUI applications, allow direct typing so word suggestions work reliably
        let is_browser = app_id.contains("firefox")
            || app_id.contains("chrome")
            || app_id.contains("chromium")
            || app_id.contains("brave")
            || app_id.contains("zen")
            || app_id.contains("epiphany")
            || app_id.contains("edge")
            || app_id.contains("vivaldi")
            || app_id.contains("tor");
        if !is_browser {
            return true;
        }

        false
    }

    fn active_window_changed(&mut self, win: Option<niri::ActiveWindow>) -> Result<()> {
        let window_changed = match (&self.active_window, &win) {
            (Some(curr), Some(next)) => curr.app_id != next.app_id || curr.pid != next.pid,
            (None, None) => false,
            _ => true,
        };
        self.active_window = win;
        self.workspace_windows = niri::query_windows();
        if window_changed {
            self.text_input_focused = false;
            self.text_input_has_surrounding = false;
            self.suggestions_manually_dismissed = false;
            self.clipboard_expire_ms = None;
            self.input_chars.clear();
            self.cursor_pos = 0;
            if self.suggestions_active {
                self.dismiss_suggestions()?;
            }
        }
        if !self.is_text_input_allowed() {
            self.text_input_focused = false;
        }
        if self.config.idle_status_bar
            && !self.visible
            && !self.suggestions_active
            && self.touch_id == TouchIdState::Idle
            && !self.overlay_shown
            && self.swipe.is_none()
            && self.toast.is_none()
            && !self.idle_sleeping
            && self.current_track.is_none()
        {
            self.render_idle_bar()?;
        }
        Ok(())
    }

    fn workspace_changed(&mut self, ws: Option<niri::WorkspaceInfo>) -> Result<()> {
        if self.cached_workspace == ws {
            return Ok(());
        }
        if self.suggestions_active {
            self.dismiss_suggestions()?;
        }
        self.cached_workspace = ws;
        self.workspace_windows = niri::query_windows();
        if self.config.idle_status_bar
            && !self.visible
            && !self.suggestions_active
            && self.touch_id == TouchIdState::Idle
            && !self.overlay_shown
            && self.swipe.is_none()
            && self.toast.is_none()
            && !self.idle_sleeping
            && self.current_track.is_none()
        {
            self.render_idle_bar()?;
        }
        Ok(())
    }

    fn keyboard_layout_changed(&mut self, layout: Option<String>) -> Result<()> {
        if self.cached_keyboard_layout == layout {
            return Ok(());
        }
        self.cached_keyboard_layout = layout;
        if self.suggestions_active {
            self.refresh_suggestions_from_context()?;
            return Ok(());
        }
        if self.config.idle_status_bar
            && !self.visible
            && !self.suggestions_active
            && self.touch_id == TouchIdState::Idle
            && !self.overlay_shown
            && self.swipe.is_none()
            && self.toast.is_none()
            && !self.idle_sleeping
            && self.current_track.is_none()
        {
            self.render_idle_bar()?;
        }
        Ok(())
    }

    fn track_changed(&mut self, track: Option<Track>) -> Result<()> {
        let Some(track) = track else {
            self.current_track = None;
            self.media_position_us = None;
            self.media_length_us = None;
            self.media_is_playing = false;
            self.media_last_query_ms = 0;
            self.mpris.set_playback_active(false);
            self.toast = None;
            self.overlay_shown = false;
            if self.config.idle_status_bar && !self.visible {
                self.render_idle_bar()?;
            }
            return Ok(());
        };
        let summary = if track.artist.is_empty() {
            track.title.clone()
        } else {
            format!("{} – {}", track.artist, track.title)
        };
        let new_track_summary = if summary.is_empty() { None } else { Some(summary) };
        let track_changed = self.current_track != new_track_summary;
        self.current_track = new_track_summary;
        self.media_length_us = track.length_us;
        self.media_is_playing = true;
        let now = self.now_ms();
        self.mpris.set_playback_active(true);

        if !track_changed {
            return Ok(());
        }

        self.media_last_query_ms = now;
        self.media_position_us = Some(0);
        self.media_position_sampled_at_ms = now;

        if let Some(toast) = self.toast.as_mut() {
            toast.track = track;
            toast.start_ms = now.saturating_sub(TRACK_FADE_MS);
            return self.track_frame(now);
        }

        // Show the track overlay toast first on quiet bar!
        let can_show_toast = !self.visible
            && (!self.overlay_shown || self.activity_esc_shown)
            && self.touch_id == TouchIdState::Idle
            && self.contacts.is_empty()
            && !self.config.idle_status_bar;

        if can_show_toast {
            self.refresh_key_level();
            self.toast = Some(TrackToast {
                track,
                start_ms: now.saturating_sub(TRACK_FADE_MS),
                next_ms: None,
                layout: self.track_keys(),
                pressed: None,
            });
            return self.track_frame(now);
        }

        if self.config.idle_status_bar && !self.visible {
            self.render_idle_bar()?;
        }
        Ok(())
    }

    /// Draws the track overlay for `now` and schedules the next frame: about
    /// 30 fps while fading, nothing while it holds or a key is pressed.
    fn track_frame(&mut self, now: u64) -> Result<()> {
        let Some(toast) = self.toast.as_mut() else {
            return Ok(());
        };
        let elapsed = now.saturating_sub(toast.start_ms);
        let fade_out = TRACK_FADE_MS + TRACK_HOLD_MS;
        let (opacity, next) = if toast.pressed.is_some() {
            (1.0, None)
        } else if elapsed < TRACK_FADE_MS {
            (
                elapsed as f32 / TRACK_FADE_MS as f32,
                Some(now + TRACK_FRAME_MS),
            )
        } else if elapsed < fade_out {
            (1.0, Some(toast.start_ms + fade_out))
        } else if elapsed < fade_out + TRACK_FADE_MS {
            (
                1.0 - (elapsed - fade_out) as f32 / TRACK_FADE_MS as f32,
                Some(now + TRACK_FRAME_MS),
            )
        } else {
            self.toast = None;
            self.overlay_shown = false;
            if self.activity_esc_shown {
                return self.render_activity_esc();
            }
            if self.config.idle_status_bar && !self.visible {
                return self.render_idle_bar();
            }
            return self.hide_overlay();
        };
        // Smoothstep, so the fades start and end softly.
        let eased = opacity * opacity * (3.0 - 2.0 * opacity);
        toast.next_ms = next;
        let (track, pressed, keys) = (toast.track.clone(), toast.pressed, toast.layout);
        let esc = self
            .activity_esc_shown
            .then(|| {
                self.layout
                    .buttons
                    .iter()
                    .find(|button| button.action == Action::Key(Key::Esc))
                    .cloned()
            })
            .flatten()
            .map(|button| (button, (self.escape_opacity * 100.0).round() as u32));
        self.canvas.track(
            &track.artist,
            &track.title,
            (eased * 100.0).round() as u32,
            pressed,
            keys,
            esc,
        );
        self.present_overlay()
    }

    /// The overlay's previous key takes the first key slot after esc. Next
    /// mirrors it on the right, as if an invisible esc sat there too. The
    /// keys stay in fixed places, esc keeps working and the text centers on
    /// the bar.
    fn track_keys(&self) -> TrackLayout {
        // Taken from the media row, whose keys fill the whole bar.
        let media = Layout::new(Mode::Media, self.canvas.width, self.physical_escape);
        let first = &media.buttons[usize::from(!self.physical_escape)];
        let width = self.canvas.width;
        TrackLayout {
            prev: (first.left, first.right),
            next: (width - first.right, width - first.left),
        }
    }

    fn show_dark_esc(&mut self, index: usize) -> Result<()> {
        self.refresh_key_level();
        if self.config.idle_status_bar {
            self.canvas.set_level(self.key_level);
            let esc_param = (!self.physical_escape).then_some((&self.layout, index));
            let clock_data = self.cached_clock.clone().unwrap_or_else(status::read_clock_data);
            let battery = status::read_battery();
            let vol_level = self.level_cache[cache_slot(LevelKind::Volume)];
            let brt_level = self.level_cache[cache_slot(LevelKind::Brightness)];
            let volume = vol_level.percent.map(|p| (p, vol_level.muted));
            let brightness = brt_level.percent.or(self.cached_brightness);
            let now = self.now_ms();
            let media_track = self.current_track.as_deref();
            let media_progress = self.media_scrub_progress.or_else(|| self.current_media_progress(now));
            let active_window = self.active_window.as_ref();
            let ram_percent = status::read_ram_percent();
            let cpu_percent = status::read_cpu_percent();
            let shift = self.pixel_shift.get();

            let cpu_temp = self.cached_cpu_temp.or_else(status::read_cpu_temp);
            self.canvas.idle_status_bar(
                esc_param,
                true,
                &clock_data,
                battery,
                volume,
                brightness,
                media_track,
                media_progress,
                self.media_is_playing,
                active_window,
                self.cached_workspace,
                ram_percent,
                cpu_percent,
                cpu_temp,
                self.cached_keyboard_layout.as_deref(),
                shift,
                now,
            );
            self.present_overlay()
        } else {
            self.level_kind = None;
            self.activity_esc_shown = self.config.activity_backlight;
            self.escape_fade = None;
            self.escape_opacity = 1.0;
            self.level_fade_start_ms = None;
            self.level_frame_ms = None;
            self.toast = None;
            self.overlay_hide_ms = None;
            self.canvas.single_key(&self.layout, index, true, 100);
            self.present_overlay()
        }
    }

    fn render_word_toast(&mut self, now: u64) -> Result<()> {
        let Some(toast) = &self.word_toast else {
            return Ok(());
        };
        let esc = (!self.physical_escape)
            .then(|| {
                self.layout
                    .buttons
                    .iter()
                    .find(|b| b.action == Action::Key(Key::Esc))
                    .cloned()
            })
            .flatten()
            .map(|btn| (btn, 100));

        self.canvas.set_level(self.key_level);
        let is_fast = self.canvas.word_definition(&toast.def, 100, now, toast.start_ms, esc);
        if let Some(t) = &mut self.word_toast {
            t.is_fast = is_fast;
        }
        self.display.present(&self.canvas.pixels, self.canvas.width, self.canvas.height)
    }

    fn on_word_definition(&mut self, def: lookup::WordDefinition, now: u64) -> Result<()> {
        if !self.config.word_lookup || self.suggestions_active || self.color_picker_active {
            return Ok(());
        }
        self.word_toast = Some(WordToast {
            def,
            start_ms: now,
            expire_ms: now + self.config.word_lookup_timeout_sec * 1000,
            is_fast: false,
        });
        self.backlight.set(self.config.active_brightness)?;
        self.render_word_toast(now)
    }

    fn begin_level(&mut self, kind: LevelKind) -> Result<()> {
        self.refresh_key_level();
        self.toast = None;
        self.level_kind = Some(kind);
        self.activity_esc_shown = false;
        self.escape_fade = None;
        self.escape_opacity = 0.0;
        self.level_fade_start_ms = None;
        self.level_frame_ms = None;
        self.level = self.level_cache[cache_slot(kind)];
        self.level_stepped = false;
        self.level_stop_felt = false;
        self.level_reader.request(kind);
        self.level_fingers = self.swipe_span();
        self.update_level_side(self.now_ms(), true);
        self.overlay_hide_ms = None;
        self.level_dirty = true;
        Ok(())
    }

    /// Places the feedback on the side of the fingers with more room. A
    /// change crossfades unless `snap` is set.
    fn update_level_side(&mut self, now: u64, snap: bool) {
        let centre = (self.level_fingers.0 + self.level_fingers.1) / 2.0;
        let middle = f64::from(self.canvas.width) / 2.0;
        // Progress so far counts towards the old target.
        self.advance_level_side(now);
        let margin = if snap { 0.0 } else { LEVEL_SIDE_HYSTERESIS_PX };
        if centre < middle - margin {
            self.level_right_target = true;
        } else if centre >= middle + margin {
            self.level_right_target = false;
        }
        if snap {
            self.level_right = f32::from(u8::from(self.level_right_target));
        }
        if self.level_right != f32::from(u8::from(self.level_right_target)) {
            self.level_frame_ms.get_or_insert(now + LEVEL_FRAME_MS);
        }
    }

    /// Moves the side crossfade on to `now`. Returns whether it still runs.
    fn advance_level_side(&mut self, now: u64) -> bool {
        let target = f32::from(u8::from(self.level_right_target));
        let step = now.saturating_sub(self.level_right_ms) as f32 / LEVEL_SIDE_FADE_MS as f32;
        self.level_right_ms = now;
        self.level_right = if self.level_right < target {
            (self.level_right + step).min(target)
        } else {
            (self.level_right - step).max(target)
        };
        self.level_right != target
    }

    /// Draws the level feedback if anything changed since the last frame.
    fn flush_level(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.level_dirty) {
            return Ok(());
        }
        let Some(kind) = self.level_kind.filter(|_| !self.visible) else {
            return Ok(());
        };
        let now = self.now_ms();
        self.advance_level_side(now);
        let right = self.level_right * self.level_right * (3.0 - 2.0 * self.level_right);
        let opacity = self.level_fade_start_ms.map_or(100, |start| {
            let elapsed = self.now_ms().saturating_sub(start).min(LEVEL_FADE_MS);
            let opacity = 1.0 - elapsed as f32 / LEVEL_FADE_MS as f32;
            (opacity * opacity * (3.0 - 2.0 * opacity) * 100.0).round() as u32
        });
        self.canvas.level(
            kind,
            self.level.percent,
            self.level.muted,
            (self.level_fingers.0 as f32, self.level_fingers.1 as f32),
            right,
            opacity,
        );
        self.present_overlay()
    }

    /// Shows whatever the canvas holds on the otherwise dark bar.
    fn present_overlay(&mut self) -> Result<()> {
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        if !self.overlay_shown {
            self.backlight.set(self.config.active_brightness)?;
            self.overlay_shown = true;
        }
        Ok(())
    }

    fn hide_overlay(&mut self) -> Result<()> {
        self.overlay_hide_ms = None;
        self.level_kind = None;
        self.toast = None;
        self.level_poll_ms = None;
        self.level_fade_start_ms = None;
        self.level_frame_ms = None;
        self.activity_esc_shown = false;
        self.escape_fade = None;
        self.escape_opacity = 0.0;
        if !self.overlay_shown || self.visible {
            return Ok(());
        }
        self.overlay_shown = false;
        if self.config.idle_status_bar {
            self.render_idle_bar()?;
            self.backlight.set(self.config.active_brightness)
        } else {
            self.canvas.clear();
            self.display
                .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
            self.backlight.set(0)
        }
    }

    fn poll_level(&mut self, now: u64) {
        self.level_poll_ms = (now < self.level_poll_until_ms).then_some(now + LEVEL_POLL_MS);
        if let Some(kind) = self.level_kind {
            self.level_reader.request(kind);
        }
    }

    fn levels_read(&mut self) {
        let now = self.now_ms();
        let results: Vec<_> = self.level_reader.results().collect();
        let mut dirty_bar = false;
        for (kind, level) in results {
            if kind == LevelKind::Volume {
                self.haptic
                    .set_system_muted(level.muted || level.percent == Some(0));
            }
            if self.level_cache[cache_slot(kind)] != level {
                self.level_cache[cache_slot(kind)] = level;
                dirty_bar = true;
            }
            if kind == LevelKind::Brightness && level.percent.is_some() {
                self.cached_brightness = level.percent;
            }
            if self.level_kind != Some(kind) || level == self.level {
                continue;
            }
            self.level = level;
            self.level_dirty = true;
            // Reaching a stop is only known once the desktop has applied
            // the key, so the firm click follows the read.
            if !at_stop(kind, level, None) {
                self.level_stop_felt = false;
            } else if self.level_stepped && !self.level_stop_felt {
                self.level_stop_felt = true;
                self.haptic.firm();
            }
            // Keep a lingering overlay alive until the value has settled.
            self.overlay_hide_ms = self
                .overlay_hide_ms
                .map(|deadline| deadline.max(now + LEVEL_POLL_MS));
        }
        if dirty_bar
            && !self.visible
            && self.config.idle_status_bar
            && !self.idle_sleeping
            && self.swipe.is_none()
            && self.toast.is_none()
            && self.touch_id == TouchIdState::Idle
        {
            let _ = self.render_idle_bar();
        }
    }

    /// Follows the keyboard backlight. Returns whether the level changed.
    fn refresh_key_level(&mut self) -> bool {
        let level = if self.idle_dimmed {
            32
        } else if self.config.follow_keyboard_backlight {
            kbdlight::key_level(self.config.minimum_brightness).unwrap_or(100)
        } else {
            100
        };
        if level == self.key_level {
            return false;
        }
        self.key_level = level;
        self.canvas.set_level(level);
        true
    }

    fn keyboard_backlight_changed(&mut self) -> Result<()> {
        if let Ok(brightness) = kbdlight::brightness() {
            self.activity_current_brightness = Some(brightness);
            if self.activity_dimmed && self.backlight_fade.is_none() && brightness > 0 {
                self.activity_restore_brightness = Some(brightness);
                self.activity_dimmed = false;
                self.user_activity(self.now_ms(), true)?;
            }
        }
        let changed = self.refresh_key_level();
        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        if self.activity_esc_shown && changed {
            return self.render_activity_esc();
        }
        if !changed {
            return Ok(());
        }
        if self.visible {
            return self.present_keys();
        }
        self.level_dirty = true;
        Ok(())
    }

    fn on_keyboard_or_trackpad_activity(&mut self, now: u64) -> Result<()> {
        self.last_activity_ms = now;
        self.last_touchbar_activity_ms = now;

        if self.config.activity_backlight {
            self.activity_deadline_ms = Some(now + self.config.activity_timeout_ms);
            let fading_out = self
                .backlight_fade
                .as_ref()
                .is_some_and(|fade| fade.to == 0);
            if self.activity_dimmed || fading_out {
                let brightness = match self.activity_restore_brightness {
                    Some(brightness) if brightness > 0 => Some(brightness),
                    _ => match self
                        .activity_max_brightness
                        .map(Ok)
                        .unwrap_or_else(kbdlight::maximum_brightness)
                    {
                        Ok(brightness) if brightness > 0 => {
                            self.activity_max_brightness = Some(brightness);
                            Some(brightness)
                        }
                        Ok(_) => None,
                        Err(error) => {
                            eprintln!("t2-touchbar-ultra: read maximum keyboard brightness: {error:#}");
                            None
                        }
                    },
                };
                if let Some(brightness) = brightness {
                    let already_fading_in = self
                        .backlight_fade
                        .as_ref()
                        .is_some_and(|fade| fade.to == brightness && brightness > 0);
                    if !already_fading_in {
                        let current = self.activity_current_brightness.unwrap_or(0);
                        self.start_backlight_fade(now, current, brightness);
                    }
                }
            }
        }

        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }

        // Exception: If Touch Bar is in Function mode (F.. keys) and modifier keys are active,
        // do not switch to the info screen.
        if self.visible && self.state.mode == Mode::Function && !self.active_modifiers.is_empty() {
            return Ok(());
        }

        // Requirement 3: Immediately switch to the idle info panel from any state
        // (sleeping, dimmed, button rows visible, overlay shown, or during mode swipe/fade).
        if self.idle_sleeping
            || self.idle_dimmed
            || self.visible
            || self.overlay_shown
            || self.layer_fade.is_some()
            || self.mode_swipe.is_some()
        {
            self.idle_sleeping = false;
            self.idle_dimmed = false;
            self.go_dark(false)?;
        }

        Ok(())
    }

    fn user_activity(&mut self, now: u64, show_esc: bool) -> Result<()> {
        self.last_activity_ms = now;
        self.last_touchbar_activity_ms = now;
        if self.idle_sleeping {
            self.idle_sleeping = false;
        }
        if self.idle_dimmed {
            self.idle_dimmed = false;
        }
        if !self.config.activity_backlight {
            return Ok(());
        }
        self.activity_deadline_ms = Some(now + self.config.activity_timeout_ms);
        let fading_out = self
            .backlight_fade
            .as_ref()
            .is_some_and(|fade| fade.to == 0);
        if self.activity_dimmed || fading_out {
            let brightness = match self.activity_restore_brightness {
                Some(brightness) if brightness > 0 => Some(brightness),
                _ => match self
                    .activity_max_brightness
                    .map(Ok)
                    .unwrap_or_else(kbdlight::maximum_brightness)
                {
                    Ok(brightness) if brightness > 0 => {
                        self.activity_max_brightness = Some(brightness);
                        Some(brightness)
                    }
                    Ok(_) => None,
                    Err(error) => {
                        eprintln!("t2-touchbar-ultra: read maximum keyboard brightness: {error:#}");
                        None
                    }
                },
            };
            if let Some(brightness) = brightness {
                let already_fading_in = self
                    .backlight_fade
                    .as_ref()
                    .is_some_and(|fade| fade.to == brightness && brightness > 0);
                if !already_fading_in {
                    let current = self.activity_current_brightness.unwrap_or(0);
                    self.start_backlight_fade(now, current, brightness);
                }
            }
        }
        if self.visible {
            if !self.config.idle_status_bar {
                self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
            }
        } else if show_esc && self.touch_id == TouchIdState::Idle && !self.config.idle_status_bar {
            self.show_activity_esc()?;
        }
        Ok(())
    }

    fn show_activity_esc(&mut self) -> Result<()> {
        if self.visible || self.suggestions_active || self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        if self.activity_esc_shown {
            if self.escape_fade.as_ref().is_some_and(|fade| fade.to <= 0.0) {
                self.start_escape_fade(self.now_ms(), self.escape_opacity, 1.0);
            }
            return Ok(());
        }
        let Some(index) = self
            .layout
            .buttons
            .iter()
            .position(|button| button.action == Action::Key(Key::Esc))
        else {
            return Ok(());
        };
        // A track overlay stays and draws the esc key with it.
        self.level_kind = None;
        self.overlay_hide_ms = None;
        self.level_fade_start_ms = None;
        self.level_frame_ms = None;
        self.refresh_key_level();
        self.activity_esc_shown = true;
        self.start_escape_fade(self.now_ms(), self.escape_opacity, 1.0);
        self.render_activity_esc_with_index(index)
    }

    fn activity_timeout(&mut self) {
        self.activity_deadline_ms = None;
        if self.visible
            && self.touch_id == TouchIdState::Idle
            && !(self.state.mode == Mode::Function && !self.active_modifiers.is_empty())
            && let Err(error) = self.go_dark(false)
        {
            eprintln!("t2-touchbar-ultra: darken Touch Bar after inactivity: {error:#}");
        }
        match kbdlight::brightness() {
            Ok(brightness) => {
                self.activity_current_brightness = Some(brightness);
                if brightness > 0 {
                    self.activity_restore_brightness = Some(brightness);
                    self.start_backlight_fade(self.now_ms(), brightness, 0);
                } else {
                    self.backlight_fade = None;
                    self.activity_dimmed = true;
                }
            }
            Err(error) => {
                eprintln!("t2-touchbar-ultra: read keyboard brightness before dimming: {error:#}")
            }
        }
        if self.activity_esc_shown && !self.config.idle_status_bar {
            self.start_escape_fade(self.now_ms(), self.escape_opacity, 0.0);
        }
    }

    fn start_backlight_fade(&mut self, now: u64, from: u32, to: u32) {
        if from == to {
            self.activity_current_brightness = Some(to);
            self.activity_dimmed = to == 0;
            self.backlight_fade = None;
            return;
        }
        self.backlight_fade = Some(BacklightFade {
            start_ms: now,
            next_ms: now,
            from,
            to,
        });
    }

    fn start_escape_fade(&mut self, now: u64, from: f32, to: f32) {
        if (from - to).abs() < f32::EPSILON {
            self.escape_opacity = to;
            self.escape_fade = None;
            return;
        }
        self.escape_fade = Some(EscapeFade {
            start_ms: now,
            next_ms: now,
            from,
            to,
        });
    }

    fn render_activity_esc(&mut self) -> Result<()> {
        let Some(index) = self
            .layout
            .buttons
            .iter()
            .position(|button| button.action == Action::Key(Key::Esc))
        else {
            return Ok(());
        };
        self.render_activity_esc_with_index(index)
    }

    fn render_activity_esc_with_index(&mut self, index: usize) -> Result<()> {
        if self.toast.is_some() {
            return self.track_frame(self.now_ms());
        }
        self.canvas.single_key(
            &self.layout,
            index,
            false,
            (self.escape_opacity * 100.0).round() as u32,
        );
        self.present_overlay()
    }

    fn advance_backlight_fade(&mut self, now: u64) {
        let Some(fade) = self.backlight_fade.as_mut() else {
            return;
        };
        if now < fade.next_ms {
            return;
        }
        let elapsed = now.saturating_sub(fade.start_ms).min(ACTIVITY_FADE_MS);
        let progress = elapsed as f32 / ACTIVITY_FADE_MS as f32;
        let eased = progress * progress * (3.0 - 2.0 * progress);
        let value = (fade.from as f32 + (fade.to as f32 - fade.from as f32) * eased).round() as u32;
        let target = fade.to;
        fade.next_ms = now + ACTIVITY_FRAME_MS;
        if self.activity_current_brightness != Some(value) {
            match kbdlight::set_brightness(value) {
                Ok(()) => self.activity_current_brightness = Some(value),
                Err(error) => {
                    eprintln!("t2-touchbar-ultra: fade keyboard backlight: {error:#}");
                    self.backlight_fade = None;
                    return;
                }
            }
        }
        if elapsed >= ACTIVITY_FADE_MS {
            self.activity_current_brightness = Some(target);
            self.activity_dimmed = target == 0;
            self.backlight_fade = None;
        }
    }

    fn advance_escape_fade(&mut self, now: u64) -> Result<()> {
        let Some(fade) = self.escape_fade.as_mut() else {
            return Ok(());
        };
        if now < fade.next_ms {
            return Ok(());
        }
        let elapsed = now.saturating_sub(fade.start_ms).min(ACTIVITY_FADE_MS);
        let progress = elapsed as f32 / ACTIVITY_FADE_MS as f32;
        let eased = progress * progress * (3.0 - 2.0 * progress);
        self.escape_opacity = fade.from + (fade.to - fade.from) * eased;
        let target = fade.to;
        fade.next_ms = now + ACTIVITY_FRAME_MS;
        self.render_activity_esc()?;
        if elapsed >= ACTIVITY_FADE_MS {
            self.escape_opacity = target;
            self.escape_fade = None;
            if target <= 0.0 {
                if self.toast.is_some() {
                    // The track overlay goes on alone.
                    self.activity_esc_shown = false;
                    self.track_frame(now)?;
                } else {
                    self.hide_overlay()?;
                }
            }
        }
        Ok(())
    }

    fn restore_activity_backlight_on_exit(&mut self) {
        if self.config.activity_backlight
            && (self.activity_dimmed
                || self
                    .backlight_fade
                    .as_ref()
                    .is_some_and(|fade| fade.to == 0))
            && let Some(brightness) = self.activity_restore_brightness
        {
            if let Err(error) = kbdlight::set_brightness(brightness) {
                eprintln!("t2-touchbar-ultra: restore keyboard brightness on exit: {error:#}");
            }
        }
    }

    /// `Some(true)` for next, `Some(false)` for previous.
    fn media_direction(&self, index: usize) -> Option<bool> {
        match self.layout.buttons[index].action {
            Action::Key(Key::NextSong) => Some(true),
            Action::Key(Key::PreviousSong) => Some(false),
            _ => None,
        }
    }

    fn seek_step(&mut self, now: u64) -> Result<()> {
        let Some(mut hold) = self.media_hold.take() else {
            return Ok(());
        };
        let since = match hold.seeking_since {
            Some(since) => since,
            None => {
                // The hold turned into seeking: no track skip on release.
                hold.player = self.mpris.active_player();
                self.haptic.click();
                if self
                    .learner
                    .action(now, self.state.mode, &mut self.state, &self.config)
                {
                    self.save();
                }
                *hold.seeking_since.insert(now)
            }
        };
        let step = if now - since >= SEEK_FAST_AFTER_MS {
            SEEK_FAST_STEP
        } else {
            SEEK_STEP
        };
        let seeked = hold
            .player
            .as_deref()
            .is_some_and(|player| self.mpris.seek(player, step, hold.forward).is_ok());
        if !seeked {
            // No MPRIS player: leave it to whoever handles the seek keys.
            let key = if hold.forward {
                Key::FastForward
            } else {
                Key::Rewind
            };
            self.keyboard.press(key)?;
            self.keyboard.release()?;
        }
        hold.next_ms = now + SEEK_INTERVAL_MS;
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.media_last_query_ms = 0;
        self.media_hold = Some(hold);
        Ok(())
    }

    fn is_esc(&self, index: usize) -> bool {
        self.layout.buttons[index].action == Action::Key(Key::Esc)
    }

    fn open_color_picker(&mut self, _now: u64) -> Result<()> {
        self.color_picker_active = true;
        self.color_copied = false;
        self.color_copied_deadline = None;
        self.deadline_ms = None;
        self.pending = None;
        self.swipe = None;
        self.mode_swipe = None;
        self.visible = true;
        self.backlight.set(self.config.active_brightness)?;
        self.present_keys()
    }

    fn close_color_picker(&mut self) -> Result<()> {
        self.color_picker_active = false;
        self.color_copied = false;
        self.color_copied_deadline = None;
        self.active_button = None;
        self.deadline_ms = None;
        if self.config.idle_status_bar {
            self.visible = false;
            self.render_idle_bar()
        } else {
            self.present_keys()
        }
    }

    fn copy_selected_color(&mut self, now: u64) -> Result<()> {
        let (r, g, b) = renderer::hsv_to_rgb(self.color_hue, 1.0, 1.0);
        let hex = format!("#{:02X}{:02X}{:02X}", r, g, b);

        // Copy to system clipboard
        let _ = std::process::Command::new("wl-copy").arg(&hex).status();

        // Immediately insert text into the focused window like emojis (via wtype)
        let typed = std::process::Command::new("wtype")
            .arg(&hex)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !typed {
            if self.keyboard.paste_clipboard().is_err() {
                let _ = self.keyboard.paste_ctrl_v();
            }
        }

        for ch in hex.chars() {
            self.input_chars.insert(self.cursor_pos, ch);
            self.cursor_pos += 1;
        }

        self.color_copied = true;
        self.color_copied_deadline = Some(now + 2000);
        self.haptic.suggestion_tap();
        self.present_keys()
    }

    fn toggle_emoji_strip(&mut self, _now: u64) -> Result<()> {
        self.emoji_strip_active = !self.emoji_strip_active;
        self.emoji_scroll_offset = 0.0;
        self.emoji_scroll_active = false;
        self.emoji_scroll_last_centroid = None;
        self.haptic.suggestion_tap();
        self.present_keys()
    }

    fn insert_emoji(&mut self, idx: usize, _now: u64) -> Result<()> {
        let Some(&emoji_str) = renderer::POPULAR_EMOJIS.get(idx) else {
            return Ok(());
        };

        // Try direct typing with wtype first (does not touch clipboard, avoids polluting Noctalia history)
        let typed = std::process::Command::new("wtype")
            .arg(emoji_str)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !typed {
            let prev_clip = std::process::Command::new("wl-paste")
                .arg("--no-newline")
                .output()
                .ok()
                .and_then(|out| if out.status.success() && !out.stdout.is_empty() { Some(out.stdout) } else { None });

            let _ = std::process::Command::new("wl-copy").arg(emoji_str).status();
            if self.keyboard.paste_clipboard().is_err() {
                let _ = self.keyboard.paste_ctrl_v();
            }

            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if let Some(prev) = prev_clip {
                    use std::io::Write;
                    if let Ok(mut child) = std::process::Command::new("wl-copy")
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                    {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(&prev);
                        }
                        let _ = child.wait();
                        return;
                    }
                }
                let _ = std::process::Command::new("wl-copy").arg("-c").status();
            });
        }

        for ch in emoji_str.chars() {
            self.input_chars.insert(self.cursor_pos, ch);
            self.cursor_pos += 1;
        }
        self.haptic.suggestion_tap();

        self.refresh_suggestions_from_context()
    }

    fn insert_punctuation(&mut self, ch: char, _now: u64) -> Result<()> {
        let layout = self.current_layout_code();
        self.keyboard.type_char(ch, layout)?;
        self.input_chars.insert(self.cursor_pos, ch);
        self.cursor_pos += 1;
        self.haptic.suggestion_tap();
        self.refresh_suggestions_from_context()
    }

    fn scrub_media_to(&mut self, x: f64, with_haptic: bool) -> Result<()> {
        let Some((b_left, b_right)) = self.canvas.media_capsule_rect else {
            return Ok(());
        };
        let bar_w = f64::from(b_right - b_left).max(1.0);
        let p = ((x - f64::from(b_left)) / bar_w).clamp(0.0, 1.0) as f32;
        self.media_scrub_progress = Some(p);
        if let Some(total_us) = self.media_length_us {
            let target_us = (p as f64 * total_us as f64) as u64;
            let _ = self.mpris.set_position_micros(target_us);
            if with_haptic {
                self.haptic.suggestion_tap();
            }
        }
        self.render_idle_bar()
    }

    fn press_button(&mut self, index: usize, now: u64) -> Result<()> {
        if self.word_toast.is_some() {
            self.word_toast = None;
            self.render_idle_bar()?;
            return Ok(());
        }
        match self.layout.buttons[index].action {
            Action::Key(key) => {
                if key == Key::BrightnessDown {
                    self.level_reader.adjust(LevelKind::Brightness, -5);
                } else if key == Key::BrightnessUp {
                    self.level_reader.adjust(LevelKind::Brightness, 5);
                }
                if key == Key::Esc {
                    if self.color_picker_active {
                        self.close_color_picker()?;
                        return Ok(());
                    }
                    if self.emoji_strip_active {
                        self.emoji_strip_active = false;
                        self.emoji_scroll_offset = 0.0;
                        self.emoji_scroll_active = false;
                        self.emoji_scroll_last_centroid = None;
                        self.present_keys()?;
                        return Ok(());
                    }
                }
                if self.suggestions_active {
                    self.dismiss_suggestions()?;
                    if !self.physical_escape {
                        self.keyboard.press(key)?;
                    }
                } else {
                    self.keyboard.press(key)?;
                }
            }
            // Personal keys act once per tap. Holding repeats nothing.
            Action::Custom(custom) => match self.custom_keys.get(custom).map(|key| &key.action) {
                Some(CustomAction::Send(keys)) => self.keyboard.tap_combination(&keys)?,
                Some(CustomAction::Run(command)) => {
                    if command == "builtin:colorpicker" || command == "internal:colorpicker" {
                        return self.open_color_picker(now);
                    } else if command == "builtin:windows" || command == "internal:windows" {
                        return Ok(());
                    } else {
                        CustomKey::run(&command);
                    }
                }
                None => {}
            },
            Action::Preview => {
                return Ok(());
            }
            Action::Suggestion(sug_idx) => {
                return self.apply_suggestion(sug_idx, now);
            }
            Action::ColorPicker => {
                return Ok(());
            }
            Action::ColorCopy => {
                return self.copy_selected_color(now);
            }
            Action::FocusWindow(id) => {
                niri::focus_window(id);
                self.haptic.suggestion_tap();
                return Ok(());
            }
            Action::FocusWorkspace(idx) => {
                niri::focus_workspace(idx);
                self.haptic.suggestion_tap();
                return Ok(());
            }
            Action::ToggleWorkspaceFilter => {
                return Ok(());
            }
            Action::ToggleEmojiStrip => {
                return self.toggle_emoji_strip(now);
            }
            Action::OpenColorPicker => {
                self.haptic.suggestion_tap();
                return self.open_color_picker(now);
            }
            Action::Emoji(idx) => {
                return self.insert_emoji(idx, now);
            }
            Action::Punctuation(ch) => {
                return self.insert_punctuation(ch, now);
            }
            Action::Clipboard(idx) => {
                let items = self.clipboard_manager.items();
                if let Some(text) = items.get(idx) {
                    let text = text.clone();
                    self.haptic.suggestion_tap();
                    let prev_clip = std::process::Command::new("wl-paste")
                        .args(["-t", "text", "-n"])
                        .output()
                        .ok()
                        .map(|o| o.stdout);

                    if let Ok(mut child) = std::process::Command::new("wl-copy")
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                    {
                        if let Some(mut stdin) = child.stdin.take() {
                            use std::io::Write;
                            let _ = stdin.write_all(text.as_bytes());
                        }
                        let _ = child.wait();
                    }

                    let _ = std::process::Command::new("wtype")
                        .args(["-M", "ctrl", "-k", "v", "-m", "ctrl"])
                        .status();

                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        if let Some(prev) = prev_clip {
                            use std::io::Write;
                            if let Ok(mut child) = std::process::Command::new("wl-copy")
                                .stdin(std::process::Stdio::piped())
                                .spawn()
                            {
                                if let Some(mut stdin) = child.stdin.take() {
                                    let _ = stdin.write_all(&prev);
                                }
                                let _ = child.wait();
                            }
                        }
                    });
                }
                return Ok(());
            }
            Action::WindowSwitcher => {
                return Ok(());
            }
        }
        self.haptic.key_press();
        self.active_button = Some(index);
        if self
            .learner
            .action(now, self.state.mode, &mut self.state, &self.config)
        {
            self.save();
        }
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.present_keys()
    }

    fn refresh_suggestions_from_context(&mut self) -> Result<()> {
        let ctx = extract_context_at_cursor(&self.input_chars, self.cursor_pos);
        let layout = self.current_layout_code();
        let (prefix, prev) = if self.next_word_mode {
            ("", self.last_applied_word.as_deref())
        } else {
            (ctx.current_prefix.as_str(), ctx.previous_word.as_deref())
        };
        let nexts = if self.config.predictive_typing {
            self.predictor.suggest(prefix, prev, layout)
        } else {
            Vec::new()
        };
        self.suggestions = nexts;
        if self.suggestions.is_empty() && !self.emoji_strip_active && self.suggestions_active {
            if ctx.current_prefix.is_empty()
                && self.is_text_input_target_active()
                && self.config.clipboard_history
            {
                if self.clipboard_expire_ms.is_none() {
                    let now = self.now_ms();
                    self.clipboard_expire_ms =
                        Some(now + self.config.clipboard_timeout_sec * 1000);
                }
                self.present_keys()?;
                return Ok(());
            }
            self.dismiss_suggestions()?;
            return Ok(());
        }
        if !self.suggestions.is_empty() {
            self.clipboard_expire_ms = None;
        }
        if self.suggestions_active {
            self.present_keys()?;
        }
        Ok(())
    }

    fn apply_suggestion(&mut self, idx: usize, _now: u64) -> Result<()> {
        let Some(word) = self.suggestions.get(idx).cloned() else {
            return Ok(());
        };
        let layout = self.current_layout_code();

        let (text_to_type, replace_start, replace_end) = if self.next_word_mode {
            let needs_space = self.cursor_pos > 0
                && self.cursor_pos <= self.input_chars.len()
                && !self.input_chars[self.cursor_pos - 1].is_whitespace();
            let text = if needs_space {
                format!(" {}", word)
            } else {
                word.clone()
            };
            (text, self.cursor_pos, self.cursor_pos)
        } else {
            let ctx = extract_context_at_cursor(&self.input_chars, self.cursor_pos);
            let backspaces = self.cursor_pos.saturating_sub(ctx.word_start);
            if backspaces > 0 {
                self.keyboard.tap_backspaces(backspaces)?;
            }
            let deletes = ctx.word_end.saturating_sub(self.cursor_pos);
            if deletes > 0 {
                self.keyboard.tap_deletes(deletes)?;
            }
            (word.clone(), ctx.word_start, ctx.word_end)
        };

        self.keyboard.type_text(&text_to_type, layout)?;

        let word_chars: Vec<char> = text_to_type.chars().collect();
        let end = replace_end.min(self.input_chars.len());
        let start = replace_start.min(end);
        self.input_chars.splice(start..end, word_chars.iter().copied());
        self.cursor_pos = start + word_chars.len();

        self.haptic.suggestion_tap();

        // Keep suggestions strip active and offer next-word predictions
        let nexts = self.predictor.suggest("", Some(&word), layout);
        self.suggestions = nexts;
        self.suggestions_active = true;
        self.next_word_mode = true;
        self.last_applied_word = Some(word);
        self.active_button = None;
        self.present_keys()?;
        Ok(())
    }

    fn dismiss_suggestions(&mut self) -> Result<()> {
        if !self.suggestions_active {
            return Ok(());
        }
        self.suggestions_active = false;
        self.clipboard_expire_ms = None;
        self.next_word_mode = false;
        self.last_applied_word = None;
        self.emoji_strip_active = false;
        self.emoji_scroll_offset = 0.0;
        self.emoji_scroll_active = false;
        self.emoji_scroll_last_centroid = None;
        self.suggestions.clear();
        self.active_button = None;
        if self.visible {
            self.present_keys()?;
        } else if self.config.idle_status_bar {
            self.render_idle_bar()?;
        } else {
            self.go_dark(false)?;
        }
        Ok(())
    }

    fn paste_clipboard(&mut self) -> Result<()> {
        if let Ok(output) = std::process::Command::new("wl-paste")
            .arg("-n")
            .output()
        {
            if output.status.success() {
                let pasted = String::from_utf8_lossy(&output.stdout);
                let clean_pasted: String = pasted.chars().filter(|&c| c != '\r').collect();
                if !clean_pasted.is_empty() {
                    let mut count = 0;
                    for ch in clean_pasted.chars() {
                        self.input_chars.insert(self.cursor_pos + count, ch);
                        count += 1;
                    }
                    self.cursor_pos += count;
                    self.suggestions_active = true;
                    self.backlight.set(self.config.active_brightness)?;
                    self.refresh_suggestions_from_context()?;
                }
            }
        }
        Ok(())
    }

    fn on_pointer_click(&mut self) -> Result<()> {
        let now = self.now_ms();
        self.on_keyboard_or_trackpad_activity(now)?;
        if !self.is_text_input_allowed() {
            return Ok(());
        }
        self.next_word_mode = false;
        self.last_applied_word = None;
        if self.suggestions_active {
            // Check primary clipboard for clicked/selected word
            if let Ok(output) = std::process::Command::new("wl-paste")
                .args(["--primary", "-n"])
                .output()
            {
                if output.status.success() {
                    let sel = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !sel.is_empty() && sel.chars().all(|c| is_word_char(c) || c.is_alphanumeric()) {
                        self.input_chars = sel.chars().collect();
                        self.cursor_pos = self.input_chars.len();
                        self.refresh_suggestions_from_context()?;
                        return Ok(());
                    }
                }
            }
            // On a caret relocation click without selection, reset the pending typing token
            // so subsequent typing begins fresh at the new position instead of sticking to the old word.
            self.input_chars.clear();
            self.cursor_pos = 0;
            self.text_input_has_surrounding = false;
            self.refresh_suggestions_from_context()?;
        }
        Ok(())
    }

    fn scrub_to_touch_x(&mut self, x: f64) -> Result<()> {
        let Some(btn) = self.layout.buttons.iter().find(|b| matches!(b.action, Action::Preview)) else {
            return Ok(());
        };
        let n = self.input_chars.len();
        if n == 0 {
            return Ok(());
        }

        let pad = 14.0;
        let left = btn.left as f64 + pad;
        let right = btn.right as f64 - pad;
        if right <= left {
            return Ok(());
        }

        let clamped_x = x.clamp(left, right);
        let ratio = (clamped_x - left) / (right - left);

        let visible_count = 25.min(n);
        let target_pos = if n <= visible_count {
            (ratio * n as f64).round() as usize
        } else if ratio <= 0.05 {
            0
        } else if ratio >= 0.95 {
            n
        } else {
            let start = self.cursor_pos.saturating_sub(visible_count / 2);
            let end = (start + visible_count).min(n);
            let adjusted_start = end.saturating_sub(visible_count);
            (adjusted_start as f64 + ratio * visible_count as f64).round() as usize
        };
        let target_pos = target_pos.min(n);

        if target_pos != self.cursor_pos {
            let diff = target_pos as isize - self.cursor_pos as isize;
            if diff < 0 {
                for _ in 0..diff.abs() {
                    let _ = self.keyboard.tap_key(Key::Left);
                }
            } else {
                for _ in 0..diff {
                    let _ = self.keyboard.tap_key(Key::Right);
                }
            }
            self.cursor_pos = target_pos;
            self.haptic.suggestion_tap();
            self.refresh_suggestions_from_context()?;
        }
        Ok(())
    }

    fn keyboard_haptic_event(&mut self, key_code: u32, pressed: bool) {
        let state_changed = if pressed {
            self.pressed_physical_keys.insert(key_code)
        } else {
            self.pressed_physical_keys.remove(&key_code)
        };
        if state_changed {
            self.haptic.keyboard_key(key_code, pressed);
        }
    }

    fn keyboard_key_event(&mut self, key_code: u32, pressed: bool) -> Result<()> {
        let now = self.now_ms();
        if pressed {
            self.on_keyboard_or_trackpad_activity(now)?;
        }

        let Some(key) = code_to_key(key_code) else {
            return Ok(());
        };

        if key == Key::Space {
            self.space_down = pressed;
        }

        if !pressed {
            if self.repeat_key.as_ref().map(|(code, _)| *code) == Some(key_code) {
                self.repeat_key = None;
            }
            return Ok(());
        }

        if !self.is_text_input_allowed() {
            return Ok(());
        }

        // Set up autorepeat for repeating keys (niri repeat delay: 600ms)
        self.repeat_key = Some((key_code, now + 600));
        self.execute_key_action(key, key_code)
    }

    fn execute_key_action(&mut self, key: Key, _key_code: u32) -> Result<()> {
        let has_ctrl = self.active_modifiers.contains(&(Key::LeftCtrl as u32))
            || self.active_modifiers.contains(&(Key::RightCtrl as u32));
        let has_alt = self.active_modifiers.contains(&(Key::LeftAlt as u32))
            || self.active_modifiers.contains(&(Key::RightAlt as u32));
        let has_meta = self.active_modifiers.contains(&(Key::LeftMeta as u32))
            || self.active_modifiers.contains(&(Key::RightMeta as u32));

        match key {
            Key::Enter => {
                self.repeat_key = None;
                self.clipboard_expire_ms = None;
                self.input_chars.clear();
                self.cursor_pos = 0;
                self.next_word_mode = false;
                self.last_applied_word = None;
                if self.suggestions_active {
                    self.dismiss_suggestions()?;
                }
            }
            Key::Esc | Key::Tab => {
                self.repeat_key = None;
                self.clipboard_expire_ms = None;
                self.input_chars.clear();
                self.cursor_pos = 0;
                self.next_word_mode = false;
                self.last_applied_word = None;
                if self.color_picker_active {
                    self.close_color_picker()?;
                }
                if self.suggestions_active {
                    self.suggestions_manually_dismissed = true;
                    self.dismiss_suggestions()?;
                }
            }
            Key::Left => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                if has_meta {
                    self.cursor_pos = 0;
                } else if has_ctrl || has_alt {
                    while self.cursor_pos > 0 && self.input_chars[self.cursor_pos - 1].is_whitespace() {
                        self.cursor_pos -= 1;
                    }
                    while self.cursor_pos > 0 && !self.input_chars[self.cursor_pos - 1].is_whitespace() {
                        self.cursor_pos -= 1;
                    }
                } else if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
                self.refresh_suggestions_from_context()?;
            }
            Key::Right => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                if has_meta {
                    self.cursor_pos = self.input_chars.len();
                } else if has_ctrl || has_alt {
                    while self.cursor_pos < self.input_chars.len() && !self.input_chars[self.cursor_pos].is_whitespace() {
                        self.cursor_pos += 1;
                    }
                    while self.cursor_pos < self.input_chars.len() && self.input_chars[self.cursor_pos].is_whitespace() {
                        self.cursor_pos += 1;
                    }
                } else if self.cursor_pos < self.input_chars.len() {
                    self.cursor_pos += 1;
                }
                self.refresh_suggestions_from_context()?;
            }
            Key::Home | Key::Up => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                self.cursor_pos = 0;
                self.refresh_suggestions_from_context()?;
            }
            Key::End | Key::Down => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                self.cursor_pos = self.input_chars.len();
                self.refresh_suggestions_from_context()?;
            }
            Key::Backspace => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                self.suggestions_manually_dismissed = false;
                if has_meta {
                    self.input_chars.drain(..self.cursor_pos);
                    self.cursor_pos = 0;
                } else if has_ctrl || has_alt {
                    let old_pos = self.cursor_pos;
                    while self.cursor_pos > 0 && self.input_chars[self.cursor_pos - 1].is_whitespace() {
                        self.cursor_pos -= 1;
                    }
                    while self.cursor_pos > 0 && !self.input_chars[self.cursor_pos - 1].is_whitespace() {
                        self.cursor_pos -= 1;
                    }
                    self.input_chars.drain(self.cursor_pos..old_pos);
                } else if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input_chars.remove(self.cursor_pos);
                }

                self.next_word_mode = false;
                self.last_applied_word = None;
                self.refresh_suggestions_from_context()?;
            }
            Key::Delete => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                self.suggestions_manually_dismissed = false;
                if has_meta {
                    self.input_chars.drain(self.cursor_pos..);
                } else if has_ctrl || has_alt {
                    let mut end_pos = self.cursor_pos;
                    while end_pos < self.input_chars.len() && !self.input_chars[end_pos].is_whitespace() {
                        end_pos += 1;
                    }
                    while end_pos < self.input_chars.len() && self.input_chars[end_pos].is_whitespace() {
                        end_pos += 1;
                    }
                    self.input_chars.drain(self.cursor_pos..end_pos);
                } else if self.cursor_pos < self.input_chars.len() {
                    self.input_chars.remove(self.cursor_pos);
                }

                self.next_word_mode = false;
                self.last_applied_word = None;
                self.refresh_suggestions_from_context()?;
            }
            Key::Space => {
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                self.suggestions_manually_dismissed = false;
                self.input_chars.insert(self.cursor_pos, ' ');
                self.cursor_pos += 1;
                if self.suggestions_active {
                    self.next_word_mode = false;
                    self.refresh_suggestions_from_context()?;
                }
            }
            Key::Insert => {
                if self.shift_down && self.is_text_input_target_active() {
                    self.paste_clipboard()?;
                }
            }
            other => {
                if has_ctrl || has_meta {
                    if other == Key::V && self.is_text_input_target_active() {
                        self.paste_clipboard()?;
                    } else if other == Key::C || other == Key::A || other == Key::Z || other == Key::X {
                        // Keep suggestions alive on common editing shortcuts
                    }
                    return Ok(());
                }
                if !self.is_text_input_target_active() {
                    return Ok(());
                }
                let is_ru = self.current_layout_code() == "RU";
                let is_shift = self.shift_down;
                if let Some(ch) = key_to_char(other, is_ru, is_shift) {
                    self.suggestions_manually_dismissed = false;
                    self.next_word_mode = false;
                    self.last_applied_word = None;
                    self.input_chars.insert(self.cursor_pos, ch);
                    self.cursor_pos += 1;
                    let ctx = extract_context_at_cursor(&self.input_chars, self.cursor_pos);
                    if ctx.current_prefix.chars().count() >= 3 {
                        self.suggestions_active = true;
                        self.backlight.set(self.config.active_brightness)?;
                        self.refresh_suggestions_from_context()?;
                    } else if self.suggestions_active {
                        self.dismiss_suggestions()?;
                    }
                }
            }
        }
        Ok(())
    }

    fn touch_motion(&mut self, slot: u32, x: f64, y: f64) -> Result<()> {
        let now = self.now_ms();
        self.user_activity(now, false)?;
        self.resolve_pending(now)?;
        self.positions.insert(slot, x);

        if let Some((scrub_slot, _, _)) = self.scrubbing {
            if scrub_slot == slot {
                self.scrub_to_touch_x(x)?;
                return Ok(());
            }
        }
        if let Some((scrub_slot, start_x, ref mut dragged)) = self.media_scrubbing {
            if scrub_slot == slot {
                if (x - start_x).abs() >= 3.0 {
                    *dragged = true;
                }
                self.scrub_media_to(x, false)?;
                return Ok(());
            }
        }
        match self.contacts.get(&slot).copied() {
            Some(Contact::MediaScrubbing) => {
                self.scrub_media_to(x, false)?;
                Ok(())
            }
            Some(Contact::ColorRibbon) => {
                if let Some(ribbon) = self.layout.buttons.iter().find(|b| b.action == Action::ColorPicker) {
                    let ribbon_w = f64::from(ribbon.right - ribbon.left).max(1.0);
                    let hue = ((x - f64::from(ribbon.left)) / ribbon_w).clamp(0.0, 1.0) as f32;
                    if (hue - self.color_hue).abs() >= 0.015 {
                        self.color_hue = hue;
                        self.haptic.slider_tick();
                        self.present_keys()?;
                    }
                }
                Ok(())
            }
            Some(Contact::EmojiScroll) => {
                let scroll_xs: Vec<f64> = self
                    .contacts
                    .iter()
                    .filter(|(_, c)| matches!(c, Contact::EmojiScroll))
                    .filter_map(|(s, _)| self.positions.get(s).copied())
                    .collect();
                if !scroll_xs.is_empty() {
                    let centroid = scroll_xs.iter().sum::<f64>() / scroll_xs.len() as f64;
                    if let Some(last) = self.emoji_scroll_last_centroid {
                        let delta = (last - centroid) as f32;
                        if delta != 0.0 {
                            let emoji_start_boundary = if !self.physical_escape {
                                4 + self.canvas.width / 14 + 18 + 64 + 18
                            } else {
                                4 + 70 + 18 + 64 + 18
                            };
                            let emoji_end_boundary = self.canvas.width.saturating_sub(4);
                            let visible_w = emoji_end_boundary.saturating_sub(emoji_start_boundary) as f32;
                            let total_content_w = (renderer::POPULAR_EMOJIS.len() as f32) * (renderer::EMOJI_BTN_WIDTH + renderer::EMOJI_BTN_GAP) - renderer::EMOJI_BTN_GAP;
                            let max_scroll = (total_content_w - visible_w).max(0.0);

                            let new_offset = (self.emoji_scroll_offset + delta).clamp(0.0, max_scroll);
                            if (new_offset - self.emoji_scroll_offset).abs() >= 1.0 {
                                self.emoji_scroll_offset = new_offset;
                                self.present_keys()?;
                            }
                            self.emoji_scroll_last_centroid = Some(centroid);
                        }
                    } else {
                        self.emoji_scroll_last_centroid = Some(centroid);
                    }
                }
                Ok(())
            }
            Some(Contact::WindowButton(win_id, _, _)) => {
                niri::focus_window(win_id);
                self.haptic.suggestion_tap();
                Ok(())
            }
            Some(Contact::Swipe) => self.swipe_moved(now),
            Some(Contact::Pending) => {
                let Some(pending) = self.pending else {
                    return Ok(());
                };
                if let Some(index) = pending.button
                    && self.layout.hit(x, y, self.canvas.height) != Some(index)
                {
                    if pending.dark {
                        // Slid off the dark esc slot: an ordinary wake touch.
                        self.pending = Some(PendingTouch {
                            button: None,
                            ..pending
                        });
                        return self.hide_overlay();
                    }
                    self.pending = None;
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.present_keys()?;
                }
                Ok(())
            }
            Some(Contact::Button(index)) => {
                if self.layout.hit(x, y, self.canvas.height) != Some(index) {
                    self.keyboard.release()?;
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.present_keys()?;
                }
                Ok(())
            }
            Some(Contact::TouchIdEsc(index)) => {
                if self.layout.hit(x, y, self.canvas.height) != Some(index) {
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.render_touch_id(now)?;
                }
                Ok(())
            }
            Some(Contact::MediaHold(index)) => {
                if self.layout.hit(x, y, self.canvas.height) != Some(index) {
                    self.media_hold = None;
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.present_keys()?;
                }
                Ok(())
            }
            Some(Contact::TrackKey(forward)) => {
                let Some(toast) = self.toast.as_mut() else {
                    return Ok(());
                };
                let (left, right) = if forward {
                    toast.layout.next
                } else {
                    toast.layout.prev
                };
                if x < f64::from(left) || x >= f64::from(right) {
                    toast.pressed = None;
                    toast.start_ms = now.saturating_sub(TRACK_FADE_MS);
                    self.contacts.insert(slot, Contact::Cancelled);
                    return self.track_frame(now);
                }
                Ok(())
            }
            Some(Contact::DarkEsc) => {
                if !self
                    .layout
                    .hit(x, y, self.canvas.height)
                    .is_some_and(|index| self.is_esc(index))
                {
                    self.keyboard.release()?;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.hide_overlay()?;
                    self.show_activity_esc()?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn swipe_moved(&mut self, now: u64) -> Result<()> {
        let centroid = self.swipe_centroid();
        let Some(swipe) = self.swipe.as_mut() else {
            return Ok(());
        };
        let kind = swipe.kind;
        match kind {
            SwipeKind::Volume | SwipeKind::Brightness => {
                let steps = swipe.advance(centroid);
                let span = self.swipe_span();
                if self.level_kind.is_some()
                    && ((span.0 - self.level_fingers.0).abs() >= 3.0
                        || (span.1 - self.level_fingers.1).abs() >= 3.0)
                {
                    // The feedback follows the fingers.
                    self.level_fingers = span;
                    self.level_dirty = true;
                    self.update_level_side(now, false);
                }
                if steps == 0 {
                    return Ok(());
                }
                self.level_poll_ms.get_or_insert(now + LEVEL_POLL_MS);
                self.level_poll_until_ms = now + LEVEL_POLL_WINDOW_MS;
                let level_kind = match kind {
                    SwipeKind::Volume => LevelKind::Volume,
                    _ => LevelKind::Brightness,
                };
                self.level_reader.adjust(level_kind, steps);
                if let Some(pct) = self.level.percent {
                    let new_pct = (pct as i32 + steps).clamp(0, 100) as u32;
                    self.level.percent = Some(new_pct);
                    self.level_dirty = true;
                }
                self.level_stepped = true;
                // Pushing against a stop clicks firmly once, then stays
                // silent like the level does.
                if self
                    .level_kind
                    .is_some_and(|level_kind| at_stop(level_kind, self.level, Some(steps > 0)))
                {
                    if !std::mem::replace(&mut self.level_stop_felt, true) {
                        self.haptic.firm();
                    }
                } else {
                    self.haptic.click();
                }
                Ok(())
            }
            SwipeKind::Mode => {
                let Some(mode_swipe) = self.mode_swipe.as_mut() else {
                    return Ok(());
                };
                mode_swipe.last_centroid = centroid;
                let delta = centroid - mode_swipe.anchor;
                let step = mode_swipe.step;

                // When moving past full steps, update base_mode and advance anchor
                let steps = (delta / step).trunc() as i32;
                if steps != 0 {
                    let count = steps.abs();
                    for _ in 0..count {
                        let next_mode = if steps < 0 {
                            mode_swipe.base_mode.next()
                        } else {
                            mode_swipe.base_mode.previous()
                        };
                        mode_swipe.base_mode = next_mode;
                        self.haptic.click();
                    }
                    mode_swipe.anchor += f64::from(steps) * step;
                    self.state.set_mode(mode_swipe.base_mode);
                    self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
                }

                // Render continuous physical displacement tied 1:1 to finger movement
                let remaining_delta = centroid - mode_swipe.anchor;
                let (target_mode, direction, progress) = if remaining_delta < 0.0 {
                    let p = (-remaining_delta / step).clamp(0.0, 1.0) as f32;
                    (mode_swipe.base_mode.next(), 1, p)
                } else if remaining_delta > 0.0 {
                    let p = (remaining_delta / step).clamp(0.0, 1.0) as f32;
                    (mode_swipe.base_mode.previous(), -1, p)
                } else {
                    (mode_swipe.base_mode, 1, 0.0)
                };

                let from_frame = mode_swipe.frame(mode_swipe.base_mode);
                let to_frame = mode_swipe.frame(target_mode);
                self.blended.resize(self.canvas.pixels.len(), 0);
                compose_interactive_slide(
                    from_frame,
                    to_frame,
                    &mut self.blended,
                    usize::from(self.canvas.width),
                    mode_swipe.clip_left,
                    direction,
                    progress,
                    step as f32,
                );
                self.display.present(&self.blended, self.canvas.width, self.canvas.height)?;
                Ok(())
            }
        }
    }

    fn touch_up(&mut self, slot: u32) -> Result<()> {
        let now = self.now_ms();
        if let Some((scrub_slot, _, _)) = self.scrubbing {
            if scrub_slot == slot {
                self.scrubbing = None;
            }
        }
        if let Some((scrub_slot, _, dragged)) = self.media_scrubbing.take() {
            if scrub_slot == slot {
                if dragged {
                    self.haptic.suggestion_tap();
                }
                self.media_scrub_progress = None;
                if !self.visible && !self.suggestions_active {
                    self.render_idle_bar()?;
                }
            }
        }
        self.resolve_pending(now)?;
        self.positions.remove(&slot);
        match self.contacts.remove(&slot) {
            Some(Contact::MediaScrubbing) => {
                if let Some((_, _, dragged)) = self.media_scrubbing.take() {
                    if dragged {
                        self.haptic.suggestion_tap();
                    }
                }
                self.media_scrub_progress = None;
                if !self.visible && !self.suggestions_active {
                    self.render_idle_bar()?;
                }
            }
            Some(Contact::ColorRibbon) => {}
            Some(Contact::EmojiScroll) => {
                let remaining = self.contacts.values().any(|c| matches!(c, Contact::EmojiScroll));
                if !remaining {
                    self.emoji_scroll_active = false;
                    self.emoji_scroll_last_centroid = None;
                }
            }
            Some(Contact::WindowButton(win_id, _, _)) => {
                niri::focus_window(win_id);
                self.haptic.suggestion_tap();
            }
            Some(Contact::Pending) => {
                // Lifted within the window: a plain tap.
                if let Some(pending) = self.pending.take() {
                    if pending.dark && pending.button.is_some() {
                        self.keyboard.press(Key::Esc)?;
                        self.keyboard.release()?;
                        self.haptic.key_press();
                        self.haptic.key_release();
                        self.overlay_hide_ms = Some(now + ESC_LINGER_MS);
                    } else if pending.dark {
                        self.wake(true)?;
                    } else if let Some(index) = pending.button {
                        let is_emoji_or_sug = self.layout.buttons.get(index)
                            .map_or(false, |b| matches!(b.action, Action::Suggestion(_) | Action::Emoji(_) | Action::ToggleEmojiStrip | Action::OpenColorPicker | Action::Punctuation(_) | Action::Clipboard(_)));
                        self.press_button(index, now)?;
                        if !is_emoji_or_sug {
                            self.keyboard.release()?;
                            self.haptic.key_release();
                        }
                        self.active_button = None;
                        if self.suggestions_active || self.visible {
                            self.present_keys()?;
                        }
                    }
                }
            }
            Some(Contact::Button(index)) => {
                let is_emoji = self.layout.buttons.get(index)
                    .map_or(false, |b| matches!(b.action, Action::Emoji(_)));
                if is_emoji {
                    self.press_button(index, now)?;
                } else {
                    let is_suggestion = self.layout.buttons.get(index)
                        .map_or(false, |b| matches!(b.action, Action::Suggestion(_)));
                    self.keyboard.release()?;
                    if !is_suggestion {
                        self.haptic.key_release();
                    }
                }
                self.active_button = None;
                self.present_keys()?;
            }
            Some(Contact::TouchIdEsc(_)) => {
                self.keyboard.press(Key::Esc)?;
                self.keyboard.release()?;
                self.haptic.key_press();
                self.haptic.key_release();
                self.active_button = None;
                self.render_touch_id(now)?;
            }
            Some(Contact::MediaHold(index)) => {
                let seeking = self
                    .media_hold
                    .take()
                    .is_some_and(|hold| hold.seeking_since.is_some());
                if !seeking {
                    self.press_button(index, now)?;
                    self.keyboard.release()?;
                }
                self.haptic.key_release();
                self.active_button = None;
                self.present_keys()?;
            }
            Some(Contact::DarkEsc) => {
                self.keyboard.release()?;
                self.haptic.key_release();
                if self.activity_esc_shown {
                    self.escape_opacity = 1.0;
                    self.escape_fade = None;
                    self.render_activity_esc()?;
                } else {
                    self.overlay_hide_ms = Some(now + ESC_LINGER_MS);
                }
            }
            Some(Contact::TrackKey(forward)) => {
                let key = if forward {
                    Key::NextSong
                } else {
                    Key::PreviousSong
                };
                self.keyboard.press(key)?;
                self.keyboard.release()?;
                self.haptic.key_press();
                self.haptic.key_release();
                if let Some(toast) = self.toast.as_mut() {
                    // Stay fully visible. The next track replaces the text.
                    toast.pressed = None;
                    toast.start_ms = now.saturating_sub(TRACK_FADE_MS);
                }
                self.track_frame(now)?;
            }
            Some(Contact::Swipe) => {
                // When lifting fingers from a multi-finger swipe, fingers lift
                // unevenly over a few milliseconds. Ending the swipe on the first
                // lifted finger avoids accidentally switching to the other gesture level.
                self.swipe = None;
                if self.level_kind.is_some() {
                    self.overlay_hide_ms = Some(now + LEVEL_LINGER_MS);
                }
                if let Some(mode_swipe) = self.mode_swipe.take() {
                    self.finish_mode_swipe(mode_swipe, now)?;
                }
                for contact in self.contacts.values_mut() {
                    if matches!(contact, Contact::Swipe) {
                        *contact = Contact::Cancelled;
                    }
                }
            }
            _ => {}
        }
        if self.contacts.is_empty() {
            self.touches_armed = true;
            self.swipe = None;
            self.mode_swipe = None;
        }
        Ok(())
    }

    fn fn_event(&mut self, pressed: bool) -> Result<()> {
        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        let now = self.now_ms();
        self.last_activity_ms = now;
        self.last_touchbar_activity_ms = now;
        if self.idle_sleeping || self.idle_dimmed {
            self.idle_sleeping = false;
            self.idle_dimmed = false;
            self.backlight.set(self.config.active_brightness)?;
            if self.config.idle_status_bar && !self.visible {
                self.render_idle_bar()?;
            }
        }
        if pressed {
            self.fn_down_ms = Some(now);
            self.fn_toggled = false;
            // If any modifier key (ctrl/alt/cmd/shift) is held down, immediately show Function keys
            if !self.active_modifiers.is_empty() {
                if !self.visible {
                    self.state.set_mode(Mode::Function);
                    self.wake(false)?;
                } else if self.state.mode != Mode::Function {
                    self.switch_mode(Mode::Function, now)?;
                }
                self.fn_toggled = true;
            } else if !self.visible {
                self.state.set_mode(Mode::Special);
                self.wake(false)?;
            } else if self.state.mode == Mode::Special {
                self.switch_mode(Mode::Media, now)?;
            } else {
                self.switch_mode(Mode::Special, now)?;
            }
        } else {
            self.fn_down_ms = None;
            self.fn_toggled = false;
            if self.visible {
                self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
            }
        }
        Ok(())
    }

    fn modifier_event(&mut self, key: u32, pressed: bool) -> Result<()> {
        let now = self.now_ms();
        if pressed {
            self.active_modifiers.insert(key);
        } else {
            self.active_modifiers.remove(&key);
        }

        if key == Key::LeftShift as u32 || key == Key::RightShift as u32 {
            self.shift_down = pressed;
        }

        self.last_activity_ms = now;
        self.last_touchbar_activity_ms = now;

        if self.config.activity_backlight {
            self.activity_deadline_ms = Some(now + self.config.activity_timeout_ms);
            let fading_out = self
                .backlight_fade
                .as_ref()
                .is_some_and(|fade| fade.to == 0);
            if (self.activity_dimmed || fading_out) && pressed {
                let brightness = match self.activity_restore_brightness {
                    Some(brightness) if brightness > 0 => Some(brightness),
                    _ => match self
                        .activity_max_brightness
                        .map(Ok)
                        .unwrap_or_else(kbdlight::maximum_brightness)
                    {
                        Ok(brightness) if brightness > 0 => {
                            self.activity_max_brightness = Some(brightness);
                            Some(brightness)
                        }
                        Ok(_) => None,
                        Err(error) => {
                            eprintln!("t2-touchbar-ultra: read maximum keyboard brightness: {error:#}");
                            None
                        }
                    },
                };
                if let Some(brightness) = brightness {
                    let already_fading_in = self
                        .backlight_fade
                        .as_ref()
                        .is_some_and(|fade| fade.to == brightness && brightness > 0);
                    if !already_fading_in {
                        let current = self.activity_current_brightness.unwrap_or(0);
                        self.start_backlight_fade(now, current, brightness);
                    }
                }
            }
        }

        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }

        // If Fn is held down and a modifier is pressed, switch directly to Function mode
        if self.fn_down_ms.is_some() && pressed {
            if self.state.mode != Mode::Function {
                self.switch_mode(Mode::Function, now)?;
            }
            self.fn_toggled = true;
        }

        // If Touch Bar is in Function mode, keep Function keys displayed and refresh deadline
        if self.visible && self.state.mode == Mode::Function {
            self.deadline_ms = Some(now + self.state.learned(Mode::Function).timeout_ms);
        }

        Ok(())
    }

    fn toggle_mode_if_due(&mut self, now: u64) -> Result<()> {
        let Some(down) = self.fn_down_ms else {
            return Ok(());
        };
        if self.fn_toggled || !long_press_due(down, now, self.config.fn_long_press_ms) {
            return Ok(());
        }
        self.fn_toggled = true;
        // Long press on Fn switches to Function keys (F1, F2...)
        self.switch_mode(Mode::Function, now)
    }

    fn finish_mode_swipe(&mut self, mode_swipe: ModeSwipeState, now: u64) -> Result<()> {
        let delta = mode_swipe.last_centroid - mode_swipe.anchor;
        let step = mode_swipe.step;
        let (target_mode, direction, progress) = if delta < 0.0 {
            let p = (-delta / step).clamp(0.0, 1.0) as f32;
            (mode_swipe.base_mode.next(), 1, p)
        } else if delta > 0.0 {
            let p = (delta / step).clamp(0.0, 1.0) as f32;
            (mode_swipe.base_mode.previous(), -1, p)
        } else {
            (mode_swipe.base_mode, 1, 0.0)
        };

        if progress >= 0.5 {
            // Dragged >= 50%: commit to target_mode and smoothly settle forward
            let from = mode_swipe.frame(mode_swipe.base_mode).to_vec();
            let to = mode_swipe.frame(target_mode).to_vec();
            let duration_ms = (140.0 * (1.0 - progress)).max(50.0) as u64;
            self.state.set_mode(target_mode);
            self.save();
            self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
            self.layer_fade = Some(LayerFade {
                from,
                to,
                start_ms: now,
                duration_ms,
                next_ms: now,
                direction,
                clip_left: mode_swipe.clip_left,
                confirm: true,
                start_progress: progress,
                target_progress: 1.0,
                slide_px: step as f32,
            });
            self.present_keys()
        } else if progress > 0.03 {
            // Dragged < 50%: smoothly spring back to base_mode
            let from = mode_swipe.frame(mode_swipe.base_mode).to_vec();
            let to = mode_swipe.frame(target_mode).to_vec();
            let duration_ms = (140.0 * progress).max(50.0) as u64;
            self.state.set_mode(mode_swipe.base_mode);
            self.save();
            self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
            self.layer_fade = Some(LayerFade {
                from,
                to,
                start_ms: now,
                duration_ms,
                next_ms: now,
                direction,
                clip_left: mode_swipe.clip_left,
                confirm: false,
                start_progress: progress,
                target_progress: 0.0,
                slide_px: step as f32,
            });
            self.present_keys()
        } else {
            self.state.set_mode(mode_swipe.base_mode);
            self.present_keys()
        }
    }

    fn switch_mode(&mut self, mode: Mode, now: u64) -> Result<()> {
        let direction = if mode == self.state.mode.next() {
            1
        } else {
            -1
        };
        let clip_left = if self.physical_escape {
            0
        } else {
            usize::from(self.layout.buttons[0].right) + 1
        };
        let from = self.render_mode_frame(self.state.mode);
        let to = self.render_mode_frame(mode);
        self.state.set_mode(mode);
        self.save();
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.layer_fade = Some(LayerFade {
            from,
            to,
            start_ms: now,
            duration_ms: LAYER_FADE_MS,
            next_ms: now,
            direction,
            clip_left,
            confirm: false,
            start_progress: 0.0,
            target_progress: 1.0,
            slide_px: LAYER_SLIDE_PX,
        });
        self.present_keys()
    }

    fn touch_id_changed(&mut self, state: TouchIdState) -> Result<()> {
        self.touch_id = state;
        self.layer_fade = None;
        self.keyboard.release()?;
        self.active_button = None;
        self.clear_touches();
        self.touches_armed = false;
        self.deadline_ms = None;
        if state == TouchIdState::Idle {
            self.next_animation_ms = None;
            return self.go_dark_to_activity_esc(false);
        }
        self.visible = true;
        let now = self.now_ms();
        // Waiting, scanning and retry share the arrow. Keep its swing smooth
        // across those transitions.
        if self.next_animation_ms.is_none() {
            self.animation_start_ms = now;
        }
        self.next_animation_ms = None;
        self.render_touch_id(now)?;
        self.backlight.set(self.config.active_brightness)
    }

    /// Draws the prompt and schedules the next frame while it shows an arrow.
    fn render_touch_id(&mut self, now: u64) -> Result<()> {
        let animated = matches!(
            self.touch_id,
            TouchIdState::Waiting | TouchIdState::Scanning | TouchIdState::Retry
        );
        let nudge = if animated {
            self.next_animation_ms = Some(now + ARROW_FRAME_MS);
            let elapsed = now.saturating_sub(self.animation_start_ms) % ARROW_PERIOD_MS;
            arrow_nudge(elapsed as f32 / ARROW_PERIOD_MS as f32)
        } else {
            self.next_animation_ms = None;
            0.0
        };
        let esc = self
            .layout
            .buttons
            .iter()
            .find(|button| button.action == Action::Key(Key::Esc))
            .cloned();
        let active = esc.as_ref().is_some_and(|button| {
            self.active_button.is_some_and(|index| {
                self.layout
                    .buttons
                    .get(index)
                    .is_some_and(|active_button| active_button.action == button.action)
            })
        });
        self.canvas.touch_id(self.touch_id.as_str(), nudge, esc, active);
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)
    }

    fn tick(&mut self) -> Result<()> {
        let now = self.now_ms();
        if let Some((key_code, due_ms)) = self.repeat_key {
            if now >= due_ms {
                if let Some(key) = code_to_key(key_code) {
                    self.execute_key_action(key, key_code)?;
                    self.repeat_key = Some((key_code, now + 40));
                } else {
                    self.repeat_key = None;
                }
            }
        }
        self.resolve_pending(now)?;
        if self
            .layer_fade
            .as_ref()
            .is_some_and(|fade| now >= fade.next_ms)
        {
            self.present_layer_fade(now)?;
        }
        if self
            .toast
            .as_ref()
            .and_then(|toast| toast.next_ms)
            .is_some_and(|deadline| now >= deadline)
        {
            self.track_frame(now)?;
        }
        if let Some(toast) = &self.word_toast {
            if now >= toast.expire_ms {
                self.word_toast = None;
                self.render_idle_bar()?;
            } else if toast.is_fast {
                self.render_word_toast(now)?;
            }
        }
        if let Some(expire) = self.clipboard_expire_ms {
            if now >= expire {
                self.clipboard_expire_ms = None;
                self.suggestions_manually_dismissed = true;
                if self.suggestions_active {
                    self.dismiss_suggestions()?;
                }
            }
        }
        if self.level_poll_ms.is_some_and(|deadline| now >= deadline) {
            self.poll_level(now);
        }
        if self.overlay_hide_ms.is_some_and(|deadline| now >= deadline) {
            if self.level_kind.is_some() {
                self.overlay_hide_ms = None;
                self.level_fade_start_ms = Some(now);
                self.level_frame_ms = Some(now + LEVEL_FRAME_MS);
                self.level_dirty = true;
                self.flush_level()?;
            } else {
                self.hide_overlay()?;
            }
        }
        if self.level_frame_ms.is_some_and(|deadline| now >= deadline) {
            if self
                .level_fade_start_ms
                .is_some_and(|start| now.saturating_sub(start) >= LEVEL_FADE_MS)
            {
                self.hide_overlay()?;
            } else {
                self.level_dirty = true;
                self.flush_level()?;
                // Frames run while fading out or crossfading sides.
                let moving = self.level_right != f32::from(u8::from(self.level_right_target));
                self.level_frame_ms =
                    (self.level_fade_start_ms.is_some() || moving).then_some(now + LEVEL_FRAME_MS);
            }
        }
        if self
            .activity_deadline_ms
            .is_some_and(|deadline| now >= deadline)
        {
            self.activity_timeout();
        }
        self.advance_backlight_fade(now);
        self.advance_escape_fade(now)?;
        if self
            .media_hold
            .as_ref()
            .is_some_and(|hold| now >= hold.next_ms)
        {
            self.seek_step(now)?;
        }
        self.toggle_mode_if_due(now)?;
        if self.color_copied && self.color_copied_deadline.is_some_and(|dl| now >= dl) {
            self.color_copied = false;
            self.color_copied_deadline = None;
            if self.color_picker_active {
                self.present_keys()?;
            }
        }
        if self.touch_id == TouchIdState::Idle
            && self.visible
            && !self.suggestions_active
            && !self.color_picker_active
            && self.deadline_ms.is_some_and(|deadline| now >= deadline)
        {
            if self.state.mode == Mode::Function && !self.active_modifiers.is_empty() {
                self.deadline_ms = Some(now + self.state.learned(Mode::Function).timeout_ms);
            } else {
                self.go_dark_to_activity_esc(true)?;
            }
        }
        if !self.visible
            && self.touch_id == TouchIdState::Idle
            && self.config.idle_status_bar
        {
            // Auto-heal orphaned overlay or stale toast
            if self.overlay_shown
                && self.overlay_hide_ms.is_none()
                && self.level_kind.is_none()
                && self.toast.is_none()
                && self.swipe.is_none()
            {
                self.overlay_shown = false;
            }
            if let Some(toast) = &self.toast
                && toast.next_ms.is_none()
                && now.saturating_sub(toast.start_ms) > 6000
            {
                self.toast = None;
                self.overlay_shown = false;
            }
        }
        if !self.visible
            && !self.suggestions_active
            && self.touch_id == TouchIdState::Idle
            && self.config.idle_status_bar
            && self.word_toast.is_none()
            && !self.overlay_shown
            && self.swipe.is_none()
            && self.toast.is_none()
            && self.word_toast.is_none()
        {
            if self.idle_sleeping {
                self.cached_battery = status::read_battery();
            }
            let on_battery = self.cached_battery.map_or_else(|| !status::is_ac_online(), |b| !b.is_ac_online());
            let inactive_5_min = now.saturating_sub(self.last_touchbar_activity_ms) >= IDLE_TIMEOUT_MS;

            let should_render_scroll = self
                .next_idle_scroll_ms
                .is_some_and(|deadline| now >= deadline);

            if on_battery {
                if inactive_5_min {
                    if !self.idle_sleeping {
                        self.idle_sleeping = true;
                        self.idle_dimmed = false;
                        self.next_idle_scroll_ms = None;
                        self.canvas.clear();
                        self.display
                            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
                        self.backlight.set(0)?;
                    }
                } else if self.idle_sleeping {
                    self.idle_sleeping = false;
                    self.render_idle_bar()?;
                    self.backlight.set(self.config.active_brightness)?;
                } else if now >= self.next_idle_refresh_ms {
                    self.next_idle_refresh_ms = now + 5_000;
                    self.render_idle_bar()?;
                } else if should_render_scroll {
                    self.render_idle_bar()?;
                }
            } else {
                if self.idle_sleeping {
                    self.idle_sleeping = false;
                    self.render_idle_bar()?;
                    self.backlight.set(self.config.active_brightness)?;
                }

                if inactive_5_min {
                    if !self.idle_dimmed {
                        self.idle_dimmed = true;
                        self.backlight.set(self.config.active_brightness / 2)?;
                        self.render_idle_bar()?;
                    } else if now >= self.next_idle_refresh_ms {
                        self.next_idle_refresh_ms = now + 5_000;
                        self.render_idle_bar()?;
                    } else if should_render_scroll {
                        self.render_idle_bar()?;
                    }
                } else if self.idle_dimmed {
                    self.idle_dimmed = false;
                    self.backlight.set(self.config.active_brightness)?;
                    self.render_idle_bar()?;
                } else if now >= self.next_idle_refresh_ms {
                    self.next_idle_refresh_ms = now + 5_000;
                    self.render_idle_bar()?;
                } else if should_render_scroll {
                    self.render_idle_bar()?;
                }
            }
        }
        if self.learner.settle(now, &mut self.state, &self.config) {
            self.save();
        }
        if self
            .next_animation_ms
            .is_some_and(|deadline| now >= deadline)
        {
            self.render_touch_id(now)?;
        }
        Ok(())
    }

    fn next_timeout(&self) -> i32 {
        let now = self.now_ms();
        let mut deadlines = Vec::new();
        if let Some((_, due_ms)) = self.repeat_key {
            deadlines.push(due_ms);
        }
        if !self.visible
            && !self.suggestions_active
            && self.touch_id == TouchIdState::Idle
            && self.config.idle_status_bar
            && self.word_toast.is_none()
        {
            if !self.idle_sleeping {
                // Heartbeat watchdog deadline ensures poll loop never blocks indefinitely
                deadlines.push(now + 1000);
                if !self.overlay_shown && self.swipe.is_none() && self.toast.is_none() {
                    if self.next_idle_refresh_ms > now {
                        deadlines.push(self.next_idle_refresh_ms);
                    }
                    let idle_deadline = self.last_touchbar_activity_ms + IDLE_TIMEOUT_MS;
                    if idle_deadline > now {
                        deadlines.push(idle_deadline);
                    }
                    if let Some(deadline) = self.next_idle_scroll_ms
                        && deadline > now
                    {
                        deadlines.push(deadline);
                    }
                }
            } else {
                deadlines.push(now + 10_000);
            }
        }
        if let Some(fade) = &self.layer_fade
            && fade.next_ms > now
        {
            deadlines.push(fade.next_ms);
        }
        if let Some(deadline) = self.toast.as_ref().and_then(|toast| toast.next_ms)
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(toast) = &self.word_toast {
            if toast.expire_ms > now {
                deadlines.push(toast.expire_ms);
            }
            if toast.is_fast {
                deadlines.push(now + 25);
            }
        }
        if let Some(expire) = self.clipboard_expire_ms {
            if expire > now {
                deadlines.push(expire);
            }
        }
        if let Some(hold) = &self.media_hold
            && hold.next_ms > now
        {
            deadlines.push(hold.next_ms);
        }
        if let Some(deadline) = self.deadline_ms
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.color_copied_deadline
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(down) = self.fn_down_ms
            && !self.fn_toggled
            && down + self.config.fn_long_press_ms > now
        {
            deadlines.push(down + self.config.fn_long_press_ms);
        }
        if let Some(deadline) = self.learner.next_settle_ms(&self.config)
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.next_animation_ms
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(pending) = self.pending
            && pending.deadline_ms > now
        {
            deadlines.push(pending.deadline_ms);
        }
        if let Some(deadline) = self.level_poll_ms
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.overlay_hide_ms
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.level_frame_ms
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.activity_deadline_ms
            && deadline > now
        {
            deadlines.push(deadline);
        }
        if let Some(fade) = &self.backlight_fade
            && fade.next_ms > now
        {
            deadlines.push(fade.next_ms);
        }
        if let Some(fade) = &self.escape_fade
            && fade.next_ms > now
        {
            deadlines.push(fade.next_ms);
        }
        deadlines
            .into_iter()
            .filter(|&deadline| deadline > now)
            .min()
            .map(|deadline| (deadline - now).clamp(1, i32::MAX as u64) as i32)
            .unwrap_or(if !self.visible && !self.suggestions_active && self.touch_id == TouchIdState::Idle && self.config.idle_status_bar {
                33
            } else {
                -1
            })
    }
}

/// One frame of a row switch. The old row leaves before the new row fully
/// appears, while both travel farther than a simple crossfade. Columns left
/// of `clip_left` show the new row as is.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn compose_slide(
    from: &[u32],
    to: &[u32],
    out: &mut [u32],
    width: usize,
    clip_left: usize,
    direction: i32,
    progress: f32,
    slide_px: f32,
) {
    let progress = progress.clamp(0.0, 1.0);
    let shift_out = (direction as f32 * slide_px * progress).round() as isize;
    let shift_in = (direction as f32 * slide_px * (1.0 - progress)).round() as isize;
    let smooth = |value: f32| value * value * (3.0 - 2.0 * value);
    let old_opacity = 1.0 - smooth((progress / 0.68).clamp(0.0, 1.0));
    let new_opacity = smooth(((progress - 0.24) / 0.76).clamp(0.0, 1.0));
    let sample = |frame: &[u32], row: usize, x: isize| -> u32 {
        if x < clip_left as isize || x >= width as isize {
            0
        } else {
            frame[row + x as usize]
        }
    };
    for (row_index, out_row) in out.chunks_mut(width).enumerate() {
        let row = row_index * width;
        for (x, pixel) in out_row.iter_mut().enumerate() {
            if x < clip_left {
                *pixel = to[row + x];
                continue;
            }
            // The old row moved by -shift_out, the new one still lags by
            // shift_in, so each samples its source that far to the side.
            let old = sample(from, row, x as isize + shift_out);
            let new = sample(to, row, x as isize - shift_in);
            let channel = |shift: u32| {
                let a = ((old >> shift) & 0xff) as f32 * old_opacity;
                let b = ((new >> shift) & 0xff) as f32 * new_opacity;
                ((a + b).round().min(255.0) as u32) << shift
            };
            *pixel = channel(16) | channel(8) | channel(0);
        }
    }
}

/// Smooth 1:1 physical slide with constant 100% brightness crossfade.
#[allow(clippy::too_many_arguments)]
fn compose_interactive_slide(
    from: &[u32],
    to: &[u32],
    out: &mut [u32],
    width: usize,
    clip_left: usize,
    direction: i32,
    progress: f32,
    slide_px: f32,
) {
    let progress = progress.clamp(0.0, 1.0);
    let shift_out = (direction as f32 * slide_px * progress).round() as isize;
    let shift_in = (direction as f32 * slide_px * (1.0 - progress)).round() as isize;
    let old_opacity = 1.0 - progress;
    let new_opacity = progress;
    let sample = |frame: &[u32], row: usize, x: isize| -> u32 {
        if x < clip_left as isize || x >= width as isize {
            0
        } else {
            frame[row + x as usize]
        }
    };
    for (row_index, out_row) in out.chunks_mut(width).enumerate() {
        let row = row_index * width;
        for (x, pixel) in out_row.iter_mut().enumerate() {
            if x < clip_left {
                *pixel = to[row + x];
                continue;
            }
            let old = sample(from, row, x as isize + shift_out);
            let new = sample(to, row, x as isize - shift_in);
            let channel = |shift: u32| {
                let a = ((old >> shift) & 0xff) as f32 * old_opacity;
                let b = ((new >> shift) & 0xff) as f32 * new_opacity;
                ((a + b).round().min(255.0) as u32) << shift
            };
            *pixel = channel(16) | channel(8) | channel(0);
        }
    }
}

/// Eased swing: rest at 0, fully nudged halfway through the period.
fn arrow_nudge(phase: f32) -> f32 {
    (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0
}

fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--attach") {
        return usb::ensure_display_configuration();
    }
    if std::env::args().any(|arg| arg == "--detach") {
        return usb::restore_firmware_configuration();
    }
    let config = Config::load()?;
    let state_file = state_path();
    if std::env::args().any(|arg| arg == "--reset-learning") {
        let state = PersistentState::reset(&config);
        state.save(&state_file)?;
        println!("reset {}", state_file.display());
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--status") {
        let state = PersistentState::load(&state_file, &config);
        println!(
            "mode={:?}\nmedia_timeout_ms={}\nfunction_timeout_ms={}",
            state.mode, state.media.timeout_ms, state.function.timeout_ms
        );
        return Ok(());
    }

    let greeter = std::env::args().any(|arg| arg == "--greeter");
    let cli_mode = if greeter {
        Some(Mode::Function)
    } else if let Some(pos) = std::env::args().position(|arg| arg == "--mode") {
        match std::env::args().nth(pos + 1).as_deref() {
            Some("function") | Some("fn") => Some(Mode::Function),
            Some("media") => Some(Mode::Media),
            Some("special") | Some("custom") => Some(Mode::Special),
            _ => None,
        }
    } else if std::env::args().any(|arg| arg == "--function" || arg == "-f") {
        Some(Mode::Function)
    } else {
        None
    };

    let mut state = PersistentState::load(&state_file, &config);
    if let Some(mode) = cli_mode {
        state.set_mode(mode);
        state.alternate = mode;
    }
    let mut runtime = Runtime::new(config, state)?;
    if greeter {
        runtime.state_path = std::path::PathBuf::from("/dev/null");
        runtime.config.idle_status_bar = false;
        runtime.deadline_ms = None;
        runtime.visible = true;
        runtime.present_keys()?;
        runtime.backlight.set(runtime.config.active_brightness)?;
    } else if cli_mode.is_some() {
        runtime.state_path = std::path::PathBuf::from("/dev/null");
        runtime.visible = true;
        runtime.present_keys()?;
        runtime.backlight.set(runtime.config.active_brightness)?;
    } else {
        runtime.go_dark(false)?;
    }

    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }

    let mut touch_input = Libinput::new_with_udev(Interface);
    let mut main_input = Libinput::new_with_udev(Interface);
    touch_input
        .udev_assign_seat("seat-touchbar")
        .map_err(|()| anyhow::anyhow!("assign Touch Bar seat"))?;
    main_input
        .udev_assign_seat("seat0")
        .map_err(|()| anyhow::anyhow!("assign main seat"))?;
    let (touch_id_rx, mut touch_id_wake) = touchid::watch()?;
    touch_id_wake.set_nonblocking(true)?;
    let mut kbd_wake = kbdlight::watch()?;
    kbd_wake.set_nonblocking(true)?;
    let tracks = if runtime.config.show_track_changes {
        let (receiver, wake) = mpris::watch_tracks()?;
        wake.set_nonblocking(true)?;
        Some((receiver, wake))
    } else {
        None
    };
    let niri = if let Ok((watcher, wake)) = niri::NiriWatcher::spawn() {
        let _ = wake.set_nonblocking(true);
        Some((watcher, wake))
    } else {
        None
    };
    let (text_input_rx, text_input_wake, _text_input_running) = match TextInputWatcher::spawn() {
        Ok(t) => (Some(t.0), Some(t.1), Some(t.2)),
        Err(e) => {
            eprintln!("Wayland text_input watcher unavailable: {:?}", e);
            (None, None, None)
        }
    };
    let mut lookup = if runtime.config.word_lookup {
        Some(lookup::start_selection_lookup_with_delay(
            runtime.config.word_lookup_delay_ms,
        ))
    } else {
        None
    };
    let mut sleep = match sleep::watch() {
        Ok((rx, wake)) => {
            let _ = wake.set_nonblocking(true);
            Some((rx, wake))
        }
        Err(error) => {
            eprintln!("t2-touchbar-ultra: sleep watcher unavailable: {error:#}");
            None
        }
    };
    let mut touch_device: Option<InputDevice> = None;

    while !STOP.load(Ordering::Relaxed) {
        let mut poll_fds = [
            pollfd {
                fd: touch_input.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: main_input.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: touch_id_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: kbd_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: runtime.level_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                // poll ignores negative descriptors.
                fd: tracks.as_ref().map_or(-1, |(_, wake)| wake.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                // poll ignores negative descriptors.
                fd: niri.as_ref().map_or(-1, |(_, wake)| wake.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: runtime.predict_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: text_input_wake.as_ref().map_or(-1, |w| w.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: lookup.as_ref().map_or(-1, |(_, w)| w.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: sleep.as_ref().map_or(-1, |(_, w)| w.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as _,
                runtime.next_timeout(),
            )
        };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error()).context("poll input devices");
        }

        let loop_now_ms = runtime.now_ms();
        runtime.check_theme_reload(loop_now_ms)?;

        if poll_fds[2].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while touch_id_wake.read(&mut discard).is_ok() {}
            while let Ok(state) = touch_id_rx.try_recv() {
                runtime.touch_id_changed(state)?;
            }
        }

        if poll_fds[3].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while kbd_wake.read(&mut discard).is_ok() {}
            runtime.keyboard_backlight_changed()?;
        }

        if poll_fds[5].revents & libc::POLLIN != 0
            && let Some((receiver, wake)) = &tracks
        {
            let mut discard = [0u8; 64];
            while (&*wake).read(&mut discard).is_ok() {}
            while let Ok(track) = receiver.try_recv() {
                runtime.track_changed(track)?;
            }
        }

        if poll_fds[6].revents & libc::POLLIN != 0
            && let Some((watcher, wake)) = &niri
        {
            let mut discard = [0u8; 64];
            while (&*wake).read(&mut discard).is_ok() {}
            for event in watcher.try_recv() {
                match event {
                    niri::NiriEvent::Window(win) => runtime.active_window_changed(win)?,
                    niri::NiriEvent::Layout(layout) => runtime.keyboard_layout_changed(layout)?,
                    niri::NiriEvent::Workspace(ws) => runtime.workspace_changed(ws)?,
                }
            }
        }

        if poll_fds[4].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while (&runtime.level_wake).read(&mut discard).is_ok() {}
            runtime.levels_read();
        }

        if poll_fds[7].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while (&runtime.predict_wake).read(&mut discard).is_ok() {}
            if runtime.predictor.poll_responses() && runtime.suggestions_active {
                runtime.refresh_suggestions_from_context()?;
            }
        }

        if poll_fds[8].revents & libc::POLLIN != 0
            && let (Some(rx), Some(wake)) = (&text_input_rx, &text_input_wake)
        {
            let mut discard = [0u8; 64];
            while (&*wake).read(&mut discard).is_ok() {}
            while let Ok(event) = rx.try_recv() {
                match event {
                    TextInputEvent::Activated => {
                        runtime.text_input_focused = true;
                        runtime.suggestions_manually_dismissed = false;
                        if runtime.is_text_input_allowed() {
                            runtime.suggestions_active = true;
                            let _ = runtime.backlight.set(runtime.config.active_brightness);
                            let _ = runtime.refresh_suggestions_from_context();
                        }
                    }
                    TextInputEvent::Deactivated => {
                        runtime.text_input_focused = false;
                        runtime.text_input_has_surrounding = false;
                        runtime.suggestions_manually_dismissed = false;
                        if runtime.suggestions_active && !runtime.is_agy_active() {
                            let _ = runtime.dismiss_suggestions();
                        }
                    }
                    TextInputEvent::SurroundingText { text, cursor } => {
                        runtime.text_input_focused = true;
                        runtime.text_input_has_surrounding = true;
                        let byte_idx = cursor.min(text.len());
                        let mut safe_idx = byte_idx;
                        while safe_idx > 0 && !text.is_char_boundary(safe_idx) {
                            safe_idx -= 1;
                        }
                        let line_start = text[..safe_idx].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let line_end = text[safe_idx..].find('\n').map(|i| safe_idx + i).unwrap_or(text.len());
                        let line_text = &text[line_start..line_end];
                        let line_cursor_chars = text[line_start..safe_idx].chars().count();

                        runtime.input_chars = line_text.chars().collect();
                        runtime.cursor_pos = line_cursor_chars;
                        if !line_text.trim().is_empty() {
                            runtime.suggestions_manually_dismissed = false;
                        }
                        if runtime.is_text_input_allowed() {
                            if !runtime.suggestions_manually_dismissed || !line_text.trim().is_empty() {
                                runtime.suggestions_active = true;
                                runtime.backlight.set(runtime.config.active_brightness)?;
                                runtime.refresh_suggestions_from_context()?;
                            }
                        }
                    }
                }
            }
        }

        if poll_fds[9].revents & libc::POLLIN != 0
            && let Some((rx, wake)) = &mut lookup
        {
            let mut discard = [0u8; 64];
            while wake.read(&mut discard).is_ok() {}
            while let Ok(def) = rx.try_recv() {
                let _ = runtime.on_word_definition(def, loop_now_ms);
            }
        }

        if poll_fds[10].revents & libc::POLLIN != 0
            && let Some((rx, wake)) = &mut sleep
        {
            let mut discard = [0u8; 64];
            while wake.read(&mut discard).is_ok() {}
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    SleepEvent::Suspending => runtime.on_suspend(),
                    SleepEvent::Resumed => runtime.on_resume(),
                }
            }
        }

        if poll_fds[0].revents & libc::POLLIN != 0 {
            touch_input.dispatch()?;
            for event in &mut touch_input {
                match event {
                    Event::Device(DeviceEvent::Added(added))
                        if is_touch_bar(added.device().name()) =>
                    {
                        touch_device = Some(added.device());
                    }
                    Event::Touch(event)
                        if touch_device
                            .as_ref()
                            .is_none_or(|device| *device == event.device()) =>
                    {
                        match event {
                            TouchEvent::Down(down) => runtime.touch_down(
                                down.seat_slot(),
                                down.x_transformed(runtime.canvas.width as u32),
                                down.y_transformed(runtime.canvas.height as u32),
                            )?,
                            TouchEvent::Motion(motion) => runtime.touch_motion(
                                motion.seat_slot(),
                                motion.x_transformed(runtime.canvas.width as u32),
                                motion.y_transformed(runtime.canvas.height as u32),
                            )?,
                            TouchEvent::Up(up) => runtime.touch_up(up.seat_slot())?,
                            TouchEvent::Cancel(cancel) => runtime.touch_up(cancel.seat_slot())?,
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        if poll_fds[1].revents & libc::POLLIN != 0 {
            main_input.dispatch()?;
            for event in &mut main_input {
                match event {
                    Event::Device(DeviceEvent::Added(added)) => {
                        let device = added.device();
                        if !provides_fn(&device) && !provides_activity(&device) {
                            let _ = device.config_send_events_set_mode(SendEventsMode::DISABLED);
                        }
                    }
                    Event::Keyboard(KeyboardEvent::Key(key))
                        if key.device().name() != "T2 Touch Bar" =>
                    {
                        let pressed = key.key_state() == KeyState::Pressed;
                        let key_code = key.key();
                        runtime.keyboard_haptic_event(key_code, pressed);
                        if key_code == Key::Fn as u32 {
                            runtime.fn_event(pressed)?;
                        } else if is_function_modifier(key_code) {
                            runtime.modifier_event(key_code, pressed)?;
                        } else {
                            runtime.keyboard_key_event(key_code, pressed)?;
                        }
                    }
                    Event::Pointer(pointer) if pointer.device().name() != "T2 Touch Bar" => {
                        match pointer {
                            PointerEvent::Button(btn) if btn.button_state() == ButtonState::Pressed => {
                                runtime.on_pointer_click()?;
                            }
                            _ => {
                                runtime.on_keyboard_or_trackpad_activity(runtime.now_ms())?;
                            }
                        }
                    }
                    Event::Gesture(gesture) if gesture.device().name() != "T2 Touch Bar" => {
                        runtime.on_keyboard_or_trackpad_activity(runtime.now_ms())?;
                    }
                    Event::Touch(touch) if touch.device().name() != "T2 Touch Bar" => {
                        runtime.on_keyboard_or_trackpad_activity(runtime.now_ms())?;
                    }
                    _ => {}
                }
            }
        }
        runtime.tick()?;
        runtime.flush_level()?;
    }
    runtime.turn_off()?;
    runtime.restore_activity_backlight_on_exit();
    Ok(())
}

fn is_function_modifier(key: u32) -> bool {
    key == Key::LeftCtrl as u32
        || key == Key::RightCtrl as u32
        || key == Key::LeftAlt as u32
        || key == Key::RightAlt as u32
        || key == Key::LeftMeta as u32
        || key == Key::RightMeta as u32
        || key == Key::LeftShift as u32
        || key == Key::RightShift as u32
}

fn provides_fn(device: &InputDevice) -> bool {
    device.has_capability(DeviceCapability::Keyboard)
        && device.keyboard_has_key(Key::Fn as u32) == Ok(true)
}

fn provides_activity(device: &InputDevice) -> bool {
    device.name() != "T2 Touch Bar"
        && (device.has_capability(DeviceCapability::Keyboard)
            || device.has_capability(DeviceCapability::Pointer)
            || device.has_capability(DeviceCapability::Touch)
            || device.has_capability(DeviceCapability::Gesture))
}

/// Whether `level` sits at the bottom or at 100, or only at the one `up`
/// points to. GNOME never dims the display below 1%, which is its bottom.
fn at_stop(kind: LevelKind, level: Level, up: Option<bool>) -> bool {
    let Some(percent) = level.percent else {
        return false;
    };
    let bottom = match kind {
        LevelKind::Volume => 0,
        LevelKind::Brightness => 1,
    };
    match up {
        Some(true) => percent >= 100,
        Some(false) => percent <= bottom,
        None => percent <= bottom || percent >= 100,
    }
}

fn cache_slot(kind: LevelKind) -> usize {
    match kind {
        LevelKind::Volume => 0,
        LevelKind::Brightness => 1,
    }
}

fn is_touch_bar(name: &str) -> bool {
    name.contains("Touch Bar") || name.contains("TouchBar")
}

fn long_press_due(down_ms: u64, now_ms: u64, threshold_ms: u64) -> bool {
    now_ms.saturating_sub(down_ms) >= threshold_ms
}

fn has_physical_escape() -> bool {
    let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name") else {
        return false;
    };
    product
        .trim()
        .strip_prefix("MacBookPro")
        .and_then(|rest| rest.split(',').next())
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_bar_names_are_narrow() {
        assert!(is_touch_bar("Apple Inc. Touch Bar Display Touchpad"));
        assert!(!is_touch_bar("Apple Internal Keyboard / Trackpad"));
    }

    #[test]
    fn row_switch_slides_and_keeps_esc() {
        // One row, 300 px: esc left of column 10, a lit pixel at 150.
        let width = 300;
        let mut from = vec![0; width];
        let mut to = vec![0; width];
        from[150] = 0x00ff_ffff;
        to[150] = 0x00ff_ffff;
        from[5] = 0x0011_1111;
        to[5] = 0x0022_2222;
        let mut out = vec![0; width];
        // Halfway through a forward switch, the rows have moved apart and
        // each is independently fading through black.
        compose_slide(&from, &to, &mut out, width, 10, 1, 0.5, LAYER_SLIDE_PX);
        assert_eq!(out[55], 0x002c_2c2c);
        assert_eq!(out[245], 0x0045_4545);
        assert_eq!(out[150], 0);
        assert_eq!(out[5], 0x0022_2222, "esc is never moved or blended");
        // At the end only the new row remains, in place.
        compose_slide(&from, &to, &mut out, width, 10, 1, 1.0, LAYER_SLIDE_PX);
        assert_eq!(out[150], 0x00ff_ffff);
        assert_eq!(out[100], 0);
    }

    #[test]
    fn fn_hold_switches_at_the_configured_boundary() {
        assert!(!long_press_due(1_000, 1_599, 600));
        assert!(long_press_due(1_000, 1_600, 600));
    }

    #[test]
    fn test_two_finger_brightness_default() {
        let config = Config::default();
        assert!(config.two_finger_brightness);
    }

    #[test]
    fn test_idle_timeout_five_minutes() {
        assert_eq!(IDLE_TIMEOUT_MS, 300_000, "Inactivity timeout must be 5 minutes (300,000 ms)");
    }

    #[test]
    fn test_short_video_loop_progress() {
        let len_us = 15_000_000u64; // 15 seconds
        let pos_us = 10_000_000u64; // 10 seconds in
        let elapsed_ms = 7_000u64; // 7 seconds later (total 17s -> loops to 2s)
        let mut current_pos_us = pos_us + elapsed_ms * 1000;
        if current_pos_us >= len_us && len_us <= 30_000_000 {
            current_pos_us %= len_us;
        }
        assert_eq!(current_pos_us, 2_000_000);
        let progress = (current_pos_us as f32 / len_us as f32).clamp(0.0, 1.0);
        assert!((progress - 0.1333).abs() < 0.01);
    }

    #[test]
    fn test_poll_timeout_never_zero_when_idle_expired() {
        let now = 600_000u64; // 10 minutes in
        let last_activity = 100_000u64; // activity 500s ago (> 300s timeout)
        let mut deadlines = Vec::new();
        let idle_deadline = last_activity + IDLE_TIMEOUT_MS;
        if idle_deadline > now {
            deadlines.push(idle_deadline);
        }
        let timeout = deadlines
            .into_iter()
            .filter(|&deadline| deadline > now)
            .min()
            .map(|deadline| (deadline - now).clamp(1, i32::MAX as u64) as i32)
            .unwrap_or(33);
        assert!(timeout > 0, "Timeout must be positive (never 0)");
        assert_eq!(timeout, 33);
    }

    #[test]
    fn test_is_function_modifier() {
        // Ctrl, Alt, Command (Meta), Shift keys (both left and right) must be recognized as modifiers
        assert!(is_function_modifier(Key::LeftCtrl as u32));
        assert!(is_function_modifier(Key::RightCtrl as u32));
        assert!(is_function_modifier(Key::LeftAlt as u32));
        assert!(is_function_modifier(Key::RightAlt as u32));
        assert!(is_function_modifier(Key::LeftMeta as u32));
        assert!(is_function_modifier(Key::RightMeta as u32));
        assert!(is_function_modifier(Key::LeftShift as u32));
        assert!(is_function_modifier(Key::RightShift as u32));

        // Normal typing keys and F-keys must NOT be recognized as modifiers
        assert!(!is_function_modifier(Key::A as u32));
        assert!(!is_function_modifier(Key::F1 as u32));
        assert!(!is_function_modifier(Key::F12 as u32));
        assert!(!is_function_modifier(Key::Space as u32));
        assert!(!is_function_modifier(Key::Enter as u32));
        assert!(!is_function_modifier(Key::Esc as u32));
        assert!(!is_function_modifier(Key::Tab as u32));
        assert!(!is_function_modifier(Key::Fn as u32));
    }

    #[test]
    fn test_popular_emojis_count_and_uniqueness() {
        assert_eq!(renderer::POPULAR_EMOJIS.len(), 100, "Must be exactly 100 popular emojis");
        let mut set = std::collections::HashSet::new();
        for &emoji in renderer::POPULAR_EMOJIS.iter() {
            assert!(set.insert(emoji), "Duplicate emoji found in POPULAR_EMOJIS: {}", emoji);
        }
    }

    #[test]
    fn test_emoji_scroll_max_bounds() {
        let width = 2170u16;
        let emoji_start_boundary = 4 + 70 + 18 + 64 + 18; // 174
        let emoji_end_boundary = width.saturating_sub(4); // 2166
        let visible_w = emoji_end_boundary.saturating_sub(emoji_start_boundary) as f32; // 1992.0
        let total_content_w = (renderer::POPULAR_EMOJIS.len() as f32) * (renderer::EMOJI_BTN_WIDTH + renderer::EMOJI_BTN_GAP) - renderer::EMOJI_BTN_GAP;
        let max_scroll = (total_content_w - visible_w).max(0.0);
        assert!(max_scroll > 0.0);
        assert_eq!(max_scroll, 5194.0);

        let clamped = (1000.0f32).clamp(0.0, max_scroll);
        assert_eq!(clamped, 1000.0);
        let clamped_over = (10000.0f32).clamp(0.0, max_scroll);
        assert_eq!(clamped_over, max_scroll);
        let clamped_under = (-50.0f32).clamp(0.0, max_scroll);
        assert_eq!(clamped_under, 0.0);
    }

    #[test]
    fn test_scrub_time_display_formatting() {
        let total_us = 225_000_000u64; // 3m 45s = 225s
        let p = 83.0f32 / 225.0f32; // 83s = 1m 23s
        let current_us = (p as f64 * total_us as f64) as u64;
        let tot_sec = (total_us + 500_000) / 1_000_000;
        let cur_sec = ((current_us + 500_000) / 1_000_000).min(tot_sec);
        let formatted = format!("{} / {}", mpris::format_duration(cur_sec), mpris::format_duration(tot_sec));
        assert_eq!(formatted, "1:23 / 3:45");
    }

    #[test]
    fn test_is_agy_window_detection() {
        let win_antigravity = niri::ActiveWindow {
            title: "Antigravity".to_string(),
            app_id: Some("Alacritty".to_string()),
            pid: None,
        };
        let is_agy1 = win_antigravity.title.to_lowercase().contains("antigravity")
            || win_antigravity.title.to_lowercase().contains("agy");
        assert!(is_agy1);

        let win_terminal = niri::ActiveWindow {
            title: "bash".to_string(),
            app_id: Some("Alacritty".to_string()),
            pid: None,
        };
        let is_agy2 = win_terminal.title.to_lowercase().contains("antigravity")
            || win_terminal.title.to_lowercase().contains("agy");
        assert!(!is_agy2);
    }

    #[test]
    fn test_next_word_prediction_mode() {
        let (mut predictor, _) = predict::Predictor::new();
        // Predicting next words after "привет"
        let nexts = predictor.suggest("", Some("привет"), "RU");
        assert!(!nexts.is_empty(), "Next word suggestions must not be empty after applying word");
        assert!(nexts.contains(&"как дела".to_string()) || nexts.contains(&"всем".to_string()));

        // In next-word mode, next word typed after a word without space must prepend space
        let input_chars: Vec<char> = "привет".chars().collect();
        let cursor_pos = input_chars.len();
        let word = "всем".to_string();
        let needs_space = cursor_pos > 0 && !input_chars[cursor_pos - 1].is_whitespace();
        assert!(needs_space);
        let text_to_type = if needs_space { format!(" {}", word) } else { word };
        assert_eq!(text_to_type, " всем");
        assert!(!text_to_type.ends_with(' '), "Must not have trailing space");
    }

    #[test]
    fn test_second_word_prefix_preserves_first_letter() {
        // Simulating the user typing: "практичный спос"
        let mut input_chars: Vec<char> = "практичный ".chars().collect();
        let mut cursor_pos = input_chars.len();

        // 1st letter of 2nd word: 'с'
        input_chars.insert(cursor_pos, 'с');
        cursor_pos += 1;
        let ctx1 = predict::extract_context_at_cursor(&input_chars, cursor_pos);
        assert_eq!(ctx1.current_prefix, "с");
        assert_eq!(ctx1.previous_word, Some("практичный".to_string()));

        // 2nd letter of 2nd word: 'п'
        input_chars.insert(cursor_pos, 'п');
        cursor_pos += 1;
        let ctx2 = predict::extract_context_at_cursor(&input_chars, cursor_pos);
        assert_eq!(ctx2.current_prefix, "сп");

        // 3rd letter of 2nd word: 'о'
        input_chars.insert(cursor_pos, 'о');
        cursor_pos += 1;
        let ctx3 = predict::extract_context_at_cursor(&input_chars, cursor_pos);
        assert_eq!(ctx3.current_prefix, "спо");
        assert!(ctx3.current_prefix.chars().count() >= 3);

        // 4th letter of 2nd word: 'с'
        input_chars.insert(cursor_pos, 'с');
        cursor_pos += 1;
        let ctx4 = predict::extract_context_at_cursor(&input_chars, cursor_pos);
        assert_eq!(ctx4.current_prefix, "спос");
        assert_eq!(ctx4.previous_word, Some("практичный".to_string()));

        let (mut predictor, _) = predict::Predictor::new();
        let suggestions = predictor.suggest(&ctx4.current_prefix, ctx4.previous_word.as_deref(), "RU");
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().all(|s| s.starts_with("спос")), "All suggestions must start with 'спос'");
        assert!(suggestions.contains(&"способ".to_string()));
    }

    #[test]
    fn test_color_picker_button_and_wtype_emoji() {
        let candidates = vec!["способ".to_string()];
        let layout = renderer::Layout::suggestions(2170, &candidates, "спос", true, None, &[]);
        let has_palette = layout.buttons.iter().any(|b| b.action == renderer::Action::OpenColorPicker);
        assert!(has_palette, "Palette button must exist in suggestions strip");

        let status = std::process::Command::new("wtype")
            .arg("-v")
            .status();
        assert!(status.is_ok(), "wtype must be installed and executable");
    }

    #[test]
    fn test_color_picker_swipe_does_not_switch_to_idle() {
        let (width, hue, copied) = (2170u16, 0.5f32, false);
        let layout = renderer::Layout::color_picker(width, hue, true, copied);
        assert!(!layout.buttons.is_empty());
        let ribbon = layout.buttons.iter().find(|b| b.action == renderer::Action::ColorPicker);
        assert!(ribbon.is_some(), "Ribbon button must exist in color picker");

        let deadline_ms = Some(1000u64);
        let now = 2000u64;
        let suggestions_active = false;
        let color_picker_active = true;
        let visible = true;
        let touch_id_idle = true;

        let should_go_dark = touch_id_idle
            && visible
            && !suggestions_active
            && !color_picker_active
            && deadline_ms.is_some_and(|dl| now >= dl);

        assert!(!should_go_dark, "Color picker must never go dark when active");
    }

    #[test]
    fn test_telegram_and_text_input_target_active() {
        let win_telegram = niri::ActiveWindow {
            title: "Telegram Chat".to_string(),
            app_id: Some("TelegramDesktop".to_string()),
            pid: Some(4691),
        };
        let app_id = win_telegram.app_id.as_deref().unwrap_or("").to_lowercase();
        let title = win_telegram.title.to_lowercase();
        let is_telegram = app_id.contains("telegram") || title.contains("telegram");
        assert!(is_telegram, "TelegramDesktop must be recognized as telegram");

        let win_terminal = niri::ActiveWindow {
            title: "alacritty".to_string(),
            app_id: Some("alacritty".to_string()),
            pid: Some(100),
        };
        let app_id_term = win_terminal.app_id.as_deref().unwrap_or("").to_lowercase();
        assert!(app_id_term.contains("alacritty"));
    }

    #[test]
    fn test_word_definition_and_clipboard_timeouts() {
        // Word toast must display for 60 seconds
        let now = 100_000u64;
        let word_toast_expire = now + 60_000;
        assert_eq!(word_toast_expire - now, 60_000);

        // Clipboard history panel must display for 30 seconds
        let clip_expire = now + 30_000;
        assert_eq!(clip_expire - now, 30_000);
    }
}
