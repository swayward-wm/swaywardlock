"""Ports of the ten auth/input e2e cases (75 named checks plus setup/negative guards).

These need the compositor: PAM prompt redraws, seat/capability changes, grace clocks and
fprintd sender ownership cannot be proved by the unit auth tests. Temporal/negative reads
stay single samples; only positive readiness/end-state checks use settled polling.
"""

import contextlib
import os
import re
import signal
import subprocess
import time
import uuid
from pathlib import Path

import pytest

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

    submitted = 0

    def send(*commands):
        nonlocal submitted
        submitted += sum(bool(line.strip()) and line.strip() != "quit"
                         for command in commands for line in command.split("\n"))
        proc.stdin.write("\n".join(commands) + "\n")
        proc.stdin.flush()

    def wait(timeout=3.0):
        def completed():
            assert proc.poll() is None, f"vkbd {name} exited before completion\n{env.log_text(log)}"
            return sum(line.startswith("done ") and line.endswith("\n")
                       for line in env.log_text(log).splitlines(keepends=True)) >= submitted
        assert poll(completed, timeout=timeout), f"vkbd {name}: waiting for {submitted} commands\n{env.log_text(log)}"

    send.wait = wait
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


def test_keyboard_completion_ignores_stale_ack(lock_env):
    # Only real vkbd can expose old done lines while a new command is still unprocessed.
    with _keyboard(lock_env, "vk-completion") as send:
        send("sleep 0\n\n", "sleep 0")
        send.wait()
        send.proc.send_signal(signal.SIGSTOP)
        try:
            assert poll(lambda: re.search(r"^State:\s+T", Path(
                f"/proc/{send.proc.pid}/status").read_text(), re.M)), "vkbd did not stop"
            send("sleep 0")
            with pytest.raises(AssertionError, match="waiting for 3 commands"):
                send.wait(timeout=0.2)
        finally:
            send.proc.send_signal(signal.SIGCONT)
        send.wait()


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


def _keyboard_entered(locker):
    return poll(lambda: bool(re.search(r"wl_keyboard#\d+\.enter\(", locker.log_text())))


def test_locked_ctrl_is_not_ctrl(lock_env):
    """StickyKeys' locked Ctrl is not Ctrl: the password can still be typed and submitted with
    Return, and j/u are letters, not Ctrl+J/Ctrl+U. A latched Ctrl still is Ctrl."""
    env = lock_env
    debug = {"WAYLAND_DEBUG": "1"}
    with _keyboard(env, "vk-lockedctrl") as send:
        locker = _locker(env, "locked ctrl typing", OFF, env=debug)
        assert _keyboard_entered(locker), "locked ctrl: keyboard never entered"
        send("mods 0 0 4", f"type {PASSWORD}", "key Return")
        assert locker.wait_exit() == 0, f"locked Ctrl: the password cannot be typed\n{locker.log_text()}"

        locker = _locker(env, "locked ctrl letters", OFF, env=debug)
        assert _keyboard_entered(locker), "locked ctrl: keyboard never entered"
        send(f"type {PASSWORD}", "mods 0 0 4", "key j", "key u")
        send.wait()
        assert locker.alive(), "locked Ctrl: j counted as Ctrl+J and submitted the password"
        # j and u were typed as letters (password intact, plus "ju"): two Backspaces remove them.
        send("key BackSpace", "key BackSpace", "mods 0 0 0", "key Return")
        assert locker.wait_exit() == 0, f"locked Ctrl: Ctrl+U cleared the password\n{locker.log_text()}"

        locker = _locker(env, "latched ctrl", OFF, env=debug)
        assert _keyboard_entered(locker), "latched ctrl: keyboard never entered"
        send(f"type {PASSWORD}", "mods 0 4 0", "key j")
        assert locker.wait_exit() == 0, "latched Ctrl: Ctrl+J still submits"


def test_locked_ctrl_keeps_composed_text(lock_env):
    """With Ctrl locked, a dead-key sequence still types the composed character (de: acute, e)."""
    env = lock_env
    with _keyboard(env, "vk-deadkey", "--layout", "de") as send:
        locker = _locker(env, "locked ctrl dead key", OFF, env={"WAYLAND_DEBUG": "1"})
        assert _keyboard_entered(locker), "dead key: keyboard never entered"
        send("mods 0 0 4", "type t", "key acute", "type e", "type st", "key Return")
        assert locker.wait_exit() == 0, f"locked Ctrl: composed text was lost\n{locker.log_text()}"


