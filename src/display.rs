// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2023 WhatAmISupposedToPutHere
//
// The DRM setup is derived from tiny-dfr's MIT-licensed display backend:
// https://github.com/AsahiLinux/tiny-dfr

use std::{
    fs::{File, OpenOptions},
    os::fd::{AsFd, BorrowedFd},
    path::Path,
};

use anyhow::{Context, Result, anyhow};
use drm::{
    ClientCapability, Device as DrmDevice,
    buffer::{Buffer, DrmFourcc},
    control::{
        AtomicCommitFlags, ClipRect, Device as ControlDevice, Mode, ResourceHandle, atomic,
        connector,
        dumbbuffer::DumbBuffer,
        framebuffer, property,
    },
};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl ControlDevice for Card {}
impl DrmDevice for Card {}

impl Card {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self(OpenOptions::new().read(true).write(true).open(path)?))
    }
}

pub struct Display {
    card: Card,
    mode: Mode,
    buffer: DumbBuffer,
    framebuffer: framebuffer::Handle,
    stride: usize,
    mapped_ptr: *mut u8,
    mapped_len: usize,
    prev_pixels: Vec<u32>,
}

unsafe impl Send for Display {}

impl Drop for Display {
    fn drop(&mut self) {
        if !self.mapped_ptr.is_null() {
            unsafe {
                let _ = libc::munmap(self.mapped_ptr as *mut _, self.mapped_len);
            }
        }
        let _ = self.card.destroy_framebuffer(self.framebuffer);
        let _ = self.card.destroy_dumb_buffer(self.buffer);
    }
}

fn property_id<T: ResourceHandle>(card: &Card, handle: T, name: &str) -> Result<property::Handle> {
    let props = card.get_properties(handle)?;
    for id in props.as_props_and_values().0 {
        let info = card.get_property(*id)?;
        if info.name().to_str()? == name {
            return Ok(*id);
        }
    }
    Err(anyhow!("DRM property {name} not found"))
}

fn try_open(path: &Path) -> Result<Display> {
    let card = Card::open(path)?;
    card.set_client_capability(ClientCapability::UniversalPlanes, true)?;
    card.set_client_capability(ClientCapability::Atomic, true)?;
    card.acquire_master_lock()?;

    let resources = card.resource_handles()?;
    let connector = resources
        .connectors()
        .iter()
        .filter_map(|handle| card.get_connector(*handle, true).ok())
        .find(|info| info.state() == connector::State::Connected)
        .ok_or_else(|| anyhow!("no connected connector"))?;
    let mode = *connector
        .modes()
        .first()
        .ok_or_else(|| anyhow!("connector has no mode"))?;
    let (physical_width, physical_height) = mode.size();
    if physical_height / physical_width < 30 {
        return Err(anyhow!("connector is not Touch-Bar-shaped"));
    }

    let crtc = resources
        .crtcs()
        .first()
        .and_then(|handle| card.get_crtc(*handle).ok())
        .ok_or_else(|| anyhow!("no CRTC"))?;
    let plane = *card
        .plane_handles()?
        .first()
        .ok_or_else(|| anyhow!("no plane"))?;
    let mut buffer = card.create_dumb_buffer((64, physical_height.into()), DrmFourcc::Xrgb8888, 32)?;
    let framebuffer = card.add_framebuffer(&buffer, 24, 32)?;
    let stride = buffer.pitch() as usize;

    let mut mapping = card.map_dumb_buffer(&mut buffer)?;
    mapping.as_mut().fill(0);
    let mapped_ptr = mapping.as_mut().as_mut_ptr();
    let mapped_len = mapping.as_mut().len();
    std::mem::forget(mapping);

    let mut request = atomic::AtomicModeReq::new();
    request.add_property(
        connector.handle(),
        property_id(&card, connector.handle(), "CRTC_ID")?,
        property::Value::CRTC(Some(crtc.handle())),
    );
    let mode_blob = card.create_property_blob(&mode)?;
    request.add_property(
        crtc.handle(),
        property_id(&card, crtc.handle(), "MODE_ID")?,
        mode_blob,
    );
    request.add_property(
        crtc.handle(),
        property_id(&card, crtc.handle(), "ACTIVE")?,
        property::Value::Boolean(true),
    );
    request.add_property(
        plane,
        property_id(&card, plane, "FB_ID")?,
        property::Value::Framebuffer(Some(framebuffer)),
    );
    request.add_property(
        plane,
        property_id(&card, plane, "CRTC_ID")?,
        property::Value::CRTC(Some(crtc.handle())),
    );
    for (name, value) in [
        ("SRC_X", 0),
        ("SRC_Y", 0),
        ("SRC_W", (physical_width as u64) << 16),
        ("SRC_H", (physical_height as u64) << 16),
    ] {
        request.add_property(
            plane,
            property_id(&card, plane, name)?,
            property::Value::UnsignedRange(value),
        );
    }
    for (name, value) in [("CRTC_X", 0), ("CRTC_Y", 0)] {
        request.add_property(
            plane,
            property_id(&card, plane, name)?,
            property::Value::SignedRange(value),
        );
    }
    for (name, value) in [
        ("CRTC_W", physical_width as u64),
        ("CRTC_H", physical_height as u64),
    ] {
        request.add_property(
            plane,
            property_id(&card, plane, name)?,
            property::Value::UnsignedRange(value),
        );
    }
    card.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, request)?;
    Ok(Display {
        card,
        mode,
        buffer,
        framebuffer,
        stride,
        mapped_ptr,
        mapped_len,
        prev_pixels: Vec::new(),
    })
}

