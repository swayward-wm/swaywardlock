"""Ports of the ten auth/input e2e cases (75 named checks plus setup/negative guards).

These need the compositor: PAM prompt redraws, seat/capability changes, grace clocks and
fprintd sender ownership cannot be proved by the unit auth tests. Temporal/negative reads
stay single samples; only positive readiness/end-state checks use settled polling.
"""

import contextlib
import re
import signal
import subprocess
import time
import uuid
from pathlib import Path

from lib import IDLE_RING, PASSWORD, RING, WRONG_RING
from lib.poll import poll
from lib.screen import parse_ppm

OFF = 'animations { off; }\n'
LAYOUT = 'background { color "#000000"; }\n' + OFF + (
    'label { text "$LAYOUT"; font "Sans 20"; color "#ff0000"; }\n'
)
BOX = (0, 0, 320, 240)


def _log(name):
    # spawn appends: even a repeated helper/locker must never see an earlier ready/verdict.
    return f"{name}-{uuid.uuid4().hex}.log"


def _locker(lock_env, name, config, **kw):
    locker = lock_env.locker(config, log=_log(name), **kw)
    locker.wait_locked(name)
    return locker


def _unlock(locker):
    locker.signal(signal.SIGUSR1)
    assert locker.wait_exit() == 0, f"{locker.log}: SIGUSR1 cleanup exit\n{locker.log_text()}"


def _stop(proc):
    if proc.poll() is None:
        proc.terminate()
    proc.wait(5)


@contextlib.contextmanager
def _keyboard(env, name, *args):
    log = _log(name)
    with open(env.path(log), "wb") as out:
        proc = subprocess.Popen(
            [str(env.contrib("vkbd")), *args, "stdin"], env=env.env,
            stdin=subprocess.PIPE, stdout=out, stderr=subprocess.STDOUT,
            start_new_session=True, text=True,
        )
    env.procs.append(proc)

    def send(*commands):
        proc.stdin.write("\n".join(commands) + "\n")
        proc.stdin.flush()

    send.proc = proc
    try:
        assert poll(lambda: "ready" in env.log_text(log)), f"vkbd {name} did not start\n{env.log_text(log)}"
        yield send
    finally:
        try:
            if proc.poll() is None:
                send("quit")
        except BrokenPipeError:
            pass
        finally:
            with contextlib.suppress(BrokenPipeError):
                proc.stdin.close()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            _stop(proc)
    assert proc.returncode == 0, f"vkbd {name} failed\n{env.log_text(log)}"


def _tool(env, name, *args, child_env=None):
    log = _log(name)
    proc = env.spawn([env.contrib(name), *args], log=log, env=child_env)
    assert proc.wait(10) == 0, f"{name} did not run\n{env.log_text(log)}"


def _width(env, colour=(255, 0, 0), box=BOX):
    return env.snapshot().colour_width(colour, box)


def _wakeups(pid):
    def switches():
        return int(re.search(r"^voluntary_ctxt_switches:\s+(\d+)",
                             Path(f"/proc/{pid}/status").read_text(), re.M)[1])
    before = switches()
    # Both full 2 s windows are required to expose a surviving 25 Hz repeat timer.
    time.sleep(2)
    return switches() - before


def test_keyboard(lock_env):
    env = lock_env
    with _keyboard(env, "vk-layout", "--layout", "us,de") as send:
        locker = _locker(env, "keyboard", LAYOUT, env={"RUST_LOG": "swaywardlock=debug"})
        env.expect_width("keyboard: $LAYOUT drawn without a key press", (255, 0, 0), BOX)
        send("type aaaaaaaaaa", "hold BackSpace 2000", f"type {PASSWORD[:-1]}",
             "hold t 1500", "key Return")
        assert locker.wait_exit(10) == 0, "keyboard: held BackSpace clears, held letter types once, password unlocks"

    env.swaymsg("seat", "seat1", "fallback", "false")
    with _keyboard(env, "vk-seat0", "--seat", "seat0") as send0:
        locker = _locker(env, "keyboard seats", LAYOUT)
        _tool(env, "vkbd", "--seat", "seat1", "sleep 500")
        time.sleep(0.5)
        send0(f"type {PASSWORD}", "key Return")
        assert locker.wait_exit() == 0, "keyboard: seat0 still types after a seat1 keyboard came and went"
        locker = _locker(env, "keyboard seats 2", LAYOUT)
        with _keyboard(env, "vk-seat1", "--seat", "seat1") as send1:
            send0("quit")
            assert send0.proc.wait(5) == 0, "keyboard: seat0 did not leave"
            time.sleep(0.5)
            send1(f"type {PASSWORD}", "key Return")
            assert locker.wait_exit() == 0, "keyboard: seat1 keyboard unlocks after seat0's keyboard is gone"


