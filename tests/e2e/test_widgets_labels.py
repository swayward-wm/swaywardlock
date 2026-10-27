"""Bash batch 4 parity: renderer pixels, preview isolation and label process ownership.

These integration checks need a compositor: config validation alone cannot see a misplaced
widget, stale preview frame, blocked input, or command process surviving an unlock.
"""

import json
import os
import re
import signal
import sys
import time
from pathlib import Path

from lib.lockenv import Locker
from lib.poll import alive, poll


BLACK = (0, 0, 0)
RED = (255, 0, 0)
BLUE = (0, 0, 255)
GREEN = (0, 255, 0)
WHITE = (255, 255, 255)
BG = (32, 64, 96)
PASSWORD = "swaywardlock-test"


def _png(env, name="face.png"):
    path = env.path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    env.run([sys.executable, env.contrib("ppm-pixel"), "--write-test-png", path])
    return path


def _lock(runtime, config, name, **kwargs):
    # Each launch has its own log, including launches inside loops. Never reuse locked markers.
    locker = runtime.locker(config, log=f"{name}.log", **kwargs)
    locker.wait_locked(name)
    return locker


def _unlock(locker):
    locker.signal(signal.SIGUSR1)
    assert locker.wait_exit() == 0, f"{locker.log}: unlock failed\n{locker.log_text()}"


def _window(env):
    def walk(node):
        if node.get("app_id") == "swaywardlock-preview":
            rect = node["rect"]
            return tuple(rect[key] for key in ("x", "y", "width", "height"))
        for child in node.get("nodes", []) + node.get("floating_nodes", []):
            found = walk(child)
            if found:
                return found
        return None

    return walk(json.loads(env.swaymsg("-t", "get_tree")))


def _pid(path):
    try:
        return int(path.read_text().strip())
    except (OSError, ValueError):
        return None


def _children(pid):
    try:
        return [int(p) for p in Path(f"/proc/{pid}/task/{pid}/children").read_text().split()]
    except OSError:
        return []


def test_image(lock_env):
    env = lock_env
    image = _png(env, "bg.png")
    locker = _lock(env, f'background {{ path "{image}"; fit "fill"; }}', "image")
    env.expect_colour("image: top is red", RED, 160, 10)
    env.expect_colour("image: bottom is blue", BLUE, 160, 230)
    _unlock(locker)


def test_converted(lock_env):
    env = lock_env
    result = env.run([env.contrib("swaylock-to-kdl"), "--args",
                      "swaylock -c 000000 --indicator-idle-visible --indicator-x-position 80 "
                      "--indicator-y-position 70 --inside-color ff0000"], check=False)
    assert result.returncode == 0, f"converted: conversion failed\n{result.stderr.decode()}"
    locker = _lock(env, result.stdout.decode(), "converted")
    # Same measured swaylock 1.8.6 reference, including its 2px nudge.
    for name, colour, x, y in [
        ("inside starts where swaylock's does", RED, 39, 70),
        ("inside ends where swaylock's does", RED, 124, 70),
        ("ring starts where swaylock's does", (51, 125, 0), 29, 70),
        ("ring ends where swaylock's does", (51, 125, 0), 134, 70),
        ("background", BLACK, 300, 220),
    ]:
        env.expect_colour(f"converted: {name}", colour, x, y)
    _unlock(locker)


