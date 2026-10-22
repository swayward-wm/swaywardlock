//! swaywardlock: a screen locker for the swayward-wm family.

#[allow(dead_code)] // copied from swayward wholesale; not every helper is used here
mod animation;
mod auth;
mod commands;
mod config;
mod egl;
mod fade;
mod fprint;
mod labels;
mod preview;
mod process;
mod render;
mod scale;
mod screencopy;
mod secret;
mod state;
mod surface;
mod textvars;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use clap::{CommandFactory as _, Parser, Subcommand};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers, RawModifiers, RepeatInfo,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::session_lock::{
    SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
    SessionLockSurfaceConfigure,
};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface};
use wayland_client::{Connection, Proxy as _, QueueHandle};

use crate::auth::{AuthEvent, Authenticator};
use crate::config::Config;
use crate::egl::Gpu;
use crate::process::Daemon;
use crate::state::{KeyOutcome, LockState};
use crate::surface::{Dynamic, Images, OutputInfo, Role, Surface};

/// Exit codes from the spec.
const EXIT_NEVER_LOCKED: u8 = 1;
const EXIT_LOCK_FINISHED: u8 = 2;
/// Something failed after `lock()` (the compositor died, an event-loop error). The compositor
/// keeps the session locked, so this must not look like 1 ("never locked").
const EXIT_FAILED_AFTER_LOCK: u8 = 3;

/// A config path from `-c` or `$SWAYWARDLOCK_CONFIG`. An empty value is an error that says where
/// it may come from: an exported but unset template variable is the usual cause.
fn config_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        Err(
            "the path is empty (if it comes from $SWAYWARDLOCK_CONFIG, unset that variable or \
             give a path)"
                .to_owned(),
        )
    } else {
        Ok(PathBuf::from(value))
    }
}

#[derive(Parser, Debug)]
#[command(version, about = "Screen locker for the swayward-wm family")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Config file (default: $XDG_CONFIG_HOME/swaywardlock/config.kdl, then
    /// /etc/swaywardlock/config.kdl, then the built-in defaults).
    #[arg(short, long, env = "SWAYWARDLOCK_CONFIG", global = true, value_parser = config_path)]
    config: Option<PathBuf>,

    /// Fork into the background once the session is locked.
    #[arg(short = 'f', long)]
    daemonize: bool,

    /// Write a newline to this file descriptor once the session is locked, then close it.
    #[arg(long, value_name = "FD")]
    ready_fd: Option<i32>,

    /// Skip the fade-in animation.
    #[arg(long)]
    no_fade_in: bool,

    /// Seconds after locking during which any key, pointer button or motion unlocks without a
    /// password (hyprlock's --grace).
    #[arg(short, long, value_name = "SECONDS", default_value_t = 0)]
    grace: u64,

    /// Lock without waiting for screenshots or decoding images (hyprlock's --immediate-render).
    #[arg(long)]
    immediate_render: bool,

    /// Wayland display to connect to (default: $WAYLAND_DISPLAY).
    #[arg(long, value_name = "NAME")]
    display: Option<String>,

    /// Draw the lock screen in an ordinary window instead of locking, and reload the config
    /// whenever it changes. For iterating on a layout; nothing is locked and no password is
    /// checked (typing still drives the indicator states). Close the window or press Ctrl+C
    /// to quit.
    #[arg(long, conflicts_with_all = ["daemonize", "ready_fd"])]
    preview: bool,

    /// With --preview, the output name the window pretends to be, for `output=` matching.
    #[arg(long, value_name = "NAME", requires = "preview")]
    preview_output: Option<String>,

    /// Verbose logging.
    #[arg(short, long, conflicts_with = "quiet", global = true)]
    verbose: bool,

    /// Errors only.
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check the config file and exit.
    Validate,
    /// Print a shell completion script to stdout.
    Completions {
        /// The shell to generate for.
        shell: clap_complete::Shell,
    },
}

struct App {
    conn: Connection,
    handle: LoopHandle<'static, App>,
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    seats: SeatState,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    surfaces: Vec<Surface>,
    /// `--preview`: an xdg window instead of a lock; see `preview.rs`.
    preview: Option<preview::Preview>,
    /// Keyboard and pointer of every seat (swaylock binds them all). A device belongs to its
    /// seat: losing a capability on one seat must not drop another seat's device.
    inputs: Vec<SeatInput>,
    /// Counts key and modifiers events across keyboards, to tell which spoke last.
    input_clock: u64,
    gpu: Gpu,
    config: Config,
    images: Images,
    auth: Option<Authenticator>,
    state: LockState,
    labels: labels::Labels,
    fades: fade::Fades,
    /// End of the grace period (`--grace`) on `CLOCK_BOOTTIME`, if any.
    grace_until: Option<Duration>,
    no_fade_in: bool,
    grace: u64,
    shm: Option<smithay_client_toolkit::shm::Shm>,
    screencopy: Option<screencopy::Screencopy>,
    scales: Option<scale::Managers>,
    timer: Option<RegistrationToken>,
    /// One-shot wake for an input field's fade-out timeout, and the clock time it is armed for.
    field_timer: Option<(RegistrationToken, std::time::Duration)>,
    /// An unlock (auth success or SIGUSR1) that arrived before `locked`. The protocol forbids
    /// unlocking earlier, so it runs as soon as `locked` arrives.
    unlock_pending: bool,
    ready_fd: Option<i32>,
    daemon: Daemon,
    /// Set when the loop should stop, with the exit code.
    exit: Option<u8>,
}

