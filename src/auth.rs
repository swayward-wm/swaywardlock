//! Authentication on a worker thread. Results come back over a calloop channel.
//!
//! PAM is called through the raw `pam-sys2` bindings with our own conversation function, so the
//! password goes from the locked [`SecretBuf`] straight into the `malloc`ed response libpam asks
//! for. No intermediate Rust `String`/`CString` copy is ever made. What libpam and its modules do
//! with the response after that is outside our control, as it is for every PAM client.

use std::sync::mpsc;
use std::thread;

use calloop::channel::Sender;

use crate::secret::SecretBuf;

#[derive(Debug)]
pub enum AuthEvent {
    Success,
    Failure(String),
}

/// Runs one PAM transaction per submitted secret, one at a time.
pub struct Authenticator {
    tx: mpsc::Sender<SecretBuf>,
}

impl Authenticator {
    pub fn spawn(service: String, events: Sender<AuthEvent>) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel::<SecretBuf>();
        thread::Builder::new().name("auth".into()).spawn(move || {
            for secret in rx {
                let event = check(&service, secret);
                if events.send(event).is_err() {
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

/// Test builds accept exactly `TEST_PASSWORD`, so end-to-end tests run without a PAM setup.
#[cfg(feature = "test-auth")]
pub const TEST_PASSWORD: &str = "swaywardlock-test";

#[cfg(feature = "test-auth")]
fn check(_service: &str, secret: SecretBuf) -> AuthEvent {
    if secret.as_str() == TEST_PASSWORD {
        AuthEvent::Success
    } else {
        AuthEvent::Failure("wrong password".into())
    }
}

#[cfg(not(feature = "test-auth"))]
fn check(service: &str, secret: SecretBuf) -> AuthEvent {
    match pam::authenticate(service, secret) {
        Ok(()) => AuthEvent::Success,
        Err(msg) => AuthEvent::Failure(msg),
    }
}

#[cfg(not(feature = "test-auth"))]
mod pam {
    use std::ffi::{c_int, c_void, CStr, CString};
    use std::ptr;

    use pam_sys2 as sys;

    use super::*;

    /// Conversation state handed to libpam as `appdata_ptr`.
    struct Conv {
        secret: Option<SecretBuf>,
    }

    pub fn authenticate(service: &str, secret: SecretBuf) -> Result<(), String> {
        let user = current_user().ok_or("cannot determine the current user")?;
        let service = CString::new(service).map_err(|_| "invalid PAM service name")?;
        let user = CString::new(user).map_err(|_| "invalid user name")?;

        let mut conv_state = Conv {
            secret: Some(secret),
        };
        let conv = sys::pam_conv {
            conv: Some(converse),
            appdata_ptr: (&mut conv_state as *mut Conv).cast(),
        };
        let mut handle: *mut sys::pam_handle_t = ptr::null_mut();

        // SAFETY: all pointers are valid for the duration of the transaction; `conv_state` and
        // `conv` outlive `pam_end`.
        unsafe {
            let rc = sys::pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
            if rc != sys::PAM_SUCCESS {
                return Err(format!("PAM start failed ({rc})"));
            }
            let mut rc = sys::pam_authenticate(handle, 0);
            if rc == sys::PAM_SUCCESS {
                rc = sys::pam_acct_mgmt(handle, 0);
            }
            let msg = if rc == sys::PAM_SUCCESS {
                None
            } else {
                let s = sys::pam_strerror(handle, rc);
                Some(if s.is_null() {
                    format!("PAM error {rc}")
                } else {
                    CStr::from_ptr(s).to_string_lossy().into_owned()
                })
            };
            sys::pam_end(handle, rc);
            match msg {
                None => Ok(()),
                Some(msg) => Err(msg),
            }
        }
        // `conv_state` drops here, zeroing the secret if no prompt consumed it.
    }

    /// Answer the first hidden prompt with the secret, report text messages, refuse anything
    /// else. Responses are `calloc`ed as libpam requires; libpam frees them.
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
        let conv = unsafe { &mut *appdata.cast::<Conv>() };
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
                sys::PAM_PROMPT_ECHO_OFF => {
                    let Some(secret) = conv.secret.take() else {
                        // A second password prompt (e.g. a changed-token flow): refuse.
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
                sys::PAM_TEXT_INFO => tracing::info!("PAM: {text}"),
                sys::PAM_ERROR_MSG => tracing::warn!("PAM: {text}"),
                // A visible prompt asks for a user name or similar; we have none to give.
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
