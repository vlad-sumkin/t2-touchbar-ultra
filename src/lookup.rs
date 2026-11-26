use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordSegment {
    pub text: String,
    pub is_yellow: bool,
    pub is_italic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordDefinition {
    pub word: String,
    pub lang: Option<String>,
    pub translation: Option<String>,
    pub definition: Option<String>,
    pub text: String,
}

impl WordDefinition {
    pub fn segments(&self) -> Vec<WordSegment> {
        let mut segs = Vec::new();
        let word_prefix = if let Some(lang) = &self.lang {
            if lang != "ru" {
                format!("{} [{}] — ", self.word, lang)
            } else {
                format!("{} — ", self.word)
            }
        } else {
            format!("{} — ", self.word)
        };
        segs.push(WordSegment {
            text: word_prefix,
            is_yellow: false,
            is_italic: false,
        });

        if let Some(tr) = &self.translation {
            segs.push(WordSegment {
                text: tr.clone(),
                is_yellow: true,
                is_italic: false,
            });
            if self.definition.is_some() {
                segs.push(WordSegment {
                    text: " — ".to_string(),
                    is_yellow: false,
                    is_italic: false,
                });
            }
        }

        if let Some(def) = &self.definition {
            segs.push(WordSegment {
                text: def.clone(),
                is_yellow: false,
                is_italic: true,
            });
        }

        segs
    }

    pub fn from_text(s: &str) -> Self {
        let parts: Vec<&str> = s.split(" — ").collect();
        match parts.len() {
            3 => {
                let first = parts[0].trim();
                let (word, lang) = parse_word_and_lang(first);
                Self {
                    word,
                    lang,
                    translation: Some(parts[1].trim().to_string()),
                    definition: Some(parts[2].trim().to_string()),
                    text: s.to_string(),
                }
            }
            2 => {
                let first = parts[0].trim();
                let (word, lang) = parse_word_and_lang(first);
                Self {
                    word,
                    lang,
                    translation: None,
                    definition: Some(parts[1].trim().to_string()),
                    text: s.to_string(),
                }
            }
            _ => Self {
                word: s.to_string(),
                lang: None,
                translation: None,
                definition: None,
                text: s.to_string(),
            },
        }
    }
}

fn parse_word_and_lang(s: &str) -> (String, Option<String>) {
    if let Some(bracket_start) = s.rfind('[') {
        if let Some(bracket_end) = s.rfind(']') {
            if bracket_end > bracket_start {
                let lang = &s[bracket_start + 1..bracket_end];
                let word = s[..bracket_start].trim();
                return (word.to_string(), Some(lang.to_string()));
            }
        }
    }
    (s.to_string(), None)
}

#[allow(dead_code)]
pub fn start_selection_lookup() -> (Receiver<WordDefinition>, UnixStream) {
    start_selection_lookup_with_delay(3000)
}

pub fn start_selection_lookup_with_delay(delay_ms: u64) -> (Receiver<WordDefinition>, UnixStream) {
    let delay = Duration::from_millis(delay_ms.clamp(200, 15_000));
    let (tx, rx) = mpsc::channel();
    let (wake_reader, mut wake_writer) = UnixStream::pair().expect("create unix stream pair");
    let _ = wake_reader.set_nonblocking(true);

    let cache: Arc<Mutex<HashMap<String, WordDefinition>>> = Arc::new(Mutex::new(HashMap::new()));

    thread::Builder::new()
        .name("selection-lookup".into())
        .spawn(move || {
            let mut pending_word: Option<String> = None;
            let mut pending_since: Option<std::time::Instant> = None;
            let mut last_dispatched = String::new();

            loop {
                thread::sleep(Duration::from_millis(200));

                let out = std::process::Command::new("wl-paste")
                    .args(["--primary", "--no-newline"])
                    .output();

                let current_candidate = if let Ok(output) = out {
                    if output.status.success() {
                        let raw = String::from_utf8_lossy(&output.stdout);
                        sanitize_candidate(&raw)
                    } else {
                        None
                    }
                } else {
                    None
                };

                match current_candidate {
                    Some(word) => {
                        if word == last_dispatched {
                            pending_word = None;
                            pending_since = None;
                        } else if pending_word.as_ref() == Some(&word) {
                            if let Some(since) = pending_since {
                                if since.elapsed() >= delay {
                                    last_dispatched = word.clone();
                                    pending_word = None;
                                    pending_since = None;

                                    // Check in-memory cache first
                                    let cached = {
                                        let guard = cache.lock().unwrap();
                                        guard.get(&word).cloned()
                                    };

                                    if let Some(def) = cached {
                                        let _ = tx.send(def);
                                        let _ = wake_writer.write_all(&[1]);
                                    } else {
                                        let cache_clone = Arc::clone(&cache);
                                        let tx_clone = tx.clone();
                                        let mut wake_clone = wake_writer.try_clone().unwrap();
                                        let word_clone = word.clone();

                                        thread::spawn(move || {
                                            if let Some(def) = fetch_word_definition(&word_clone) {
                                                {
                                                    let mut guard = cache_clone.lock().unwrap();
                                                    guard.insert(word_clone.clone(), def.clone());
                                                }
                                                let _ = tx_clone.send(def);
                                                let _ = wake_clone.write_all(&[1]);
                                            }
                                        });
                                    }
                                }
                            }
                        } else {
                            pending_word = Some(word);
                            pending_since = Some(std::time::Instant::now());
                        }
                    }
                    None => {
                        pending_word = None;
                        pending_since = None;
                        last_dispatched.clear();
                    }
                }
            }
        })
        .expect("spawn selection lookup watcher");

    (rx, wake_reader)
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |
        '\u{3400}'..='\u{4DBF}' |
        '\u{F900}'..='\u{FAFF}' |
        '\u{3040}'..='\u{309F}' |
        '\u{30A0}'..='\u{30FF}' |
        '\u{AC00}'..='\u{D7AF}'
    )
}

