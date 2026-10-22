use std::str::FromStr;

use knuffel::errors::DecodeError;
use miette::miette;

pub use super::merge_with::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Percent(pub f64);

// MIN and MAX generics are only used during parsing to check the value.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct FloatOrInt<const MIN: i32, const MAX: i32>(pub f64);

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PositiveFloatOrInt<const MAX: i32>(pub f64);

/// Flag, with an optional explicit value.
///
/// Intended to be used as an `Option<MaybeBool>` field, as a tri-state:
/// - (missing): unset, `None`
/// - just `field`: set, `Some(true)`
/// - explicitly `field true` or `field false`: set, `Some(true)` or `Some(false)`
#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag(#[knuffel(argument, default = true)] pub bool);

impl FromStr for Percent {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((value, empty)) = s.split_once('%') else {
            return Err(miette!("value must end with '%'"));
        };

        if !empty.is_empty() {
            return Err(miette!("trailing characters after '%' are not allowed"));
        }

        let value: f64 = value.parse().map_err(|_| miette!("error parsing value"))?;
        if !value.is_finite() || value < 0. {
            return Err(miette!("percentage must be finite and non-negative"));
        }

        Ok(Percent(value / 100.))
    }
}

impl<const MIN: i32, const MAX: i32> MergeWith<FloatOrInt<MIN, MAX>> for f64 {
    fn merge_with(&mut self, part: &FloatOrInt<MIN, MAX>) {
        *self = part.0;
    }
}

impl<const MAX: i32> MergeWith<PositiveFloatOrInt<MAX>> for f64 {
    fn merge_with(&mut self, part: &PositiveFloatOrInt<MAX>) {
        *self = part.0;
    }
}

impl MergeWith<Flag> for bool {
    fn merge_with(&mut self, part: &Flag) {
        *self = part.0;
    }
}

impl<S: knuffel::traits::ErrorSpan, const MIN: i32, const MAX: i32> knuffel::DecodeScalar<S>
    for FloatOrInt<MIN, MAX>
{
    fn type_check(
        type_name: &Option<knuffel::span::Spanned<knuffel::ast::TypeName, S>>,
        ctx: &mut knuffel::decode::Context<S>,
    ) {
        if let Some(type_name) = &type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }
    }

    fn raw_decode(
        val: &knuffel::span::Spanned<knuffel::ast::Literal, S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        match &**val {
            knuffel::ast::Literal::Int(ref value) => match value.try_into() {
                Ok(v) => {
                    if (MIN..=MAX).contains(&v) {
                        Ok(FloatOrInt(f64::from(v)))
                    } else {
                        ctx.emit_error(DecodeError::conversion(
                            val,
                            format!("value must be between {MIN} and {MAX}"),
                        ));
                        Ok(FloatOrInt::default())
                    }
                }
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    Ok(FloatOrInt::default())
                }
            },
            knuffel::ast::Literal::Decimal(ref value) => match value.try_into() {
                Ok(v) => {
                    if (f64::from(MIN)..=f64::from(MAX)).contains(&v) {
                        Ok(FloatOrInt(v))
                    } else {
                        ctx.emit_error(DecodeError::conversion(
                            val,
                            format!("value must be between {MIN} and {MAX}"),
                        ));
                        Ok(FloatOrInt::default())
                    }
                }
                Err(e) => {
                    ctx.emit_error(DecodeError::conversion(val, e));
                    Ok(FloatOrInt::default())
                }
            },
            _ => {
                ctx.emit_error(DecodeError::unsupported(
                    val,
                    "Unsupported value, only numbers are recognized",
                ));
                Ok(FloatOrInt::default())
            }
        }
    }
}

impl<S: knuffel::traits::ErrorSpan, const MAX: i32> knuffel::DecodeScalar<S>
    for PositiveFloatOrInt<MAX>
{
    fn type_check(
        type_name: &Option<knuffel::span::Spanned<knuffel::ast::TypeName, S>>,
        ctx: &mut knuffel::decode::Context<S>,
    ) {
        FloatOrInt::<0, MAX>::type_check(type_name, ctx);
    }

    fn raw_decode(
        val: &knuffel::span::Spanned<knuffel::ast::Literal, S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        let value = FloatOrInt::<0, MAX>::raw_decode(val, ctx)?.0;
        if value > 0. {
            Ok(Self(value))
        } else {
            ctx.emit_error(DecodeError::conversion(
                val,
                format!("value must be greater than 0 and at most {MAX}"),
            ));
            Ok(Self::default())
        }
    }
}

pub fn expect_only_children<S>(
    node: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
) where
    S: knuffel::traits::ErrorSpan,
{
    if let Some(type_name) = &node.type_name {
        ctx.emit_error(DecodeError::unexpected(
            type_name,
            "type name",
            "no type name expected for this node",
        ));
    }

    for val in node.arguments.iter() {
        ctx.emit_error(DecodeError::unexpected(
            &val.literal,
            "argument",
            "no arguments expected for this node",
        ))
    }

    for name in node.properties.keys() {
        ctx.emit_error(DecodeError::unexpected(
            name,
            "property",
            "no properties expected for this node",
        ))
    }
}

pub fn parse_arg_node<S: knuffel::traits::ErrorSpan, T: knuffel::traits::DecodeScalar<S>>(
    name: &str,
    node: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
) -> Result<T, DecodeError<S>> {
    let mut iter_args = node.arguments.iter();
    let val = iter_args.next().ok_or_else(|| {
        DecodeError::missing(node, format!("additional argument `{name}` is required"))
    })?;

    let value = knuffel::traits::DecodeScalar::decode(val, ctx)?;

    if let Some(val) = iter_args.next() {
        ctx.emit_error(DecodeError::unexpected(
            &val.literal,
            "argument",
            "unexpected argument",
        ));
    }
    for name in node.properties.keys() {
        ctx.emit_error(DecodeError::unexpected(
            name,
            "property",
            format!("unexpected property `{}`", name.escape_default()),
        ));
    }
    for child in node.children() {
        ctx.emit_error(DecodeError::unexpected(
            child,
            "node",
            format!("unexpected node `{}`", child.node_name.escape_default()),
        ));
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::Percent;

    #[test]
    fn percent_rejects_non_finite_and_negative_values() {
        for value in ["NaN%", "inf%", "-1%"] {
            assert!(value.parse::<Percent>().is_err(), "accepted {value}");
        }
    }
}
