//! Authentication on a worker thread. Results come back over a calloop channel.
//!
//! A PAM transaction starts only when the user submits, as in swaylock: nothing runs while the
//! lock screen is idle. Every check a stack makes before it asks (pam_faillock's preauth, a
//! pam_nologin in the auth phase) is therefore decided at the moment the user pressed Enter, and
//! a pam_fprintd in the stack reads the reader only after Enter (Enter, then touch, as with
//! swaylock; `auth { fingerprint }` talks to fprintd directly and unlocks on a touch at any time).
//! A stack that would succeed without asking anything (pam_permit, an empty password under
//! nullok) still needs that submit: nothing unlocks, and no attempt counts, without input.
//!
//! The submitted secret answers the transaction's first question. Every later question, hidden
//! or visible and even with the same text, goes to the UI as a follow-up [`AuthEvent::Prompt`]
//! and is answered with the next submit, so a password-then-one-time-code stack works and a
//! secret never goes to a question it was not typed for. `auth { pam { reuse-password; } }` opts
//! in to answering a repeated identical question with the previous answer, for stacks with two
//! modules that each ask for the password.
//!
//! `$PAMPROMPT` shows "Password: " until a transaction has asked something, then the first
//! question the last transaction asked.
//!
//! Only the auth phase runs (`pam_authenticate`, then `pam_setcred(PAM_REFRESH_CRED)` on
//! success), as in swaylock. The account phase decides whether a new login may start (expiry,
//! `pam_nologin`, `pam_time`); a locker re-authenticates a session that already exists, and an
//! account failure there would refuse the right password for as long as it lasts.
//!
//! PAM is called through the raw `pam-sys2` bindings with our own conversation function, so the
//! password goes from the locked [`SecretBuf`] straight into the `malloc`ed response libpam asks
//! for. No intermediate Rust `String`/`CString` copy is ever made. What libpam and its modules do
//! with the response after that is outside our control, as it is for every PAM client.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use calloop::channel::Sender;

use crate::secret::SecretBuf;

/// `$PAMPROMPT` before any transaction has asked a question (pam_unix's own default).
pub const DEFAULT_PROMPT: &str = "Password: ";

#[derive(Debug)]
pub enum AuthEvent {
    Success,
    Failure(String),
    /// A question from PAM (e.g. "Password: "), for `$PAMPROMPT`. `follow_up` is set when the
    /// transaction has already used a submitted secret and asks for another (a one-time code),
    /// so the UI must stop checking and let the answer be typed.
    Prompt {
        text: String,
        follow_up: bool,
    },
}

/// Runs one PAM transaction per submit on a worker thread.
pub struct Authenticator {
    tx: mpsc::Sender<SecretBuf>,
}

impl Authenticator {
    /// `reuse_password` answers a repeated identical question with the previous answer.
    pub fn spawn(
        service: String,
        reuse_password: bool,
        events: Sender<AuthEvent>,
    ) -> anyhow::Result<Self> {
        Self::spawn_with(service, reuse_password, None, events)
    }

    /// `confdir` replaces /etc/pam.d, for tests only.
    fn spawn_with(
        service: String,
        reuse_password: bool,
        confdir: Option<PathBuf>,
        events: Sender<AuthEvent>,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<SecretBuf>();
        thread::Builder::new().name("auth".into()).spawn(move || {
            // Idle until the user submits: no transaction runs (and no module decides anything)
            // before that. The channel closes when the locker exits.
            while let Ok(secret) = rx.recv() {
                let mut conv = Conversation {
                    rx: &rx,
                    events: &events,
                    pending: Some(secret),
                    reuse_password,
                    last: None,
                    first_question: None,
                    closed: false,
                    fail_text: None,
                };
                let result = transaction(&service, confdir.as_deref(), &mut conv);
                if conv.closed {
                    break; // the locker is exiting
                }
                let event = match result {
                    Ok(()) => AuthEvent::Success,
                    Err(msg) => {
                        // Back to the first question for the next attempt (not the one-time
                        // code the failed transaction may have asked last).
                        if let Some(text) = conv.first_question.take() {
                            let _ = events.send(AuthEvent::Prompt {
                                text,
                                follow_up: false,
                            });
                        }
                        AuthEvent::Failure(conv.fail_text.take().unwrap_or(msg))
                    }
                };
                // After a success there is nothing left to ask: the session unlocks.
                let done = matches!(event, AuthEvent::Success);
                if events.send(event).is_err() || done {
                    break;
                }
            }
        })?;
        Ok(Self { tx })
    }

    pub fn submit(&self, secret: SecretBuf) {
        // The thread only exits when the main loop's receiver is gone, i.e. at shutdown.
        let _ = self.tx.send(secret);
    }
}

