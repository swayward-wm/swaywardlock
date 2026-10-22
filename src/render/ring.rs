//! swaylock's unlock indicator, drawn with cairo into a texture.
//!
//! A port of swaylock's `render_frame` (`render.c`), using Pango for text so font descriptions
//! match the rest of swaywardlock. Geometry and colour choice follow swaylock line by line.
//! The swayward style (`style "swayward"`) draws the same geometry with animated colours and
//! angles from `ring_anim`, plus a highlight trail, a verifying spinner and a glow.

use std::f64::consts::PI;

use pangocairo::{cairo, pango};

use crate::config::{Color, LineUses, RingIndicator, ShowLayout, StateColors};
use crate::state::{AuthState, InputState, LockState};

/// swaylock's `TYPE_INDICATOR_RANGE`.
const HIGHLIGHT_RANGE: f64 = PI / 3.;

/// What the ring shows this frame. Comparing two of these tells whether a redraw is needed.
#[derive(Debug, Clone, PartialEq)]
pub struct RingFrame {
    pub visible: bool,
    pub text: Option<String>,
    pub layout_text: Option<String>,
    pub input: InputState,
    pub auth: AuthState,
    pub caps_lock: bool,
    pub highlight_start: u32,
}

impl RingFrame {
    pub fn new(ring: &RingIndicator, state: &LockState) -> Self {
        let visible =
            state.auth != AuthState::Idle || state.input != InputState::Idle || ring.idle_visible;

        let mut text = None;
        let mut layout_text = None;
        if visible {
            text = match (state.input, state.auth) {
                (InputState::Clear, _) => Some("Cleared".into()),
                (_, AuthState::Validating) => Some("Verifying".into()),
                (_, AuthState::Invalid) => Some("Wrong".into()),
                _ if state.caps_lock && ring.show_caps_lock_text => Some("Caps Lock".into()),
                _ if ring.show_failed_attempts && state.failed_attempts > 0 => {
                    Some(if state.failed_attempts > 999 {
                        "999+".into()
                    } else {
                        state.failed_attempts.to_string()
                    })
                }
                _ => None,
            };

            let show_layout = match ring.show_keyboard_layout {
                ShowLayout::Always => true,
                ShowLayout::Never => false,
                ShowLayout::Auto => state.layout_names.len() > 1,
            };
            // swaylock shows the layout only in the non-message states.
            let message_state = state.input == InputState::Clear || state.auth != AuthState::Idle;
            if show_layout && !message_state {
                // An unnamed layout would draw an empty box; skip it.
                layout_text = state
                    .layout_name()
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned);
            }
        }

        Self {
            visible,
            text,
            layout_text,
            input: state.input,
            auth: state.auth,
            caps_lock: state.caps_lock,
            highlight_start: state.highlight_start,
        }
    }

    /// swaylock's `set_color_for_state`.
    fn pick(&self, ring: &RingIndicator, set: &StateColors) -> Color {
        if self.input == InputState::Clear {
            set.cleared
        } else if self.auth == AuthState::Validating {
            set.verifying
        } else if self.auth == AuthState::Invalid {
            set.wrong
        } else if self.caps_lock && ring.show_caps_lock_indicator {
            set.caps_lock
        } else {
            set.input
        }
    }

    /// Text colour has one extra rule: caps-lock text without the caps-lock indicator uses the
    /// caps-lock text colour on the input colour set.
    fn pick_text(&self, ring: &RingIndicator) -> Color {
        let set = &ring.colors.text;
        let plain = self.input != InputState::Clear && self.auth == AuthState::Idle;
        if plain && self.caps_lock && !ring.show_caps_lock_indicator && ring.show_caps_lock_text {
            set.caps_lock
        } else {
            self.pick(ring, set)
        }
    }
}

fn set_source(cr: &cairo::Context, c: Color) {
    cr.set_source_rgba(c.r.into(), c.g.into(), c.b.into(), c.a.into());
}

fn font(ring: &RingIndicator, arc_radius: f64) -> pango::FontDescription {
    let mut font = pango::FontDescription::from_string(&super::text::strip_nul(&ring.font));
    let size = if ring.font_size > 0. {
        ring.font_size
    } else {
        arc_radius / 3.
    };
    font.set_absolute_size(size * f64::from(pango::SCALE));
    font
}

fn layout(cr: &cairo::Context, font: &pango::FontDescription, text: &str) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    layout.set_font_description(Some(font));
    layout.set_text(&super::text::strip_nul(text));
    layout
}

/// Everything the drawing takes from the lock state, resolved to colours and angles: the
/// swaylock style picks them as swaylock does, the swayward style animates them
/// (`ring_anim`). Comparing two of these (with the [`RingFrame`]) tells whether a redraw is
/// needed.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub inside: Color,
    pub ring: Color,
    pub line: Color,
    pub text: Color,
    /// The typing highlight: its start in `highlight_start` units (2048 per turn) and colour.
    pub highlight: Option<(f64, Color)>,
    /// The highlight's trail: the start it swept from, the start it is at (same units), and an
    /// alpha factor.
    pub trail: Option<(f64, f64, f32)>,
    /// The verifying spinner's head angle, in radians.
    pub spinner: Option<f64>,
    /// Scale of everything around the ring's centre.
    pub scale: f64,
    /// Halo width outside the ring, logical px.
    pub glow: f64,
}