fn main() -> ExitCode {
    // Block SIGUSR1/2 before any thread exists so every thread inherits the mask. Otherwise the
    // kernel may deliver it to a Mesa or auth thread, whose default action kills the process; the
    // calloop signalfd below only sees signals blocked in every thread.
    block_unlock_signal();
    // Before any cmd[] child exists: SIGTERM/SIGINT/SIGHUP still end the process (never
    // unlocking), but take the children's process groups with it.
    commands::kill_children_on_termination();

    let cli = Cli::parse();
    // Before logging, the config and anything that touches Wayland: the script must come out even
    // when the config is broken.
    if let Some(Command::Completions { shell }) = cli.command {
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "swaywardlock",
            &mut std::io::stdout(),
        );
        return ExitCode::SUCCESS;
    }
    init_logging(&cli);
    // Before anything opens a descriptor that could reuse the number, and before cmd[] children
    // exist (they must not inherit it).
    if let Some(fd) = cli.ready_fd {
        if let Err(err) = process::check_ready_fd(fd) {
            tracing::error!("{err:#}");
            return ExitCode::from(EXIT_NEVER_LOCKED);
        }
    }

    let path = config::resolve_path(cli.config.as_deref());
    // `validate` without a config prints its own line below; the load would log the same news.
    let validating_default = matches!(cli.command, Some(Command::Validate)) && path.is_none();
    let loaded = if validating_default {
        Ok((Config::default(), Vec::new()))
    } else {
        Config::load_with_includes(path.as_deref())
    };
    let (config, config_files) = match loaded {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("{err:?}");
            return ExitCode::from(EXIT_NEVER_LOCKED);
        }
    };
    if let Some(Command::Validate) = cli.command {
        match &path {
            Some(path) => println!("{}: ok", path.display()),
            None => println!("no config file; the built-in defaults are valid"),
        }
        return ExitCode::SUCCESS;
    }

    // Running out of descriptors after `lock()` would leave a session nobody can unlock.
    if let Err(err) = process::ensure_fd_limit() {
        tracing::error!("{err:#}");
        return ExitCode::from(EXIT_NEVER_LOCKED);
    }
    match boottime() {
        Some(now) if grace_deadline(now, cli.grace).is_none() => {
            tracing::error!("--grace {}: too large", cli.grace);
            return ExitCode::from(EXIT_NEVER_LOCKED);
        }
        // Lock anyway: refusing would leave the session unlocked. There is just no grace.
        None if cli.grace > 0 => {
            tracing::warn!("cannot read CLOCK_BOOTTIME; --grace is ignored");
        }
        _ => {}
    }

    // Fork before any thread exists (see process.rs).
    let daemon = if cli.daemonize {
        match Daemon::fork() {
            Ok(daemon) => daemon,
            Err(err) => {
                tracing::error!("failed to daemonize: {err:#}");
                return ExitCode::from(EXIT_NEVER_LOCKED);
            }
        }
    } else {
        Daemon::none()
    };

    let preview = cli.preview.then(|| preview::Options {
        path: path.clone(),
        files: config_files,
        output: cli.preview_output.clone(),
    });
    match run(cli, config, daemon, preview) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::from(EXIT_NEVER_LOCKED)
        }
    }
}

/// `CLOCK_BOOTTIME`: monotonic, and unlike `Instant` (`CLOCK_MONOTONIC`) it counts time spent
/// suspended, so a grace period started just before a suspend has run out after the resume.
/// `None` if the clock cannot be read (a sandbox or emulation layer can refuse it): callers must
/// then treat the grace period as over, never as open.
fn boottime() -> Option<Duration> {
    read_clock(libc::CLOCK_BOOTTIME)
}

fn read_clock(id: libc::clockid_t) -> Option<Duration> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime into a local struct.
    if unsafe { libc::clock_gettime(id, &mut ts) } != 0 {
        return None;
    }
    let nanos = u32::try_from(ts.tv_nsec)
        .ok()
        .filter(|&n| n < 1_000_000_000)?;
    Some(Duration::new(u64::try_from(ts.tv_sec).ok()?, nanos))
}

/// When a grace period of `secs` seconds starting at `now` ends; `None` if that does not fit.
fn grace_deadline(now: Duration, secs: u64) -> Option<Duration> {
    now.checked_add(Duration::from_secs(secs))
}

/// Whether a grace period ending at `until` (boot-time clock) is still open at `now`. Fails
/// closed: no deadline or an unreadable clock means it is over.
fn grace_open(until: Option<Duration>, now: Option<Duration>) -> bool {
    until.zip(now).is_some_and(|(t, now)| now < t)
}

fn block_unlock_signal() {
    // SAFETY: plain signal-mask calls on a local set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        // SIGUSR2 (refresh cmd[] labels) has the same kill-by-default problem.
        libc::sigaddset(&mut set, libc::SIGUSR2);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

fn init_logging(cli: &Cli) {
    let default = if cli.verbose {
        "debug"
    } else if cli.quiet {
        "error"
    } else {
        "info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(|| Lossy(std::io::stderr()))
        // On a failed write the fmt layer would eprintln!, which panics (an abort here).
        .log_internal_errors(false)
        // Escape codes only for a person at a terminal, not in a log file or a pipe.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
}

/// A writer that drops write errors. stderr can be a pipe whose reader has gone (EPIPE) or a
/// hung-up terminal (EIO) while locked; a lost log line must never take the locker down.
struct Lossy<W>(W);

impl<W: std::io::Write> std::io::Write for Lossy<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.0.flush();
        Ok(())
    }
}

