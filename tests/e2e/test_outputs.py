"""Output lifecycle and capture parity with contrib/e2e-sway.

These need a compositor: protocol traces alone cannot show a missing lock surface,
physical pixel scaling, or a redraw wedged waiting for a dark output's callback.
"""

import json
import re
import signal
import time
from pathlib import Path

import pytest

from lib.poll import poll
from lib.screen import parse_ppm

BG = (32, 64, 96)
BASE = 'animations { off; }\nbackground { color "#204060"; }\n'
SHAPE = BASE + '''shape {
    size 100 50; color "#ff0000"; halign "left"; valign "top"; corner-radius 0;
}
'''
TICK = 'animations { off; }\nlabel { text "cmd[update:300] date +%N"; }\n'
PASSWORD = "swaywardlock-test"


def _unlock(lock, name="locker cleanup"):
    # Assert production exit before the fixture can kill a stuck locker for us.
    lock.signal(signal.SIGUSR1)
    assert lock.wait_exit() == 0, f"{name}: did not unlock\n{lock.log_text()}"


def _log(lock, name, marker, failure=None):
    assert lock.wait_log(marker), f"{failure or name} (no {marker!r})\n{lock.log_text()}"


def _locked(lock, failure):
    assert lock.wait_log("session locked"), f"{failure}\n{lock.log_text()}"


def _output_colour(env, name, output, want):
    # grim -o may fail while the newly advertised output is not yet enabled.
    seen = []

    def sample():
        result = env.run(["grim", "-o", output, "-t", "ppm", "-"], check=False)
        if result.returncode:
            seen.clear()
            return False
        got = parse_ppm(result.stdout).pixel(5, 5)
        stable = bool(seen) and got == seen[-1]
        seen.append(got)
        return stable and got == want

    assert poll(sample), f"{name}: want {want}, saw {seen[-6:]}"


def test_outputs(lock_env):
    env = lock_env
    env.swaymsg("create_output")
    env.swaymsg("output", "HEADLESS-2", "resolution", "320x240", "position", "320", "0")
    lock = env.locker('''animations { off; }
background { color "#ff0000"; }
background { color "#0000ff"; }
background output="HEADLESS-2" { color "#00ff00"; }
''', log="outputs.log")
    lock.wait_locked("outputs")
    for output, want in [("HEADLESS-1", (0, 0, 255)), ("HEADLESS-2", (0, 255, 0))]:
        _output_colour(env, f"outputs: {output} background", output, want)
    _unlock(lock)


def test_hotplug(lock_env):
    env = lock_env
    lock = env.locker(BASE, env={"WAYLAND_DEBUG": "1"}, log="hotplug.log")
    lock.wait_locked("hotplug")
    env.swaymsg("create_output")
    _output_colour(env, "hotplug: a new output is covered by the lock screen", "HEADLESS-2", BG)
    destroy = r"ext_session_lock_surface_v1#[0-9]+\.destroy\(\)"
    destroyed = len(re.findall(destroy, lock.log_text()))
    env.swaymsg("output", "HEADLESS-2", "unplug")
    # Survival is a negative observation, not eventual readiness.
    time.sleep(0.5)
    assert lock.alive(), "hotplug: survives the output going (died when the output went)"
    assert len(re.findall(destroy, lock.log_text())) > destroyed, (
        "hotplug: the unplugged output's lock surface is destroyed "
        "(the unplugged output's lock surface is kept)\n" + lock.log_text()
    )
    env.swaymsg("create_output")
    _output_colour(env, "hotplug: an output plugged after an unplug is covered", "HEADLESS-3", BG)
    env.swaymsg("output", "HEADLESS-3", "unplug")
    time.sleep(0.5)
    _unlock(lock, "hotplug: SIGUSR1 unlocks after outputs came and went")
    assert not re.search("protocol error", lock.log_text(), re.I), (
        "hotplug: no protocol error\n" + lock.log_text()
    )