impl Look {
    /// swaylock's colours for `frame`, and nothing it does not draw.
    pub fn swaylock(ring: &RingIndicator, frame: &RingFrame) -> Self {
        let (inside, ring_colour, line, text) = frame.targets(ring);
        Self {
            inside,
            ring: ring_colour,
            line,
            text,
            highlight: frame.highlighted().then(|| {
                (
                    f64::from(frame.highlight_start),
                    frame.highlight_colour(ring),
                )
            }),
            trail: None,
            spinner: None,
            scale: 1.,
            glow: 0.,
        }
    }
}

impl RingFrame {
    /// The inside, ring, line and text colours for this state.
    pub(crate) fn targets(&self, ring: &RingIndicator) -> (Color, Color, Color, Color) {
        let line_set = match ring.line_uses {
            LineUses::Line => &ring.colors.line,
            LineUses::Inside => &ring.colors.inside,
            LineUses::Ring => &ring.colors.ring,
        };
        (
            self.pick(ring, &ring.colors.inside),
            self.pick(ring, &ring.colors.ring),
            self.pick(ring, line_set),
            self.pick_text(ring),
        )
    }

    /// The typing highlight is drawn.
    pub(crate) fn highlighted(&self) -> bool {
        matches!(self.input, InputState::Letter | InputState::Backspace)
    }

    pub(crate) fn highlight_colour(&self, ring: &RingIndicator) -> Color {
        let caps = self.caps_lock && ring.show_caps_lock_indicator;
        let c = &ring.colors;
        match (self.input == InputState::Letter, caps) {
            (true, false) => c.key_highlight,
            (true, true) => c.caps_lock_key_highlight,
            (false, false) => c.bs_highlight,
            (false, true) => c.caps_lock_bs_highlight,
        }
    }
}

/// Radians per `highlight_start` unit.
const UNIT: f64 = PI / 1024.;

/// Largest ring texture side, in buffer pixels. A ring is re-rendered on every key (the
/// highlight moves; the swayward style on every animation frame), on the main thread: at radius
/// 2000 and scale 2 that was 0.5 s per key and GBs of memory. A bigger ring renders at a lower
/// scale and the GPU stretches it.
pub const MAX_RING_PIXELS: f64 = 4096.;

/// The buffer scale `ring` renders at on an output of `scale`: the output's, unless that makes
/// the ring (with its glow) more than [`MAX_RING_PIXELS`] across.
pub fn render_scale(ring: &RingIndicator, scale: f64) -> f64 {
    let diameter = 2. * (ring.radius + ring.thickness + ring.glow);
    scale.min(MAX_RING_PIXELS / diameter)
}

/// Render the ring at `scale` (buffer pixels per logical pixel; see [`render_scale`]) in the
/// swaylock style. Returns the image and the logical offset within it of the point placed at the
/// ring's centre, or `None` when the ring is hidden.
pub fn render(
    ring: &RingIndicator,
    frame: &RingFrame,
    scale: f64,
) -> anyhow::Result<Option<(cairo::ImageSurface, (f64, f64))>> {
    if !frame.visible {
        return Ok(None);
    }
    render_look(ring, frame, &Look::swaylock(ring, frame), scale).map(Some)
}

