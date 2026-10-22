//! Screenshots for `background { screenshot }`, taken before locking.
//!
//! The compositor shows only lock surfaces once the session is locked, so the frames must be
//! captured first (hyprlock does the same). wlr-screencopy is tried first, then
//! ext-image-copy-capture; with neither, the background falls back to its colour.
//!
//! wlr-screencopy: each output gets one `capture_output`; the first usable shm `buffer` event sizes
//! a pool, `copy` fills it, and `ready` / `failed` finish the capture. Version 3 lists every buffer
//! type before `buffer_done`, and `copy` waits for it, as the protocol asks.
//!
//! ext-image-copy-capture: each output gets a session; its buffer constraints (`buffer_size`,
//! `shm_format`, `done`) size a pool, one frame captures into it, and `ready` / `failed` /
//! `stopped` finish the capture. Frame and session are destroyed afterwards.
//!
//! The buffer holds the output in buffer order; [`untransform`] turns it upright using the
//! output's transform (wlr) or the frame's `transform` event (ext). Captures that never finish
//! are abandoned after a timeout; their outputs fall back to the colour.

use std::collections::HashMap;

use smithay_client_toolkit::output::OutputData;
use smithay_client_toolkit::reexports::protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use smithay_client_toolkit::reexports::protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
    ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use smithay_client_toolkit::reexports::protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};
use smithay_client_toolkit::shm::raw::RawPool;
use smithay_client_toolkit::shm::Shm;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output::{Transform, WlOutput};
use wayland_client::protocol::{wl_buffer, wl_shm};
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};

use crate::App;

/// A captured frame as premultiplied ARGB8888 rows (cairo's layout), top row first, upright.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// An shm buffer that a capture is filling, and how to read it back.
struct ShmBuffer {
    pool: RawPool,
    buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    y_invert: bool,
    transform: Transform,
}

/// One output's capture and every protocol object it owns, so that finishing or abandoning it
/// can destroy them all exactly once (the capture leaves the map first).
enum Capture {
    Wlr {
        frame: ZwlrScreencopyFrameV1,
        /// The captured output; wlr-screencopy reports no transform, so the output's is used.
        output: WlOutput,
        phase: WlrPhase,
        copy: Option<ShmBuffer>,
    },
    Ext {
        session: ExtImageCopyCaptureSessionV1,
        /// At most one frame per session; created once the constraints are known.
        frame: Option<ExtImageCopyCaptureFrameV1>,
        phase: ExtPhase,
        copy: Option<ShmBuffer>,
    },
}

impl Capture {
    fn destroy(self) {
        let copy = match self {
            Capture::Wlr { frame, copy, .. } => {
                frame.destroy();
                copy
            }
            Capture::Ext {
                session,
                frame,
                copy,
                ..
            } => {
                // Destroying the session does not destroy its frame.
                if let Some(frame) = frame {
                    frame.destroy();
                }
                session.destroy();
                copy
            }
        };
        if let Some(copy) = copy {
            copy.buffer.destroy();
        }
    }
}

/// Captures in flight and finished, keyed by output.
#[derive(Default)]
pub struct Screencopy {
    captures: HashMap<u32, Capture>,
    pub done: HashMap<u32, Shot>,
    /// Test hook `ext-fail`: offer a 1x1 buffer so the compositor fails the ext frame.
    tiny_ext_buffer: bool,
}

/// User data on each frame and session: which output it captures.
pub struct FrameData {
    output_id: u32,
}

/// Which backends the `SWAYWARDLOCK_TEST_SCREENCOPY` test hook leaves in play.
#[derive(Default)]
struct Hook {
    skip_wlr: bool,
    skip_ext: bool,
    tiny_ext_buffer: bool,
}

impl Hook {
    /// `ext` skips wlr-screencopy, `none` skips both, `ext-fail` skips wlr-screencopy and makes
    /// the ext capture fail. Release builds ignore the variable.
    fn from_env() -> Self {
        #[cfg(feature = "test-auth")]
        if let Some(v) = std::env::var_os("SWAYWARDLOCK_TEST_SCREENCOPY") {
            return match v.to_str() {
                Some("ext") => Hook {
                    skip_wlr: true,
                    ..Hook::default()
                },
                Some("none") => Hook {
                    skip_wlr: true,
                    skip_ext: true,
                    ..Hook::default()
                },
                Some("ext-fail") => Hook {
                    skip_wlr: true,
                    tiny_ext_buffer: true,
                    ..Hook::default()
                },
                _ => Hook::default(),
            };
        }
        Hook::default()
    }
}

