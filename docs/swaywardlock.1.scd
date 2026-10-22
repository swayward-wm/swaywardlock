swaywardlock(1)

# NAME

swaywardlock - screen locker for the swayward-wm family

# SYNOPSIS

*swaywardlock* [_OPTIONS_]++
*swaywardlock validate* [*-c* _PATH_]++
*swaywardlock completions* <_SHELL_>

# DESCRIPTION

swaywardlock locks a Wayland session with the ext-session-lock-v1 protocol, so it runs on any
compositor that implements it (swayward, sway, niri and others) and does not need $SWAYSOCK. It
keeps swaylock's process contract (*-f*, *--ready-fd*, SIGUSR1) and aims for hyprlock's feature
set: blurred screenshots, labels, images, shapes, an input field, gradients, animations and
fingerprint unlock. It is configured in KDL; see *CONFIGURATION* below, and the examples under
*SEE ALSO*. Background and image files are
shown upright: their EXIF orientation is applied, as swaylock does.

Passwords are checked through PAM with the service _swaywardlock_. swaywardlock refuses to lock
(exit status 1) when the PAM service file is missing.

The session stays locked if swaywardlock dies for any reason: the compositor, not the locker,
holds the lock.

# OPTIONS

*-c*, *--config* _CONFIG_
	Config file. Without it the search path under *FILES* applies. A file named explicitly,
	here or in *SWAYWARDLOCK_CONFIG*, that does not exist is an error. The flag wins over the
	environment variable.

*-f*, *--daemonize*
	Fork into the background once the session is locked. The process that was started exits
	with status 0 only after the session is locked, so a caller can wait for it.

*--ready-fd* _FD_
	Write a newline to this file descriptor once the session is locked, then close it. The
	descriptor must be open when swaywardlock starts, or it exits with status 1.

*--no-fade-in*
	Skip the fade-in animation.

*-g*, *--grace* _SECONDS_
	Within this many seconds after the session is locked, any key, pointer button or pointer
	motion unlocks without a password. The default is 0 (no grace). Time spent suspended
	counts. If the clock cannot be read there is no grace.

*--immediate-render*
	Lock without waiting for screenshots or decoding images: backgrounds that need one, and
	image widgets, fall back to their colour.

*--display* _NAME_
	Wayland display to connect to. The default is $WAYLAND_DISPLAY.

*--preview*
	Draw the lock screen in an ordinary window instead of locking, and reload the config
	whenever it or a file it includes changes. Nothing is locked and no password is checked:
	typing drives the indicator states only. Close the window or press Ctrl+C to quit. It
	cannot be combined with *-f* or *--ready-fd*.

*--preview-output* _NAME_
	With *--preview*, the output name the window pretends to be, for widgets that match on
	*output=*.

*-v*, *--verbose*
	Verbose logging. It cannot be combined with *-q*.

*-q*, *--quiet*
	Log errors only.

*-h*, *--help*
	Print help and exit.

*-V*, *--version*
	Print the version and exit.

# COMMANDS

*validate*
	Parse the config (and the files it includes), print any diagnostics and exit. It does not
	connect to Wayland. Exit status 0 if the config is valid, 1 if not.

*completions* _SHELL_
	Print a shell completion script to standard output. _SHELL_ is one of *bash*, *zsh*,
	*fish*, *elvish* or *powershell*. It does not connect to Wayland and does not read the
	config, so it works with a broken one.

# KEYS

*Enter*, *Ctrl+J*, *Ctrl+M*, *Ctrl+D*
	Submit the password. The Ctrl keys are swaylock's; Ctrl+D submits at once, it does not
	delete a character.

*Backspace*, *Delete*
	Remove the last character. They repeat while held; no other key does.

*Escape*, *Ctrl+U*, *Ctrl+C*, *Ctrl+A*, *Ctrl+Backspace*, *Ctrl+Delete*
	Clear the password (Ctrl+C and Ctrl+U are swaylock's, Ctrl+A is hyprlock's).

Any other key with text appends it. Keys typed while a password is being checked are dropped.

# CONFIGURATION

