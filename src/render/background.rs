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
