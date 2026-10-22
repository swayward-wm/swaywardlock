# Using swaywardlock

[Back to the README](../README.md).

Source-build and development commands below run from the checkout root; source paths in prose
are relative to this document. Packages install this guide at
`/usr/share/doc/swaywardlock/docs/USING.md`; the binary tarball has `docs/USING.md` beside its README.

## Trying it safely

Typing follows hyprlock where the two lockers differ: only Backspace and Delete repeat, and
keys typed while a password is being checked are dropped. swaylock's control keys are kept:
Ctrl+J, Ctrl+M and Ctrl+D submit, Ctrl+C and Ctrl+U clear (as do Escape, Ctrl+A and
Ctrl+Backspace). The man page's KEYS section and the spec's Input section have the details.

To try it without locking your own session, `../contrib/try-lock` starts a nested sway in a window
and locks only that (password `swaywardlock-test`, or `--real-pam`; `--preview` for layout work;
`-c FILE` for another config; `-v` for the full log). It needs `sway`, `cargo` and a Wayland
session to open the window in. It runs inside a distrobox named `swayward-dev` when you have one,
and otherwise right where you start it, so any machine with sway and cargo works
(`TRY_LOCK_NATIVE=1` forces that). `--no-build` reuses an earlier build (`target/try-lock`, or
`target/debug` built with `--features test-auth`). Closing the window, Ctrl+C or SIGTERM ends it
and cleans up.

`../examples/hyprlock-example.kdl` is hyprlock's sample config in swaywardlock KDL;
`../examples/swayward-ring.kdl` shows the animated ring (`ring-indicator { style "swayward"; }`).
`../contrib/swaylock-to-kdl` and `../contrib/hyprlock-to-kdl` convert a swaylock or hyprlock config to
KDL, warning about every key they cannot carry over (installed as
`swaywardlock-swaylock-to-kdl` and `swaywardlock-hyprlock-to-kdl`). Design and config reference:
[`specs/2026-10-19-swaywardlock.md`](specs/2026-10-19-swaywardlock.md); the man page
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