def test_hyprlock_converted(lock_env):
    env = lock_env
    conf = env.path("hyprlock.conf")
    conf.write_text("""background {
    color = rgba(204060ff)
}
shape {
    size = 160, 80
    color = rgb(255,0,0)
    rounding = 0
    border_size = 6
    border_color = rgba(00ff00ff) rgba(0000ffff) 0deg
}
shape {
    size = 25%, 25%
    position = 30, -20
    halign = left
    valign = top
    color = rgb(0,255,0)
    rounding = 0
}
""")
    result = env.run([env.contrib("hyprlock-to-kdl"), conf], check=False)
    assert result.returncode == 0, f"hyprlock converted: conversion failed\n{result.stderr.decode()}"
    locker = _lock(env, result.stdout.decode(), "hyprlock converted")
    for name, colour, x, y in [
        ("background", BG, 5, 230), ("red fill", RED, 160, 100),
        ("green box inside, top-left corner", GREEN, 30, 20),
        ("green box inside, bottom-right corner", GREEN, 105, 70),
        ("left of the green box", BG, 29, 25), ("above the green box", BG, 35, 19),
        ("below the green box", BG, 35, 80), ("right of the green box", BG, 115, 50),
    ]:
        env.expect_colour(f"hyprlock converted: {name}", colour, x, y)
    env.expect("hyprlock converted: left border is green", lambda s: s.pixel(76, 120),
               lambda c: c[1] > 200 and c[2] < 60 and c[0] < 40)
    env.expect("hyprlock converted: right border is blue", lambda s: s.pixel(243, 120),
               lambda c: c[2] > 200 and c[1] < 60 and c[0] < 40)
    _unlock(locker)


def test_broken_image(lock_env):
    env = lock_env
    broken = env.path("broken.png")
    broken.write_text("not a png\n")
    locker = _lock(env, f'background {{ color "#204060"; path "{broken}"; }}\n'
                   'ring-indicator { idle-visible; }', "broken image")
    env.expect_colour("broken image: colour still drawn", BG, 5, 5)
    env.expect_colour("broken image: ring still drawn", (51, 125, 0), 210, 120)
    _unlock(locker)


def test_failing_widget(lock_env):
    env = lock_env
    locker = _lock(env, 'background { color "#204060"; }\nring-indicator { idle-visible; }',
                   "failing widget", env={"SWAYWARDLOCK_TEST_FAIL_BACKGROUND": "1"})
    env.expect_colour("failing widget: fallback colour", BLACK, 5, 5)
    env.expect_colour("failing widget: ring still drawn", (51, 125, 0), 210, 120)
    _unlock(locker)


def test_preview(lock_env):
    env = lock_env
    config = '''background { color "#204060"; }
shape {
    size 160 80
    color "#ff0000"
    corner-radius 20
    border { width 6; gradient from="#00ff00" to="#0000ff" angle=90; }
}
'''
    locker = env.locker(config, args=["--preview"], log="preview.log",
                        env={"RUST_LOG": "swaywardlock=debug"})
    # Negative lock window, not an eventual-readiness wait.
    time.sleep(1.5)
    assert "session locked" not in locker.log_text(), "preview: did not lock (locked the session)"
    rect = poll(lambda: _window(env))
    assert rect, f"preview: no window\n{locker.log_text()}"
    x, y, w, h = rect
    assert w > 0 and h > 0, f"preview: window mapped ({w}x{h})"
    cx, cy = x + w // 2, y + h // 2
    env.expect_colour("preview: shape fill at the window centre", RED, cx, cy)
    env.expect("preview: gradient border starts green on the left", lambda s: s.pixel(cx - 84, cy),
               lambda c: c[0] == 0 and c[1] > 200)
    env.expect("preview: gradient border ends blue on the right", lambda s: s.pixel(cx + 83, cy),
               lambda c: c[1] < 60 and c[2] > 200)
    env.expect_colour("preview: background", BG, x + 5, y + 5)
    env.path("lock.kdl").write_text(config.replace("#204060", "#406020"))
    env.expect_colour("preview: reloads on change", (64, 96, 32), x + 5, y + 5)
    env.path("lock.kdl").write_text("shape { size\n")
    time.sleep(1.2)  # Full broken-edit survival/last-good-frame observation window.
    assert locker.alive(), "preview: survives a broken edit (died on a broken edit)"
    assert env.snapshot().pixel(x + 5, y + 5) == (64, 96, 32), "preview: keeps the last good frame"
    locker.type("whatever")
    time.sleep(1)
    locker.type(PASSWORD)
    time.sleep(1)
    assert locker.alive(), "preview: Enter does not exit (exited on Enter)"
    assert not re.search(r"PAM prompt|authenticated|authentication failed", locker.log_text()), (
        f"preview: Enter never reaches PAM (Enter reached the authenticator)\n{locker.log_text()}"
    )
    assert "session locked" not in locker.log_text(), "preview: locked the session"
    env.swaymsg("[app_id=swaywardlock-preview] kill")
    assert locker.wait_exit() == 0, "preview: closing the window exits 0"


