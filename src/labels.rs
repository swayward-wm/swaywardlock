//! Runtime for `label` widgets: `cmd[]` schedules, their latest output, and the clock check for
//! `$TIME`. Owns the timers so a config reload (preview) can replace them wholesale.

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};

use crate::commands::{CmdOutput, Runner};
use crate::config::{Config, Widget};
use crate::state::LockState;
use crate::textvars::{self, Template};
use crate::App;

/// How often `$TIME` labels look at the wall clock.
const CLOCK_CHECK: std::time::Duration = std::time::Duration::from_secs(1);

pub struct Labels {
    runner: Runner,
    /// Latest output per widget index; `None` for non-command widgets or before the first run.
    pub output: Vec<Option<String>>,
    /// Commands that re-run on SIGUSR2, as (widget index, shell).
    on_signal: Vec<(usize, String)>,
    timers: Vec<RegistrationToken>,
    /// An idle draw for changed output is queued; see the channel callback.
    redraw_pending: bool,
    pub user: String,
    pub desc: String,
}

impl Labels {
    /// Set up for `config`: start every command once, arm its interval, and arm the clock check
    /// if any label shows the time.
    pub fn new(
        config: &Config,
        state: &LockState,
        handle: &LoopHandle<'static, App>,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = calloop::channel::channel::<CmdOutput>();
        let token = handle
            .insert_source(rx, |event, _, app| {
                if let calloop::channel::Event::Msg(out) = event {
                    let Some(slot) = app.labels.output.get_mut(out.id) else {
                        return;
                    };
                    // A result that says what is already shown needs no frame.
                    if slot.as_deref() == Some(out.text.as_str()) {
                        return;
                    }
                    *slot = Some(out.text);
                    // One draw per loop iteration, however many results arrived in it: calloop
                    // hands over up to 1024 messages per dispatch, and a full draw of every
                    // surface per message starved input and signals behind the redraws.
                    if !std::mem::replace(&mut app.labels.redraw_pending, true) {
                        app.handle.insert_idle(|app| {
                            app.labels.redraw_pending = false;
                            app.draw_all();
                        });
                    }
                }
            })
            .map_err(|e| anyhow::anyhow!("label channel: {}", e.error))?;

        let (user, desc) = textvars::user_names();
        let mut labels = Self {
            runner: Runner::new(tx),
            output: vec![None; config.widgets.len()],
            on_signal: Vec::new(),
            timers: vec![token],
            redraw_pending: false,
            user,
            desc,
        };

        let mut uses_time = false;
        for (i, w) in config.widgets.iter().enumerate() {
            let Widget::Label(label) = w else { continue };
            let template = Template::parse(&label.text);
            uses_time |= template.uses_time();
            let Template::Cmd(cmd) = template else {
                continue;
            };
            labels.run(i, &cmd.shell, state);
            if cmd.on_signal {
                labels.on_signal.push((i, cmd.shell.clone()));
            }
            if let Some(every) = cmd.every {
                let shell = cmd.shell.clone();
                let token = handle
                    .insert_source(Timer::from_duration(every), move |_, _, app| {
                        app.labels.run(i, &shell, &app.state);
                        TimeoutAction::ToDuration(every)
                    })
                    .map_err(|e| anyhow::anyhow!("label timer: {}", e.error))?;
                labels.timers.push(token);
            }
        }

        if uses_time {
            // Calloop timers count CLOCK_MONOTONIC, which stops during suspend, so a timer armed
            // for the next minute fired up to a minute late after a resume and the clock showed
            // the time from before the sleep. Look at the wall clock every second instead (as
            // hyprlock does) and redraw when the minute shown changes.
            let mut shown = textvars::local_time();
            let token = handle
                .insert_source(Timer::from_duration(CLOCK_CHECK), move |_, _, app| {
                    let now = textvars::local_time();
                    if now != shown {
                        shown = now;
                        app.draw_all();
                    }
                    TimeoutAction::ToDuration(CLOCK_CHECK)
                })
                .map_err(|e| anyhow::anyhow!("clock timer: {}", e.error))?;
            labels.timers.push(token);
        }
        Ok(labels)
    }

    /// Run label `i`'s command. `$TIME` and `$ATTEMPTS` are substituted into the shell text
    /// first, like hyprlock; the foreign values (`$USER`, `$FAIL`, `$PAMPROMPT`, ...) are
    /// environment variables of the child, so they are data to the shell. The output is shown as
    /// is.
    fn run(&self, i: usize, shell: &str, state: &LockState) {
        let vars = self.vars(state);
        let shell = textvars::substitute_shell(shell, &vars);
        self.runner.run(i, shell, textvars::shell_env(&vars));
    }

    /// SIGUSR2: re-run the commands marked `cmd[update:N:1]`.
    pub fn refresh(&self, state: &LockState) {
        for (i, shell) in &self.on_signal {
            self.run(*i, shell, state);
        }
    }

    /// Remove this instance's timers; its children die when it drops.
    pub fn stop(&mut self, handle: &LoopHandle<'static, App>) {
        for token in self.timers.drain(..) {
            handle.remove(token);
        }
    }

    pub fn vars<'a>(&'a self, state: &'a LockState) -> textvars::Vars<'a> {
        textvars::Vars {
            user: &self.user,
            desc: &self.desc,
            time: textvars::local_time(),
            attempts: state.failed_attempts,
            layout_index: state.layout,
            layout_name: state.layout_name().unwrap_or(""),
            fail: state.last_failure.as_deref().unwrap_or(""),
            pam_prompt: &state.pam_prompt,
            pam_fail: state.pam_failure.as_deref().unwrap_or(""),
            fprint_prompt: &state.fprint_prompt,
            fprint_fail: &state.fprint_fail,
        }
    }
}
