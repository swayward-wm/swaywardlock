//! Rounded rectangles with gradient borders and shadows, drawn with swayward's border and shadow
//! shaders (the same ones that draw swayward's window borders).

use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::border::BorderRenderElement;
use super::shadow::ShadowRenderElement;
use super::shadow_geometry::Shadow as ShadowGeometry;
use crate::config::{Border, Color, CornerRadius, Gradient, Shadow};

/// The render elements for one rounded box, rebuilt when its parameters change.
pub struct RoundedBox {
    params: Option<Params>,
    fill: BorderRenderElement,
    border: BorderRenderElement,
    shadow: ShadowGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub size: Size<f64, Logical>,
    pub fill: Option<Color>,
    pub border: Option<Border>,
    /// `None` means fully round.
    pub radius: Option<CornerRadius>,
    pub shadow: Shadow,
    pub scale: f64,
    pub alpha: f32,
}

/// A box's corner radius: `radius`, or fully round (half the shorter side) when `None`, fitted
/// to `size` like CSS corner overlap.
pub fn resolve_radius(radius: Option<CornerRadius>, size: Size<f64, Logical>) -> CornerRadius {
    radius
        .unwrap_or_else(|| CornerRadius::from((size.w.min(size.h) / 2.) as f32))
        .fit_to(size.w as f32, size.h as f32)
}

impl RoundedBox {
    pub fn new() -> Self {
        Self {
            params: None,
            fill: BorderRenderElement::empty(),
            border: BorderRenderElement::empty(),
            shadow: ShadowGeometry::new(Shadow::default()),
        }
    }

    fn update(&mut self, p: Params) {
        if self.params == Some(p) {
            return;
        }
        self.params = Some(p);
        let full = Rectangle::from_size(p.size);
        let radius = resolve_radius(p.radius, p.size);
        let scale = p.scale as f32;

        if let Some(color) = p.fill {
            let g = Gradient::from(color);
            self.fill.update(
                p.size, full, g.in_, g.from, g.to, 0., full, 0., radius, scale, p.alpha,
            );
        }
        if let Some(b) = p.border {
            // The gradient angle follows CSS: 0 points up, 90 right. The shader's angle is
            // measured from the x axis, so subtract 90°, as swayward does for window borders.
            let angle = (f32::from(b.gradient.angle) - 90.).to_radians();
            self.border.update(
                p.size,
                full,
                b.gradient.in_,
                b.gradient.from,
                b.gradient.to,
                angle,
                full,
                b.width as f32,
                radius,
                scale,
                p.alpha,
            );
        }
        self.shadow.update_config(p.shadow);
        self.shadow
            .update_render_elements(p.size, true, radius, p.scale, p.alpha);
    }

    /// Push elements back to front: shadow, fill, border.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        params: Params,
        location: Point<f64, Logical>,
        push: &mut dyn FnMut(Element),
    ) {
        self.update(params);
        if !BorderRenderElement::has_shader(renderer) {
            return;
        }
        self.shadow
            .render(renderer, location, &mut |e| push(Element::Shadow(e)));
        if params.fill.is_some() {
            push(Element::Border(self.fill.clone().with_location(location)));
        }
        if params.border.is_some() {
            push(Element::Border(self.border.clone().with_location(location)));
        }
    }
}

pub enum Element {
    Border(BorderRenderElement),
    Shadow(ShadowRenderElement),
}
