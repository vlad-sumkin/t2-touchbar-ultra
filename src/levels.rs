// SPDX-License-Identifier: GPL-3.0-or-later

//! Current volume and display brightness for the swipe feedback. The daemon
//! only sends keys. The desktop applies them, so these are read back.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, OnceLock,
    },
    thread,
    time::Duration,
};

use anyhow::Result;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelKind {
    Volume,
    Brightness,
}

#[derive(Clone, Copy, Debug)]
pub enum LevelJob {
    Read(LevelKind),
    Adjust(LevelKind, i32),
}

/// Reads levels on a worker thread: `wpctl` takes 10-20 ms, which would
/// otherwise stall touch handling in the middle of a swipe.
pub struct Reader {
    requests: Sender<LevelJob>,
    results: Receiver<(LevelKind, Level)>,
    running: Arc<AtomicBool>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Reader {
    /// Returns the reader and a socket that becomes readable with results.
    pub fn spawn() -> Result<(Self, UnixStream)> {
        let (requests, jobs) = mpsc::channel::<LevelJob>();
        let (done, results) = mpsc::channel();
        let (mut wake_writer, wake_reader) = UnixStream::pair()?;
        let running = Arc::new(AtomicBool::new(true));

        // Worker thread that queries and returns levels
        let done_worker = done.clone();
        thread::Builder::new()
            .name("level-reader".to_owned())
            .spawn(move || {
                // Initial read so levels are immediately populated on startup
                let init_vol = volume();
                let init_brt = brightness();
                let _ = done_worker.send((LevelKind::Volume, init_vol));
                let _ = done_worker.send((LevelKind::Brightness, init_brt));
                let _ = wake_writer.write_all(&[1]);

                while let Ok(mut job) = jobs.recv() {
                    match job {
                        LevelJob::Read(mut kind) => {
                            // Only the newest request matters.
                            while let Ok(newer) = jobs.try_recv() {
                                match newer {
                                    LevelJob::Read(k) => kind = k,
                                    LevelJob::Adjust(k, s) => {
                                        job = LevelJob::Adjust(k, s);
                                        break;
                                    }
                                }
                            }
                            if matches!(job, LevelJob::Read(_)) {
                                let level = match kind {
                                    LevelKind::Volume => volume(),
                                    LevelKind::Brightness => brightness(),
                                };
                                if done_worker.send((kind, level)).is_err() {
                                    return;
                                }
                                let _ = wake_writer.write_all(&[1]);
                            }
                        }
                        LevelJob::Adjust(kind, mut steps) => {
                            // Drain and coalesce pending adjustments of the same kind
                            while let Ok(newer) = jobs.try_recv() {
                                match newer {
                                    LevelJob::Adjust(k, s) if k == kind => {
                                        steps += s;
                                    }
                                    LevelJob::Adjust(k, s) => {
                                        perform_adjust(kind, steps);
                                        let level = match kind {
                                            LevelKind::Volume => volume(),
                                            LevelKind::Brightness => brightness(),
                                        };
                                        let _ = done_worker.send((kind, level));
                                        let _ = wake_writer.write_all(&[1]);
                                        job = LevelJob::Adjust(k, s);
                                        break;
                                    }
                                    LevelJob::Read(_) => {}
                                }
                            }
                            if let LevelJob::Adjust(k, s) = job {
                                perform_adjust(k, s);
                                let level = match k {
                                    LevelKind::Volume => volume(),
                                    LevelKind::Brightness => brightness(),
                                };
                                if done_worker.send((k, level)).is_err() {
                                    return;
                                }
                                let _ = wake_writer.write_all(&[1]);
                            }
                        }
                    }
                }
            })?;

        // Background monitor for Brightness changes
        let req_brt = requests.clone();
        let running_brt = Arc::clone(&running);
        thread::Builder::new()
            .name("brightness-watcher".to_owned())
            .spawn(move || {
                let mut last_percent = brightness().percent;
                while running_brt.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(500));
                    let current = brightness().percent;
                    if current != last_percent {
                        last_percent = current;
                        if req_brt.send(LevelJob::Read(LevelKind::Brightness)).is_err() {
                            break;
                        }
                    }
                }
            })?;

        // Background monitor for Volume changes via pactl subscribe (event-driven, 0% CPU)
        let req_vol = requests.clone();
        let running_vol = Arc::clone(&running);
        thread::Builder::new()
            .name("volume-watcher".to_owned())
            .spawn(move || {
                while running_vol.load(Ordering::Relaxed) {
                    let mut child = match Command::new("pactl")
                        .arg("subscribe")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(_) => {
                            // If pactl is not available, fallback to slow poll
                            thread::sleep(Duration::from_secs(2));
                            let _ = req_vol.send(LevelJob::Read(LevelKind::Volume));
                            continue;
                        }
                    };

                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if !running_vol.load(Ordering::Relaxed) {
                                break;
                            }
                            if (line.contains("sink") || line.contains("server"))
                                && req_vol.send(LevelJob::Read(LevelKind::Volume)).is_err()
                            {
                                break;
                            }
                        }
                    }

