//! The input field's animated state, following hyprlock's `CPasswordInputField` (`updateFade`,
//! `updateDots`, `updateWidth`, `updateColors`): five animated values (fade, dots, width, inner
//! and outline colour) retargeted from the lock state every frame.
//!
//! [`FieldAnim::update`] is pure given a clock: tests drive it with a fake one. Colours blend in
//! OkLab with linear alpha, as hyprlock does.

use std::time::Duration;

use crate::animation::{Animation, Clock};
use crate::config::{self, Animations, Color, Gradient};
use crate::render::input_field::{oklab_to_srgb, srgb_to_oklab};

/// What the field's animations follow, gathered by the caller each frame.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// `fade-on-empty`: the timeout in ms, or `None` when the field never fades.
    pub fade_on_empty: Option<u32>,
    /// Typed characters.
    pub len: usize,
    /// Authenticating right now.
    pub checking: bool,
    /// The check text is empty (dots then stay while checking).
    pub check_text_empty: bool,
    /// Configured size.
    pub width: f64,
    pub height: f64,
    /// Width of the placeholder / check / fail text asset, if there is one.
    pub text_width: Option<f64>,
    /// Target outline and inner colours.
    pub outline: Gradient,
    pub inner: Color,
    pub borderless: bool,
}

/// How far an eased progress may overshoot 0..1: the range validate allows for a cubic-bezier's
/// control y values (a Bézier stays within its control points; a spring overshoots less).
const MIN_PROGRESS: f64 = -2.;
const MAX_PROGRESS: f64 = 3.;

/// One animated scalar. The animation runs a 0..1 progress from `from` to `goal`, so a spring's
/// settle time does not depend on the distance (validate caps it at the unit range). A
/// zero-length or `off` animation warps.
#[derive(Debug, Clone)]
pub(crate) struct Var {
    from: f64,
    pub(crate) goal: f64,
    anim: Option<Animation>,
}

impl Var {
    pub(crate) fn new(value: f64) -> Self {
        Self {
            from: value,
            goal: value,
            anim: None,
        }
    }

    pub(crate) fn value(&self) -> f64 {
        self.anim.as_ref().map_or(self.goal, |a| {
            let p = a.clamped_value().clamp(MIN_PROGRESS, MAX_PROGRESS);
            self.from + (self.goal - self.from) * p
        })
    }

    pub(crate) fn animating(&self) -> bool {
        self.anim.as_ref().is_some_and(|a| !a.is_done())
    }

    pub(crate) fn warp(&mut self, value: f64) {
        self.from = value;
        self.goal = value;
        self.anim = None;
    }

    /// Animate from the current value to `to`; restarts from where it is. An overshooting curve
    /// (or spring) restarts from the overshot value clamped to the span of the old animation and
    /// the new goal: starting from the overshoot itself grew the value with every retarget,
    /// without bound. Every start then stays within the goals seen so far. Including `to` keeps
    /// a retarget further in the overshoot's direction from jumping back.
    pub(crate) fn set(&mut self, to: f64, cfg: config::Animation, clock: &Clock) {
        if to == self.goal {
            return;
        }
        let start = if self.anim.is_some() {
            let lo = self.from.min(self.goal).min(to);
            let hi = self.from.max(self.goal).max(to);
            self.value().clamp(lo, hi)
        } else {
            self.goal
        };
        let anim = Animation::new(clock.clone(), 0., 1., 0., cfg);
        self.from = start;
        self.goal = to;
        self.anim = (!cfg.off && !anim.is_done()).then_some(anim);
    }
}

pub(crate) trait Blend: Copy + PartialEq {
    fn blend(a: Self, b: Self, t: f32) -> Self;
}

pub(crate) fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    if t <= 0. || a == b {
        return a;
    }
    if t >= 1. {
        return b;
    }
    let (la, aa, ba) = srgb_to_oklab(a);
    let (lb, ab, bb) = srgb_to_oklab(b);
    let mix = |x: f32, y: f32| x + (y - x) * t;
    oklab_to_srgb(mix(la, lb), mix(aa, ab), mix(ba, bb), mix(a.a, b.a))
}

impl Blend for Color {
    fn blend(a: Self, b: Self, t: f32) -> Self {
        lerp_color(a, b, t)
    }
}