impl Screencopy {
    /// Start capturing every output in `outputs`. Returns `None` if the compositor has neither
    /// wlr-screencopy nor ext-image-copy-capture.
    pub fn start(
        globals: &GlobalList,
        qh: &QueueHandle<App>,
        outputs: &[(u32, WlOutput)],
    ) -> Option<Self> {
        let hook = Hook::from_env();
        let mut sc = Self {
            tiny_ext_buffer: hook.tiny_ext_buffer,
            ..Self::default()
        };
        let wlr = (!hook.skip_wlr)
            .then(|| {
                globals
                    .bind::<ZwlrScreencopyManagerV1, _, _>(qh, 1..=3, Ignore)
                    .ok()
            })
            .flatten();
        if let Some(manager) = wlr {
            tracing::debug!("screenshots via wlr-screencopy");
            for (id, output) in outputs {
                let frame = manager.capture_output(0, output, qh, FrameData { output_id: *id });
                sc.captures.insert(
                    *id,
                    Capture::Wlr {
                        frame,
                        output: output.clone(),
                        phase: WlrPhase::Offers(None),
                        copy: None,
                    },
                );
            }
            manager.destroy();
            return Some(sc);
        }
        let ext = (!hook.skip_ext)
            .then(|| {
                let manager = globals.bind::<ExtImageCopyCaptureManagerV1, _, _>(qh, 1..=1, Ignore);
                let sources =
                    globals.bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(qh, 1..=1, Ignore);
                manager.ok().zip(sources.ok())
            })
            .flatten();
        let Some((manager, sources)) = ext else {
            tracing::warn!(
                "screenshots unavailable: the compositor has neither wlr-screencopy nor \
                 ext-image-copy-capture; using the colour"
            );
            return None;
        };
        tracing::debug!("screenshots via ext-image-copy-capture");
        for (id, output) in outputs {
            let source = sources.create_source(output, qh, Ignore);
            let session =
                manager.create_session(&source, Options::empty(), qh, FrameData { output_id: *id });
            source.destroy();
            sc.captures.insert(
                *id,
                Capture::Ext {
                    session,
                    frame: None,
                    phase: ExtPhase::Constraints {
                        size: None,
                        format: None,
                    },
                    copy: None,
                },
            );
        }
        manager.destroy();
        sources.destroy();
        Some(sc)
    }

    pub fn is_finished(&self) -> bool {
        self.captures.is_empty()
    }

    /// Destroy every capture still in flight (called when the wait times out, before locking).
    pub fn abandon(&mut self) {
        for (_, capture) in self.captures.drain() {
            capture.destroy();
        }
    }

    /// Give up on one output's capture; it falls back to the colour.
    fn abort(&mut self, id: u32, why: &str) {
        if let Some(capture) = self.captures.remove(&id) {
            tracing::warn!("screenshot: {why}; using the colour");
            capture.destroy();
        }
    }

    /// The capture's buffer is filled: keep it, upright, and destroy the capture.
    fn finish(&mut self, id: u32) {
        let Some(mut capture) = self.captures.remove(&id) else {
            return;
        };
        let (Capture::Wlr { copy, .. } | Capture::Ext { copy, .. }) = &mut capture;
        if let Some(c) = copy {
            match convert(
                c.pool.mmap(),
                c.width,
                c.height,
                c.stride,
                c.format,
                c.y_invert,
            ) {
                Some(shot) => {
                    self.done.insert(id, untransform(shot, c.transform));
                }
                None => tracing::warn!(
                    "screenshot: unusable buffer {}x{}, stride {}; using the colour",
                    c.width,
                    c.height,
                    c.stride
                ),
            }
        }
        capture.destroy();
    }

