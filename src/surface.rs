//! One lock surface per output: its EGL surface, damage tracker and widget textures.

use anyhow::Context as _;
use smithay::backend::egl::EGLSurface;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind as _, Color32F, Offscreen as _};
use smithay::utils::{Buffer, Logical, Physical, Point, Size, Transform};
use smithay_client_toolkit::session_lock::SessionLockSurface;
use wayland_client::protocol::wl_output::WlOutput;

use crate::config::{Background, Config, HAlign, RingIndicator, VAlign, Widget};
use crate::render::background::{self, Image};
use crate::render::blur::{Blur, BlurProgram};
use crate::render::ring::{self, RingFrame};
use crate::render::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render::texture::{TextureBuffer, TextureRenderElement};
use crate::render::{cairo_to_memory, LockElement};
use crate::state::LockState;

pub struct OutputInfo {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// A rendered ring and the logical offset of its centre within the texture.
type RingTexture = (TextureBuffer<GlesTexture>, (f64, f64));

/// A widget's rendered texture, cached until its inputs change.
enum Cached {
    Background {
        tex: Option<TextureBuffer<GlesTexture>>,
    },
    Ring {
        frame: Option<RingFrame>,
        tex: Option<RingTexture>,
    },
}

pub struct Surface {
    // Fields drop in declaration order: the EGL surface (and its wl_egl_window) must go before
    // the wl_surface, which is destroyed with `lock_surface`.
    egl: Option<EGLSurface>,
    damage: Option<OutputDamageTracker>,
    pub lock_surface: SessionLockSurface,
    pub output: WlOutput,
    /// Logical size from `configure`.
    pub size: Size<i32, Logical>,
    pub scale: i32,
    /// Indices into `config.widgets` that apply to this output, in draw order.
    widgets: Vec<usize>,
    cache: Vec<Cached>,
    fallback: SolidColorBuffer,
}

impl Surface {
    pub fn new(lock_surface: SessionLockSurface, output: WlOutput) -> Self {
        Self {
            lock_surface,
            output,
            egl: None,
            damage: None,
            size: Size::default(),
            scale: 1,
            widgets: Vec::new(),
            cache: Vec::new(),
            fallback: SolidColorBuffer::default(),
        }
    }

    fn buffer_size(&self) -> Size<i32, Physical> {
        self.size.to_physical(self.scale)
    }

    /// Pick this output's widgets and forget cached textures. Called on configure and on scale
    /// changes.
    pub fn layout(&mut self, config: &Config, info: &OutputInfo) {
        let mut chosen: Vec<usize> = Vec::new();
        let mut background = None;
        for (i, w) in config.widgets.iter().enumerate() {
            let applies = w
                .common()
                .output
                .as_ref()
                .is_none_or(|m| m.matches(info.name.as_deref(), info.description.as_deref()));
            if !applies {
                continue;
            }
            match w {
                // One background per output; the last match wins (spec).
                Widget::Background(_) => background = Some(i),
                _ => chosen.push(i),
            }
        }
        chosen.extend(background);
        // Stable sort keeps file order for equal z-index.
        chosen.sort_by_key(|&i| config.widgets[i].common().z_index);

        self.cache = chosen
            .iter()
            .map(|&i| match &config.widgets[i] {
                Widget::Background(_) => Cached::Background { tex: None },
                Widget::RingIndicator(_) => Cached::Ring {
                    frame: None,
                    tex: None,
                },
            })
            .collect();
        self.widgets = chosen;
        self.fallback
            .update(self.size.to_f64(), Color32F::new(0., 0., 0., 1.));
    }

    /// Create or resize the EGL surface to match the current size and scale.
    pub fn ensure_egl(&mut self, gpu: &crate::egl::Gpu) -> anyhow::Result<()> {
        let size = self.buffer_size();
        match &self.egl {
            Some(egl) => {
                egl.resize(size.w, size.h, 0, 0);
            }
            None => {
                let wl = self.lock_surface.wl_surface();
                self.egl = Some(gpu.create_surface(wl, size.w, size.h)?);
            }
        }
        wl_set_buffer_scale(&self.lock_surface, self.scale);
        // EGL window surfaces are y-up; smithay renders them with Flipped180, as in its winit
        // examples. Without it everything is upside down (symmetric widgets hide this).
        self.damage = Some(OutputDamageTracker::new(
            size,
            f64::from(self.scale),
            Transform::Flipped180,
        ));
        Ok(())
    }