def test_labels(lock_env):
    env = lock_env
    d = env.dir
    counter = env.path("counter")
    counter.write_text("one\n")
    face = _png(env)
    config = f'''background {{ color "#000000"; }}
label {{ text "cmd[update:300:1] cat {counter}"; font "Sans 20"; color "#ff0000"; halign "left"; valign "bottom"; position 10 10; }}
label {{ text "cmd[] cat {counter}"; font "Sans 20"; color "#0000ff"; halign "left"; valign "top"; position 10 -10; }}
label {{ text "cmd[update:0:1] cat {counter}"; font "Sans 20"; color "#ff00ff"; halign "right"; valign "top"; position -10 -10; }}
label {{ text "cmd[] echo $$ >{d}/long.pid; sleep 300; echo never"; color "#ffffff"; }}
label {{ text "cmd[] sleep 301 >/dev/null & echo $! >{d}/bg.pid; echo started"; color "#ffffff"; }}
label {{ text "$ATTEMPTS[none] tries"; font "Sans 20"; color "#00ff00"; halign "right"; valign "bottom"; position -10 10; }}
label {{ text "cmd[update:300] test -n \\"$ATTEMPTS\\" && echo shown"; font "Sans 20"; color "#00ffff"; halign "left"; valign "center"; }}
label {{ text "[$PAMPROMPT]"; font "Sans 20"; color "#ffff00"; halign "right"; valign "center"; }}
image {{ path "{face}"; size 60; border {{ width 4; color "#ffff00"; }}; }}
'''
    decoy = env.spawn(["sh", "-c", "sleep 300; echo never"], log="labels-decoy.log")
    locker = _lock(env, config, "labels", env={"RUST_LOG": "swaywardlock=debug"})
    before = env.expect_width("labels: cmd[] output drawn", RED, (0, 180, 200, 240))
    once = env.expect_width("labels: one-shot command initial output", BLUE, (0, 0, 200, 60))
    green_before = env.expect_width("labels: initial attempts", GREEN, (160, 180, 320, 240))
    env.expect_width("labels: $VARS substituted before cmd[] runs", (0, 255, 255), (0, 100, 100, 140))
    for name, colour, x, y in [
        ("image picture (red half)", RED, 160, 110), ("image picture (blue half)", BLUE, 160, 130),
        ("image border", (255, 255, 0), 160, 88), ("image corner is round", BLACK, 129, 89),
    ]:
        env.expect_colour(f"labels: {name}", colour, x, y)
    counter.write_text("twotwotwotwo\n")
    time.sleep(1)  # Also observes that the one-shot command does NOT refresh over multiple ticks.
    after = env.snapshot()
    width = after.colour_width(RED, (0, 180, 200, 240))
    assert width > before, f"labels: interval command re-ran ({before} -> {width} px)"
    assert after.colour_width(BLUE, (0, 0, 200, 60)) == once, "labels: one-shot command ran once (re-ran)"
    prompt_before = after.colour_width((255, 255, 0), (200, 100, 320, 140))
    assert prompt_before > 40, (
        f"labels: $PAMPROMPT shows the PAM prompt before any input ({prompt_before} px)"
    )
    locker.type("nope")
    assert locker.wait_log("authentication failed"), f"labels: failure never arrived\n{locker.log_text()}"
    # Both metrics come from the post-failure frame, not the still-valid old prompt frame.
    metrics = env.expect("labels: $ATTEMPTS updates after a failure", lambda s: (
        s.colour_width(GREEN, (160, 180, 320, 240)),
        s.colour_width((255, 255, 0), (200, 100, 320, 140)),
    ), lambda widths: widths[0] != green_before)
    assert metrics[1] > 40, f"labels: $PAMPROMPT still shown after a failure ({metrics[1]} px)"
    magenta_before = env.expect_width("labels: refreshable initial output", (255, 0, 255), (160, 0, 320, 60))
    counter.write_text("x\n")
    locker.signal(signal.SIGUSR2)
    time.sleep(0.8)  # Signal must not end the lock, not merely survive one instant.
    assert locker.alive(), "labels: SIGUSR2 does not kill the locker (killed the locker)"
    assert env.snapshot().colour_width((255, 0, 255), (160, 0, 320, 60)) < magenta_before, (
        "labels: SIGUSR2 re-ran the refreshable command (did not re-run cmd[update:0:1])"
    )
    long = poll(lambda: _pid(env.path("long.pid")))
    children = poll(lambda: _children(long)) if long else []
    assert long and alive(long) and children, "labels: long-running command is running (not started)"
    bg = poll(lambda: _pid(env.path("bg.pid")))
    assert bg and alive(bg), "labels: background job of a finished command is running (not started)"
    _unlock(locker)
    # MUST precede fixture teardown: its reaper would otherwise conceal production leaks.
    time.sleep(0.2)
    assert not alive(long) and not any(alive(p) for p in children), (
        "labels: cmd[] children killed on unlock (cmd[] child left running after unlock)"
    )
    assert not alive(bg), "labels: background job of a cmd[] label killed on unlock (left running)"
    assert decoy.poll() is None, "labels: an unrelated process with the same command line is untouched"
    os.killpg(decoy.pid, signal.SIGTERM)
    decoy.wait(timeout=3)


