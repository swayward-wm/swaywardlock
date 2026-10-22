//! Label text: `$VARIABLE` substitution and the `cmd[...]` prefix, in hyprlock's syntax so its
//! labels can be pasted in unchanged (`IWidget::formatString`).

use std::time::Duration;

/// A label template, parsed once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Template {
    /// Plain text with `$VARIABLES`, substituted on every frame.
    Text(String),
    /// `cmd[update:MS(:1)] <shell>`: substitute variables into `<shell>`, run it, show its output.
    Cmd(Cmd),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub shell: String,
    /// Re-run interval; `None` runs once.
    pub every: Option<Duration>,
    /// `cmd[update:MS:1]`: also re-run on SIGUSR2.
    pub on_signal: bool,
}

impl Template {
    /// Parse hyprlock's `cmd[update:1000] date` prefix. Unknown options are warned about and
    /// ignored, as hyprlock does.
    pub fn parse(text: &str) -> Self {
        let Some(rest) = text.strip_prefix("cmd[") else {
            return Template::Text(text.to_owned());
        };
        let Some((opts, shell)) = rest.split_once(']') else {
            return Template::Text(text.to_owned());
        };
        let mut cmd = Cmd {
            shell: shell.trim_start().to_owned(),
            every: None,
            on_signal: false,
        };
        for opt in opts.split(',').map(str::trim).filter(|o| !o.is_empty()) {
            match opt.strip_prefix("update:") {
                Some(v) => {
                    let mut parts = v.split(':');
                    match parts.next().and_then(|ms| ms.parse::<u64>().ok()) {
                        Some(0) | None => tracing::warn!("cmd[]: bad update interval {v:?}"),
                        Some(ms) => cmd.every = Some(Duration::from_millis(ms)),
                    }
                    cmd.on_signal = matches!(parts.next(), Some("1" | "true"));
                }
                None => tracing::warn!("cmd[]: unknown option {opt:?}"),
            }
        }
        Template::Cmd(cmd)
    }

    /// Whether `text` is a `cmd[...]` label: the prefix and a closing `]`, as in hyprlock.
    /// Anything else is plain text, even if it starts with `cmd[`.
    pub fn is_cmd(text: &str) -> bool {
        matches!(Self::parse(text), Template::Cmd(_))
    }

    /// Whether the text shows the clock, so it must redraw when the minute changes.
    pub fn uses_time(&self) -> bool {
        let s = match self {
            Template::Text(s) => s,
            Template::Cmd(c) => &c.shell,
        };
        s.contains("$TIME")
    }
}

/// Values for substitution.
#[derive(Debug, Default, Clone)]
pub struct Vars<'a> {
    pub user: &'a str,
    /// GECOS full name.
    pub desc: &'a str,
    /// Local time as (hour, minute).
    pub time: (u32, u32),
    pub attempts: u32,
    pub layout_index: u32,
    pub layout_name: &'a str,
    /// The most recent failure message from any auth source.
    pub fail: &'a str,
    pub pam_prompt: &'a str,
    pub pam_fail: &'a str,
    pub fprint_prompt: &'a str,
    pub fprint_fail: &'a str,
}

/// Substitute every `$VARIABLE` in `text`.
pub fn substitute(text: &str, v: &Vars<'_>) -> String {
    let (h, m) = v.time;
    let time24 = format!("{h:02}:{m:02}");
    let h12 = match h % 12 {
        0 => 12,
        x => x,
    };
    let time12 = format!("{h12:02}:{m:02} {}", if h < 12 { "AM" } else { "PM" });

    // Order matters where one name prefixes another ($TIME12 before $TIME, $FAIL last among
    // the *FAIL names is fine since those start with PAM/FPRINT).
    let mut out = text
        .replace("<br/>", "\n")
        .replace("$TIME12", &time12)
        .replace("$TIME", &time24);
    out = replace_attempts(&out, v.attempts);
    out = replace_layout(&out, v.layout_index, v.layout_name);
    for (name, value) in [
        ("$PAMPROMPT", v.pam_prompt),
        ("$PAMFAIL", v.pam_fail),
        ("$FPRINTPROMPT", v.fprint_prompt),
        ("$FPRINTFAIL", v.fprint_fail),
        ("$FAIL", v.fail),
        ("$USER", v.user),
        ("$DESC", v.desc),
    ] {
        out = out.replace(name, value);
    }
    out
}

/// `$ATTEMPTS` is the count; `$ATTEMPTS[text]` shows `text` instead of 0.
fn replace_attempts(s: &str, attempts: u32) -> String {
    replace_with_arg(s, "$ATTEMPTS", |arg| match arg {
        Some(zero) if attempts == 0 => zero.to_owned(),
        _ => attempts.to_string(),
    })
}

/// `$LAYOUT` is the xkb layout name; `$LAYOUT[us,de,!]` maps the layout index to a replacement,
/// where an empty entry keeps the name and `!` hides it.
fn replace_layout(s: &str, index: u32, name: &str) -> String {
    replace_with_arg(s, "$LAYOUT", |arg| match arg {
        None => name.to_owned(),
        Some(list) => match list.split(',').nth(index as usize) {
            Some("") | None => name.to_owned(),
            Some("!") => String::new(),
            Some(text) => text.to_owned(),
        },
    })
}

