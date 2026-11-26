// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
};

use ab_glyph::{Font, FontVec, OutlineCurve, VariableFont};
use anyhow::{Context, Result, anyhow};
use input_linux::Key;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use crate::{niri, policy::Mode, status};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Key(Key),
    /// Index into the personal keys from keys.toml.
    Custom(usize),
    /// Index into predictive word suggestions.
    Suggestion(usize),
    /// Non-clickable context line preview capsule.
    Preview,
    ColorPicker,
    ColorCopy,
    OpenColorPicker,
    #[allow(dead_code)]
    FocusWindow(u64),
    #[allow(dead_code)]
    FocusWorkspace(u32),
    #[allow(dead_code)]
    ToggleWorkspaceFilter,
    ToggleEmojiStrip,
    Emoji(usize),
    #[allow(dead_code)]
    Punctuation(char),
    Clipboard(usize),
    #[allow(dead_code)]
    WindowSwitcher,
}

#[derive(Clone)]
enum Glyph {
    Text(&'static str),
    #[allow(dead_code)]
    Dynamic(String),
    Clipboard(String),
    Preview { text: String, cursor_pos: usize, prefix_len: usize },
    Suggestion { text: String, match_len: usize, is_typo_fix: bool },
    Icon(Icon),
    Svg(Arc<Pixmap>),
    ColorRibbon { hue: f32 },
    ColorSwatch { r: u8, g: u8, b: u8, copied: bool },
    #[allow(dead_code)]
    WindowItem { title: String, app_id: Option<String>, is_focused: bool },
    Emoji(&'static str),
}

pub const POPULAR_EMOJIS: &[&str] = &[
    // 1. Recommended & top daily favorites (1-25)
    "😊", "😂", "👍", "❤️", "🔥", "✨", "🎉", "🙏", "🚀", "👀",
    "🤔", "👏", "👌", "💯", "🤣", "😎", "💪", "🥳", "🙌", "😍",
    "💡", "⚡", "😴", "🤝", "🥰",
    // 2. High-frequency emotional expressions (26-50)
    "😭", "🥺", "😁", "😉", "🤩", "😘", "😋", "😜", "🤪", "🤫",
    "🤗", "🤭", "🫡", "🤨", "😐", "😑", "🙄", "😬", "😮‍💨", "🤥",
    "😌", "😔", "😪", "🤤", "😷",
    // 3. Reactions & states (51-75)
    "🤒", "🤕", "🤢", "🤮", "🤧", "🥵", "🥶", "🥴", "😵", "🤯",
    "🤠", "🥸", "🤓", "🧐", "😕", "😟", "🙁", "😮", "😯", "😲",
    "😳", "😦", "😧", "😨", "😰",
    // 4. Hands, hearts & top symbols (76-100)
    "😥", "😢", "😱", "😖", "😣", "💖", "💕", "💔", "🧡", "💛",
    "💚", "💙", "💜", "🖤", "🤍", "⭐", "🌟", "💫", "💥", "✌️",
    "🤞", "🤟", "🤘", "🤙", "👋",
];

pub const EMOJI_BTN_WIDTH: f32 = 58.0;
pub const EMOJI_BTN_GAP: f32 = 14.0;
pub const EMOJI_GLYPH_SIZE: f32 = 52.0;

#[allow(dead_code)]
pub const QUICK_PUNCTUATION: &[char] = &[
    ',', '.', '?', '!', ':', ';', '-', '"', '(', ')',
];

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = (h % 1.0 + 1.0) % 1.0;
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

#[derive(Clone, Debug)]
pub struct CustomItem {
    pub label: &'static str,
    pub icon: Option<&'static str>,
    pub svg: Option<Arc<Pixmap>>,
}

impl From<&'static str> for CustomItem {
    fn from(label: &'static str) -> Self {
        Self {
            label,
            icon: None,
            svg: None,
        }
    }
}

impl From<(&'static str, Option<&'static str>)> for CustomItem {
    fn from(val: (&'static str, Option<&'static str>)) -> Self {
        Self {
            label: val.0,
            icon: val.1,
            svg: None,
        }
    }
}

pub fn find_icon_svg(name: &str) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env_dir) = std::env::var_os("T2_TOUCHBAR_ULTRA_ICONS") {
        dirs.push(PathBuf::from(env_dir));
    }
    if let Some(parent) = crate::keys::path().parent() {
        dirs.push(parent.join("icons"));
    }
    dirs.push(PathBuf::from("./icons"));
    dirs.push(PathBuf::from("/usr/share/t2-touchbar-ultra/icons"));
    dirs.push(PathBuf::from("/etc/t2-touchbar-ultra/icons"));

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let exact = dir.join(format!("{name}.svg"));
        if exact.is_file() {
            return Some(exact);
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            let target_lower = format!("{}.svg", name.to_lowercase());
            for entry in entries.flatten() {
                if let Some(file_name) = entry.file_name().to_str()
                    && file_name.to_lowercase() == target_lower
                {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

pub fn render_svg_to_pixmap(svg_bytes: &[u8], max_w: u32, max_h: u32) -> Option<Pixmap> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg_bytes, &opt).ok()?;
    let size = tree.size();
    let (sw, sh) = (size.width(), size.height());
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    let scale = (max_w as f32 / sw).min(max_h as f32 / sh);
    let out_w = ((sw * scale).round() as u32).max(1);
    let out_h = ((sh * scale).round() as u32).max(1);
    let mut pixmap = Pixmap::new(out_w, out_h)?;
    let transform = Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap)
}

type SvgCache = Mutex<Option<HashMap<(String, u32, u32), Option<Arc<Pixmap>>>>>;
static SVG_CACHE: SvgCache = Mutex::new(None);

pub fn load_icon_svg(name: &str, max_w: u32, max_h: u32) -> Option<Arc<Pixmap>> {
    let key = (name.to_lowercase(), max_w, max_h);
    let mut guard = SVG_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let loaded = (|| {
        let path = find_icon_svg(name)?;
        let data = fs::read(&path).ok()?;
        render_svg_to_pixmap(&data, max_w, max_h).map(Arc::new)
    })();
    cache.insert(key, loaded.clone());
    loaded
}

pub fn render_png_to_pixmap(png_bytes: &[u8], max_w: u32, max_h: u32) -> Option<Pixmap> {
    let src = Pixmap::decode_png(png_bytes).ok()?;
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    let scale = (max_w as f32 / sw).min(max_h as f32 / sh);
    let out_w = ((sw * scale).round() as u32).max(1);
    let out_h = ((sh * scale).round() as u32).max(1);
    let mut target = Pixmap::new(out_w, out_h)?;
    let transform = Transform::from_scale(scale, scale);
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    target.draw_pixmap(0, 0, src.as_ref(), &paint, transform, None);
    Some(target)
}

pub fn load_image_file_to_pixmap(path: &FsPath, max_w: u32, max_h: u32) -> Option<Pixmap> {
    let data = fs::read(path).ok()?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "svg" {
        render_svg_to_pixmap(&data, max_w, max_h)
    } else if ext == "png" {
        render_png_to_pixmap(&data, max_w, max_h)
    } else {
        render_svg_to_pixmap(&data, max_w, max_h)
            .or_else(|| render_png_to_pixmap(&data, max_w, max_h))
    }
}

pub fn find_icon_file(name_or_path: &str) -> Option<PathBuf> {
    let p = FsPath::new(name_or_path);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }

    // 1. Check custom user/package dirs first (custom SVGs)
    if let Some(path) = find_icon_svg(name_or_path) {
        return Some(path);
    }

    // 2. Common system icon search directories (pixmaps, scalable SVG, high-res PNG)
    let mut search_dirs = Vec::new();

    // User icons
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        search_dirs.push(p.join(".local/share/icons/hicolor/scalable/apps"));
        search_dirs.push(p.join(".local/share/icons/hicolor/512x512/apps"));
        search_dirs.push(p.join(".local/share/icons/hicolor/256x256/apps"));
        search_dirs.push(p.join(".local/share/icons/hicolor/128x128/apps"));
        search_dirs.push(p.join(".local/share/icons/hicolor/64x64/apps"));
        search_dirs.push(p.join(".local/share/icons/hicolor/48x48/apps"));
        search_dirs.push(p.join(".local/share/pixmaps"));
    }

    // System icons
    search_dirs.push(PathBuf::from("/usr/share/pixmaps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/scalable/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/512x512/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/256x256/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/128x128/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/64x64/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/48x48/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/hicolor/32x32/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/Adwaita/scalable/apps"));
    search_dirs.push(PathBuf::from("/usr/share/icons/Adwaita/48x48/apps"));

    let lower = name_or_path.to_lowercase();
    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }
        for ext in &[".svg", ".png"] {
            let candidate = dir.join(format!("{name_or_path}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if lower != name_or_path {
            for ext in &[".svg", ".png"] {
                let candidate = dir.join(format!("{lower}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

type ImageCache = Mutex<Option<HashMap<(String, u32, u32), Option<Arc<Pixmap>>>>>;
static IMAGE_CACHE: ImageCache = Mutex::new(None);

pub fn load_app_icon(name_or_path: &str, max_w: u32, max_h: u32) -> Option<Arc<Pixmap>> {
    let key = (name_or_path.to_lowercase(), max_w, max_h);
    let mut guard = IMAGE_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }

    let loaded = (|| {
        let path = find_icon_file(name_or_path)?;
        load_image_file_to_pixmap(&path, max_w, max_h).map(Arc::new)
    })();

    cache.insert(key, loaded.clone());
    loaded
}

static DESKTOP_ICON_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn get_desktop_entry_icons() -> HashMap<String, String> {
    let mut guard = DESKTOP_ICON_CACHE.lock().unwrap();
    if let Some(map) = guard.as_ref() {
        return map.clone();
    }
    let mut map = HashMap::new();
    let mut dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        dirs.push(p.join(".local/share/applications"));
        dirs.push(p.join(".local/share/flatpak/exports/share/applications"));
    }

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let key = file_stem.to_lowercase();

            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };

            let mut icon_val = None;
            let mut wm_class = None;
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('[') && line != "[Desktop Entry]" {
                    break;
                }
                if let Some(val) = line.strip_prefix("Icon=")
                    && icon_val.is_none()
                {
                    icon_val = Some(val.trim().to_string());
                } else if let Some(val) = line.strip_prefix("StartupWMClass=")
                    && wm_class.is_none()
                {
                    wm_class = Some(val.trim().to_string());
                }
            }

            if let Some(icon) = icon_val {
                map.entry(key).or_insert_with(|| icon.clone());
                if let Some(wm) = wm_class {
                    map.entry(wm.to_lowercase()).or_insert_with(|| icon);
                }
            }
        }
    }

    *guard = Some(map.clone());
    map
}

pub fn resolve_custom_icon(label: &str, configured_icon: Option<&str>) -> Option<Icon> {
    let name = configured_icon.unwrap_or(label).to_lowercase();
    match name.as_str() {
        "fullscreen" | "full" | "fscreen" => Some(Icon::Fullscreen),
        "emoji" | "emo" | "smiley" | "smile" => Some(Icon::Emoji),
        "palette" | "color" | "colorpicker" | "picker" => Some(Icon::Palette),
        "screenshot" | "screen" | "snip" | "shot" | "capture" | "camera" => Some(Icon::Screenshot),
        "terminal" | "term" | "console" | "kgx" | "kitty" | "alacritty" => Some(Icon::Terminal),
        "browser" | "web" | "firefox" | "chrome" => Some(Icon::Browser),
        "folder" | "files" | "nautilus" => Some(Icon::Folder),
        "calc" | "calculator" => Some(Icon::Calculator),
        "lock" => Some(Icon::Lock),
        "settings" | "config" | "gear" => Some(Icon::Settings),
        "music" | "spotify" => Some(Icon::Music),
        "search" | "find" => Some(Icon::Search),
        "play" => Some(Icon::Play),
        "playpause" => Some(Icon::PlayPause),
        "plug" | "ac" => Some(Icon::Plug),
        "mute" => Some(Icon::Mute),
        "volume" | "vol" => Some(Icon::Volume(true)),
        "brightness" | "brt" => Some(Icon::Brightness(true)),
        "mic" | "microphone" => Some(Icon::Microphone),
        "ram" | "mem" | "memory" => Some(Icon::Ram),
        "cpu" | "processor" => Some(Icon::Cpu),
        "temp" | "temperature" | "thermometer" => Some(Icon::Temperature),
        "window" | "app" => Some(Icon::Window),
        _ => None,
    }
}

pub fn resolve_window_svg(
    app_id: Option<&str>,
    title: &str,
    max_w: u32,
    max_h: u32,
) -> Option<Arc<Pixmap>> {
    let combined = format!("{} {}", app_id.unwrap_or_default(), title).to_lowercase();

    // 1. Check known aliases and web apps first
    if (combined.contains("2gis") || combined.contains("2гис"))
        && let Some(svg) = load_icon_svg("2Gis", max_w, max_h)
    {
        return Some(svg);
    }
    if ((combined.contains("google") && (combined.contains("drive") || combined.contains("диск")))
        || combined.contains("drive.google.com")
        || combined.contains("googledrive"))
        && let Some(svg) = load_icon_svg("GoogleDrive", max_w, max_h)
    {
        return Some(svg);
    }
    if (combined.contains("youtube") || combined.contains("ютуб"))
        && let Some(svg) = load_icon_svg("Youtube", max_w, max_h)
    {
        return Some(svg);
    }
    if (combined.contains("chatgpt") || combined.contains("chat.openai") || combined.contains("чатгпт"))
        && let Some(svg) = load_icon_svg("Chatgpt", max_w, max_h)
    {
        return Some(svg);
    }
    if combined.contains("claude")
        && let Some(svg) = load_icon_svg("Claude", max_w, max_h)
    {
        return Some(svg);
    }
    if combined.contains("gemini")
        && let Some(svg) = load_icon_svg("Gemini", max_w, max_h)
    {
        return Some(svg);
    }
    if combined.contains("grok")
        && let Some(svg) = load_icon_svg("Grok", max_w, max_h)
    {
        return Some(svg);
    }
    if (combined.contains("qwen") || combined.contains("通义千问"))
        && let Some(svg) = load_icon_svg("Qwen", max_w, max_h)
    {
        return Some(svg);
    }
    if (combined.contains("lampa") || combined.contains("лампа"))
        && let Some(svg) = load_icon_svg("Lampa", max_w, max_h)
    {
        return Some(svg);
    }

    // 2. Real application icon matching via desktop entries and system icon themes
    if let Some(id) = app_id {
        // A. Curated custom icon first
        if let Some(svg) = load_icon_svg(id, max_w, max_h) {
            return Some(svg);
        }

        // B. Desktop entry icon mapping (XDG standard)
        let desktop_map = get_desktop_entry_icons();
        if let Some(icon_name) = desktop_map.get(&id.to_lowercase())
            && let Some(img) = load_app_icon(icon_name, max_w, max_h)
        {
            return Some(img);
        }

        // C. Direct system icon matching by app_id
        if let Some(img) = load_app_icon(id, max_w, max_h) {
            return Some(img);
        }

        // D. Stripped prefix / domain matching (e.g. chrome-app or reverse-DNS)
        let stripped = id
            .strip_prefix("chrome-")
            .or_else(|| id.strip_prefix("chromium-"))
            .unwrap_or(id);
        let domain_core = stripped
            .split(['.', '_', '-'])
            .find(|seg| !seg.is_empty() && *seg != "com" && *seg != "ru" && *seg != "org" && *seg != "net");
        if let Some(core) = domain_core {
            if let Some(svg) = load_icon_svg(core, max_w, max_h) {
                return Some(svg);
            }
            if let Some(img) = load_app_icon(core, max_w, max_h) {
                return Some(img);
            }
        }

        // E. Reverse-DNS last segment (e.g. org.telegram.desktop -> telegram / desktop)
        if let Some(last_seg) = id.split('.').next_back()
            && !last_seg.is_empty()
            && last_seg != id
            && let Some(img) = load_app_icon(last_seg, max_w, max_h)
        {
            return Some(img);
        }
    }

    None
}

pub fn resolve_window_icon(app_id: Option<&str>, title: &str) -> Icon {
    let raw = app_id.unwrap_or(title).to_lowercase();
    if raw.contains("term")
        || raw.contains("kitty")
        || raw.contains("foot")
        || raw.contains("alacritty")
        || raw.contains("console")
        || raw.contains("kgx")
    {
        Icon::Terminal
    } else if raw.contains("firefox")
        || raw.contains("chrom")
        || raw.contains("brave")
        || raw.contains("edge")
        || raw.contains("browser")
        || raw.contains("zen")
    {
        Icon::Browser
    } else if raw.contains("nautilus")
        || raw.contains("thunar")
        || raw.contains("dolphin")
        || raw.contains("file")
        || raw.contains("folder")
    {
        Icon::Folder
    } else if raw.contains("calc") {
        Icon::Calculator
    } else if raw.contains("music") || raw.contains("spotify") {
        Icon::Music
    } else {
        Icon::Window
    }
}

pub use crate::levels::LevelKind;

#[derive(Clone, Copy)]
pub enum Icon {
    Brightness(bool),
    Microphone,
    Keyboard(bool),
    KeyboardDevice,
    Previous,
    PlayPause,
    Play,
    Next,
    Mute,
    Volume(bool),
    Ram,
    Cpu,
    Temperature,
    Window,
    Fullscreen,
    Emoji,
    Palette,
    Screenshot,
    Terminal,
    Browser,
    Folder,
    Calculator,
    Lock,
    Settings,
    Music,
    Search,
    Plug,
    Battery(u32),
    BatteryCharging,
}

const KEY_GAP: u16 = 10;
const KEY_GAP_F: f32 = KEY_GAP as f32;
/// Swipe level feedback: icon slot, gap and meter.
const LEVEL_ICON_SLOT: f32 = 60.0;
const LEVEL_SPACING: f32 = 22.0;
const LEVEL_METER: f32 = 320.0;
const GROUP_GAP: u16 = 56;

/// Previous and next key spans (left, right) of the track overlay.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrackLayout {
    pub prev: (u16, u16),
    pub next: (u16, u16),
}

#[derive(Clone)]
pub struct Button {
    pub left: u16,
    pub right: u16,
    glyph: Glyph,
    pub action: Action,
}

pub struct Layout {
    pub buttons: Vec<Button>,
}

impl Layout {
    pub fn new(mode: Mode, width: u16, physical_escape: bool) -> Self {
        Self::with_custom(mode, width, physical_escape, &[] as &[CustomItem])
    }

