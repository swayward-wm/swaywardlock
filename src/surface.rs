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

use crate::config::{
    Background, Color, Config, HAlign, ImageWidget, InputField, RingIndicator, RingStyle, VAlign,
    Widget,
};
pub use crate::render::background::Image;
use crate::render::blur::Blur;
use crate::render::field_anim::{FieldAnim, Inputs};
use crate::render::postprocess::{PostProcess, PostProcessProgram};
use crate::render::ring::{self, Look, RingFrame};
use crate::render::ring_anim::RingAnim;
use crate::render::rotated::{RotatedTexture, RotatedTextureElement};
use crate::render::shape::{self, RoundedBox};
use crate::render::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render::texture::{TextureBuffer, TextureRenderElement};
use crate::render::{background, cairo_to_memory, text_shadow, LockElement};
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
        /// The reload generation `tex` was rendered at.
        generation: u64,
        /// The picture a reload replaced, drawn under `tex` while it fades in, and when that
        /// began (animation clock time).
        fading_from: Option<(TextureBuffer<GlesTexture>, std::time::Duration)>,
    },
    Ring(Box<RingCache>),
    /// The border box (border and shadow) and the fill box inside it, and with `rotate` both
    /// rendered into one texture, keyed by the boxes' parameters.
    Shape(Box<[RoundedBox; 2]>, Option<Rotated<[shape::Params; 2]>>),
    Label {
        /// The text last rendered; re-render when the substituted text changes.
        text: Option<String>,
        tex: Option<TextureBuffer<GlesTexture>>,
        /// With `rotate`: `tex` with a transparent border, dropped when the text changes.
        rotated: Option<Rotated<()>>,
        /// With `text-shadow`: the blurred shadow of `tex` and what it was made from; it is
        /// blurred again only when that changes, and kept across a relayout. `None` inside
        /// when the render failed (logged once, the label is drawn without a shadow).
        shadow: Option<(text_shadow::Key, Option<text_shadow::Rendered>)>,
    },
    Image {
        tex: Option<TextureBuffer<GlesTexture>>,
        /// The reload generation `tex` was rendered at.
        generation: u64,
        /// The pixel size of the picture `tex` was rendered from.
        drawn: (i32, i32),
        frame: Box<RoundedBox>,
        /// With `rotate`: picture, border and shadow in one texture, keyed by the frame.
        rotated: Option<Rotated<(shape::Params, bool)>>,
    },
    InputField(Box<FieldCache>),
}

/// A widget drawn rotated: its unrotated elements rendered once into a texture, kept until
/// `key` changes. `tex` is `None` when that render failed (logged once; the widget is skipped).
struct Rotated<K> {
    key: K,
    /// The texture and its top-left relative to the widget's unrotated top-left (logical).
    tex: Option<(RotatedTexture, Point<f64, Logical>)>,
}

/// Render state for one ring-indicator.
#[derive(Default)]
pub struct RingCache {
    /// The frame and (swayward style) look last rendered, and the result.
    frame: Option<RingFrame>,
    look: Option<Look>,
    tex: Option<RingTexture>,
    /// The swayward style's animations.
    anim: RingAnim,
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
    /// Fade, width, dots and colour animations.
    anim: FieldAnim,
}

/// Per-frame values the widgets read that live outside the config: label variables and the
/// latest `cmd[]` outputs, indexed like `config.widgets`.
pub struct Dynamic<'a> {
    pub vars: crate::textvars::Vars<'a>,
    pub cmd_output: &'a [Option<String>],
    /// Which picture each `background` and `image` shows now (reload-ms, reload-cmd).
    pub reload: &'a crate::reload::Reload,
    /// Whole-screen opacity for fade-in/out, 0..1. Drawn as a black overlay at `1 - opacity`.
    pub opacity: f32,
    /// The animation clock, already advanced to this frame.
    pub clock: &'a crate::animation::Clock,
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
    /// wp_fractional_scale + wp_viewport, when the compositor has both; must drop before the
    /// wl_surface.
    pub fractional: Option<crate::scale::PerSurface>,
    pub role: Role,
    /// Logical size from `configure` (the preview starts with a default before its first one).
    pub size: Size<i32, Logical>,
    /// A `configure` has arrived; until then there is nothing to draw into.
    pub configured: bool,
    /// Fractional with wp_fractional_scale, otherwise the integer preferred buffer scale.
    pub scale: f64,
    /// `scale` came from the compositor for this surface, not from the output.
    pub scale_known: bool,
    /// Indices into `config.widgets` that apply to this output, in draw order.
    widgets: Vec<usize>,
    cache: Vec<Cached>,
    /// Set by `draw`: an input-field animation or a background crossfade is running (needs
    /// frames at the redraw rate).
    pub field_animating: bool,
    /// Set by `draw`: when the earliest input-field fade-out timer fires (animation clock time).
    pub field_wake: Option<std::time::Duration>,
    /// Set by `draw`: a swayward-style ring animation is running.
    pub ring_animating: bool,
    /// Set by `draw`: each widget whose new (reloaded) picture it tried, and whether it rendered.
    pub trials: Vec<(usize, bool)>,
    fallback: SolidColorBuffer,
    fade: SolidColorBuffer,
    /// Screenshot of this output taken before locking, for `background { screenshot }`.
    pub screenshot: Option<Image>,
    /// A frame has been swapped onto this surface.
    pub presented: bool,
}

