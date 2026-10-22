//! Background image loading and fitting.
//!
//! Fit modes follow swaylock's `render_background_image` (`background-image.c`).

use std::path::Path;

use anyhow::Context as _;
use pangocairo::cairo;

use crate::config::{Color, Fit};

/// A decoded image as a premultiplied cairo surface.
pub struct Image(cairo::ImageSurface);

impl Image {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let img = image::ImageReader::open(path)
            .with_context(|| format!("error opening {path:?}"))?
            .with_guessed_format()?
            .decode()
            .with_context(|| format!("error decoding {path:?}"))?
            .into_rgba8();
        let (w, h) = img.dimensions();
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32)?;
        let stride = surface.stride() as usize;
        {
            let mut data = surface.data()?;
            for (y, row) in img.rows().enumerate() {
                for (x, px) in row.enumerate() {
                    let [r, g, b, a] = px.0;
                    let pre = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
                    // cairo ARGB32 is a native-endian u32 0xAARRGGBB.
                    let argb = u32::from_be_bytes([a, pre(r), pre(g), pre(b)]);
                    let o = y * stride + x * 4;
                    data[o..o + 4].copy_from_slice(&argb.to_ne_bytes());
                }
            }
        }
        surface.mark_dirty();
        Ok(Self(surface))
    }
}

/// hyprlock's `FRAGBLURPREPARE`, on the CPU: per-channel gain for contrast, then brighten if
/// `brightness > 1`. Run once per background before the blur.
pub fn prepare(
    surface: &mut cairo::ImageSurface,
    contrast: f64,
    brightness: f64,
) -> anyhow::Result<()> {
    if contrast == 1. && brightness <= 1. {
        return Ok(());
    }
    let gain = |x: f64| {
        let a = 0.5 * (2. * if x < 0.5 { x } else { 1. - x }).powf(contrast);
        if x < 0.5 {
            a
        } else {
            1. - a
        }
    };
    map_rgb(surface, |c| {
        let mut c = if contrast != 1. { gain(c) } else { c };
        if brightness > 1. {
            c *= brightness;
        }
        c
    })
}

/// hyprlock's `FRAGBLURFINISH` brightness: dim if `brightness < 1`.
pub fn finish(surface: &mut cairo::ImageSurface, brightness: f64) -> anyhow::Result<()> {
    if brightness >= 1. {
        return Ok(());
    }
    map_rgb(surface, |c| c * brightness)
}

/// Apply `f` to each colour channel in [0, 1] of a premultiplied ARGB surface.
fn map_rgb(surface: &mut cairo::ImageSurface, f: impl Fn(f64) -> f64) -> anyhow::Result<()> {
    surface.flush();
    let lut: Vec<u8> = (0..=255u16)
        .map(|v| (f(f64::from(v) / 255.).clamp(0., 1.) * 255.).round() as u8)
        .collect();
    {
        let mut data = surface.data()?;
        for px in data.chunks_exact_mut(4) {
            let argb = u32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
            let [a, r, g, b] = argb.to_be_bytes();
            // Keep premultiplication valid.
            let (r, g, b) = (
                lut[r as usize].min(a),
                lut[g as usize].min(a),
                lut[b as usize].min(a),
            );
            px.copy_from_slice(&u32::from_be_bytes([a, r, g, b]).to_ne_bytes());
        }
    }
    surface.mark_dirty();
    Ok(())
}

/// Paint `color`, then `image` fitted per `fit`, into a `width` x `height` buffer.
pub fn render(
    color: Color,
    image: Option<&Image>,
    fit: Fit,
    width: i32,
    height: i32,
) -> anyhow::Result<cairo::ImageSurface> {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(
        color.r.into(),
        color.g.into(),
        color.b.into(),
        color.a.into(),
    );
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);

    if let Some(Image(image)) = image {
        let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
        let (bw, bh) = (f64::from(width), f64::from(height));
        cr.save()?;
        match fit {
            Fit::Fill => {
                cr.scale(bw / iw, bh / ih);
                cr.set_source_surface(image, 0., 0.)?;
            }
            Fit::Cover | Fit::Contain => {
                let wider = bw / bh > iw / ih;
                // Cover scales to the axis that overflows; contain to the one that fits.
                let by_width = wider == (fit == Fit::Cover);
                let s = if by_width { bw / iw } else { bh / ih };
                cr.scale(s, s);
                cr.set_source_surface(image, (bw / s - iw) / 2., (bh / s - ih) / 2.)?;
            }
            Fit::Center => {
                // Whole pixels keep odd-sized images sharp, as in swaylock.
                cr.set_source_surface(image, ((bw - iw) / 2.).trunc(), ((bh - ih) / 2.).trunc())?;
            }
            Fit::Tile => {
                let pattern = cairo::SurfacePattern::create(image);
                pattern.set_extend(cairo::Extend::Repeat);
                cr.set_source(&pattern)?;
            }
        }
        cr.paint()?;
        cr.restore()?;
    }

    drop(cr);
    Ok(surface)
}

impl Image {
    /// Wrap a screenshot (premultiplied ARGB rows, cairo's layout).
    pub fn from_argb(width: u32, height: u32, data: &[u8]) -> anyhow::Result<Self> {
        let mut surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, width as i32, height as i32)?;
        let stride = surface.stride() as usize;
        let row = width as usize * 4;
        {
            let mut dst = surface.data()?;
            for y in 0..height as usize {
                dst[y * stride..][..row].copy_from_slice(&data[y * row..][..row]);
            }
        }
        surface.mark_dirty();
        Ok(Self(surface))
    }

    pub fn size(&self) -> (i32, i32) {
        (self.0.width(), self.0.height())
    }
}