def test_cmd_output(lock_env):
    env = lock_env
    cases = [
        ("cmd output", "cmd[] seq 200000", "usr1", 1),
        ("cmd output, CR lines", r"cmd[update:500] date +%N; yes a | head -32000 | tr '\\n' '\\r'", "password", 2),
        ("cmd output, TABs", r"cmd[update:500] date +%N; yes a | head -16000 | tr '\\n' '\\t'", "password", 2),
    ]
    for name, command, how, limit in cases:
        locker = _lock(env, f'animations {{ off; }}\nlabel {{ text "{command}"; font "Sans 6"; }}', name)
        time.sleep(1)  # Intentional workload window: command output has arrived and is being drawn.
        start = time.monotonic()
        if how == "usr1":
            locker.signal(signal.SIGUSR1)
        else:
            locker.type(PASSWORD)
        assert locker.wait_exit() == 0, f"{name}: exit code\n{locker.log_text()}"
        elapsed = time.monotonic() - start
        assert elapsed < limit, f"{name}: {how} honoured at once ({elapsed * 1000:.0f} ms; limit {limit}s)"


def test_label_redraws(lock_env):
    env = lock_env
    config = 'animations { off; }\n' + "\n".join(
        f'label {{ text "cmd[update:200] echo {i}"; position 0 {i * 5}; }}' for i in range(1, 21)
    )
    locker = _lock(env, config, "label redraws", env={"RUST_LOG": "swaywardlock=trace"})
    time.sleep(0.5)  # First batch of command results; retain the following full negative window.
    before = sum("draw_all" in line for line in locker.log_text().splitlines())
    time.sleep(2)
    draws = sum("draw_all" in line for line in locker.log_text().splitlines()) - before
    assert draws <= 5, f"label redraws: constant output not redrawn ({draws} draws in 2 s)"
    _unlock(locker)


def test_preview_refresh(lock_env):
    env = lock_env
    runs = env.path("runs")
    locker = env.locker(f'label {{ text "cmd[update:60000:1] echo run >>{runs}; echo x"; }}',
                        args=["--preview"], log="preview-refresh.log")
    assert poll(lambda: runs.exists() and runs.stat().st_size, timeout=5), (
        f"preview refresh: initial command did not run\n{locker.log_text()}"
    )
    assert poll(lambda: _window(env)), "preview refresh: no window"
    time.sleep(0.3)
    locker.signal(signal.SIGUSR2)
    assert poll(lambda: len(runs.read_text().splitlines()) >= 2), "preview refresh: command did not re-run"
    assert len(runs.read_text().splitlines()) == 2, "preview refresh: SIGUSR2 re-runs the label command"
    assert locker.alive(), "preview refresh: still running (SIGUSR2 ended the preview)"
    env.swaymsg("[app_id=swaywardlock-preview] kill")
    assert locker.wait_exit() == 0, "preview refresh: window close did not exit"


