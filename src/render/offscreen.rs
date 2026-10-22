//! Render elements into a texture instead of the output: a widget drawn rotated is first drawn
//! unrotated here, then turned as one texture (as hyprlock renders a rotated shape or image into
//! a framebuffer first).

use anyhow::Context as _;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{Element, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesRenderer, GlesTarget, GlesTexture};
use smithay::backend::renderer::{Bind as _, Color32F, Frame as _, Offscreen as _, Renderer as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Physical, Rectangle, Scale, Size, Transform};

pub fn encompassing_geo(
    scale: Scale<f64>,
    elements: impl Iterator<Item = impl Element>,
) -> Rectangle<i32, Physical> {
    elements
        .map(|ele| ele.geometry(scale))
        .reduce(|a, b| a.merge(b))
        .unwrap_or_default()
}

pub fn create_texture(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    fourcc: Fourcc,
) -> Result<GlesTexture, GlesError> {
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    renderer.create_buffer(fourcc, buffer_size)
}

/// Render `elements` (back to front) into a texture covering all of them plus `pad` transparent
/// pixels on every side. Returns the texture and where its top-left sits in the elements'
/// physical coordinates.
pub fn render_to_encompassing_texture(
    renderer: &mut GlesRenderer,
    scale: Scale<f64>,
    pad: i32,
    elements: &[impl RenderElement<GlesRenderer>],
) -> anyhow::Result<(GlesTexture, Rectangle<i32, Physical>)> {
    let mut geo = encompassing_geo(scale, elements.iter());
    anyhow::ensure!(!geo.is_empty(), "nothing to draw");
    geo.loc -= (pad, pad).into();
    geo.size += (2 * pad, 2 * pad).into();
    let elements = elements.iter().map(|ele| {
        RelocateRenderElement::from_element(ele, geo.loc.upscale(-1), Relocate::Relative)
    });

    let texture = render_to_texture(renderer, geo.size, scale, Fourcc::Abgr8888, elements)?;

    Ok((texture, geo))
}

pub fn render_to_texture(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    fourcc: Fourcc,
    elements: impl Iterator<Item = impl RenderElement<GlesRenderer>>,
) -> anyhow::Result<GlesTexture> {
    // Over the GPU limit glTexImage2D fails and smithay still hands back a texture; refuse first.
    super::check_texture_size(size.w, size.h)?;
    // SAFETY: plain error queries on the renderer's current context.
    renderer.with_context(|gl| unsafe {
        while gl.GetError() != smithay::backend::renderer::gles::ffi::NO_ERROR {}
    })?;
    let mut texture = create_texture(renderer, size, fourcc).context("error creating texture")?;
    let err = renderer.with_context(|gl| unsafe { gl.GetError() })?;
    anyhow::ensure!(
        err == smithay::backend::renderer::gles::ffi::NO_ERROR,
        "creating a {}x{} px texture failed (GL error {err:#x})",
        size.w,
        size.h
    );

    {
        let mut target = renderer
            .bind(&mut texture)
            .context("error binding texture")?;

        render_elements(renderer, &mut target, size, scale, elements)?;
    }

    Ok(texture)
}

fn render_elements(
    renderer: &mut GlesRenderer,
    target: &mut GlesTarget,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    elements: impl Iterator<Item = impl RenderElement<GlesRenderer>>,
) -> anyhow::Result<()> {
    let output_rect = Rectangle::from_size(size);

    let mut frame = renderer
        .render(target, size, Transform::Normal)
        .context("error starting frame")?;

    frame
        .clear(Color32F::TRANSPARENT, &[output_rect])
        .context("error clearing")?;

    for element in elements {
        let src = element.src();
        let dst = element.geometry(scale);

        if let Some(mut damage) = output_rect.intersection(dst) {
            damage.loc -= dst.loc;

            let cache = UserDataMap::new();
            if element.is_framebuffer_effect() {
                element
                    .capture_framebuffer(&mut frame, src, dst, &cache)
                    .context("error in capture_framebuffer()")?;
            }
            element
                .draw(&mut frame, src, dst, &[damage], &[], Some(&cache))
                .context("error drawing element")?;
        }
    }

    // The texture is sampled by later draws on the same context, so GL orders them; the sync
    // point is only needed across contexts.
    let _sync = frame.finish().context("error finishing frame")?;
    Ok(())
}
