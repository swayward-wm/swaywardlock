use knuffel::errors::DecodeError;
use knuffel::Decode as _;

use super::super::utils::{expect_only_children, parse_arg_node, FloatOrInt};
use super::*;

impl Animation {
    pub fn new_off() -> Self {
        Self {
            off: true,
            kind: Kind::Easing(EasingParams {
                duration_ms: 0,
                curve: Curve::Linear,
            }),
        }
    }

    pub fn decode_node<S: knuffel::traits::ErrorSpan>(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
        default: Self,
        mut process_children: impl FnMut(
            &knuffel::ast::SpannedNode<S>,
            &mut knuffel::decode::Context<S>,
        ) -> Result<bool, DecodeError<S>>,
    ) -> Result<Self, DecodeError<S>> {
        #[derive(Default, PartialEq)]
        struct OptionalEasingParams {
            duration_ms: Option<u32>,
            curve: Option<Curve>,
        }

        expect_only_children(node, ctx);

        let mut off = false;
        let mut easing_params = OptionalEasingParams::default();
        let mut spring_params = None;

        for child in node.children() {
            match &**child.node_name {
                "off" => {
                    knuffel::decode::check_flag_node(child, ctx);
                    if off {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "node",
                            "duplicate node `off`, single node expected",
                        ));
                    } else {
                        off = true;
                    }
                }
                "spring" => {
                    if easing_params != OptionalEasingParams::default() {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            "cannot set both spring and easing parameters at once",
                        ));
                    }
                    if spring_params.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "node",
                            "duplicate node `spring`, single node expected",
                        ));
                    }

                    spring_params = Some(SpringParams::decode_node(child, ctx)?);
                }
                "duration-ms" => {
                    if spring_params.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            "cannot set both spring and easing parameters at once",
                        ));
                    }
                    if easing_params.duration_ms.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "node",
                            "duplicate node `duration-ms`, single node expected",
                        ));
                    }

                    easing_params.duration_ms = Some(parse_arg_node("duration-ms", child, ctx)?);
                }
                "curve" => {
                    if spring_params.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            "cannot set both spring and easing parameters at once",
                        ));
                    }
                    if easing_params.curve.is_some() {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "node",
                            "duplicate node `curve`, single node expected",
                        ));
                    }

                    let mut iter_args = child.arguments.iter();
                    let val = iter_args.next().ok_or_else(|| {
                        DecodeError::missing(child, "additional argument `curve` is required")
                    })?;
                    let animation_curve_string: String =
                        knuffel::traits::DecodeScalar::decode(val, ctx)?;

                    let animation_curve = match animation_curve_string.as_str() {
                        "linear" => Some(Curve::Linear),
                        "ease-out-quad" => Some(Curve::EaseOutQuad),
                        "ease-out-cubic" => Some(Curve::EaseOutCubic),
                        "ease-out-expo" => Some(Curve::EaseOutExpo),
                        "cubic-bezier" => {
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(
                                    child,
                                    "missing x1 coordinate for cubic Bézier curve control point",
                                )
                            })?;
                            // the X axis represents time frame so it cannot be negative
                            // or larger than 1
                            let x1: FloatOrInt<0, 1> =
                                knuffel::traits::DecodeScalar::decode(val, ctx)?;
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(
                                    child,
                                    "missing y1 coordinate for cubic Bézier curve control point",
                                )
                            })?;
                            let y1: FloatOrInt<{ i32::MIN }, { i32::MAX }> =
                                knuffel::traits::DecodeScalar::decode(val, ctx)?;
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(
                                    child,
                                    "missing x2 coordinate for cubic Bézier curve control point",
                                )
                            })?;
                            let x2: FloatOrInt<0, 1> =
                                knuffel::traits::DecodeScalar::decode(val, ctx)?;
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(
                                    child,
                                    "missing y2 coordinate for cubic Bézier curve control point",
                                )
                            })?;
                            let y2: FloatOrInt<{ i32::MIN }, { i32::MAX }> =
                                knuffel::traits::DecodeScalar::decode(val, ctx)?;

                            Some(Curve::CubicBezier(x1.0, y1.0, x2.0, y2.0))
                        }
                        unexpected_curve => {
                            ctx.emit_error(DecodeError::unexpected(
                                &val.literal,
                                "argument",
                                format!(
                                    "unexpected animation curve `{unexpected_curve}`. \
                                    swaywardlock only supports five animation curves: \
                                    `ease-out-quad`, `ease-out-cubic`, `ease-out-expo`, `linear` and `cubic-bezier`."
                                ),
                            ));

                            None
                        }
                    };

                    if let Some(val) = iter_args.next() {
                        ctx.emit_error(DecodeError::unexpected(
                            &val.literal,
                            "argument",
                            "unexpected argument",
                        ));
                    }
                    for name in child.properties.keys() {
                        ctx.emit_error(DecodeError::unexpected(
                            name,
                            "property",
                            format!("unexpected property `{}`", name.escape_default()),
                        ));
                    }
                    for child in child.children() {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            format!("unexpected node `{}`", child.node_name.escape_default()),
                        ));
                    }

                    easing_params.curve = animation_curve;
                }
                name_str => {
                    if !process_children(child, ctx)? {
                        ctx.emit_error(DecodeError::unexpected(
                            child,
                            "node",
                            format!("unexpected node `{}`", name_str.escape_default()),
                        ));
                    }
                }
            }
        }

        let kind = if let Some(spring_params) = spring_params {
            // Configured spring.
            Kind::Spring(spring_params)
        } else if easing_params == OptionalEasingParams::default() {
            // Did not configure anything.
            default.kind
        } else {
            // Configured easing.
            let default = if let Kind::Easing(easing) = default.kind {
                easing
            } else {
                // Generic fallback values for when the default animation is spring, but the user
                // configured an easing animation.
                EasingParams {
                    duration_ms: 250,
                    curve: Curve::EaseOutCubic,
                }
            };

            Kind::Easing(EasingParams {
                duration_ms: easing_params.duration_ms.unwrap_or(default.duration_ms),
                curve: easing_params.curve.unwrap_or(default.curve),
            })
        };

        Ok(Self { off, kind })
    }
}

