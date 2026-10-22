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
    /// The latest auth failure message from any source, for `$FAIL`.
    pub last_failure: Option<String>,
    /// The latest PAM failure message, for `$PAMFAIL`.
    pub pam_failure: Option<String>,
    /// PAM's latest password prompt, for `$PAMPROMPT`.
    pub pam_prompt: String,
    /// PAM asked a further question in the same transaction (a one-time code after the
    /// password); the input field shows it instead of its placeholder.
    pub pam_follow_up: bool,
    /// fprintd's current prompt and last failure, for `$FPRINTPROMPT` / `$FPRINTFAIL`.
    pub fprint_prompt: String,
    pub fprint_fail: String,
    pub caps_lock: bool,
    pub num_lock: bool,
    /// Counts accepted key presses that changed the buffer, so widgets can react to "a key was
    /// typed" (hide-input recolours on every key) without seeing the password.
    pub keystrokes: u64,
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
            last_failure: None,
            pam_failure: None,
            pam_prompt: String::new(),
            pam_follow_up: false,
            fprint_prompt: String::new(),
            fprint_fail: String::new(),
            caps_lock: false,
            num_lock: false,
            keystrokes: 0,
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

    /// Number of characters typed, for dots. Never the content.
    pub fn password_len(&self) -> usize {
        self.password.len_chars()
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
        // Keys typed while a check runs are dropped, as in hyprlock (swaylock keeps them, so a
        // password typed ahead after a wrong one survives there). Deliberate: the field shows the
        // checking state, and keys landing in a buffer nobody sees are a surprise either way.
        if self.auth == AuthState::Validating {
            return KeyOutcome::None;
        }
        match keysym {
            Keysym::Return | Keysym::KP_Enter => return self.submit(),
            Keysym::j | Keysym::m | Keysym::d if ctrl => return self.submit(),
            Keysym::BackSpace | Keysym::Delete if ctrl => self.clear(),
            Keysym::BackSpace | Keysym::Delete => {
                self.password.pop_char();
                self.keystrokes += 1;
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
                    self.keystrokes += 1;
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

    /// A question from PAM. A follow-up (a one-time code after the password, in the same
    /// transaction) pauses the check until the answer is typed and submitted.
    pub fn on_auth_prompt(&mut self, text: String, follow_up: bool) {
        self.pam_prompt = text;
        self.pam_follow_up = follow_up;
        if follow_up && self.auth == AuthState::Validating {
            self.auth = AuthState::Idle;
        }
    }

    pub fn on_auth_failed(&mut self, now: Instant) {
        self.on_auth_failed_with(now, "Authentication failed");
    }

    pub fn on_auth_failed_with(&mut self, now: Instant, message: &str) {
        self.pam_follow_up = false;
        self.pam_failure = Some(message.to_owned());
        self.failed(now, message);
    }

    /// A fingerprint non-match is a failed attempt too (hyprlock's `enqueueFail`). It does not
    /// interrupt a password check that is running.
    pub fn on_fprint_failed(&mut self, now: Instant, message: &str) {
        self.fprint_fail = message.to_owned();
        if self.auth == AuthState::Validating {
            self.last_failure = Some(message.to_owned());
            self.failed_attempts = self.failed_attempts.saturating_add(1);
        } else {
            self.failed(now, message);
        }
    }

    fn failed(&mut self, now: Instant, message: &str) {
        self.last_failure = Some(message.to_owned());
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

    /// A fingerprint miss is a failed attempt, as in hyprlock: it counts, shows the fail state
    /// and sets `$FAIL`, but leaves `$PAMFAIL` alone.
    #[test]
    fn fingerprint_failure_counts_without_touching_pam_failure() {
        let mut s = state();
        let t0 = Instant::now();
        s.on_auth_failed_with(t0, "Wrong password");
        s.tick(t0 + Duration::from_secs(5));
        s.on_fprint_failed(t0 + Duration::from_secs(5), "No match");
        assert_eq!((s.auth, s.failed_attempts), (AuthState::Invalid, 2));
        assert_eq!(s.last_failure.as_deref(), Some("No match"));
        assert_eq!(s.pam_failure.as_deref(), Some("Wrong password"));
        assert_eq!(s.fprint_fail, "No match");
        assert!(s.tick(t0 + Duration::from_secs(10)));
        assert_eq!(s.auth, AuthState::Idle);
    }

    /// A fingerprint miss while a password check runs must not end that check.
    #[test]
    fn fingerprint_failure_during_a_password_check_keeps_validating() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "pw", now);
        s.on_key(Keysym::Return, None, false, now);
        s.on_fprint_failed(now, "No match");
        assert_eq!((s.auth, s.failed_attempts), (AuthState::Validating, 1));
        assert_eq!(s.next_deadline(), None);
    }

    /// A second PAM question (an OTP after the password) ends the checking state so the answer
    /// can be typed and submitted; it is not a failed attempt.
    #[test]
    fn a_follow_up_prompt_lets_the_answer_be_typed() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "pw", now);
        s.on_key(Keysym::Return, None, false, now);
        drop(s.take_password());
        s.on_auth_prompt("OTP: ".into(), true);
        assert_eq!((s.auth, s.failed_attempts), (AuthState::Idle, 0));
        assert!(s.pam_follow_up);
        assert_eq!(s.password_len(), 0);
        typed(&mut s, "123456", now);
        assert_eq!(
            s.on_key(Keysym::Return, None, false, now),
            KeyOutcome::Submit
        );
        assert_eq!(s.take_password().as_str(), "123456");
        s.on_auth_failed_with(now, "Authentication failure");
        assert!(!s.pam_follow_up);
        // A new transaction's first question is not a follow-up and does not end a check.
        typed(&mut s, "pw", now);
        s.on_key(Keysym::Return, None, false, now);
        s.on_auth_prompt("Password: ".into(), false);
        assert_eq!(s.auth, AuthState::Validating);
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
    fn escape_clears_the_password() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "abc", now);
        assert_eq!(
            s.on_key(Keysym::Escape, Some("\u{1b}"), false, now),
            KeyOutcome::Redraw
        );
        assert_eq!(s.input, InputState::Clear);
        assert_eq!(s.password_len(), 0);
    }

    /// Tab, Escape's text and other control characters never enter the password: a stray Tab
    /// would make the right password fail with nothing on screen to say why.
    #[test]
    fn control_characters_do_not_enter_the_password() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "ab", now);
        let keystrokes = s.keystrokes;
        for (sym, text) in [
            (Keysym::Tab, "\t"),
            (Keysym::ISO_Left_Tab, "\t"),
            (Keysym::a, "a\u{0}"),
            (Keysym::a, ""),
        ] {
            assert_eq!(s.on_key(sym, Some(text), false, now), KeyOutcome::None);
        }
        assert_eq!(s.on_key(Keysym::F1, None, false, now), KeyOutcome::None);
        assert_eq!((s.password_len(), s.keystrokes), (2, keystrokes));
        s.on_key(Keysym::Return, None, false, now);
        assert_eq!(s.take_password().as_str(), "ab");
    }

    /// Modifier keys show the neutral indicator (swaylock) and leave the password alone.
    #[test]
    fn modifier_keys_are_neutral() {
        for sym in [
            Keysym::Caps_Lock,
            Keysym::Shift_L,
            Keysym::Shift_R,
            Keysym::Control_L,
            Keysym::Control_R,
            Keysym::Meta_L,
            Keysym::Meta_R,
            Keysym::Alt_L,
            Keysym::Alt_R,
            Keysym::Super_L,
            Keysym::Super_R,
        ] {
            let mut s = state();
            let now = Instant::now();
            typed(&mut s, "ab", now);
            let keystrokes = s.keystrokes;
            assert_eq!(s.on_key(sym, None, false, now), KeyOutcome::Redraw);
            assert_eq!(s.input, InputState::Neutral, "{sym:?}");
            assert_eq!((s.password_len(), s.keystrokes), (2, keystrokes), "{sym:?}");
            // The stale-password timer restarts, as for any key.
            assert_eq!(s.clear_at, Some(now + s.clear_after));
        }
    }

    /// Ctrl+BackSpace (and Ctrl+Delete) clear; without Ctrl they delete one character.
    #[test]
    fn backspace_deletes_one_character_and_ctrl_backspace_clears() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "abc", now);
        let keystrokes = s.keystrokes;
        s.on_key(Keysym::BackSpace, None, false, now);
        assert_eq!(s.password_len(), 2);
        assert_eq!(s.keystrokes, keystrokes + 1);
        s.on_key(Keysym::Delete, None, false, now);
        assert_eq!(s.password_len(), 1);
        assert_eq!(s.keystrokes, keystrokes + 2);
        typed(&mut s, "xy", now);
        s.on_key(Keysym::BackSpace, None, true, now);
        assert_eq!((s.password_len(), s.input), (0, InputState::Clear));
        typed(&mut s, "xy", now);
        s.on_key(Keysym::Delete, None, true, now);
        assert_eq!((s.password_len(), s.input), (0, InputState::Clear));
    }

    /// A typo fixed with one BackSpace keeps the rest of the password: the clear timer restarts
    /// from the BackSpace, it does not fire on the next tick.
    #[test]
    fn backspace_restarts_the_clear_timer() {
        let mut s = state();
        let t0 = Instant::now();
        typed(&mut s, "abcx", t0);
        let t1 = t0 + Duration::from_secs(5);
        s.on_key(Keysym::BackSpace, None, false, t1);
        assert_eq!(s.next_deadline(), Some(t1 + INPUT_IDLE));
        s.tick(t1 + INPUT_IDLE);
        assert_eq!(s.password_len(), 3, "the next tick wiped the password");
        // The 10 s clear timer counts from the BackSpace, not from the first key.
        s.tick(t0 + Duration::from_secs(12));
        assert_eq!(s.password_len(), 3);
        s.tick(t1 + Duration::from_secs(10));
        assert_eq!(s.password_len(), 0);
    }

    /// `ignore-empty-input` drops only an empty submit.
    #[test]
    fn ignore_empty_input_still_submits_a_password() {
        let mut s = LockState::new(&General {
            ignore_empty_input: true,
            ..General::default()
        });
        let now = Instant::now();
        typed(&mut s, "pw", now);
        assert_eq!(
            s.on_key(Keysym::Return, None, false, now),
            KeyOutcome::Submit
        );
        assert_eq!(s.take_password().as_str(), "pw");
        // Without the option an empty Enter is an attempt, as in swaylock.
        let mut s = state();
        assert_eq!(
            s.on_key(Keysym::Return, None, false, now),
            KeyOutcome::Submit
        );
    }

    /// Every accepted key that changes the buffer counts, so hide-input recolours on each.
    #[test]
    fn keystrokes_count_letters_and_deletions() {
        let mut s = state();
        let now = Instant::now();
        typed(&mut s, "abc", now);
        assert_eq!(s.keystrokes, 3);
        s.on_key(Keysym::BackSpace, None, false, now);
        s.on_key(Keysym::BackSpace, None, false, now);
        assert_eq!(s.keystrokes, 5);
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
