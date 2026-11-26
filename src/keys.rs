// SPDX-License-Identifier: GPL-3.0-or-later

//! Personal keys for the third Touch Bar row, read from
//! `$XDG_CONFIG_HOME/t2-touchbar-ultra/keys.toml`.

use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use input_linux::Key;

#[derive(Clone, Debug, PartialEq)]
pub enum CustomAction {
    /// Modifiers first, the key last.
    Send(Vec<Key>),
    Run(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CustomKey {
    /// Leaked once at startup, so layouts can keep plain `&'static str`
    /// labels like the built-in keys.
    pub label: &'static str,
    pub icon: Option<&'static str>,
    pub action: CustomAction,
}

/// Editable specification entry used by `t2-touchbar-ultra-settings`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySpecAction {
    Run(String),
    Send(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySpec {
    pub label: String,
    pub icon: Option<String>,
    pub action: KeySpecAction,
}

impl CustomKey {
    /// Keys the virtual keyboard must be able to send for this entry.
    pub fn keys(&self) -> &[Key] {
        match &self.action {
            CustomAction::Send(keys) => keys,
            CustomAction::Run(_) => &[],
        }
    }

    /// Starts the command outside the daemon's service, so it survives a
    /// restart of the Touch Bar.
    pub fn run(command: &str) {
        let spawned = Command::new("systemd-run")
            .args(["--user", "--collect", "--quiet", "--", "sh", "-c", command])
            .spawn();
        match spawned {
            Ok(mut child) => {
                thread::spawn(move || child.wait());
            }
            Err(error) => eprintln!("t2-touchbar-ultra: could not start {command:?}: {error}"),
        }
    }
}

pub fn path() -> PathBuf {
    if let Some(path) = env::var_os("T2_TOUCHBAR_ULTRA_KEYS") {
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
        .join("t2-touchbar-ultra/keys.toml")
}

const TEMPLATE: &str = r#"# Personal keys for the third Touch Bar row (swipe with two fingers to reach it).
# Restart after editing:  systemctl --user restart t2-touchbar-ultra
#
# Each entry is a simple key-value pair: Label = "command"
#
# Examples:
#   fullscreen = "niri msg action fullscreen-window"
#   screenshot = "noctalia msg screenshot-region"
#   emoji = "noctalia msg panel-toggle launcher /emo"
#   Youtube = "chromium --app=https://youtube.com"
"#;

fn create_template(path: &Path) {
    let created = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| OpenOptions::new().write(true).create_new(true).open(path))
        .and_then(|mut file| file.write_all(TEMPLATE.as_bytes()));
    if let Err(error) = created {
        eprintln!(
            "t2-touchbar-ultra: could not create {}: {error}",
            path.display()
        );
    }
}

/// Loads raw editable specs for the GUI editor without leaking strings.
pub fn load_specs() -> Result<Vec<KeySpec>> {
    let path = path();
    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    parse_specs(&body)
}

pub fn parse_specs(body: &str) -> Result<Vec<KeySpec>> {
    let table: toml::Table = toml::from_str(body).context("invalid keys file")?;
    let mut specs = Vec::new();
    for (k, v) in &table {
        match v {
            toml::Value::String(cmd) => {
                specs.push(KeySpec {
                    label: k.clone(),
                    icon: None,
                    action: KeySpecAction::Run(cmd.clone()),
                });
            }
            toml::Value::Table(sub_table) => {
                for (sub_k, sub_v) in sub_table {
                    if let Some(cmd) = sub_v.as_str() {
                        specs.push(KeySpec {
                            label: sub_k.clone(),
                            icon: None,
                            action: KeySpecAction::Run(cmd.to_owned()),
                        });
                    }
                }
            }
            toml::Value::Array(entries) if k == "key" => {
                for entry_val in entries {
                    if let toml::Value::Table(entry_table) = entry_val
                        && let Some(label) = entry_table.get("label").and_then(|l| l.as_str())
                    {
                        let icon = entry_table
                            .get("icon")
                            .and_then(|i| i.as_str())
                            .map(str::to_owned);
                        if let Some(send) = entry_table.get("send").and_then(|s| s.as_str()) {
                            specs.push(KeySpec {
                                label: label.to_owned(),
                                icon,
                                action: KeySpecAction::Send(send.to_owned()),
                            });
                        } else if let Some(run) = entry_table.get("run").and_then(|r| r.as_str()) {
                            specs.push(KeySpec {
                                label: label.to_owned(),
                                icon,
                                action: KeySpecAction::Run(run.to_owned()),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(specs)
}

pub fn serialize_specs(specs: &[KeySpec]) -> Result<String> {
    let mut out = String::from(
        "# Personal keys for the third Touch Bar row\n\
         # Edit directly or via T2 Touchbar Ultra Settings.\n\n",
    );
    let mut table_entries = Vec::new();
    for spec in specs {
        let label = spec.label.trim();
        if label.is_empty() || label.chars().count() > 32 {
            bail!("key label {:?} must be 1 to 32 characters", spec.label);
        }
        match &spec.action {
            KeySpecAction::Run(cmd) => {
                let cmd = cmd.trim();
                if cmd.is_empty() {
                    bail!("command for {:?} cannot be empty", label);
                }
                if let Some(icon) = spec.icon.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    table_entries.push((label.to_owned(), Some(icon.to_owned()), None, Some(cmd.to_owned())));
                } else {
                    let k = toml::Value::String(label.to_owned()).to_string();
                    let v = toml::Value::String(cmd.to_owned()).to_string();
                    out.push_str(&format!("{k} = {v}\n"));
                }
            }
            KeySpecAction::Send(combo) => {
                validate_combination(combo)?;
                let icon = spec.icon.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                table_entries.push((label.to_owned(), icon, Some(combo.trim().to_owned()), None));
            }
        }
    }
    if !table_entries.is_empty() {
        out.push('\n');
        for (label, icon, send, run) in table_entries {
            out.push_str("[[key]]\n");
            out.push_str(&format!("label = {}\n", toml::Value::String(label)));
            if let Some(ic) = icon {
                out.push_str(&format!("icon = {}\n", toml::Value::String(ic)));
            }
            if let Some(s) = send {
                out.push_str(&format!("send = {}\n", toml::Value::String(s)));
            }
            if let Some(r) = run {
                out.push_str(&format!("run = {}\n", toml::Value::String(r)));
            }
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn save_specs(specs: &[KeySpec]) -> Result<PathBuf> {
    let body = serialize_specs(specs)?;
    let path = path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn validate_combination(text: &str) -> Result<()> {
    let _ = combination(text)?;
    Ok(())
}

/// Loads the personal keys. A missing file is created from the commented
/// template and means none. A broken entry is reported and skipped, so one
/// typo never takes the Touch Bar down.
pub fn load() -> Vec<CustomKey> {
    let path = path();
    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_template(&path);
            return Vec::new();
        }
        Err(error) => {
            eprintln!("t2-touchbar-ultra: read {}: {error}", path.display());
            return Vec::new();
        }
    };
    match parse(&body) {
        Ok(keys) => keys,
        Err(error) => {
            eprintln!("t2-touchbar-ultra: {}: {error:#}", path.display());
            Vec::new()
        }
    }
}

fn parse(body: &str) -> Result<Vec<CustomKey>> {
    let table: toml::Table = toml::from_str(body).context("invalid keys file")?;
    let mut keys = Vec::new();

    for (k, v) in &table {
        match v {
            toml::Value::String(cmd) => {
                match build_custom_key(k, None, None, Some(cmd)) {
                    Ok(key) => keys.push(key),
                    Err(error) => eprintln!("t2-touchbar-ultra: key {k}: {error:#}"),
                }
            }
            toml::Value::Table(sub_table) => {
                for (sub_k, sub_v) in sub_table {
                    if let Some(cmd) = sub_v.as_str() {
                        match build_custom_key(sub_k, None, None, Some(cmd)) {
                            Ok(key) => keys.push(key),
                            Err(error) => eprintln!("t2-touchbar-ultra: key {sub_k}: {error:#}"),
                        }
                    }
                }
            }
            toml::Value::Array(entries) if k == "key" => {
                for (index, entry_val) in entries.iter().enumerate() {
                    if let toml::Value::Table(entry_table) = entry_val
                        && let Some(label) = entry_table.get("label").and_then(|l| l.as_str())
                    {
                        let icon = entry_table.get("icon").and_then(|i| i.as_str());
                        let send = entry_table.get("send").and_then(|s| s.as_str());
                        let run = entry_table.get("run").and_then(|r| r.as_str());
                        match build_custom_key(label, icon, send, run) {
                            Ok(key) => keys.push(key),
                            Err(error) => eprintln!("t2-touchbar-ultra: key {}: {error:#}", index + 1),
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(keys)
}

fn build_custom_key(
    label: &str,
    icon: Option<&str>,
    send: Option<&str>,
    run: Option<&str>,
) -> Result<CustomKey> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 32 {
        bail!("label must be 1 to 32 characters");
    }
    let icon = icon
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|i| &*Box::leak(i.to_owned().into_boxed_str()));
    let action = match (send, run) {
        (Some(send), None) => CustomAction::Send(combination(send)?),
        (None, Some(run)) if !run.trim().is_empty() => CustomAction::Run(run.trim().to_string()),
        _ => bail!("set exactly one of send or run"),
    };
    Ok(CustomKey {
        label: Box::leak(label.to_owned().into_boxed_str()),
        icon,
        action,
    })
}

fn combination(text: &str) -> Result<Vec<Key>> {
    let parts: Vec<String> = text
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect();
    let (last, modifiers) = parts
        .split_last()
        .filter(|(last, _)| !last.is_empty())
        .ok_or_else(|| anyhow!("empty combination"))?;
    let mut keys = Vec::new();
    for modifier in modifiers {
        let key = match modifier.as_str() {
            "ctrl" | "control" => Key::LeftCtrl,
            "shift" => Key::LeftShift,
            "alt" | "option" => Key::LeftAlt,
            "altgr" => Key::RightAlt,
            "super" | "meta" | "cmd" | "command" => Key::LeftMeta,
            other => bail!("unknown modifier {other:?}"),
        };
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.push(key_by_name(last).ok_or_else(|| anyhow!("unknown key {last:?}"))?);
    Ok(keys)
}

fn key_by_name(name: &str) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A, Key::B, Key::C, Key::D, Key::E, Key::F, Key::G, Key::H, Key::I,
        Key::J, Key::K, Key::L, Key::M, Key::N, Key::O, Key::P, Key::Q, Key::R,
        Key::S, Key::T, Key::U, Key::V, Key::W, Key::X, Key::Y, Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4,
        Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9,
    ];
    const FUNCTION: [Key; 12] = [
        Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6,
        Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12,
    ];
    let mut chars = name.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        if single.is_ascii_lowercase() {
            return Some(LETTERS[(single as u8 - b'a') as usize]);
        }
        if single.is_ascii_digit() {
            return Some(DIGITS[(single as u8 - b'0') as usize]);
        }
    }
    if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
        return (1..=12).contains(&number).then(|| FUNCTION[number - 1]);
    }
    Some(match name {
        "-" | "minus" => Key::Minus,
        "=" | "equal" => Key::Equal,
        "[" | "leftbrace" => Key::LeftBrace,
        "]" | "rightbrace" => Key::RightBrace,
        ";" | "semicolon" => Key::Semicolon,
        "'" | "apostrophe" => Key::Apostrophe,
        "`" | "grave" => Key::Grave,
        "\\" | "backslash" => Key::Backslash,
        "," | "comma" => Key::Comma,
        "." | "dot" => Key::Dot,
        "/" | "slash" => Key::Slash,
        "iso" | "102nd" => Key::NonUsBackslashAndPipe,
        "space" => Key::Space,
        "tab" => Key::Tab,
        "enter" | "return" => Key::Enter,
        "backspace" => Key::Backspace,
        "esc" | "escape" => Key::Esc,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "print" => Key::Sysrq,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_file_is_parsed_and_broken_entries_skipped() {
        let keys = parse(
            r#"
            [[key]]
            label = "~"
            send = "Alt+N"

            [[key]]
            label = "|"
            send = "alt+shift+7"

            [[key]]
            label = "term"
            run = "kgx"

            [[key]]
            label = "bad"
            send = "hyper+q"

            [[key]]
            label = "both"
            send = "a"
            run = "b"
            "#,
        )
        .unwrap();
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].label, "~");
        assert_eq!(keys[0].action, CustomAction::Send(vec![Key::LeftAlt, Key::N]));
        assert_eq!(keys[1].action, CustomAction::Send(vec![Key::LeftAlt, Key::LeftShift, Key::Num7]));
        assert_eq!(keys[2].action, CustomAction::Run("kgx".to_owned()));
    }

    #[test]
    fn template_is_valid_and_defines_no_keys() {
        assert!(parse(TEMPLATE).unwrap().is_empty());
        let examples: String = TEMPLATE
            .lines()
            .filter_map(|line| line.strip_prefix("#   "))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(parse(&examples).unwrap().len(), 4);
    }

    #[test]
    fn template_never_replaces_an_existing_file() {
        let dir = env::temp_dir().join(format!("t2-ultra-keys-{}", std::process::id()));
        let path = dir.join("t2-touchbar-ultra/keys.toml");
        create_template(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        fs::write(&path, "# mine").unwrap();
        create_template(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "# mine");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_names_cover_positions_and_specials() {
        assert_eq!(
            combination("ctrl+alt+t").unwrap(),
            [Key::LeftCtrl, Key::LeftAlt, Key::T]
        );
        assert_eq!(
            combination("altgr+iso").unwrap(),
            [Key::RightAlt, Key::NonUsBackslashAndPipe]
        );
        assert_eq!(combination("F12").unwrap(), [Key::F12]);
        assert!(combination("ctrl+").is_err());
        assert!(combination("f13").is_err());
    }

    #[test]
    fn specs_roundtrip_both_run_and_send() {
        let specs = vec![
            KeySpec {
                label: "gemini".into(),
                icon: None,
                action: KeySpecAction::Run("chromium --app=https://gemini.google.com".into()),
            },
            KeySpec {
                label: "screenshot".into(),
                icon: Some("screenshot".into()),
                action: KeySpecAction::Run("noctalia msg screenshot-region".into()),
            },
            KeySpec {
                label: "copy".into(),
                icon: None,
                action: KeySpecAction::Send("ctrl+c".into()),
            },
        ];
        let toml_text = serialize_specs(&specs).unwrap();
        let parsed = parse_specs(&toml_text).unwrap();
        assert_eq!(specs, parsed);
        assert_eq!(parse(&toml_text).unwrap().len(), 3);
    }
}
