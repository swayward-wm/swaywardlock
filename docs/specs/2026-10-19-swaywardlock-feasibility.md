# swaywardlock feasibility

Date: 2026-10-19. Base: swayward `339f99e8`, smithay rev `0c05927e` (the
swayward workspace pin), swaylock `44b82de`, hyprlock `0332e40`.

## Decisions so far

1. **"IPC compliant"** means: swaylock's process contract, the
   ext-session-lock-v1 protocol, and optionally the `$SWAYSOCK` client side.
2. **No crate sharing.** Splitting swayward's crates is too costly.
3. **Its own config:** `$XDG_CONFIG_HOME/swaywardlock/config.kdl`.
4. **Copy, don't extract.** The code we need is copied into swaywardlock.
5. **Standalone:** it runs on any compositor with ext-session-lock-v1, not
   only swayward.
6. **Smithay for rendering, as far as it goes.** Keep smithay's
   `GlesRenderer`, its render elements and `OutputDamageTracker`. Do not
   fall back to `glow`.
7. **winit is an accepted dependency** (through smithay's `backend_winit`),
   so we use smithay's own `WlEglSurface` support.
8. **fprintd is in scope** for hyprlock parity (zbus).
9. **swayward does not add ext-image-copy-capture.** swaywardlock
   supports it for other compositors only.
10. **Own repo:** `swayward-wm/swaywardlock`.

## Verdict

Feasible. With crate sharing ruled out, the only code-level question is which
parts to copy and which to rewrite. The answer: copy the parts with no
coupling (config value types, animation maths, shaders), and write the
application from scratch, since nothing in swayward does a client's job.

"Standalone" costs little. Two features need a fallback, and both fallbacks
use plain Wayland: screenshots and keyboard layout names.

## What "IPC compliant" covers

1. **Process contract.** These are what swayidle, DMS and scripts depend on:
   `-f`/`--daemonize`, `--ready-fd N`, SIGUSR1 unlocks, exit 0 on unlock,
   plus hyprlock's `--grace`, `--no-fade-in`, `--immediate-render`,
   `--display` and SIGUSR2 (refresh). `swayidle before-sleep 'swaywardlock -f'`
   must work unchanged. We do not accept swaylock's flag set; that comes from
   the migration script instead (see below).
2. **ext-session-lock-v1**, including staying locked if the locker dies. The
   compositor guarantees this, not the locker. swayward does it
   (`src/swayward/lock.rs:53-60`, tested in `src/tests/session_lock.rs`), and
   so do sway and niri.
3. **`$SWAYSOCK`, optional.** Use it only when the variable is set. It is never
   needed, because every value it gives has a Wayland source (next section).

## Standalone: the technical implications

| Feature | Wayland source (any compositor) | Better source if available | Notes |
|---|---|---|---|
| Lock | ext-session-lock-v1 | | Required. Exit with an error if missing |
| Output name for `output="DP-1"` | `wl_output` v4 `name`/`description` (sctk `OutputInfo`) | | No IPC needed |
| Keyboard layout `$LAYOUT` | `wl_keyboard` keymap + modifiers `group` → `xkb_keymap_layout_get_name` (sctk keyboard, `xkbcommon` 0.9 `layout_get_name`) | | No IPC needed; this is how hyprlock does it |
| Caps/Num lock | xkb state from `wl_keyboard` | | |
| Screenshot background | **wlr-screencopy-unstable-v1** | **ext-image-copy-capture-v1** | See below |
| Fractional scale, viewporter, cursor-shape | standard protocols | | Fall back to integer scale and a hidden cursor |
| Unlock on idle/sleep events | none needed | logind over D-Bus | Like hyprlock; optional |

**Screenshots** are the one real divergence between compositors:

- swayward serves only wlr-screencopy (`src/protocols/screencopy.rs`). niri
  is the same.
- sway serves both wlr-screencopy and ext-image-copy-capture
  (`sway/sway/server.c:516`).
- Newer compositors are moving to ext-image-copy-capture. smithay already
  has a server implementation (`smithay/src/wayland/image_copy_capture/`), so
  swayward could add it later.

So the client implements wlr-screencopy first (it covers swayward, sway and
niri) and adds ext-image-copy-capture second. If neither exists, use the
`color` fallback, as hyprlock does (`Background.cpp:86`). Take the frames
before requesting the lock, because after locking the compositor shows only
the lock surface (hyprlock: `hyprlock.cpp:363-375`).

**Testing on other compositors.** The swayward headless harness covers
swayward. To check "standalone", add a CI smoke run against headless sway
(`WLR_BACKENDS=headless`): lock, unlock with SIGUSR1, check the exit code.
That backs up the claim with a run.

## Copy or rewrite: piece by piece

The license is the same (GPL-3.0-or-later) both ways, so copying is
legal. Keep the niri/swayward copyright lines.

| Piece | Source | Lines | Coupling | Decision |
|---|---|---|---|---|
| `Color`, `Gradient` (oklab, hue interpolation), CSS colour parsing | `swayward-config/src/appearance/color.rs` | 510 | `smithay::...::Color32F` only | **Copy** |
| `Blur`, `Shadow` and their KDL decoding | `appearance/effects.rs`, `appearance/border.rs` | 435 | `FloatOrInt`, `MergeWith` | **Copy**, delete the window-rule and workspace parts |
| Animation KDL (curves, springs, durations) | `swayward-config/src/animations/` | 837 | `FloatOrInt`, `parse_arg_node` | **Copy**, delete swayward's animation list |
| Value helpers (`FloatOrInt`, `Flag`, `MergeWith`, `parse_arg_node`) | `swayward-config/src/utils.rs`, `utils/merge_with.rs` | 277 | none | **Copy** |
| Error type, miette report with include errors | `swayward-config/src/error.rs` | 99 | none | **Copy** |
| Includes | `decode_include` and stack in `swayward-config/src/decode.rs:24-30,308-435` | ~150 | swayward's root `Config` | **Copy**; change the root type |
| `ConfigPath` loader | `swayward-config/src/loader.rs` | 142 | `Config`, the `swayward/config.kdl` path | **Rewrite**, ~60 lines |
| Root config (`general`, `auth`, widgets) | | | | **New** |
| Animation runtime: bezier, spring, clock | `src/animation/` | 837 | `get_monotonic_time` | **Copy** |
| Shaders: blur down/up, border (rounded, gradient), shadow, rounding alpha | `src/render_helpers/shaders/*.{frag,vert}` | ~470 | `niri_*` uniform names | **Copy** as is; GLSL has no coupling |
| Shader programs and elements: `shader_element`, `border`, `shadow`, `blur`, `solid_color`, `texture`, `memory`, `resources`, `shaders/mod.rs` | `src/render_helpers/` | ~2.5k | `TtyRenderer` impls, `NiriRenderer` | **Copy**, delete every `TtyRenderer` impl and replace `NiriRenderer` with plain `GlesRenderer` |
| Pango text to texture | pattern repeated in `src/ui/*.rs` | ~80 | | **Copy** into one helper |
| sway IPC client | `swayward-ipc/src/sway_socket.rs` + `wire.rs` | ~520 | `MessageType` | **Copy** only if the optional `$SWAYSOCK` feature stays; otherwise drop |
| App: Wayland state, session lock, outputs, seat, input, widgets, auth, layout | | | | **New** |
| `sway-to-kdl` structure | `contrib/sway-to-kdl` | 1852 | | Template for the migration scripts |

About 6k lines copied. That is more than hyprlock's whole GL layer (~2k
lines: `Renderer.cpp`, `Framebuffer.cpp`, `Shader.cpp`, `Shaders.hpp`,
`Egl.cpp`), so it is fair to ask whether a rewrite would be smaller.

