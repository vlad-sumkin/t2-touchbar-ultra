// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveWindow {
    pub title: String,
    pub app_id: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct NiriFocusedWindow {
    title: Option<String>,
    app_id: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NiriKeyboardLayouts {
    pub names: Vec<String>,
    pub current_idx: usize,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct NiriWindowLayout {
    #[serde(default)]
    pub pos_in_scrolling_layout: Option<(usize, usize)>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NiriWindowItem {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub workspace_id: Option<u64>,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub layout: Option<NiriWindowLayout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WorkspaceInfo {
    pub current: u32,
    pub total: u32,
    pub active_id: Option<u64>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NiriWorkspaceItem {
    pub id: u64,
    pub idx: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub is_urgent: bool,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub active_window_id: Option<u64>,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
pub enum NiriStreamEvent {
    WorkspacesChanged {
        workspaces: Vec<NiriWorkspaceItem>,
    },
    WorkspaceActivated {
        id: u64,
        focused: bool,
    },
    WorkspaceActiveWindowChanged {
        workspace_id: u64,
        active_window_id: Option<u64>,
    },
    WindowsChanged {
        windows: Vec<NiriWindowItem>,
    },
    WindowOpenedOrChanged {
        window: NiriWindowItem,
    },
    WindowClosed {
        id: u64,
    },
    WindowFocusChanged {
        id: Option<u64>,
    },
    KeyboardLayoutsChanged {
        keyboard_layouts: NiriKeyboardLayouts,
    },
    KeyboardLayoutSwitched {
        idx: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NiriEvent {
    Window(Option<ActiveWindow>),
    Layout(Option<String>),
    Workspace(Option<WorkspaceInfo>),
}

pub struct NiriWatcher {
    running: Arc<AtomicBool>,
    results: Receiver<NiriEvent>,
}

impl Drop for NiriWatcher {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

pub fn build_active_window(title: Option<&str>, app_id: Option<&str>, pid: Option<u32>) -> Option<ActiveWindow> {
    let clean = title.map(crate::renderer::clean_display_text);
    let title = clean.as_deref().unwrap_or_default().trim();
    let app_id = app_id.map(|s| s.trim()).filter(|s| !s.is_empty());
    if title.is_empty() && app_id.is_none() {
        return None;
    }
    let display_title = if !title.is_empty() {
        title.to_string()
    } else {
        app_id.unwrap_or_default().to_string()
    };
    Some(ActiveWindow {
        title: display_title,
        app_id: app_id.map(ToString::to_string),
        pid,
    })
}

pub fn query_focused_window() -> Option<ActiveWindow> {
    let output = Command::new("niri")
        .args(["msg", "-j", "focused-window"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let win: NiriFocusedWindow = serde_json::from_slice(&output.stdout).ok()?;
    build_active_window(win.title.as_deref(), win.app_id.as_deref(), win.pid)
}

pub fn compute_workspace_info(workspaces: &[NiriWorkspaceItem]) -> Option<WorkspaceInfo> {
    if workspaces.is_empty() {
        return None;
    }
    let focused = workspaces
        .iter()
        .find(|w| w.is_focused)
        .or_else(|| workspaces.iter().find(|w| w.is_active))
        .or_else(|| workspaces.first())?;

    let output = focused.output.as_deref();
    let on_output: Vec<&NiriWorkspaceItem> = if let Some(out) = output {
        workspaces.iter().filter(|w| w.output.as_deref() == Some(out)).collect()
    } else {
        workspaces.iter().collect()
    };

    let current = focused.idx;
    let active_id = Some(focused.id);
    // Exclude trailing empty workspaces: only count workspaces that are occupied (have a window)
    // or are currently focused (where the user is right now).
    let max_occupied_or_focused_idx = on_output
        .iter()
        .filter(|w| w.active_window_id.is_some() || w.is_focused || w.idx == current)
        .map(|w| w.idx)
        .max()
        .unwrap_or(current);

    let total = max_occupied_or_focused_idx.max(current).max(1);

    Some(WorkspaceInfo { current, total, active_id })
}

pub fn query_workspaces() -> (Option<WorkspaceInfo>, Vec<NiriWorkspaceItem>) {
    let output = Command::new("niri")
        .args(["msg", "-j", "workspaces"])
        .output()
        .ok();
    if let Some(out) = output
        && out.status.success()
        && let Ok(items) = serde_json::from_slice::<Vec<NiriWorkspaceItem>>(&out.stdout)
    {
        let info = compute_workspace_info(&items);
        return (info, items);
    }
    (None, Vec::new())
}

pub fn parse_keyboard_layout(data: &[u8]) -> Option<String> {
    let layouts: NiriKeyboardLayouts = serde_json::from_slice(data).ok()?;
    if layouts.current_idx < layouts.names.len() {
        Some(format_layout_code(&layouts.names[layouts.current_idx]))
    } else {
        None
    }
}

pub fn format_layout_code(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    if lower.contains("russian") || lower.contains("русск") || lower.starts_with("ru") {
        "RU".to_string()
    } else if lower.contains("english") || lower.contains("(us)") || lower.starts_with("en") || lower == "us" {
        "EN".to_string()
    } else if lower.contains("german") || lower.contains("deutsch") || lower.starts_with("de") {
        "DE".to_string()
    } else if lower.contains("french") || lower.contains("français") || lower.starts_with("fr") {
        "FR".to_string()
    } else if lower.contains("spanish") || lower.contains("español") || lower.starts_with("es") {
        "ES".to_string()
    } else if lower.contains("ukrainian") || lower.contains("україн") || lower.starts_with("uk") || lower.starts_with("ua") {
        "UA".to_string()
    } else if lower.contains("belarusian") || lower.contains("беларус") || lower.starts_with("by") {
        "BY".to_string()
    } else if lower.contains("kazakh") || lower.contains("қазақ") || lower.starts_with("kk") || lower.starts_with("kz") {
        "KZ".to_string()
    } else {
        let chars: String = name.chars().filter(|c| c.is_alphabetic()).take(2).collect();
        if !chars.is_empty() {
            chars.to_uppercase()
        } else {
            "??".to_string()
        }
    }
}

pub fn query_keyboard_layout() -> Option<String> {
    let output = Command::new("niri")
        .args(["msg", "-j", "keyboard-layouts"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_keyboard_layout(&output.stdout)
}

pub fn parse_windows(data: &[u8]) -> Vec<NiriWindowItem> {
    let mut windows: Vec<NiriWindowItem> = serde_json::from_slice(data).unwrap_or_default();
    windows.sort_by_key(|w| {
        (
            w.layout.as_ref().and_then(|l| l.pos_in_scrolling_layout).map(|p| p.0).unwrap_or(usize::MAX),
            w.layout.as_ref().and_then(|l| l.pos_in_scrolling_layout).map(|p| p.1).unwrap_or(usize::MAX),
            w.id,
        )
    });
    windows
}

pub fn query_windows() -> Vec<NiriWindowItem> {
    let output = Command::new("niri")
        .args(["msg", "-j", "windows"])
        .output()
        .ok();
    if let Some(out) = output
        && out.status.success()
    {
        return parse_windows(&out.stdout);
    }
    Vec::new()
}

pub fn focus_window(id: u64) {
    let _ = Command::new("niri")
        .args(["msg", "action", "focus-window", "--id", &id.to_string()])
        .spawn();
}

#[allow(dead_code)]
pub fn close_window(id: u64) {
    let _ = Command::new("niri")
        .args(["msg", "action", "close-window", "--id", &id.to_string()])
        .spawn();
}

pub fn focus_workspace(idx: u32) {
    let _ = Command::new("niri")
        .args(["msg", "action", "focus-workspace", &idx.to_string()])
        .spawn();
}

pub fn parse_stream_event(line: &str) -> Option<NiriStreamEvent> {
    serde_json::from_str(line).ok()
}

fn sync_workspace_info(
    last_workspace: &mut Option<WorkspaceInfo>,
    workspaces: &HashMap<u64, NiriWorkspaceItem>,
    sender: &mpsc::Sender<NiriEvent>,
    wake_writer: &mut UnixStream,
) -> bool {
    let items: Vec<NiriWorkspaceItem> = workspaces.values().cloned().collect();
    let current = compute_workspace_info(&items);
    if current != *last_workspace {
        *last_workspace = current;
        if sender.send(NiriEvent::Workspace(current)).is_err() {
            return false;
        }
        let _ = wake_writer.write_all(&[1]);
    }
    true
}

fn refresh_workspaces_from_niri(workspaces: &mut HashMap<u64, NiriWorkspaceItem>) {
    let (_, items) = query_workspaces();
    if !items.is_empty() {
        workspaces.clear();
        for w in items {
            workspaces.insert(w.id, w);
        }
    }
}

impl NiriWatcher {
    pub fn spawn() -> Result<(Self, UnixStream)> {
        let (sender, results) = mpsc::channel();
        let (mut wake_writer, wake_reader) = UnixStream::pair()?;
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        thread::Builder::new()
            .name("niri-watcher".to_owned())
            .spawn(move || {
                let (initial_ws, initial_items) = query_workspaces();
                let mut last_window = query_focused_window();
                let mut last_layout = query_keyboard_layout();
                let mut last_workspace = initial_ws;
                let _ = sender.send(NiriEvent::Window(last_window.clone()));
                let _ = sender.send(NiriEvent::Layout(last_layout.clone()));
                let _ = sender.send(NiriEvent::Workspace(last_workspace));
                let _ = wake_writer.write_all(&[1]);

                let mut windows: HashMap<u64, (Option<String>, Option<String>, Option<u32>)> = HashMap::new();
                let mut focused_id: Option<u64> = None;
                let mut layout_names: Vec<String> = Vec::new();
                let mut workspaces: HashMap<u64, NiriWorkspaceItem> = HashMap::new();
                for w in initial_items {
                    workspaces.insert(w.id, w);
                }

                while running_clone.load(Ordering::Relaxed) {
                    let mut child = match Command::new("niri")
                        .args(["msg", "-j", "event-stream"])
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(_) => {
                            thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                    };

                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if !running_clone.load(Ordering::Relaxed) {
                                break;
                            }
                            let Some(event) = parse_stream_event(&line) else {
                                continue;
                            };
                            match event {
                                NiriStreamEvent::WorkspacesChanged { workspaces: new_workspaces } => {
                                    workspaces.clear();
                                    for w in new_workspaces {
                                        workspaces.insert(w.id, w);
                                    }
                                    if !sync_workspace_info(&mut last_workspace, &workspaces, &sender, &mut wake_writer) {
                                        break;
                                    }
                                }
                                NiriStreamEvent::WorkspaceActivated { id, focused } => {
                                    if focused {
                                        refresh_workspaces_from_niri(&mut workspaces);
                                        let target_output = workspaces.get(&id).and_then(|w| w.output.clone());
                                        for w in workspaces.values_mut() {
                                            if w.output == target_output {
                                                w.is_focused = w.id == id;
                                                w.is_active = w.id == id;
                                            }
                                        }
                                        if !sync_workspace_info(&mut last_workspace, &workspaces, &sender, &mut wake_writer) {
                                            break;
                                        }
                                    }
                                }
                                NiriStreamEvent::WorkspaceActiveWindowChanged { workspace_id, active_window_id } => {
                                    if let Some(w) = workspaces.get_mut(&workspace_id) {
                                        w.active_window_id = active_window_id;
                                    }
                                    if !sync_workspace_info(&mut last_workspace, &workspaces, &sender, &mut wake_writer) {
                                        break;
                                    }
                                }
                                NiriStreamEvent::WindowsChanged { windows: new_windows } => {
                                    windows.clear();
                                    focused_id = None;
                                    for win in new_windows {
                                        if win.is_focused {
                                            focused_id = Some(win.id);
                                        }
                                        windows.insert(win.id, (win.title, win.app_id, win.pid));
                                    }
                                    let current = focused_id
                                        .and_then(|id| windows.get(&id))
                                        .and_then(|(t, a, p)| build_active_window(t.as_deref(), a.as_deref(), *p));
                                    if current != last_window {
                                        last_window = current.clone();
                                        if sender.send(NiriEvent::Window(current)).is_err() {
                                            break;
                                        }
                                        let _ = wake_writer.write_all(&[1]);
                                    }
                                    refresh_workspaces_from_niri(&mut workspaces);
                                    if !sync_workspace_info(&mut last_workspace, &workspaces, &sender, &mut wake_writer) {
                                        break;
                                    }
                                }
                                NiriStreamEvent::WindowOpenedOrChanged { window } => {
                                    if window.is_focused {
                                        focused_id = Some(window.id);
                                    }
                                    windows.insert(window.id, (window.title, window.app_id, window.pid));
                                    let current = focused_id
                                        .and_then(|id| windows.get(&id))
                                        .and_then(|(t, a, p)| build_active_window(t.as_deref(), a.as_deref(), *p));
                                    if current != last_window {
                                        last_window = current.clone();
                                        if sender.send(NiriEvent::Window(current)).is_err() {
                                            break;
                                        }
                                        let _ = wake_writer.write_all(&[1]);
                                    }
                                    refresh_workspaces_from_niri(&mut workspaces);
                                    if !sync_workspace_info(&mut last_workspace, &workspaces, &sender, &mut wake_writer) {
                                        break;
                                    }
                                }
                                NiriStreamEvent::WindowClosed { id } => {
                                    windows.remove(&id);
                                    for w in workspaces.values_mut() {
                                        if w.active_window_id == Some(id) {
                                            w.active_window_id = None;
                                        }
                                    }
                                    if focused_id == Some(id) {
                                        focused_id = None;
                                        let current = None;
                                        if current != last_window {
                                            last_window = current.clone();
                                            if sender.send(NiriEvent::Window(current)).is_err() {
                                                break;
                                            }
                                            let _ = wake_writer.write_all(&[1]);
                                        }
                                    }
                                    refresh_workspaces_from_niri(&mut workspaces);
                                    if !sync_workspace_info(&mut last_workspace, &workspaces, &sender, &mut wake_writer) {
                                        break;
                                    }
                                }
                                NiriStreamEvent::WindowFocusChanged { id } => {
                                    focused_id = id;
                                    let current = focused_id
                                        .and_then(|id| windows.get(&id))
                                        .and_then(|(t, a, p)| build_active_window(t.as_deref(), a.as_deref(), *p));
                                    if current != last_window {
                                        last_window = current.clone();
                                        if sender.send(NiriEvent::Window(current)).is_err() {
                                            break;
                                        }
                                        let _ = wake_writer.write_all(&[1]);
                                    }
                                }
                                NiriStreamEvent::KeyboardLayoutsChanged { keyboard_layouts } => {
                                    layout_names = keyboard_layouts.names;
                                    let current_idx = keyboard_layouts.current_idx;
                                    let current = layout_names
                                        .get(current_idx)
                                        .map(|n| format_layout_code(n));
                                    if current != last_layout {
                                        last_layout = current.clone();
                                        if sender.send(NiriEvent::Layout(current)).is_err() {
                                            break;
                                        }
                                        let _ = wake_writer.write_all(&[1]);
                                    }
                                }
                                NiriStreamEvent::KeyboardLayoutSwitched { idx } => {
                                    let current = layout_names
                                        .get(idx)
                                        .map(|n| format_layout_code(n));
                                    if current != last_layout {
                                        last_layout = current.clone();
                                        if sender.send(NiriEvent::Layout(current)).is_err() {
                                            break;
                                        }
                                        let _ = wake_writer.write_all(&[1]);
                                    }
                                }
                            }
                        }
                    }

                    let _ = child.kill();
                    let status = child.wait();
                    if !running_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    if status.as_ref().is_ok_and(|s| !s.success()) {
                        thread::sleep(Duration::from_secs(3));
                    } else {
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            })?;

        Ok((Self { running, results }, wake_reader))
    }

    pub fn try_recv(&self) -> impl Iterator<Item = NiriEvent> + '_ {
        self.results.try_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_layout_json() {
        let sample = br#"{"names":["English (US)","Russian (Macintosh)"],"current_idx":1}"#;
        assert_eq!(parse_keyboard_layout(sample), Some("RU".to_string()));

        let sample_en = br#"{"names":["English (US)","Russian (Macintosh)"],"current_idx":0}"#;
        assert_eq!(parse_keyboard_layout(sample_en), Some("EN".to_string()));
    }

    #[test]
    fn format_layout_names() {
        assert_eq!(format_layout_code("English (US)"), "EN");
        assert_eq!(format_layout_code("Russian (Macintosh)"), "RU");
        assert_eq!(format_layout_code("Russian"), "RU");
        assert_eq!(format_layout_code("German"), "DE");
        assert_eq!(format_layout_code("us"), "EN");
        assert_eq!(format_layout_code("ru"), "RU");
    }

    #[test]
    fn parse_stream_events() {
        let win_json = r#"{"WindowsChanged":{"windows":[{"id":1,"title":"Firefox","app_id":"firefox","is_focused":true},{"id":2,"title":"Alacritty","app_id":"alacritty","is_focused":false}]}}"#;
        let event = parse_stream_event(win_json).unwrap();
        match event {
            NiriStreamEvent::WindowsChanged { windows } => {
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].id, 1);
                assert!(windows[0].is_focused);
            }
            _ => panic!("wrong event"),
        }

        let focus_json = r#"{"WindowFocusChanged":{"id":2}}"#;
        let event = parse_stream_event(focus_json).unwrap();
        assert_eq!(event, NiriStreamEvent::WindowFocusChanged { id: Some(2) });

        let focus_null = r#"{"WindowFocusChanged":{"id":null}}"#;
        let event = parse_stream_event(focus_null).unwrap();
        assert_eq!(event, NiriStreamEvent::WindowFocusChanged { id: None });

        let kbd_json = r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)","Russian"],"current_idx":0}}}"#;
        let event = parse_stream_event(kbd_json).unwrap();
        match event {
            NiriStreamEvent::KeyboardLayoutsChanged { keyboard_layouts } => {
                assert_eq!(keyboard_layouts.current_idx, 0);
                assert_eq!(keyboard_layouts.names, vec!["English (US)", "Russian"]);
            }
            _ => panic!("wrong event"),
        }

        let switch_json = r#"{"KeyboardLayoutSwitched":{"idx":1}}"#;
        let event = parse_stream_event(switch_json).unwrap();
        assert_eq!(event, NiriStreamEvent::KeyboardLayoutSwitched { idx: 1 });

        let ws_json = r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"output":"eDP-1","is_focused":true,"is_active":true}]}}"#;
        let event = parse_stream_event(ws_json).unwrap();
        match event {
            NiriStreamEvent::WorkspacesChanged { workspaces } => {
                assert_eq!(workspaces.len(), 1);
                assert_eq!(workspaces[0].idx, 1);
            }
            _ => panic!("wrong event"),
        }

        let ws_act_json = r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#;
        let event = parse_stream_event(ws_act_json).unwrap();
        assert_eq!(event, NiriStreamEvent::WorkspaceActivated { id: 2, focused: true });

        let ignored_config = r#"{"ConfigLoaded":{"failed":false}}"#;
        assert_eq!(parse_stream_event(ignored_config), None);
    }

    #[test]
    fn test_compute_workspace_info_trailing_empty_ignored() {
        // Single occupied workspace with trailing empty workspace: shows 1/1
        let items_1 = vec![
            NiriWorkspaceItem {
                id: 1,
                idx: 1,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: Some(10),
            },
            NiriWorkspaceItem {
                id: 2,
                idx: 2,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: None,
            },
        ];
        let info_1 = compute_workspace_info(&items_1).unwrap();
        assert_eq!(info_1.current, 1);
        assert_eq!(info_1.total, 1);

        // When switching to the empty trailing workspace: shows 2/2
        let items_switched = vec![
            NiriWorkspaceItem {
                id: 1,
                idx: 1,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: Some(10),
            },
            NiriWorkspaceItem {
                id: 2,
                idx: 2,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: None,
            },
        ];
        let info_switched = compute_workspace_info(&items_switched).unwrap();
        assert_eq!(info_switched.current, 2);
        assert_eq!(info_switched.total, 2);

        // Two occupied workspaces with a trailing empty workspace: shows 1/2 or 2/2
        let items_2 = vec![
            NiriWorkspaceItem {
                id: 1,
                idx: 1,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: Some(10),
            },
            NiriWorkspaceItem {
                id: 2,
                idx: 2,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: Some(20),
            },
            NiriWorkspaceItem {
                id: 3,
                idx: 3,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: None,
            },
        ];
        let info_2 = compute_workspace_info(&items_2).unwrap();
        assert_eq!(info_2.current, 1);
        assert_eq!(info_2.total, 2);

        // Middle empty workspace preserves total
        let items_middle_empty = vec![
            NiriWorkspaceItem {
                id: 1,
                idx: 1,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: Some(10),
            },
            NiriWorkspaceItem {
                id: 2,
                idx: 2,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: None,
            },
            NiriWorkspaceItem {
                id: 3,
                idx: 3,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: Some(20),
            },
            NiriWorkspaceItem {
                id: 4,
                idx: 4,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: false,
                is_focused: false,
                active_window_id: None,
            },
        ];
        let info_mid = compute_workspace_info(&items_middle_empty).unwrap();
        assert_eq!(info_mid.current, 1);
        assert_eq!(info_mid.total, 3);
    }

    #[test]
    fn test_compute_workspace_info_visited_empty_not_counted() {
        // Workspace 2 was visited (is_active: true), but is not focused and has no windows.
        // It must NOT be counted towards total.
        let items_visited_empty = vec![
            NiriWorkspaceItem {
                id: 1,
                idx: 1,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: Some(10),
            },
            NiriWorkspaceItem {
                id: 2,
                idx: 2,
                name: None,
                output: Some("eDP-1".to_string()),
                is_urgent: false,
                is_active: true,
                is_focused: false,
                active_window_id: None,
            },
        ];
        let info_visited = compute_workspace_info(&items_visited_empty).unwrap();
        assert_eq!(info_visited.current, 1);
        assert_eq!(info_visited.total, 1);
    }

    #[test]
    fn test_parse_windows() {
        let json = br#"[
            {"id":4,"title":"Antigravity","app_id":"Alacritty","pid":3149,"workspace_id":1,"is_focused":true},
            {"id":5,"title":"Firefox","app_id":"firefox","pid":4000,"workspace_id":2,"is_focused":false}
        ]"#;
        let windows = parse_windows(json);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].id, 4);
        assert_eq!(windows[0].title.as_deref(), Some("Antigravity"));
        assert_eq!(windows[0].app_id.as_deref(), Some("Alacritty"));
        assert_eq!(windows[0].workspace_id, Some(1));
        assert!(windows[0].is_focused);

        assert_eq!(windows[1].id, 5);
        assert_eq!(windows[1].workspace_id, Some(2));
        assert!(!windows[1].is_focused);
    }

    #[test]
    fn test_parse_windows_sorted_by_layout() {
        let json = br#"[
            {"id":5,"title":"Shelly","app_id":"com.shellyorg.shelly","workspace_id":1,"layout":{"pos_in_scrolling_layout":[2,1]}},
            {"id":2,"title":"Antigravity","app_id":"Alacritty","workspace_id":1,"layout":{"pos_in_scrolling_layout":[1,1]}}
        ]"#;
        let windows = parse_windows(json);
        assert_eq!(windows.len(), 2);
        // Antigravity has pos [1, 1], so it MUST come first before Shelly [2, 1]
        assert_eq!(windows[0].id, 2);
        assert_eq!(windows[1].id, 5);
    }
}