def test_layout_name_is_data_in_cmd_labels(lock_env):
    """$LAYOUT comes from the keymap (foreign text): in cmd[] it is an environment variable the
    shell expands as data, bare, double-quoted and single-quoted (where it stays literal)."""
    env = lock_env
    pwned = env.path("pwned")
    out = env.path("layout-out")
    payload = f"$(touch {pwned})`touch {pwned}`';touch {pwned} #"
    config = OFF + (
        'label { text "cmd[update:100] printf \'%s|\' \\"$LAYOUT\\" \\"Hi $LAYOUT\\" \'$LAYOUT\' '
        f'>> {out}; : $LAYOUT"; }}\n'
    )
    with _keyboard(env, "vk-layout-name", "--group-name", payload) as send:
        locker = _locker(env, "layout injection", config)
        send("sleep 300")
        send.wait()
        # The first runs can precede the keymap (empty name); wait for a run that has it.
        want = f"{payload}|Hi {payload}|$LAYOUT|"
        assert poll(lambda: out.exists() and want in out.read_text()), f"layout: no data-only run: {out.read_text()!r}"
        time.sleep(0.5)
        assert not pwned.exists(), "layout: the keymap's group name ran as a command"
        _unlock(locker)


def test_unlocked_app_rejects_late_auth_and_stops_ticker(desktop):
    env = desktop
    locker = _locker(env, "unlocked App callbacks", OFF,
                     env={"SWAYWARDLOCK_TEST_EXIT_PROBE": "1", "RUST_LOG": "swaywardlock=info"})
    locker.type(PASSWORD)
    assert locker.wait_exit() == 0, f"retained unlocked App checks failed\n{locker.log_text()}"
    assert "test exit probe: late auth, surfaces and ticker checked" in locker.log_text()
    env.expect_unlocked("unlocked App callbacks")


@pytest.mark.parametrize("late_bind", [False, True], ids=["early", "late-bind"])
def test_keyboard_layout(lock_env, late_bind):
    env = lock_env
    env.swaymsg("seat", "seat1", "fallback", "false")
    with _keyboard(env, "vk-de", "--seat", "seat0", "--layout", "de,us") as send0:
        locker = _locker(env, "keyboard layout", LAYOUT,
                         env={"RUST_LOG": "swaywardlock=debug"})
        assert locker.wait_log("keyboard focus entered"), "keyboard layout: seat0 has focus"
        send0("key Shift_L")
        send0.wait()
        de = env.expect_width("keyboard layout: initial de layout", (255, 0, 0), BOX)
        with contextlib.ExitStack() as stack:
            if late_bind:
                # Delay get_keyboard until vkbd has installed a real XKB keymap. Otherwise
                # SCTK can discard enter/modifiers on the initial NoKeymap and hide the bug.
                stack.callback(locker.signal, signal.SIGCONT)
                locker.signal(signal.SIGSTOP)
                assert poll(lambda: re.search(r"^State:\s+T", Path(
                    f"/proc/{locker.pid}/status").read_text(), re.M)), "locker did not stop"
            send1 = stack.enter_context(_keyboard(env, "vk-us", "--seat", "seat1", "--layout", "us"))
            if late_bind:
                locker.signal(signal.SIGCONT)
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
            # Order the reset before sending input through the other keyboard's pipe.
            send1.wait()
            send0("key Shift_L")
            env.expect_width("keyboard layout: seat0 before departure", (255, 0, 0), BOX, de)
            send1("key Shift_L")
            env.expect_width("keyboard layout: seat1 before departure", (255, 0, 0), BOX, us)
        env.expect_width("keyboard layout: the remaining keyboard's layout returns when the other goes", (255, 0, 0), BOX, de)
        _unlock(locker)


def _keyboard_ids(text, seat):
    """Locker-created wl_keyboard ids belonging to the named seat."""
    seats = {m[1] for m in re.finditer(r'wl_seat#(\d+)\.name\("([^\"]+)"\)', text)
             if m[2] == seat}
    return [m[2] for m in re.finditer(
        r"wl_seat#(\d+)\.get_keyboard\(new id wl_keyboard#(\d+)\)", text) if m[1] in seats]