fn sanitize_candidate(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }
    // Don't treat URLs, file paths, or numbers as dictionary lookup candidates
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with('/')
        || trimmed.starts_with("file://")
    {
        return None;
    }

    // Strip edge punctuation (parentheses, quotes, brackets, comma, dot, colon, etc.)
    let word = trimmed.trim_matches(|c: char| {
        c == '('
            || c == ')'
            || c == '['
            || c == ']'
            || c == '{'
            || c == '}'
            || c == '"'
            || c == '\''
            || c == '`'
            || c == '«'
            || c == '»'
            || c == '“'
            || c == '”'
            || c == '.'
            || c == ','
            || c == ';'
            || c == ':'
            || c == '!'
            || c == '?'
            || c == '<'
            || c == '>'
            || c == '、'
            || c == '。'
            || c == '「'
            || c == '」'
            || c == '『'
            || c == '』'
            || c == '（'
            || c == '）'
    });

    let char_count = word.chars().count();
    let has_cjk = word.chars().any(is_cjk);
    let min_chars = if has_cjk { 1 } else { 2 };
    if char_count < min_chars || char_count > 35 {
        return None;
    }

    // Must contain letters/ideographs and not be purely digits/symbols
    let has_alphabetic = word.chars().any(char::is_alphabetic);
    let valid_chars = word
        .chars()
        .all(|c| c.is_alphabetic() || c == '-' || c == '\'');

    if (has_alphabetic && valid_chars) || has_cjk {
        Some(word.to_string())
    } else {
        None
    }
}

fn translate_text(text: &str, target_lang: &str) -> Option<String> {
    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "-m",
            "4",
            "-G",
            "--data-urlencode",
            &format!("q={text}"),
            &format!("https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl={target_lang}&dt=t"),
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let segments = json.get(0)?.as_array()?;
    let mut parts = Vec::new();
    for seg in segments {
        if let Some(s) = seg.get(0).and_then(|v| v.as_str()) {
            parts.push(s);
        }
    }
    let res = parts.join("").trim().to_string();
    if res.is_empty() { None } else { Some(res) }
}

