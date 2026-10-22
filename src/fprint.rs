//! Fingerprint unlock through fprintd on the system bus, after hyprlock's `CFingerprint`.
//!
//! One worker thread owns a blocking zbus connection. It claims the default device, runs
//! `VerifyStart("any")`, and turns `VerifyStatus` into events on a calloop channel. A non-match
//! retries after `retry_delay` up to three times; retry statuses (finger not centred, swipe too
//! short, ...) update the prompt and keep scanning. It stops scanning before sleep and starts
//! again after (logind `PrepareForSleep`). If fprintd or a device is missing, it logs and exits;
//! PAM keeps working.
//!
//! Only `verify-match` produces [`FprintEvent::Matched`], which the main loop turns into an
//! unlock request, the same path as a PAM success.
//!
//! Signals are trusted only from the connection that owns `net.reactivated.Fprint` (or
//! `org.freedesktop.login1`) when we start, compared by unique name. The system bus lets any
//! client send a signal addressed to us, and zbus cannot match a well-known sender name locally,
//! so filtering by the well-known name would accept a forged `verify-match` from any local
//! process. If fprintd restarts (a new unique name), fingerprint unlock stops for this lock.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use calloop::channel::Sender;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::MatchRule;

const FPRINT: &str = "net.reactivated.Fprint";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const MANAGER: &str = "net.reactivated.Fprint.Manager";
const DEVICE: &str = "net.reactivated.Fprint.Device";
/// Restarts after a non-match; hyprlock gives up on the non-match after the third restart.
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FprintEvent {
    /// Text for `$FPRINTPROMPT`.
    Prompt(String),
    /// Text for `$FPRINTFAIL`; scanning may continue.
    Failed(String),
    Matched,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub ready_message: String,
    pub present_message: String,
    pub retry_delay: Duration,
}

/// Start the worker. Returns immediately; problems are reported as log warnings.
pub fn spawn(options: Options, events: Sender<FprintEvent>) {
    let _ = thread::Builder::new().name("fprint".into()).spawn(move || {
        if let Err(err) = run(&options, &events) {
            tracing::warn!("fingerprint unlock unavailable: {err:#}");
        }
    });
}

