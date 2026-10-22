# swaywardlock: animated ring style

Status: implemented (2026-10-20, Phase 3 unit `ring_style`). Parent spec: `2026-10-19-swaywardlock.md`.
Readings of points the text leaves open are under "Implementation notes" at the end.

## Problem

`ring-indicator` is a line-by-line port of swaylock's `render_frame`: colours snap between
states, the typing highlight jumps to a random spot on every key, and "Verifying" and "Wrong"
are only text. That parity is part of the contract (swaylock users get the same pixels, and
`contrib/swaylock-to-kdl` relies on it), but next to the animated input field (fieldanim) the
ring looks static and dated. We want a ring that feels alive without giving up the swaylock look
for those who want it.

## Approach

An opt-in style on the existing widget. `style "swaylock"` (the default) is today's code,
untouched and still pinned by the existing e2e pixel checks. `style "swayward"` draws the same
geometry with the same cairo/Pango path in `src/render/ring.rs`, but takes its colours, angles,
offsets and alpha from a new `RingAnim` (the same `Var`/`Mix` tween pattern as
`src/render/field_anim.rs`), and adds a glow.

The texture is re-rendered only while something moves (the `RingFrame` cache comparison is
extended with the animated values, quantised so a settled ring stops redrawing), driven by the
existing 60 Hz redraw ticker in `src/fade.rs`. A settled ring costs nothing; an animating ring
costs one cairo render per frame (about 1 ms for a 100 px ring at scale 2; the
oversized-texture guard from the audit already rejects absurd sizes).

Why cairo and not a GL shader: every effect here is a few extra cairo calls on code that already
exists, text stays on the one Pango path, it renders at buffer scale (sharp at fractional
scales), and tests stay plain pixel checks. A shader ring (near-zero cost per frame at any size)
remains possible later behind the same `style` value if profiling ever asks for it. A third
option, transforming a cached texture on the GPU, needs arbitrary rotation (the Phase 3 rotate
unit) and was not chosen.

## Config

```kdl
ring-indicator {
    style "swayward"   // "swaylock" (default): swaylock's ring, pixel for pixel
    glow 12            // logical px 0..64, 0 = off; absent = 12 with "swayward". Any glow with "swaylock" is an error
    // every existing property and the colors block keep their meaning
}

animations {
    ring-colors    { duration-ms 250; curve "ease-out-cubic"; }  // state colour crossfade
    ring-highlight { duration-ms 180; curve "ease-out-cubic"; }  // highlight sweep
    ring-appear    { duration-ms 250; curve "ease-out-cubic"; }  // fade and scale in and out
    ring-wrong     { duration-ms 450; curve "linear"; }          // shake and flash
}
```

Three keys only: `style`, `glow`, and the four `ring-*` animation entries, which reuse the
existing animation node syntax (easing or spring) and the global `animations { off; }`. Nothing
else becomes configurable (one style with good defaults; knobs are added when someone asks).
`glow` with `style "swaylock"` is a validation error, so a swaylock config cannot drift from
swaylock's pixels by accident. The converter `contrib/swaylock-to-kdl` keeps emitting no `style`
(the default).

## Behaviour (style "swayward")

Everything not listed is as in the swaylock style: geometry (`radius`, `thickness`), the state
precedence of `pick` (cleared > verifying > wrong > caps lock > input), the message texts
("Cleared", "Verifying", "Wrong", "Caps Lock", failed-attempt count), the keyboard layout box,
`idle-visible`, `line-uses`, and when the ring is visible.

1. **Colour crossfade.** inside, ring, line and text colours (and the layout box colours) tween
   to the new state's colours over `ring-colors`, blended in OkLab with linear alpha
   (`field_anim::lerp_color`). A new target restarts from the colour on screen.
2. **Highlight sweep.** On a key, the highlight arc (same length, `PI / 3`, and the same
   key-highlight / backspace-highlight / caps-lock colours) moves from its current angle to
   `highlight_start`'s new angle along the shorter direction over `ring-highlight`. A trail of
   the same colour covers the swept span and fades to transparent over the same duration. The
   random positions are swaylock's (`state.rs` `highlight_start`), so typing still does not
   reveal password length by position.
3. **Verifying spinner.** While `auth == Validating`, a comet arc in the key-highlight
   colour (head at full alpha, tail fading to 0 over a quarter turn) rotates at one turn per 1.2 s, on top
   of the verifying colours. The ticker runs only while validating. It stops at the next state.
4. **Wrong.** On a transition to `Invalid`: the whole ring (and its text) shakes horizontally,
   a damped sine of 3 oscillations with amplitude `0.08 * radius`, decaying to 0 over
   `ring-wrong`; and a flash: the ring colour starts at the wrong colour at full alpha mixed
   20% towards white and settles to the wrong colour over the same duration. The wrong state
   itself still lasts as long as it does today (the auth timing does not change).
5. **Appear and disappear.** When the ring becomes visible it fades in from alpha 0 and scales
   from 0.9 to 1.0 around its centre over `ring-appear`; when it becomes hidden it fades out
   (no scale) over the same duration, then stops drawing. With `idle-visible` it never hides,
   so this does not apply.