The config is a KDL file (see *FILES* for where it is looked for). Without one a built-in
default applies: a swaylock-style look, a plain background and a ring indicator. To start,
copy an example (listed under *SEE ALSO*) to _~/.config/swaywardlock/config.kdl_, or translate
an existing swaylock or hyprlock config with *swaywardlock-swaylock-to-kdl* or
*swaywardlock-hyprlock-to-kdl*, then check it with *swaywardlock validate*.

In KDL, a node written on one line needs a *;* (or a newline) before the closing brace:
_auth { pam { off; }; }_ is valid, _auth { pam { off } }_ is not. Comments are _//_ to the
end of a line and _/\* ... \*/_.

```
general {
    hide-cursor
    fail-timeout-ms 2000
}
auth {
    pam service="swaywardlock"
    fingerprint { }
}
animations {
    fade-in { duration-ms 200; curve "ease-out-cubic"; }
}
background {
    color "#111111"
    screenshot
    blur { passes 3; offset 3.0; }
}
label {
    text "$TIME"
    font "Sans 48"
    color "#ffffff"
    position 0 -80
    valign "top"
}
ring-indicator {
    radius 60
    idle-visible
}
```

Top-level nodes:

*general*
	*hide-cursor*, *ignore-empty-input*, *fail-timeout-ms* _MS_, *clear-input-after-ms* _MS_.

*auth*
	*pam service=*_NAME_ (or *pam { off; }* to disable PAM; *reuse-password* answers a repeated
	question with the last answer), and *fingerprint { }*, with *ready-message*,
	*present-message* and *retry-delay-ms*. Without a *fingerprint* node there is no
	fingerprint authentication.

*animations*
	*off*, or any of *fade-in*, *fade-out*, *input-field-fade*, *input-field-width*,
	*input-field-colors*, *input-field-dots* and (ring style *swayward*) *ring-colors*,
	*ring-highlight*, *ring-appear*, *ring-wrong*. Each is _{ duration-ms N; curve "NAME"; }_,
	a *cubic-bezier* curve, a *spring*, or *off*. *fade-in* and *fade-out* take at most 3 s.

*include* _"PATH"_
	Read another file here; *include optional=true* _"PATH"_ ignores a missing one.

Widgets, in any order and any number of each. Every widget takes *output=*_"NAME"_ (an output
name, or *desc:* and a description prefix), *position* _X Y_ (logical pixels or percentages;
positive Y is up), *halign*, *valign*, *z-index* and, on *image*, *shape* and *input-field*, a
*shadow* node.

*background*
	*color*, *screenshot* or *path* _"FILE"_, *fit* (_cover_, _contain_, _fill_, _tile_ or
	_center_), *blur { passes N; offset PX; noise N; saturation N; }*, *contrast*, *brightness*.
	The last matching one wins per output.

*label*
	*text* (with substitutions such as _$TIME_, _$USER_, _$FAIL_, _$ATTEMPTS_ and
	_cmd[update:MS] COMMAND_), *font*, *color*, *align*, and hyprlock's text shadow
	*text-shadow { passes N; size PX; color "C"; boost N; offset x=PX y=PX; }* (passes 0 to 4,
	required, 0 is off; size 0 to 20).

*image*
	*path*, *size*, *corner-radius*, *border { width N; color "C"; }*.

*shape*
	*size*, *color*, *corner-radius*, *border*, *xray*.

*input-field*
	hyprlock's password field: *size*, *corner-radius*, *inner-color*, *outline*, *font*,
	*font-color*, *placeholder-text*, *dots*, *hide-input*, *fade-on-empty*, *check*, *fail*,
	*capslock-color*, *numlock-color*, *bothlock-color*, *invert-numlock*, *swap-font-color*.

*ring-indicator*
	swaylock's indicator: *radius*, *thickness*, *font*, *font-size*, *idle-visible*,
	*show-failed-attempts*, *show-keyboard-layout*, *show-caps-lock-text*,
	*show-caps-lock-indicator*, *line-uses*, *style* (_swaylock_ or the animated _swayward_,
	which also takes *glow*) and a *colors* node with one *state* line per state (_input_,
	_clear_, _verifying_, _wrong_, _caps-lock_).