    fn on_wlr_event(
        &mut self,
        shm: &Shm,
        id: u32,
        event: zwlr_screencopy_frame_v1::Event,
        qh: &QueueHandle<App>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        let Some(Capture::Wlr {
            frame,
            output,
            phase,
            copy,
        }) = self.captures.get_mut(&id)
        else {
            return;
        };
        let event = match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => WlrEvent::Buffer(WlrOffer {
                format,
                width,
                height,
                stride,
            }),
            Event::BufferDone => WlrEvent::BufferDone,
            Event::Flags { flags } => {
                if let Some(c) = copy {
                    c.y_invert = matches!(flags, WEnum::Value(f) if f.contains(zwlr_screencopy_frame_v1::Flags::YInvert));
                }
                return;
            }
            Event::Ready { .. } if copy.is_some() => return self.finish(id),
            Event::Failed => return self.abort(id, "wlr-screencopy capture failed"),
            _ => return,
        };
        match wlr_step(phase, frame.version(), event) {
            WlrStep::Wait => {}
            WlrStep::Copy(offer, format) => {
                // Read the transform now, not when the capture started: in headless sway with a
                // rotated output, sctk's output info was still unset at start (so Normal) while
                // the wl_output events were already received. They come before this frame's
                // events on the same queue, so sctk has handled them by now.
                let transform = output
                    .data::<OutputData>()
                    .map_or(Transform::Normal, OutputData::transform);
                let WlrOffer {
                    width,
                    height,
                    stride,
                    ..
                } = offer;
                match ShmBuffer::new(shm, width, height, stride, format, transform, qh) {
                    Ok(c) => {
                        frame.copy(&c.buffer);
                        *copy = Some(c);
                    }
                    Err(why) => self.abort(id, &why),
                }
            }
            WlrStep::Abort(why) => self.abort(id, why),
        }
    }

    fn on_ext_event(&mut self, shm: &Shm, id: u32, event: ExtEvent, qh: &QueueHandle<App>) {
        let Some(Capture::Ext { phase, .. }) = self.captures.get_mut(&id) else {
            return;
        };
        match ext_step(phase, event) {
            ExtStep::Wait => {}
            ExtStep::Capture(width, height, format) => {
                self.ext_capture(shm, id, width, height, format, qh)
            }
            ExtStep::Finish => self.finish(id),
            ExtStep::Abort(why) => self.abort(id, why),
        }
    }

    /// The constraints are in: allocate a buffer and capture one frame into it.
    fn ext_capture(
        &mut self,
        shm: &Shm,
        id: u32,
        width: u32,
        height: u32,
        format: wl_shm::Format,
        qh: &QueueHandle<App>,
    ) {
        let (width, height) = if self.tiny_ext_buffer {
            (1, 1)
        } else {
            (width, height)
        };
        // Every accepted format is 4 bytes per pixel, and the client picks the stride.
        let buffer = width
            .checked_mul(4)
            .ok_or_else(|| format!("unusable capture buffer {width}x{height}"))
            .and_then(|stride| {
                ShmBuffer::new(shm, width, height, stride, format, Transform::Normal, qh)
            });
        let buffer = match buffer {
            Ok(b) => b,
            Err(why) => return self.abort(id, &why),
        };
        let Some(Capture::Ext {
            session,
            frame,
            copy,
            ..
        }) = self.captures.get_mut(&id)
        else {
            return;
        };
        let f = session.create_frame(qh, FrameData { output_id: id });
        f.attach_buffer(&buffer.buffer);
        // ShmBuffer::new checked that both fit in i32.
        f.damage_buffer(0, 0, width as i32, height as i32);
        f.capture();
        *frame = Some(f);
        *copy = Some(buffer);
    }

    fn on_ext_transform(&mut self, id: u32, transform: WEnum<Transform>) {
        if let Some(Capture::Ext { copy: Some(c), .. }) = self.captures.get_mut(&id) {
            c.transform = known(transform);
        }
    }
}

impl ShmBuffer {
    /// Allocate an shm buffer for a `width` x `height` capture with `stride` bytes per row.
    fn new(
        shm: &Shm,
        width: u32,
        height: u32,
        stride: u32,
        format: wl_shm::Format,
        transform: Transform,
        qh: &QueueHandle<App>,
    ) -> Result<Self, String> {
        let len = shm_len(width, height, stride)
            .ok_or_else(|| format!("unusable capture buffer {width}x{height}, stride {stride}"))?;
        let mut pool =
            RawPool::new(len, shm).map_err(|err| format!("cannot allocate a buffer: {err}"))?;
        // shm_len checked that all of these fit in i32.
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            Ignore,
            qh,
        );
        Ok(Self {
            pool,
            buffer,
            width,
            height,
            stride,
            format,
            y_invert: false,
            transform,
        })
    }
}

/// Bytes in a 4-byte-per-pixel shm buffer, if it is usable: non-empty, rows at least `width * 4`
/// bytes long, and every size wl_shm takes as an i32 in range.
fn shm_len(width: u32, height: u32, stride: u32) -> Option<usize> {
    let fits = |v: u32| i32::try_from(v).is_ok();
    let len = stride.checked_mul(height)?;
    let usable = width > 0
        && height > 0
        && stride >= width.checked_mul(4)?
        && fits(width)
        && fits(height)
        && fits(stride)
        && fits(len);
    usable.then_some(len as usize)
}