                    let _ = child.kill();
                    let _ = child.wait();
                    if !running_vol.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            })?;

        Ok((Self { requests, results, running }, wake_reader))
    }

    pub fn request(&self, kind: LevelKind) {
        let _ = self.requests.send(LevelJob::Read(kind));
    }

    pub fn adjust(&self, kind: LevelKind, steps: i32) {
        let _ = self.requests.send(LevelJob::Adjust(kind, steps));
    }

    pub fn results(&self) -> impl Iterator<Item = (LevelKind, Level)> + '_ {
        self.results.try_iter()
    }
}

fn perform_adjust(kind: LevelKind, steps: i32) {
    if steps == 0 {
        return;
    }
    let abs_steps = steps.unsigned_abs();
    match kind {
        LevelKind::Volume => {
            let action = if steps > 0 { "volume-up" } else { "volume-down" };
            let ok = Command::new("noctalia")
                .args(["msg", action, &abs_steps.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                let sign = if steps > 0 { "+" } else { "-" };
                let percent_arg = format!("{}%{}", abs_steps, sign);
                let _ = Command::new("wpctl")
                    .args(["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", &percent_arg])
                    .output();
            }
        }
        LevelKind::Brightness => {
            let action = if steps > 0 { "brightness-up" } else { "brightness-down" };
            let ok = Command::new("noctalia")
                .args(["msg", action, "eDP-1", &abs_steps.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                let sign = if steps > 0 { "+" } else { "-" };
                let percent_arg = format!("{}%{}", abs_steps, sign);
                let _ = Command::new("brightnessctl")
                    .args(["--device=gmux_backlight", "set", &percent_arg])
                    .output();
            }
            let lvl = brightness();
            if let Some(pct) = lvl.percent {
                let icon = if pct < 40 { "brightness-low" } else { "brightness-high" };
                let progress = (pct as f64 / 100.0).clamp(0.0, 1.0);
                let json = format!(
                    r#"{{"icon":"{}","value":"{}%","progress":{:.2}}}"#,
                    icon, pct, progress
                );
                let _ = Command::new("noctalia")
                    .args(["msg", "osd-show", &json])
                    .spawn();
                let _ = Command::new("noctalia")
                    .args(["msg", "brightness-osd", &format!("{pct}%")])
                    .spawn();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Level {
    pub percent: Option<u32>,
    pub muted: bool,
}

pub fn volume() -> Level {
    Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(Level::default, |text| parse_wpctl(&text))
}

fn parse_wpctl(text: &str) -> Level {
    let trimmed = text.trim();
    let percent = trimmed
        .strip_prefix("Volume:")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| (value * 100.0).round() as u32);
    Level {
        percent,
        muted: trimmed.contains("[MUTED]"),
    }
}

static BACKLIGHT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn find_backlight() -> Option<PathBuf> {
    fs::read_dir("/sys/class/backlight")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            name != "t2tb_backlight" && name != "appletb_backlight"
        })
        .filter_map(|path| Some((type_rank(&read(&path, "type")?)?, path)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, path)| path)
}

/// The display backlight, chosen like desktops do: firmware before platform
/// before raw, never one of the Touch Bar's own controllers.
pub fn brightness() -> Level {
    let path = BACKLIGHT_PATH.get_or_init(find_backlight);
    let percent = path.as_ref().and_then(|path| {
        let current: u64 = read(path, "brightness")?.parse().ok()?;
        let max: u64 = read(path, "max_brightness")?.parse().ok()?;
        (max > 0).then(|| ((current * 100 + max / 2) / max) as u32)
    });
    Level {
        percent,
        muted: false,
    }
}

fn type_rank(kind: &str) -> Option<u8> {
    match kind {
        "firmware" => Some(0),
        "platform" => Some(1),
        "raw" => Some(2),
        _ => None,
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name))
        .ok()
        .map(|value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpctl_output_is_parsed() {
        assert_eq!(
            parse_wpctl("Volume: 0.36\n"),
            Level {
                percent: Some(36),
                muted: false
            }
        );
        assert_eq!(
            parse_wpctl("Volume: 1.20 [MUTED]\n"),
            Level {
                percent: Some(120),
                muted: true
            }
        );
        assert_eq!(parse_wpctl("garbage"), Level::default());
    }
}
