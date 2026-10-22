//! A label's text shadow, after hyprlock's `Shadowable`: the text's alpha mask, blurred with the
//! background's dual-Kawase blur (`size` plays hyprlock's blur radius), alpha boosted and clamped
//! at 1, drawn in the shadow colour behind the text.
//!
//! hyprlock draws the widget into a framebuffer and blurs that (`blurFB`, with its colorize and
//! boost-alpha finish pass). Here the text is rendered in the shadow colour with the label's alpha,
//! padded so the blur's tail fits, uploaded, blurred, then boosted and tinted in one CPU pass over
//! a read-back of the result. That runs only when what the shadow is made from changes (see
//! [`Key`]), and on at most [`MAX_PIXELS`] px. hyprlock ignores `shadow_color`'s alpha; here it
//! multiplies the shadow's.

use anyhow::Context as _;
use pangocairo::cairo;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ExportMem as _, Offscreen as _};
use smithay::utils::{Buffer, Rectangle, Size, Transform};

use crate::config::{Color, TextAlign, TextShadow};
use crate::render::blur::{Blur, BlurOptions};
use crate::render::memory::MemoryBuffer;
use crate::render::rotated::RotatedTexture;
use crate::render::texture::TextureBuffer;

/// A rendered shadow: the texture (for the plain draw and, wrapped, for the rotated one) and the
/// transparent margin around the text in logical px.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub buf: TextureBuffer<GlesTexture>,
    pub rot: RotatedTexture,
    pub pad: f64,
}

/// Everything a shadow is made from: an equal key gives the same shadow, so it is not blurred
/// again (a relayout keeps it, and a label whose text did not change keeps it).
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub text: String,
    pub font: String,
    pub color: Color,
    pub align: TextAlign,
    pub markup: bool,
    pub shadow: TextShadow,
    pub scale: f64,
}

/// Most pixels a shadow is blurred on. A bigger one (a large label, a wide blur, a high scale)
/// is blurred at 1/2, 1/4, ... resolution and drawn scaled up, so the work (cairo, upload, blur
/// chain, read-back, tint) stays a few milliseconds on llvmpipe whatever the config. 2^19 is
/// 1024 x 512.
pub const MAX_PIXELS: i64 = 1 << 19;

/// Margin on every side of the text, in px of the texture that is blurred, for a blur offset of
/// `offset` px. Twice `offset * 2^passes` for the offset's reach, plus `2^(passes+1)` for the
/// down/up chain's own spread (each level halves the resolution, so an edge spreads by about
/// 2^passes px even at offset 0), plus 2. Measured reach (alpha above 0) beyond a 16 px block:
/// 11 / 24 / 44 / 79 px at offset 3 and 1 / 2 / 3 / 4 passes, 10 at offset 1 and 2 passes, 68 at
/// offset 10 and 2 passes (scratch/shadows/measure-out.txt); at offset 0 the tail reached the
/// edge of a 2 px margin at 2 to 4 passes (scratch/review-shadows/probe2.txt).
pub fn pad(offset: f64, passes: u8) -> i32 {
    let level = f64::from(1u32 << passes.min(20));
    (offset * level * 2.).ceil() as i32 + 2 * level as i32 + 2
}

/// How a shadow is blurred: the texture is the text at 1/`down` resolution plus `pad` px, and is
/// blurred `passes` times with `offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Plan {
    down: i32,
    passes: u8,
    offset: f64,
    pad: i32,
    w: i64,
    h: i64,
}

