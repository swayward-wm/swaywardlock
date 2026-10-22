"""Typing a password: a wrong one keeps the lock, the right one unlocks the compositor."""

import time

from lib import IDLE_RING, PASSWORD, RING, WRONG_RING


def test_password(desktop):
    env = desktop
    locker = env.locker(
        'background { color "#204060"; }\nring-indicator { idle-visible; }\n',
        env={"RUST_LOG": "swaywardlock=debug"},
    )
    locker.wait_locked("password")
    env.expect_colour("password: background colour", (32, 64, 96), 5, 5)
    # swaylock's default input ring colour, 337d00.
    env.expect_colour("password: idle ring", IDLE_RING, *RING)

    locker.type("wrong")
    # The check runs off the main thread; give it time to come back before asking.
    assert locker.wait_log("authentication failed"), "password: the wrong password was never checked"
    time.sleep(0.2)
    assert locker.alive(), "password: wrong password keeps the lock"
    # swaylock's wrong ring colour, 7d3300.
    env.expect_colour("password: wrong ring", WRONG_RING, *RING)

    locker.type(PASSWORD)
    assert locker.wait_exit() == 0, "password: exit code after unlock"
    env.expect_unlocked("password")