fn fetch_ru_dictionary_definition(word: &str) -> Vec<String> {
    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "-m",
            "4",
            "-G",
            "--data-urlencode",
            &format!("q={word}"),
            "https://translate.googleapis.com/translate_a/single?client=gtx&sl=ru&tl=ru&dt=t&dt=bd&dt=md",
        ])
        .output()
        .ok();

    let mut defs = Vec::new();
    let Some(output) = output else { return defs; };
    if !output.status.success() { return defs; }

    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else { return defs; };
    if let Some(def_sections) = json.get(12).and_then(|v| v.as_array()) {
        for sec in def_sections {
            if let Some(def_list) = sec.get(1).and_then(|v| v.as_array()) {
                for d in def_list {
                    if let Some(text) = d.get(0).and_then(|v| v.as_str()) {
                        let text = text.trim();
                        if !text.is_empty() && !defs.contains(&text.to_string()) {
                            defs.push(text.to_string());
                            if defs.len() >= 2 {
                                return defs;
                            }
                        }
                    }
                }
            }
        }
    }
    defs
}

pub fn fetch_word_definition(word: &str) -> Option<WordDefinition> {
    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "-m",
            "4",
            "-G",
            "--data-urlencode",
            &format!("q={word}"),
            "https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl=ru&dt=t&dt=bd&dt=md",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let lang = json
        .get(2)
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_lowercase();

    // 1. Try to find Russian translation
    let mut trans = None;
    if let Some(meanings) = json.get(1).and_then(|v| v.as_array()) {
        for m in meanings {
            if let Some(words) = m.get(1).and_then(|v| v.as_array()) {
                if let Some(first) = words.first().and_then(|v| v.as_str()) {
                    trans = Some(first.trim().to_string());
                    break;
                }
            }
        }
    }
    if trans.is_none() {
        if let Some(t) = json
            .get(0)
            .and_then(|v| v.get(0))
            .and_then(|v| v.get(0))
            .and_then(|v| v.as_str())
        {
            let t = t.trim().to_string();
            if !t.is_empty() && t.to_lowercase() != word.to_lowercase() {
                trans = Some(t);
            }
        }
    }

    // 2. Extract source definitions from data[12]
    let mut source_defs = Vec::new();
    if let Some(def_sections) = json.get(12).and_then(|v| v.as_array()) {
        for sec in def_sections {
            if let Some(def_list) = sec.get(1).and_then(|v| v.as_array()) {
                for d in def_list {
                    if let Some(text) = d.get(0).and_then(|v| v.as_str()) {
                        let text = text.trim();
                        if !text.is_empty() && !source_defs.contains(&text.to_string()) {
                            source_defs.push(text.to_string());
                            if source_defs.len() >= 2 {
                                break;
                            }
                        }
                    }
                }
            }
            if source_defs.len() >= 2 {
                break;
            }
        }
    }

    // 3. Ensure definitions are in Russian
    let mut ru_defs = Vec::new();
    if lang == "ru" {
        ru_defs = source_defs;
    } else {
        // If source definitions exist (e.g. English, French, German), translate them into Russian
        if !source_defs.is_empty() {
            for sd in &source_defs {
                if let Some(tr) = translate_text(sd, "ru") {
                    if !tr.is_empty() && !ru_defs.contains(&tr) {
                        ru_defs.push(tr);
                    }
                }
            }
        }
        // If no definitions yet (or hieroglyphs where dt=md is empty), look up definition of Russian translation
        if ru_defs.is_empty() {
            if let Some(t) = &trans {
                let first_word = t.split_whitespace().next().unwrap_or(t).trim_matches(|c: char| !c.is_alphabetic());
                if !first_word.is_empty() {
                    ru_defs = fetch_ru_dictionary_definition(first_word);
                }
            }
        }
    }

    if lang == "ru" {
        if !ru_defs.is_empty() {
            let def_text = ru_defs.join(" • ");
            Some(WordDefinition {
                word: word.to_string(),
                lang: Some("ru".to_string()),
                translation: None,
                definition: Some(def_text.clone()),
                text: format!("{word} — {def_text}"),
            })
        } else {
            trans.map(|t| WordDefinition {
                word: word.to_string(),
                lang: Some("ru".to_string()),
                translation: Some(t.clone()),
                definition: None,
                text: format!("{word} — {t}"),
            })
        }
    } else {
        let tr_str = trans.clone();
        let def_str = if !ru_defs.is_empty() {
            Some(ru_defs.join(" • "))
        } else {
            None
        };
        let mut parts = Vec::new();
        if let Some(t) = &tr_str {
            parts.push(t.clone());
        }
        if let Some(d) = &def_str {
            parts.push(d.clone());
        }
        if !parts.is_empty() {
            let combined = parts.join(" — ");
            Some(WordDefinition {
                word: word.to_string(),
                lang: Some(lang.clone()),
                translation: tr_str,
                definition: def_str,
                text: format!("{word} [{lang}] — {combined}"),
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_candidate() {
        assert_eq!(sanitize_candidate("word"), Some("word".to_string()));
        assert_eq!(sanitize_candidate("  word  "), Some("word".to_string()));
        assert_eq!(sanitize_candidate("(word)"), Some("word".to_string()));
        assert_eq!(sanitize_candidate("«практичный»"), Some("практичный".to_string()));
        assert_eq!(sanitize_candidate("two words"), None);
        assert_eq!(sanitize_candidate("https://example.com"), None);
        assert_eq!(sanitize_candidate("/path/to/file"), None);
        assert_eq!(sanitize_candidate("12345"), None);
        assert_eq!(sanitize_candidate("a"), None);
        assert_eq!(sanitize_candidate("猫"), Some("猫".to_string()));
        assert_eq!(sanitize_candidate("「山」"), Some("山".to_string()));
        assert_eq!(sanitize_candidate("爱"), Some("爱".to_string()));
    }

    #[test]
    fn test_word_definition_segments() {
        let def_en = WordDefinition::from_text("serendipity [en] — случайность — интуитивная прозорливость");
        assert_eq!(def_en.word, "serendipity");
        assert_eq!(def_en.lang, Some("en".to_string()));
        assert_eq!(def_en.translation, Some("случайность".to_string()));
        assert_eq!(def_en.definition, Some("интуитивная прозорливость".to_string()));

        let segs = def_en.segments();
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].text, "serendipity [en] — ");
        assert!(!segs[0].is_yellow && !segs[0].is_italic);
        assert_eq!(segs[1].text, "случайность");
        assert!(segs[1].is_yellow && !segs[1].is_italic);
        assert_eq!(segs[2].text, " — ");
        assert!(!segs[2].is_yellow && !segs[2].is_italic);
        assert_eq!(segs[3].text, "интуитивная прозорливость");
        assert!(!segs[3].is_yellow && segs[3].is_italic);

        let def_ru = WordDefinition::from_text("практичный — Деловитый, экономный");
        assert_eq!(def_ru.word, "практичный");
        assert_eq!(def_ru.lang, None);
        assert_eq!(def_ru.translation, None);
        assert_eq!(def_ru.definition, Some("Деловитый, экономный".to_string()));

        let segs_ru = def_ru.segments();
        assert_eq!(segs_ru.len(), 2);
        assert_eq!(segs_ru[0].text, "практичный — ");
        assert!(!segs_ru[0].is_yellow && !segs_ru[0].is_italic);
        assert_eq!(segs_ru[1].text, "Деловитый, экономный");
        assert!(!segs_ru[1].is_yellow && segs_ru[1].is_italic);
    }
}