/// Replace `name` and `name[arg]` occurrences using `f`.
fn replace_with_arg(s: &str, name: &str, f: impl Fn(Option<&str>) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(name) {
        out.push_str(&rest[..i]);
        let after = &rest[i + name.len()..];
        match after.strip_prefix('[').and_then(|a| a.split_once(']')) {
            Some((arg, tail)) => {
                out.push_str(&f(Some(arg)));
                rest = tail;
            }
            None => {
                out.push_str(&f(None));
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The current local time as (hour, minute), honouring `TZ`.
pub fn local_time() -> (u32, u32) {
    // SAFETY: time/localtime_r with local storage.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return (0, 0);
        }
        (tm.tm_hour as u32, tm.tm_min as u32)
    }
}

/// The login name and GECOS name of the current user.
pub fn user_names() -> (String, String) {
    // SAFETY: getpwuid_r with a local buffer.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let mut buf = vec![0 as libc::c_char; 4096];
        let rc = libc::getpwuid_r(
            libc::getuid(),
            &mut pw,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() {
            return (String::new(), String::new());
        }
        let s = |p: *const libc::c_char| {
            if p.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        // GECOS is "Full Name,room,phone,..."; hyprlock shows the whole field, but the name is
        // what people want.
        let gecos = s(pw.pw_gecos);
        let desc = gecos.split(',').next().unwrap_or("").to_owned();
        (s(pw.pw_name), desc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars<'static> {
        Vars {
            user: "ada",
            desc: "Ada Lovelace",
            time: (9, 5),
            attempts: 0,
            layout_index: 1,
            layout_name: "German",
            fail: "Authentication failure",
            pam_prompt: "Password: ",
            ..Vars::default()
        }
    }

    /// hyprlock needs the closing `]`; without it the text is shown as is.
    #[test]
    fn cmd_needs_a_closing_bracket() {
        assert!(Template::is_cmd("cmd[] date"));
        assert!(Template::is_cmd("cmd[update:1000] date"));
        assert!(!Template::is_cmd("cmd[ HELLO"));
        assert!(!Template::is_cmd("cmd HELLO"));
    }

    #[test]
    fn plain_variables() {
        assert_eq!(
            substitute("Hi $USER ($DESC)", &vars()),
            "Hi ada (Ada Lovelace)"
        );
        assert_eq!(substitute("$TIME / $TIME12", &vars()), "09:05 / 09:05 AM");
        let pm = Vars {
            time: (0, 30),
            ..vars()
        };
        assert_eq!(substitute("$TIME12", &pm), "12:30 AM");
        let noon = Vars {
            time: (12, 0),
            ..vars()
        };
        assert_eq!(substitute("$TIME12", &noon), "12:00 PM");
        assert_eq!(
            substitute("<i>$FAIL</i>", &vars()),
            "<i>Authentication failure</i>"
        );
        assert_eq!(substitute("$PAMPROMPT", &vars()), "Password: ");
    }

    /// hyprlock turns `<br/>` into a newline before Pango sees it.
    #[test]
    fn br_is_a_newline() {
        assert_eq!(substitute("$USER<br/>$TIME", &vars()), "ada\n09:05");
    }

    #[test]
    fn attempts_with_zero_text() {
        assert_eq!(substitute("$ATTEMPTS", &vars()), "0");
        assert_eq!(substitute("[$ATTEMPTS[none yet]]", &vars()), "[none yet]");
        let three = Vars {
            attempts: 3,
            ..vars()
        };
        assert_eq!(substitute("$ATTEMPTS[none yet] tries", &three), "3 tries");
    }

    #[test]
    fn layout_mapping() {
        assert_eq!(substitute("$LAYOUT", &vars()), "German");
        assert_eq!(substitute("$LAYOUT[us,de]", &vars()), "de");
        assert_eq!(substitute("$LAYOUT[us,]", &vars()), "German");
        assert_eq!(substitute("[$LAYOUT[us,!]]", &vars()), "[]");
        assert_eq!(
            substitute("$LAYOUT[us]", &vars()),
            "German",
            "index past the list"
        );
    }

    #[test]
    fn cmd_prefix() {
        assert_eq!(Template::parse("hello"), Template::Text("hello".into()));
        assert_eq!(
            Template::parse("cmd[update:1000] date +%H:%M"),
            Template::Cmd(Cmd {
                shell: "date +%H:%M".into(),
                every: Some(Duration::from_secs(1)),
                on_signal: false,
            })
        );
        assert_eq!(
            Template::parse("cmd[update:500:1] echo $USER"),
            Template::Cmd(Cmd {
                shell: "echo $USER".into(),
                every: Some(Duration::from_millis(500)),
                on_signal: true,
            })
        );
        assert_eq!(
            Template::parse("cmd[] uptime"),
            Template::Cmd(Cmd {
                shell: "uptime".into(),
                every: None,
                on_signal: false
            })
        );
        assert!(Template::parse("$TIME").uses_time());
        assert!(!Template::parse("cmd[update:1000] date").uses_time());
    }

    /// `$USER` is the login name of the user running the locker.
    #[test]
    fn user_is_the_login_name() {
        let out = std::process::Command::new("id")
            .arg("-un")
            .output()
            .expect("id");
        let want = String::from_utf8(out.stdout).unwrap();
        assert_eq!(user_names().0, want.trim());
    }
}
