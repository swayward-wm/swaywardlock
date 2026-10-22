//! Runtime for `label` widgets: `cmd[]` schedules, their latest output, and the minute tick for
//! `$TIME`. Owns the timers so a config reload (preview) can replace them wholesale.

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};

use crate::commands::{CmdOutput, Runner};
use crate::config::{Config, Widget};
use crate::state::LockState;
use crate::textvars::{self, Template};
use crate::App;

pub struct Labels {
    runner: Runner,
    /// Latest output per widget index; `None` for non-command widgets or before the first run.
    pub output: Vec<Option<String>>,
    /// Commands that re-run on SIGUSR2, as (widget index, shell).
    on_signal: Vec<(usize, String)>,
    timers: Vec<RegistrationToken>,
    pub user: String,
    pub desc: String,
}

impl Labels {
    /// Set up for `config`: start every command once, arm its interval, and arm the minute tick
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
                    if let Some(slot) = app.labels.output.get_mut(out.id) {
                        *slot = Some(out.text);
                        app.draw_all();
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
            let token = handle
                .insert_source(
                    Timer::from_duration(textvars::until_next_minute()),
                    |_, _, app| {
                        app.draw_all();
                        TimeoutAction::ToDuration(textvars::until_next_minute())
                    },
                )
                .map_err(|e| anyhow::anyhow!("clock timer: {}", e.error))?;
            labels.timers.push(token);
        }
        Ok(labels)
    }

    /// Run label `i`'s command. Variables are substituted into the shell text first, like
    /// hyprlock ("parsed before the command is ran"); the output is shown as is.
    fn run(&self, i: usize, shell: &str, state: &LockState) {
        let shell = textvars::substitute(shell, &self.vars(state));
        self.runner.run(i, shell);
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