def test_clock_resume(lock_env):
    env = lock_env
    shim = env.path("clock-jump.so")
    result = env.run(["cc", "-shared", "-fPIC", "-o", shim, env.contrib("clock-jump.c"), "-ldl"], check=False)
    assert result.returncode == 0, f"clock resume: cannot build the clock shim\n{result.stderr.decode()}"
    # Do not let a natural minute boundary certify resume invalidation.
    assert poll(lambda: time.localtime().tm_sec < 50, timeout=12, interval=0.1), "clock resume: minute boundary avoidance"
    locker = _lock(env, 'background { color "#000000"; }\nanimations { off; }\n'
                   'label { text "$TIME"; font "Sans 40"; color "#ffffff"; }', "clock resume",
                   env={"LD_PRELOAD": str(shim), "CLOCK_JUMP_TRIGGER": str(env.path("jump"))})
    env.expect_width("clock resume: initial time drawn", WHITE, (0, 0, 320, 240))
    before = env.snapshot().data
    env.path("jump").touch()
    assert poll(lambda: env.snapshot().data != before, timeout=2.5), (
        "clock resume: $TIME redrawn after a simulated suspend ($TIME still shows the time from before the suspend)"
    )
    _unlock(locker)


def test_immediate_render(lock_env):
    env = lock_env
    image = _png(env)
    locker = _lock(env, f'background {{ color "#204060"; path "{image}"; fit "fill"; }}',
                   "immediate render", args=["--immediate-render"])
    env.expect_colour("immediate render: the image is skipped, the colour drawn", BG, 160, 10)
    _unlock(locker)


