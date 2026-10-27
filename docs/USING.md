# Using swaywardlock

[Back to the README](../README.md).

Source-build and development commands below run from the checkout root; source paths in prose
are relative to this document. Packages install this guide at
`/usr/share/doc/swaywardlock/docs/USING.md`; the binary tarball has `docs/USING.md` beside its README.

## Trying it safely

Typing follows hyprlock where the two lockers differ: only Backspace and Delete repeat, and
keys typed while a password is being checked are dropped. swaylock's control keys are kept:
Ctrl+J, Ctrl+M and Ctrl+D submit, Ctrl+C and Ctrl+U clear (as do Escape, Ctrl+A and
Ctrl+Backspace). Ctrl counts when held or latched, not when locked (an accessibility
StickyKeys lock would otherwise turn typed letters into shortcuts). The man page's KEYS section and the spec's Input section have the details.

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

### Nix

The community-maintained `flake.nix` follows swayward's release/default, debug, checks,
dev shell, formatter and overlay structure. `flake.lock` pins the same nixpkgs revision
as swayward. Package versions come from Cargo (`26.10.0-beta.1`), not the checkout revision.
Both packages use real PAM; **neither enables `test-auth`**.

```sh
nix build .                         # release package, ./result
nix build .#swaywardlock-debug       # unstripped default-auth debug package
nix flake check                     # default-auth tests and installation checks, both profiles
nix develop                         # Rust, pkg-config and native dependencies
nix fmt                             # format the flake
# From another directory (choose a trusted revision):
nix profile add github:swayward-wm/swaywardlock#swaywardlock
```

The flake exposes Linux systems supported by nixpkgs; evaluation is not proof that every
architecture builds. It installs the converters (with store-pinned Python shebangs), bash/fish/zsh
completions, man page, examples and linked documentation under
`result/share/doc/swaywardlock/`. The full `docs/` directory includes screenshots and test-health
reports. The distributed `pam/swaywardlock` is reference material, **not an installed PAM service**.
No NixOS module is supplied; the overlay is `overlays.default`.

On NixOS, add the package to your system configuration and let NixOS generate the PAM stack:

```nix
{ inputs, pkgs, ... }: {
  environment.systemPackages = [ inputs.swaywardlock.packages.${pkgs.stdenv.hostPlatform.system}.default ];
  security.pam.services.swaywardlock = {};
}
```

Here `inputs.swaywardlock` is your flake input. Rebuild your NixOS configuration as usual.
With the locked nixpkgs, the empty service uses the normal generated auth rules, including
`pam_unix` with an absolute store module path, and NixOS provides the privileged
`/run/wrappers/bin/unix_chkpwd` helper. Review local PAM customisations and test authentication
in a disposable nested session before relying on the locker. **Do not copy the distro
`auth include login` file into the Nix store as a substitute for the live NixOS service.**
A package build or container PAM probe does not verify real NixOS user authentication.

