//! Runtime for `reload-ms` / `reload-cmd` on `background` and `image` widgets: which picture each
//! one shows while locked.
//!
//! A widget with a reload key looks again on a timer (`reload-ms`) and on SIGUSR2. Looking again
//! means running `reload-cmd` (its output names the new path; without a command the widget's own
//! path is checked), then decoding the file if the path or its modification time changed. The
//! command runs on [`crate::commands::Runner`]'s worker threads and the decode on a thread of its
//! own, so a slow command or a huge file never stalls input or the unlock. The worker refuses a
//! picture too big to decode safely and scales the rest down to what the outputs need, so the
//! main thread only wraps a screen-sized buffer. A new picture is a trial until the current
//! drawing batch reports its outcomes ([`Reload::settle`]); configure-triggered frames settle
//! independently, without waiting for later or unready outputs. A failed trial keeps the
//! old picture and logs a warning. A picture that arrives while a crossfade runs waits
//! for it to end. Nothing here can unlock the session, and every callback stands down once
//! `App::exit` is set.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use calloop::channel::{Event, Sender};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};

use crate::commands::{CmdOutput, Runner};
use crate::config::{Config, Fit, ReloadSpec, Widget};
use crate::render::background::{self, Decoded};
use crate::surface::{Image, Images};

/// How big a widget's picture is drawn, so the worker can scale it down to that.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cover {
    /// Fitted to the whole output (`cover`, `contain`, `fill`).
    Screen,
    /// Shorter side this many logical px (an `image`'s `size`).
    Square(f64),
    /// Drawn at its own pixel size (`tile`, `center`): never scaled.
    Native,
}

/// The largest output buffer and scale, for [`Cover`]. All zero until an output is configured,
/// when nothing is scaled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Screens {
    pub buffer: (u32, u32),
    pub scale: f64,
}

impl Screens {
    /// The size a picture drawn as `cover` must still cover; `None` keeps its own size.
    fn target(self, cover: Cover) -> Option<(u32, u32)> {
        if self.buffer.0 == 0 || self.buffer.1 == 0 {
            return None;
        }
        match cover {
            Cover::Screen => Some(self.buffer),
            Cover::Square(size) => {
                let px = (size * self.scale.max(1.)).ceil().max(1.) as u32;
                Some((px, px))
            }
            Cover::Native => None,
        }
    }
}
use crate::App;

/// What a worker found when it looked at the file.
pub enum Outcome {
    /// Same path and modification time as the last attempt.
    Unchanged,
    Failed(String),
    Loaded(Decoded),
}

pub struct Decode {
    id: usize,
    path: PathBuf,
    mtime: Option<SystemTime>,
    outcome: Outcome,
    target: Option<(u32, u32)>,
}

/// One widget that reloads.
struct Item {
    /// Index into `config.widgets`.
    index: usize,
    spec: ReloadSpec,
    /// The widget's configured path, checked when there is no command.
    path: Option<PathBuf>,
    cover: Cover,
    /// How long a new picture fades in (a background's `crossfade-ms`); zero swaps at once.
    crossfade: Duration,
}

/// The last file looked at, for the "unchanged" test.
type Seen = Option<(PathBuf, Option<SystemTime>)>;

pub struct Reload {
    runner: Runner,
    decode_tx: Sender<Decode>,
    handle: LoopHandle<'static, App>,
    items: Vec<Item>,
    /// Per widget index: a decode is running or its picture waits for a crossfade (one at a time
    /// per widget, as in hyprlock).
    pending: Vec<bool>,
    seen: Vec<Seen>,
    /// Resize work survives a pending decode or held crossfade, even with reload-ms 0.
    resize_retry: Vec<bool>,
    committed_targets: Vec<Option<(u32, u32)>>,
    trial_targets: Vec<Option<(u32, u32)>>,
    held_targets: Vec<Option<(u32, u32)>>,
    /// Per widget index, the reloaded picture on screen; `None` means the configured one.
    committed: Vec<Option<Image>>,
    /// A new picture not yet drawn, and its file: committed by [`Reload::settle`] once it renders.
    trial: Vec<Option<(Image, PathBuf)>>,
    /// A new picture that waits for the running crossfade to end, and its timer.
    held: Vec<Option<(Image, PathBuf, RegistrationToken)>>,
    /// When the crossfade to the committed picture ends.
    fade_until: Vec<Option<Instant>>,
    /// Bumped when a widget's picture changed; the surface re-renders when it differs from the
    /// one its cache was made at.
    generations: Vec<u64>,
    /// The outputs' sizes, from the app on every layout ([`Reload::set_screens`]).
    screens: Screens,
    tokens: Vec<RegistrationToken>,
}

/// The widgets that reload, and a warning for each reload key that has no effect.
fn plan(config: &Config) -> (Vec<Item>, Vec<String>) {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for (index, w) in config.widgets.iter().enumerate() {
        let (spec, path, what) = match w {
            // The decoder warns about ignored screenshot keys, also during `validate`.
            Widget::Background(b) if b.screenshot => continue,
            Widget::Background(b) => (&b.reload, b.path.clone(), "background"),
            Widget::Image(i) => (&i.reload, Some(i.path.clone()), "image"),
            _ => continue,
        };
        let (cover, crossfade_ms) = match w {
            Widget::Background(b) if matches!(b.fit, Fit::Tile | Fit::Center) => {
                (Cover::Native, b.crossfade_ms)
            }
            Widget::Background(b) => (Cover::Screen, b.crossfade_ms),
            Widget::Image(i) => (Cover::Square(i.size), 0),
            _ => continue,
        };
        let crossfade_ms = if config.animations.off {
            0
        } else {
            crossfade_ms
        };
        if !spec.is_set() {
            continue;
        }
        if spec.cmd.is_none() && path.is_none() {
            warnings.push(format!(
                "{what}: reload-ms needs a `path` or a `reload-cmd`"
            ));
            continue;
        }
        items.push(Item {
            index,
            spec: spec.clone(),
            path,
            cover,
            crossfade: Duration::from_millis(crossfade_ms.into()),
        });
    }
    (items, warnings)
}