/// The first plan, from full resolution down by powers of two, whose texture fits `budget` px.
/// Halving the resolution first drops a pass: dual Kawase's first down pass is a halving, so the
/// shorter chain on the half-size copy is the full chain minus its smallest level (offset
/// unchanged, reach in screen px nearly the same). Only past one pass is the offset halved instead.
/// The texture is rounded up to a multiple of 2^passes: the blur textures halve once per pass, and
/// a size that halves exactly keeps the blur from shifting the shadow by a fraction of a texel.
fn plan(text_w: i32, text_h: i32, shadow: &TextShadow, scale: f64, budget: i64) -> Plan {
    let mut down = 1;
    let mut passes = shadow.passes.max(1);
    let mut offset = shadow.size * scale;
    loop {
        let p = pad(offset, passes);
        let align = 1i64 << passes;
        let side = |n: i32| {
            let n = (i64::from(n) + i64::from(down) - 1) / i64::from(down) + 2 * i64::from(p);
            (n + align - 1) / align * align
        };
        let (w, h) = (side(text_w), side(text_h));
        // 2^16 is a backstop: the text is a few px by then and the margin a few dozen.
        if w * h <= budget || down >= 1 << 16 {
            return Plan {
                down,
                passes,
                offset,
                pad: p,
                w,
                h,
            };
        }
        down *= 2;
        if passes > 1 {
            passes -= 1;
        } else {
            offset /= 2.;
        }
    }
}

/// The shadow of `text` (the label's text rendered in the shadow colour, with the label's alpha:
/// only its alpha channel is used). `scale` is the output scale.
pub fn render(
    renderer: &mut GlesRenderer,
    text: &cairo::ImageSurface,
    shadow: &TextShadow,
    scale: f64,
) -> anyhow::Result<Rendered> {
    render_within(renderer, text, shadow, scale, MAX_PIXELS)
}

fn render_within(
    renderer: &mut GlesRenderer,
    text: &cairo::ImageSurface,
    shadow: &TextShadow,
    scale: f64,
    budget: i64,
) -> anyhow::Result<Rendered> {
    let plan = plan(text.width(), text.height(), shadow, scale, budget);
    let (w, h, p) = (plan.w, plan.h, plan.pad);
    anyhow::ensure!(w * h <= budget, "text shadow {w}x{h} px is too large");
    let (w, h) = (w as i32, h as i32);
    let d = f64::from(plan.down);
    // Texture px per logical px.
    let tex_scale = scale / d;
    let started = std::time::Instant::now();

    let padded = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    {
        let cr = cairo::Context::new(&padded)?;
        cr.translate(f64::from(p), f64::from(p));
        cr.scale(1. / d, 1. / d);
        cr.set_source_surface(text, 0., 0.)?;
        cr.paint()?;
    }
    let mem = crate::render::cairo_to_memory(padded, tex_scale)?;
    let source = crate::render::upload_memory(renderer, &mem)?;

    let options = BlurOptions {
        passes: plan.passes,
        offset: plan.offset,
    };
    let mut blur = Blur::new(renderer).context("blur shader unavailable")?;
    blur.prepare_textures(
        |fourcc, s: Size<i32, Buffer>| renderer.create_buffer(fourcc, s),
        source.texture(),
        options,
    )?;
    let blurred = blur.render(renderer, source.texture(), options)?;

    // Read back, boost and tint. GL row order is the order the text was uploaded in (row 0 is
    // the top), so no flip is needed.
    let map = renderer
        .copy_texture(
            &blurred,
            Rectangle::from_size((w, h).into()),
            Fourcc::Abgr8888,
        )
        .context("reading back the blurred shadow")?;
    let data = renderer
        .map_texture(&map)
        .context("mapping the blurred shadow")?;
    let out = tint(data, shadow);
    let buf = crate::render::upload_memory(
        renderer,
        &MemoryBuffer::new(out, Fourcc::Abgr8888, (w, h), tex_scale, Transform::Normal),
    )?;
    let rot = RotatedTexture::new(renderer, buf.texture().clone(), tex_scale);
    tracing::debug!(
        "text shadow: {w}x{h} px at 1/{} resolution, {} passes, in {:?}",
        plan.down,
        plan.passes,
        started.elapsed()
    );
    Ok(Rendered {
        buf,
        rot,
        pad: f64::from(p) / tex_scale,
    })
}

/// Premultiplied RGBA bytes of `shadow`'s colour with alpha = blurred alpha x boost, clamped at 1,
/// x the colour's own alpha. Input: RGBA bytes, only alpha is read.
fn tint(rgba: &[u8], shadow: &TextShadow) -> Vec<u8> {
    let c = shadow.color;
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = (f64::from(px[3]) / 255. * shadow.boost).min(1.) * f64::from(c.a);
        let byte = |v: f32| (f64::from(v) * a * 255.).round() as u8;
        out.extend_from_slice(&[byte(c.r), byte(c.g), byte(c.b), (a * 255.).round() as u8]);
    }
    out
}

