//! The root decoder and `include`.
//!
//! `decode_include` and `parse` are copied from swayward-config's `decode.rs` (sha in `COPIED.md`),
//! trimmed: swayward's root sections are replaced with ours and the MRU-binds tracking is gone.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use knuffel::errors::DecodeError;

use super::copied::error::{ConfigIncludeError, ConfigParseResult};
use super::copied::utils::{expect_only_children, parse_arg_node};
use super::{Config, Widget};

const RECURSION_LIMIT: u8 = 10;
/// Most files one config may read through `include`. Cycle detection is per path, so a diamond
/// of includes was read once per path through it (10 includes 8 levels deep never finished).
const MAX_INCLUDES: usize = 100;
/// Deepest `{` or `/*` nesting accepted. knuffel's parser recurses per level and overflowed the
/// main thread's stack (an abort, not a config error) at about 300 in a debug build; a 2 MiB
/// thread manages about 50. Real configs nest four or five deep.
const MAX_NESTING: usize = 32;

// Newtypes for putting information into the knuffel context.
/// The directory of the file being decoded; relative paths in it resolve against this.
pub(super) struct BasePath(pub(super) PathBuf);
struct RootBase(PathBuf);
struct Recursion(u8);
#[derive(Default)]
struct Includes(Vec<PathBuf>);
#[derive(Default)]
struct IncludeErrors(Vec<knuffel::Error>);
// Used for recursive include detection.
//
// We don't *need* it because we have a recursion limit, but it makes for nicer error messages.
struct IncludeStack(HashSet<PathBuf>);
/// The root file was too deeply nested to parse; the message to report.
struct TooDeep(String);

/// Internal root decoder; merges a KDL file into [`Config`] held in the context.
pub struct ConfigPart;

impl<S> knuffel::DecodeChildren<S> for ConfigPart
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_children(
        nodes: &[knuffel::ast::SpannedNode<S>],
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        if let Some(TooDeep(msg)) = ctx.get::<TooDeep>() {
            let message = msg.clone();
            ctx.emit_error(DecodeError::MissingNode { message });
        }
        let config = ctx.get::<Rc<RefCell<Config>>>().unwrap().clone();
        let mut seen = HashSet::new();

        for node in nodes {
            let name = &**node.node_name;

            // Widgets and includes repeat; settings sections appear once per file.
            if matches!(name, "general" | "auth" | "animations") && !seen.insert(name) {
                ctx.emit_error(DecodeError::unexpected(
                    &node.node_name,
                    "node",
                    format!("duplicate node `{name}`, single node expected"),
                ));
                continue;
            }

            match name {
                "general" => decode_general(node, ctx, &config)?,
                "auth" => decode_auth(node, ctx, &config)?,
                "animations" => decode_animations(node, ctx, &config)?,
                "background" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::Background(w));
                }
                "ring-indicator" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::RingIndicator(w));
                }
                "shape" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::Shape(w));
                }
                "label" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::Label(w));
                }
                "image" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::Image(w));
                }
                "input-field" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::InputField(w));
                }
                "include" => decode_include(node, ctx)?,
                name => ctx.emit_error(DecodeError::unexpected(
                    node,
                    "node",
                    format!("unexpected node `{}`", name.escape_default()),
                )),
            }
        }

        Ok(Self)
    }
}

fn decode_general<S: knuffel::traits::ErrorSpan>(
    node: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
    config: &RefCell<Config>,
) -> Result<(), DecodeError<S>> {
    expect_only_children(node, ctx);
    let mut config = config.borrow_mut();
    let general = &mut config.general;
    for child in node.children() {
        match &**child.node_name {
            "hide-cursor" => general.hide_cursor = super::widgets::flag(child, ctx)?,
            "ignore-empty-input" => general.ignore_empty_input = super::widgets::flag(child, ctx)?,
            "fail-timeout-ms" => {
                general.fail_timeout_ms = parse_arg_node("fail-timeout-ms", child, ctx)?
            }
            "clear-input-after-ms" => {
                let ms: u32 = parse_arg_node("clear-input-after-ms", child, ctx)?;
                // A short timer empties the buffer while typing, leaving no way to unlock by
                // password.
                if ms < 1000 {
                    ctx.emit_error(DecodeError::conversion(
                        &child.arguments[0].literal,
                        "must be at least 1000 ms, or the password clears while typing",
                    ));
                }
                general.clear_input_after_ms = ms;
            }
            other => ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", other.escape_default()),
            )),
        }
    }
    Ok(())
}

