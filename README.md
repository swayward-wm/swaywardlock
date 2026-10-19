# swaywardlock

A screen locker for the swayward-wm family, written in Rust on Smithay.

It keeps swaylock's process contract (`-f`, `--ready-fd`, SIGUSR1), so
swayidle and scripts keep working, and aims for hyprlock's feature set:
blurred screenshots, labels, an input field, gradients, animations and
fingerprint unlock. It is configured in KDL with the same colour, gradient,
blur, shadow and animation syntax as swayward. It runs on any compositor
with `ext-session-lock-v1`.

**Status: design.** No code yet. See
[`docs/specs/2026-10-19-swaywardlock.md`](docs/specs/2026-10-19-swaywardlock.md).

License: GPL-3.0-or-later.
