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
| D8 | Screenshots: wlr-screencopy first, ext-image-copy-capture second, colour fallback | wlr-screencopy covers swayward, sway and niri. ext-image-copy-capture covers newer compositors. Both give the frame in buffer order; it is turned upright with the output's transform (wlr) or the frame's `transform` event (ext) |
| D9 | Output names and keyboard layout come from Wayland, `$SWAYSOCK` is not used | `wl_output` v4 names and xkb layout names work on every compositor. `$SWAYSOCK` would add nothing |
| D10 | `panic = "abort"` | A panic must never run an unlock path. The protocol keeps the session locked when the client dies |

D9 drops the optional `$SWAYSOCK` support from the feasibility study:
everything it offered already comes from Wayland.

## Command line

```
swaywardlock [OPTIONS]
swaywardlock validate [-c PATH]
swaywardlock completions <SHELL>   # bash, zsh, fish, elvish or powershell
```

| Flag | Source | Meaning |
|---|---|---|
| `-c, --config PATH` | both | Config path. Also `SWAYWARDLOCK_CONFIG`. Flag wins |
| `-f, --daemonize` | swaylock | Fork into the background once locked |
| `--ready-fd FD` | swaylock | Write `\n` to FD once locked, then close it. FD must be open at start, or exit 1 |
| `-g, --grace SECONDS` | hyprlock | Any key, pointer button or motion unlocks within this window, without auth. Time asleep counts (`CLOCK_BOOTTIME`); if that clock cannot be read there is no grace |
| `--no-fade-in` | hyprlock | Skip the fade-in animation |
| `--immediate-render` | hyprlock | Lock without taking screenshots or decoding images; backgrounds and images fall back to their colour |
| `--display NAME` | hyprlock | Wayland display to connect to |
| `--preview` | new | Draw in an xdg-shell window instead of locking; reload on config change (see below) |
| `--preview-output NAME` | new | With `--preview`, the output name the window matches as |
| `-v, --verbose` / `-q, --quiet` | hyprlock | Log level. `RUST_LOG` also works |
| `-V, --version`, `-h, --help` | both | |

`completions` prints the script to stdout, exits 0, and never reads the config or connects to
Wayland, so it works with a broken config. `contrib/check-man` (run by the gate) compares
`docs/swaywardlock.1.scd` with `--help`: every flag and subcommand must be in the man page and
the page must not name a flag the binary lacks.