impl Surface {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            egl: None,
            damage: None,
            fractional: None,
            size: Size::default(),
            configured: false,
            scale: 1.,
            scale_known: false,
            widgets: Vec::new(),
            cache: Vec::new(),
            field_animating: false,
            field_wake: None,
            ring_animating: false,
            trials: Vec::new(),
            fallback: SolidColorBuffer::default(),
            fade: SolidColorBuffer::default(),
            screenshot: None,
            presented: false,
        }
    }

    pub fn buffer_size(&self) -> Size<i32, Physical> {
        buffer_size(self.size, self.scale)
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

        // A relayout (scale change) keeps a field's and a ring's running animations, so a typed
        // password does not make the field fade in again.
        let mut old_anims: std::collections::HashMap<usize, FieldAnim> =
            std::collections::HashMap::new();
        let mut old_rings: std::collections::HashMap<usize, RingAnim> =
            std::collections::HashMap::new();
        // And a label's shadow: blurring it again for a configure costs the most of any widget.
        // It is reused only if its key (text, font, shadow, scale) still matches.
        let mut old_shadows: std::collections::HashMap<usize, _> = std::collections::HashMap::new();
        for (i, c) in self
            .widgets
            .iter()
            .copied()
            .zip(std::mem::take(&mut self.cache))
        {
            match c {
                Cached::InputField(f) => {
                    old_anims.insert(i, f.anim);
                }
                Cached::Ring(c) => {
                    old_rings.insert(i, c.anim);
                }
                Cached::Label {
                    shadow: Some(sh), ..
                } => {
                    old_shadows.insert(i, sh);
                }
                _ => {}
            }
        }
        self.cache = chosen
            .iter()
            .map(|&i| match &config.widgets[i] {
                Widget::Background(_) => Cached::Background {
                    tex: None,
                    generation: 0,
                    fading_from: None,
                },
                Widget::Shape(_) => {
                    Cached::Shape(Box::new([RoundedBox::new(), RoundedBox::new()]), None)
                }
                Widget::Label(_) => Cached::Label {
                    text: None,
                    tex: None,
                    rotated: None,
                    shadow: old_shadows.remove(&i),
                },
                Widget::InputField(f) => Cached::InputField(Box::new(FieldCache {
                    body: RoundedBox::new(),
                    fill: RoundedBox::new(),
                    dots: Vec::new(),
                    hidden: RoundedBox::new(),
                    text: None,
                    dot_text: None,
                    anim: old_anims
                        .remove(&i)
                        .unwrap_or_else(|| FieldAnim::new(f.outline.gradient, f.inner_color)),
                })),
                Widget::Image(_) => Cached::Image {
                    tex: None,
                    generation: 0,
                    drawn: (0, 0),
                    frame: Box::new(RoundedBox::new()),
                    rotated: None,
                },
                Widget::RingIndicator(_) => Cached::Ring(Box::new(RingCache {
                    anim: old_rings.remove(&i).unwrap_or_default(),
                    ..RingCache::default()
                })),
            })
            .collect();
        self.widgets = chosen;
        self.fallback
            .update(self.size.to_f64(), Color32F::new(0., 0., 0., 1.));
    }

    /// Create or resize the EGL surface to match the current size and scale. With no usable size
    /// the EGL surface goes, so nothing draws until a configure with one: a buffer of the old
    /// size committed against the acked configure is a protocol error, and the compositor
    /// disconnects the locker.
    pub fn ensure_egl(&mut self, gpu: &crate::egl::Gpu) -> anyhow::Result<()> {
        let size = self.buffer_size();
        if size.w <= 0 || size.h <= 0 {
            self.damage = None;
            self.egl = None;
            anyhow::bail!(
                "no usable size yet ({}x{} logical)",
                self.size.w,
                self.size.h
            );
        }
        let max = crate::render::max_texture_size();
        if size.w > max || size.h > max {
            tracing::warn!(
                "the output is {}x{} px, over the GPU texture limit ({max} px): the background \
                 is drawn at the limit and stretched, and nothing is drawn past {max} px",
                size.w,
                size.h
            );
        }
        match &self.egl {
            Some(egl) => {
                egl.resize(size.w, size.h, 0, 0);
            }
            None => {
                self.egl = Some(gpu.create_surface(self.wl_surface(), size.w, size.h)?);
            }
        }
        match &self.fractional {
            Some(f) => {
                tracing::debug!(
                    "fractional scale: on, {} -> {}x{} buffer for {}x{}",
                    self.scale,
                    size.w,
                    size.h,
                    self.size.w,
                    self.size.h
                );
                // The buffer is in physical pixels; the viewport maps it onto the logical size.
                f.viewport.set_destination(self.size.w, self.size.h);
                self.wl_surface().set_buffer_scale(1);
            }
            None => {
                // Only integer scales reach here (scale_factor_changed).
                let scale = self.scale as i32;
                tracing::debug!("fractional scale: unavailable, using integer scale {scale}");
                self.wl_surface().set_buffer_scale(scale);
            }
        }
        // EGL window surfaces are y-up; smithay renders them with Flipped180, as in its winit
        // examples. Without it everything is upside down (symmetric widgets hide this).
        self.damage = Some(OutputDamageTracker::new(
            size,
            self.scale,
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
        let buffer = self.buffer_size();
        self.trials.clear();
        let (Some(egl), Some(damage)) = (self.egl.as_mut(), self.damage.as_mut()) else {
            return Ok(false);
        };
        let scale = self.scale;
        let size = self.size.to_f64();
        self.field_animating = false;
        self.field_wake = None;
        self.ring_animating = false;

        let mut elements: Vec<LockElement> = Vec::new();
        for (slot, &i) in self.cache.iter_mut().zip(&self.widgets) {
            match (&config.widgets[i], slot) {
                (
                    Widget::Background(bg),
                    Cached::Background {
                        tex,
                        generation,
                        fading_from,
                    },
                ) => {
                    let wanted = dynamic.reload.generation(i);
                    let reloaded = *generation != wanted;
                    if tex.is_none() || reloaded {
                        *generation = wanted;
                        let scale = self.scale;
                        let mut paint = |image: Option<&Image>| {
                            render_background(renderer, bg, image, buffer, scale)
                                .map_err(|err| tracing::warn!("background: {err:#}"))
                                .ok()
                        };
                        // A widget that fails to render is skipped, never the whole frame: the
                        // fallback colour and the other widgets still reach the screen.
                        if bg.screenshot {
                            *tex = paint(self.screenshot.as_ref()).or(tex.take());
                        } else {
                            let configured = bg.path.as_ref().and_then(|p| images.get(p));
                            let trial = dynamic.reload.on_trial(i);
                            let painted = paint(dynamic.reload.newest(i).or(configured));
                            if trial {
                                self.trials.push((i, painted.is_some()));
                            }
                            match painted {
                                Some(t) => {
                                    let old = tex.replace(t);
                                    *fading_from = old
                                        .filter(|_| {
                                            trial && bg.crossfade_ms > 0 && !config.animations.off
                                        })
                                        .map(|old| (old, dynamic.clock.now()));
                                }
                                // A new picture that fails to render leaves the old one up
                                // (rendered again if a relayout dropped it).
                                None if trial && tex.is_none() => {
                                    *tex = paint(dynamic.reload.shown(i).or(configured));
                                }
                                None => {}
                            }
                        }
                    }
                    let Some(buf) = tex.clone() else { continue };
                    let mut alpha = 1.;
                    if let Some((old, start)) = fading_from.as_ref() {
                        let elapsed = dynamic.clock.now().saturating_sub(*start);
                        match crossfade_alpha(elapsed, bg.crossfade_ms) {
                            Some(p) => {
                                alpha = p;
                                self.field_animating = true;
                                elements.push(LockElement::Texture(
                                    TextureRenderElement::from_texture_buffer(
                                        old.clone(),
                                        (0., 0.),
                                        1.,
                                        None,
                                        Some(size),
                                        Kind::Unspecified,
                                    ),
                                ));
                            }
                            None => *fading_from = None,
                        }
                    }
                    elements.push(LockElement::Texture(
                        TextureRenderElement::from_texture_buffer(
                            buf,
                            (0., 0.),
                            alpha,
                            None,
                            Some(size),
                            Kind::Unspecified,
                        ),
                    ));
                }
                (Widget::RingIndicator(r), Cached::Ring(c)) => {
                    let RingCache {
                        frame,
                        look,
                        tex,
                        anim,
                    } = &mut **c;
                    let now = RingFrame::new(r, state);
                    let (alpha, shake) = match r.style {
                        RingStyle::Swaylock => {
                            if frame.as_ref() != Some(&now) {
                                *tex =
                                    render_ring(renderer, r, &now, scale).unwrap_or_else(|err| {
                                        tracing::warn!("ring-indicator: {err:#}");
                                        None
                                    });
                                *frame = Some(now);
                            }
                            (1., 0.)
                        }
                        RingStyle::Swayward => {
                            anim.update(r, &now, dynamic.clock, &config.animations);
                            self.ring_animating |= anim.animating();
                            // Re-render only when the drawn frame or its (quantised) look
                            // changes: a settled ring costs nothing.
                            let wanted = anim.look(r);
                            let drawn = frame.clone().zip(look.clone());
                            if wanted != drawn {
                                *tex = wanted.as_ref().and_then(|(f, l)| {
                                    render_ring_look(renderer, r, f, l, scale)
                                        .map_err(|err| tracing::warn!("ring-indicator: {err:#}"))
                                        .ok()
                                });
                                (*frame, *look) = wanted.unzip();
                            }
                            (anim.alpha(), anim.offset(r))
                        }
                    };
                    if let Some((buf, (cx, cy))) = tex.clone() {
                        // The ring is placed as a box of its diameter; the texture can be wider
                        // (long text, swaylock's padding), so align by its anchor point.
                        let d = 2. * (r.radius + r.thickness);
                        let corner = place(&r.common, Size::from((d, d)), size);
                        let nudge = ring_nudge(self.scale);
                        let loc = Point::<f64, Logical>::from((
                            corner.x + d / 2. - cx + nudge + shake,
                            corner.y + d / 2. - cy,
                        ));
                        elements.push(LockElement::Texture(
                            TextureRenderElement::from_texture_buffer(
                                buf,
                                loc,
                                alpha,
                                None,
                                None,
                                Kind::Unspecified,
                            ),
                        ));
                    }
                }
                (Widget::Shape(sh), Cached::Shape(boxes, rotated)) => {
                    // hyprlock: `size` is the fill; the border goes around it, and the border's
                    // corner radius is the fill's plus the border width (as for input-field).
                    let inner = Size::from((sh.size.0.resolve(size.w), sh.size.1.resolve(size.h)));
                    let bw = sh.border.map_or(0., |b| b.width);
                    let outer = inner + Size::from((2. * bw, 2. * bw));
                    // hyprlock ignores rotate on xray shapes.
                    let angle = if sh.xray { 0. } else { sh.rotate.to_radians() };
                    // An xray shape is placed by its cut-out, the border hanging outside.
                    let loc = if sh.xray {
                        let at = place(&sh.common, inner, size);
                        Point::from((at.x - bw, at.y - bw))
                    } else if angle != 0. {
                        place_rotated_padded(&sh.common, outer, angle, size, scale)
                    } else {
                        place(&sh.common, outer, size)
                    };
                    let outer_radius = shape::resolve_radius(
                        sh.corner_radius.map(|r| r.expanded_by(bw as f32)),
                        outer,
                    );
                    let border_params = shape::Params {
                        size: outer,
                        fill: None,
                        border: sh.border,
                        radius: Some(outer_radius),
                        shadow: sh.shadow,
                        scale,
                        alpha: 1.,
                    };
                    let fill_params = shape::Params {
                        size: inner,
                        fill: Some(sh.color),
                        border: None,
                        radius: sh.corner_radius,
                        shadow: crate::config::Shadow::default(),
                        scale,
                        alpha: 1.,
                    };
                    let [border_box, fill_box] = &mut **boxes;
                    if angle != 0. {
                        let build = |r: &mut GlesRenderer, out: &mut Vec<LockElement>| {
                            let mut push = |e: shape::Element| out.push(e.into());
                            border_box.render(r, border_params, Point::default(), &mut push);
                            fill_box.render(r, fill_params, (bw, bw).into(), &mut push);
                        };
                        let centre = Point::from((loc.x + outer.w / 2., loc.y + outer.h / 2.));
                        elements.extend(draw_rotated(
                            rotated,
                            [border_params, fill_params],
                            renderer,
                            scale,
                            "shape",
                            build,
                            (loc, centre, angle),
                        ));
                        continue;
                    }
                    let mut push = |e: shape::Element| elements.push(e.into());
                    border_box.render(renderer, border_params, loc, &mut push);
                    if !sh.xray {
                        fill_box.render(
                            renderer,
                            fill_params,
                            (loc.x + bw, loc.y + bw).into(),
                            &mut push,
                        );
                    }
                }
                (
                    Widget::Label(l),
                    Cached::Label {
                        text,
                        tex,
                        rotated,
                        shadow,
                    },
                ) => {
                    // Command output is shown as is: its variables were substituted into the
                    // command before it ran.
                    let now = match &dynamic.cmd_output[i] {
                        Some(out) => out.clone(),
                        None if crate::textvars::Template::is_cmd(&l.text) => String::new(),
                        None => crate::textvars::substitute(&l.text, &dynamic.vars),
                    };
                    if text.as_deref() != Some(now.as_str()) {
                        let surface = crate::render::text::render(
                            &now, &l.font, l.color, l.align, l.markup, scale,
                        );
                        // The shadow is made from the text's alpha: a failure costs the shadow.
                        // It is blurred only when what it is made from changed.
                        let key = l.text_shadow.map(|sh| text_shadow::Key {
                            text: now.clone(),
                            font: l.font.clone(),
                            color: l.color,
                            align: l.align,
                            markup: l.markup,
                            shadow: sh,
                            scale,
                        });
                        if key.as_ref() != shadow.as_ref().map(|(k, _)| k) {
                            *shadow = match (&surface, key) {
                                (Ok(Some(s)), Some(key)) => {
                                    let r = text_shadow::render(renderer, s, &key.shadow, scale)
                                        .map_err(|err| {
                                            tracing::warn!("label: text shadow: {err:#}")
                                        })
                                        .ok();
                                    Some((key, r))
                                }
                                _ => None,
                            };
                        }
                        *tex = surface
                            .and_then(|s| s.map(|s| upload(renderer, s, scale)).transpose())
                            .unwrap_or_else(|err| {
                                tracing::warn!("label: {err:#}");
                                None
                            });
                        *text = Some(now);
                        *rotated = None;
                    }
                    // The shadow is behind the text, `pad` px out on every side, moved by the
                    // offset (positive y up, as in `position`).
                    let behind = |loc: Point<f64, Logical>, sh: &text_shadow::Rendered| {
                        let (dx, dy) = l.text_shadow.map_or((0., 0.), |s| s.offset);
                        Point::<f64, Logical>::from((loc.x - sh.pad + dx, loc.y - sh.pad - dy))
                    };
                    let shadow = shadow.as_ref().and_then(|(_, r)| r.as_ref());
                    if let (Some(buf), true) = (tex.clone(), l.rotate != 0.) {
                        // hyprlock places a rotated label by its own size, with no margin.
                        let angle = l.rotate.to_radians();
                        let ls = buf.logical_size();
                        let loc = snap(place_rotated(&l.common, ls, angle, size), scale);
                        let centre = Point::from((loc.x + ls.w / 2., loc.y + ls.h / 2.));
                        if let Some(sh) = shadow {
                            // It turns with the text about the text's centre; the offset then
                            // moves the turned shadow on screen (pivot and corner shift alike).
                            let (dx, dy) = l.text_shadow.map_or((0., 0.), |s| s.offset);
                            elements.push(LockElement::Rotated(RotatedTextureElement::new(
                                sh.rot.clone(),
                                behind(loc, sh),
                                Point::from((centre.x + dx, centre.y - dy)),
                                angle,
                                scale,
                                1.,
                            )));
                        }
                        let build = |_: &mut GlesRenderer, out: &mut Vec<LockElement>| {
                            out.push(LockElement::Texture(
                                TextureRenderElement::from_texture_buffer(
                                    buf,
                                    (0., 0.),
                                    1.,
                                    None,
                                    None,
                                    Kind::Unspecified,
                                ),
                            ));
                        };
                        elements.extend(draw_rotated(
                            rotated,
                            (),
                            renderer,
                            scale,
                            "label",
                            build,
                            (loc, centre, angle),
                        ));
                    } else if let Some(buf) = tex.clone() {
                        let loc = place(&l.common, buf.logical_size(), size);
                        if let Some(sh) = shadow {
                            elements.push(LockElement::Texture(
                                TextureRenderElement::from_texture_buffer(
                                    sh.buf.clone(),
                                    behind(loc, sh),
                                    1.,
                                    None,
                                    None,
                                    Kind::Unspecified,
                                ),
                            ));
                        }
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
                (
                    Widget::Image(im),
                    Cached::Image {
                        tex,
                        generation,
                        drawn,
                        frame,
                        rotated,
                    },
                ) => {
                    let wanted = dynamic.reload.generation(i);
                    let reloaded = *generation != wanted;
                    *generation = wanted;
                    let configured = images.get(&im.path);
                    let newest = dynamic.reload.newest(i).or(configured);
                    if tex.is_none() || reloaded {
                        let fresh = newest.and_then(|img| {
                            Some((render_image(renderer, im, img, scale)?, img.size()))
                        });
                        let trial = dynamic.reload.on_trial(i);
                        if trial {
                            self.trials.push((i, fresh.is_some()));
                        }
                        if let Some((t, from)) = fresh {
                            *tex = Some(t);
                            *drawn = from;
                            *rotated = None;
                        } else if trial && tex.is_none() {
                            // A new picture that fails to render leaves the old one up (rendered
                            // again if a relayout dropped it).
                            if let Some(img) = dynamic.reload.shown(i).or(configured) {
                                *tex = render_image(renderer, im, img, scale);
                                *drawn = img.size();
                            }
                        }
                    }
                    // The size of the picture `tex` shows, which a failed reload keeps.
                    let (iw, ih) = match (tex.is_some(), newest) {
                        (true, _) => *drawn,
                        (false, Some(img)) => img.size(),
                        (false, None) => continue,
                    };
                    let inner = image_inner(im, (iw, ih));
                    let bw = im.border.map_or(0., |b| b.width);
                    let outer = inner + Size::from((2. * bw, 2. * bw));
                    let angle = im.rotate.to_radians();
                    let loc = if angle != 0. {
                        place_rotated_padded(&im.common, outer, angle, size, scale)
                    } else {
                        place(&im.common, outer, size)
                    };
                    // hyprlock: the picture keeps `corner-radius`, the border around it gets the
                    // radius plus its width (roundingForBorderBox).
                    let outer_radius = shape::resolve_radius(
                        im.corner_radius.map(|r| r.expanded_by(bw as f32)),
                        outer,
                    );
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
                    if angle != 0. {
                        let picture = tex.clone();
                        let key = (params, picture.is_some());
                        let build = |r: &mut GlesRenderer, out: &mut Vec<LockElement>| {
                            frame.render(r, params, Point::default(), &mut |e| out.push(e.into()));
                            if let Some(buf) = picture {
                                out.push(LockElement::Texture(
                                    TextureRenderElement::from_texture_buffer(
                                        buf,
                                        (bw, bw),
                                        1.,
                                        None,
                                        Some(inner),
                                        Kind::Unspecified,
                                    ),
                                ));
                            }
                        };
                        let centre = Point::from((loc.x + outer.w / 2., loc.y + outer.h / 2.));
                        elements.extend(draw_rotated(
                            rotated,
                            key,
                            renderer,
                            scale,
                            "image",
                            build,
                            (loc, centre, angle),
                        ));
                        continue;
                    }
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
                    let (animating, wake) = draw_input_field(
                        renderer,
                        f,
                        c,
                        &config.animations,
                        state,
                        dynamic,
                        size,
                        scale,
                        &mut elements,
                    );
                    self.field_animating |= animating;
                    self.field_wake = match (self.field_wake, wake) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
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
                let h = buffer.h;
                for r in &mut rects {
                    r.loc.y = h - r.loc.y - r.size.h;
                }
                egl.swap_buffers(Some(&mut rects)).context("swap_buffers")?;
                self.presented = true;
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

/// Buffer size for a logical size at `scale`: rounded, as wlroots sizes a fractionally scaled
/// output (213x160 at 1.5 is 320x240, not 319x240).
fn buffer_size(size: Size<i32, Logical>, scale: f64) -> Size<i32, Physical> {
    size.to_f64().to_physical(scale).to_i32_round()
}

/// swaylock nudges the indicator right by `2 / scale` logical pixels (integer division, render.c
/// `subsurf_xpos`), with wl_output's integer scale, which wlroots sends rounded up. With the
/// ring's own buffer padding (`ring::render`) this keeps pixel parity at integer scales.
fn ring_nudge(scale: f64) -> f64 {
    f64::from(2 / scale.ceil() as i32)
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

/// [`place`] for a box turned `angle` radians about its centre: hyprlock's `posFromHVAlign` with
/// its rotation correction, so against an edge the turned box's bounding box touches the edge.
/// Centred and `none` axes get no correction. Returns the unrotated box's top-left.
pub fn place_rotated(
    common: &crate::config::Common,
    size: Size<f64, Logical>,
    angle: f64,
    output: Size<f64, Logical>,
) -> Point<f64, Logical> {
    let mut at = place(common, size, output);
    if angle == 0. {
        return at;
    }
    let (cos, sin) = (angle.cos().abs(), angle.sin().abs());
    let bbox =
        Size::<f64, Logical>::from((size.w * cos + size.h * sin, size.w * sin + size.h * cos));
    // hyprlock's `rot`, per axis; negative where the bounding box is larger than the box.
    let rot = ((size.w - bbox.w) / 2., (size.h - bbox.h) / 2.);
    match common.halign {
        HAlign::Left => at.x -= rot.0,
        HAlign::Right => at.x += rot.0,
        HAlign::Center | HAlign::None => {}
    }
    // hyprlock's y is up: its top edge correction is +rot.y from the top, which is -rot.y here.
    match common.valign {
        VAlign::Top => at.y -= rot.1,
        VAlign::Bottom => at.y += rot.1,
        VAlign::Center | VAlign::None => {}
    }
    at
}

/// [`place_rotated`] for a shape or image: as in hyprlock, the box is placed with one physical
/// pixel of antialiasing margin on each side. Returns the unrotated box's top-left, without the
/// margin, on whole physical pixels.
fn place_rotated_padded(
    common: &crate::config::Common,
    size: Size<f64, Logical>,
    angle: f64,
    output: Size<f64, Logical>,
    scale: f64,
) -> Point<f64, Logical> {
    let pad = 1. / scale;
    let at = place_rotated(
        common,
        size + Size::from((2. * pad, 2. * pad)),
        angle,
        output,
    );
    snap(Point::from((at.x + pad, at.y + pad)), scale)
}

/// Round a logical point to whole physical pixels (hyprlock rounds the box it draws).
fn snap(p: Point<f64, Logical>, scale: f64) -> Point<f64, Logical> {
    Point::from(((p.x * scale).round() / scale, (p.y * scale).round() / scale))
}

/// Draw a widget rotated: `build` pushes its unrotated elements (back to front) relative to the
/// widget's top-left; they are rendered into a texture with a one-pixel transparent margin
/// (re-rendered only when `key` changes) and drawn at `loc`, turned `angle` radians about
/// `pivot`. A failed render is logged and skips this widget only.
fn draw_rotated<K: PartialEq>(
    cache: &mut Option<Rotated<K>>,
    key: K,
    renderer: &mut GlesRenderer,
    scale: f64,
    what: &str,
    build: impl FnOnce(&mut GlesRenderer, &mut Vec<LockElement>),
    (loc, pivot, angle): (Point<f64, Logical>, Point<f64, Logical>, f64),
) -> Option<LockElement> {
    if cache.as_ref().is_none_or(|c| c.key != key) {
        let mut parts = Vec::new();
        build(renderer, &mut parts);
        let tex = crate::render::offscreen::render_to_encompassing_texture(
            renderer,
            scale.into(),
            1,
            &parts,
        )
        .map(|(t, geo)| {
            let offset = geo.loc.to_f64().to_logical(scale);
            (RotatedTexture::new(renderer, t, scale), offset)
        })
        .map_err(|err| tracing::warn!("{what}: rotated: {err:#}"))
        .ok();
        *cache = Some(Rotated { key, tex });
    }
    let (tex, offset) = cache.as_ref()?.tex.clone()?;
    Some(LockElement::Rotated(RotatedTextureElement::new(
        tex,
        loc + offset,
        pivot,
        angle,
        scale,
        1.,
    )))
}

/// An `image` widget's picture area: hyprlock scales so the shorter side is `size`, keeping the
/// aspect ratio of a `w` x `h` picture.
fn image_inner(im: &ImageWidget, (w, h): (i32, i32)) -> Size<f64, Logical> {
    let k = im.size / f64::from(w.min(h).max(1));
    Size::from((f64::from(w) * k, f64::from(h) * k))
}

/// An `image` widget's picture, clipped to its corner radius, as a texture; `None` (logged) when
/// it cannot be drawn, such as over the GPU texture limit.
fn render_image(
    renderer: &mut GlesRenderer,
    im: &ImageWidget,
    img: &Image,
    scale: f64,
) -> Option<TextureBuffer<GlesTexture>> {
    let inner = image_inner(im, img.size());
    let phys = inner.to_physical_precise_round(scale);
    let radius = shape::resolve_radius(im.corner_radius, inner);
    let r = <[f32; 4]>::from(radius).map(|v| f64::from(v) * scale);
    crate::render::check_texture_size(phys.w, phys.h)
        .and_then(|()| background::render_clipped(img, phys.w, phys.h, r))
        .and_then(|s| upload(renderer, s, scale))
        .map_err(|err| tracing::warn!("image: {err:#}"))
        .ok()
}

/// Decoded images, keyed by path, shared by all outputs.
#[derive(Default)]
pub struct Images(std::collections::HashMap<std::path::PathBuf, Image>);

impl Images {
    pub fn get(&self, path: &std::path::Path) -> Option<&Image> {
        self.0.get(path)
    }

    pub fn contains(&self, path: &std::path::Path) -> bool {
        self.0.contains_key(path)
    }

    pub fn remove(&mut self, path: &std::path::Path) {
        self.0.remove(path);
    }

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

/// The new picture's opacity `elapsed` into a crossfade of `duration_ms`, linear; `None` once it
/// is over (or there is none), when the old picture is dropped. Drawing the old picture opaque
/// and the new one at this alpha is `mix(old, new, p)` for opaque pictures.
fn crossfade_alpha(elapsed: std::time::Duration, duration_ms: u32) -> Option<f32> {
    let duration = std::time::Duration::from_millis(duration_ms.into());
    (elapsed < duration).then(|| elapsed.as_secs_f32() / duration.as_secs_f32())
}

fn render_background(
    renderer: &mut GlesRenderer,
    bg: &Background,
    image: Option<&Image>,
    phys: Size<i32, Physical>,
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    // Test builds can force this widget to fail, to check one failing widget does not blank the
    // output (the GPU texture size limit is a real-world trigger).
    #[cfg(feature = "test-auth")]
    if std::env::var_os("SWAYWARDLOCK_TEST_FAIL_BACKGROUND").is_some() {
        anyhow::bail!("forced failure (SWAYWARDLOCK_TEST_FAIL_BACKGROUND)");
    }
    // An output over the GPU texture limit gets a background at the limit, stretched over it
    // (the texture is drawn at the output's logical size).
    let max = crate::render::max_texture_size();
    let phys = Size::<i32, Physical>::from((phys.w.min(max), phys.h.min(max)));
    let mut surface = background::render(bg.color, image, bg.fit, phys.w, phys.h)?;
    background::prepare(&mut surface, bg.contrast, bg.brightness)?;
    // hyprlock dims after the blur; dimming is a uniform scale, which commutes with the linear
    // blur, so it can run here.
    background::finish(&mut surface, bg.brightness)?;
    let mem = cairo_to_memory(surface, scale)?;
    let mut buf = crate::render::upload_memory(renderer, &mem)?;

    // hyprlock: blur_passes 0 is no blur.
    if bg.blurred() {
        match Blur::new(renderer) {
            Some(mut b) => {
                let options = bg.blur.into();
                let src = buf.texture().clone();
                b.prepare_textures(
                    |fourcc, s: Size<i32, Buffer>| renderer.create_buffer(fourcc, s),
                    &src,
                    options,
                )?;
                let mut blurred = b.render(renderer, &src, options)?;
                let post = PostProcess::from_config(bg);
                if !post.is_neutral() {
                    // Vibrancy, saturation and noise. A failure costs only the post-processing.
                    match PostProcessProgram::get(renderer) {
                        Some(p) => match p.render(renderer, &blurred, &post) {
                            Ok(done) => blurred = done,
                            Err(err) => {
                                tracing::warn!("background post-processing skipped: {err:#}")
                            }
                        },
                        None => tracing::warn!("background post-processing skipped: no shader"),
                    }
                }
                buf = TextureBuffer::from_texture(
                    renderer,
                    blurred,
                    scale,
                    Transform::Normal,
                    Vec::new(),
                );
            }
            None => tracing::warn!("blur shader unavailable; drawing the background unblurred"),
        }
    }
    Ok(buf)
}

/// Draw one input field. Returns whether one of its animations is running and when its fade-out
/// timer fires (animation clock time), if one is armed.
#[allow(clippy::too_many_arguments)]
fn draw_input_field(
    renderer: &mut GlesRenderer,
    f: &InputField,
    c: &mut FieldCache,
    animations: &crate::config::Animations,
    state: &LockState,
    dynamic: &Dynamic<'_>,
    output: Size<f64, Logical>,
    scale: f64,
    elements: &mut Vec<LockElement>,
) -> (bool, Option<std::time::Duration>) {
    use crate::render::input_field::{dot_rects, dot_size, Look};
    let fail_shown = state.auth == crate::state::AuthState::Invalid;
    let look = Look::new(f, state, fail_shown);
    let config_size =
        Size::<f64, Logical>::from((f.size.0.resolve(output.w), f.size.1.resolve(output.h)));
    let ow = f.outline.width;

    // The text asset (placeholder, check or fail text) first: its width sets the field's target
    // width while the field is empty.
    let asset_text = look
        .asset_template
        .as_ref()
        .map(|t| crate::textvars::substitute(t, &dynamic.vars));
    if let Some(text) = &asset_text {
        let stale = c
            .text
            .as_ref()
            .is_none_or(|(t, col, _)| t != text || *col != look.font);
        if stale {
            let font = format!("{} {}", f.font, (config_size.h / 4.).round());
            let tex = crate::render::text::render(
                text,
                &font,
                look.font,
                crate::config::TextAlign::Center,
                true,
                scale,
            )
            .ok()
            .flatten()
            .and_then(|s| upload(renderer, s, scale).ok());
            c.text = Some((text.clone(), look.font, tex));
        }
    }
    let text_width = match (&asset_text, &c.text) {
        (Some(_), Some((_, _, Some(buf)))) => Some(buf.logical_size().w),
        _ => None,
    };

    c.anim.update(
        &Inputs {
            fade_on_empty: f.fade_on_empty,
            len: state.password_len(),
            checking: state.auth == crate::state::AuthState::Validating,
            check_text_empty: f.check.text.is_empty(),
            width: config_size.w,
            height: config_size.h,
            text_width,
            outline: look.outline,
            inner: look.inner,
            borderless: ow == 0.,
        },
        dynamic.clock,
        animations,
    );
    let status = (c.anim.animating(), c.anim.next_wake());
    let alpha = c.anim.alpha();
    if alpha <= 0. {
        return status;
    }

    let size = Size::<f64, Logical>::from((c.anim.width(), config_size.h));
    let outer = size + Size::from((2. * ow, 2. * ow));
    let loc = place(&f.common, outer, output);
    let inner_loc = Point::<f64, Logical>::from((loc.x + ow, loc.y + ow));
    let outer_radius =
        shape::resolve_radius(f.corner_radius.map(|r| r.expanded_by(ow as f32)), outer);
    let inner_radius = outer_radius.expanded_by(-(ow as f32));
    // hyprlock skips the shadow while the width animates.
    let shadow = if c.anim.width_animating() {
        crate::config::Shadow::default()
    } else {
        f.shadow
    };

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
                gradient: c.anim.outline(),
            }),
            radius: Some(outer_radius),
            shadow,
            scale,
            alpha,
        },
        loc,
        &mut push,
    );
    c.fill.render(
        renderer,
        shape::Params {
            size,
            fill: Some(c.anim.inner()),
            border: None,
            radius: Some(inner_radius),
            shadow: crate::config::Shadow::default(),
            scale,
            alpha,
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
                    alpha,
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
    } else if f.hide_input.is_none() && c.anim.dots() > 0. {
        let amount = c.anim.dots();
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
                    let (rects, _) = dot_rects(f, size, amount, Some(buf.logical_size()));
                    for (r, a) in &rects {
                        elements.push(LockElement::Texture(
                            TextureRenderElement::from_texture_buffer(
                                buf.clone(),
                                (inner_loc.x + r.loc.x, inner_loc.y + r.loc.y),
                                alpha * a,
                                None,
                                Some(r.size),
                                Kind::Unspecified,
                            ),
                        ));
                    }
                }
            }
            None => {
                let (rects, radius) = dot_rects(f, size, amount, None);
                c.dots.resize_with(rects.len(), RoundedBox::new);
                for (b, (r, a)) in c.dots.iter_mut().zip(&rects) {
                    b.render(
                        renderer,
                        shape::Params {
                            size: r.size,
                            fill: Some(look.font),
                            border: None,
                            radius: Some(crate::config::CornerRadius::from(radius as f32)),
                            shadow: crate::config::Shadow::default(),
                            scale,
                            alpha: alpha * a,
                        },
                        Point::from((inner_loc.x + r.loc.x, inner_loc.y + r.loc.y)),
                        &mut |e| elements.push(e.into()),
                    );
                }
            }
        }
    }

    // Placeholder / check / fail text, centred, at a quarter of the field height (hyprlock).
    if look.text_template.is_some() {
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
                    alpha,
                    Some(src),
                    Some(Size::from((w, ts.h))),
                    Kind::Unspecified,
                ),
            ));
        }
    }
    status
}