def test_scale_change(lock_env):
    env = lock_env
    lock = env.locker(SHAPE.replace("100 50", "100 40"),
                      env={"SWAYWARDLOCK_TEST_NO_FRACTIONAL": "1"}, log="scale-change.log")
    lock.wait_locked("scale change")
    for scale in (1, 2, 1):
        env.swaymsg("output", "HEADLESS-1", "scale", str(scale))
        env.expect_width(f"scale change: shape is {100 * scale} px wide at scale {scale}",
                         (255, 0, 0), (0, 5, 320, 10), 100 * scale)
    _unlock(lock)


def test_fractional(lock_env):
    env = lock_env
    env.swaymsg("output", "HEADLESS-1", "scale", "1.5")
    lock = env.locker(SHAPE, env={"RUST_LOG": "swaywardlock=debug"}, log="fractional-initial.log")
    lock.wait_locked("fractional")
    env.expect_colour("fractional: shape fill", (255, 0, 0), 149, 10)
    env.expect_colour("fractional: shape right edge is crisp", BG, 150, 10)
    _log(lock, "fractional: used wp_fractional_scale", "fractional scale: on",
         "fractional: never used wp_fractional_scale")
    _unlock(lock)

    env.swaymsg("output", "HEADLESS-1", "scale", "1")
    lock = env.locker(SHAPE, log="fractional-runtime.log")
    _locked(lock, "fractional: runtime never locked")
    env.expect_colour("fractional: at scale 1 the edge is at 100", BG, 100, 10)
    env.swaymsg("output", "HEADLESS-1", "scale", "1.5")
    env.expect_colour("fractional: scale change, shape fill", (255, 0, 0), 149, 10)
    env.expect_colour("fractional: scale change, right edge is crisp", BG, 150, 10)
    _unlock(lock)

    env.swaymsg("output", "HEADLESS-1", "scale", "2")
    lock = env.locker(SHAPE, log="fractional-fallback.log", env={
        "SWAYWARDLOCK_TEST_NO_FRACTIONAL": "1", "WAYLAND_DEBUG": "1",
        "RUST_LOG": "swaywardlock=debug",
    })
    _locked(lock, "fractional: fallback never locked")
    env.expect_colour("fractional: integer fallback, shape fill", (255, 0, 0), 199, 10)
    env.expect_colour("fractional: integer fallback, right edge", BG, 200, 10)
    _log(lock, "fractional: integer fallback ran",
         "fractional scale: unavailable, using integer scale 2", "fractional: integer fallback did not run")
    _log(lock, "fractional: fallback sets buffer scale 2", "set_buffer_scale(2)",
         "fractional: fallback never sent set_buffer_scale(2)")
    assert not re.search("get_viewport|get_fractional_scale", lock.log_text()), (
        "fractional: fallback still created viewport/fractional objects; "
        "fallback creates no viewport/fractional objects\n" + lock.log_text()
    )
    _unlock(lock)


def test_dark_output(lock_env):
    env = lock_env
    lock = env.locker(TICK, log="dark-password.log")
    lock.wait_locked("dark output")
    env.swaymsg("output", "HEADLESS-1", "power", "off")
    # Several 300 ms redraw ticks must happen without frame callbacks before input.
    time.sleep(1.5)
    lock.type(PASSWORD)
    assert lock.wait_exit() == 0, "dark output: the password unlocks with the output off"
    env.swaymsg("output", "HEADLESS-1", "power", "on")
    env.swaymsg("create_output")
    lock = env.locker(TICK, log="dark-usr1.log")
    _locked(lock, "dark output: two outputs never locked")
    time.sleep(0.5)  # Let the second output draw before removing its callbacks.
    env.swaymsg("output", "HEADLESS-2", "power", "off")
    time.sleep(1.5)
    _unlock(lock, "dark output: SIGUSR1 unlocks with a second output off")