/// One wlr-screencopy `buffer` event: an shm buffer type the compositor can copy into.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WlrOffer {
    format: WEnum<wl_shm::Format>,
    width: u32,
    height: u32,
    stride: u32,
}

/// Where a wlr-screencopy frame is, apart from its proxies and buffer.
#[derive(Debug, PartialEq)]
enum WlrPhase {
    /// Collecting `buffer` events: the first usable one, with its accepted format.
    Offers(Option<(WlrOffer, wl_shm::Format)>),
    /// `copy` was sent; later buffer events are ignored.
    Copying,
}

#[derive(Debug)]
enum WlrEvent {
    Buffer(WlrOffer),
    BufferDone,
}

#[derive(Debug, PartialEq)]
enum WlrStep {
    Wait,
    /// Allocate a buffer for this offer and send `copy`.
    Copy(WlrOffer, wl_shm::Format),
    Abort(&'static str),
}

/// The wlr capture state machine. Version 3 sends every buffer type (shm, then dmabuf) and then
/// `buffer_done`, and the client picks and copies only after it; versions 1 and 2 have no
/// `buffer_done`, so the first usable shm buffer is copied at once.
fn wlr_step(phase: &mut WlrPhase, version: u32, event: WlrEvent) -> WlrStep {
    let WlrPhase::Offers(chosen) = phase else {
        return WlrStep::Wait;
    };
    let ready = match event {
        WlrEvent::Buffer(offer) => {
            if chosen.is_none() {
                *chosen = pick(None, offer.format).map(|f| (offer, f));
            }
            version < 3 && chosen.is_some()
        }
        WlrEvent::BufferDone => true,
    };
    if !ready {
        return WlrStep::Wait;
    }
    let step = match chosen.take() {
        Some((offer, format)) => WlrStep::Copy(offer, format),
        // v3: all buffer events sent and none usable: give up now instead of at the timeout.
        None => WlrStep::Abort("wlr-screencopy offered no supported shm format"),
    };
    *phase = WlrPhase::Copying;
    step
}

/// Where an ext-image-copy-capture session is, apart from its proxies and buffer.
#[derive(Debug, PartialEq)]
enum ExtPhase {
    /// Collecting buffer constraints until `done`.
    Constraints {
        size: Option<(u32, u32)>,
        format: Option<wl_shm::Format>,
    },
    /// One frame is capturing; further constraint batches are ignored.
    Copying,
    /// Finished or given up; later events are ignored.
    Gone,
}

/// Session and frame events, reduced to what [`ext_step`] needs.
#[derive(Debug)]
enum ExtEvent {
    BufferSize(u32, u32),
    ShmFormat(WEnum<wl_shm::Format>),
    Done,
    Ready,
    Failed(&'static str),
    Stopped,
}

/// What to do after an ext event.
#[derive(Debug, PartialEq)]
enum ExtStep {
    Wait,
    /// Allocate a buffer of this size and format and capture one frame.
    Capture(u32, u32, wl_shm::Format),
    /// The frame is ready: convert it and clean up.
    Finish,
    /// Give up and clean up.
    Abort(&'static str),
}

/// The ext capture state machine: one frame per session, never resurrected once gone.
fn ext_step(phase: &mut ExtPhase, event: ExtEvent) -> ExtStep {
    use ExtEvent as E;
    let (next, step) = match (&mut *phase, event) {
        (ExtPhase::Gone, _) => return ExtStep::Wait,
        (_, E::Stopped) => (
            ExtPhase::Gone,
            ExtStep::Abort("the ext-image-copy-capture session stopped"),
        ),
        (_, E::Failed(why)) => (ExtPhase::Gone, ExtStep::Abort(why)),
        (ExtPhase::Constraints { size, .. }, E::BufferSize(w, h)) => {
            *size = Some((w, h));
            return ExtStep::Wait;
        }
        (ExtPhase::Constraints { format, .. }, E::ShmFormat(offered)) => {
            *format = pick(*format, offered);
            return ExtStep::Wait;
        }
        (ExtPhase::Constraints { size, format }, E::Done) => match (*size, *format) {
            (Some((w, h)), Some(f)) => (ExtPhase::Copying, ExtStep::Capture(w, h, f)),
            _ => (
                ExtPhase::Gone,
                ExtStep::Abort("ext-image-copy-capture offered no supported shm format"),
            ),
        },
        (ExtPhase::Copying, E::Ready) => (ExtPhase::Gone, ExtStep::Finish),
        // Constraint updates while copying, or a ready before any capture.
        _ => return ExtStep::Wait,
    };
    *phase = next;
    step
}

fn failure_reason(reason: WEnum<FailureReason>) -> &'static str {
    match reason {
        WEnum::Value(FailureReason::BufferConstraints) => {
            "ext-image-copy-capture failed: buffer_constraints"
        }
        WEnum::Value(FailureReason::Stopped) => "ext-image-copy-capture failed: stopped",
        _ => "ext-image-copy-capture failed: unknown",
    }
}

/// The first offered shm format that `convert` understands; once one is chosen it stays.
fn pick(chosen: Option<wl_shm::Format>, offered: WEnum<wl_shm::Format>) -> Option<wl_shm::Format> {
    chosen.or(match offered {
        WEnum::Value(f) if accepted(f) => Some(f),
        _ => None,
    })
}

/// A transform from the wire; unknown values count as normal.
fn known(transform: WEnum<Transform>) -> Transform {
    match transform {
        WEnum::Value(t) => t,
        WEnum::Unknown(v) => {
            tracing::warn!("screenshot: unknown transform {v}; treating it as normal");
            Transform::Normal
        }
    }
}

/// Turn a captured buffer upright. The buffer holds the output in buffer order and `transform`
/// (the output's wl_output transform) maps it to what is on screen, so each pixel moves to where
/// wlroots' `wlr_box_transform` puts it. Quarter turns swap width and height.
fn untransform(shot: Shot, transform: Transform) -> Shot {
    use Transform as T;
    if transform == T::Normal {
        return shot;
    }
    let (w, h) = (shot.width as usize, shot.height as usize);
    let quarter = matches!(transform, T::_90 | T::_270 | T::Flipped90 | T::Flipped270);
    let (ow, oh) = if quarter { (h, w) } else { (w, h) };
    let mut data = vec![0u8; shot.data.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match transform {
                T::_90 => (h - 1 - y, x),
                T::_180 => (w - 1 - x, h - 1 - y),
                T::_270 => (y, w - 1 - x),
                T::Flipped => (w - 1 - x, y),
                T::Flipped90 => (y, x),
                T::Flipped180 => (x, h - 1 - y),
                T::Flipped270 => (h - 1 - y, w - 1 - x),
                _ => (x, y),
            };
            data[(ny * ow + nx) * 4..][..4].copy_from_slice(&shot.data[(y * w + x) * 4..][..4]);
        }
    }
    Shot {
        width: ow as u32,
        height: oh as u32,
        data,
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
/// their top 8 bits. `None` if the sizes do not describe a non-empty image inside `src`.
fn convert(
    src: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: wl_shm::Format,
    y_invert: bool,
) -> Option<Shot> {
    let row = (width as usize).checked_mul(4)?;
    let (rows, stride) = (height as usize, stride as usize);
    // The last row needs only `row` bytes, not a full stride.
    let needed = stride.checked_mul(rows.checked_sub(1)?)?.checked_add(row)?;
    if row == 0 || stride < row || src.len() < needed {
        return None;
    }
    let mut data = vec![0u8; row * rows];
    for y in 0..height as usize {
        let sy = if y_invert { height as usize - 1 - y } else { y };
        let s = &src[sy * stride..][..row];
        let d = &mut data[y * row..][..row];
        for (px, sp) in d.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
            px.copy_from_slice(&to_bgra(format, [sp[0], sp[1], sp[2], sp[3]]));
        }
    }
    Some(Shot {
        width,
        height,
        data,
    })
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

/// User data for managers, sources and buffers, so `delegate_dispatch2!` routes their events
/// here.
pub struct Ignore;

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl smithay_client_toolkit::dispatch2::Dispatch2<$proxy, App> for Ignore {
            fn event(
                &self,
                _: &mut App,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &Connection,
                _: &QueueHandle<App>,
            ) {
            }
        }
    )*};
}

