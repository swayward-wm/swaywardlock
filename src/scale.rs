//! Fractional scale: `wp_fractional_scale_v1` for the preferred scale, `wp_viewporter` to map a
//! buffer of `round(logical * scale)` pixels onto the logical size.
//!
//! Both globals are needed; with either missing, surfaces use the integer `wl_surface` preferred
//! buffer scale instead. Each surface gets its objects once, at creation, and they are destroyed
//! before the `wl_surface` (see the field order in `Surface`).

use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy as _, QueueHandle};

use crate::App;

pub struct Managers {
    fractional: WpFractionalScaleManagerV1,
    viewporter: WpViewporter,
}

/// One surface's fractional-scale and viewport objects, destroyed on drop.
pub struct PerSurface {
    fractional: WpFractionalScaleV1,
    pub viewport: WpViewport,
}

impl Drop for PerSurface {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.fractional.destroy();
    }
}

/// User data for objects whose events carry nothing we use.
pub struct Ignore;

/// User data for a surface's `wp_fractional_scale_v1`: the surface it scales.
pub struct ScaleData(WlSurface);

impl Managers {
    /// Both globals, or None (integer scale).
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<Self> {
        // Test builds can hide the globals, to check the integer fallback.
        #[cfg(feature = "test-auth")]
        if std::env::var_os("SWAYWARDLOCK_TEST_NO_FRACTIONAL").is_some() {
            return None;
        }
        let fractional = globals.bind(qh, 1..=1, Ignore).ok()?;
        let viewporter = globals.bind(qh, 1..=1, Ignore).ok()?;
        Some(Self {
            fractional,
            viewporter,
        })
    }

    pub fn attach(&self, surface: &WlSurface, qh: &QueueHandle<App>) -> PerSurface {
        PerSurface {
            fractional: self.fractional.get_fractional_scale(
                surface,
                qh,
                ScaleData(surface.clone()),
            ),
            viewport: self.viewporter.get_viewport(surface, qh, Ignore),
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl smithay_client_toolkit::dispatch2::Dispatch2<$t, App> for Ignore {
            fn event(
                &self,
                _: &mut App,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &Connection,
                _: &QueueHandle<App>,
            ) {
            }
        }
    )*};
}
ignore!(WpFractionalScaleManagerV1, WpViewporter, WpViewport);

impl smithay_client_toolkit::dispatch2::Dispatch2<WpFractionalScaleV1, App> for ScaleData {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
            return;
        };
        // The protocol sends the scale in 120ths.
        let scale = f64::from(scale) / 120.;
        tracing::debug!("fractional scale {scale} for {}", self.0.id());
        app.set_scale(&self.0, scale);
    }
}
