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

// Newtypes for putting information into the knuffel context.
struct BasePath(PathBuf);
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
        let config = ctx.get::<Rc<RefCell<Config>>>().unwrap().clone();
        let mut seen = HashSet::new();

        for node in nodes {
            let name = &**node.node_name;

            // Widgets and includes repeat; settings sections appear once per file.
            if matches!(name, "general" | "auth") && !seen.insert(name) {
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
                "background" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::Background(w));
                }
                "ring-indicator" => {
                    let w = knuffel::Decode::decode_node(node, ctx)?;
                    config.borrow_mut().widgets.push(Widget::RingIndicator(w));
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
        let flag = |ctx: &mut knuffel::decode::Context<S>| -> Result<bool, DecodeError<S>> {
            if child.arguments.is_empty() {
                Ok(true)
            } else {
                parse_arg_node(&child.node_name, child, ctx)
            }
        };
        match &**child.node_name {
            "hide-cursor" => general.hide_cursor = flag(ctx)?,
            "ignore-empty-input" => general.ignore_empty_input = flag(ctx)?,
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
                for arg in &child.arguments {
                    match &*arg.literal {
                        knuffel::ast::Literal::String(s) if &**s == "off" => service = None,
                        _ => ctx.emit_error(DecodeError::unexpected(
                            &arg.literal,
                            "argument",
                            "only `off` is accepted here",
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
                config.borrow_mut().auth.pam_service = service;
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
    includes.borrow_mut().0.push(path.to_path_buf());
    let include_errors = ctx.get::<Rc<RefCell<IncludeErrors>>>().unwrap().clone();
    let config = ctx.get::<Rc<RefCell<Config>>>().unwrap().clone();

    match fs::read_to_string(&path) {
        Ok(text) => {
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

/// A config file starts from swaywardlock's settings defaults with **no widgets**: a file that
/// defines no widgets gets the default look, any widget replaces the default set entirely.
pub fn parse(path: &Path, text: &str) -> ConfigParseResult<Config, ConfigIncludeError> {
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

    let part =
        knuffel::parse_with_context::<ConfigPart, knuffel::span::Span, _>(filename, text, |ctx| {
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
