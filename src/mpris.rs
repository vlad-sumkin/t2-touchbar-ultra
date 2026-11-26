// SPDX-License-Identifier: GPL-3.0-or-later

//! Media players over MPRIS: seeking in the active player (desktops treat
//! the rewind and fast forward keys inconsistently) and track changes for
//! the dark bar.

use std::{
    collections::HashMap,
    io::Write,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use zbus::blocking::{Connection, MessageIterator, connection::Builder};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{MatchRule, message::Type};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
/// A hung player must not stall the Touch Bar.
const CALL_TIMEOUT: Duration = Duration::from_millis(200);

pub type PlaybackSample = (u64, Option<u64>, bool, std::time::Instant);

#[derive(Clone)]
pub struct PositionSampler {
    shared: Arc<Mutex<Option<PlaybackSample>>>,
    active: Arc<AtomicBool>,
}

impl PositionSampler {
    pub fn spawn() -> Result<Self> {
        let shared = Arc::new(Mutex::new(None));
        let active = Arc::new(AtomicBool::new(false));
        let shared_clone = Arc::clone(&shared);
        let active_clone = Arc::clone(&active);

        thread::Builder::new()
            .name("mpris-pos".to_owned())
            .spawn(move || {
                let mut connection: Option<Connection> = None;
                let mut query_state = PositionQueryState::default();
                while !crate::STOP.load(Ordering::Relaxed) {
                    if !active_clone.load(Ordering::Relaxed) {
                        query_state.reset();
                        thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                    if connection.is_none() {
                        connection = Builder::session()
                            .and_then(|b| b.method_timeout(Duration::from_millis(150)).build())
                            .ok();
                    }
                    if let Some(conn) = &connection {
                        if let Some((pos_us, len_us, is_playing)) = query_pos_and_len(conn, &mut query_state) {
                            let instant = std::time::Instant::now();
                            if let Ok(mut lock) = shared_clone.lock() {
                                *lock = Some((pos_us, len_us, is_playing, instant));
                            }
                        }
                    } else {
                        thread::sleep(Duration::from_millis(1000));
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            })?;

        Ok(Self { shared, active })
    }

    pub fn set_active(&self, is_active: bool) {
        self.active.store(is_active, Ordering::Relaxed);
    }

    pub fn get_sample(&self) -> Option<PlaybackSample> {
        self.shared.lock().ok()?.take()
    }
}

impl Default for PositionSampler {
    fn default() -> Self {
        Self::spawn().unwrap_or_else(|_| Self {
            shared: Arc::new(Mutex::new(None)),
            active: Arc::new(AtomicBool::new(false)),
        })
    }
}

#[derive(Default)]
pub struct Mpris {
    connection: Option<Connection>,
    pos_sampler: PositionSampler,
}

impl Mpris {
    pub fn set_playback_active(&self, active: bool) {
        self.pos_sampler.set_active(active);
    }

    pub fn get_playback_sample(&self) -> Option<PlaybackSample> {
        self.pos_sampler.get_sample()
    }
    /// Picks the player a seek should go to: a playing one first, then a
    /// paused one, then any.
    pub fn active_player(&mut self) -> Option<String> {
        let connection = self.connection().ok()?;
        let names: Vec<String> = connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        names
            .into_iter()
            .filter(|name| name.starts_with(PREFIX))
            .map(|name| {
                let rank = match playback_status(connection, &name).as_deref() {
                    Some("Playing") => 0,
                    Some("Paused") => 1,
                    _ => 2,
                };
                (rank, name)
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, name)| name)
    }

    pub fn seek(&mut self, player: &str, offset: Duration, forward: bool) -> Result<()> {
        let micros = offset.as_micros() as i64;
        let micros = if forward { micros } else { -micros };
        self.connection()?
            .call_method(Some(player), PATH, Some(PLAYER), "Seek", &(micros,))?;
        Ok(())
    }

    pub fn set_position_micros(&mut self, target_us: u64) -> Result<()> {
        let Some(player) = self.active_player() else {
            return Ok(());
        };
        let connection = self.connection()?;
        let mut success = false;

        let meta_reply = connection.call_method(
            Some(player.as_str()),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER, "Metadata"),
        );
        if let Ok(reply) = meta_reply
            && let Ok(meta_val) = reply.body().deserialize::<OwnedValue>()
        {
            if let Some(trackid) = parse_trackid(&meta_val) {
                if connection
                    .call_method(
                        Some(player.as_str()),
                        PATH,
                        Some(PLAYER),
                        "SetPosition",
                        &(trackid, target_us as i64),
                    )
                    .is_ok()
                {
                    success = true;
                }
            }
        }

        if !success {
            let pos_val: Result<OwnedValue, _> = connection
                .call_method(
                    Some(player.as_str()),
                    PATH,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(PLAYER, "Position"),
                )
                .and_then(|r| r.body().deserialize());
            if let Ok(pos_val) = pos_val {
                let curr_pos = match unwrap_variant(&pos_val) {
                    Value::I64(v) if *v >= 0 => *v as u64,
                    Value::U64(v) => *v,
                    _ => 0,
                };
                let diff = target_us as i64 - curr_pos as i64;
                let _ = connection.call_method(
                    Some(player.as_str()),
                    PATH,
                    Some(PLAYER),
                    "Seek",
                    &(diff,),
                );
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn playback_position_and_length(&mut self) -> Option<(u64, u64)> {
        let player = self.active_player()?;
        let connection = self.connection().ok()?;
        let pos_val: OwnedValue = connection
            .call_method(
                Some(player.as_str()),
                PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(PLAYER, "Position"),
            )
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        let pos_us = match unwrap_variant(&pos_val) {
            Value::I64(v) if *v >= 0 => *v as u64,
            Value::U64(v) => *v,
            _ => 0,
        };

        let meta_val: OwnedValue = connection
            .call_method(
                Some(player.as_str()),
                PATH,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(PLAYER, "Metadata"),
            )
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        let len_us = parse_length(&meta_val)?;
        Some((pos_us, len_us))
    }

    #[allow(dead_code)]
    pub fn is_playing(&mut self) -> bool {
        let Ok(connection) = self.connection() else { return false };
        let Ok(reply) = connection.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        ) else { return false };
        let Ok(names) = reply.body().deserialize::<Vec<String>>() else { return false };
        names
            .into_iter()
            .filter(|name| name.starts_with(PREFIX))
            .any(|name| playback_status(connection, &name).as_deref() == Some("Playing"))
    }

    fn connection(&mut self) -> Result<&Connection> {
        if self.connection.is_none() {
            self.connection = Some(Builder::session()?.method_timeout(CALL_TIMEOUT).build()?);
        }
        Ok(self
            .connection
            .as_ref()
            .expect("connection was just created"))
    }
}

fn playback_status(connection: &Connection, name: &str) -> Option<String> {
    let value: OwnedValue = connection
        .call_method(
            Some(name),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER, "PlaybackStatus"),
        )
        .ok()?
        .body()
        .deserialize()
        .ok()?;
    String::try_from(value).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub artist: String,
    pub title: String,
    pub length_us: Option<u64>,
}

/// Reports a track once it is playing and differs from the last reported
/// one. When playback pauses or stops, reports None so status can clear.
pub fn watch_tracks() -> Result<(Receiver<Option<Track>>, UnixStream)> {
    let (sender, receiver) = mpsc::channel();
    let (mut wake_writer, wake_reader) = UnixStream::pair()?;
    thread::Builder::new()
        .name("mpris-tracks".to_owned())
        .spawn(move || {
            let mut reported = false;
            loop {
                if let Err(error) = listen_tracks(&sender, &mut wake_writer) {
                    if !reported {
                        eprintln!("t2-touchbar-ultra: track listener: {error:#}");
                        reported = true;
                    }
                    thread::sleep(Duration::from_secs(30));
                }
            }
        })?;
    Ok((receiver, wake_reader))
}

#[derive(Default)]
struct PlayerState {
    playing: bool,
    track: Option<Track>,
}

fn listen_tracks(sender: &Sender<Option<Track>>, wake: &mut UnixStream) -> Result<()> {
    let connection = Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
        .context("connect session bus")?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .path(PATH)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .arg(0, PLAYER)?
        .build();
    let iterator = MessageIterator::for_match_rule(rule, &connection, None)?;
    let mut players: HashMap<String, PlayerState> = HashMap::new();
    let mut last: Option<Track> = None;
    for message in iterator {
        let message = message?;
        let Some(name) = message.header().sender().map(ToString::to_string) else {
            continue;
        };
        let Ok((_, changed, _)) = message
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            continue;
        };
        let known = players.contains_key(&name);
        let state = players.entry(name.clone()).or_default();
        if let Some(status) = changed.get("PlaybackStatus") {
            state.playing = matches!(&**status, Value::Str(status) if status.as_str() == "Playing");
        } else if !known {
            state.playing = playback_status(&connection, &name).as_deref() == Some("Playing");
        }
        if let Some(metadata) = changed.get("Metadata") {
            state.track = parse_track(metadata);
        }

        let current = players
            .values()
            .find(|p| p.playing && p.track.is_some())
            .and_then(|p| p.track.clone());

        if last != current {
            last = current.clone();
            if sender.send(current).is_err() {
                return Ok(());
            }
            let _ = wake.write_all(&[1]);
        }
    }
    Err(anyhow::anyhow!("session bus iterator ended"))
}

fn parse_track(metadata: &OwnedValue) -> Option<Track> {
    let Value::Dict(dict) = unwrap_variant(metadata) else {
        return None;
    };
    let mut artist = String::new();
    let mut title = String::new();
    let mut length_us = None;
    for (key, value) in dict.iter() {
        let Value::Str(key) = unwrap_variant(key) else {
            continue;
        };
        match (key.as_str(), unwrap_variant(value)) {
            ("xesam:title", Value::Str(value)) => title = value.to_string(),
            ("xesam:artist", Value::Array(values)) => {
                artist = values
                    .iter()
                    .filter_map(|value| match unwrap_variant(value) {
                        Value::Str(name) => Some(name.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
            }
            // Some players send a single string instead of the list.
            ("xesam:artist", Value::Str(value)) => artist = value.to_string(),
            ("mpris:length", Value::I64(value)) if *value > 0 => length_us = Some(*value as u64),
            ("mpris:length", Value::U64(value)) if *value > 0 => length_us = Some(*value),
            _ => {}
        }
    }
    let (artist, title) = (
        crate::renderer::clean_display_text(&artist).trim().to_owned(),
        crate::renderer::clean_display_text(&title).trim().to_owned(),
    );
    (!title.is_empty()).then_some(Track { artist, title, length_us })
}

pub fn parse_length(metadata: &OwnedValue) -> Option<u64> {
    let Value::Dict(dict) = unwrap_variant(metadata) else {
        return None;
    };
    for (key, value) in dict.iter() {
        let Value::Str(key) = unwrap_variant(key) else {
            continue;
        };
        if key.as_str() == "mpris:length" {
            match unwrap_variant(value) {
                Value::I64(v) if *v > 0 => return Some(*v as u64),
                Value::U64(v) if *v > 0 => return Some(*v),
                _ => {}
            }
        }
    }
    None
}

pub fn parse_trackid(metadata: &OwnedValue) -> Option<ObjectPath<'_>> {
    let Value::Dict(dict) = unwrap_variant(metadata) else {
        return None;
    };
    for (key, value) in dict.iter() {
        let Value::Str(key) = unwrap_variant(key) else {
            continue;
        };
        if key.as_str() == "mpris:trackid" {
            match unwrap_variant(value) {
                Value::ObjectPath(path) => return Some(path.clone()),
                _ => {}
            }
        }
    }
    None
}

#[allow(dead_code)]
pub fn format_duration(seconds: u64) -> String {
    let m = seconds / 60;
    let s = seconds % 60;
    if m >= 60 {
        let h = m / 60;
        let rem_m = m % 60;
        format!("{h}:{rem_m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[derive(Default)]
struct PositionQueryState {
    cached_player: Option<String>,
    cached_len: Option<u64>,
    last_pos: Option<u64>,
}

impl PositionQueryState {
    fn reset(&mut self) {
        self.cached_player = None;
        self.cached_len = None;
        self.last_pos = None;
    }
}

fn query_pos_and_len(conn: &Connection, state: &mut PositionQueryState) -> Option<(u64, Option<u64>, bool)> {
    if let Some(player) = &state.cached_player {
        let pos_reply = conn.call_method(
            Some(player.as_str()),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER, "Position"),
        );
        if let Ok(pos_reply) = pos_reply
            && let Ok(pos_val) = pos_reply.body().deserialize::<OwnedValue>()
        {
            let pos_us = match unwrap_variant(&pos_val) {
                Value::I64(v) if *v >= 0 => *v as u64,
                Value::U64(v) => *v,
                _ => 0,
            };

            if let Some(prev) = state.last_pos
                && pos_us < prev.saturating_sub(1_500_000)
            {
                state.cached_len = None;
            }

            let is_playing = if let Some(prev) = state.last_pos {
                if pos_us > prev {
                    true
                } else {
                    let status = playback_status(conn, player.as_str());
                    status.as_deref() == Some("Playing")
                }
            } else {
                let status = playback_status(conn, player.as_str());
                status.as_deref() == Some("Playing")
            };
            state.last_pos = Some(pos_us);

            let len_us = if let Some(cached) = state.cached_len {
                Some(cached)
            } else {
                let meta_reply = conn.call_method(
                    Some(player.as_str()),
                    PATH,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(PLAYER, "Metadata"),
                );
                if let Ok(meta_reply) = meta_reply
                    && let Ok(meta_val) = meta_reply.body().deserialize::<OwnedValue>()
                {
                    let parsed = parse_length(&meta_val);
                    state.cached_len = parsed;
                    parsed
                } else {
                    None
                }
            };

            return Some((pos_us, len_us, is_playing));
        } else {
            state.reset();
        }
    }

    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .ok()?;
    let names: Vec<String> = reply.body().deserialize().ok()?;
    let mut best_player = None;
    for name in names.into_iter().filter(|n| n.starts_with(PREFIX)) {
        let status = playback_status(conn, &name);
        if status.as_deref() == Some("Playing") {
            best_player = Some(name);
            break;
        } else if best_player.is_none() && status.as_deref() == Some("Paused") {
            best_player = Some(name);
        }
    }
    let player = best_player?;
    let status = playback_status(conn, player.as_str());
    let is_playing = status.as_deref() == Some("Playing");

    let pos_reply = conn
        .call_method(
            Some(player.as_str()),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER, "Position"),
        )
        .ok()?;
    let pos_val: OwnedValue = pos_reply.body().deserialize().ok()?;
    let pos_us = match unwrap_variant(&pos_val) {
        Value::I64(v) if *v >= 0 => *v as u64,
        Value::U64(v) => *v,
        _ => 0,
    };

    let meta_reply = conn
        .call_method(
            Some(player.as_str()),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER, "Metadata"),
        )
        .ok();
    let len_us = if let Some(meta_reply) = meta_reply
        && let Ok(meta_val) = meta_reply.body().deserialize::<OwnedValue>()
    {
        parse_length(&meta_val)
    } else {
        None
    };

    state.cached_player = Some(player);
    state.cached_len = len_us;
    state.last_pos = Some(pos_us);
    Some((pos_us, len_us, is_playing))
}

fn unwrap_variant<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    match value {
        Value::Value(inner) => unwrap_variant(inner),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(entries: Vec<(&str, Value<'static>)>) -> OwnedValue {
        let map: HashMap<String, Value<'static>> = entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        OwnedValue::try_from(Value::from(map)).unwrap()
    }

    #[test]
    fn artist_list_and_title_are_read() {
        let value = metadata(vec![
            ("xesam:title", Value::from("Duality")),
            (
                "xesam:artist",
                Value::from(vec!["Slipknot", "Corey Taylor"]),
            ),
            ("mpris:length", Value::from(252_000_000i64)),
        ]);
        assert_eq!(
            parse_track(&value),
            Some(Track {
                artist: "Slipknot, Corey Taylor".to_owned(),
                title: "Duality".to_owned(),
                length_us: Some(252_000_000),
            })
        );
    }

    #[test]
    fn missing_title_is_no_track() {
        let value = metadata(vec![("xesam:artist", Value::from("Heino"))]);
        assert_eq!(parse_track(&value), None);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(9), "0:09");
        assert_eq!(format_duration(83), "1:23");
        assert_eq!(format_duration(600), "10:00");
        assert_eq!(format_duration(3665), "1:01:05");
    }
}
