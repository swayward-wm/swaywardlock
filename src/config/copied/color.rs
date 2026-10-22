use std::ops::{Mul, MulAssign};
use std::str::FromStr;

use knuffel::errors::DecodeError;
use miette::{miette, IntoDiagnostic as _};
use smithay::backend::renderer::Color32F;

/// RGB color in [0, 1] with unpremultiplied alpha.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new_unpremul(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgba8_unpremul(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::from_array_unpremul([r, g, b, a].map(|x| x as f32 / 255.))
    }

    pub fn from_array_premul([r, g, b, a]: [f32; 4]) -> Self {
        let a = a.clamp(0., 1.);

        if a == 0. {
            Self::new_unpremul(0., 0., 0., 0.)
        } else {
            Self {
                r: (r / a).clamp(0., 1.),
                g: (g / a).clamp(0., 1.),
                b: (b / a).clamp(0., 1.),
                a,
            }
        }
    }

    pub const fn from_array_unpremul([r, g, b, a]: [f32; 4]) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_color32f(color: Color32F) -> Self {
        Self::from_array_premul(color.components())
    }

    pub fn to_array_unpremul(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn to_array_premul(self) -> [f32; 4] {
        let [r, g, b, a] = [self.r, self.g, self.b, self.a];
        [r * a, g * a, b * a, a]
    }
}

impl Mul<f32> for Color {
    type Output = Self;

    fn mul(mut self, rhs: f32) -> Self::Output {
        self.a *= rhs;
        self
    }
}

impl MulAssign<f32> for Color {
    fn mul_assign(&mut self, rhs: f32) {
        self.a *= rhs;
    }
}

impl From<Color> for Color32F {
    fn from(value: Color) -> Self {
        Color32F::from(value.to_array_premul())
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct Gradient {
    #[knuffel(property, str)]
    pub from: Color,
    #[knuffel(property, str)]
    pub to: Color,
    #[knuffel(property, default = 180)]
    pub angle: i16,
    #[knuffel(property(name = "in"), str, default)]
    pub in_: GradientInterpolation,
}

impl From<Color> for Gradient {
    fn from(value: Color) -> Self {
        Self {
            from: value,
            to: value,
            angle: 0,
            in_: GradientInterpolation::default(),
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub struct GradientInterpolation {
    pub color_space: GradientColorSpace,
    pub hue_interpolation: HueInterpolation,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GradientColorSpace {
    #[default]
    Srgb,
    SrgbLinear,
    Oklab,
    Oklch,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HueInterpolation {
    #[default]
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CornerRadius {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl From<CornerRadius> for [f32; 4] {
    fn from(value: CornerRadius) -> Self {
        [
            value.top_left,
            value.top_right,
            value.bottom_right,
            value.bottom_left,
        ]
    }
}

impl From<f32> for CornerRadius {
    fn from(value: f32) -> Self {
        Self {
            top_left: value,
            top_right: value,
            bottom_right: value,
            bottom_left: value,
        }
    }
}

impl CornerRadius {
    pub fn fit_to(self, width: f32, height: f32) -> Self {
        // Like in CSS: https://drafts.csswg.org/css-backgrounds/#corner-overlap
        let reduction = f32::min(
            f32::min(
                width / (self.top_left + self.top_right),
                width / (self.bottom_left + self.bottom_right),
            ),
            f32::min(
                height / (self.top_left + self.bottom_left),
                height / (self.top_right + self.bottom_right),
            ),
        );
        let reduction = f32::min(1., reduction);

        Self {
            top_left: self.top_left * reduction,
            top_right: self.top_right * reduction,
            bottom_right: self.bottom_right * reduction,
            bottom_left: self.bottom_left * reduction,
        }
    }

    pub fn expanded_by(mut self, width: f32) -> Self {
        // Radius = 0 is preserved, so that square corners remain square.
        if self.top_left > 0. {
            self.top_left += width;
        }
        if self.top_right > 0. {
            self.top_right += width;
        }
        if self.bottom_right > 0. {
            self.bottom_right += width;
        }
        if self.bottom_left > 0. {
            self.bottom_left += width;
        }

        if width < 0. {
            self.top_left = self.top_left.max(0.);
            self.top_right = self.top_right.max(0.);
            self.bottom_left = self.bottom_left.max(0.);
            self.bottom_right = self.bottom_right.max(0.);
        }

        self
    }

    pub fn scaled_by(self, scale: f32) -> Self {
        Self {
            top_left: self.top_left * scale,
            top_right: self.top_right * scale,
            bottom_right: self.bottom_right * scale,
            bottom_left: self.bottom_left * scale,
        }
    }
}

impl FromStr for GradientInterpolation {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut iter = s.split_whitespace();
        let in_part1 = iter.next();
        let in_part2 = iter.next();
        let in_part3 = iter.next();

        let Some(in_part1) = in_part1 else {
            return Err(miette!("missing color space"));
        };

        let color = match in_part1 {
            "srgb" => GradientColorSpace::Srgb,
            "srgb-linear" => GradientColorSpace::SrgbLinear,
            "oklab" => GradientColorSpace::Oklab,
            "oklch" => GradientColorSpace::Oklch,
            x => {
                return Err(miette!(
                    "invalid color space {x}; can be srgb, srgb-linear, oklab or oklch"
                ))
            }
        };

        let interpolation = if let Some(in_part2) = in_part2 {
            if color != GradientColorSpace::Oklch {
                return Err(miette!("only oklch color space can have hue interpolation"));
            }

            if in_part3 != Some("hue") {
                return Err(miette!(
                    "interpolation must end with \"hue\", like \"oklch shorter hue\""
                ));
            } else if iter.next().is_some() {
                return Err(miette!("unexpected text after hue interpolation"));
            } else {
                match in_part2 {
                    "shorter" => HueInterpolation::Shorter,
                    "longer" => HueInterpolation::Longer,
                    "increasing" => HueInterpolation::Increasing,
                    "decreasing" => HueInterpolation::Decreasing,
                    x => {
                        return Err(miette!(
                            "invalid hue interpolation {x}; \
                             can be shorter, longer, increasing, decreasing"
                        ))
                    }
                }
            }
        } else {
            HueInterpolation::default()
        };

        Ok(Self {
            color_space: color,
            hue_interpolation: interpolation,
        })
    }
}

impl FromStr for Color {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let color = csscolorparser::parse(s)
            .into_diagnostic()?
            .clamp()
            .to_array();
        Ok(Self::from_array_unpremul(color))
    }
}

#[derive(knuffel::Decode)]
struct ColorRgba {
    #[knuffel(argument)]
    r: u8,
    #[knuffel(argument)]
    g: u8,
    #[knuffel(argument)]
    b: u8,
    #[knuffel(argument)]
    a: u8,
}

impl From<ColorRgba> for Color {
    fn from(value: ColorRgba) -> Self {
        let ColorRgba { r, g, b, a } = value;
        Self::from_array_unpremul([r, g, b, a].map(|x| x as f32 / 255.))
    }
}

// Manual impl to allow both one-argument string and 4-argument RGBA forms.
impl<S> knuffel::Decode<S> for Color
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        // Check for unexpected type name.
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        // Get the first argument.
        let mut iter_args = node.arguments.iter();
        let val = iter_args
            .next()
            .ok_or_else(|| DecodeError::missing(node, "additional argument is required"))?;

        // Check for unexpected type name.
        if let Some(typ) = &val.type_name {
            ctx.emit_error(DecodeError::TypeName {
                span: typ.span().clone(),
                found: Some((**typ).clone()),
                expected: knuffel::errors::ExpectedType::no_type(),
                rust_type: "str",
            });
        }

        // Check the argument type.
        let rv = match *val.literal {
            // If it's a string, use FromStr.
            knuffel::ast::Literal::String(ref s) => {
                Color::from_str(s).map_err(|e| DecodeError::conversion(&val.literal, e))
            }
            // Otherwise, fall back to the 4-argument RGBA form.
            _ => return ColorRgba::decode_node(node, ctx).map(Color::from),
        }?;

        // Check for unexpected following arguments.
        if let Some(val) = iter_args.next() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "unexpected argument",
            ));
        }

        // Check for unexpected properties and children.
        for name in node.properties.keys() {
            ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                format!("unexpected property `{}`", name.escape_default()),
            ));
        }
        for child in node.children.as_ref().map(|lst| &lst[..]).unwrap_or(&[]) {
            ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", child.node_name.escape_default()),
            ));
        }

        Ok(rv)
    }
}

