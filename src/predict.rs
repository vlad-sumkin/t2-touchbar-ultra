// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
    thread,
};

use input_linux::Key;

const MAX_CANDIDATES: usize = 6;
const MAX_LOCAL_WORDS: usize = 200_000;

// Embedded fallbacks for common words in case dictionary files are missing
const EMBEDDED_RU_WORDS: &[&str] = &[
    "что", "это", "как", "мне", "меня", "все", "она", "тебя", "если", "тебе",
    "только", "теперь", "даже", "знаю", "может", "здесь", "сейчас", "очень",
    "когда", "будет", "просто", "почему", "потом", "тоже", "хочу", "было",
    "сказать", "привет", "спасибо", "пожалуйста", "программа", "программист",
    "система", "работа", "компьютер", "хорошо", "отлично", "человек", "время",
    "дело", "жизнь", "день", "друг", "вопрос", "дом", "город", "машина",
    "деньги", "решение", "возможность", "результат", "разговор", "понедельник",
    "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье",
];

const EMBEDDED_EN_WORDS: &[&str] = &[
    "you", "the", "that", "what", "this", "have", "your", "with", "just",
    "from", "they", "will", "like", "know", "about", "there", "here", "would",
    "think", "time", "really", "when", "some", "good", "people", "want",
    "hello", "thanks", "thank", "please", "program", "project", "system",
    "computer", "developer", "work", "great", "today", "tomorrow", "yesterday",
    "problem", "question", "friend", "house", "night", "morning", "afternoon",
];

pub struct WorkerRequest {
    pub query: String,
    pub is_next_word: bool,
    pub prev_word: String,
    pub lang: String,
}

pub struct WorkerResponse {
    pub query: String,
    pub is_next_word: bool,
    pub prev_word: String,
    pub candidates: Vec<String>,
}

pub struct Predictor {
    ru_words: Vec<String>,
    en_words: Vec<String>,
    ru_bigrams: HashMap<String, Vec<String>>,
    en_bigrams: HashMap<String, Vec<String>>,
    worker_tx: mpsc::Sender<WorkerRequest>,
    worker_rx: mpsc::Receiver<WorkerResponse>,
    cache: HashMap<String, Vec<String>>,
}

impl Predictor {
    pub fn new() -> (Self, UnixStream) {
        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<WorkerResponse>();
        let (wake_reader, wake_writer) = UnixStream::pair().expect("create predict wake socket pair");

        // Load local dictionaries
        let dict_dir = Self::find_dict_dir();
        let ru_words = Self::load_word_list(dict_dir.as_deref().map(|p| p.join("ru.txt")), EMBEDDED_RU_WORDS);
        let en_words = Self::load_word_list(dict_dir.as_deref().map(|p| p.join("en.txt")), EMBEDDED_EN_WORDS);
        let ru_bigrams = Self::load_bigrams(dict_dir.as_deref().map(|p| p.join("ru_bigrams.txt")));
        let en_bigrams = Self::load_bigrams(dict_dir.as_deref().map(|p| p.join("en_bigrams.txt")));

        // Spawn background worker for online suggestions
        thread::Builder::new()
            .name("t2-predict-worker".into())
            .spawn(move || {
                worker_loop(req_rx, resp_tx, wake_writer);
            })
            .expect("spawn predict worker thread");

        (
            Self {
                ru_words,
                en_words,
                ru_bigrams,
                en_bigrams,
                worker_tx: req_tx,
                worker_rx: resp_rx,
                cache: HashMap::new(),
            },
            wake_reader,
        )
    }