**Rewriting the renderer wholesale** (plain `glow`, our own blur and
rounded-rect shaders, as hyprlock does) would come to about 2-3k lines and
drop the smithay dependency. Against that:

- Gradients, rounded borders, shadows and blur would no longer look like the
  same KDL renders in swayward. Matching looks is the main reason the
  locker belongs in the swayward family.
- The copied element code drops into smithay's `OutputDamageTracker`, so
  partial redraws come free. A rewrite builds its own frame loop.
- The copied shaders and the decoding behind them already have tests in
  swayward (`swayward-visual-tests`, config snapshot tests).

**Decided:** copy the shaders and config types, keep smithay's
`GlesRenderer` as the renderer, and write the application from scratch.
The `glow` rewrite is off the table. If Phase 0 finds a problem with the
smithay renderer on a client surface, we fix or work around it in
smithay (a local patch, then upstream), and keep the renderer.

### Keeping copies from drifting

Copies drift, by design. Make the drift visible without making it work:

- Add a `COPIED.md` table: destination file, source path, source sha, and
  how far it has diverged ("verbatim", "trimmed", "rewritten").
- Add a script, `contrib/copied-diff`, that diffs each verbatim/trimmed file
  against swayward at a given sha. Run it by hand when swayward fixes a bug
  in a copied file. Do not gate CI on it.