def test_unplug(lock_env):
    env = lock_env
    first = 2
    for how in ("password", "usr1"):
        for _ in range(3):
            env.swaymsg("create_output")
        for n in range(first, first + 3):
            env.swaymsg("output", f"HEADLESS-{n}", "resolution", "160x120", "position", str(n * 400), "0")
        lock = env.locker(TICK + 'background { color "#204060"; }\n', log=f"unplug-{how}.log")
        lock.wait_locked(f"unplug {how}")
        time.sleep(1)  # Draw on all four outputs before the back-to-back unplugs.
        for n in range(first + 2, first - 1, -1):
            env.swaymsg("output", f"HEADLESS-{n}", "unplug")
        time.sleep(1.5)  # Multiple redraw ticks expose the swap-interval deadlock.
        sc = Path(f"/proc/{lock.pid}/syscall").read_text().split()[0]
        assert sc != "271", (
            f"unplug {how}: main thread not stuck in ppoll (syscall {sc}); main thread stuck in ppoll"
        )
        # Success label preserves the measured syscall, not the historical 281.
        if how == "password":
            lock.type(PASSWORD)
        else:
            lock.signal(signal.SIGUSR1)
        assert lock.wait_exit() == 0, f"unplug {how}: unlocks after three of four outputs are unplugged"
        first += 3


def test_tiny_output(lock_env):
    env = lock_env
    env.swaymsg("output", "HEADLESS-1", "resolution", "16x16")
    lock = env.locker(args=("-c", env.contrib("../examples/hyprlock-example.kdl")), log="tiny.log")
    lock.wait_locked("tiny output")
    time.sleep(1)  # Survive subpixel widget/field animation, not just initial locking.
    assert lock.alive(), "tiny output: still running (died)"
    _unlock(lock, "tiny output: unlocks")


def test_huge_output(lock_env):
    env = lock_env
    env.swaymsg("output", "HEADLESS-1", "resolution", "20000x100")
    lock = env.locker(BASE, log="huge.log")
    lock.wait_locked("huge output")
    env.expect_colour("huge output: background colour", BG, 5, 5)
    _log(lock, "huge output: warned (no warning)", "over the GPU texture limit")
    _unlock(lock, "huge output: unlocks")


def test_hidpi_first_frame(lock_env):
    env = lock_env
    env.swaymsg("output", "HEADLESS-1", "scale", "2")
    lock = env.locker(BASE, env={"SWAYWARDLOCK_TEST_NO_FRACTIONAL": "1", "WAYLAND_DEBUG": "1"},
                      log="hidpi.log")
    lock.wait_locked("hidpi")
    _log(lock, "hidpi: first commit uses the output scale", "set_buffer_scale(")
    first = re.search(r"set_buffer_scale\([0-9]+\)", lock.log_text()).group()
    assert first == "set_buffer_scale(2)", f"hidpi: first commit uses the output scale (got {first})"
    _unlock(lock)


def test_zero_size(lock_env):
    env = lock_env
    lock = env.locker(TICK, log="zero-size.log")
    lock.wait_locked("zero size")
    time.sleep(0.3)  # At least one command redraw before losing the usable size.
    env.swaymsg("output", "HEADLESS-1", "scale", "3")
    env.swaymsg("output", "HEADLESS-1", "resolution", "1x1")
    time.sleep(1.5)  # cmd[] redraws against the acked 0x0 configure.
    env.run(["wtype", "-s", "50", "a"])
    time.sleep(0.5)  # A key-driven redraw must also survive.
    assert lock.alive(), "zero size: survives a 0x0 configure, cmd[] ticks and a key\n" + lock.log_text()
    _log(lock, "zero size: got the 0x0 configure (sway never sent a 0x0 configure)", "no usable size yet (0x0")
    env.swaymsg("output", "HEADLESS-1", "resolution", "320x240")
    env.swaymsg("output", "HEADLESS-1", "scale", "1")
    time.sleep(0.5)  # Return from zero size before clearing the key typed above.
    env.wtype("-k", "BackSpace")
    env.run(["wtype", "-s", "30", PASSWORD, "-k", "Return"])
    assert lock.wait_exit() == 0, "zero size: password unlocks after the output returns"