fn decode_animations<S: knuffel::traits::ErrorSpan>(
    node: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
    config: &RefCell<Config>,
) -> Result<(), DecodeError<S>> {
    use super::{Animation, AnimationCurve, AnimationKind, EasingParams};
    expect_only_children(node, ctx);
    let mut config = config.borrow_mut();
    let anims = &mut config.animations;
    for child in node.children() {
        let slot = match &**child.node_name {
            "off" => {
                knuffel::decode::check_flag_node(child, ctx);
                anims.off = true;
                continue;
            }
            "fade-in" => &mut anims.fade_in,
            "fade-out" => &mut anims.fade_out,
            "input-field-fade" => &mut anims.input_field_fade,
            "input-field-width" => &mut anims.input_field_width,
            "input-field-colors" => &mut anims.input_field_colors,
            "input-field-dots" => &mut anims.input_field_dots,
            "ring-colors" => &mut anims.ring_colors,
            "ring-highlight" => &mut anims.ring_highlight,
            "ring-appear" => &mut anims.ring_appear,
            "ring-wrong" => &mut anims.ring_wrong,
            other => {
                ctx.emit_error(DecodeError::unexpected(
                    child,
                    "node",
                    format!(
                        "unexpected animation `{}`; expected fade-in, fade-out, input-field-fade, input-field-width, input-field-colors, input-field-dots, ring-colors, ring-highlight, ring-appear or ring-wrong",
                        other.escape_default()
                    ),
                ));
                continue;
            }
        };
        *slot = Animation::decode_node(child, ctx, *slot, |_, _| Ok(false))?;
        let name = &**child.node_name;
        if let AnimationKind::Easing(EasingParams {
            curve: AnimationCurve::CubicBezier(_, y1, _, y2),
            ..
        }) = slot.kind
        {
            if !(BEZIER_Y.contains(&y1) && BEZIER_Y.contains(&y2)) {
                ctx.emit_error(DecodeError::conversion(
                    &child.node_name,
                    format!(
                        "{name}: cubic-bezier y values must be between {} and {}; a larger \
                         overshoot is not an animation",
                        BEZIER_Y.start(),
                        BEZIER_Y.end()
                    ),
                ));
            }
        }
        let length = animation_length(*slot);
        if length > MAX_ANIMATION {
            let why = if matches!(name, "fade-in" | "fade-out") {
                "or a typo keeps the screen dark and the session locked for that long"
            } else {
                "or a typo redraws every output at 60 Hz for that long"
            };
            ctx.emit_error(DecodeError::conversion(
                &child.node_name,
                format!(
                    "{name} takes {} ms; at most {} ms is allowed, {why}",
                    length.as_millis(),
                    MAX_ANIMATION.as_millis()
                ),
            ));
        }
    }
    Ok(())
}

/// Longest animation a config may ask for. The fade-out runs before every unlock, so a value
/// like `duration-ms 4294967295` (or a very soft spring) would hold the session locked; the
/// input field's animations keep the redraw ticker running for as long as they last.
pub const MAX_ANIMATION: std::time::Duration = std::time::Duration::from_secs(3);

/// The control-point y values a `cubic-bezier` may use: up to 2 units of overshoot either way.
/// The runtime clamps eased progress to this range too.
const BEZIER_Y: std::ops::RangeInclusive<f64> = -2.0..=3.0;

/// How long `anim` takes over a 0..1 range, as the fade and field runtimes build it.
fn animation_length(anim: super::Animation) -> std::time::Duration {
    let clock = crate::animation::Clock::default();
    crate::animation::Animation::new(clock, 1., 0., 0., anim).duration()
}