/// Everything up to the event loop. Errors here happen before `lock()` and map to exit 1.
fn run(
    cli: Cli,
    config: Config,
    mut daemon: Daemon,
    preview: Option<preview::Options>,
) -> anyhow::Result<u8> {
    // A missing PAM service file falls back to /etc/pam.d/other, which refuses every password:
    // say so and never lock.
    if let (Some(service), None) = (&config.auth.pam_service, &preview) {
        auth::check_service_installed(service).map_err(anyhow::Error::msg)?;
    }

    if let Some(display) = &cli.display {
        // SAFETY: still single-threaded except for a possible logging thread that does not read
        // the environment.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", display) };
    }
    let conn = Connection::connect_to_env().with_context(|| {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "unset".to_owned());
        format!(
            "error connecting to the Wayland display ($WAYLAND_DISPLAY: {}, $XDG_RUNTIME_DIR: {}); \
             run it inside a Wayland session, set WAYLAND_DISPLAY, or pass --display NAME",
            var("WAYLAND_DISPLAY"),
            var("XDG_RUNTIME_DIR"),
        )
    })?;
    let (globals, queue) = registry_queue_init(&conn).context("error reading Wayland globals")?;
    let qh: QueueHandle<App> = queue.handle();

    // Never dropped: dropping the loop drops its signal sources, and calloop's `Signals` unblocks
    // SIGUSR1/2 on drop. A signal pending then (a second SIGUSR1 at unlock, a status bar's
    // SIGUSR2) would take its default action and kill the process with 138/140 instead of the
    // exit code. The process exits right after `run` anyway.
    let mut event_loop = std::mem::ManuallyDrop::new(EventLoop::<App>::try_new()?);
    let handle = event_loop.handle();

    let session_lock_state = SessionLockState::new(&globals, &qh);
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor is missing")?;

    // Renderer and shaders before lock: a GL failure must never leave a locked session showing
    // nothing.
    let mut gpu = Gpu::new(&conn)?;
    render::init(&mut gpu.renderer);
    let scratch = compositor.create_surface(&qh);
    gpu.check_surface(&scratch)?;
    scratch.destroy();
    // Decoding is synchronous and has no time limit (as in swaylock); --immediate-render skips
    // it, and image widgets fall back to their colour.
    let images = if cli.immediate_render {
        Images::default()
    } else {
        Images::load_all(&config)
    };

    let auth = match &config.auth.pam_service {
        // Preview never authenticates: no PAM, and no code path that could unlock anything.
        _ if preview.is_some() => None,
        Some(service) => {
            let (auth_tx, auth_rx) = calloop::channel::channel();
            handle
                .insert_source(auth_rx, |event, _, app| {
                    if let calloop::channel::Event::Msg(event) = event {
                        app.on_auth(event);
                    }
                })
                .map_err(|e| e.error)?;
            Some(Authenticator::spawn(
                service.clone(),
                config.auth.pam_reuse_password,
                auth_tx,
            )?)
        }
        None => {
            tracing::warn!("PAM is disabled in the config; only SIGUSR1 can unlock");
            None
        }
    };

    // Fingerprint unlock (optional; never in preview).
    if let (Some(fp), None) = (&config.auth.fingerprint, &preview) {
        let (tx, rx) = calloop::channel::channel();
        handle
            .insert_source(rx, |event, _, app| {
                if let calloop::channel::Event::Msg(event) = event {
                    app.on_fprint(event);
                }
            })
            .map_err(|e| e.error)?;
        fprint::spawn(
            fprint::Options {
                ready_message: fp.ready_message.clone(),
                present_message: fp.present_message.clone(),
                retry_delay: std::time::Duration::from_millis(fp.retry_delay_ms.into()),
            },
            tx,
        );
    }

    let mut state = LockState::new(&config.general);
    if config.auth.pam_service.is_some() {
        // No transaction runs before the first submit; PAM's own question replaces this once a
        // transaction has asked one. Preview shows it too, for layout work.
        state.pam_prompt = auth::DEFAULT_PROMPT.to_owned();
    }
    let mut app = App {
        conn: conn.clone(),
        handle: handle.clone(),
        registry: RegistryState::new(&globals),
        compositor,
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        session_lock_state,
        session_lock: None,
        surfaces: Vec::new(),
        preview: None,
        inputs: Vec::new(),
        input_clock: 0,
        gpu,
        labels: labels::Labels::new(&config, &state, &handle)?,
        state,
        fades: fade::Fades::new(config.animations),
        grace_until: None,
        no_fade_in: cli.no_fade_in,
        grace: cli.grace,
        shm: smithay_client_toolkit::shm::Shm::bind(&globals, &qh).ok(),
        screencopy: None,
        scales: scale::Managers::bind(&globals, &qh),
        config,
        images,
        auth,
        timer: None,
        field_timer: None,
        unlock_pending: false,
        ready_fd: cli.ready_fd,
        daemon: Daemon::none(),
        exit: None,
    };

    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| e.error)?;

    // Learn outputs (names, descriptions) and seats before locking.
    dispatch(&mut event_loop, &mut app, Some(std::time::Duration::ZERO))?;
    conn.roundtrip()?;
    dispatch(&mut event_loop, &mut app, Some(std::time::Duration::ZERO))?;

    // Screenshots must be taken before locking: afterwards only lock surfaces are visible.
    let wants_screenshot = app
        .config
        .widgets
        .iter()
        .any(|w| matches!(w, config::Widget::Background(b) if b.screenshot));
    if wants_screenshot && !cli.immediate_render {
        capture_screenshots(&mut app, &mut event_loop, &globals, &qh)?;
    }

    // SIGUSR2 refreshes cmd[] labels, in preview too. SIGUSR1 comes after the preview branch, so
    // the preview has no unlock path.
    let refresh = Signals::new(&[Signal::SIGUSR2])?;
    handle
        .insert_source(refresh, |_, _, app| app.labels.refresh(&app.state))
        .map_err(|e| e.error)?;

    if let Some(options) = preview {
        preview::start(&mut app, &globals, &qh, options)?;
        while app.exit.is_none() {
            dispatch(&mut event_loop, &mut app, None)?;
        }
        return Ok(0);
    }

    // Registered before `lock()`, so a failure here is still "never locked". Nothing dispatches
    // until after `lock()`, so a SIGUSR1 already pending is read then, and `request_unlock`
    // defers it until `locked`.
    let signals = Signals::new(&[Signal::SIGUSR1])?;
    handle
        .insert_source(signals, |_, _, app| {
            tracing::info!("unlocking on SIGUSR1");
            app.request_unlock();
        })
        .map_err(|e| e.error)?;

    let lock = app
        .session_lock_state
        .lock(&qh)
        .context("the compositor does not support ext-session-lock-v1")?;
    app.session_lock = Some(lock);
    for output in app.outputs.outputs() {
        app.add_lock_surface(&qh, output);
    }

    // From here on the daemon pipe belongs to the app: it reports 0 on `locked`, 2 on `finished`.
    app.daemon = std::mem::replace(&mut daemon, Daemon::none());

    // From here on nothing may return an error: the session may be locked, so a failure exits 3.
    let code = loop {
        if let Some(code) = app.exit {
            break code;
        }
        if let Err(err) = dispatch(&mut event_loop, &mut app, None) {
            tracing::error!("{err:#}; the session stays locked");
            break EXIT_FAILED_AFTER_LOCK;
        }
    };
    app.daemon.report(code);
    Ok(code)
}

/// One pass of the event loop. A protocol error from the compositor (`wl_display.error`) is an
/// error here too: calloop-wayland-source drops it, then wakes the loop at once on every pass
/// (libwayland refuses to read once the display has an error), so the loop would spin forever
/// at full CPU, logging the error each time, and never exit.
fn dispatch(
    event_loop: &mut EventLoop<'static, App>,
    app: &mut App,
    timeout: Option<std::time::Duration>,
) -> anyhow::Result<()> {
    event_loop.dispatch(timeout, app)?;
    match app.conn.protocol_error() {
        Some(err) => Err(anyhow::anyhow!("Wayland protocol error: {err}")),
        None => Ok(()),
    }
}

/// How long to wait for screenshots before locking anyway (spec: at most 2 s).
const SCREENSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn capture_screenshots(
    app: &mut App,
    event_loop: &mut EventLoop<'static, App>,
    globals: &wayland_client::globals::GlobalList,
    qh: &QueueHandle<App>,
) -> anyhow::Result<()> {
    let outputs: Vec<(u32, wl_output::WlOutput)> = app
        .outputs
        .outputs()
        .map(|o| (o.id().protocol_id(), o))
        .collect();
    if app.shm.is_none() {
        tracing::warn!("screenshot background: no wl_shm; using the colour");
        return Ok(());
    }
    let Some(sc) = screencopy::Screencopy::start(globals, qh, &outputs) else {
        return Ok(());
    };
    app.screencopy = Some(sc);
    let deadline = Instant::now() + SCREENSHOT_TIMEOUT;
    while !app.screencopy.as_ref().is_some_and(|s| s.is_finished()) {
        let now = Instant::now();
        if now >= deadline {
            tracing::warn!("screenshots did not finish in time; locking without them");
            // No capture may continue once the session is locked.
            if let Some(sc) = &mut app.screencopy {
                sc.abandon();
            }
            break;
        }
        dispatch(event_loop, app, Some(deadline - now))?;
    }
    tracing::debug!(
        "captured {} screenshot(s)",
        app.screencopy.as_ref().map_or(0, |s| s.done.len())
    );
    Ok(())
}