## Repository

With no crates shared, nothing ties swaywardlock to swayward's workspace.
**Decided: its own repo, `swayward-wm/swaywardlock`.** It keeps
swayward's gate, oracle tiers and release from growing, and it fits the
standalone goal. Pin smithay to swayward's rev at copy time so the copied
render code builds without changes, then update the pin on its own
schedule.

## Rendering on a client surface

Smithay is a compositor library, but its renderer works on a client
`wl_egl_window`. Smithay's own winit backend does exactly this
(`smithay/src/backend/winit/mod.rs:220-273`).

With winit accepted, the stack is:

| Layer | Comes from |
|---|---|
| Protocols: `session_lock`, `output`, `seat`/xkb, calloop | smithay-client-toolkit 0.21.1 |
| `wl_egl_window` per lock surface | `wayland-egl` `WlEglSurface::new(surface.id(), w, h)` |
| EGL surface | smithay `EGLSurface::new` over its `EGLNativeSurface for WlEglSurface` impl (`native.rs:401`, `backend_winit`) |
| EGL display | **ours, ~20 lines**: `EGLNativeDisplay` for the client's `wl_display` pointer, using the exported `egl_platform!(PLATFORM_WAYLAND_KHR, ...)` |
| Context, renderer, elements, damage | smithay `EGLContext`, `GlesRenderer`, `Bind<EGLSurface>`, `OutputDamageTracker` |

The one piece winit does not give us is the display. Smithay implements
`EGLNativeDisplay` only for `Arc<dyn WinitWindow>`, not for a bare
`wl_display`, and a lock surface is not a winit window. The impl is a copy of
the Wayland arm of `native.rs:189-216`. It is also small enough to
upstream to smithay as `EGLNativeDisplay for wayland_client::backend::Backend`
(or similar), which would remove our only hand-written EGL code.

Constraints that follow:

- `wayland-egl` requires `wayland-backend/client_system`, so sctk and
  `wayland-client` must be built with the `system` feature (libwayland-client
  C backend). That is how we get the raw `wl_display` pointer.
- smithay features: `backend_winit` (implies `backend_egl`, `renderer_gl`,
  `wayland-client`, `wayland-egl`). No compositor features
  (`wayland_frontend`, `desktop`, `backend_drm`, ...).
- winit compiles into the binary, but we never create a winit window or event
  loop. The cost is build time and binary size, not behaviour.
- One `GlesRenderer`/`EGLContext` shared by all outputs, one `EGLSurface`
  per lock surface. The copied `shaders::init` and `resources::init` run once
  on that renderer, as in swayward's headless backend
  (`src/backend/headless.rs:77-87`).
- Fractional scale: resize the `WlEglSurface` to the buffer size and set
  the viewporter destination to the logical size.

**Risk:** nobody tests smithay as a client renderer. Phase 0 exists to find
out early, and the headless-sway CI run keeps checking it.

## Config sketch

Widgets are nodes. Colours, gradients, blur, shadow and animation curves use
the same syntax as swayward, because the decoding code is the same.