/// Upload a cairo surface as a texture at `scale`.
fn upload(
    renderer: &mut GlesRenderer,
    surface: pangocairo::cairo::ImageSurface,
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let mem = cairo_to_memory(surface, scale)?;
    crate::render::upload_memory(renderer, &mem)
}

fn render_ring_look(
    renderer: &mut GlesRenderer,
    ring: &RingIndicator,
    frame: &RingFrame,
    look: &Look,
    scale: f64,
) -> anyhow::Result<RingTexture> {
    let scale = ring::render_scale(ring, scale);
    let (surface, centre) = ring::render_look(ring, frame, look, scale)?;
    let mem = cairo_to_memory(surface, scale)?;
    Ok((crate::render::upload_memory(renderer, &mem)?, centre))
}

fn render_ring(
    renderer: &mut GlesRenderer,
    ring: &RingIndicator,
    frame: &RingFrame,
    scale: f64,
) -> anyhow::Result<Option<RingTexture>> {
    let scale = ring::render_scale(ring, scale);
    let Some((surface, centre)) = ring::render(ring, frame, scale)? else {
        return Ok(None);
    };
    let mem = cairo_to_memory(surface, scale)?;
    Ok(Some((
        crate::render::upload_memory(renderer, &mem)?,
        centre,
    )))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{Common, HAlign, Length, VAlign};

    #[test]
    fn crossfade_is_linear_and_ends() {
        let at = |ms| crossfade_alpha(Duration::from_millis(ms), 2000);
        assert_eq!(at(0), Some(0.));
        assert_eq!(at(500), Some(0.25));
        assert_eq!(at(1000), Some(0.5));
        assert_eq!(at(1999).map(|p| p > 0.99), Some(true));
        assert_eq!(at(2000), None);
        assert_eq!(at(9000), None);
        // No duration: nothing to fade.
        assert_eq!(crossfade_alpha(Duration::ZERO, 0), None);
    }

    fn common(halign: HAlign, valign: VAlign, x: f64, y: f64) -> Common {
        Common {
            output: None,
            position: (Length::Px(x), Length::Px(y)),
            halign,
            valign,
            z_index: 0,
        }
    }

    #[test]
    fn buffer_size_rounds_like_wlroots() {
        let at = |w, h, scale| buffer_size(Size::from((w, h)), scale);
        assert_eq!(at(213, 160, 1.5), (320, 240).into());
        assert_eq!(at(209, 131, 1.5), (314, 197).into());
        assert_eq!(at(256, 192, 1.25), (320, 240).into());
        assert_eq!(at(160, 120, 2.), (320, 240).into());
    }

    #[test]
    fn ring_nudge_matches_swaylock() {
        assert_eq!(ring_nudge(1.), 2.);
        assert_eq!(ring_nudge(1.5), 1.);
        assert_eq!(ring_nudge(2.), 1.);
    }

    /// hyprlock's rotation correction: against an edge, the rotated bounding box touches it.
    /// 100x20 turned 90 degrees has a 20x100 bounding box, centred on the box.
    #[test]
    fn place_rotated_keeps_the_bounding_box_on_the_edge() {
        let out = Size::from((300., 200.));
        let s = Size::from((100., 20.));
        let quarter = std::f64::consts::FRAC_PI_2;
        let at = |c| place_rotated(&c, s, quarter, out);
        let near = |a: Point<f64, Logical>, b: (f64, f64)| {
            assert!(
                (a.x - b.0).abs() < 1e-9 && (a.y - b.1).abs() < 1e-9,
                "{a:?} != {b:?}"
            );
        };
        // rot = ((100 - 20) / 2, (20 - 100) / 2) = (40, -40): the bounding box's top-left is
        // (0, 0), so the unrotated top-left is (-40, 40).
        near(at(common(HAlign::Left, VAlign::Top, 0., 0.)), (-40., 40.));
        near(
            at(common(HAlign::Right, VAlign::Bottom, 0., 0.)),
            (300. - 100. + 40., 200. - 20. - 40.),
        );
        // Centre and none get no correction.
        for (h, v) in [
            (HAlign::Center, VAlign::Center),
            (HAlign::None, VAlign::None),
        ] {
            let c = common(h, v, 7., 9.);
            near(at(c.clone()), {
                let p = place(&c, s, out);
                (p.x, p.y)
            });
        }
        // Angle 0 is place() for every alignment.
        for h in [HAlign::Left, HAlign::Center, HAlign::Right, HAlign::None] {
            for v in [VAlign::Top, VAlign::Center, VAlign::Bottom, VAlign::None] {
                let c = common(h, v, 3., -4.);
                assert_eq!(place_rotated(&c, s, 0., out), place(&c, s, out));
            }
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
