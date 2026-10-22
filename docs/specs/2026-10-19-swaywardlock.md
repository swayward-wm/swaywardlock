# swaywardlock

Status: draft for review. Date: 2026-10-19.
Background and evidence: [feasibility study](2026-10-19-swaywardlock-feasibility.md).

## Problem

swaylock is a dependable locker with a small surface: a coloured ring, a
background image and about 50 colour flags. hyprlock has the visuals people
want (blurred screenshot, labels, an input field, gradients, animations,
fingerprint unlock) but runs its own config language and its own GL stack.

swaywardlock is the swayward family's locker. It keeps swaylock's process
contract, so swayidle and scripts keep working, and matches hyprlock's
features. It uses KDL config with the same syntax swayward uses for colours,
gradients, blur, shadows and animations, and it renders with the same
shaders, so a gradient border looks identical in the compositor and the
lock screen.

## Goals

1. A drop-in replacement for swaylock in swayidle and scripts:
   `swaywardlock -f`, `--ready-fd`, SIGUSR1, exit codes.
2. Feature parity with hyprlock (list under "Scope").
3. Runs on any Wayland compositor that offers `ext-session-lock-v1`: swayward,
   sway, niri, and others.
4. KDL config whose shared vocabulary (colours, gradients, blur, shadow,
   animations, `include`) decodes with the same code and syntax as swayward.
5. Converters from swaylock and hyprlock configs.
6. Never unlocks by accident. Every failure path either leaves the session
   locked or exits without unlocking.

## Non-goals

- Accepting swaylock's or hyprlock's config files or styling flags. The
  converters take care of those.
- A setuid or shadow-file auth path. PAM only.
- An IPC socket of its own.
- Compositor-side changes. swayward does not add ext-image-copy-capture
  for this.