impl Blend for Gradient {
    fn blend(a: Self, b: Self, t: f32) -> Self {
        if t <= 0. || a == b {
            return a;
        }
        if t >= 1. {
            return b;
        }
        Gradient {
            from: lerp_color(a.from, b.from, t),
            to: lerp_color(a.to, b.to, t),
            angle: (f32::from(a.angle) + (f32::from(b.angle) - f32::from(a.angle)) * t).round()
                as i16,
            in_: b.in_,
        }
    }
}

/// A colour animating from where it was to a target, as a 0..1 progress.
#[derive(Debug, Clone)]
pub(crate) struct Mix<T: Blend> {
    from: T,
    to: T,
    t: Var,
}

impl<T: Blend> Mix<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            from: value,
            to: value,
            t: Var::new(1.),
        }
    }

    pub(crate) fn value(&self) -> T {
        T::blend(self.from, self.to, self.t.value().clamp(0., 1.) as f32)
    }

    pub(crate) fn animating(&self) -> bool {
        self.t.animating()
    }

    /// Jump to `value`, ending any blend.
    pub(crate) fn warp(&mut self, value: T) {
        *self = Self::new(value);
    }

    /// Restart from the current value towards `target`, when that differs from the goal.
    pub(crate) fn retarget(&mut self, target: T, cfg: config::Animation, clock: &Clock) {
        if target == self.to {
            return;
        }
        self.from = self.value();
        self.to = target;
        self.t = Var::new(0.);
        self.t.set(1., cfg, clock);
    }
}

#[derive(Debug, Clone)]
pub struct FieldAnim {
    fade: Var,
    dots: Var,
    width: Var,
    outline: Mix<Gradient>,
    inner: Mix<Color>,
    /// The configured size the width animates around; a change (output resize) warps.
    base: Option<(f64, f64)>,
    /// When the fade-out timer fires, while armed.
    fade_deadline: Option<Duration>,
    /// The timer fired: the next unused frame fades out.
    allow_fade_out: bool,
}

impl FieldAnim {
    pub fn new(outline: Gradient, inner: Color) -> Self {
        Self {
            // Starts hidden: with fade-on-empty the field appears with the first key.
            fade: Var::new(0.),
            dots: Var::new(0.),
            width: Var::new(0.),
            outline: Mix::new(outline),
            inner: Mix::new(inner),
            base: None,
            fade_deadline: None,
            allow_fade_out: false,
        }
    }

    pub fn update(&mut self, i: &Inputs, clock: &Clock, anims: &Animations) {
        let now = clock.now();
        self.update_fade(i, now, clock, anims);
        self.update_dots(i, clock, anims);
        self.update_colours(i, clock, anims);
        self.update_width(i, clock, anims);
    }

    fn update_fade(&mut self, i: &Inputs, now: Duration, clock: &Clock, anims: &Animations) {
        let cfg = anims.get(anims.input_field_fade);
        let Some(timeout) = i.fade_on_empty else {
            self.fade.warp(1.);
            self.fade_deadline = None;
            self.allow_fade_out = false;
            return;
        };
        let used = i.len > 0 || i.checking;
        if used {
            self.allow_fade_out = false;
            self.fade_deadline = None;
            if self.fade.goal != 1. {
                self.fade.set(1., cfg, clock);
            }
        } else if self.fade.goal != 0. {
            if self.fade_deadline.is_some_and(|d| now >= d) {
                self.fade_deadline = None;
                self.allow_fade_out = true;
            }
            if self.allow_fade_out || timeout == 0 {
                self.fade.set(0., cfg, clock);
                self.allow_fade_out = false;
                self.fade_deadline = None;
            } else if self.fade_deadline.is_none() {
                self.fade_deadline = Some(now + Duration::from_millis(u64::from(timeout)));
            }
        }
    }

    fn update_dots(&mut self, i: &Inputs, clock: &Clock, anims: &Animations) {
        if self.dots.goal == i.len as f64 {
            return;
        }
        // The password buffer is already empty while it is being checked: hold the dots.
        if i.checking && i.check_text_empty {
            return;
        }
        if i.len == 0 {
            self.dots.warp(0.);
        } else {
            self.dots
                .set(i.len as f64, anims.get(anims.input_field_dots), clock);
        }
    }

    fn update_colours(&mut self, i: &Inputs, clock: &Clock, anims: &Animations) {
        let cfg = anims.get(anims.input_field_colors);
        if !i.borderless {
            self.outline.retarget(i.outline, cfg, clock);
        }
        self.inner.retarget(i.inner, cfg, clock);
    }