Colours are any CSS colour string (_"#rrggbbaa"_, _"rgb(...)"_, names); a gradient is
_gradient from="C" to="C" angle=N_ with CSS angles (0 points up). Animations, blur, shadow and
colours use the same syntax as swayward.

The full reference, with every node's defaults, is the design document
_specs/2026-10-19-swaywardlock.md_ under /usr/share/doc/swaywardlock (see *SEE ALSO*).

# SIGNALS

*SIGUSR1*
	Unlock: fade out, then unlock. A second SIGUSR1, or a password or fingerprint match, while
	the fade-out runs unlocks at once. A SIGUSR1 that arrives before the compositor confirms
	the lock is held until then. Wait for *--ready-fd*, or for *-f* to return, before sending it.

*SIGUSR2*
	Re-run the labels whose command is marked to refresh on this signal, *cmd[update:MS:1]*,
	and reload the picture of every *background* and *image* with *reload-ms* or *reload-cmd*.
	A picture that fails to load or to draw keeps the old one.

*SIGTERM*, *SIGINT*, *SIGHUP*
	Keep their default action: the process ends and the compositor keeps the session locked.
	They never unlock. The commands started by *cmd[]* labels are killed with it.

# EXIT STATUS

*0*
	Unlocked after authentication, grace, or SIGUSR1.

*1*
	Never locked: a config error, a missing PAM service file, no Wayland display to connect to,
	no ext-session-lock-v1 in the compositor, a renderer setup failure, a *--ready-fd* that is not open, a *--grace* too
	large for the clock, or too low an open-file limit.

*2*
	The compositor sent _finished_: another locker holds the lock, or the lock was refused. It
	is also the status for a command-line error.

*3*
	An error after the lock was taken (the compositor died or sent a protocol error, an
	event-loop error). The compositor keeps the session locked.

# FILES

_$XDG_CONFIG_HOME/swaywardlock/config.kdl_
	The user config. _~/.config/swaywardlock/config.kdl_ when XDG_CONFIG_HOME is unset.

_/etc/swaywardlock/config.kdl_
	The system config, used when the user config does not exist. With neither, a built-in
	default applies.

_/etc/pam.d/swaywardlock_
	The PAM service file, _auth include login_. The release packages (.deb, .rpm, Arch) install
	it and mark it as a config file, so a local edit survives an upgrade. Without it
	swaywardlock refuses to lock.

# ENVIRONMENT

*SWAYWARDLOCK_CONFIG*
	Config file, like *--config*. An empty value is a command-line error (the message names
	this variable: unset it or give a path).

*RUST_LOG*
	Log filter, in the tracing-subscriber syntax. It replaces the level chosen with *-v* and *-q*.

*WAYLAND_DISPLAY*, *XDG_RUNTIME_DIR*
	The display to connect to unless *--display* is given, and the directory its socket is in.
	Without a reachable display swaywardlock exits with status 1 and says so; run it inside a
	Wayland session, or pass *--display*.

# SEE ALSO

*swaylock*(1), *hyprlock*(1)

Packages for Ubuntu 24.04, Fedora and Arch are on
_https://github.com/swayward-wm/swaywardlock/releases_.

Under /usr/share/doc/swaywardlock: _specs/2026-10-19-swaywardlock.md_, the design document and
config reference; _examples/hyprlock-example.kdl_, hyprlock's sample config in swaywardlock KDL;
_examples/swayward-ring.kdl_, the animated ring (*ring-indicator { style "swayward"; }*).

*swaywardlock-swaylock-to-kdl* [_CONFIG_] (it can also translate a swaylock command line; see its
help) and *swaywardlock-hyprlock-to-kdl* [_CONFIG_] print a swaywardlock config on standard output, and
on standard error the command line to use and a warning for every option they could not carry
over. For example:

```
swaywardlock-hyprlock-to-kdl ~/.config/hypr/hyprlock.conf > ~/.config/swaywardlock/config.kdl
swaywardlock validate
```

With swayidle: _swayidle -w timeout 300 'swaywardlock -f' before-sleep 'swaywardlock -f'_.