/// Draw `image` scaled to cover `w` x `h`, clipped to a rounded rectangle with `radius` (four
/// corners, already fitted). For the `image` widget; the border is drawn separately by the
/// border shader so it matches swayward's.
pub fn render_clipped(
    image: &Image,
    w: i32,
    h: i32,
    radius: [f64; 4],
) -> anyhow::Result<cairo::ImageSurface> {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    let cr = cairo::Context::new(&surface)?;
    let (bw, bh) = (f64::from(w), f64::from(h));
    let [tl, tr, br, bl] = radius;
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(bw - tr, tr, tr, -FRAC_PI_2, 0.);
    cr.arc(bw - br, bh - br, br, 0., FRAC_PI_2);
    cr.arc(bl, bh - bl, bl, FRAC_PI_2, PI);
    cr.arc(tl, tl, tl, PI, 3. * FRAC_PI_2);
    cr.close_path();
    cr.clip();
    let (iw, ih) = (f64::from(image.0.width()), f64::from(image.0.height()));
    let s = (bw / iw).max(bh / ih);
    cr.scale(s, s);
    cr.set_source_surface(&image.0, (bw / s - iw) / 2., (bh / s - ih) / 2.)?;
    cr.paint()?;
    drop(cr);
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(surface: &mut cairo::ImageSurface, x: usize, y: usize) -> [u8; 4] {
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        let p = &data[y * stride + x * 4..][..4];
        let [b, g, r, a] = u32::from_ne_bytes([p[0], p[1], p[2], p[3]]).to_le_bytes();
        [r, g, b, a]
    }

    /// A 20x10 image: red on the left half, blue on the right. Big enough that bilinear
    /// filtering at the seam does not reach the sampled pixels.
    fn red_blue(dir: &Path) -> Image {
        let path = dir.join("rb.png");
        let img = image::RgbaImage::from_fn(20, 10, |x, _| {
            if x < 10 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        img.save(&path).unwrap();
        Image::load(&path).unwrap()
    }

    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn prepare_and_finish_follow_hyprlock() {
        let c = Color::from_rgba8_unpremul(64, 128, 192, 255);
        let mut s = render(c, None, Fit::Cover, 2, 2).unwrap();
        // gain(x, k): k > 1 pushes values away from the middle.
        prepare(&mut s, 2., 1.).unwrap();
        let [r, g, b, _] = pixel(&mut s, 0, 0);
        assert!(r < 64 && b > 192, "contrast widens: {r} {g} {b}");
        assert!((i32::from(g) - 128).abs() <= 1, "0.5 is a fixed point: {g}");
        let mut s = render(c, None, Fit::Cover, 2, 2).unwrap();
        finish(&mut s, 0.5).unwrap();
        assert_eq!(pixel(&mut s, 0, 0), [32, 64, 96, 255]);
        // Brightness above 1 only applies before the blur, below 1 only after.
        let mut s = render(c, None, Fit::Cover, 2, 2).unwrap();
        finish(&mut s, 2.).unwrap();
        prepare(&mut s, 1., 0.5).unwrap();
        assert_eq!(pixel(&mut s, 0, 0), [64, 128, 192, 255]);
        // And brightness above 1 does brighten (clamped at 1).
        let mut s = render(c, None, Fit::Cover, 2, 2).unwrap();
        prepare(&mut s, 1., 2.).unwrap();
        assert_eq!(pixel(&mut s, 0, 0), [128, 255, 255, 255]);
    }

    #[test]
    fn colour_only() {
        let c = Color::from_rgba8_unpremul(1, 2, 3, 255);
        let mut s = render(c, None, Fit::Cover, 4, 4).unwrap();
        assert_eq!(pixel(&mut s, 2, 2), [1, 2, 3, 255]);
    }

    #[test]
    fn fit_modes() {
        let dir = tempfile::tempdir().unwrap();
        let img = red_blue(dir.path());
        let black = Color::from_rgba8_unpremul(0, 0, 0, 255);

        // Contain into 40x40: scaled by 2 to fill the width, letterboxed to rows 10..30.
        let mut s = render(black, Some(&img), Fit::Contain, 40, 40).unwrap();
        assert_eq!(pixel(&mut s, 20, 3), BLACK, "letterbox");
        assert_eq!(pixel(&mut s, 5, 20), RED);
        assert_eq!(pixel(&mut s, 35, 20), BLUE);

        // Cover into 40x40: scaled by 4 to fill the height (80 wide), centred, so 20 columns
        // are cropped on each side and nothing is letterboxed.
        let mut s = render(black, Some(&img), Fit::Cover, 40, 40).unwrap();
        assert_eq!(pixel(&mut s, 5, 2), RED);
        assert_eq!(pixel(&mut s, 35, 37), BLUE);

        // Fill stretches to the exact size.
        let mut s = render(black, Some(&img), Fit::Fill, 40, 40).unwrap();
        assert_eq!(pixel(&mut s, 5, 2), RED);
        assert_eq!(pixel(&mut s, 35, 2), BLUE);

        // Tile repeats at natural size: column 25 is column 5 of the second tile.
        let mut s = render(black, Some(&img), Fit::Tile, 40, 20).unwrap();
        assert_eq!(pixel(&mut s, 25, 15), RED);
        assert_eq!(pixel(&mut s, 35, 15), BLUE);

        // Center at natural size: 20x10 in 40x30 sits at (10, 10); the rest is background.
        let mut s = render(black, Some(&img), Fit::Center, 40, 30).unwrap();
        assert_eq!(pixel(&mut s, 12, 15), RED);
        assert_eq!(pixel(&mut s, 5, 5), BLACK);
    }
}