impl App {
    fn on_locked(&mut self) {
        tracing::info!("session locked");
        if let Err(err) = process::notify_ready(self.ready_fd.take()) {
            tracing::error!("failed to write the ready fd: {err}");
        }
        self.daemon.report(0);
        if self.grace > 0 {
            // Checked before locking; if it still does not fit or the clock cannot be read, no
            // grace (never a panic here).
            self.grace_until = boottime().and_then(|now| grace_deadline(now, self.grace));
        }
        if self.unlock_pending {
            self.request_unlock();
            return;
        }
        // A compositor may wait for our first frame before sending `locked`; that frame is
        // already on screen at full opacity, and fading in now would flash it to black.
        let shown = self.surfaces.iter().any(|s| s.presented);
        if shown && !self.no_fade_in {
            tracing::debug!("a frame was shown before locked; no fade-in");
        }
        if !self.no_fade_in && !shown {
            self.fades.fade_in();
            fade::ensure_ticking(self);
        }
    }

    fn add_lock_surface(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let Some(lock) = &self.session_lock else {
            return;
        };
        let surface = self.compositor.create_surface(qh);
        let lock_surface = lock.create_lock_surface(surface, &output, qh);
        let shot = self
            .screencopy
            .as_mut()
            .and_then(|sc| sc.done.remove(&output.id().protocol_id()))
            .and_then(|s| {
                surface::Image::from_argb(s.width, s.height, &s.data)
                    .map_err(|e| tracing::warn!("screenshot: {e:#}"))
                    .ok()
            });
        let mut s = Surface::new(Role::Lock {
            lock_surface,
            output,
        });
        s.screenshot = shot;
        s.fractional = self.scales.as_ref().map(|m| m.attach(s.wl_surface(), qh));
        self.surfaces.push(s);
    }

    fn output_info(&self, output: &wl_output::WlOutput) -> OutputInfo {
        let info = self.outputs.info(output);
        OutputInfo {
            name: info.as_ref().and_then(|i| i.name.clone()),
            description: info.and_then(|i| i.description),
        }
    }

    /// Every unlock request (auth, grace, SIGUSR1) comes here. It fades out, then `finish_unlock`
    /// does the unlocking; with no fade out it calls `finish_unlock` at once.
    fn request_unlock(&mut self) {
        let Some(lock) = self.session_lock.take() else {
            return;
        };
        if !lock.is_locked() {
            // Not locked yet: keep the lock object and unlock on `locked`.
            self.session_lock = Some(lock);
            tracing::info!("unlock requested before the session was locked; deferring");
            self.unlock_pending = true;
            return;
        }
        self.session_lock = Some(lock);
        if self.fades.unlock_when_done {
            // A second request (another password, SIGUSR1, a fingerprint) during the fade-out
            // unlocks now, so a long fade can always be cut short.
            self.fades.unlock_when_done = false;
            self.finish_unlock();
            return;
        }
        if self.fades.fade_out() {
            self.fades.unlock_when_done = true;
            fade::ensure_ticking(self);
        } else {
            self.finish_unlock();
        }
    }

    /// Unlock now. Only called by `request_unlock`, directly or when its fade out ends.
    fn finish_unlock(&mut self) {
        let Some(lock) = self.session_lock.take() else {
            return;
        };
        // Set first: nothing draws once `exit` is set (see `draw_all` and the fade ticker).
        self.exit = Some(0);
        lock.unlock();
        // The lock surfaces are gone on the compositor's side. A leftover timer (the fade-in
        // ticker) drawing into one blocked in eglSwapBuffers forever, so drop them now; the EGL
        // surface goes before its wl_surface (see `Surface`).
        self.surfaces.clear();
        let _ = self.conn.roundtrip();
    }

    fn on_fprint(&mut self, event: fprint::FprintEvent) {
        match event {
            fprint::FprintEvent::Matched => {
                tracing::info!("fingerprint matched");
                self.request_unlock();
            }
            fprint::FprintEvent::Prompt(p) => {
                self.state.fprint_prompt = p;
                self.state_changed();
            }
            fprint::FprintEvent::Failed(why) => {
                tracing::info!("fingerprint: {why}");
                self.state.on_fprint_failed(Instant::now(), &why);
                self.state_changed();
            }
            fprint::FprintEvent::Unavailable(why) => {
                self.state.fprint_prompt.clear();
                self.state.fprint_fail = why;
                self.state_changed();
            }
        }
    }

    fn on_auth(&mut self, event: AuthEvent) {
        if self.exit.is_some() {
            return; // unlocked: nothing may draw on the lock surfaces any more
        }
        match event {
            AuthEvent::Success => {
                tracing::info!("authenticated");
                self.request_unlock();
            }
            AuthEvent::Prompt { text, follow_up } => {
                tracing::debug!("PAM prompt: {text}");
                self.state.on_auth_prompt(text, follow_up);
                self.state_changed();
            }
            AuthEvent::Failure(msg) => {
                tracing::info!("authentication failed: {msg}");
                self.state.on_auth_failed_with(Instant::now(), &msg);
                self.state_changed();
            }
        }
    }

    /// Within the grace period any input unlocks without a password (hyprlock's --grace).
    fn in_grace(&self) -> bool {
        grace_open(self.grace_until, boottime())
    }

    fn on_key(&mut self, keyboard: &wl_keyboard::WlKeyboard, event: KeyEvent) {
        if self.in_grace() {
            // Only the first input: a second request would cut the fade-out short.
            if !self.fades.unlock_when_done {
                tracing::info!("input during the grace period; unlocking");
                self.request_unlock();
            }
            return;
        }
        // Ctrl is per keyboard: holding it on one keyboard does not change keys from another.
        let ctrl = self
            .input_of(keyboard)
            .is_some_and(|i| i.keys.modifiers.ctrl);
        let outcome = self
            .state
            .on_key(event.keysym, event.utf8.as_deref(), ctrl, Instant::now());
        match outcome {
            KeyOutcome::None => return,
            KeyOutcome::Redraw => {}
            KeyOutcome::Submit => {
                let secret = self.state.take_password();
                match &self.auth {
                    Some(auth) => auth.submit(secret),
                    None if self.preview.is_some() => preview::fake_check(self),
                    None => self.state.on_auth_failed(Instant::now()),
                }
            }
        }
        self.state_changed();
    }

