//! Input and auth state, with swaylock's timers. Pure logic: no Wayland, no rendering.
//!
//! The state model follows swaylock (`swaylock.h`, `password.c`): an auth state (idle,
//! validating, invalid) and an input state (idle, clear, letter, backspace, neutral) that decays
//! to idle 1.5 s after the last key.

use std::time::{Duration, Instant};

use smithay_client_toolkit::seat::keyboard::Keysym;

use crate::secret::SecretBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthState {
    Idle,
    Validating,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputState {
    Idle,
    Clear,
    Letter,
    Backspace,
    Neutral,
}

/// swaylock's 1.5 s input-idle timer.
const INPUT_IDLE: Duration = Duration::from_millis(1500);

/// What the caller must do after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Nothing changed visibly.
    None,
    /// Redraw.
    Redraw,
    /// Submit this secret to the authenticator, then redraw.
    Submit,
}

pub struct LockState {
    pub auth: AuthState,
    pub input: InputState,
    /// Position of the typing highlight; 2048 = one full turn (swaylock's `highlight_start`).
    pub highlight_start: u32,
    pub failed_attempts: u32,
    pub caps_lock: bool,
    /// Active xkb layout index and the keymap's layout names.
    pub layout: u32,
    pub layout_names: Vec<String>,
    password: SecretBuf,
    ignore_empty: bool,
    fail_timeout: Duration,
    clear_after: Duration,
    input_idle_at: Option<Instant>,
    auth_idle_at: Option<Instant>,
    clear_at: Option<Instant>,
    rng: fastrand::Rng,
}

impl LockState {
    pub fn new(config: &crate::config::General) -> Self {
        Self {
            auth: AuthState::Idle,
            input: InputState::Idle,
            highlight_start: 0,
            failed_attempts: 0,
            caps_lock: false,
            layout: 0,
            layout_names: Vec::new(),
            password: SecretBuf::new(),
            ignore_empty: config.ignore_empty_input,
            fail_timeout: Duration::from_millis(config.fail_timeout_ms.into()),
            clear_after: Duration::from_millis(config.clear_input_after_ms.into()),
            input_idle_at: None,
            auth_idle_at: None,
            clear_at: None,
            rng: fastrand::Rng::new(),
        }
    }

    pub fn layout_name(&self) -> Option<&str> {
        self.layout_names
            .get(self.layout as usize)
            .map(String::as_str)
    }

    /// Take the password for submission. Only valid right after [`KeyOutcome::Submit`].
    pub fn take_password(&mut self) -> SecretBuf {
        self.password.take()
    }

    pub fn on_key(
        &mut self,
        keysym: Keysym,
        utf8: Option<&str>,
        ctrl: bool,
        now: Instant,
    ) -> KeyOutcome {
        // swaylock ignores input while a check runs.
        if self.auth == AuthState::Validating {
            return KeyOutcome::None;
        }
        match keysym {
            Keysym::Return | Keysym::KP_Enter => return self.submit(),
            Keysym::j | Keysym::m | Keysym::d if ctrl => return self.submit(),
            Keysym::BackSpace | Keysym::Delete if ctrl => self.clear(),
            Keysym::BackSpace | Keysym::Delete => {
                self.password.pop_char();
                if self.password.is_empty() {
                    self.clear();
                } else {
                    self.input = InputState::Backspace;
                    self.clear_at = Some(now + self.clear_after);
                    self.advance_highlight();
                }
            }
            Keysym::Escape => self.clear(),
            // swaylock clears on Ctrl+C/U; hyprlock adds Ctrl+A.
            Keysym::c | Keysym::u | Keysym::a if ctrl => self.clear(),
            Keysym::Caps_Lock
            | Keysym::Shift_L
            | Keysym::Shift_R
            | Keysym::Control_L
            | Keysym::Control_R
            | Keysym::Meta_L
            | Keysym::Meta_R
            | Keysym::Alt_L
            | Keysym::Alt_R
            | Keysym::Super_L
            | Keysym::Super_R => {
                self.input = InputState::Neutral;
                self.clear_at = Some(now + self.clear_after);
            }
            _ => {
                let Some(text) = utf8.filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
                else {
                    return KeyOutcome::None;
                };
                if self.password.push_str(text) {
                    self.input = InputState::Letter;
                    self.clear_at = Some(now + self.clear_after);
                    self.advance_highlight();
                }
            }
        }
        self.input_idle_at = Some(now + INPUT_IDLE);
        KeyOutcome::Redraw
    }

    fn submit(&mut self) -> KeyOutcome {
        if self.ignore_empty && self.password.is_empty() {
            return KeyOutcome::None;
        }
        self.auth = AuthState::Validating;
        self.input = InputState::Idle;
        self.input_idle_at = None;
        self.clear_at = None;
        // A fail timer from the previous attempt must not end this check early (swaylock has
        // this bug: submit_password leaves auth_idle_timer running).
        self.auth_idle_at = None;
        KeyOutcome::Submit
    }

    fn clear(&mut self) {
        self.password.clear();
        self.input = InputState::Clear;
        self.clear_at = None;
    }

