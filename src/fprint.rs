//! Fingerprint unlock through fprintd on the system bus, after hyprlock's `CFingerprint`.
//!
//! One worker thread owns a blocking zbus connection. It claims the default device, runs
//! `VerifyStart("any")`, and turns `VerifyStatus` into events on a calloop channel. A non-match
//! retries after `retry_delay` up to three times; retry statuses (finger not centred, swipe too
//! short, ...) update the prompt and keep scanning. It stops scanning before sleep and starts
//! again after (logind `PrepareForSleep`). It does not start while logind is already preparing
//! for sleep, and ignores a match unless it is scanning and awake. Device, logind and fprintd-gone
//! signals are handled on one thread, but each comes from its own match-rule stream, so handling
//! order is not bus order. Instead a match is accepted only if its bus receive position is after
//! the reply to the current `VerifyStart`; a match sent while asleep (or before the restart)
//! is therefore rejected even if the wake is handled first. If fprintd or a device is missing, it
//! logs and exits; PAM keeps working.
//!
//! Only `verify-match` produces [`FprintEvent::Matched`], which the main loop turns into an
//! unlock request, the same path as a PAM success.
//!
//! Signals are trusted only from the connection that owns `net.reactivated.Fprint` (or
//! `org.freedesktop.login1`) when we start, compared by unique name. The system bus lets any
//! client send a signal addressed to us, and zbus cannot match a well-known sender name locally,
//! so filtering by the well-known name would accept a forged `verify-match` from any local
//! process. If fprintd restarts (a new unique name), fingerprint unlock stops for this lock: the
//! bus's `NameOwnerChanged` for the old unique name clears the prompt and sets `$FPRINTFAIL`.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

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
    /// Fingerprints are no longer read (fprintd went away, or scanning could not restart): the
    /// prompt clears and the text goes to `$FPRINTFAIL`. Not a failed attempt.
    Unavailable(String),
    Matched,
}

const UNAVAILABLE: &str = "Fingerprint unavailable";

#[derive(Debug, Clone)]
pub struct Options {
    pub ready_message: String,
    pub present_message: String,
    pub retry_delay: Duration,
}