    /// Redraw and re-arm the timer for the next state deadline.
    fn state_changed(&mut self) {
        self.draw_all();
        if let Some(token) = self.timer.take() {
            self.handle.remove(token);
        }
        if let Some(deadline) = self.state.next_deadline() {
            let timer = Timer::from_deadline(deadline);
            self.timer = self
                .handle
                .insert_source(timer, |_, _, app| {
                    // `state_changed` removes this source and arms a fresh one, so return Drop
                    // after it; otherwise keep this source and re-arm it in place.
                    if app.state.tick(Instant::now()) {
                        app.timer = None;
                        app.state_changed();
                        return TimeoutAction::Drop;
                    }
                    match app.state.next_deadline() {
                        Some(next) => TimeoutAction::ToInstant(next),
                        None => {
                            app.timer = None;
                            TimeoutAction::Drop
                        }
                    }
                })
                .ok();
        }
    }

    fn draw_all(&mut self) {
        if self.exit.is_some() {
            return;
        }
        tracing::trace!("draw_all");
        // Animations started during this draw must start at the current time, not at whenever
        // the clock was last read.
        self.fades.tick_clock();
        for surface in &mut self.surfaces {
            let dynamic = Dynamic {
                vars: self.labels.vars(&self.state),
                cmd_output: &self.labels.output,
                opacity: self.fades.opacity(),
                clock: self.fades.clock(),
            };
            if let Err(err) = surface.draw(
                &mut self.gpu.renderer,
                &self.config,
                &self.images,
                &self.state,
                &dynamic,
            ) {
                tracing::warn!("error drawing: {err:#}");
            }
        }
        self.after_draw();
    }

    /// Something is animating and needs frames at the redraw rate: a whole-screen fade, or an
    /// input field's or a ring's animation. Not the latter once an unlock is pending: the unlock
    /// never waits for a widget.
    fn needs_frames(&self) -> bool {
        self.fades.is_animating()
            || (!self.fades.unlock_when_done
                && self
                    .surfaces
                    .iter()
                    .any(|s| s.field_animating || s.ring_animating))
    }

    /// Keep the redraw ticker running while an animation is, and wake once for the earliest
    /// input-field fade-out timeout, without ticking while merely waiting for it.
    fn after_draw(&mut self) {
        fade::ensure_ticking(self);
        let wake = if self.fades.unlock_when_done {
            None
        } else {
            self.surfaces.iter().filter_map(|s| s.field_wake).min()
        };
        if self.field_timer.as_ref().map(|(_, at)| *at) == wake {
            return;
        }
        if let Some((token, _)) = self.field_timer.take() {
            self.handle.remove(token);
        }
        let Some(at) = wake else { return };
        let delay = at.saturating_sub(self.fades.clock().now());
        let token = self
            .handle
            .insert_source(Timer::from_duration(delay), |_, _, app| {
                app.field_timer = None;
                app.draw_all();
                TimeoutAction::Drop
            })
            .ok();
        self.field_timer = token.map(|t| (t, at));
    }

    /// A new preferred scale (fractional or integer). Redraws only once a configure has given
    /// the surface a size; before that the scale is kept for the first configure.
    pub fn set_scale(&mut self, surface: &wl_surface::WlSurface, scale: f64) {
        let Some(index) = self.surfaces.iter().position(|s| s.wl_surface() == surface) else {
            return;
        };
        let s = &mut self.surfaces[index];
        s.scale_known = true;
        if s.scale != scale {
            s.scale = scale;
            if s.configured {
                self.reconfigure(index);
            }
        }
    }

    fn reconfigure(&mut self, index: usize) {
        // Nothing may be attached before the first configure (the preview's reload can get here
        // first).
        if !self.surfaces[index].configured {
            return;
        }
        let info = match &self.surfaces[index].role {
            Role::Lock { output, .. } => self.output_info(output),
            Role::Preview { info, .. } => info.clone(),
        };
        let surface = &mut self.surfaces[index];
        surface.layout(&self.config, &info);
        if let Err(err) = surface.ensure_egl(&self.gpu) {
            tracing::error!("cannot create a surface for an output: {err:#}");
            return;
        }
        self.fades.tick_clock();
        let dynamic = Dynamic {
            vars: self.labels.vars(&self.state),
            cmd_output: &self.labels.output,
            opacity: self.fades.opacity(),
            clock: self.fades.clock(),
        };
        if let Err(err) = surface.draw(
            &mut self.gpu.renderer,
            &self.config,
            &self.images,
            &self.state,
            &dynamic,
        ) {
            tracing::warn!("error drawing: {err:#}");
        }
        self.after_draw();
    }
}

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        // Test builds can act on `locked` late, as a compositor that waits for the first frame
        // before sending it does.
        #[cfg(feature = "test-auth")]
        if let Some(ms) = std::env::var("SWAYWARDLOCK_TEST_LATE_LOCKED")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            let delay = std::time::Duration::from_millis(ms);
            let _ = self
                .handle
                .insert_source(Timer::from_duration(delay), |_, _, app| {
                    app.on_locked();
                    TimeoutAction::Drop
                });
            return;
        }
        self.on_locked();
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        tracing::error!("the compositor refused or ended the lock; is another locker running?");
        // The protocol wants `unlock_and_destroy` if `locked` came first, else `destroy`; a bare
        // `destroy` after `locked` is a protocol error. sctk's `unlock` sends
        // `unlock_and_destroy` only once `locked` was received, and the drop sends `destroy`
        // otherwise. The compositor has already ended this lock, so this unlocks nothing.
        if let Some(lock) = self.session_lock.take() {
            lock.unlock();
        }
        let _ = self.conn.flush();
        self.exit = Some(EXIT_LOCK_FINISHED);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        lock_surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|s| s.wl_surface() == lock_surface.wl_surface())
        else {
            return;
        };
        let (w, h) = configure.new_size;
        let s = &mut self.surfaces[index];
        // The compositor's preferred scale for a surface comes only after its first commit, so
        // that frame would go out at scale 1, blurry on a HiDPI output. wl_output.scale is known
        // by now; wp_fractional_scale corrects a fractional output (sent rounded up).
        if !s.scale_known {
            if let Some(info) = s.output().and_then(|o| self.outputs.info(o)) {
                s.scale = f64::from(info.scale_factor.max(1));
            }
        }
        s.size = (w as i32, h as i32).into();
        s.configured = true;
        self.reconfigure(index);
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let Some(index) = self.surfaces.iter().position(|s| s.wl_surface() == surface) else {
            return;
        };
        // With wp_fractional_scale the integer factor (2 at 1.5) would fight the real scale.
        if self.surfaces[index].fractional.is_some() {
            return;
        }
        self.set_scale(surface, f64::from(factor));
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // Before `lock()` this is a no-op; outputs are picked up when locking starts.
        if self.session_lock.is_some() && !self.surfaces.iter().any(|s| s.output() == Some(&output))
        {
            self.add_lock_surface(qh, output);
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // EGL surfaces must go before their wl_surface, which drops with the lock surface.
        self.surfaces.retain(|s| s.output() != Some(&output));
    }
}

