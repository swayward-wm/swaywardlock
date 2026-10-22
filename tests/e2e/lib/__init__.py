"""Helpers for the end-to-end tests: a headless sway per test, the locker, the screen."""

# The password the `test-auth` build accepts.
PASSWORD = "swaywardlock-test"

# A point on a default ring-indicator (radius 50, thickness 10) centred on the 320x240 output.
RING = (210, 120)

# swaylock's default ring colours, as the e2e checks see them.
IDLE_RING = (51, 125, 0)  # 337d00
WRONG_RING = (125, 51, 0)  # 7d3300

# The desktop start_desktop draws behind the lock, and what sway shows on a dead lock.
DESKTOP = (0, 255, 0)
