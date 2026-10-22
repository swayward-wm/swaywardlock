//! Screenshots for `background { screenshot }`, taken through wlr-screencopy before locking.
//!
//! The compositor shows only lock surfaces once the session is locked, so the frames must be
//! captured first (hyprlock does the same). Each output gets one `capture_output`; the first shm
//! `buffer` event sizes a pool, `copy` fills it, and `ready` / `failed` finish the capture. Frames
//! that never arrive are dropped after a timeout; the background then falls back to its colour.

use std::collections::HashMap;

use smithay_client_toolkit::reexports::protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};
use smithay_client_toolkit::shm::raw::RawPool;
use smithay_client_toolkit::shm::Shm;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_buffer, wl_output::WlOutput, wl_shm};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

/// A captured frame as premultiplied ARGB8888 rows (cairo's layout), top row first.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

enum Pending {
    /// Waiting for the `buffer` event.
    Requested,
    /// `copy` sent; holding the pool and buffer until `ready`.
    Copying {
        pool: RawPool,
        buffer: wl_buffer::WlBuffer,
        width: u32,
        height: u32,
        stride: u32,
        format: wl_shm::Format,
        y_invert: bool,
    },
}

/// Captures in flight and finished, keyed by output.
#[derive(Default)]
pub struct Screencopy {
    pending: HashMap<u32, Pending>,
    pub done: HashMap<u32, Shot>,
}

/// User data on each frame: which output it captures.
pub struct FrameData {
    output_id: u32,
}

impl Screencopy {
    /// Start capturing every output in `outputs` (as `(id, wl_output)`). Returns `None` if the
    /// compositor lacks wlr-screencopy or shm.
    pub fn start<D>(
        globals: &GlobalList,
        qh: &QueueHandle<D>,
        outputs: &[(u32, WlOutput)],
    ) -> Option<Self>
    where
        D: Dispatch<ZwlrScreencopyManagerV1, Ignore>
            + Dispatch<ZwlrScreencopyFrameV1, FrameData>
            + 'static,
    {
        let manager: ZwlrScreencopyManagerV1 = match globals.bind(qh, 1..=3, Ignore) {
            Ok(m) => m,
            Err(_) => {
                tracing::warn!(
                    "screenshot background: compositor has no wlr-screencopy; using the colour"
                );
                return None;
            }
        };
        let mut sc = Self::default();
        for (id, output) in outputs {
            manager.capture_output(0, output, qh, FrameData { output_id: *id });
            sc.pending.insert(*id, Pending::Requested);
        }
        Some(sc)
    }

    pub fn is_finished(&self) -> bool {
        self.pending.is_empty()
    }

    fn on_event<D>(
        &mut self,
        shm: &Shm,
        frame: &ZwlrScreencopyFrameV1,
        id: u32,
        event: zwlr_screencopy_frame_v1::Event,
        qh: &QueueHandle<D>,
    ) where
        D: Dispatch<wl_buffer::WlBuffer, Ignore> + 'static,
    {
        use zwlr_screencopy_frame_v1::Event;
        match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                // Several buffer events may arrive (one per supported format); take the first
                // shm format we can convert.
                let Some(Pending::Requested) = self.pending.get(&id) else {
                    return;
                };
                let WEnum::Value(format) = format else { return };
                if !accepted(format) {
                    return;
                }
                let len = (stride * height) as usize;
                let mut pool = match RawPool::new(len, shm) {
                    Ok(pool) => pool,
                    Err(err) => {
                        tracing::warn!("screencopy: cannot allocate a buffer: {err}");
                        self.pending.remove(&id);
                        return;
                    }
                };
                let buffer = pool.create_buffer(
                    0,
                    width as i32,
                    height as i32,
                    stride as i32,
                    format,
                    Ignore,
                    qh,
                );
                frame.copy(&buffer);
                self.pending.insert(
                    id,
                    Pending::Copying {
                        pool,
                        buffer,
                        width,
                        height,
                        stride,
                        format,
                        y_invert: false,
                    },
                );
            }
            Event::Flags { flags } => {
                if let Some(Pending::Copying { y_invert, .. }) = self.pending.get_mut(&id) {
                    *y_invert = matches!(flags, WEnum::Value(f) if f.contains(zwlr_screencopy_frame_v1::Flags::YInvert));
                }
            }
            Event::Ready { .. } => {
                if let Some(Pending::Copying {
                    mut pool,
                    buffer,
                    width,
                    height,
                    stride,
                    format,
                    y_invert,
                }) = self.pending.remove(&id)
                {
                    let shot = convert(pool.mmap(), width, height, stride, format, y_invert);
                    buffer.destroy();
                    self.done.insert(id, shot);
                }
                frame.destroy();
            }
            // v3: all buffer events sent. If none was usable, give up now instead of waiting
            // for the timeout.
            Event::BufferDone => {
                if let Some(Pending::Requested) = self.pending.get(&id) {
                    tracing::warn!("screencopy: no supported shm format offered; using the colour");
                    self.pending.remove(&id);
                    frame.destroy();
                }
            }
            Event::Failed => {
                tracing::warn!("screencopy: capture failed for an output; using the colour");
                if let Some(Pending::Copying { buffer, .. }) = self.pending.remove(&id) {
                    buffer.destroy();
                }
                frame.destroy();
            }
            _ => {}
        }
    }
}