/// What one keyboard says: its modifiers (Ctrl for the shortcuts, Caps/Num for the indicators),
/// active xkb layout index and the keymap's layout names.
#[derive(Default)]
struct KeyboardState {
    modifiers: Modifiers,
    layout: u32,
    layout_names: Vec<String>,
    /// When this keyboard last sent a key or modifiers (`App::input_clock`); 0 = never.
    last_input: u64,
    /// The surface with this keyboard's focus, for the repeat to stop if it dies.
    focus: Option<wl_surface::WlSurface>,
}

/// The keyboard whose state is shown: the one that last sent a key or modifiers, else one that
/// has a keymap. `keyboards` are the ones still present.
fn active_keyboard<'a>(
    keyboards: impl Iterator<Item = &'a KeyboardState>,
) -> Option<&'a KeyboardState> {
    keyboards.max_by_key(|k| (k.last_input, !k.layout_names.is_empty()))
}

/// A held key's repeat: our own calloop timer per keyboard, so releasing the keyboard can stop
/// it. (sctk's client-side timer only stops on a key release or leave from the compositor, which
/// a capability change does not require; it would keep waking the loop until unlock.)
#[derive(Default)]
struct Repeater {
    /// Delay and gap from `wl_keyboard.repeat_info`; `None` = no client-side repeat (the
    /// compositor repeats with rate 0, or sent nothing).
    info: Option<(Duration, Duration)>,
    /// The held key being repeated and its timer.
    key: Option<(u32, RegistrationToken)>,
}

impl Repeater {
    /// Set the rate and delay from `wl_keyboard.repeat_info`.
    fn set_info(&mut self, info: RepeatInfo) {
        self.info = match info {
            RepeatInfo::Repeat { rate, delay } => Some((
                Duration::from_millis(delay.into()),
                Duration::from_micros(1_000_000 / u64::from(rate.get())),
            )),
            RepeatInfo::Disable => None,
        };
    }

    /// Start repeating `code` (replacing any held key): `tick` runs after the delay, then every
    /// gap, until it returns false or the repeat is stopped.
    fn start<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        code: u32,
        mut tick: impl FnMut(&mut D) -> bool + 'static,
    ) {
        self.stop(handle);
        let Some((delay, gap)) = self.info else {
            return;
        };
        let token = handle.insert_source(Timer::from_duration(delay), move |_, _, data| {
            if tick(data) {
                TimeoutAction::ToDuration(gap)
            } else {
                TimeoutAction::Drop
            }
        });
        if let Ok(token) = token {
            self.key = Some((code, token));
        }
    }

    /// The key went up: stop if it is the repeating one.
    fn release<D: 'static>(&mut self, handle: &LoopHandle<'static, D>, code: u32) {
        if self.key.as_ref().is_some_and(|(held, _)| *held == code) {
            self.stop(handle);
        }
    }

    fn stop<D: 'static>(&mut self, handle: &LoopHandle<'static, D>) {
        if let Some((_, token)) = self.key.take() {
            handle.remove(token);
        }
    }
}

/// One seat's input devices.
struct SeatInput {
    seat: wl_seat::WlSeat,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    keys: KeyboardState,
    repeat: Repeater,
}

impl SeatInput {
    fn release_keyboard(&mut self, handle: &LoopHandle<'static, App>) {
        self.repeat.stop(handle);
        self.repeat = Repeater::default();
        if let Some(keyboard) = self.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }
        self.keys = KeyboardState::default();
    }

    fn release_pointer(&mut self) {
        if let Some(pointer) = self.pointer.take() {
            if pointer.version() >= 3 {
                pointer.release();
            }
        }
    }
}

impl App {
    fn seat_input(&mut self, seat: &wl_seat::WlSeat) -> &mut SeatInput {
        let index = match self.inputs.iter().position(|i| &i.seat == seat) {
            Some(index) => index,
            None => {
                self.inputs.push(SeatInput {
                    seat: seat.clone(),
                    keyboard: None,
                    pointer: None,
                    keys: KeyboardState::default(),
                    repeat: Repeater::default(),
                });
                self.inputs.len() - 1
            }
        };
        &mut self.inputs[index]
    }

    /// Whether `keyboard` is one we hold.
    fn has_keyboard(&self, keyboard: &wl_keyboard::WlKeyboard) -> bool {
        self.inputs
            .iter()
            .any(|i| i.keyboard.as_ref() == Some(keyboard))
    }

    /// A repeated key, from our timer (`wl_keyboard.repeat_info` with a rate) or the compositor
    /// (v10 `repeated` state, sent only after a rate of 0, which turns the timer off, so a key
    /// never repeats twice).
    fn on_repeat(&mut self, keyboard: &wl_keyboard::WlKeyboard, event: KeyEvent) {
        if repeats(event.keysym) && self.has_keyboard(keyboard) {
            self.on_key(keyboard, event);
        }
    }

    /// A key went down on `keyboard`: it becomes the keyboard whose state is shown, and a
    /// deletion key starts repeating.
    fn on_press(&mut self, keyboard: &wl_keyboard::WlKeyboard, event: KeyEvent) {
        self.touch(keyboard, true, |_| {});
        let handle = self.handle.clone();
        if let Some(input) = self.input_of(keyboard) {
            if repeats(event.keysym) {
                let kbd = keyboard.clone();
                let tick = event.clone();
                input
                    .repeat
                    .start(&handle, event.raw_code, move |app: &mut App| {
                        // The surface can die without a leave, and the keyboard can go without a
                        // key release; either ends the repeat.
                        let alive = app
                            .input_of(&kbd)
                            .is_some_and(|i| i.keys.focus.as_ref().is_some_and(|s| s.is_alive()));
                        if !alive {
                            if let Some(input) = app.input_of(&kbd) {
                                input.repeat.key = None;
                            }
                            return false;
                        }
                        app.on_repeat(&kbd, tick.clone());
                        true
                    });
            } else {
                input.repeat.stop(&handle);
            }
        }
        self.on_key(keyboard, event);
    }

    fn input_of(&mut self, keyboard: &wl_keyboard::WlKeyboard) -> Option<&mut SeatInput> {
        self.inputs
            .iter_mut()
            .find(|i| i.keyboard.as_ref() == Some(keyboard))
    }