```kdl
include "~/.config/swayward/colors.kdl" optional=true

general {
    grace-seconds 5
    hide-cursor
    ignore-empty-input
    fail-timeout-ms 2000
}
auth {
    pam service="swaywardlock"
    fingerprint ready-message="Scan finger" present-message="Scanning..."
}
animations {
    fade-in { duration-ms 200; curve "ease-out-cubic"; }
}
background {
    screenshot
    blur passes=3 size=8 noise=0.0117 brightness=0.8
}
input-field output="DP-1" {
    size 300 50
    position 0 -80; halign "center"; valign "center"
    outline { gradient from="#80c8ff" to="#c7ff7f" angle=45; width 3; }
    dots size=0.33 spacing=0.15
    fail-text "$FAIL ($ATTEMPTS)"
}
ring-indicator { radius 50; thickness 10; }   // the swaylock look
label {
    text "cmd[update:1000] date +%H:%M"
    font "Sans 90"
    position 0 80; halign "center"; valign "top"
}
```

Note: `include` takes a whole config file, so sharing colours with swayward
needs a colours-only file that both configs include. Swayward's config does
not support named colour variables yet. Treat this as a later nicety, not a
launch feature.

## Migration scripts

- `swaylock-to-kdl`: easy, about 200 lines of Python in the style of
  `sway-to-kdl`, with a corpus test. swaylock's config file holds ~50 long
  options, nearly all ring-indicator colours, and they map onto one
  `background` and one `ring-indicator`. The same script can take a command
  line (`swaylock -f -c 000000 ...`) so swayidle invocations convert as well.
- `hyprlock-to-kdl`: moderate. hyprlang has variables and `source=`, but
  real configs are flat sections that map one-to-one onto our widgets.

## Security

- PAM only, on its own thread, like hyprlock. No setuid shadow path:
  swaylock's privileged fork (`comm.c`) exists for setuid shadow builds.
  Ship a `pam.d/swaywardlock` file.
- Keep the password buffer in `mlock`ed memory and zero it after use
  (`zeroize`), like swaylock's `password-buffer.c`.
- `panic = "abort"`. A crash must leave the screen locked, and it does,
  because the compositor guarantees it. Never unlock on an error path.
- `cmd[...]` labels run user commands, as hyprlock does. Document this,
  and kill the child processes on unlock.

## Testing

- Unit and snapshot tests for config decoding and migration scripts.
- Auth goes behind a trait so tests can use a fake PAM.
- Integration: swaywardlock against headless sway in CI (lock, wrong
  password, right password, SIGUSR1, `--ready-fd`, compositor without
  screencopy). On a GPU-less runner, Mesa's Wayland EGL falls back to
  software rendering (llvmpipe/swrast, buffers over `wl_shm`), so the run
  also exercises the smithay client renderer. Phase 0 must confirm this on
  the CI image.
- Visual: render widgets offscreen with `GlesRenderer` and compare
  snapshots, the same way `swayward-visual-tests` does it.

## Plan

| Phase | Scope | Rough size |
|---|---|---|
| 0 | Spike: `EGLNativeDisplay` for `wl_display` + smithay `WlEglSurface`/`GlesRenderer` on an sctk lock surface, solid colour, PAM unlock, `-f`/`--ready-fd`/SIGUSR1, headless-sway CI run | 1-2 days, ~800 lines |
| 1 | swaylock parity: copy config types and shaders, ring indicator, image background, caps/layout text, `swaylock-to-kdl`, `COPIED.md` | ~2k new + ~3k copied |
| 2 | hyprlock parity: widget model, labels with variables and `cmd`, input field, wlr-screencopy + blur, shapes, images, animations, grace, fprintd, per-output widgets | +3-4k new |
| 3 | ext-image-copy-capture (for sway and others, not swayward), `hyprlock-to-kdl`, optional `$SWAYSOCK`, logind | +1k |

## Still open

Nothing blocks a spec. Questions the spec should settle:

1. The exact KDL schema for each widget, including how hyprlock's
   `position`/`halign`/`valign`/`zindex` map onto nodes.
2. The CLI flag list. It is the union of swaylock's contract flags and
   hyprlock's flags, and every swaylock styling flag stays out.
3. Whether to offer the `EGLNativeDisplay` impl upstream to smithay during
   Phase 0 or after.