def _shot_lock(env, config, name, hook=None, immediate=False):
    lock = env.locker(config, args=("--immediate-render",) if immediate else ("--no-fade-in",),
                      env={"RUST_LOG": "swaywardlock=debug", **(
                          {"SWAYWARDLOCK_TEST_SCREENCOPY": hook} if hook else {})}, log=f"{name}.log")
    if name.startswith("rotation-"):
        transform, hook_name = name.removeprefix("rotation-").rsplit("-", 1)
        label = f"rotation {transform} {hook_name}"
    else:
        suffix = name.removeprefix("screenshot-")
        label = "screenshot:" if suffix == "wlr" else f"screenshot: {suffix}"
    _locked(lock, f"{label} never locked")
    return lock


def test_screenshot(lock_env):
    env = lock_env
    env.spawn(["swaybg", "-c", "#ff00ff"], log="screenshot-desktop.log")
    env.expect_colour("screenshot: desktop is magenta before locking", (255, 0, 255), 5, 5)
    conf = 'background { color "#204060"; screenshot; }\n'
    for name, hook, marker, colour in [
        ("wlr-screencopy is tried first", None, "screenshots via wlr-screencopy", (255, 0, 255)),
        ("ext-image-copy-capture is the fallback", "ext", "screenshots via ext-image-copy-capture", (255, 0, 255)),
        ("no capture protocol is reported", "none", "screenshots unavailable", BG),
        ("failed ext capture is reported", "ext-fail", "buffer_constraints", BG),
    ]:
        key = hook or "wlr"
        lock = _shot_lock(env, conf, f"screenshot-{key}", hook)
        _log(lock, f"screenshot: {name}", marker)
        labels = {"wlr": "lock shows the desktop", "ext": "ext-image-copy-capture shows the desktop",
                  "none": "no capture protocol falls back to the colour",
                  "ext-fail": "failed ext capture falls back to the colour"}
        env.expect_colour(f"screenshot: {labels[key]}", colour, 160, 120)
        if hook == "ext-fail":
            assert lock.alive(), "screenshot: failed ext capture keeps the lock (exited after a failed ext capture)"
        _unlock(lock)
    lock = _shot_lock(env, conf, "screenshot-immediate", immediate=True)
    env.expect_colour("screenshot: --immediate-render falls back to the colour", BG, 160, 120)
    _unlock(lock)