def test_modifiers(lock_env):
    env = lock_env
    config = 'background { color "#000000"; }\n' + OFF + (
        'input-field { size 200 50; inner-color "#ffffff"; outline { width 4; color "#0000ff"; }; '
        'capslock-color "#ff00ff"; fade-on-empty false; }\n'
    )
    with _keyboard(env, "vk-mods") as send:
        locker = _locker(env, "modifiers", config)
        env.expect_colour("modifiers: outline without Caps Lock", (0, 0, 255), 160, 92)
        send("key Caps_Lock", "mods 0 0 2")
        env.expect_colour("modifiers: Caps Lock gives the field its capslock colour", (255, 0, 255), 160, 92)
        send("key Caps_Lock", "mods 0 0 0")
        env.expect_colour("modifiers: Caps Lock off restores the outline", (0, 0, 255), 160, 92)

        def ctrl(key):
            send("down Control_L", "mods 4 0 0", f"key {key}", "up Control_L", "mods 0 0 0")

        send("type abcdef")
        ctrl("u")
        send("type xyz")
        ctrl("BackSpace")
        send(f"type {PASSWORD}")
        ctrl("j")
        assert locker.wait_exit() == 0, "modifiers: Ctrl+U and Ctrl+BackSpace clear, Ctrl+J submits"


def test_keyboard_layout(lock_env):
    env = lock_env
    env.swaymsg("seat", "seat1", "fallback", "false")
    with _keyboard(env, "vk-de", "--seat", "seat0", "--layout", "de,us") as send0:
        locker = _locker(env, "keyboard layout", LAYOUT)
        send0("key Shift_L")
        de = env.expect_width("keyboard layout: initial de layout", (255, 0, 0), BOX)
        with _keyboard(env, "vk-us", "--seat", "seat1", "--layout", "us") as send1:
            # Keymap arrival must NOT take over: preserve the negative observation window.
            time.sleep(0.5)
            assert _width(env) == de, "keyboard layout: a keyboard that only arrived is not shown"
            send1("key Shift_L")
            us = env.expect_width("keyboard layout: the keyboard that spoke last is shown",
                                  (255, 0, 0), BOX, lambda w: w > 0 and w != de)
            send0("key Shift_L")
            env.expect_width("keyboard layout: back to seat0's layout after a key there", (255, 0, 0), BOX, de)
            send1("mods 0 0 2")
            env.expect_width("keyboard layout: a modifiers change makes its keyboard the one shown", (255, 0, 0), BOX, us)
            send1("mods 0 0 0")
            send0("key Shift_L")
            env.expect_width("keyboard layout: seat0 before departure", (255, 0, 0), BOX, de)
            send1("key Shift_L")
            env.expect_width("keyboard layout: seat1 before departure", (255, 0, 0), BOX, us)
        env.expect_width("keyboard layout: the remaining keyboard's layout returns when the other goes", (255, 0, 0), BOX, de)
        _unlock(locker)


def test_keyboard_repeat(lock_env):
    env = lock_env
    env.swaymsg("seat", "seat1", "fallback", "false")
    proxy_log = _log("droprel")
    proxy = env.spawn([env.contrib("wl-drop-key-release")], log=proxy_log)
    assert poll(lambda: env.path("droprel-ready").exists()), f"keyboard repeat: proxy not ready\n{env.log_text(proxy_log)}"
    config = 'background { color "#000000"; }\n' + OFF + (
        'input-field { size 200 50; inner-color "#ffffff"; font-color "#000000"; '
        'outline { width 4; color "#0000ff"; }; dots size=0.4 spacing=0.5; fade-on-empty false; }\n'
    )
    with _keyboard(env, "vk-seat0", "--seat", "seat0") as send:
        locker = _locker(env, "keyboard repeat", config,
                         env={"WAYLAND_DISPLAY": "wayland-droprel-0", "RUST_LOG": "swaywardlock=debug"})
        send("type abcdefghijkl")
        # Exclude the known non-repeat burst ~1.75 s after typing, just as the bash case.
        time.sleep(2.5)
        base = _wakeups(locker.pid)
        assert base < 10, f"keyboard repeat: idle locker ({base} wakeups in 2 s)"
        _tool(env, "vkbd", "--seat", "seat1", "hold BackSpace 1500")
        time.sleep(0.5)
        w1 = _width(env, (0, 0, 0), (62, 112, 258, 128))
        woke = _wakeups(locker.pid)
        w2 = _width(env, (0, 0, 0), (62, 112, 258, 128))
        assert w2 == w1, "keyboard repeat: the dots stop changing once the keyboard is gone"
        assert woke < 10, f"keyboard repeat: no repeat timer left running ({woke} wakeups in 2 s)"
        assert "seat capability removed" in locker.log_text(), "keyboard repeat: no capability removal seen"
        send("key Escape", f"type {PASSWORD}", "key Return")
        assert locker.wait_exit() == 0, "keyboard repeat: seat0 still unlocks"
    _stop(proxy)