impl Reload {
    /// Set up for `config`: check every reloading widget once as soon as an output has a size (a
    /// `reload-cmd` gives the first picture of a widget without a path) and arm the `reload-ms`
    /// timers.
    pub fn new(
        config: &Config,
        images: &Images,
        screens: Screens,
        handle: &LoopHandle<'static, App>,
    ) -> anyhow::Result<Self> {
        let (items, warnings) = plan(config);
        for w in warnings {
            tracing::warn!("{w}");
        }
        let n = config.widgets.len();
        let (cmd_tx, cmd_rx) = calloop::channel::channel::<CmdOutput>();
        let (decode_tx, decode_rx) = calloop::channel::channel::<Decode>();
        let mut tokens = Vec::new();
        tokens.push(
            handle
                .insert_source(cmd_rx, |event, _, app| {
                    if let Event::Msg(out) = event {
                        if app.exit.is_none() {
                            app.reload.on_output(out);
                        }
                    }
                })
                .map_err(|e| anyhow::anyhow!("reload command channel: {}", e.error))?,
        );
        tokens.push(
            handle
                .insert_source(decode_rx, |event, _, app| {
                    if let Event::Msg(done) = event {
                        if app.exit.is_none() && app.reload.on_decoded(done) {
                            app.draw_all();
                        }
                    }
                })
                .map_err(|e| anyhow::anyhow!("reload decode channel: {}", e.error))?,
        );

        let mut seen: Vec<Seen> = vec![None; n];
        for item in &items {
            // A picture loaded at startup counts as seen, so the first check does not decode it
            // again.
            if let Some(p) = item.path.as_ref().filter(|p| images.contains(p)) {
                seen[item.index] = Some((p.clone(), mtime(p)));
            }
        }
        let mut reload = Self {
            runner: Runner::new(cmd_tx),
            decode_tx,
            handle: handle.clone(),
            items,
            pending: vec![false; n],
            seen,
            resize_retry: vec![false; n],
            committed_targets: vec![None; n],
            trial_targets: vec![None; n],
            held_targets: vec![None; n],
            committed: (0..n).map(|_| None).collect(),
            trial: (0..n).map(|_| None).collect(),
            held: (0..n).map(|_| None).collect(),
            fade_until: vec![None; n],
            generations: vec![0; n],
            screens,
            tokens,
        };
        for k in 0..reload.items.len() {
            // Until an output has a size, the first look waits for it (`set_screens`), so even the
            // first picture is scaled down off the main thread.
            reload.tick(k);
            let Some(ms) = reload.items[k].spec.every_ms.filter(|&ms| ms > 0) else {
                continue;
            };
            let every = Duration::from_millis(ms.into());
            let token = handle
                .insert_source(Timer::from_duration(every), move |_, _, app| {
                    if app.exit.is_some() {
                        return TimeoutAction::Drop;
                    }
                    app.reload.tick(k);
                    TimeoutAction::ToDuration(every)
                })
                .map_err(|e| anyhow::anyhow!("reload timer: {}", e.error))?;
            reload.tokens.push(token);
        }
        Ok(reload)
    }

    pub fn screens(&self) -> Screens {
        self.screens
    }

    /// The outputs changed. The first sizes start the first look; larger ones request another
    /// decode once pending/trial/held work clears, so a smaller picture is only temporary.
    pub fn set_screens(&mut self, screens: Screens) {
        let old = self.screens;
        if screens == old {
            return;
        }
        self.screens = screens;
        if old.target(Cover::Screen).is_none() {
            self.refresh();
        }
        for k in 0..self.items.len() {
            let id = self.items[k].index;
            if self.picture_needs_growth(k) {
                self.seen[id] = None;
                self.resize_retry[id] = true;
                self.retry_resize(k);
            }
        }
    }

    /// Drop a picture decoded at startup once every widget configured with it shows a reloaded
    /// one instead.
    pub fn prune(&self, config: &Config, images: &mut Images) {
        let mut keep = std::collections::HashSet::new();
        let mut all = Vec::new();
        for (index, w) in config.widgets.iter().enumerate() {
            let path = match w {
                Widget::Background(b) if !b.screenshot => b.path.as_deref(),
                Widget::Image(i) => Some(i.path.as_path()),
                _ => None,
            };
            let Some(path) = path else { continue };
            all.push(path);
            if self.committed[index].is_none() {
                keep.insert(path);
            }
        }
        for path in all {
            if !keep.contains(path) && images.contains(path) {
                images.remove(path);
            }
        }
    }

    /// SIGUSR2: look again at every reloading widget now.
    pub fn refresh(&mut self) {
        for k in 0..self.items.len() {
            self.tick(k);
        }
    }

    /// Remove this instance's timers and channels; its children die when it drops.
    pub fn stop(&mut self, handle: &LoopHandle<'static, App>) {
        let held = self
            .held
            .iter_mut()
            .filter_map(|h| h.take())
            .map(|(.., t)| t);
        for token in self.tokens.drain(..).chain(held) {
            handle.remove(token);
        }
    }

    /// How many times widget `index`'s picture has changed.
    pub fn generation(&self, index: usize) -> u64 {
        self.generations.get(index).copied().unwrap_or(0)
    }

    /// The newest picture of widget `index`, drawn or on trial; `None` means its configured one.
    pub fn newest(&self, index: usize) -> Option<&Image> {
        self.trial
            .get(index)
            .and_then(Option::as_ref)
            .map(|(image, _)| image)
            .or_else(|| self.shown(index))
    }

    /// Whether widget `index` has a new picture that no frame has drawn yet.
    pub fn on_trial(&self, index: usize) -> bool {
        self.trial.get(index).is_some_and(Option::is_some)
    }

    /// The picture widget `index` showed before any trial; `None` means its configured one.
    pub fn shown(&self, index: usize) -> Option<&Image> {
        self.committed.get(index).and_then(Option::as_ref)
    }