    /// Like `new`, but the special row also shows `custom` keys in its
    /// free space next to esc, as many as fit.
    pub fn with_custom<I: Clone + Into<CustomItem>>(
        mode: Mode,
        width: u16,
        physical_escape: bool,
        custom: &[I],
    ) -> Self {
        // Keys inside a group sit KEY_GAP apart. Groups are separated by the
        // wider GROUP_GAP, like the clusters of a hardware function row.
        let mut groups: Vec<Vec<(Glyph, Key)>> = Vec::new();
        match mode {
            Mode::Function => {
                const LABELS: [&str; 12] = [
                    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
                ];
                let keys = [
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
                ];
                groups.push(
                    LABELS
                        .into_iter()
                        .zip(keys)
                        .map(|(label, key)| (Glyph::Text(label), key))
                        .collect(),
                );
            }
            Mode::Media => groups.extend([
                vec![
                    (Glyph::Icon(Icon::Brightness(false)), Key::BrightnessDown),
                    (Glyph::Icon(Icon::Brightness(true)), Key::BrightnessUp),
                ],
                vec![
                    (Glyph::Icon(Icon::Keyboard(false)), Key::IllumDown),
                    (Glyph::Icon(Icon::Keyboard(true)), Key::IllumUp),
                ],
                vec![
                    (Glyph::Icon(Icon::Previous), Key::PreviousSong),
                    (Glyph::Icon(Icon::PlayPause), Key::PlayPause),
                    (Glyph::Icon(Icon::Next), Key::NextSong),
                ],
                vec![
                    (Glyph::Icon(Icon::Microphone), Key::MicMute),
                    (Glyph::Icon(Icon::Mute), Key::Mute),
                    (Glyph::Icon(Icon::Volume(false)), Key::VolumeDown),
                    (Glyph::Icon(Icon::Volume(true)), Key::VolumeUp),
                ],
            ]),
            Mode::Special => {}
        }

        // esc keeps the same slot in every mode, so switching layers only
        // swaps the keys to its right and never moves or resizes it.
        let mut buttons = Vec::new();
        let mut start = KEY_GAP;
        if !physical_escape {
            let right = start + width / 14;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("esc"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        }

        // Media cell width used as standard key width so custom keys have the
        // exact same width and inner padding as media keys.
        let media_cell = {
            let m_count = if physical_escape { 11 } else { 12 };
            let m_groups = 4;
            let m_spacing = KEY_GAP * (m_count - m_groups) + GROUP_GAP * (m_groups - 1);
            let m_usable = width.saturating_sub(start + KEY_GAP + m_spacing);
            m_usable / m_count
        };

        if mode == Mode::Special {
            let usable = width.saturating_sub(start + KEY_GAP);
            let count = custom.len() as u16;
            if count == 0 {
                return Self { buttons };
            }
            let total_gaps = count.saturating_sub(1) * KEY_GAP;
            let avail_w = usable.saturating_sub(total_gaps);

            // Compute glyph and desired width for each custom key.
            // Keys with icons match media_cell so their inner padding is identical to media keys.
            let items: Vec<(Glyph, u16)> = custom
                .iter()
                .map(|raw_item| {
                    let item: CustomItem = (*raw_item).clone().into();
                    let svg = item.svg.or_else(|| {
                        let name = item.icon.unwrap_or(item.label);
                        load_icon_svg(name, 64, 44)
                    });
                    if let Some(svg) = svg {
                        (Glyph::Svg(svg), media_cell)
                    } else if let Some(icon) = resolve_custom_icon(item.label, item.icon) {
                        (Glyph::Icon(icon), media_cell)
                    } else {
                        let char_w: f32 = item
                            .label
                            .chars()
                            .map(|c| if (c as u32) > 0x2e80 { 24.0 } else { 13.5 })
                            .sum();
                        let w = ((char_w + 32.0).round() as u16).max(media_cell);
                        (Glyph::Text(item.label), w)
                    }
                })
                .collect();

            let total_raw: u16 = items.iter().map(|(_, w)| *w).sum();
            let button_widths: Vec<u16> = if total_raw <= avail_w {
                items.iter().map(|(_, w)| *w).collect()
            } else {
                let sum_w = total_raw as f32;
                items
                    .iter()
                    .map(|(_, w)| ((f32::from(*w) / sum_w) * f32::from(avail_w)).max(40.0).round() as u16)
                    .collect()
            };

            let mut left = start;
            for (index, ((glyph, _), &btn_w)) in items.into_iter().zip(&button_widths).enumerate() {
                let right = (left + btn_w).min(width.saturating_sub(KEY_GAP));
                if left >= right {
                    break;
                }
                buttons.push(Button {
                    left,
                    right,
                    glyph,
                    action: Action::Custom(index),
                });
                left = right + KEY_GAP;
            }
            return Self { buttons };
        }

        // The remaining span is filled edge to edge. Rounding leftovers widen
        // the first keys by one pixel instead of shifting the row.
        let count = groups.iter().map(Vec::len).sum::<usize>().max(1) as u16;
        let group_count = groups.len().max(1) as u16;
        let spacing = KEY_GAP * (count - group_count) + GROUP_GAP * (group_count - 1);
        let usable = width.saturating_sub(start + KEY_GAP + spacing);
        let cell = usable / count;
        let mut extra = usable - cell * count;
        let mut left = start;
        for group in groups {
            for (glyph, key) in group {
                let mut right = left + cell;
                if extra > 0 {
                    right += 1;
                    extra -= 1;
                }
                buttons.push(Button {
                    left,
                    right,
                    glyph,
                    action: Action::Key(key),
                });
                left = right + KEY_GAP;
            }
            left += GROUP_GAP - KEY_GAP;
        }
        Self { buttons }
    }

    pub fn suggestions(
        width: u16,
        candidates: &[String],
        typed_prefix: &str,
        physical_escape: bool,
        input_preview: Option<(&str, usize, usize)>,
        clipboard_items: &[String],
    ) -> Self {
        let mut buttons = Vec::new();
        let mut start = KEY_GAP;

        if !physical_escape {
            let right = start + width / 14;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("esc"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        } else {
            // Dismiss button on left edge
            let right = start + 70;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("✕"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        }

        // Left section: macOS-style sentence/line preview capsule
        if let Some((line, cursor_pos, prefix_len)) = input_preview {
            let preview_w = (width / 4).clamp(380, 580);
            let right = start + preview_w;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Preview {
                    text: line.to_string(),
                    cursor_pos,
                    prefix_len,
                },
                action: Action::Preview,
            });
            start = right + GROUP_GAP;
        }

        let btn_w = 54u16;
        let right_limit = width.saturating_sub(KEY_GAP);
        let emoji_left = right_limit.saturating_sub(btn_w);
        let palette_right = emoji_left.saturating_sub(KEY_GAP);
        let palette_left = palette_right.saturating_sub(btn_w);

        if candidates.is_empty() && typed_prefix.is_empty() && !clipboard_items.is_empty() {
            let n = clipboard_items.len().min(4);
            let avail_w = palette_left.saturating_sub(start + GROUP_GAP);
            let total_gaps = KEY_GAP * (n as u16 - 1);
            let cell_w = avail_w.saturating_sub(total_gaps) / n as u16;

            for (i, clip_text) in clipboard_items.iter().take(n).enumerate() {
                let left = start + (i as u16) * (cell_w + KEY_GAP);
                let right = if i == n - 1 {
                    palette_left.saturating_sub(KEY_GAP)
                } else {
                    left + cell_w
                };
                buttons.push(Button {
                    left,
                    right,
                    glyph: Glyph::Clipboard(clip_text.clone()),
                    action: Action::Clipboard(i),
                });
            }
        } else if !candidates.is_empty() {
            let n = candidates.len().clamp(1, 5);
            let avail_w = palette_left.saturating_sub(start + GROUP_GAP);
            let total_gaps = KEY_GAP * (n as u16 - 1);
            let cell_w = avail_w.saturating_sub(total_gaps) / n as u16;

            let highlights = compute_suggestion_highlights(candidates, typed_prefix);

            for (i, word) in candidates.iter().take(n).enumerate() {
                let left = start + (i as u16) * (cell_w + KEY_GAP);
                let right = if i == n - 1 {
                    palette_left.saturating_sub(KEY_GAP)
                } else {
                    left + cell_w
                };
                let (match_len, is_typo_fix) = highlights.get(i).copied().unwrap_or((0, false));
                buttons.push(Button {
                    left,
                    right,
                    glyph: Glyph::Suggestion {
                        text: word.clone(),
                        match_len,
                        is_typo_fix,
                    },
                    action: Action::Suggestion(i),
                });
            }
        }

        buttons.push(Button {
            left: palette_left,
            right: palette_right,
            glyph: Glyph::Icon(Icon::Palette),
            action: Action::OpenColorPicker,
        });

        buttons.push(Button {
            left: emoji_left,
            right: right_limit,
            glyph: Glyph::Icon(Icon::Emoji),
            action: Action::ToggleEmojiStrip,
        });

        Self { buttons }
    }

    pub fn color_picker(width: u16, hue: f32, physical_escape: bool, copied: bool) -> Self {
        let mut buttons = Vec::new();
        let mut start = KEY_GAP;

        if !physical_escape {
            let right = start + width / 14;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("esc"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        } else {
            let right = start + 70;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("✕"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        }

        let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
        let right_limit = width.saturating_sub(KEY_GAP);
        let swatch_w = 240u16;
        let swatch_left = right_limit.saturating_sub(swatch_w);
        let ribbon_right = swatch_left.saturating_sub(GROUP_GAP);

        buttons.push(Button {
            left: start,
            right: ribbon_right,
            glyph: Glyph::ColorRibbon { hue },
            action: Action::ColorPicker,
        });

        buttons.push(Button {
            left: swatch_left,
            right: right_limit,
            glyph: Glyph::ColorSwatch { r, g, b, copied },
            action: Action::ColorCopy,
        });

        Self { buttons }
    }

    #[allow(dead_code)]
    pub fn windows(
        width: u16,
        windows: &[niri::NiriWindowItem],
        current_workspace: u32,
        show_all: bool,
        physical_escape: bool,
    ) -> Self {
        let mut buttons = Vec::new();
        let mut start = KEY_GAP;

        if !physical_escape {
            let right = start + width / 14;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("esc"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        } else {
            let right = start + 70;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("✕"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        }

        let filter_w = 110u16;
        let right = start + filter_w;
        let filter_label = if show_all { "Все" } else { "Текущий" };
        buttons.push(Button {
            left: start,
            right,
            glyph: Glyph::Dynamic(format!("⊞ {filter_label}")),
            action: Action::ToggleWorkspaceFilter,
        });
        start = right + GROUP_GAP;

        for ws_idx in 1..=4u32 {
            let ws_w = 42u16;
            let right = start + ws_w;
            let label = if ws_idx == current_workspace {
                format!("•{ws_idx}•")
            } else {
                format!("{ws_idx}")
            };
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Dynamic(label),
                action: Action::FocusWorkspace(ws_idx),
            });
            start = right + KEY_GAP;
        }
        start += GROUP_GAP - KEY_GAP;

        let filtered: Vec<&niri::NiriWindowItem> = windows
            .iter()
            .filter(|w| {
                if show_all {
                    true
                } else if let Some(ws_id) = w.workspace_id {
                    ws_id == current_workspace as u64
                } else {
                    true
                }
            })
            .collect();

        if filtered.is_empty() {
            let right = width.saturating_sub(KEY_GAP);
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("Нет окон"),
                action: Action::Preview,
            });
            return Self { buttons };
        }

        let count = filtered.len().min(8) as u16;
        let avail_w = width.saturating_sub(start + KEY_GAP);
        let total_gaps = KEY_GAP * (count.saturating_sub(1));
        let cell_w = (avail_w.saturating_sub(total_gaps) / count).clamp(80, 260);

        for (i, win) in filtered.iter().take(count as usize).enumerate() {
            let left = start + (i as u16) * (cell_w + KEY_GAP);
            let right = if i == (count as usize - 1) {
                (left + cell_w).min(width.saturating_sub(KEY_GAP))
            } else {
                left + cell_w
            };
            let title = win.title.clone().unwrap_or_else(|| {
                win.app_id.clone().unwrap_or_else(|| "Window".to_string())
            });
            buttons.push(Button {
                left,
                right,
                glyph: Glyph::WindowItem {
                    title,
                    app_id: win.app_id.clone(),
                    is_focused: win.is_focused,
                },
                action: Action::FocusWindow(win.id),
            });
        }

        Self { buttons }
    }

    pub fn emoji_and_punctuation(width: u16, physical_escape: bool, scroll_offset: f32) -> Self {
        let mut buttons = Vec::new();
        let mut start = KEY_GAP;

        if !physical_escape {
            let right = start + width / 14;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("esc"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        } else {
            let right = start + 70;
            buttons.push(Button {
                left: start,
                right,
                glyph: Glyph::Text("✕"),
                action: Action::Key(Key::Esc),
            });
            start = right + GROUP_GAP;
        }

        let abc_w = 64u16;
        let right = start + abc_w;
        buttons.push(Button {
            left: start,
            right,
            glyph: Glyph::Text("ABC"),
            action: Action::ToggleEmojiStrip,
        });
        start = right + GROUP_GAP;

        let emoji_start_boundary = start;
        let emoji_end_boundary = width.saturating_sub(KEY_GAP);
        let emoji_w = EMOJI_BTN_WIDTH;
        let emoji_gap = EMOJI_BTN_GAP;

        let total_emojis = POPULAR_EMOJIS.len();
        for idx in 0..total_emojis {
            let btn_left = emoji_start_boundary as f32 + (idx as f32) * (emoji_w + emoji_gap) - scroll_offset;
            let btn_right = btn_left + emoji_w;

            if btn_right < emoji_start_boundary as f32 {
                continue;
            }
            if btn_left >= emoji_end_boundary as f32 {
                break;
            }

            let left = btn_left.round().max(emoji_start_boundary as f32) as u16;
            let right = btn_right.round().min(emoji_end_boundary as f32) as u16;
            if right > left + 14 {
                buttons.push(Button {
                    left,
                    right,
                    glyph: Glyph::Emoji(POPULAR_EMOJIS[idx]),
                    action: Action::Emoji(idx),
                });
            }
        }

        Self { buttons }
    }

    pub fn hit(&self, x: f64, y: f64, height: u16) -> Option<usize> {
        if y < 1.0 || y > f64::from(height.saturating_sub(1)) {
            return None;
        }
        self.buttons
            .iter()
            .position(|button| {
                // Let the virtual Esc key respond all the way to the left edge
                let left = if button.action == Action::Key(Key::Esc) {
                    0.0
                } else {
                    f64::from(button.left)
                };
                x >= left && x < f64::from(button.right)
            })
    }
}

/// Compute matching prefix/root highlight lengths and typo correction flags for candidates.
pub fn compute_suggestion_highlights(
    candidates: &[String],
    typed_prefix: &str,
) -> Vec<(usize, bool)> {
    let p_clean = typed_prefix.trim().to_lowercase();
    let p_chars: Vec<char> = p_clean.chars().collect();

    if p_chars.is_empty() {
        return candidates.iter().map(|_| (0, false)).collect();
    }

    let prefix_matches: Vec<bool> = candidates
        .iter()
        .map(|c| c.to_lowercase().starts_with(&p_clean))
        .collect();

    let matching_candidates: Vec<Vec<char>> = candidates
        .iter()
        .zip(prefix_matches.iter())
        .filter_map(|(c, &m)| if m { Some(c.to_lowercase().chars().collect()) } else { None })
        .collect();

    let cluster_common_len = if matching_candidates.len() >= 2 {
        let first = &matching_candidates[0];
        let mut common = first.len();
        for other in &matching_candidates[1..] {
            let shared = first.iter().zip(other.iter()).take_while(|(a, b)| a == b).count();
            common = common.min(shared);
        }
        common
    } else {
        p_chars.len()
    };

    candidates
        .iter()
        .zip(prefix_matches.iter())
        .map(|(word, &starts_with_p)| {
            let w_chars: Vec<char> = word.to_lowercase().chars().collect();
            if starts_with_p {
                let match_len = cluster_common_len.max(p_chars.len()).min(w_chars.len());
                (match_len, false)
            } else {
                let shared = p_chars.iter().zip(w_chars.iter()).take_while(|(a, b)| a == b).count();
                let is_typo = p_chars.len() >= 3 && (shared >= 1 || p_chars.len() <= w_chars.len() + 2);
                (shared, is_typo)
            }
        })
        .collect()
}

/// Variable sans fonts that match the San Francisco legends of the keyboard
/// closely enough. Adwaita Sans ships with GNOME on Fedora.
const FONT_PATHS: [&str; 6] = [
    "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/rsms-inter-vf-fonts/InterVariable.ttf",
    "/usr/share/fonts/abattis-cantarell-vf-fonts/Cantarell-VF.otf",
    "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/inter/InterVariable.ttf",
    "/usr/share/fonts/cantarell/Cantarell-VF.otf",
];
const FONT_WEIGHT: f32 = 500.0;
/// Emphasis in the Touch ID prompt ("**Unlock** with Touch ID").
const STRONG_WEIGHT: f32 = 700.0;
const PROMPT_MARGIN: f32 = 18.0;
const TRACK_FONT_SIZE: f32 = 24.0;
const TRACK_GAP: f32 = 40.0;
const ARROW_LENGTH: f32 = 22.0;
const ARROW_GAP: f32 = 14.0;
const ARROW_STROKE: f32 = 3.0;
/// How far the arrow's tail moves towards the sensor. The tip moves twice as
/// far.
const ARROW_TAIL_TRAVEL: f32 = 7.0;
const KEY_FONT_SIZE: f32 = 23.0;

/// Icons are drawn on a 24 px design grid and centered by their bounds so
/// every key gets the same optical margin.
const ICON_SCALE: f32 = 1.3;
const ICON_STROKE: f32 = 2.3;
const KEY_INSET: f32 = 2.0;
const KEY_RADIUS: f32 = 7.0;
const KEY_OUTLINE: f32 = 1.5;

pub fn is_emoji_char(c: char) -> bool {
    let u = c as u32;
    matches!(
        u,
        0x1F000..=0x1FAFF
        | 0x2600..=0x27BF
        | 0x2300..=0x23FF
        | 0x2B00..=0x2BFF
        | 0x2000..=0x206F
        | 0x2190..=0x21FF
        | 0x2900..=0x297F
        | 0x3000..=0x303F
        | 0x3200..=0x32FF
    )
}

pub fn is_ignorable_format_or_control(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    matches!(
        c,
        '\u{200B}'..='\u{200F}' // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{206F}' // WJ, invisible math, isolates (LRI, RLI, FSI, PDI)
        | '\u{FE00}'..='\u{FE0F}' // Variation selectors
        | '\u{FEFF}'             // BOM / Zero-width no-break space
        | '\u{061C}'             // Arabic letter mark
        | '\u{180E}'             // Mongolian vowel separator
        | '\u{FFF9}'..='\u{FFFB}' // Interlinear annotation
        | '\u{E0000}'..='\u{E007F}' // Language tags
        | '\u{E0100}'..='\u{E01EF}' // Variation selectors supplement
    )
}

pub fn clean_display_text(s: &str) -> String {
    s.chars().filter(|&c| !is_ignorable_format_or_control(c)).collect()
}

fn is_emoji_modifier(c: char) -> bool {
    is_ignorable_format_or_control(c)
}

fn load_emoji_face() -> Option<freetype::Face> {
    let lib = freetype::Library::init().ok()?;
    const EMOJI_PATHS: &[&str] = &[
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/google-noto-color-emoji-fonts/NotoColorEmoji.ttf",
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/TTF/NotoColorEmoji.ttf",
    ];
    for path in EMOJI_PATHS {
        if let Ok(face) = lib.new_face(*path, 0) {
            let _ = face.select_size(0);
            return Some(face);
        }
    }
    None
}

type EmojiCache = RefCell<HashMap<char, (Pixmap, f32, f32, f32)>>;

struct MarqueeCache {
    text: String,
    size: f32,
    color_rgb: (u8, u8, u8),
    pixmap: Pixmap,
    loop_dist: f32,
    start_ms: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct IdleBaseKey {
    has_esc: bool,
    active_esc: bool,
    has_media_capsule: bool,
    clock_time: String,
    clock_weekday: String,
    clock_date: String,
    clock_idx: usize,
    battery: Option<status::BatteryStatus>,
    battery_idx: usize,
    cpu_percent: Option<u32>,
    ram_percent: Option<u32>,
    cpu_temp: Option<u32>,
    sysmon_idx: usize,
    volume: Option<(u32, bool)>,
    brightness: Option<u32>,
    sound_brt_idx: usize,
    keyboard_layout: Option<String>,
    level: u32,
    shift: (i32, i32),
}

struct IdleBaseCache {
    key: IdleBaseKey,
    pixmap: Pixmap,
    left_limit: f32,
    right_limit: f32,
}

#[allow(dead_code)]
#[derive(Clone)]
pub enum CenterIcon {
    Icon(Icon),
    Svg(Arc<Pixmap>),
    None,
}

pub struct Canvas {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
    pixmap: Pixmap,
    font: FontVec,
    fallback_fonts: Vec<FontVec>,
    emoji_face: Option<freetype::Face>,
    emoji_cache: EmojiCache,
    marquee_cache: Option<MarqueeCache>,
    #[allow(dead_code)]
    cached_win_icon: Option<(Option<String>, String, CenterIcon)>,
    idle_base_cache: Option<IdleBaseCache>,
    /// The configured key color at full intensity.
    base: Rgb,
    /// `base` dimmed to the current key level.
    color: Rgb,
    pub level: u32,
    pub key_font_size: f32,
    pub track_font_size: f32,
    pub status_date_size: f32,
    pub status_text_size: f32,
    pub theme: Theme,
    pub media_capsule_rect: Option<(f32, f32)>,
    pub window_capsule_rect: Option<(f32, f32)>,
    pub status_workspace_buttons: Vec<(u32, f32, f32)>,
    pub status_window_buttons: Vec<(u64, f32, f32)>,
    pub workspace_windows: Vec<niri::NiriWindowItem>,
}

pub use crate::theme::{BLACK, Rgb, Theme};

struct CyclingItem {
    icon: Icon,
    text: String,
    color: Rgb,
}

pub const CYCLING_STEP_MS: u64 = 5000;

impl Canvas {
    pub fn new(
        width: u16,
        height: u16,
        key_color: u32,
        font_name: Option<&str>,
        font_size: Option<f32>,
        status_font_size: Option<f32>,
        theme: Option<Theme>,
    ) -> Result<Self> {
        let pixmap = Pixmap::new(u32::from(width), u32::from(height))
            .ok_or_else(|| anyhow!("invalid canvas size {width}x{height}"))?;
        let key_font_size = font_size.unwrap_or(KEY_FONT_SIZE);
        let track_font_size = font_size
            .map(|s| s * (TRACK_FONT_SIZE / KEY_FONT_SIZE))
            .unwrap_or(TRACK_FONT_SIZE);
        let status_text_size = status_font_size
            .or_else(|| font_size.map(|s| s * (20.0 / 23.0)))
            .unwrap_or(20.0);
        let status_date_size = status_text_size;
        let theme = theme.unwrap_or_default();
        let base_rgb = theme.keys.color.unwrap_or_else(|| Rgb::from_u32(key_color));
        let mut canvas = Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
            pixmap,
            font: load_font(font_name)?,
            fallback_fonts: load_fallback_fonts(),
            emoji_face: load_emoji_face(),
            emoji_cache: RefCell::new(HashMap::new()),
            marquee_cache: None,
            cached_win_icon: None,
            idle_base_cache: None,
            base: base_rgb,
            color: base_rgb,
            level: 100,
            key_font_size,
            track_font_size,
            status_date_size,
            status_text_size,
            theme,
            media_capsule_rect: None,
            window_capsule_rect: None,
            status_workspace_buttons: Vec::new(),
            status_window_buttons: Vec::new(),
            workspace_windows: Vec::new(),
        };
        canvas.clear();
        Ok(canvas)
    }

    pub fn set_theme(&mut self, theme: Theme) {
        if self.theme != theme {
            if let Some(col) = theme.keys.color {
                self.base = col;
                self.color = col.scale(self.level);
            }
            self.theme = theme;
            self.idle_base_cache = None;
            self.marquee_cache = None;
        }
    }

    fn glyph_font_and_id_impl<'a>(
        font: &'a FontVec,
        fallback_fonts: &'a [FontVec],
        c: char,
    ) -> (&'a FontVec, ab_glyph::GlyphId) {
        let id = font.glyph_id(c);
        if id.0 != 0 {
            return (font, id);
        }
        for fallback in fallback_fonts {
            let fid = fallback.glyph_id(c);
            if fid.0 != 0 {
                return (fallback, fid);
            }
        }
        (font, id)
    }

    fn glyph_font_and_id(&self, c: char) -> (&FontVec, ab_glyph::GlyphId) {
        Self::glyph_font_and_id_impl(&self.font, &self.fallback_fonts, c)
    }

    /// Dims everything drawn in the key color, in percent of full intensity.
    pub fn set_level(&mut self, percent: u32) {
        let new_level = percent.min(100);
        if self.level != new_level {
            self.level = new_level;
            self.color = self.base.scale(self.level);
            self.idle_base_cache = None;
        }
    }

    pub fn clear(&mut self) {
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);
        self.pixels.fill(0);
    }