Prebuilt x86_64 packages are on the [release page](https://github.com/swayward-wm/swaywardlock/releases):

| File | For |
|---|---|
| `swaywardlock_VERSION_amd64.deb` | Ubuntu 24.04 |
| `swaywardlock-VERSION-fedora43-x86_64.rpm` | Fedora 43 |
| `swaywardlock-VERSION-fedora44-x86_64.rpm` | Fedora 44 |
| `swaywardlock-VERSION-arch-x86_64.pkg.tar.zst` | Arch |
| `swaywardlock-VERSION-ubuntu24.04-x86_64.tar.gz` | prebuilt binaries, with a `.sha256` |
| `swaywardlock-VERSION-vendored-dependencies.tar.xz` | the Rust dependencies, for building offline |

The release workflow installs and executes the Ubuntu package on a fresh Ubuntu 24.04 GitHub
runner and the Fedora and Arch packages in clean containers before it drafts the release:
`swaywardlock --version`, `swaywardlock validate` on every shipped example, the PAM file compared with `pam/swaywardlock` and checked to be marked as a config file, the converters
and `--help`, and a search of the installed binary for the test-auth password and test hooks
(there are none). The tarball is an Ubuntu 24.04 build, not a generic Linux build; it needs
glibc 2.39 and the libraries listed in its `RUNTIME.txt`.

```
sudo dnf install ./swaywardlock-VERSION-fedora44-x86_64.rpm       # Fedora (use fedora43 on 43)
sudo pacman -U swaywardlock-VERSION-arch-x86_64.pkg.tar.zst       # Arch
sudo apt install ./swaywardlock_VERSION_amd64.deb                 # Ubuntu 24.04
```

There is no COPR repository, PPA or AUR package. They wait until the release packages have
survived real installations. Or build from source (below), or make a package yourself: `../contrib/PKGBUILD`
(`cd contrib && SWAYWARDLOCK_LOCAL=1 makepkg -si` builds the checkout it is in; without the
variable it downloads the tag tarball), `swaywardlock.spec.rpkg` (`rpkg local`; needs `rpkg`,
`rpm-build`, `cargo-rpm-macros` >= 26 and a git checkout, and packs the committed state of the
tree) and `../contrib/build-deb VERSION` (after `cargo build --release`; needs `scdoc`).
The unit tests need one font installed to run (`ttf-liberation`, `fonts-liberation`,
`liberation-sans-fonts`); without any font they skip the three tests that draw text. The PKGBUILD
and the spec declare it, and the release workflow installs it.

All of them install `/etc/pam.d/swaywardlock` as a config file (it includes `login`, so the packages
depend on the owner of `/etc/pam.d/login`: `util-linux` on Fedora and Arch, `login` on Ubuntu),
`swaywardlock(1)` (`man swaywardlock`, built from `docs/swaywardlock.1.scd` with scdoc), shell
completions and the converters on `PATH`; the examples are in `/usr/share/doc/swaywardlock/examples` and
the design document in `/usr/share/doc/swaywardlock/specs`. `swaywardlock completions
bash|zsh|fish|elvish|powershell` prints a completion script. (The tarball has no completions and
no design document; see its `RUNTIME.txt`.)

From source, swaywardlock authenticates through PAM with the service `swaywardlock`, so install
the shipped PAM file yourself (as root):

```
install -Dm644 pam/swaywardlock /etc/pam.d/swaywardlock
```

Without it, PAM would fall back to `/etc/pam.d/other`, which refuses every password; swaywardlock
checks that the file is there and readable (as libpam looks for it: the service name
lowercased) and exits with an error before locking. The file is swaylock's and
hyprlock's: `auth include login`, so `/etc/pam.d/login` (util-linux) must exist too. `auth { pam service="login"; }` uses another service instead,
and `auth { pam { off; }; }` disables PAM (SIGUSR1, a fingerprint or an explicitly enabled
grace period can unlock then).

Like swaylock, swaywardlock starts PAM when you press Enter, so a stack with `pam_fprintd` first
(Fedora's authselect `with-fingerprint`) reads the finger after Enter; `auth { fingerprint { }; }`
unlocks on a touch at any time. Each question PAM asks after the first (a one-time code) is
shown and answered with the next Enter. If your stack has two modules that both ask for the
password with exactly the same prompt text, `auth { pam { reuse-password; }; }` permits one
reuse of the preceding answer for the immediately repeated prompt. Reuse consumes that cached
answer: a third identical prompt needs another Enter with a fresh answer.

## Working on a layout

```
swaywardlock --preview -c ~/.config/swaywardlock/config.kdl
```

draws the lock screen in an ordinary window instead of locking, and redraws whenever the config
(or anything it includes) changes. A config that fails to parse prints the error and keeps the
last good frame. Typing drives the indicator; Enter shows the checking and wrong-password states
but never checks a password. `--preview-output DP-1` makes the window match `output="DP-1"`
widgets. Close the window to quit.

Known limitation: editing the config when the preview has nearly exhausted its file-descriptor
limit can abort it instead of keeping the last good frame. Preview reload creates fresh calloop
channels, whose constructors panic if they cannot allocate a descriptor. This happens only in
preview replacement; locked mode creates these channels at startup. Restart the preview after
restoring descriptor capacity.

`swaywardlock validate` checks a config without drawing anything.

## Gates

Run inside the dev container (for example, `distrobox enter swayward-dev`),
from this checkout:

```sh
contrib/gate --fast  # iteration checks
contrib/gate --slow  # merge checks; works without a preceding fast run
contrib/gate         # both, preserving the full check inventory
```

Fast runs nightly fmt, clippy with default features and with `test-auth`, default-feature
unit tests, a debug test-auth build, both converters, man-page and documentation KDL checks,
release-metadata consistency checks, and bash/zsh completions with a broken config. Slow runs
real PAM, its own debug test-auth build, try-lock, the complete pytest headless-sway suite,
and a default-feature release build with all eight test-hook scans. CI uses these same modes.
Each harness retains a five-minute timeout; CI also has a 45-minute job timeout.

The **slow-gate budget is five minutes**, including builds. This is a target for test upkeep,
not permission to omit checks. The gate prints each step duration; use pytest's `--durations=20`
to inspect slow cases. The suite runs in parallel with a separate compositor and runtime directory
per test. The migration preserved the existing assertions and temporal checks.

For a targeted slow check after building the debug test-auth executable:

```sh
python3 -m pytest tests/e2e/test_widgets_labels.py::test_preview --timeout 60 --swaywardlock target/debug/swaywardlock
```

The full slow gate clears inherited `PYTEST_ADDOPTS`; a targeted run is not a full slow-gate pass.
Real PAM reports a skip without root or passwordless sudo; man-page syntax reports a skip
without scdoc. Record these honestly, along with any optional pam_unix password check not
requested. An exit-zero run with privileged checks skipped is not full privileged coverage.

Keep a gate record with commit/tree, exact command and scope, wall and step/case durations,
toolchain/container, and skips or failures. Reuse it only for the same tree, scope and
equivalent execution conditions. Workers use fast while iterating and targeted slow checks
for their area; reviewers reuse matching records; the merge owner runs full slow once per
merge batch. Queue test upkeep if slow exceeds the budget. The cold-build five-minute budget
remains unresolved.

Before tearing down an upkeep pass, commit a new dated report under `docs/test-health/`
with conditions, BEFORE/AFTER measurements, findings and evidence, rejected/refuted proposals
and their reasons, and the remaining bottleneck. Do not edit older reports. The next pass
reads the latest report first and re-measures; see [the 2026-10-22 report](https://github.com/swayward-wm/swaywardlock/blob/main/docs/test-health/2026-10-22.md).

Builds stay private to this checkout: the gate overrides inherited `CARGO_TARGET_DIR`, using
`target/gate-normal` for default-auth tests, `target/debug` for test-auth consumers, and
`target/gate-release` for release builds. Do not run concurrent gates in one checkout or
share a writable target across workspaces. The sequential default run can reuse its warm
test-auth build; standalone slow builds it independently. CI caches dependency downloads
only; no shared target or compiler-cache service is required.
