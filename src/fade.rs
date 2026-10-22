//! Whole-screen fade in after locking and fade out before unlocking, driven by swayward's
//! animation runtime. Also owns the redraw ticker that runs while any animation is in progress.

use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::LoopHandle;

use crate::animation::{Animation, Clock};
use crate::config::Animations;
use crate::App;

/// How often to redraw while animating. Lock screens do not need frame-callback pacing; 60 Hz is
/// smooth and cheap at these sizes.
const FRAME: Duration = Duration::from_millis(16);

pub struct Fades {
    clock: Clock,
    opacity: Option<Animation>,
    config: Animations,
    ticking: bool,
    /// Run when the fade out finishes: unlock.
    pub unlock_when_done: bool,
}

impl Fades {
    pub fn new(config: Animations) -> Self {
        Self {
            clock: Clock::default(),
            opacity: None,
            config,
            ticking: false,
            unlock_when_done: false,
        }
    }

    /// Current whole-screen opacity, 0..1.
    pub fn opacity(&self) -> f32 {
        self.opacity
            .as_ref()
            .map_or(1., |a| a.clamped_value().clamp(0., 1.) as f32)
    }

    pub fn is_animating(&self) -> bool {
        self.opacity.as_ref().is_some_and(|a| !a.is_done())
    }

    pub fn fade_in(&mut self) {
        self.clock.clear();
        let cfg = self.config.get(self.config.fade_in);
        self.opacity = Some(Animation::new(self.clock.clone(), 0., 1., 0., cfg));
    }

    /// Start fading out from wherever the opacity is now. Returns false if there is nothing to
    /// animate (fade-out off or zero length), in which case the caller must unlock
    /// immediately: only a running animation arms the ticker that performs the unlock.
    pub fn fade_out(&mut self) -> bool {
        self.clock.clear();
        let cfg = self.config.get(self.config.fade_out);
        if cfg.off {
            return false;
        }
        let from = f64::from(self.opacity());
        let anim = Animation::new(self.clock.clone(), from, 0., 0., cfg);
        if anim.is_done() {
            return false;
        }
        self.opacity = Some(anim);
        true
    }

    /// Advance the clock to now; call before reading values for a frame.
    pub fn tick_clock(&mut self) {
        self.clock.clear();
    }

    /// The shared animation clock; the input fields' animations run on it too.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }
}

/// Make sure a redraw timer runs while an animation is in progress: a whole-screen fade or, until
/// an unlock is pending, an input-field animation. It stops itself when they finish, and performs
/// the pending unlock as soon as the fade out ends, whatever the fields are doing.
pub fn ensure_ticking(app: &mut App) {
    if app.fades.ticking || !app.needs_frames() {
        return;
    }
    app.fades.ticking = true;
    let handle: LoopHandle<'static, App> = app.handle.clone();
    let _ = handle.insert_source(Timer::from_duration(FRAME), |_, _, app| tick(app));
}

/// The registered redraw callback, also exercised on a retained, actually unlocked test App.
fn tick(app: &mut App) -> TimeoutAction {
    if app.exit.is_some() {
        // Unlocked (or finished) while animating: the surfaces are gone.
        app.fades.ticking = false;
        return TimeoutAction::Drop;
    }
    app.fades.tick_clock();
    app.draw_all();
    if app.fades.is_animating() {
        return TimeoutAction::ToDuration(FRAME);
    }
    if app.fades.unlock_when_done {
        app.fades.ticking = false;
        app.fades.unlock_when_done = false;
        app.finish_unlock();
        return TimeoutAction::Drop;
    }
    if app.needs_frames() {
        return TimeoutAction::ToDuration(FRAME);
    }
    app.fades.ticking = false;
    TimeoutAction::Drop
}

#[cfg(feature = "test-auth")]
pub(crate) fn check_exited_ticker(app: &mut App) {
    let mut config = Animations::default();
    config.fade_in.kind = crate::config::AnimationKind::Easing(crate::config::EasingParams {
        duration_ms: 60_000,
        curve: crate::config::AnimationCurve::Linear,
    });
    app.fades = Fades::new(config);
    app.fades.fade_in();
    app.fades.ticking = true;
    assert!(
        app.fades.is_animating(),
        "unfinished fade required for ticker probe"
    );
    assert!(
        matches!(tick(app), TimeoutAction::Drop),
        "exited ticker must return Drop"
    );
    assert!(!app.fades.ticking, "exited ticker must stop ticking");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Animations;

    #[test]
    fn fade_in_starts_dark_and_off_is_instant() {
        let mut f = Fades::new(Animations::default());
        assert_eq!(f.opacity(), 1.);
        f.fade_in();
        assert_eq!(f.opacity(), 0.);
        assert!(f.is_animating());

        let mut off = Fades::new(Animations {
            off: true,
            ..Animations::default()
        });
        off.fade_in();
        std::thread::sleep(Duration::from_millis(2));
        off.tick_clock();
        assert_eq!(off.opacity(), 1.);
        assert!(!off.fade_out(), "fade-out off asks for an immediate unlock");
    }

    fn fade_out_ms(ms: u32) -> Animations {
        Animations {
            fade_out: crate::config::Animation {
                off: false,
                kind: crate::config::AnimationKind::Easing(crate::config::EasingParams {
                    duration_ms: ms,
                    curve: crate::config::AnimationCurve::Linear,
                }),
            },
            ..Animations::default()
        }
    }

    /// A fade out that is finished as soon as it starts would never arm the ticker that performs
    /// the unlock, wedging the session locked; it must ask for an immediate unlock instead.
    #[test]
    fn instant_fade_out_unlocks_immediately() {
        let mut zero = Fades::new(fade_out_ms(0));
        assert!(!zero.fade_out(), "duration-ms 0");
        assert!(Fades::new(fade_out_ms(200)).fade_out());
    }

    #[test]
    fn fade_out_reaches_zero() {
        let cfg = Animations {
            fade_out: crate::config::Animation {
                off: false,
                kind: crate::config::AnimationKind::Easing(crate::config::EasingParams {
                    duration_ms: 20,
                    curve: crate::config::AnimationCurve::Linear,
                }),
            },
            ..Animations::default()
        };
        let mut f = Fades::new(cfg);
        assert!(f.fade_out());
        assert!(f.is_animating());
        std::thread::sleep(Duration::from_millis(30));
        f.tick_clock();
        assert!(!f.is_animating());
        assert_eq!(f.opacity(), 0.);
    }
}