    /// Look again at item `k`: run its command, or check its own path.
    fn tick(&mut self, k: usize) {
        let item = &self.items[k];
        // Before the first output has a size there is nothing to scale to; `set_screens` looks.
        if self.pending[item.index] || self.screens.target(Cover::Screen).is_none() {
            return;
        }
        self.resize_retry[item.index] = false;
        match &item.spec.cmd {
            Some(cmd) => self.runner.run(item.index, cmd.clone()),
            None => {
                if let Some(path) = item.path.clone() {
                    self.consider(k, path);
                }
            }
        }
    }

    /// A command finished: its output is the path to show.
    fn on_output(&mut self, out: CmdOutput) {
        let Some(k) = self.items.iter().position(|i| i.index == out.id) else {
            return;
        };
        if self.pending[out.id] {
            return;
        }
        let home = std::env::home_dir();
        let base = self.items[k].spec.base.as_deref();
        match parse_reload_output(&out.text, base, home.as_deref()) {
            Some(path) => self.consider(k, path),
            None => tracing::debug!("reload: the command printed no path; keeping the picture"),
        }
    }

    /// Decode `path` for item `k` on a thread of its own, unless it is what was seen last.
    fn consider(&mut self, k: usize, path: PathBuf) {
        let index = self.items[k].index;
        let target = self.screens.target(self.items[k].cover);
        let seen = self.seen[index].clone();
        let tx = self.decode_tx.clone();
        self.pending[index] = true;
        let spawned = std::thread::Builder::new()
            .name("reload".into())
            .spawn(move || {
                let mtime = mtime(&path);
                let outcome = if !needs_load(seen.as_ref(), &path, mtime) {
                    Outcome::Unchanged
                } else {
                    match background::decode_for_reload(&path, target) {
                        Ok(d) => Outcome::Loaded(d),
                        Err(err) => Outcome::Failed(format!("{err:#}")),
                    }
                };
                let _ = tx.send(Decode {
                    id: index,
                    path,
                    mtime,
                    outcome,
                    target,
                });
            });
        if let Err(err) = spawned {
            // Out of threads: skip this round, keep the picture.
            self.pending[index] = false;
            tracing::warn!("reload: no worker thread: {err}");
        }
    }
}

impl Reload {
    /// A decode finished: a new picture goes on trial (drawn by the next frame), or waits for a
    /// running crossfade. Returns whether a frame is due. Never touches the lock.
    pub fn on_decoded(&mut self, done: Decode) -> bool {
        let Decode {
            id,
            path,
            mtime,
            outcome,
            target,
        } = done;
        self.pending[id] = false;
        let k = self.items.iter().position(|i| i.index == id).unwrap();
        let stale = target_needs_growth(target, self.screens.target(self.items[k].cover))
            && (matches!(&outcome, Outcome::Loaded(_)) || self.picture_needs_growth(k));
        // An adequate completion satisfies growth requested while an older picture was shown.
        if !stale && matches!(&outcome, Outcome::Loaded(_) | Outcome::Failed(_)) {
            self.resize_retry[id] = false;
        }
        if stale || self.resize_retry[id] {
            self.seen[id] = None;
            self.resize_retry[id] = true;
        } else {
            self.seen[id] = Some((path.clone(), mtime));
        }
        let decoded = match outcome {
            Outcome::Unchanged => {
                self.retry_resize(k);
                return false;
            }
            Outcome::Failed(err) => {
                tracing::warn!("reload: {err}; keeping the picture");
                self.retry_resize(k);
                return false;
            }
            Outcome::Loaded(decoded) => decoded,
        };
        let image = match Image::from_decoded(decoded) {
            Ok(image) => image,
            Err(err) => {
                tracing::warn!("reload: {path:?}: {err:#}; keeping the picture");
                self.retry_resize(k);
                return false;
            }
        };
        tracing::debug!("reload: {path:?} for widget {id}");
        // As in hyprlock, a crossfade runs to the end; the next picture waits for it.
        if let Some(until) = self.fade_until[id].filter(|&t| t > Instant::now()) {
            let timer = self
                .handle
                .insert_source(Timer::from_deadline(until), move |_, _, app| {
                    if app.exit.is_none() && app.reload.release(id) {
                        app.draw_all();
                    }
                    TimeoutAction::Drop
                });
            match timer {
                Ok(token) => {
                    // Still pending: no other reload starts for this widget meanwhile.
                    self.pending[id] = true;
                    self.held[id] = Some((image, path, token));
                    self.held_targets[id] = target;
                    return false;
                }
                Err(err) => tracing::warn!("reload: crossfade timer: {}", err.error),
            }
        }
        self.try_picture(id, image, path, target);
        true
    }

    /// A crossfade ended: the picture that waited for it goes on trial.
    fn release(&mut self, id: usize) -> bool {
        self.pending[id] = false;
        match self.held[id].take() {
            Some((image, path, _)) => {
                self.try_picture(id, image, path, self.held_targets[id]);
                true
            }
            None => false,
        }
    }

    fn try_picture(&mut self, id: usize, image: Image, path: PathBuf, target: Option<(u32, u32)>) {
        self.trial[id] = Some((image, path));
        self.trial_targets[id] = target;
        self.generations[id] += 1;
    }

    /// After a frame: each `(widget, drawn)` says whether an output rendered that widget's new
    /// picture. One that rendered everywhere it was drawn replaces the old picture; one that
    /// failed anywhere is dropped (its file stays "seen", so it is not tried again until it
    /// changes) and the old picture stays. Returns whether a frame is due: an output that drew the
    /// failed picture must go back to the old one.
    pub fn settle(&mut self, outcomes: &[(usize, bool)]) -> bool {
        let mut redraw = false;
        for (id, slot) in self.trial.iter_mut().enumerate() {
            let mine = || outcomes.iter().filter(|o| o.0 == id).map(|o| o.1);
            // Not drawn yet (no output configured): still on trial.
            if mine().next().is_none() {
                continue;
            }
            let Some((image, path)) = slot.take() else {
                continue;
            };
            if mine().any(|ok| !ok) {
                tracing::warn!("reload: {path:?} cannot be drawn; keeping the picture");
                // A failed final frame can already have cached the candidate texture.
                self.generations[id] += 1;
                redraw = true;
                continue;
            }
            self.committed[id] = Some(image);
            self.committed_targets[id] = self.trial_targets[id];
            let crossfade = self
                .items
                .iter()
                .find(|i| i.index == id)
                .map_or(Duration::ZERO, |i| i.crossfade);
            self.fade_until[id] = (!crossfade.is_zero()).then(|| Instant::now() + crossfade);
        }
        for k in 0..self.items.len() {
            self.retry_resize(k);
        }
        redraw
    }

