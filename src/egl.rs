//! EGL setup on a client `wl_display`.
//!
//! smithay implements [`EGLNativeDisplay`] only for winit windows, X11 and devices. A lock
//! surface is not a winit window, so this module supplies the Wayland platform from the client's
//! own `wl_display`. The two platforms below copy the Wayland branch of smithay's winit impl
//! (`smithay/src/backend/egl/native.rs`). Surfaces use smithay's existing
//! `EGLNativeSurface for WlEglSurface`.

use std::ffi::c_void;

use anyhow::Context as _;
use smithay::backend::egl::context::{GlAttributes, PixelFormatRequirements};
use smithay::backend::egl::native::{EGLNativeDisplay, EGLPlatform};
use smithay::backend::egl::{ffi, EGLContext, EGLDisplay, EGLSurface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::egl_platform;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy as _};
use wayland_egl::WlEglSurface;

/// A client `wl_display` pointer, valid while the [`Connection`] it came from lives.
struct WaylandDisplay(*mut c_void);

// SAFETY: libwayland-client's `wl_display` is thread-safe, and EGL only uses the pointer to create
// its display. smithay requires `Send` because `EGLDisplay` may move between threads.
unsafe impl Send for WaylandDisplay {}

impl EGLNativeDisplay for WaylandDisplay {
    fn supported_platforms(&self) -> Vec<EGLPlatform<'_>> {
        let display = self.0;
        vec![
            // https://registry.khronos.org/EGL/extensions/KHR/EGL_KHR_platform_wayland.txt
            egl_platform!(PLATFORM_WAYLAND_KHR, display, &["EGL_KHR_platform_wayland"]),
            // https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_platform_wayland.txt
            egl_platform!(PLATFORM_WAYLAND_EXT, display, &["EGL_EXT_platform_wayland"]),
        ]
    }

    fn identifier(&self) -> Option<String> {
        Some("swaywardlock/Wayland".into())
    }
}

/// The EGL display and the renderer shared by every lock surface.
pub struct Gpu {
    // Field order matters: the renderer (and its context) drop before the display.
    pub renderer: GlesRenderer,
    display: EGLDisplay,
}

impl Gpu {
    /// Create the EGL display, context and renderer for `conn`.
    ///
    /// `conn` must outlive the returned value.
    pub fn new(conn: &Connection) -> anyhow::Result<Self> {
        let ptr = conn.backend().display_ptr() as *mut c_void;
        // SAFETY: the pointer is a live `wl_display` owned by `conn`, which the caller keeps alive.
        let display = unsafe { EGLDisplay::new(WaylandDisplay(ptr)) }
            .context("error creating EGL display")?;

        let attributes = GlAttributes {
            version: (3, 0),
            profile: None,
            debug: false,
            // Frame callbacks pace rendering; a blocking swap would stall the event loop.
            vsync: false,
        };
        let context =
            EGLContext::new_with_config(&display, attributes, PixelFormatRequirements::_8_bit())
                .or_else(|_| {
                    // Software rasterisers (llvmpipe on CI) are not "hardware accelerated".
                    let mut reqs = PixelFormatRequirements::_8_bit();
                    reqs.hardware_accelerated = None;
                    EGLContext::new_with_config(&display, attributes, reqs)
                })
                .context("error creating EGL context")?;

        // SAFETY: the context was created on this thread and is not current anywhere else.
        let renderer = unsafe { GlesRenderer::new(context) }.context("error creating renderer")?;

        Ok(Self { renderer, display })
    }

    /// Create an EGL surface for a lock surface of `width` x `height` buffer pixels.
    pub fn create_surface(
        &self,
        surface: &WlSurface,
        width: i32,
        height: i32,
    ) -> anyhow::Result<EGLSurface> {
        let wl_egl = WlEglSurface::new(surface.id(), width, height)
            .context("error creating wl_egl_window")?;
        let context = self.renderer.egl_context();
        let pixel_format = context
            .pixel_format()
            .context("EGL context has no pixel format")?;
        // SAFETY: the config comes from the renderer's context on the same display.
        unsafe { EGLSurface::new(&self.display, pixel_format, context.config_id(), wl_egl) }
            .context("error creating EGL surface")
    }
}
