//! Widget nodes. Phase 1 has `background` and `ring-indicator`.

use std::path::PathBuf;
use std::str::FromStr;

use knuffel::errors::DecodeError;

use super::copied::color::Color;
use super::copied::effects::{Blur, BlurPart};
use super::copied::utils::{FloatOrInt, MergeWith as _};

// A handful of widgets, decoded once: the size difference does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Background(Background),
    RingIndicator(RingIndicator),
}

impl Widget {
    pub fn common(&self) -> &Common {
        match self {
            Widget::Background(w) => &w.common,
            Widget::RingIndicator(w) => &w.common,
        }
    }
}

/// Properties every widget accepts.
#[derive(Debug, Clone, PartialEq)]
pub struct Common {
    pub output: Option<OutputMatch>,
    pub position: (Length, Length),
    pub halign: HAlign,
    pub valign: VAlign,
    pub z_index: i32,
}

impl Common {
    fn with_z(z_index: i32) -> Self {
        Self {
            output: None,
            position: (Length::Px(0.), Length::Px(0.)),
            halign: HAlign::Center,
            valign: VAlign::Center,
            z_index,
        }
    }
}

/// `output="DP-1"` matches the `wl_output` name; `output="desc:Dell ..."` a description prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputMatch {
    Name(String),
    DescriptionPrefix(String),
}

impl OutputMatch {
    pub fn matches(&self, name: Option<&str>, description: Option<&str>) -> bool {
        match self {
            OutputMatch::Name(want) => name == Some(want.as_str()),
            OutputMatch::DescriptionPrefix(want) => {
                description.is_some_and(|d| d.starts_with(want.as_str()))
            }
        }
    }
}

impl FromStr for OutputMatch {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix("desc:") {
            Some(desc) => Ok(Self::DescriptionPrefix(desc.trim_start().to_owned())),
            None => Ok(Self::Name(s.to_owned())),
        }
    }
}

/// Logical pixels, or a fraction of the output size written as `"50%"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f64),
    Fraction(f64),
}

impl Length {
    pub fn resolve(self, extent: f64) -> f64 {
        match self {
            Length::Px(px) => px,
            Length::Fraction(f) => f * extent,
        }
    }
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
    None,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Center,
    Bottom,
    None,
}

/// swaylock's scaling modes, with CSS-ish names.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Scale to cover the output, cropping (swaylock `fill`, its default).
    #[default]
    Cover,
    /// Scale to fit inside the output, letterboxing (swaylock `fit`).
    Contain,
    /// Stretch to the output size (swaylock `stretch`).
    Fill,
    /// Repeat at natural size (swaylock `tile`).
    Tile,
    /// Natural size, centred (swaylock `center`).
    Center,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    pub common: Common,
    pub color: Color,
    pub path: Option<PathBuf>,
    pub fit: Fit,
    pub blur: Blur,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            common: Common::with_z(-1),
            // swaylock's default background colour.
            color: Color::from_rgba8_unpremul(0xa3, 0xa3, 0xa3, 0xff),
            path: None,
            fit: Fit::default(),
            blur: Blur {
                off: true,
                ..Blur::default()
            },
        }
    }
}

/// When the ring shows the keyboard layout.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShowLayout {
    /// When the keymap has more than one layout (swaylock's default).
    #[default]
    Auto,
    Always,
    Never,
}

/// Which colour set the inner and outer border lines use.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineUses {
    #[default]
    Line,
    Inside,
    Ring,
}

/// One colour per indicator state, as in swaylock's `struct swaylock_colorset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateColors {
    pub input: Color,
    pub cleared: Color,
    pub caps_lock: Color,
    pub verifying: Color,
    pub wrong: Color,
}

impl StateColors {
    const fn rgba(input: u32, cleared: u32, caps_lock: u32, verifying: u32, wrong: u32) -> Self {
        Self {
            input: hex(input),
            cleared: hex(cleared),
            caps_lock: hex(caps_lock),
            verifying: hex(verifying),
            wrong: hex(wrong),
        }
    }
}