fn decode_auth<S: knuffel::traits::ErrorSpan>(
    node: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
    config: &RefCell<Config>,
) -> Result<(), DecodeError<S>> {
    expect_only_children(node, ctx);
    for child in node.children() {
        match &**child.node_name {
            "pam" => {
                let mut service = Some("swaywardlock".to_owned());
                let mut reuse_password = false;
                for arg in &child.arguments {
                    match &*arg.literal {
                        knuffel::ast::Literal::String(s) if &**s == "off" => service = None,
                        _ => ctx.emit_error(DecodeError::unexpected(
                            &arg.literal,
                            "argument",
                            "only \"off\" is accepted here; `pam { off; }` disables PAM",
                        )),
                    }
                }
                // `pam { off; }`, the way `off` is written everywhere else (`animations { off }`).
                for c in child.children() {
                    match &**c.node_name {
                        "off" => {
                            knuffel::decode::check_flag_node(c, ctx);
                            service = None;
                        }
                        "reuse-password" => {
                            knuffel::decode::check_flag_node(c, ctx);
                            reuse_password = true;
                        }
                        other => ctx.emit_error(DecodeError::unexpected(
                            c,
                            "node",
                            format!(
                                "unexpected node `{}`; expected off or reuse-password",
                                other.escape_default()
                            ),
                        )),
                    }
                }
                for (name, val) in &child.properties {
                    match &***name {
                        "service" if service.is_some() => {
                            service = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?)
                        }
                        other => ctx.emit_error(DecodeError::unexpected(
                            name,
                            "property",
                            format!("unexpected property `{other}`"),
                        )),
                    }
                }
                let mut config = config.borrow_mut();
                config.auth.pam_service = service;
                config.auth.pam_reuse_password = reuse_password;
            }
            "fingerprint" => {
                let mut fp = super::Fingerprint::default();
                for c in child.children() {
                    match &**c.node_name {
                        "ready-message" => {
                            fp.ready_message = parse_arg_node("ready-message", c, ctx)?
                        }
                        "present-message" => {
                            fp.present_message = parse_arg_node("present-message", c, ctx)?
                        }
                        "retry-delay-ms" => {
                            fp.retry_delay_ms = parse_arg_node("retry-delay-ms", c, ctx)?
                        }
                        other => ctx.emit_error(DecodeError::unexpected(
                            c,
                            "node",
                            format!("unexpected node `{}`", other.escape_default()),
                        )),
                    }
                }
                config.borrow_mut().auth.fingerprint = Some(fp);
            }
            other => ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", other.escape_default()),
            )),
        }
    }
    Ok(())
}

