// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::Result;

#[derive(Debug, Clone)]
pub enum TextInputEvent {
    Activated,
    Deactivated,
    SurroundingText { text: String, cursor: usize },
}

pub struct TextInputWatcher {
    _running: Arc<AtomicBool>,
}

impl TextInputWatcher {
    pub fn spawn() -> Result<(Receiver<TextInputEvent>, UnixStream, Arc<AtomicBool>)> {
        let (tx, rx) = mpsc::channel();
        let (wake_reader, wake_writer) = UnixStream::pair()?;
        wake_reader.set_nonblocking(true)?;
        wake_writer.set_nonblocking(true)?;

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);

        thread::Builder::new()
            .name("t2-text-input".into())
            .spawn(move || {
                run_watcher(running_clone, tx, wake_writer);
            })?;

        Ok((rx, wake_reader, running))
    }
}

fn run_watcher(
    running: Arc<AtomicBool>,
    tx: Sender<TextInputEvent>,
    mut wake_writer: UnixStream,
) {
    while running.load(Ordering::Relaxed) {
        if let Err(_e) = run_session(&running, &tx, &mut wake_writer) {
            thread::sleep(Duration::from_millis(1000));
        }
    }
}

fn run_session(
    running: &Arc<AtomicBool>,
    tx: &Sender<TextInputEvent>,
    wake_writer: &mut UnixStream,
) -> Result<()> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        let uid = unsafe { libc::getuid() };
        format!("/run/user/{}", uid)
    });
    let display = std::env::var("WAYLAND_DISPLAY").ok().or_else(|| {
        for entry in std::fs::read_dir(&runtime_dir).ok()? {
            if let Ok(entry) = entry {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("wayland-") && !name.ends_with(".lock") {
                    return Some(name);
                }
            }
        }
        None
    }).unwrap_or_else(|| "wayland-1".into());
    let socket_path = format!("{}/{}", runtime_dir, display);
    let mut stream = UnixStream::connect(&socket_path)?;
    stream.set_read_timeout(Some(Duration::from_millis(2000)))?;

    // 1. Get registry (id 1, opcode 1, new_id 2)
    stream.write_all(&[1u32, (12 << 16) | 1, 2].iter().flat_map(|x| x.to_ne_bytes()).collect::<Vec<_>>())?;
    // Sync (id 1, opcode 0, new_id 3)
    stream.write_all(&[1u32, (12 << 16) | 0, 3].iter().flat_map(|x| x.to_ne_bytes()).collect::<Vec<_>>())?;

    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf)?;

    let mut ime_mgr_name = 0u32;
    let mut seat_name = 0u32;
    let mut pos = 0;
    while pos + 8 <= n {
        let sender = u32::from_ne_bytes(buf[pos..pos+4].try_into().unwrap());
        let op_size = u32::from_ne_bytes(buf[pos+4..pos+8].try_into().unwrap());
        let size = (op_size >> 16) as usize;
        let opcode = (op_size & 0xffff) as u16;
        if size == 0 || pos + size > n { break; }
        if sender == 2 && opcode == 0 {
            let name = u32::from_ne_bytes(buf[pos+8..pos+12].try_into().unwrap());
            let iface_len = u32::from_ne_bytes(buf[pos+12..pos+16].try_into().unwrap()) as usize;
            let iface_str = String::from_utf8_lossy(&buf[pos+16..pos+16+iface_len-1]);
            if iface_str == "zwp_input_method_manager_v2" { ime_mgr_name = name; }
            if iface_str == "wl_seat" { seat_name = name; }
        }
        pos += size;
    }

    if ime_mgr_name == 0 || seat_name == 0 {
        return Err(anyhow::anyhow!("input_method_manager_v2 or wl_seat not found in Wayland registry"));
    }

    // Bind seat to id 4: registry.bind(seat_name, "wl_seat", 9, 4)
    let iface = b"wl_seat\0";
    let iface_padded = (iface.len() + 3) & !3;
    let total_len = 12 + 4 + iface_padded + 4 + 4;
    let mut bind_seat = Vec::new();
    bind_seat.extend_from_slice(&2u32.to_ne_bytes());
    bind_seat.extend_from_slice(&(((total_len as u32) << 16) | 0).to_ne_bytes());
    bind_seat.extend_from_slice(&seat_name.to_ne_bytes());
    bind_seat.extend_from_slice(&(iface.len() as u32).to_ne_bytes());
    bind_seat.extend_from_slice(iface);
    bind_seat.resize(12 + 4 + iface_padded, 0);
    bind_seat.extend_from_slice(&9u32.to_ne_bytes());
    bind_seat.extend_from_slice(&4u32.to_ne_bytes());
    stream.write_all(&bind_seat)?;

    // Bind input_method_manager_v2 to id 5
    let iface_ime = b"zwp_input_method_manager_v2\0";
    let iface_ime_padded = (iface_ime.len() + 3) & !3;
    let total_len = 12 + 4 + iface_ime_padded + 4 + 4;
    let mut bind_ime = Vec::new();
    bind_ime.extend_from_slice(&2u32.to_ne_bytes());
    bind_ime.extend_from_slice(&(((total_len as u32) << 16) | 0).to_ne_bytes());
    bind_ime.extend_from_slice(&ime_mgr_name.to_ne_bytes());
    bind_ime.extend_from_slice(&(iface_ime.len() as u32).to_ne_bytes());
    bind_ime.extend_from_slice(iface_ime);
    bind_ime.resize(12 + 4 + iface_ime_padded, 0);
    bind_ime.extend_from_slice(&1u32.to_ne_bytes());
    bind_ime.extend_from_slice(&5u32.to_ne_bytes());
    stream.write_all(&bind_ime)?;

    // get_input_method(seat = 4, new_id = 6)
    let get_im = [5u32, (16 << 16) | 0, 4, 6];
    stream.write_all(&get_im.iter().flat_map(|x| x.to_ne_bytes()).collect::<Vec<_>>())?;

    stream.set_read_timeout(Some(Duration::from_millis(500)))?;

    let mut current_text: Option<String> = None;
    let mut current_cursor: usize = 0;
    let mut stream_buf = Vec::new();

    while running.load(Ordering::Relaxed) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(read_bytes) => {
                stream_buf.extend_from_slice(&buf[..read_bytes]);
                let mut pos = 0;
                while pos + 8 <= stream_buf.len() {
                    let sender = u32::from_ne_bytes(stream_buf[pos..pos+4].try_into().unwrap());
                    let op_size = u32::from_ne_bytes(stream_buf[pos+4..pos+8].try_into().unwrap());
                    let size = (op_size >> 16) as usize;
                    let opcode = (op_size & 0xffff) as u16;
                    if size == 0 || pos + size > stream_buf.len() { break; }

                    if sender == 6 {
                        match opcode {
                            0 => {
                                // activate
                                let _ = tx.send(TextInputEvent::Activated);
                                let _ = wake_writer.write_all(&[1]);
                            }
                            1 => {
                                // deactivate
                                current_text = None;
                                current_cursor = 0;
                                let _ = tx.send(TextInputEvent::Deactivated);
                                let _ = wake_writer.write_all(&[1]);
                            }
                            2 => {
                                // surrounding_text(text: string, cursor: uint, anchor: uint)
                                if pos + 12 <= stream_buf.len() {
                                    let text_len = u32::from_ne_bytes(stream_buf[pos+8..pos+12].try_into().unwrap()) as usize;
                                    let text_padded = (text_len + 3) & !3;
                                    if pos + 12 + text_padded <= stream_buf.len() {
                                        let raw_bytes = if text_len > 1 {
                                            &stream_buf[pos+12..pos+12+text_len-1]
                                        } else {
                                            b""
                                        };
                                        let text = String::from_utf8_lossy(raw_bytes).to_string();
                                        let c_pos = pos + 12 + text_padded;
                                        if c_pos + 4 <= stream_buf.len() {
                                            let cursor_offset = u32::from_ne_bytes(stream_buf[c_pos..c_pos+4].try_into().unwrap()) as usize;
                                            current_text = Some(text);
                                            current_cursor = cursor_offset;
                                        }
                                    }
                                }
                            }
                            5 => {
                                // done: commit state
                                if let Some(text) = &current_text {
                                    let _ = tx.send(TextInputEvent::SurroundingText {
                                        text: text.clone(),
                                        cursor: current_cursor,
                                    });
                                    let _ = wake_writer.write_all(&[1]);
                                }
                            }
                            _ => {}
                        }
                    }
                    pos += size;
                }
                if pos > 0 {
                    stream_buf.drain(..pos);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(_) => {
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_input_session() {
        let running = Arc::new(AtomicBool::new(true));
        let (tx, _rx) = mpsc::channel();
        let (_wake_r, mut wake_w) = UnixStream::pair().unwrap();
        let r_clone = Arc::clone(&running);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            r_clone.store(false, Ordering::Relaxed);
        });
        let res = run_session(&running, &tx, &mut wake_w);
        println!("run_session result: {:?}", res);
        let _ = t.join();
        assert!(res.is_ok(), "run_session failed: {:?}", res);
    }
}