impl<S> knuffel::Decode<S> for SpringParams
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }
        if let Some(val) = node.arguments.first() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "unexpected argument",
            ));
        }
        for child in node.children() {
            ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", child.node_name.escape_default()),
            ));
        }

        let mut damping_ratio = None;
        let mut stiffness = None;
        let mut epsilon = None;
        for (name, val) in &node.properties {
            match &***name {
                "damping-ratio" => {
                    damping_ratio = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?);
                }
                "stiffness" => {
                    stiffness = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?);
                }
                "epsilon" => {
                    epsilon = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?);
                }
                name_str => {
                    ctx.emit_error(DecodeError::unexpected(
                        name,
                        "property",
                        format!("unexpected property `{}`", name_str.escape_default()),
                    ));
                }
            }
        }
        let damping_ratio = damping_ratio
            .ok_or_else(|| DecodeError::missing(node, "property `damping-ratio` is required"))?;
        let stiffness = stiffness
            .ok_or_else(|| DecodeError::missing(node, "property `stiffness` is required"))?;
        let epsilon =
            epsilon.ok_or_else(|| DecodeError::missing(node, "property `epsilon` is required"))?;

        if !(0.1..=10.).contains(&damping_ratio) {
            ctx.emit_error(DecodeError::conversion(
                node,
                "damping-ratio must be between 0.1 and 10.0",
            ));
        }
        if stiffness < 1 {
            ctx.emit_error(DecodeError::conversion(node, "stiffness must be >= 1"));
        }
        if !(0.00001..=0.1).contains(&epsilon) {
            ctx.emit_error(DecodeError::conversion(
                node,
                "epsilon must be between 0.00001 and 0.1",
            ));
        }

        Ok(SpringParams {
            damping_ratio,
            stiffness,
            epsilon,
        })
    }
}