impl Display {
    pub fn open() -> Result<Self> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(10);
        let path = loop {
            if let Some(card) = crate::usb::touchbar_drm_card() {
                break card;
            }
            if start.elapsed() >= timeout {
                return Err(anyhow!("Touch Bar DRM device is not attached"));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        try_open(&path).with_context(|| format!("open Touch Bar DRM device {}", path.display()))
    }

    /// Logical dimensions after applying the panel's 90-degree orientation.
    pub fn dimensions(&self) -> (u16, u16) {
        let (height, width) = self.mode.size();
        (width, height)
    }

    pub fn present(&mut self, logical: &[u32], width: u16, height: u16) -> Result<()> {
        anyhow::ensure!(
            logical.len() == width as usize * height as usize,
            "bad frame size"
        );
        let w = width as usize;
        let h = height as usize;

        if self.prev_pixels.len() != logical.len() {
            self.prev_pixels = vec![0; logical.len()];
        }

        if logical == self.prev_pixels.as_slice() {
            // Nothing changed on the entire display: skip copy, ioctl, and USB transmission
            return Ok(());
        }

        let mut min_x = usize::MAX;
        let mut max_x = 0;

        for y in 0..h {
            let row_offset = y * w;
            let log_row = &logical[row_offset..row_offset + w];
            let prev_row = &self.prev_pixels[row_offset..row_offset + w];
            if log_row == prev_row {
                continue;
            }
            if let Some(first) = (0..w).find(|&x| log_row[x] != prev_row[x])
                && first < min_x
            {
                min_x = first;
            }
            if let Some(last) = (0..w).rposition(|x| log_row[x] != prev_row[x])
                && last > max_x
            {
                max_x = last;
            }
        }

        if min_x > max_x {
            // Nothing changed on the entire display: skip copy, ioctl, and USB transmission
            return Ok(());
        }

        let target = unsafe { std::slice::from_raw_parts_mut(self.mapped_ptr, self.mapped_len) };
        for x in min_x..=max_x {
            let row_offset = x * self.stride;
            let row = &mut target[row_offset..row_offset + h * 4];
            for px in 0..h {
                let y = h - 1 - px;
                let pixel = logical[y * w + x];
                self.prev_pixels[y * w + x] = pixel;
                row[px * 4..px * 4 + 4].copy_from_slice(&pixel.to_le_bytes());
            }
        }

        // Apple T2 BridgeOS display controller expects full panel damage rectangles.
        // Sending partial/unaligned clips saturates or stalls BridgeOS's internal double-buffer,
        // causing t2bdrm USB timeouts and DRM framebuffer errors.
        // We submit the standard full panel bounds (0, 0, height, width), but catch any
        // transient DRM errors so t2-touchbar-ultra never freezes or terminates the event loop.
        if let Err(err) = self.card.dirty_framebuffer(
            self.framebuffer,
            &[ClipRect::new(0, 0, height, width)],
        ) {
            eprintln!("t2-touchbar-ultra: dirty_framebuffer warning: {err:#}");
        }
        Ok(())
    }
}
