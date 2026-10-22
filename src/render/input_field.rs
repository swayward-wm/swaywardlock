//! The `input-field` widget: hyprlock's password field.
//!
//! [`Look::new`] is a pure function from config + lock state to what to draw, following hyprlock's
//! `CPasswordInputField::updateColors` / `updatePlaceholder` / `draw`. The rendering below turns a
//! `Look` into elements with the shape renderer (box, outline) and textures (dots text,
//! placeholder).

use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::config::{Color, DotRadius, Gradient, InputField};
use crate::state::{AuthState, LockState};

/// What the field looks like this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub outline: Gradient,
    pub inner: Color,
    pub font: Color,
    /// Number of dots (typed characters). 0 while checking unless a check text is set, as in
    /// hyprlock.
    pub dots: usize,
    /// The placeholder / check / fail text to show (already `$VAR`-substituted by the caller),
    /// or `None` when dots show instead.
    pub text_template: Option<String>,
    /// Hide-input: the random recolour for this keystroke, as (colour, quadrant 0..4).
    pub hidden: Option<(Color, u8)>,
    /// Overall opacity (fade-on-empty).
    pub visible: bool,
}

impl Look {
    pub fn new(f: &InputField, s: &LockState, fail_shown: bool) -> Self {
        let checking = s.auth == AuthState::Validating;
        let len = s.password_len();
        let numlock = s.num_lock != f.invert_numlock;
        let borderless = f.outline.width == 0.;

        // hyprlock: caps+num (if set) > caps > num (if set); then checking / fail override.
        let mut target: Option<Gradient> = None;
        if s.caps_lock && numlock && f.bothlock_color.is_some() {
            target = f.bothlock_color;
        } else if s.caps_lock {
            target = Some(f.capslock_color.unwrap_or(f.fail.color));
        } else if numlock && f.numlock_color.is_some() {
            target = f.numlock_color;
        }
        if checking {
            target = Some(f.check.color);
        } else if fail_shown && len == 0 {
            target = Some(f.fail.color);
        }

        let mut outline = f.outline.gradient;
        let mut inner = f.inner_color;
        let mut font = f.font_color;
        if fail_shown {
            font = f.fail.color.from;
        } else if checking && !f.check.text.is_empty() {
            font = f.check.color.from;
        }
        if let Some(t) = target {
            match (borderless, f.swap_font_color) {
                (true, true) => font = t.from,
                (true, false) => {
                    inner = t.from;
                    // When the inner colour changes, the font cannot be the fail colour.
                    font = f.font_color;
                }
                (false, _) => outline = t,
            }
        }

        let text_template = if len > 0 {
            None
        } else if fail_shown {
            Some(f.fail.text.clone())
        } else if checking && !f.check.text.is_empty() {
            Some(f.check.text.clone())
        } else if checking {
            None
        } else {
            Some(f.placeholder.clone())
        };

        let dots = if checking && f.check.text.is_empty() {
            0
        } else {
            len
        };

        let hidden = f
            .hide_input
            .filter(|_| len > 0 && !checking)
            .map(|base| hidden_colour(base, s.keystrokes));

        Self {
            outline,
            inner,
            font,
            dots,
            text_template,
            hidden,
            visible: true,
        }
    }
}

