//! swaywardlock: a screen locker for the swayward-wm family.

mod auth;
mod config;
mod egl;
mod process;
mod render;
mod secret;
mod state;
mod surface;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::Context as _;
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use clap::{Parser, Subcommand};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers, RawModifiers,
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
use crate::render::blur::BlurProgram;
use crate::state::{KeyOutcome, LockState};
use crate::surface::{Images, OutputInfo, Surface};

/// Exit codes from the spec.
const EXIT_NEVER_LOCKED: u8 = 1;
const EXIT_LOCK_FINISHED: u8 = 2;

#[derive(Parser, Debug)]
#[command(version, about = "Screen locker for the swayward-wm family")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Config file (default: $XDG_CONFIG_HOME/swaywardlock/config.kdl).
    #[arg(short, long, env = "SWAYWARDLOCK_CONFIG", global = true)]
    config: Option<PathBuf>,

    /// Fork into the background once the session is locked.
    #[arg(short = 'f', long)]
    daemonize: bool,

    /// Write a newline to this file descriptor once the session is locked, then close it.
    #[arg(long, value_name = "FD")]
    ready_fd: Option<i32>,

    /// Wayland display to connect to (default: $WAYLAND_DISPLAY).
    #[arg(long, value_name = "NAME")]
    display: Option<String>,

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
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    gpu: Gpu,
    blur: Option<BlurProgram>,
    config: Config,
    images: Images,
    auth: Option<Authenticator>,
    state: LockState,
    timer: Option<RegistrationToken>,
    /// An unlock (auth success or SIGUSR1) that arrived before `locked`. The protocol forbids
    /// unlocking earlier, so it runs as soon as `locked` arrives.
    unlock_pending: bool,
    ready_fd: Option<i32>,
    daemon: Daemon,
    /// Set when the loop should stop, with the exit code.
    exit: Option<u8>,
}

fn main() -> ExitCode {
    // Block SIGUSR1 before any thread exists so every thread inherits the mask. Otherwise the
    // kernel may deliver it to a Mesa or auth thread, whose default action kills the process; the
    // calloop signalfd below only sees signals blocked in every thread.
    block_unlock_signal();

    let cli = Cli::parse();
    init_logging(&cli);

    let path = config::resolve_path(cli.config.as_deref());
    let config = match Config::load(path.as_deref()) {
        Ok(config) => config,
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

    match run(cli, config, daemon) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::from(EXIT_NEVER_LOCKED)
        }
    }
}

fn block_unlock_signal() {
    // SAFETY: plain signal-mask calls on a local set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
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
        .with_writer(std::io::stderr)
        .init();
}

/// Everything up to the event loop. Errors here happen before `lock()` and map to exit 1.
fn run(cli: Cli, config: Config, mut daemon: Daemon) -> anyhow::Result<u8> {
    if let Some(display) = &cli.display {
        // SAFETY: still single-threaded except for a possible logging thread that does not read
        // the environment.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", display) };
    }
    let conn = Connection::connect_to_env().context("error connecting to the Wayland display")?;
    let (globals, queue) = registry_queue_init(&conn).context("error reading Wayland globals")?;
    let qh: QueueHandle<App> = queue.handle();

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();

    let session_lock_state = SessionLockState::new(&globals, &qh);
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor is missing")?;

    // Renderer and shaders before lock: a GL failure must never leave a locked session showing
    // nothing.
    let mut gpu = Gpu::new(&conn)?;
    let blur = match BlurProgram::compile(&mut gpu.renderer) {
        Ok(program) => Some(program),
        Err(err) => {
            tracing::warn!("blur unavailable: {err:#}");
            None
        }
    };
    let images = Images::load_all(&config);

    let auth = match &config.auth.pam_service {
        Some(service) => {
            let (auth_tx, auth_rx) = calloop::channel::channel();
            handle
                .insert_source(auth_rx, |event, _, app| {
                    if let calloop::channel::Event::Msg(event) = event {
                        app.on_auth(event);
                    }
                })
                .map_err(|e| e.error)?;
            Some(Authenticator::spawn(service.clone(), auth_tx)?)
        }
        None => {
            tracing::warn!("PAM is disabled in the config; only SIGUSR1 can unlock");
            None
        }
    };

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
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        gpu,
        blur,
        state: LockState::new(&config.general),
        config,
        images,
        auth,
        timer: None,
        unlock_pending: false,
        ready_fd: cli.ready_fd,
        daemon: Daemon::none(),
        exit: None,
    };

    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| e.error)?;

    // Learn outputs (names, descriptions) and seats before locking.
    event_loop.dispatch(Some(std::time::Duration::ZERO), &mut app)?;
    conn.roundtrip()?;
    event_loop.dispatch(Some(std::time::Duration::ZERO), &mut app)?;

    // Test builds can request an unlock before `locked` arrives, to exercise the deferral.
    #[cfg(feature = "test-auth")]
    if std::env::var_os("SWAYWARDLOCK_TEST_EARLY_UNLOCK").is_some() {
        app.unlock_pending = true;
    }

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

    let signals = Signals::new(&[Signal::SIGUSR1])?;
    handle
        .insert_source(signals, |_, _, app| {
            tracing::info!("unlocking on SIGUSR1");
            app.request_unlock();
        })
        .map_err(|e| e.error)?;

    while app.exit.is_none() {
        event_loop.dispatch(None, &mut app)?;
    }
    let code = app.exit.unwrap_or(EXIT_NEVER_LOCKED);
    app.daemon.report(code);
    Ok(code)
}

