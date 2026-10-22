//! Rendering: render elements copied from swayward plus swaywardlock's own widget drawing.

#![allow(dead_code)]

pub mod background;
pub mod blur;
pub mod memory;
pub mod ring;
pub mod solid_color;
pub mod texture;

use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};

use self::solid_color::SolidColorRenderElement;
use self::texture::TextureRenderElement;

render_elements! {
    pub LockElement<=GlesRenderer>;
    Solid = SolidColorRenderElement,
    Texture = TextureRenderElement<GlesTexture>,
}

/// Convert an unpremultiplied ARGB cairo surface into a [`memory::MemoryBuffer`].
///
/// cairo's `ARgb32` is premultiplied BGRA in memory on little-endian, which is DRM `Argb8888`.
pub fn cairo_to_memory(
    mut surface: pangocairo::cairo::ImageSurface,
    scale: f64,
) -> anyhow::Result<memory::MemoryBuffer> {
    use smithay::backend::allocator::Fourcc;
    use smithay::utils::Transform;

    surface.flush();
    let (w, h) = (surface.width(), surface.height());
    let stride = surface.stride();
    anyhow::ensure!(
        stride == w * 4,
        "unexpected cairo stride {stride} for width {w}"
    );
    let data = surface
        .data()
        .map_err(|e| anyhow::anyhow!("cairo surface data: {e}"))?
        .to_vec();
    Ok(memory::MemoryBuffer::new(
        data,
        Fourcc::Argb8888,
        (w, h),
        scale,
        Transform::Normal,
    ))
}