    fn advance_highlight(&mut self) {
        // swaylock: advance a random amount between 1/4 and 3/4 of a turn.
        self.highlight_start = (self.highlight_start + self.rng.u32(0..1024) + 512) % 2048;
    }

    pub fn on_auth_failed(&mut self, now: Instant) {
        self.auth = AuthState::Invalid;
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        self.auth_idle_at = Some(now + self.fail_timeout);
    }

    /// Apply expired timers. Returns true if anything changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.clear_at.is_some_and(|t| now >= t) {
            self.clear_at = None;
            self.clear();
            self.input_idle_at = Some(now + INPUT_IDLE);
            changed = true;
        }
        if self.input_idle_at.is_some_and(|t| now >= t) {
            self.input_idle_at = None;
            self.input = InputState::Idle;
            changed = true;
        }
        if self.auth_idle_at.is_some_and(|t| now >= t) {
            self.auth_idle_at = None;
            self.auth = AuthState::Idle;
            changed = true;
        }
        changed
    }

    /// The earliest pending timer, for scheduling a wake-up.
    pub fn next_deadline(&self) -> Option<Instant> {
        [self.clear_at, self.input_idle_at, self.auth_idle_at]
            .into_iter()
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::General;

    fn state() -> LockState {
        LockState::new(&General::default())
    }

    fn typed(s: &mut LockState, text: &str, now: Instant) {
        for c in text.chars() {
            let utf8 = c.to_string();
            s.on_key(Keysym::from_char(c), Some(&utf8), false, now);
        }
    }

    #[test]
    fn typing_then_enter_submits_the_password() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "pw", now);
        assert_eq!(s.input, InputState::Letter);
        assert_eq!(
            s.on_key(Keysym::Return, None, false, now),
            KeyOutcome::Submit
        );
        assert_eq!(s.auth, AuthState::Validating);
        assert_eq!(s.take_password().as_str(), "pw");
        // Keys are ignored while validating.
        assert_eq!(s.on_key(Keysym::a, Some("a"), false, now), KeyOutcome::None);
    }

    #[test]
    fn ctrl_shortcuts_follow_swaylock() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "abc", now);
        s.on_key(Keysym::u, Some("u"), true, now);
        assert_eq!(s.input, InputState::Clear);
        typed(&mut s, "x", now);
        assert_eq!(
            s.on_key(Keysym::j, Some("j"), true, now),
            KeyOutcome::Submit
        );
        assert_eq!(s.take_password().as_str(), "x");
    }

    #[test]
    fn backspace_to_empty_reads_as_clear() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "ab", now);
        s.on_key(Keysym::BackSpace, None, false, now);
        assert_eq!(s.input, InputState::Backspace);
        s.on_key(Keysym::BackSpace, None, false, now);
        assert_eq!(s.input, InputState::Clear);
    }

    #[test]
    fn ignore_empty_input() {
        let mut s = LockState::new(&General {
            ignore_empty_input: true,
            ..General::default()
        });
        assert_eq!(
            s.on_key(Keysym::Return, None, false, Instant::now()),
            KeyOutcome::None
        );
        assert_eq!(s.auth, AuthState::Idle);
    }

    #[test]
    fn timers_decay_states() {
        let mut s = state();
        let t0 = Instant::now();
        typed(&mut s, "a", t0);
        assert_eq!(s.next_deadline(), Some(t0 + INPUT_IDLE));
        assert!(s.tick(t0 + INPUT_IDLE));
        assert_eq!(s.input, InputState::Idle);

        // The stale password is cleared after 10 s.
        assert!(s.tick(t0 + Duration::from_secs(10)));
        assert_eq!(s.input, InputState::Clear);
        assert!(s.take_password().is_empty());

        s.on_auth_failed(t0);
        assert_eq!((s.auth, s.failed_attempts), (AuthState::Invalid, 1));
        assert!(s.tick(t0 + Duration::from_secs(3)));
        assert_eq!(s.auth, AuthState::Idle);
    }

    #[test]
    fn a_pending_fail_timer_does_not_end_a_new_check() {
        let mut s = state();
        let t0 = Instant::now();
        s.on_auth_failed(t0);
        typed(&mut s, "pw", t0 + Duration::from_secs(1));
        assert_eq!(
            s.on_key(Keysym::Return, None, false, t0 + Duration::from_secs(1)),
            KeyOutcome::Submit
        );
        // The old 3 s fail timer must not flip a running check back to idle.
        s.tick(t0 + Duration::from_secs(4));
        assert_eq!(s.auth, AuthState::Validating);
        assert_eq!(s.next_deadline(), None);
    }

    #[test]
    fn ctrl_a_clears_like_hyprlock() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "abc", now);
        s.on_key(Keysym::a, Some("a"), true, now);
        assert_eq!(s.input, InputState::Clear);
        assert!(s.take_password().is_empty());
    }

    #[test]
    fn highlight_moves_between_a_quarter_and_three_quarters_turn() {
        let mut s = state();
        let now = Instant::now();
        for _ in 0..100 {
            let before = s.highlight_start;
            typed(&mut s, "x", now);
            let step = (s.highlight_start + 2048 - before) % 2048;
            assert!((512..1536).contains(&step), "step {step}");
        }
    }
}