6. **Glow.** A soft halo outside the ring stroke, `glow` logical px wide, in the current
   (animated) ring colour at 40% of its alpha, falling off as a radial gradient to 0. Static: it
   follows the colour crossfade but has no motion of its own. It enlarges the texture by `glow`
   on each side and is included in the centre-alignment offset, so the ring does not move.
   It is not drawn behind the layout box.

`animations { off; }` (or any `ring-*` entry with `off`): every effect jumps to its end state:
colours switch at once, the highlight is drawn at its new angle without a trail, no spinner
(verifying colours only), no shake or flash, no fade or scale. The glow stays (it is not
motion). The same holds under `--immediate-render` for the first frame.

Fade interplay: the whole-screen fade-in and fade-out multiply over the ring as they do over
every widget; a running ring animation never delays the unlock (finish_unlock does not wait for
`RingAnim`), and nothing draws after unlock (the audit's draw guard covers this widget like the
rest).

## Not in scope

- Frosted glass inside the ring (background blur behind the ring). It overlaps with the Phase 3
  vibrancy and background-blur unit; it will be built on that unit's blur once it lands, as a
  follow-up to this spec.
- Idle "breathing" or any motion while nothing happens (keeps the GPU idle on an idle lock
  screen).
- A shader-drawn ring, per-effect switches, new colour keys.
- Any change to the swaylock style.

## Testing

Unit (`src/render/ring.rs`, new `src/render/ring_anim.rs`):
- `RingAnim` values over time with a manual `Clock`: colour mix at 0, mid and end; highlight
  sweep takes the shorter direction across 0/2048; trail alpha reaches 0; shake offset is 0 at
  start and end and bounded by `0.08 * radius`; spinner angle advances only while validating;
  appear alpha and scale reach 1.
- `animations { off; }`: every value equals its end state immediately.
- Settled ring: two successive `RingFrame`s compare equal (no redraw loop).
- Config: `style` values, `glow` range, `glow` with `style "swaylock"` rejected, the four
  `ring-*` animation entries decode, spec example validates (existing e2e check).

e2e (`contrib/e2e-sway`, new test `ring-style`, headless sway, test-auth):
- The swaylock style's existing ring pixel checks are unchanged and still pass (parity guard).
- Crossfade: after a key, a ring pixel sampled during `ring-colors` lies strictly between the
  idle and input colours, and equals the input colour after it.
- Spinner: during a validating window, two samples of the same ring pixel 300 ms apart differ.
  The test backend answers instantly, so test builds get a new test-auth-only hook,
  `SWAYWARDLOCK_TEST_AUTH_DELAY_MS`, that delays the test backend's verdict (absent from
  release builds, like the other `SWAYWARDLOCK_TEST_*` hooks; the gate's release grep covers it).
- Wrong: the ring's horizontal centre (colour-width scan) is offset during `ring-wrong` and back
  at rest after it; the session stays locked.
- Glow: a pixel just outside the ring stroke is tinted with glow on and is background with
  `glow 0`.
- `animations { off; }`: a sample right after a key already has the final colour.
- Unlock during an animation (spinner and shake) exits 0 promptly; no draw after unlock.

## Implementation checklist

1. Config: `style` and `glow` on `RingIndicator` (`src/config/widgets.rs`), the four `ring-*`
   entries on `Animations` (`src/config/mod.rs`), validation, spec Config section.
2. `src/render/ring_anim.rs`: `RingAnim` (tweens from `field_anim`'s `Var`/`Mix`, made
   `pub(crate)` where needed), retargeted from `LockState` changes each frame, `is_animating()`.
3. `src/render/ring.rs`: take an animated frame (colours, highlight angle and trail, spinner,
   offset, alpha, scale, glow) for the swayward style; the swaylock path stays byte-identical.
4. `src/surface.rs` ring arm and the redraw ticker: re-render while `RingAnim` animates; include
   glow and shake in the placement offset.
5. Tests above; README and spec (Widgets, Animations, Phase 3 progress).
6. Example config `examples/swayward-ring.kdl`, validated by the existing spec-example check.

Builds after the audit fix tasks touching `src/main.rs`/`src/surface.rs` merge (fix_auth_2,
fix_input_2, fix_widgets_2, fix_tests), as a Phase 3 unit `ring_style` with a review gate.
Frosted glass follows the vibrancy unit.

## Open questions

None blocking. Defaults (durations, glow 12, shake amplitude, spinner speed) are first guesses to
be tuned on screen with `contrib/try-lock`.

## Implementation notes

How the implementation reads points this spec leaves open:

- `ring-*` entries with `off` end only the effects they drive; the spinner has no entry of its own
  and only the global `animations { off; }` removes it.
- The highlight sweeps only from a highlight that is on screen; the first key after the highlight
  went away draws it at its new angle, without a trail. The trail covers the span between the
  start it swept from and the current start, plus the highlight's length.
- A ring that appears takes the new state's colours at once (nothing on screen to crossfade
  from), and the first frame of the lock shows end states (the whole-screen fade-in covers it).
  While fading out it keeps its last visible content. Coming back during a fade-out it fades in
  from where it is, without the scale.
- The shake and the appear alpha move and fade the cached texture when it is placed; only
  colours, angles, the trail, the spinner and the appear scale re-render it.
- The e2e crossfade check uses input to clear (Escape) on the inside fill: idle and typing share
  the input colour set, so a key alone changes no colour, and the highlight's random angle could
  cover a band sample.