/// Start the worker. Returns immediately; problems are reported as log warnings.
pub fn spawn(options: Options, events: Sender<FprintEvent>) {
    let _ = thread::Builder::new().name("fprint".into()).spawn(move || {
        let mut prompted = false;
        if let Err(err) = run(&options, &events, &mut prompted) {
            tracing::warn!("fingerprint unlock unavailable: {err:#}");
            // A prompt already on screen must not keep asking for a finger nobody reads.
            if prompted {
                let _ = events.send(FprintEvent::Unavailable(UNAVAILABLE.into()));
            }
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

/// logind's `PreparingForSleep` property; false when it cannot be read.
fn preparing_for_sleep(conn: &Connection, logind: &OwnedUniqueName) -> bool {
    Proxy::new(
        conn,
        logind.clone(),
        "/org/freedesktop/login1",
        "org.freedesktop.DBus.Properties",
    )
    .and_then(|p| {
        p.call::<_, _, OwnedValue>(
            "Get",
            &("org.freedesktop.login1.Manager", "PreparingForSleep"),
        )
    })
    .map_err(|err| tracing::warn!("fprint: cannot read PreparingForSleep: {err}"))
    .ok()
    .and_then(|v| bool::try_from(&v).ok())
    .unwrap_or(false)
}

/// Whether `msg` was sent by `trusted` (a unique name).
fn sent_by(msg: &zbus::Message, trusted: &OwnedUniqueName) -> bool {
    msg.header()
        .sender()
        .is_some_and(|s| *s == trusted.as_ref())
}

/// Whether `msg` is the bus saying the connection `name` has gone (`NameOwnerChanged(name, name,
/// "")`). Only the bus itself can send it; anything else is a forgery.
fn is_gone(msg: &zbus::Message, name: &OwnedUniqueName) -> bool {
    let from_bus = msg
        .header()
        .sender()
        .is_some_and(|s| s.as_str() == "org.freedesktop.DBus");
    from_bus
        && msg
            .body()
            .deserialize::<(String, String, String)>()
            .is_ok_and(|(n, _, new)| n == name.as_str() && new.is_empty())
}

fn run(o: &Options, events: &Sender<FprintEvent>, prompted: &mut bool) -> anyhow::Result<()> {
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
    let device_signals = MessageIterator::for_match_rule(rule, &conn, None)?;
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
    // fprintd's connection going away (crash, restart). Subscribed before claiming, so a
    // vanish after the owner lookup above is either seen here or fails the Claim.
    let vanish_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, fprintd.as_str())?
        .build();
    let vanish_signals = MessageIterator::for_match_rule(vanish_rule, &conn, None)?;

    // Empty user name means "the caller".
    let () = device.call("Claim", &("",))?;
    let release = scopeguard(|| {
        let _ = device.call_method("VerifyStop", &());
        let _ = device.call_method("Release", &());
    });

    // One ordered queue for device, logind and fprintd-vanish signals, handled on this thread
    // alone. Arrival order is what matters: a match fprintd sent while the machine slept must
    // be seen before the wake, however long a retry delay was running.
    let (tx, rx) = mpsc::channel::<Item>();
    let forward = |name: &str, stream: MessageIterator, wrap: fn(zbus::Message) -> Item| {
        let tx = tx.clone();
        let spawned = thread::Builder::new().name(name.into()).spawn(move || {
            for msg in stream {
                let item = match msg {
                    Ok(msg) => wrap(msg),
                    Err(err) => Item::Failed(err.to_string()),
                };
                if tx.send(item).is_err() {
                    return;
                }
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("fprint: cannot start {name} watcher: {err}");
        }
    };
    forward("fprint-dev", device_signals, Item::Device);
    forward("fprint-vanish", vanish_signals, Item::Vanish);
    if let Some(sleep_signals) = sleep_signals {
        forward("fprint-sleep", sleep_signals, Item::Sleep);
    }
    drop(tx);

    let mut scan = Scan::default();
    let start = |scan: &mut Scan, prompt: &str| -> anyhow::Result<()> {
        // Asleep: waking restarts it. Scanning: a wake inside the retry delay already restarted
        // it, and fprintd refuses a second VerifyStart (AlreadyInUse).
        if scan.asleep || scan.scanning {
            return Ok(());
        }
        let reply = device.call_method("VerifyStart", &("any",))?;
        // Statuses received before this reply belong to an older session.
        scan.since = reply.recv_position();
        scan.scanning = true;
        let _ = events.send(FprintEvent::Prompt(prompt.to_owned()));
        Ok(())
    };
    // Already preparing for sleep (logind property): wait for the wake signal instead.
    if let Some(logind) = &logind {
        if preparing_for_sleep(&conn, logind) {
            tracing::info!("fprint: preparing for sleep; not scanning until wake");
            scan.asleep = true;
        }
    }
    start(&mut scan, &o.ready_message)?;
    *prompted = true;

    let mut retries = 0;
    // When a scan restarts after a non-match.
    let mut restart_at: Option<Instant> = None;
    loop {
        let item = match restart_at {
            None => match rx.recv() {
                Ok(item) => item,
                Err(_) => break,
            },
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(item) => item,
                Err(RecvTimeoutError::Timeout) => {
                    restart_at = None;
                    if let Err(err) = start(&mut scan, "Could not match fingerprint. Try again.") {
                        // Keep the claim and the loop: a wake from sleep tries again.
                        tracing::warn!("fprint: could not restart scanning: {err:#}");
                        let _ = events.send(FprintEvent::Unavailable(UNAVAILABLE.into()));
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };
        let msg = match item {
            Item::Failed(err) => anyhow::bail!("{err}"),
            Item::Vanish(msg) => {
                if is_gone(&msg, &fprintd) {
                    // The claim died with fprintd's connection.
                    tracing::warn!("fprint: fprintd went away; fingerprint unlock stops");
                    let _ = events.send(FprintEvent::Unavailable(UNAVAILABLE.into()));
                    std::mem::forget(release);
                    return Ok(());
                }
                continue;
            }
            Item::Sleep(msg) => {
                if !logind.as_ref().is_some_and(|l| sent_by(&msg, l)) {
                    tracing::warn!("fprint: ignoring PrepareForSleep not from logind");
                    continue;
                }
                let Ok(starting) = msg.body().deserialize::<bool>() else {
                    continue;
                };
                tracing::info!("fprint: PrepareForSleep({starting})");
                scan.asleep = starting;
                if starting {
                    if scan.scanning {
                        let _ = device.call_method("VerifyStop", &());
                        scan.scanning = false;
                    }
                } else {
                    restart_at = None;
                    if let Err(err) = start(&mut scan, &o.ready_message) {
                        tracing::warn!("fprint: could not restart scanning after wake: {err:#}");
                    }
                }
                continue;
            }
            Item::Device(msg) => msg,
        };
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
                        if scan.asleep || !scan.scanning || msg.recv_position() < scan.since {
                            tracing::warn!("fprint: ignoring a match while not scanning");
                            continue;
                        }
                        let _ = events.send(FprintEvent::Matched);
                        break;
                    }
                    Next::Retry(prompt) => {
                        let _ = events.send(FprintEvent::Prompt(prompt.into()));
                    }
                    Next::Restart(why) => {
                        retries += 1;
                        let _ = events.send(FprintEvent::Failed(why.into()));
                        if scan.scanning {
                            let _ = device.call_method("VerifyStop", &());
                            scan.scanning = false;
                        }
                        restart_at = Some(Instant::now() + o.retry_delay);
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
    drop(release);
    Ok(())
}

/// A signal for the device loop (queue order is scheduler order, not bus order).
enum Item {
    Device(zbus::Message),
    Sleep(zbus::Message),
    Vanish(zbus::Message),
    Failed(String),
}

/// Scanning state.
#[derive(Default)]
struct Scan {
    /// A VerifyStart is outstanding.
    scanning: bool,
    /// Between PrepareForSleep(true) and (false).
    asleep: bool,
    /// Receive position of the current VerifyStart reply.
    since: zbus::message::Sequence,
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

    fn name_owner_changed(sender: &str, args: (&str, &str, &str)) -> zbus::Message {
        zbus::Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameOwnerChanged",
        )
        .unwrap()
        .sender(sender)
        .unwrap()
        .build(&args)
        .unwrap()
    }

    #[test]
    fn only_the_bus_can_say_fprintd_is_gone() {
        let fprintd = OwnedUniqueName::try_from(":1.7").unwrap();
        let bus = "org.freedesktop.DBus";
        assert!(is_gone(
            &name_owner_changed(bus, (":1.7", ":1.7", "")),
            &fprintd
        ));
        // Another connection, another name, or a new owner (not a vanish).
        assert!(!is_gone(
            &name_owner_changed(":1.9", (":1.7", ":1.7", "")),
            &fprintd
        ));
        assert!(!is_gone(
            &name_owner_changed(bus, (":1.8", ":1.8", "")),
            &fprintd
        ));
        assert!(!is_gone(
            &name_owner_changed(bus, (":1.7", "", ":1.7")),
            &fprintd
        ));
    }
}