fn decode_include<S>(
    node: &knuffel::ast::SpannedNode<S>,
    ctx: &mut knuffel::decode::Context<S>,
) -> Result<(), DecodeError<S>>
where
    S: knuffel::traits::ErrorSpan,
{
    let mut iter_args = node.arguments.iter();
    let path_val = iter_args.next().ok_or_else(|| {
        DecodeError::missing(node, "additional argument for include path is required")
    })?;
    let path: PathBuf = knuffel::traits::DecodeScalar::decode(path_val, ctx)?;

    if let Some(val) = iter_args.next() {
        ctx.emit_error(DecodeError::unexpected(
            &val.literal,
            "argument",
            "unexpected argument",
        ));
    }

    let mut optional = false;
    for (name, val) in &node.properties {
        match &***name {
            "optional" => optional = knuffel::traits::DecodeScalar::decode(val, ctx)?,
            name_str => ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                format!("unexpected property `{}`", name_str.escape_default()),
            )),
        }
    }

    for child in node.children() {
        ctx.emit_error(DecodeError::unexpected(
            child,
            "node",
            format!("unexpected node `{}`", child.node_name.escape_default()),
        ));
    }

    let path = if let Ok(rest) = path.strip_prefix("~") {
        let Some(home) = std::env::home_dir() else {
            ctx.emit_error(DecodeError::missing(
                node,
                format!("error retrieving home directory to expand {path:?}"),
            ));
            return Ok(());
        };
        home.join(rest)
    } else {
        ctx.get::<BasePath>().unwrap().0.join(path)
    };

    let recursion = ctx.get::<Recursion>().unwrap().0 + 1;
    if recursion == RECURSION_LIMIT {
        ctx.emit_error(DecodeError::missing(
            node,
            format!(
                "reached the recursion limit; includes cannot be {RECURSION_LIMIT} levels deep"
            ),
        ));
        return Ok(());
    }

    let Some(filename) = path.file_name().and_then(OsStr::to_str) else {
        ctx.emit_error(DecodeError::missing(
            node,
            "include path doesn't have a valid file name",
        ));
        return Ok(());
    };
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();

    let mut include_stack = ctx.get::<IncludeStack>().unwrap().0.clone();
    if !include_stack.insert(path.to_path_buf()) {
        ctx.emit_error(DecodeError::missing(
            node,
            "recursive include (file includes itself)",
        ));
        return Ok(());
    }

    let includes = ctx.get::<Rc<RefCell<Includes>>>().unwrap().clone();
    if includes.borrow().0.len() >= MAX_INCLUDES {
        ctx.emit_error(DecodeError::missing(
            node,
            format!("too many includes; one config may read at most {MAX_INCLUDES} files"),
        ));
        return Ok(());
    }
    includes.borrow_mut().0.push(path.to_path_buf());
    let include_errors = ctx.get::<Rc<RefCell<IncludeErrors>>>().unwrap().clone();
    let config = ctx.get::<Rc<RefCell<Config>>>().unwrap().clone();

    let read = fs::read_to_string(&path).map(|text| (too_deep(&text), text));
    match read {
        Ok((Some(msg), _)) => ctx.emit_error(DecodeError::missing(
            node,
            format!("included config {path:?} is {msg}"),
        )),
        Ok((None, text)) => {
            let root_base = &ctx.get::<RootBase>().unwrap().0;
            let relative_path = path.strip_prefix(root_base).ok().unwrap_or(&path);
            let filename = relative_path.to_str().unwrap_or(filename);

            let part = knuffel::parse_with_context::<ConfigPart, knuffel::span::Span, _>(
                filename,
                &text,
                |ctx| {
                    ctx.set(BasePath(base));
                    ctx.set(RootBase(root_base.clone()));
                    ctx.set(Recursion(recursion));
                    ctx.set(includes);
                    ctx.set(include_errors.clone());
                    ctx.set(IncludeStack(include_stack));
                    ctx.set(config);
                },
            );

            if let Err(err) = part {
                include_errors.borrow_mut().0.push(err);
                ctx.emit_error(DecodeError::missing(
                    node,
                    "failed to parse included config",
                ));
            }
        }
        Err(err) if optional && err.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!("optional include not found: {path:?}");
        }
        Err(err) => ctx.emit_error(DecodeError::missing(
            node,
            format!("failed to read included config from {path:?}: {err}"),
        )),
    }

    Ok(())
}

/// Whether `b` starts with a KDL newline (knuffel's grammar: LF, CR, FF, NEL, U+2028, U+2029;
/// CR LF counts from its CR).
fn kdl_newline_at(b: &[u8]) -> bool {
    matches!(
        b,
        [b'\n' | b'\r' | 0x0c, ..] | [0xc2, 0x85, ..] | [0xe2, 0x80, 0xa8 | 0xa9, ..]
    )
}