    fn update_width(&mut self, i: &Inputs, clock: &Clock, anims: &Animations) {
        let mut target = i.width;
        if i.len == 0 {
            if let Some(text) = i.text_width {
                target = text + i.height;
            }
        }
        let target = target.max(i.width);
        // Starts at the configured width, as hyprlock does, and animates to the target.
        if self.base != Some((i.width, i.height)) {
            self.base = Some((i.width, i.height));
            self.width.warp(i.width);
        }
        self.width
            .set(target, anims.get(anims.input_field_width), clock);
    }

    /// Fade, 0 (hidden) to 1.
    pub fn alpha(&self) -> f32 {
        self.fade.value().clamp(0., 1.) as f32
    }

    pub fn width(&self) -> f64 {
        self.width.value().max(0.)
    }

    /// Fractional number of dots: at most one past the typed count, whatever the curve does.
    pub fn dots(&self) -> f64 {
        self.dots.value().clamp(0., self.dots.goal.max(0.) + 1.)
    }

    pub fn outline(&self) -> Gradient {
        self.outline.value()
    }

    pub fn inner(&self) -> Color {
        self.inner.value()
    }

    pub fn width_animating(&self) -> bool {
        self.width.animating()
    }

    /// Something needs frames at the redraw rate.
    pub fn animating(&self) -> bool {
        self.fade.animating()
            || self.dots.animating()
            || self.width.animating()
            || self.outline.animating()
            || self.inner.animating()
    }

    /// When the fade-out timer fires (clock time), if one is armed: the one thing that needs a
    /// redraw without anything animating.
    pub fn next_wake(&self) -> Option<Duration> {
        self.fade_deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AnimationCurve, AnimationKind, EasingParams};

    fn linear(ms: u32) -> config::Animation {
        config::Animation {
            off: false,
            kind: AnimationKind::Easing(EasingParams {
                duration_ms: ms,
                curve: AnimationCurve::Linear,
            }),
        }
    }

