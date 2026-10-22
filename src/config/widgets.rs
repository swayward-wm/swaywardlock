//! Widget nodes: `background`, `ring-indicator`, `shape`, `label`, `image` and `input-field`.

use std::path::PathBuf;
use std::str::FromStr;

use knuffel::errors::DecodeError;

use super::copied::color::{Color, CornerRadius, Gradient};
use super::copied::effects::{Blur, BlurPart};
use super::copied::shadow::{Shadow, ShadowOffset, ShadowRule};
use super::copied::utils::{FloatOrInt, MergeWith as _};

// A handful of widgets, decoded once: the size difference does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Background(Background),
    RingIndicator(RingIndicator),
    Shape(Shape),
    Label(Label),
    Image(ImageWidget),
    InputField(InputField),
}

impl Widget {
    pub fn common(&self) -> &Common {
        match self {
            Widget::Background(w) => &w.common,
            Widget::RingIndicator(w) => &w.common,
            Widget::Shape(w) => &w.common,
            Widget::Label(w) => &w.common,
            Widget::Image(w) => &w.common,
            Widget::InputField(w) => &w.common,
        }
    }
}

/// Text (hyprlock's `label`). `text` is a template: `$VARIABLES` and an optional `cmd[...]`
/// prefix, in hyprlock's syntax.
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub common: Common,
    pub text: String,
    /// Pango font description, e.g. "Sans Bold 24".
    pub font: String,
    pub color: Color,
    pub align: TextAlign,
    /// Pango markup is on by default, as in hyprlock.
    pub markup: bool,
    /// Degrees counter-clockwise, 0..360 (hyprlock's `rotate`).
    pub rotate: f64,
    /// hyprlock's `shadow_passes` / `shadow_size` / `shadow_color` / `shadow_boost`; `None` when
    /// absent or `passes 0` (hyprlock draws no shadow then).
    pub text_shadow: Option<TextShadow>,
}

/// A label's text shadow: the text's alpha, blurred `passes` times (dual Kawase, `size` is the
/// blur offset), boosted, drawn in `color` behind the text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    /// 1..=4 (0 is "off" and never stored).
    pub passes: u8,
    /// Blur offset in logical px, 0..=20 (hyprlock's `shadow_size`, default 3).
    pub size: f64,
    /// Only the colour is hyprlock's; its alpha multiplies the shadow's, which hyprlock ignores.
    pub color: Color,
    /// Alpha multiplier, clamped at 1 (hyprlock's `shadow_boost`, default 1.2).
    pub boost: f64,
    /// Logical px; positive y moves up, as in `position`. Not in hyprlock.
    pub offset: (f64, f64),
}

