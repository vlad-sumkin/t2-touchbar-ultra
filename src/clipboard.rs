use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MAX_CLIPBOARD_ENTRIES: usize = 10;
const MAX_ITEM_CHARS: usize = 1000;
const POLL_INTERVAL_MS: u64 = 800;

fn is_valid_text(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.len() > 30000 {
        return false;
    }
    let bytes = trimmed.as_bytes();
    if bytes.starts_with(b"\x89PNG")
        || trimmed.starts_with("\u{FFFD}PNG")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"%PDF")
        || bytes.starts_with(b"\x7fELF")
    {
        return false;
    }
    if trimmed.contains('\0') {
        return false;
    }
    true
}

fn sanitize_item(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() > MAX_ITEM_CHARS {
        trimmed.chars().take(MAX_ITEM_CHARS).collect()
    } else {
        trimmed.to_string()
    }
}

#[derive(Clone)]
pub struct ClipboardManager {
    entries: Arc<Mutex<VecDeque<String>>>,
    #[allow(dead_code)]
    state_file: PathBuf,
}

impl ClipboardManager {
    pub fn new() -> Self {
        let state_dir = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local/state")
            })
            .join("t2-touchbar-ultra");
        let _ = fs::create_dir_all(&state_dir);
        let state_file = state_dir.join("clipboard_history.json");

        let mut initial_entries = VecDeque::new();
        if let Ok(content) = fs::read_to_string(&state_file) {
            if let Ok(loaded) = serde_json::from_str::<Vec<String>>(&content) {
                for item in loaded {
                    if is_valid_text(&item) {
                        let clean = sanitize_item(&item);
                        if !initial_entries.contains(&clean) {
                            initial_entries.push_back(clean);
                            if initial_entries.len() >= MAX_CLIPBOARD_ENTRIES {
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Seed with current clipboard if empty
        if initial_entries.is_empty() {
            if let Ok(out) = std::process::Command::new("wl-paste")
                .args(["-t", "text", "-n"])
                .output()
            {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    if is_valid_text(&text) {
                        initial_entries.push_back(sanitize_item(&text));
                    }
                }
            }
        }

        let entries = Arc::new(Mutex::new(initial_entries));
        let manager = Self {
            entries: Arc::clone(&entries),
            state_file: state_file.clone(),
        };

        // Start background watcher thread
        let thread_entries = Arc::clone(&entries);
        let thread_file = state_file;
        thread::Builder::new()
            .name("clipboard-watcher".into())
            .spawn(move || {
                let mut last_seen = {
                    let guard = thread_entries.lock().unwrap();
                    guard.front().cloned().unwrap_or_default()
                };

                loop {
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                    let out = std::process::Command::new("wl-paste")
                        .args(["-t", "text", "-n"])
                        .output();

                    if let Ok(output) = out {
                        if output.status.success() {
                            let raw = String::from_utf8_lossy(&output.stdout);
                            if is_valid_text(&raw) {
                                let clean = sanitize_item(&raw);
                                if clean != last_seen {
                                    last_seen = clean.clone();
                                    let mut guard = thread_entries.lock().unwrap();
                                    guard.retain(|item| item != &clean);
                                    guard.push_front(clean);
                                    while guard.len() > MAX_CLIPBOARD_ENTRIES {
                                        guard.pop_back();
                                    }
                                    let to_save: Vec<String> = guard.iter().cloned().collect();
                                    drop(guard);
                                    if let Ok(json) = serde_json::to_string_pretty(&to_save) {
                                        let _ = fs::write(&thread_file, json);
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .expect("spawn clipboard watcher thread");

        manager
    }

    pub fn get_entries(&self) -> Vec<String> {
        let guard = self.entries.lock().unwrap();
        guard.iter().cloned().collect()
    }

    pub fn items(&self) -> Vec<String> {
        self.get_entries()
    }

    #[allow(dead_code)]
    pub fn add_entry(&self, text: &str) {
        if !is_valid_text(text) {
            return;
        }
        let clean = sanitize_item(text);
        let mut guard = self.entries.lock().unwrap();
        guard.retain(|item| item != &clean);
        guard.push_front(clean);
        while guard.len() > MAX_CLIPBOARD_ENTRIES {
            guard.pop_back();
        }
        let to_save: Vec<String> = guard.iter().cloned().collect();
        drop(guard);
        if let Ok(json) = serde_json::to_string_pretty(&to_save) {
            let _ = fs::write(&self.state_file, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_add_and_dedup() {
        let dir = std::env::temp_dir().join(format!("test_clip_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("history.json");

        let manager = ClipboardManager {
            entries: Arc::new(Mutex::new(VecDeque::new())),
            state_file: file.clone(),
        };

        manager.add_entry("first item");
        manager.add_entry("second item");
        manager.add_entry("first item"); // Should move to front

        let list = manager.get_entries();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], "first item");
        assert_eq!(list[1], "second item");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_clipboard_binary_filtering_and_sanitizing() {
        assert!(!is_valid_text(""));
        assert!(!is_valid_text("   \n\t  "));
        assert!(!is_valid_text("\x7fELF\x02\x01\x01"));
        assert!(!is_valid_text("%PDF-1.4\n"));
        assert!(!is_valid_text("hello\0world"));
        let binary_png = String::from_utf8_lossy(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR");
        assert!(!is_valid_text(&binary_png));

        let valid_text = "https://github.com/torvalds/linux";
        assert!(is_valid_text(valid_text));

        let huge_str = "a".repeat(5000);
        let sanitized = sanitize_item(&huge_str);
        assert_eq!(sanitized.chars().count(), MAX_ITEM_CHARS);
    }
}