/// Render `frame`'s texts with `look`'s colours and effects. With [`Look::swaylock`] this is
/// swaylock's `render_frame`, pixel for pixel (pinned by `swaylock_style_matches_the_fixtures`).
///
/// The geometry is swaylock's integer arithmetic, for both styles: radius and thickness in whole
/// buffer pixels, `arc_radius - arc_thickness / 2` rounding down for an odd thickness, and a
/// buffer padded to the next multiple of the (integer) scale, which moves its centre. The glow
/// adds whole pixels on every side, so the swayward ring sits where the swaylock ring does.
pub fn render_look(
    ring: &RingIndicator,
    frame: &RingFrame,
    look: &Look,
    scale: f64,
) -> anyhow::Result<(cairo::ImageSurface, (f64, f64))> {
    // swaylock: `int arc_radius = state->args.radius * surface->scale;` (truncates).
    let arc_radius = (ring.radius * scale) as i32;
    let arc_thickness = (ring.thickness * scale) as i32;
    let diameter = (arc_radius + arc_thickness) * 2;
    let font = font(ring, f64::from(arc_radius));
    let box_padding = 4. * scale;
    // The glow enlarges the texture on every side; the centre offset returned keeps the ring
    // where it would be without it.
    let glow_width = look.glow * scale;
    let pad = glow_width.ceil() as i32;

    // Measure text to size the buffer, as swaylock does.
    let mut width = diameter + 2 * pad;
    let mut below = pad;
    let mut layout_size = None;
    {
        let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)?;
        let cr = cairo::Context::new(&probe)?;
        if let Some(text) = &frame.text {
            let (w, _) = layout(&cr, &font, text).pixel_size();
            width = width.max(w);
        }
        if let Some(text) = &frame.layout_text {
            let (w, h) = layout(&cr, &font, text).pixel_size();
            let (w, h) = (f64::from(w), f64::from(h));
            below = below.max((h + 2. * box_padding) as i32);
            width = width.max((w + 2. * box_padding) as i32);
            layout_size = Some((w, h));
        }
    }
    let mut height = diameter + pad + below;
    // swaylock pads the buffer to a multiple of its integer scale (always adding at least one
    // step). Not at a fractional scale, where swaylock draws at the rounded-up integer scale.
    let int_scale = (scale.fract() == 0.).then_some(scale as i32);
    if let Some(s) = int_scale {
        width += s - width % s;
        height += s - height % s;
    }
    // Over the GPU limit the upload fails anyway; refuse before allocating (very long text).
    super::check_texture_size(width, height)?;
    let (bw, bh) = (width, height);

    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, bw, bh)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_antialias(cairo::Antialias::Best);
    // C integer division, as swaylock's `buffer_width / 2`.
    let cx = f64::from(bw / 2);
    let cy = f64::from(pad + diameter / 2);
    let (arc_r, arc_t) = (f64::from(arc_radius), f64::from(arc_thickness));
    // swaylock's `arc_radius - arc_thickness / 2` and `+`, in ints.
    let inner_r = f64::from(arc_radius - arc_thickness / 2);
    let outer_r = f64::from(arc_radius + arc_thickness / 2);
    // The layout box: under the ring, centred.
    let layout_box = layout_size.map(|(w, h)| {
        (
            cx - w / 2. - box_padding,
            f64::from(pad + diameter),
            w + 2. * box_padding,
            h + 2. * box_padding,
        )
    });

    if look.scale != 1. {
        cr.translate(cx, cy);
        cr.scale(look.scale, look.scale);
        cr.translate(-cx, -cy);
    }
    if glow_width > 0. {
        glow(
            &cr,
            look.ring,
            (cx, cy),
            outer_r,
            glow_width,
            layout_box,
            (bw, bh),
        )?;
    }

    // Fill inner circle.
    cr.arc(cx, cy, inner_r, 0., 2. * PI);
    set_source(&cr, look.inside);
    cr.fill()?;

    // Ring.
    cr.set_line_width(arc_t);
    cr.arc(cx, cy, arc_r, 0., 2. * PI);
    set_source(&cr, look.ring);
    cr.stroke()?;

    // Message.
    if let Some(text) = &frame.text {
        let l = layout(&cr, &font, text);
        let (w, h) = l.pixel_size();
        cr.move_to(cx - f64::from(w) / 2., cy - f64::from(h) / 2.);
        set_source(&cr, look.text);
        pangocairo::functions::show_layout(&cr, &l);
        cr.new_path();
    }

    // The swept span behind the highlight, fading out.
    if let (Some((from, to, alpha)), Some((_, colour))) = (look.trail, look.highlight) {
        let (a, b) = if to >= from {
            (from, to + HIGHLIGHT_RANGE / UNIT)
        } else {
            (to, from + HIGHLIGHT_RANGE / UNIT)
        };
        cr.arc(cx, cy, arc_r, a * UNIT, b * UNIT);
        set_source(
            &cr,
            Color {
                a: colour.a * alpha,
                ..colour
            },
        );
        cr.stroke()?;
    }

    // Typing indicator: highlight a random part of the ring on each key.
    if let Some((start, colour)) = look.highlight {
        let start = start * UNIT;
        cr.arc(cx, cy, arc_r, start, start + HIGHLIGHT_RANGE);
        set_source(&cr, colour);
        cr.stroke()?;

        // Separators at both ends of the highlight (doubles in swaylock).
        let inner = f64::from(diameter) / 2. - arc_t * 1.5;
        let outer = f64::from(diameter) / 2. - arc_t / 2.;
        cr.set_line_width(2. * scale);
        set_source(&cr, ring.colors.separator);
        for angle in [start, start + HIGHLIGHT_RANGE] {
            cr.move_to(cx + angle.cos() * inner, cy + angle.sin() * inner);
            cr.line_to(cx + angle.cos() * outer, cy + angle.sin() * outer);
            cr.stroke()?;
        }
    }

    // Verifying: a comet, its tail fading to nothing over a quarter turn.
    if let Some(head) = look.spinner {
        let colour = ring.colors.key_highlight;
        let steps = 24;
        let step = (PI / 2.) / f64::from(steps);
        cr.set_line_width(arc_t);
        for i in 0..steps {
            let a = head - PI / 2. + f64::from(i) * step;
            cr.arc(cx, cy, arc_r, a, a + step);
            let alpha = (i + 1) as f32 / steps as f32;
            set_source(
                &cr,
                Color {
                    a: colour.a * alpha,
                    ..colour
                },
            );
            cr.stroke()?;
        }
    }

    // Inner and outer border lines.
    set_source(&cr, look.line);
    cr.set_line_width(2. * scale);
    cr.arc(cx, cy, inner_r, 0., 2. * PI);
    cr.stroke()?;
    cr.arc(cx, cy, outer_r, 0., 2. * PI);
    cr.stroke()?;

    // Keyboard layout in a box under the ring.
    if let (Some(text), Some((x, y, w, h))) = (&frame.layout_text, layout_box) {
        let l = layout(&cr, &font, text);
        cr.rectangle(x, y, w, h);
        set_source(&cr, ring.colors.layout_bg);
        cr.fill_preserve()?;
        // swaylock does not reset the line width here: the box border inherits the 2 px of the
        // border lines above.
        set_source(&cr, ring.colors.layout_border);
        cr.stroke()?;
        cr.move_to(x + box_padding, y + box_padding);
        set_source(&cr, ring.colors.layout_text);
        pangocairo::functions::show_layout(&cr, &l);
    }

    drop(cr);
    // swaylock places the buffer's left edge at `width / 2 - buffer_width / (2 * scale)` in
    // whole logical pixels, which puts the ring's centre up to a buffer pixel right of the middle.
    let anchor_x = match int_scale {
        Some(s) => f64::from(bw / (2 * s)),
        None => cx / scale,
    };
    Ok((surface, (anchor_x, cy / scale)))
}