    fn anims(ms: u32) -> Animations {
        Animations {
            off: false,
            fade_in: linear(ms),
            fade_out: linear(ms),
            input_field_fade: linear(ms),
            input_field_width: linear(ms),
            input_field_colors: linear(ms),
            input_field_dots: linear(ms),
            ..Animations::default()
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn inputs() -> Inputs {
        Inputs {
            fade_on_empty: Some(2000),
            len: 0,
            checking: false,
            check_text_empty: true,
            width: 100.,
            height: 40.,
            text_width: None,
            outline: Color::from_rgba8_unpremul(0, 0, 255, 255).into(),
            inner: Color::from_rgba8_unpremul(255, 255, 255, 255),
            borderless: false,
        }
    }

    struct Rig {
        clock: Clock,
        anim: FieldAnim,
        cfg: Animations,
    }

    impl Rig {
        fn new(cfg: Animations) -> Self {
            let i = inputs();
            Self {
                clock: Clock::with_time(ms(1000)),
                anim: FieldAnim::new(i.outline, i.inner),
                cfg,
            }
        }

        fn at(&mut self, t: u64, i: &Inputs) {
            self.clock.set_unadjusted(ms(t));
            self.anim.update(i, &self.clock, &self.cfg);
        }
    }

    #[test]
    fn starts_hidden_and_shows_on_the_first_key() {
        let mut r = Rig::new(anims(100));
        r.at(1000, &inputs());
        assert_eq!(r.anim.alpha(), 0.);
        assert!(!r.anim.animating() && r.anim.next_wake().is_none());
        r.at(1010, &Inputs { len: 1, ..inputs() });
        assert!(r.anim.animating());
        r.at(1060, &Inputs { len: 1, ..inputs() });
        assert!((r.anim.alpha() - 0.5).abs() < 0.01, "{}", r.anim.alpha());
        r.at(1200, &Inputs { len: 1, ..inputs() });
        assert_eq!(r.anim.alpha(), 1.);
        assert!(!r.anim.animating());
    }

    #[test]
    fn waits_for_the_timeout_then_fades_and_input_cancels_it() {
        let mut r = Rig::new(anims(100));
        let typed = Inputs { len: 1, ..inputs() };
        r.at(1000, &typed);
        r.at(1200, &typed);
        // Emptied at t = 1300: a timer for start + timeout, nothing fading yet.
        r.at(1300, &inputs());
        assert_eq!(r.anim.next_wake(), Some(ms(1300 + 2000)));
        r.at(2500, &inputs());
        assert_eq!(r.anim.alpha(), 1.);
        // A key cancels it.
        r.at(2600, &typed);
        assert_eq!(r.anim.next_wake(), None);
        r.at(2700, &inputs());
        assert_eq!(r.anim.next_wake(), Some(ms(4700)));
        // The timer fires: the fade starts and the timer is gone.
        r.at(4700, &inputs());
        assert_eq!(r.anim.next_wake(), None);
        assert!(r.anim.animating());
        r.at(4750, &inputs());
        assert!((r.anim.alpha() - 0.5).abs() < 0.01);
        r.at(4900, &inputs());
        assert_eq!(r.anim.alpha(), 0.);
    }

    #[test]
    fn timeout_zero_fades_at_once() {
        let mut r = Rig::new(anims(100));
        let typed = Inputs {
            len: 1,
            fade_on_empty: Some(0),
            ..inputs()
        };
        r.at(1000, &typed);
        r.at(1200, &typed);
        r.at(
            1300,
            &Inputs {
                fade_on_empty: Some(0),
                ..inputs()
            },
        );
        assert_eq!(r.anim.next_wake(), None);
        assert!(r.anim.animating());
    }

    #[test]
    fn fade_off_and_global_off_are_instant() {
        // fade-on-empty off: always visible.
        let mut r = Rig::new(anims(100));
        r.at(
            1000,
            &Inputs {
                fade_on_empty: None,
                ..inputs()
            },
        );
        assert_eq!(r.anim.alpha(), 1.);
        assert!(!r.anim.animating());
        // The fade animation off (or the global off): shows on the key with no animation.
        let mut cfg = anims(100);
        cfg.off = true;
        let mut r = Rig::new(cfg);
        r.at(1000, &inputs());
        assert_eq!(r.anim.alpha(), 0.);
        r.at(1000, &Inputs { len: 1, ..inputs() });
        assert_eq!(r.anim.alpha(), 1.);
        assert!(!r.anim.animating());
    }

    #[test]
    fn checking_counts_as_used() {
        let mut r = Rig::new(anims(100));
        let checking = Inputs {
            checking: true,
            ..inputs()
        };
        r.at(1000, &checking);
        r.at(1200, &checking);
        assert_eq!(r.anim.alpha(), 1.);
        assert_eq!(r.anim.next_wake(), None);
    }

    #[test]
    fn width_grows_for_text_and_animates() {
        let mut r = Rig::new(anims(100));
        let text = Inputs {
            text_width: Some(130.),
            ..inputs()
        };
        r.at(1000, &text);
        // Starts at the configured width and grows to cfg 100 + (text 130, height 40) = 170.
        assert_eq!(r.anim.width(), 100.);
        r.at(1050, &text);
        let w = r.anim.width();
        assert!(w > 100. && w < 170., "{w}");
        r.at(1200, &text);
        assert_eq!(r.anim.width(), 170.);
        // Typing returns to the configured width, animated.
        let typed = Inputs {
            len: 1,
            text_width: Some(130.),
            ..inputs()
        };
        r.at(1100, &typed);
        r.at(1150, &typed);
        let w = r.anim.width();
        assert!(w > 100. && w < 170., "{w}");
        assert!(r.anim.width_animating());
        r.at(1300, &typed);
        assert_eq!(r.anim.width(), 100.);
    }

    /// With the check text empty hyprlock keeps the placeholder asset while checking, so the
    /// field stays wide.
    #[test]
    fn width_keeps_the_text_asset_while_checking_with_empty_check_text() {
        let mut r = Rig::new(anims(100));
        let checking = Inputs {
            checking: true,
            text_width: Some(130.),
            ..inputs()
        };
        r.at(1000, &checking);
        r.at(1200, &checking);
        assert_eq!(r.anim.width(), 170.);
    }

    #[test]
    fn width_follows_the_configured_size_without_animating() {
        let mut r = Rig::new(anims(100));
        r.at(1000, &inputs());
        r.at(
            1100,
            &Inputs {
                width: 200.,
                ..inputs()
            },
        );
        assert_eq!(r.anim.width(), 200.);
        assert!(!r.anim.animating());
    }

    #[test]
    fn colours_blend_in_oklab_and_retarget_from_the_current_value() {
        let blue = Color::from_rgba8_unpremul(0, 0, 255, 255);
        let red = Color::from_rgba8_unpremul(255, 0, 0, 255);
        let rgb8 = |c: Color| [c.r, c.g, c.b].map(|v| (v * 255.).round() as i32);
        let close = |got: [i32; 3], want: [i32; 3]| {
            assert!(
                got.iter().zip(want).all(|(a, b)| (a - b).abs() <= 2),
                "{got:?} vs {want:?}"
            );
        };
        close(rgb8(lerp_color(blue, red, 0.25)), [81, 71, 210]);
        close(rgb8(lerp_color(blue, red, 0.5)), [140, 83, 162]);
        close(rgb8(lerp_color(blue, red, 0.75)), [198, 73, 109]);

        let mut r = Rig::new(anims(1000));
        r.at(1000, &inputs());
        let to_red = Inputs {
            outline: red.into(),
            ..inputs()
        };
        r.at(1000, &to_red);
        r.at(1500, &to_red);
        close(rgb8(r.anim.outline().from), [140, 83, 162]);
        // Retarget back to blue halfway: the new animation starts from the mixed colour.
        r.at(1500, &inputs());
        close(rgb8(r.anim.outline().from), [140, 83, 162]);
        r.at(2000, &inputs());
        let mid = lerp_color(lerp_color(blue, red, 0.5), blue, 0.5);
        close(rgb8(r.anim.outline().from), rgb8(mid));
        r.at(2500, &inputs());
        assert_eq!(r.anim.outline().from, blue);
    }

    #[test]
    fn borderless_never_retargets_the_outline() {
        let mut r = Rig::new(anims(100));
        r.at(1000, &inputs());
        let red: Gradient = Color::from_rgba8_unpremul(255, 0, 0, 255).into();
        r.at(
            1000,
            &Inputs {
                outline: red,
                borderless: true,
                ..inputs()
            },
        );
        assert_eq!(r.anim.outline(), inputs().outline);
    }

    #[test]
    fn dots_animate_and_warp_to_zero() {
        let mut r = Rig::new(anims(100));
        let n = |len| Inputs { len, ..inputs() };
        r.at(1000, &n(0));
        r.at(1000, &n(3));
        r.at(1050, &n(3));
        assert!((r.anim.dots() - 1.5).abs() < 0.01, "{}", r.anim.dots());
        r.at(1200, &n(3));
        assert_eq!(r.anim.dots(), 3.);
        r.at(1210, &n(0));
        assert_eq!(r.anim.dots(), 0.);
        assert!(!r.anim.animating());
    }

    fn bezier(ms: u32, y1: f64, y2: f64) -> config::Animation {
        config::Animation {
            off: false,
            kind: AnimationKind::Easing(EasingParams {
                duration_ms: ms,
                curve: AnimationCurve::CubicBezier(0., y1, 1., y2),
            }),
        }
    }

    /// An overshooting curve used to restart each retarget from its overshot value, so the dot
    /// count (and the width) grew with every key until drawing the dots never finished.
    #[test]
    fn overshooting_curves_stay_bounded_across_retargets() {
        for (y1, y2) in [(1000., 1000.), (-1000., -1000.), (3., 3.), (-2., -2.)] {
            let mut cfg = anims(300);
            cfg.input_field_dots = bezier(300, y1, y2);
            cfg.input_field_width = bezier(300, y1, y2);
            let mut r = Rig::new(cfg);
            let mut t = 1000;
            for len in 1..=60 {
                let i = Inputs {
                    len,
                    text_width: Some(if len % 2 == 0 { 300. } else { 10. }),
                    ..inputs()
                };
                for _ in 0..3 {
                    t += 50;
                    r.at(t, &i);
                    let d = r.anim.dots();
                    assert!(
                        d.is_finite() && d <= 3. * len as f64,
                        "{y1}: {len} keys, {d} dots"
                    );
                    let w = r.anim.width();
                    assert!(w.is_finite() && w <= 2000., "{y1}: width {w}");
                }
            }
        }
    }

    /// The worst case: keys at every gap from 20 to 400 ms with a frame
    /// every 16 ms, 1000 keys. Before, y 2.5 grew the dot count to 6e9 and y 2.7 to 2e68.
    /// y 1000 is rejected by validate, but the runtime must hold too.
    #[test]
    fn overshoot_stays_bounded_over_a_thousand_keys_at_any_gap() {
        for y in [2.5, 2.7, 3., 1000.] {
            for gap in (20..=400).step_by(20) {
                let mut cfg = anims(300);
                cfg.input_field_dots = bezier(300, y, y);
                cfg.input_field_width = bezier(300, y, y);
                let mut r = Rig::new(cfg);
                let mut t = 1000;
                for len in 1..=1000 {
                    let i = Inputs {
                        len,
                        text_width: Some(if len % 2 == 0 { 300. } else { 10. }),
                        ..inputs()
                    };
                    let end = t + gap;
                    while t < end {
                        r.at(t, &i);
                        let d = r.anim.dots();
                        assert!(
                            (0.0..=len as f64 + 1.).contains(&d),
                            "y {y} gap {gap}: {len} keys, {d} dots"
                        );
                        let w = r.anim.width();
                        // Between the configured 100 and the 340 text target, plus 2 spans of
                        // overshoot either way.
                        assert!((-380.0..=820.).contains(&w), "y {y} gap {gap}: width {w}");
                        t += 16;
                    }
                }
            }
        }
    }

    /// Retargeting further in the direction of an overshoot continues from the overshot value
    /// (the clamp span includes the new goal); clamping to the old span alone made the
    /// dot count jump back, here from 4 to 3, before rising again.
    #[test]
    fn retarget_past_an_overshoot_does_not_jump_back() {
        let mut cfg = anims(300);
        cfg.input_field_dots = bezier(300, 3., 3.);
        let mut r = Rig::new(cfg);
        let n = |len| Inputs { len, ..inputs() };
        r.at(1000, &n(2));
        r.at(1400, &n(2));
        assert_eq!(r.anim.dots(), 2.);
        r.at(1400, &n(3));
        // Mid-curve the eased progress of 0 3 1 3 is well past 1: past the goal of 3.
        r.at(1550, &n(3));
        let before = r.anim.dots();
        assert!(before > 3.2, "{before}");
        r.at(1550, &n(4));
        let after = r.anim.dots();
        assert!(
            (after - before).abs() < 1e-9,
            "jumped from {before} to {after}"
        );
    }

    /// The e2e field-overshoot shape: a key then Backspace, over and over, so the width retargets
    /// between the configured 100 and the 340 placeholder width every key. Retargeting from the
    /// overshoot compounded it to infinity on the parent.
    #[test]
    fn width_stays_bounded_when_typing_and_erasing_with_overshoot() {
        for y in [2.5, 3., 1000.] {
            for gap in (20..=400).step_by(20) {
                let mut cfg = anims(300);
                cfg.input_field_width = bezier(300, y, y);
                let mut r = Rig::new(cfg);
                let mut t = 1000;
                for k in 0..1000 {
                    let i = Inputs {
                        len: k % 2,
                        text_width: Some(300.),
                        ..inputs()
                    };
                    let end = t + gap;
                    while t < end {
                        r.at(t, &i);
                        let w = r.anim.width();
                        // 100..340 plus two spans of overshoot either way.
                        assert!(
                            (0.0..=820.).contains(&w),
                            "y {y} gap {gap} key {k}: width {w}"
                        );
                        t += 16;
                    }
                }
            }
        }
    }

    /// The buffer is empty while the password is being checked; with no check text the dots
    /// stay as they are (a running animation continues, nothing retargets to 0).
    #[test]
    fn dots_hold_while_checking_without_text() {
        let mut r = Rig::new(anims(100));
        r.at(1000, &Inputs { len: 3, ..inputs() });
        r.at(1200, &Inputs { len: 3, ..inputs() });
        let checking = Inputs {
            checking: true,
            ..inputs()
        };
        r.at(1300, &checking);
        r.at(1900, &checking);
        assert_eq!(r.anim.dots(), 3.);
        // With a check text the dots go away.
        let with_text = Inputs {
            check_text_empty: false,
            ..checking.clone()
        };
        r.at(2000, &with_text);
        assert_eq!(r.anim.dots(), 0.);

        // Mid-animation: the running animation continues to its old target.
        let mut r = Rig::new(anims(100));
        r.at(1000, &Inputs { len: 0, ..inputs() });
        r.at(1000, &Inputs { len: 3, ..inputs() });
        r.at(1050, &checking);
        r.at(1200, &checking);
        assert_eq!(r.anim.dots(), 3.);
    }
}