const fn hex(rgba: u32) -> Color {
    let [r, g, b, a] = rgba.to_be_bytes();
    Color::from_array_unpremul([
        r as f32 / 255.,
        g as f32 / 255.,
        b as f32 / 255.,
        a as f32 / 255.,
    ])
}

#[derive(Debug, Clone, PartialEq)]
pub struct RingColors {
    pub inside: StateColors,
    pub ring: StateColors,
    pub line: StateColors,
    pub text: StateColors,
    pub key_highlight: Color,
    pub bs_highlight: Color,
    pub caps_lock_key_highlight: Color,
    pub caps_lock_bs_highlight: Color,
    pub separator: Color,
    pub layout_bg: Color,
    pub layout_border: Color,
    pub layout_text: Color,
}

impl Default for RingColors {
    /// swaylock's `set_default_colors`.
    fn default() -> Self {
        Self {
            inside: StateColors::rgba(0x000000c0, 0xe5a445c0, 0x000000c0, 0x0072ffc0, 0xfa0000c0),
            line: StateColors::rgba(0x000000ff, 0x000000ff, 0x000000ff, 0x000000ff, 0x000000ff),
            ring: StateColors::rgba(0x337d00ff, 0xe5a445ff, 0xe5a445ff, 0x3300ffff, 0x7d3300ff),
            text: StateColors::rgba(0xe5a445ff, 0x000000ff, 0xe5a445ff, 0x000000ff, 0x000000ff),
            key_highlight: hex(0x33db00ff),
            bs_highlight: hex(0xdb3300ff),
            caps_lock_key_highlight: hex(0x33db00ff),
            caps_lock_bs_highlight: hex(0xdb3300ff),
            separator: hex(0x000000ff),
            layout_bg: hex(0x000000c0),
            layout_border: hex(0x00000000),
            layout_text: hex(0xffffffff),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RingIndicator {
    pub common: Common,
    pub radius: f64,
    pub thickness: f64,
    pub font: String,
    /// 0 scales the font with the radius, as swaylock does.
    pub font_size: f64,
    pub idle_visible: bool,
    pub show_failed_attempts: bool,
    pub show_keyboard_layout: ShowLayout,
    pub show_caps_lock_text: bool,
    /// swaylock's `--indicator-caps-lock`: use the caps-lock colour set while caps lock is on.
    pub show_caps_lock_indicator: bool,
    pub line_uses: LineUses,
    pub colors: RingColors,
}

impl Default for RingIndicator {
    fn default() -> Self {
        Self {
            common: Common::with_z(0),
            radius: 50.,
            thickness: 10.,
            font: "sans-serif".into(),
            font_size: 0.,
            idle_visible: false,
            show_failed_attempts: false,
            show_keyboard_layout: ShowLayout::Auto,
            show_caps_lock_text: true,
            show_caps_lock_indicator: false,
            line_uses: LineUses::Line,
            colors: RingColors::default(),
        }
    }
}

// --- Decoding ----------------------------------------------------------------------------------

type Ctx<S> = knuffel::decode::Context<S>;
type Node<S> = knuffel::ast::SpannedNode<S>;

/// Decode the common properties and children of a widget node. Returns the children not consumed,
/// for the widget to handle.
fn decode_common<'a, S: knuffel::traits::ErrorSpan>(
    node: &'a Node<S>,
    ctx: &mut Ctx<S>,
    common: &mut Common,
) -> Result<Vec<&'a Node<S>>, DecodeError<S>> {
    if let Some(type_name) = &node.type_name {
        ctx.emit_error(DecodeError::unexpected(
            type_name,
            "type name",
            "no type name expected",
        ));
    }
    for arg in &node.arguments {
        ctx.emit_error(DecodeError::unexpected(
            &arg.literal,
            "argument",
            "no arguments expected",
        ));
    }
    for (name, val) in &node.properties {
        match &***name {
            "output" => {
                let s: String = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                common.output = Some(
                    s.parse()
                        .map_err(|e| DecodeError::conversion(&val.literal, e))?,
                );
            }
            other => ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                format!("unexpected property `{}`", other.escape_default()),
            )),
        }
    }

    let mut rest = Vec::new();
    for child in node.children() {
        match &**child.node_name {
            "position" => common.position = decode_pair(child, ctx)?,
            "halign" => common.halign = arg(child, ctx)?,
            "valign" => common.valign = arg(child, ctx)?,
            "z-index" => common.z_index = arg(child, ctx)?,
            _ => rest.push(child),
        }
    }
    Ok(rest)
}