    /// `keyboard` sent a key, modifiers or keymap: apply `update` to its state, mark it as the
    /// one that spoke last if `speaks` (a keymap alone does not), and show the state of the
    /// keyboard that spoke last.
    fn touch(
        &mut self,
        keyboard: &wl_keyboard::WlKeyboard,
        speaks: bool,
        update: impl FnOnce(&mut KeyboardState),
    ) {
        self.input_clock += 1;
        let stamp = self.input_clock;
        if let Some(input) = self.input_of(keyboard) {
            if speaks {
                input.keys.last_input = stamp;
            }
            update(&mut input.keys);
            self.sync_keyboard_state();
        }
    }

    /// Copy the shown keyboard's layout and Caps/Num state into `state`, and redraw if it
    /// changed. With no keyboard left the state is empty.
    fn sync_keyboard_state(&mut self) {
        let shown = active_keyboard(
            self.inputs
                .iter()
                .filter(|i| i.keyboard.is_some())
                .map(|i| &i.keys),
        );
        let (layout, names, caps, num) = match shown {
            Some(k) => (
                k.layout,
                k.layout_names.clone(),
                k.modifiers.caps_lock,
                k.modifiers.num_lock,
            ),
            None => (0, Vec::new(), false, false),
        };
        let changed = self.state.caps_lock != caps
            || self.state.num_lock != num
            || self.state.layout != layout
            || self.state.layout_names != names;
        if changed {
            self.state.caps_lock = caps;
            self.state.num_lock = num;
            self.state.layout = layout;
            self.state.layout_names = names;
            self.state_changed();
        }
    }
}