On other Linux distributions, `nix profile add` does not configure system authentication.
Use the [manual PAM setup](#manual-pam-setup) below with your distribution's live PAM policy;
check that `login` exists before including it. Nixpkgs' PAM expects its NixOS helper path for
`pam_unix`; a profile install alone does not provide that privileged helper. Do not assume
that successful package installation means password authentication works, and do not create
a setuid helper casually. Use a distro-native package if you cannot arrange and verify a
compatible PAM stack. The host compositor must also have working EGL/OpenGL ES drivers;
the software-rendered checks are not a hardware/driver certification.

The package tests provide C tooling, `id`, stock PAM modules, fonts/fontconfig and Mesa
software EGL. Test output includes conditional skips; a skipped installed-service or real-user
auth check is not privileged coverage. `nix flake check` is a packaging gate, not a replacement
for the [full repository gate](#gates) or a real desktop authentication test. In `nix develop`,
append flags with `RUSTFLAGS="$RUSTFLAGS -C your-flags"`: replacing `RUSTFLAGS` removes the forced
EGL/Wayland links needed for `dlopen()` discovery.

### User-local source install

From a checkout, run as your normal user on the host (requires Python 3, host `ldd` and
`scdoc`, and the existing `swayward-dev` distrobox with build dependencies):

```sh
contrib/install
contrib/install --config swaylock
contrib/install --config hyprlock --config-file /path/to/hyprlock.conf
contrib/install --uninstall
```

The default prefix is `$HOME/.local` (`--prefix DIR` selects another prefix). Put its `bin`
on `PATH`; binaries, converters, man page, completions, examples and linked documentation
are installed there. Builds use only the existing `swayward-dev`, even from another container;
an invocation inside a container delegates installation to the host using an already installed
`host-spawn` (otherwise it fails and asks you to run on the host). `ldd` and `scdoc` therefore
always run on the host, while Cargo runs inside the named build container.
The default-feature release uses private
`target/install/release`, two jobs, an exclusive build lock and a 900-second in-container timeout.
The build container also needs `flock`, `timeout`, `sh`, `rm`, `env` and Python 3.
After Cargo succeeds under the lock, the inner build writes a unique invocation receipt with
an artifact hash. The installer requires that receipt and matching bytes, not just exit status 0:
a declined container entry, failed or timed-out build never falls back to an old binary. The installer neither creates
containers nor installs dependencies, changes sessions or runs sudo. Install/uninstall reject root.

To skip building, supply an **explicitly trusted** binary:

```sh
contrib/install --no-build --binary /path/to/trusted/release/swaywardlock
```

`--no-build` requires `--binary`; `--binary` is forbidden when building. Exact staged bytes
are scanned for test-auth strings before any execution. That scan is **not a provenance
guarantee**: `ldd` can execute untrusted code. Never feed an untrusted download to this option.
Host `ldd` errors and missing libraries stop installation; the check does not certify dlopened
EGL drivers, the compositor or PAM. Completion generation and conversion validation execute
only the staged binary, never launch the locker.

Without `--config` no config is written. Explicit conversion respects `XDG_CONFIG_HOME`
(default `$HOME/.config`), reads `swaylock/config` or `hypr/hyprlock.conf` unless `--config-file`
is provided, and prints converter warnings. A private converted file must pass `validate -c`
before any installed file is replaced. Existing regular configs get a unique private
`config.kdl.bak.*` backup before atomic replacement; conversion, validation or backup failure
leaves the old config alone. `--config-file` alone is an error.

`share/swaywardlock/install-manifest.json` records relative paths and SHA-256 hashes. Repeat
installs replace only unchanged owned files; unrecorded collisions are refused, even if
byte-identical (no implicit adoption). Modified owned files, symlink destinations and linked
ancestors, including dangling links, are refused. The sole system alias exception is Fedora
Atomic's root-owned `/home` → `/var/home`; it is canonicalized after checking its root-owned
real-directory target and parents. Files are individually atomically renamed,
not a whole-prefix transaction: filesystem failure or interruption during replacement can leave
a partial install. Keep the manifest intact. After interruption, rerun; if it refuses, compare
the named files with your checkout and trusted artifact. Save any user edits, then manually
remove only confirmed installer-written partial files and rerun. Uninstall can first remove
unchanged recorded files, but will not clear changed or unrecorded partial files for you.
Uninstall removes only hash-matching recorded files;
modified/unrecorded files, config, backups, directories and PAM are preserved. It never recursively
removes a shared directory. `--uninstall` rejects build/config options.

### Manual PAM setup

From source, swaywardlock authenticates through PAM with the service `swaywardlock`. The
installer leaves an existing `/etc/pam.d/swaywardlock` or `/usr/lib/pam.d/swaywardlock` alone.
If absent, it prints a shell-quoted command to run manually as root, guarded against overwriting
a service that appears later. It **never writes PAM or executes sudo**. Review `pam/swaywardlock`
and ensure the included `login` service exists before running that command. For manual setup
without the installer, as root (noclobber also rejects a dangling destination link):

```sh
test ! -e /etc/pam.d/swaywardlock && test ! -L /etc/pam.d/swaywardlock &&
  test ! -e /usr/lib/pam.d/swaywardlock && test ! -L /usr/lib/pam.d/swaywardlock &&
  (umask 022; set -C; cat pam/swaywardlock > /etc/pam.d/swaywardlock)
```

Without it, PAM would fall back to `/etc/pam.d/other`, which refuses every password; swaywardlock
checks that the file is there and readable (as libpam looks for it: the service name
lowercased) and, if it is not, warns and still locks, with passwords refused (only SIGUSR1 unlocks). The file is swaylock's and
hyprlock's: `auth include login`, so `/etc/pam.d/login` (util-linux) must exist too. `auth { pam service="login"; }` uses another service instead,
and `auth { pam { off; }; }` disables PAM (SIGUSR1, a fingerprint or an explicitly enabled
grace period can unlock then). In the grace period (`--grace SECONDS`) a key press, or pointer motion
of more than 5 px from where the pointer entered, unlocks; a click or smaller jitter does not.

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
release-metadata consistency checks, isolated stdlib installer tests, and bash/zsh completions
with a broken config. Slow runs
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