def test_widgets(lock_env):
    env = lock_env
    face = _png(env)
    locker = _lock(env, f'''background {{ color "#204060"; }}
animations {{ off; }}
shape {{ size 100 60; color "#ff0000"; corner-radius 0; halign "left"; valign "top"; position 20 -20; border {{ width 4; color "#00ff00"; }}; }}
shape {{ size 40 40; color "#ff0000"; corner-radius 0; halign "right"; valign "top"; position -10 -10; border {{ width 0; color "#00ff00"; }}; }}
image {{ path "{face}"; size 60; corner-radius 20; border {{ width 6; color "#00ffff"; }}; halign "left"; valign "bottom"; position 10 10; }}
label {{ text "cmd[ HELLO"; font "Sans Bold 30"; color "#ffffff"; valign "bottom"; position 0 10; }}
''', "widgets")
    env.expect_colour("widgets: shape border is outside the size", GREEN, 124, 50)
    env.expect_colour("widgets: image border corner has radius + width", BG, 22, 160)
    env.expect_colour("widgets: image border just inside the corner", (0, 255, 255), 30, 160)
    env.expect_width("widgets: shape fill spans exactly its size", RED, (0, 45, 200, 55), until=100)
    env.expect_colour("widgets: border width 0 keeps the fill", RED, 290, 30)
    env.expect_width("widgets: unclosed cmd[ label is drawn as text", WHITE, (0, 150, 320, 240))
    _unlock(locker)

    env.swaymsg("output", "HEADLESS-1", "scale", "2")
    locker = _lock(env, f'background {{ color "#204060"; }}\nanimations {{ off; }}\n'
                   f'image {{ path "{face}"; size 10000; }}', "widgets huge")
    env.expect_colour("widgets: an oversized image leaves the background", BG, 5, 5)
    assert "GPU texture limit" in locker.log_text(), "widgets: oversized textures are logged (not logged)"
    _unlock(locker)
    for style, extra in [("", ""), (" swayward", 'style "swayward"; glow 64;')]:
        locker = _lock(env, 'background { color "#204060"; }\nanimations { off; }\n'
                       f'ring-indicator {{ {extra} radius 9000; idle-visible; }}', f"widgets huge{style} ring")
        env.expect_colour(f"widgets: a huge{style} ring draws, capped", (8, 16, 24), 5, 5)
        assert "GPU texture limit" not in locker.log_text(), (
            f"widgets: the capped{style} ring stays under the texture limit (hit the texture limit)"
        )
        _unlock(locker)
    env.swaymsg("output", "HEADLESS-1", "scale", "1")

    locker = _lock(env, f'background {{ path "{face}"; fit "fill"; blur {{ passes 0; offset 50; }}; }}\n'
                   'animations { off; }', "widgets blur")
    env.expect_colour("widgets: blur passes 0, red above the seam", RED, 160, 100)
    env.expect_colour("widgets: blur passes 0, blue below the seam", BLUE, 160, 140)
    _unlock(locker)
    locker = _lock(env, f'background {{ path "{face}"; fit "fill"; blur {{ passes 3; offset 8; }}; }}\n'
                   'animations { off; }', "widgets blur on")
    env.expect("widgets: blur mixes red and blue at the seam", lambda s: s.pixel(160, 122),
               lambda c: c[0] > 40 and c[2] > 40)
    env.expect("widgets: blur keeps red far from the seam", lambda s: s.pixel(160, 30),
               lambda c: c[0] > 200 and c[2] < 60)
    _unlock(locker)
    locker = _lock(env, f'background {{ path "{face}"; fit "fill"; brightness 0.5; }}\n'
                   'animations { off; }', "widgets brightness")
    env.expect_colour("widgets: brightness 0.5 halves the colour", (128, 0, 0), 160, 30)
    _unlock(locker)

    spec = (env.contrib("..").resolve() / "docs/specs/2026-10-19-swaywardlock.md").read_text()
    block = next(b for b in re.findall(r"```kdl\n(.*?)```", spec, re.S) if b.startswith("label {"))
    locker = _lock(env, 'background { color "#000000"; }\nanimations { off; }\n'
                   + block.replace('"$TIME"', '"HELLO"'), "widgets spec label")
    env.expect_width("widgets: the spec's label example is on screen", WHITE, (0, 0, 320, 240))
    _unlock(locker)

    _png(env, "conf/rel.png")
    conf = env.path("conf/rel.kdl")
    config = 'background { color "#00ff00"; path "rel.png"; fit "fill"; }\nanimations { off; }\n'
    conf.write_text(config)
    log = "widgets-relative.log"
    locker = Locker(env, env.spawn([env.binary, "-c", conf], log=log, cwd="/"), log)
    locker.wait_locked("widgets: relative")
    env.expect_colour("widgets: relative image path loads from the config dir", RED, 160, 10)
    _unlock(locker)
    # Command writes its parent's PID; never discover the daemon by global newest-pgrep.
    for index, cfg in enumerate(("rel.kdl", "./rel.kdl", "conf/rel.kdl")):
        pidfile = env.path(f"conf/daemon-{index}.pid")
        conf.write_text(config + f'label {{ text "cmd[] echo $PPID >{pidfile}; echo x"; }}\n')
        cwd = env.dir if cfg == "conf/rel.kdl" else conf.parent
        log = f"widgets-daemon-{index}.log"
        parent = env.spawn([env.binary, "-f", "-c", cfg], log=log, cwd=cwd)
        assert parent.wait(timeout=10) == 0, f"widgets: -f -c {cfg}, parent exit code\n{env.log_text(log)}"
        daemon = poll(lambda: _pid(pidfile), timeout=5)
        assert daemon and alive(daemon), f"widgets: -f -c {cfg}, no daemon pid\n{env.log_text(log)}"
        # Scope the discovered PID to this invocation before sending any signal.
        # /proc/<pid>/environ is unreadable (non-dumpable daemon); cmdline is not.
        cmdline = Path(f"/proc/{daemon}/cmdline").read_bytes().split(b"\0")[:4]
        assert cmdline == [os.fsencode(env.binary), b"-f", b"-c", cfg.encode()], f"widgets: foreign daemon pid {cmdline}"
        env.expect_colour(f"widgets: -f -c {cfg}, relative image path loads", RED, 160, 10)
        os.kill(daemon, signal.SIGUSR1)
        assert poll(lambda: not alive(daemon), timeout=5), f"widgets: -f -c {cfg}, daemon did not exit"