impl Default for Label {
    fn default() -> Self {
        Self {
            common: Common::with_z(0),
            text: String::new(),
            font: "Sans 16".into(),
            color: hex(0xffffffff),
            align: TextAlign::Center,
            markup: true,
            rotate: 0.,
            text_shadow: None,
        }
    }
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// hyprlock's password field: a rounded box with an outline, dots for typed characters, and
/// placeholder / checking / failure text. Colour precedence follows hyprlock's `updateColors`.
#[derive(Debug, Clone, PartialEq)]
pub struct InputField {
    pub common: Common,
    pub size: (Length, Length),
    /// `None` means fully round (hyprlock's `rounding -1`, its default).
    pub corner_radius: Option<CornerRadius>,
    pub inner_color: Color,
    /// Outline width and its idle colour/gradient (hyprlock's outline_thickness/outer_color).
    pub outline: Border,
    pub font: String,
    pub font_color: Color,
    pub placeholder: String,
    pub dots: Dots,
    pub hide_input: Option<Color>,
    pub fade_on_empty: Option<u32>,
    pub check: StateStyle,
    pub fail: StateStyle,
    pub capslock_color: Option<Gradient>,
    pub numlock_color: Option<Gradient>,
    pub bothlock_color: Option<Gradient>,
    pub invert_numlock: bool,
    pub swap_font_color: bool,
    pub shadow: Shadow,
}

/// The colour and text shown in a state (checking, failed).
#[derive(Debug, Clone, PartialEq)]
pub struct StateStyle {
    pub color: Gradient,
    pub text: String,
}

/// Dot rounding, with hyprlock's `dots_rounding` sentinels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DotRadius {
    /// `-1`: round dots.
    Round,
    /// `-2`: follow the field's rounding, scaled by the dot size.
    Inherit,
    Px(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dots {
    /// Dot size as a fraction of the field height (hyprlock clamps to 0.001..0.8).
    pub size: f64,
    /// Gap as a fraction of the dot size (hyprlock: -1..1).
    pub spacing: f64,
    pub center: bool,
    pub corner_radius: DotRadius,
    /// Draw this text per character instead of a dot (hyprlock's dots_text_format).
    pub text: Option<String>,
}

impl Default for InputField {
    /// hyprlock's defaults (ConfigManager.cpp).
    fn default() -> Self {
        Self {
            common: Common::with_z(0),
            size: (Length::Px(400.), Length::Px(90.)),
            corner_radius: None,
            inner_color: hex(0xddddddff),
            outline: Border {
                width: 4.,
                gradient: Gradient::from(hex(0x111111ff)),
            },
            font: "Sans".into(),
            font_color: hex(0x000000ff),
            placeholder: "<i>Input Password</i>".into(),
            dots: Dots {
                size: 0.25,
                spacing: 0.2,
                center: true,
                corner_radius: DotRadius::Round,
                text: None,
            },
            hide_input: None,
            fade_on_empty: Some(2000),
            check: StateStyle {
                color: Gradient::from(hex(0x22cc88ff)),
                text: String::new(),
            },
            fail: StateStyle {
                color: Gradient::from(hex(0xcc2222ff)),
                text: "<i>$FAIL</i>".into(),
            },
            capslock_color: None,
            numlock_color: None,
            bothlock_color: None,
            invert_numlock: false,
            swap_font_color: false,
            shadow: Shadow::default(),
        }
    }
}

/// A picture, e.g. an avatar (hyprlock's `image`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageWidget {
    pub common: Common,
    pub path: PathBuf,
    /// The shorter side after scaling, in logical pixels (hyprlock's `size`); the image keeps
    /// its aspect ratio.
    pub size: f64,
    /// `None` means a circle (hyprlock's `rounding -1`, its default).
    pub corner_radius: Option<CornerRadius>,
    pub border: Option<Border>,
    pub shadow: Shadow,
    /// Degrees counter-clockwise, 0..360 (hyprlock's `rotate`).
    pub rotate: f64,
}

/// A rectangle with optional rounded corners, border and shadow (hyprlock's `shape`).
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub common: Common,
    pub size: (Length, Length),
    pub color: Color,
    /// `None` means fully round (hyprlock's `rounding -1`): half the shorter side.
    pub corner_radius: Option<CornerRadius>,
    pub border: Option<Border>,
    pub shadow: Shadow,
    /// Cut through to the background instead of painting (hyprlock's `xray`).
    pub xray: bool,
    /// Degrees counter-clockwise, 0..360 (hyprlock's `rotate`). Ignored with `xray`, as in
    /// hyprlock.
    pub rotate: f64,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            common: Common::with_z(0),
            size: (Length::Px(100.), Length::Px(100.)),
            color: hex(0x111111ff),
            corner_radius: Some(CornerRadius::default()),
            border: None,
            shadow: Shadow::default(),
            xray: false,
            rotate: 0.,
        }
    }
}

/// A border line: width plus a colour or a gradient, in swayward's syntax.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub width: f64,
    pub gradient: Gradient,
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
    /// Use a screenshot of the output taken before locking (hyprlock's `path screenshot`).
    pub screenshot: bool,
    pub path: Option<PathBuf>,
    pub fit: Fit,
    pub blur: Blur,
    /// hyprlock's blur prepare/finish adjustments: contrast and brightness (>1 brightens) before
    /// the blur, brightness (<1 dims) after it. 1.0 leaves the image alone.
    pub contrast: f64,
    pub brightness: f64,
    /// hyprlock's saturation boost of the blurred colours (its FRAGBLUR1); 0 is off. Needs blur.
    pub vibrancy: f64,
    /// How much less dark colours are boosted, 0 to 1.
    pub vibrancy_darkness: f64,
}

impl Background {
    /// Whether the background is blurred (and post-processed). hyprlock: blur_passes 0 is no blur.
    pub fn blurred(&self) -> bool {
        !self.blur.off && self.blur.passes > 0
    }
}