def test_pam_off(desktop):
    env = desktop
    locker = _locker(env, "pam off", 'background { color "#204060"; }\n'
                     'ring-indicator { idle-visible; }\n' + OFF + 'auth { pam { off; }; }\n',
                     env={"RUST_LOG": "swaywardlock=debug"})
    assert "PAM is disabled" in locker.log_text(), "pam off: the log says PAM is disabled"
    env.wtype("-k", "Return")
    time.sleep(0.5)
    locker.type(PASSWORD)
    time.sleep(0.5)
    assert locker.alive(), "pam off: Enter and the test password keep the lock"
    env.expect_colour("pam off: Enter is a failed attempt", WRONG_RING, *RING)
    assert env.snapshot().pixel(5, 5) == (32, 64, 96), "pam off: the desktop stays covered"
    locker.signal(signal.SIGUSR1)
    assert locker.wait_exit() == 0, "pam off: SIGUSR1 unlocks"
    env.expect_unlocked("pam off")


def test_pam_service(lock_env):
    env = lock_env
    directory = env.path("pam.d")
    directory.mkdir()
    child_env = {"SWAYWARDLOCK_TEST_PAM_DIR": str(directory)}
    config = 'auth { pam service="swl-missing"; }\n'
    locker = env.locker(config, env=child_env, log=_log("pam-missing"))
    assert locker.wait_exit() == 1, "pam service: missing file exits 1"
    assert "session locked" not in locker.log_text(), "pam service: never locked without a PAM service file"
    assert str(directory / "swl-missing") in locker.log_text(), "pam service: the error names the file"
    (directory / "swl-missing").touch()
    locker = _locker(env, "pam service: locks when the file exists", config, env=child_env)
    _unlock(locker)
    (directory / "swl-missing").unlink()
    locker = _locker(env, "pam service: pam { off; } locks without a file", 'auth { pam { off; }; }\n', env=child_env)
    _unlock(locker)


def test_otp(desktop):
    env = desktop
    config = 'background { color "#204060"; }\nring-indicator { idle-visible; }\n' + OFF + (
        'label { text "[$PAMPROMPT]"; font "Sans 20"; color "#ffff00"; valign "top"; position 0 -10; }\n'
    )
    locker = _locker(env, "otp", config, env={"RUST_LOG": "swaywardlock=debug"})
    prompt_pw = env.expect_width("otp: initial password prompt", (255, 255, 0), (0, 0, 320, 60))
    with _keyboard(env, "vk-otp") as send:
        send("type swaywardlock-2fa", "key Return")
        time.sleep(0.8)
        assert "PAM prompt: OTP: " in locker.log_text(), "otp: the second question reaches the UI"
        prompt_otp = _width(env, (255, 255, 0), (0, 0, 320, 60))
        assert 0 < prompt_otp < prompt_pw, f"otp: $PAMPROMPT redrawn with the code question ({prompt_pw} -> {prompt_otp} px)"
        assert locker.alive(), "otp: the password alone does not unlock"
        assert env.snapshot().pixel(*RING) == IDLE_RING, "otp: waiting for the code, not failed"
        locker.type("000000")
        time.sleep(0.8)
        assert locker.alive(), "otp: a wrong code keeps the lock"
        assert "authentication failed" in locker.log_text(), "otp: a wrong code is a failed attempt"
        time.sleep(3)
        locker.type("swaywardlock-2fa")
        time.sleep(0.8)
        locker.type("123456")
        assert locker.wait_exit() == 0, "otp: password then code unlocks"
    env.expect_unlocked("otp")


