//! Process contract: `--daemonize` and `--ready-fd`.
//!
//! swaylock forks once the session is locked. We cannot: by then Mesa and the auth thread have
//! started threads, and `fork` keeps only the calling thread. So `--daemonize` forks first, before
//! any thread exists, and the parent waits on a pipe until the child reports the lock (or an exit
//! code). Seen from outside the contract is the same: the parent exits 0 once the screen is locked.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{FromRawFd as _, OwnedFd};

/// Write end of the daemonize pipe, held by the child until it reports.
pub struct Daemon(Option<File>);

impl Daemon {
    pub fn none() -> Self {
        Self(None)
    }

    /// Fork into the background. Returns in the child only; the parent exits with the status the
    /// child reports (1 if the child dies without reporting).
    ///
    /// Must be called before any thread is spawned.
    pub fn fork() -> anyhow::Result<Self> {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: both descriptors were just created and are owned here.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

        // SAFETY: single-threaded at this point (documented precondition).
        match unsafe { libc::fork() } {
            -1 => Err(std::io::Error::last_os_error().into()),
            0 => {
                drop(read);
                // SAFETY: plain syscalls in the child; failures leave us attached, which is
                // harmless. stdio stays connected until `report` so startup errors still reach
                // the terminal.
                unsafe {
                    libc::setsid();
                    let _ = libc::chdir(c"/".as_ptr());
                }
                Ok(Self(Some(File::from(write))))
            }
            _ => {
                drop(write);
                let mut status = [1u8];
                let mut read = File::from(read);
                let code = match read.read(&mut status) {
                    Ok(1) => status[0],
                    _ => 1,
                };
                std::process::exit(code.into());
            }
        }
    }

    /// Tell the waiting parent to exit with `code`, then detach from the terminal. Only the first
    /// call has an effect.
    pub fn report(&mut self, code: u8) {
        if let Some(mut pipe) = self.0.take() {
            let _ = pipe.write_all(&[code]);
            // SAFETY: plain syscalls on descriptors we own.
            unsafe {
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if null >= 0 {
                    libc::dup2(null, libc::STDIN_FILENO);
                    libc::dup2(null, libc::STDOUT_FILENO);
                    libc::dup2(null, libc::STDERR_FILENO);
                    libc::close(null);
                }
            }
        }
    }
}

/// Check that `--ready-fd` names an open descriptor and mark it close-on-exec (inherited
/// descriptors arrive without it, and `cmd[]` children must not get it). Called first thing, so
/// a number that was not open cannot later name one of our own descriptors: writing to that and
/// closing it after `locked` broke the event loop with the session locked.
pub fn check_ready_fd(fd: i32) -> anyhow::Result<()> {
    // SAFETY: fcntl on a caller-supplied number; an invalid one just fails.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        anyhow::bail!(
            "--ready-fd {fd}: {}; pass a descriptor that is open",
            std::io::Error::last_os_error()
        );
    }
    // SAFETY: as above, on a descriptor that is open.
    unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    Ok(())
}

/// Write `\n` to the ready fd and close it, as swaylock does. Raw calls rather than an owned
/// `File`: the number came from the command line, and Rust's I/O-safety check aborts the process
/// if it ever names a descriptor something else owns.
pub fn notify_ready(fd: Option<i32>) -> std::io::Result<()> {
    let Some(fd) = fd else {
        return Ok(());
    };
    // SAFETY: plain write/close on the descriptor the user handed us for this; `check_ready_fd`
    // made sure it was open at startup, and nothing else opened it since.
    let written = unsafe { libc::write(fd, c"\n".as_ptr().cast(), 1) };
    let result = if written == 1 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    };
    // SAFETY: as above.
    unsafe { libc::close(fd) };
    result
}

/// At least this many descriptors must be available, or the locker refuses to lock. A locked
/// session measured about 30 (Wayland, EGL/Mesa, eventfds, timers, PAM, `cmd[]` pipes); with
/// 16 or 17 it locked and then crashed or failed, leaving a session nobody could unlock.
const MIN_FDS: libc::rlim_t = 64;
/// The soft limit is raised to at most this (the usual default), so `cmd[]` children never get
/// a soft limit above what they would normally see.
const RAISED_FDS: libc::rlim_t = 1024;

/// Raise a low soft `RLIMIT_NOFILE` towards the hard limit, and refuse (before locking) if even
/// the hard limit is too small to run a lock.
pub fn ensure_fd_limit() -> anyhow::Result<()> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit/setrlimit on a local struct.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return Ok(()); // cannot tell; go on as before
        }
        let want = RAISED_FDS.min(lim.rlim_max);
        if lim.rlim_cur < want {
            lim.rlim_cur = want;
            libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim);
        }
    }
    check_fd_limit(lim.rlim_cur)
}

fn check_fd_limit(soft: libc::rlim_t) -> anyhow::Result<()> {
    if soft < MIN_FDS {
        anyhow::bail!(
            "the open file limit is {soft} (ulimit -n); swaywardlock needs at least {MIN_FDS}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_ready_fd_is_refused() {
        let mut fds = [0; 2];
        // SAFETY: pipe into a local array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        check_ready_fd(fds[1]).unwrap();
        // SAFETY: we own both ends.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        assert!(check_ready_fd(fds[1]).is_err());
        assert!(check_ready_fd(-1).is_err());
    }

    #[test]
    fn notify_ready_writes_a_newline_and_closes() {
        let mut fds = [0; 2];
        // SAFETY: pipe into a local array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        notify_ready(Some(fds[1])).unwrap();
        let mut buf = [0u8; 4];
        // SAFETY: read from the end we own; EOF follows because the write end is closed.
        let n = unsafe { libc::read(fds[0], buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n as usize], b"\n");
        // SAFETY: as above.
        assert_eq!(unsafe { libc::read(fds[0], buf.as_mut_ptr().cast(), 1) }, 0);
        // SAFETY: we own the read end.
        unsafe { libc::close(fds[0]) };
    }

    #[test]
    fn tiny_fd_limits_are_refused() {
        assert!(check_fd_limit(16).is_err());
        assert!(check_fd_limit(63).is_err());
        assert!(check_fd_limit(64).is_ok());
    }
}