def _keyboard_events(text, kid, kind):
    return re.findall(rf"wl_keyboard#{kid}\.{kind}\(([^)]*)\)", text)


def test_keyboard_refocus(lock_env):
    # Losing the enter re-arm (m2: re-arm only when last_input == 0) must fail the
    # no-takeover assertion, not merely a setup/trace prerequisite.
    env = lock_env
    env.swaymsg("create_output")
    env.swaymsg("output", "HEADLESS-2", "resolution", "320x240", "position", "320", "0")
    env.swaymsg("seat", "seat1", "fallback", "false")
    config = 'background { color "#000000"; }\n' + OFF + (
        'label { text "$LAYOUT"; font "Sans 20"; color "#ff0000"; valign "top"; position 0 -10; }\n'
        'input-field { size 200 50; inner-color "#ffffff"; outline { width 4; color "#0000ff"; }; '
        'capslock-color "#ff00ff"; fade-on-empty false; }\n'
    )
    layout_box, caps_at = (0, 0, 320, 60), (160, 92)
    red, outline, caps = (255, 0, 0), (0, 0, 255), (255, 0, 255)
    with _keyboard(env, "vk-refocus-de", "--seat", "seat0", "--layout", "de") as send0, \
            _keyboard(env, "vk-refocus-fr", "--seat", "seat1", "--layout", "fr") as send1:
        locker = _locker(env, "keyboard refocus", config,
                         env={"WAYLAND_DEBUG": "1", "RUST_LOG": "swaywardlock=debug"})

        def entered():
            text = locker.log_text()
            ids = _keyboard_ids(text, "seat0")[-1:] + _keyboard_ids(text, "seat1")[-1:]
            return len(ids) == 2 and all(_keyboard_events(text, i, "enter") and
                                         _keyboard_events(text, i, "modifiers") for i in ids)

        assert poll(entered), "keyboard refocus: both keyboards entered with snapshots"
        send0("mods 0 0 2")
        send0.wait()
        de = env.expect_width("keyboard refocus: seat0 spoke", red, layout_box)
        env.expect_colour("keyboard refocus: seat0 Caps", caps, *caps_at)
        send1("key Shift_L")
        send1.wait()
        fr = env.expect_width("keyboard refocus: seat1 key takes over", red, layout_box,
                              lambda w: w > 0 and w != de)
        env.expect_colour("keyboard refocus: seat1 has no Caps", outline, *caps_at)
        k0 = _keyboard_ids(locker.log_text(), "seat0")[-1]
        for target in ("HEADLESS-2", "HEADLESS-1"):
            # Start a NEW trace slice: earlier enter/modifiers cannot satisfy this barrier.
            before = len(locker.log_text())
            env.swaymsg("focus", "output", target)

            def refocused():
                text = locker.log_text()[before:]
                events = re.findall(rf"wl_keyboard#{k0}\.(leave|enter|modifiers)\(", text)
                return events == ["leave", "enter", "modifiers"]

            assert poll(refocused), f"keyboard refocus: no new seat0 leave/enter/snapshot for {target}"
            snapshot = _keyboard_events(locker.log_text()[before:], k0, "modifiers")[-1]
            assert snapshot.split(", ")[1:] == ["0", "0", "2", "0"], snapshot
            time.sleep(0.5)
            shot = env.snapshot()
            assert shot.colour_width(red, layout_box) == fr, (
                f"keyboard refocus: quiet seat0 ({target}) must not take over the layout")
            assert shot.pixel(*caps_at) == outline, (
                f"keyboard refocus: quiet seat0 ({target}) must not show its Caps")
        # wlroots drops unchanged updates: change Caps to prove a genuine modifiers event.
        before = len(_keyboard_events(locker.log_text(), k0, "modifiers"))
        send0("mods 0 0 0")
        send0.wait()
        assert poll(lambda: len(_keyboard_events(locker.log_text(), k0, "modifiers")) > before)
        env.expect_width("keyboard refocus: genuine modifiers speak", red, layout_box, de)
        env.expect_colour("keyboard refocus: changed Caps cached", outline, *caps_at)
        send1("key Shift_L")
        env.expect_width("keyboard refocus: other key retakes", red, layout_box, fr)
        _unlock(locker)