#[cfg(test)]
mod tests {
    use smithay::backend::egl::native::EGLSurfacelessDisplay;
    use smithay::backend::egl::{EGLContext, EGLDisplay};
    use smithay::backend::renderer::Texture as _;

    use super::*;

    /// The blur's reach, so the margin is pinned to what was measured (a 16 px block, offset 3),
    /// plus the chain's own spread.
    #[test]
    fn pad_covers_the_blur_reach() {
        assert_eq!(pad(3., 2), 34);
        assert_eq!(pad(3., 1), 18);
        assert_eq!(pad(3., 3), 66);
        assert_eq!(pad(3., 4), 130);
        assert_eq!(pad(1., 2), 18);
        assert_eq!(pad(0.5, 2), 14);
        assert_eq!(pad(10., 2), 90);
        assert_eq!(pad(0., 3), 18);
        for (size, passes, reach) in [(3., 1, 11), (3., 2, 24), (3., 3, 44), (3., 4, 79)] {
            assert!(pad(size, passes) >= reach, "{size} {passes}");
        }
    }

    fn shadow(boost: f64, color: Color) -> TextShadow {
        TextShadow {
            passes: 2,
            size: 3.,
            color,
            boost,
            offset: (0., 0.),
        }
    }

    #[test]
    fn tint_boosts_alpha_clamps_and_premultiplies() {
        let red = Color::from_rgba8_unpremul(255, 0, 0, 255);
        // alpha 100 x 1.2 = 120; alpha 230 x 1.2 clamps at 255; rgb ignored on input.
        let out = tint(&[9, 9, 9, 100, 9, 9, 9, 230, 1, 2, 3, 0], &shadow(1.2, red));
        assert_eq!(out, [120, 0, 0, 120, 255, 0, 0, 255, 0, 0, 0, 0]);
        // The colour's alpha multiplies: half alpha, premultiplied.
        let half = Color::from_rgba8_unpremul(0, 255, 0, 128);
        let out = tint(&[0, 0, 0, 255], &shadow(1., half));
        assert_eq!(out, [0, 128, 0, 128]);
        // boost 0 draws nothing.
        assert_eq!(tint(&[0, 0, 0, 255], &shadow(0., red)), [0, 0, 0, 0]);
    }