    fn find_dict_dir() -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(env_dir) = std::env::var_os("T2_TOUCHBAR_ULTRA_DICT") {
            candidates.push(PathBuf::from(env_dir));
        }
        candidates.push(PathBuf::from("./dict"));
        candidates.push(PathBuf::from("/usr/share/t2-touchbar-ultra/dict"));
        candidates.into_iter().find(|p| p.is_dir())
    }

    fn load_word_list(path: Option<PathBuf>, fallback: &[&str]) -> Vec<String> {
        if let Some(p) = path
            && let Ok(file) = File::open(&p)
        {
            let reader = BufReader::new(file);
            let words: Vec<String> = reader
                .lines()
                .filter_map(Result::ok)
                .map(|line| line.trim().to_lowercase())
                .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_alphabetic() || c == '-'))
                .take(MAX_LOCAL_WORDS)
                .collect();
            if !words.is_empty() {
                return words;
            }
        }
        fallback.iter().map(|&s| s.to_string()).collect()
    }

    fn load_bigrams(path: Option<PathBuf>) -> HashMap<String, Vec<String>> {
        let mut map = HashMap::new();
        if let Some(p) = path
            && let Ok(file) = File::open(&p)
        {
            let reader = BufReader::new(file);
            for line in reader.lines().filter_map(Result::ok) {
                let parts: Vec<&str> = line.split('\t').map(str::trim).filter(|s| !s.is_empty()).collect();
                if parts.len() >= 2 {
                    let word = parts[0].to_lowercase();
                    let nexts = parts[1..].iter().map(|s| s.to_string()).collect();
                    map.insert(word, nexts);
                }
            }
        }
        map
    }

    /// Process any online responses that arrived via background worker.
    pub fn poll_responses(&mut self) -> bool {
        let mut updated = false;
        while let Ok(resp) = self.worker_rx.try_recv() {
            let key = if resp.is_next_word {
                format!("next:{}", resp.prev_word)
            } else {
                resp.query.clone()
            };
            self.cache.insert(key, resp.candidates);
            updated = true;
        }
        updated
    }

    /// Get combined predictive suggestions for current typed prefix and previous word.
    pub fn suggest(&mut self, prefix: &str, prev_word: Option<&str>, layout: &str) -> Vec<String> {
        let is_ru = layout.trim().eq_ignore_ascii_case("RU");
        let lang = if is_ru { "ru" } else { "en" };
        let mut results = Vec::new();

        let prefix_clean = prefix.trim().to_lowercase();

        if prefix_clean.is_empty() {
            // Next-word prediction mode!
            if let Some(prev) = prev_word {
                let prev_clean = prev.trim().to_lowercase();
                if !prev_clean.is_empty() {
                    // Extract last word for local bigram lookup
                    let last_word = prev_clean.split_whitespace().last().unwrap_or(&prev_clean);
                    let bigrams = if is_ru { &self.ru_bigrams } else { &self.en_bigrams };
                    if let Some(nexts) = bigrams.get(last_word) {
                        for word in nexts {
                            if !results.contains(word) {
                                results.push(word.clone());
                            }
                        }
                    }

                    // Check cache for online next words
                    let cache_key = format!("next:{prev_clean}");
                    if let Some(cached) = self.cache.get(&cache_key) {
                        for word in cached {
                            if !results.contains(word) && results.len() < MAX_CANDIDATES {
                                results.push(word.clone());
                            }
                        }
                    } else {
                        // Request background online next-word prediction using full context
                        let _ = self.worker_tx.send(WorkerRequest {
                            query: format!("{prev_clean} "),
                            is_next_word: true,
                            prev_word: prev_clean.clone(),
                            lang: lang.to_string(),
                        });
                    }

                    // If still empty (waiting for online worker), provide high-probability smart next words
                    // so the Touch Bar ALWAYS displays next-word options immediately without disappearing!
                    if results.is_empty() {
                        let fallbacks = if is_ru {
                            &["и", "в", "не", "на", "что", "для"][..]
                        } else {
                            &["and", "the", "to", "in", "is", "for"][..]
                        };
                        for &word in fallbacks {
                            results.push(word.to_string());
                        }
                    }
                }
            }

            // If beginning of a new sentence, capitalize suggestions
            let is_sentence_start = prev_word.map_or(false, |pw| {
                let trimmed = pw.trim_end();
                trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?')
            });
            if is_sentence_start {
                results = results.into_iter().map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    }
                }).collect();
            }

            return results;
        }

        // Suggestions only activate after typing at least 3 letters
        if prefix_clean.chars().count() < 3 {
            return Vec::new();
        }

        let dict = if is_ru { &self.ru_words } else { &self.en_words };

        // 1. Online cached results first (ranked by real-world popularity by Yandex/Google)
        if let Some(cached) = self.cache.get(&prefix_clean) {
            for word in cached {
                if !results.contains(word) {
                    results.push(word.clone());
                    if results.len() >= MAX_CANDIDATES {
                        break;
                    }
                }
            }
        }

        // 2. Exact prefix matching from local dictionary with stem diversity
        // Avoid filling all candidate slots with near-identical case inflections of one word
        let mut deferred_inflections = Vec::new();
        for word in dict {
            if word.starts_with(&prefix_clean) {
                if results.contains(word) {
                    continue;
                }
                // Check if we already have a word with the exact same base stem
                let is_inflection = results.iter().any(|existing| {
                    let w_chars: Vec<char> = word.chars().collect();
                    let e_chars: Vec<char> = existing.chars().collect();
                    if (w_chars.len() as isize - e_chars.len() as isize).abs() <= 2 {
                        let min_len = w_chars.len().min(e_chars.len());
                        if min_len >= 4 {
                            let common = w_chars.iter().zip(e_chars.iter()).take_while(|(a, b)| a == b).count();
                            return common >= min_len - 1;
                        }
                    }
                    false
                });

                if is_inflection {
                    deferred_inflections.push(word.clone());
                } else {
                    results.push(word.clone());
                    if results.len() >= MAX_CANDIDATES {
                        break;
                    }
                }
            }
        }

        // If we still have slots available, add deferred inflections
        if results.len() < MAX_CANDIDATES {
            for word in deferred_inflections {
                if !results.contains(&word) {
                    results.push(word);
                    if results.len() >= MAX_CANDIDATES {
                        break;
                    }
                }
            }
        }

        // 3. Fuzzy typo-tolerant matching if not enough exact candidates
        if results.len() < MAX_CANDIDATES && prefix_clean.chars().count() >= 3 {
            let p_len = prefix_clean.chars().count();
            let max_dist = if p_len >= 6 { 2 } else { 1 };
            let first_char = prefix_clean.chars().next().unwrap();

            let mut fuzzy_candidates: Vec<(usize, String)> = Vec::new();

            for word in dict {
                if results.contains(word) {
                    continue;
                }
                let w_len = word.chars().count();
                if w_len < p_len.saturating_sub(1) {
                    continue;
                }
                let w_first = word.chars().next().unwrap();
                if w_first != first_char && p_len < 4 {
                    continue;
                }

                // Check prefix of same length as typed
                let w_sub: String = word.chars().take(p_len).collect();
                let dist = damerau_levenshtein(&prefix_clean, &w_sub);
                if dist <= max_dist {
                    fuzzy_candidates.push((dist, word.clone()));
                    if fuzzy_candidates.len() >= 20 {
                        break;
                    }
                    continue;
                }

                // Check prefix one char longer (for missing letter typos like 'програма' -> 'программа')
                if w_len >= p_len + 1 {
                    let w_sub_plus: String = word.chars().take(p_len + 1).collect();
                    let dist_plus = damerau_levenshtein(&prefix_clean, &w_sub_plus);
                    if dist_plus <= max_dist {
                        fuzzy_candidates.push((dist_plus, word.clone()));
                        if fuzzy_candidates.len() >= 20 {
                            break;
                        }
                    }
                }
            }

            fuzzy_candidates.sort_by_key(|(dist, _)| *dist);
            for (_, word) in fuzzy_candidates {
                if !results.contains(&word) {
                    results.push(word);
                    if results.len() >= MAX_CANDIDATES {
                        break;
                    }
                }
            }
        }

        // 4. Always queue async request for online completions if not cached yet
        if prefix_clean.chars().count() >= 3 && !self.cache.contains_key(&prefix_clean) {
            let _ = self.worker_tx.send(WorkerRequest {
                query: prefix_clean.clone(),
                is_next_word: false,
                prev_word: prev_word.unwrap_or("").to_lowercase(),
                lang: lang.to_string(),
            });
        }

        // 5. Adapt casing to match user's typed prefix (TitleCase or ALL CAPS)
        let is_title_case = prefix.chars().next().map_or(false, |c| c.is_uppercase());
        let is_all_caps = !prefix.is_empty() && prefix.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());

        if is_all_caps && prefix.chars().count() >= 2 {
            results = results.into_iter().map(|w| w.to_uppercase()).collect();
        } else if is_title_case {
            results = results.into_iter().map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            }).collect();
        }

        results
    }
}

