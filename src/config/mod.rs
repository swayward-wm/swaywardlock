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

pub use copied::color::Color;
pub use copied::effects::Blur;
pub use copied::error::{ConfigIncludeError, ConfigParseResult};
pub use widgets::*;

/// The fully merged configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub general: General,
    pub auth: Auth,
    /// Widgets in file order; the renderer sorts by z-index, keeping this order for ties.
    pub widgets: Vec<Widget>,
}

impl Default for Config {
    /// The compiled-in look when no config file exists: swaylock's defaults.
    fn default() -> Self {
        Self {
            general: General::default(),
            auth: Auth::default(),
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
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            pam_service: Some("swaywardlock".into()),
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
    pub fn load(path: Option<&Path>) -> miette::Result<Self> {
        let Some(path) = path else {
            tracing::info!("no config file found; using the built-in defaults");
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(path)
            .map_err(|err| miette::miette!("error reading {path:?}: {err}"))?;
        let config = Self::parse(path, &text).config?;
        tracing::debug!("loaded config from {path:?}");
        Ok(config)
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
