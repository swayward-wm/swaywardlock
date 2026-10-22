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
| `src/config/decode.rs` (`decode_include`, `parse`) | `swayward-config/src/decode.rs` | trimmed: swayward's root sections replaced, MRU tracking removed, `warn!` qualified |
| `src/render/memory.rs` | `src/render_helpers/memory.rs` | verbatim |
| `src/render/texture.rs` | `src/render_helpers/texture.rs` | trimmed: `warn!` qualified as `tracing::warn!` |
| `src/render/solid_color.rs` | `src/render_helpers/solid_color.rs` | verbatim |
| `src/render/blur.rs` | `src/render_helpers/blur.rs` | trimmed: `Blur::new` takes the program instead of swayward's `Shaders` cache; tracy spans removed; log macros qualified |
| `src/render/shaders/blur.vert`, `blur_down.frag`, `blur_up.frag` | `src/render_helpers/shaders/` | verbatim |

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
