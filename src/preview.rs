//! `--preview`: draw the lock screen in an ordinary xdg-shell window and reload the config when
//! it changes, so layouts can be iterated on without locking the session.
//!
//! The preview shares every widget and render path with the real lock; only the surface role
//! differs (an xdg toplevel instead of an ext-session-lock surface). It never calls `lock()`, never
//! starts PAM, and has no unlock path: Enter shows "Verifying" briefly and then "Wrong", so the
//! failure styling can be checked too.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface as _;
use wayland_client::globals::GlobalList;
use wayland_client::{Connection, QueueHandle};

use crate::config::Config;
use crate::state::LockState;
use crate::surface::{Images, OutputInfo, Role, Surface};
use crate::App;

/// How often the config files are checked for changes.
const POLL: Duration = Duration::from_millis(500);
/// How long the fake check shows "Verifying".
const FAKE_CHECK: Duration = Duration::from_millis(600);
/// The window's size before the compositor picks one.
const DEFAULT_SIZE: (u32, u32) = (1280, 800);

pub struct Options {
    /// The config file, if any; `None` previews the built-in defaults.
    pub path: Option<PathBuf>,
    /// Every file the config read (main file plus includes), to watch.
    pub files: Vec<PathBuf>,
    /// The output name the window pretends to be.
    pub output: Option<String>,
}

pub struct Preview {
    // Kept alive for the window's lifetime.
    _xdg: XdgShell,
    options: Options,
    stamps: Vec<Option<SystemTime>>,
}

pub fn start(
    app: &mut App,
    globals: &GlobalList,
    qh: &QueueHandle<App>,
    options: Options,
) -> anyhow::Result<()> {
    let xdg = XdgShell::bind(globals, qh).context("xdg-shell is missing; --preview needs it")?;
    let surface = app.compositor.create_surface(qh);
    let window = xdg.create_window(surface, WindowDecorations::ServerDefault, qh);
    window.set_title("swaywardlock preview");
    window.set_app_id("swaywardlock-preview");
    window.commit();

    let info = OutputInfo {
        name: options.output.clone(),
        description: None,
    };
    let mut surface = Surface::new(Role::Preview { window, info });
    surface.fractional = app
        .scales
        .as_ref()
        .map(|m| m.attach(surface.wl_surface(), qh));
    surface.size = (DEFAULT_SIZE.0 as i32, DEFAULT_SIZE.1 as i32).into();
    app.surfaces.push(surface);

    let stamps = stamps(&options.files);
    tracing::info!(
        "preview: watching {} file(s); edit the config and the window reloads",
        options.files.len()
    );
    app.preview = Some(Preview {
        _xdg: xdg,
        options,
        stamps,
    });

    app.handle
        .insert_source(Timer::from_duration(POLL), |_, _, app| {
            poll(app);
            TimeoutAction::ToDuration(POLL)
        })
        .map_err(|e| anyhow::anyhow!("timer: {}", e.error))?;
    Ok(())
}

fn stamps(files: &[PathBuf]) -> Vec<Option<SystemTime>> {
    files
        .iter()
        .map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
        .collect()
}

/// Reload when any watched file changed. A config that fails to parse keeps the last good one on
/// screen and prints the error, so a typo mid-edit does not kill the preview.
fn poll(app: &mut App) {
    let Some(preview) = &mut app.preview else {
        return;
    };
    let now = stamps(&preview.options.files);
    if now == preview.stamps {
        return;
    }
    preview.stamps = now;

    match Config::load_with_includes(preview.options.path.as_deref()) {
        Ok((config, files)) => {
            tracing::info!("preview: config reloaded");
            preview.stamps = stamps(&files);
            preview.options.files = files;
            app.images = Images::load_all(&config);
            app.state = LockState::new(&config.general);
            app.labels.stop(&app.handle);
            match crate::labels::Labels::new(&config, &app.state, &app.handle) {
                Ok(labels) => app.labels = labels,
                Err(err) => tracing::warn!("preview: labels: {err:#}"),
            }
            app.config = config;
            for i in 0..app.surfaces.len() {
                app.reconfigure(i);
            }
        }
        // Not eprintln!: it panics when stderr is gone (a closed terminal).
        Err(err) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "{err:?}");
        }
    }
}

/// Stand-in for PAM: show the checking state, then fail. Nothing is ever unlocked.
pub fn fake_check(app: &mut App) {
    let _ = app
        .handle
        .insert_source(Timer::from_duration(FAKE_CHECK), |_, _, app| {
            app.state.on_auth_failed(Instant::now());
            app.state_changed();
            TimeoutAction::Drop
        });
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = Some(0);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|s| s.wl_surface() == window.wl_surface())
        else {
            return;
        };
        let current = self.surfaces[index].size;
        let w = configure.new_size.0.map_or(current.w, |w| w.get() as i32);
        let h = configure.new_size.1.map_or(current.h, |h| h.get() as i32);
        self.surfaces[index].size = (w, h).into();
        self.surfaces[index].configured = true;
        self.reconfigure(index);
    }
}