def test_keyboard_repeat(lock_env):
    env = lock_env
    env.swaymsg("seat", "seat1", "fallback", "false")
    proxy_log = _log("droprel")
    proxy = env.spawn([env.contrib("wl-drop-key-release")], log=proxy_log)
    assert poll(lambda: env.path("droprel-ready").exists()), f"keyboard repeat: proxy not ready\n{env.log_text(proxy_log)}"
    config = 'background { color "#000000"; }\n' + OFF + (
        'input-field { size 280 50; inner-color "#ffffff"; font-color "#000000"; '
        'outline { width 4; color "#0000ff"; }; dots size=0.04 spacing=0.1; fade-on-empty false; }\n'
    )
    with _keyboard(env, "vk-seat0", "--seat", "seat0") as send:
        locker = _locker(env, "keyboard repeat", config,
                         env={"WAYLAND_DISPLAY": "wayland-droprel-0", "WAYLAND_DEBUG": "1",
                              "RUST_LOG": "swaywardlock=debug"})
        send("type " + "a" * 80)
        send.wait()
        dots_box = (22, 112, 298, 128)
        filled = env.expect_width("keyboard repeat: visible unsaturated dots", (0, 0, 0), dots_box,
                                  lambda w: 0 < w < 276)
        # Exclude the known non-repeat burst ~1.75 s after typing, just as the bash case.
        time.sleep(2.5)
        base = _wakeups(locker.pid)
        assert base < 10, f"keyboard repeat: idle locker ({base} wakeups in 2 s)"
        with contextlib.ExitStack() as stack:
            # As in late-bind layout: install a real keymap before get_keyboard, so
            # SCTK does not discard enter on NoKeymap and silently prevent repeating.
            stack.callback(locker.signal, signal.SIGCONT)
            locker.signal(signal.SIGSTOP)
            assert poll(lambda: re.search(r"^State:\s+T", Path(
                f"/proc/{locker.pid}/status").read_text(), re.M)), "repeat: locker did not stop"
            held = stack.enter_context(_keyboard(env, "vk-repeat-seat1", "--seat", "seat1"))
            locker.signal(signal.SIGCONT)

            def repeat_ready():
                text = locker.log_text()
                ids = _keyboard_ids(text, "seat1")
                return (ids and _keyboard_events(text, ids[-1], "keymap") and
                        _keyboard_events(text, ids[-1], "enter") and
                        _keyboard_events(text, ids[-1], "modifiers") and
                        re.search(r"keyboard focus entered", text[text.index(
                            f"wl_keyboard#{ids[-1]}.enter("):]))

            assert poll(repeat_ready), "keyboard repeat: seat1-specific dispatched enter with keymap"
            k1 = _keyboard_ids(locker.log_text(), "seat1")[-1]
            info = _keyboard_events(locker.log_text(), k1, "repeat_info")
            assert info and int(info[-1].split(", ")[0]) > 0, f"repeat rate must be positive: {info}"
            held("down BackSpace")
            held.wait()
            # This screen is deliberately changing: two settled samples can never be
            # required at 25 Hz. Observe the first deletion before the 600 ms delay.
            first = env.expect("keyboard repeat: immediate deletion",
                               lambda shot: shot.colour_width((0, 0, 0), dots_box),
                               lambda w: 0 < w < filled, timeout=0.5, settled=False)
            env.expect("keyboard repeat: deletes beyond the first key",
                       lambda shot: shot.colour_width((0, 0, 0), dots_box),
                       lambda w: 0 < w < first, settled=False)
            running = _wakeups(locker.pid)
            remaining = _width(env, (0, 0, 0), dots_box)
            assert running >= 20, f"keyboard repeat: held timer active ({running} wakeups in 2 s)"
            assert 0 < remaining < first, "keyboard repeat: still nonempty and repeating before removal"
            print(f"repeat seat1={k1} info={info} dots={filled}->{first}->{remaining} held-wakeups={running}")
            # Quit with BackSpace still down. The proxy drops release AND leave;
            # only capability removal (or the tick's missing-keyboard guard) can stop it.
        time.sleep(0.5)
        w1 = _width(env, (0, 0, 0), dots_box)
        assert w1 > 0, "keyboard repeat: removal leaves visible dots"
        woke = _wakeups(locker.pid)
        w2 = _width(env, (0, 0, 0), dots_box)
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