impl Default for Background {
    fn default() -> Self {
        Self {
            common: Common::with_z(-1),
            // swaylock's default background colour.
            color: Color::from_rgba8_unpremul(0xa3, 0xa3, 0xa3, 0xff),
            screenshot: false,
            path: None,
            fit: Fit::default(),
            blur: Blur {
                off: true,
                ..Blur::default()
            },
            contrast: 1.,
            brightness: 1.,
            vibrancy: 0.,
            vibrancy_darkness: 0.,
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

/// How the ring is drawn.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RingStyle {
    /// swaylock's ring, pixel for pixel.
    #[default]
    Swaylock,
    /// The same ring, animated, with a glow.
    Swayward,
}

/// Glow width of the swayward style when `glow` is absent.
pub const DEFAULT_RING_GLOW: f64 = 12.;

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
    pub style: RingStyle,
    /// Logical px of halo outside the ring stroke (swayward style only; 0 = none).
    pub glow: f64,
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
            style: RingStyle::Swaylock,
            glow: 0.,
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

/// A boolean child: `name` alone means true, `name true` / `name false` set it. Anything else
/// (another argument type, a second argument, a property or a child) is a config error.
pub(super) fn flag<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<bool, DecodeError<S>> {
    for name in node.properties.keys() {
        ctx.emit_error(DecodeError::unexpected(
            name,
            "property",
            format!("unexpected property `{}`", name.escape_default()),
        ));
    }
    flag_value(node, ctx)
}

/// `flag`'s argument and children check, leaving properties to the caller.
fn flag_value<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<bool, DecodeError<S>> {
    for child in node.children() {
        unexpected(child, ctx);
    }
    let mut args = node.arguments.iter();
    let value = match args.next() {
        None => true,
        Some(val) => knuffel::traits::DecodeScalar::decode(val, ctx)?,
    };
    for val in args {
        ctx.emit_error(DecodeError::unexpected(
            &val.literal,
            "argument",
            "unexpected argument",
        ));
    }
    Ok(value)
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

/// A widget `size`: like a position, but never negative, and a percentage at most 1000%.
fn decode_size<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<(Length, Length), DecodeError<S>> {
    let pair = decode_pair(node, ctx)?;
    for (len, val) in [pair.0, pair.1].into_iter().zip(&node.arguments) {
        let ok = match len {
            Length::Px(px) => px >= 0.,
            Length::Fraction(f) => (0. ..=10.).contains(&f),
        };
        if !ok {
            return Err(DecodeError::conversion(
                &val.literal,
                "a size must be 0 or more (px), or 0% to 1000%",
            ));
        }
    }
    Ok(pair)
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
        // Post-processing options that were written, for the warning when blur is off.
        let mut post_set = Vec::new();
        for child in decode_common(node, ctx, &mut bg.common)? {
            match &**child.node_name {
                "color" => bg.color = color(child, ctx)?,
                "screenshot" => bg.screenshot = flag(child, ctx)?,
                "path" => bg.path = Some(image_path(child, ctx)?),
                "fit" => bg.fit = arg(child, ctx)?,
                "contrast" => bg.contrast = arg::<_, FloatOrInt<0, 10>>(child, ctx)?.0,
                "brightness" => bg.brightness = arg::<_, FloatOrInt<0, 10>>(child, ctx)?.0,
                "vibrancy" => {
                    bg.vibrancy = arg::<_, FloatOrInt<0, 10>>(child, ctx)?.0;
                    post_set.push("vibrancy");
                }
                "vibrancy-darkness" => {
                    bg.vibrancy_darkness = arg::<_, FloatOrInt<0, 1>>(child, ctx)?.0;
                    post_set.push("vibrancy-darkness");
                }
                "blur" => {
                    // A `blur` block turns blur on unless it says `off`.
                    let part: BlurPart = knuffel::Decode::decode_node(child, ctx)?;
                    bg.blur.off = false;
                    bg.blur.merge_with(&part);
                    if part.noise.is_some() {
                        post_set.push("blur noise");
                    }
                    if part.saturation.is_some() {
                        post_set.push("blur saturation");
                    }
                }
                _ => unexpected(child, ctx),
            }
        }
        // As in hyprlock and swayward, post-processing runs on the blurred background only.
        if !post_set.is_empty() && !bg.blurred() {
            post_set.sort_unstable();
            post_set.dedup();
            tracing::warn!(
                "background: {} needs blur (a blur block with passes above 0); ignored",
                post_set.join(", ")
            );
        } else if post_set.contains(&"vibrancy-darkness") && bg.vibrancy == 0. {
            tracing::warn!("background: vibrancy-darkness needs vibrancy above 0; ignored");
        }
        Ok(bg)
    }
}

/// An image path: `~` expands to the home directory, and a relative path resolves against the
/// directory of the file it is written in, as `include` does. The locker may chdir to `/`
/// (daemonize) before it loads images, so the working directory means nothing.
fn image_path<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<PathBuf, DecodeError<S>> {
    let path: PathBuf = arg(node, ctx)?;
    Ok(match path.strip_prefix("~") {
        Ok(rest) => std::env::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or(path),
        Err(_) => match ctx.get::<super::decode::BasePath>() {
            Some(base) => base.0.join(path),
            None => path,
        },
    })
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for RingIndicator {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut ring = RingIndicator::default();
        let mut glow = None;
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
                "style" => ring.style = arg(child, ctx)?,
                "glow" => glow = Some((child, arg::<_, FloatOrInt<0, 64>>(child, ctx)?.0)),
                _ => unexpected(child, ctx),
            }
        }
        match (ring.style, glow) {
            (RingStyle::Swayward, None) => ring.glow = DEFAULT_RING_GLOW,
            (RingStyle::Swayward, Some((_, g))) => ring.glow = g,
            // A swaylock config must not drift from swaylock's pixels by accident.
            (RingStyle::Swaylock, Some((child, _))) => ctx.emit_error(DecodeError::unexpected(
                &child.node_name,
                "node",
                "`glow` needs `style \"swayward\"`; the swaylock style has no glow",
            )),
            (RingStyle::Swaylock, None) => {}
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

/// `corner-radius 12`, `corner-radius 1 2 3 4` (swayward's syntax) or `corner-radius "full"`.
fn decode_corner_radius<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<Option<CornerRadius>, DecodeError<S>> {
    if let [only] = &node.arguments[..] {
        if let knuffel::ast::Literal::String(s) = &*only.literal {
            return if &**s == "full" {
                Ok(None)
            } else {
                Err(DecodeError::conversion(
                    &only.literal,
                    "expected a number or \"full\"",
                ))
            };
        }
    }
    Ok(Some(knuffel::Decode::decode_node(node, ctx)?))
}

/// `border { width 2; color "#fff"; }` or `border { width 2; gradient from=.. to=..; }`.
fn decode_border<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<Border, DecodeError<S>> {
    super::copied::utils::expect_only_children(node, ctx);
    let mut width = 2.;
    let mut gradient = None;
    for child in node.children() {
        match &**child.node_name {
            "width" => width = arg::<_, FloatOrInt<0, 1000>>(child, ctx)?.0,
            "color" => gradient = Some(Gradient::from(color(child, ctx)?)),
            "gradient" => gradient = Some(knuffel::Decode::decode_node(child, ctx)?),
            _ => unexpected(child, ctx),
        }
    }
    let gradient =
        gradient.ok_or_else(|| DecodeError::missing(node, "border needs `color` or `gradient`"))?;
    Ok(Border { width, gradient })
}

/// A `shadow { ... }` block in swayward's syntax; present means on unless it says `off`.
fn decode_shadow<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
    shadow: &mut Shadow,
) -> Result<(), DecodeError<S>> {
    let part: ShadowRule = knuffel::Decode::decode_node(node, ctx)?;
    if part.draw_behind_window.is_some() || part.inactive_color.is_some() {
        ctx.emit_error(DecodeError::unexpected(
            node,
            "node",
            "draw-behind-window and inactive-color apply to swayward windows only",
        ));
    }
    shadow.on = true;
    shadow.merge_with(&part);
    Ok(())
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for Shape {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut shape = Shape::default();
        for child in decode_common(node, ctx, &mut shape.common)? {
            match &**child.node_name {
                "size" => shape.size = decode_size(child, ctx)?,
                "color" => shape.color = color(child, ctx)?,
                "corner-radius" => shape.corner_radius = decode_corner_radius(child, ctx)?,
                "border" => shape.border = Some(decode_border(child, ctx)?),
                "shadow" => decode_shadow(child, ctx, &mut shape.shadow)?,
                "xray" => shape.xray = flag(child, ctx)?,
                "rotate" => shape.rotate = rotate(child, ctx)?,
                _ => unexpected(child, ctx),
            }
        }
        if shape.xray && shape.rotate != 0. {
            tracing::warn!("shape: rotate is ignored with xray (as in hyprlock)");
        }
        Ok(shape)
    }
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for Label {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut label = Label::default();
        let mut has_text = false;
        for child in decode_common(node, ctx, &mut label.common)? {
            match &**child.node_name {
                "text" => {
                    label.text = arg(child, ctx)?;
                    has_text = true;
                }
                "font" => label.font = arg(child, ctx)?,
                "color" => label.color = color(child, ctx)?,
                "align" => label.align = arg(child, ctx)?,
                "markup" => label.markup = flag(child, ctx)?,
                "rotate" => label.rotate = rotate(child, ctx)?,
                "text-shadow" => label.text_shadow = decode_text_shadow(child, ctx)?,
                _ => unexpected(child, ctx),
            }
        }
        if !has_text {
            return Err(DecodeError::missing(node, "label needs `text`"));
        }
        Ok(label)
    }
}

/// Most blur passes of a text shadow. Each pass doubles the blur's reach and the margin the
/// shadow needs: at 4 passes and size 20 (the caps) the margin is 674 logical px on every side,
/// wider than any label's shadow needs; hyprlock configs use 1 to 3 passes.
pub const MAX_TEXT_SHADOW_PASSES: u8 = 4;

/// `text-shadow { passes 2; size 3; color "#000"; boost 1.2; offset x=0 y=0; }`. `passes` is
/// required, so a block never silently draws nothing; `passes 0` is off, as in hyprlock.
fn decode_text_shadow<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<Option<TextShadow>, DecodeError<S>> {
    super::copied::utils::expect_only_children(node, ctx);
    let mut shadow = TextShadow {
        passes: 0,
        size: 3.,
        color: hex(0x000000ff),
        boost: 1.2,
        offset: (0., 0.),
    };
    let mut passes = None;
    for child in node.children() {
        match &**child.node_name {
            "passes" => {
                let n: u8 = arg(child, ctx)?;
                if n > MAX_TEXT_SHADOW_PASSES {
                    ctx.emit_error(DecodeError::conversion(
                        &child.arguments[0].literal,
                        "passes must be 0 to 4",
                    ));
                }
                passes = Some(n);
            }
            "size" => shadow.size = arg::<_, FloatOrInt<0, 20>>(child, ctx)?.0,
            "color" => shadow.color = color(child, ctx)?,
            "boost" => shadow.boost = arg::<_, FloatOrInt<0, 10>>(child, ctx)?.0,
            "offset" => {
                let o: ShadowOffset = knuffel::Decode::decode_node(child, ctx)?;
                shadow.offset = (o.x.0, o.y.0);
            }
            _ => unexpected(child, ctx),
        }
    }
    let passes = passes.ok_or_else(|| DecodeError::missing(node, "text-shadow needs passes"))?;
    shadow.passes = passes;
    Ok((passes > 0).then_some(shadow))
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for ImageWidget {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut common = Common::with_z(0);
        let mut path = None;
        let mut size = 150.;
        let mut corner_radius = None;
        let mut border = None;
        let mut shadow = Shadow::default();
        let mut rotation = 0.;
        for child in decode_common(node, ctx, &mut common)? {
            match &**child.node_name {
                "path" => path = Some(image_path(child, ctx)?),
                "size" => size = arg::<_, FloatOrInt<1, 10000>>(child, ctx)?.0,
                "corner-radius" => corner_radius = decode_corner_radius(child, ctx)?,
                "border" => border = Some(decode_border(child, ctx)?),
                "shadow" => decode_shadow(child, ctx, &mut shadow)?,
                "rotate" => rotation = rotate(child, ctx)?,
                _ => unexpected(child, ctx),
            }
        }
        let path = path.ok_or_else(|| DecodeError::missing(node, "image needs `path`"))?;
        Ok(ImageWidget {
            common,
            path,
            size,
            corner_radius,
            border,
            shadow,
            rotate: rotation,
        })
    }
}

/// `rotate <degrees>`: hyprlock's counter-clockwise angle, normalised to 0..360.
fn rotate<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<f64, DecodeError<S>> {
    let angle = arg::<_, FloatOrInt<-3600, 3600>>(node, ctx)?
        .0
        .rem_euclid(360.);
    // rem_euclid of a tiny negative angle rounds to exactly 360, which is no rotation.
    Ok(if angle >= 360. { 0. } else { angle })
}

/// `color "#.."` or `gradient from=.. to=..` among a node's children.
fn colour_or_gradient<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
) -> Result<Gradient, DecodeError<S>> {
    match &*node.node_name {
        n if n.ends_with("gradient") => knuffel::Decode::decode_node(node, ctx),
        _ => Ok(Gradient::from(color(node, ctx)?)),
    }
}

/// `check { color ..; text ..; }` / `fail { gradient ..; text ..; }`
fn decode_state_style<S: knuffel::traits::ErrorSpan>(
    node: &Node<S>,
    ctx: &mut Ctx<S>,
    style: &mut StateStyle,
) -> Result<(), DecodeError<S>> {
    super::copied::utils::expect_only_children(node, ctx);
    for child in node.children() {
        match &**child.node_name {
            "color" | "gradient" => style.color = colour_or_gradient(child, ctx)?,
            "text" => style.text = arg(child, ctx)?,
            _ => unexpected(child, ctx),
        }
    }
    Ok(())
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for InputField {
    fn decode_node(node: &Node<S>, ctx: &mut Ctx<S>) -> Result<Self, DecodeError<S>> {
        let mut f = InputField::default();
        for child in decode_common(node, ctx, &mut f.common)? {
            match &**child.node_name {
                "size" => f.size = decode_size(child, ctx)?,
                "corner-radius" => f.corner_radius = decode_corner_radius(child, ctx)?,
                "inner-color" => f.inner_color = color(child, ctx)?,
                "outline" => f.outline = decode_border(child, ctx)?,
                "font" => f.font = arg(child, ctx)?,
                "font-color" => f.font_color = color(child, ctx)?,
                "placeholder-text" => f.placeholder = arg(child, ctx)?,
                "dots" => {
                    for (name, val) in &child.properties {
                        match &***name {
                            "size" => {
                                let v: FloatOrInt<0, 1> = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                                f.dots.size = v.0.clamp(0.001, 0.8);
                            }
                            "spacing" => {
                                let v: FloatOrInt<-1, 1> = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                                f.dots.spacing = v.0;
                            }
                            "center" => f.dots.center = knuffel::traits::DecodeScalar::decode(val, ctx)?,
                            "corner-radius" => {
                                let v: FloatOrInt<-2, 1000> = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                                f.dots.corner_radius = match v.0 {
                                    -1. => DotRadius::Round,
                                    -2. => DotRadius::Inherit,
                                    r if r >= 0. => DotRadius::Px(r),
                                    _ => {
                                        ctx.emit_error(DecodeError::conversion(
                                            &val.literal,
                                            "dot corner-radius must be >= 0, -1 (round) or -2 (follow the field)",
                                        ));
                                        DotRadius::Round
                                    }
                                };
                            }
                            "text" => f.dots.text = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?),
                            other => ctx.emit_error(DecodeError::unexpected(
                                name,
                                "property",
                                format!("unexpected property `{other}`; expected size, spacing, center, corner-radius or text"),
                            )),
                        }
                    }
                }
                "hide-input" => {
                    // `hide-input` (or `true`) uses hyprlock's base colour, `hide-input "#..."`
                    // sets it, `hide-input false` turns it off like any other flag.
                    let colour = child
                        .arguments
                        .first()
                        .is_some_and(|a| matches!(&*a.literal, knuffel::ast::Literal::String(_)));
                    f.hide_input = if colour {
                        Some(color(child, ctx)?)
                    } else {
                        flag(child, ctx)?.then_some(hex(0x00ff99ee))
                    };
                }
                "fade-on-empty" => {
                    // A flag with an optional timeout-ms; `false` wins over the timeout.
                    let on = flag_value(child, ctx)?;
                    let mut timeout = 2000;
                    for (name, val) in &child.properties {
                        match &***name {
                            "timeout-ms" => {
                                timeout = knuffel::traits::DecodeScalar::decode(val, ctx)?
                            }
                            other => ctx.emit_error(DecodeError::unexpected(
                                name,
                                "property",
                                format!("unexpected property `{other}`"),
                            )),
                        }
                    }
                    f.fade_on_empty = on.then_some(timeout);
                }
                "check" => decode_state_style(child, ctx, &mut f.check)?,
                "fail" => decode_state_style(child, ctx, &mut f.fail)?,
                "capslock-color" | "capslock-gradient" => {
                    f.capslock_color = Some(colour_or_gradient(child, ctx)?)
                }
                "numlock-color" | "numlock-gradient" => {
                    f.numlock_color = Some(colour_or_gradient(child, ctx)?)
                }
                "bothlock-color" | "bothlock-gradient" => {
                    f.bothlock_color = Some(colour_or_gradient(child, ctx)?)
                }
                "invert-numlock" => f.invert_numlock = flag(child, ctx)?,
                "swap-font-color" => f.swap_font_color = flag(child, ctx)?,
                "shadow" => decode_shadow(child, ctx, &mut f.shadow)?,
                _ => unexpected(child, ctx),
            }
        }
        Ok(f)
    }
}