fn arg<S: knuffel::traits::ErrorSpan, T: knuffel::traits::DecodeScalar<S>>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<T, DecodeError<S>> {
    super::copied::utils::parse_arg_node(&node.node_name, node, ctx)
}

/// A boolean child: `name` alone means true, `name false` means false.
fn flag<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<bool, DecodeError<S>> {
    if node.arguments.is_empty() {
        Ok(true)
    } else {
        arg(node, ctx)
    }
}

fn color<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<Color, DecodeError<S>> {
    knuffel::Decode::decode_node(node, ctx)
}

fn decode_length<S: knuffel::traits::ErrorSpan>(
    val: &knuffel::ast::Value<S>,
    ctx: &mut Ctx<S>,
) -> Result<Length, DecodeError<S>> {
    match &*val.literal {
        knuffel::ast::Literal::String(s) => {
            let pct = s
                .strip_suffix('%')
                .and_then(|n| n.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .ok_or_else(|| {
                    DecodeError::conversion(&val.literal, "expected a number or \"N%\"")
                })?;
            Ok(Length::Fraction(pct / 100.))
        }
        _ => {
            let n: FloatOrInt<-100000, 100000> = knuffel::traits::DecodeScalar::decode(val, ctx)?;
            Ok(Length::Px(n.0))
        }
    }
}

fn decode_pair<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<(Length, Length), DecodeError<S>> {
    let [x, y] = &node.arguments[..] else {
        return Err(DecodeError::missing(node, "expected two values: x y"));
    };
    Ok((decode_length(x, ctx)?, decode_length(y, ctx)?))
}

fn unexpected<S: knuffel::traits::ErrorSpan>(node: &Node<S>, ctx: &mut Ctx<S>) {
    ctx.emit_error(DecodeError::unexpected(
        node,
        "node",
        format!("unexpected node `{}`", node.node_name.escape_default()),
    ));
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for Background {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut bg = Background::default();
        for child in decode_common(node, ctx, &mut bg.common)? {
            match &**child.node_name {
                "color" => bg.color = color(child, ctx)?,
                "path" => {
                    let path: PathBuf = arg(child, ctx)?;
                    bg.path = Some(expand_home(path));
                }
                "fit" => bg.fit = arg(child, ctx)?,
                "blur" => {
                    // A `blur` block turns blur on unless it says `off`.
                    let part: BlurPart = knuffel::Decode::decode_node(child, ctx)?;
                    bg.blur.off = false;
                    bg.blur.merge_with(&part);
                }
                _ => unexpected(child, ctx),
            }
        }
        Ok(bg)
    }
}

fn expand_home(path: PathBuf) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => std::env::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or(path),
        Err(_) => path,
    }
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for RingIndicator {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut ring = RingIndicator::default();
        for child in decode_common(node, ctx, &mut ring.common)? {
            match &**child.node_name {
                "radius" => ring.radius = arg::<_, FloatOrInt<1, 10000>>(child, ctx)?.0,
                "thickness" => ring.thickness = arg::<_, FloatOrInt<1, 10000>>(child, ctx)?.0,
                "font" => ring.font = arg(child, ctx)?,
                "font-size" => ring.font_size = arg::<_, FloatOrInt<0, 10000>>(child, ctx)?.0,
                "idle-visible" => ring.idle_visible = flag(child, ctx)?,
                "show-failed-attempts" => ring.show_failed_attempts = flag(child, ctx)?,
                "show-keyboard-layout" => ring.show_keyboard_layout = arg(child, ctx)?,
                "show-caps-lock-text" => ring.show_caps_lock_text = flag(child, ctx)?,
                "show-caps-lock-indicator" => ring.show_caps_lock_indicator = flag(child, ctx)?,
                "line-uses" => ring.line_uses = arg(child, ctx)?,
                "colors" => decode_ring_colors(child, ctx, &mut ring.colors)?,
                _ => unexpected(child, ctx),
            }
        }
        Ok(ring)
    }
}