def test_grace(desktop):
    env = desktop
    locker = _locker(env, "grace", OFF, args=("--grace", "3"))
    env.wtype("x")
    assert locker.wait_exit() == 0, "grace: a key within the grace period unlocks"
    env.expect_unlocked("grace")
    fade = 'animations { fade-in { off; }; fade-out { duration-ms 3000; curve "linear"; }; }\n'
    locker = _locker(env, "grace fade", fade, args=("--grace", "5"))
    env.wtype("x")
    env.wtype("x")
    time.sleep(0.2)
    assert locker.alive(), "grace: a second key keeps the fade-out running"
    assert locker.wait_exit() == 0, "grace: the fade-out ends in the unlock"
    locker = _locker(env, "grace expired", OFF, args=("--grace", "1"))
    time.sleep(1.5)
    env.wtype("x")
    time.sleep(0.5)
    assert locker.alive(), "grace: after the grace period a key does not unlock"
    _unlock(locker)

    shim = env.path("clock-jump.so")
    result = env.run(["cc", "-shared", "-fPIC", "-o", shim, env.contrib("clock-jump.c"), "-ldl"], check=False)
    assert result.returncode == 0, f"grace: cannot build the clock shim\n{result.stderr.decode()}"
    locker = _locker(env, "grace suspend", OFF, args=("--grace", "30"),
                     env={"LD_PRELOAD": str(shim), "CLOCK_JUMP_TRIGGER": str(env.path("jump"))})
    env.path("jump").touch()
    env.wtype("x")
    time.sleep(0.5)
    assert locker.alive(), "grace: a suspend inside the grace period ends it"
    _unlock(locker)
    locker = _locker(env, "grace failing-clock", OFF, args=("--grace", "1"),
                     env={"LD_PRELOAD": str(shim), "CLOCK_BOOTTIME_FAIL": "1"})
    time.sleep(1.5)
    env.wtype("x")
    time.sleep(0.5)
    assert locker.alive(), "grace: a failing boot clock closes the grace period"
    _unlock(locker)
    locker = env.locker(OFF, args=("--grace", "18446744073709551615"), log=_log("grace-overflow"))
    assert locker.wait_exit() == 1, "grace: an overflowing value exits 1"
    assert "session locked" not in locker.log_text(), "grace: an overflowing value is refused before locking"


def _cursor_pixels(env):
    shot = parse_ppm(env.run(["grim", "-c", "-t", "ppm", "-"]).stdout)
    return sum(shot.pixel(x, y) != (32, 64, 96) for y in range(240) for x in range(320))


def test_pointer(desktop):
    env = desktop
    config = 'background { color "#204060"; }\n' + OFF
    for how in (("move", "5", "5"), ("click",)):
        locker = _locker(env, "pointer grace", config, args=("--grace", "30"))
        _tool(env, "vptr", "move", "5", "5", "sleep", "300", *how)
        assert locker.wait_exit() == 0, f"pointer: {' '.join(how)} within the grace period unlocks"
        env.expect_unlocked(f"pointer {' '.join(how)}")
    locker = _locker(env, "pointer late", config, args=("--grace", "1"))
    time.sleep(1.5)
    _tool(env, "vptr", "move", "5", "5", "click")
    time.sleep(0.5)
    assert locker.alive(), "pointer: after the grace period the pointer does not unlock"
    env.expect_colour("pointer: background ready", (32, 64, 96), 5, 5)
    pointer = env.spawn([env.contrib("vptr"), "move", "10", "10", "sleep", "500"], log=_log("vptr-hidden"))
    time.sleep(0.3)
    assert _cursor_pixels(env) == 0, "pointer: hide-cursor, no cursor drawn"
    assert pointer.wait(5) == 0, "pointer: hidden cursor helper failed"
    _unlock(locker)
    locker = _locker(env, "pointer cursor", config + 'general { hide-cursor false; }\n')
    env.expect_colour("pointer: cursor background ready", (32, 64, 96), 5, 5)
    pointer = env.spawn([env.contrib("vptr"), "move", "10", "10", "sleep", "500"], log=_log("vptr-visible"))
    time.sleep(0.3)
    n = _cursor_pixels(env)
    assert n > 20, f"pointer: hide-cursor false, the cursor is drawn ({n} px)"
    assert pointer.wait(5) == 0, "pointer: visible cursor helper failed"
    _unlock(locker)


