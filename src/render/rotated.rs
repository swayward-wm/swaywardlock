//! A texture drawn rotated by an arbitrary angle (hyprlock's `rotate` on labels, images and
//! shapes). smithay's `Transform` only has the eight 90-degree variants, so this element calls
//! `GlesFrame::render_texture` with its own texture matrix.
//!
//! The rotation lives in the texture matrix, not the vertex matrix: the quads drawn are the
//! damage rectangles in output pixels, exactly as `render_texture_from_to` draws them, and the
//! texture matrix maps each output pixel back into the unrotated texture. Nothing outside the
//! damage is blended twice. Outside the rotated texture the coordinates leave 0..1 and
//! CLAMP_TO_EDGE (smithay's wrap mode) repeats the edge texels, so every texture drawn here must
//! have a transparent border at least one pixel wide; bilinear filtering across that border is
//! what antialiases the rotated edges.

use glam::{Affine2, Mat2, Mat3, Vec2};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{ffi, GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::backend::renderer::{ContextId, Frame as _, Renderer as _, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size};

/// A texture with a transparent border, ready to be drawn rotated. Clones share the texture and
/// the damage identity.
#[derive(Debug, Clone)]
pub struct RotatedTexture {
    id: Id,
    commit: CommitCounter,
    context: ContextId<GlesTexture>,
    texture: GlesTexture,
    /// Physical pixels per logical pixel the texture was rendered at.
    scale: f64,
}

impl RotatedTexture {
    pub fn new(renderer: &GlesRenderer, texture: GlesTexture, scale: f64) -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            context: renderer.context_id(),
            texture,
            scale,
        }
    }

    /// The whole texture's size in logical pixels.
    pub fn logical_size(&self) -> Size<f64, Logical> {
        let s = self.texture.size();
        Size::from((f64::from(s.w) / self.scale, f64::from(s.h) / self.scale))
    }
}

/// [`RotatedTexture`] drawn with its unrotated top-left at `loc`, turned by `angle` radians
/// counter-clockwise on screen about `pivot` (both logical, y down).
#[derive(Debug, Clone)]
pub struct RotatedTextureElement {
    tex: RotatedTexture,
    /// Output scale; geometry and the texture matrix are in physical pixels.
    scale: f64,
    loc: Point<f64, Logical>,
    pivot: Point<f64, Logical>,
    angle: f64,
    alpha: f32,
}

impl RotatedTextureElement {
    pub fn new(
        tex: RotatedTexture,
        loc: Point<f64, Logical>,
        pivot: Point<f64, Logical>,
        angle: f64,
        scale: f64,
        alpha: f32,
    ) -> Self {
        Self {
            tex,
            scale,
            loc,
            pivot,
            angle,
            alpha,
        }
    }

    fn size(&self) -> Size<f64, Logical> {
        self.tex.logical_size()
    }

    fn phys(&self, p: Point<f64, Logical>) -> Vec2 {
        Vec2::new((p.x * self.scale) as f32, (p.y * self.scale) as f32)
    }
}

/// Rotation by `angle` radians counter-clockwise on a y-down screen. On a y-down screen a
/// counter-clockwise turn takes +x towards -y, so this is the standard (y-up) rotation by
/// `-angle`.
fn screen_rotation(angle: f64) -> Mat2 {
    Mat2::from_angle(-(angle as f32))
}

/// Where the quad `loc`..`loc + size` lands on screen, rotated about `pivot`: its four corners.
pub fn rotated_corners(loc: Vec2, size: Vec2, pivot: Vec2, angle: f64) -> [Vec2; 4] {
    let r = screen_rotation(angle);
    [
        Vec2::ZERO,
        Vec2::new(size.x, 0.),
        Vec2::new(0., size.y),
        size,
    ]
    .map(|c| pivot + r * (loc + c - pivot))
}

/// Maps an output pixel position to texture coordinates (0..1 over the texture) of the quad
/// `loc`..`loc + size` turned by `angle` about `pivot`: the inverse of [`rotated_corners`],
/// then normalised. `y_inverted` flips the result for textures stored bottom-up.
pub fn texture_matrix(loc: Vec2, size: Vec2, pivot: Vec2, angle: f64, y_inverted: bool) -> Mat3 {
    let unrotate = Affine2::from_translation(pivot)
        * Affine2::from_mat2(screen_rotation(angle).inverse())
        * Affine2::from_translation(-pivot);
    let mut m = Affine2::from_scale(Vec2::ONE / size) * Affine2::from_translation(-loc) * unrotate;
    if y_inverted {
        m = Affine2::from_cols_array(&[1., 0., 0., -1., 0., 1.]) * m;
    }
    m.into()
}