/// One transaction's side of the conversation: questions out, submitted secrets in.
struct Conversation<'a> {
    rx: &'a mpsc::Receiver<SecretBuf>,
    events: &'a Sender<AuthEvent>,
    /// The submit that started this transaction, until the first question takes it.
    pending: Option<SecretBuf>,
    /// `auth { pam { reuse-password; } }`.
    reuse_password: bool,
    /// The last question and its answer, kept only with `reuse_password`.
    last: Option<(String, SecretBuf)>,
    /// The first question asked in this transaction, for `$PAMPROMPT` on the next attempt.
    first_question: Option<String>,
    /// The submit channel closed: the locker is exiting.
    closed: bool,
    /// A failure reason PAM sent as a message, shown instead of the generic error.
    fail_text: Option<String>,
}

impl Conversation<'_> {
    /// Answer a question: the first one with the submit that started the transaction, every
    /// later one with the next submit, after showing it to the user. `None` once the locker
    /// exits.
    fn ask(&mut self, text: &str) -> Option<SecretBuf> {
        // Opt-in, and once only: a module that keeps asking the same thing gets the user's
        // answer next, so it cannot spin on a stored answer.
        if let Some((_, secret)) = self.last.take().filter(|(q, _)| q == text) {
            return Some(secret);
        }
        self.first_question.get_or_insert_with(|| text.to_owned());
        // The prompt text only; never anything derived from a secret.
        let _ = self.events.send(AuthEvent::Prompt {
            text: text.to_owned(),
            follow_up: self.pending.is_none(),
        });
        let secret = match self.pending.take() {
            Some(secret) => secret,
            None => match self.rx.recv() {
                Ok(secret) => secret,
                Err(_) => {
                    self.closed = true;
                    return None;
                }
            },
        };
        if self.reuse_password {
            self.last = Some((text.to_owned(), secret.copy()));
        }
        Some(secret)
    }

    #[cfg_attr(feature = "test-auth", allow(dead_code))]
    fn info(&mut self, text: &str) {
        tracing::info!("PAM: {text}");
        // pam_faillock's lockout notice ("(9 minutes left to unlock)"): hyprlock shows it as
        // the failure text, since it says why the right password is refused.
        if text.contains("left to unlock") {
            self.fail_text = Some(text.trim().to_owned());
        }
    }
}

/// Where Linux-PAM looks for service files.
const PAM_DIRS: [&str; 2] = ["/etc/pam.d", "/usr/lib/pam.d"];

/// Refuse to lock when libpam would not read the service's file. Linux-PAM would fall back to
/// `/etc/pam.d/other`, which denies on Fedora and Arch, so every password, the right one too,
/// would be refused and only SIGUSR1 could end the lock.
pub fn check_service_installed(service: &str) -> Result<(), String> {
    #[cfg(not(feature = "test-auth"))]
    let dirs: Vec<PathBuf> = PAM_DIRS.iter().map(PathBuf::from).collect();
    // Test builds never open PAM; the e2e tests opt in to the check with a directory of their own
    // (one directory, as libpam uses only the one given to pam_start_confdir).
    #[cfg(feature = "test-auth")]
    let dirs: Vec<PathBuf> = match std::env::var_os("SWAYWARDLOCK_TEST_PAM_DIR") {
        Some(dir) => vec![dir.into()],
        None => return Ok(()),
    };
    check_service(service, &dirs)
}

