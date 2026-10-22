"""No config file: swaylock's look."""

import signal

from lib import RING

GREY = (163, 163, 163)  # swaylock's a3a3a3


def test_defaults(lock_env):
    locker = lock_env.locker()
    locker.wait_locked("defaults")
    lock_env.expect_colour("defaults: swaylock grey", GREY)
    # The ring is hidden while idle.
    lock_env.expect_colour("defaults: ring hidden when idle", GREY, *RING)
    locker.signal(signal.SIGUSR1)
    assert locker.wait_exit() == 0, "defaults: SIGUSR1 unlocks, exit code"
