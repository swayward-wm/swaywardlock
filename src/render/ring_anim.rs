//! The swayward ring style's animated state (docs/specs/2026-10-20-ring-style.md): colour
//! crossfade, highlight sweep and trail, verifying spinner, wrong shake and flash, appear fade
//! and scale. Retargeted from the [`RingFrame`] every frame, like `field_anim`, and pure given a
//! clock: tests drive it with a fake one.

use std::f64::consts::PI;
use std::time::Duration;

use super::field_anim::{lerp_color, Mix, Var};
use super::ring::{Look, RingFrame};
use crate::animation::Clock;
use crate::config::{Animations, Color, RingIndicator};
use crate::state::AuthState;

/// One spinner turn.
const SPIN_PERIOD: f64 = 1.2;
/// Shake amplitude as a fraction of the radius, and its number of oscillations.
const SHAKE: f64 = 0.08;
const SHAKE_CYCLES: f64 = 3.;
/// Scale the ring appears from.
const APPEAR_SCALE: f64 = 0.9;
/// `highlight_start` units per turn.
const TURN: f64 = 2048.;

#[derive(Debug, Clone)]
pub struct RingAnim {
    started: bool,
    /// The frame drawn: the latest visible one, kept while the ring fades out. `None` once it
    /// is fully hidden.
    shown: Option<RingFrame>,
    inside: Mix<Color>,
    ring: Mix<Color>,
    line: Mix<Color>,
    text: Mix<Color>,
    highlight_colour: Mix<Color>,
    /// The `highlight_start` the highlight is heading to, while one is drawn.
    highlight_target: Option<u32>,
    /// The highlight's start in `highlight_start` units, unwrapped so a sweep never crosses the
    /// long way round.
    highlight: Var,
    sweep_from: f64,
    /// The trail's alpha, 1 to 0 over a sweep.
    trail: Var,
    /// When the spinner started, while verifying.
    spinner_since: Option<Duration>,
    /// Shake and flash progress, 0 to 1; 1 is at rest.
    wrong: Var,
    prev_auth: AuthState,
    /// Opacity, 0 to 1.
    appear: Var,
    /// Fading in (scales too); fading out does not scale.
    appearing: bool,
    now: Duration,
}

impl Default for RingAnim {
    fn default() -> Self {
        let clear = Color::new_unpremul(0., 0., 0., 0.);
        Self {
            started: false,
            shown: None,
            inside: Mix::new(clear),
            ring: Mix::new(clear),
            line: Mix::new(clear),
            text: Mix::new(clear),
            highlight_colour: Mix::new(clear),
            highlight_target: None,
            highlight: Var::new(0.),
            sweep_from: 0.,
            trail: Var::new(0.),
            spinner_since: None,
            wrong: Var::new(1.),
            prev_auth: AuthState::Idle,
            appear: Var::new(0.),
            appearing: false,
            now: Duration::ZERO,
        }
    }
}

impl RingAnim {
    pub fn update(
        &mut self,
        ring: &RingIndicator,
        frame: &RingFrame,
        clock: &Clock,
        anims: &Animations,
    ) {
        self.now = clock.now();
        let colours = anims.get(anims.ring_colors);
        let sweep = anims.get(anims.ring_highlight);
        let appear = anims.get(anims.ring_appear);
        let wrong = anims.get(anims.ring_wrong);

        // The first frame shows end states: the whole-screen fade-in covers the start.
        if !self.started {
            self.started = true;
            self.appear.warp(if frame.visible { 1. } else { 0. });
            self.prev_auth = frame.auth;
        }

        // Keep the unwrapped angle small once a sweep is over.
        if !self.highlight.animating() {
            self.highlight.warp(self.highlight.goal.rem_euclid(TURN));
        }

        if frame.visible {
            if self.appear.goal != 1. {
                // Scale only from fully hidden; back from a partial fade-out it only fades.
                self.appearing = self.appear.value() <= 0.;
                self.appear.set(1., appear, clock);
            }
            // Nothing on screen to blend from: start at the targets.
            let fresh = self.shown.is_none();
            self.shown = Some(frame.clone());
            let (inside, ring_c, line, text) = frame.targets(ring);
            for (mix, target) in [
                (&mut self.inside, inside),
                (&mut self.ring, ring_c),
                (&mut self.line, line),
                (&mut self.text, text),
            ] {
                if fresh {
                    mix.warp(target);
                } else {
                    mix.retarget(target, colours, clock);
                }
            }
            self.update_highlight(ring, frame, fresh, clock, colours, sweep);
        } else {
            // Fade out with the last visible content, then stop drawing.
            if self.appear.goal != 0. {
                self.appearing = false;
                self.appear.set(0., appear, clock);
            }
            if !self.appear.animating() {
                self.shown = None;
                self.highlight_target = None;
                self.trail.warp(0.);
            }
        }

        self.spinner_since = (frame.auth == AuthState::Validating && !anims.off)
            .then(|| self.spinner_since.unwrap_or(self.now));

        if frame.auth == AuthState::Invalid && self.prev_auth != AuthState::Invalid {
            self.wrong = Var::new(0.);
            self.wrong.set(1., wrong, clock);
        }
        self.prev_auth = frame.auth;
    }

