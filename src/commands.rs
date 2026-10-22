//! `cmd[...]` labels: run shell commands off the main thread and deliver their output.
//!
//! Each command runs as the user with the user's environment, via `sh -c`, like hyprlock. A command
//! still running when its next tick comes is not started again. Every child is killed when the
//! runner is dropped (on unlock or exit).

use std::collections::HashMap;
use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use calloop::channel::Sender;

/// Output of one command run, keyed by the label's id.
#[derive(Debug)]
pub struct CmdOutput {
    pub id: usize,
    pub text: String,
}

/// Starts commands on worker threads and tracks running children so they can be killed.
pub struct Runner {
    tx: Sender<CmdOutput>,
    running: Arc<Mutex<HashMap<usize, Child>>>,
}

impl Runner {
    pub fn new(tx: Sender<CmdOutput>) -> Self {
        Self {
            tx,
            running: Arc::default(),
        }
    }

    /// Run `shell` for label `id` unless the previous run is still going.
    pub fn run(&self, id: usize, shell: String) {
        let mut running = self.running.lock().unwrap();
        if running.contains_key(&id) {
            return;
        }
        use std::os::unix::process::CommandExt as _;
        let child = Command::new("sh")
            .arg("-c")
            .arg(&shell)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so killing it reaches the whole pipeline, not just `sh`.
            .process_group(0)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!("cmd[]: cannot run {shell:?}: {err}");
                return;
            }
        };
        let mut stdout = child.stdout.take().expect("piped stdout");
        running.insert(id, child);
        drop(running);

        let tx = self.tx.clone();
        let running = self.running.clone();
        thread::spawn(move || {
            let mut out = String::new();
            let _ = stdout.read_to_string(&mut out);
            // The child may outlive its stdout. Keep it tracked (so the next tick skips it and
            // drop kills it) until it exits, and never hold the lock while waiting: the main
            // thread takes it in `run`.
            loop {
                let mut running = running.lock().unwrap();
                let Some(child) = running.get_mut(&id) else {
                    break; // killed and reaped by drop
                };
                if !matches!(child.try_wait(), Ok(None)) {
                    running.remove(&id);
                    break;
                }
                drop(running);
                thread::sleep(std::time::Duration::from_millis(100));
            }
            // hyprlock trims trailing whitespace (the newline from `date` and friends).
            let text = out.trim_end().to_owned();
            let _ = tx.send(CmdOutput { id, text });
        });
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        for (_, mut child) in self.running.lock().unwrap().drain() {
            // SAFETY: plain kill(2) on the group we created in `run`.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn runs_and_trims_output() {
        let (tx, rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        runner.run(7, "printf 'hello\\n\\n'".into());
        let mut event_loop: calloop::EventLoop<Option<CmdOutput>> =
            calloop::EventLoop::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(rx, |e, _, got| {
                if let calloop::channel::Event::Msg(m) = e {
                    *got = Some(m);
                }
            })
            .unwrap();
        let mut got = None;
        for _ in 0..50 {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut got)
                .unwrap();
            if got.is_some() {
                break;
            }
        }
        let got = got.expect("output");
        assert_eq!((got.id, got.text.as_str()), (7, "hello"));
    }

    #[test]
    fn drop_kills_running_children() {
        let (tx, _rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        runner.run(1, "sleep 30".into());
        let pid = runner.running.lock().unwrap()[&1].id();
        drop(runner);
        // The process is gone (reaped by the drop).
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn drop_kills_the_whole_pipeline() {
        let (tx, _rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        // `sh` stays alive as the parent of `sleep`; killing only `sh` would orphan `sleep`.
        runner.run(1, "sleep 31 | cat".into());
        std::thread::sleep(Duration::from_millis(200));
        let pgid = runner.running.lock().unwrap()[&1].id();
        drop(runner);
        std::thread::sleep(Duration::from_millis(100));
        let alive = std::fs::read_dir("/proc").unwrap().filter_map(|e| {
            let stat = std::fs::read_to_string(e.ok()?.path().join("stat")).ok()?;
            // Field 5 of /proc/PID/stat is the process group.
            let after = stat.rsplit_once(") ")?.1;
            let pgrp: u32 = after.split(' ').nth(2)?.parse().ok()?;
            let state = after.split(' ').next()?;
            (pgrp == pgid && state != "Z").then_some(())
        });
        assert_eq!(alive.count(), 0, "processes left in group {pgid}");
    }

    /// A child that closes stdout but keeps running must not block the main thread's next
    /// `run` (it once held the lock across `wait`), and must stay tracked so drop kills it.
    #[test]
    fn a_child_that_closes_stdout_neither_blocks_nor_escapes() {
        let (tx, _rx) = calloop::channel::channel();
        let runner = Arc::new(Runner::new(tx));
        runner.run(1, "exec >/dev/null; sleep 32".into());
        let pid = runner.running.lock().unwrap()[&1].id();
        std::thread::sleep(Duration::from_millis(300)); // the worker has seen EOF by now
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let r = runner.clone();
        std::thread::spawn(move || {
            r.run(1, "true".into());
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "run blocked behind a running child"
        );
        assert!(
            runner.running.lock().unwrap().contains_key(&1),
            "child no longer tracked"
        );
        drop(Arc::into_inner(runner).expect("sole owner"));
        std::thread::sleep(Duration::from_millis(100));
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(
            state.is_empty()
                || state
                    .rsplit_once(") ")
                    .is_some_and(|(_, a)| a.starts_with('Z')),
            "child {pid} survived the drop"
        );
    }

    #[test]
    fn a_running_command_is_not_started_twice() {
        let (tx, _rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        runner.run(1, "sleep 30".into());
        let pid = runner.running.lock().unwrap()[&1].id();
        runner.run(1, "sleep 30".into());
        assert_eq!(runner.running.lock().unwrap()[&1].id(), pid);
    }
}
