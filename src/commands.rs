//! `cmd[...]` labels: run shell commands off the main thread and deliver their output.
//!
//! Each command runs as the user with the user's environment, via `sh -c`, like hyprlock. A command
//! still running when its next tick comes is not started again. Every child is killed when the
//! runner is dropped (on unlock or exit) and when SIGTERM, SIGINT or SIGHUP ends the process (see
//! [`kill_children_on_termination`]). That includes jobs a command leaves in the background
//! (`daemon & echo started`): the label's process group counts as running, and is killed with the
//! rest, until it is empty; the output is shown as soon as `sh` exits.
//!
//! Output is capped at [`MAX_OUTPUT`] bytes and [`crate::render::text::MAX_LINES`] lines: Pango
//! lays the text out on the main thread, and 50k lines took 18 s, with keyboard, auth and SIGUSR1
//! waiting behind it. A command that writes past the cap is killed.

use std::collections::HashMap;
use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use calloop::channel::Sender;

/// Most bytes of a command's output that are kept; the command is killed when it writes more.
pub const MAX_OUTPUT: usize = 64 * 1024;

/// Output of one command run, keyed by the label's id.
#[derive(Debug)]
pub struct CmdOutput {
    pub id: usize,
    pub text: String,
}

/// Starts commands on worker threads and tracks running children so they can be killed.
pub struct Runner {
    tx: Sender<CmdOutput>,
    /// One entry per label whose process group still has members, even after `sh` itself has
    /// exited (a job it put in the background: `daemon & echo started`).
    running: Arc<Mutex<HashMap<usize, Child>>>,
}

impl Runner {
    pub fn new(tx: Sender<CmdOutput>) -> Self {
        Self {
            tx,
            running: Arc::default(),
        }
    }

    /// Run `shell` for label `id` unless the previous run (any process in its group) is still
    /// going.
    pub fn run(&self, id: usize, shell: String) {
        let mut running = self.running.lock().unwrap();
        if running.contains_key(&id) {
            return;
        }
        use std::os::unix::process::CommandExt as _;
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&shell)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so killing it reaches the whole pipeline, not just `sh`.
            .process_group(0);
        // SAFETY: `reset_child` only makes async-signal-safe calls.
        unsafe { command.pre_exec(reset_child) };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!("cmd[]: cannot run {shell:?}: {err}");
                return;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        GROUPS.track(child.id());
        running.insert(id, child);