/// The deepest `{ }` or `/* */` nesting in KDL `text`, skipping strings and line comments.
fn nesting_depth(text: &str) -> usize {
    let b = text.as_bytes();
    let (mut braces, mut comments, mut max) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < b.len() {
        let next = b.get(i + 1).copied();
        if comments > 0 {
            match (b[i], next) {
                (b'/', Some(b'*')) => {
                    comments += 1;
                    i += 1;
                }
                (b'*', Some(b'/')) => {
                    comments -= 1;
                    i += 1;
                }
                _ => {}
            }
        } else {
            match (b[i], next) {
                (b'/', Some(b'*')) => {
                    comments = 1;
                    i += 1;
                }
                (b'/', Some(b'/')) => {
                    while i < b.len() && !kdl_newline_at(&b[i..]) {
                        i += 1;
                    }
                }
                (b'r', Some(b'"' | b'#'))
                    if b.get(i + 1 + b[i + 1..].iter().take_while(|&&c| c == b'#').count())
                        == Some(&b'"') =>
                {
                    // Raw string: r"..." or r#"..."#; no escapes.
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                    let close: Vec<u8> = std::iter::once(b'"')
                        .chain(std::iter::repeat_n(b'#', hashes))
                        .collect();
                    let start = i + 2 + hashes;
                    i = b
                        .get(start..)
                        .and_then(|rest| rest.windows(close.len()).position(|w| w == close))
                        .map_or(b.len(), |p| start + p + close.len() - 1);
                }
                (b'"', _) => {
                    i += 1;
                    while i < b.len() && b[i] != b'"' {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                }
                (b'{', _) => braces += 1,
                (b'}', _) => braces = braces.saturating_sub(1),
                _ => {}
            }
        }
        max = max.max(braces).max(comments);
        i += 1;
    }
    max
}

/// A file nested too deeply for the parser's stack, as a message for the user.
fn too_deep(text: &str) -> Option<String> {
    let depth = nesting_depth(text);
    (depth > MAX_NESTING).then(|| {
        format!("nested {depth} levels deep (`{{` or `/*`); at most {MAX_NESTING} allowed")
    })
}

/// A config file starts from swaywardlock's settings defaults with **no widgets**: a file that
/// defines no widgets gets the default look, any widget replaces the default set entirely.
pub fn parse(path: &Path, text: &str) -> ConfigParseResult<Config, ConfigIncludeError> {
    // Relative image and include paths resolve against this file's directory, so it must be
    // absolute: `-c rel.kdl` has an empty parent, and images load after `-f` has changed the
    // working directory to `/`. The config is parsed before that, against the user's cwd.
    let path = &std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let filename = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("config.kdl");

    let start = Config {
        widgets: Vec::new(),
        ..Config::default()
    };
    let config = Rc::new(RefCell::new(start));
    let includes = Rc::new(RefCell::new(Includes(Vec::new())));
    let include_errors = Rc::new(RefCell::new(IncludeErrors(Vec::new())));
    let include_stack = HashSet::from([path.to_path_buf()]);

    // A file the parser cannot take is reported through the decoder, so the error has the usual
    // type and shape.
    let deep = too_deep(text);
    let text = if deep.is_some() { "" } else { text };

    let part =
        knuffel::parse_with_context::<ConfigPart, knuffel::span::Span, _>(filename, text, |ctx| {
            if let Some(msg) = deep {
                ctx.set(TooDeep(msg));
            }
            ctx.set(BasePath(base.clone()));
            ctx.set(RootBase(base));
            ctx.set(Recursion(0));
            ctx.set(includes.clone());
            ctx.set(include_errors.clone());
            ctx.set(IncludeStack(include_stack));
            ctx.set(config.clone());
        });

    let includes = includes.take().0;
    let include_errors = include_errors.take().0;
    let config = part
        .map(|_| {
            let mut config = config.take();
            if config.widgets.is_empty() {
                config.widgets = Config::default().widgets;
            }
            config
        })
        .map_err(move |err| ConfigIncludeError {
            main: err,
            includes: include_errors,
        });

    ConfigParseResult { config, includes }
}

#[cfg(test)]
mod tests {
    use super::nesting_depth;

    #[test]
    fn nesting_depth_skips_strings_and_line_comments() {
        assert_eq!(nesting_depth("a { b { } }"), 2);
        assert_eq!(nesting_depth(r#"a "{{{" { }"#), 1);
        assert_eq!(nesting_depth(r#"a "\"{{" { }"#), 1);
        assert_eq!(nesting_depth("a r#\"{{\"# { }"), 1);
        assert_eq!(nesting_depth("a // {{{\nb { }"), 1);
        assert_eq!(nesting_depth("/* /* */ */ a {}"), 2);
        // An identifier ending in r is not a raw string.
        assert_eq!(nesting_depth("color #fff\nr { a { } }"), 2);
    }

    /// KDL (knuffel's newline()) also ends a line comment at CR, FF, NEL, U+2028 and U+2029;
    /// stopping only at LF hid everything after the comment from the scan.
    #[test]
    fn a_line_comment_ends_at_every_kdl_newline() {
        for nl in [
            "\n", "\r", "\r\n", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}",
        ] {
            assert_eq!(
                nesting_depth(&format!("// x{nl}a {{ b {{ }} }}")),
                2,
                "{nl:?}"
            );
        }
        // Other multi-byte characters do not end it.
        assert_eq!(nesting_depth("// \u{2027}\u{c5} a { b { } }\nc { }"), 1);
    }
}