@pytest.mark.parametrize("mode", ("wlr", "ext-session", "ext-frame"))
def test_capture_timeout_destroys_inflight_objects(lock_env, mode):
    env = lock_env
    env.start_desktop()
    name = f"wayland-stall-{mode}"
    proxy_log = f"stall-{mode}.log"
    env.spawn(["python3", Path(__file__).parent / "lib/stall_capture.py", mode, name], log=proxy_log)
    assert poll(lambda: "ready" in env.log_text(proxy_log)), env.log_text(proxy_log)
    lock = env.locker('background { color "#204060"; screenshot; }\n', args=("--no-fade-in",),
                      env={"WAYLAND_DISPLAY": name, "WAYLAND_DEBUG": "1",
                           "RUST_LOG": "swaywardlock=debug",
                           **({"SWAYWARDLOCK_TEST_SCREENCOPY": "ext"} if mode != "wlr" else {})},
                      log=f"timeout-{mode}.log")
    lock.wait_locked(f"capture timeout {mode}")
    _log(lock, f"capture timeout {mode}: production timeout", "screenshots did not finish in time")
    env.expect_colour(f"capture timeout {mode}: falls back to colour", BG, 160, 120)
    assert "suppressed" in env.log_text(proxy_log), env.log_text(proxy_log)
    # Negative observation: retain an interval after locked. The proxy still forwards
    # callbacks/input/fds; this checks no capture retry while the static lock is observed.
    time.sleep(0.3)
    assert lock.alive(), lock.log_text()
    _unlock(lock, f"capture timeout {mode}")
    env.expect_unlocked(f"capture timeout {mode}")

    trace = lock.log_text()
    request = re.search(r"ext_session_lock_manager_v1#[0-9]+\.lock\(", trace)
    assert request, trace
    start = re.search(r"\.(capture_output|create_session)\(", trace)
    assert start, trace
    before, after = trace[start.start():request.start()], trace[request.start():]
    types = ["zwlr_screencopy_frame_v1"] if mode == "wlr" else ["ext_image_copy_capture_session_v1"]
    if mode == "ext-frame":
        types += ["ext_image_copy_capture_frame_v1", "wl_buffer", "wl_shm_pool"]
    for iface in types:
        ids = re.findall(r"new id " + iface + r"#([0-9]+)", before)
        assert len(ids) == 1, f"{mode}: expected one owned {iface}, saw {ids}\n{trace}"
        destroy = iface + "#" + ids[0] + ".destroy()"
        assert before.count(destroy) == 1, f"{mode}: {destroy} must precede lock exactly once\n{trace}"
        # Mesa reuses capture IDs for render buffers. Count until the next creation of
        # that interface/ID, not the whole log; a second destroy before reuse is a failure.
        reused = re.search(r"new id " + iface + "#" + ids[0] + r"\b", after)
        lifetime_end = request.start() + reused.start() if reused else len(trace)
        assert trace[start.start():lifetime_end].count(destroy) == 1, trace
    if mode != "ext-frame":
        assert "new id wl_buffer#" not in before, trace
    if mode == "ext-session":
        assert "new id ext_image_copy_capture_frame_v1#" not in before, trace
    assert not re.search(r"\.(capture_output|capture_output_region|create_session|create_frame|capture|copy|copy_with_damage)\(", after), trace
    assert "protocol error" not in trace.lower(), trace


def test_screenshot_rotation(lock_env):
    env = lock_env
    image = env.path("bg.png")
    env.run(["python3", env.contrib("ppm-pixel"), "--write-quadrant-png", image])
    env.spawn(["swaybg", "-i", image, "-m", "stretch"], log="rotation-desktop.log")
    conf = 'background { color "#204060"; screenshot; fit "fill"; }\n'
    colours = ((255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255))

    def quadrants(shot):
        return (shot.width, shot.height, tuple(
            shot.pixel(x, y) for y in (shot.height // 4, 3 * shot.height // 4)
            for x in (shot.width // 4, 3 * shot.width // 4)))

    for tf in ("normal", "90", "180", "270", "flipped", "flipped-90", "flipped-180", "flipped-270"):
        env.swaymsg("output", "HEADLESS-1", "transform", tf)
        assert poll(lambda: next(o["transform"] for o in json.loads(env.swaymsg("-r", "-t", "get_outputs"))
                                 if o["name"] == "HEADLESS-1") == tf), f"rotation {tf}: transform never applied"

        dimensions = (240, 320) if tf.endswith(("90", "270")) else (320, 240)
        # swaybg presents the fixture upright in logical output coordinates. Read the actual
        # settled desktop rather than deriving a golden image from screencopy::untransform.
        reference = env.expect(
            f"rotation {tf}: settled distinct desktop quadrants", quadrants,
            lambda v: v[:2] == dimensions and v[2] == colours)
        for hook in ("wlr", "ext"):
            lock = _shot_lock(env, conf, f"rotation-{tf}-{hook}", "ext" if hook == "ext" else None)
            label = "ext run uses ext-image-copy-capture" if hook == "ext" else "default run uses wlr-screencopy"
            _log(lock, f"rotation {tf}: {label}", f"screenshots via {'ext-image-copy-capture' if hook == 'ext' else 'wlr-screencopy'}")
            env.expect(f"rotation {tf} {hook}: dimensions and four interiors match desktop",
                       quadrants, reference)
            _unlock(lock)
        env.swaymsg("output", "HEADLESS-1", "transform", "normal")