        let tx = self.tx.clone();
        let worker_running = self.running.clone();
        let worker_shell = shell.clone();
        let spawned = spawn_worker(move || {
            let (running, shell) = (worker_running, worker_shell);
            let mut out = Vec::new();
            // One byte past the cap tells a full pipe from output that just fits.
            let _ = stdout.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut out);
            if out.len() > MAX_OUTPUT {
                tracing::warn!("cmd[]: output of {shell:?} is over {MAX_OUTPUT} bytes; killed");
                if let Some(child) = running.lock().unwrap().get(&id) {
                    // SAFETY: plain kill(2) on the group we created in `run`; still tracked, so
                    // not reaped.
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                }
            }
            // The child may outlive its stdout, and a background job may outlive the child. Keep
            // the group tracked (so the next tick skips it and drop kills it) until it is empty,
            // and never hold the lock while waiting: the main thread takes it in `run`.
            let mut text = Some(clamp_output(&out));
            loop {
                let mut running = running.lock().unwrap();
                let Some(child) = running.get_mut(&id) else {
                    break; // killed and reaped by drop
                };
                if !matches!(child.try_wait(), Ok(None)) {
                    // `sh` is done: its output goes out now, whatever it left running.
                    if let Some(text) = text.take() {
                        let _ = tx.send(CmdOutput { id, text });
                    }
                    // Checked under the lock that drop takes to kill the group, at most 100 ms
                    // after the group emptied: the number is not reused that fast.
                    if !group_alive(child.id()) {
                        GROUPS.untrack(child.id());
                        running.remove(&id);
                        break;
                    }
                }
                drop(running);
                thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        if let Err(err) = spawned {
            // Out of threads (RLIMIT_NPROC, pids.max): skip this run and keep the lock.
            tracing::warn!("cmd[]: cannot run {shell:?}: no worker thread: {err}");
            if let Some(mut child) = running.remove(&id) {
                // SAFETY: plain kill(2) on the group created above; `sh` is not reaped yet.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                GROUPS.untrack(child.id());
            }
        }
    }
}

/// Whether process group `pgid` has a member left.
fn group_alive(pgid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 only checks that the group exists.
    unsafe { libc::kill(-(pgid as i32), 0) == 0 }
}

/// `thread::spawn` panics when no thread can be created, and the release profile aborts on a
/// panic: that killed the locker with the session locked. Tests can make it fail.
fn spawn_worker(f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_SPAWN.get() {
        return Err(std::io::Error::from_raw_os_error(libc::EAGAIN));
    }
    thread::Builder::new().name("cmd".into()).spawn(f).map(drop)
}

impl Drop for Runner {
    fn drop(&mut self) {
        for (_, mut child) in self.running.lock().unwrap().drain() {
            // SAFETY: plain kill(2) on the group we created in `run`. Either `sh` is not reaped
            // yet, or the worker saw the group alive at most 100 ms ago (see `run`).
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            GROUPS.untrack(child.id());
        }
    }
}

/// Raw output to label text: at most [`MAX_OUTPUT`] bytes, cut at a character boundary, at most
/// [`crate::render::text::MAX_LINES`] lines, trailing whitespace trimmed (hyprlock trims the
/// newline from `date` and friends).
fn clamp_output(out: &[u8]) -> String {
    let out = &out[..out.len().min(MAX_OUTPUT)];
    let text = match std::str::from_utf8(out) {
        Ok(text) => std::borrow::Cow::Borrowed(text),
        // Cut inside the last character by the cap: drop the partial character.
        Err(e) if e.error_len().is_none() => {
            std::borrow::Cow::Borrowed(std::str::from_utf8(&out[..e.valid_up_to()]).unwrap())
        }
        Err(_) => String::from_utf8_lossy(out),
    };
    crate::render::text::clamp_lines(&text)
        .trim_end()
        .to_owned()
}

/// In the forked child, before exec: undo what the locker set up for itself. SIGUSR1/2 are
/// blocked in the locker (for its signalfd) and the mask survives exec; descriptors that Mesa
/// opened without close-on-exec would leak into every command.
fn reset_child() -> std::io::Result<()> {
    // SAFETY: async-signal-safe calls on local data, in the single-threaded child.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
        // Close-on-exec rather than close: std's own exec-error pipe is above 2 and must stay
        // open until exec.
        if libc::close_range(
            3,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC as libc::c_int,
        ) != 0
        {
            // Kernels before 5.11: mark them one by one.
            for fd in 3..4096 {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
    }
    Ok(())
}

/// Process groups of running children, readable from a signal handler.
struct Groups([AtomicI32; 256]);

static GROUPS: Groups = Groups([const { AtomicI32::new(0) }; 256]);

impl Groups {
    fn track(&self, pgid: u32) {
        let pgid = pgid as i32;
        if !self.0.iter().any(|slot| {
            slot.compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        }) {
            tracing::warn!("cmd[]: too many running commands to clean up on SIGTERM");
        }
    }

    fn untrack(&self, pgid: u32) {
        let pgid = pgid as i32;
        for slot in &self.0 {
            let _ = slot.compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst);
        }
    }
}

/// Kill every running `cmd[]` group when SIGTERM, SIGINT or SIGHUP ends the process. Those keep
/// their default action (the process dies, the session stays locked; the spec's way to stop a
/// stuck locker), so `Runner`'s drop never runs, and the children are in their own process groups,
/// out of reach of a terminal's Ctrl+C. A handler rather than the event loop: it must work while
/// the main thread is stuck.
pub fn kill_children_on_termination() {
    extern "C" fn on_signal(sig: libc::c_int) {
        for slot in &GROUPS.0 {
            let pgid = slot.load(Ordering::SeqCst);
            if pgid > 0 {
                // SAFETY: kill(2) is async-signal-safe.
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
            }
        }
        // SA_RESETHAND restored the default action; the signal is blocked while this runs, so it
        // is delivered again, and kills the process, when the handler returns.
        // SAFETY: raise(3) is async-signal-safe.
        unsafe { libc::raise(sig) };
    }
    // SAFETY: installs an async-signal-safe handler; plain sigaction calls.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESETHAND | libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(sig, std::ptr::null(), &mut old);
            // An ignored signal stays ignored (`nohup`, or SIGINT for a background job).
            if old.sa_sigaction != libc::SIG_IGN {
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::Duration;

    use super::*;

    thread_local! {
        /// Makes `spawn_worker` fail on this thread, as `pthread_create` does at RLIMIT_NPROC.
        pub static FAIL_SPAWN: Cell<bool> = const { Cell::new(false) };
    }

    fn recv_within(rx: &calloop::channel::Channel<CmdOutput>, secs: u64) -> Option<CmdOutput> {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if let Ok(got) = rx.try_recv() {
                return Some(got);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| Some(s.rsplit_once(") ")?.1.chars().next()? != 'Z'))
            .unwrap_or(false)
    }

    /// Run `shell` as label 7 and wait up to `secs` for its output.
    fn output_of(shell: &str, secs: u64) -> Option<CmdOutput> {
        let (tx, rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        runner.run(7, shell.into());
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
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while got.is_none() && std::time::Instant::now() < deadline {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut got)
                .unwrap();
        }
        got
    }

    #[test]
    fn runs_and_trims_output() {
        let got = output_of("printf 'hello\\n\\n'", 3).expect("output");
        assert_eq!((got.id, got.text.as_str()), (7, "hello"));
    }

    /// A log-tail label once kept the main thread in Pango for minutes (50k lines: 18 s).
    #[test]
    fn long_output_is_cut_to_a_hundred_lines() {
        let got = output_of("seq 200000", 10).expect("output");
        assert_eq!(got.text.lines().count(), 100);
        assert!(got.text.starts_with("1\n2\n") && got.text.ends_with("\n100"));
    }

    /// Progress output separates lines with CR, which Pango breaks on too.
    #[test]
    fn carriage_return_output_is_cut_to_a_hundred_lines() {
        let got = output_of("seq 30000 | tr '\\n' '\\r'", 10).expect("output");
        assert_eq!(got.text.split('\r').count(), 100);
        assert!(got.text.starts_with("1\r2\r") && got.text.ends_with("\r100"));
    }

    /// `yes` once grew the worker's buffer by about 1 GB/s until unlock; past the cap the command
    /// is killed and what was read so far is shown.
    #[test]
    fn endless_output_is_capped_and_the_command_killed() {
        let got = output_of("yes", 5).expect("output within 5 s");
        assert!(got.text.len() <= 64 * 1024);
        assert_eq!(got.text.lines().count(), 100);
        // One endless line of 3-byte characters: the cap (65536 = 3 * 21845 + 1) cuts one, which
        // must be dropped, not turned into a replacement character.
        let got = output_of("yes € | tr -d '\\n'", 5).expect("output within 5 s");
        assert_eq!(got.text.len(), 3 * 21845);
        assert!(!got.text.contains('\u{fffd}'));
    }

    /// The locker blocks SIGUSR1/2 for its signalfd; the mask survives exec, so commands (and
    /// whatever they start) never saw those signals. Descriptors without close-on-exec leaked too.
    #[test]
    fn children_get_a_clean_signal_mask_and_no_stray_descriptors() {
        // SAFETY: plain signal-mask and descriptor calls; the mask is this test thread's only.
        let (stray, old) = unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            let mut old: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGUSR1);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
            // dup() never sets close-on-exec, like the descriptors Mesa leaves behind.
            (libc::dup(2), old)
        };
        let got = output_of(
            // grep's mask, a grandchild like any program a label starts (sh blocks SIGCHLD
            // itself).
            &format!("grep SigBlk /proc/self/status; test -e /proc/$$/fd/{stray} && echo leaked"),
            3,
        );
        // SAFETY: restore this thread's mask and close our descriptor.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
            libc::close(stray);
        }
        let text = got.expect("output").text;
        assert!(text.contains("SigBlk:\t0000000000000000"), "{text}");
        assert!(!text.contains("leaked"), "fd {stray} reached the child");
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
        // Wait (with a deadline, not a fixed sleep) until `sh` has closed the pipe: from then on
        // the worker sees EOF and moves on to waiting for the child.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::fs::read_link(format!("/proc/{pid}/fd/1"))
            .is_ok_and(|p| p != std::path::Path::new("/dev/null"))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the child never closed stdout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let r = runner.clone();
        std::thread::spawn(move || {
            r.run(1, "true".into());
            // Before signalling: the test then takes sole ownership of the runner.
            drop(r);
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
        // The helper thread dropped its clone before signalling, so this cannot race it.
        drop(Arc::into_inner(runner).expect("sole owner"));
        // Drop kills the group and reaps `sh` before it returns: no wait needed.
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

    /// No thread for the worker once aborted the locker (panic=abort) with the session locked,
    /// and left the forked `sh` running.
    #[test]
    fn no_worker_thread_skips_the_run_and_kills_the_child() {
        let (tx, _rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        // `sh -c` has exec'd by the time `spawn` returns, so its command line is this.
        let marker = format!("sleep 33.{}", std::process::id());
        FAIL_SPAWN.set(true);
        runner.run(1, marker.clone());
        FAIL_SPAWN.set(false);
        assert!(
            runner.running.lock().unwrap().is_empty(),
            "the run is still tracked"
        );
        let left = std::fs::read_dir("/proc").unwrap().filter_map(|e| {
            let pid: u32 = e.ok()?.file_name().to_str()?.parse().ok()?;
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            (String::from_utf8_lossy(&cmdline).contains(&marker) && alive(pid)).then_some(pid)
        });
        assert_eq!(left.collect::<Vec<_>>(), [], "the child was left running");
        // The next run gets its thread.
        runner.run(1, "sleep 30".into());
        assert!(runner.running.lock().unwrap().contains_key(&1));
    }

    /// A job the command puts in the background stays in its process group after `sh` exits;
    /// the group was untracked then and the job outlived unlock and SIGTERM.
    #[test]
    fn a_background_job_is_killed_on_drop_and_blocks_the_next_run() {
        let (tx, rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        runner.run(1, "exec sleep 34 >/dev/null & echo $!".into());
        // The output arrives once `sh` exits, without waiting for the job.
        let got = recv_within(&rx, 3).expect("output");
        let job: u32 = got.text.parse().expect("the job's pid");
        std::thread::sleep(Duration::from_millis(300)); // the worker has seen `sh` exit
        assert!(alive(job));
        let pgid = runner.running.lock().unwrap().get(&1).map(Child::id);
        assert!(pgid.is_some(), "the group was untracked while the job runs");
        runner.run(1, "true".into());
        assert_eq!(runner.running.lock().unwrap().get(&1).map(Child::id), pgid);
        drop(runner);
        std::thread::sleep(Duration::from_millis(100));
        assert!(!alive(job), "background job {job} survived the drop");
    }

    #[test]
    fn a_finished_group_is_forgotten() {
        let (tx, rx) = calloop::channel::channel();
        let runner = Runner::new(tx);
        runner.run(1, "sleep 0.2 >/dev/null & echo x".into());
        recv_within(&rx, 3).expect("output");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while runner.running.lock().unwrap().contains_key(&1) {
            assert!(
                std::time::Instant::now() < deadline,
                "the empty group is still tracked"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
