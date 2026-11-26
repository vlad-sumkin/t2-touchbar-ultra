// SPDX-License-Identifier: GPL-3.0-or-later

//! Listens for systemd-logind `PrepareForSleep(bool)` signals over the system D-Bus
//! so the haptic actuator fd can be cleanly released before suspend and re-acquired
//! automatically on wake.

use std::{
    io::Write,
    os::unix::net::UnixStream,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use zbus::blocking::{Connection, MessageIterator};
use zbus::{MatchRule, message::Type};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SleepEvent {
    Suspending,
    Resumed,
}

pub fn watch() -> Result<(Receiver<SleepEvent>, UnixStream)> {
    let (sender, receiver) = mpsc::channel();
    let (mut wake_writer, wake_reader) = UnixStream::pair()?;
    thread::Builder::new()
        .name("sleep-signals".to_owned())
        .spawn(move || {
            let mut reported = false;
            loop {
                if let Err(error) = listen(&sender, &mut wake_writer) {
                    if !reported {
                        eprintln!("t2-touchbar-ultra: sleep listener: {error:#}");
                        reported = true;
                    }
                    thread::sleep(Duration::from_secs(5));
                }
            }
        })?;
    Ok((receiver, wake_reader))
}

fn listen(sender: &mpsc::Sender<SleepEvent>, wake: &mut UnixStream) -> Result<()> {
    let connection = Connection::system().context("connect system bus")?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.login1")?
        .path("/org/freedesktop/login1")?
        .interface("org.freedesktop.login1.Manager")?
        .member("PrepareForSleep")?
        .build();
    let iterator = MessageIterator::for_match_rule(rule, &connection, None)?;
    for message in iterator {
        let message = message?;
        let (starting,): (bool,) = message.body().deserialize()?;
        let event = if starting {
            SleepEvent::Suspending
        } else {
            SleepEvent::Resumed
        };
        if sender.send(event).is_err() {
            return Ok(());
        }
        let _ = wake.write_all(&[1]);
    }
    Err(anyhow::anyhow!("logind sleep signal stream ended"))
}