def test_pam_service(desktop):
    env = desktop
    directory = env.path("pam.d")
    directory.mkdir()
    child_env = {"SWAYWARDLOCK_TEST_PAM_DIR": str(directory)}
    config = 'auth { pam service="swl-missing"; }\n'
    locker = env.locker(config, env=child_env, log=_log("pam-missing"))
    locker.wait_locked("pam service: locks without the file")
    assert str(directory / "swl-missing") in locker.log_text(), "pam service: the warning names the file"
    assert "SIGUSR1" in locker.log_text(), "pam service: the warning says how to unlock"
    env.wtype(PASSWORD)
    env.wtype("-k", "Return")
    time.sleep(.5)
    assert locker.alive(), "pam service: Enter does not unlock without PAM"
    _unlock(locker)
    env.expect_unlocked("pam service: missing file")
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
    # Grace: the pointer entering, jitter of up to 5 px and clicks do not unlock; more than 5 px
    # from the entry point does (hyprlock's rule).
    locker = _locker(env, "pointer grace", config, args=("--grace", "30"))
    _tool(env, "vptr", "move", "1", "0", "sleep", "300", "move", "3", "0", "move", "0", "4", "click", "sleep", "300")
    assert locker.alive(), "pointer: entering, jitter within 5 px and a click do not unlock during grace"
    _tool(env, "vptr", "move", "-2", "0", "sleep", "300", "move", "-9", "0", "sleep", "300")
    assert locker.wait_exit() == 0, "pointer: motion of more than 5 px within the grace period unlocks"
    env.expect_unlocked("pointer grace")
    locker = _locker(env, "pointer grace key", config, args=("--grace", "30"))
    env.wtype("x")
    assert locker.wait_exit() == 0, "pointer: a key within the grace period still unlocks"
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

    with _fake(env, child_env, "matchasleep", "sleepnow", "verify-match") as log:
        locker = _locker(env, "fingerprint match asleep", sleep_config, env=child_env)
        assert poll(lambda: "WAKE" in log(), timeout=6), f"fingerprint: no wake\n{log()}"
        time.sleep(0.5)
        assert "status verify-match" in log(), f"fingerprint: match not sent while asleep\n{log()}"
        assert locker.alive(), f"fingerprint: a match while asleep must not unlock\n{log()}"
        _unlock(locker)
    race_config = OFF + 'auth { fingerprint { retry-delay-ms 1000; }; }\n'
    with _fake(env, child_env, "race", "verify-no-match+race", "verify-match") as log:
        locker = _locker(env, "fingerprint race", race_config, env=child_env)
        assert poll(lambda: "WAKE" in log(), timeout=6), f"fingerprint: no wake\n{log()}"
        time.sleep(1.5)
        assert _ordered(log(), "status verify-no-match", "SLEEP", "status verify-match", "WAKE"), f"fingerprint: race order\n{log()}"
        assert locker.alive(), f"fingerprint: a match queued while asleep must not unlock after wake\n{log()}"
        _unlock(locker)
    with _fake(env, child_env, "preparing", "preparing", "verify-match") as log:
        locker = _locker(env, "fingerprint preparing", sleep_config, env=child_env)
        assert poll(lambda: "WAKE" in log(), timeout=6), f"fingerprint: no wake\n{log()}"
        before = log().split("WAKE", 1)[0]
        assert "VerifyStart" not in before, f"fingerprint: scan started while preparing for sleep\n{log()}"
        assert locker.wait_exit() == 0, f"fingerprint: scan starts on wake and a match unlocks\n{log()}"
        assert log().split("WAKE", 1)[1].count("VerifyStart") == 1, f"fingerprint: one VerifyStart after wake\n{log()}"

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


def _burst_race(env, i, name, *statuses):
    """One run of a no-gap burst scenario: the lock must stay after the burst."""
    busdir = env.path("bus")
    busdir.mkdir()
    address = f"unix:path={busdir / 'sock'}"
    env.spawn(["dbus-daemon", "--session", "--nofork", f"--address={address}"], log=_log("dbus"))
    assert poll(lambda: (busdir / "sock").is_socket()), "fingerprint: bus not ready"
    child_env = {"DBUS_SYSTEM_BUS_ADDRESS": address}
    config = OFF + 'auth { fingerprint { retry-delay-ms 1000; }; }\n'
    with _fake(env, child_env, name, *statuses) as log:
        locker = _locker(env, f"fingerprint {name} {i}", config, env=child_env)
        assert poll(lambda: "WAKE" in log(), timeout=6), f"fingerprint: no wake\n{log()}"
        time.sleep(0.5)
        assert locker.alive(), f"fingerprint: {name} burst unlocked (run {i})\n{log()}"
        _unlock(locker)