    pub fn keys(&mut self, layout: &Layout, active: Option<usize>) {
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);
        for (index, button) in layout.buttons.iter().enumerate() {
            self.button(button, active == Some(index));
        }
        self.flush();
    }

    /// Level feedback for a swipe on the dark bar: icon and meter beside the
    /// fingers. `fingers` is the x range from the leftmost to the rightmost
    /// finger. `right` is how much of it shows on their right side (0 to 1),
    /// the rest shows on their left, so a side change can crossfade.
    pub fn level(
        &mut self,
        kind: LevelKind,
        percent: Option<u32>,
        muted: bool,
        fingers: (f32, f32),
        right: f32,
        fade: u32,
    ) {
        // From the touch point of the finger nearest to the feedback, so any
        // number of fingers keeps the same clearance.
        const DISTANCE: f32 = 110.0;
        const TEXT_GAP: f32 = 16.0;
        let text_size = self.status_text_size;
        let text_slot = self.text_width("100%", text_size) + 8.0;
        let total = LEVEL_ICON_SLOT + LEVEL_SPACING + LEVEL_METER + TEXT_GAP + text_slot;
        let width = f32::from(self.width);
        let (first, last) = fingers;
        let full = self.color;
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);
        let right = right.clamp(0.0, 1.0);
        for (left, share) in [
            (first - DISTANCE - total, 1.0 - right),
            (last + DISTANCE, right),
        ] {
            let opacity = (fade.min(100) as f32 * share).round() as u32;
            if opacity == 0 {
                continue;
            }
            let left = left.clamp(KEY_GAP_F, width - KEY_GAP_F - total).round();
            self.color = full.scale(opacity);
            self.level_at(left, kind, percent, muted);
        }
        self.flush();
        self.color = full;
    }

    fn level_at(&mut self, left: f32, kind: LevelKind, percent: Option<u32>, muted: bool) {
        const ICON_SIZE: f32 = 1.6;
        const METER_HEIGHT: f32 = 10.0;
        const TEXT_GAP: f32 = 16.0;
        let cy = f32::from(self.height) / 2.0;
        let icon = match (kind, muted) {
            (LevelKind::Volume, true) => Icon::Mute,
            (LevelKind::Volume, false) => Icon::Volume(true),
            (LevelKind::Brightness, _) => Icon::Brightness(true),
        };
        self.icon_sized(left + LEVEL_ICON_SLOT / 2.0, cy, icon, BLACK, ICON_SIZE, self.color);

        let meter_left = left + LEVEL_ICON_SLOT + LEVEL_SPACING;
        let (top, bottom) = (cy - METER_HEIGHT / 2.0, cy + METER_HEIGHT / 2.0);
        let radius = METER_HEIGHT / 2.0;
        let track = rounded_rect(meter_left, top, meter_left + LEVEL_METER, bottom, radius);
        self.fill(&track, self.color.scale(22));
        let fraction = percent.map_or(0.0, |percent| percent.min(100) as f32 / 100.0);
        let fill_color = self.theme.levels.level_meter.map(|c| c.scale(self.level)).unwrap_or(self.color);
        if fraction > 0.0 && !muted {
            let right = meter_left + (LEVEL_METER * fraction).max(METER_HEIGHT);
            self.fill(
                &rounded_rect(meter_left, top, right, bottom, radius),
                fill_color,
            );
        }

        let text_size = self.status_text_size;
        let text_str = if muted {
            "0%".to_string()
        } else {
            percent.map_or_else(|| "--%".to_string(), |p| format!("{p}%"))
        };
        let text_x = meter_left + LEVEL_METER + TEXT_GAP;
        let baseline = (cy + self.cap_height(text_size) / 2.0).round();
        let text_color = self.theme.levels.level_text.map(|c| c.scale(self.level)).unwrap_or(self.color);
        self.draw_text_on_main(
            text_x,
            baseline,
            &text_str,
            text_size,
            FONT_WEIGHT,
            text_color,
        );
    }

    /// "**Artist** – Title" on the dark bar with borderless previous/next
    /// keys at `keys`, the fixed slots at both ends of the bar. `fade`
    /// scales everything (percent) for the fade in and out; `pressed`
    /// highlights next (`true`) or previous (`false`). `esc` is the esc
    /// key with its own opacity, drawn while it is shown on the dark bar.
    pub fn track(
        &mut self,
        artist: &str,
        title: &str,
        fade: u32,
        pressed: Option<bool>,
        keys: TrackLayout,
        esc: Option<(Button, u32)>,
    ) {
        let space_left = f32::from(keys.prev.1) + TRACK_GAP;
        let space_right = f32::from(keys.next.0) - TRACK_GAP;
        let (artist, title) = self.fit_track(artist, title, space_right - space_left);
        let separator = if artist.is_empty() { "" } else { " – " };
        let parts = [
            (artist.as_str(), STRONG_WEIGHT),
            (separator, FONT_WEIGHT),
            (title.as_str(), FONT_WEIGHT),
        ];
        let track_size = self.track_font_size;
        let text_width: f32 = parts
            .iter()
            .map(|(text, weight)| self.text_width_at(text, track_size, *weight))
            .sum();

        let full = self.color;
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);
        if let Some((esc, opacity)) = esc {
            self.color = full.scale(opacity.min(100));
            self.button(&esc, false);
        }
        self.color = full.scale(fade.min(100));
        let top = KEY_INSET;
        let bottom = f32::from(self.height) - KEY_INSET;
        let cy = f32::from(self.height) / 2.0;
        for ((left, right), icon, forward) in [
            (keys.prev, Icon::Previous, false),
            (keys.next, Icon::Next, true),
        ] {
            let (left, right) = (f32::from(left), f32::from(right));
            let background = if pressed == Some(forward) {
                let fill = self.color.scale(20);
                self.fill(&rounded_rect(left, top, right, bottom, KEY_RADIUS), fill);
                fill
            } else {
                BLACK
            };
            self.icon((left + right) / 2.0, cy, icon, background);
        }
        let track_base = self.theme.track_popup.color.unwrap_or(Rgb(255, 140, 0));
        let track_color = track_base
            .scale(self.level)
            .scale(fade.min(100));
        let baseline = (cy + self.cap_height(track_size) / 2.0).round();
        let mut x = ((space_left + space_right - text_width) / 2.0).round();
        for (text, weight) in parts {
            x = self.draw_text_on_main(x, baseline, text, track_size, weight, track_color);
        }
        self.color = full;
        self.flush();
    }

    /// Shortens the title, then the artist, with an ellipsis until the line
    /// fits.
    fn fit_track(&mut self, artist: &str, title: &str, max: f32) -> (String, String) {
        let track_size = self.track_font_size;
        let mut artist: Vec<char> = artist.chars().collect();
        let mut title: Vec<char> = title.chars().collect();
        let (mut artist_cut, mut title_cut) = (false, false);
        loop {
            let shown = |chars: &[char], cut: bool| {
                let mut text: String = chars.iter().collect();
                if cut {
                    text = format!("{}…", text.trim_end());
                }
                text
            };
            let (a, t) = (shown(&artist, artist_cut), shown(&title, title_cut));
            let separator = if a.is_empty() {
                0.0
            } else {
                self.text_width_at(" – ", track_size, FONT_WEIGHT)
            };
            let width = self.text_width_at(&a, track_size, STRONG_WEIGHT)
                + separator
                + self.text_width_at(&t, track_size, FONT_WEIGHT);
            if width <= max || (artist.len() <= 1 && title.len() <= 1) {
                return (a, t);
            }
            if title.len() > 12 || artist.len() <= 12 {
                title.pop();
                title_cut = true;
            } else {
                artist.pop();
                artist_cut = true;
            }
        }
    }

    /// One key on an otherwise black bar, with optional pressed styling and
    /// opacity for the activity escape key.
    pub fn single_key(&mut self, layout: &Layout, index: usize, active: bool, fade: u32) {
        let full = self.color;
        let base_esc = self.theme.keys.esc_color.unwrap_or(self.base);
        self.color = base_esc.scale(self.level).scale(fade.min(100));
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);
        if let Some(button) = layout.buttons.get(index) {
            self.button(button, active);
        }
        self.flush();
        self.color = full;
    }

    fn button(&mut self, button: &Button, active: bool) {
        let top = KEY_INSET;
        let bottom = f32::from(self.height) - KEY_INSET;
        let cy = f32::from(self.height) / 2.0;
        let (left, right) = (f32::from(button.left), f32::from(button.right));
        let mut background = BLACK;
        let is_emoji = matches!(button.action, Action::Emoji(_));
        let is_typo = matches!(button.glyph, Glyph::Suggestion { is_typo_fix: true, .. });
        let is_recommended = matches!(button.action, Action::Suggestion(0)) && !is_typo;

        if !matches!(button.glyph, Glyph::ColorRibbon { .. } | Glyph::ColorSwatch { .. }) {
            background = if active {
                let fill = self.theme.keys.active_color.unwrap_or_else(|| self.color.scale(20));
                let (btn_top, btn_bottom) = if is_emoji {
                    (4.0, f32::from(self.height) - 4.0)
                } else {
                    (top, bottom)
                };
                self.fill(&rounded_rect(left, btn_top, right, btn_bottom, KEY_RADIUS), fill);
                fill
            } else if is_emoji {
                BLACK
            } else if is_typo {
                // Colored amber background for autocorrect / typo fix suggestions
                let fill = Rgb(255, 149, 0).scale(self.level as u32).scale(48);
                self.fill(&rounded_rect(left, top, right, bottom, KEY_RADIUS), fill);
                fill
            } else if is_recommended {
                // Colored blue / accent background for primary recommended suggestion
                let rec_base = self.theme.keys.active_color.unwrap_or(Rgb(10, 132, 255));
                let fill = rec_base.scale(self.level as u32).scale(46);
                self.fill(&rounded_rect(left, top, right, bottom, KEY_RADIUS), fill);
                fill
            } else if matches!(button.action, Action::Clipboard(_)) {
                let fill = Rgb(20, 32, 54).scale(self.level as u32).scale(60);
                self.fill(&rounded_rect(left, top, right, bottom, KEY_RADIUS), fill);
                fill
            } else if matches!(button.action, Action::Preview) {
                let fill = self.color.scale(5);
                self.fill(&rounded_rect(left, top, right, bottom, KEY_RADIUS), fill);
                fill
            } else {
                BLACK
            };

            // Remove button outline completely for emoji strip buttons!
            if !is_emoji {
                let half = KEY_OUTLINE / 2.0;
                let outline_scale = if active {
                    34
                } else if matches!(button.action, Action::Clipboard(_)) {
                    24
                } else if matches!(button.action, Action::Preview) {
                    12
                } else {
                    22
                };
                let outline_color = if active {
                    self.color.scale(outline_scale)
                } else if is_typo {
                    Rgb(255, 149, 0).scale(self.level as u32).scale(120)
                } else if is_recommended {
                    let rec_base = self.theme.keys.active_color.unwrap_or(Rgb(10, 132, 255));
                    rec_base.scale(self.level as u32).scale(110)
                } else {
                    self.color.scale(outline_scale)
                };
                self.stroke(
                    &rounded_rect(
                        left + half,
                        top + half,
                        right - half,
                        bottom - half,
                        KEY_RADIUS,
                    ),
                    outline_color,
                    KEY_OUTLINE,
                );
            }
        }
        let cx = (left + right) / 2.0;
        match &button.glyph {
            Glyph::Text(text) => {
                self.draw_button_text(cx, cy, left, right, text);
            }
            Glyph::Dynamic(text) => {
                self.draw_button_text(cx, cy, left, right, text);
            }
            Glyph::Suggestion { text, match_len, is_typo_fix } => {
                self.draw_suggestion(cx, cy, left, right, text, *match_len, *is_typo_fix);
            }
            Glyph::Preview { text, cursor_pos, prefix_len } => {
                self.draw_preview_line(left, right, cy, text, *cursor_pos, *prefix_len);
            }
            Glyph::Icon(icon) => self.icon(cx, cy, *icon, background),
            Glyph::Svg(pixmap) => {
                let px = (cx - pixmap.width() as f32 / 2.0).round() as i32;
                let py = (cy - pixmap.height() as f32 / 2.0).round() as i32;
                let paint = tiny_skia::PixmapPaint {
                    opacity: (self.level as f32) / 100.0,
                    blend_mode: tiny_skia::BlendMode::SourceOver,
                    quality: tiny_skia::FilterQuality::Bilinear,
                };
                self.pixmap.draw_pixmap(px, py, pixmap.as_ref().as_ref(), &paint, Transform::identity(), None);
            }
            Glyph::ColorRibbon { hue } => {
                self.draw_color_ribbon(left, right, *hue);
            }
            Glyph::ColorSwatch { r, g, b, copied } => {
                self.draw_color_swatch(left, right, cy, *r, *g, *b, *copied);
            }
            Glyph::WindowItem { title, app_id, is_focused } => {
                self.draw_window_item(left, right, cy, title, app_id.as_deref(), *is_focused);
            }
            Glyph::Emoji(emoji) => {
                self.draw_emoji(cx, cy, emoji, EMOJI_GLYPH_SIZE);
            }
            Glyph::Clipboard(text) => {
                self.draw_clipboard_item(left, right, cy, text);
            }
        }
    }

    fn draw_button_text(&mut self, cx: f32, cy: f32, left: f32, right: f32, text: &str) {
        let max_text_w = (right - left - 16.0).max(10.0);
        let cur_text_w = self.text_width(text, self.key_font_size);
        let mut font_size = self.key_font_size;
        if cur_text_w > max_text_w {
            font_size = (self.key_font_size * (max_text_w / cur_text_w) * 0.95).max(11.0);
        }
        while font_size > 11.0 && self.text_width(text, font_size) > max_text_w {
            font_size -= 1.0;
        }
        if self.text_width(text, font_size) > max_text_w {
            let truncated = self.truncate_text(text, font_size, max_text_w);
            self.center_text(cx, cy, &truncated, font_size, self.color);
        } else {
            self.center_text(cx, cy, text, font_size, self.color);
        }
    }

    pub fn draw_emoji(&mut self, cx: f32, cy: f32, emoji_str: &str, size: f32) {
        let ch = emoji_str
            .chars()
            .find(|&c| !is_ignorable_format_or_control(c) && is_emoji_char(c))
            .or_else(|| emoji_str.chars().next());
        if let Some(ch) = ch
            && let Some((emoji_pixmap, _adv, _left, _top)) = self.get_or_load_emoji(ch)
        {
            let w = emoji_pixmap.width() as f32;
            let h = emoji_pixmap.height() as f32;
            let scale = size / h.max(1.0);
            let dest_x = (cx - (w * scale) / 2.0).round();
            let dest_y = (cy - (h * scale) / 2.0).round();
            let transform = Transform::from_scale(scale, scale).post_translate(dest_x, dest_y);
            let paint = tiny_skia::PixmapPaint {
                opacity: (self.level as f32) / 100.0,
                blend_mode: tiny_skia::BlendMode::SourceOver,
                quality: tiny_skia::FilterQuality::Bilinear,
            };
            self.pixmap.draw_pixmap(0, 0, emoji_pixmap.as_ref(), &paint, transform, None);
            return;
        }
        self.center_text(cx, cy, emoji_str, size, self.color);
    }

    fn draw_suggestion(
        &mut self,
        cx: f32,
        cy: f32,
        left: f32,
        right: f32,
        text: &str,
        match_len: usize,
        _is_typo_fix: bool,
    ) {
        let max_text_w = (right - left - 16.0).max(10.0);
        let cur_text_w = self.text_width(text, self.key_font_size);
        let mut font_size = self.key_font_size;
        if cur_text_w > max_text_w {
            font_size = (self.key_font_size * (max_text_w / cur_text_w) * 0.95).max(11.0);
        }
        while font_size > 11.0 && self.text_width(text, font_size) > max_text_w {
            font_size -= 1.0;
        }

        let chars: Vec<char> = text.chars().collect();
        let display_text;
        let effective_match_len;

        if self.text_width(text, font_size) > max_text_w {
            let truncated = self.truncate_text(text, font_size, max_text_w);
            let trunc_chars: Vec<char> = truncated.chars().collect();
            effective_match_len = match_len.min(trunc_chars.len());
            display_text = truncated;
        } else {
            effective_match_len = match_len.min(chars.len());
            display_text = text.to_string();
        }

        let yellow = self.theme.keys.suggestion_match_color
            .unwrap_or(Rgb(255, 214, 10))
            .scale(self.level as u32);
        let default_color = self.color;

        if effective_match_len == 0 {
            self.center_text(cx, cy, &display_text, font_size, default_color);
        } else {
            let d_chars: Vec<char> = display_text.chars().collect();
            let m = effective_match_len.min(d_chars.len());
            let prefix: String = d_chars[..m].iter().collect();
            let suffix: String = d_chars[m..].iter().collect();

            let total_w = self.text_width(&display_text, font_size);
            let start_x = (cx - total_w / 2.0).round();
            let baseline = (cy + self.cap_height(font_size) / 2.0).round();

            let mut next_x = start_x;
            if !prefix.is_empty() {
                let mut prefix_path = PathBuilder::new();
                next_x = self.append_text(&mut prefix_path, next_x, baseline, &prefix, font_size, FONT_WEIGHT);
                if let Some(path) = prefix_path.finish() {
                    self.fill(&path, yellow);
                }
            }
            if !suffix.is_empty() {
                let mut suffix_path = PathBuilder::new();
                self.append_text(&mut suffix_path, next_x, baseline, &suffix, font_size, FONT_WEIGHT);
                if let Some(path) = suffix_path.finish() {
                    self.fill(&path, default_color);
                }
            }
        }
    }

    fn draw_preview_line(
        &mut self,
        left: f32,
        right: f32,
        cy: f32,
        text: &str,
        cursor_pos: usize,
        prefix_len: usize,
    ) {
        let pad = 14.0;
        let max_w = (right - left - pad * 2.0).max(10.0);
        // Matching button text (key_font_size is ~23px), making line preview large and crisp
        let font_size = (self.key_font_size * 1.05).round().clamp(22.0, 25.0);

        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let c = cursor_pos.min(n);

        let mut start = 0;
        let mut end = n;
        let cursor_width = 6.0; // 2.0px bar + 2px side margins

        // Slide the visible text window so cursor stays centered/in-view
        while start < c || end > c {
            let p_dots = if start > 0 { "…" } else { "" };
            let s_dots = if end < n { "…" } else { "" };
            let before: String = chars[start..c].iter().collect();
            let after: String = chars[c..end].iter().collect();

            let before_str = format!("{}{}", p_dots, before);
            let after_str = format!("{}{}", after, s_dots);

            let w_before = self.text_width(&before_str, font_size);
            let w_after = self.text_width(&after_str, font_size);
            let total_w = w_before + cursor_width + w_after;

            if total_w <= max_w {
                break;
            }

            if (c - start) > (end - c) && start < c {
                start += 1;
            } else if end > c {
                end -= 1;
            } else if start < c {
                start += 1;
            } else {
                break;
            }
        }

        let p_dots = if start > 0 { "…" } else { "" };
        let s_dots = if end < n { "…" } else { "" };
        let before_full = format!("{}{}", p_dots, chars[start..c].iter().collect::<String>());
        let after_str = format!("{}{}", chars[c..end].iter().collect::<String>(), s_dots);

        let w_before = self.text_width(&before_full, font_size);
        let w_after = self.text_width(&after_str, font_size);
        let total_w = w_before + cursor_width + w_after;

        let x = if total_w < max_w {
            left + pad
        } else {
            right - pad - total_w
        };

        let baseline = (cy + self.cap_height(font_size) / 2.0).round();
        let yellow = self.theme.keys.suggestion_match_color
            .unwrap_or(Rgb(255, 214, 10))
            .scale(self.level as u32);

        // 1. Draw text preceding the cursor (separating earlier text and active typed prefix)
        let visible_before_count = c - start;
        let p_count = prefix_len.min(visible_before_count);
        let pre_chars: String = chars[start..(c - p_count)].iter().collect();
        let prefix_chars: String = chars[(c - p_count)..c].iter().collect();

        let mut cur_x = x;
        if !pre_chars.is_empty() {
            let pre_str = format!("{}{}", p_dots, pre_chars);
            let mut path = PathBuilder::new();
            cur_x = self.append_text(&mut path, cur_x, baseline, &pre_str, font_size, FONT_WEIGHT);
            if let Some(path) = path.finish() {
                self.fill(&path, self.color.scale(85));
            }
        } else if !p_dots.is_empty() {
            let mut path = PathBuilder::new();
            cur_x = self.append_text(&mut path, cur_x, baseline, p_dots, font_size, FONT_WEIGHT);
            if let Some(path) = path.finish() {
                self.fill(&path, self.color.scale(85));
            }
        }

        if !prefix_chars.is_empty() {
            let mut path = PathBuilder::new();
            cur_x = self.append_text(&mut path, cur_x, baseline, &prefix_chars, font_size, FONT_WEIGHT);
            if let Some(path) = path.finish() {
                self.fill(&path, yellow);
            }
        }

        // 2. Draw vertical cursor bar at exact cursor position
        let cursor_x = cur_x + 1.5;
        let cursor_top = (cy - self.cap_height(font_size) / 2.0).round() - 1.0;
        let cursor_bottom = (cy + self.cap_height(font_size) / 2.0).round() + 1.0;
        let cursor_bar = rounded_rect(cursor_x, cursor_top, cursor_x + 2.0, cursor_bottom, 1.0);
        self.fill(&cursor_bar, yellow);

        // 3. Draw text following the cursor
        if !after_str.is_empty() {
            let after_x = cursor_x + 2.0 + 2.5;
            let mut path = PathBuilder::new();
            self.append_text(&mut path, after_x, baseline, &after_str, font_size, FONT_WEIGHT);
            if let Some(path) = path.finish() {
                self.fill(&path, self.color.scale(85));
            }
        }
    }

    fn draw_clipboard_item(&mut self, left: f32, right: f32, cy: f32, text: &str) {
        let first_chars: String = text.chars().take(80).collect();
        let single_line: String = first_chars
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let label = format!("📋 {}", single_line);
        let max_text_w = (right - left - 16.0).max(10.0);
        let font_size = self.key_font_size;
        let truncated = self.truncate_text(&label, font_size, max_text_w);
        let cx = (left + right) / 2.0;
        self.center_text(cx, cy, &truncated, font_size, self.color);
    }

    pub fn word_definition(
        &mut self,
        def: &crate::lookup::WordDefinition,
        fade: u32,
        now_ms: u64,
        start_ms: u64,
        esc: Option<(Button, u32)>,
    ) -> bool {
        let full = self.color;
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);

        let mut left_bound = 16.0f32;
        if let Some((esc_btn, opacity)) = esc {
            self.color = full.scale(opacity.min(100));
            self.button(&esc_btn, false);
            left_bound = f32::from(esc_btn.right) + 20.0;
        }

        self.color = full.scale(fade.min(100));
        let cy = f32::from(self.height) / 2.0;

        let font_size = self.key_font_size;
        self.draw_emoji(left_bound + 16.0, cy, "📖", font_size + 2.0);
        let content_left = left_bound + font_size + 24.0;
        let right_bound = f32::from(self.width) - 24.0;
        let avail_w = (right_bound - content_left).max(10.0);

        let segments = def.segments();
        let total_w: f32 = segments.iter().map(|s| self.text_width(&s.text, font_size)).sum();
        let yellow = self.theme.keys.suggestion_match_color.unwrap_or(Rgb(255, 214, 10)).scale(self.level);

        let is_fast = if total_w <= avail_w {
            let start_x = ((content_left + right_bound) - total_w) / 2.0;
            let baseline = (cy + self.cap_height(font_size) / 2.0).round();
            let mut curr_x = start_x;
            for seg in &segments {
                let col = if seg.is_yellow { yellow } else { self.color };
                let mut seg_path = PathBuilder::new();
                curr_x = self.append_text_styled(&mut seg_path, curr_x, baseline, &seg.text, font_size, FONT_WEIGHT, seg.is_italic);
                if let Some(p) = seg_path.finish() {
                    self.fill(&p, col);
                }
            }
            false
        } else {
            let text_color = self.color;
            let sep_color = self.theme.status.center.marquee_separator.unwrap_or(Rgb(85, 85, 85)).scale(self.level);
            let (r, g, b) = (text_color.0, text_color.1, text_color.2);
            let is_cached = self.marquee_cache.as_ref().is_some_and(|c| {
                c.text == def.text && (c.size - font_size).abs() < 0.1 && c.color_rgb == (r, g, b)
            });

            if !is_cached {
                let sep = "   •   ";
                let sep_w = self.text_width(sep, font_size);
                let loop_dist = (total_w + sep_w).round();
                let cache_w = (loop_dist + avail_w + 50.0).round() as u32;

                if let Some(mut pixmap) = Pixmap::new(cache_w.max(100), u32::from(self.height)) {
                    pixmap.fill(Color::TRANSPARENT);
                    let baseline = (cy + self.cap_height(font_size) / 2.0).round();

                    let mut sx = 0.0f32;
                    for seg in &segments {
                        let col = if seg.is_yellow { yellow } else { text_color };
                        let mut path = tiny_skia::PathBuilder::new();
                        sx = self.append_text_styled(&mut path, sx, baseline, &seg.text, font_size, FONT_WEIGHT, seg.is_italic);
                        if let Some(p) = path.finish() {
                            let mut paint = Paint::default();
                            paint.set_color_rgba8(col.0, col.1, col.2, 255);
                            paint.anti_alias = true;
                            pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                        }
                    }

                    let mut sep_path = tiny_skia::PathBuilder::new();
                    sx = self.append_text(&mut sep_path, sx, baseline, sep, font_size, FONT_WEIGHT);
                    if let Some(sp) = sep_path.finish() {
                        let mut sep_paint = Paint::default();
                        sep_paint.set_color_rgba8(sep_color.0, sep_color.1, sep_color.2, 255);
                        sep_paint.anti_alias = true;
                        pixmap.fill_path(&sp, &sep_paint, FillRule::Winding, Transform::identity(), None);
                    }

                    for seg in &segments {
                        let col = if seg.is_yellow { yellow } else { text_color };
                        let mut path = tiny_skia::PathBuilder::new();
                        sx = self.append_text_styled(&mut path, sx, baseline, &seg.text, font_size, FONT_WEIGHT, seg.is_italic);
                        if let Some(p) = path.finish() {
                            let mut paint = Paint::default();
                            paint.set_color_rgba8(col.0, col.1, col.2, 255);
                            paint.anti_alias = true;
                            pixmap.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                        }
                    }

                    self.marquee_cache = Some(MarqueeCache {
                        text: def.text.clone(),
                        size: font_size,
                        color_rgb: (r, g, b),
                        pixmap,
                        loop_dist,
                        start_ms,
                    });
                }
            }

            if let Some(cache) = &self.marquee_cache {
                let win_x = content_left.max(0.0) as usize;
                let win_w = (right_bound - content_left).max(0.0) as usize;
                Self::blit_marquee(
                    &mut self.pixmap,
                    cache,
                    win_x,
                    win_w,
                    now_ms,
                );
            }
            true
        };
        self.flush();
        is_fast
    }

    fn draw_color_ribbon(&mut self, left: f32, right: f32, current_hue: f32) {
        let top = 4.0f32;
        let bottom = f32::from(self.height) - 4.0;
        let radius = 8.0f32;
        let rib_w = (right - left).max(10.0);

        let path = capsule_fill_path(left, top, right, bottom, radius, right);

        let step = 3.0f32;
        let mut x = left;
        while x < right {
            let next_x = (x + step).min(right);
            let h = ((x - left) / rib_w).clamp(0.0, 1.0);
            let (r, g, b) = hsv_to_rgb(h, 1.0, 1.0);
            let mut paint = Paint::default();
            let alpha = ((self.level as f32 / 100.0) * 235.0).round() as u8;
            paint.set_color_rgba8(r, g, b, alpha);
            paint.anti_alias = false;

            let rect_w = next_x - x;
            let rect_h = bottom - top;
            if let Some(rect) = Rect::from_xywh(x, top, rect_w, rect_h) {
                self.pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
            x = next_x;
        }

        let stroke = Stroke {
            width: 1.5,
            ..Default::default()
        };
        let mut outline_paint = Paint::default();
        let border_alpha = ((self.level as f32 / 100.0) * 120.0).round() as u8;
        outline_paint.set_color_rgba8(255, 255, 255, border_alpha);
        outline_paint.anti_alias = true;
        self.pixmap.stroke_path(&path, &outline_paint, &stroke, Transform::identity(), None);

        let needle_x = (left + rib_w * current_hue.clamp(0.0, 1.0)).round();
        let mut needle_builder = PathBuilder::new();
        needle_builder.move_to(needle_x, top - 2.0);
        needle_builder.line_to(needle_x, bottom + 2.0);
        if let Some(needle_path) = needle_builder.finish() {
            let n_stroke = Stroke {
                width: 3.0,
                line_cap: LineCap::Round,
                ..Default::default()
            };
            let mut n_paint = Paint::default();
            n_paint.set_color_rgba8(255, 255, 255, 255);
            n_paint.anti_alias = true;
            self.pixmap.stroke_path(&needle_path, &n_paint, &n_stroke, Transform::identity(), None);
        }
    }

    fn draw_color_swatch(&mut self, left: f32, right: f32, cy: f32, r: u8, g: u8, b: u8, copied: bool) {
        let top = 4.0f32;
        let bottom = f32::from(self.height) - 4.0;
        let radius = 8.0f32;

        let path = capsule_fill_path(left, top, right, bottom, radius, right);
        let mut bg_paint = Paint::default();
        let bg_alpha = ((self.level as f32 / 100.0) * 45.0).round() as u8;
        bg_paint.set_color_rgba8(40, 40, 40, bg_alpha);
        self.pixmap.fill_path(&path, &bg_paint, FillRule::Winding, Transform::identity(), None);

        let box_size = 24.0f32;
        let box_x = left + 12.0;
        let box_y = cy - box_size / 2.0;
        if let Some(rect) = Rect::from_xywh(box_x, box_y, box_size, box_size) {
            let mut col_paint = Paint::default();
            col_paint.set_color_rgba8(r, g, b, 255);
            self.pixmap.fill_rect(rect, &col_paint, Transform::identity(), None);

            let stroke = Stroke { width: 1.0, ..Default::default() };
            let mut border_paint = Paint::default();
            border_paint.set_color_rgba8(255, 255, 255, 180);
            let mut b_builder = PathBuilder::new();
            b_builder.push_rect(rect);
            if let Some(p) = b_builder.finish() {
                self.pixmap.stroke_path(&p, &border_paint, &stroke, Transform::identity(), None);
            }
        }

        let text_x = box_x + box_size + 14.0;
        let hex_code = format!("#{:02X}{:02X}{:02X}", r, g, b);
        let label = if copied {
            format!("✓ {hex_code}")
        } else {
            hex_code
        };
        let font_size = 18.0f32;
        let baseline = (cy + self.cap_height(font_size) / 2.0).round();
        let mut text_path = PathBuilder::new();
        self.append_text(&mut text_path, text_x, baseline, &label, font_size, FONT_WEIGHT);
        if let Some(p) = text_path.finish() {
            let col = if copied {
                Rgb(52, 199, 89)
            } else {
                Rgb(255, 255, 255)
            };
            self.fill(&p, col);
        }
    }

    fn draw_window_item(
        &mut self,
        left: f32,
        right: f32,
        cy: f32,
        title: &str,
        app_id: Option<&str>,
        is_focused: bool,
    ) {
        let top = 4.0f32;
        let bottom = f32::from(self.height) - 4.0;
        let radius = 8.0f32;
        let path = capsule_fill_path(left, top, right, bottom, radius, right);

        if is_focused {
            let mut bg_paint = Paint::default();
            let bg_alpha = ((self.level as f32 / 100.0) * 70.0).round() as u8;
            bg_paint.set_color_rgba8(0, 122, 255, bg_alpha);
            self.pixmap.fill_path(&path, &bg_paint, FillRule::Winding, Transform::identity(), None);

            let stroke = Stroke { width: 2.0, ..Default::default() };
            let mut stroke_paint = Paint::default();
            stroke_paint.set_color_rgba8(0, 122, 255, 240);
            stroke_paint.anti_alias = true;
            self.pixmap.stroke_path(&path, &stroke_paint, &stroke, Transform::identity(), None);
        }

        let icon_pixmap = resolve_window_svg(app_id, title, 28, 28);

        let mut text_start = left + 10.0;
        if let Some(pixmap) = icon_pixmap {
            let icon_w = pixmap.width() as f32;
            let icon_h = pixmap.height() as f32;
            let px = (text_start).round() as i32;
            let py = (cy - icon_h / 2.0).round() as i32;
            let paint = tiny_skia::PixmapPaint {
                opacity: (self.level as f32) / 100.0,
                blend_mode: tiny_skia::BlendMode::SourceOver,
                quality: tiny_skia::FilterQuality::Bilinear,
            };
            self.pixmap.draw_pixmap(px, py, pixmap.as_ref().as_ref(), &paint, Transform::identity(), None);
            text_start += icon_w + 8.0;
        }

        let avail_w = (right - text_start - 8.0).max(10.0);
        let font_size = 17.0f32;
        let truncated = self.truncate_text(title, font_size, avail_w);
        let baseline = (cy + self.cap_height(font_size) / 2.0).round();
        let mut text_path = PathBuilder::new();
        self.append_text(&mut text_path, text_start, baseline, &truncated, font_size, FONT_WEIGHT);
        if let Some(p) = text_path.finish() {
            let col = if is_focused {
                Rgb(255, 255, 255)
            } else {
                Rgb(210, 210, 215)
            };
            self.fill(&p, col);
        }
    }

    /// Authentication prompt: a line of text with an arrow towards the
    /// sensor right of the bar. `nudge` (0..1) pushes the arrow towards the
    /// sensor: its tail moves by `ARROW_TAIL_TRAVEL`, its tip twice as far,
    /// so it stretches while it moves.
    pub fn touch_id(&mut self, state: &str, nudge: f32, esc: Option<Button>, active: bool) {
        self.idle_base_cache = None;
        self.pixmap.fill(Color::BLACK);
        if let Some(esc) = esc {
            self.button(&esc, active);
        }
        let (parts, color, arrow): (&[(&str, f32)], Rgb, bool) = match state {
            "matched" => (&[("Разблокировано", STRONG_WEIGHT)], Rgb(0x34, 0xc7, 0x59), false),
            "retry" => (&[("Повторите попытку", STRONG_WEIGHT)], Rgb(0xff, 0x9f, 0x0a), true),
            "failed" => (
                &[("Введите пароль", STRONG_WEIGHT)],
                Rgb(0xff, 0x45, 0x3a),
                false,
            ),
            _ => (
                &[("Разблокируйте", STRONG_WEIGHT), (" с помощью Touch ID", FONT_WEIGHT)],
                self.color,
                true,
            ),
        };
        let size = self.key_font_size;
        let height = f32::from(self.height);
        let cy = height / 2.0;
        let text_width: f32 = parts
            .iter()
            .map(|(text, weight)| self.text_width_at(text, size, *weight))
            .sum();
        // Room for the fully stretched arrow, so the text never moves.
        let arrow_width = if arrow {
            ARROW_GAP + ARROW_LENGTH + 2.0 * ARROW_TAIL_TRAVEL
        } else {
            0.0
        };
        let right = f32::from(self.width) - PROMPT_MARGIN;
        let left = (right - arrow_width - text_width).round();

        let baseline = (cy + self.cap_height(size) / 2.0).round();
        let mut text = PathBuilder::new();
        let mut x = left;
        for (part, weight) in parts {
            x = self.append_text(&mut text, x, baseline, part, size, *weight);
        }
        let mut arrow_path = PathBuilder::new();
        if arrow {
            let nudge = nudge.clamp(0.0, 1.0) * ARROW_TAIL_TRAVEL;
            let tail = right - 2.0 * ARROW_TAIL_TRAVEL - ARROW_LENGTH + nudge;
            let tip = right - 2.0 * ARROW_TAIL_TRAVEL + 2.0 * nudge;
            let head = ARROW_LENGTH * 0.42;
            arrow_path.move_to(tail, cy);
            arrow_path.line_to(tip, cy);
            arrow_path.move_to(tip - head, cy - head);
            arrow_path.line_to(tip, cy);
            arrow_path.line_to(tip - head, cy + head);
        }

        let paint = color.paint();
        if let Some(path) = text.finish() {
            self.pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if let Some(path) = arrow_path.finish() {
            let stroke = Stroke {
                width: ARROW_STROKE,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
        self.flush();
    }

    fn truncate_text(&self, text: &str, size: f32, max_w: f32) -> String {
        if self.text_width(text, size) <= max_w {
            return text.to_owned();
        }
        let ellipsis = "…";
        let ell_w = self.text_width(ellipsis, size);
        let mut truncated = String::new();
        let mut width = 0.0f32;
        for c in text.chars() {
            let mut buf = [0u8; 4];
            let ch_str = c.encode_utf8(&mut buf);
            let ch_w = self.text_width(ch_str, size);
            if width + ch_w + ell_w > max_w {
                break;
            }
            width += ch_w;
            truncated.push(c);
        }
        format!("{truncated}{ellipsis}")
    }

fn blit_marquee(
    main_pixmap: &mut Pixmap,
    cache: &MarqueeCache,
    window_x: usize,
    window_w: usize,
    now_ms: u64,
) {
    let loop_dist = cache.loop_dist;
    if loop_dist <= 0.0 || window_w == 0 {
        return;
    }
    let speed = 54.0f32; // 54 pixels per second (smooth 30fps scrolling)
    let elapsed = (now_ms.saturating_sub(cache.start_ms) as f32) / 1000.0;
    let offset = ((elapsed * speed) % loop_dist).round() as usize;

    let cache_w = cache.pixmap.width() as usize;
    let cache_h = cache.pixmap.height() as usize;
    let cache_pixels = cache.pixmap.pixels();
    let main_w = main_pixmap.width() as usize;
    let main_h = main_pixmap.height() as usize;
    let main_pixels = main_pixmap.pixels_mut();
    let loop_dist_px = loop_dist.round() as usize;

    if window_x >= main_w || cache_h > main_h {
        return;
    }
    let actual_w = window_w.min(main_w - window_x);
    let feather = 16usize.min(actual_w / 3);

    for y in 0..cache_h {
        let cache_row = y * cache_w;
        let main_row = y * main_w + window_x;
        for x in 0..actual_w {
            let src_x = (offset + x) % loop_dist_px;
            if src_x < cache_w {
                let p = cache_pixels[cache_row + src_x];
                let a = p.alpha();
                if a > 0 {
                    let (r, g, b) = (p.red(), p.green(), p.blue());
                    let (r, g, b, a) = if feather > 0 && x < feather {
                        let f = x as u32;
                        let denom = feather as u32;
                        (
                            ((r as u32 * f + denom / 2) / denom) as u8,
                            ((g as u32 * f + denom / 2) / denom) as u8,
                            ((b as u32 * f + denom / 2) / denom) as u8,
                            ((a as u32 * f + denom / 2) / denom) as u8,
                        )
                    } else if feather > 0 && x >= actual_w - feather {
                        let f = (actual_w - 1 - x) as u32;
                        let denom = feather as u32;
                        (
                            ((r as u32 * f + denom / 2) / denom) as u8,
                            ((g as u32 * f + denom / 2) / denom) as u8,
                            ((b as u32 * f + denom / 2) / denom) as u8,
                            ((a as u32 * f + denom / 2) / denom) as u8,
                        )
                    } else {
                        (r, g, b, a)
                    };
                    let dst_idx = main_row + x;
                    let dst = main_pixels[dst_idx];
                    let inv_a = 255 - a as u32;
                    let dr = r as u32 + (dst.red() as u32 * inv_a + 127) / 255;
                    let dg = g as u32 + (dst.green() as u32 * inv_a + 127) / 255;
                    let db = b as u32 + (dst.blue() as u32 * inv_a + 127) / 255;
                    let da = a as u32 + (dst.alpha() as u32 * inv_a + 127) / 255;
                    if let Some(res) = tiny_skia::PremultipliedColorU8::from_rgba(
                        dr.min(255) as u8,
                        dg.min(255) as u8,
                        db.min(255) as u8,
                        da.min(255) as u8,
                    ) {
                        main_pixels[dst_idx] = res;
                    }
                }
            }
        }
    }
}

    #[allow(clippy::too_many_arguments)]
    fn draw_icon_text(
        &mut self,
        right_x: f32,
        cy: f32,
        icon: Icon,
        icon_scale: f32,
        text: &str,
        text_size: f32,
        text_color: Rgb,
        icon_color: Rgb,
        reserved_text_w: f32,
    ) -> f32 {
        let text_w = self.text_width(text, text_size);
        let text_slot_w = text_w.max(reserved_text_w);
        let icon_box_w = 26.0 * icon_scale;
        let gap = 10.0f32;
        let total_w = icon_box_w + gap + text_slot_w;
        let content_w = icon_box_w + gap + text_w;
        let offset_x = ((total_w - content_w) / 2.0).round().max(0.0);
        let start_x = right_x - total_w + offset_x;
        let icon_cx = start_x + icon_box_w / 2.0;
        self.icon_sized(icon_cx, cy, icon, BLACK, icon_scale, icon_color);
        let text_x = start_x + icon_box_w + gap;
        let baseline = (cy + self.cap_height(text_size) / 2.0).round();
        let mut path = PathBuilder::new();
        self.append_text(&mut path, text_x, baseline, text, text_size, FONT_WEIGHT);
        if let Some(path) = path.finish() {
            self.fill(&path, text_color);
        }
        total_w
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cycling_widget(
        &mut self,
        right_x: f32,
        cy: f32,
        items: &[CyclingItem],
        icon_scale: f32,
        text_size: f32,
        reserved_text_w: f32,
        now_ms: u64,
    ) -> (f32, bool, Option<u64>) {
        if items.is_empty() {
            return (0.0, false, None);
        }
        if items.len() == 1 {
            let w = self.draw_icon_text(
                right_x,
                cy,
                items[0].icon,
                icon_scale,
                &items[0].text,
                text_size,
                items[0].color,
                items[0].color,
                reserved_text_w,
            );
            return (w, false, None);
        }

        let total_period = items.len() as u64 * CYCLING_STEP_MS;
        let cycle_time = now_ms % total_period;
        let idx = (cycle_time / CYCLING_STEP_MS) as usize;
        let step_time = cycle_time % CYCLING_STEP_MS;

        // Steady state (no 60 FPS fade, discrete 0-cost switch)
        let w = self.draw_icon_text(
            right_x,
            cy,
            items[idx].icon,
            icon_scale,
            &items[idx].text,
            text_size,
            items[idx].color,
            items[idx].color,
            reserved_text_w,
        );
        let next_wake = now_ms + (CYCLING_STEP_MS - step_time);
        (w, false, Some(next_wake))
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cycling_clock(
        &mut self,
        left_x: f32,
        cy: f32,
        items: &[&str],
        text_size: f32,
        color: Rgb,
        slot_w: f32,
        now_ms: u64,
    ) -> (f32, bool, Option<u64>) {
        if items.is_empty() {
            return (0.0, false, None);
        }
        let baseline = (cy + self.cap_height(text_size) / 2.0).round();
        if items.len() == 1 {
            let text = items[0];
            let text_w = self.text_width(text, text_size);
            let text_x = left_x + ((slot_w - text_w) / 2.0).round().max(0.0);
            let mut path = PathBuilder::new();
            self.append_text(&mut path, text_x, baseline, text, text_size, FONT_WEIGHT);
            if let Some(path) = path.finish() {
                self.fill(&path, color);
            }
            return (slot_w, false, None);
        }

        let total_period = items.len() as u64 * CYCLING_STEP_MS;
        let cycle_time = now_ms % total_period;
        let idx = (cycle_time / CYCLING_STEP_MS) as usize;
        let step_time = cycle_time % CYCLING_STEP_MS;

        // Steady state: draw directly onto canvas centered in slot_w
        let text = items[idx];
        let text_w = self.text_width(text, text_size);
        let text_x = left_x + ((slot_w - text_w) / 2.0).round().max(0.0);
        let mut path = PathBuilder::new();
        self.append_text(&mut path, text_x, baseline, text, text_size, FONT_WEIGHT);
        if let Some(path) = path.finish() {
            self.fill(&path, color);
        }
        let next_wake = now_ms + (CYCLING_STEP_MS - step_time);
        (slot_w, false, Some(next_wake))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn idle_status_bar(
        &mut self,
        layout: Option<(&Layout, usize)>,
        active_esc: bool,
        clock_data: &status::ClockWidgetData,
        battery: Option<status::BatteryStatus>,
        volume: Option<(u32, bool)>,
        brightness: Option<u32>,
        media_track: Option<&str>,
        media_progress: Option<f32>,
        media_playing: bool,
        active_window: Option<&niri::ActiveWindow>,
        workspace: Option<niri::WorkspaceInfo>,
        ram_percent: Option<u32>,
        cpu_percent: Option<u32>,
        cpu_temp: Option<u32>,
        keyboard_layout: Option<&str>,
        shift: (f32, f32),
        now_ms: u64,
    ) -> (bool, Option<u64>) {
        self.media_capsule_rect = None;
        self.window_capsule_rect = None;
        self.status_workspace_buttons.clear();
        self.status_window_buttons.clear();

        let clock_cycle_time = now_ms % (3 * CYCLING_STEP_MS);
        let clock_idx = (clock_cycle_time / CYCLING_STEP_MS) as usize;

        let mut sysmon_count = 0;
        if cpu_percent.is_some() {
            sysmon_count += 1;
        }
        if ram_percent.is_some() {
            sysmon_count += 1;
        }
        if cpu_temp.is_some() {
            sysmon_count += 1;
        }
        let sysmon_idx = if sysmon_count > 0 {
            ((now_ms % (sysmon_count as u64 * CYCLING_STEP_MS)) / CYCLING_STEP_MS) as usize
        } else {
            0
        };

        let mut sound_brt_count = 0;
        if volume.is_some() {
            sound_brt_count += 1;
        }
        if brightness.is_some() {
            sound_brt_count += 1;
        }
        let sound_brt_idx = if sound_brt_count > 0 {
            ((now_ms % (sound_brt_count as u64 * CYCLING_STEP_MS)) / CYCLING_STEP_MS) as usize
        } else {
            0
        };

        let shift_key = (shift.0.round() as i32, shift.1.round() as i32);
        let has_media_capsule = media_track.is_some() && media_progress.is_some();

        let battery_has_watts = battery.as_ref().and_then(|b| b.power_mw).is_some_and(|mw| mw > 50);
        let battery_count = if battery_has_watts { 2 } else { 1 };
        let battery_idx = if battery_count > 1 {
            ((now_ms % (battery_count as u64 * CYCLING_STEP_MS)) / CYCLING_STEP_MS) as usize
        } else {
            0
        };

        let key = IdleBaseKey {
            has_esc: layout.is_some(),
            active_esc,
            has_media_capsule,
            clock_time: clock_data.time.clone(),
            clock_weekday: clock_data.weekday.clone(),
            clock_date: clock_data.date.clone(),
            clock_idx,
            battery,
            battery_idx,
            cpu_percent,
            ram_percent,
            cpu_temp,
            sysmon_idx,
            volume,
            brightness,
            sound_brt_idx,
            keyboard_layout: keyboard_layout.map(str::to_string),
            level: self.level,
            shift: shift_key,
        };

        let is_cache_hit = self.idle_base_cache.as_ref().is_some_and(|c| c.key == key);

        let (left_limit, right_limit) = if is_cache_hit {
            let cached = self.idle_base_cache.as_ref().unwrap();
            self.pixmap.data_mut().copy_from_slice(cached.pixmap.data());
            (cached.left_limit, cached.right_limit)
        } else {
            self.pixmap.fill(Color::BLACK);
            let cy = (f32::from(self.height) / 2.0 + shift.1).round();
            let icon_scale = (self.status_text_size / 20.0) * 0.85;
            let text_size = self.status_text_size;

            // 1. Esc button on the left if present
            let mut left_bound = 0.0;
            if let Some((layout, index)) = layout
                && let Some(button) = layout.buttons.get(index)
            {
                let full = self.color;
                if let Some(esc) = self.theme.keys.esc_color {
                    self.color = esc.scale(self.level);
                }
                self.button(button, active_esc);
                self.color = full;
                left_bound = f32::from(button.right) + 12.0;
            }

            // 2. Left side: Cycling Date & Time in the left corner
            // Format: 1) HH:MM, 2) Weekday, 3) Date
            let actual_left = (left_bound + shift.0).max(0.0);
            let date_size = self.status_date_size;
            let reserved_clock_w = self
                .text_width("30 сентября", date_size)
                .max(self.text_width("Понедельник", date_size))
                .max(self.text_width("Воскресенье", date_size))
                + 8.0;
            let slot_w = reserved_clock_w
                .max(self.text_width(&clock_data.time, date_size))
                .max(self.text_width(&clock_data.weekday, date_size))
                .max(self.text_width(&clock_data.date, date_size));
            let clock_items = [
                clock_data.time.as_str(),
                clock_data.weekday.as_str(),
                clock_data.date.as_str(),
            ];
            let clock_color = if clock_idx == 0 {
                self.theme.status.clock.time.unwrap_or(self.base).scale(self.level)
            } else {
                self.theme.status.clock.date.unwrap_or(self.base).scale(self.level)
            };
            let (clock_w, _is_clock_anim, _clock_wake) = self.draw_cycling_clock(
                actual_left,
                cy,
                &clock_items,
                date_size,
                clock_color,
                slot_w,
                now_ms,
            );
            let left_bound_end = actual_left + clock_w;

            // 3. Right side widgets (drawn right-to-left, flush to right edge):
            // Battery -> SysMon (CPU/RAM/Temp) -> Sound/Brightness (Vol/Brt) -> Keyboard
            let mut right_bound = (f32::from(self.width) + shift.0).min(f32::from(self.width));
            const WIDGET_GAP: f32 = 14.0;
            const KEYBOARD_GAP: f32 = 33.0;
            let widget_base = self.base;

            // 3a. Battery (always furthest right)
            if let Some(bat) = battery {
                let (bat_icon, bat_str) = match bat.state {
                    status::BatteryPowerState::Charging => {
                        (Icon::BatteryCharging, format!("{}%", bat.percent))
                    }
                    status::BatteryPowerState::DirectMains => {
                        (Icon::Plug, format!("{}%", bat.percent))
                    }
                    status::BatteryPowerState::Full => {
                        (Icon::Plug, "100%".to_string())
                    }
                    status::BatteryPowerState::Discharging => {
                        (Icon::Battery(bat.percent), format!("{}%", bat.percent))
                    }
                };

                let bat_base = match bat.state {
                    status::BatteryPowerState::DirectMains | status::BatteryPowerState::Full => {
                        self.theme.status.battery.plugged.unwrap_or(Rgb(0, 159, 255))
                    }
                    status::BatteryPowerState::Charging => {
                        self.theme.status.battery.charging.unwrap_or(Rgb(100, 240, 140))
                    }
                    status::BatteryPowerState::Discharging => {
                        if bat.percent <= 15 {
                            self.theme.status.battery.critical.unwrap_or(Rgb(255, 80, 80))
                        } else if bat.percent <= 30 {
                            self.theme.status.battery.warning.unwrap_or(Rgb(255, 180, 60))
                        } else {
                            self.theme.status.battery.normal.unwrap_or(self.base)
                        }
                    }
                };
                let bat_color = bat_base.scale(self.level);

                if let Some(watts) = bat.power_watts().filter(|&w| w > 0.05) {
                    let watts_str = if watts >= 10.0 {
                        format!("{:.1}W", watts)
                    } else {
                        format!("{:.2}W", watts)
                    };
                    let battery_items = [
                        CyclingItem {
                            icon: bat_icon,
                            text: bat_str,
                            color: bat_color,
                        },
                        CyclingItem {
                            icon: bat_icon,
                            text: watts_str,
                            color: bat_color,
                        },
                    ];
                    let reserved_bat_w = self
                        .text_width("100%", text_size)
                        .max(self.text_width("99.9W", text_size));
                    let (w, _is_anim, _next_wake) = self.draw_cycling_widget(
                        right_bound,
                        cy,
                        &battery_items,
                        icon_scale,
                        text_size,
                        reserved_bat_w,
                        now_ms,
                    );
                    right_bound -= w + WIDGET_GAP;
                } else {
                    let w = self.draw_icon_text(
                        right_bound,
                        cy,
                        bat_icon,
                        icon_scale,
                        &bat_str,
                        text_size,
                        bat_color,
                        bat_color,
                        0.0,
                    );
                    right_bound -= w + WIDGET_GAP;
                }
            }

            // 3b. Combined SysMon Widget: CPU Usage, RAM Usage, CPU Temperature
            // Cycles smoothly every 3 seconds
            let mut sysmon_items = Vec::with_capacity(3);
            if let Some(cpu) = cpu_percent {
                let col = self.theme.status.sysmon.cpu.unwrap_or(widget_base).scale(self.level);
                sysmon_items.push(CyclingItem {
                    icon: Icon::Cpu,
                    text: format!("{cpu}%"),
                    color: col,
                });
            }
            if let Some(ram) = ram_percent {
                let col = self.theme.status.sysmon.ram.unwrap_or(widget_base).scale(self.level);
                sysmon_items.push(CyclingItem {
                    icon: Icon::Ram,
                    text: format!("{ram}%"),
                    color: col,
                });
            }
            if let Some(temp) = cpu_temp {
                let col = self.theme.status.sysmon.temp.unwrap_or(widget_base).scale(self.level);
                sysmon_items.push(CyclingItem {
                    icon: Icon::Temperature,
                    text: format!("{temp}°C"),
                    color: col,
                });
            }
            if !sysmon_items.is_empty() {
                let reserved_sysmon_w = self
                    .text_width("100%", text_size)
                    .max(self.text_width("100°C", text_size));
                let (w, _is_anim, _next_wake) = self.draw_cycling_widget(
                    right_bound,
                    cy,
                    &sysmon_items,
                    icon_scale,
                    text_size,
                    reserved_sysmon_w,
                    now_ms,
                );
                right_bound -= w + WIDGET_GAP;
            }

            // 3c. Combined Sound / Brightness Widget: Volume level, Screen brightness
            // Cycles smoothly every 3 seconds
            let mut sound_brt_items = Vec::with_capacity(2);
            if let Some((vol, muted)) = volume {
                let vol_icon = if muted {
                    Icon::Mute
                } else if vol > 50 {
                    Icon::Volume(true)
                } else {
                    Icon::Volume(false)
                };
                let col = if muted {
                    self.theme.status.hardware.volume_muted.unwrap_or(Rgb(255, 80, 80)).scale(self.level)
                } else {
                    self.theme.status.hardware.volume.unwrap_or(widget_base).scale(self.level)
                };
                sound_brt_items.push(CyclingItem {
                    icon: vol_icon,
                    text: format!("{vol}%"),
                    color: col,
                });
            }
            if let Some(brt) = brightness {
                let col = self.theme.status.hardware.brightness.unwrap_or(widget_base).scale(self.level);
                sound_brt_items.push(CyclingItem {
                    icon: Icon::Brightness(true),
                    text: format!("{brt}%"),
                    color: col,
                });
            }
            if !sound_brt_items.is_empty() {
                let reserved_sound_brt_w = self.text_width("100%", text_size);
                let (w, _is_anim, _next_wake) = self.draw_cycling_widget(
                    right_bound,
                    cy,
                    &sound_brt_items,
                    icon_scale,
                    text_size,
                    reserved_sound_brt_w,
                    now_ms,
                );
                right_bound -= w + KEYBOARD_GAP;
            }

            // 3d. Keyboard layout: Keyboard icon + RU/EN (placed before Sound/Brightness)
            if let Some(layout_str) = keyboard_layout {
                let kbd_color = self.theme.status.hardware.keyboard_layout.unwrap_or(widget_base).scale(self.level);
                let reserved_layout_w = self
                    .text_width("RU", text_size)
                    .max(self.text_width("EN", text_size));
                let w = self.draw_icon_text(
                    right_bound,
                    cy,
                    Icon::KeyboardDevice,
                    icon_scale,
                    layout_str,
                    text_size,
                    kbd_color,
                    kbd_color,
                    reserved_layout_w,
                );
                right_bound -= w + WIDGET_GAP;
            }

            let left_limit = left_bound_end + 36.0;
            let right_limit = right_bound + WIDGET_GAP - 32.0;

            if has_media_capsule && right_limit > left_limit + 60.0 {
                let bar_left = (left_limit - 10.0).round();
                let bar_right = (right_limit + 10.0).round();
                let top = 4.0f32;
                let bottom = f32::from(self.height) - 4.0;
                let radius = 10.0f32;

                let level_ratio = (self.level as f32) / 100.0;
                let bg_alpha = (level_ratio * 18.0).round() as u8;
                let border_alpha = (level_ratio * 44.0).round() as u8;

                let capsule_col = self.theme.status.media_bar.capsule_bg.unwrap_or(Rgb(255, 255, 255));
                // 1. Background capsule (unplayed area)
                let capsule_path = rounded_rect(bar_left, top, bar_right, bottom, radius);
                let mut bg_paint = Paint::default();
                bg_paint.set_color_rgba8(capsule_col.0, capsule_col.1, capsule_col.2, bg_alpha);
                bg_paint.anti_alias = true;
                self.pixmap
                    .fill_path(&capsule_path, &bg_paint, FillRule::Winding, Transform::identity(), None);

                // 2. Subtle outer border of the capsule
                let stroke = Stroke {
                    width: 1.0,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Default::default()
                };
                let mut border_paint = Paint::default();
                border_paint.set_color_rgba8(capsule_col.0, capsule_col.1, capsule_col.2, border_alpha);
                border_paint.anti_alias = true;
                self.pixmap
                    .stroke_path(&capsule_path, &border_paint, &stroke, Transform::identity(), None);
            }

            let mut cache_pixmap = match self.idle_base_cache.take() {
                Some(c) => c.pixmap,
                None => Pixmap::new(u32::from(self.width), u32::from(self.height)).unwrap(),
            };
            cache_pixmap.data_mut().copy_from_slice(self.pixmap.data());
            self.idle_base_cache = Some(IdleBaseCache {
                key,
                pixmap: cache_pixmap,
                left_limit,
                right_limit,
            });

            (left_limit, right_limit)
        };

        // 4. Center Widget: Media track (if playing) or Active Niri window (if idle),
        // with colored emojis and smooth marquee scrolling when overflowing.
        let cy = (f32::from(self.height) / 2.0 + shift.1).round();
        let text_size = self.status_text_size;
        let step_time = now_ms % CYCLING_STEP_MS;
        let mut min_next_wake = Some(now_ms + (CYCLING_STEP_MS - step_time));
        let mut needs_scroll = false;

        let center_base = if media_track.is_some() {
            self.theme.status.center.media_title.unwrap_or(Rgb(255, 140, 0))
        } else {
            self.theme.status.center.window_title.unwrap_or(Rgb(255, 140, 0))
        };
        let center_color = center_base.scale(self.level);
        let prefix_color = self.theme.status.center.workspace.unwrap_or(center_base).scale(self.level);
        let sep_color = self.theme.status.center.marquee_separator
            .unwrap_or_else(|| center_base.scale(55))
            .scale(self.level);

        if let Some(track) = media_track
            && right_limit > left_limit + 60.0
        {
            if let Some(progress) = media_progress {
                let p = progress.clamp(0.0, 1.0);
                let bar_left = (left_limit - 10.0).round();
                let bar_right = (right_limit + 10.0).round();
                self.media_capsule_rect = Some((bar_left, bar_right));
                let bar_w = bar_right - bar_left;
                let top = 4.0f32;
                let bottom = f32::from(self.height) - 4.0;
                let radius = 10.0f32;

                let level_ratio = (self.level as f32) / 100.0;
                let played_alpha = (level_ratio * 38.0).round() as u8;
                let needle_alpha = (level_ratio * 240.0).round() as u8;

                let played_col = self.theme.status.media_bar.progress_fill.unwrap_or(Rgb(255, 255, 255));
                let needle_col = self.theme.status.media_bar.needle.unwrap_or(Rgb(255, 255, 255));

                if p > 0.005 {
                    let played_right = (bar_left + bar_w * p).min(bar_right).max(bar_left + 2.0);
                    let played_path =
                        capsule_fill_path(bar_left, top, played_right, bottom, radius, bar_right);
                    let mut played_paint = Paint::default();
                    played_paint.set_color_rgba8(played_col.0, played_col.1, played_col.2, played_alpha);
                    played_paint.anti_alias = true;
                    self.pixmap
                        .fill_path(&played_path, &played_paint, FillRule::Winding, Transform::identity(), None);

                    if p < 0.99 && played_right <= bar_right - radius {
                        let mut needle_builder = PathBuilder::new();
                        needle_builder.move_to(played_right, top);
                        needle_builder.line_to(played_right, bottom);
                        if let Some(needle_path) = needle_builder.finish() {
                            let stroke = Stroke {
                                width: 1.5,
                                line_cap: LineCap::Butt,
                                ..Default::default()
                            };
                            let mut needle_paint = Paint::default();
                            needle_paint.set_color_rgba8(needle_col.0, needle_col.1, needle_col.2, needle_alpha);
                            needle_paint.anti_alias = true;
                            self.pixmap
                                .stroke_path(&needle_path, &needle_paint, &stroke, Transform::identity(), None);
                        }
                    }
                }

                let progress_wake = if media_playing {
                    now_ms + 250
                } else {
                    now_ms + 5000
                };
                min_next_wake = Some(min_next_wake.map_or(progress_wake, |w| w.min(progress_wake)));
            }

            let center_icon_scale = (self.status_text_size / 24.0).clamp(1.1, 1.35);
            let icon_box_w = (26.0 * center_icon_scale).round();
            let icon_gap = 10.0f32;
            let stationary_w = icon_box_w + icon_gap;
            let available_w = right_limit - left_limit;
            let max_text_w = (available_w - stationary_w).max(0.0);
            let full_text_w = self.text_width(track, text_size);

            if available_w > 30.0 && max_text_w > 20.0 {
                let baseline = (cy + self.cap_height(text_size) / 2.0).round();
                if full_text_w <= max_text_w {
                    self.marquee_cache = None;
                    let total_w = stationary_w + full_text_w;
                    let center_x = (left_limit + right_limit) / 2.0;
                    let start_x = (center_x - total_w / 2.0).round().max(left_limit);
                    let mut cur_x = start_x;

                    let icon_cx = cur_x + icon_box_w / 2.0;
                    self.icon_sized(icon_cx, cy, Icon::Play, BLACK, center_icon_scale, center_color);
                    cur_x += icon_box_w + icon_gap;

                    self.draw_text_on_main(
                        cur_x,
                        baseline,
                        track,
                        text_size,
                        FONT_WEIGHT,
                        center_color,
                    );
                } else {
                    needs_scroll = true;
                    let mut cur_x = left_limit;
                    let icon_cx = cur_x + icon_box_w / 2.0;
                    self.icon_sized(icon_cx, cy, Icon::Play, BLACK, center_icon_scale, center_color);
                    cur_x += icon_box_w + icon_gap;

                    let window_x = cur_x.round();
                    let window_w = (right_limit - window_x).max(1.0).round() as u32;

                    let color_rgb = (center_color.0, center_color.1, center_color.2);
                    let is_cached = self.marquee_cache.as_ref().is_some_and(|c| {
                        c.text == track && (c.size - text_size).abs() < 0.1 && c.color_rgb == color_rgb
                    });

                    if !is_cached {
                        let sep = "    •    ";
                        let sep_w = self.text_width(sep, text_size);
                        let loop_dist = (full_text_w + sep_w).round();
                        let pixmap_w = (loop_dist + 10.0).ceil() as u32;
                        let pixmap_h = u32::from(self.height);
                        if let Some(mut cache_pixmap) = Pixmap::new(pixmap_w, pixmap_h) {
                            cache_pixmap.fill(Color::TRANSPARENT);
                            self.draw_text_on_target(
                                &mut cache_pixmap,
                                0.0,
                                baseline,
                                track,
                                text_size,
                                FONT_WEIGHT,
                                center_color,
                            );
                            self.draw_text_on_target(
                                &mut cache_pixmap,
                                full_text_w,
                                baseline,
                                sep,
                                text_size,
                                FONT_WEIGHT,
                                sep_color,
                            );
                            self.marquee_cache = Some(MarqueeCache {
                                text: track.to_string(),
                                size: text_size,
                                color_rgb,
                                pixmap: cache_pixmap,
                                loop_dist,
                                start_ms: now_ms,
                            });
                        }
                    }

                    if let Some(cache) = &self.marquee_cache {
                        Self::blit_marquee(
                            &mut self.pixmap,
                            cache,
                            window_x as usize,
                            window_w as usize,
                            now_ms,
                        );
                    }
                }
            }
        } else if media_track.is_none() && right_limit > left_limit + 60.0 {
            // Workspace buttons, active workspace application icon-buttons, and window title marquee
            let mut cur_x = left_limit;
            let btn_size = 44.0f32;
            let btn_top = ((f32::from(self.height) - btn_size) / 2.0 + shift.1).round();
            let btn_bottom = btn_top + btn_size;
            let btn_radius = 8.0f32;

            // 1. Workspace buttons
            if let Some(ws) = workspace {
                for idx in 1..=ws.total {
                    let btn_left = cur_x;
                    let btn_right = btn_left + btn_size;
                    if btn_right > right_limit - 40.0 {
                        break;
                    }
                    self.status_workspace_buttons.push((idx, btn_left, btn_right));
                    let is_active = idx == ws.current;
                    let path = rounded_rect(btn_left, btn_top, btn_right, btn_bottom, btn_radius);

                    if is_active {
                        let mut fill_paint = Paint::default();
                        let alpha = ((self.level as f32 / 100.0) * 80.0).round() as u8;
                        fill_paint.set_color_rgba8(prefix_color.0, prefix_color.1, prefix_color.2, alpha);
                        fill_paint.anti_alias = true;
                        self.pixmap.fill_path(&path, &fill_paint, FillRule::Winding, Transform::identity(), None);

                        let stroke = Stroke {
                            width: 2.0,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Round,
                            ..Default::default()
                        };
                        let mut border_paint = Paint::default();
                        let border_alpha = ((self.level as f32 / 100.0) * 255.0).round() as u8;
                        border_paint.set_color_rgba8(prefix_color.0, prefix_color.1, prefix_color.2, border_alpha);
                        border_paint.anti_alias = true;
                        self.pixmap.stroke_path(&path, &border_paint, &stroke, Transform::identity(), None);

                        let num_str = idx.to_string();
                        let num_w = self.text_width(&num_str, 22.0);
                        let num_x = (btn_left + (btn_size - num_w) / 2.0).round();
                        let baseline = (cy + self.cap_height(22.0) / 2.0).round();
                        self.draw_text_on_main(num_x, baseline, &num_str, 22.0, FONT_WEIGHT, prefix_color);
                    } else {
                        let mut fill_paint = Paint::default();
                        let alpha = ((self.level as f32 / 100.0) * 16.0).round() as u8;
                        fill_paint.set_color_rgba8(255, 255, 255, alpha);
                        fill_paint.anti_alias = true;
                        self.pixmap.fill_path(&path, &fill_paint, FillRule::Winding, Transform::identity(), None);

                        let stroke = Stroke {
                            width: 1.0,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Round,
                            ..Default::default()
                        };
                        let mut border_paint = Paint::default();
                        let border_alpha = ((self.level as f32 / 100.0) * 40.0).round() as u8;
                        border_paint.set_color_rgba8(255, 255, 255, border_alpha);
                        border_paint.anti_alias = true;
                        self.pixmap.stroke_path(&path, &border_paint, &stroke, Transform::identity(), None);

                        let num_str = idx.to_string();
                        let num_w = self.text_width(&num_str, 22.0);
                        let num_x = (btn_left + (btn_size - num_w) / 2.0).round();
                        let baseline = (cy + self.cap_height(22.0) / 2.0).round();
                        let text_col = Rgb(170, 170, 170).scale(self.level);
                        self.draw_text_on_main(num_x, baseline, &num_str, 22.0, FONT_WEIGHT, text_col);
                    }
                    cur_x = btn_right + 6.0;
                }
                cur_x += 8.0;
            }

            // 2. Large application icon buttons in active workspace
            let active_ws_id = workspace.and_then(|w| w.active_id);
            let mut active_windows: Vec<niri::NiriWindowItem> = self
                .workspace_windows
                .iter()
                .filter(|w| {
                    if let Some(target_id) = active_ws_id {
                        w.workspace_id == Some(target_id)
                    } else {
                        w.is_focused
                    }
                })
                .cloned()
                .collect();

            if active_windows.is_empty() && let Some(win) = active_window {
                active_windows.push(niri::NiriWindowItem {
                    id: 0,
                    title: Some(win.title.clone()),
                    app_id: win.app_id.clone(),
                    pid: win.pid,
                    workspace_id: active_ws_id,
                    is_focused: true,
                    layout: None,
                });
            }

            active_windows.sort_by_key(|w| {
                (
                    w.layout.as_ref().and_then(|l| l.pos_in_scrolling_layout).map(|p| p.0).unwrap_or(usize::MAX),
                    w.layout.as_ref().and_then(|l| l.pos_in_scrolling_layout).map(|p| p.1).unwrap_or(usize::MAX),
                    w.id,
                )
            });

            if !active_windows.is_empty() {
                for win in &active_windows {
                    let btn_left = cur_x;
                    let btn_right = btn_left + btn_size;
                    if btn_right > right_limit - 40.0 {
                        break;
                    }
                    if win.id != 0 {
                        self.status_window_buttons.push((win.id, btn_left, btn_right));
                    }
                    let is_focused = win.is_focused
                        || active_window.is_some_and(|aw| aw.title == win.title.as_deref().unwrap_or(""));
                    let path = rounded_rect(btn_left, btn_top, btn_right, btn_bottom, btn_radius);

                    if is_focused {
                        let mut fill_paint = Paint::default();
                        let alpha = ((self.level as f32 / 100.0) * 50.0).round() as u8;
                        fill_paint.set_color_rgba8(center_color.0, center_color.1, center_color.2, alpha);
                        fill_paint.anti_alias = true;
                        self.pixmap.fill_path(&path, &fill_paint, FillRule::Winding, Transform::identity(), None);

                        let stroke = Stroke {
                            width: 2.0,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Round,
                            ..Default::default()
                        };
                        let mut border_paint = Paint::default();
                        let border_alpha = ((self.level as f32 / 100.0) * 255.0).round() as u8;
                        border_paint.set_color_rgba8(center_color.0, center_color.1, center_color.2, border_alpha);
                        border_paint.anti_alias = true;
                        self.pixmap.stroke_path(&path, &border_paint, &stroke, Transform::identity(), None);
                    } else {
                        let mut fill_paint = Paint::default();
                        let alpha = ((self.level as f32 / 100.0) * 16.0).round() as u8;
                        fill_paint.set_color_rgba8(255, 255, 255, alpha);
                        fill_paint.anti_alias = true;
                        self.pixmap.fill_path(&path, &fill_paint, FillRule::Winding, Transform::identity(), None);

                        let stroke = Stroke {
                            width: 1.0,
                            line_cap: LineCap::Round,
                            line_join: LineJoin::Round,
                            ..Default::default()
                        };
                        let mut border_paint = Paint::default();
                        let border_alpha = ((self.level as f32 / 100.0) * 40.0).round() as u8;
                        border_paint.set_color_rgba8(255, 255, 255, border_alpha);
                        border_paint.anti_alias = true;
                        self.pixmap.stroke_path(&path, &border_paint, &stroke, Transform::identity(), None);
                    }

                    let btn_cx = (btn_left + btn_right) / 2.0;
                    if let Some(svg) = resolve_window_svg(win.app_id.as_deref(), win.title.as_deref().unwrap_or(""), 28, 28) {
                        let px = (btn_cx - svg.width() as f32 / 2.0).round() as i32;
                        let py = (cy - svg.height() as f32 / 2.0).round() as i32;
                        let paint = tiny_skia::PixmapPaint {
                            opacity: (self.level as f32 / 100.0) * 0.95,
                            blend_mode: tiny_skia::BlendMode::SourceOver,
                            quality: tiny_skia::FilterQuality::Bilinear,
                        };
                        self.pixmap.draw_pixmap(px, py, svg.as_ref().as_ref(), &paint, Transform::identity(), None);
                    } else {
                        let icon = resolve_window_icon(win.app_id.as_deref(), win.title.as_deref().unwrap_or(""));
                        let ic_color = if is_focused { center_color } else { Rgb(190, 190, 190).scale(self.level) };
                        self.icon_sized(btn_cx, cy, icon, BLACK, 1.25, ic_color);
                    }
                    cur_x = btn_right + 6.0;
                }
                cur_x += 8.0;
            }

            // 3. Active window title with marquee
            if let Some(win) = active_window {
                let text = win.title.as_str();
                let available_w = (right_limit - cur_x).max(0.0);
                let full_text_w = self.text_width(text, text_size);
                if available_w > 30.0 && !text.is_empty() {
                    let baseline = (cy + self.cap_height(text_size) / 2.0).round();
                    if full_text_w <= available_w {
                        self.marquee_cache = None;
                        self.draw_text_on_main(
                            cur_x,
                            baseline,
                            text,
                            text_size,
                            FONT_WEIGHT,
                            center_color,
                        );
                    } else {
                        needs_scroll = true;
                        let window_x = cur_x.round();
                        let window_w = (right_limit - window_x).max(1.0).round() as u32;

                        let color_rgb = (center_color.0, center_color.1, center_color.2);
                        let is_cached = self.marquee_cache.as_ref().is_some_and(|c| {
                            c.text == text && (c.size - text_size).abs() < 0.1 && c.color_rgb == color_rgb
                        });

                        if !is_cached {
                            let sep = "    •    ";
                            let sep_w = self.text_width(sep, text_size);
                            let loop_dist = (full_text_w + sep_w).round();
                            let pixmap_w = (loop_dist + 10.0).ceil() as u32;
                            let pixmap_h = u32::from(self.height);
                            if let Some(mut cache_pixmap) = Pixmap::new(pixmap_w, pixmap_h) {
                                cache_pixmap.fill(Color::TRANSPARENT);
                                self.draw_text_on_target(
                                    &mut cache_pixmap,
                                    0.0,
                                    baseline,
                                    text,
                                    text_size,
                                    FONT_WEIGHT,
                                    center_color,
                                );
                                self.draw_text_on_target(
                                    &mut cache_pixmap,
                                    full_text_w,
                                    baseline,
                                    sep,
                                    text_size,
                                    FONT_WEIGHT,
                                    sep_color,
                                );
                                self.marquee_cache = Some(MarqueeCache {
                                    text: text.to_string(),
                                    size: text_size,
                                    color_rgb,
                                    pixmap: cache_pixmap,
                                    loop_dist,
                                    start_ms: now_ms,
                                });
                            }
                        }

                        if let Some(cache) = &self.marquee_cache {
                            Self::blit_marquee(
                                &mut self.pixmap,
                                cache,
                                window_x as usize,
                                window_w as usize,
                                now_ms,
                            );
                        }
                    }
                }
            } else {
                self.marquee_cache = None;
            }
        } else {
            self.marquee_cache = None;
        }

        if is_cache_hit {
            let dirty_left = (left_limit - 14.0).max(0.0).floor() as usize;
            let dirty_right = (right_limit + 14.0).min(f32::from(self.width)).ceil() as usize;
            self.flush_cols(dirty_left, dirty_right);
        } else {
            self.flush();
        }

        let needs_fast_refresh = needs_scroll;
        (needs_fast_refresh, min_next_wake)
    }

    /// Converts the premultiplied, always opaque RGBA pixmap into XRGB8888.
    fn flush(&mut self) {
        self.flush_cols(0, usize::from(self.width));
    }

    pub fn flush_cols(&mut self, min_col: usize, max_col: usize) {
        let w = usize::from(self.width);
        let h = usize::from(self.height);
        let min_x = min_col.min(w);
        let max_x = max_col.min(w);
        if min_x >= max_x {
            return;
        }
        let skia_pixels = self.pixmap.pixels();
        for y in 0..h {
            let row_offset = y * w;
            let start = row_offset + min_x;
            let end = row_offset + max_x;
            let target_slice = &mut self.pixels[start..end];
            let src_slice = &skia_pixels[start..end];
            for (target, p) in target_slice.iter_mut().zip(src_slice.iter()) {
                *target = (u32::from(p.red()) << 16)
                    | (u32::from(p.green()) << 8)
                    | u32::from(p.blue());
            }
        }
    }

    fn fill(&mut self, path: &Path, color: Rgb) {
        self.pixmap.fill_path(
            path,
            &color.paint(),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    fn stroke(&mut self, path: &Path, color: Rgb, width: f32) {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        self.pixmap
            .stroke_path(path, &color.paint(), &stroke, Transform::identity(), None);
    }

    fn icon(&mut self, cx: f32, cy: f32, icon: Icon, background: Rgb) {
        let scale = self.key_font_size / KEY_FONT_SIZE;
        self.icon_sized(cx, cy, icon, background, scale, self.color);
    }

    /// `size` scales the key-sized icon, including its stroke weight.
    fn icon_sized(&mut self, cx: f32, cy: f32, icon: Icon, background: Rgb, size: f32, color: Rgb) {
        let scale = ICON_SCALE * size;
        let stroke = ICON_STROKE * size;
        let shapes = icon_shapes(icon);
        let mut bounds: Option<Rect> = None;
        for shape in &shapes {
            let pad = shape.pad() * size;
            let Some(path) = shape
                .path()
                .clone()
                .transform(Transform::from_scale(scale, scale))
            else {
                continue;
            };
            let path_bounds = path.bounds();
            let rect = Rect::from_ltrb(
                path_bounds.left() - pad,
                path_bounds.top() - pad,
                path_bounds.right() + pad,
                path_bounds.bottom() + pad,
            );
            bounds = match (bounds, rect) {
                (Some(old), Some(new)) => Rect::from_ltrb(
                    old.left().min(new.left()),
                    old.top().min(new.top()),
                    old.right().max(new.right()),
                    old.bottom().max(new.bottom()),
                ),
                (old, new) => old.or(new),
            };
        }
        let Some(bounds) = bounds else { return };
        let dx = (cx - (bounds.left() + bounds.right()) / 2.0).round();
        let dy = (cy - (bounds.top() + bounds.bottom()) / 2.0).round();
        let transform = Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
        for shape in shapes {
            match shape {
                Shape::Stroke(path) => {
                    if let Some(path) = path.transform(transform) {
                        self.stroke(&path, color, stroke);
                    }
                }
                Shape::Solid(path) => {
                    if let Some(path) = path.transform(transform) {
                        // A thin round-joined stroke softens the corners the
                        // same way the keycap glyphs are rounded.
                        self.fill(&path, color);
                        self.stroke(&path, color, 1.4 * size);
                    }
                }
                Shape::Slash(path) => {
                    if let Some(path) = path.transform(transform) {
                        self.stroke(&path, background, stroke * 2.6);
                        self.stroke(&path, color, stroke);
                    }
                }
            }
        }
    }

    #[allow(clippy::chunks_exact_to_as_chunks)]
    fn get_or_load_emoji_impl(
        emoji_face: &Option<freetype::Face>,
        emoji_cache: &EmojiCache,
        c: char,
    ) -> Option<(Pixmap, f32, f32, f32)> {
        if !is_emoji_char(c) {
            return None;
        }
        if let Some(entry) = emoji_cache.borrow().get(&c) {
            return Some(entry.clone());
        }
        let face = emoji_face.as_ref()?;
        let _ = face.select_size(0);
        if face.load_char(c as usize, freetype::face::LoadFlag::COLOR).is_err() {
            return None;
        }
        let glyph = face.glyph();
        if glyph.render_glyph(freetype::render_mode::RenderMode::Normal).is_err() {
            return None;
        }
        let bitmap = glyph.bitmap();
        let w = bitmap.width() as u32;
        let h = bitmap.rows() as u32;
        if w == 0 || h == 0 {
            return None;
        }
        let mut emoji_pixmap = Pixmap::new(w, h)?;
        for (src, dst) in bitmap.buffer().chunks_exact(4).zip(emoji_pixmap.pixels_mut()) {
            let a = src[3];
            let r = src[2].min(a);
            let g = src[1].min(a);
            let b = src[0].min(a);
            *dst = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a)
                .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
        }
        let adv = (glyph.advance().x >> 6) as f32;
        let left = glyph.bitmap_left() as f32;
        let top = glyph.bitmap_top() as f32;
        let entry = (emoji_pixmap, adv, left, top);
        emoji_cache.borrow_mut().insert(c, entry.clone());
        Some(entry)
    }

    fn get_or_load_emoji(&self, c: char) -> Option<(Pixmap, f32, f32, f32)> {
        Self::get_or_load_emoji_impl(&self.emoji_face, &self.emoji_cache, c)
    }

    fn text_width(&self, text: &str, size: f32) -> f32 {
        let mut width = 0.0;
        let mut previous: Option<(&FontVec, ab_glyph::GlyphId)> = None;
        for character in text.chars() {
            if is_emoji_modifier(character) {
                continue;
            }
            if let Some((_, adv, _, _)) = self.get_or_load_emoji(character) {
                let emoji_adv = if adv > 0.0 { adv } else { 128.0 } * (size / 128.0);
                width += emoji_adv;
                previous = None;
                continue;
            }
            let (font, id) = self.glyph_font_and_id(character);
            if id.0 != 0 {
                let scale = size / font.units_per_em().unwrap_or(1000.0);
                if let Some((prev_font, prev_id)) = previous
                    && std::ptr::eq(font, prev_font) {
                        width += font.kern_unscaled(prev_id, id) * scale;
                    }
                width += font.h_advance_unscaled(id) * scale;
                previous = Some((font, id));
            }
        }
        width
    }

    #[allow(dead_code)]
    pub fn truncate_to_width(&self, text: &str, max_w: f32, size: f32) -> String {
        let ellipsis = "…";
        let ellipsis_w = self.text_width(ellipsis, size);
        if max_w <= ellipsis_w {
            return String::new();
        }
        let target_w = max_w - ellipsis_w;
        let mut last_valid = 0;
        for (idx, ch) in text.char_indices() {
            let end_idx = idx + ch.len_utf8();
            let slice = &text[..end_idx];
            if self.text_width(slice, size) > target_w {
                break;
            }
            last_valid = end_idx;
        }
        if last_valid == 0 {
            return String::new();
        }
        let mut result = text[..last_valid].trim_end().to_string();
        result.push('…');
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_impl(
        font: &mut FontVec,
        fallback_fonts: &mut [FontVec],
        emoji_face: &Option<freetype::Face>,
        emoji_cache: &EmojiCache,
        target: &mut Pixmap,
        mut x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        weight: f32,
        color: Rgb,
    ) -> f32 {
        font.set_variation(b"wght", weight);
        for fallback in fallback_fonts.iter_mut() {
            fallback.set_variation(b"wght", weight);
        }

        let mut path_builder = PathBuilder::new();
        let mut previous: Option<(&FontVec, ab_glyph::GlyphId)> = None;

        for character in text.chars() {
            if is_emoji_modifier(character) {
                continue;
            }
            if let Some((emoji_pixmap, adv, left, top)) =
                Self::get_or_load_emoji_impl(emoji_face, emoji_cache, character)
            {
                if let Some(p) = std::mem::take(&mut path_builder).finish() {
                    target.fill_path(
                        &p,
                        &color.paint(),
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
                previous = None;

                let scale = size / 128.0;
                let dest_x = x + left * scale;
                let dest_y = baseline - top * scale;
                let transform = Transform::from_scale(scale, scale).post_translate(dest_x, dest_y);
                let text_brightness = (color.0.max(color.1).max(color.2) as f32) / 255.0;
                let emoji_opacity = (text_brightness * 0.70).clamp(0.0, 1.0);
                let paint = tiny_skia::PixmapPaint {
                    opacity: emoji_opacity,
                    quality: tiny_skia::FilterQuality::Bilinear,
                    ..Default::default()
                };
                target.draw_pixmap(0, 0, emoji_pixmap.as_ref(), &paint, transform, None);

                let emoji_adv = if adv > 0.0 { adv } else { 128.0 } * scale;
                x += emoji_adv;
                continue;
            }

            let (active_font, id) = Self::glyph_font_and_id_impl(font, fallback_fonts, character);
            if id.0 != 0 {
                let scale = size / active_font.units_per_em().unwrap_or(1000.0);
                if let Some((prev_font, prev_id)) = previous
                    && std::ptr::eq(active_font, prev_font)
                {
                    x += active_font.kern_unscaled(prev_id, id) * scale;
                }
                if let Some(outline) = active_font.outline(id) {
                    let point = |p: ab_glyph::Point| (x + p.x * scale, baseline - p.y * scale);
                    let mut last = None;
                    for curve in outline.curves {
                        let start = match curve {
                            OutlineCurve::Line(a, _)
                            | OutlineCurve::Quad(a, _, _)
                            | OutlineCurve::Cubic(a, _, _, _) => a,
                        };
                        if last != Some(start) {
                            let (sx, sy) = point(start);
                            path_builder.move_to(sx, sy);
                        }
                        last = Some(match curve {
                            OutlineCurve::Line(_, b) => {
                                let (bx, by) = point(b);
                                path_builder.line_to(bx, by);
                                b
                            }
                            OutlineCurve::Quad(_, c, b) => {
                                let ((cx, cy), (bx, by)) = (point(c), point(b));
                                path_builder.quad_to(cx, cy, bx, by);
                                b
                            }
                            OutlineCurve::Cubic(_, c1, c2, b) => {
                                let ((ax, ay), (cx, cy), (bx, by)) = (point(c1), point(c2), point(b));
                                path_builder.cubic_to(ax, ay, cx, cy, bx, by);
                                b
                            }
                        });
                    }
                }
                x += active_font.h_advance_unscaled(id) * scale;
                previous = Some((active_font, id));
            }
        }

        if let Some(p) = path_builder.finish() {
            target.fill_path(
                &p,
                &color.paint(),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }

        font.set_variation(b"wght", FONT_WEIGHT);
        for fallback in fallback_fonts.iter_mut() {
            fallback.set_variation(b"wght", FONT_WEIGHT);
        }
        x
    }

    fn draw_text_on_main(
        &mut self,
        x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        weight: f32,
        color: Rgb,
    ) -> f32 {
        Self::draw_text_impl(
            &mut self.font,
            &mut self.fallback_fonts,
            &self.emoji_face,
            &self.emoji_cache,
            &mut self.pixmap,
            x,
            baseline,
            text,
            size,
            weight,
            color,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_on_target(
        &mut self,
        target: &mut Pixmap,
        x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        weight: f32,
        color: Rgb,
    ) -> f32 {
        Self::draw_text_impl(
            &mut self.font,
            &mut self.fallback_fonts,
            &self.emoji_face,
            &self.emoji_cache,
            target,
            x,
            baseline,
            text,
            size,
            weight,
            color,
        )
    }


    fn cap_height(&self, size: f32) -> f32 {
        let scale = size / self.font.units_per_em().unwrap_or(1000.0);
        self.font
            .outline(self.font.glyph_id('H'))
            .map_or(size * 0.7, |outline| outline.bounds.height().abs() * scale)
    }

    fn text_width_at(&mut self, text: &str, size: f32, weight: f32) -> f32 {
        self.font.set_variation(b"wght", weight);
        for fallback in &mut self.fallback_fonts {
            fallback.set_variation(b"wght", weight);
        }
        let width = self.text_width(text, size);
        self.font.set_variation(b"wght", FONT_WEIGHT);
        for fallback in &mut self.fallback_fonts {
            fallback.set_variation(b"wght", FONT_WEIGHT);
        }
        width
    }

    fn center_text(&mut self, cx: f32, cy: f32, text: &str, size: f32, color: Rgb) {
        let x = (cx - self.text_width(text, size) / 2.0).round();
        let baseline = (cy + self.cap_height(size) / 2.0).round();
        let mut path = PathBuilder::new();
        self.append_text(&mut path, x, baseline, text, size, FONT_WEIGHT);
        if let Some(path) = path.finish() {
            self.fill(&path, color);
        }
    }

    /// Appends the glyph outlines of `text` at `weight` and returns the pen
    /// position after the last glyph.
    fn append_text(
        &mut self,
        path: &mut PathBuilder,
        x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        weight: f32,
    ) -> f32 {
        self.append_text_styled(path, x, baseline, text, size, weight, false)
    }

    fn append_text_styled(
        &mut self,
        path: &mut PathBuilder,
        mut x: f32,
        baseline: f32,
        text: &str,
        size: f32,
        weight: f32,
        italic: bool,
    ) -> f32 {
        let slant = if italic { 0.22f32 } else { 0.0f32 };
        self.font.set_variation(b"wght", weight);
        for fallback in &mut self.fallback_fonts {
            fallback.set_variation(b"wght", weight);
        }
        let mut previous: Option<(&FontVec, ab_glyph::GlyphId)> = None;
        for character in text.chars() {
            if is_ignorable_format_or_control(character) {
                continue;
            }
            let (font, id) = self.glyph_font_and_id(character);
            if id.0 != 0 {
                let scale = size / font.units_per_em().unwrap_or(1000.0);
                if let Some((prev_font, prev_id)) = previous
                    && std::ptr::eq(font, prev_font) {
                        x += font.kern_unscaled(prev_id, id) * scale;
                    }
                if let Some(outline) = font.outline(id) {
                    let point = |p: ab_glyph::Point| (x + (p.x + p.y * slant) * scale, baseline - p.y * scale);
                    let mut last = None;
                    for curve in outline.curves {
                        let start = match curve {
                            OutlineCurve::Line(a, _)
                            | OutlineCurve::Quad(a, _, _)
                            | OutlineCurve::Cubic(a, _, _, _) => a,
                        };
                        if last != Some(start) {
                            let (sx, sy) = point(start);
                            path.move_to(sx, sy);
                        }
                        last = Some(match curve {
                            OutlineCurve::Line(_, b) => {
                                let (bx, by) = point(b);
                                path.line_to(bx, by);
                                b
                            }
                            OutlineCurve::Quad(_, c, b) => {
                                let ((cx, cy), (bx, by)) = (point(c), point(b));
                                path.quad_to(cx, cy, bx, by);
                                b
                            }
                            OutlineCurve::Cubic(_, c1, c2, b) => {
                                let ((ax, ay), (cx, cy), (bx, by)) = (point(c1), point(c2), point(b));
                                path.cubic_to(ax, ay, cx, cy, bx, by);
                                b
                            }
                        });
                    }
                }
                x += font.h_advance_unscaled(id) * scale;
                previous = Some((font, id));
            }
        }
        self.font.set_variation(b"wght", FONT_WEIGHT);
        for fallback in &mut self.fallback_fonts {
            fallback.set_variation(b"wght", FONT_WEIGHT);
        }
        x
    }
}

fn load_font(custom_name: Option<&str>) -> Result<FontVec> {
    if let Some(name) = custom_name.filter(|s| !s.trim().is_empty()) {
        let trimmed = name.trim();
        let path = if trimmed.starts_with('~') {
            if let Some(home) = std::env::var_os("HOME") {
                PathBuf::from(home).join(
                    trimmed
                        .strip_prefix('~')
                        .unwrap_or(trimmed)
                        .trim_start_matches('/'),
                )
            } else {
                PathBuf::from(trimmed)
            }
        } else {
            PathBuf::from(trimmed)
        };
        if path.is_file() {
            match fs::read(&path) {
                Ok(data) => match FontVec::try_from_vec(data) {
                    Ok(mut font) => {
                        font.set_variation(b"wght", FONT_WEIGHT);
                        return Ok(font);
                    }
                    Err(error) => {
                        eprintln!(
                            "t2-touchbar-ultra: parse custom font file {}: {error:#}",
                            path.display()
                        );
                    }
                },
                Err(error) => {
                    eprintln!(
                        "t2-touchbar-ultra: read custom font file {}: {error:#}",
                        path.display()
                    );
                }
            }
        } else if let Ok(output) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", trimmed])
            .output()
        {
            if output.status.success() {
                let matched_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !matched_path.is_empty() {
                    let p = FsPath::new(&matched_path);
                    if p.is_file() {
                        match fs::read(p) {
                            Ok(data) => match FontVec::try_from_vec(data) {
                                Ok(mut font) => {
                                    font.set_variation(b"wght", FONT_WEIGHT);
                                    return Ok(font);
                                }
                                Err(error) => {
                                    eprintln!(
                                        "t2-touchbar-ultra: parse font from fc-match {matched_path}: {error:#}"
                                    );
                                }
                            },
                            Err(error) => {
                                eprintln!(
                                    "t2-touchbar-ultra: read font from fc-match {matched_path}: {error:#}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    for path in FONT_PATHS {
        let Ok(data) = fs::read(path) else { continue };
        let mut font = FontVec::try_from_vec(data).with_context(|| format!("parse font {path}"))?;
        font.set_variation(b"wght", FONT_WEIGHT);
        return Ok(font);
    }
    Err(anyhow!(
        "no Touch Bar font found; install adwaita-sans-fonts ({})",
        FONT_PATHS.join(", ")
    ))
}

fn load_fallback_fonts() -> Vec<FontVec> {
    let mut fonts = Vec::new();
    const CJK_PATHS: &[&str] = &[
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    ];
    for path in CJK_PATHS {
        if let Ok(data) = fs::read(path)
            && let Ok(mut font) = FontVec::try_from_vec_and_index(data, 0)
        {
            font.set_variation(b"wght", FONT_WEIGHT);
            fonts.push(font);
            break;
        }
    }
    if fonts.is_empty() {
        for lang in [":lang=ja", ":lang=zh", ":lang=ko"] {
            if let Ok(output) = std::process::Command::new("fc-match")
                .args(["-f", "%{file}", lang])
                .output()
                && output.status.success()
            {
                let p = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !p.is_empty()
                    && FsPath::new(&p).is_file()
                    && let Ok(data) = fs::read(&p)
                    && let Ok(mut font) = FontVec::try_from_vec_and_index(data, 0)
                {
                    font.set_variation(b"wght", FONT_WEIGHT);
                    fonts.push(font);
                    break;
                }
            }
        }
    }

    // Comprehensive Unicode fallback fonts: Math, Symbols, Indic, Arabic, Hebrew, Thai, DejaVu
    const SCRIPT_AND_SYMBOL_PATHS: &[&str] = &[
        "/usr/share/fonts/noto/NotoSansMath-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/noto/NotoMusic-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansTelugu-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansBengali-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansDevanagari-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansThai-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansTamil-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansKannada-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMalayalam-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansGujarati-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansGurmukhi-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansKhmer-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMyanmar-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSinhala-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansLao-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansEthiopic-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansCherokee-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansCanadianAboriginal-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMongolian-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansArmenian-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansGeorgian-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ];
    for path in SCRIPT_AND_SYMBOL_PATHS {
        if let Ok(data) = fs::read(path)
            && let Ok(mut font) = FontVec::try_from_vec_and_index(data, 0)
        {
            font.set_variation(b"wght", FONT_WEIGHT);
            fonts.push(font);
        }
    }

    fonts
}

enum Shape {
    Stroke(Path),
    Solid(Path),
    /// A crossing stroke that cuts a gap into the strokes below it.
    Slash(Path),
}

impl Shape {
    fn path(&self) -> &Path {
        match self {
            Self::Stroke(path) | Self::Solid(path) | Self::Slash(path) => path,
        }
    }

    fn pad(&self) -> f32 {
        match self {
            Self::Stroke(_) | Self::Slash(_) => ICON_STROKE / 2.0,
            Self::Solid(_) => 0.7,
        }
    }
}

fn icon_shapes(icon: Icon) -> Vec<Shape> {
    let mut shapes = Vec::new();
    let build = |build: &dyn Fn(&mut PathBuilder)| {
        let mut path = PathBuilder::new();
        build(&mut path);
        path.finish()
    };
    match icon {
        Icon::Brightness(up) => {
            let (core, inner, outer) = if up {
                (4.6, 8.2, 11.0)
            } else {
                (3.6, 7.0, 8.0)
            };
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 0.0, core);
                rays(path, 0.0, 0.0, inner, outer, 0.0, 360.0, 8);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Keyboard(up) => {
            let (inner, outer) = if up { (7.5, 10.5) } else { (7.0, 8.0) };
            if let Some(path) = build(&|path| {
                arc(path, 0.0, 3.0, 4.2, 180.0, 360.0);
                path.close();
                rays(path, 0.0, 3.0, inner, outer, 180.0, 360.0, 5);
                path.move_to(-6.0, 8.0);
                path.line_to(6.0, 8.0);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Microphone => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-3.6, -11.0, 3.6, 2.5, 3.6));
                arc(path, 0.0, -2.0, 7.4, 0.0, 180.0);
                path.move_to(0.0, 5.4);
                path.line_to(0.0, 9.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.move_to(-8.5, -10.0);
                path.line_to(8.5, 9.0);
            }) {
                shapes.push(Shape::Slash(path));
            }
        }
        Icon::Previous | Icon::Next => {
            let side = if matches!(icon, Icon::Next) {
                1.0
            } else {
                -1.0
            };
            if let Some(path) = build(&|path| {
                for offset in [-9.0f32, 0.5] {
                    path.move_to(side * offset, -6.8);
                    path.line_to(side * (offset + 9.5), 0.0);
                    path.line_to(side * offset, 6.8);
                    path.close();
                }
                // The bar at the tips marks a track skip (|◀◀, ▶▶|).
                let (near, far) = (side * 11.2, side * 13.8);
                path.push_path(&rounded_rect(near.min(far), -6.8, near.max(far), 6.8, 0.6));
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Play => {
            if let Some(path) = build(&|path| {
                path.move_to(-5.5, -7.2);
                path.line_to(6.5, 0.0);
                path.line_to(-5.5, 7.2);
                path.close();
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::PlayPause => {
            if let Some(path) = build(&|path| {
                path.move_to(-11.0, -6.8);
                path.line_to(-1.5, 0.0);
                path.line_to(-11.0, 6.8);
                path.close();
                path.push_path(&rounded_rect(2.5, -6.8, 5.5, 6.8, 0.6));
                path.push_path(&rounded_rect(8.0, -6.8, 11.0, 6.8, 0.6));
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Mute | Icon::Volume(_) => {
            if let Some(path) = build(&|path| {
                path.move_to(-10.0, -3.6);
                path.line_to(-6.0, -3.6);
                path.line_to(-0.8, -8.4);
                path.line_to(-0.8, 8.4);
                path.line_to(-6.0, 3.6);
                path.line_to(-10.0, 3.6);
                path.close();
            }) {
                shapes.push(Shape::Solid(path));
            }
            let waves: &[f32] = match icon {
                Icon::Volume(true) => &[4.6, 8.4, 12.2],
                Icon::Volume(false) => &[4.6],
                _ => &[],
            };
            if let Some(path) = build(&|path| {
                for radius in waves {
                    let sweep = 46.0 - radius * 1.2;
                    arc(path, 0.8, 0.0, *radius, -sweep, sweep);
                }
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if matches!(icon, Icon::Mute)
                && let Some(path) = build(&|path| {
                    path.move_to(-11.0, -9.5);
                    path.line_to(6.0, 9.5);
                })
            {
                shapes.push(Shape::Slash(path));
            }
        }
        Icon::KeyboardDevice => {
            // Outer body outline matching Apple SF Symbols style
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-9.5, -6.0, 9.5, 6.0, 2.2));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            // Keys inside: 4 circles on top, 1 circle + spacebar + 1 circle on bottom
            if let Some(path) = build(&|path| {
                // Top row: 4 circular dots
                for x in [-6.0, -2.0, 2.0, 6.0] {
                    path.push_circle(x, -2.1, 1.05);
                }
                // Bottom row: left and right dots
                path.push_circle(-6.0, 2.1, 1.05);
                path.push_circle(6.0, 2.1, 1.05);
                // Spacebar pill
                path.push_path(&rounded_rect(-3.3, 1.05, 3.3, 3.15, 1.05));
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Ram => {
            // Memory board outline
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-10.0, -6.5, 10.0, 6.5, 1.5));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            // 3 small memory chips inside
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-7.5, -4.0, -3.5, 2.0, 0.8));
                path.push_path(&rounded_rect(-2.0, -4.0, 2.0, 2.0, 0.8));
                path.push_path(&rounded_rect(3.5, -4.0, 7.5, 2.0, 0.8));
            }) {
                shapes.push(Shape::Solid(path));
            }
            // Connector pins/contacts along the bottom
            if let Some(path) = build(&|path| {
                for x in [-7.0, -4.5, -2.0, 2.0, 4.5, 7.0] {
                    path.move_to(x, 4.0);
                    path.line_to(x, 6.5);
                }
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Cpu => {
            // Main CPU square package
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-6.5, -6.5, 6.5, 6.5, 1.5));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            // Inner silicon die / core
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-3.0, -3.0, 3.0, 3.0, 0.8));
            }) {
                shapes.push(Shape::Solid(path));
            }
            // Connector pins on all 4 sides
            if let Some(path) = build(&|path| {
                for pos in [-4.0, 0.0, 4.0] {
                    // Top pins
                    path.move_to(pos, -6.5);
                    path.line_to(pos, -9.5);
                    // Bottom pins
                    path.move_to(pos, 6.5);
                    path.line_to(pos, 9.5);
                    // Left pins
                    path.move_to(-6.5, pos);
                    path.line_to(-9.5, pos);
                    // Right pins
                    path.move_to(6.5, pos);
                    path.line_to(9.5, pos);
                }
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Temperature => {
            // Thermometer outline with ticks
            if let Some(path) = build(&|path| {
                path.move_to(-1.8, 0.5);
                path.line_to(-1.8, -7.0);
                arc_to(path, 0.0, -7.0, 1.8, 180.0, 360.0);
                path.line_to(1.8, 0.5);
                arc_to(path, 0.0, 3.5, 3.5, -59.0, 239.0);
                path.close();
                // 3 tick marks on the right side
                path.move_to(3.2, -6.0);
                path.line_to(5.2, -6.0);
                path.move_to(3.2, -3.0);
                path.line_to(5.2, -3.0);
                path.move_to(3.2, 0.0);
                path.line_to(5.2, 0.0);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            // Inner mercury bulb and column
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 3.5, 2.0);
                path.push_path(&rounded_rect(-0.9, -4.5, 0.9, 3.5, 0.9));
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Window => {
            // Window frame outline
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-9.0, -7.0, 9.0, 7.0, 1.5));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            // Title bar divider line
            if let Some(path) = build(&|path| {
                path.move_to(-9.0, -2.5);
                path.line_to(9.0, -2.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            // Title bar window controls
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-6.5, -5.2, -4.5, -3.8, 0.5));
                path.push_path(&rounded_rect(-3.5, -5.2, -1.5, -3.8, 0.5));
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Fullscreen => {
            if let Some(path) = build(&|path| {
                // Top-left
                path.move_to(-8.5, -3.5);
                path.line_to(-8.5, -8.5);
                path.line_to(-3.5, -8.5);
                // Top-right
                path.move_to(3.5, -8.5);
                path.line_to(8.5, -8.5);
                path.line_to(8.5, -3.5);
                // Bottom-left
                path.move_to(-8.5, 3.5);
                path.line_to(-8.5, 8.5);
                path.line_to(-3.5, 8.5);
                // Bottom-right
                path.move_to(3.5, 8.5);
                path.line_to(8.5, 8.5);
                path.line_to(8.5, 3.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Emoji => {
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 0.0, 8.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_circle(-3.2, -2.5, 1.2);
                path.push_circle(3.2, -2.5, 1.2);
            }) {
                shapes.push(Shape::Solid(path));
            }
            if let Some(path) = build(&|path| {
                arc(path, 0.0, 0.5, 5.0, 25.0, 155.0);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Palette => {
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 0.0, 8.5);
                path.push_circle(3.2, 3.2, 1.8);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_circle(-3.5, -4.2, 1.3);
                path.push_circle(1.0, -5.0, 1.3);
                path.push_circle(-5.0, 0.0, 1.3);
                path.push_circle(-3.0, 4.0, 1.3);
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Screenshot => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-8.5, -4.5, 8.5, 7.5, 2.0));
                path.push_path(&rounded_rect(-4.0, -7.5, -0.5, -4.5, 1.0));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 1.5, 3.4);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 1.5, 1.2);
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Terminal => {
            if let Some(path) = build(&|path| {
                path.move_to(-7.5, -5.0);
                path.line_to(-2.5, -1.0);
                path.line_to(-7.5, 3.0);
                path.move_to(0.5, 3.5);
                path.line_to(7.5, 3.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Browser => {
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 0.0, 8.5);
                path.move_to(-8.5, 0.0);
                path.line_to(8.5, 0.0);
                path.push_oval(Rect::from_ltrb(-4.0, -8.5, 4.0, 8.5).unwrap());
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Folder => {
            if let Some(path) = build(&|path| {
                path.move_to(-8.5, 6.5);
                path.line_to(-8.5, -4.5);
                path.line_to(-3.5, -4.5);
                path.line_to(-1.5, -2.5);
                path.line_to(8.5, -2.5);
                path.line_to(8.5, 6.5);
                path.close();
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Calculator => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-6.5, -8.5, 6.5, 8.5, 2.0));
                path.push_path(&rounded_rect(-4.5, -6.5, 4.5, -3.5, 0.8));
                path.push_path(&rounded_rect(-4.5, -1.5, -1.0, 1.5, 0.6));
                path.push_path(&rounded_rect(1.0, -1.5, 4.5, 1.5, 0.6));
                path.push_path(&rounded_rect(-4.5, 3.5, -1.0, 6.5, 0.6));
                path.push_path(&rounded_rect(1.0, 3.5, 4.5, 6.5, 0.6));
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Lock => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-6.0, -1.0, 6.0, 8.0, 1.8));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                arc(path, 0.0, -1.0, 4.2, 180.0, 360.0);
                path.move_to(-4.2, -1.0);
                path.line_to(-4.2, 0.0);
                path.move_to(4.2, -1.0);
                path.line_to(4.2, 0.0);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Settings => {
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 0.0, 3.8);
                rays(path, 0.0, 0.0, 5.0, 8.5, 0.0, 360.0, 6);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Music => {
            if let Some(path) = build(&|path| {
                path.push_oval(Rect::from_ltrb(-7.0, 2.5, -2.5, 7.0).unwrap());
                path.push_oval(Rect::from_ltrb(1.5, 0.5, 6.0, 5.0).unwrap());
            }) {
                shapes.push(Shape::Solid(path));
            }
            if let Some(path) = build(&|path| {
                path.move_to(-3.0, 4.0);
                path.line_to(-3.0, -5.5);
                path.line_to(5.5, -7.5);
                path.line_to(5.5, 2.0);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Search => {
            if let Some(path) = build(&|path| {
                path.push_circle(-2.5, -2.5, 5.5);
                path.move_to(1.5, 1.5);
                path.line_to(7.5, 7.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Plug => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-5.0, -1.0, 5.0, 5.5, 1.5));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-3.5, -6.5, -1.8, -1.0, 0.6));
                path.push_path(&rounded_rect(1.8, -6.5, 3.5, -1.0, 0.6));
            }) {
                shapes.push(Shape::Solid(path));
            }
            if let Some(path) = build(&|path| {
                path.move_to(0.0, 5.5);
                path.line_to(0.0, 8.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Battery(percent) => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-8.5, -5.0, 6.5, 5.0, 1.8));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(6.5, -2.5, 8.5, 2.5, 0.8));
            }) {
                shapes.push(Shape::Solid(path));
            }
            let fill_w = 12.0 * (percent as f32 / 100.0).clamp(0.0, 1.0);
            if fill_w > 0.5 {
                if let Some(path) = build(&|path| {
                    path.push_path(&rounded_rect(-7.0, -3.5, -7.0 + fill_w, 3.5, 1.0));
                }) {
                    shapes.push(Shape::Solid(path));
                }
            }
        }
        Icon::BatteryCharging => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-8.5, -5.0, 6.5, 5.0, 1.8));
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(6.5, -2.5, 8.5, 2.5, 0.8));
            }) {
                shapes.push(Shape::Solid(path));
            }
            if let Some(path) = build(&|path| {
                path.move_to(0.5, -4.0);
                path.line_to(-2.5, 0.0);
                path.line_to(0.0, 0.0);
                path.line_to(-0.5, 4.0);
                path.line_to(2.5, 0.0);
                path.line_to(0.0, 0.0);
                path.close();
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
    }
    shapes
}

/// Appends an open arc. Angles are in degrees, clockwise from +x.
fn arc(path: &mut PathBuilder, cx: f32, cy: f32, radius: f32, from: f32, to: f32) {
    let steps = ((to - from).abs() / 6.0).ceil().max(2.0) as usize;
    for step in 0..=steps {
        let angle = (from + (to - from) * step as f32 / steps as f32).to_radians();
        let (x, y) = (cx + radius * angle.cos(), cy + radius * angle.sin());
        if step == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
}

/// Appends an arc connecting with a line from the current path position.
fn arc_to(path: &mut PathBuilder, cx: f32, cy: f32, radius: f32, from: f32, to: f32) {
    let steps = ((to - from).abs() / 6.0).ceil().max(2.0) as usize;
    for step in 0..=steps {
        let angle = (from + (to - from) * step as f32 / steps as f32).to_radians();
        let (x, y) = (cx + radius * angle.cos(), cy + radius * angle.sin());
        path.line_to(x, y);
    }
}

#[allow(clippy::too_many_arguments)]
fn rays(
    path: &mut PathBuilder,
    cx: f32,
    cy: f32,
    inner: f32,
    outer: f32,
    from: f32,
    to: f32,
    count: usize,
) {
    let full = (to - from - 360.0).abs() < f32::EPSILON;
    let divisions = if full { count } else { count - 1 };
    for index in 0..count {
        let angle = (from + (to - from) * index as f32 / divisions as f32).to_radians();
        let (sin, cos) = angle.sin_cos();
        path.move_to(cx + inner * cos, cy + inner * sin);
        path.line_to(cx + outer * cos, cy + outer * sin);
    }
}

fn rounded_rect(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> Path {
    let radius = radius.min((right - left) / 2.0).min((bottom - top) / 2.0);
    let mut path = PathBuilder::new();
    // Cubic quarter circles with the usual 0.5523 control distance.
    let k = radius * (1.0 - 0.552_284_8);
    path.move_to(left + radius, top);
    path.line_to(right - radius, top);
    path.cubic_to(right - k, top, right, top + k, right, top + radius);
    path.line_to(right, bottom - radius);
    path.cubic_to(right, bottom - k, right - k, bottom, right - radius, bottom);
    path.line_to(left + radius, bottom);
    path.cubic_to(left + k, bottom, left, bottom - k, left, bottom - radius);
    path.line_to(left, top + radius);
    path.cubic_to(left, top + k, left + k, top, left + radius, top);
    path.close();
    path.finish()
        .expect("rounded rectangle has a non-empty outline")
}

fn capsule_fill_path(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
    total_right: f32,
) -> Path {
    let left_r = radius.min((right - left) / 2.0).min((bottom - top) / 2.0);
    // Right corners are flat (0.0) while moving. Only when reaching the end of the
    // capsule (within `radius` of `total_right`) do they curve to fit the outer boundary.
    let right_r = if right > total_right - radius {
        radius * ((right - (total_right - radius)) / radius).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let right_r = right_r.min((right - left) / 2.0).min((bottom - top) / 2.0);

    let mut path = PathBuilder::new();
    let kl = left_r * (1.0 - 0.552_284_8);
    let kr = right_r * (1.0 - 0.552_284_8);

    path.move_to(left + left_r, top);
    path.line_to(right - right_r, top);
    if right_r > 0.1 {
        path.cubic_to(right - kr, top, right, top + kr, right, top + right_r);
        path.line_to(right, bottom - right_r);
        path.cubic_to(right, bottom - kr, right - kr, bottom, right - right_r, bottom);
    } else {
        path.line_to(right, top);
        path.line_to(right, bottom);
    }
    path.line_to(left + left_r, bottom);
    path.cubic_to(left + kl, bottom, left, bottom - kl, left, bottom - left_r);
    path.line_to(left, top + left_r);
    path.cubic_to(left, top + kl, left + kl, top, left + left_r, top);
    path.close();
    path.finish().expect("capsule fill path has non-empty outline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_have_no_overlapping_buttons() {
        for mode in [Mode::Media, Mode::Function, Mode::Special] {
            let layout = Layout::new(mode, 2170, false);
            for pair in layout.buttons.windows(2) {
                assert!(pair[0].right < pair[1].left);
            }
        }
    }

    #[test]
    fn esc_and_row_edges_stay_put_across_modes() {
        let media = Layout::new(Mode::Media, 2170, false);
        let function = Layout::new(Mode::Function, 2170, false);
        let edges = |layout: &Layout| {
            let first = &layout.buttons[0];
            let last = &layout.buttons[layout.buttons.len() - 1];
            (first.left, first.right, layout.buttons[1].left, last.right)
        };
        assert_eq!(edges(&media), edges(&function));
        // The special row keeps esc and the media width.
        let special = Layout::new(Mode::Special, 2170, false);
        let esc = &special.buttons[0];
        assert_eq!(
            (esc.left, esc.right),
            (media.buttons[0].left, media.buttons[0].right)
        );
    }

    #[test]
    fn personal_keys_fill_only_the_free_space() {
        let labels = ["~", "|", "a", "b", "c", "d", "e"];
        let special = Layout::with_custom(Mode::Special, 2170, false, &labels);
        let custom: Vec<_> = special
            .buttons
            .iter()
            .filter(|button| matches!(button.action, Action::Custom(_)))
            .collect();
        assert_eq!(custom.len(), 7, "all custom keys fit on the special panel");
        assert_eq!(custom[0].action, Action::Custom(0));
        assert!(custom[0].left >= special.buttons[0].right + GROUP_GAP);
        for pair in special.buttons.windows(2) {
            assert!(pair[0].right < pair[1].left);
        }
    }

    #[test]
    fn touch_id_draws_only_near_the_sensor_and_label() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        canvas.touch_id("waiting", 0.5, None, false);
        assert!(canvas.pixels.iter().any(|pixel| *pixel != 0));
        assert_eq!(canvas.pixels[0], 0);

        canvas.touch_id("matched", 0.0, None, false);
        assert!(canvas.pixels.iter().any(|pixel| *pixel != 0));

        canvas.touch_id("retry", 0.5, None, false);
        assert!(canvas.pixels.iter().any(|pixel| *pixel != 0));

        canvas.touch_id("failed", 0.0, None, false);
        assert!(canvas.pixels.iter().any(|pixel| *pixel != 0));
    }

    #[test]
    fn custom_font_and_sizes_can_be_loaded() {
        let canvas = Canvas::new(
            2170,
            60,
            0x00dce6ff,
            Some("JetBrains Mono"),
            Some(26.0),
            Some(22.0),
            None,
        )
        .unwrap();
        assert_eq!(canvas.key_font_size, 26.0);
        assert_eq!(canvas.status_date_size, 22.0);
        assert_eq!(canvas.status_text_size, 22.0);
    }

    #[test]
    #[allow(clippy::chunks_exact_to_as_chunks)]
    fn test_freetype_emoji() {
        let face = {
            let lib = freetype::Library::init().unwrap();
            lib.new_face("/usr/share/fonts/noto/NotoColorEmoji.ttf", 0).unwrap()
        };
        let _ = face.select_size(0);
        let char_code = '🔥' as usize;
        face.load_char(char_code, freetype::face::LoadFlag::COLOR).unwrap();
        let glyph = face.glyph();
        glyph.render_glyph(freetype::render_mode::RenderMode::Normal).unwrap();
        let bitmap = glyph.bitmap();
        let w = bitmap.width() as u32;
        let h = bitmap.rows() as u32;
        let mut emoji_pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
        for (src, dst) in bitmap.buffer().chunks_exact(4).zip(emoji_pixmap.pixels_mut()) {
            let a = src[3];
            let r = src[2].min(a);
            let g = src[1].min(a);
            let b = src[0].min(a);
            *dst = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a)
                .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
        }
        let mut dest = tiny_skia::Pixmap::new(60, 60).unwrap();
        let scale = 24.0 / 128.0;
        let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(10.0, 10.0);
        let paint = tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        };
        dest.draw_pixmap(0, 0, emoji_pixmap.as_ref(), &paint, transform, None);
        assert!(dest.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn test_cjk_load() {
        if let Ok(data) = fs::read("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc") {
            let font = FontVec::try_from_vec_and_index(data, 0).unwrap();
            let gid = font.glyph_id('漢');
            assert_ne!(gid.0, 0);
        }
    }

    #[test]
    fn cjk_text_renders_with_outlines_and_width() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let cjk_title = "YOASOBI – アイドル (アイドル)";
        let w = canvas.text_width(cjk_title, 24.0);
        assert!(w > 100.0, "CJK text width must be non-zero and measured");

        let mut path = PathBuilder::new();
        let x = canvas.append_text(&mut path, 10.0, 35.0, cjk_title, 24.0, FONT_WEIGHT);
        assert!(x > 100.0);
        let path = path.finish();
        assert!(path.is_some(), "CJK outlines must produce vector paths");
    }

    #[test]
    fn dynamic_custom_keys_expand_for_longer_labels() {
        let labels = [
            "~",
            "App1",
            "MediumName",
            "Super Long Application Name That Exceeds Media Button Width",
        ];
        let layout = Layout::with_custom(Mode::Special, 2170, true, &labels);
        assert_eq!(layout.buttons.len(), 4);
        let w0 = layout.buttons[0].right - layout.buttons[0].left;
        let w3 = layout.buttons[3].right - layout.buttons[3].left;
        assert!(w3 > w0, "Long name button ({w3}px) should be wider than ~ button ({w0}px)");
    }

    #[test]
    fn custom_keys_with_icons_render_compact() {
        let labels = ["fullscreen", "emoji", "screenshot"];
        let layout = Layout::with_custom(Mode::Special, 2170, true, &labels);
        assert_eq!(layout.buttons.len(), 3);
        let media = Layout::new(Mode::Media, 2170, true);
        let expected_w = media.buttons[0].right - media.buttons[0].left;
        for button in &layout.buttons {
            let w = button.right - button.left;
            assert!(
                (w as i32 - expected_w as i32).abs() <= 1,
                "Icon button width ({w}px) should match media button width ({expected_w}px)"
            );
            assert!(matches!(button.glyph, Glyph::Icon(_) | Glyph::Svg(_)));
        }
    }

    #[test]
    fn web_app_icons_resolve_and_render() {
        let app_names = [
            "Gemini",
            "Chatgpt",
            "Claude",
            "Grok",
            "Qwen",
            "Youtube",
            "GoogleDrive",
            "Lampa",
            "2Gis",
        ];
        let items: Vec<CustomItem> = app_names
            .iter()
            .map(|&name| {
                let svg = load_icon_svg(name, 64, 44);
                CustomItem {
                    label: name,
                    icon: None,
                    svg,
                }
            })
            .collect();
        let layout = Layout::with_custom(Mode::Special, 2170, true, &items);
        assert_eq!(layout.buttons.len(), 9);
        for button in &layout.buttons {
            assert!(matches!(button.glyph, Glyph::Svg(_)));
        }
    }

    #[test]
    fn all_custom_svg_icons_exist_and_load() {
        let all_names = [
            "fullscreen",
            "emoji",
            "screenshot",
            "Gemini",
            "Chatgpt",
            "Claude",
            "Grok",
            "Qwen",
            "Youtube",
            "GoogleDrive",
            "Lampa",
            "2Gis",
        ];
        for name in all_names {
            let svg = load_icon_svg(name, 64, 44);
            assert!(svg.is_some(), "SVG icon for '{name}' should load successfully");
        }
    }

    #[test]
    fn idle_status_bar_right_widgets_have_fixed_width() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let reserved_w = canvas.text_width("100%", canvas.status_text_size);
        let scale = (canvas.status_text_size / 20.0) * 0.85;
        let w_1digit = canvas.draw_icon_text(
            1000.0,
            30.0,
            Icon::Cpu,
            scale,
            "3%",
            canvas.status_text_size,
            canvas.color,
            canvas.color,
            reserved_w,
        );
        let w_2digit = canvas.draw_icon_text(
            1000.0,
            30.0,
            Icon::Cpu,
            scale,
            "24%",
            canvas.status_text_size,
            canvas.color,
            canvas.color,
            reserved_w,
        );
        let w_3digit = canvas.draw_icon_text(
            1000.0,
            30.0,
            Icon::Cpu,
            scale,
            "100%",
            canvas.status_text_size,
            canvas.color,
            canvas.color,
            reserved_w,
        );
        assert_eq!(w_1digit, w_2digit, "1-digit and 2-digit slot widths must be identical");
        assert_eq!(w_2digit, w_3digit, "2-digit and 3-digit slot widths must be identical");
    }

    #[test]
    fn test_cycling_widget_timing_and_slot_width() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let items = [
            CyclingItem { icon: Icon::Cpu, text: "15%".to_string(), color: canvas.color },
            CyclingItem { icon: Icon::Ram, text: "42%".to_string(), color: canvas.color },
            CyclingItem { icon: Icon::Temperature, text: "45°C".to_string(), color: canvas.color },
        ];
        let reserved_w = canvas
            .text_width("100%", canvas.status_text_size)
            .max(canvas.text_width("100°C", canvas.status_text_size));
        let scale = (canvas.status_text_size / 20.0) * 0.85;

        // Steady state at t = 1000ms: CPU
        let (w1, anim1, wake1) = canvas.draw_cycling_widget(
            1000.0,
            30.0,
            &items,
            scale,
            canvas.status_text_size,
            reserved_w,
            1000,
        );
        assert!(!anim1);
        assert_eq!(wake1, Some(5000));

        // Next item at t = 5000ms: RAM
        let (w2, anim2, wake2) = canvas.draw_cycling_widget(
            1000.0,
            30.0,
            &items,
            scale,
            canvas.status_text_size,
            reserved_w,
            5000,
        );
        assert!(!anim2);
        assert_eq!(wake2, Some(10000));

        // Slot width should be identical
        assert_eq!(w1, w2);
    }

    #[test]
    fn test_canvas_custom_theme_application() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let mut custom_theme = Theme::default();
        custom_theme.status.clock.time = Some(Rgb(255, 0, 0));
        custom_theme.status.sysmon.cpu = Some(Rgb(0, 255, 0));
        canvas.set_theme(custom_theme);

        assert_eq!(canvas.theme.status.clock.time, Some(Rgb(255, 0, 0)));
        assert_eq!(canvas.theme.status.sysmon.cpu, Some(Rgb(0, 255, 0)));
    }

    #[test]
    fn test_cycling_clock_timing_and_fixed_slot_width() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let clock_data = status::ClockWidgetData {
            time: "08:36".to_string(),
            weekday: "Четверг".to_string(),
            date: "29 октября".to_string(),
        };
        let date_size = canvas.status_date_size;
        let reserved_clock_w = canvas
            .text_width("30 сентября", date_size)
            .max(canvas.text_width("Понедельник", date_size))
            .max(canvas.text_width("Воскресенье", date_size))
            + 8.0;
        let slot_w = reserved_clock_w
            .max(canvas.text_width(&clock_data.time, date_size))
            .max(canvas.text_width(&clock_data.weekday, date_size))
            .max(canvas.text_width(&clock_data.date, date_size));
        let items = [
            clock_data.time.as_str(),
            clock_data.weekday.as_str(),
            clock_data.date.as_str(),
        ];

        // Item 1 at t = 1000ms: Time ("08:36")
        let (w1, anim1, wake1) = canvas.draw_cycling_clock(
            10.0,
            30.0,
            &items,
            date_size,
            canvas.color,
            slot_w,
            1000,
        );
        assert!(!anim1);
        assert_eq!(wake1, Some(5000));
        assert_eq!(w1, slot_w);

        // Item 2 at t = 5000ms: Weekday
        let (w2, anim2, wake2) = canvas.draw_cycling_clock(
            10.0,
            30.0,
            &items,
            date_size,
            canvas.color,
            slot_w,
            5000,
        );
        assert!(!anim2);
        assert_eq!(wake2, Some(10000));
        assert_eq!(w2, slot_w);
    }

    #[test]
    fn idle_status_bar_renders_without_panics() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let layout = Layout::new(Mode::Media, 2170, false);
        let active_win = niri::ActiveWindow {
            title: "kitty – fish".to_string(),
            app_id: Some("kitty".to_string()),
            pid: None,
        };
        let clock = status::ClockWidgetData::default();
        let (needs_fast_anim, next_wake) = canvas.idle_status_bar(
            Some((&layout, 0)),
            false,
            &clock,
            Some(status::BatteryStatus {
                percent: 42,
                state: status::BatteryPowerState::Charging,
                power_mw: None,
            }),
            Some((50, false)),
            Some(80),
            Some("YOASOBI – アイドル 🔥"),
            Some(0.35),
            true,
            Some(&active_win),
            None,
            Some(45),
            Some(12),
            Some(48),
            Some("RU"),
            (0.0, 0.0),
            1000,
        );
        assert!(canvas.pixels.iter().any(|&pixel| pixel != 0));
        assert!(!needs_fast_anim, "Media playback should not force 60fps refresh; min_next_wake handles it at 250ms");
        assert_eq!(next_wake, Some(1250));
    }

    #[test]
    fn test_idle_status_bar_media_capsule_cached() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let clock = status::ClockWidgetData::default();
        let long_title = "A".repeat(400);

        // Frame 1: Miss -> creates base cache including media capsule
        let (needs_fast1, _) = canvas.idle_status_bar(
            None,
            false,
            &clock,
            Some(status::BatteryStatus {
                percent: 80,
                state: status::BatteryPowerState::Discharging,
                power_mw: None,
            }),
            Some((50, false)),
            Some(50),
            Some(&long_title),
            Some(0.50),
            true,
            None,
            None,
            Some(30),
            Some(10),
            Some(45),
            Some("EN"),
            (0.0, 0.0),
            1000,
        );
        assert!(needs_fast1, "Long title marquee must trigger fast anim");
        assert!(canvas.idle_base_cache.is_some(), "Base cache must be created");
        assert!(canvas.idle_base_cache.as_ref().unwrap().key.has_media_capsule, "Media capsule flag must be set in key");

        // Verify capsule pixels exist in base cache pixmap
        let cached = canvas.idle_base_cache.as_ref().unwrap();
        let lit_cached = cached.pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(lit_cached > 100, "Base cache must have pre-rendered capsule pixels");

        // Frame 2: Hit -> uses base cache directly
        let (needs_fast2, _) = canvas.idle_status_bar(
            None,
            false,
            &clock,
            Some(status::BatteryStatus {
                percent: 80,
                state: status::BatteryPowerState::Discharging,
                power_mw: None,
            }),
            Some((50, false)),
            Some(50),
            Some(&long_title),
            Some(0.52),
            true,
            None,
            None,
            Some(30),
            Some(10),
            Some(45),
            Some("EN"),
            (0.0, 0.0),
            1033,
        );
        assert!(needs_fast2, "Cache hit must still trigger fast anim for marquee");
        let lit_frame2 = canvas.pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(lit_frame2 >= lit_cached, "Frame 2 must retain full capsule rendering");
    }

    #[test]
    fn idle_status_bar_scrolls_on_long_title() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let long_title = "A".repeat(400);
        let clock = status::ClockWidgetData::default();
        let (needs_scroll, _) = canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            Some((50, false)),
            Some(50),
            Some(&long_title),
            None,
            false,
            None,
            None,
            Some(50),
            Some(10),
            Some(52),
            Some("EN"),
            (0.0, 0.0),
            1500,
        );
        assert!(needs_scroll, "Long title must trigger scrolling");
    }

    #[test]
    fn idle_status_bar_media_progress_bar_renders() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let clock = status::ClockWidgetData::default();
        canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            None,
            None,
            Some("Awesome Song"),
            Some(0.50),
            true,
            None,
            None,
            None,
            None,
            None,
            None,
            (0.0, 0.0),
            1000,
        );
        // Verify pixels in full-height progress capsule (e.g. y = 10 and y = 50)
        let row_start = 10 * 2170;
        let row_pixels = &canvas.pixels[row_start..row_start + 2170];
        let lit_count = row_pixels.iter().filter(|&&p| (p & 0x00ffffff) > 0).count();
        assert!(lit_count > 100, "Media full-height progress capsule must render visible pixels on y=10");
    }

    #[test]
    fn idle_status_bar_active_window_renders_large_icon() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, Some(36.0), None).unwrap();
        let clock = status::ClockWidgetData::default();
        let active_win = niri::ActiveWindow {
            title: "Firefox – Web Browser".to_string(),
            app_id: Some("firefox".to_string()),
            pid: None,
        };
        canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(&active_win),
            None,
            None,
            None,
            None,
            None,
            (0.0, 0.0),
            1000,
        );
        let mut center_lit = 0;
        for y in 10..50 {
            let row_start = y * 2170;
            center_lit += canvas.pixels[row_start..row_start + 2170]
                .iter()
                .filter(|&&p| (p & 0x00ffffff) > 0)
                .count();
        }
        assert!(center_lit > 100, "Active window title and browser icon must render visible pixels");

        // Verify that the icon and text do NOT bleed off the top and bottom borders (y < 4 or y > 56)
        let top_edge_lit: usize = (0..4)
            .map(|y| {
                let row_start = y * 2170;
                canvas.pixels[row_start + 200..row_start + 1800]
                    .iter()
                    .filter(|&&p| (p & 0x00ffffff) > 0)
                    .count()
            })
            .sum();
        let bottom_edge_lit: usize = (56..60)
            .map(|y| {
                let row_start = y * 2170;
                canvas.pixels[row_start + 200..row_start + 1800]
                    .iter()
                    .filter(|&&p| (p & 0x00ffffff) > 0)
                    .count()
            })
            .sum();
        assert_eq!(top_edge_lit, 0, "Browser icon and text must not overflow top border");
        assert_eq!(bottom_edge_lit, 0, "Browser icon and text must not overflow bottom border");
    }

    #[test]
    fn idle_status_bar_workspace_label_renders() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, Some(36.0), None).unwrap();
        let clock = status::ClockWidgetData::default();
        let active_win = niri::ActiveWindow {
            title: "kitty – fish".to_string(),
            app_id: Some("kitty".to_string()),
            pid: None,
        };
        let ws = niri::WorkspaceInfo {
            current: 3,
            total: 7,
            active_id: Some(1),
        };
        canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(&active_win),
            Some(ws),
            None,
            None,
            None,
            None,
            (0.0, 0.0),
            1000,
        );
        let mut center_lit = 0;
        for y in 10..50 {
            let row_start = y * 2170;
            center_lit += canvas.pixels[row_start..row_start + 2170]
                .iter()
                .filter(|&&p| (p & 0x00ffffff) > 0)
                .count();
        }
        assert!(center_lit > 100, "Workspace label + active window must render visible pixels");
    }

    #[test]
    fn idle_status_bar_workspace_only_renders() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, Some(36.0), None).unwrap();
        let clock = status::ClockWidgetData::default();
        let ws = niri::WorkspaceInfo {
            current: 2,
            total: 5,
            active_id: Some(1),
        };
        canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            Some(ws),
            None,
            None,
            None,
            None,
            (0.0, 0.0),
            1000,
        );
        let mut center_lit = 0;
        for y in 10..50 {
            let row_start = y * 2170;
            center_lit += canvas.pixels[row_start..row_start + 2170]
                .iter()
                .filter(|&&p| (p & 0x00ffffff) > 0)
                .count();
        }
        assert!(center_lit > 50, "Workspace only label must render visible pixels");
    }

    #[test]
    fn idle_status_bar_workspace_prefix_stays_stationary_on_long_title() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let long_title = "A".repeat(400);
        let clock = status::ClockWidgetData::default();
        let active_win = niri::ActiveWindow {
            title: long_title.clone(),
            app_id: Some("firefox".to_string()),
            pid: None,
        };
        let ws = niri::WorkspaceInfo {
            current: 2,
            total: 4,
            active_id: Some(1),
        };
        let (needs_scroll, _) = canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(&active_win),
            Some(ws),
            None,
            None,
            None,
            None,
            (0.0, 0.0),
            1000,
        );
        assert!(needs_scroll, "Long title must trigger scrolling");
        // Verify that marquee cache contains ONLY the long title and NOT the prefix "[2/4]:"
        assert_eq!(
            canvas.marquee_cache.as_ref().unwrap().text,
            long_title,
            "Marquee cache must contain only window title without prefix"
        );
    }

    #[test]
    fn test_resolve_real_application_icons() {
        // Alacritty has a real SVG icon in /usr/share/pixmaps/Alacritty.svg
        if FsPath::new("/usr/share/pixmaps/Alacritty.svg").is_file() {
            let icon = resolve_window_svg(Some("Alacritty"), "Antigravity", 52, 38);
            assert!(icon.is_some(), "Alacritty real icon must resolve");
            let pixmap = icon.unwrap();
            assert!(pixmap.width() <= 52 && pixmap.height() <= 38);
        }

        // Chromium has real PNG icons in /usr/share/icons/hicolor/*/apps/chromium.png
        if FsPath::new("/usr/share/icons/hicolor/256x256/apps/chromium.png").is_file() {
            let icon = resolve_window_svg(Some("chromium"), "Random Web Page", 52, 38);
            assert!(icon.is_some(), "Chromium real icon must resolve from PNG");
            let pixmap = icon.unwrap();
            assert!(pixmap.width() <= 52 && pixmap.height() <= 38);
        }
    }

    #[test]
    fn test_resolve_window_svg_for_googledrive_and_2gis() {
        // 2GIS in Russian and English
        let svg_2gis_ru = resolve_window_svg(Some("chromium"), "2ГИС — поиск мест, карта, навигатор", 52, 38);
        assert!(svg_2gis_ru.is_some(), "2ГИС in Russian must resolve to 2Gis.svg");

        let svg_2gis_en = resolve_window_svg(Some("chrome-2gis.ru__-Default"), "2GIS", 52, 38);
        assert!(svg_2gis_en.is_some(), "2GIS in English must resolve to 2Gis.svg");

        // Google Drive in Russian and English
        let svg_drive_ru = resolve_window_svg(Some("chromium"), "Google Диск – Мой диск", 52, 38);
        assert!(svg_drive_ru.is_some(), "Google Диск in Russian must resolve to GoogleDrive.svg");

        let svg_drive_en = resolve_window_svg(Some("chromium"), "Google Drive - My Drive", 52, 38);
        assert!(svg_drive_en.is_some(), "Google Drive in English must resolve to GoogleDrive.svg");

        let svg_drive_app = resolve_window_svg(Some("chrome-drive.google.com__drive_my-drive-Default"), "Мой диск", 52, 38);
        assert!(svg_drive_app.is_some(), "Chromium app id with drive.google.com must resolve to GoogleDrive.svg");
    }

    #[test]
    fn capsule_fill_path_produces_valid_outlines() {
        // Flat right edge during playback
        let path1 = capsule_fill_path(100.0, 4.0, 300.0, 56.0, 10.0, 500.0);
        let bounds1 = path1.bounds();
        assert!((bounds1.left() - 100.0).abs() < 0.1);
        assert!((bounds1.right() - 300.0).abs() < 0.1);

        // Curving right edge at completion
        let path2 = capsule_fill_path(100.0, 4.0, 500.0, 56.0, 10.0, 500.0);
        let bounds2 = path2.bounds();
        assert!((bounds2.left() - 100.0).abs() < 0.1);
        assert!((bounds2.right() - 500.0).abs() < 0.1);

        // Narrow progress at start
        let path3 = capsule_fill_path(100.0, 4.0, 102.0, 56.0, 10.0, 500.0);
        let bounds3 = path3.bounds();
        assert!((bounds3.left() - 100.0).abs() < 0.1);
        assert!((bounds3.right() - 102.0).abs() < 0.1);
    }

    #[test]
    fn text_with_emoji_renders_and_measures() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let text = "Track 🎵 with emoji 🔥✨ and text";
        let width = canvas.text_width(text, 20.0);
        assert!(width > 100.0);
        let pen = canvas.draw_text_on_main(10.0, 30.0, text, 20.0, FONT_WEIGHT, canvas.color);
        assert_eq!((pen - 10.0).round() as i32, width.round() as i32);
        assert!(canvas.pixmap.pixels().iter().any(|p| p.alpha() > 0));
    }

    #[test]
    fn test_telegram_and_punctuation_render() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let tg_title = "\u{200e}\u{2068}Sana\u{2069} – (21362)";
        let cleaned = clean_display_text(tg_title);
        assert_eq!(cleaned, "Sana – (21362)");

        let raw_width = canvas.text_width(tg_title, 20.0);
        let cleaned_width = canvas.text_width(&cleaned, 20.0);
        assert!((raw_width - cleaned_width).abs() < 0.001);

        let pen = canvas.draw_text_on_main(100.0, 35.0, tg_title, 20.0, FONT_WEIGHT, canvas.color);
        assert!((pen - (100.0 + cleaned_width)).abs() < 0.01);

        // Ensure punctuation characters render properly
        for c in ['–', '—', '―', '‒', '‐', '‑', '«', '»', '“', '”', '‘', '’', '•', '…', '·'] {
            let (font, id) = canvas.glyph_font_and_id(c);
            assert_ne!(id.0, 0, "Punctuation char {c} must have valid glyph");
            assert!(font.outline(id).is_some(), "Punctuation char {c} must have outline");
        }
    }

    #[test]
    fn test_special_characters_math_and_telugu_bengali_render() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let test_title = "𝑅𝑒𝑝𝑙𝑎𝑦 & 𝑓𝑖𝑛𝑑 𝑜𝑢𝑡 ౨ৎ #BabyDONTCry #Replay #BDC_Replay #261104_6pmKST";
        for c in test_title.chars() {
            if c == ' ' {
                continue;
            }
            let (font, id) = canvas.glyph_font_and_id(c);
            assert_ne!(
                id.0, 0,
                "Character {c} (U+{:04X}) must have a valid glyph ID in fallback fonts",
                c as u32
            );
            assert!(
                font.outline(id).is_some(),
                "Character {c} must produce a valid glyph outline"
            );
        }
        let width = canvas.text_width(test_title, 20.0);
        assert!(width > 100.0, "Test title width must be measured properly");

        let pen = canvas.draw_text_on_main(10.0, 30.0, test_title, 20.0, FONT_WEIGHT, canvas.color);
        assert!(pen > 10.0);
        assert!(canvas.pixmap.pixels().iter().any(|p| p.alpha() > 0));
    }

    /// Renders both layers and the Touch ID prompt for design review:
    /// `T2_TOUCHBAR_ULTRA_PREVIEW=/tmp/tb cargo test preview -- --ignored`
    #[test]
    #[ignore]
    fn preview() {
        let dir = std::env::var("T2_TOUCHBAR_ULTRA_PREVIEW").unwrap_or_else(|_| ".".into());
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let save = |name: &str, canvas: &Canvas| {
            let mut ppm = format!("P6 {} {} 255\n", canvas.width, canvas.height).into_bytes();
            for pixel in &canvas.pixels {
                ppm.extend([(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
            }
            fs::write(format!("{dir}/{name}.ppm"), ppm).unwrap();
        };
        canvas.keys(&Layout::new(Mode::Media, 2170, false), None);
        save("media", &canvas);
        canvas.keys(&Layout::new(Mode::Function, 2170, false), None);
        save("function", &canvas);
        canvas.keys(
            &Layout::with_custom(Mode::Special, 2170, false, &["~", "|", "term"]),
            None,
        );
        save("special", &canvas);
        let all_12 = [
            "fullscreen", "emoji", "screenshot", "Gemini", "Chatgpt", "Claude", "Grok",
            "Qwen", "Youtube", "GoogleDrive", "Lampa", "2Gis",
        ];
        canvas.keys(
            &Layout::with_custom(Mode::Special, 2170, true, &all_12),
            None,
        );
        save("special_12", &canvas);
        canvas.touch_id("waiting", 0.0, None, false);
        save("touchid", &canvas);
        canvas.touch_id("waiting", 1.0, None, false);
        save("touchid-nudged", &canvas);
        let media = Layout::new(Mode::Media, 2170, false);
        let keys = TrackLayout {
            prev: (media.buttons[1].left, media.buttons[1].right),
            next: (2170 - media.buttons[1].right, 2170 - media.buttons[1].left),
        };
        canvas.track("Slipknot", "Duality", 100, None, keys, None);
        save("track", &canvas);
        canvas.track(
            "Slipknot",
            "Duality",
            100,
            None,
            keys,
            Some((media.buttons[0].clone(), 100)),
        );
        save("track-esc", &canvas);
        canvas.track(
            "Heino",
            "Blau blüht der Enzian (Live aus der Festhalle mit Orchester und sehr langem Titel, der gekürzt werden muss, weil er sonst nicht passt)",
            60,
            Some(true),
            keys,
            None,
        );
        save("track-long", &canvas);
        canvas.touch_id("retry", 0.0, None, false);
        save("touchid-retry", &canvas);
        canvas.level(LevelKind::Volume, Some(45), false, (560.0, 640.0), 1.0, 100);
        save("volume", &canvas);
        canvas.level(
            LevelKind::Brightness,
            Some(80),
            false,
            (1620.0, 1780.0),
            0.0,
            100,
        );
        save("brightness", &canvas);
        canvas.level(
            LevelKind::Volume,
            Some(45),
            false,
            (1000.0, 1100.0),
            0.3,
            100,
        );
        save("volume-crossfade", &canvas);
    }

    #[test]
    fn test_svg_render() {
        let svg_data = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="800" height="800"><circle cx="50" cy="50" r="40" fill="red"/></svg>"#;
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_str(svg_data, &opt).unwrap();
        let size = tree.size();
        let scale = 24.0 / size.width();
        let mut pixmap = tiny_skia::Pixmap::new(24, 24).unwrap();
        let transform = tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        assert_eq!(pixmap.width(), 24);
        assert_eq!(pixmap.height(), 24);
    }

    #[test]
    fn test_level_renders_percentage_text() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        canvas.level(LevelKind::Volume, Some(45), false, (560.0, 640.0), 1.0, 100);
        assert!(canvas.pixels.iter().any(|&p| p != 0));
        let row_start = 30 * 2170;
        let text_pixels = &canvas.pixels[row_start + 1160..row_start + 1240];
        let lit_count = text_pixels.iter().filter(|&&p| (p & 0x00ffffff) > 0).count();
        assert!(lit_count > 0, "Percentage text must render visible pixels to the right of the level meter");
    }

    #[test]
    fn test_truncate_to_width_cyrillic_and_ascii() {
        let canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let long_cyrillic = "Очень длинный заголовок окна браузера с кучей вкладок";
        let truncated = canvas.truncate_to_width(long_cyrillic, 100.0, 20.0);
        assert!(truncated.ends_with('…'));
        assert!(canvas.text_width(&truncated, 20.0) <= 100.0);

        let ascii = "Extremely long window title";
        let truncated_ascii = canvas.truncate_to_width(ascii, 80.0, 20.0);
        assert!(truncated_ascii.ends_with('…'));
        assert!(canvas.text_width(&truncated_ascii, 20.0) <= 80.0);

        let tiny = canvas.truncate_to_width(ascii, 5.0, 20.0);
        assert_eq!(tiny, "");
    }

    #[test]
    fn test_suggestions_layout_with_preview() {
        let candidates = vec!["геолокация".to_string(), "функция".to_string(), "программа".to_string()];
        let layout = Layout::suggestions(2170, &candidates, "гео", true, Some(("Привет, текущая ", 16, 7)), &[]);
        assert!(!layout.buttons.is_empty());

        // Button 0 is the physical escape or dismiss button
        assert_eq!(layout.buttons[0].action, Action::Key(Key::Esc));

        // Button 1 is the preview capsule
        assert_eq!(layout.buttons[1].action, Action::Preview);
        if let Glyph::Preview { text, cursor_pos, prefix_len } = &layout.buttons[1].glyph {
            assert_eq!(text, "Привет, текущая ");
            assert_eq!(*cursor_pos, 16);
            assert_eq!(*prefix_len, 7);
        } else {
            panic!("Expected Glyph::Preview");
        }

        // Remaining buttons are suggestions
        let sug_buttons: Vec<_> = layout.buttons.iter().filter_map(|b| {
            if let Action::Suggestion(idx) = b.action {
                Some((idx, &b.glyph))
            } else {
                None
            }
        }).collect();
        assert_eq!(sug_buttons.len(), 3);
        assert_eq!(sug_buttons[0].0, 0);
        assert_eq!(sug_buttons[1].0, 1);
        assert_eq!(sug_buttons[2].0, 2);
        assert_eq!(layout.buttons.last().unwrap().action, Action::ToggleEmojiStrip);
        assert!(matches!(layout.buttons.last().unwrap().glyph, Glyph::Icon(Icon::Emoji)));
        assert!(layout.buttons.iter().any(|b| b.action == Action::OpenColorPicker));
    }

    #[test]
    fn test_suggestions_layout_with_clipboard_when_empty() {
        let clip_items = vec![
            "https://docs.noctalia.dev".to_string(),
            "cargo build --release".to_string(),
            "first line\nsecond line".to_string(),
        ];
        let layout = Layout::suggestions(2170, &[], "", true, None, &clip_items);
        assert!(!layout.buttons.is_empty());

        let clip_buttons: Vec<_> = layout
            .buttons
            .iter()
            .filter_map(|b| match b.action {
                Action::Clipboard(idx) => Some(idx),
                _ => None,
            })
            .collect();
        assert_eq!(clip_buttons, vec![0, 1, 2]);

        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        canvas.keys(&layout, None);
        assert!(canvas.pixels.iter().any(|&p| p != 0));
    }

    #[test]
    fn test_word_definition_rendering() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let short_def = crate::lookup::WordDefinition::from_text("практичный — Деловитый, экономный");
        let is_fast = canvas.word_definition(&short_def, 100, 1000, 1000, None);
        assert!(!is_fast, "Short definition fitting touchbar should not require fast marquee anim");
        assert!(canvas.pixels.iter().any(|&p| p != 0));

        let long_def = crate::lookup::WordDefinition::from_text("serendipity [en] — случайность — the occurrence and development of events by chance in a happy or beneficial way. Extremely long definition text that spans across multiple sentences and definitely exceeds the full width of the touchbar display.");
        let is_fast_long = canvas.word_definition(&long_def, 100, 1000, 1000, None);
        assert!(is_fast_long, "Long definition overflowing touchbar must trigger marquee");
    }

    #[test]
    fn test_compute_suggestion_highlights() {
        // Shared prefix and common root
        let cands = vec![
            "функция".to_string(),
            "функционал".to_string(),
            "функционировать".to_string(),
        ];
        let hl = compute_suggestion_highlights(&cands, "функц");
        assert_eq!(hl.len(), 3);
        // Common root 'функци' (6 chars)
        assert_eq!(hl[0], (6, false));
        assert_eq!(hl[1], (6, false));
        assert_eq!(hl[2], (6, false));

        // Typo fix
        let typo_cands = vec!["сегодня".to_string()];
        let typo_hl = compute_suggestion_highlights(&typo_cands, "севодня");
        assert_eq!(typo_hl.len(), 1);
        assert_eq!(typo_hl[0], (2, true)); // 'се' matches, is_typo_fix = true

        // Empty prefix (next word)
        let empty_cands = vec!["как".to_string(), "что".to_string()];
        let empty_hl = compute_suggestion_highlights(&empty_cands, "");
        assert_eq!(empty_hl[0], (0, false));
        assert_eq!(empty_hl[1], (0, false));
    }

    #[test]
    fn test_hsv_to_rgb() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv_to_rgb(1.0 / 3.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsv_to_rgb(2.0 / 3.0, 1.0, 1.0), (0, 0, 255));
    }

    #[test]
    fn test_color_picker_layout() {
        let layout = Layout::color_picker(2170, 0.5, false, false);
        assert!(!layout.buttons.is_empty());
        assert_eq!(layout.buttons[0].action, Action::Key(Key::Esc));
        assert!(layout.buttons.iter().any(|b| b.action == Action::ColorPicker));
        assert!(layout.buttons.iter().any(|b| b.action == Action::ColorCopy));
    }

    #[test]
    fn test_windows_layout() {
        let windows = vec![
            niri::NiriWindowItem {
                id: 1,
                title: Some("Terminal".to_string()),
                app_id: Some("Alacritty".to_string()),
                pid: None,
                workspace_id: Some(1),
                is_focused: true,
                layout: None,
            },
            niri::NiriWindowItem {
                id: 2,
                title: Some("Browser".to_string()),
                app_id: Some("firefox".to_string()),
                pid: None,
                workspace_id: Some(2),
                is_focused: false,
                layout: None,
            },
        ];
        let layout_cur = Layout::windows(2170, &windows, 1, false, false);
        assert!(!layout_cur.buttons.is_empty());
        assert!(layout_cur.buttons.iter().any(|b| b.action == Action::FocusWindow(1)));
        // Workspace 2 window should not appear when show_all is false and current_workspace is 1
        assert!(!layout_cur.buttons.iter().any(|b| b.action == Action::FocusWindow(2)));

        let layout_all = Layout::windows(2170, &windows, 1, true, false);
        assert!(layout_all.buttons.iter().any(|b| b.action == Action::FocusWindow(1)));
        assert!(layout_all.buttons.iter().any(|b| b.action == Action::FocusWindow(2)));
    }

    #[test]
    fn test_emoji_and_punctuation_layout() {
        let layout = Layout::emoji_and_punctuation(2170, false, 0.0);
        assert!(!layout.buttons.is_empty());
        assert_eq!(layout.buttons[0].action, Action::Key(Key::Esc));
        assert_eq!(layout.buttons[1].action, Action::ToggleEmojiStrip);
        let b0 = layout.buttons.iter().find(|b| b.action == Action::Emoji(0)).unwrap();
        let b1 = layout.buttons.iter().find(|b| b.action == Action::Emoji(1)).unwrap();
        assert_eq!(b0.right - b0.left, EMOJI_BTN_WIDTH as u16);
        assert_eq!(b1.left - b0.right, EMOJI_BTN_GAP as u16);

        let layout_scrolled = Layout::emoji_and_punctuation(2170, false, 300.0);
        assert!(!layout_scrolled.buttons.is_empty());
        assert!(layout_scrolled.buttons.iter().any(|b| matches!(b.action, Action::Emoji(_))));
    }

    #[test]
    fn test_color_emoji_rendering() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        canvas.draw_emoji(100.0, 30.0, "🔥", 32.0);
        let colored_pixels = canvas
            .pixmap
            .pixels()
            .iter()
            .filter(|p| p.alpha() > 0 && (p.red() > 0 || p.green() > 0 || p.blue() > 0))
            .count();
        assert!(colored_pixels > 20, "Color emoji 🔥 must render colored pixels");
    }

    #[test]
    fn test_suggestion_background_and_border_colors() {
        let mut canvas = Canvas::new(500, 60, 0x00dce6ff, None, None, None, None).unwrap();
        // Recommended suggestion (index 0)
        let rec_btn = Button {
            left: 10,
            right: 150,
            glyph: Glyph::Suggestion {
                text: "рекомендация".to_string(),
                match_len: 4,
                is_typo_fix: false,
            },
            action: Action::Suggestion(0),
        };
        canvas.button(&rec_btn, false);
        // Typo fix suggestion
        let typo_btn = Button {
            left: 160,
            right: 300,
            glyph: Glyph::Suggestion {
                text: "исправление".to_string(),
                match_len: 3,
                is_typo_fix: true,
            },
            action: Action::Suggestion(1),
        };
        canvas.button(&typo_btn, false);
        // Emoji button (should be borderless)
        let emoji_btn = Button {
            left: 310,
            right: 366,
            glyph: Glyph::Emoji("🔥"),
            action: Action::Emoji(0),
        };
        canvas.button(&emoji_btn, false);

        // Verify that rec_btn and typo_btn have colored pixels from their tinted backgrounds
        let rec_pixels = canvas.pixmap.pixels().iter().filter(|p| {
            p.alpha() > 0 && p.blue() > 0
        }).count();
        assert!(rec_pixels > 0, "Recommended button should have blue tinted fill");

        let typo_pixels = canvas.pixmap.pixels().iter().filter(|p| {
            p.alpha() > 0 && p.red() > 0 && p.blue() == 0
        }).count();
        assert!(typo_pixels > 0, "Typo button should have amber tinted fill");
    }

    #[test]
    fn test_status_bar_workspace_and_window_buttons() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff, None, None, None, None).unwrap();
        let clock = status::ClockWidgetData::default();
        let active_win = niri::ActiveWindow {
            title: "kitty – fish".to_string(),
            app_id: Some("kitty".to_string()),
            pid: None,
        };
        let ws = niri::WorkspaceInfo {
            current: 1,
            total: 3,
            active_id: Some(10),
        };
        canvas.workspace_windows = vec![
            niri::NiriWindowItem {
                id: 101,
                title: Some("kitty – fish".to_string()),
                app_id: Some("kitty".to_string()),
                pid: None,
                workspace_id: Some(10),
                is_focused: true,
                layout: None,
            },
            niri::NiriWindowItem {
                id: 102,
                title: Some("firefox".to_string()),
                app_id: Some("firefox".to_string()),
                pid: None,
                workspace_id: Some(10),
                is_focused: false,
                layout: None,
            },
            niri::NiriWindowItem {
                id: 201,
                title: Some("gimp".to_string()),
                app_id: Some("gimp".to_string()),
                pid: None,
                workspace_id: Some(20),
                is_focused: false,
                layout: None,
            },
        ];

        canvas.idle_status_bar(
            None,
            false,
            &clock,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(&active_win),
            Some(ws),
            None,
            None,
            None,
            None,
            (0.0, 0.0),
            1000,
        );

        assert_eq!(canvas.status_workspace_buttons.len(), 3);
        assert_eq!(canvas.status_workspace_buttons[0].0, 1);
        assert_eq!(canvas.status_workspace_buttons[1].0, 2);
        assert_eq!(canvas.status_workspace_buttons[2].0, 3);

        assert_eq!(canvas.status_window_buttons.len(), 2);
        assert_eq!(canvas.status_window_buttons[0].0, 101);
        assert_eq!(canvas.status_window_buttons[1].0, 102);
    }
}
