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
Typing follows hyprlock where the two lockers differ: only Backspace and Delete repeat, and
keys typed while a password is being checked are dropped. swaylock's control keys are kept:
Ctrl+J, Ctrl+M and Ctrl+D submit, Ctrl+C and Ctrl+U clear (as do Escape, Ctrl+A and
Ctrl+Backspace). The man page's KEYS section and the spec's Input section have the details.

To try it without locking your own session, `contrib/try-lock` starts a nested sway in a window
and locks only that (password `swaywardlock-test`, or `--real-pam`; `--preview` for layout work;
`-c FILE` for another config; `-v` for the full log). It needs `sway`, `cargo` and a Wayland
session to open the window in. It runs inside a distrobox named `swayward-dev` when you have one,
and otherwise right where you start it, so any machine with sway and cargo works
(`TRY_LOCK_NATIVE=1` forces that). `--no-build` reuses an earlier build (`target/try-lock`, or
`target/debug` built with `--features test-auth`). Closing the window, Ctrl+C or SIGTERM ends it
and cleans up.

`examples/hyprlock-example.kdl` is hyprlock's sample config in swaywardlock KDL;
`examples/swayward-ring.kdl` shows the animated ring (`ring-indicator { style "swayward"; }`).
`contrib/swaylock-to-kdl` and `contrib/hyprlock-to-kdl` convert a swaylock or hyprlock config to
KDL, warning about every key they cannot carry over (installed as
`swaywardlock-swaylock-to-kdl` and `swaywardlock-hyprlock-to-kdl`). Design and config reference:
[`docs/specs/2026-10-19-swaywardlock.md`](docs/specs/2026-10-19-swaywardlock.md); the man page
(`man swaywardlock`) lists the config nodes too.

## Quickstart

Without a config file swaywardlock uses a built-in swaylock-style look, so
`swaywardlock` works as it is. To make it yours, start from an example or a converter. The
config goes in `~/.config/swaywardlock/config.kdl` (or `/etc/swaywardlock/config.kdl`, or
anywhere with `-c FILE` or `$SWAYWARDLOCK_CONFIG`):

```
mkdir -p ~/.config/swaywardlock
# from an example (installed under /usr/share/doc/swaywardlock/examples):
cp examples/hyprlock-example.kdl ~/.config/swaywardlock/config.kdl
# or from a hyprlock or swaylock config (warnings and the command line go to stderr):
contrib/hyprlock-to-kdl ~/.config/hypr/hyprlock.conf > ~/.config/swaywardlock/config.kdl
contrib/swaylock-to-kdl ~/.config/swaylock/config > ~/.config/swaywardlock/config.kdl

swaywardlock validate                # parse it without drawing anything
swaywardlock --preview               # see it in a window; edit the file and it redraws
swaywardlock -f                      # lock; -f returns once the session is locked
```

Use it from swayidle like swaylock (`-f` makes the lock the thing swayidle waits for):

```
swayidle -w timeout 300 'swaywardlock -f' before-sleep 'swaywardlock -f'
```

or bind a key, e.g. in sway: `bindsym $mod+l exec swaywardlock -f`.

A config is KDL, where a block written on one line needs a `;` (or a newline) before its
closing `}`, as in `auth { pam { off; }; }`.

## Install

Packages (both ship the PAM file, the man page, shell completions, the config converters, the
examples and the design document):

- Arch: `contrib/PKGBUILD` (`cd contrib && makepkg -si`). There is no release tag yet, so it
  builds the checkout it is in (not a downloaded tarball); needs `base-devel`, `cargo` and
  `scdoc`. Run it in a git checkout, not from a copied PKGBUILD.
- Fedora: `swaywardlock.spec.rpkg` (`rpkg local`, then install the rpm from the printed path).
  Needs `rpkg`, `rpm-build`, `cargo-rpm-macros` >= 26 and a git checkout; it packs the committed
  state of the tree.

They install `/etc/pam.d/swaywardlock` as a config file (it includes `login`, so the packages depend
on the owner of `/etc/pam.d/login`: `util-linux` on Fedora and Arch), `swaywardlock(1)` (`man swaywardlock`,
built from `docs/swaywardlock.1.scd` with scdoc) and
the converters on `PATH`; the examples are in `/usr/share/doc/swaywardlock/examples` and the design
document in `/usr/share/doc/swaywardlock/specs`. `swaywardlock completions bash|zsh|fish|elvish|powershell`
prints a completion script.

From source, swaywardlock authenticates through PAM with the service `swaywardlock`, so install
the shipped PAM file yourself (as root):

```
install -Dm644 pam/swaywardlock /etc/pam.d/swaywardlock
```

Without it, PAM would fall back to `/etc/pam.d/other`, which refuses every password; swaywardlock
checks that the file is there and readable (as libpam looks for it: the service name
lowercased) and exits with an error before locking. The file is swaylock's and
hyprlock's: `auth include login`, so `/etc/pam.d/login` (util-linux) must exist too. `auth { pam service="login"; }` uses another service instead,
and `auth { pam { off; }; }` disables PAM (only SIGUSR1 or a fingerprint can unlock then).

Like swaylock, swaywardlock starts PAM when you press Enter, so a stack with `pam_fprintd` first
(Fedora's authselect `with-fingerprint`) reads the finger after Enter; `auth { fingerprint { }; }`
unlocks on a touch at any time. Each question PAM asks after the first (a one-time code) is
shown and answered with the next Enter. If your stack has two modules that both ask for the
password, `auth { pam { reuse-password; }; }` answers the second from the first.

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
