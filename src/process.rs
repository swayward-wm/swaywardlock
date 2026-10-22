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

/// Write `\n` to the ready fd and close it, as swaylock does.
pub fn notify_ready(fd: Option<i32>) -> anyhow::Result<()> {
    if let Some(fd) = fd {
        // SAFETY: the user passed this descriptor to us for exactly this purpose; we take
        // ownership and close it after writing.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(b"\n")?;
    }
    Ok(())
}