/// A halo `width` wide outside radius `from`, in `colour` at 40% of its alpha fading to nothing,
/// kept out of the layout box.
fn glow(
    cr: &cairo::Context,
    colour: Color,
    (cx, cy): (f64, f64),
    from: f64,
    width: f64,
    layout_box: Option<(f64, f64, f64, f64)>,
    (bw, bh): (i32, i32),
) -> anyhow::Result<()> {
    cr.save()?;
    if let Some((x, y, w, h)) = layout_box {
        cr.rectangle(0., 0., f64::from(bw), f64::from(bh));
        cr.rectangle(x, y, w, h);
        cr.set_fill_rule(cairo::FillRule::EvenOdd);
        cr.clip();
        cr.set_fill_rule(cairo::FillRule::Winding);
    }
    let gradient = cairo::RadialGradient::new(cx, cy, from, cx, cy, from + width);
    let (r, g, b, a) = (
        colour.r.into(),
        colour.g.into(),
        colour.b.into(),
        f64::from(colour.a),
    );
    gradient.add_color_stop_rgba(0., r, g, b, 0.4 * a);
    gradient.add_color_stop_rgba(1., r, g, b, 0.);
    cr.set_source(&gradient)?;
    cr.arc(cx, cy, from + width, 0., 2. * PI);
    cr.new_sub_path();
    cr.arc_negative(cx, cy, from, 2. * PI, 0.);
    cr.fill()?;
    cr.restore()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::config::General;

    #[test]
    fn hidden_when_idle_unless_idle_visible() {
        let state = LockState::new(&General::default());
        let mut ring = RingIndicator::default();
        assert!(!RingFrame::new(&ring, &state).visible);
        ring.idle_visible = true;
        assert!(RingFrame::new(&ring, &state).visible);
    }

    #[test]
    fn message_priority_follows_swaylock() {
        let ring = RingIndicator {
            show_failed_attempts: true,
            ..RingIndicator::default()
        };
        let mut state = LockState::new(&General::default());
        let now = Instant::now();
        state.on_auth_failed(now);
        assert_eq!(RingFrame::new(&ring, &state).text.as_deref(), Some("Wrong"));
        state.tick(now + std::time::Duration::from_secs(5));
        state.caps_lock = true;
        state.on_key(
            smithay_client_toolkit::seat::keyboard::Keysym::a,
            Some("a"),
            false,
            now,
        );
        assert_eq!(
            RingFrame::new(&ring, &state).text.as_deref(),
            Some("Caps Lock")
        );
        state.caps_lock = false;
        assert_eq!(RingFrame::new(&ring, &state).text.as_deref(), Some("1"));
    }

    /// `font-size` replaces swaylock's radius / 3.
    #[test]
    fn font_size_overrides_the_radius_based_size() {
        let mut ring = RingIndicator::default();
        let size = |ring: &RingIndicator| font(ring, 60.).size() / pango::SCALE;
        assert_eq!(size(&ring), 20);
        ring.font_size = 31.;
        assert_eq!(size(&ring), 31);
    }

    /// Caps Lock picks the caps-lock colours: the highlight with `show-caps-lock-indicator`,
    /// and the text's caps-lock colour when only the text is shown (swaylock's rules).
    #[test]
    fn caps_lock_colours() {
        let rgb = |r, g, b| Color::from_rgba8_unpremul(r, g, b, 255);
        let mut ring = RingIndicator::default();
        ring.colors.key_highlight = rgb(1, 0, 0);
        ring.colors.bs_highlight = rgb(2, 0, 0);
        ring.colors.caps_lock_key_highlight = rgb(3, 0, 0);
        ring.colors.caps_lock_bs_highlight = rgb(4, 0, 0);
        ring.colors.text.input = rgb(5, 0, 0);
        ring.colors.text.caps_lock = rgb(6, 0, 0);
        let mut state = LockState::new(&General::default());
        let now = Instant::now();
        let key = |s: &mut LockState, k| {
            s.on_key(k, Some("a"), false, now);
        };
        key(
            &mut state,
            smithay_client_toolkit::seat::keyboard::Keysym::a,
        );
        state.caps_lock = true;
        let frame = |ring: &RingIndicator, state: &LockState| RingFrame::new(ring, state);

        // Text only (the default): plain highlight, caps-lock text colour.
        let f = frame(&ring, &state);
        assert_eq!(f.highlight_colour(&ring), rgb(1, 0, 0));
        assert_eq!(f.targets(&ring).3, rgb(6, 0, 0));
        ring.show_caps_lock_text = false;
        assert_eq!(frame(&ring, &state).targets(&ring).3, rgb(5, 0, 0));

        // The indicator: caps-lock highlights for letters and BackSpace.
        ring.show_caps_lock_indicator = true;
        assert_eq!(frame(&ring, &state).highlight_colour(&ring), rgb(3, 0, 0));
        key(
            &mut state,
            smithay_client_toolkit::seat::keyboard::Keysym::a,
        );
        key(
            &mut state,
            smithay_client_toolkit::seat::keyboard::Keysym::BackSpace,
        );
        assert_eq!(frame(&ring, &state).highlight_colour(&ring), rgb(4, 0, 0));
        state.caps_lock = false;
        assert_eq!(frame(&ring, &state).highlight_colour(&ring), rgb(2, 0, 0));
    }

    #[test]
    fn layout_text() {
        let mut state = LockState::new(&General::default());
        state.on_key(
            smithay_client_toolkit::seat::keyboard::Keysym::a,
            Some("a"),
            false,
            Instant::now(),
        );
        let always = RingIndicator {
            show_keyboard_layout: ShowLayout::Always,
            ..RingIndicator::default()
        };
        let auto = RingIndicator::default();

        state.layout_names = vec!["English (US)".into()];
        assert_eq!(
            RingFrame::new(&always, &state).layout_text.as_deref(),
            Some("English (US)")
        );
        // Auto shows the layout only with more than one.
        assert_eq!(RingFrame::new(&auto, &state).layout_text, None);
        state.layout_names.push("German".into());
        state.layout = 1;
        assert_eq!(
            RingFrame::new(&auto, &state).layout_text.as_deref(),
            Some("German")
        );

        state.layout_names = vec![String::new()];
        state.layout = 0;
        assert_eq!(RingFrame::new(&always, &state).layout_text, None);
    }

    /// swaylock strokes the layout box border with the 2 px width left over from the ring's
    /// border lines.
    #[test]
    fn layout_box_border_is_two_pixels() {
        let white = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let mut ring = RingIndicator {
            idle_visible: true,
            show_keyboard_layout: ShowLayout::Always,
            ..RingIndicator::default()
        };
        ring.colors.layout_border = white;
        ring.colors.layout_bg = Color::from_rgba8_unpremul(0, 0, 0, 0);
        let mut state = LockState::new(&General::default());
        state.layout_names = vec!["us".into()];
        let frame = RingFrame::new(&ring, &state);
        let (mut surface, _) = render(&ring, &frame, 1.).unwrap().unwrap();
        // The box's top edge is at y = 120 (the ring's diameter); a 2 px line covers 119 and 120.
        let stride = surface.stride() as usize;
        let cx = surface.width() as usize / 2;
        let data = surface.data().unwrap();
        for y in [119, 120] {
            let px = &data[y * stride + cx * 4..][..4];
            assert_eq!(px, [255, 255, 255, 255], "row {y}");
        }
    }

    /// A NUL in the font aborted debug builds in glib.
    #[test]
    fn nul_in_the_font_is_dropped() {
        let ring = RingIndicator {
            idle_visible: true,
            font: "Sans\0Bold".into(),
            ..RingIndicator::default()
        };
        let mut state = LockState::new(&General::default());
        state.on_auth_failed(Instant::now());
        assert!(render(&ring, &RingFrame::new(&ring, &state), 1.)
            .unwrap()
            .is_some());
    }

    /// A big ring was re-rendered at full size on every key (radius 4000: 12 s to type a
    /// password). It now renders at most MAX_RING_PIXELS across, at a lower scale.
    #[test]
    fn huge_ring_renders_at_a_capped_size() {
        let ring = RingIndicator {
            idle_visible: true,
            radius: 10000.,
            thickness: 10000.,
            ..RingIndicator::default()
        };
        let state = LockState::new(&General::default());
        let scale = render_scale(&ring, 2.);
        assert!((scale - 4096. / 40000.).abs() < 1e-12, "{scale}");
        let (surface, (cx, cy)) = render(&ring, &RingFrame::new(&ring, &state), scale)
            .unwrap()
            .unwrap();
        assert!(surface.width() <= 4096 && surface.height() <= 4096);
        // Logical centre still at radius + thickness.
        assert!(
            (cx - 20000.).abs() < 10. && (cy - 20000.).abs() < 10.,
            "{cx} {cy}"
        );
        // Rings that fit keep the output's scale.
        assert_eq!(render_scale(&RingIndicator::default(), 2.), 2.);

        // The swayward style re-renders every animation frame: the cap counts its glow.
        let ring = RingIndicator {
            style: crate::config::RingStyle::Swayward,
            glow: 64.,
            ..ring
        };
        let scale = render_scale(&ring, 2.);
        assert!((scale - 4096. / 40128.).abs() < 1e-12, "{scale}");
        let frame = RingFrame::new(&ring, &state);
        let look = Look {
            glow: ring.glow,
            ..Look::swaylock(&ring, &frame)
        };
        let (surface, (cx, cy)) = render_look(&ring, &frame, &look, scale).unwrap();
        assert!(surface.width() <= 4096 && surface.height() <= 4096);
        assert!(
            (cx - 20064.).abs() < 20. && (cy - 20064.).abs() < 20.,
            "{cx} {cy}"
        );
    }

    /// The colour at (x, y) of the ring's buffer as (r, g, b, a).
    fn rgba(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> (u8, u8, u8, u8) {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let px = &data[y * stride + x * 4..][..4];
        (px[2], px[1], px[0], px[3])
    }

    /// swaylock does the geometry in ints: with an odd thickness `arc_radius - arc_thickness / 2`
    /// rounds down, so for radius 50 thickness 11 the inner circle has radius 45 and the outer
    /// border line 55 (the floats gave 44.5 and 55.5).
    #[test]
    fn odd_thickness_uses_swaylocks_integer_radii() {
        let mut ring = RingIndicator {
            idle_visible: true,
            thickness: 11.,
            ..RingIndicator::default()
        };
        ring.colors.inside.input = Color::from_rgba8_unpremul(255, 255, 255, 255);
        ring.colors.ring.input = Color::from_rgba8_unpremul(255, 0, 0, 255);
        ring.colors.line.input = Color::from_rgba8_unpremul(0, 0, 255, 255);
        let state = LockState::new(&General::default());
        let (mut s, (cx, cy)) = render(&ring, &RingFrame::new(&ring, &state), 1.)
            .unwrap()
            .unwrap();
        // diameter 122, padded to 123 like swaylock's buffer; centre at 61.
        assert_eq!((s.width(), s.height()), (123, 123));
        assert_eq!((cx, cy), (61., 61.));
        // Down the vertical through the centre: the 2 px border lines straddle y = 61 - 45 = 16
        // and y = 61 - 55 = 6.
        let col: Vec<_> = (0..20).map(|y| rgba(&mut s, 61, y)).collect();
        let blue = |p: (u8, u8, u8, u8)| p.2 >= 240 && p.0 <= 16 && p.1 <= 16;
        assert!(
            blue(col[15]) && blue(col[16]),
            "inner line at 15..17: {:?}",
            &col[13..19]
        );
        assert!(
            blue(col[5]) && blue(col[6]),
            "outer line at 5..7: {:?}",
            &col[3..9]
        );
        assert_eq!(col[4].3, 0, "nothing outside the outer line: {:?}", col[4]);
    }

    /// At scale 2 swaylock pads the buffer by a scale step, which moves the ring's centre one
    /// buffer pixel right of the middle; the anchor carries that shift.
    #[test]
    fn integer_scale_pads_the_buffer_like_swaylock() {
        let ring = RingIndicator {
            idle_visible: true,
            ..RingIndicator::default()
        };
        let state = LockState::new(&General::default());
        let (s, (ax, ay)) = render(&ring, &RingFrame::new(&ring, &state), 2.)
            .unwrap()
            .unwrap();
        // diameter 240 + 2 = 242; the ring is drawn at buffer x 121, the anchor is 242 / 4 = 60
        // logical (120 px), so the ring sits 1 buffer px right of the anchor.
        assert_eq!((s.width(), s.height()), (242, 242));
        assert_eq!((ax, ay), (60., 60.));
    }

    #[test]
    fn renders_and_picks_state_colours() {
        let ring = RingIndicator {
            idle_visible: true,
            ..RingIndicator::default()
        };
        let mut state = LockState::new(&General::default());
        state.on_auth_failed(Instant::now());
        let frame = RingFrame::new(&ring, &state);
        let (mut surface, (cx, cy)) = render(&ring, &frame, 1.).unwrap().unwrap();
        assert_eq!((cx, cy), (60., 60.));
        // swaylock's buffer padding: one extra column and row at scale 1.
        assert_eq!((surface.width(), surface.height()), (121, 121));

        // Sample the ring band at the far right of the circle: wrong-state ring colour 7d3300.
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let (x, y) = (60 + 50, 60);
        let px = &data[y * stride + x * 4..][..4];
        // cairo ARGB32 little-endian: B G R A.
        assert_eq!((px[2], px[1], px[0], px[3]), (0x7d, 0x33, 0x00, 0xff));
    }
    /// The swaylock style is pinned pixel for pixel: hashes of renders captured from main 24ebfbd
    /// before the swayward style existed. Text is transparent so the fixtures do not depend on
    /// the installed fonts (the text still sizes nothing: every message fits the ring).
    #[test]
    fn swaylock_style_matches_the_fixtures() {
        let mut ring = RingIndicator {
            idle_visible: true,
            show_caps_lock_indicator: true,
            ..RingIndicator::default()
        };
        let clear = Color::from_rgba8_unpremul(0, 0, 0, 0);
        ring.colors.text = StateColors {
            input: clear,
            cleared: clear,
            caps_lock: clear,
            verifying: clear,
            wrong: clear,
        };
        let now = Instant::now();
        let key = |s: &mut LockState, k| {
            s.on_key(k, Some("a"), false, now);
        };
        let mut cases: Vec<(&str, LockState)> = Vec::new();
        let base = || LockState::new(&General::default());
        cases.push(("idle", base()));
        let mut s = base();
        key(&mut s, smithay_client_toolkit::seat::keyboard::Keysym::a);
        s.highlight_start = 300;
        cases.push(("letter", s));
        let mut s = base();
        key(&mut s, smithay_client_toolkit::seat::keyboard::Keysym::a);
        key(&mut s, smithay_client_toolkit::seat::keyboard::Keysym::a);
        key(
            &mut s,
            smithay_client_toolkit::seat::keyboard::Keysym::BackSpace,
        );
        s.highlight_start = 1900;
        s.caps_lock = true;
        cases.push(("backspace-caps", s));
        let mut s = base();
        key(
            &mut s,
            smithay_client_toolkit::seat::keyboard::Keysym::Escape,
        );
        cases.push(("clear", s));
        let mut s = base();
        key(&mut s, smithay_client_toolkit::seat::keyboard::Keysym::a);
        key(
            &mut s,
            smithay_client_toolkit::seat::keyboard::Keysym::Return,
        );
        cases.push(("verifying", s));
        let mut s = base();
        s.on_auth_failed(now);
        cases.push(("wrong", s));

        let mut got = Vec::new();
        for (name, state) in &cases {
            for scale in [1., 1.5, 2.] {
                let frame = RingFrame::new(&ring, state);
                let (mut surface, centre) = render(&ring, &frame, scale).unwrap().unwrap();
                let (w, h) = (surface.width(), surface.height());
                let data = surface.data().unwrap();
                // FNV-1a: stable across Rust versions, unlike DefaultHasher.
                let hash = data.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
                    (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
                });
                got.push(format!("{name}@{scale}: {w}x{h} {centre:?} {hash:016x}"));
            }
        }
        let want = include_str!("testdata/ring-swaylock.txt");
        assert_eq!(got.join("\n"), want.trim_end(), "swaylock style changed");
    }

    /// The buffer x the ring is drawn around: swaylock's `buffer_width / 2`. At an integer
    /// scale above 1 the buffer padding puts it right of the returned anchor.
    fn drawn_cx(surface: &cairo::ImageSurface) -> f64 {
        f64::from(surface.width() / 2)
    }

    fn pixel(surface: &mut cairo::ImageSurface, x: f64, y: f64) -> [u8; 4] {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let (x, y) = (x.floor() as usize, y.floor() as usize);
        // cairo ARGB32 little-endian: B G R A.
        let px = &data[y * stride + x * 4..][..4];
        [px[2], px[1], px[0], px[3]]
    }

    /// `base` with `text` drawn in `colour` at (`x`, `y`), the way the ring draws its texts.
    fn with_text(
        base: &cairo::ImageSurface,
        ring: &RingIndicator,
        scale: f64,
        text: &str,
        (x, y): (f64, f64),
        colour: Color,
    ) -> Vec<u8> {
        let mut out =
            cairo::ImageSurface::create(cairo::Format::ARgb32, base.width(), base.height())
                .unwrap();
        {
            let cr = cairo::Context::new(&out).unwrap();
            cr.set_source_surface(base, 0., 0.).unwrap();
            cr.paint().unwrap();
            let l = layout(&cr, &font(ring, ring.radius * scale), text);
            cr.move_to(x, y);
            set_source(&cr, colour);
            pangocairo::functions::show_layout(&cr, &l);
        }
        let bytes = out.data().unwrap().to_vec();
        bytes
    }

    fn text_size(ring: &RingIndicator, scale: f64, text: &str) -> (f64, f64) {
        let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).unwrap();
        let cr = cairo::Context::new(&probe).unwrap();
        let (w, h) = layout(&cr, &font(ring, ring.radius * scale), text).pixel_size();
        (f64::from(w), f64::from(h))
    }

    /// The fixtures draw no text (transparent, so they do not depend on the installed fonts), so
    /// they cannot see the message move. This compares an opaque-text render with the
    /// text-less render plus the same Pango layout drawn by the test, centred on the ring: any
    /// font gives the same equality, a text moved by one pixel does not.
    #[test]
    fn message_is_centred_on_the_ring() {
        let mut state = LockState::new(&General::default());
        state.on_key(
            smithay_client_toolkit::seat::keyboard::Keysym::Escape,
            None,
            false,
            Instant::now(),
        );
        let red = Color::from_rgba8_unpremul(255, 0, 0, 255);
        for scale in [1., 2.] {
            let mut ring = RingIndicator {
                font_size: 10.,
                ..RingIndicator::default()
            };
            ring.colors.text.cleared = Color::from_rgba8_unpremul(0, 0, 0, 0);
            let frame = RingFrame::new(&ring, &state);
            assert_eq!(frame.text.as_deref(), Some("Cleared"));
            let (without, (_, cy)) = render(&ring, &frame, scale).unwrap().unwrap();
            let (w, h) = text_size(&ring, scale, "Cleared");
            let want = with_text(
                &without,
                &ring,
                scale,
                "Cleared",
                (drawn_cx(&without) - w / 2., cy * scale - h / 2.),
                red,
            );

            ring.colors.text.cleared = red;
            let (mut got, _) = render(&ring, &frame, scale).unwrap().unwrap();
            let mut without = without;
            assert_ne!(
                want,
                without.data().unwrap().to_vec(),
                "the message drew nothing at scale {scale}"
            );
            assert!(
                got.data().unwrap().to_vec() == want,
                "message not centred on the ring at scale {scale}"
            );
        }
    }

    /// The keyboard layout box: under the ring, centred, `layout_bg` fill, `layout_border`
    /// 2 px stroke, text at the box's padding. Colours are opaque and distinct; the text is
    /// checked like the message (its metrics depend on the font, so the box size is taken from
    /// the same Pango layout).
    #[test]
    fn layout_box_geometry_and_colours() {
        let mut state = LockState::new(&General::default());
        state.layout_names = vec!["English (US)".into()];
        let bg = Color::from_rgba8_unpremul(10, 20, 30, 255);
        let border = Color::from_rgba8_unpremul(200, 100, 50, 255);
        let text = Color::from_rgba8_unpremul(0, 255, 255, 255);
        for scale in [1., 2.] {
            let mut ring = RingIndicator {
                idle_visible: true,
                show_keyboard_layout: ShowLayout::Always,
                ..RingIndicator::default()
            };
            ring.colors.layout_bg = bg;
            ring.colors.layout_border = border;
            ring.colors.layout_text = Color::from_rgba8_unpremul(0, 0, 0, 0);
            let frame = RingFrame::new(&ring, &state);
            assert_eq!(frame.layout_text.as_deref(), Some("English (US)"));
            let (mut surface, _) = render(&ring, &frame, scale).unwrap().unwrap();

            // The box, from the layout's own size.
            let (w, h) = text_size(&ring, scale, "English (US)");
            let pad = 4. * scale;
            let diameter = (ring.radius + ring.thickness) * scale * 2.;
            let (x, y) = (drawn_cx(&surface) - w / 2. - pad, diameter);
            let (bw, bh) = (w + 2. * pad, h + 2. * pad);
            let rgba = |c: Color| {
                [
                    (c.r * 255.).round() as u8,
                    (c.g * 255.).round() as u8,
                    (c.b * 255.).round() as u8,
                    255,
                ]
            };
            // Inside: the fill. On the edges: the border (2 px, centred on the edge). Outside the
            // box on the side: nothing.
            assert_eq!(pixel(&mut surface, x + bw / 2., y + bh / 2.), rgba(bg));
            assert_eq!(pixel(&mut surface, x, y + bh / 2.), rgba(border));
            assert_eq!(pixel(&mut surface, x + bw - 1., y + bh / 2.), rgba(border));
            assert_eq!(pixel(&mut surface, x + bw / 2., y + bh - 1.), rgba(border));
            assert_eq!(pixel(&mut surface, x - 3., y + bh / 2.)[3], 0);

            // The text: where the padding puts it.
            let want = with_text(
                &surface,
                &ring,
                scale,
                "English (US)",
                (x + pad, y + pad),
                text,
            );
            ring.colors.layout_text = text;
            let (mut got, _) = render(&ring, &frame, scale).unwrap().unwrap();
            assert!(
                got.data().unwrap().to_vec() == want,
                "layout text not at the box padding at scale {scale}"
            );
        }
    }
}