/// Only deletion repeats, as in hyprlock (swaylock repeats every key). A held letter adds one
/// character, so a slow release cannot add stray characters to the password.
fn repeats(keysym: Keysym) -> bool {
    matches!(keysym, Keysym::BackSpace | Keysym::Delete)
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seat_input(&seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        tracing::debug!("seat capability added: {capability}");
        match capability {
            Capability::Keyboard if self.seat_input(&seat).keyboard.is_none() => {
                match self.seats.get_keyboard(qh, &seat, None) {
                    Ok(keyboard) => self.seat_input(&seat).keyboard = Some(keyboard),
                    Err(err) => tracing::warn!("cannot get a keyboard: {err}"),
                }
            }
            Capability::Pointer if self.seat_input(&seat).pointer.is_none() => {
                match self.seats.get_pointer(qh, &seat) {
                    Ok(pointer) => self.seat_input(&seat).pointer = Some(pointer),
                    Err(err) => tracing::warn!("cannot get a pointer: {err}"),
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        tracing::debug!("seat capability removed: {capability}");
        let Some(input) = self.inputs.iter_mut().find(|i| i.seat == seat) else {
            return;
        };
        match capability {
            Capability::Keyboard => {
                input.release_keyboard(&self.handle);
                self.sync_keyboard_state();
            }
            Capability::Pointer => input.release_pointer(),
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if let Some(index) = self.inputs.iter().position(|i| i.seat == seat) {
            let mut input = self.inputs.remove(index);
            input.release_keyboard(&self.handle);
            input.release_pointer();
            self.sync_keyboard_state();
        }
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        tracing::debug!("keyboard focus entered");
        if let Some(input) = self.input_of(keyboard) {
            input.keys.focus = Some(surface.clone());
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        // All held keys are released on leave.
        let handle = self.handle.clone();
        if let Some(input) = self.input_of(keyboard) {
            input.keys.focus = None;
            input.repeat.stop(&handle);
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.on_press(keyboard, event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.on_repeat(keyboard, event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let handle = self.handle.clone();
        if let Some(input) = self.input_of(keyboard) {
            input.repeat.release(&handle, event.raw_code);
        }
    }

    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        info: RepeatInfo,
    ) {
        if let Some(input) = self.input_of(keyboard) {
            input.repeat.set_info(info);
        }
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        layout: u32,
    ) {
        self.touch(keyboard, true, |k| {
            k.modifiers = modifiers;
            k.layout = layout;
        });
    }

    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        let names = layout_names(&keymap.as_string());
        tracing::debug!("keymap layouts: {names:?}");
        // The first frame is usually drawn before the keymap arrives; with layout 0 active no
        // modifiers event follows, so $LAYOUT and the ring's layout box would stay empty.
        self.touch(keyboard, false, |k| k.layout_names = names);
    }
}

/// Layout names from a keymap's text form. xkbcommon's keymap object is not `Send`, so sctk only
/// hands out the text; re-parsing it is how sctk suggests getting an independent keymap.
fn layout_names(keymap: &str) -> Vec<String> {
    use xkbcommon::xkb;
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let Some(keymap) = xkb::Keymap::new_from_string(
        &ctx,
        keymap.to_owned(),
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    ) else {
        return Vec::new();
    };
    (0..keymap.num_layouts())
        .map(|i| keymap.layout_get_name(i).to_owned())
        .collect()
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        if self.in_grace()
            && events.iter().any(|e| {
                matches!(
                    e.kind,
                    PointerEventKind::Motion { .. } | PointerEventKind::Press { .. }
                )
            })
        {
            if !self.fades.unlock_when_done {
                tracing::info!("pointer input during the grace period; unlocking");
                self.request_unlock();
            }
            return;
        }
        if !self.config.general.hide_cursor {
            return;
        }
        for event in events {
            if let PointerEventKind::Enter { serial } = event.kind {
                // A null cursor hides it, as swaylock does.
                pointer.set_cursor(serial, None, 0, 0);
            }
        }
    }
}

impl smithay_client_toolkit::shm::ShmHandler for App {
    fn shm_state(&mut self) -> &mut smithay_client_toolkit::shm::Shm {
        self.shm
            .as_mut()
            .expect("shm events only arrive once shm is bound")
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
delegate_dispatch2!(App);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_is_consistent_and_has_both_subcommands() {
        let cmd = Cli::command();
        cmd.clone().debug_assert();
        let names: Vec<_> = cmd.get_subcommands().map(|sub| sub.get_name()).collect();
        for want in ["validate", "completions"] {
            assert!(
                names.contains(&want),
                "missing subcommand {want}: {names:?}"
            );
        }
    }

    #[test]
    fn lossy_writer_swallows_a_broken_pipe() {
        use std::io::Write as _;
        let (read, write) = std::io::pipe().unwrap();
        drop(read);
        let mut out = Lossy(write);
        assert_eq!(out.write(b"log line\n").unwrap(), 9);
        out.flush().unwrap();
    }

    #[test]
    fn an_empty_config_path_names_the_environment_variable() {
        let err = Cli::try_parse_from(["swaywardlock", "validate", "-c", ""]).unwrap_err();
        assert!(err.to_string().contains("SWAYWARDLOCK_CONFIG"), "{err}");
        let cli = Cli::try_parse_from(["swaywardlock", "-c", "a.kdl"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("a.kdl")));
    }

    #[test]
    fn only_deletion_repeats() {
        assert!(repeats(Keysym::BackSpace));
        assert!(repeats(Keysym::Delete));
        for key in [Keysym::a, Keysym::Return, Keysym::space, Keysym::Escape] {
            assert!(!repeats(key), "{key:?} must not repeat");
        }
    }

    fn kb(last_input: u64, names: &[&str], layout: u32) -> KeyboardState {
        KeyboardState {
            layout,
            layout_names: names.iter().map(|n| n.to_string()).collect(),
            last_input,
            ..Default::default()
        }
    }

    #[test]
    fn the_keyboard_that_spoke_last_is_shown() {
        let (de, us) = (kb(5, &["German", "English"], 0), kb(3, &["English"], 0));
        assert_eq!(
            active_keyboard([&us, &de].into_iter())
                .unwrap()
                .layout_names[0],
            "German"
        );
        assert_eq!(
            active_keyboard([&de, &us].into_iter())
                .unwrap()
                .layout_names[0],
            "German"
        );
        // The newer keyboard goes: the remaining one is shown, and with none left, nothing.
        assert_eq!(
            active_keyboard([&us].into_iter()).unwrap().layout_names[0],
            "English"
        );
        assert!(active_keyboard(std::iter::empty()).is_none());
    }

    #[test]
    fn a_keyboard_that_never_spoke_shows_a_keymap_over_none() {
        let (quiet, empty) = (kb(0, &["German"], 0), kb(0, &[], 0));
        assert_eq!(
            active_keyboard([&empty, &quiet].into_iter())
                .unwrap()
                .layout_names
                .len(),
            1
        );
        assert_eq!(
            active_keyboard([&quiet, &empty].into_iter())
                .unwrap()
                .layout_names
                .len(),
            1
        );
    }

    fn repeater() -> (EventLoop<'static, u32>, Repeater) {
        let mut r = Repeater::default();
        r.set_info(RepeatInfo::Repeat {
            rate: std::num::NonZeroU32::new(100).unwrap(),
            delay: 10,
        });
        (EventLoop::try_new().unwrap(), r)
    }

    fn run(event_loop: &mut EventLoop<'static, u32>, ms: u64, count: &mut u32) {
        let end = Instant::now() + Duration::from_millis(ms);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            event_loop.dispatch(left, count).unwrap();
        }
    }

    #[test]
    fn a_held_key_repeats_until_released() {
        let (mut event_loop, mut r) = repeater();
        let handle = event_loop.handle();
        let mut count = 0;
        r.start(&handle, 14, |n: &mut u32| {
            *n += 1;
            true
        });
        run(&mut event_loop, 100, &mut count);
        assert!(count >= 2, "{count}");
        r.release(&handle, 99);
        let held = count;
        run(&mut event_loop, 50, &mut count);
        assert!(count > held, "another key's release must not stop it");
        r.release(&handle, 14);
        let stopped = count;
        run(&mut event_loop, 100, &mut count);
        assert_eq!(count, stopped);
    }

    #[test]
    fn stopping_a_repeat_removes_its_timer() {
        // What release_keyboard does: no key release came, yet nothing repeats afterwards.
        let (mut event_loop, mut r) = repeater();
        let handle = event_loop.handle();
        let mut count = 0;
        r.start(&handle, 14, |n: &mut u32| {
            *n += 1;
            true
        });
        run(&mut event_loop, 50, &mut count);
        r.stop(&handle);
        let stopped = count;
        run(&mut event_loop, 150, &mut count);
        assert_eq!(count, stopped);
        assert!(r.key.is_none());
    }

    #[test]
    fn no_repeat_without_repeat_info() {
        let (mut event_loop, _) = repeater();
        let mut r = Repeater::default();
        let mut count = 0;
        r.start(&event_loop.handle(), 14, |n: &mut u32| {
            *n += 1;
            true
        });
        run(&mut event_loop, 100, &mut count);
        assert_eq!(count, 0);
        assert!(r.key.is_none());
    }

    #[test]
    fn grace_values_that_overflow_are_refused() {
        let now = boottime().unwrap();
        assert!(grace_deadline(now, 5).is_some());
        // 2^63 overflowed `Instant` (an i64 timespec) and aborted after lock; a Duration holds it.
        assert!(grace_deadline(now, 1 << 63).is_some());
        assert_eq!(grace_deadline(now, u64::MAX), None);
    }

    #[test]
    fn grace_runs_out_on_the_boot_clock() {
        let start = Duration::from_secs(1000);
        let until = grace_deadline(start, 30);
        assert!(grace_open(until, Some(start + Duration::from_secs(29))));
        // The deadline itself is outside: --grace 30 is 30 s, not a moment more.
        assert!(!grace_open(until, Some(start + Duration::from_secs(30))));
        // A suspend moves the boot clock past the deadline, so the wake key is not grace input.
        assert!(!grace_open(until, Some(start + Duration::from_secs(3600))));
        assert!(!grace_open(None, Some(start)));
    }

    #[test]
    fn grace_is_closed_when_the_clock_cannot_be_read() {
        // A clock id the kernel rejects (EINVAL), as CLOCK_BOOTTIME is in some sandboxes.
        const NO_SUCH_CLOCK: libc::clockid_t = 1234;
        assert_eq!(read_clock(NO_SUCH_CLOCK), None);
        // As locked() and in_grace() compute them when the clock fails.
        let until = read_clock(NO_SUCH_CLOCK).and_then(|now| grace_deadline(now, 30));
        assert!(!grace_open(until, read_clock(NO_SUCH_CLOCK)));
        assert!(!grace_open(until, boottime()));
        // A deadline set while the clock worked does not stay open once it fails.
        let until = grace_deadline(boottime().unwrap(), 30);
        assert!(grace_open(until, boottime()));
        assert!(!grace_open(until, read_clock(NO_SUCH_CLOCK)));
    }

    #[test]
    fn the_boot_clock_counts_like_the_monotonic_one_while_awake() {
        let (b0, m0) = (boottime().unwrap(), Instant::now());
        std::thread::sleep(Duration::from_millis(50));
        let (db, dm) = (boottime().unwrap() - b0, m0.elapsed());
        assert!(db >= Duration::from_millis(50), "{db:?}");
        assert!(
            db.abs_diff(dm) < Duration::from_millis(20),
            "{db:?} vs {dm:?}"
        );
    }
}