fn decode_ring_colors<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
    colors: &mut RingColors,
) -> Result<(), DecodeError<S>> {
    super::copied::utils::expect_only_children(node, ctx);
    for child in node.children() {
        match &**child.node_name {
            "state" => decode_state(child, ctx, colors)?,
            "key-highlight" => colors.key_highlight = color(child, ctx)?,
            "bs-highlight" => colors.bs_highlight = color(child, ctx)?,
            "caps-lock-key-highlight" => colors.caps_lock_key_highlight = color(child, ctx)?,
            "caps-lock-bs-highlight" => colors.caps_lock_bs_highlight = color(child, ctx)?,
            "separator" => colors.separator = color(child, ctx)?,
            "layout" => {
                for (name, val) in &child.properties {
                    let s: String = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                    let c: Color = s
                        .parse()
                        .map_err(|e| DecodeError::conversion(&val.literal, e))?;
                    match &***name {
                        "bg" => colors.layout_bg = c,
                        "border" => colors.layout_border = c,
                        "text" => colors.layout_text = c,
                        other => ctx.emit_error(DecodeError::unexpected(
                            name,
                            "property",
                            format!("unexpected property `{other}`; expected bg, border or text"),
                        )),
                    }
                }
            }
            _ => unexpected(child, ctx),
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum StateName {
    Input,
    Clear,
    CapsLock,
    Verifying,
    Wrong,
}

impl StateColors {
    fn get_mut(&mut self, state: StateName) -> &mut Color {
        match state {
            StateName::Input => &mut self.input,
            StateName::Clear => &mut self.cleared,
            StateName::CapsLock => &mut self.caps_lock,
            StateName::Verifying => &mut self.verifying,
            StateName::Wrong => &mut self.wrong,
        }
    }
}

/// `state "wrong" inside="#..." ring="#..." line="#..." text="#..."`
fn decode_state<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
    colors: &mut RingColors,
) -> Result<(), DecodeError<S>> {
    let Some(name_val) = node.arguments.first() else {
        return Err(DecodeError::missing(node, "expected a state name"));
    };
    let name: String = knuffel::traits::DecodeScalar::decode(name_val, ctx)?;
    let state = match name.as_str() {
        "input" => StateName::Input,
        "clear" => StateName::Clear,
        "caps-lock" => StateName::CapsLock,
        "verifying" => StateName::Verifying,
        "wrong" => StateName::Wrong,
        _ => {
            return Err(DecodeError::conversion(
                &name_val.literal,
                "expected input, clear, caps-lock, verifying or wrong",
            ))
        }
    };
    for (prop, val) in &node.properties {
        let s: String = knuffel::traits::DecodeScalar::decode(val, ctx)?;
        let c: Color = s
            .parse()
            .map_err(|e| DecodeError::conversion(&val.literal, e))?;
        let set = match &***prop {
            "inside" => &mut colors.inside,
            "ring" => &mut colors.ring,
            "line" => &mut colors.line,
            "text" => &mut colors.text,
            other => {
                ctx.emit_error(DecodeError::unexpected(
                    prop,
                    "property",
                    format!("unexpected property `{other}`; expected inside, ring, line or text"),
                ));
                continue;
            }
        };
        *set.get_mut(state) = c;
    }
    Ok(())
}
