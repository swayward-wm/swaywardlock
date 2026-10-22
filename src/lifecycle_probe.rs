//! Test-auth-only observations while the real, unlocked App is still alive.

use crate::{App, AuthEvent};

std::thread_local! {
    static EVENTS: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
}

pub(super) fn state_changed() {
    EVENTS.set(EVENTS.get().map(|(state, draw)| (state + 1, draw)));
}

pub(super) fn draw_entry() {
    EVENTS.set(EVENTS.get().map(|(state, draw)| (state, draw + 1)));
}

pub(super) fn check(app: &mut App) {
    assert_eq!(app.exit, Some(0));
    assert!(app.session_lock.is_none(), "probe requires actual unlock");
    assert!(
        app.surfaces.is_empty(),
        "unlock must clear surfaces before App destruction"
    );
    let snapshot = |app: &App| {
        (
            app.state.auth,
            app.state.input,
            app.state.failed_attempts,
            app.state.last_failure.clone(),
            app.state.pam_failure.clone(),
            app.state.pam_prompt.clone(),
            app.state.pam_follow_up,
            app.state.password_len(),
            app.state.next_deadline(),
        )
    };
    let before = snapshot(app);
    EVENTS.set(Some((0, 0)));
    app.on_auth(AuthEvent::Prompt {
        text: "late test prompt".into(),
        follow_up: true,
    });
    assert_eq!(
        snapshot(app),
        before,
        "late Prompt must not change authentication state"
    );
    app.on_auth(AuthEvent::Failure("late test failure".into()));
    assert_eq!(
        snapshot(app),
        before,
        "late Failure must not change authentication state"
    );
    assert_eq!(
        EVENTS.get(),
        Some((0, 0)),
        "late auth must not enter state_changed or draw_all"
    );
    crate::fade::check_exited_ticker(app);
    assert_eq!(
        EVENTS.take(),
        Some((0, 0)),
        "exited ticker must not enter draw_all"
    );
    tracing::info!("test exit probe: late auth, surfaces and ticker checked");
}