    /// Draw a frame if anything changed. Returns true if a buffer was swapped.
    pub fn draw(
        &mut self,
        renderer: &mut GlesRenderer,
        blur: Option<&BlurProgram>,
        config: &Config,
        images: &Images,
        state: &LockState,
    ) -> anyhow::Result<bool> {
        let (Some(egl), Some(damage)) = (self.egl.as_mut(), self.damage.as_mut()) else {
            return Ok(false);
        };
        let scale = f64::from(self.scale);
        let size = self.size.to_f64();

        let mut elements: Vec<LockElement> = Vec::new();
        for (slot, &i) in self.cache.iter_mut().zip(&self.widgets) {
            match (&config.widgets[i], slot) {
                (Widget::Background(bg), Cached::Background { tex }) => {
                    if tex.is_none() {
                        // A widget that fails to render is skipped, never the whole frame: the
                        // fallback colour and the other widgets still reach the screen.
                        match render_background(renderer, blur, bg, images, self.size, self.scale) {
                            Ok(t) => *tex = Some(t),
                            Err(err) => tracing::warn!("background: {err:#}"),
                        }
                    }
                    let Some(buf) = tex.clone() else { continue };
                    elements.push(LockElement::Texture(
                        TextureRenderElement::from_texture_buffer(
                            buf,
                            (0., 0.),
                            1.,
                            None,
                            Some(size),
                            Kind::Unspecified,
                        ),
                    ));
                }
                (Widget::RingIndicator(r), Cached::Ring { frame, tex }) => {
                    let now = RingFrame::new(r, state);
                    if frame.as_ref() != Some(&now) {
                        *tex = render_ring(renderer, r, &now, scale).unwrap_or_else(|err| {
                            tracing::warn!("ring-indicator: {err:#}");
                            None
                        });
                        *frame = Some(now);
                    }
                    if let Some((buf, (cx, cy))) = tex.clone() {
                        let anchor = anchor(r, size);
                        // swaylock nudges the indicator right by `2 / scale` logical pixels
                        // (integer division, render.c `subsurf_xpos`); match it for pixel parity.
                        let nudge = f64::from(2 / self.scale);
                        let loc =
                            Point::<f64, Logical>::from((anchor.x - cx + nudge, anchor.y - cy));
                        elements.push(LockElement::Texture(
                            TextureRenderElement::from_texture_buffer(
                                buf,
                                loc,
                                1.,
                                None,
                                None,
                                Kind::Unspecified,
                            ),
                        ));
                    }
                }
                _ => unreachable!("cache built from the same widget list"),
            }
        }
        // Elements are front to back for the damage tracker.
        elements.reverse();
        elements.push(LockElement::Solid(SolidColorRenderElement::from_buffer(
            &self.fallback,
            (0., 0.),
            1.,
            Kind::Unspecified,
        )));

        // Age 0 means "redraw everything". The real age needs the surface current, which only
        // happens inside `bind`, and the target then borrows the surface. Lock screens redraw
        // rarely, so full redraws are fine until this shows up in a profile.
        let age = 0;
        let mut target = renderer.bind(egl)?;
        let result = damage
            .render_output(
                renderer,
                &mut target,
                age,
                &elements,
                Color32F::new(0., 0., 0., 1.),
            )
            .map_err(|e| anyhow::anyhow!("render error: {e:?}"))?;
        let damage_rects = result.damage.cloned();
        drop(target);

        match damage_rects {
            None => Ok(false),
            Some(mut rects) => {
                // EGL damage is y-up.
                let h = self.size.to_physical(self.scale).h;
                for r in &mut rects {
                    r.loc.y = h - r.loc.y - r.size.h;
                }
                egl.swap_buffers(Some(&mut rects)).context("swap_buffers")?;
                Ok(true)
            }
        }
    }
}

fn wl_set_buffer_scale(lock_surface: &SessionLockSurface, scale: i32) {
    lock_surface.wl_surface().set_buffer_scale(scale);
}

/// Where a widget's anchor point is, in logical coordinates (y down), from its alignment and
/// position. Positive `position.y` moves up, as in hyprlock.
fn anchor(ring: &RingIndicator, size: Size<f64, Logical>) -> Point<f64, Logical> {
    let c = &ring.common;
    let x = match c.halign {
        HAlign::Left => ring.radius + ring.thickness,
        HAlign::Center | HAlign::None => size.w / 2.,
        HAlign::Right => size.w - ring.radius - ring.thickness,
    };
    let y = match c.valign {
        VAlign::Top => ring.radius + ring.thickness,
        VAlign::Center | VAlign::None => size.h / 2.,
        VAlign::Bottom => size.h - ring.radius - ring.thickness,
    };
    Point::from((
        x + c.position.0.resolve(size.w),
        y - c.position.1.resolve(size.h),
    ))
}

/// Decoded images, keyed by path, shared by all outputs.
#[derive(Default)]
pub struct Images(std::collections::HashMap<std::path::PathBuf, Image>);

impl Images {
    pub fn load_all(config: &Config) -> Self {
        let mut images = std::collections::HashMap::new();
        for w in &config.widgets {
            if let Widget::Background(Background {
                path: Some(path), ..
            }) = w
            {
                if images.contains_key(path) {
                    continue;
                }
                match Image::load(path) {
                    Ok(img) => {
                        images.insert(path.clone(), img);
                    }
                    // A missing image falls back to the colour; never refuse to lock over it.
                    Err(err) => tracing::warn!("{err:#}"),
                }
            }
        }
        Self(images)
    }
}

fn render_background(
    renderer: &mut GlesRenderer,
    blur: Option<&BlurProgram>,
    bg: &Background,
    images: &Images,
    size: Size<i32, Logical>,
    scale: i32,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    // Test builds can force this widget to fail, to check one failing widget does not blank the
    // output (the GPU texture size limit is a real-world trigger).
    #[cfg(feature = "test-auth")]
    if std::env::var_os("SWAYWARDLOCK_TEST_FAIL_BACKGROUND").is_some() {
        anyhow::bail!("forced failure (SWAYWARDLOCK_TEST_FAIL_BACKGROUND)");
    }
    let phys = size.to_physical(scale);
    let image = bg.path.as_ref().and_then(|p| images.0.get(p));
    let surface = background::render(bg.color, image, bg.fit, phys.w, phys.h)?;
    let mem = cairo_to_memory(surface, f64::from(scale))?;
    let mut buf = TextureBuffer::from_memory_buffer(renderer, &mem)?;

    if !bg.blur.off {
        match blur.and_then(|program| Blur::new(renderer, program.clone())) {
            Some(mut b) => {
                let options = bg.blur.into();
                let src = buf.texture().clone();
                b.prepare_textures(
                    |fourcc, s: Size<i32, Buffer>| renderer.create_buffer(fourcc, s),
                    &src,
                    options,
                )?;
                let blurred = b.render(renderer, &src, options)?;
                buf = TextureBuffer::from_texture(
                    renderer,
                    blurred,
                    f64::from(scale),
                    Transform::Normal,
                    Vec::new(),
                );
            }
            None => tracing::warn!("blur shader unavailable; drawing the background unblurred"),
        }
    }
    Ok(buf)
}

fn render_ring(
    renderer: &mut GlesRenderer,
    ring: &RingIndicator,
    frame: &RingFrame,
    scale: f64,
) -> anyhow::Result<Option<RingTexture>> {
    let Some((surface, centre)) = ring::render(ring, frame, scale)? else {
        return Ok(None);
    };
    let mem = cairo_to_memory(surface, scale)?;
    let buf = TextureBuffer::from_memory(
        renderer,
        mem.data(),
        mem.format(),
        mem.size(),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;
    Ok(Some((buf, centre)))
}