ignore_events!(
    ZwlrScreencopyManagerV1,
    wl_buffer::WlBuffer,
    ExtImageCopyCaptureManagerV1,
    ExtOutputImageCaptureSourceManagerV1,
    ExtImageCaptureSourceV1
);

impl smithay_client_toolkit::dispatch2::Dispatch2<ZwlrScreencopyFrameV1, App> for FrameData {
    fn event(
        &self,
        app: &mut App,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let (Some(sc), Some(shm)) = (&mut app.screencopy, &app.shm) {
            sc.on_wlr_event(shm, self.output_id, event, qh);
        }
    }
}

impl smithay_client_toolkit::dispatch2::Dispatch2<ExtImageCopyCaptureSessionV1, App> for FrameData {
    fn event(
        &self,
        app: &mut App,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let event = match event {
            Event::BufferSize { width, height } => ExtEvent::BufferSize(width, height),
            Event::ShmFormat { format } => ExtEvent::ShmFormat(format),
            Event::Done => ExtEvent::Done,
            Event::Stopped => ExtEvent::Stopped,
            _ => return,
        };
        if let (Some(sc), Some(shm)) = (&mut app.screencopy, &app.shm) {
            sc.on_ext_event(shm, self.output_id, event, qh);
        }
    }
}

impl smithay_client_toolkit::dispatch2::Dispatch2<ExtImageCopyCaptureFrameV1, App> for FrameData {
    fn event(
        &self,
        app: &mut App,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let (Some(sc), Some(shm)) = (&mut app.screencopy, &app.shm) else {
            return;
        };
        let event = match event {
            Event::Transform { transform } => {
                return sc.on_ext_transform(self.output_id, transform)
            }
            Event::Ready => ExtEvent::Ready,
            Event::Failed { reason } => ExtEvent::Failed(failure_reason(reason)),
            _ => return,
        };
        sc.on_ext_event(shm, self.output_id, event, qh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_flips_and_fills_alpha() {
        // 1x2 XRGB with stride 8 (padding), rows A then B.
        let src = [1, 2, 3, 0, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        let shot = convert(&src, 1, 2, 8, wl_shm::Format::Xrgb8888, true).unwrap();
        assert_eq!(shot.data, vec![4, 5, 6, 255, 1, 2, 3, 255]);
        let shot = convert(&src, 1, 2, 8, wl_shm::Format::Argb8888, false).unwrap();
        assert_eq!(shot.data, vec![1, 2, 3, 0, 4, 5, 6, 0]);
        // The last row may end at the row, without the stride's padding.
        assert!(convert(&src[..12], 1, 2, 8, wl_shm::Format::Xrgb8888, true).is_some());
    }

    /// Sizes from the compositor that do not fit the mapped buffer once panicked (abort, before
    /// locking). Found by fuzzing: short sources, strides under width * 4, empty images.
    #[test]
    fn convert_rejects_sizes_outside_the_buffer() {
        let f = wl_shm::Format::Abgr2101010;
        for (src, w, h, stride) in [
            (&[][..], 1, 1, 0),
            (&[0; 11][..], 3, 1, 11),
            (&[0; 12][..], 3, 2, 12),
            (&[0; 23][..], 3, 2, 12),
            (&[0; 8][..], 2, 2, 4),
            (&[][..], 0, 2, 1),
            (&[0; 4][..], 1, 0, 4),
            (&[0; 4][..], u32::MAX, 1, 4),
        ] {
            assert!(
                convert(src, w, h, stride, f, false).is_none(),
                "{w}x{h} stride {stride}"
            );
            assert!(
                convert(src, w, h, stride, f, true).is_none(),
                "{w}x{h} stride {stride}"
            );
        }
        assert!(convert(&[0; 24], 3, 2, 12, f, true).is_some());
    }

    /// Every accepted format comes out as cairo's B G R A bytes. Pixel: r=0x10 g=0x20 b=0x30.
    #[test]
    fn convert_handles_bgr_and_10_bit_formats() {
        use wl_shm::Format as F;
        let one = |src: [u8; 4], format| convert(&src, 1, 1, 4, format, false).unwrap().data;
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

    #[test]
    fn pick_keeps_the_first_accepted_format() {
        use wl_shm::Format as F;
        assert_eq!(pick(None, WEnum::Value(F::Rgb565)), None);
        assert_eq!(pick(None, WEnum::Value(F::Xbgr8888)), Some(F::Xbgr8888));
        assert_eq!(
            pick(Some(F::Xbgr8888), WEnum::Value(F::Argb8888)),
            Some(F::Xbgr8888)
        );
        assert_eq!(pick(None, WEnum::Unknown(0x3432_5258)), None);
    }

    /// A 2x3 image whose pixel i is [i, i, i, i], as rows of pixel numbers after `untransform`.
    fn turned(transform: Transform) -> (u32, u32, Vec<Vec<u8>>) {
        let shot = Shot {
            width: 2,
            height: 3,
            data: (0..6u8).flat_map(|i| [i; 4]).collect(),
        };
        let out = untransform(shot, transform);
        let px: Vec<u8> = out.data.chunks_exact(4).map(|p| p[0]).collect();
        let rows = px.chunks(out.width as usize).map(<[u8]>::to_vec).collect();
        (out.width, out.height, rows)
    }

    /// Golden arrangements from wlroots' wlr_box_transform (util/box.c) applied to each pixel of
    /// the 2x3 buffer [[0, 1], [2, 3], [4, 5]].
    #[test]
    fn untransform_moves_pixels_like_wlroots() {
        use Transform as T;
        let cases: [(T, u32, u32, &[&[u8]]); 8] = [
            (T::Normal, 2, 3, &[&[0, 1], &[2, 3], &[4, 5]]),
            (T::_90, 3, 2, &[&[4, 2, 0], &[5, 3, 1]]),
            (T::_180, 2, 3, &[&[5, 4], &[3, 2], &[1, 0]]),
            (T::_270, 3, 2, &[&[1, 3, 5], &[0, 2, 4]]),
            (T::Flipped, 2, 3, &[&[1, 0], &[3, 2], &[5, 4]]),
            (T::Flipped90, 3, 2, &[&[0, 2, 4], &[1, 3, 5]]),
            (T::Flipped180, 2, 3, &[&[4, 5], &[2, 3], &[0, 1]]),
            (T::Flipped270, 3, 2, &[&[5, 3, 1], &[4, 2, 0]]),
        ];
        for (transform, w, h, rows) in cases {
            let want: Vec<Vec<u8>> = rows.iter().map(|r| r.to_vec()).collect();
            assert_eq!(turned(transform), (w, h, want), "{transform:?}");
        }
    }

    #[test]
    fn unknown_transform_is_normal() {
        assert_eq!(known(WEnum::Unknown(42)), Transform::Normal);
        assert_eq!(known(WEnum::Value(Transform::_90)), Transform::_90);
    }

    #[test]
    fn shm_len_rejects_unusable_sizes() {
        assert_eq!(shm_len(320, 240, 1280), Some(320 * 240 * 4));
        assert_eq!(shm_len(0, 240, 0), None);
        assert_eq!(shm_len(320, 0, 1280), None);
        // Stride shorter than a row.
        assert_eq!(shm_len(320, 240, 1279), None);
        // stride * height overflows u32, or the size does not fit wl_shm's i32.
        assert_eq!(shm_len(65536, 65536, 262144), None);
        assert_eq!(shm_len(32768, 16384, 131072), None);
        assert_eq!(shm_len(u32::MAX, 1, u32::MAX), None);
    }

    fn offer(format: wl_shm::Format) -> WlrOffer {
        WlrOffer {
            format: WEnum::Value(format),
            width: 320,
            height: 240,
            stride: 1280,
        }
    }

    /// Version 3 copies only after `buffer_done`, into the first usable shm buffer.
    #[test]
    fn wlr_v3_copies_after_buffer_done() {
        use wl_shm::Format as F;
        let mut phase = WlrPhase::Offers(None);
        let xrgb = offer(F::Xrgb8888);
        assert_eq!(
            wlr_step(&mut phase, 3, WlrEvent::Buffer(offer(F::Rgb565))),
            WlrStep::Wait
        );
        assert_eq!(
            wlr_step(&mut phase, 3, WlrEvent::Buffer(xrgb)),
            WlrStep::Wait
        );
        assert_eq!(
            wlr_step(&mut phase, 3, WlrEvent::Buffer(offer(F::Argb8888))),
            WlrStep::Wait
        );
        assert_eq!(
            wlr_step(&mut phase, 3, WlrEvent::BufferDone),
            WlrStep::Copy(xrgb, F::Xrgb8888)
        );
        // One copy only.
        assert_eq!(
            wlr_step(&mut phase, 3, WlrEvent::Buffer(xrgb)),
            WlrStep::Wait
        );
        assert_eq!(wlr_step(&mut phase, 3, WlrEvent::BufferDone), WlrStep::Wait);
        // Nothing usable: give up at buffer_done.
        let mut phase = WlrPhase::Offers(None);
        assert_eq!(
            wlr_step(&mut phase, 3, WlrEvent::Buffer(offer(F::Rgb565))),
            WlrStep::Wait
        );
        assert!(matches!(
            wlr_step(&mut phase, 3, WlrEvent::BufferDone),
            WlrStep::Abort(_)
        ));
        assert_eq!(phase, WlrPhase::Copying);
    }

    /// Versions 1 and 2 have no `buffer_done`: the first usable buffer is copied at once.
    #[test]
    fn wlr_v1_and_v2_copy_the_first_usable_buffer() {
        use wl_shm::Format as F;
        for version in [1, 2] {
            let mut phase = WlrPhase::Offers(None);
            let rgb = offer(F::Rgb565);
            assert_eq!(
                wlr_step(&mut phase, version, WlrEvent::Buffer(rgb)),
                WlrStep::Wait
            );
            let argb = offer(F::Argb8888);
            assert_eq!(
                wlr_step(&mut phase, version, WlrEvent::Buffer(argb)),
                WlrStep::Copy(argb, F::Argb8888)
            );
            assert_eq!(
                wlr_step(&mut phase, version, WlrEvent::Buffer(argb)),
                WlrStep::Wait
            );
        }
    }

    fn constraints() -> ExtPhase {
        ExtPhase::Constraints {
            size: None,
            format: None,
        }
    }

    #[test]
    fn ext_captures_once_constraints_are_done() {
        use wl_shm::Format as F;
        let mut phase = constraints();
        assert_eq!(
            ext_step(&mut phase, ExtEvent::BufferSize(320, 240)),
            ExtStep::Wait
        );
        let rgb565 = ExtEvent::ShmFormat(WEnum::Value(F::Rgb565));
        assert_eq!(ext_step(&mut phase, rgb565), ExtStep::Wait);
        let xrgb = ExtEvent::ShmFormat(WEnum::Value(F::Xrgb8888));
        assert_eq!(ext_step(&mut phase, xrgb), ExtStep::Wait);
        assert_eq!(
            ext_step(&mut phase, ExtEvent::Done),
            ExtStep::Capture(320, 240, F::Xrgb8888)
        );
        // A repeated constraint batch while copying must not start a second frame.
        assert_eq!(
            ext_step(&mut phase, ExtEvent::BufferSize(640, 480)),
            ExtStep::Wait
        );
        assert_eq!(ext_step(&mut phase, ExtEvent::Done), ExtStep::Wait);
        assert_eq!(phase, ExtPhase::Copying);
        assert_eq!(ext_step(&mut phase, ExtEvent::Ready), ExtStep::Finish);
        // Late events after the capture finished are ignored.
        assert_eq!(ext_step(&mut phase, ExtEvent::Stopped), ExtStep::Wait);
        assert_eq!(ext_step(&mut phase, ExtEvent::Failed("x")), ExtStep::Wait);
        assert_eq!(ext_step(&mut phase, ExtEvent::Done), ExtStep::Wait);
        assert_eq!(phase, ExtPhase::Gone);
    }

    #[test]
    fn ext_gives_up_on_bad_constraints_and_failures() {
        use wl_shm::Format as F;
        // No usable format, or no size.
        let mut phase = constraints();
        ext_step(&mut phase, ExtEvent::BufferSize(320, 240));
        ext_step(&mut phase, ExtEvent::ShmFormat(WEnum::Value(F::Rgb565)));
        assert!(matches!(
            ext_step(&mut phase, ExtEvent::Done),
            ExtStep::Abort(_)
        ));
        let mut phase = constraints();
        ext_step(&mut phase, ExtEvent::ShmFormat(WEnum::Value(F::Xrgb8888)));
        assert!(matches!(
            ext_step(&mut phase, ExtEvent::Done),
            ExtStep::Abort(_)
        ));
        // A ready before any capture is ignored.
        let mut phase = constraints();
        assert_eq!(ext_step(&mut phase, ExtEvent::Ready), ExtStep::Wait);
        // Failed while copying, then stopped: one abort only.
        let mut phase = ExtPhase::Copying;
        assert_eq!(
            ext_step(&mut phase, ExtEvent::Failed("failed")),
            ExtStep::Abort("failed")
        );
        assert_eq!(ext_step(&mut phase, ExtEvent::Stopped), ExtStep::Wait);
        assert_eq!(ext_step(&mut phase, ExtEvent::Ready), ExtStep::Wait);
        // Stopped before the constraints are done.
        let mut phase = constraints();
        assert!(matches!(
            ext_step(&mut phase, ExtEvent::Stopped),
            ExtStep::Abort(_)
        ));
        assert_eq!(ext_step(&mut phase, ExtEvent::Done), ExtStep::Wait);
    }
}
