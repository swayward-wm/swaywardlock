//! Rendering: render elements copied from swayward plus swaywardlock's own widget drawing.

#![allow(dead_code)]

pub mod background;
pub mod blur;
pub mod border;
pub mod field_anim;
pub mod input_field;
pub mod memory;
pub mod offscreen;
pub mod postprocess;
pub mod resources;
pub mod ring;
pub mod ring_anim;
pub mod rotated;
pub mod shader_element;
pub mod shaders;
pub mod shadow;
pub mod shadow_geometry;
pub mod shape;
pub mod solid_color;
pub mod text;
pub mod text_shadow;
pub mod texture;

use std::sync::atomic::{AtomicI32, Ordering};

use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};

use self::border::BorderRenderElement;
use self::rotated::RotatedTextureElement;
use self::shadow::ShadowRenderElement;
use self::solid_color::SolidColorRenderElement;
use self::texture::TextureRenderElement;

render_elements! {
    pub LockElement<=GlesRenderer>;
    Solid = SolidColorRenderElement,
    Texture = TextureRenderElement<GlesTexture>,
    Border = BorderRenderElement,
    Shadow = ShadowRenderElement,
    CroppedBorder = CropRenderElement<BorderRenderElement>,
    Rotated = RotatedTextureElement,
}

impl From<shape::Element> for LockElement {
    fn from(e: shape::Element) -> Self {
        match e {
            shape::Element::Border(b) => LockElement::Border(b),
            shape::Element::Shadow(s) => LockElement::Shadow(s),
        }
    }
}

/// GL_MAX_TEXTURE_SIZE, queried once in [`init`]. Unknown (no renderer yet, unit tests) is
/// cairo's own limit.
static MAX_TEXTURE_SIZE: AtomicI32 = AtomicI32::new(32767);

/// Compile shaders and GL resources once on a new renderer, as swayward does.
pub fn init(renderer: &mut GlesRenderer) {
    resources::init(renderer);
    shaders::init(renderer);
    let mut max = 0;
    // SAFETY: a plain query on the renderer's current context.
    let _ = renderer.with_context(|gl| unsafe {
        gl.GetIntegerv(
            smithay::backend::renderer::gles::ffi::MAX_TEXTURE_SIZE,
            &mut max,
        )
    });
    if max > 0 {
        tracing::debug!("GL_MAX_TEXTURE_SIZE {max}");
        MAX_TEXTURE_SIZE.store(max, Ordering::Relaxed);
    }
}

pub fn max_texture_size() -> i32 {
    MAX_TEXTURE_SIZE.load(Ordering::Relaxed)
}

/// Refuse a `w` x `h` texture over the GPU limit, before anything is allocated for it. Over the
/// limit, glTexImage2D fails, smithay's upload still returns Ok, and the empty texture draws as
/// an opaque black quad over the whole frame.
pub fn check_texture_size(w: impl Into<f64>, h: impl Into<f64>) -> anyhow::Result<()> {
    let (w, h) = (w.into(), h.into());
    let max = f64::from(max_texture_size());
    anyhow::ensure!(
        w <= max && h <= max,
        "{}x{} px is over the GPU texture limit ({max} px); not drawn",
        w.ceil(),
        h.ceil()
    );
    Ok(())
}

/// Upload a memory buffer as a texture, failing if the GPU rejected it (smithay does not check
/// glTexImage2D's error, so a failed upload would draw as a black quad).
pub fn upload_memory(
    renderer: &mut GlesRenderer,
    mem: &memory::MemoryBuffer,
) -> anyhow::Result<texture::TextureBuffer<GlesTexture>> {
    use smithay::backend::renderer::gles::ffi;
    let (w, h) = (mem.size().w, mem.size().h);
    check_texture_size(w, h)?;
    // SAFETY: plain error queries on the renderer's current context.
    renderer.with_context(|gl| unsafe { while gl.GetError() != ffi::NO_ERROR {} })?;
    let buf = texture::TextureBuffer::from_memory_buffer(renderer, mem)?;
    let err = renderer.with_context(|gl| unsafe { gl.GetError() })?;
    anyhow::ensure!(
        err == ffi::NO_ERROR,
        "GPU texture upload of {w}x{h} px failed (GL error {err:#x})"
    );
    Ok(buf)
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