    fn update_highlight(
        &mut self,
        ring: &RingIndicator,
        frame: &RingFrame,
        fresh: bool,
        clock: &Clock,
        colours: crate::config::Animation,
        sweep: crate::config::Animation,
    ) {
        if !frame.highlighted() {
            self.highlight_target = None;
            self.trail.warp(0.);
            return;
        }
        let colour = frame.highlight_colour(ring);
        let target = frame.highlight_start;
        match self.highlight_target {
            Some(old) if !fresh => {
                if old != target {
                    // Along the shorter direction, from where it is now.
                    let from = self.highlight.value();
                    let mut delta = (f64::from(target) - from).rem_euclid(TURN);
                    if delta > TURN / 2. {
                        delta -= TURN;
                    }
                    self.sweep_from = from;
                    self.highlight.set(from + delta, sweep, clock);
                    self.trail = Var::new(1.);
                    self.trail.set(0., sweep, clock);
                }
                self.highlight_colour.retarget(colour, colours, clock);
            }
            _ => {
                self.highlight.warp(f64::from(target));
                self.trail.warp(0.);
                self.highlight_colour.warp(colour);
            }
        }
        self.highlight_target = Some(target);
    }

    /// The frame and look to draw, or `None` while the ring is hidden. Values are quantised so
    /// that a settled ring compares equal frame to frame.
    pub fn look(&self, ring: &RingIndicator) -> Option<(RingFrame, Look)> {
        let frame = self.shown.clone()?;
        if self.alpha() <= 0. {
            return None;
        }
        let mut ring_colour = self.ring.value();
        let p = self.wrong_progress();
        if p < 1. {
            // The flash: from the wrong colour, opaque and 20% towards white, to the ring colour.
            let wrong = ring.colors.ring.wrong;
            let white = Color::new_unpremul(1., 1., 1., 1.);
            let start = lerp_color(Color { a: 1., ..wrong }, white, 0.2);
            ring_colour = lerp_color(start, ring_colour, p as f32);
        }
        let head = self.highlight.value();
        let trail = self.trail.value().clamp(0., 1.);
        let look = Look {
            inside: quantise(self.inside.value()),
            ring: quantise(ring_colour),
            line: quantise(self.line.value()),
            text: quantise(self.text.value()),
            highlight: self
                .highlight_target
                .map(|_| (step(head, 8.), quantise(self.highlight_colour.value()))),
            trail: (trail > 0.).then(|| {
                (
                    step(self.sweep_from, 8.),
                    step(head, 8.),
                    step(trail, 255.) as f32,
                )
            }),
            spinner: self.spinner_angle(),
            scale: if self.appearing {
                step(
                    APPEAR_SCALE + (1. - APPEAR_SCALE) * self.appear.value().clamp(0., 1.),
                    1024.,
                )
            } else {
                1.
            },
            glow: ring.glow,
        };
        Some((frame, look))
    }

    /// The spinner's head angle (radians), while verifying; it starts at the top.
    fn spinner_angle(&self) -> Option<f64> {
        let since = self.spinner_since?;
        let turns = self.now.saturating_sub(since).as_secs_f64() / SPIN_PERIOD;
        Some(step(turns.fract(), TURN) * 2. * PI - PI / 2.)
    }

    fn wrong_progress(&self) -> f64 {
        self.wrong.value().clamp(0., 1.)
    }

    /// Opacity of the whole ring, applied when it is placed.
    pub fn alpha(&self) -> f32 {
        self.appear.value().clamp(0., 1.) as f32
    }

    /// Horizontal offset (logical px) of the whole ring: the wrong shake, a damped sine.
    pub fn offset(&self, ring: &RingIndicator) -> f64 {
        let p = self.wrong_progress();
        if p >= 1. {
            return 0.;
        }
        SHAKE * ring.radius * (2. * PI * SHAKE_CYCLES * p).sin() * (1. - p)
    }

