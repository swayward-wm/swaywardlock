# Code copied from swayward

swaywardlock shares no crates with swayward (see the spec, D2). These files are copies, taken
from swayward at one commit so the shared KDL vocabulary and the shaders match the compositor.

Source: `swayward-wm/swayward` at `339f99e8d70c099f75aaad86dcc8b23c9cd32bd2`.

| Destination | Source | Treatment |
|---|---|---|
| `src/config/copied/macros.rs` | `swayward-config/src/macros.rs` | verbatim |
| `src/config/copied/merge_with.rs` | `swayward-config/src/utils/merge_with.rs` | verbatim |
| `src/config/copied/error.rs` | `swayward-config/src/error.rs` | verbatim |
| `src/config/copied/utils.rs` | `swayward-config/src/utils.rs` | trimmed: `RegexEq` removed, `merge_with` imported from the sibling module |
| `src/config/copied/color.rs` | `swayward-config/src/appearance/color.rs` | trimmed: `DEFAULT_*_COLOR` and `GradientRelativeTo` removed |
| `src/config/copied/effects.rs` | `swayward-config/src/appearance/effects.rs` | trimmed: `BackgroundEffect*` removed, imports adjusted |
| `src/config/copied/shadow.rs` | `swayward-config/src/appearance/border.rs` | trimmed: only `Shadow`, `ShadowOffset`, `ShadowRule` |
| `src/config/decode.rs` (`decode_include`, `parse`) | `swayward-config/src/decode.rs` | trimmed: swayward's root sections replaced, MRU tracking removed, `warn!` qualified |
| `src/render/memory.rs` | `src/render_helpers/memory.rs` | verbatim |
| `src/render/texture.rs` | `src/render_helpers/texture.rs` | trimmed: `warn!` qualified as `tracing::warn!` |
| `src/render/solid_color.rs` | `src/render_helpers/solid_color.rs` | verbatim |
| `src/render/blur.rs` | `src/render_helpers/blur.rs` | trimmed: tracy spans removed, log macros qualified, paths adjusted |
| `src/render/shader_element.rs` | `src/render_helpers/shader_element.rs` | trimmed: `TtyRenderer` impl and `AsGlesFrame` removed, tracy spans removed |
| `src/render/border.rs` | `src/render_helpers/border.rs` | trimmed: `TtyRenderer` impl removed, `NiriRenderer` → `GlesRenderer`, tracy spans removed |
| `src/render/shadow.rs` | `src/render_helpers/shadow.rs` | trimmed: as `border.rs` |
| `src/render/resources.rs` | `src/render_helpers/resources.rs` | trimmed: tracy span removed, log macros qualified |
| `src/render/shadow_geometry.rs` | `src/layout/shadow.rs` | trimmed: `NiriRenderer` → `GlesRenderer`, config paths adjusted |
| `src/animation/mod.rs` | `src/animation/mod.rs` | trimmed: config paths adjusted, tracy span removed |
| `src/animation/bezier.rs`, `spring.rs` | `src/animation/` | verbatim |
| `src/animation/clock.rs` | `src/animation/clock.rs` | trimmed: `get_monotonic_time` inlined (libc instead of rustix) |
| `src/config/copied/animations/decode.rs` | `swayward-config/src/animations/decode.rs` | trimmed: imports, `decode_node` made public, product name in an error message |
| `src/config/copied/animations/types.rs` | `swayward-config/src/animations/types.rs` | trimmed: only `Animation`, `Kind`, `EasingParams`, `Curve`, `SpringParams` |
| `src/render/shaders/mod.rs` | `src/render_helpers/shaders/mod.rs` | rewritten from the original: only border, shadow and blur programs |
| `src/render/shaders/blur.vert`, `blur_down.frag`, `blur_up.frag`, `border.frag`, `shadow.frag`, `rounding_alpha.frag`, `texture.vert` | `src/render_helpers/shaders/` | verbatim |

Not copied, written for swaywardlock: the widget nodes (`src/config/widgets.rs`), the ring
(`src/render/ring.rs`, a port of swaylock's `render.c`), background fitting
(`src/render/background.rs`, after swaylock's `background-image.c`) and everything else.

## Keeping up

Copies drift on purpose. To check them:

```
contrib/copied-diff [path/to/swayward] [sha]
```

For every file above it reports whether our copy still matches swayward at the source sha (a
`verbatim` copy that differs fails the script), and whether swayward changed the source between
that sha and `sha` (default: the checkout's HEAD). Run it by hand when swayward fixes something in
a copied file; it is not a CI gate. When you take a change, update the source sha here.
