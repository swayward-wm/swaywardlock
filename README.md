# swaywardlock

A screen locker for the swayward-wm family, written in Rust on Smithay.

It keeps swaylock's process contract (`-f`, `--ready-fd`, SIGUSR1), so
swayidle and scripts keep working, and aims for hyprlock's feature set:
blurred screenshots, labels, an input field, gradients, animations and
fingerprint unlock. It is configured in KDL with the same colour, gradient,
blur, shadow and animation syntax as swayward. It runs on any compositor
with `ext-session-lock-v1`.

**Status: early.** swaylock parity and most of hyprlock's features are implemented and tested
headless; the spec lists what is left for Phase 3. Not yet used daily on real hardware.

`examples/hyprlock-example.kdl` is hyprlock's sample config in swaywardlock KDL. Design:
[`docs/specs/2026-10-19-swaywardlock.md`](docs/specs/2026-10-19-swaywardlock.md).

## Working on a layout

```
swaywardlock --preview -c ~/.config/swaywardlock/config.kdl
```

draws the lock screen in an ordinary window instead of locking, and redraws whenever the config
(or anything it includes) changes. A config that fails to parse prints the error and keeps the
last good frame. Typing drives the indicator; Enter shows the checking and wrong-password states
but never checks a password. `--preview-output DP-1` makes the window match `output="DP-1"`
widgets. Close the window to quit.

`swaywardlock validate` checks a config without drawing anything.

License: GPL-3.0-or-later.
