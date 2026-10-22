//! One lock surface per output (or the preview window): its EGL surface, damage tracker and
//! widget textures.

use anyhow::Context as _;
use smithay::backend::egl::EGLSurface;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind as _, Color32F, Offscreen as _};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};
use smithay_client_toolkit::session_lock::SessionLockSurface;
use smithay_client_toolkit::shell::xdg::window::Window;
use smithay_client_toolkit::shell::WaylandSurface as _;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;

use crate::config::{Background, Color, Config, HAlign, InputField, RingIndicator, VAlign, Widget};
pub use crate::render::background::Image;
use crate::render::blur::Blur;
use crate::render::ring::{self, RingFrame};
use crate::render::shape::{self, RoundedBox};
use crate::render::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render::texture::{TextureBuffer, TextureRenderElement};
use crate::render::{background, cairo_to_memory, LockElement};
use crate::state::LockState;

#[derive(Clone, Default)]
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
    Shape(Box<RoundedBox>),
    Label {
        /// The text last rendered; re-render when the substituted text changes.
        text: Option<String>,
        tex: Option<TextureBuffer<GlesTexture>>,
    },
    Image {
        tex: Option<TextureBuffer<GlesTexture>>,
        frame: Box<RoundedBox>,
    },
    InputField(Box<FieldCache>),
}

/// Render state for one input field: its box, a box per dot, and the text texture.
pub struct FieldCache {
    body: RoundedBox,
    fill: RoundedBox,
    dots: Vec<RoundedBox>,
    hidden: RoundedBox,
    text: Option<(String, Color, Option<TextureBuffer<GlesTexture>>)>,
    /// The dots text texture, keyed by the (colour, size) it was rendered at.
    #[allow(clippy::type_complexity)]
    dot_text: Option<((Color, f64), Option<TextureBuffer<GlesTexture>>)>,
}

/// Per-frame values the widgets read that live outside the config: label variables and the
/// latest `cmd[]` outputs, indexed like `config.widgets`.
pub struct Dynamic<'a> {
    pub vars: crate::textvars::Vars<'a>,
    pub cmd_output: &'a [Option<String>],
    /// Whole-screen opacity for fade-in/out, 0..1. Drawn as a black overlay at `1 - opacity`.
    pub opacity: f32,
}

/// What the widgets are drawn into.
pub enum Role {
    /// A lock surface on a real output.
    Lock {
        lock_surface: SessionLockSurface,
        output: WlOutput,
    },
    /// `--preview`: an ordinary window standing in for one output, named for `output=` matching.
    Preview { window: Window, info: OutputInfo },
}

pub struct Surface {
    // Fields drop in declaration order: the EGL surface (and its wl_egl_window) must go before
    // the wl_surface, which is destroyed with `role`.
    egl: Option<EGLSurface>,
    damage: Option<OutputDamageTracker>,
    pub role: Role,
    /// Logical size from `configure`.
    pub size: Size<i32, Logical>,
    pub scale: i32,
    /// Indices into `config.widgets` that apply to this output, in draw order.
    widgets: Vec<usize>,
    cache: Vec<Cached>,
    fallback: SolidColorBuffer,
    fade: SolidColorBuffer,
    /// Screenshot of this output taken before locking, for `background { screenshot }`.
    pub screenshot: Option<Image>,
}