@contextlib.contextmanager
def _fake(env, child_env, name, *statuses):
    log = _log(f"fprintd-{name}")
    proc = env.spawn([env.contrib("fake-fprintd"), *statuses], log=log, env=child_env)
    try:
        assert poll(lambda: "ready" in env.log_text(log)), f"fingerprint: fake {name} not ready\n{env.log_text(log)}"
        yield lambda: env.log_text(log)
    finally:
        _stop(proc)


def _ordered(text, *markers):
    start = 0
    for marker in markers:
        found = text.find(marker, start)
        if found < 0:
            return False
        start = found + len(marker)
    return True


def _read(path):
    return path.read_text() if path.exists() else ""


def test_fingerprint(desktop):
    env = desktop
    busdir = env.path("bus")
    busdir.mkdir()
    address = f"unix:path={busdir / 'sock'}"
    buslog = _log("dbus")
    bus = env.spawn(["dbus-daemon", "--session", "--nofork", f"--address={address}", "--print-address"], log=buslog)
    assert poll(lambda: (busdir / "sock").is_socket()), f"fingerprint: bus not ready\n{env.log_text(buslog)}"
    child_env = {"DBUS_SYSTEM_BUS_ADDRESS": address}
    config = 'background { color "#000000"; }\n' + OFF + (
        'auth { fingerprint { retry-delay-ms 100; }; }\n'
        'label { text "$FPRINTPROMPT"; font "Sans 12"; color "#ffffff"; }\n'
    )
    with _fake(env, child_env, "match", "verify-finger-not-centered", "verify-no-match", "verify-match") as log:
        locker = _locker(env, "fingerprint", config, env=child_env)
        assert locker.wait_exit() == 0, "fingerprint: a match unlocks"
        env.expect_unlocked("fingerprint")
        # Check production Release BEFORE stopping the fake or fixture cleanup can hide it.
        assert poll(lambda: "Release" in log()), f"fingerprint: device released on exit\n{log()}"
        assert _ordered(log(), "VerifyStart any", "status verify-no-match", "VerifyStart any", "status verify-match"), f"fingerprint: retried after a non-match\n{log()}"
        assert "REFUSED" not in log() and _ordered(log(), "Claim", "VerifyStart any"), f"fingerprint: Claim before VerifyStart\n{log()}"
        tail = log().rsplit("status verify-match", 1)[1]
        assert re.fullmatch(r"\s+\d+ VerifyStop\s+\d+ Release\s*", tail), f"fingerprint: VerifyStop, then Release after the match\n{tail}"

    present = 'background { color "#000000"; }\n' + OFF + (
        'auth { fingerprint { ready-message "r"; present-message "present-present-present"; }; }\n'
        'label { text "$FPRINTPROMPT"; font "Sans 16"; color "#ffffff"; }\n'
    )
    with _fake(env, child_env, "present", "pause", "finger-present") as log:
        locker = _locker(env, "fingerprint present", present, env=child_env)
        assert poll(lambda: "VerifyStart" in log()), f"fingerprint: present scan not started\n{log()}"
        ready = env.expect_width("fingerprint: ready prompt drawn", (255, 255, 255), BOX)
        assert poll(lambda: "finger-present" in log(), timeout=4), f"fingerprint: no finger-present\n{log()}"
        env.expect_width("fingerprint: the present prompt is drawn when a finger touches",
                         (255, 255, 255), BOX, lambda w: w > ready + 50)
        _unlock(locker)

    prompts = env.path("prompts")
    prompt_config = OFF + 'auth { fingerprint { ready-message "ready-msg"; present-message "present-msg"; retry-delay-ms 1200; }; }\n' + (
        f'label {{ text "cmd[update:100] echo \\"$FPRINTPROMPT\\" >> {prompts}"; }}\n'
    )
    with _fake(env, child_env, "prompts", "finger-present", "verify-finger-not-centered", "verify-no-match", "verify-match") as log:
        locker = _locker(env, "fingerprint prompt", prompt_config, env=child_env)
        assert locker.wait_exit(10) == 0, "fingerprint: prompt run unlocks on the match"
        seen = _read(prompts)
        assert _ordered(seen, "ready-msg", "present-msg", "Finger not centered", "Could not match"), f"fingerprint: ready, present and retry prompts shown in order\n{seen}"
        match = re.search(r"(\d+) status verify-no-match", log())
        assert match, f"fingerprint: retry-delay-ms honoured (no non-match)\n{log()}"
        nomatch = int(match[1])
        restarts = [int(t) for t in re.findall(r"(\d+) VerifyStart\b", log()) if int(t) > nomatch]
        assert restarts and restarts[0] - nomatch >= 1100, f"fingerprint: retry-delay-ms honoured\n{log()}"

    with _fake(env, child_env, "forgery") as log:
        locker = _locker(env, "fingerprint forgery", config, env=child_env)
        assert poll(lambda: "VerifyStart" in log()), f"fingerprint: forgery scan not started\n{log()}"
        _tool(env, "forge-fprint", "broadcast", child_env=child_env)
        _tool(env, "forge-fprint", "directed", child_env=child_env)
        time.sleep(1.5)
        assert locker.alive(), "fingerprint: forged VerifyStatus does not unlock"
        _tool(env, "forge-fprint", "directed", "sleep", child_env=child_env)
        time.sleep(0.5)
        assert "VerifyStop" not in log(), f"fingerprint: forged PrepareForSleep ignored\n{log()}"
        _unlock(locker)

    sleep_config = OFF + 'auth { fingerprint { retry-delay-ms 1000; }; }\n'
    with _fake(env, child_env, "sleep", "verify-no-match+sleep") as log:
        locker = _locker(env, "fingerprint sleep", sleep_config, env=child_env)
        assert poll(lambda: "WAKE" in log(), timeout=6), f"fingerprint: no wake\n{log()}"
        # Full post-wake negative window: do not poll away a duplicate VerifyStart.
        time.sleep(0.5)
        assert _ordered(log(), "SLEEP", "WAKE"), f"fingerprint: unexpected sleep log\n{log()}"
        sleeping = log().split("SLEEP", 1)[1].split("WAKE", 1)[0]
        waking = log().split("WAKE", 1)[1]
        assert "VerifyStart" not in sleeping and "VerifyStart" in waking, f"fingerprint: no scan while asleep, rescans on wake\n{log()}"
        assert waking.count("VerifyStart") == 1, f"fingerprint: scan started twice after wake\n{log()}"
        _unlock(locker)
    with _fake(env, child_env, "quicksleep", "verify-no-match+quicksleep", "verify-match") as log:
        locker = _locker(env, "fingerprint quick sleep", sleep_config, env=child_env)
        assert locker.wait_exit() == 0, "fingerprint: sleep inside the retry delay, then a match unlocks"
        assert "REFUSED" not in log(), f"fingerprint: one VerifyStart after a quick wake\n{log()}"

    variables = env.path("fpvars")
    vanish = OFF + 'auth { fingerprint { ready-message "scan"; }; }\n' + (
        f'label {{ text "cmd[update:200] echo \\"$FPRINTPROMPT|$FPRINTFAIL\\" > {variables}"; }}\n'
    )
    with _fake(env, child_env, "vanish"):
        locker = _locker(env, "fingerprint vanish", vanish,
                         env=dict(child_env, RUST_LOG="swaywardlock=debug"))
        assert poll(lambda: _read(variables).strip() == "scan|"), "fingerprint: ready prompt before fprintd goes"
    assert poll(lambda: _read(variables).strip() == "|Fingerprint unavailable"), "fingerprint: fprintd gone clears the prompt and sets $FPRINTFAIL"
    assert locker.wait_log("fprintd went away"), "fingerprint: fprintd gone is logged"
    locker.type(PASSWORD)
    assert locker.wait_exit() == 0, "fingerprint: password unlocks after fprintd went away"

    attempts = env.path("attempts")
    misses = OFF + 'auth { fingerprint { retry-delay-ms 100; }; }\n' + (
        f'label {{ text "cmd[update:200] echo $ATTEMPTS > {attempts}"; }}\n'
    )
    with _fake(env, child_env, "misses", *(["verify-no-match"] * 4), "verify-match") as log:
        locker = _locker(env, "fingerprint four misses", misses, env=child_env)
        # Wait long enough that an erroneous fifth scan/match would unlock.
        time.sleep(3.5)
        assert locker.alive(), "fingerprint: gives up after four non-matches"
        assert _read(attempts).strip() == "4", "fingerprint: misses count as failed attempts"
        assert log().count("VerifyStart any") == 4 and log().count("status verify-no-match") == 4, f"fingerprint: four scans then give up\n{log()}"
        locker.type(PASSWORD)
        assert locker.wait_exit() == 0, "fingerprint: password still unlocks"
    locker = _locker(env, "fingerprint: locks without fprintd", misses, env=child_env)
    _unlock(locker)
    _stop(bus)
