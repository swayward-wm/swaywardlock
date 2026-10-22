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
        // wayland-sys loads libwayland-egl lazily on the first `WlEglSurface::new`, which runs on
        // the first configure after `lock()`, and panics if it cannot. Check here, before locking.
        anyhow::ensure!(
            wayland_egl::is_available(),
            "libwayland-egl cannot be loaded"
        );
        let ptr = conn.backend().display_ptr() as *mut c_void;
        // SAFETY: the pointer is a live `wl_display` owned by `conn`, which the caller keeps alive.
        let display = unsafe { EGLDisplay::new(WaylandDisplay(ptr)) }
            .context("error creating EGL display")?;

        let attributes = GlAttributes {
            version: (3, 0),
            profile: None,
            debug: false,
            // No frame callbacks are used (redraws are event driven); a vsynced swap would stall
            // the event loop.
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
        let surface =
            unsafe { EGLSurface::new(&self.display, pixel_format, context.config_id(), wl_egl) }
                .context("error creating EGL surface")?;
        // `vsync: false` above only picks a config; Mesa still defaults to swap interval 1, where
        // eglSwapBuffers waits for the previous frame callback. A lock surface on a powered-off
        // output gets none, so the next redraw would block the main thread (keyboard, auth and
        // SIGUSR1 included) until the output wakes. The interval is per surface.
        // SAFETY: main thread; the renderer binds its surface again before every draw.
        unsafe {
            context
                .make_current_with_surface(&surface)
                .context("error making the EGL surface current")?;
            if ffi::egl::SwapInterval(**self.display.get_display_handle(), 0) == ffi::egl::FALSE {
                tracing::warn!("eglSwapInterval(0) failed; a dark output may stall redraws");
            }
        }
        Ok(surface)
    }

    /// Create and drop an EGL surface on `scratch`, a fresh role-less `wl_surface`, so window
    /// surface setup fails here, before `lock()`, rather than on the first lock surface.
    pub fn check_surface(&self, scratch: &WlSurface) -> anyhow::Result<()> {
        let surface = self.create_surface(scratch, 1, 1)?;
        // SAFETY: main thread. Release the surface before dropping it.
        unsafe { self.renderer.egl_context().make_current() }
            .context("error making the EGL context current")?;
        drop(surface);
        Ok(())
    }
}