impl Surface {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            egl: None,
            damage: None,
            size: Size::default(),
            scale: 1,
            widgets: Vec::new(),
            cache: Vec::new(),
            fallback: SolidColorBuffer::default(),
            fade: SolidColorBuffer::default(),
            screenshot: None,
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
                Widget::Shape(_) => Cached::Shape(Box::new(RoundedBox::new())),
                Widget::Label(_) => Cached::Label {
                    text: None,
                    tex: None,
                },
                Widget::InputField(_) => Cached::InputField(Box::new(FieldCache {
                    body: RoundedBox::new(),
                    fill: RoundedBox::new(),
                    dots: Vec::new(),
                    hidden: RoundedBox::new(),
                    text: None,
                    dot_text: None,
                })),
                Widget::Image(_) => Cached::Image {
                    tex: None,
                    frame: Box::new(RoundedBox::new()),
                },
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
                self.egl = Some(gpu.create_surface(self.wl_surface(), size.w, size.h)?);
            }
        }
        self.wl_surface().set_buffer_scale(self.scale);
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
        config: &Config,
        images: &Images,
        state: &LockState,
        dynamic: &Dynamic<'_>,
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
                        let image = if bg.screenshot {
                            self.screenshot.as_ref()
                        } else {
                            bg.path.as_ref().and_then(|p| images.0.get(p))
                        };
                        match render_background(renderer, bg, image, self.size, self.scale) {
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
                        // The ring is placed as a box of its diameter; the texture can be wider
                        // (long text), so align by the ring's centre within it.
                        let d = 2. * (r.radius + r.thickness);
                        let corner = place(&r.common, Size::from((d, d)), size);
                        // swaylock nudges the indicator right by `2 / scale` logical pixels
                        // (integer division, render.c `subsurf_xpos`); match it for pixel parity.
                        let nudge = f64::from(2 / self.scale);
                        let loc = Point::<f64, Logical>::from((
                            corner.x + d / 2. - cx + nudge,
                            corner.y + d / 2. - cy,
                        ));
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
                (Widget::Shape(sh), Cached::Shape(rbox)) => {
                    let w_size = Size::from((sh.size.0.resolve(size.w), sh.size.1.resolve(size.h)));
                    let loc = place(&sh.common, w_size, size);
                    let params = shape::Params {
                        size: w_size,
                        fill: (!sh.xray).then_some(sh.color),
                        border: sh.border,
                        radius: sh.corner_radius,
                        shadow: sh.shadow,
                        scale,
                        alpha: 1.,
                    };
                    rbox.render(renderer, params, loc, &mut |e| elements.push(e.into()));
                }
                (Widget::Label(l), Cached::Label { text, tex }) => {
                    // Command output is shown as is: its variables were substituted into the
                    // command before it ran.
                    let now = match &dynamic.cmd_output[i] {
                        Some(out) => out.clone(),
                        None if l.text.starts_with("cmd[") => String::new(),
                        None => crate::textvars::substitute(&l.text, &dynamic.vars),
                    };
                    if text.as_deref() != Some(now.as_str()) {
                        *tex = crate::render::text::render(
                            &now, &l.font, l.color, l.align, l.markup, scale,
                        )
                        .and_then(|s| s.map(|s| upload(renderer, s, scale)).transpose())
                        .unwrap_or_else(|err| {
                            tracing::warn!("label: {err:#}");
                            None
                        });
                        *text = Some(now);
                    }
                    if let Some(buf) = tex.clone() {
                        let loc = place(&l.common, buf.logical_size(), size);
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
                (Widget::Image(im), Cached::Image { tex, frame }) => {
                    let Some(img) = images.0.get(&im.path) else {
                        continue;
                    };
                    let (iw, ih) = img.size();
                    // hyprlock: scale so the shorter side is `size`, keeping the aspect ratio.
                    let k = im.size / f64::from(iw.min(ih));
                    let inner = Size::<f64, Logical>::from((f64::from(iw) * k, f64::from(ih) * k));
                    let bw = im.border.map_or(0., |b| b.width);
                    let outer = inner + Size::from((2. * bw, 2. * bw));
                    let loc = place(&im.common, outer, size);
                    let outer_radius = shape::resolve_radius(im.corner_radius, outer);
                    let inner_radius = outer_radius.expanded_by(-(bw as f32));
                    if tex.is_none() {
                        let phys = inner.to_physical_precise_round(scale);
                        let r = <[f32; 4]>::from(inner_radius).map(|v| f64::from(v) * scale);
                        *tex = crate::render::background::render_clipped(img, phys.w, phys.h, r)
                            .and_then(|s| upload(renderer, s, scale))
                            .map_err(|err| tracing::warn!("image: {err:#}"))
                            .ok();
                    }
                    // Shadow and border behind/around the picture, via the shape renderer.
                    let params = shape::Params {
                        size: outer,
                        fill: None,
                        border: im.border,
                        radius: Some(outer_radius),
                        shadow: im.shadow,
                        scale,
                        alpha: 1.,
                    };
                    frame.render(renderer, params, loc, &mut |e| elements.push(e.into()));
                    if let Some(buf) = tex.clone() {
                        elements.push(LockElement::Texture(
                            TextureRenderElement::from_texture_buffer(
                                buf,
                                (loc.x + bw, loc.y + bw),
                                1.,
                                None,
                                Some(inner),
                                Kind::Unspecified,
                            ),
                        ));
                    }
                }
                (Widget::InputField(f), Cached::InputField(c)) => {
                    draw_input_field(renderer, f, c, state, dynamic, size, scale, &mut elements);
                }
                _ => unreachable!("cache built from the same widget list"),
            }
        }
        // Elements are front to back for the damage tracker.
        elements.reverse();
        if dynamic.opacity < 1. {
            self.fade.update(
                size,
                Color32F::new(0., 0., 0., 1. - dynamic.opacity.clamp(0., 1.)),
            );
            elements.insert(
                0,
                LockElement::Solid(SolidColorRenderElement::from_buffer(
                    &self.fade,
                    (0., 0.),
                    1.,
                    Kind::Unspecified,
                )),
            );
        }
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

impl Surface {
    pub fn wl_surface(&self) -> &WlSurface {
        match &self.role {
            Role::Lock { lock_surface, .. } => lock_surface.wl_surface(),
            Role::Preview { window, .. } => window.wl_surface(),
        }
    }

    pub fn output(&self) -> Option<&WlOutput> {
        match &self.role {
            Role::Lock { output, .. } => Some(output),
            Role::Preview { .. } => None,
        }
    }
}

/// Top-left corner (logical, y down) of a `size` box placed by `common`'s alignment and position.
/// Follows hyprlock's `posFromHVAlign`: the box sits against the chosen edge (or centred), then
/// moves by `position`, where positive y moves up. `none` on an axis means no edge: the position
/// alone, measured from the left / bottom as in hyprlock.
pub fn place(
    common: &crate::config::Common,
    size: Size<f64, Logical>,
    output: Size<f64, Logical>,
) -> Point<f64, Logical> {
    let x = match common.halign {
        HAlign::Left | HAlign::None => 0.,
        HAlign::Center => (output.w - size.w) / 2.,
        HAlign::Right => output.w - size.w,
    };
    let y = match common.valign {
        VAlign::Top => 0.,
        VAlign::Center => (output.h - size.h) / 2.,
        VAlign::Bottom | VAlign::None => output.h - size.h,
    };
    Point::from((
        x + common.position.0.resolve(output.w),
        y - common.position.1.resolve(output.h),
    ))
}

/// Decoded images, keyed by path, shared by all outputs.
#[derive(Default)]
pub struct Images(std::collections::HashMap<std::path::PathBuf, Image>);

impl Images {
    pub fn load_all(config: &Config) -> Self {
        let mut images = std::collections::HashMap::new();
        for w in &config.widgets {
            let path = match w {
                Widget::Background(Background { path: Some(p), .. }) => Some(p),
                Widget::Image(im) => Some(&im.path),
                _ => None,
            };
            if let Some(path) = path {
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
    bg: &Background,
    image: Option<&Image>,
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
    let mut surface = background::render(bg.color, image, bg.fit, phys.w, phys.h)?;
    background::prepare(&mut surface, bg.contrast, bg.brightness)?;
    // hyprlock dims after the blur; dimming is a uniform scale, which commutes with the linear
    // blur, so it can run here.
    background::finish(&mut surface, bg.brightness)?;
    let mem = cairo_to_memory(surface, f64::from(scale))?;
    let mut buf = TextureBuffer::from_memory_buffer(renderer, &mem)?;

    if !bg.blur.off {
        match Blur::new(renderer) {
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

#[allow(clippy::too_many_arguments)]
fn draw_input_field(
    renderer: &mut GlesRenderer,
    f: &InputField,
    c: &mut FieldCache,
    state: &LockState,
    dynamic: &Dynamic<'_>,
    output: Size<f64, Logical>,
    scale: f64,
    elements: &mut Vec<LockElement>,
) {
    use crate::render::input_field::{dot_rects, dot_size, Look};
    let fail_shown = state.auth == crate::state::AuthState::Invalid;
    let look = Look::new(f, state, fail_shown);
    let size = Size::<f64, Logical>::from((f.size.0.resolve(output.w), f.size.1.resolve(output.h)));
    let ow = f.outline.width;
    let outer = size + Size::from((2. * ow, 2. * ow));
    let loc = place(&f.common, outer, output);
    let inner_loc = Point::<f64, Logical>::from((loc.x + ow, loc.y + ow));
    let outer_radius =
        shape::resolve_radius(f.corner_radius.map(|r| r.expanded_by(ow as f32)), outer);
    let inner_radius = outer_radius.expanded_by(-(ow as f32));

    // Front-to-back order is reversed at the end of `draw`, so push back to front.
    let mut push = |e: crate::render::shape::Element| elements.push(e.into());

    // Body: inner fill plus the outline (an outline border around a filled box).
    c.body.render(
        renderer,
        shape::Params {
            size: outer,
            fill: None,
            border: (ow > 0.).then_some(crate::config::Border {
                width: ow,
                gradient: look.outline,
            }),
            radius: Some(outer_radius),
            shadow: f.shadow,
            scale,
            alpha: 1.,
        },
        loc,
        &mut push,
    );
    c.fill.render(
        renderer,
        shape::Params {
            size,
            fill: Some(look.inner),
            border: None,
            radius: Some(inner_radius),
            shadow: crate::config::Shadow::default(),
            scale,
            alpha: 1.,
        },
        inner_loc,
        &mut push,
    );

    // Hide-input: a quarter of the outline in a random colour per keystroke. As in hyprlock,
    // the whole outline is drawn and clipped to the quadrant.
    if let Some((colour, quadrant)) = look.hidden {
        if ow > 0. {
            let q = Size::<f64, Logical>::from((outer.w / 2., outer.h / 2.));
            let qloc = Point::<f64, Logical>::from((
                loc.x + if quadrant % 2 == 1 { q.w } else { 0. },
                loc.y + if quadrant > 1 { q.h } else { 0. },
            ));
            let clip = Rectangle::new(qloc, q).to_physical_precise_round(scale);
            c.hidden.render(
                renderer,
                shape::Params {
                    size: outer,
                    fill: None,
                    border: Some(crate::config::Border {
                        width: ow,
                        gradient: colour.into(),
                    }),
                    radius: Some(outer_radius),
                    shadow: crate::config::Shadow::default(),
                    scale,
                    alpha: 1.,
                },
                loc,
                &mut |e| {
                    if let shape::Element::Border(b) = e {
                        if let Some(e) = CropRenderElement::from_element(b, scale, clip) {
                            elements.push(LockElement::CroppedBorder(e));
                        }
                    }
                },
            );
        }
    } else if look.dots > 0 {
        match &f.dots.text {
            Some(t) => {
                // Re-render when the colour (fail, check) or the size changes.
                let d = dot_size(f, size.h);
                let key = (look.font, d);
                if c.dot_text.as_ref().is_none_or(|(k, _)| *k != key) {
                    let font = format!("{} {}", f.font, d);
                    let buf = crate::render::text::render(
                        t,
                        &font,
                        look.font,
                        crate::config::TextAlign::Center,
                        false,
                        scale,
                    )
                    .ok()
                    .flatten()
                    .and_then(|s| upload(renderer, s, scale).ok());
                    c.dot_text = Some((key, buf));
                }
                if let Some((_, Some(buf))) = &c.dot_text {
                    let (rects, _) = dot_rects(f, size, look.dots, Some(buf.logical_size()));
                    for r in &rects {
                        elements.push(LockElement::Texture(
                            TextureRenderElement::from_texture_buffer(
                                buf.clone(),
                                (inner_loc.x + r.loc.x, inner_loc.y + r.loc.y),
                                1.,
                                None,
                                Some(r.size),
                                Kind::Unspecified,
                            ),
                        ));
                    }
                }
            }
            None => {
                let (rects, radius) = dot_rects(f, size, look.dots, None);
                c.dots.resize_with(rects.len(), RoundedBox::new);
                for (b, r) in c.dots.iter_mut().zip(&rects) {
                    b.render(
                        renderer,
                        shape::Params {
                            size: r.size,
                            fill: Some(look.font),
                            border: None,
                            radius: Some(crate::config::CornerRadius::from(radius as f32)),
                            shadow: crate::config::Shadow::default(),
                            scale,
                            alpha: 1.,
                        },
                        Point::from((inner_loc.x + r.loc.x, inner_loc.y + r.loc.y)),
                        &mut |e| elements.push(e.into()),
                    );
                }
            }
        }
    }

    // Placeholder / check / fail text, centred, at a quarter of the field height (hyprlock).
    if let Some(template) = &look.text_template {
        let text = crate::textvars::substitute(template, &dynamic.vars);
        let stale = c
            .text
            .as_ref()
            .is_none_or(|(t, col, _)| *t != text || *col != look.font);
        if stale {
            let font = format!("{} {}", f.font, (size.h / 4.).round());
            let tex = crate::render::text::render(
                &text,
                &font,
                look.font,
                crate::config::TextAlign::Center,
                true,
                scale,
            )
            .ok()
            .flatten()
            .and_then(|s| upload(renderer, s, scale).ok());
            c.text = Some((text, look.font, tex));
        }
        if let Some((_, _, Some(buf))) = &c.text {
            let ts = buf.logical_size();
            // Clip to the field width, as hyprlock scissors it.
            let w = ts.w.min(size.w);
            let src = Rectangle::new(Point::from(((ts.w - w) / 2., 0.)), Size::from((w, ts.h)));
            elements.push(LockElement::Texture(
                TextureRenderElement::from_texture_buffer(
                    buf.clone(),
                    (
                        inner_loc.x + (size.w - w) / 2.,
                        inner_loc.y + (size.h - ts.h) / 2.,
                    ),
                    1.,
                    Some(src),
                    Some(Size::from((w, ts.h))),
                    Kind::Unspecified,
                ),
            ));
        }
    }
}

/// Upload a cairo surface as a texture at `scale`.
fn upload(
    renderer: &mut GlesRenderer,
    surface: pangocairo::cairo::ImageSurface,
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let mem = cairo_to_memory(surface, scale)?;
    Ok(TextureBuffer::from_memory_buffer(renderer, &mem)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Common, HAlign, Length, VAlign};

    fn common(halign: HAlign, valign: VAlign, x: f64, y: f64) -> Common {
        Common {
            output: None,
            position: (Length::Px(x), Length::Px(y)),
            halign,
            valign,
            z_index: 0,
        }
    }

    /// hyprlock's `posFromHVAlign`, with y flipped: positive y moves up from the chosen edge.
    #[test]
    fn place_follows_hyprlock_alignment() {
        let out = Size::from((300., 200.));
        let s = Size::from((20., 10.));
        let at = |c| place(&c, s, out);
        assert_eq!(
            at(common(HAlign::Center, VAlign::Center, 0., 0.)),
            (140., 95.).into()
        );
        assert_eq!(
            at(common(HAlign::Left, VAlign::Top, 5., -5.)),
            (5., 5.).into()
        );
        assert_eq!(
            at(common(HAlign::Right, VAlign::Bottom, -5., 5.)),
            (275., 185.).into()
        );
        // "none": the position alone, from the bottom-left corner, y up.
        assert_eq!(
            at(common(HAlign::None, VAlign::None, 10., 100.)),
            (10., 90.).into()
        );
    }
}
