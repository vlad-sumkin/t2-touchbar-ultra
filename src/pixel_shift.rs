// SPDX-License-Identifier: GPL-3.0-or-later

//! Anti-burn-in pixel shift for the idle status bar.
//! Subtly shifts rendering coordinates in the X and Y axes over time
//! to prevent OLED/display burn-in on static elements.

pub const PIXEL_SHIFT_WIDTH_PX: i32 = 4; // -2 .. +2 px
pub const PIXEL_SHIFT_HEIGHT_PX: i32 = 2; // -1 .. +1 px

#[derive(Clone, Debug)]
pub struct PixelShiftManager {
    shift_x: i32,
    shift_y: i32,
    dir_x: i32,
    dir_y: i32,
    step_count: u32,
    last_step_ms: u64,
}

impl Default for PixelShiftManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PixelShiftManager {
    pub fn new() -> Self {
        Self {
            shift_x: 0,
            shift_y: 0,
            dir_x: 1,
            dir_y: 1,
            step_count: 0,
            last_step_ms: 0,
        }
    }

    /// Advances the pixel shift state at most once every 30 seconds.
    pub fn step_time(&mut self, now_ms: u64) -> (f32, f32) {
        if self.last_step_ms == 0 {
            self.last_step_ms = now_ms;
            return self.get();
        }
        if now_ms.saturating_sub(self.last_step_ms) >= 30_000 {
            self.last_step_ms = now_ms;
            self.step()
        } else {
            self.get()
        }
    }

    /// Advances the pixel shift state by one tick.
    pub fn step(&mut self) -> (f32, f32) {
        self.step_count = self.step_count.wrapping_add(1);

        // Shift X every 2 ticks (~10s)
        if self.step_count.is_multiple_of(2) {
            self.shift_x += self.dir_x;
            if self.shift_x >= (PIXEL_SHIFT_WIDTH_PX / 2) {
                self.shift_x = PIXEL_SHIFT_WIDTH_PX / 2;
                self.dir_x = -1;
            } else if self.shift_x <= -(PIXEL_SHIFT_WIDTH_PX / 2) {
                self.shift_x = -(PIXEL_SHIFT_WIDTH_PX / 2);
                self.dir_x = 1;
            }

            // Shift Y every 6 ticks (~30s)
            if self.step_count.is_multiple_of(6) {
                self.shift_y += self.dir_y;
                if self.shift_y >= (PIXEL_SHIFT_HEIGHT_PX / 2) {
                    self.shift_y = PIXEL_SHIFT_HEIGHT_PX / 2;
                    self.dir_y = -1;
                } else if self.shift_y <= -(PIXEL_SHIFT_HEIGHT_PX / 2) {
                    self.shift_y = -(PIXEL_SHIFT_HEIGHT_PX / 2);
                    self.dir_y = 1;
                }
            }
        }

        self.get()
    }

    /// Returns the current (shift_x, shift_y) in floating point pixels.
    pub fn get(&self) -> (f32, f32) {
        (self.shift_x as f32, self.shift_y as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_shift_step_time_does_not_shift_rapidly() {
        let mut ps = PixelShiftManager::new();
        let (x0, y0) = ps.step_time(1000);
        // Repeated calls over short interval (30 FPS frames) must NOT shift
        for ms in 1001..15000 {
            let (x, y) = ps.step_time(ms);
            assert_eq!(x, x0);
            assert_eq!(y, y0);
        }
    }

    #[test]
    fn pixel_shift_stays_within_bounds() {
        let mut ps = PixelShiftManager::new();
        for _ in 0..500 {
            let (x, y) = ps.step();
            assert!(
                x >= -(PIXEL_SHIFT_WIDTH_PX as f32 / 2.0)
                    && x <= (PIXEL_SHIFT_WIDTH_PX as f32 / 2.0),
                "x out of bounds: {x}"
            );
            assert!(
                y >= -(PIXEL_SHIFT_HEIGHT_PX as f32 / 2.0)
                    && y <= (PIXEL_SHIFT_HEIGHT_PX as f32 / 2.0),
                "y out of bounds: {y}"
            );
        }
    }
}