/// hyprlock's hide-input: rotate the base colour's OkLab hue to a pseudo-random angle and pick a
/// different quadrant on each keystroke. Seeded by the keystroke count, so it changes per key
/// but never reflects the password itself.
fn hidden_colour(base: Color, keystrokes: u64) -> (Color, u8) {
    let mut rng = fastrand::Rng::with_seed(keystrokes.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let (l, a, b) = srgb_to_oklab(base);
    let chroma = (a * a + b * b).sqrt();
    let hue = rng.f32() * std::f32::consts::TAU;
    let c = oklab_to_srgb(l, chroma * hue.cos(), chroma * hue.sin(), base.a);
    (c, (keystrokes % 4) as u8)
}

fn srgb_to_oklab(c: Color) -> (f32, f32, f32) {
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
    let l = (0.412_221_5 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    (
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    )
}

fn oklab_to_srgb(l: f32, a: f32, b: f32, alpha: f32) -> Color {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let bb = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    let enc = |v: f32| {
        let v = v.clamp(0., 1.);
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    Color::from_array_unpremul([enc(r), enc(g), enc(bb), alpha])
}

/// The side of a square dot in a field of height `h` (hyprlock's `RECTPASSSIZE`); also the font
/// size for text dots.
pub fn dot_size(f: &InputField, h: f64) -> f64 {
    ((h * f.dots.size * 0.5).round() * 2.).max(1.)
}

/// Dot geometry within a field of `size`, following hyprlock's `draw`: returns the rectangles of
/// visible dots (logical, relative to the field's top-left) and the dot corner radius. Text dots
/// pass their texture's size as `text`; square dots pass `None`.
pub fn dot_rects(
    f: &InputField,
    size: Size<f64, Logical>,
    count: usize,
    text: Option<Size<f64, Logical>>,
) -> (Vec<Rectangle<f64, Logical>>, f64) {
    let square = dot_size(f, size.h);
    let dot = text.unwrap_or(Size::from((square, square)));
    let d = dot.w.max(1.);
    let spacing = (d * f.dots.spacing).floor();
    let pad = (size.h - dot.h) / 2.;
    let area = size.w - 2. * pad;
    let max_dots = ((area / (d + spacing)).round() as usize).max(1);
    let shown = count.min(max_dots);
    let width = (d + spacing) * shown as f64 - spacing;
    let x0 = if f.dots.center {
        (area - width) / 2. + pad
    } else {
        pad
    };
    let rects = (0..shown)
        .map(|i| Rectangle::new(Point::from((x0 + i as f64 * (d + spacing), pad)), dot))
        .collect();
    let round = d / 2.;
    let radius = match f.dots.corner_radius {
        DotRadius::Round => round,
        DotRadius::Inherit => f
            .corner_radius
            .map_or(round, |r| f64::from(r.top_left) * f.dots.size),
        DotRadius::Px(r) => r,
    };
    (rects, radius.min(dot.w.min(dot.h) / 2.))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use smithay_client_toolkit::seat::keyboard::Keysym;

    use super::*;
    use crate::config::General;

    fn typed(s: &mut LockState, text: &str) {
        for c in text.chars() {
            s.on_key(
                Keysym::from_char(c),
                Some(&c.to_string()),
                false,
                Instant::now(),
            );
        }
    }

    #[test]
    fn idle_shows_placeholder_and_default_colours() {
        let f = InputField::default();
        let s = LockState::new(&General::default());
        let look = Look::new(&f, &s, false);
        assert_eq!(look.text_template.as_deref(), Some("<i>Input Password</i>"));
        assert_eq!(look.outline, f.outline.gradient);
        assert_eq!(look.dots, 0);
    }

    #[test]
    fn typing_shows_dots_and_capslock_colours_the_outline() {
        let f = InputField::default();
        let mut s = LockState::new(&General::default());
        typed(&mut s, "abc");
        let look = Look::new(&f, &s, false);
        assert_eq!((look.dots, look.text_template.clone()), (3, None));
        s.caps_lock = true;
        // No capslock colour set: hyprlock falls back to the fail colour.
        assert_eq!(Look::new(&f, &s, false).outline, f.fail.color);
    }

    #[test]
    fn checking_and_failed_states() {
        let mut f = InputField::default();
        let mut s = LockState::new(&General::default());
        typed(&mut s, "pw");
        s.on_key(Keysym::Return, None, false, Instant::now());
        let _ = s.take_password();
        let look = Look::new(&f, &s, false);
        assert_eq!(look.outline, f.check.color);
        assert_eq!((look.dots, look.text_template.clone()), (0, None));
        f.check.text = "Checking…".into();
        assert_eq!(
            Look::new(&f, &s, false).text_template.as_deref(),
            Some("Checking…")
        );

        s.on_auth_failed(Instant::now());
        let look = Look::new(&f, &s, true);
        assert_eq!(look.outline, f.fail.color);
        assert_eq!(look.font, f.fail.color.from);
        assert_eq!(look.text_template.as_deref(), Some("<i>$FAIL</i>"));
    }

    #[test]
    fn borderless_recolours_the_inner_box() {
        let mut f = InputField::default();
        f.outline.width = 0.;
        let mut s = LockState::new(&General::default());
        s.caps_lock = true;
        let look = Look::new(&f, &s, false);
        assert_eq!(look.inner, f.fail.color.from);
        assert_eq!(look.font, f.font_color);
        f.swap_font_color = true;
        let look = Look::new(&f, &s, false);
        assert_eq!((look.inner, look.font), (f.inner_color, f.fail.color.from));
    }

    #[test]
    fn hide_input_changes_per_keystroke_but_is_not_the_password() {
        let f = InputField {
            hide_input: Some(Color::from_rgba8_unpremul(0, 255, 153, 238)),
            ..InputField::default()
        };
        let mut a = LockState::new(&General::default());
        let mut b = LockState::new(&General::default());
        typed(&mut a, "x");
        typed(&mut b, "y");
        // Same keystroke count, different characters: identical look.
        assert_eq!(
            Look::new(&f, &a, false).hidden,
            Look::new(&f, &b, false).hidden
        );
        typed(&mut a, "x");
        assert_ne!(
            Look::new(&f, &a, false).hidden,
            Look::new(&f, &b, false).hidden
        );
    }

    /// Text dots use the glyph's own size, and spacing follows its width (hyprlock).
    #[test]
    fn text_dots_use_their_natural_size() {
        let f = InputField::default();
        let size = Size::from((400., 90.));
        let (rects, _) = dot_rects(&f, size, 2, Some(Size::from((30., 20.))));
        assert_eq!(rects[0].size, Size::from((30., 20.)));
        // spacing floor(30 * 0.2) = 6; vertically centred.
        assert_eq!(rects[1].loc.x - rects[0].loc.x, 36.);
        assert_eq!(rects[0].loc.y, 35.);
    }

    #[test]
    fn dot_radius_sentinels() {
        let mut f = InputField::default();
        let size = Size::from((400., 90.));
        f.dots.corner_radius = DotRadius::Px(4.);
        assert_eq!(dot_rects(&f, size, 1, None).1, 4.);
        // Inherit: the field's radius times the dot size, or round when the field is round.
        f.dots.corner_radius = DotRadius::Inherit;
        assert_eq!(dot_rects(&f, size, 1, None).1, 11.);
        f.corner_radius = Some(crate::config::CornerRadius::from(20.));
        assert_eq!(dot_rects(&f, size, 1, None).1, 5.);
    }

    #[test]
    fn dots_centre_and_cap_at_the_field_width() {
        let f = InputField::default();
        let size = Size::from((400., 90.));
        let (rects, r) = dot_rects(&f, size, 3, None);
        // hyprlock: dot = round(90 * 0.25 * 0.5) * 2 = 22, spacing floor(22 * 0.2) = 4.
        assert_eq!(rects[0].size, Size::from((22., 22.)));
        assert_eq!(rects[1].loc.x - rects[0].loc.x, 26.);
        assert_eq!(r, 11.);
        let mid = (rects[0].loc.x + rects[2].loc.x + 22.) / 2.;
        assert!((mid - 200.).abs() < 0.5, "centred: {mid}");
        // hyprlock caps at round(area / (dot + spacing)) = round(332 / 26) = 13 dots, which can
        // run a little into the padding but never past the field.
        let (many, _) = dot_rects(&f, size, 500, None);
        assert_eq!(many.len(), 13);
        assert!(many.last().unwrap().loc.x + 22. <= 400.);
    }

    #[test]
    fn oklab_round_trips() {
        let c = Color::from_rgba8_unpremul(0x20, 0x80, 0xc0, 0xff);
        let (l, a, b) = srgb_to_oklab(c);
        let back = oklab_to_srgb(l, a, b, 1.);
        assert!(
            (back.r - c.r).abs() < 1e-3
                && (back.g - c.g).abs() < 1e-3
                && (back.b - c.b).abs() < 1e-3
        );
    }
}