Ordering (swaylock's, kept exactly): lock, wait for `locked`, write the ready
fd, then daemonize. Whoever reads the ready fd or waits on the parent's exit
knows the screen is locked.

Signals: SIGUSR1 unlocks (fade out, then unlock); a second SIGUSR1, password
or fingerprint match during the fade-out unlocks at once. SIGUSR2 forces `cmd[]`
labels, reload-enabled images and backgrounds to refresh now. SIGTERM,
SIGINT and SIGHUP keep their default action: the process dies and the
compositor keeps the session locked. They never unlock. This is swaylock's
behaviour, and it lets a service manager stop a stuck locker. A handler
kills the `cmd[]` children's process groups first and then re-raises the
signal, so they do not outlive the locker (or a Ctrl+C'd `--preview`).

Exit codes:

| Code | When |
|---|---|
| 0 | Unlocked after auth, grace, or SIGUSR1 |
| 1 | Config error, missing or unreadable PAM service file, missing `ext-session-lock-v1`, EGL/renderer setup failure, a `--ready-fd` that is not open, a `--grace` too large for the clock, an open-file limit (`ulimit -n`) under 64 even after raising the soft limit: never locked |
| 2 | The compositor sent `finished` (another locker holds the lock, or the lock was refused). swaylock uses the same code |
| 3 | An error after `lock()` (the compositor died, a protocol error from the compositor, an event-loop error). The compositor keeps the session locked |

SIGUSR1 and SIGUSR2 are blocked as the first thing `main` does. One that arrives before that
(between `exec` and the first line of `main`, a few milliseconds) still has its default action
and ends the process before it locks (exit status 138 or 140 from the shell). Nothing is
unlocked, but a launcher that signals the locker right after starting it should wait for
`--ready-fd` or for `-f` to return first.

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
`/etc/swaywardlock/config.kdl`, then a compiled-in default. When none of the two
default paths exists, the compiled-in default config applies (a plain swaylock-style
look). A file named explicitly, by `--config` or `SWAYWARDLOCK_CONFIG`, that is
missing is an error (`error reading`, exit 1), and so is a path that cannot be read;
an empty `SWAYWARDLOCK_CONFIG=` is a command-line error (clap, exit 2). Errors abort with exit 1 before locking: a lock screen that fails to
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
  swayward's dual-Kawase blur. `passes 0` is no blur, as hyprlock's `blur_passes 0`
  (swayward draws one pass). `saturation` and `noise` apply after the blur with swayward's
  `postprocess.frag` maths (saturation mixes with the Rec. 709 luma; noise adds
  `(hash12(pixel) - 0.5) * noise`); `saturation 1; noise 0;` leaves the blurred image as it is.
- **Shadow:** `shadow { softness 30; spread 5; offset x=0 y=5; color "#00000070"; }`.
- **Animation:** `{ duration-ms 200; curve "ease-out-cubic"; }`, or
  `curve "cubic-bezier" a b c d`, or `spring damping-ratio=1.0 stiffness=800 epsilon=0.001`
  (the damping ratio must be written as a float).
  `off` disables one. Every animation may take at most 3 s (a spring's settle
  time counts); longer is a config error, since the fade-out runs before every
  unlock and a running field animation redraws every output at 60 Hz. A
  `cubic-bezier`'s y values must lie in -2..3 (x in 0..1, as in swayward), and the
  input field's and the swayward ring's animated values are clamped to that much overshoot.
- **include:** `include "path"` and `include optional=true "path"`, with
  the same recursion and error rules. On top of swayward's: one config reads at most 100
  files through `include`, and a file nested more than 32 levels deep (`{` or `/*`) is a
  config error; both used to hang or overflow the stack before locking.

hyprlock's background `contrast` and `brightness` do not exist in swayward's
blur. They are added to the swaywardlock `background` node (next section)
and are not part of the shared vocabulary. So are `vibrancy` and `vibrancy-darkness`.

### Top-level nodes

A flag node (`hide-cursor`, `idle-visible`, `xray`, `markup`, `fade-on-empty`, ...) takes no
argument (true) or one boolean, `true` or `false`. A number, a string, a second argument, a stray
property or a child block is a config error; hyprlock's `0`/`1` spelling is not accepted, and
contrib/hyprlock-to-kdl writes the boolean. Of the flags only `fade-on-empty` takes a property,
`timeout-ms`.

```kdl
general {
    hide-cursor
    ignore-empty-input
    fail-timeout-ms 3000        // how long the fail state shows (the default)
    clear-input-after-ms 10000  // swaylock clears a stale buffer after 10 s
}

auth {
    pam service="swaywardlock"  // pam { off; } disables it (pam "off" also works)
    // pam { reuse-password; }  // answer a repeated identical question with the last answer
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
    input-field-fade   { duration-ms 300; curve "ease-out-cubic"; }  // fade-on-empty in and out
    input-field-width  { duration-ms 300; curve "ease-out-cubic"; }  // growing for long text
    input-field-colors { duration-ms 300; curve "linear"; }          // outline and inner colour
    input-field-dots   { duration-ms 200; curve "linear"; }          // a dot appearing
    ring-colors    { duration-ms 250; curve "ease-out-cubic"; }      // ring style "swayward" only:
    ring-highlight { duration-ms 180; curve "ease-out-cubic"; }      //   state colour crossfade,
    ring-appear    { duration-ms 250; curve "ease-out-cubic"; }      //   highlight sweep, fade in/out,
    ring-wrong     { duration-ms 450; curve "linear"; }              //   wrong shake and flash
}
```

The four `input-field-*` animations follow hyprlock's `inputFieldFade`, `inputFieldWidth`,
`inputFieldColors` and `inputFieldDots`; the defaults are shorter than hyprlock's 800 ms. Colours
blend in OkLab (linear alpha); a new target restarts from the colour on screen. Dots are counted
fractionally: the dot being added fades in, and past the field's capacity the oldest fades out
while the rest stay centred. While a password is being checked and `check` has no text, the dots
hold. The field grows to fit the placeholder, check or fail text while empty and returns to its
size on the first key. The shadow is not drawn while the width animates. A field animation
never delays an unlock and never causes one: the redraw timer stops following fields once a
fade-out is pending, and the fade-out alone ends in `finish_unlock`. There is no `fractional-scale` option: fractional scale is
used whenever the compositor offers it (hyprlock's `fractional_scaling` only meaningfully offers
"off").

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
| `rotate` | number, degrees | `0` | On `label`, `image` and `shape` only (hyprlock's `rotate`) |

`position` uses logical pixels. Positive `y` moves up, as in hyprlock. A widget
`size` (shape, input-field) is 0 or more px, or 0% to 1000%; a negative size is a config
error. With
`halign`/`valign "none"` the position alone places the widget, measured from
the left / bottom edge. A label's shadow is its own `text-shadow` block (see `label`); ring
shadows are not planned.

`rotate` turns a label, image or shape counter-clockwise by that many degrees about its centre,
as in hyprlock. It takes -3600 to 3600 and is normalised to 0..360; on any other widget it is a
config error. As in hyprlock's `posFromHVAlign`, against a `left`, `right`, `top` or `bottom`
edge the turned widget's bounding box touches the edge; centred and `none` axes place the
unrotated box. An image or shape is drawn unrotated, with its border and shadow, into a texture
and that texture is turned (so the shadow turns with it); hyprlock keeps one pixel of margin
around that texture for antialiasing, and so do we, so a shape against the left edge starts at
x 1. A label turns its text texture. Edges are antialiased at any angle (bilinear filtering of
the turned texture). `rotate 0` draws exactly as without it. hyprlock ignores `rotate` on an
`xray` shape; so does swaywardlock, with a warning.

### Widgets

**`background`**: one per output; the last matching one wins.

```kdl
background output="DP-1" {
    color "#111111"            // under everything; shown if nothing else loads
    screenshot                 // or: path "~/wall.png" (see "Image paths")
    fit "cover"                // cover | contain | fill | tile | center (swaylock's scaling modes)
    blur { passes 3; offset 3.0; noise 0.0117; saturation 1.0; }
    contrast 0.8917
    brightness 0.8172
    vibrancy 0.1686            // 0 to 10, default 0 (off); hyprlock's default is 0.1686
    vibrancy-darkness 0.05     // 0 to 1, default 0
    reload-cmd "~/bin/wallpaper-path"   // picture changes while locked (see "Reloading")
    reload-ms 60000
    crossfade-ms 1000
}
```

`vibrancy` is hyprlock's saturation boost of the blurred colours (its FRAGBLUR1): colours that
are already bright and saturated gain the most, and `vibrancy-darkness` boosts dark colours
less. Order after the blur: vibrancy, then the blur's `saturation`, then its `noise`. All three
need blur (`blur` with `passes` above 0), as in hyprlock and swayward; without it they do
nothing and the locker logs a warning.

**Reloading** (`background` and `image`): the picture can change while the session is locked.

- `reload-cmd "<shell>"`: run with `sh -c`; its output (first line, NULs and surrounding white
  space dropped, a `file://` prefix dropped, `~` expanded, a relative path taken against the
  directory of the config file) is the path to show. An empty output, or a command that fails,
  keeps the picture. A `background` with a command and no `path` runs it once at startup for
  its first picture.
- `reload-ms <ms>`: run the command (or, without one, look at the widget's own `path`) every
  that many ms; at least 100. `0` means only on SIGUSR2. Without it nothing reloads on a timer.
  SIGUSR2 reloads every widget that has either key at once.
- A reload decodes a file only when its path or modification time differs from the last one
  looked at, and skips a round while the previous decode is still running. Writing a new file
  and renaming it over the old one avoids a half-written read.
- `crossfade-ms <ms>` (`background` only): the new picture fades in over the old one, linearly,
  in that time; absent or `0` swaps at once, and so does `animations { off; }`. A picture that
  arrives while a crossfade runs waits for it to end (as in hyprlock, which skips reloads until
  then), then fades in.
- The command and the decode run off the main thread, so a slow command or a huge file never
  delays input or the unlock. The worker reads the picture's size first and refuses one with
  more than 64 megapixels or a side over the GPU texture limit (the decoder gets the same
  bounds), then scales it down, aspect kept, until it just covers the largest output's buffer
  (`cover`, `contain`, `fill`) or the `image`'s `size` in pixels; `tile` and `center` keep the
  full size. So the main thread only wraps a screen-sized buffer. The first look waits for an
  output to have a size; a larger output later makes the next look decode again.
- A failed reload (a missing, corrupt or refused file, a command that cannot start, a picture
  that cannot be drawn, such as an `image` whose texture would be over the GPU limit) keeps the
  old picture and logs one warning; the file is not tried again until it changes. A new picture
  replaces the old one only once every output has rendered it. Nothing in a reload affects the
  lock or can unlock the session. Commands run like `cmd[]` ones (own process group, output capped at 64
  KiB) and are killed on unlock and on SIGTERM, SIGINT and SIGHUP.
- `reload-*` on a `background { screenshot; }` is ignored with a warning (as in hyprlock).

Deliberate differences from hyprlock: the keys are in ms (`reload_time` is seconds, and
`hyprlock-to-kdl` multiplies by 1000); a `background` without a command also follows its file's
modification time (hyprlock reloads a background only with `reload_cmd`); an `image` is
refreshed by SIGUSR2 at any `reload-ms` (hyprlock skips it for a positive interval); the command
runs off the main thread (hyprlock blocks on it); and the crossfade lasts `crossfade-ms`,
linear (hyprlock parses `crossfade_time` but ignores it, fading with its `fadeIn` animation).

**Image paths** (`background` and `image` `path`): a leading `~` is the home directory; a
relative path resolves against the directory of the file it is written in (as `include`
does), never the working directory, which is `/` once the locker daemonizes (`-f`). A relative
`-c` path is itself taken against the working directory at startup, before `-f`. Formats:
png, jpeg, webp, gif, bmp, tiff, ico and jxl (JPEG XL, Fedora's stock wallpaper), decoded by
content, not extension. AVIF is not supported: it needs the system dav1d library; a file that
fails to decode is logged and the colour shows instead. The EXIF Orientation tag is applied
(jpeg, png, webp, tiff), as swaylock does through gdk-pixbuf, so a phone photo shows upright;
JPEG XL uses its codestream orientation. Palette TIFFs fail to decode in image-rs, as they do
in swaylock's glycin loader.

A widget whose texture would be larger than the GPU's texture limit (`GL_MAX_TEXTURE_SIZE`,
16384 px on llvmpipe; a font size of thousands) is
skipped with a warning, and the rest of the frame still draws. An output over the limit is
locked and logs a warning; its background is rendered at the limit and stretched over the
output, but GL draws nothing past the limit, so the rest of a wider output stays black (on
llvmpipe: past x 16384). A ring larger than the output
covers it with its inside fill (swaylock's `inside` colour), as it would in swaylock.

The ring is redrawn on every key (the highlight moves; with `style "swayward"` on every
animation frame), on the main thread. Its texture is capped at 4096 px across: a ring whose
`2 * (radius + thickness + glow)` times the output scale is larger renders at the scale that
fits and is stretched (blurrier edges). Without the cap a
radius of 4000 took 12 s to type a password into and 1.3 GB of memory.

The ring's geometry uses swaylock's integer arithmetic (`render.c`): radius and thickness in
whole buffer pixels, `radius - thickness / 2` rounding down for an odd thickness, and a buffer
padded to the next multiple of the integer output scale, which shifts the ring a buffer pixel
right at scale 2. A swaylock config converted with `swaylock-to-kdl` matches swaylock 1.8.6
pixel for pixel at scale 1 and 2, for odd and even thickness. `style "swayward"` shares this
geometry; its glow adds whole buffer pixels on each side.

**`label`**

```kdl
label {
    text "$TIME"               // see "Text substitution"
    font "Sans 16"             // Pango font description
    color "#ffffff"
    align "center"             // left | center | right (multi-line; `<br/>` breaks a line)
    position 0 -80; valign "top"   // 80 px below the top edge (positive y moves up)
    rotate 0                   // degrees, counter-clockwise
    text-shadow { passes 2; size 3; color "#000000"; boost 1.2; offset x=0 y=0; }   // off by default
}
```

`text-shadow` is hyprlock's label shadow (`shadow_passes`, `shadow_size`, `shadow_color`,
`shadow_boost`): a copy of the text's shape, blurred, drawn behind the text in the shadow colour.
It is a different block from `shadow { }` on shapes, images and input fields (swayward's box
shadow: softness, spread, offset), which a label rejects. `passes` (0 to 4) is required: a block
without it is a config error, so a block never silently draws nothing; `passes 0` is accepted and
means off, like leaving the block out (hyprlock's `shadow_passes 0`). `size` (0 to 20, default 3)
is the blur offset in logical px, `color` (default black) the shadow colour, `boost` (0 to 10,
default 1.2) multiplies the shadow's alpha, which is then clamped at 1. `offset x= y=` (default
0 0, logical px, positive `y` up as in `position`) moves the shadow; hyprlock has none. Two
extensions to hyprlock: the colour's alpha multiplies the shadow's (hyprlock uses only the rgb),
and the offset. With `rotate`, the shadow turns with the text. The caps are lower than the
background blur's: each pass doubles the shadow's reach, and at 4 passes and size 20 it already
spreads hundreds of px past the text (hyprlock configs use 1 to 3 passes). A shadow that would
need a big texture (a large label, a wide blur, a high scale) is blurred at lower resolution,
which looks the same once blurred.

The blur is the background's dual-Kawase blur (`size` is its offset, as hyprlock's `shadow_size`
is its blur radius), so the shadow is hyprlock's model (mask, blurred, boosted, tinted) at an
approximation: the exact falloff differs from hyprlock's shader, and hyprlock's prepare pass
(contrast and brightness, 0 for shadows) changes only colour, which the tint replaces. The
shadow is rendered when the text changes, padded by `2 * size * 2^passes + 2` px on each side
(the blur reaches 11 px past a 16 px block at size 3 and one pass, 24 at two, 44 at three, 79 at
four); a failure draws the label without its shadow.

**`image`**

```kdl
image {
    path "~/.face"
    size 150                   // shorter side, px (hyprlock's)
    corner-radius "full"       // the default: a circle (hyprlock's rounding -1); or px, or 4 values
    border { width 4; color "#dddddd"; }   // or gradient from=... to=...; around the picture
    rotate 0                   // degrees, counter-clockwise
    reload-ms 30000            // optional; see "Reloading" under `background`
}
```

An image swaps at once when it reloads (no crossfade, as in hyprlock).

**`shape`**

```kdl
shape {
    size 360 60                // px or "%"
    color "#11111180"
    corner-radius 12
    border { width 2; gradient from="#80c8ff" to="#c7ff7f" angle=45; }
    xray                       // no fill, so the background shows (see below)
    rotate 0                   // degrees, counter-clockwise; ignored with xray (hyprlock)
}
```

As in hyprlock, a border goes around `size` (the fill keeps its size and `corner-radius`;
the border's outer radius is the radius plus the border width), and the bordered box is what
`halign`/`valign`/`position` place. The same holds for `image`. `border { width 0 }` is no
border.

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
    hide-input "#00ff99ee"                      // random colour per key instead of dots; colour optional; false = off
    fade-on-empty timeout-ms=2000               // the default; `fade-on-empty false` keeps the field visible
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
    style "swaylock"               // swaylock (pixel for pixel) | swayward (animated)
    // glow 12                     // swayward only: halo px 0..64 (default 12); an error with swaylock
    colors {
        state "input"     inside="#000000c0" ring="#337d00ff" line="#000000ff" text="#e5a445ff"
        state "clear"     inside="#e5a445c0" ring="#e5a445ff" text="#000000ff"
        state "verifying" inside="#0072ffc0" ring="#3300ffff" text="#000000ff"
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

`style "swayward"` draws the same geometry, states and texts with the `ring-*` animations:
colours crossfade in OkLab, the typing highlight sweeps to its new (still random) position with a
fading trail, a comet spins while verifying, a wrong password shakes the ring and flashes it
towards white, and the ring fades and scales in (fades out) when it shows and hides. A glow of
`glow` px surrounds it without moving it. With `animations { off; }` every effect is at its end
state (the glow stays). Design: [`2026-10-20-ring-style.md`](2026-10-20-ring-style.md).

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
| `$PAMPROMPT` / `$PAMFAIL` | PAM's current question: `Password: ` until a transaction has asked one, then the question being asked, and after a failure the first question of that transaction / last PAM failure |
| `$FPRINTPROMPT` / `$FPRINTFAIL` | fprintd prompt / failure |

`cmd[update:MS] <shell>` (hyprlock's syntax, kept unchanged so users can
copy it): substitute the tokens above into `<shell>`, run it with `sh -c`
every MS milliseconds and show stdout, trimmed, as is (NUL characters are dropped, as in every
text Pango draws). `cmd[update:MS:1]` also refreshes on SIGUSR2. Text that starts with `cmd[`
but has no `]` is plain text, as in hyprlock. Commands run as the
user with the user's environment, with an empty signal mask and no
descriptors from the locker beyond stdin/stdout/stderr. They run on a worker
thread, so a slow command never blocks rendering. At most 64 KiB of output is
read (a command that writes more is killed) and at most 100 lines are shown;
any label's text is cut to 100 lines, because Pango lays it out on the main
thread. Every line break Pango honours counts: LF, CR, CR LF (one break),
U+2028 and U+2029, and in markup also character references such as `&#13;`
(markup text cut that way is shown without its markup). Pango's cost also grows with the TABs
in one paragraph, so after the first 256 TABs a label draws the rest as spaces. A command still running when the next tick comes is not started
again; a job it left in the background (`daemon & echo started`) counts as running. All children,
background jobs included, are killed on unlock and when a signal ends the locker.

Labels that contain `$TIME` look at the wall clock every second and redraw when the minute
changes, so the clock is right within a second of a resume (a minute timer counts monotonic
time, which stops during suspend).

## Architecture

One process, one calloop event loop on the main thread, two kinds of worker
thread (auth and commands/resources). Modules:

| Module | Job | Depends on |
|---|---|---|
| `main` | clap args (`validate`, `completions`), the `App` state and its sctk handlers: registry, outputs, seat/keyboard/pointer, session lock, startup and unlock | clap, sctk, wayland-protocols |
| `config` | KDL types and decoding; `copied/` holds verbatim or trimmed copies from swayward-config | knuffel, miette |
| `egl` | `EGLNativeDisplay` for `wl_display`; one `EGLDisplay`, `EGLContext` and `GlesRenderer`, plus one `WlEglSurface` + `EGLSurface` per lock surface | smithay, wayland-egl |
| `surface` | one lock surface (or the preview window): widget layout from properties and output size, cached textures, the `OutputDamageTracker`, drawing; also the decoded `Images` | `render`, `config` |
| `render` | copied render helpers (shaders, border, shadow, blur, solid colour, texture, memory buffer) and our own widget drawing (ring, input field, shape, background, Pango text) | smithay |
| `scale` | `wp_fractional_scale_v1` and `wp_viewporter` | sctk |
| `animation`, `fade` | copied clock, bezier and spring; the fade in and out and the redraw ticker | none |
| `auth` | one PAM transaction per submitted secret on a worker thread; results come back over a calloop channel (`AuthEvent`) | pam-sys2 |
| `fprint` | fprintd and logind over zbus on a worker thread | zbus |
| `state` | input and auth state machine with swaylock's timers, grace period (pure logic, unit tested) | none |
| `secret` | the locked, zeroed password buffer | libc |
| `textvars`, `commands`, `labels` | `$VARIABLE` and `cmd[]` substitution, the command runner, label refresh timers | none |
| `reload` | `reload-ms`/`reload-cmd` timers, off-thread command and decode, the path each picture shows | none |
| `screencopy` | wlr-screencopy screenshots before locking | sctk |
| `preview` | `--preview` window and config watcher | sctk |
| `process` | ready-fd, daemonize, open-file limit | libc |

### Startup sequence

1. Parse args and config. Error → exit 1. Unless PAM is off, the PAM service file must be one
   libpam reads: the service name lowercased (as `pam_start` does), the first of
   `/etc/pam.d/<service>` and `/usr/lib/pam.d/<service>` that this process can open. Without
   it Linux-PAM uses `/etc/pam.d/other`, which denies on Fedora and Arch, so no password could
   unlock. Missing or unreadable (installed mode 600 root) → an error naming the file, exit 1,
   never locked.
2. Connect. Bind globals. If `ext_session_lock_manager_v1` is missing,
   exit 1.
3. Unless `--immediate-render` is set: take a screenshot of each output that
   any `background` asks for (wait at most 2 s, then go on with whatever has
   arrived), and decode images. Decoding runs on the main thread before
   `lock()` with no time limit, as in swaylock; a huge image delays the lock.
   hyprlock does the same for screenshots: they must be taken before locking,
   because the compositor shows only lock surfaces afterwards.
4. Create the EGL display, context and renderer. Compile the copied
   shaders (`shaders::init`, `resources::init`). Check that libwayland-egl
   loads, and create and drop an EGL window surface on a scratch
   `wl_surface`. Error → exit 1. This comes before `lock()`, so a GL
   failure never leaves a locked session showing nothing.
5. Call `lock()`. Create a lock surface for each output. Render the first
   frame on each configure.
6. On `locked`: write the ready fd, daemonize, start the grace timer. The
   SIGUSR1 and SIGUSR2 handlers are installed before `lock()` (SIGUSR2 also in
   `--preview`); a SIGUSR1 before `locked` is deferred until then.
7. Run until unlock.
8. Unlock: fade out, `unlock_and_destroy`, drop the lock surfaces (nothing
   draws after the unlock), roundtrip, kill children, exit 0.

On `finished` at any point: exit 2. The lock object is answered with
`unlock_and_destroy` if `locked` came first and `destroy` otherwise, as the
protocol asks; the compositor has already ended that lock, so this unlocks
nothing.

The event loop is never dropped: calloop unblocks SIGUSR1/2 when its signal
sources drop, and a signal pending then would kill the process after the
unlock with 138/140 instead of exit 0.

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
  No `wl_surface.frame` callback is requested. Redraws are event driven: a frame is drawn when
  a configure, an input or auth event, a resource or a command output arrives, and a 16 ms
  timer (`fade::ensure_ticking`) redraws while an animation runs. Every EGL surface uses
  swap interval 0: with Mesa's default of 1, `eglSwapBuffers` waits for the
  previous frame callback, which a lock surface on a powered-off output never
  gets, and the main thread (keyboard, auth, SIGUSR1) blocked until the output
  woke. A disabled or unplugged output loses its lock surface; with no lock
  surface left the compositor has nowhere to send keys, so only SIGUSR1 (or
  re-enabling the output) unlocks.
- Scale: `wp_fractional_scale_v1` with `wp_viewporter` when the compositor
  has both, otherwise the integer `wl_surface` preferred buffer scale. With
  fractional scale the `WlEglSurface` is `round(logical * scale)` pixels,
  the buffer scale is 1 and `wp_viewport` sets the logical size; the integer
  preferred scale is ignored then (at 1.5 sway also sends 2). A compositor sends the preferred
  scale only after a surface's first commit, so until then the surface uses its output's
  `wl_output.scale`: the first frame on a HiDPI output is not drawn at scale 1.
- `background` post-processing: `contrast` and `brightness` run on the CPU over the decoded
  image or screenshot (`src/render/background.rs` `prepare()` before the blur and `finish()`
  after it, a per-channel lookup table after hyprlock's FRAGBLURPREPARE and FRAGBLURFINISH),
  once per background when its texture is built. Vibrancy, the blur's saturation and noise run
  after the blur in one fragment shader (`src/render/shaders/postprocess.frag`, hyprlock's
  vibrancy plus swayward's postprocess maths), drawn from the blurred texture into a new one,
  once per background render (configure, scale change); the result is the cached background
  texture.
- Fade in and out: a black overlay over the whole output at `1 - opacity`, so the lock
  screen fades in from black and out to black. A compositor may send `locked` only after it
  has shown our first frame (at full opacity); then there is no fade-in, since it would flash
  the lock screen to black first.

### Auth

```rust
/// auth.rs: a worker thread that runs one PAM transaction per submitted secret.
struct Authenticator { /* mpsc::Sender<SecretBuf> */ }
impl Authenticator {
    fn spawn(service: String, reuse_password: bool, events: Sender<AuthEvent>)
        -> anyhow::Result<Self>;
    fn submit(&self, secret: SecretBuf);
}

enum AuthEvent { Success, Failure(String), Prompt { text: String, follow_up: bool } }
```

fprintd has its own channel and event type (`fprint::FprintEvent`, only `Matched` unlocks).

- PAM runs in a dedicated thread. A transaction starts only when the user submits, as in
  swaylock; nothing runs while the screen is idle. (hyprlock starts the next transaction as soon
  as the last ends, so it knows the first question early. swaywardlock did too, and that took
  the stack's pre-prompt verdicts at the wrong time: pam_faillock's preauth, decided right after
  a locking failure, refused the first right password once the lockout had expired; an account
  locked from elsewhere while the screen sat idle was still accepted; and a pam_fprintd-first
  stack consumed a touch on the idle screen and threw the match away.) So every check is decided
  when Enter is pressed, and a stack that succeeds without asking anything (pam_permit, an
  empty password under nullok) still needs a submit. On a stack with pam_fprintd first (Fedora
  authselect `with-fingerprint`, pulled in by `auth include login`) the order is Enter, then
  touch, as with swaylock; `auth { fingerprint }` talks to fprintd directly and unlocks on a
  touch at any time.
- `$PAMPROMPT` is `Password: ` until a transaction asks something. Every question, hidden or
  visible, is sent as `AuthEvent::Prompt`. The submit that started the transaction answers the
  first question; every later one, even with the same text, is a `follow_up`: it ends the
  checking state, the input field shows `$PAMPROMPT` instead of its placeholder, and the
  conversation blocks on the next `submit`. So a password-then-one-time-code stack works, also
  when the code's prompt is "Password: " (pam_radius_auth's default), and a secret never goes
  to a question it was not typed for. After a failure `$PAMPROMPT` goes back to the first
  question of that transaction. `auth { pam { reuse-password; } }` (off by default) answers a
  question asked again with the same text from the previous answer, once, for stacks with two
  modules that each ask for the password (hyprlock does this always). A `PAM_TEXT_INFO`
  containing "left to unlock" (pam_faillock's lockout notice) becomes the failure text, as in
  hyprlock; other messages are logged. Nothing aborts a transaction by failing the
  conversation, which pam_faillock would count as a failed attempt.
- Only the auth phase runs: `pam_authenticate`, then `pam_setcred(PAM_REFRESH_CRED)` on success
  to renew Kerberos tickets and the like (swaylock does the same; its result is logged and does
  not decide the unlock). There is no `pam_acct_mgmt`, as in swaylock and hyprlock. The account
  phase decides whether a new login may start; a locker re-authenticates a session that is
  already running, and an account failure there refused the right password for as long as it
  lasted: `pam_nologin` (systemd creates `/run/nologin` 5 minutes before a scheduled shutdown)
  or an expired password. The cost: an account the admin expired (`chage -E`) or one outside its
  `pam_time`/`pam_access` window can still unlock its existing session, as with swaylock and
  hyprlock. A locked password (`passwd -l`) and a pam_faillock lockout are auth-phase checks and
  still refuse. `pam/swaywardlock` is therefore just `auth include login`, the file swaylock and
  hyprlock ship. swaywardlock enforces whatever the system's auth stack decides: an account
  with no password under the distro's `pam_unix nullok` unlocks with any input, as it does with
  swaylock and hyprlock.
- fprintd: `net.reactivated.Fprint.Manager.GetDefaultDevice`, `Claim`,
  `VerifyStart("any")`, the `VerifyStatus` signal, retry after
  `retry-delay-ms`. It listens to logind `PrepareForSleep` to stop scanning
  before sleep and start again after. Its presence is optional: if there is no
  fprintd or no device, log a warning and go on with PAM only.
- Whichever source succeeds first wins. Every unlock goes through one
  function, `request_unlock()`; it starts the fade out, or calls `finish_unlock()`
  straight away, and `finish_unlock` is the only caller of `lock.unlock()`
  (`unlock_and_destroy`). The fade out calls `finish_unlock` when it ends.
- `SecretBuf` is `mlock`ed (with a warning when that fails, like
  swaylock), zeroed on drop (`zeroize`), and never `Debug`- or
  `Display`-printable. It is cleared after `clear-input-after-ms` of no
  input.

### Input

- Keys: compose-aware via sctk. Enter submits (dropped if the buffer is
  empty and `ignore-empty-input` is set), and so do swaylock's Ctrl+J, Ctrl+M and Ctrl+D (Ctrl+D
  submits at once; it is not a delete). Backspace and Delete remove one
  character and repeat. Escape, Ctrl+U, Ctrl+C (swaylock's), Ctrl+Backspace, Ctrl+Delete and Ctrl+A
  (hyprlock's) clear. Other keysyms append their UTF-8.
- Key repeat: a client-side repeat timer of our own per keyboard (`Repeater`), at that
  keyboard's `repeat_info` rate and delay (sway sends 25/s after 600 ms on `wl_seat` v9). It
  stops on key release, keyboard leave, the keyboard being released (capability change) and
  seat removal, so a key held while its keyboard goes away does not keep typing. A
  `wl_keyboard` v10 compositor that repeats keys itself first sends a rate of 0, which turns the
  timer off, so a key never repeats twice. Only Backspace and Delete repeat, as in hyprlock. swaylock repeats every key,
  so there a letter held a little too long types extra characters into the password.
- While a check is running, keys are dropped, as in hyprlock. swaylock keeps them: a password
  typed ahead after a wrong one is still there when the check ends. Dropping is deliberate; the
  field shows the checking state, and type-ahead into a buffer that is about to be cleared on
  failure is easy to get wrong.
- Seats: the keyboard and pointer of every seat are bound, as in swaylock, and each belongs to
  its seat. A seat losing its keyboard releases only that seat's keyboard; a keyboard added to
  any seat while locked is picked up. Ctrl is tracked per keyboard.
- Caps/Num lock state and the active layout are kept per keyboard, from that keyboard's xkb
  state after each of its `modifiers` events. `$LAYOUT`, the ring's layout box and the Caps
  Lock text and indicator show the keyboard that last sent a key or modifiers among those still
  present; when it goes away they fall back to a remaining keyboard, or empty. A new keymap redraws too: the first frame is usually drawn before the
  keymap arrives, and with layout 0 active no `modifiers` event follows, so `$LAYOUT` and the
  ring's layout box would stay empty until the first key.
- Grace period: until it ends, any key press, pointer button or motion calls
  `request_unlock()`. Touch is not handled.
- `hide-cursor`: set a null cursor on pointer enter. Otherwise no cursor is
  set and the compositor draws its own; cursor-shape is not used.

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
- Output: KDL on stdout. The command line to use (`swaywardlock -f`, with styling
  flags removed) goes to stderr, with the warnings.
- Maps `color`/`image`/`scaling`/`tiling` to `background`, and every
  indicator, ring, line, text, inside, layout and highlight flag to
  `ring-indicator`. Maps `ignore-empty-password` to `general`.
- Long options may be abbreviated to any unambiguous prefix, on the command line and in a
  config line, as swaylock's getopt_long accepts (`--daemon`, `indicator-idle`); an ambiguous
  prefix (`--line-uses`) is skipped with a warning naming the candidates.
- Unknown keys are skipped with a warning on stderr; no comment is written to the
  KDL. The tool never silently drops a key. Values swaywardlock's decoder would
  reject but swaylock accepts (`--indicator-radius 0`, `--indicator-thickness 0`,
  a negative `--font-size`, out-of-range positions) are clamped to the nearest
  accepted value with a warning, since a config that fails to load never locks.
  Relative `--image` paths are made absolute (swaywardlock may chdir to `/`), and an
  image path without a supported extension (png, jpg, jpeg, webp, gif, bmp, tif, tiff, ico,
  jxl) draws
  a warning.
- swaylock-effects options have no KDL; each is reported with its swaywardlock counterpart.
  `--grace N` is carried over to the command line as `-g N` (skipped with a warning when N does
  not fit swaywardlock's u64).
- A corpus test runs each sample through the converter and then through
  `swaywardlock validate`.

`contrib/hyprlock-to-kdl [CONFIG]` (Python, stdlib only): the same approach, with the same
stdout/stderr split and the same clamping of values the decoder rejects.

- Input: a hyprlock.conf, or hyprlock's search path when none is given. It reads hyprlang's
  syntax: nested sections, `$var = value` substitution (longest name first, plain substring),
  `{{ a + b }}` expressions, `source =` (globs, `~`, relative to the including file, loops cut),
  `# hyprlang if/endif`, `\` continuations and `##` for a literal `#`.
- Output: each `background`, `shape`, `image`, `input-field` and `label` block becomes the matching
  widget, in hyprlock's draw order (types in that order, file order within a type, `z-index`
  only where it differs from the type's default). `general`, `auth` and `fingerprint` map
  onto `general` and `auth`; `general:immediate_render` becomes the `--immediate-render` flag on
  the stderr command line. `bezier`/`animation` lines are resolved through hyprlock's animation
  tree (`global` > `fade` > `fadeIn`/`fadeOut`, `global` > `inputField` > the four input-field
  leaves) into `fade-in`, `fade-out` and the `input-field-*` animations, with speed x 100 ms as
  the duration and an identity bezier as `linear`.
- hyprlock defaults that differ from ours are written out: `hide-cursor false`,
  `fail-timeout-ms 2000` (ours 3000), the background colour, `align "left"` on labels, the default
  image border, 800 ms fades, and with `blur_passes > 0` (as in hyprlock) the blur offset 8
  (`blur_size`; ours 3), the noise, contrast/brightness 0.8917/0.8172 and vibrancy/
  vibrancy-darkness 0.1686/0.05, plus `saturation 1` in the blur (hyprlock has no blur
  saturation; ours defaults to swayward's 1.5). Two stay ours:
  `clear-input-after-ms` (hyprlock has no such timer and never clears a typed password; ours clears
  it after 10 s, and the decoder accepts no value that turns it off), and the `input-field-*`
  animations (see below).
- `capslock_color`, `numlock_color` and `bothlock_color` follow hyprlock 0.9.6: an empty value is
  its fallback (nothing is written), and `-1` has no special meaning (it is the integer
  0xFFFFFFFF, opaque white, like any other colour number), so it is written as `#ffffffff`.
- hyprlock text is never copied raw: every comment, string and stderr message goes through one
  escape that writes control characters (CR, VT, FF, NEL and the rest of C0/C1), U+2028, U+2029 and
  bidi controls as `\u{hex}`. KDL ends a line on CR, FF, NEL, U+2028 and U+2029, so one of them in a
  `// hyprlock:` comment would end it and let a crafted hyprlock.conf inject nodes (for example an
  `auth { pam "off"; }`). In strings the escape keeps the value, so `swaywardlock` sees the same
  text. Lines are split on LF only, as hyprlang does: a lone CR stays inside its value.
- A file or directory name (or environment variable) that is not UTF-8 reaches the converter as
  lone surrogates, which KDL cannot hold, not even as `\u{dcff}`. A widget path containing one is
  refused with a warning naming the bytes (`\xFF`) and "rename it": an image widget is skipped,
  a background keeps its default look. In comments and warnings the bytes are written as plain
  ASCII (`\xFF`).
- Gradient angles are hyprlock degrees + 90, normalised mod 360: the closest CSS equivalent
  (hyprlock's 0deg runs left to right and its shader angle is not geometric, so only multiples of
  90 are exact). Gradients with more than two stops keep the first and last colour, with a warning.
- Animation defaults: `fade-in` and `fade-out` are always written (hyprlock's default is 800 ms
  with its default curve, ours 200 ms); the `input-field-*` animations only when an explicit
  `animation` line resolves to them. An animation over 3 s is clamped to 3000 ms and a bezier y
  outside -2..3 is clamped into it (the decoder's limits), each with a warning.
- Paths: `~` and `$ENV` are expanded. A relative widget path becomes absolute against the
  directory of the conf file it is written in. This is a deliberate divergence: hyprlock resolves
  widget paths against the working directory at lock time (only `source =` uses the conf's
  directory), but swaywardlock may chdir to `/`, and its decoder resolves a relative path against
  the KDL file.
- `auth:pam:module = hyprlock` (hyprlock's default service) is omitted with a note, so the shipped
  `/etc/pam.d/swaywardlock` is used; any other module becomes `pam service=`. `enabled = false`
  becomes `pam "off"`, with a warning unless the fingerprint is enabled.
- Unlike `swaylock-to-kdl`, which only warns, every key that has no KDL counterpart gets a
  warning on stderr and a `// hyprlock: key = value (reason)` comment inside its node, so the
  KDL shows what was left behind: `onclick`, `shadow_boost`, and
  `general:text_trim`, `fractional_scaling` and `screencopy_mode`, plus unknown keys and sections.
  The handlers live in one table (`UNSUPPORTED`); the unit that adds a KDL key moves it from the
  table to a translator and extends the converter's tests. Keys hyprlock itself ignores (blur
  settings without `blur_passes`, `shadow_size` without `shadow_passes`) warn without a comment.
  Shadows on shapes, images and input fields become an approximate `shadow` node
  (softness = `shadow_size` x `shadow_passes`), with a warning. `rotate` becomes `rotate`,
  normalised to 0..360; on an `xray` shape (which hyprlock does not rotate) it stays a comment.
- Tests: `contrib/test-hyprlock-to-kdl.py` (every hyprlock key either changes the output or is
  named on stderr; every output goes through `swaywardlock validate`), the Rust test
  `hyprlock_converter_reproduces_the_example` (the converted hyprlock `example.conf`, kept as
  `contrib/fixtures/hyprlock-example.conf`, decodes to the same `Config` as
  `examples/hyprlock-example.kdl`), and the `hyprlock-converted` e2e check.

## Testing

| Layer | What | How |
|---|---|---|
| Config | every node decodes; defaults; errors carry spans | unit tests with assertions on the decoded values (no snapshot tests) |
| Converters | corpus round trip | Python test + `swaywardlock validate` |
| Text | each substitution token, `cmd[]` parsing | unit tests |
| Input/auth | key handling (Escape, control characters, modifiers, Ctrl shortcuts, ignore-empty-input), timers, grace (`state.rs`) | unit tests; there is no `FakeAuth`, the `test-auth` build feature swaps PAM for a fixed password |
| Real PAM | `src/auth.rs` against libpam: wrong and right password, auth phase only, follow-up questions, faillock text, setcred | unit tests with throwaway service files through `pam_start_confdir` and the probe module `contrib/test-pam-probe.c`; `contrib/test-real-pam` (in the gate) installs a throwaway `/etc/pam.d/swl-test-<pid>` (`pam_exec` script, then `account required pam_deny.so`) so the shipped `pam_start` lookup runs too; it needs root or passwordless sudo and says SKIP otherwise. `SWAYWARDLOCK_TEST_REAL_PASSWORD=<pw> contrib/test-real-pam` also checks a real password through `auth include login` (opt-in: it needs a user with a throwaway password) |
| Process | ready-fd ordering, daemonize, SIGUSR1, exit 2 on `finished` | integration |
| End to end | lock, wrong password, right password, screenshots, widgets by pixel sampling (`contrib/e2e-sway`, moving to the pytest suite `tests/e2e`). Every unlock path checks that the compositor unlocked (a `swaybg` desktop is visible again), not only the exit code. Also: `pam { off; }` with Enter, an unlock before `locked` (a proxy holds `locked` back), outputs plugged and unplugged while locked, scale changes, fade direction, blur and brightness, the fprintd call order and retry delay, pointer grace and hide-cursor, the daemon's session and stdio | headless sway (`WLR_BACKENDS=headless`, `WLR_RENDERER=pixman` or llvmpipe), `wtype` for keys (`contrib/vkbd` for a chosen seat or a multi-layout keymap, which wtype cannot do), `contrib/vptr` for a pointer, the `test-auth` build feature |
| Visual | the ring against swaylock's pixels | one e2e check (`converted`) compares a row measured on swaylock 1.8.6; there are no reference PNGs and no offscreen render tests |

The test password exists only behind the cargo feature `test-auth`, which release builds don't
enable. The gate checks that a release build does not contain it. Not done: tests on swayward
itself, and snapshot or offscreen-render tests.

Gate: `contrib/gate` runs nightly fmt, clippy, `cargo test`, `contrib/test-real-pam`, the
converter corpus and the headless-sway end-to-end runs: `contrib/e2e-sway`, then the pytest
suite in `tests/e2e` (`pytest tests/e2e -n auto`; one area: `pytest tests/e2e -k password`),
which the bash tests move to area by area (`2026-10-21-e2e-pytest.md`).

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
and the ring matches swaylock's pixels on the one measured row (the `converted` e2e check).

**Phase 2: hyprlock parity.** `label`, `image`, `shape`, `input-field`,
text substitution, `cmd[]`, screencopy, blur and post-processing,
animations and fades, grace, per-output widgets, fprintd. Exit criterion:
the converted examples from hyprlock's wiki render.

**Phase 3: rest.** ext-image-copy-capture (done), input-field animations and fade-on-empty
(done), the animated ring style (done), `rotate` (done), `hyprlock-to-kdl` (done; keys without KDL stay comments until their units land),
`reload-cmd`/crossfade (done), packaging (PKGBUILD, rpm spec, pam file) and man page (done),
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

## Phase 3 notes (2026-10-19)

- Fractional scale: lock surfaces and the preview window bind `wp_fractional_scale_v1` and
  `wp_viewporter` once, at creation, and redraw when `preferred_scale` changes after a configure.
  The buffer is `round(logical * scale)`, as wlroots sizes the output (213x160 at 1.5 is
  320x240). Before, sway at 1.5 got a scale-2 buffer and downsampled it, so edges blurred: a
  100 px shape's right edge at physical x 150 read `66 54 81` instead of the background. The
  ring's swaylock nudge uses `2 / ceil(scale)`, the integer scale swaylock sees. The e2e test
  `fractional` checks the edge at 1.5, a scale change while locked, and the integer fallback
  (test builds hide the globals with `SWAYWARDLOCK_TEST_NO_FRACTIONAL`).

- PAM audit fixes: auth-only (no `pam_acct_mgmt`, no account line in `pam/swaywardlock`),
  `pam_setcred(PAM_REFRESH_CRED)` after success, every prompt answered from the next submit in
  the same transaction (password then OTP; visible prompts), `$PAMPROMPT` set before the first
  attempt, pam_faillock's "left to unlock" notice as the failure text, refusal to lock when the
  service file is missing, and `pam { off; }`. `src/auth.rs` tests run the real libpam on
  throwaway service files (`pam_start_confdir`, test builds only) with stock modules and the
  probe module `contrib/test-pam-probe.c`. Test builds take `swaywardlock-2fa` then `123456`
  for the e2e `otp` test, and check `SWAYWARDLOCK_TEST_PAM_DIR` instead of `/etc/pam.d`.
- PAM audit fixes, round 2: a transaction starts only on submit, as in swaylock (a transaction
  started at lock time or right after a failure applied pam_faillock's preauth verdict from
  then: the first right password after a lockout expired was refused, and a pam_fprintd-first
  stack threw away a touch on the idle screen); a repeated question is asked, not answered with
  the previous secret, unless `auth { pam { reuse-password; } }`; the service file check
  lowercases the name and opens the file, as libpam does. The real-PAM tests use new probe
  modes `codepw`, `locked` (a preauth stand-in) and `finger` (a pam_fprintd stand-in).
- Audit fixes (unlock and rendering): eglSwapInterval(0) per surface (a DPMS-off output wedged
  the locker at the next redraw); libwayland-egl and a scratch EGL window surface checked before
  `lock()` (a broken library panicked after locking); nothing draws after `unlock_and_destroy` (a
  password during a fade-in with fade-out off left the process running forever, blocked in
  `eglSwapBuffers` on the destroyed surface); fades capped at 3 s at validate and a second unlock
  request finishes a running fade-out; `cmd[]` output capped at 64 KiB and 100 lines, labels at
  100 lines (a 50k-line label held input for 18 s); `cmd[]` children get an empty signal mask and
  close-on-exec descriptors, and are killed when SIGTERM, SIGINT or SIGHUP ends the locker. e2e
  tests: `dark-output`, `no-wayland-egl`, `cmd-output`, `sigterm`, two new `fade` checks.
  After review: the line cap counts CR, CR LF, U+2028 and U+2029 as well as LF (64 KiB of
  CR-separated progress output was 32k Pango lines and kept the password unread for over 120 s),
  and markup is cut after parsing; e2e `cmd-output` has a CR case and the new `unplug` test
  unplugs three of four outputs while locked (the password and SIGUSR1 must still unlock).
  Screenshots: wlr-screencopy v3 sends `copy` only after `buffer_done`, as the protocol asks;
  the conversion checks the stride and the mapped length and skips a bad frame with a warning
  instead of panicking.

- Packaging: `contrib/PKGBUILD` and `swaywardlock.spec.rpkg` (swayward's style) build without
  `test-auth`, install `pam/swaywardlock` byte for byte as `/etc/pam.d/swaywardlock` (`backup=` and
  `%config(noreplace)`: a local edit survives upgrades), the scdoc man page
  `docs/swaywardlock.1.scd`, bash/zsh/fish completions, both converters
  (`swaywardlock-swaylock-to-kdl`, `swaywardlock-hyprlock-to-kdl`, hence the `python` dependency),
  `examples/*.kdl` and `docs/specs/*.md` under `/usr/share/doc/swaywardlock` (the man page's
  CONFIGURATION section points there). The PKGBUILD downloads the tag tarball
  (`swaywardlock-v$_tagver`) by default and builds the checkout it sits in with
  `SWAYWARDLOCK_LOCAL=1`; `.gitignore` covers its `contrib/src`, `contrib/pkg` and package files,
  and `BUILDDIR`/`PKGDEST` can point outside the tree. Both
  run the Rust tests but not the headless-sway e2e suite. `swaywardlock completions <SHELL>` is
  implemented with clap_complete (swayward's choice) and handled before the config is loaded.
  `contrib/check-man`, the gate and CI keep the man page in step with `--help`; the gate also runs
  `completions` with a broken config. CI installs scdoc so the page's syntax is checked there.
  `contrib/check-doc-kdl` validates the KDL snippets in the README with `swaywardlock validate`, and
  `contrib/test-try-lock` checks `contrib/try-lock`'s usage errors, `--help`, and that SIGTERM,
  SIGINT and SIGHUP leave no temp dir, no nested sway, no locker and no `--unlock-after` sleep.
  Both run in the gate and in CI.

- Release machinery, following swayward's (`.github/workflows/release.yml`, `contrib/package-version`,
  `check-release-consistency`, `test-release-consistency`, `build-deb`; see COPIED.md). The workflow
  is `workflow_dispatch` with a `version` input and only drafts: validate (tag
  `swaywardlock-vVERSION`, `Cargo.toml`, `Cargo.lock`, PKGBUILD and spec agree), build-ubuntu
  (vendored dependencies archive, build, test, the tarball with `.sha256`, the `.deb`),
  build-rpm (Fedora 43 and 44, in containers, through rpkg), build-arch (`archlinux:latest`,
  makepkg from `git archive` of the tagged commit packed under GitHub's own tag-tarball
  top directory `swaywardlock-swaywardlock-vVERSION`, which is the directory the PKGBUILD's release
  mode builds in, so the workflow exercises the user's path; namcap with no errors), a verify job per
  package that installs it in a clean container and runs `contrib/verify-installed`
  (`--version` equal to the version, `validate` on every shipped example, `cmp` of
  `/etc/pam.d/swaywardlock` with `pam/swaywardlock`, no `swaywardlock-test` or
  `SWAYWARDLOCK_TEST_` string in the installed binary, both converters' `--help`, man page and
  completions installed) plus the package manager's own "marked as config" check (dpkg conffiles,
  `rpm -qc`, pacman backup), and draft-release (`softprops/action-gh-release`, `draft: true`).
  Naming: `swaywardlock_VERSION_amd64.deb`, `swaywardlock-VERSION-fedora43|44-x86_64.rpm`,
  `swaywardlock-VERSION-arch-x86_64.pkg.tar.zst`, `swaywardlock-VERSION-ubuntu24.04-x86_64.tar.gz`.
  The `.deb` depends on `login` (the owner of `/etc/pam.d/login` on Ubuntu 24.04, from `dpkg -S`)
  and `python3` besides the shlibs. Tagging and dispatching stay with the maintainer. No
  CHANGELOG.md: swayward has none either (release notes are GitHub's generated ones).
  `contrib/test-release-consistency` is in the gate and CI.

- Test fonts. The unit tests that measure or draw text (`layout_box_geometry_and_colours`,
  `scale_doubles_the_size_and_markup_falls_back`, `text_over_the_texture_limit_is_refused`) need
  Pango to load some font; a bare container has none, and they failed there. They now compare
  geometry that does not depend on glyph metrics (the layout box's right edge is read at the pixel
  `x + bw` the stroke covers whatever the text width), and skip with a `SKIPPED` message when
  Pango can load no font. Every package declares Liberation Sans, the font they are verified with
  in clean containers: `checkdepends=('ttf-liberation')` in the PKGBUILD, `BuildRequires:
  liberation-sans-fonts` in the spec, `fonts-liberation` in the release workflow's build-ubuntu
  job (the `.deb` is a binary package without a source package, so it has no Build-Depends field;
  `contrib/build-deb` says where the dependency is declared), and the CI image. The Arch build job installs
  it explicitly because makepkg runs with `--nodeps`.

## Phase 2 notes (2026-10-19)

- `label`: `text`, `font` (a Pango description such as "Sans Bold 24"), `color`, `align`,
  `markup` (on by default, as in hyprlock). There is no separate `font-size`: Pango descriptions
  carry the size. Invalid markup is drawn as literal text with a warning. Label shadows from the
  spec sketch were deferred here and landed in Phase 3 (`text-shadow`, see Phase 3 progress).
- `image`: `size` is the shorter side after scaling, keeping the aspect ratio (hyprlock scales by
  `max(size / w, size / h)`, which is the same thing). Circular by default, like hyprlock's
  `rounding -1`. The border and shadow are drawn by the shape renderer, so they match `shape`.
- `cmd[]` children run in their own process group, and the whole group is killed on unlock. With
  only `sh` killed, a pipeline such as `sleep 30 | cat` would leave `sleep` behind. A unit test
  fails with plain `Child::kill`. The group stays tracked after `sh` exits until it is empty, so a
  job the command put in the background is killed too (it outlived unlock and SIGTERM while
  only a running `sh` kept its group tracked).
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
- The width, fade-on-empty, colour and dot animations landed in Phase 3 (see Phase 3 progress).
- `background { screenshot }` captures every output through wlr-screencopy before `lock()`, waits
  at most 2 s, and falls back to the colour for outputs that fail or time out.
  `--immediate-render` skips the capture. shm (A|X)(RGB|BGR)8888 and 2101010 and y-invert are
  handled; if none is offered the capture gives up at `buffer_done`. With v3 the buffer is
  chosen and `copy` sent at `buffer_done`; v1 and v2 copy on the first usable `buffer`.
  ext-image-copy-capture landed in Phase 3 (see Phase 3 progress).
- `contrast` and `brightness` follow hyprlock's FRAGBLURPREPARE (gain curve, brighten above 1)
  and FRAGBLURFINISH (dim below 1), applied on the CPU once per background. `vibrancy`, the
  blur's `saturation` and `noise` landed in Phase 3 (see Phase 3 progress); until then noise and
  saturation were decoded but had no effect.
- Animations: swayward's runtime (`src/animation/`) and KDL (`duration-ms` + `curve`, `spring`,
  `off`) are copied. swaywardlock's named animations are `fade-in`, `fade-out`,
  `input-field-fade`, `-width`, `-colors` and `-dots`, and `ring-colors`, `-highlight`, `-appear`
  and `-wrong`; `animations { off }` disables all.
  A zero-length fade-out unlocks at once. Fade in starts on `locked`. Fade out
  runs before the unlock: `request_unlock` starts it, and `finish_unlock`, the only caller of
  `unlock_and_destroy`, runs when it ends. An unlock skips past the fade-out only when it is off
  or when a second unlock request arrives during it. A redraw timer at about 60 Hz runs only while something animates.
- `--grace N`: within N seconds of `locked`, a key press, pointer motion or button unlocks without
  a password, as in hyprlock. It is off by default. The grace window opens on `locked`, not at
  start, so slow screenshots cannot use it up. It is measured on `CLOCK_BOOTTIME`, which counts
  time asleep (hyprlock uses the wall clock): with `Instant` (monotonic) a lock started with
  `--grace` just before a suspend still had its grace after the resume, and the wake key
  unlocked. Grace fails closed: if `clock_gettime(CLOCK_BOOTTIME)` fails (a sandbox or
  emulation layer can refuse it), the locker still locks, logs a warning, and gives no grace;
  a failed read after `locked` also counts as the window being over.
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
  that lock; PAM still works. The bus's `NameOwnerChanged` for fprintd's unique name (accepted
  only from `org.freedesktop.DBus`) tells the locker: `$FPRINTPROMPT` clears, `$FPRINTFAIL`
  reads "Fingerprint unavailable" and a warning is logged, so nobody keeps touching a sensor
  that is no longer read. The same happens when a retry cannot restart scanning.
- A sleep and wake inside `retry-delay-ms` restarts scanning from the wake; the retry then
  skips its own `VerifyStart`, which fprintd would refuse with `AlreadyInUse`. A failed restart
  keeps the claim and logs a warning (a later wake tries again) instead of ending the worker.
  `contrib/fake-fprintd` refuses a second `VerifyStart` the same way.
- `$DESC` is the GECOS name up to the first comma. hyprlock shows the whole field, but the
  rest is room and phone numbers.

### Phase 2 exit check

hyprlock's `assets/example.conf`, translated by hand to `examples/hyprlock-example.kdl`, parses
(unit test) and renders on headless sway at 1280x800 over a swaybg wallpaper. The checks were
done by pixel sampling: the screenshot background blurs (a striped image goes uniform grey with
`passes 3`, and is pixel-exact without blur); the time and date labels sit top-right; the
gradient outline draws; typing adds dots in the font colour; the `$LAYOUT[en,ru]` label draws.
Left out of the translation: `onclick` (out of scope).

Phase 2 leftovers, moved to Phase 3: `rotate`, label shadows, background and image
reload/crossfade, fractional scale and `vibrancy`/`vibrancy-darkness` (rotate, label shadows,
reload/crossfade, fractional scale and vibrancy done in Phase 3). Ring shadows are not planned:
neither swaylock nor hyprlock has them (hyprlock has no ring widget, and its shadows apply to
shape, image, input-field and label only).

### Phase 2 review (2026-10-19)

A six-reviewer panel (correctness, security, simplicity, tests, docs, hyprlock parity) went over
5569c54..a8ea570. Fixed in 3aaa6f4 (security) and 087ca8b (the rest): forged fprintd/logind
signals could unlock (signals are now trusted by unique name); `fade-out { duration-ms 0 }` and a
`cmd[]` child that closed stdout both left the session locked for good; `--ready-fd` leaked to
`cmd[]` children; hide-input drew inside the field; `cmd[]` ran before `$VAR` substitution;
fingerprint misses did not count as attempts; `$PAMPROMPT`, `<br/>`, text dots at natural size,
dot rounding sentinels, `"none"` alignment and more screencopy formats now follow hyprlock. This
section of the spec was brought back in line with the code: every `kdl` example validates.

## Phase 3 progress

- The input field animates like hyprlock's. With `fade-on-empty` (the default, 2000 ms) the field
  is hidden until the first key and fades out `timeout-ms` after it is empty again (a key
  cancels the wait; `timeout-ms=0` fades at once; checking counts as use). The wait is one
  calloop timer, not a ticking redraw. `fade-on-empty false` wins over `timeout-ms=` on the same
  node and keeps the field always visible. The fade alpha scales the box, outline, dots, text and
  shadow; a hidden field draws nothing but the background. `FieldAnim`
  (`src/render/field_anim.rs`) holds the five animated values and is unit-tested on a fake clock;
  the app clears the shared animation clock at the start of every draw so animations started by
  a key get the right start time. Order of effects, from the unit tests: starts hidden, shows on
  the first key, width target `max(configured, text + height)` while empty, colours in OkLab,
  dots fractional with hyprlock's overflow window (up to `max + 1` rects, partial alpha at both
  ends, centred even with `center=false`).
- Screenshots fall back to ext-image-copy-capture-v1 when wlr-screencopy is missing (hyprlock has
  no ext path). One session and one frame per output: the first accepted shm format from the
  session's constraints, a client-chosen stride of width * 4, full damage, then `capture`. A
  failed or stopped capture, a size that does not fit wl_shm's i32, or no usable format falls back
  to the colour for that output; the session stays locked. On the 2 s timeout every frame and
  session still in flight is destroyed before `lock()`.
- Rotated and flipped outputs: both protocols deliver the frame in buffer order, so the lock showed
  it sideways or mirrored. Each pixel is now moved to where wlroots' `wlr_box_transform` puts it,
  using the output's wl_output transform for wlr-screencopy and the frame's `transform` event for
  ext-image-copy-capture (an unknown value counts as normal). e2e checks the lock against grim's
  desktop at `transform 90` and `flipped-180` for both protocols.
- `contrib/hyprlock-to-kdl` converts a hyprlock.conf (see Converters). It emits only KDL that
  exists today: hyprlock keys without a KDL counterpart
  (`reload_time`/`reload_cmd`/`crossfade_time`, `onclick`) become a stderr warning plus a
  `// hyprlock: key = value (not supported yet)` comment. The units that add those keys switch them
  to real KDL (one `UNSUPPORTED` table entry each) and extend the converter tests. The shipped
  `examples/hyprlock-example.kdl` was corrected against the converter's output: label 3 uses
  hyprlock's default font `Sans 24` (not `Monospace 24`) and the labels say `align "left"`
  (hyprlock's default; ours is centre).
- Test builds read `SWAYWARDLOCK_TEST_SCREENCOPY` (`ext`, `none`, `ext-fail`) to skip a protocol
  or force a failed capture; `contrib/gate` checks that release builds do not contain it.
- Audit fixes (startup, process, clocks): `--grace` on `CLOCK_BOOTTIME` (a suspend inside the
  grace period no longer leaves it open after the resume; an unreadable boot clock means no grace,
  where it first read as 0 forever and kept the window open), an overflowing `--grace` refused with
  exit 1 instead of a panic after locking; `--ready-fd` checked at start and written with raw
  `write`/`close` (a number that was not open later named one of our own descriptors, and closing
  it after `locked` aborted or broke the auth channel); the soft `RLIMIT_NOFILE` raised to at
  most 1024 and a limit under 64 refused before locking (with 16 or 17 it locked, then crashed);
  exit 3 for failures after `lock()`; the event loop is not dropped, so SIGUSR1/2 stay blocked
  through teardown; SIGUSR2 refreshes labels in `--preview`; `$TIME` checks the wall clock every
  second; `--immediate-render` also skips image decoding; `finished` after `locked` is answered
  with `unlock_and_destroy`. Not done: ending grace on logind `PrepareForSleep` (the boot clock
  already counts the sleep, and it would need a system-bus connection in every lock), and a
  pre-main SIGUSR1 handler (documented under exit codes instead). e2e tests: `fd-limit`,
  `compositor-death`, `finished-after-locked` (proxy `contrib/wl-inject-finished`),
  `preview-refresh`, `clock-resume` and new `grace`, `sigusr1` and `ready-fd` checks; the clock
  tests preload `contrib/clock-jump.c`, which moves the wall and boot clocks an hour on (or,
  with `CLOCK_BOOTTIME_FAIL`, makes the boot clock fail).
- `ring-indicator { style "swayward"; }` (spec `2026-10-20-ring-style.md`): `RingAnim`
  (`src/render/ring_anim.rs`) tweens colours, highlight angle, trail, flash, shake and appear with
  `field_anim`'s `Var`/`Mix`, retargeted from the `RingFrame` every draw, unit-tested on a fake
  clock. The texture is re-rendered only when the frame or the quantised look changes; the shake
  and the appear alpha are applied when the texture is placed. A surface's `ring_animating` keeps
  the redraw ticker running like `field_animating`, and stops counting once an unlock is pending.
  The swaylock style renders through the same function with swaylock's colours; a unit test
  pins its pixels to hashes captured on the code before the change. Test builds read
  `SWAYWARDLOCK_TEST_AUTH_DELAY_MS` to hold the test password check (the spinner's e2e check);
  `contrib/gate` checks release builds do not contain it. e2e test: `ring-style`.
- Background `vibrancy` and `vibrancy-darkness` (hyprlock's), and the blur's `noise` and
  `saturation`, which were decoded but ignored until now. One fragment shader
  (`src/render/shaders/postprocess.frag`) runs after the blur: hyprlock's FRAGBLUR1 vibrancy
  branch `passes` times at `vibrancy / passes`, then swayward's `saturate` and `hash12` noise
  (on `gl_FragCoord`, as in swayward), on unpremultiplied colours with alpha kept. hyprlock boosts
  inside each blur down pass; here the passes run back to back after the blur, the same for a
  uniform colour and an approximation elsewhere. A CPU pass was tried first: readback, maths and
  upload took 280-460 ms for a 3840x2160 background on llvmpipe (release build), the shader 60-80
  ms. The program compiles on first use and is cached on the EGL context; if it fails to compile
  or draw, the blurred background is drawn without post-processing and a warning is logged.
  Behaviour change: a `blur` block now gets swayward's defaults `noise 0.02; saturation 1.5;`, so
  existing blurred backgrounds look more saturated and grainy; write `noise 0; saturation 1;` for
  the old look. `contrib/hyprlock-to-kdl` writes `saturation 1` in every blur (hyprlock has
  none) and hyprlock's vibrancy defaults with `blur_passes > 0`. Unit tests run the shader on
  Mesa's surfaceless EGL (skipped when there is none); e2e test: `vibrancy`.
- `rotate` on `label`, `image` and `shape` (see Common widget properties). smithay's `Transform`
  has only the eight 90-degree variants, so `RotatedTextureElement` (`src/render/rotated.rs`)
  calls `GlesFrame::render_texture` itself: the quads drawn are the damage rectangles clipped to
  the turned texture's bounding box, and the texture matrix maps each output pixel back into the
  unrotated texture (outside it, the clamped transparent margin samples as nothing). So nothing
  outside the damage is blended twice if buffer age ever stops being 0. Shapes and images render
  their unrotated elements (shadow, border, fill or picture) into an offscreen texture with a
  1 px margin (`src/render/offscreen.rs`, trimmed from swayward), cached until their parameters
  change and checked against the GPU texture limit first; a render that fails is logged and
  skips only that widget. Labels turn their text texture, also with a 1 px margin. `place_rotated`
  ports hyprlock's edge correction. hyprlock's on-screen sign rests on its wiki ("degrees,
  counter-clockwise"); nobody ran hyprlock here, and the e2e test pins counter-clockwise.
  `hyprlock-to-kdl` now emits `rotate`. e2e test: `rotate`.
- `text-shadow` on `label`: hyprlock's `shadow_passes`/`shadow_size`/`shadow_color`/`shadow_boost`
  (see `label`). The text is rendered in the shadow colour with the label's alpha into a cairo
  surface padded by `2 * size * 2^passes + 2^(passes+1) + 2` px (measured: a 16 px block's blur
  reached 7, 11, 24, 44 and 79 px with alpha above 0 at size 3 and 1 to 4 passes; the
  `2^(passes+1)` term holds the down/up chain's own spread, which reaches about 2^passes px even
  at size 0), uploaded, blurred with the background's `Blur`, then read back once for the boost,
  the clamp and the premultiplied tint on the CPU; a shader would add a second program for a few
  kilobytes. The work is bounded: passes are capped at 4 and size at 20, and a shadow whose
  texture would pass 2^19 px (1024 x 512) is blurred at 1/2, 1/4, ... resolution, each halving
  replacing the first down pass (dual Kawase's first pass is itself a halving; past one pass the
  offset is halved instead), then drawn scaled up. Against the full-resolution shadow the reach
  and the total alpha differ by under 1/8 (unit test). The worst config (passes 4, size 20, scale
  2) on a label changing every 300 ms went from a full-resolution blur (about 2800 x 2700 px),
  211 percent CPU and 458 MB peak RSS to a 1/4-resolution one, 19 percent and 134 MB (release,
  llvmpipe; no shadow: 3 percent, 115 MB); at
  the old caps (passes 5, size 100) the reviewer measured 4.2 GB and a 6 to 10 s stall. The
  shadow is cached by text, font, colour, alignment, markup, shadow and scale: a label whose text
  did not change is never blurred again, and a relayout (configure) keeps it. The result is drawn
  behind the text at `loc - pad + offset`; with `rotate` it goes through the same
  `RotatedTextureElement`, pivoting on the text's centre so it turns with the text. An oversize or
  failed shadow is logged and the label is drawn without it. e2e test: `text-shadow`.
  `hyprlock-to-kdl` writes the `text-shadow` node (`shadow_passes 0` is none). **Ring shadows are
  dropped from scope**: hyprlock applies SHADOWABLE to shape, image, input-field and label only
  and has no ring widget, and swaylock's ring has no shadow.

- `background`/`image` `reload-ms`, `reload-cmd` and `crossfade-ms` (see "Reloading" under
  `background`): `src/reload.rs` runs the command on `commands::Runner` (its own channel, like
  `labels.rs`) and decodes on a thread per request (`render::background::decode_for_reload`:
  header check, `image::Limits`, scaled down to the outputs, premultiplied ARGB that
  `Image::from_decoded` wraps without a copy). The pictures and a generation counter per widget
  live in `Reload`, passed to `draw` in `Dynamic`; a changed generation re-renders the cached
  texture (the image arm's clipped texture, the background's blur and fit). A new picture is a
  trial: each surface reports whether it rendered it (`Surface::trials`), and `Reload::settle`
  after the frame keeps it or drops it, in which case the old picture comes back (re-rendered on
  an output that had drawn the failed one). The crossfade keeps the old texture in
  `Cached::Background` and draws it opaque under the new one at alpha `elapsed / crossfade-ms`;
  `field_animating` covers it, so the redraw ticker runs and an unlock never waits for it. A
  picture decoded during it is held, the widget stays pending, and a timer at the fade's end
  puts it on trial. `hyprlock-to-kdl` maps `reload_time` (seconds, to ms; -1 is off),
  `reload_cmd` and `crossfade_time` (note: hyprlock itself ignores it), and warns for the
  combinations hyprlock never acts on. e2e test `reload`: command, SIGUSR2-only, image mtime,
  crossfade samples, failures (corrupt file, empty output), unlock with a slow command running
  (its process group is gone afterwards). e2e test `reload-keep` (review round 2): an image
  reload that decodes but is over the GPU texture limit keeps the old picture and warns once; a
  144-megapixel PNG of a few hundred KB is refused from its header (peak RSS stays about 120 MB
  instead of over 1 GB); a 2000x1000 picture is scaled to 480x240 for a 320x240 output; a reload
  during a crossfade waits for it. SIGUSR1 latency right at the handoff (release build,
  llvmpipe, baseline about 60 ms) was 281 ms for an 8000x8000 picture and 1.4 s for 22000x22000
  (main-thread copy and cairo scale of the full picture); now 59 to 65 ms from 1000 to 8000 px,
  and larger pictures are refused. Under `ulimit -v 3000000` a 16000x16000 grey PNG aborted the
  locker after lock; now it is refused with a peak RSS of about 110 MB.

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