    /// A block at the top of the text surface gives a shadow centred above the middle: nothing is
    /// flipped on the way through the GPU and the read-back.
    #[test]
    fn shadow_keeps_the_text_orientation() {
        let Some(mut r) = renderer() else { return };
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 12, 40).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            cr.set_source_rgba(1., 1., 1., 1.);
            cr.rectangle(0., 0., 12., 6.);
            cr.fill().unwrap();
        }
        let white = Color::from_rgba8_unpremul(255, 255, 255, 255);
        let s = render(&mut r, &surface, &shadow(1.2, white), 1.).unwrap();
        let size = s.buf.texture().size();
        let map = r
            .copy_texture(
                s.buf.texture(),
                Rectangle::from_size(size),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let px = r.map_texture(&map).unwrap();
        let (mut sum, mut total) = (0., 0.);
        for y in 0..size.h {
            for x in 0..size.w {
                let a = f64::from(px[((y * size.w + x) * 4 + 3) as usize]);
                sum += a * f64::from(y);
                total += a;
            }
        }
        // The block's centre is 3 px below the text's top, 34 px of margin above it.
        let mean_y = sum / total;
        assert!((mean_y - 37.).abs() < 1.5, "mean row {mean_y}");
    }

    /// Max alpha on the texture's outermost rows and columns: the blur's tail must fade out
    /// inside the margin, never be cut by the texture edge.
    fn edge_alpha(r: &mut GlesRenderer, s: &Rendered) -> u8 {
        let size = s.buf.texture().size();
        let map = r
            .copy_texture(
                s.buf.texture(),
                Rectangle::from_size(size),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let px = r.map_texture(&map).unwrap();
        let a = |x: i32, y: i32| px[((y * size.w + x) * 4 + 3) as usize];
        let rows = (0..size.w).flat_map(|x| [a(x, 0), a(x, size.h - 1)]);
        let cols = (0..size.h).flat_map(|y| [a(0, y), a(size.w - 1, y)]);
        rows.chain(cols).max().unwrap()
    }

    fn block(w: i32, h: i32) -> cairo::ImageSurface {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        cr.set_source_rgba(1., 1., 1., 1.);
        cr.paint().unwrap();
        drop(cr);
        surface
    }

    /// size 0 still blurs (the down/up chain loses resolution: about 2^passes px), so the margin
    /// must hold that tail too: a soft shadow, transparent at the texture edge.
    #[test]
    fn small_size_shadow_has_no_hard_edge() {
        let Some(mut r) = renderer() else { return };
        let white = Color::from_rgba8_unpremul(255, 255, 255, 255);
        for size in [0., 0.1, 0.5] {
            for passes in 1..=4 {
                let sh = TextShadow {
                    passes,
                    size,
                    color: white,
                    boost: 1.,
                    offset: (0., 0.),
                };
                let s = render(&mut r, &block(40, 20), &sh, 1.).unwrap();
                assert_eq!(edge_alpha(&mut r, &s), 0, "size {size} passes {passes}");
            }
        }
    }

    /// The worst shadow the decoder accepts (passes 4, size 20) at scale 2 on a large label is
    /// blurred on a bounded number of pixels, not a texture that grows with 2^passes.
    #[test]
    fn worst_shadow_work_is_bounded() {
        let Some(mut r) = renderer() else { return };
        let sh = TextShadow {
            passes: 4,
            size: 20.,
            color: Color::from_rgba8_unpremul(0, 0, 0, 255),
            boost: 1.2,
            offset: (0., 0.),
        };
        let t = std::time::Instant::now();
        let s = render(&mut r, &block(600, 200), &sh, 2.).unwrap();
        let size = s.buf.texture().size();
        eprintln!("worst: {}x{} in {:?}", size.w, size.h, t.elapsed());
        assert!(
            i64::from(size.w) * i64::from(size.h) <= 1 << 20,
            "{}x{} px",
            size.w,
            size.h
        );
    }

    /// Alpha along the middle row of a shadow, one value per logical px from the text's left
    /// edge minus `reach` to its right edge plus `reach` (nearest texel).
    fn profile(r: &mut GlesRenderer, s: &Rendered, text_w: f64, reach: i32) -> Vec<i32> {
        let size = s.buf.texture().size();
        let map = r
            .copy_texture(
                s.buf.texture(),
                Rectangle::from_size(size),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let px = r.map_texture(&map).unwrap();
        let k = f64::from(size.w) / s.rot.logical_size().w;
        let y = size.h / 2;
        (-reach..text_w as i32 + reach)
            .map(|x| {
                let tx = ((f64::from(x) + s.pad + 0.5) * k) as i32;
                if (0..size.w).contains(&tx) {
                    i32::from(px[((y * size.w + tx) * 4 + 3) as usize])
                } else {
                    0
                }
            })
            .collect()
    }

    /// Over budget, the shadow is blurred at a lower resolution with a pass fewer: it covers
    /// about the same area with about the same strength as the full-resolution one.
    #[test]
    fn downscaled_shadow_matches_full_resolution() {
        let Some(mut r) = renderer() else { return };
        let white = Color::from_rgba8_unpremul(255, 255, 255, 255);
        // Each case keeps the shadow strong enough to measure: at passes 4 size 20 on an 80x30
        // block the peak is 1 of 255 and its mass is rounding noise, which differed by 15
        // percent between Mesa builds. A larger block gives that case a real profile.
        for (passes, size, w, h) in [
            (1, 3., 80, 30),
            (2, 3., 80, 30),
            (3, 3., 80, 30),
            (3, 6., 80, 30),
            (4, 20., 400, 300),
        ] {
            let sh = TextShadow {
                passes,
                size,
                color: white,
                boost: 1.,
                offset: (0., 0.),
            };
            let text = block(w, h);
            let full = render_within(&mut r, &text, &sh, 1., i64::MAX).unwrap();
            let p = plan(w, h, &sh, 1., i64::MAX);
            let budget = p.w * p.h - 1;
            let half = render_within(&mut r, &text, &sh, 1., budget).unwrap();
            assert_eq!(plan(w, h, &sh, 1., budget).down, 2);
            let reach = pad(size, passes);
            let (a, b) = (
                profile(&mut r, &full, f64::from(w), reach),
                profile(&mut r, &half, f64::from(w), reach),
            );
            let extent = |v: &[i32]| v.iter().filter(|&&a| a >= 16).count() as i32;
            let mass = |v: &[i32]| v.iter().sum::<i32>();
            eprintln!(
                "passes {passes} size {size}: extent {} vs {}, mass {} vs {}, peak {} vs {}",
                extent(&a),
                extent(&b),
                mass(&a),
                mass(&b),
                a.iter().max().unwrap(),
                b.iter().max().unwrap()
            );
            assert!(*a.iter().max().unwrap() >= 8, "case too faint to compare");
            assert!((extent(&a) - extent(&b)).abs() <= extent(&a) / 8, "extent");
            assert!((mass(&a) - mass(&b)).abs() <= mass(&a) / 8, "mass");
        }
    }

    /// Mesa's surfaceless EGL, or `None` where there is none (the e2e test covers it there).
    fn renderer() -> Option<GlesRenderer> {
        let made = (|| -> anyhow::Result<GlesRenderer> {
            let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay)? };
            let context = EGLContext::new(&display)?;
            let mut r = unsafe { GlesRenderer::new(context)? };
            crate::render::init(&mut r);
            Ok(r)
        })();
        made.map_err(|err| eprintln!("no surfaceless EGL ({err:#}): skipping"))
            .ok()
    }

    /// A 10x4 opaque block: the shadow is bigger than the block, symmetric about it, and in
    /// the shadow colour, premultiplied.
    #[test]
    fn shadow_surrounds_the_text_in_its_colour() {
        let Some(mut r) = renderer() else { return };
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 10, 4).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            cr.set_source_rgba(1., 0., 0., 1.);
            cr.paint().unwrap();
        }
        let red = Color::from_rgba8_unpremul(255, 0, 0, 255);
        let s = render(&mut r, &surface, &shadow(1.2, red), 1.).unwrap();
        assert_eq!(s.pad, f64::from(pad(3., 2)));
        let size = s.buf.texture().size();
        assert_eq!(size.w % 4, 0);
        assert_eq!(size.h % 4, 0);
        let p = pad(3., 2);
        assert!(size.w >= 10 + 2 * p && size.h >= 4 + 2 * p);
        let map = r
            .copy_texture(
                s.buf.texture(),
                Rectangle::from_size(size),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let px = r.map_texture(&map).unwrap();
        let at = |x: i32, y: i32| &px[((y * size.w + x) * 4) as usize..][..4];
        // Centre of the block, and the shadow's tail beside it: red, premultiplied.
        let (cx, cy) = (p + 5, p + 2);
        assert!(at(cx, cy)[3] > 8, "{:?}", at(cx, cy));
        assert!(at(cx - 9, cy)[3] > 0 && at(cx + 9, cy)[3] > 0);
        assert!(at(cx, cy)
            .iter()
            .zip([1, 0, 0, 1])
            .all(|(&v, k)| k == 1 || v == 0));
        assert_eq!(at(cx, cy)[0], at(cx, cy)[3], "premultiplied red");
        // Nothing at the margin's edge; symmetric left/right and up/down about the block.
        assert_eq!(at(0, 0)[3], 0);
        assert!(at(cx - 7, cy)[3].abs_diff(at(cx + 6, cy)[3]) <= 1);
        assert!(at(cx, cy - 6)[3].abs_diff(at(cx, cy + 5)[3]) <= 1);
    }
}