/// Calculate Damerau-Levenshtein distance (insertions, deletions, substitutions, transpositions).
pub fn damerau_levenshtein(s1: &str, s2: &str) -> usize {
    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();
    let len1 = s1_chars.len();
    let len2 = s2_chars.len();

    if len1 == 0 {
        return len2;
    }
    if len2 == 0 {
        return len1;
    }

    let mut d = vec![vec![0usize; len2 + 1]; len1 + 1];

    for i in 0..=len1 {
        d[i][0] = i;
    }
    for j in 0..=len2 {
        d[0][j] = j;
    }

    for i in 1..=len1 {
        for j in 1..=len2 {
            let cost = if s1_chars[i - 1] == s2_chars[j - 1] { 0 } else { 1 };
            d[i][j] = std::cmp::min(
                std::cmp::min(d[i - 1][j] + 1, d[i][j - 1] + 1),
                d[i - 1][j - 1] + cost,
            );
            if i > 1
                && j > 1
                && s1_chars[i - 1] == s2_chars[j - 2]
                && s1_chars[i - 2] == s2_chars[j - 1]
            {
                d[i][j] = std::cmp::min(d[i][j], d[i - 2][j - 2] + cost);
            }
        }
    }

    d[len1][len2]
}

/// Background loop that performs curl queries without blocking the main rendering thread.
fn worker_loop(
    rx: mpsc::Receiver<WorkerRequest>,
    tx: mpsc::Sender<WorkerResponse>,
    mut wake_writer: UnixStream,
) {
    while let Ok(req) = rx.recv() {
        let candidates = if req.is_next_word {
            fetch_online_next_words(&req.prev_word, &req.lang)
        } else {
            fetch_online_completions(&req.query, &req.lang)
        };

        if !candidates.is_empty() {
            let _ = tx.send(WorkerResponse {
                query: req.query,
                is_next_word: req.is_next_word,
                prev_word: req.prev_word,
                candidates,
            });
            // Wake up main thread's poll
            let _ = wake_writer.write_all(&[1]);
        }
    }
}