- Sharing crates with swayward. The code we need is copied.
- Click actions on widgets (hyprlock's `onclick`). They are rarely used and
  run commands from the lock screen. Left out of scope until someone asks for
  it.

## Decisions and why

| # | Decision | Why |
|---|---|---|
| D1 | Own repo `swayward-wm/swaywardlock`, own release | No shared crates, so nothing ties it to swayward's gates, oracle tiers or release |
| D2 | Copy code from swayward, no shared crates | Splitting swayward's niri-derived `render_helpers` into a crate makes every niri tag merge harder. Copying costs one-off effort and visible drift |
| D3 | Smithay for rendering: `GlesRenderer`, render elements, `OutputDamageTracker` | Same shaders and the same results as swayward. If a smithay problem turns up, patch smithay rather than replace the renderer |
| D4 | smithay `backend_winit` is allowed | Gives the existing `EGLNativeSurface for WlEglSurface`. winit compiles in, but no winit window or event loop is ever created |
| D5 | smithay-client-toolkit for protocols, calloop for the loop | Same event-loop model as swayward. sctk ships session-lock, output, seat/xkb with compose |
| D6 | Own config file `$XDG_CONFIG_HOME/swaywardlock/config.kdl` | Standalone tool; works without swayward |
| D7 | PAM in a worker thread; fprintd over zbus | hyprlock's model. No privileged helper process |
| D8 | Screenshots: wlr-screencopy first, ext-image-copy-capture second, colour fallback | wlr-screencopy covers swayward, sway and niri. ext-image-copy-capture covers newer compositors |
| D9 | Output names and keyboard layout come from Wayland, `$SWAYSOCK` is not used | `wl_output` v4 names and xkb layout names work on every compositor. `$SWAYSOCK` would add nothing |
| D10 | `panic = "abort"` | A panic must never run an unlock path. The protocol keeps the session locked when the client dies |

D9 drops the optional `$SWAYSOCK` support from the feasibility study:
everything it offered already comes from Wayland.

## Command line

```
swaywardlock [OPTIONS]
swaywardlock validate [-c PATH]
swaywardlock completions <SHELL>
```

| Flag | Source | Meaning |
|---|---|---|
| `-c, --config PATH` | both | Config path. Also `SWAYWARDLOCK_CONFIG`. Flag wins |
| `-f, --daemonize` | swaylock | Fork into the background once locked |
| `--ready-fd FD` | swaylock | Write `\n` to FD once locked, then close it |
| `-g, --grace SECONDS` | hyprlock | Any key, pointer button or motion unlocks within this window, without auth |
| `--no-fade-in` | hyprlock | Skip the fade-in animation |
| `--immediate-render` | hyprlock | Lock before screenshots and images finish loading |
| `--display NAME` | hyprlock | Wayland display to connect to |
| `--preview` | new | Draw in an xdg-shell window instead of locking; reload on config change (see below) |
| `--preview-output NAME` | new | With `--preview`, the output name the window matches as |
| `-v, --verbose` / `-q, --quiet` | hyprlock | Log level. `RUST_LOG` also works |
| `-V, --version`, `-h, --help` | both | |

Ordering (swaylock's, kept exactly): lock, wait for `locked`, write the ready
fd, then daemonize. Whoever reads the ready fd or waits on the parent's exit
knows the screen is locked.

Signals: SIGUSR1 unlocks (fade out, then unlock). SIGUSR2 forces `cmd[]`
labels and `reload-cmd` images and backgrounds to refresh now. SIGTERM and
SIGINT keep their default action: the process dies and the compositor keeps
the session locked. They never unlock. This is swaylock's behaviour, and it
lets a service manager stop a stuck locker.

Exit codes:

| Code | When |
|---|---|
| 0 | Unlocked after auth, grace, or SIGUSR1 |
| 1 | Config error, missing `ext-session-lock-v1`, EGL/renderer setup failure. Never locked |
| 2 | The compositor sent `finished` (another locker holds the lock, or the lock was refused). swaylock uses the same code |

`swaywardlock validate` parses the config, prints miette diagnostics and
exits 0 or 1 without connecting to Wayland.

### Preview mode

Added during Phase 2 for iterating on layouts. `--preview` uses the same widgets, renderer and
input state machine as the lock, but the surface is an xdg toplevel rather than a lock surface.
It never calls `lock()`, never starts PAM and has no unlock path; Enter runs a fake 600 ms check
that always fails, so the verifying and wrong styling can be seen. It polls the config file and
its includes twice a second and rebuilds on change. A parse error keeps the previous config on
screen and prints the diagnostic. The window stands in for one output; `--preview-output` names
it for `output=` matching. It conflicts with `-f` and `--ready-fd`.

## Config

Path: `--config`, then `SWAYWARDLOCK_CONFIG`, then
`$XDG_CONFIG_HOME/swaywardlock/config.kdl`, then
`/etc/swaywardlock/config.kdl`, then a compiled-in default. A missing file is
not an error; it falls back to the default config (a plain swaylock-style
look). Errors abort with exit 1 before locking: a lock screen that fails to
parse its config must not leave the user with a broken lock.

### Shared vocabulary (copied from swayward-config)

Each of these decodes exactly as in swayward, from copied code:

- **Colour:** any CSS colour string, `"#rrggbbaa"`, `"rgb(...)"`, names.
- **Gradient:** `from=... to=... angle=180 in="oklch longer hue"`. Same
  as swayward's `active-gradient`, with CSS angles: 0 points up, 90 runs
  left to right. hyprlock's angles are 90° behind (its 0 runs left to
  right, 90 top to bottom), so a hyprlock `45deg` is `angle=135` here.
  `relative-to` does not apply and is rejected.
- **Blur:** `blur { passes 3; offset 3.0; noise 0.02; saturation 1.5; }`,
  swayward's dual-Kawase blur.
- **Shadow:** `shadow { softness 30; spread 5; offset x=0 y=5; color "#00000070"; }`.
- **Animation:** `{ duration-ms 200; curve "ease-out-cubic"; }`, or
  `curve "cubic-bezier" a b c d`, or `spring damping-ratio=1.0 stiffness=800 epsilon=0.001`
  (the damping ratio must be written as a float).
  `off` disables one.
- **include:** `include "path"` and `include optional=true "path"`, with
  the same recursion and error rules.

hyprlock's background `contrast` and `brightness` do not exist in swayward's
blur. They are added to the swaywardlock `background` node (next section)
and are not part of the shared vocabulary. `vibrancy` and
`vibrancy-darkness` are Phase 3 leftovers.

### Top-level nodes

```kdl
general {
    hide-cursor
    ignore-empty-input
    fail-timeout-ms 2000        // how long the fail state shows
    clear-input-after-ms 10000  // swaylock clears a stale buffer after 10 s
}

auth {
    pam service="swaywardlock"  // off to disable
    fingerprint {               // absent = disabled
        ready-message "Scan fingerprint to unlock"
        present-message "Scanning fingerprint"
        retry-delay-ms 250
    }
}

animations {
    off                         // disables all
    fade-in   { duration-ms 200; curve "ease-out-cubic"; }
    fade-out  { duration-ms 200; curve "ease-out-cubic"; }
    input-field-fade { duration-ms 300; curve "ease-out-cubic"; }  // accepted; unused until fade-on-empty
}
```

`fractional-scale` and the `input-field-colors`, `input-field-width` and
`input-field-dots` animations are Phase 3 leftovers and are rejected for now.

Widgets follow, in any order. Any number of each kind is allowed.

### Common widget properties

Every widget node accepts these:

| Property | Type | Default | Meaning |
|---|---|---|---|
| `output="DP-1"` | node property, string | all outputs | Match on `wl_output` name, or on a description prefix with `output="desc:Dell ..."` (hyprlock's `monitor`) |
| `position x y` | child, two numbers, each px or `"50%"` | `0 0` | Offset from the anchor |
| `halign` | `"left" "center" "right" "none"` | `"center"` | Horizontal anchor |
| `valign` | `"top" "center" "bottom" "none"` | `"center"` | Vertical anchor |
| `z-index` | integer | `0` (`background` -1) | Draw order. Equal z-index keeps file order |
| `shadow { ... }` | shared shadow | off | On `image`, `shape` and `input-field` |

`position` uses logical pixels. Positive `y` moves up, as in hyprlock. With
`halign`/`valign "none"` the position alone places the widget, measured from
the left / bottom edge. `rotate` (hyprlock's `rotate` on labels, images and
shapes) is a Phase 3 leftover; label shadows are too.

### Widgets

**`background`**: one per output; the last matching one wins.

```kdl
background output="DP-1" {
    color "#111111"            // under everything; shown if nothing else loads
    screenshot                 // or: path "~/wall.png"
    fit "cover"                // cover | contain | fill | tile | center (swaylock's scaling modes)
    blur { passes 3; offset 3.0; noise 0.0117; saturation 1.0; }
    contrast 0.8917
    brightness 0.8172
}
```

hyprlock's `vibrancy`, `vibrancy_darkness`, `reload_time`, `reload_cmd` and
`crossfade_time` are Phase 3 leftovers.

**`label`**

```kdl
label {
    text "$TIME"               // see "Text substitution"
    font "Sans 16"             // Pango font description
    color "#ffffff"
    align "center"             // left | center | right (multi-line; `<br/>` breaks a line)
    position 0 80; valign "top"
}
```

**`image`**

```kdl
image {
    path "~/.face"
    size 150                   // shorter side, px (hyprlock's)
    corner-radius "full"       // the default: a circle (hyprlock's rounding -1); or px, or 4 values
    border { width 4; color "#dddddd"; }   // or gradient from=... to=...
}
```

Image `reload_time`/`reload_cmd` are Phase 3 leftovers.

**`shape`**

```kdl
shape {
    size 360 60                // px or "%"
    color "#11111180"
    corner-radius 12
    border { width 2; gradient from="#80c8ff" to="#c7ff7f" angle=45; }
    xray                       // no fill, so the background shows (see below)
}
```

`xray` differs from hyprlock: hyprlock clears the box to transparent, wiping
lower widgets too; here the shape just has no fill, so lower widgets still
show through. Over a plain background the two look the same.

**`input-field`**: hyprlock's password field.

```kdl
input-field {
    size 400 90
    corner-radius "full"                        // the default (hyprlock's rounding -1)
    inner-color "#dddddd"
    outline { width 4; color "#111111"; }       // or gradient
    font "Sans"; font-color "#000000"
    placeholder-text "<i>Input Password</i>"    // Pango markup
    dots size=0.25 spacing=0.2 center=true corner-radius=-1   // -1 round, -2 follow the field
    // dots text="*"                            // draw text at its own size instead of dots
    hide-input "#00ff99ee"                      // random colour per key instead of dots; colour optional
    fade-on-empty false                         // accepted; Phase 3 (fade-on-empty timeout-ms=2000)
    check { color "#22cc88"; text "<i>Checking…</i>"; }
    fail  { color "#cc2222"; text "<i>$FAIL ($ATTEMPTS)</i>"; }
    capslock-color "#ffaa00"
    numlock-color  "#00aaff"
    bothlock-color "#ff00ff"
    invert-numlock
    swap-font-color
}
```

**`ring-indicator`**: swaylock's indicator, as a widget. Its states match
swaylock's: idle, typing (key highlight), backspace highlight, clear,
verifying, wrong, caps lock.

```kdl
ring-indicator {
    radius 50
    thickness 10
    font "Sans"; font-size 0       // 0 = scale with radius, like swaylock
    idle-visible false             // swaylock's indicator-idle-visible
    show-failed-attempts
    show-keyboard-layout "auto"    // auto | always | never
    show-caps-lock-text true
    show-caps-lock-indicator       // swaylock's --indicator-caps-lock
    line-uses "line"               // line | inside | ring
    colors {
        state "input"     inside="#000000c0" ring="#337d00ff" line="#000000ff" text="#e5a445ff"
        state "clear"     inside="#e5a445c0" ring="#e5a445ff" text="#000000ff"
        state "verifying" inside="#0072ffc0" ring="#3300faff" text="#000000ff"
        state "wrong"     inside="#fa0000c0" ring="#7d3300ff" text="#000000ff"
        state "caps-lock" inside="#000000c0" ring="#e5a445ff" text="#e5a445ff"
        key-highlight "#33db00ff"
        bs-highlight "#db3300ff"
        caps-lock-key-highlight "#33db00ff"
        caps-lock-bs-highlight "#db3300ff"
        separator "#000000ff"
        layout bg="#000000c0" border="#00000000" text="#ffffffff"
    }
}
```

Each swaylock colour flag has exactly one place in this node. That makes
`swaylock-to-kdl` mechanical.

### Text substitution

These apply to `label text`, `input-field` `placeholder-text`,
`check text` and `fail text`.

| Token | Value |
|---|---|
| `$USER` | login name |
| `$DESC` | GECOS full name |
| `$TIME` / `$TIME12` | `HH:MM` / `hh:MM AM`, local `TZ` |
| `$ATTEMPTS` / `$ATTEMPTS[text]` | failed attempts; the bracket text replaces `0` |
| `$LAYOUT` / `$LAYOUT[us,de,!]` | active xkb layout name; the bracket list maps layout index to text, `!` hides it |
| `$FAIL` | last failure message (PAM or fprintd) |
| `$PAMPROMPT` / `$PAMFAIL` | PAM's password prompt / last PAM failure |
| `$FPRINTPROMPT` / `$FPRINTFAIL` | fprintd prompt / failure |

`cmd[update:MS] <shell>` (hyprlock's syntax, kept unchanged so users can
copy it): substitute the tokens above into `<shell>`, run it with `sh -c`
every MS milliseconds and show stdout, trimmed, as is. `cmd[update:MS:1]` also refreshes on SIGUSR2. Commands run as the
user with the user's environment. They run on a worker thread, so a slow
command never blocks rendering. A command still running when the next tick
comes is not started again. All children are killed on unlock.

Labels that contain `$TIME` refresh once a minute, on the minute boundary.

## Architecture

One process, one calloop event loop on the main thread, two kinds of worker
thread (auth and commands/resources). Modules:

| Module | Job | Depends on |
|---|---|---|
| `cli` | clap args, `validate`, `completions` | `config` |
| `config` | KDL types and decoding; `copied/` holds verbatim or trimmed copies from swayward-config | knuffel, miette |
| `wayland` | sctk state: registry, outputs, seat/keyboard/pointer, session lock, screencopy clients | sctk, wayland-protocols(-wlr) |
| `egl` | `EGLNativeDisplay` for `wl_display`; creates one `EGLDisplay`, `EGLContext`, `GlesRenderer`, plus one `WlEglSurface` + `EGLSurface` per lock surface | smithay, wayland-egl |
| `render` | copied render helpers (shaders, border, shadow, blur, solid colour, texture, memory buffer), Pango text to texture, one `OutputDamageTracker` per output | smithay |
| `widgets` | each widget: layout from properties + output size, state, `render_elements(&mut GlesRenderer) -> Vec<Element>` | `render`, `config`, `animation` |
| `animation` | copied clock, bezier and spring | none |
| `auth` | `Authenticator` trait; `Pam` and `Fprintd` implementations on threads; results come back over a calloop channel | a PAM binding (`pam-client2` or `nonstick`, picked in Phase 0), zbus |
| `input` | password buffer, key handling, grace period | `auth` |
| `resources` | async loads: images, screenshots, `cmd[]` output, `reload-cmd` | image crate |
| `process` | ready-fd, daemonize, signals | libc |

### Startup sequence

1. Parse args and config. Error → exit 1.
2. Connect. Bind globals. If `ext_session_lock_manager_v1` is missing,
   exit 1.
3. Unless `--immediate-render` is set: take a screenshot of each output that
   any `background` asks for, and load images. Wait at most 2 s, then go on
   with whatever has arrived. hyprlock does the same: the screenshot must be
   taken before locking, because the compositor shows only lock surfaces
   afterwards.
4. Create the EGL display, context and renderer. Compile the copied
   shaders (`shaders::init`, `resources::init`). Error → exit 1. This
   comes before `lock()`, so a GL failure never leaves a locked session
   showing nothing.
5. Call `lock()`. Create a lock surface for each output. Render the first
   frame on each configure.
6. On `locked`: write the ready fd, daemonize, install signal handlers, start
   the grace timer.
7. Run until unlock.
8. Unlock: fade out, `unlock_and_destroy`, roundtrip, kill children,
   exit 0.

On `finished` at any point: exit 2 without unlocking (there is nothing to
unlock).

Hotplug: a new output gets a lock surface; a removed one drops its surface,
renderer surface and damage tracker. Widgets re-lay out on `configure`
(size or scale change).

### Rendering

- The `egl` module has one hand-written piece: `EGLNativeDisplay for
  WaylandDisplay(NonNull<wl_display>)`. It copies the Wayland branch of
  smithay's winit impl (`PLATFORM_WAYLAND_KHR`, `_EXT`). We offer it upstream
  to smithay after Phase 0, when it has run in practice.
- sctk and `wayland-client` use the `system` feature (libwayland-client),
  which `wayland-egl` requires. `Connection::backend().display_ptr()` gives
  the `wl_display`.
- Each frame: make the output's `EGLSurface` current, collect widget elements
  in z order, call `OutputDamageTracker::render_output`, then swap with damage.
  Frame callbacks drive redraws, the same way swayward's redraw loop works;
  a frame is drawn only when an animation is running, input changed, or a
  resource arrived.
- Scale: `wp_fractional_scale_v1` when present, otherwise the integer
  `wl_surface` preferred scale. The `WlEglSurface` gets buffer-size
  dimensions; `wp_viewport` sets the logical size.
- `background` post-processing (contrast, brightness, vibrancy) goes in a
  new fragment shader written for swaywardlock, in the copied
  `postprocess.frag` style. It runs once per resource and per blur
  change, and the result is cached in an offscreen texture.
- Fade in and out: a black overlay over the whole output at `1 - opacity`, so the lock
  screen fades in from black and out to black.

### Auth

```rust
trait Authenticator {
    /// Start, or restart after a result. Results arrive on the channel.
    fn start(&mut self);
    fn submit(&mut self, secret: SecretBuf); // PAM only; fprintd ignores it
    fn cancel(&mut self);
}

enum AuthEvent { Prompt(Source, String), Success(Source), Failure(Source, String) }
```

- PAM runs `pam_authenticate` and `pam_acct_mgmt` in a dedicated thread. The
  conversation function sends prompts as `AuthEvent::Prompt` and blocks on
  the next `submit`. Ship `pam.d/swaywardlock` with
  `auth include login`.
- fprintd: `net.reactivated.Fprint.Manager.GetDefaultDevice`, `Claim`,
  `VerifyStart("any")`, the `VerifyStatus` signal, retry after
  `retry-delay-ms`. It listens to logind `PrepareForSleep` to stop scanning
  before sleep and start again after. Its presence is optional: if there is no
  fprintd or no device, log a warning and go on with PAM only.
- Whichever source sends `Success` first wins. Unlock goes through one
  function, `request_unlock(Reason)`, which is the only caller of
  `unlock_and_destroy`.
- `SecretBuf` is `mlock`ed (with a warning when that fails, like
  swaylock), zeroed on drop (`zeroize`), and never `Debug`- or
  `Display`-printable. It is cleared after `clear-input-after-ms` of no
  input.

### Input

- Keys: compose-aware via sctk. Enter submits (dropped if the buffer is
  empty and `ignore-empty-input` is set). Backspace and Delete remove one
  character and repeat. Escape, Ctrl+U, Ctrl+Backspace and Ctrl+A clear.
  Other keysyms append their UTF-8.
- While a check is running, keys are ignored, as in hyprlock.
- Caps/Num lock state and the active layout come from xkb state after each
  `modifiers` event.
- Grace period: until it ends, any key press, pointer button or motion calls
  `request_unlock(Grace)`. Touch is not handled.
- `hide-cursor`: set a null cursor on pointer enter.
  Otherwise use cursor-shape `default` when present.

## Copying from swayward

`COPIED.md` in the repository root is the authoritative list: each copied file, its source in
swayward, the source sha and how far it was trimmed. `contrib/copied-diff <swayward> [sha]` checks
that verbatim copies still match their source and lists upstream changes since the sha. It is
run by hand, not in CI.

Phase 2 copied swayward's border, shadow and shader-element code with their shaders, and the
animation runtime and its KDL. Their `TtyRenderer` impls are removed and `NiriRenderer` is plain
`GlesRenderer`; `COPIED.md` has the details.

Keep the niri/swayward copyright and GPL-3.0-or-later lines. No per-file AI markers, following
swayward's AGENTS.md.

## Converters

`contrib/swaylock-to-kdl` (Python, stdlib only, in the style of
swayward's `sway-to-kdl`):

- Input: a swaylock config file, or `--args 'swaylock -f -c 000000 ...'`.
- Output: KDL on stdout, plus the command line to use
  (`swaywardlock -f`), with styling flags removed.
- Maps `color`/`image`/`scaling`/`tiling` to `background`, and every
  indicator, ring, line, text, inside, layout and highlight flag to
  `ring-indicator`. Maps `ignore-empty-password` to `general`.
- Unknown keys become a comment with a warning on stderr. The tool never
  silently drops a key.
- A corpus test runs each sample through the converter and then through
  `swaywardlock validate`.

`contrib/hyprlock-to-kdl`: the same approach. It understands hyprlang
sections, `$var = value` substitution and `source =`, and maps each section
onto the matching widget. `bezier`/`animation` lines become `animations`
entries, with a warning where a curve has no counterpart.

## Testing

| Layer | What | How |
|---|---|---|
| Config | every node decodes; defaults; errors carry spans | insta snapshots, as in swayward-config |
| Converters | corpus round trip | Python test + `swaywardlock validate` |
| Text | each substitution token, `cmd[]` parsing | unit tests |
| Input/auth | key handling, grace, `request_unlock` only on success | unit tests with a `FakeAuth` |
| Process | ready-fd ordering, daemonize, SIGUSR1, exit 2 on `finished` | integration |
| End to end | lock, wrong password, right password, hotplug, no screencopy | headless sway (`WLR_BACKENDS=headless`, `WLR_RENDERER=pixman` or llvmpipe), `wtype` for keys, `FakeAuth` via a `--test-auth` build feature |
| End to end | the same on swayward | swayward with `WLR_HEADLESS_OUTPUTS` |
| Visual | each widget against reference PNGs | offscreen `GlesRenderer`, as in `swayward-visual-tests` |

`--test-auth` exists only behind a cargo feature that release builds don't
enable. CI checks that a release build has no such flag.

Gate: `contrib/gate` runs nightly fmt, clippy, `cargo test`, the
converter corpus and the headless-sway end-to-end run.

## Phases

Each phase ends with the gate passing.

**Phase 0: spike.** Repo skeleton, cargo workspace, CI. sctk session lock,
`egl` module, `GlesRenderer` clearing each surface to a colour, PAM with a
hard-coded prompt, `-f`, `--ready-fd`, SIGUSR1, exit codes. Headless-sway
end-to-end test. Exit criterion: lock and unlock work on swayward and
headless sway, and on llvmpipe in CI.

**Phase 1: swaylock parity.** Copy config and render code with
`COPIED.md`; config loader, `include`, `validate`; `background` (colour,
image, fit); `ring-indicator`; caps and layout text; password buffer
hardening; `swaylock-to-kdl`. Exit criterion: the converter corpus renders
and visual tests match swaylock's look.

**Phase 2: hyprlock parity.** `label`, `image`, `shape`, `input-field`,
text substitution, `cmd[]`, screencopy, blur and post-processing,
animations and fades, grace, per-output widgets, fprintd. Exit criterion:
the converted examples from hyprlock's wiki render.

**Phase 3: rest.** ext-image-copy-capture, `hyprlock-to-kdl`,
`reload-cmd`/crossfade, packaging (PKGBUILD, rpm spec, pam file), man page,
offering the EGL display impl upstream to smithay.

## Risks

| Risk | Mitigation |
|---|---|
| smithay renderer has an unknown problem on a client surface | Phase 0 finds it. Patch smithay with a `[patch]` entry and upstream the fix |
| Mesa EGL on a headless CI runner without a GPU | Phase 0 exit criterion; llvmpipe via `LIBGL_ALWAYS_SOFTWARE=1` |
| Copied code drifts from swayward | `COPIED.md` + `copied-diff` |
| smithay pin gets old | Bump on our own schedule; `copied-diff` shows what swayward changed with each bump |
| A crash at the wrong time shows a blank lock screen | The compositor keeps the session locked, which is the safe failure. Renderer setup happens before `lock()` |

## Open questions

None block Phase 0. Answer before Phase 2:

1. Should `$TIME` accept a strftime format (`$TIME[%H:%M:%S]`) on top of
   hyprlock's two forms? `cmd[update:1000] date +...` covers it already, so
   the default is no.

## Phase 0 findings (2026-10-19)

Recorded after the spike; the sections above are unchanged.

- The smithay renderer works on a client lock surface: `EGLDisplay` from our
  `EGLNativeDisplay` for `wl_display` (`src/egl.rs`, ~30 lines), smithay's
  `WlEglSurface` support, `GlesRenderer::bind(EGLSurface)`, clear, swap.
  Verified on llvmpipe (Mesa 26.2.3) against headless sway 1.11 and
  swayward 339f99e8.
- `--daemonize` cannot fork after locking, as swaylock does: Mesa and the
  auth thread exist by then and `fork` keeps only the calling thread.
  swaywardlock forks first, and the parent waits on a pipe until the child
  reports `locked` (exit 0) or an error code. Seen from outside, the contract
  is unchanged.
- SIGUSR1 must be blocked in every thread before any thread starts.
  Otherwise the kernel can deliver it to a Mesa thread, where the default
  action kills the process. The e2e test caught this (exit 138).
- PAM library: `pam-client2` 0.5.5 (MPL-2.0). Checked against real
  `pam_deny`/`pam_permit` services. It cannot sit beside `nonstick` because
  both link `pam`. (Phase 1 replaced it with raw `pam-sys2`; see below.)
- Headless sway has no keyboard until a virtual one exists. Each `wtype` run
  creates and destroys its own, so tests leave a delay (`wtype -s`) before
  typing.

## Phase 2 notes (2026-10-19)

- `label`: `text`, `font` (a Pango description such as "Sans Bold 24"), `color`, `align`,
  `markup` (on by default, as in hyprlock). There is no separate `font-size`: Pango descriptions
  carry the size. Invalid markup is drawn as literal text with a warning. Label shadows from the
  spec sketch are not implemented: the shadow shader draws a box, and a text shadow needs a
  blurred copy of the glyphs. Deferred.
- `image`: `size` is the shorter side after scaling, keeping the aspect ratio (hyprlock scales by
  `max(size / w, size / h)`, which is the same thing). Circular by default, like hyprlock's
  `rounding -1`. The border and shadow are drawn by the shape renderer, so they match `shape`.
- `cmd[]` children run in their own process group, and the whole group is killed on unlock. With
  only `sh` killed, a pipeline such as `sleep 30 | cat` would leave `sleep` behind. A unit test
  fails with plain `Child::kill`.
- SIGUSR2 has to be blocked in all threads early, for the same reason as SIGUSR1.
- `input-field` follows hyprlock's `updateColors`/`updatePlaceholder`/`draw`: colour
  precedence (caps+num > caps > num, then checking and failed override), the fail colour as the
  caps-lock fallback, borderless fields recolouring the inner box, and dot sizing and capping.
  Implemented as the pure `Look::new` plus geometry in `dot_rects`, with unit tests per rule.
  Renamed for KDL: `outline { width; color|gradient }` (hyprlock's `outline_thickness` and
  `outer_color`), `dots size= spacing= center= corner-radius= text=`, `check { color; text }`,
  `fail { color; text }`. "Failed" means swaylock's invalid state (until `fail-timeout-ms`).
  hyprlock instead shows the fail text until the next key.
- Hide-input colours are seeded by a keystroke counter, never by the typed characters, so the
  animation cannot leak anything about the password. A unit test checks that two different
  characters at the same position look identical.
- Not yet: the width animation that grows the field to fit long placeholder text, and
  fade-on-empty (parsed, drawn fully opaque until fades land in 2e). Both are animations.
- `background { screenshot }` captures every output through wlr-screencopy before `lock()`, waits
  at most 2 s, and falls back to the colour for outputs that fail or time out.
  `--immediate-render` skips the capture. shm (A|X)(RGB|BGR)8888 and 2101010 and y-invert are
  handled; if none is offered the capture gives up at `buffer_done`.
  ext-image-copy-capture stays in Phase 3.
- `contrast` and `brightness` follow hyprlock's FRAGBLURPREPARE (gain curve, brighten above 1)
  and FRAGBLURFINISH (dim below 1), applied on the CPU once per background. `vibrancy` is not
  implemented: it is a saturation boost inside hyprlock's own blur shader, and swaywardlock uses
  swayward's blur. Noise comes from swayward's blur options.
- Animations: swayward's runtime (`src/animation/`) and KDL (`duration-ms` + `curve`, `spring`,
  `off`) are copied. swaywardlock's named animations are `fade-in`, `fade-out` and
  `input-field-fade` (accepted, unused until `fade-on-empty`); `animations { off }` disables all.
  A zero-length fade-out unlocks at once. Fade in starts on `locked`. Fade out
  runs before the unlock: `request_unlock` starts it, and `finish_unlock`, the only caller of
  `unlock_and_destroy`, runs when it ends. An unlock never skips past the fade-out except when it
  is off. A redraw timer at about 60 Hz runs only while something animates.
- `--grace N`: within N seconds of `locked`, a key press, pointer motion or button unlocks without
  a password, as in hyprlock. It is off by default. The grace window opens on `locked`, not at
  start, so slow screenshots cannot use it up.
- Fingerprint: fprintd over zbus's blocking API on a worker thread, following hyprlock's
  `CFingerprint`. It claims the default device, runs `VerifyStart("any")`, and follows
  `VerifyStatus` through retry prompts, up to three non-matches with `retry-delay-ms` between
  them, and logind `PrepareForSleep`. Only `verify-match` requests an unlock, through the same
  `request_unlock` as PAM. It is off by default (`auth { fingerprint {} }` enables it) and never
  runs in preview. A missing fprintd or device is a warning. The worker releases the device when
  its loop ends; on a normal unlock the process exits and fprintd drops the claim with the
  connection. e2e tests use `contrib/fake-fprintd` on a private bus set as
  `DBUS_SYSTEM_BUS_ADDRESS`.
- Signal trust (fixed after the Phase 2 review): fprintd and logind signals are accepted only
  from the unique name that owned `net.reactivated.Fprint` / `org.freedesktop.login1` at start.
  The system bus policy lets any client send a signal addressed to another connection, and
  zbus cannot match a well-known sender name locally, so filtering by the well-known name let
  any local process forge `verify-match` and unlock. `contrib/forge-fprint` tests this, both
  broadcast and directed. A restarted fprintd (new unique name) ends fingerprint unlock for
  that lock; PAM still works.
- `$DESC` is the GECOS name up to the first comma. hyprlock shows the whole field, but the
  rest is room and phone numbers.

### Phase 2 exit check

hyprlock's `assets/example.conf`, translated by hand to `examples/hyprlock-example.kdl`, parses
(unit test) and renders on headless sway at 1280x800 over a swaybg wallpaper. The checks were
done by pixel sampling: the screenshot background blurs (a striped image goes uniform grey with
`passes 3`, and is pixel-exact without blur); the time and date labels sit top-right; the
gradient outline draws; typing adds dots in the font colour; the `$LAYOUT[en,ru]` label draws.
Left out of the translation: `onclick` (out of scope) and hyprlock's `inputFieldDots` animation.

Phase 2 leftovers, moved to Phase 3: `rotate`, the input-field width/colour/dots animations,
`fade-on-empty`, label and ring shadows, `vibrancy`/`vibrancy-darkness`, background and image
reload/crossfade, fractional scale.

### Phase 2 review (2026-10-19)

A six-reviewer panel (correctness, security, simplicity, tests, docs, hyprlock parity) went over
5569c54..a8ea570. Fixed in 3aaa6f4 (security) and 087ca8b (the rest): forged fprintd/logind
signals could unlock (signals are now trusted by unique name); `fade-out { duration-ms 0 }` and a
`cmd[]` child that closed stdout both left the session locked for good; `--ready-fd` leaked to
`cmd[]` children; hide-input drew inside the field; `cmd[]` ran before `$VAR` substitution;
fingerprint misses did not count as attempts; `$PAMPROMPT`, `<br/>`, text dots at natural size,
dot rounding sentinels, `"none"` alignment and more screencopy formats now follow hyprlock. This
section of the spec was brought back in line with the code: every `kdl` example validates.

## Phase 1 findings (2026-10-19)

- The ring is drawn with cairo and Pango into a texture and composited with smithay's
  `TextureRenderElement`. It is a port of swaylock's `render_frame`, rather than being built from
  copied swayward shader elements. Those elements (border, shadow) are still worth copying for
  Phase 2's `shape` and `input-field`, where gradients and shadows matter.
- The ring's state name is `input`, not `idle`. swaylock calls the colour set `input` and it
  covers typing as well as idle. The `line-uses` default is `line`, swaylock's own default.
- EGL window surfaces are y-up. The damage tracker needs `Transform::Flipped180`, as in smithay's
  winit examples. The symmetric Phase 0 solid colour hid this; the e2e image test caught it.
- Buffer age is fixed at 0 (full redraw). The real age needs the surface current, and that only
  holds inside `bind`, while the target borrows the surface. Lock screens redraw rarely; revisit
  if a profile says otherwise.
- `swaylock-to-kdl` maps swaylock's pixel `--indicator-x/y-position` (the ring centre from the
  top-left) to `halign "left"`, `valign "top"` and an offset of radius+thickness. swaylock also
  nudges the indicator right by `2 / scale` logical pixels; swaywardlock matches it. The e2e test
  checks one row against coordinates measured from swaylock 1.8.6 on the same headless sway, and
  fails on a 2 px error.
- A widget that fails to render is skipped rather than failing the frame, so the fallback colour
  and the other widgets still draw. An unlock (auth success or SIGUSR1) that arrives before
  `locked` is deferred until `locked`, not dropped. Both have e2e tests that fail on the old code.
- PAM is called through `pam-sys2` directly, replacing `pam-client2`. pam-client2 needs the
  response as a `CString`, which left an unzeroed copy of the password in freed heap memory.
  Our conversation function copies the locked buffer straight into the response libpam frees.
- `clear-input-after-ms` must be at least 1000. Smaller values clear the buffer while the user
  is typing, which leaves no way to unlock by password.
- A pending wrong-password timer is cancelled on submit. swaylock has this bug: a check that
  outlasts the 3 s fail timer flips back to idle and accepts input while PAM is still running.