impl<S> knuffel::Decode<S> for CornerRadius
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        // Check for unexpected type name.
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        let decode_radius = |ctx: &mut knuffel::decode::Context<S>,
                             val: &knuffel::ast::Value<S>| {
            // Check for unexpected type name.
            if let Some(typ) = &val.type_name {
                ctx.emit_error(DecodeError::TypeName {
                    span: typ.span().clone(),
                    found: Some((**typ).clone()),
                    expected: knuffel::errors::ExpectedType::no_type(),
                    rust_type: "str",
                });
            }

            // Decode both integers and floats.
            let radius = match *val.literal {
                knuffel::ast::Literal::Int(ref x) => f32::from(match x.try_into() {
                    Ok(x) => x,
                    Err(err) => {
                        ctx.emit_error(DecodeError::conversion(&val.literal, err));
                        0i16
                    }
                }),
                knuffel::ast::Literal::Decimal(ref x) => match x.try_into() {
                    Ok(x) => x,
                    Err(err) => {
                        ctx.emit_error(DecodeError::conversion(&val.literal, err));
                        0.
                    }
                },
                _ => {
                    ctx.emit_error(DecodeError::scalar_kind(
                        knuffel::decode::Kind::Int,
                        &val.literal,
                    ));
                    0.
                }
            };

            if radius < 0. {
                ctx.emit_error(DecodeError::conversion(&val.literal, "radius must be >= 0"));
            }

            radius
        };

        // Get the first argument.
        let mut iter_args = node.arguments.iter();
        let val = iter_args
            .next()
            .ok_or_else(|| DecodeError::missing(node, "additional argument is required"))?;

        let top_left = decode_radius(ctx, val);

        let mut rv = CornerRadius {
            top_left,
            top_right: top_left,
            bottom_right: top_left,
            bottom_left: top_left,
        };

        if let Some(val) = iter_args.next() {
            rv.top_right = decode_radius(ctx, val);

            let val = iter_args.next().ok_or_else(|| {
                DecodeError::missing(node, "either 1 or 4 arguments are required")
            })?;
            rv.bottom_right = decode_radius(ctx, val);

            let val = iter_args.next().ok_or_else(|| {
                DecodeError::missing(node, "either 1 or 4 arguments are required")
            })?;
            rv.bottom_left = decode_radius(ctx, val);

            // Check for unexpected following arguments.
            if let Some(val) = iter_args.next() {
                ctx.emit_error(DecodeError::unexpected(
                    &val.literal,
                    "argument",
                    "unexpected argument",
                ));
            }
        }

        // Check for unexpected properties and children.
        for name in node.properties.keys() {
            ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                format!("unexpected property `{}`", name.escape_default()),
            ));
        }
        for child in node.children.as_ref().map(|lst| &lst[..]).unwrap_or(&[]) {
            ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", child.node_name.escape_default()),
            ));
        }

        Ok(rv)
    }
}