fn fetch_online_completions(prefix: &str, lang: &str) -> Vec<String> {
    if lang == "ru" {
        let mut words = fetch_yandex_completions(prefix);
        if words.is_empty() {
            words = fetch_google_completions(prefix, lang);
        }
        words
    } else {
        let mut words = fetch_google_completions(prefix, lang);
        if words.is_empty() {
            words = fetch_yandex_completions(prefix);
        }
        words
    }
}

fn fetch_yandex_completions(prefix: &str) -> Vec<String> {
    let url = format!(
        "https://suggest.yandex.ru/suggest-ya.cgi?part={}&v=4",
        url_encode(prefix)
    );
    let output = std::process::Command::new("curl")
        .args(["-s", "-A", "Mozilla/5.0", "--max-time", "2", &url])
        .output()
        .ok();

    let mut words = Vec::new();
    if let Some(out) = output
        && out.status.success()
        && let Ok(json_str) = String::from_utf8(out.stdout)
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str)
    {
        let prefix_lower = prefix.to_lowercase();
        // Check "continue" field if present: e.g. prefix="геолок", continue="ация" -> "геолокация"
        if let Some(obj) = data.get(2).and_then(|v| v.as_object())
            && let Some(cont) = obj.get("continue").and_then(|v| v.as_str())
        {
            let full = format!("{}{}", prefix_lower, cont.to_lowercase());
            let clean = full.trim_matches(|c: char| !c.is_alphabetic() && c != '-').to_string();
            if clean.chars().count() >= 2 && !words.contains(&clean) {
                words.push(clean);
            }
        }

        if let Some(array) = data.get(1).and_then(|v| v.as_array()) {
            for item in array.iter().filter_map(|v| v.as_str()) {
                for part in item.split_whitespace() {
                    let clean = part.trim_matches(|c: char| !c.is_alphabetic() && c != '-').to_lowercase();
                    if clean.chars().count() >= 2 {
                        let c_sub: String = clean.chars().take(prefix_lower.chars().count()).collect();
                        let dist = damerau_levenshtein(&prefix_lower, &c_sub);
                        if clean.starts_with(&prefix_lower) || dist <= 2 {
                            if !words.contains(&clean) {
                                words.push(clean);
                                if words.len() >= MAX_CANDIDATES {
                                    return words;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    words
}

fn fetch_google_completions(prefix: &str, lang: &str) -> Vec<String> {
    let url = format!(
        "https://suggestqueries.google.com/complete/search?client=firefox&q={}&hl={}",
        url_encode(prefix),
        lang
    );
    let output = std::process::Command::new("curl")
        .args(["-s", "-A", "Mozilla/5.0", "--max-time", "2", &url])
        .output()
        .ok();

    let mut words = Vec::new();
    if let Some(out) = output
        && out.status.success()
        && let Ok(json_str) = String::from_utf8(out.stdout)
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str)
        && let Some(array) = data.get(1).and_then(|v| v.as_array())
    {
        let prefix_lower = prefix.to_lowercase();
        for item in array.iter().filter_map(|v| v.as_str()) {
            for part in item.split_whitespace() {
                let clean = part.trim_matches(|c: char| !c.is_alphabetic() && c != '-').to_lowercase();
                if clean.chars().count() >= 2 {
                    let c_sub: String = clean.chars().take(prefix_lower.chars().count()).collect();
                    let dist = damerau_levenshtein(&prefix_lower, &c_sub);
                    if clean.starts_with(&prefix_lower) || dist <= 2 {
                        if !words.contains(&clean) {
                            words.push(clean);
                            if words.len() >= MAX_CANDIDATES {
                                return words;
                            }
                        }
                    }
                }
            }
        }
    }
    words
}

fn fetch_online_next_words(prev_word: &str, lang: &str) -> Vec<String> {
    if lang == "ru" {
        let mut words = fetch_yandex_next_words(prev_word);
        if words.is_empty() {
            words = fetch_google_next_words(prev_word, lang);
        }
        words
    } else {
        let mut words = fetch_google_next_words(prev_word, lang);
        if words.is_empty() {
            words = fetch_yandex_next_words(prev_word);
        }
        words
    }
}

fn fetch_yandex_next_words(prev_word: &str) -> Vec<String> {
    let query = format!("{prev_word} ");
    let url = format!(
        "https://suggest.yandex.ru/suggest-ya.cgi?part={}&v=4",
        url_encode(&query)
    );
    let output = std::process::Command::new("curl")
        .args(["-s", "-A", "Mozilla/5.0", "--max-time", "2", &url])
        .output()
        .ok();

    let mut words = Vec::new();
    if let Some(out) = output
        && out.status.success()
        && let Ok(json_str) = String::from_utf8(out.stdout)
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str)
    {
        let prev_lower = prev_word.to_lowercase();
        if let Some(obj) = data.get(2).and_then(|v| v.as_object())
            && let Some(cont) = obj.get("continue").and_then(|v| v.as_str())
        {
            let clean = cont.trim_matches(|c: char| !c.is_alphabetic() && c != '-').to_lowercase();
            if clean.chars().count() >= 2 && !words.contains(&clean) {
                words.push(clean);
            }
        }

        if let Some(array) = data.get(1).and_then(|v| v.as_array()) {
            for item in array.iter().filter_map(|v| v.as_str()) {
                let item_lower = item.to_lowercase();
                if item_lower.starts_with(&prev_lower) {
                    let rest = item_lower[prev_lower.len()..].trim();
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if !parts.is_empty() {
                        let next_w = parts[0].trim_matches(|c: char| !c.is_alphabetic() && c != '-').to_string();
                        if next_w.chars().count() >= 2 && !words.contains(&next_w) {
                            words.push(next_w);
                            if words.len() >= MAX_CANDIDATES {
                                return words;
                            }
                        }
                    }
                }
            }
        }
    }
    words
}

fn fetch_google_next_words(prev_word: &str, lang: &str) -> Vec<String> {
    let query = format!("{prev_word} ");
    let url = format!(
        "https://suggestqueries.google.com/complete/search?client=firefox&q={}&hl={}",
        url_encode(&query),
        lang
    );
    let output = std::process::Command::new("curl")
        .args(["-s", "-A", "Mozilla/5.0", "--max-time", "2", &url])
        .output()
        .ok();

    let mut words = Vec::new();
    if let Some(out) = output
        && out.status.success()
        && let Ok(json_str) = String::from_utf8(out.stdout)
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str)
        && let Some(array) = data.get(1).and_then(|v| v.as_array())
    {
        let prev_lower = prev_word.to_lowercase();
        for item in array.iter().filter_map(|v| v.as_str()) {
            let item_lower = item.to_lowercase();
            if item_lower.starts_with(&prev_lower) {
                let rest = item_lower[prev_lower.len()..].trim();
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if !parts.is_empty() {
                    let next_w = parts[0].trim_matches(|c: char| !c.is_alphabetic() && c != '-').to_string();
                    if next_w.chars().count() >= 2 && !words.contains(&next_w) {
                        words.push(next_w);
                        if words.len() >= MAX_CANDIDATES {
                            return words;
                        }
                    }
                }
            }
        }
    }
    words
}

fn url_encode(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.as_bytes() {
        match *byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            b' ' => encoded.push('+'),
            other => {
                encoded.push_str(&format!("%{:02X}", other));
            }
        }
    }
    encoded
}

/// Translate physical Key into character depending on layout (RU/EN) and Shift.
pub fn key_to_char(key: Key, is_ru: bool, is_shift: bool) -> Option<char> {
    if is_ru {
        let (lower, upper) = match key {
            Key::Q => ('й', 'Й'),
            Key::W => ('ц', 'Ц'),
            Key::E => ('у', 'У'),
            Key::R => ('к', 'К'),
            Key::T => ('е', 'Е'),
            Key::Y => ('н', 'Н'),
            Key::U => ('г', 'Г'),
            Key::I => ('ш', 'Ш'),
            Key::O => ('щ', 'Щ'),
            Key::P => ('з', 'З'),
            Key::LeftBrace => ('х', 'Х'),
            Key::RightBrace => ('ъ', 'Ъ'),
            Key::A => ('ф', 'Ф'),
            Key::S => ('ы', 'Ы'),
            Key::D => ('в', 'В'),
            Key::F => ('а', 'А'),
            Key::G => ('п', 'П'),
            Key::H => ('р', 'Р'),
            Key::J => ('о', 'О'),
            Key::K => ('л', 'Л'),
            Key::L => ('д', 'Д'),
            Key::Semicolon => ('ж', 'Ж'),
            Key::Apostrophe => ('э', 'Э'),
            Key::Z => ('я', 'Я'),
            Key::X => ('ч', 'Ч'),
            Key::C => ('с', 'С'),
            Key::V => ('м', 'М'),
            Key::B => ('и', 'И'),
            Key::N => ('т', 'Т'),
            Key::M => ('ь', 'Ь'),
            Key::Comma => ('б', 'Б'),
            Key::Dot => ('ю', 'Ю'),
            Key::Minus => ('-', '_'),
            Key::Equal => ('=', '+'),
            Key::Slash => ('.', ','),
            Key::Backslash => ('ё', 'Ё'),
            Key::Grave => ('<', '>'),
            Key::Num1 => ('1', '!'),
            Key::Num2 => ('2', '"'),
            Key::Num3 => ('3', '№'),
            Key::Num4 => ('4', '%'),
            Key::Num5 => ('5', ':'),
            Key::Num6 => ('6', ','),
            Key::Num7 => ('7', '.'),
            Key::Num8 => ('8', ';'),
            Key::Num9 => ('9', '('),
            Key::Num0 => ('0', ')'),
            _ => return None,
        };
        Some(if is_shift { upper } else { lower })
    } else {
        let (lower, upper) = match key {
            Key::A => ('a', 'A'),
            Key::B => ('b', 'B'),
            Key::C => ('c', 'C'),
            Key::D => ('d', 'D'),
            Key::E => ('e', 'E'),
            Key::F => ('f', 'F'),
            Key::G => ('g', 'G'),
            Key::H => ('h', 'H'),
            Key::I => ('i', 'I'),
            Key::J => ('j', 'J'),
            Key::K => ('k', 'K'),
            Key::L => ('l', 'L'),
            Key::M => ('m', 'M'),
            Key::N => ('n', 'N'),
            Key::O => ('o', 'O'),
            Key::P => ('p', 'P'),
            Key::Q => ('q', 'Q'),
            Key::R => ('r', 'R'),
            Key::S => ('s', 'S'),
            Key::T => ('t', 'T'),
            Key::U => ('u', 'U'),
            Key::V => ('v', 'V'),
            Key::W => ('w', 'W'),
            Key::X => ('x', 'X'),
            Key::Y => ('y', 'Y'),
            Key::Z => ('z', 'Z'),
            Key::Num1 => ('1', '!'),
            Key::Num2 => ('2', '@'),
            Key::Num3 => ('3', '#'),
            Key::Num4 => ('4', '$'),
            Key::Num5 => ('5', '%'),
            Key::Num6 => ('6', '^'),
            Key::Num7 => ('7', '&'),
            Key::Num8 => ('8', '*'),
            Key::Num9 => ('9', '('),
            Key::Num0 => ('0', ')'),
            Key::Minus => ('-', '_'),
            Key::Equal => ('=', '+'),
            Key::Grave => ('`', '~'),
            Key::LeftBrace => ('[', '{'),
            Key::RightBrace => (']', '}'),
            Key::Backslash => ('\\', '|'),
            Key::Semicolon => (';', ':'),
            Key::Apostrophe => ('\'', '"'),
            Key::Comma => (',', '<'),
            Key::Dot => ('.', '>'),
            Key::Slash => ('/', '?'),
            _ => return None,
        };
        Some(if is_shift { upper } else { lower })
    }
}

/// Convert Linux evdev key code to input_linux::Key for typing/control keys.
pub fn code_to_key(code: u32) -> Option<Key> {
    match code {
        1 => Some(Key::Esc),
        2 => Some(Key::Num1),
        3 => Some(Key::Num2),
        4 => Some(Key::Num3),
        5 => Some(Key::Num4),
        6 => Some(Key::Num5),
        7 => Some(Key::Num6),
        8 => Some(Key::Num7),
        9 => Some(Key::Num8),
        10 => Some(Key::Num9),
        11 => Some(Key::Num0),
        12 => Some(Key::Minus),
        13 => Some(Key::Equal),
        14 => Some(Key::Backspace),
        15 => Some(Key::Tab),
        16 => Some(Key::Q),
        17 => Some(Key::W),
        18 => Some(Key::E),
        19 => Some(Key::R),
        20 => Some(Key::T),
        21 => Some(Key::Y),
        22 => Some(Key::U),
        23 => Some(Key::I),
        24 => Some(Key::O),
        25 => Some(Key::P),
        26 => Some(Key::LeftBrace),
        27 => Some(Key::RightBrace),
        28 => Some(Key::Enter),
        30 => Some(Key::A),
        31 => Some(Key::S),
        32 => Some(Key::D),
        33 => Some(Key::F),
        34 => Some(Key::G),
        35 => Some(Key::H),
        36 => Some(Key::J),
        37 => Some(Key::K),
        38 => Some(Key::L),
        39 => Some(Key::Semicolon),
        40 => Some(Key::Apostrophe),
        41 => Some(Key::Grave),
        43 => Some(Key::Backslash),
        44 => Some(Key::Z),
        45 => Some(Key::X),
        46 => Some(Key::C),
        47 => Some(Key::V),
        48 => Some(Key::B),
        49 => Some(Key::N),
        50 => Some(Key::M),
        51 => Some(Key::Comma),
        52 => Some(Key::Dot),
        53 => Some(Key::Slash),
        57 => Some(Key::Space),
        58 => Some(Key::CapsLock),
        102 => Some(Key::Home),
        103 => Some(Key::Up),
        104 => Some(Key::PageUp),
        105 => Some(Key::Left),
        106 => Some(Key::Right),
        107 => Some(Key::End),
        108 => Some(Key::Down),
        109 => Some(Key::PageDown),
        111 => Some(Key::Delete),
        _ => None,
    }
}

pub fn is_word_char(ch: char) -> bool {
    ch.is_alphabetic() || ch == '-' || ch == '\''
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordContext {
    pub current_prefix: String,
    pub word_start: usize,
    pub word_end: usize,
    pub previous_word: Option<String>,
}

/// Extract the word prefix, word bounds, and preceding word relative to the cursor position.
pub fn extract_context_at_cursor(chars: &[char], cursor_pos: usize) -> WordContext {
    let cursor_pos = cursor_pos.min(chars.len());

    let (word_start, word_end, is_in_word) = if cursor_pos > 0 && is_word_char(chars[cursor_pos - 1]) {
        let mut start = cursor_pos;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = cursor_pos;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        (start, end, true)
    } else if cursor_pos < chars.len() && is_word_char(chars[cursor_pos]) {
        let start = cursor_pos;
        let mut end = cursor_pos;
        while end < chars.len() && is_word_char(chars[end]) {
            end += 1;
        }
        (start, end, true)
    } else {
        (cursor_pos, cursor_pos, false)
    };

    let current_prefix = if is_in_word {
        chars[word_start..cursor_pos].iter().collect::<String>()
    } else {
        String::new()
    };

    let mut p = word_start;
    while p > 0 && !is_word_char(chars[p - 1]) {
        p -= 1;
    }
    let prev_end = p;
    while p > 0 && is_word_char(chars[p - 1]) {
        p -= 1;
    }
    let prev_start = p;
    let previous_word = if prev_end > prev_start {
        Some(chars[prev_start..prev_end].iter().collect::<String>())
    } else {
        None
    };

    WordContext {
        current_prefix,
        word_start,
        word_end,
        previous_word,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predict_local_words() {
        let (mut predictor, _) = Predictor::new();

        // Prefixes under 3 letters must return empty suggestions
        assert!(predictor.suggest("п", None, "RU").is_empty());
        assert!(predictor.suggest("пр", None, "RU").is_empty());
        assert!(predictor.suggest("p", None, "EN").is_empty());
        assert!(predictor.suggest("pr", None, "EN").is_empty());

        let ru_res = predictor.suggest("прив", None, "RU");
        assert!(!ru_res.is_empty());
        assert!(ru_res.contains(&"привет".to_string()));

        let geo_res = predictor.suggest("геолок", None, "RU");
        assert!(!geo_res.is_empty());
        assert!(geo_res.contains(&"геолокация".to_string()));

        let func_res = predictor.suggest("функц", None, "RU");
        assert!(!func_res.is_empty());
        assert!(func_res.contains(&"функция".to_string()));

        let en_res = predictor.suggest("prog", None, "EN");
        assert!(!en_res.is_empty());
        assert!(en_res.contains(&"program".to_string()));
    }

    #[test]
    fn test_predict_typo_fuzzy_match() {
        let (mut predictor, _) = Predictor::new();
        // Typo: 'функцы' -> should suggest 'функция' or 'функции'
        let func_typo = predictor.suggest("функцы", None, "RU");
        assert!(!func_typo.is_empty());
        assert!(func_typo.iter().any(|w| w.starts_with("функци")));

        // Typo: 'геолокацыя' -> should suggest 'геолокация'
        let geo_typo = predictor.suggest("геолокацыя", None, "RU");
        assert!(!geo_typo.is_empty());
        assert!(geo_typo.contains(&"геолокация".to_string()));

        // Typo: 'функиця' -> transposition typo, should suggest 'функция'
        let trans_typo = predictor.suggest("функиця", None, "RU");
        assert!(!trans_typo.is_empty());
        assert!(trans_typo.contains(&"функция".to_string()));

        // Typo: 'програма' -> should suggest 'программа'
        let prog_typo = predictor.suggest("програма", None, "RU");
        assert!(!prog_typo.is_empty());
        assert!(prog_typo.contains(&"программа".to_string()));

        // Typo: 'functin' -> should suggest 'function'
        let en_typo = predictor.suggest("functin", None, "EN");
        assert!(!en_typo.is_empty());
        assert!(en_typo.contains(&"function".to_string()));
    }

    #[test]
    fn test_damerau_levenshtein() {
        assert_eq!(damerau_levenshtein("функцы", "функци"), 1);
        assert_eq!(damerau_levenshtein("геолокацыя", "геолокация"), 1);
        assert_eq!(damerau_levenshtein("функиця", "функция"), 1); // transposition
        assert_eq!(damerau_levenshtein("програма", "программа"), 1); // insertion
        assert_eq!(damerau_levenshtein("same", "same"), 0);
    }

    #[test]
    fn test_predict_next_words() {
        let (mut predictor, _) = Predictor::new();
        let ru_next = predictor.suggest("", Some("привет"), "RU");
        assert!(!ru_next.is_empty());
        assert!(ru_next.contains(&"как дела".to_string()) || ru_next.contains(&"всем".to_string()));

        let en_next = predictor.suggest("", Some("how"), "EN");
        assert!(!en_next.is_empty());
        assert!(en_next.contains(&"are you".to_string()) || en_next.contains(&"to".to_string()));

        // Sentence start capitalized next words
        let sentence_next = predictor.suggest("", Some("конец."), "RU");
        assert!(!sentence_next.is_empty());
        let first_char = sentence_next[0].chars().next().unwrap();
        assert!(first_char.is_uppercase());
    }

    #[test]
    fn test_predict_casing_adaptation() {
        let (mut predictor, _) = Predictor::new();
        let title_res = predictor.suggest("Прив", None, "RU");
        assert!(!title_res.is_empty());
        assert!(title_res[0].starts_with('П'));

        let caps_res = predictor.suggest("ФУНКЦ", None, "RU");
        assert!(!caps_res.is_empty());
        assert_eq!(caps_res[0], caps_res[0].to_uppercase());
    }

    #[test]
    fn test_key_to_char_ru_en() {
        assert_eq!(key_to_char(Key::F, true, false), Some('а'));
        assert_eq!(key_to_char(Key::F, true, true), Some('А'));
        assert_eq!(key_to_char(Key::F, false, false), Some('f'));
        assert_eq!(key_to_char(Key::F, false, true), Some('F'));
        assert_eq!(key_to_char(Key::Q, true, false), Some('й'));
        assert_eq!(key_to_char(Key::Q, false, false), Some('q'));
        // Symbols & punctuation (Russian Macintosh)
        assert_eq!(key_to_char(Key::Num5, true, true), Some(':'));
        assert_eq!(key_to_char(Key::Num6, true, true), Some(','));
        assert_eq!(key_to_char(Key::Num7, true, true), Some('.'));
        assert_eq!(key_to_char(Key::Num8, true, true), Some(';'));
        assert_eq!(key_to_char(Key::Num9, true, true), Some('('));
        assert_eq!(key_to_char(Key::Num0, true, true), Some(')'));
        assert_eq!(key_to_char(Key::Backslash, true, false), Some('ё'));
        assert_eq!(key_to_char(Key::Equal, true, false), Some('='));
        assert_eq!(key_to_char(Key::Equal, true, true), Some('+'));
        assert_eq!(key_to_char(Key::Num9, false, true), Some('('));
        assert_eq!(key_to_char(Key::Num0, false, true), Some(')'));
        assert_eq!(key_to_char(Key::Dot, false, false), Some('.'));
        assert_eq!(key_to_char(Key::Comma, false, false), Some(','));
    }

    #[test]
    fn test_code_to_key() {
        assert_eq!(code_to_key(30), Some(Key::A));
        assert_eq!(code_to_key(57), Some(Key::Space));
        assert_eq!(code_to_key(14), Some(Key::Backspace));
        assert_eq!(code_to_key(1), Some(Key::Esc));
        assert_eq!(code_to_key(105), Some(Key::Left));
        assert_eq!(code_to_key(106), Some(Key::Right));
        assert_eq!(code_to_key(102), Some(Key::Home));
        assert_eq!(code_to_key(107), Some(Key::End));
        assert_eq!(code_to_key(111), Some(Key::Delete));
    }

    #[test]
    fn test_extract_context_at_cursor() {
        // "Привет, геолокация"
        let line: Vec<char> = "Привет, геолокация".chars().collect();

        // Cursor at the very end
        let ctx = extract_context_at_cursor(&line, line.len());
        assert_eq!(ctx.current_prefix, "геолокация");
        assert_eq!(ctx.word_start, 8);
        assert_eq!(ctx.word_end, 18);
        assert_eq!(ctx.previous_word, Some("Привет".to_string()));

        // Cursor in the middle: after 'к' in 'геолок' (index 14)
        let ctx = extract_context_at_cursor(&line, 14);
        assert_eq!(ctx.current_prefix, "геолок");
        assert_eq!(ctx.word_start, 8);
        assert_eq!(ctx.word_end, 18);
        assert_eq!(ctx.previous_word, Some("Привет".to_string()));

        // "Привет, " with cursor after space
        let line2: Vec<char> = "Привет, ".chars().collect();
        let ctx2 = extract_context_at_cursor(&line2, line2.len());
        assert_eq!(ctx2.current_prefix, "");
        assert_eq!(ctx2.previous_word, Some("Привет".to_string()));

        // Empty line
        let empty: Vec<char> = Vec::new();
        let ctx_empty = extract_context_at_cursor(&empty, 0);
        assert_eq!(ctx_empty.current_prefix, "");
        assert_eq!(ctx_empty.previous_word, None);
    }
}