/// What to do with a `VerifyStatus` result (pure, after hyprlock's `handleVerifyStatus`).
#[derive(Debug, PartialEq, Eq)]
enum Next {
    Matched,
    /// Keep scanning, with a new prompt.
    Retry(&'static str),
    /// Scan finished without a match; restart after the delay.
    Restart(&'static str),
    /// Give up on fingerprints.
    Stop(&'static str),
    Ignore,
}

fn classify(result: &str, retries: u32) -> Next {
    match result {
        "verify-match" => Next::Matched,
        // `retries` counts restarts done, so the scan after the third restart is the last.
        "verify-no-match" if retries >= MAX_RETRIES => {
            Next::Stop("Fingerprint auth disabled (too many failed attempts)")
        }
        "verify-no-match" => Next::Restart("Fingerprint did not match"),
        "verify-retry-scan" => Next::Retry("Please retry fingerprint scan"),
        "verify-swipe-too-short" => Next::Retry("Swipe too short - try again"),
        "verify-finger-not-centered" => Next::Retry("Finger not centered - try again"),
        "verify-remove-and-retry" => Next::Retry("Remove your finger and try again"),
        "verify-disconnected" => Next::Stop("Fingerprint device disconnected"),
        "verify-unknown-error" => Next::Stop("Fingerprint auth disabled (unknown error)"),
        other => {
            tracing::warn!("fprint: unknown status {other:?}");
            Next::Ignore
        }
    }
}

/// The unique name currently owning `name`, e.g. `:1.42`.
fn owner(conn: &Connection, name: &str) -> anyhow::Result<OwnedUniqueName> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn)?;
    Ok(dbus.get_name_owner(name.try_into()?)?)
}

/// Whether `msg` was sent by `trusted` (a unique name).
fn sent_by(msg: &zbus::Message, trusted: &OwnedUniqueName) -> bool {
    msg.header()
        .sender()
        .is_some_and(|s| *s == trusted.as_ref())
}

fn run(o: &Options, events: &Sender<FprintEvent>) -> anyhow::Result<()> {
    let conn = Connection::system()?;
    let fprintd = owner(&conn, FPRINT)?;
    let manager = Proxy::new(&conn, fprintd.clone(), MANAGER_PATH, MANAGER)?;
    let path: OwnedObjectPath = manager.call("GetDefaultDevice", &())?;
    tracing::info!(
        "fprint: using device {} on {}",
        path.as_str(),
        fprintd.as_str()
    );
    let device = Proxy::new(&conn, fprintd.clone(), path.clone(), DEVICE)?;

    // Subscribe before claiming so no status is missed. The rule names fprintd's unique name,
    // which zbus does check locally; every message is checked again below regardless.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(fprintd.clone())?
        .path(path.as_str())?
        .build();
    let mut device_signals = MessageIterator::for_match_rule(rule, &conn, None)?;
    let logind = owner(&conn, "org.freedesktop.login1").ok();
    let sleep_signals = logind.clone().and_then(|logind| {
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(logind)
            .ok()?
            .interface("org.freedesktop.login1.Manager")
            .ok()?
            .member("PrepareForSleep")
            .ok()?
            .build();
        MessageIterator::for_match_rule(rule, &conn, None).ok()
    });

    // Empty user name means "the caller".
    let () = device.call("Claim", &("",))?;
    let release = scopeguard(|| {
        let _ = device.call_method("VerifyStop", &());
        let _ = device.call_method("Release", &());
    });

    // Shared by this loop and the sleep thread, so a retry never restarts scanning while the
    // machine sleeps and nothing touches the device after the loop has released it.
    let scan = Arc::new(Mutex::new(Scan::default()));
    let start = |scan: &mut Scan, prompt: &str| -> anyhow::Result<()> {
        if scan.asleep || scan.done {
            return Ok(()); // waking restarts it
        }
        let () = device.call("VerifyStart", &("any",))?;
        scan.scanning = true;
        let _ = events.send(FprintEvent::Prompt(prompt.to_owned()));
        Ok(())
    };
    start(&mut scan.lock().unwrap(), &o.ready_message)?;

    // Sleep handling on its own thread: stop before sleep, restart after.
    if let Some(sleep_signals) = sleep_signals {
        let scan = scan.clone();
        let device = device.clone();
        let ready = o.ready_message.clone();
        let events = events.clone();
        thread::spawn(move || {
            let Some(logind) = logind else { return };
            for msg in sleep_signals.flatten() {
                if !sent_by(&msg, &logind) {
                    tracing::warn!("fprint: ignoring PrepareForSleep not from logind");
                    continue;
                }
                let Ok(starting) = msg.body().deserialize::<bool>() else {
                    continue;
                };
                let mut s = scan.lock().unwrap();
                if s.done {
                    return;
                }
                tracing::info!("fprint: PrepareForSleep({starting})");
                s.asleep = starting;
                if starting {
                    if s.scanning {
                        let _ = device.call_method("VerifyStop", &());
                        s.scanning = false;
                    }
                } else if !s.scanning && device.call_method("VerifyStart", &("any",)).is_ok() {
                    s.scanning = true;
                    let _ = events.send(FprintEvent::Prompt(ready.clone()));
                }
            }
        });
    }

    let mut retries = 0;
    for msg in device_signals.by_ref() {
        let msg = msg?;
        if !sent_by(&msg, &fprintd) {
            tracing::warn!(
                "fprint: ignoring a device signal not from fprintd ({:?})",
                msg.header().sender()
            );
            continue;
        }
        let header = msg.header();
        let member = header
            .member()
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default();
        match member.as_str() {
            "VerifyStatus" => {
                let (result, _done): (String, bool) = msg.body().deserialize()?;
                match classify(&result, retries) {
                    Next::Matched => {
                        let _ = events.send(FprintEvent::Matched);
                        break;
                    }
                    Next::Retry(prompt) => {
                        let _ = events.send(FprintEvent::Prompt(prompt.into()));
                    }
                    Next::Restart(why) => {
                        retries += 1;
                        let _ = events.send(FprintEvent::Failed(why.into()));
                        {
                            let mut s = scan.lock().unwrap();
                            if s.scanning {
                                let _ = device.call_method("VerifyStop", &());
                                s.scanning = false;
                            }
                        }
                        thread::sleep(o.retry_delay);
                        start(
                            &mut scan.lock().unwrap(),
                            "Could not match fingerprint. Try again.",
                        )?;
                    }
                    Next::Stop(why) => {
                        let _ = events.send(FprintEvent::Failed(why.into()));
                        break;
                    }
                    Next::Ignore => {}
                }
            }
            "PropertiesChanged" => {
                let (iface, changed, _): (
                    String,
                    std::collections::HashMap<String, OwnedValue>,
                    Vec<String>,
                ) = msg.body().deserialize()?;
                if iface == DEVICE {
                    if let Some(v) = changed.get("finger-present") {
                        if bool::try_from(v).unwrap_or(false) {
                            let _ = events.send(FprintEvent::Prompt(o.present_message.clone()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    scan.lock().unwrap().done = true;
    drop(release);
    Ok(())
}

/// Scanning state shared with the sleep thread.
#[derive(Default)]
struct Scan {
    /// A VerifyStart is outstanding.
    scanning: bool,
    /// Between PrepareForSleep(true) and (false).
    asleep: bool,
    /// The main loop ended and released the device.
    done: bool,
}

/// Run `f` on drop.
fn scopeguard<F: FnOnce()>(f: F) -> impl Drop {
    struct Guard<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for Guard<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    Guard(Some(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_follows_hyprlock() {
        assert_eq!(classify("verify-match", 0), Next::Matched);
        assert_eq!(
            classify("verify-no-match", 0),
            Next::Restart("Fingerprint did not match")
        );
        assert_eq!(
            classify("verify-no-match", 1),
            Next::Restart("Fingerprint did not match")
        );
        assert_eq!(
            classify("verify-no-match", 2),
            Next::Restart("Fingerprint did not match")
        );
        assert!(
            matches!(classify("verify-no-match", 3), Next::Stop(_)),
            "the fourth miss (third restart done) gives up, as in hyprlock"
        );
        assert!(matches!(
            classify("verify-finger-not-centered", 0),
            Next::Retry(_)
        ));
        assert!(matches!(classify("verify-disconnected", 0), Next::Stop(_)));
        assert_eq!(classify("verify-something-new", 0), Next::Ignore);
    }
}
