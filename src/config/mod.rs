//! KDL configuration.
//!
//! The shared vocabulary (colours, gradients, blur, `include`) decodes with code copied from
//! swayward-config (`copied/`), so it matches swayward byte for byte. The root and widget nodes are
//! swaywardlock's own.

#[macro_use]
pub mod copied;
mod decode;
mod widgets;

use std::path::{Path, PathBuf};

pub use copied::animations::{
    Animation, Curve as AnimationCurve, EasingParams, Kind as AnimationKind,
};
pub use copied::color::{
    Color, CornerRadius, Gradient, GradientColorSpace, GradientInterpolation, HueInterpolation,
};
pub use copied::effects::Blur;
pub use copied::error::{ConfigIncludeError, ConfigParseResult};
pub use copied::shadow::Shadow;
pub use widgets::*;

/// The fully merged configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub general: General,
    pub auth: Auth,
    pub animations: Animations,
    /// Widgets in file order; the renderer sorts by z-index, keeping this order for ties.
    pub widgets: Vec<Widget>,
}

impl Default for Config {
    /// The compiled-in look when no config file exists: swaylock's defaults.
    fn default() -> Self {
        Self {
            general: General::default(),
            auth: Auth::default(),
            animations: Animations::default(),
            widgets: vec![
                Widget::Background(Background::default()),
                Widget::RingIndicator(RingIndicator::default()),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct General {
    pub hide_cursor: bool,
    pub ignore_empty_input: bool,
    /// How long the wrong-password state shows, like swaylock's 3 s auth-idle timer.
    pub fail_timeout_ms: u32,
    /// Clear a stale password after this long without input (swaylock: 10 s).
    pub clear_input_after_ms: u32,
}

impl Default for General {
    fn default() -> Self {
        Self {
            hide_cursor: true,
            ignore_empty_input: false,
            fail_timeout_ms: 3000,
            clear_input_after_ms: 10_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Auth {
    /// PAM service name; `None` disables PAM.
    pub pam_service: Option<String>,
    /// fprintd fingerprint unlock; `None` (the default) disables it.
    pub fingerprint: Option<Fingerprint>,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            pam_service: Some("swaywardlock".into()),
            fingerprint: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fingerprint {
    pub ready_message: String,
    pub present_message: String,
    pub retry_delay_ms: u32,
}

impl Default for Fingerprint {
    /// hyprlock's defaults.
    fn default() -> Self {
        Self {
            ready_message: "(Scan fingerprint to unlock)".into(),
            present_message: "Scanning fingerprint".into(),
            retry_delay_ms: 250,
        }
    }
}

/// Animations for the lock screen. Each uses swayward's animation syntax (`duration-ms` +
/// `curve`, or `spring ...`, or `off`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animations {
    /// `animations { off }` disables every animation.
    pub off: bool,
    pub fade_in: Animation,
    pub fade_out: Animation,
    pub input_field_fade: Animation,
}

fn easing(ms: u32, curve: AnimationCurve) -> Animation {
    Animation {
        off: false,
        kind: AnimationKind::Easing(EasingParams {
            duration_ms: ms,
            curve,
        }),
    }
}

impl Default for Animations {
    /// Close to hyprlock's defaults: short fades.
    fn default() -> Self {
        Self {
            off: false,
            fade_in: easing(200, AnimationCurve::EaseOutCubic),
            fade_out: easing(200, AnimationCurve::EaseOutCubic),
            input_field_fade: easing(300, AnimationCurve::EaseOutCubic),
        }
    }
}

impl Animations {
    /// The effective animation, honouring the global `off`.
    pub fn get(&self, a: Animation) -> Animation {
        if self.off {
            Animation { off: true, ..a }
        } else {
            a
        }
    }
}

/// Where the config comes from, in the spec's order.
pub fn resolve_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_owned());
    }
    if let Some(path) = std::env::var_os("SWAYWARDLOCK_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))
        .map(|dir| dir.join("swaywardlock/config.kdl"));
    let system = PathBuf::from("/etc/swaywardlock/config.kdl");
    user.into_iter().chain([system]).find(|path| path.exists())
}

impl Config {
    /// Load from `path`, or return the default when `path` is `None`.
    ///
    /// An explicitly named file that is missing is an error; the search path finding nothing is
    /// not.
    #[cfg(test)]
    pub fn load(path: Option<&Path>) -> miette::Result<Self> {
        Self::load_with_includes(path).map(|(config, _)| config)
    }

    /// Like [`Config::load`], also returning every file read, for `--preview` to watch.
    pub fn load_with_includes(path: Option<&Path>) -> miette::Result<(Self, Vec<PathBuf>)> {
        let Some(path) = path else {
            tracing::info!("no config file found; using the built-in defaults");
            return Ok((Self::default(), Vec::new()));
        };
        let text = std::fs::read_to_string(path)
            .map_err(|err| miette::miette!("error reading {path:?}: {err}"))?;
        let res = Self::parse(path, &text);
        let mut files = res.includes;
        files.insert(0, path.to_owned());
        let config = res.config?;
        tracing::debug!("loaded config from {path:?}");
        Ok((config, files))
    }

    pub fn parse(path: &Path, text: &str) -> ConfigParseResult<Self, ConfigIncludeError> {
        decode::parse(path, text)
    }

    #[cfg(test)]
    pub fn parse_mem(text: &str) -> Result<Self, ConfigIncludeError> {
        Self::parse(Path::new("config.kdl"), text).config
    }
}

#[cfg(test)]
mod tests;