impl App {
    fn add_lock_surface(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let Some(lock) = &self.session_lock else {
            return;
        };
        let surface = self.compositor.create_surface(qh);
        let lock_surface = lock.create_lock_surface(surface, &output, qh);
        self.surfaces.push(Surface::new(lock_surface, output));
    }

    fn output_info(&self, output: &wl_output::WlOutput) -> OutputInfo {
        let info = self.outputs.info(output);
        OutputInfo {
            name: info.as_ref().and_then(|i| i.name.clone()),
            description: info.and_then(|i| i.description),
        }
    }

    /// The only place that unlocks the session.
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
        lock.unlock();
        let _ = self.conn.roundtrip();
        self.exit = Some(0);
    }

    fn on_auth(&mut self, event: AuthEvent) {
        match event {
            AuthEvent::Success => {
                tracing::info!("authenticated");
                self.request_unlock();
            }
            AuthEvent::Failure(msg) => {
                tracing::info!("authentication failed: {msg}");
                self.state.on_auth_failed(Instant::now());
                self.state_changed();
            }
        }
    }

    fn on_key(&mut self, event: KeyEvent) {
        let outcome = self.state.on_key(
            event.keysym,
            event.utf8.as_deref(),
            self.modifiers.ctrl,
            Instant::now(),
        );
        match outcome {
            KeyOutcome::None => return,
            KeyOutcome::Redraw => {}
            KeyOutcome::Submit => {
                let secret = self.state.take_password();
                match &self.auth {
                    Some(auth) => auth.submit(secret),
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
        for surface in &mut self.surfaces {
            if let Err(err) = surface.draw(
                &mut self.gpu.renderer,
                self.blur.as_ref(),
                &self.config,
                &self.images,
                &self.state,
            ) {
                tracing::warn!("error drawing: {err:#}");
            }
        }
    }

    fn reconfigure(&mut self, index: usize) {
        let info = self.output_info(&self.surfaces[index].output);
        let surface = &mut self.surfaces[index];
        surface.layout(&self.config, &info);
        if let Err(err) = surface.ensure_egl(&self.gpu) {
            tracing::error!("cannot create a surface for an output: {err:#}");
            return;
        }
        if let Err(err) = surface.draw(
            &mut self.gpu.renderer,
            self.blur.as_ref(),
            &self.config,
            &self.images,
            &self.state,
        ) {
            tracing::warn!("error drawing: {err:#}");
        }
    }
}

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        tracing::info!("session locked");
        if let Err(err) = process::notify_ready(self.ready_fd.take()) {
            tracing::error!("failed to write the ready fd: {err}");
        }
        self.daemon.report(0);
        if self.unlock_pending {
            self.request_unlock();
        }
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        tracing::error!("the compositor refused or ended the lock; is another locker running?");
        // Nothing to unlock; drop the lock object without `unlock_and_destroy`.
        self.session_lock = None;
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
            .position(|s| s.lock_surface.wl_surface() == lock_surface.wl_surface())
        else {
            return;
        };
        let (w, h) = configure.new_size;
        self.surfaces[index].size = (w as i32, h as i32).into();
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
        // Integer scale for now; fractional scale arrives with Phase 2.
        let Some(index) = self
            .surfaces
            .iter()
            .position(|s| s.lock_surface.wl_surface() == surface)
        else {
            return;
        };
        if self.surfaces[index].scale != factor {
            self.surfaces[index].scale = factor;
            if self.surfaces[index].size.w > 0 {
                self.reconfigure(index);
            }
        }
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
        if self.session_lock.is_some() && !self.surfaces.iter().any(|s| s.output == output) {
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
        self.surfaces.retain(|s| s.output != output);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        tracing::debug!("seat capability added: {capability}");
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                match self.seats.get_keyboard(qh, &seat, None) {
                    Ok(keyboard) => self.keyboard = Some(keyboard),
                    Err(err) => tracing::warn!("cannot get a keyboard: {err}"),
                }
            }
            Capability::Pointer if self.pointer.is_none() => {
                match self.seats.get_pointer(qh, &seat) {
                    Ok(pointer) => self.pointer = Some(pointer),
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
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        tracing::debug!("seat capability removed: {capability}");
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release();
                    }
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release();
                    }
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        tracing::debug!("keyboard focus entered");
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.on_key(event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        // Only deletion repeats, as in hyprlock.
        if matches!(event.keysym, Keysym::BackSpace | Keysym::Delete) {
            self.on_key(event);
        }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        layout: u32,
    ) {
        self.modifiers = modifiers;
        let changed = self.state.caps_lock != modifiers.caps_lock || self.state.layout != layout;
        self.state.caps_lock = modifiers.caps_lock;
        self.state.layout = layout;
        if changed {
            self.state_changed();
        }
    }

    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        self.state.layout_names = layout_names(&keymap.as_string());
        tracing::debug!("keymap layouts: {:?}", self.state.layout_names);
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

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
delegate_dispatch2!(App);
