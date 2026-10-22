//! swaylock's unlock indicator, drawn with cairo into a texture.
//!
//! A port of swaylock's `render_frame` (`render.c`), using Pango for text so font descriptions
//! match the rest of swaywardlock. Geometry and colour choice follow swaylock line by line.

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
    let mut font = pango::FontDescription::from_string(&ring.font);
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
    layout.set_text(text);
    layout
}

/// Render the ring at `scale` (buffer pixels per logical pixel). Returns the image and the
/// logical offset of the ring's centre within it, or `None` when the ring is hidden.
pub fn render(
    ring: &RingIndicator,
    frame: &RingFrame,
    scale: f64,
) -> anyhow::Result<Option<(cairo::ImageSurface, (f64, f64))>> {
    if !frame.visible {
        return Ok(None);
    }

    let arc_radius = ring.radius * scale;
    let arc_thickness = ring.thickness * scale;
    let diameter = (arc_radius + arc_thickness) * 2.;
    let font = font(ring, arc_radius);
    let box_padding = 4. * scale;

    // Measure text to size the buffer, as swaylock does.
    let mut width = diameter;
    let mut height = diameter;
    {
        let probe = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)?;
        let cr = cairo::Context::new(&probe)?;
        if let Some(text) = &frame.text {
            let (w, _) = layout(&cr, &font, text).pixel_size();
            width = width.max(f64::from(w));
        }
        if let Some(text) = &frame.layout_text {
            let (w, h) = layout(&cr, &font, text).pixel_size();
            height += f64::from(h) + 2. * box_padding;
            width = width.max(f64::from(w) + 2. * box_padding);
        }
    }
    let (bw, bh) = (width.ceil() as i32, height.ceil() as i32);

    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, bw, bh)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_antialias(cairo::Antialias::Best);
    let cx = f64::from(bw) / 2.;
    let cy = diameter / 2.;

    // Fill inner circle.
    cr.arc(cx, cy, arc_radius - arc_thickness / 2., 0., 2. * PI);
    set_source(&cr, frame.pick(ring, &ring.colors.inside));
    cr.fill()?;

    // Ring.
    cr.set_line_width(arc_thickness);
    cr.arc(cx, cy, arc_radius, 0., 2. * PI);
    set_source(&cr, frame.pick(ring, &ring.colors.ring));
    cr.stroke()?;

    // Message.
    if let Some(text) = &frame.text {
        let l = layout(&cr, &font, text);
        let (w, h) = l.pixel_size();
        cr.move_to(cx - f64::from(w) / 2., cy - f64::from(h) / 2.);
        set_source(&cr, frame.pick_text(ring));
        pangocairo::functions::show_layout(&cr, &l);
        cr.new_path();
    }

    // Typing indicator: highlight a random part of the ring on each key.
    if matches!(frame.input, InputState::Letter | InputState::Backspace) {
        let start = f64::from(frame.highlight_start) * (PI / 1024.);
        cr.arc(cx, cy, arc_radius, start, start + HIGHLIGHT_RANGE);
        let caps = frame.caps_lock && ring.show_caps_lock_indicator;
        let c = &ring.colors;
        set_source(
            &cr,
            match (frame.input == InputState::Letter, caps) {
                (true, false) => c.key_highlight,
                (true, true) => c.caps_lock_key_highlight,
                (false, false) => c.bs_highlight,
                (false, true) => c.caps_lock_bs_highlight,
            },
        );
        cr.stroke()?;

        // Separators at both ends of the highlight.
        let inner = diameter / 2. - arc_thickness * 1.5;
        let outer = diameter / 2. - arc_thickness / 2.;
        cr.set_line_width(2. * scale);
        set_source(&cr, ring.colors.separator);
        for angle in [start, start + HIGHLIGHT_RANGE] {
            cr.move_to(cx + angle.cos() * inner, cy + angle.sin() * inner);
            cr.line_to(cx + angle.cos() * outer, cy + angle.sin() * outer);
            cr.stroke()?;
        }
    }

    // Inner and outer border lines.
    let line_set = match ring.line_uses {
        LineUses::Line => &ring.colors.line,
        LineUses::Inside => &ring.colors.inside,
        LineUses::Ring => &ring.colors.ring,
    };
    set_source(&cr, frame.pick(ring, line_set));
    cr.set_line_width(2. * scale);
    cr.arc(cx, cy, arc_radius - arc_thickness / 2., 0., 2. * PI);
    cr.stroke()?;
    cr.arc(cx, cy, arc_radius + arc_thickness / 2., 0., 2. * PI);
    cr.stroke()?;

    // Keyboard layout in a box under the ring.
    if let Some(text) = &frame.layout_text {
        let l = layout(&cr, &font, text);
        let (w, h) = l.pixel_size();
        let (w, h) = (f64::from(w), f64::from(h));
        let x = cx - w / 2. - box_padding;
        let y = diameter;
        cr.rectangle(x, y, w + 2. * box_padding, h + 2. * box_padding);
        set_source(&cr, ring.colors.layout_bg);
        cr.fill_preserve()?;
        set_source(&cr, ring.colors.layout_border);
        cr.set_line_width(1. * scale);
        cr.stroke()?;
        cr.move_to(x + box_padding, y + box_padding);
        set_source(&cr, ring.colors.layout_text);
        pangocairo::functions::show_layout(&cr, &l);
    }

    drop(cr);
    Ok(Some((surface, (cx / scale, cy / scale))))
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
        assert_eq!((surface.width(), surface.height()), (120, 120));

        // Sample the ring band at the far right of the circle: wrong-state ring colour 7d3300.
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let (x, y) = (60 + 50, 60);
        let px = &data[y * stride + x * 4..][..4];
        // cairo ARGB32 little-endian: B G R A.
        assert_eq!((px[2], px[1], px[0], px[3]), (0x7d, 0x33, 0x00, 0xff));
    }
}