/// The shm formats `convert` understands (hyprlock's Screencopy.cpp handles the same set).
fn accepted(format: wl_shm::Format) -> bool {
    use wl_shm::Format as F;
    matches!(
        format,
        F::Argb8888
            | F::Xrgb8888
            | F::Abgr8888
            | F::Xbgr8888
            | F::Argb2101010
            | F::Xrgb2101010
            | F::Abgr2101010
            | F::Xbgr2101010
    )
}

/// An [`accepted`] shm format into tight, top-down rows of cairo's ARgb32 (little-endian
/// B G R A in memory). Screens are opaque, so X formats get alpha 255; 10-bit channels keep
/// their top 8 bits.
fn convert(
    src: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    y_invert: bool,
) -> Shot {
    let row = width as usize * 4;
    let mut data = vec![0u8; row * height as usize];
    for y in 0..height as usize {
        let sy = if y_invert { height as usize - 1 - y } else { y };
        let s = &src[sy * stride as usize..][..row];
        let d = &mut data[y * row..][..row];
        for (px, sp) in d.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
            px.copy_from_slice(&to_bgra(format, [sp[0], sp[1], sp[2], sp[3]]));
        }
    }
    Shot {
        width,
        height,
        data,
    }
}

fn to_bgra(format: wl_shm::Format, p: [u8; 4]) -> [u8; 4] {
    use wl_shm::Format as F;
    let v = u32::from_le_bytes(p);
    // 2101010: (hi, mid, lo) 10-bit channels and a 2-bit alpha in the top bits.
    let ten = |shift: u32| (((v >> shift) & 0x3ff) >> 2) as u8;
    let a2 = (((v >> 30) & 3) * 85) as u8;
    match format {
        F::Argb8888 => p,
        F::Xrgb8888 => [p[0], p[1], p[2], 0xff],
        F::Abgr8888 => [p[2], p[1], p[0], p[3]],
        F::Xbgr8888 => [p[2], p[1], p[0], 0xff],
        F::Argb2101010 => [ten(0), ten(10), ten(20), a2],
        F::Xrgb2101010 => [ten(0), ten(10), ten(20), 0xff],
        F::Abgr2101010 => [ten(20), ten(10), ten(0), a2],
        F::Xbgr2101010 => [ten(20), ten(10), ten(0), 0xff],
        _ => [0, 0, 0, 0xff],
    }
}

/// User data for the manager and buffers, so `delegate_dispatch2!` routes their events here.
pub struct Ignore;

impl smithay_client_toolkit::dispatch2::Dispatch2<ZwlrScreencopyManagerV1, crate::App> for Ignore {
    fn event(
        &self,
        _: &mut crate::App,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as wayland_client::Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<crate::App>,
    ) {
    }
}

impl smithay_client_toolkit::dispatch2::Dispatch2<wl_buffer::WlBuffer, crate::App> for Ignore {
    fn event(
        &self,
        _: &mut crate::App,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &Connection,
        _: &QueueHandle<crate::App>,
    ) {
    }
}

impl smithay_client_toolkit::dispatch2::Dispatch2<ZwlrScreencopyFrameV1, crate::App> for FrameData {
    fn event(
        &self,
        app: &mut crate::App,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &Connection,
        qh: &QueueHandle<crate::App>,
    ) {
        if let (Some(sc), Some(shm)) = (&mut app.screencopy, &app.shm) {
            sc.on_event(shm, frame, self.output_id, event, qh);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_flips_and_fills_alpha() {
        // 1x2 XRGB with stride 8 (padding), rows A then B.
        let src = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        let shot = convert(&src, 1, 2, 8, wl_shm::Format::Xrgb8888, true);
        assert_eq!(shot.data, vec![4, 5, 6, 255, 1, 2, 3, 255]);
        let shot = convert(&src, 1, 2, 8, wl_shm::Format::Argb8888, false);
        assert_eq!(shot.data, vec![1, 2, 3, 0, 4, 5, 6, 0]);
    }

    /// Every accepted format comes out as cairo's B G R A bytes. Pixel: r=0x10 g=0x20 b=0x30.
    #[test]
    fn convert_handles_bgr_and_10_bit_formats() {
        use wl_shm::Format as F;
        let one = |src: [u8; 4], format| convert(&src, 1, 1, 4, format, false).data;
        // ABGR/XBGR8888 are R G B A in memory.
        assert_eq!(
            one([0x10, 0x20, 0x30, 0xff], F::Abgr8888),
            [0x30, 0x20, 0x10, 0xff]
        );
        assert_eq!(
            one([0x10, 0x20, 0x30, 0x00], F::Xbgr8888),
            [0x30, 0x20, 0x10, 0xff]
        );
        // 2101010: a little-endian u32 with 10-bit channels; keep the top 8 bits.
        let (r, g, b) = (0x10u32 << 2, 0x20u32 << 2, 0x30u32 << 2);
        let xrgb = ((r << 20) | (g << 10) | b).to_le_bytes();
        assert_eq!(one(xrgb, F::Xrgb2101010), [0x30, 0x20, 0x10, 0xff]);
        let xbgr = ((b << 20) | (g << 10) | r).to_le_bytes();
        assert_eq!(one(xbgr, F::Xbgr2101010), [0x30, 0x20, 0x10, 0xff]);
        assert!(accepted(F::Xbgr2101010) && accepted(F::Abgr8888) && !accepted(F::Rgb565));
    }
}