# FPRINT_LOOPS sets the iteration count of the fingerprint race tests. The default is small
# (CI's e2e step has a tight time budget). Stress form, run before touching src/fprint.rs:
#   FPRINT_LOOPS=400 python3 -m pytest tests/e2e/test_auth_input.py -k fingerprint -n 16
# The ordering races (sleep_then_match, matches_then_wake, forged_wake: about 1-4% per run
# on the old code) are NOT reliably red at the default count: there they are smoke checks.
# The flood-inside-Claim test failed 50/50 on the old code, so it is a deterministic
# regression test even at a count of 2.
_LOOPS = int(os.environ.get("FPRINT_LOOPS", "3"))
_METHOD_LOOPS = int(os.environ.get("FPRINT_LOOPS", "2"))


@pytest.mark.parametrize("i", range(_LOOPS))
def test_fingerprint_matches_then_wake(desktop, i):
    """Asleep, then twenty matches and the WAKE back to back: the matches were sent asleep."""
    _burst_race(desktop, i, "matchburstwake", "verify-no-match+matchburstwake")


@pytest.mark.parametrize("i", range(_LOOPS))
def test_fingerprint_sleep_then_match(desktop, i):
    """PrepareForSleep(true) then a match back to back while scanning: the match is asleep."""
    _burst_race(desktop, i, "sleepmatch", "sleepmatch")


@pytest.mark.parametrize("i", range(_LOOPS))
def test_fingerprint_forged_wake_stays_locked(desktop, i):
    """Forged PrepareForSleep(false)/NameOwnerChanged (broadcast and unicast) change nothing:
    a match sent while really asleep is ignored."""
    _burst_race(desktop, i, "forgewake", "forgewake")


@pytest.mark.parametrize("i", range(_LOOPS))
def test_fingerprint_signal_flood_does_not_stall(desktop, i):
    """Hundreds of signals addressed to the locker while it is in VerifyStop must not stall the
    bus reader: after the wake, a real match still unlocks."""
    busdir = desktop.path("bus")
    busdir.mkdir()
    address = f"unix:path={busdir / 'sock'}"
    desktop.spawn(["dbus-daemon", "--session", "--nofork", f"--address={address}"], log=_log("dbus"))
    assert poll(lambda: (busdir / "sock").is_socket()), "fingerprint: bus not ready"
    child_env = {"DBUS_SYSTEM_BUS_ADDRESS": address}
    config = OFF + 'auth { fingerprint { retry-delay-ms 1000; }; }\n'
    with _fake(desktop, child_env, "flood", "flood", "verify-match") as log:
        locker = _locker(desktop, f"fingerprint flood {i}", config, env=child_env)
        assert locker.wait_exit(10) == 0, f"fingerprint: flood stalled the locker\n{log()}"


@pytest.mark.parametrize("i", range(_METHOD_LOOPS))
@pytest.mark.parametrize("method", ["Claim", "Get", "VerifyStart", "VerifyStop", "Release"])
def test_fingerprint_flood_inside_method_call(desktop, method, i):
    """300 unicast signals sent to the locker from inside fprintd's handler for one method call
    (before it replies) must not stall the locker: a no-match, then a match, still unlocks."""
    busdir = desktop.path("bus")
    busdir.mkdir()
    address = f"unix:path={busdir / 'sock'}"
    desktop.spawn(["dbus-daemon", "--session", "--nofork", f"--address={address}"], log=_log("dbus"))
    assert poll(lambda: (busdir / "sock").is_socket()), "fingerprint: bus not ready"
    child_env = {"DBUS_SYSTEM_BUS_ADDRESS": address, "FLOOD_IN": method}
    config = OFF + 'auth { fingerprint { retry-delay-ms 100; }; }\n'
    with _fake(desktop, child_env, f"flood-{method}", "verify-no-match", "verify-match") as log:
        locker = _locker(desktop, f"fingerprint flood in {method} {i}", config, env=child_env)
        assert locker.wait_exit(10) == 0, f"fingerprint: flood in {method} stalled the locker\n{log()}"
        assert f"FLOOD in {method}" in log(), log()
        assert _ordered(log(), "status verify-no-match", "status verify-match"), log()