    /// Something needs frames at the redraw rate.
    pub fn animating(&self) -> bool {
        let drawn = self.shown.is_some()
            && (self.inside.animating()
                || self.ring.animating()
                || self.line.animating()
                || self.text.animating()
                || self.highlight_colour.animating()
                || self.highlight.animating()
                || self.trail.animating());
        drawn || self.appear.animating() || self.wrong.animating() || self.spinner_since.is_some()
    }
}

fn step(v: f64, per_unit: f64) -> f64 {
    (v * per_unit).round() / per_unit
}

fn quantise(c: Color) -> Color {
    let q = |v: f32| (v.clamp(0., 1.) * 255.).round() / 255.;
    Color::new_unpremul(q(c.r), q(c.g), q(c.b), q(c.a))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use smithay_client_toolkit::seat::keyboard::Keysym;

    use super::*;
    use crate::config::{self, AnimationCurve, AnimationKind, EasingParams, General, RingStyle};
    use crate::state::LockState;

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
            ring_colors: linear(ms),
            ring_highlight: linear(ms),
            ring_appear: linear(ms),
            ring_wrong: linear(ms),
            ..Animations::default()
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn ring_cfg() -> RingIndicator {
        RingIndicator {
            style: RingStyle::Swayward,
            glow: 12.,
            ..RingIndicator::default()
        }
    }

    struct Rig {
        clock: Clock,
        anim: RingAnim,
        cfg: Animations,
        ring: RingIndicator,
        state: LockState,
    }

    impl Rig {
        fn new(cfg: Animations) -> Self {
            Self {
                clock: Clock::with_time(ms(1000)),
                anim: RingAnim::default(),
                cfg,
                ring: ring_cfg(),
                state: LockState::new(&General::default()),
            }
        }

        fn at(&mut self, t: u64) {
            self.clock.set_unadjusted(ms(t));
            let frame = RingFrame::new(&self.ring, &self.state);
            self.anim.update(&self.ring, &frame, &self.clock, &self.cfg);
        }

        fn key(&mut self, k: Keysym) {
            self.state.on_key(k, Some("a"), false, Instant::now());
        }

        fn look(&self) -> Look {
            self.anim.look(&self.ring).expect("visible").1
        }
    }

    fn close(a: Color, b: Color) -> bool {
        [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a]
            .iter()
            .all(|d| d.abs() < 0.01)
    }

    #[test]
    fn colours_crossfade_at_start_mid_and_end() {
        let mut r = Rig::new(anims(100));
        r.ring.idle_visible = true;
        r.at(1000);
        let input = r.ring.colors.inside.input;
        let cleared = r.ring.colors.inside.cleared;
        assert_eq!(r.look().inside, quantise(input));
        assert!(!r.anim.animating());
        r.key(Keysym::Escape);
        r.at(1000);
        assert_eq!(r.look().inside, quantise(input));
        assert!(r.anim.animating());
        r.at(1050);
        assert!(close(r.look().inside, lerp_color(input, cleared, 0.5)));
        r.at(1100);
        assert_eq!(r.look().inside, quantise(cleared));
        assert!(!r.anim.animating());
    }

    #[test]
    fn highlight_sweeps_the_shorter_way_across_zero() {
        let mut r = Rig::new(anims(100));
        r.ring.idle_visible = true;
        r.at(1000);
        r.key(Keysym::a);
        r.state.highlight_start = 2000;
        r.at(1000);
        // First highlight: drawn where it lands, no sweep.
        assert_eq!(r.look().highlight.unwrap().0, 2000.);
        assert!(r.look().trail.is_none());
        r.key(Keysym::a);
        r.state.highlight_start = 100;
        r.at(1000);
        r.at(1050);
        // 2000 -> 100 forwards is 148 units; halfway is 2074 (= 26 past a full turn).
        let (head, _) = r.look().highlight.unwrap();
        assert!((head - 2074.).abs() < 1., "{head}");
        let (from, to, alpha) = r.look().trail.unwrap();
        assert_eq!((from, to), (2000., head));
        assert!((alpha - 0.5).abs() < 0.01);
        r.at(1100);
        assert_eq!(r.look().highlight.unwrap().0.rem_euclid(TURN), 100.);
        assert!(r.look().trail.is_none(), "the trail reaches 0");
        // And backwards across 0: 100 -> 1900 is 248 units back.
        r.key(Keysym::a);
        r.state.highlight_start = 1900;
        r.at(1200);
        r.at(1250);
        let (head, _) = r.look().highlight.unwrap();
        assert!((head.rem_euclid(TURN) - 2024.).abs() < 1., "{head}");
    }

    #[test]
    fn shake_is_zero_at_both_ends_and_bounded() {
        let mut r = Rig::new(anims(450));
        r.ring.idle_visible = true;
        r.at(1000);
        assert_eq!(r.anim.offset(&r.ring), 0.);
        r.state.on_auth_failed(Instant::now());
        r.at(1000);
        assert_eq!(r.anim.offset(&r.ring), 0.);
        let bound = 0.08 * r.ring.radius;
        let mut peak: f64 = 0.;
        for t in (1000..=1450).step_by(5) {
            r.at(t);
            let o = r.anim.offset(&r.ring);
            assert!(o.abs() <= bound + 1e-9, "{o} at {t}");
            peak = peak.max(o.abs());
        }
        assert!(peak > bound / 2., "it does shake: {peak}");
        assert_eq!(r.anim.offset(&r.ring), 0.);
        assert!(!r.anim.animating());
        // The flash starts lighter than the wrong colour and ends at it.
        let wrong = r.ring.colors.ring.wrong;
        assert_eq!(r.look().ring, quantise(wrong));
    }

    #[test]
    fn flash_starts_towards_white() {
        let mut r = Rig::new(anims(400));
        r.ring.idle_visible = true;
        r.at(1000);
        r.state.on_auth_failed(Instant::now());
        r.at(1000);
        let wrong = r.ring.colors.ring.wrong;
        let start = lerp_color(wrong, Color::new_unpremul(1., 1., 1., 1.), 0.2);
        assert!(close(r.look().ring, start));
    }

    #[test]
    fn spinner_advances_only_while_validating() {
        let mut r = Rig::new(anims(100));
        r.ring.idle_visible = true;
        r.at(1000);
        assert_eq!(r.look().spinner, None);
        r.key(Keysym::a);
        r.key(Keysym::Return);
        r.at(1000);
        let a0 = r.look().spinner.unwrap();
        assert_eq!(a0, -PI / 2.);
        r.at(1300);
        let a1 = r.look().spinner.unwrap();
        assert!((a1 - a0 - PI / 2.).abs() < 0.01, "a quarter turn in 300 ms");
        assert!(r.anim.animating());
        r.state.on_auth_failed(Instant::now());
        r.at(1400);
        assert_eq!(r.look().spinner, None);
        r.at(2000);
        assert!(!r.anim.animating());
    }

    #[test]
    fn appears_with_fade_and_scale_and_fades_out_without_scale() {
        let mut r = Rig::new(anims(100));
        r.at(1000);
        assert_eq!(r.anim.alpha(), 0.);
        r.key(Keysym::a);
        r.at(1000);
        assert_eq!(r.anim.alpha(), 0.);
        assert_eq!(r.anim.look(&r.ring), None, "nothing to draw at alpha 0");
        r.at(1050);
        assert!((r.anim.alpha() - 0.5).abs() < 0.01);
        assert!((r.look().scale - 0.95).abs() < 0.002);
        r.at(1100);
        assert_eq!(r.anim.alpha(), 1.);
        assert_eq!(r.look().scale, 1.);
        // The input idle timer hides it: fades out with its last content, unscaled.
        r.state.tick(Instant::now() + Duration::from_secs(2));
        r.at(1200);
        r.at(1250);
        assert!((r.anim.alpha() - 0.5).abs() < 0.01);
        assert_eq!(r.look().scale, 1.);
        assert!(r.look().highlight.is_some(), "last content kept");
        r.at(1300);
        assert_eq!(r.anim.look(&r.ring), None);
        assert!(!r.anim.animating());
        // Back during a fade-out: fades in again from where it is, without the scale jump.
        r.key(Keysym::a);
        r.at(1400);
        r.at(1500);
        r.state.tick(Instant::now() + Duration::from_secs(4));
        r.at(1600);
        r.at(1650);
        r.key(Keysym::a);
        r.at(1650);
        assert!((r.anim.alpha() - 0.5).abs() < 0.01);
        assert_eq!(r.look().scale, 1.);
    }

    #[test]
    fn animations_off_gives_end_states_at_once() {
        let mut cfg = anims(1000);
        cfg.off = true;
        let mut r = Rig::new(cfg);
        r.at(1000);
        r.key(Keysym::a);
        r.state.highlight_start = 500;
        r.at(1000);
        assert_eq!(r.anim.alpha(), 1.);
        assert_eq!(r.look().scale, 1.);
        r.key(Keysym::a);
        r.state.highlight_start = 1500;
        r.at(1000);
        assert_eq!(r.look().highlight.unwrap().0.rem_euclid(TURN), 1500.);
        assert!(r.look().trail.is_none());
        r.key(Keysym::Escape);
        r.at(1000);
        assert_eq!(r.look().inside, quantise(r.ring.colors.inside.cleared));
        r.key(Keysym::a);
        r.key(Keysym::Return);
        r.at(1000);
        assert_eq!(r.look().spinner, None, "no spinner");
        assert_eq!(r.look().ring, quantise(r.ring.colors.ring.verifying));
        r.state.on_auth_failed(Instant::now());
        r.at(1000);
        assert_eq!(r.anim.offset(&r.ring), 0.);
        assert_eq!(
            r.look().ring,
            quantise(r.ring.colors.ring.wrong),
            "no flash"
        );
        assert_eq!(r.look().glow, 12., "the glow stays");
        assert!(!r.anim.animating());
    }

    #[test]
    fn settled_looks_compare_equal() {
        let mut r = Rig::new(anims(100));
        r.at(1000);
        r.key(Keysym::a);
        r.at(1000);
        r.key(Keysym::a);
        r.at(1010);
        r.at(1500);
        let a = r.anim.look(&r.ring);
        r.at(1516);
        let b = r.anim.look(&r.ring);
        assert!(a.is_some());
        assert_eq!(a, b);
        assert!(!r.anim.animating());
    }

    /// The swayward renderer draws what the look says: a glow outside the ring, and the
    /// resting centre offset accounts for it.
    #[test]
    fn renders_glow_outside_the_ring() {
        let mut r = Rig::new(anims(100));
        r.ring.idle_visible = true;
        r.at(1000);
        let (frame, look) = r.anim.look(&r.ring).unwrap();
        let (mut surface, (cx, cy)) =
            crate::render::ring::render_look(&r.ring, &frame, &look, 1.).unwrap();
        assert_eq!((cx, cy), (72., 72.));
        // 120 + 2 x 12 of glow, plus swaylock's one-step buffer padding at scale 1.
        assert_eq!((surface.width(), surface.height()), (145, 145));
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let alpha = |x: usize, y: usize| data[y * stride + x * 4 + 3];
        // Just outside the outer edge (radius 55): tinted; past the glow (radius 67): clear.
        assert!(alpha(72 + 57, 72) > 0);
        assert_eq!(alpha(72 + 68, 72), 0);
    }

    /// The ring's Vars are field_anim's: progress clamped to validate's bezier range, and a
    /// retarget mid-overshoot starting from a clamped value. Fast keys under an overshooting
    /// ring-highlight curve keep the head and the trail within two turns of the goal (about one
    /// at worst). With an unclamped Var, y 1000 put the head about 370 turns off its goal and
    /// y -1000 drew a trail as long, every frame. y beyond -2..3 is a validate error, but the
    /// runtime must hold too.
    #[test]
    fn overshooting_highlight_stays_bounded_with_fast_keys() {
        let bezier = |y: f64| config::Animation {
            off: false,
            kind: AnimationKind::Easing(EasingParams {
                duration_ms: 300,
                curve: AnimationCurve::CubicBezier(0., y, 1., y),
            }),
        };
        for y in [3., -2., 1000., -1000.] {
            for gap in (20..=200).step_by(20) {
                let cfg = Animations {
                    ring_highlight: bezier(y),
                    ring_wrong: bezier(y),
                    ring_appear: bezier(y),
                    ..anims(300)
                };
                let mut r = Rig::new(cfg);
                r.ring.idle_visible = true;
                let mut t = 1000;
                r.at(t);
                for k in 0..300_u32 {
                    r.key(Keysym::a);
                    r.state.highlight_start = (k * 701) % 2048;
                    let end = t + gap;
                    while t < end {
                        r.at(t);
                        let look = r.look();
                        let (head, _) = look.highlight.unwrap();
                        let goal = r.anim.highlight.goal;
                        assert!(
                            (head - goal).abs() <= 2. * TURN,
                            "y {y} gap {gap}: key {k}: head {head}, goal {goal}"
                        );
                        if let Some((from, to, _)) = look.trail {
                            assert!(
                                (to - from).abs() <= 2. * TURN,
                                "y {y} gap {gap}: key {k}: trail {from}..{to}"
                            );
                        }
                        assert!((0. ..=1.).contains(&look.scale), "scale {}", look.scale);
                        t += 16;
                    }
                }
            }
        }
    }
}