/// Look for the file the way libpam does: the service name lowercased (pam_start does that),
/// then the first directory where opening the path succeeds. libpam stops there, so if that is
/// not a regular file the service is unusable even when a later directory has a good file.
fn check_service(service: &str, dirs: &[PathBuf]) -> Result<(), String> {
    if service.is_empty() || service.contains('/') || service == "." || service == ".." {
        return Err(format!(
            "invalid PAM service name {service:?}: it names a file in {}",
            PAM_DIRS[0]
        ));
    }
    let name = service.to_ascii_lowercase();
    let mut unreadable = Vec::new();
    for dir in dirs {
        let path = dir.join(&name);
        match std::fs::File::open(&path) {
            Ok(file) if file.metadata().is_ok_and(|m| m.is_file()) => return Ok(()),
            // libpam fopen()s the path, which succeeds on a directory, and stops at the first
            // path that opens: it never looks in the later directories.
            Ok(_) => {
                return Err(format!(
                    "the PAM service file for {service:?} is not a file ({}); PAM would refuse \
                     every password. Remove it, or install the service file as a regular file \
                     with mode 644 (install -m644 pam/swaywardlock {}/swaywardlock)",
                    path.display(),
                    PAM_DIRS[0]
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => unreadable.push(format!("{} cannot be read: {e}", path.display())),
        }
    }
    // No pam.d at all (an /etc/pam.conf system): nothing to check against.
    if !dirs.iter().any(|d| d.is_dir()) {
        return Ok(());
    }
    if !unreadable.is_empty() {
        return Err(format!(
            "the PAM service file for {service:?} is not readable by this process ({}); PAM \
             would refuse every password. Install it with mode 644 (install -m644 \
             pam/swaywardlock {}/swaywardlock)",
            unreadable.join("; "),
            PAM_DIRS[0]
        ));
    }
    let files: Vec<String> = dirs
        .iter()
        .map(|d| d.join(&name).display().to_string())
        .collect();
    Err(format!(
        "the PAM service file for {service:?} is missing ({}); PAM would refuse every password. \
         Install the one shipped as pam/swaywardlock (install -m644 pam/swaywardlock \
         {}/swaywardlock) or set auth {{ pam service=\"...\"; }}",
        files.join(" and "),
        PAM_DIRS[0]
    ))
}

/// Test builds accept exactly `TEST_PASSWORD`, so end-to-end tests run without a PAM setup.
#[cfg(feature = "test-auth")]
pub const TEST_PASSWORD: &str = "swaywardlock-test";
/// Test builds also take this password followed by the code `TEST_OTP`, like a PAM stack with a
/// one-time code after the password.
#[cfg(feature = "test-auth")]
const TEST_PASSWORD_2FA: &str = "swaywardlock-2fa";
#[cfg(feature = "test-auth")]
const TEST_OTP: &str = "123456";

#[cfg(feature = "test-auth")]
fn transaction(
    _service: &str,
    _confdir: Option<&Path>,
    conv: &mut Conversation,
) -> Result<(), String> {
    let wrong = || Err("wrong password".to_owned());
    let Some(secret) = conv.ask("Password: ") else {
        return wrong();
    };
    // A slow PAM stack, so tests can see the verifying state (the ring's spinner).
    if let Some(ms) = std::env::var("SWAYWARDLOCK_TEST_AUTH_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    match secret.as_str() {
        TEST_PASSWORD => Ok(()),
        TEST_PASSWORD_2FA => match conv.ask("OTP: ") {
            Some(code) if code.as_str() == TEST_OTP => Ok(()),
            _ => wrong(),
        },
        _ => wrong(),
    }
}

#[cfg(not(feature = "test-auth"))]
use pam::transaction;

#[cfg(not(feature = "test-auth"))]
mod pam {
    use std::ffi::{c_int, c_void, CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;
    use std::ptr;

    use pam_sys2 as sys;

    use super::*;

    pub fn transaction(
        service: &str,
        confdir: Option<&Path>,
        conv: &mut Conversation,
    ) -> Result<(), String> {
        let user = current_user().ok_or("cannot determine the current user")?;
        let service = CString::new(service).map_err(|_| "invalid PAM service name")?;
        let user = CString::new(user).map_err(|_| "invalid user name")?;
        let confdir = confdir
            .map(|d| CString::new(d.as_os_str().as_bytes()))
            .transpose()
            .map_err(|_| "invalid PAM directory")?;

        let pam_conv = sys::pam_conv {
            conv: Some(converse),
            appdata_ptr: (conv as *mut Conversation).cast(),
        };
        let mut handle: *mut sys::pam_handle_t = ptr::null_mut();

        // SAFETY: all pointers are valid for the duration of the transaction; `conv` and
        // `pam_conv` outlive `pam_end`.
        unsafe {
            let rc = match &confdir {
                // pam_start_confdir needs Linux-PAM 1.4; only the tests use it.
                #[cfg(test)]
                Some(dir) => sys::pam_start_confdir(
                    service.as_ptr(),
                    user.as_ptr(),
                    &pam_conv,
                    dir.as_ptr(),
                    &mut handle,
                ),
                _ => sys::pam_start(service.as_ptr(), user.as_ptr(), &pam_conv, &mut handle),
            };
            if rc != sys::PAM_SUCCESS {
                return Err(format!("PAM start failed ({rc})"));
            }
            let rc = sys::pam_authenticate(handle, 0);
            if rc == sys::PAM_SUCCESS {
                // Renew credentials (Kerberos tickets and the like), as swaylock does. Like
                // swaylock, its result does not decide the unlock.
                let cred = sys::pam_setcred(handle, sys::PAM_REFRESH_CRED as c_int);
                if cred != sys::PAM_SUCCESS {
                    tracing::warn!("pam_setcred: {}", strerror(handle, cred));
                }
            }
            let msg = (rc != sys::PAM_SUCCESS).then(|| strerror(handle, rc));
            sys::pam_end(handle, rc);
            match msg {
                None => Ok(()),
                Some(msg) => Err(msg),
            }
        }
    }

    /// # Safety
    /// `handle` is a live PAM handle.
    unsafe fn strerror(handle: *mut sys::pam_handle_t, rc: c_int) -> String {
        // SAFETY: per the contract; pam_strerror returns a static string or null.
        let s = unsafe { sys::pam_strerror(handle, rc) };
        if s.is_null() {
            format!("PAM error {rc}")
        } else {
            // SAFETY: a NUL-terminated static string.
            unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
        }
    }

    /// Answer each prompt (hidden or visible) with the next submitted secret; report text
    /// messages. Responses are `calloc`ed as libpam requires; libpam frees them.
    unsafe extern "C" fn converse(
        num_msg: c_int,
        msg: *mut *const sys::pam_message,
        resp: *mut *mut sys::pam_response,
        appdata: *mut c_void,
    ) -> c_int {
        if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata.is_null() {
            return sys::PAM_CONV_ERR;
        }
        // SAFETY: libpam passes back the pointer we gave pam_start; the transaction is
        // single-threaded.
        let conv = unsafe { &mut *appdata.cast::<Conversation<'_>>() };
        let n = num_msg as usize;
        // SAFETY: calloc returns zeroed memory or null.
        let replies = unsafe { libc::calloc(n, std::mem::size_of::<sys::pam_response>()) }
            .cast::<sys::pam_response>();
        if replies.is_null() {
            return sys::PAM_BUF_ERR;
        }

        for i in 0..n {
            // Linux-PAM passes an array of pointers to messages.
            // SAFETY: libpam guarantees `num_msg` valid entries.
            let m = unsafe { &**msg.add(i) };
            let text = if m.msg.is_null() {
                std::borrow::Cow::Borrowed("")
            } else {
                // SAFETY: libpam messages are NUL-terminated.
                unsafe { CStr::from_ptr(m.msg) }.to_string_lossy()
            };
            match m.msg_style {
                // Visible prompts are answered too, as swaylock and hyprlock do ("Token code: ").
                sys::PAM_PROMPT_ECHO_OFF | sys::PAM_PROMPT_ECHO_ON => {
                    let Some(secret) = conv.ask(&text) else {
                        // The locker is exiting.
                        unsafe { free_replies(replies, n) };
                        return sys::PAM_CONV_ERR;
                    };
                    let bytes = secret.as_str().as_bytes();
                    // SAFETY: room for the bytes plus the NUL, which calloc already zeroed.
                    let buf = unsafe { libc::calloc(bytes.len() + 1, 1) }.cast::<u8>();
                    if buf.is_null() {
                        unsafe { free_replies(replies, n) };
                        return sys::PAM_BUF_ERR;
                    }
                    unsafe {
                        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
                        (*replies.add(i)).resp = buf.cast();
                    }
                    // `secret` drops here and is zeroed.
                }
                sys::PAM_TEXT_INFO => conv.info(&text),
                sys::PAM_ERROR_MSG => tracing::warn!("PAM: {text}"),
                // Binary prompts and anything newer: refuse.
                _ => {
                    unsafe { free_replies(replies, n) };
                    return sys::PAM_CONV_ERR;
                }
            }
        }
        // SAFETY: `resp` is libpam's out-pointer.
        unsafe { *resp = replies };
        sys::PAM_SUCCESS
    }

    /// Free a reply array on a failure path, zeroing any password copy first.
    unsafe fn free_replies(replies: *mut sys::pam_response, n: usize) {
        for i in 0..n {
            // SAFETY: `replies` has `n` entries, each null or a calloc'd NUL-terminated string.
            unsafe {
                let r = (*replies.add(i)).resp;
                if !r.is_null() {
                    let len = libc::strlen(r);
                    ptr::write_bytes(r, 0, len);
                    libc::free(r.cast());
                }
            }
        }
        unsafe { libc::free(replies.cast()) };
    }

    fn current_user() -> Option<String> {
        // SAFETY: getpwuid_r with a local buffer is thread-safe.
        unsafe {
            let mut pw: libc::passwd = std::mem::zeroed();
            let mut result = ptr::null_mut();
            let mut buf = vec![0 as libc::c_char; 4096];
            let rc = libc::getpwuid_r(
                libc::getuid(),
                &mut pw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            );
            if rc != 0 || result.is_null() {
                return None;
            }
            CStr::from_ptr(pw.pw_name).to_str().ok().map(str::to_owned)
        }
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;

    #[test]
    fn a_missing_service_file_is_refused_before_locking() {
        let etc = tempfile::tempdir().unwrap();
        let lib = tempfile::tempdir().unwrap();
        let dirs = [etc.path().to_owned(), lib.path().to_owned()];
        let err = check_service("swaywardlock", &dirs).unwrap_err();
        assert!(
            err.contains("swaywardlock") && err.contains("missing"),
            "{err}"
        );
        // Either directory will do, as for libpam.
        std::fs::write(lib.path().join("swaywardlock"), "auth include login\n").unwrap();
        assert_eq!(check_service("swaywardlock", &dirs), Ok(()));
        std::fs::write(etc.path().join("other-svc"), "").unwrap();
        assert_eq!(check_service("other-svc", &dirs), Ok(()));
    }

    #[test]
    fn service_names_are_file_names() {
        let etc = tempfile::tempdir().unwrap();
        std::fs::write(etc.path().join("ok"), "").unwrap();
        let dirs = [etc.path().to_owned()];
        for bad in ["", ".", "..", "../ok", "a/b"] {
            assert!(check_service(bad, &dirs).is_err(), "{bad:?}");
        }
    }

    /// pam_start lowercases the service name before it looks for the file.
    #[test]
    fn the_service_name_is_lowercased_as_libpam_does() {
        let etc = tempfile::tempdir().unwrap();
        let dirs = [etc.path().to_owned()];
        std::fs::write(etc.path().join("swl-lower"), "").unwrap();
        assert_eq!(check_service("SWL-Lower", &dirs), Ok(()));
        // A file with capitals is never opened by libpam.
        std::fs::write(etc.path().join("SWL-UPPER"), "").unwrap();
        let err = check_service("SWL-UPPER", &dirs).unwrap_err();
        assert!(
            err.contains("missing") && err.contains("swl-upper"),
            "{err}"
        );
    }

    /// A file libpam cannot open (installed mode 600 root) falls back to `other` like a missing
    /// one. libpam goes on to the next directory, so a readable copy there still works.
    #[test]
    fn an_unreadable_service_file_is_refused_before_locking() {
        use std::os::unix::fs::PermissionsExt as _;
        let etc = tempfile::tempdir().unwrap();
        let lib = tempfile::tempdir().unwrap();
        let dirs = [etc.path().to_owned(), lib.path().to_owned()];
        let path = etc.path().join("swaywardlock");
        std::fs::write(&path, "auth include login\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&path).is_ok() {
            eprintln!("skipped: running as root, mode 000 is readable");
            return;
        }
        let err = check_service("swaywardlock", &dirs).unwrap_err();
        assert!(
            err.contains("not readable") && err.contains(&path.display().to_string()),
            "{err}"
        );
        std::fs::write(lib.path().join("swaywardlock"), "auth include login\n").unwrap();
        assert_eq!(check_service("swaywardlock", &dirs), Ok(()));
        // A directory where the file should be is not a service file either.
        std::fs::create_dir(etc.path().join("dir")).unwrap();
        assert!(check_service("dir", &dirs[..1]).is_err());
    }

    /// libpam stops at the first path that opens, a directory included, and refuses everything
    /// even when a later directory holds a valid file.
    #[test]
    fn a_directory_ends_the_lookup_like_in_libpam() {
        let etc = tempfile::tempdir().unwrap();
        let lib = tempfile::tempdir().unwrap();
        let dirs = [etc.path().to_owned(), lib.path().to_owned()];
        std::fs::create_dir(etc.path().join("svc")).unwrap();
        std::fs::write(lib.path().join("svc"), "auth include login\n").unwrap();
        let err = check_service("svc", &dirs).unwrap_err();
        assert!(
            err.contains("not a file")
                && err.contains(&etc.path().join("svc").display().to_string()),
            "{err}"
        );
    }

    /// Systems without pam.d (only /etc/pam.conf) are left to libpam.
    #[test]
    fn no_pam_d_at_all_is_not_checked() {
        let dirs = [PathBuf::from("/nonexistent/pam.d")];
        assert_eq!(check_service("swaywardlock", &dirs), Ok(()));
    }
}

/// These run the real libpam against throwaway service files (`pam_start_confdir`), with stock
/// modules and the probe module in `contrib/test-pam-probe.c`, compiled here with `cc`.
#[cfg(all(test, not(feature = "test-auth")))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    struct Pam {
        dir: tempfile::TempDir,
        auth: Authenticator,
        events: calloop::channel::Channel<AuthEvent>,
    }

    /// A service named `svc` with these rules; `@` in a rule is replaced by the temp dir.
    fn pam(rules: &str) -> Pam {
        pam_with(rules, false)
    }

    /// The same, with `reuse_password` as set by `auth { pam { reuse-password; } }`.
    fn pam_with(rules: &str, reuse_password: bool) -> Pam {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe.so");
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/contrib/test-pam-probe.c");
        let status = std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-o"])
            .arg(&probe)
            .arg(src)
            .status()
            .expect("cc");
        assert!(status.success(), "compiling the PAM probe failed");
        let rules = rules.replace('@', dir.path().to_str().unwrap());
        std::fs::write(dir.path().join("svc"), rules).unwrap();
        let (tx, events) = calloop::channel::channel();
        let auth = Authenticator::spawn_with(
            "svc".into(),
            reuse_password,
            Some(dir.path().to_owned()),
            tx,
        )
        .unwrap();
        Pam { dir, auth, events }
    }

    fn secret(s: &str) -> SecretBuf {
        let mut b = SecretBuf::new();
        b.push_str(s);
        b
    }

    impl Pam {
        fn path(&self, name: &str) -> std::path::PathBuf {
            self.dir.path().join(name)
        }

        fn next(&self, timeout: Duration) -> Option<AuthEvent> {
            let end = Instant::now() + timeout;
            while Instant::now() < end {
                if let Ok(event) = self.events.try_recv() {
                    return Some(event);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        }

        /// The next event that is not a prompt.
        fn result(&self) -> AuthEvent {
            loop {
                match self.next(Duration::from_secs(10)) {
                    Some(AuthEvent::Prompt { .. }) => continue,
                    Some(event) => return event,
                    None => panic!("no result from the auth thread"),
                }
            }
        }

        /// Wait for a follow-up question and return its text.
        fn follow_up(&self) -> String {
            loop {
                match self.next(Duration::from_secs(10)) {
                    Some(AuthEvent::Prompt {
                        text,
                        follow_up: true,
                    }) => return text,
                    Some(AuthEvent::Prompt { .. }) => continue,
                    other => panic!("expected a follow-up prompt, got {other:?}"),
                }
            }
        }

        fn log(&self, name: &str) -> String {
            std::fs::read_to_string(self.path(name)).unwrap_or_default()
        }
    }

    /// The account line only matters to builds that run the account phase (they did once).
    const PASSWORD: &str =
        "auth required @/probe.so mode=password\naccount required pam_permit.so\n";

    #[test]
    fn wrong_then_right_password() {
        let p = pam(PASSWORD);
        p.auth.submit(secret("wrong"));
        assert!(matches!(p.result(), AuthEvent::Failure(_)));
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
        // Nothing follows a success: no new transaction, so no prompt after the unlock.
        assert!(p.next(Duration::from_millis(300)).is_none());
        // Not even for a key typed during the fade-out: the auth thread has finished.
        p.auth.submit(secret("right-pw"));
        assert!(p.next(Duration::from_millis(300)).is_none());
    }

    /// PAM checks the password of the user running the locker, not of some fixed account: a
    /// wrong user would refuse the owner's password (a lockout) or accept another account's.
    #[test]
    fn pam_is_started_for_the_calling_user() {
        let out = std::process::Command::new("id")
            .arg("-un")
            .output()
            .expect("id");
        let user = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        assert!(!user.is_empty(), "id -un printed nothing");
        let p = pam(&format!(
            "auth requisite pam_succeed_if.so user = {user}\n{PASSWORD}"
        ));
        p.auth.submit(secret("right-pw"));
        match p.result() {
            AuthEvent::Success => {}
            other => panic!("PAM did not see the user {user}: {other:?}"),
        }
    }

    /// The locker exiting in the middle of a transaction (a follow-up question pending) ends it
    /// quietly: no failure is reported for an answer nobody gave.
    #[test]
    fn exiting_during_a_follow_up_reports_nothing() {
        let p = pam("auth required @/probe.so mode=otp\n");
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "OTP: ");
        let Pam {
            dir: _dir,
            auth,
            events,
        } = p;
        drop(auth);
        std::thread::sleep(Duration::from_millis(300));
        let left: Vec<AuthEvent> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// pam_nologin (systemd's /run/nologin before a shutdown) and password aging fail the
    /// account phase. A locker re-authenticates an existing session, so like swaylock and
    /// hyprlock it runs only the auth phase.
    #[test]
    fn account_policy_does_not_refuse_the_right_password() {
        let p = pam(&format!(
            "{PASSWORD}account required pam_nologin.so file=@/nologin\n\
             account required pam_debug.so acct=new_authtok_reqd\n"
        ));
        std::fs::write(p.path("nologin"), "System maintenance\n").unwrap();
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// A password-then-one-time-code stack: the second prompt reaches the UI and is answered
    /// with the next submitted secret, in the same transaction.
    #[test]
    fn a_second_prompt_is_answered_with_the_next_submit() {
        let p = pam("auth required @/probe.so mode=otp\n");
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "OTP: ");
        p.auth.submit(secret("123456"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// A second factor whose prompt text is the password's (pam_radius_auth's default
    /// "Password: ") is asked, not answered with the login password.
    #[test]
    fn a_repeated_question_is_asked_again() {
        let p = pam("auth required @/probe.so mode=codepw\n");
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "Password: ");
        p.auth.submit(secret("123456"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// `reuse-password`: the same question twice in one transaction is answered from the first
    /// submit (two modules that each ask for the password).
    #[test]
    fn reuse_password_answers_a_repeated_question() {
        let p = pam_with("auth required @/probe.so mode=twice\n", true);
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
        // Without it the second question goes to the user.
        let p = pam("auth required @/probe.so mode=twice\n");
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "Password: ");
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// `reuse-password` answers only the question the password was typed for: the one-time code
    /// of a password-then-OTP stack is still asked, not answered with the password.
    #[test]
    fn reuse_password_never_answers_another_question() {
        let p = pam_with("auth required @/probe.so mode=otp\n", true);
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "OTP: ");
        p.auth.submit(secret("123456"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// Only once: a third identical question goes to the user, so a module that keeps asking
    /// cannot spin on a stored answer.
    #[test]
    fn a_question_asked_three_times_reaches_the_user_again() {
        let p = pam_with("auth required @/probe.so mode=thrice\n", true);
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "Password: ");
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// swaylock and hyprlock answer visible prompts too.
    #[test]
    fn an_echo_on_prompt_is_answered() {
        let p = pam("auth required @/probe.so mode=echoon\n");
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// pam_faillock says why a locked account fails in a TEXT_INFO; hyprlock shows it.
    #[test]
    fn faillock_text_becomes_the_failure_message() {
        let p = pam("auth required pam_echo.so file=@/msg\nauth required pam_deny.so\n");
        std::fs::write(p.path("msg"), "(9 minutes left to unlock)").unwrap();
        p.auth.submit(secret("anything"));
        match p.result() {
            AuthEvent::Failure(msg) => assert_eq!(msg, "(9 minutes left to unlock)"),
            other => panic!("{other:?}"),
        }
    }

    /// swaylock refreshes credentials (Kerberos tickets and the like) after unlocking.
    #[test]
    fn credentials_are_refreshed_after_success() {
        let p = pam(&format!("auth required @/probe.so out=@/cred\n{PASSWORD}"));
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
        let log = std::fs::read_to_string(p.path("cred")).unwrap_or_default();
        assert_eq!(
            log.trim(),
            format!("setcred {}", pam_sys2::PAM_REFRESH_CRED)
        );
    }

    /// No transaction runs before the first submit, so nothing is asked on the idle screen
    /// (main.rs shows the default prompt). After a failed attempt `$PAMPROMPT` goes back to the
    /// transaction's first question, not the one-time code it asked last.
    #[test]
    fn the_first_question_is_the_prompt_for_the_next_attempt() {
        let p = pam("auth required @/probe.so mode=otp\n");
        assert!(p.next(Duration::from_millis(300)).is_none());
        p.auth.submit(secret("right-pw"));
        assert_eq!(p.follow_up(), "OTP: ");
        p.auth.submit(secret("000000"));
        match p.next(Duration::from_secs(10)) {
            Some(AuthEvent::Prompt {
                text,
                follow_up: false,
            }) => assert_eq!(text, "Password: "),
            other => panic!("expected the first question again, got {other:?}"),
        }
        assert!(matches!(p.result(), AuthEvent::Failure(_)));
        assert!(p.next(Duration::from_millis(300)).is_none());
    }

    /// pam_faillock decides in its preauth, when the transaction reaches it, whether the
    /// account is locked. That must be when the user submits: once a lockout has expired, the
    /// first right password unlocks.
    #[test]
    fn a_lockout_that_expired_before_the_submit_does_not_refuse() {
        let p = pam("auth required @/probe.so mode=locked file=@/locked\n");
        std::fs::write(p.path("locked"), "").unwrap();
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Failure(_)));
        std::thread::sleep(Duration::from_millis(200));
        std::fs::remove_file(p.path("locked")).unwrap(); // unlock_time is over
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Success));
    }

    /// And the other way round: an account locked (from elsewhere) while the screen sat idle
    /// is refused, the right password too.
    #[test]
    fn a_lockout_that_began_while_idle_refuses() {
        let p = pam("auth required @/probe.so mode=locked file=@/locked\n");
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(p.path("locked"), "").unwrap();
        p.auth.submit(secret("right-pw"));
        assert!(matches!(p.result(), AuthEvent::Failure(_)));
    }

    /// A pam_fprintd-first stack (Fedora authselect with-fingerprint) reads the reader only after
    /// Enter, as with swaylock: a touch on the idle screen is never consumed and thrown away.
    #[test]
    fn a_fingerprint_module_runs_only_after_a_submit() {
        let p = pam(&format!(
            "auth sufficient @/probe.so mode=finger file=@/touch out=@/log\n{PASSWORD}"
        ));
        std::fs::write(p.path("touch"), "").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(p.log("log"), "", "a transaction ran before any submit");
        assert!(p.path("touch").exists(), "the idle touch was consumed");
        std::fs::remove_file(p.path("touch")).unwrap();
        // Enter, then touch.
        p.auth.submit(secret(""));
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(p.path("touch"), "").unwrap();
        assert!(matches!(p.result(), AuthEvent::Success));
        assert!(
            p.log("log").starts_with("finger start\n"),
            "{}",
            p.log("log")
        );
    }

    /// A stack that fails before asking anything reports one failure per submit and does not
    /// spin while nobody types.
    #[test]
    fn a_failure_before_any_prompt_waits_for_input() {
        let p = pam("auth required pam_deny.so\n");
        assert!(p.next(Duration::from_millis(300)).is_none());
        p.auth.submit(secret("x"));
        assert!(matches!(p.result(), AuthEvent::Failure(_)));
        assert!(p.next(Duration::from_millis(300)).is_none());
        p.auth.submit(secret("y"));
        assert!(matches!(p.result(), AuthEvent::Failure(_)));
    }

    /// A stack that would succeed without asking (no password under nullok) still waits for a
    /// submit: nothing unlocks, and no credentials are refreshed, without input.
    #[test]
    fn a_success_without_a_prompt_still_needs_a_submit() {
        let p = pam("auth required pam_permit.so\nauth optional @/probe.so out=@/cred\n");
        assert!(p.next(Duration::from_millis(300)).is_none());
        assert_eq!(
            std::fs::read_to_string(p.path("cred")).unwrap_or_default(),
            ""
        );
        p.auth.submit(secret(""));
        assert!(matches!(p.result(), AuthEvent::Success));
        let log = std::fs::read_to_string(p.path("cred")).unwrap_or_default();
        assert_eq!(log.lines().count(), 1, "{log}");
    }

    /// The shipped path: `pam_start` and the service file in /etc/pam.d, as a release build
    /// runs it (the tests above use `pam_start_confdir`). `contrib/test-real-pam` installs a
    /// throwaway service (root or sudo) whose pam_exec script accepts only "right-pw", and only
    /// for the user running the test (PAM_USER), followed by `account required pam_deny.so`, which
    /// must not matter: only the auth phase runs. With `SWAYWARDLOCK_TEST_REAL_PASSWORD` it
    /// also installs one with `auth include login` and checks the user's real password against
    /// it.
    #[test]
    #[ignore = "needs a PAM service in /etc/pam.d: run contrib/test-real-pam"]
    fn an_installed_service_in_etc_pam_d() {
        let service = std::env::var("SWAYWARDLOCK_TEST_PAM_SERVICE")
            .expect("SWAYWARDLOCK_TEST_PAM_SERVICE (set by contrib/test-real-pam)");
        let installed = |service: &str, right: &str| {
            assert_eq!(check_service_installed(service), Ok(()));
            let (tx, events) = calloop::channel::channel();
            let auth = Authenticator::spawn(service.to_owned(), false, tx).unwrap();
            let p = Pam {
                dir: tempfile::tempdir().unwrap(),
                auth,
                events,
            };
            p.auth.submit(secret("wrong"));
            assert!(matches!(p.result(), AuthEvent::Failure(_)), "{service}");
            p.auth.submit(secret(right));
            match p.result() {
                AuthEvent::Success => {}
                other => panic!("{service}: the right password got {other:?}"),
            }
        };
        installed(&service, "right-pw");
        let missing = format!("{service}-missing");
        assert!(check_service_installed(&missing).is_err());
        if let (Ok(login), Ok(password)) = (
            std::env::var("SWAYWARDLOCK_TEST_PAM_LOGIN_SERVICE"),
            std::env::var("SWAYWARDLOCK_TEST_REAL_PASSWORD"),
        ) {
            installed(&login, &password);
        }
    }

    /// The faillock text is read when the user submits, not when the transaction started.
    #[test]
    fn a_message_before_any_prompt_is_current_at_submit() {
        let p = pam("auth required pam_echo.so file=@/msg\nauth required pam_deny.so\n");
        std::fs::write(p.path("msg"), "(9 minutes left to unlock)").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(p.path("msg"), "(3 minutes left to unlock)").unwrap();
        p.auth.submit(secret("x"));
        match p.result() {
            AuthEvent::Failure(msg) => assert_eq!(msg, "(3 minutes left to unlock)"),
            other => panic!("{other:?}"),
        }
    }
}