    fn picture_needs_growth(&self, k: usize) -> bool {
        let id = self.items[k].index;
        let target = self.screens.target(self.items[k].cover);
        [
            (self.committed[id].is_some(), self.committed_targets[id]),
            (self.trial[id].is_some(), self.trial_targets[id]),
            (self.held[id].is_some(), self.held_targets[id]),
        ]
        .into_iter()
        .any(|(exists, from)| exists && target_needs_growth(from, target))
    }

    fn retry_resize(&mut self, k: usize) {
        let id = self.items[k].index;
        // Automatic growth waits for a frame outcome. Normal looks must still replace an
        // undrawn trial when no configured output selects this widget.
        if self.resize_retry[id] && self.trial[id].is_none() {
            self.tick(k);
        }
    }
}

/// None means full resolution, not a zero-sized decode. Shrinking and regrowing within the
/// captured dimensions must not replace an already adequate picture.
fn target_needs_growth(from: Option<(u32, u32)>, to: Option<(u32, u32)>) -> bool {
    matches!((from, to), (Some((w, h)), Some((nw, nh))) if nw > w || nh > h)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Whether `path` with `mtime` is worth decoding: not what was looked at last.
fn needs_load(
    seen: Option<&(PathBuf, Option<SystemTime>)>,
    path: &Path,
    mtime: Option<SystemTime>,
) -> bool {
    !matches!(seen, Some((p, m)) if p == path && *m == mtime)
}

/// A reload command's output as a path (hyprlock's `runAndGetPath`, plus `~` and relative paths):
/// the first line without NULs and surrounding whitespace, minus a `file://` prefix. `~/` is the
/// home directory; a relative path is against `base`, the directory of the config file. `None`
/// when nothing is left.
fn parse_reload_output(text: &str, base: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let text = text.replace('\0', "");
    let line = text.trim().lines().next()?.trim();
    let line = line.strip_prefix("file://").unwrap_or(line);
    if line.is_empty() {
        return None;
    }
    let path = PathBuf::from(line);
    if let (Ok(rest), Some(home)) = (path.strip_prefix("~"), home) {
        return Some(home.join(rest));
    }
    match base {
        Some(base) if path.is_relative() => Some(base.join(path)),
        _ => Some(path),
    }
}

#[cfg(test)]
mod discrimination_tests {
    use super::*;
    fn fixture(text: &str) -> (Config, calloop::EventLoop<'static, App>, Reload) {
        let config = Config::parse_mem(text).unwrap();
        let event_loop = calloop::EventLoop::try_new().unwrap();
        let r = Reload::new(
            &config,
            &Images::default(),
            Screens::default(),
            &event_loop.handle(),
        )
        .unwrap();
        (config, event_loop, r)
    }
    fn loaded() -> Decode {
        Decode {
            target: None,
            id: 0,
            path: "/new.png".into(),
            mtime: None,
            outcome: Outcome::Loaded((1, 1, vec![0; 4])),
        }
    }
    #[test]
    fn failed_decode_preserves_committed_after_trial_settles() {
        let (_, event_loop, mut r) = fixture("background { reload-cmd \"echo\"; reload-ms 0; }");
        assert!(r.on_decoded(loaded()));
        r.settle(&[(0, true)]);
        let old = r.shown(0).unwrap() as *const Image;
        let generation = r.generation(0);
        assert!(!r.on_decoded(Decode {
            target: None,
            id: 0,
            path: "/bad.png".into(),
            mtime: None,
            outcome: Outcome::Failed("bad".into())
        }));
        assert_eq!(r.shown(0).map(|i| i as *const Image), Some(old));
        assert_eq!(r.generation(0), generation);
        assert!(!r.on_trial(0));
        r.stop(&event_loop.handle());
    }
    fn receive(rx: &calloop::channel::Channel<Decode>) -> Decode {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(done) = rx.try_recv() {
                return done;
            }
            assert!(Instant::now() < deadline, "reload did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    // Seed an in-flight adequate decode: this checks delivery state, not worker scheduling.
    #[test]
    fn adequate_completion_clears_retry_raised_while_pending() {
        for failed in [false, true] {
            let (_, event_loop, mut r) =
                fixture("background { reload-cmd \"echo\"; reload-ms 0; }");
            r.screens = Screens {
                buffer: (128, 96),
                scale: 1.,
            };
            let mut old = loaded();
            old.target = Some((32, 24));
            old.outcome = Outcome::Loaded((32, 24, vec![0; 32 * 24 * 4]));
            assert!(r.on_decoded(old));
            r.settle(&[(0, true)]);
            let old_image = r.shown(0).unwrap() as *const Image;
            let generation = r.generation(0);
            r.pending[0] = true;
            r.set_screens(Screens {
                buffer: (32, 24),
                scale: 1.,
            });
            r.set_screens(Screens {
                buffer: (128, 96),
                scale: 1.,
            });
            assert!(r.pending[0] && r.resize_retry[0]);
            let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
            let path = PathBuf::from("/adequate.png");
            assert_eq!(
                r.on_decoded(Decode {
                    id: 0,
                    target: Some((128, 96)),
                    path: path.clone(),
                    mtime: Some(stamp),
                    outcome: if failed {
                        Outcome::Failed("bad".into())
                    } else {
                        Outcome::Loaded((128, 96, vec![0; 128 * 96 * 4]))
                    },
                }),
                !failed
            );
            // A later tick can clear the retry and mask omission of the delivery-time reset.
            assert!(
                !r.resize_retry[0],
                "adequate completion must clear retry: failed={failed}"
            );
            assert_eq!(r.seen[0], Some((path, Some(stamp))));
            assert_eq!(r.on_trial(0), !failed);
            if failed {
                assert_eq!(r.shown(0).map(|i| i as *const Image), Some(old_image));
                assert_eq!(r.generation(0), generation);
            }
            r.settle(&[(0, true)]);
            assert!(!r.pending[0] && !r.resize_retry[0]);
            assert_eq!(
                r.shown(0).unwrap().size(),
                if failed { (32, 24) } else { (128, 96) }
            );
            r.stop(&event_loop.handle());
        }
    }

    // Release while small: growing before release would independently leave retry raised.
    #[test]
    fn held_small_target_survives_release_before_regrowth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let (_, event_loop, mut r) = fixture(&format!(
            "background {{ path \"{}\"; reload-ms 0; crossfade-ms 3000; }}",
            path.display()
        ));
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.screens = Screens {
            buffer: (128, 96),
            scale: 1.,
        };
        let mut old = loaded();
        old.target = Some((128, 96));
        old.outcome = Outcome::Loaded((128, 96, vec![0; 128 * 96 * 4]));
        assert!(r.on_decoded(old));
        r.settle(&[(0, true)]);
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        let mut small = loaded();
        small.target = Some((32, 24));
        small.path = path;
        small.outcome = Outcome::Loaded((32, 24, vec![0; 32 * 24 * 4]));
        assert!(!r.on_decoded(small));
        assert!(r.held[0].is_some() && r.pending[0]);
        assert!(!r.resize_retry[0]);
        let (_, _, token) = r.held[0].as_ref().unwrap();
        event_loop.handle().remove(*token);
        assert!(r.release(0));
        assert_eq!(r.trial_targets[0], Some((32, 24)));
        r.settle(&[(0, true)]);
        assert_eq!(r.committed_targets[0], Some((32, 24)));
        assert!(!r.pending[0] && !r.resize_retry[0]);
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert!(
            r.pending[0],
            "regrowth must start work for the released small picture"
        );
        let done = receive(&rx);
        assert_eq!(done.target, Some((128, 96)));
        assert!(matches!(done.outcome, Outcome::Loaded(_)));
        r.stop(&event_loop.handle());
    }

    #[test]
    fn stale_command_picture_invalidates_previously_seen_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        image::RgbaImage::new(400, 300).save(&a).unwrap();
        image::RgbaImage::new(400, 300).save(&b).unwrap();
        let (_, event_loop, mut r) = fixture("background { reload-cmd \"echo\"; reload-ms 0; }");
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        // Completed command delivery is the seam; file decoding uses the real worker.
        r.screens = Screens {
            buffer: (128, 96),
            scale: 1.,
        };
        r.on_output(CmdOutput {
            id: 0,
            text: a.display().to_string(),
        });
        let first = receive(&rx);
        let stamp = first.mtime;
        assert!(matches!(&first.outcome, Outcome::Loaded(_)));
        assert!(r.on_decoded(first));
        r.settle(&[(0, true)]);
        assert_eq!(r.seen[0], Some((a.clone(), stamp)));
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        r.on_output(CmdOutput {
            id: 0,
            text: b.display().to_string(),
        });
        let small = receive(&rx); // Hold delivery, not a worker timing race.
        assert_eq!(small.target, Some((32, 24)));
        assert!(matches!(&small.outcome, Outcome::Loaded(_)));
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert_eq!(r.seen[0], Some((a.clone(), stamp)));
        assert!(!r.resize_retry[0]); // The still-committed A is adequate.
        assert!(r.on_decoded(small));
        assert!(r.seen[0].is_none() && r.resize_retry[0]);
        r.settle(&[(0, true)]);
        // Inject the retry command's result; subprocess execution is not credited here.
        r.on_output(CmdOutput {
            id: 0,
            text: a.display().to_string(),
        });
        let retry = receive(&rx);
        assert_eq!(retry.path, a);
        assert_eq!(retry.mtime, stamp);
        assert_eq!(retry.target, Some((128, 96)));
        assert!(
            matches!(&retry.outcome, Outcome::Loaded(_)),
            "previously seen A must be decoded again"
        );
        assert!(r.on_decoded(retry));
        r.settle(&[(0, true)]);
        assert_eq!(r.committed_targets[0], Some((128, 96)));
        assert!(!r.pending[0] && !r.resize_retry[0]);
        r.stop(&event_loop.handle());
    }

    #[test]
    fn single_axis_growth_starts_automatic_decode() {
        for target in [(128, 24), (32, 96)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("picture.png");
            image::RgbaImage::new(400, 300).save(&path).unwrap();
            let (_, event_loop, mut r) = fixture(&format!(
                "background {{ path \"{}\"; reload-ms 0; }}",
                path.display()
            ));
            let (tx, rx) = calloop::channel::channel();
            r.decode_tx = tx;
            r.screens = Screens {
                buffer: (32, 24),
                scale: 1.,
            };
            let mut small = loaded();
            small.target = Some((32, 24));
            small.outcome = Outcome::Loaded((32, 24, vec![0; 32 * 24 * 4]));
            assert!(r.on_decoded(small));
            r.settle(&[(0, true)]);
            assert!(!r.pending[0] && !r.resize_retry[0]);
            r.set_screens(Screens {
                buffer: target,
                scale: 1.,
            });
            assert!(r.pending[0], "one-axis growth must start work: {target:?}");
            assert!(target_needs_growth(Some((32, 24)), Some(target)));
            let done = receive(&rx);
            assert_eq!(done.target, Some(target));
            assert!(matches!(&done.outcome, Outcome::Loaded(_)));
            assert!(r.on_decoded(done));
            r.settle(&[(0, true)]);
            assert_eq!(r.committed_targets[0], Some(target));
            assert!(!r.pending[0] && !r.resize_retry[0]);
            r.stop(&event_loop.handle());
        }
    }

    #[test]
    fn growth_during_unchanged_startup_look_keeps_full_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("startup.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let config = Config::parse_mem(&format!(
            "background {{ path \"{}\"; reload-ms 0; }}",
            path.display()
        ))
        .unwrap();
        let event_loop = calloop::EventLoop::try_new().unwrap();
        let mut images = Images::load_all(&config);
        let mut r =
            Reload::new(&config, &images, Screens::default(), &event_loop.handle()).unwrap();
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        let done = receive(&rx);
        assert!(matches!(&done.outcome, Outcome::Unchanged));
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert!(!r.on_decoded(done));
        assert!(!r.on_trial(0));
        r.prune(&config, &mut images);
        assert!(images.contains(&path));
        assert!(!r.pending[0] && !r.resize_retry[0]);
        r.refresh();
        assert!(matches!(receive(&rx).outcome, Outcome::Unchanged));
        r.stop(&event_loop.handle());
    }

    #[test]
    fn growth_waits_for_held_picture_then_retries_and_regrowth_skips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let (_, event_loop, mut r) = fixture(&format!(
            "background {{ path \"{}\"; reload-ms 0; crossfade-ms 3000; }}",
            path.display()
        ));
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        assert!(r.on_decoded(receive(&rx)));
        r.settle(&[(0, true)]);
        // Another actual decode waits for that crossfade. Growth must survive its release.
        r.seen[0] = None;
        r.refresh();
        assert!(!r.on_decoded(receive(&rx)));
        assert!(r.held[0].is_some());
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert!(r.pending[0] && r.resize_retry[0]);
        no_decode(&rx);
        let (_, _, token) = r.held[0].as_ref().unwrap();
        event_loop.handle().remove(*token);
        assert!(r.release(0));
        r.settle(&[(0, true)]);
        assert!(!r.on_decoded(receive(&rx))); // Larger picture waits for the new fade.
        let (_, _, token) = r.held[0].as_ref().unwrap();
        event_loop.handle().remove(*token);
        r.release(0);
        r.settle(&[(0, true)]);
        assert_eq!(r.shown(0).unwrap().size(), (128, 96));
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert!(!r.pending[0]);
        r.refresh();
        assert!(matches!(receive(&rx).outcome, Outcome::Unchanged));
        r.stop(&event_loop.handle());
    }

    // Hold delivery, not a large-image timing race: the real worker has captured the small
    // target before the larger configure. Unconditional seen assignment loses this resize.
    #[test]
    fn larger_output_during_decode_retries_without_a_file_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stripes.png");
        image::RgbaImage::from_fn(400, 300, |x, _| {
            let c = if x % 8 < 4 { 0 } else { 255 };
            image::Rgba([c, c, c, 255])
        })
        .save(&path)
        .unwrap();
        let text = format!("background {{ path \"{}\"; reload-ms 0; }}", path.display());
        let (_, event_loop, mut r) = fixture(&text);
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        let small = receive(&rx);
        assert!(matches!(&small.outcome, Outcome::Loaded((32, 24, _))));
        let stamp = small.mtime;
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert!(r.on_decoded(small));
        r.settle(&[(0, true), (0, true)]);
        let large = receive(&rx);
        assert_eq!(large.mtime, stamp);
        assert!(matches!(&large.outcome, Outcome::Loaded((128, 96, _))));
        assert!(r.on_decoded(large));
        r.settle(&[(0, true), (0, true)]);
        r.refresh(); // SIGUSR2 on the same path must now skip, not decode again.
        assert!(matches!(receive(&rx).outcome, Outcome::Unchanged));
        r.stop(&event_loop.handle());
    }

    #[test]
    fn every_failed_frame_discards_candidate_caches() {
        let (_, event_loop, mut r) = fixture("background { reload-cmd \"echo\"; reload-ms 0; }");
        assert!(r.on_decoded(loaded()));
        r.settle(&[(0, true), (0, true)]);
        let old = r.shown(0).unwrap() as *const Image;
        assert!(r.on_decoded(loaded()));
        let generation = r.generation(0);
        assert!(
            r.settle(&[(0, false), (0, false)]),
            "failed frames can already cache the candidate"
        );
        assert!(r.generation(0) > generation);
        assert_eq!(r.shown(0).map(|i| i as *const Image), Some(old));
        r.stop(&event_loop.handle());
    }

    // No matching output means settle has no outcomes for this widget. Normal looks must
    // still replace that trial; only automatic resize work waits for it to be drawn.
    #[test]
    fn refresh_replaces_undrawn_trial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let (_, event_loop, mut r) = fixture(&format!(
            "background output=\"DP-9\" {{ path \"{}\"; reload-ms 0; }}",
            path.display()
        ));
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        assert!(r.on_decoded(receive(&rx)));
        assert!(!r.settle(&[]));
        assert!(r.on_trial(0) && !r.pending[0]);
        image::RgbaImage::new(200, 100).save(&path).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(100))
            .unwrap();
        let generation = r.generation(0);
        r.refresh();
        let done = receive(&rx);
        assert!(matches!(&done.outcome, Outcome::Loaded((48, 24, _))));
        assert!(r.on_decoded(done));
        assert!(r.generation(0) > generation);
        r.settle(&[(0, true)]);
        assert_eq!(r.shown(0).unwrap().size(), (48, 24));
        r.stop(&event_loop.handle());
    }

    #[test]
    fn command_output_replaces_undrawn_trial() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        image::RgbaImage::new(400, 300).save(&a).unwrap();
        image::RgbaImage::new(200, 100).save(&b).unwrap();
        let (_, event_loop, mut r) =
            fixture("background output=\"DP-9\" { reload-cmd \"echo\"; reload-ms 0; }");
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        // Command delivery is the seam; the real file worker captures the screen target.
        r.screens = Screens {
            buffer: (32, 24),
            scale: 1.,
        };
        r.on_output(CmdOutput {
            id: 0,
            text: a.display().to_string(),
        });
        assert!(r.on_decoded(receive(&rx)));
        assert!(!r.settle(&[]));
        assert!(r.on_trial(0) && !r.pending[0]);
        r.on_output(CmdOutput {
            id: 0,
            text: b.display().to_string(),
        });
        let done = receive(&rx);
        assert_eq!(done.path, b);
        assert!(matches!(&done.outcome, Outcome::Loaded((48, 24, _))));
        assert!(r.on_decoded(done));
        r.settle(&[(0, true)]);
        assert_eq!(r.shown(0).unwrap().size(), (48, 24));
        r.stop(&event_loop.handle());
    }

    #[test]
    fn unchanged_completion_after_growth_retries_same_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let (_, event_loop, mut r) = fixture(&format!(
            "background {{ path \"{}\"; reload-ms 0; }}",
            path.display()
        ));
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        assert!(r.on_decoded(receive(&rx)));
        r.settle(&[(0, true)]);
        r.refresh();
        let unchanged = receive(&rx);
        assert!(matches!(&unchanged.outcome, Outcome::Unchanged));
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        no_decode(&rx);
        assert!(!r.on_decoded(unchanged));
        let large = receive(&rx);
        assert!(matches!(&large.outcome, Outcome::Loaded((128, 96, _))));
        assert!(r.on_decoded(large));
        r.settle(&[(0, true)]);
        assert_eq!(r.shown(0).unwrap().size(), (128, 96));
        no_decode(&rx);
        r.refresh();
        assert!(matches!(receive(&rx).outcome, Outcome::Unchanged));
        r.stop(&event_loop.handle());
    }

    // Both actual worker errors must terminate automatic growth while keeping the old picture.
    fn failed_growth_stops(missing: bool, stale: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let (_, event_loop, mut r) = fixture(&format!(
            "background {{ path \"{}\"; reload-ms 0; }}",
            path.display()
        ));
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        assert!(r.on_decoded(receive(&rx)));
        r.settle(&[(0, true)]);
        if missing {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::write(&path, b"not an image").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(100))
                .unwrap();
        }
        if stale {
            r.refresh();
        } else {
            r.set_screens(Screens {
                buffer: (128, 96),
                scale: 1.,
            });
        }
        let failed = receive(&rx);
        assert!(matches!(&failed.outcome, Outcome::Failed(_)));
        assert_eq!(
            failed.target,
            Some(if stale { (32, 24) } else { (128, 96) })
        );
        if stale {
            r.set_screens(Screens {
                buffer: (128, 96),
                scale: 1.,
            });
            no_decode(&rx);
        }
        assert!(!r.on_decoded(failed));
        if stale {
            let retry = receive(&rx);
            assert_eq!(retry.target, Some((128, 96)));
            assert!(matches!(&retry.outcome, Outcome::Failed(_)));
            assert!(!r.on_decoded(retry));
        }
        assert_eq!(r.shown(0).unwrap().size(), (32, 24));
        assert!(!r.pending[0] && !r.resize_retry[0]);
        assert!(!r.settle(&[]));
        no_decode(&rx);
        r.refresh();
        assert!(matches!(receive(&rx).outcome, Outcome::Unchanged));
        r.stop(&event_loop.handle());
    }

    #[test]
    fn adequate_corrupt_growth_failure_stops() {
        failed_growth_stops(false, false);
    }
    #[test]
    fn adequate_missing_growth_failure_stops() {
        failed_growth_stops(true, false);
    }
    #[test]
    fn stale_failure_retries_current_target_once() {
        failed_growth_stops(false, true);
    }

    #[test]
    fn stale_failed_trial_waits_then_retries_without_final_frame_loop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        image::RgbaImage::new(400, 300).save(&path).unwrap();
        let (_, event_loop, mut r) = fixture(&format!(
            "background {{ path \"{}\"; reload-ms 0; }}",
            path.display()
        ));
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.set_screens(Screens {
            buffer: (32, 24),
            scale: 1.,
        });
        let small = receive(&rx);
        r.set_screens(Screens {
            buffer: (128, 96),
            scale: 1.,
        });
        assert!(r.on_decoded(small));
        assert!(r.on_trial(0) && r.resize_retry[0]);
        // Calling settlement without outcomes must not start an automatic look either.
        assert!(!r.settle(&[]));
        no_decode(&rx);
        let generation = r.generation(0);
        assert!(r.settle(&[(0, false), (0, true)]));
        assert!(r.shown(0).is_none() && r.generation(0) > generation);
        let large = receive(&rx);
        assert!(matches!(&large.outcome, Outcome::Loaded((128, 96, _))));
        assert!(r.on_decoded(large));
        assert!(!r.resize_retry[0]);
        assert!(r.settle(&[(0, false), (0, false)]));
        assert!(!r.on_trial(0) && r.shown(0).is_none());
        assert!(!r.settle(&[(0, false), (0, false)]));
        assert!(!r.settle(&[]));
        no_decode(&rx);
        r.refresh();
        assert!(matches!(receive(&rx).outcome, Outcome::Unchanged));
        r.stop(&event_loop.handle());
    }

    fn no_decode(rx: &calloop::channel::Channel<Decode>) {
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            assert!(
                rx.try_recv().is_err(),
                "started another decode while held/pending"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn tick_does_not_decode_during_held_crossfade() {
        let (_, event_loop, mut r) =
            fixture("background { path \"/a.png\"; reload-ms 0; crossfade-ms 3000; }");
        assert!(r.on_decoded(loaded()));
        r.settle(&[(0, true)]);
        assert!(!r.on_decoded(loaded()));
        assert!(r.held[0].is_some() && r.pending[0]);
        r.screens = Screens {
            buffer: (320, 240),
            scale: 1.,
        };
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.tick(0);
        no_decode(&rx);
        r.stop(&event_loop.handle());
    }
    #[test]
    fn queued_command_output_does_not_decode_during_held_crossfade() {
        let (_, event_loop, mut r) =
            fixture("background { reload-cmd \"echo\"; reload-ms 0; crossfade-ms 3000; }");
        assert!(r.on_decoded(loaded()));
        r.settle(&[(0, true)]);
        assert!(!r.on_decoded(loaded()));
        assert!(r.held[0].is_some() && r.pending[0]);
        let (tx, rx) = calloop::channel::channel();
        r.decode_tx = tx;
        r.on_output(CmdOutput {
            id: 0,
            text: "/queued.png".into(),
        });
        no_decode(&rx);
        r.stop(&event_loop.handle());
    }
    #[test]
    fn prune_keeps_shared_startup_then_releases_last_reference() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("startup.png");
        image::RgbaImage::new(1, 1).save(&path).unwrap();
        let text = format!(
            "background {{ path \"{}\"; reload-ms 0; }}\nimage {{ path \"{}\"; reload-ms 0; }}",
            path.display(),
            path.display()
        );
        let (config, event_loop, mut r) = fixture(&text);
        let mut images = Images::load_all(&config);
        assert!(images.contains(&path));
        assert!(r.on_decoded(loaded()));
        r.settle(&[(0, true)]);
        r.prune(&config, &mut images);
        assert!(
            images.contains(&path),
            "unreloaded widget still needs startup picture"
        );
        let mut decoded = loaded();
        decoded.id = 1;
        assert!(r.on_decoded(decoded));
        r.settle(&[(1, true)]);
        r.prune(&config, &mut images);
        assert!(
            !images.contains(&path),
            "all widgets have replaced startup picture"
        );
        r.stop(&event_loop.handle());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Option<PathBuf> {
        parse_reload_output(text, Some(Path::new("/cfg")), Some(Path::new("/home/u")))
    }

    #[test]
    fn output_becomes_a_path() {
        assert_eq!(parse("/p.png\n"), Some("/p.png".into()));
        assert_eq!(parse("file:///p.png"), Some("/p.png".into()));
        assert_eq!(parse("file:///p.png\n\0"), Some("/p.png".into()));
        assert_eq!(parse("\0"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n\n"), None);
        assert_eq!(parse("~/x.png"), Some("/home/u/x.png".into()));
        assert_eq!(parse("x.png"), Some("/cfg/x.png".into()));
        assert_eq!(parse("a/../x.png\nignored"), Some("/cfg/a/../x.png".into()));
        assert_eq!(
            parse_reload_output("x.png", None, None),
            Some("x.png".into())
        );
    }

    #[test]
    fn unchanged_path_and_mtime_are_skipped() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
        let seen = (PathBuf::from("/a.png"), Some(t));
        assert!(!needs_load(Some(&seen), Path::new("/a.png"), Some(t)));
        assert!(needs_load(None, Path::new("/a.png"), Some(t)));
        assert!(needs_load(Some(&seen), Path::new("/b.png"), Some(t)));
        assert!(needs_load(
            Some(&seen),
            Path::new("/a.png"),
            Some(t + Duration::from_secs(1))
        ));
        // A file that cannot be read is tried once, not on every tick.
        let gone = (PathBuf::from("/a.png"), None);
        assert!(!needs_load(Some(&gone), Path::new("/a.png"), None));
        assert!(needs_load(Some(&gone), Path::new("/a.png"), Some(t)));
    }

    #[test]
    fn plan_skips_screenshots_and_warns() {
        let config = Config::parse_mem(
            "background { screenshot; reload-ms 500; }\n\
             background { reload-ms 500; }\n\
             background { path \"/a.png\"; reload-ms 500; }\n\
             image { path \"/b.png\"; reload-cmd \"echo\"; }\n\
             image { path \"/c.png\"; }\n",
        )
        .unwrap();
        let (items, warnings) = plan(&config);
        assert_eq!(items.iter().map(|i| i.index).collect::<Vec<_>>(), [2, 3]);
        // Screenshot warnings are emitted at decode time, including by `validate`, not twice.
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("needs a `path`"), "{warnings:?}");
    }

    #[test]
    fn screens_give_the_size_to_cover() {
        assert_eq!(Screens::default().target(Cover::Screen), None);
        let s = Screens {
            buffer: (640, 480),
            scale: 2.,
        };
        assert_eq!(s.target(Cover::Screen), Some((640, 480)));
        assert_eq!(s.target(Cover::Square(60.5)), Some((121, 121)));
        assert_eq!(s.target(Cover::Native), None);
    }

    fn loaded(id: usize) -> Decode {
        Decode {
            target: None,
            id,
            path: "/new.png".into(),
            mtime: None,
            outcome: Outcome::Loaded((1, 1, vec![0; 4])),
        }
    }

    /// A new picture is on trial until a frame draws it: kept if every reported outcome succeeded,
    /// dropped (the old one stays) if any could not. One that arrives during the crossfade to
    /// the last waits for it to end.
    #[test]
    fn a_new_picture_is_kept_only_once_it_renders() {
        let config = Config::parse_mem(
            "background { path \"/a.png\"; reload-cmd \"echo\"; reload-ms 0; crossfade-ms 3000; }",
        )
        .unwrap();
        let event_loop = calloop::EventLoop::<App>::try_new().unwrap();
        let mut r = Reload::new(
            &config,
            &Images::default(),
            Screens::default(),
            &event_loop.handle(),
        )
        .unwrap();
        assert!(r.newest(0).is_none() && !r.on_trial(0));

        // Fails to render: dropped, redraw discards any provisional candidate cache.
        assert!(r.on_decoded(loaded(0)));
        assert!(r.on_trial(0) && r.newest(0).is_some() && r.shown(0).is_none());
        let g = r.generation(0);
        assert!(r.settle(&[(0, false)]));
        assert!(!r.on_trial(0) && r.newest(0).is_none());

        // Rendered on one output, not on another: dropped, and the first redraws the old one.
        assert!(r.on_decoded(loaded(0)));
        assert!(r.settle(&[(0, true), (0, false)]));
        assert!(r.newest(0).is_none() && r.generation(0) > g + 1);

        // Not drawn by any output yet: still on trial.
        assert!(r.on_decoded(loaded(0)));
        assert!(!r.settle(&[]));
        assert!(r.on_trial(0));

        // Rendered: kept, and its crossfade starts.
        assert!(!r.settle(&[(0, true)]));
        assert!(r.shown(0).is_some() && !r.on_trial(0));

        // During that crossfade the next picture waits, and no reload starts meanwhile.
        let g = r.generation(0);
        assert!(!r.on_decoded(loaded(0)));
        assert!(!r.on_trial(0) && r.pending[0] && r.generation(0) == g);
        assert!(r.release(0));
        assert!(r.on_trial(0) && !r.pending[0]);

        // A failed decode keeps everything as it was.
        let failed = Decode {
            target: None,
            id: 0,
            path: "/bad.png".into(),
            mtime: None,
            outcome: Outcome::Failed("bad".into()),
        };
        assert!(!r.on_decoded(failed));
        assert!(r.on_trial(0));
        r.stop(&event_loop.handle());
    }
}