impl Element for RotatedTextureElement {
    fn id(&self) -> &Id {
        &self.tex.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.tex.commit
    }

    /// The rotated quad's bounding box, out to whole pixels.
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let size = self.size();
        let corners = rotated_corners(
            self.phys(self.loc),
            self.phys(Point::from((size.w, size.h))),
            self.phys(self.pivot),
            self.angle,
        );
        let min = corners
            .iter()
            .fold(Vec2::INFINITY, |a, c| a.min(*c))
            .floor();
        let max = corners
            .iter()
            .fold(Vec2::NEG_INFINITY, |a, c| a.max(*c))
            .ceil();
        if !(min.is_finite() && max.is_finite()) {
            return Rectangle::default();
        }
        Rectangle::from_extremities((min.x as i32, min.y as i32), (max.x as i32, max.y as i32))
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.tex.texture.size()).to_f64()
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for RotatedTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        if frame.context_id() != self.tex.context {
            tracing::warn!("trying to render texture from different renderer");
            return Ok(());
        }
        // Damage arrives relative to `dest`; draw exactly those rectangles, in output pixels.
        let bounds = Rectangle::from_size(dest.size);
        let instances: Vec<f32> = damage
            .iter()
            .filter_map(|r| r.intersection(bounds))
            .flat_map(|r| {
                [
                    (dest.loc.x + r.loc.x) as f32,
                    (dest.loc.y + r.loc.y) as f32,
                    r.size.w as f32,
                    r.size.h as f32,
                ]
            })
            .collect();
        if instances.is_empty() {
            return Ok(());
        }
        // Uploads get CLAMP_TO_EDGE from smithay, offscreen textures keep GL's default REPEAT,
        // which would tile the texture over the bounding box.
        let id = self.tex.texture.tex_id();
        // SAFETY: binds our own live texture and sets its wrap mode; render_texture binds the
        // texture again itself.
        // A GL error here or in render_texture must not fail the whole frame (every other
        // widget and the background would be lost): log it and skip this widget.
        let wrap = frame.with_context(|gl| unsafe {
            gl.BindTexture(ffi::TEXTURE_2D, id);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
        });
        if let Err(err) = wrap {
            tracing::warn!("rotate: cannot set the texture wrap mode: {err}");
            return Ok(());
        }
        let size = self.size();
        let tex_matrix = texture_matrix(
            self.phys(self.loc),
            self.phys(Point::from((size.w, size.h))),
            self.phys(self.pivot),
            self.angle,
            self.tex.texture.is_y_inverted(),
        );
        if let Err(err) = frame.render_texture(
            &self.tex.texture,
            tex_matrix,
            Mat3::IDENTITY,
            Some(instances),
            self.alpha,
            None,
            &[],
        ) {
            tracing::warn!("rotate: cannot draw a rotated widget: {err}");
        }
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-4
    }

    /// A 100x20 quad at the origin turned 90 degrees about its centre (50, 10): its top-right
    /// corner (100, 0) swings up to (40, -40), so the right end points up (counter-clockwise).
    #[test]
    fn ninety_degrees_turns_the_right_end_up() {
        let (loc, size, pivot) = (Vec2::ZERO, Vec2::new(100., 20.), Vec2::new(50., 10.));
        let angle = 90f64.to_radians();
        let [tl, tr, bl, br] = rotated_corners(loc, size, pivot, angle);
        assert!(close(tr, Vec2::new(40., -40.)), "{tr}");
        assert!(close(tl, Vec2::new(40., 60.)), "{tl}");
        assert!(close(bl, Vec2::new(60., 60.)), "{bl}");
        assert!(close(br, Vec2::new(60., -40.)), "{br}");

        // The texture matrix undoes it: the screen point (40, -40) samples texture (1, 0).
        let m = texture_matrix(loc, size, pivot, angle, false);
        assert!(close(m.transform_point2(tr), Vec2::new(1., 0.)));
        assert!(close(m.transform_point2(bl), Vec2::new(0., 1.)));
        assert!(close(m.transform_point2(pivot), Vec2::new(0.5, 0.5)));
        // A bottom-up texture samples the mirrored row.
        let flipped = texture_matrix(loc, size, pivot, angle, true);
        assert!(close(flipped.transform_point2(tr), Vec2::new(1., 1.)));
    }

    /// Angle 0 is a plain placement, and the pivot does not matter.
    #[test]
    fn zero_degrees_is_a_translation() {
        let (loc, size) = (Vec2::new(10., 30.), Vec2::new(40., 20.));
        let m = texture_matrix(loc, size, Vec2::new(-7., 99.), 0., false);
        assert!(close(m.transform_point2(loc), Vec2::ZERO));
        assert!(close(m.transform_point2(loc + size), Vec2::ONE));
    }
}
