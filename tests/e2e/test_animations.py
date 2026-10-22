"""Animation integration contracts: pixels, temporal samples and compositor unlocks.

These are the ten animation cases from contrib/e2e-sway, not unit substitutes:
only a running compositor can expose frame/ticker stalls and premature unlocks.
Sleeps retain temporal samples, negative windows and overshoot retarget spacing.
"""

import signal
import time
from pathlib import Path

from lib.poll import poll

BLACK = (0, 0, 0)
WHITE = (255, 255, 255)
BLUE = (0, 0, 255)
RED = (255, 0, 0)
GREEN = (0, 255, 0)
TEXT_BOX = (62, 112, 258, 128)


def _start(env, name, config, *, no_fade=True, extra_env=None):
    # spawn appends logs. Name each invocation, not merely each test function.
    log = f"animation-{len(env.procs)}.log"
    lock = env.locker(config, args=["--no-fade-in"] if no_fade else [], env=extra_env, log=log)
    lock.wait_locked(name)
    return lock


def _stop(lock):
    lock.signal(signal.SIGUSR1)
    assert lock.wait_exit() == 0, f"animation cleanup: locker did not unlock\n{lock.log_text()}"


def _until(t0, ms):
    left = (t0 + ms * 1_000_000 - time.monotonic_ns()) / 1e9
    if left > 0:
        time.sleep(left)


def _width(env, colour=WHITE, box=(0, 118, 320, 122)):
    return env.snapshot().colour_width(colour, box)


def _centre(shot, colour, y=120):
    xs = [x for x, _ in shot.matching(colour, (0, y, shot.width, y + 1))]
    return min(xs) + max(xs) if xs else -1


def _wakeups(lock):
    def count():
        fields = dict(line.split(":", 1) for line in Path(f"/proc/{lock.pid}/status").read_text().splitlines())
        return int(fields["voluntary_ctxt_switches"])

    before = count()
    # Full negative observation window: no residual animation/repeat ticker.
    time.sleep(2)
    return count() - before


def _field_config(config):
    # Parity probes can replace this factory with FIELD_BASE's removal of animations.
    # Normal runs deliberately do not inherit historical harness selectors.
    return config


def _ring_config(animations="", *, swayward=True, glow=None, ring_base=False):
    # ring_base mirrors RING_BASE; swayward=False alone is the centre reference.
    if ring_base:
        swayward = False
        glow = None  # Bash's later glow substitution cannot match the removed style.
        animations = "\n".join(line for line in animations.splitlines() if "ring-colors" not in line and "ring-wrong" not in line)
    style = 'style "swayward"' if swayward else ""
    glow = f"glow {glow}" if glow is not None else ""
    return f'''background {{ color "#000000"; }}
ring-indicator {{
    {style}
    {glow}
    idle-visible
    radius 80
    thickness 10
    colors {{
        state "input" inside="#0000ffff"
        state "clear" inside="#ff0000ff"
        state "wrong" inside="#00ff00ff"
        key-highlight "#ff00ffff"
        bs-highlight "#ff00ffff"
    }}
}}
{animations}
'''


def test_input_field(lock_env):
    e = lock_env
    lock = _start(e, "input-field", '''background { color "#000000"; }
input-field {
    fade-on-empty false
    size 200 50
    inner-color "#ffffff"
    outline { width 4; color "#0000ff"; }
    font-color "#000000"
    placeholder-text "zzzzzzzzzzzz"
    dots size=0.4 spacing=0.5
    check { color "#00ff00"; }
    fail { color "#ff0000"; text "ffffffff"; }
}
''', no_fade=False)
    e.expect_colour("input-field: outline idle colour", BLUE, 160, 92)
    e.expect_colour("input-field: inner fill", WHITE, 70, 120)
    e.expect_width("input-field: placeholder drawn", BLACK, TEXT_BOX, lambda w: w > 40)
    e.wtype("abc")
    e.expect_width("input-field: three dots span 80 px", BLACK, TEXT_BOX, 80)
    e.wtype("-k", "Return")
    e.expect_colour("input-field: fail outline", RED, 160, 92)
    e.expect_width("input-field: fail text drawn in the fail colour", RED, TEXT_BOX, lambda w: w > 30)
    _stop(lock)

    lock = _start(e, "input-field: dot text", '''background { color "#ffffff"; }
animations { off; }
general { fail-timeout-ms 1500; }
input-field { fade-on-empty false; size 200 50; inner-color "#ffffff"; font-color "#000000"; dots text="W";
  outline { width 4; color "#0000ff"; };
  fail { color "#ff0000"; text ""; }; }
''', no_fade=False)
    e.wtype("x", "-k", "Return")
    time.sleep(0.5)
    e.wtype("ab")
    time.sleep(0.2)
    red = _width(e, RED, (62, 100, 258, 140))
    time.sleep(1.5)
    black = _width(e, BLACK, (62, 100, 258, 140))
    assert red > 0, f"input-field: text dots in the fail colour ({red}px); text dots not red during the fail state"
    assert black > 0, f"input-field: text dots back to the font colour ({black}px); text dots stuck in the fail colour"
    _stop(lock)

    lock = _start(e, "input-field: hide-input", '''background { color "#000000"; }
animations { off; }
input-field { fade-on-empty false; size 200 80; corner-radius 0; inner-color "#ffffff";
  outline { width 6; color "#0000ff"; }; hide-input "#ff0000"; }
''', no_fade=False)
    e.wtype("a")
    time.sleep(0.4)
    shot = e.snapshot()
    edge, other, inside = (shot.pixel(*p) for p in [(240, 77), (80, 77), (200, 117)])
    assert edge not in (BLUE, WHITE), f"input-field: hide-input recolours the outline ({edge}); hide-input outline not recoloured"
    assert other == BLUE, "input-field: hide-input leaves other quadrants"
    assert inside == WHITE, "input-field: hide-input draws nothing inside the field"
    _stop(lock)


def test_field_fade(lock_env):
    e = lock_env
    lock = _start(e, "field-fade", '''background { color "#000000"; }
animations { input-field-fade { duration-ms 400; curve "linear"; }; }
input-field {
    size 200 50
    inner-color "#ffffff"
    outline { width 4; color "#0000ff"; }
    fade-on-empty timeout-ms=600
}
''')
    t0 = time.monotonic_ns()
    _until(t0, 300)
    assert e.snapshot().pixel(70, 120) == BLACK, "field-fade: hidden at lock"
    e.wtype("a")
    t0 = time.monotonic_ns()
    _until(t0, 600)
    assert e.snapshot().pixel(70, 120) == WHITE, "field-fade: visible after a key"
    e.wtype("-k", "BackSpace")
    t0 = time.monotonic_ns()
    _until(t0, 300)
    assert e.snapshot().pixel(70, 120) == WHITE, "field-fade: still visible before the timeout"
    _until(t0, 800)
    mid = e.snapshot().pixel(70, 120)[0]
    assert 20 < mid < 235, f"field-fade: partway through the fade ({mid}); not mid-fade at +0.8 s"
    _until(t0, 1600)
    assert e.snapshot().pixel(70, 120) == BLACK, "field-fade: gone after the fade"
    e.wtype("a")
    t0 = time.monotonic_ns()
    _until(t0, 600)
    assert e.snapshot().pixel(70, 120) == WHITE, "field-fade: visible again after a key"
    _stop(lock)


def test_field_colors(lock_env):
    e = lock_env
    lock = _start(e, "field-colors", _field_config('''background { color "#000000"; }
general { fail-timeout-ms 5000; }
animations { input-field-colors { duration-ms 2000; curve "linear"; }; }
input-field {
    size 200 50
    inner-color "#ffffff"
    outline { width 4; color "#0000ff"; }
    check { color "#0000ff"; }
    fail { color "#ff0000"; text ""; }
    fade-on-empty false
}
'''))
    e.wtype("x", "-k", "Return")
    t0 = time.monotonic_ns()
    _until(t0, 1000)
    mid = e.snapshot().pixel(160, 92)
    assert 50 <= mid[1] <= 100, f"field-colors: OkLab blend at ~1 s ({mid}); outline not mid-blend at ~1 s"
    _until(t0, 2500)
    assert e.snapshot().pixel(160, 92) == RED, "field-colors: outline red at ~2.5 s"
    _stop(lock)


def test_field_dots(lock_env):
    e = lock_env
    lock = _start(e, "field-dots", _field_config('''background { color "#000000"; }
animations { input-field-dots { duration-ms 2000; curve "linear"; }; }
input-field {
    size 200 50
    inner-color "#ffffff"
    font-color "#000000"
    outline { width 4; color "#0000ff"; }
    dots size=0.4 spacing=0.5
    fade-on-empty false
}
'''))
    e.wtype("abc")
    t0 = time.monotonic_ns()
    _until(t0, 1000)
    early = _width(e, BLACK, TEXT_BOX)
    assert early < 80, f"field-dots: still filling at ~1 s ({early}px); all three dots at ~1 s"
    _until(t0, 2600)
    assert _width(e, BLACK, TEXT_BOX) == 80, "field-dots: three dots span 80 px at ~2.6 s"
    _stop(lock)


def _field_overshoot_config(y=3, control=False):
    animations = "" if control else f'''input-field-dots {{ duration-ms 300; curve "cubic-bezier" 0 {y} 1 {y}; }}
    input-field-width {{ duration-ms 300; curve "cubic-bezier" 0 -2 1 {y}; }}'''
    return f'''background {{ color "#000000"; }}
animations {{
    {animations}
}}
input-field {{ size 200 50; fade-on-empty false; placeholder-text "wwwwwwwwwwww"; }}
'''


def test_field_overshoot(lock_env):
    e = lock_env
    lock = _start(e, "field-overshoot", _field_overshoot_config())
    e.wtype("-d", "100", "abcdef")
    time.sleep(0.5)
    e.wtype("-k", "Escape")
    time.sleep(0.5)
    args = []
    for _ in range(20):
        args += ["x", "-s", "100", "-k", "BackSpace", "-s", "100"]
    e.wtype(*args)
    time.sleep(0.5)
    e.wtype("-d", "100", "swaywardlock-test", "-k", "Return")
    assert lock.wait_exit() == 0, "field-overshoot: password unlocks after overshooting retargets"


def _ring_overshoot_config(y=3, ring_base=False):
    return _ring_config(f'''animations {{
    ring-highlight {{ duration-ms 300; curve "cubic-bezier" 0 {y} 1 {y}; }}
    ring-wrong {{ duration-ms 300; curve "cubic-bezier" 0 {y} 1 -2; }}
    ring-appear {{ duration-ms 300; curve "cubic-bezier" 0 -2 1 {y}; }}
}}''', ring_base=ring_base)


def test_ring_overshoot(lock_env):
    e = lock_env
    lock = _start(e, "ring-overshoot", _ring_overshoot_config())
    e.wtype("-d", "50", "abcdefghijklmnopqrstuvwxyz", "-k", "Return")
    time.sleep(1)
    assert lock.alive(), "ring-overshoot: still locked after a wrong password; exited after a wrong password"
    e.wtype("-d", "50", "abcdefghijklmnopqrstuvwxyz", "-k", "Escape")
    time.sleep(0.5)
    t0 = time.monotonic_ns()
    e.wtype("-d", "50", "swaywardlock-test", "-k", "Return")
    assert lock.wait_exit() == 0, "ring-overshoot: password unlocks after overshooting retargets"
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert ms < 5000, f"ring-overshoot: typed and unlocked in {ms} ms; typing the password took {ms} ms"


WIDTH_CONFIG = '''background { color "#000000"; }
animations { input-field-width { duration-ms 2000; curve "linear"; }; }
input-field {
    size 100 40
    inner-color "#ffffff"
    font-color "#000000"
    outline { width 4; color "#0000ff"; }
    placeholder-text "wwwwwwwwwwwwwwww"
    fade-on-empty false
}
'''


def test_field_width(lock_env):
    e = lock_env
    lock = _start(e, "field-width", _field_config(WIDTH_CONFIG))
    t0 = time.monotonic_ns()
    _until(t0, 1000)
    mid = _width(e)
    _until(t0, 3000)
    final = _width(e)
    assert 130 < final < 300, f"field-width: grows for the placeholder (100 -> {final}px); final width {final}px"
    assert 100 < mid < final, f"field-width: partway at ~1 s ({mid}px); not mid-animation at ~1 s ({mid}px of {final}px)"
    e.wtype("a")
    t0 = time.monotonic_ns()
    _until(t0, 1000)
    mid = _width(e)
    _until(t0, 3000)
    back = _width(e)
    assert 100 < mid < final, f"field-width: shrinking at ~1 s ({mid}px); not mid-shrink at ~1 s"
    assert back == 100, "field-width: back to 100 px after typing"
    _stop(lock)


def test_field_unlock(lock_env):
    e = lock_env
    config = WIDTH_CONFIG.replace('input-field-width { duration-ms 2000;', 'fade-out { duration-ms 800; curve "linear"; }; input-field-width { duration-ms 3000;')
    lock = _start(e, "field-unlock", config)
    time.sleep(1)
    w1 = _width(e)
    time.sleep(0.5)
    w2 = _width(e)
    assert w1 > 100 and w2 > w1, f"field-unlock: a field animation is running ({w1} -> {w2}px); field animation not running"
    t0 = time.monotonic_ns()
    lock.signal(signal.SIGUSR1)
    assert lock.wait_exit() == 0, "field-unlock: exit code"
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert 750 <= ms <= 1800, f"field-unlock: unlock took the fade-out, not the field ({ms} ms); fade-out 800 ms, width animation 3000 ms"
    lock = _start(e, "field-unlock", '''background { color "#000000"; }
animations {
    input-field-width { duration-ms 1000; }
    input-field-colors { duration-ms 1000; }
    input-field-dots { duration-ms 1000; }
}
input-field { placeholder-text "wwwwwwwwwwwwwwww"; fade-on-empty timeout-ms=500; }
''')
    e.wtype("abc", "-k", "Return")
    time.sleep(2.5)
    assert lock.alive(), "field-unlock: still locked after a wrong password and its animations; exited after a wrong password"
    e.wtype("abc")
    time.sleep(2.5)
    assert lock.alive(), "field-unlock: still locked after typing and its animations; exited after typing"
    _stop(lock)


def test_ring_style(lock_env):
    e = lock_env
    band = [(242, 120), (219, 177), (162, 200), (105, 177), (82, 120), (105, 63), (162, 40), (219, 63)]
    lock = _start(e, "ring-style", _ring_config('animations { ring-colors { duration-ms 800; curve "linear"; }; }'))
    e.expect_colour("ring-style: idle inside", BLUE, 162, 80)
    base = _wakeups(lock)
    assert base < 10, f"ring-style: settled ring is idle ({base} wakeups in 2 s); settled ring wakes {base} times in 2 s"
    e.wtype("-k", "Escape")
    t0 = time.monotonic_ns()
    _until(t0, 300)
    mid = e.snapshot().pixel(162, 80)
    assert 20 < mid[0] < 235 and 20 < mid[2] < 235, f"ring-style: inside mid-crossfade at ~0.3 s ({mid}); inside not mid-crossfade at ~0.3 s"
    _until(t0, 1100)
    shot = e.snapshot()
    assert shot.pixel(162, 80) == RED, "ring-style: inside at the clear colour after the crossfade"
    c = shot.pixel(251, 120)[1]
    assert c > 10, f"ring-style: glow outside the ring (green {c}); no glow outside the ring"
    _stop(lock)

    lock = _start(e, "ring-style", _ring_config(glow=0))
    # Black alone is already true on sway's blank pre-first-frame lock screen.
    e.expect_colour("ring-style: glow 0 ring ready", BLUE, 162, 80)
    assert e.snapshot().pixel(251, 120) == BLACK, "ring-style: glow 0 leaves the background"
    _stop(lock)

    lock = _start(e, "ring-style", _ring_config(), extra_env={"SWAYWARDLOCK_TEST_AUTH_DELAY_MS": "2000", "RUST_LOG": "swaywardlock=debug"})
    e.wtype("x", "-k", "Return")
    time.sleep(0.4)
    shot = e.snapshot()
    sig1 = tuple(shot.pixel(*p) for p in band)
    time.sleep(0.3)
    shot = e.snapshot()
    sig2 = tuple(shot.pixel(*p) for p in band)
    assert "authentication failed" not in lock.log_text(), "ring-style: still verifying during the spinner samples; the verdict came before the spinner samples"
    assert sig1 != sig2, f"ring-style: the spinner moves while verifying; the band did not change in 300 ms while verifying ({sig1})"
    # Verdict plus wrong-state expiry, then a separate full ticker observation window.
    time.sleep(5.5)
    woke = _wakeups(lock)
    assert woke < 10, f"ring-style: no ticker after verifying ({woke} wakeups in 2 s); {woke} wakeups in 2 s after verifying"
    _stop(lock)

    lock = _start(e, "ring-style", _ring_config(swayward=False))
    centre = e.expect("ring-style: swaylock centre ready", lambda s: _centre(s, BLUE), lambda v: v >= 0)
    _stop(lock)
    lock = _start(e, "ring-style", _ring_config('''animations { ring-colors { off; }; ring-wrong { duration-ms 3000; curve "linear"; }; }
general { fail-timeout-ms 5000; }'''))
    rest = e.expect("ring-style: swayward centre ready", lambda s: _centre(s, BLUE), lambda v: v >= 0)
    assert rest == centre, "ring-style: the glow does not move the ring"
    e.wtype("x", "-k", "Return")
    def wrong():
        r, g, b = e.snapshot().pixel(162, 80)
        return g >= 250 and r <= 150 and b <= 150
    assert poll(wrong, timeout=2, interval=0.05), "ring-style: the wrong verdict reached the screen"
    moved = 0
    for _ in range(12):
        c = _centre(e.snapshot(), GREEN)
        if c >= 0:
            moved = max(moved, abs(c - rest))
    assert 4 <= moved <= 16, f"ring-style: the ring shakes on a wrong password (max {moved // 2} px); no plausible shake on a wrong password (max offset {moved} / 2 px, want 2..8)"
    time.sleep(3.5)
    assert _centre(e.snapshot(), GREEN) == rest, "ring-style: back at rest after the shake"
    assert lock.alive(), "ring-style: still locked after a wrong password; exited after a wrong password"
    e.wtype("x", "-k", "Return")
    time.sleep(0.3)
    t0 = time.monotonic_ns()
    lock.signal(signal.SIGUSR1)
    assert lock.wait_exit() == 0, "ring-style: SIGUSR1 during the shake exits 0"
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert ms < 1500, f"ring-style: unlock during the shake took {ms} ms"

    lock = _start(e, "ring-style", _ring_config(), extra_env={"SWAYWARDLOCK_TEST_AUTH_DELAY_MS": "1000"})
    e.wtype("swaywardlock-test", "-k", "Return")
    t0 = time.monotonic_ns()
    assert lock.wait_exit() == 0, "ring-style: the password during the spinner exits 0"
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert 800 <= ms < 2500, f"ring-style: unlock after the delayed verdict took {ms} ms; unlock after a 1 s verdict delay took {ms} ms"
    lock = _start(e, "ring-style", _ring_config('animations { off; ring-colors { duration-ms 2000; curve "linear"; }; }'))
    e.expect_colour("ring-style: off initial frame ready", BLUE, 162, 80)
    e.wtype("-k", "Escape")
    time.sleep(0.2)
    assert e.snapshot().pixel(162, 80) == RED, "ring-style: animations off, final colour right after the key"
    _stop(lock)


FADE_CONFIG = '''background { color "#ffffff"; }
animations {
    fade-in { duration-ms 1500; curve "linear"; }
    fade-out { duration-ms 1500; curve "linear"; }
}
'''


def test_fade(lock_env):
    e = lock_env
    lock = _start(e, "fade", FADE_CONFIG, no_fade=False)
    time.sleep(0.4)
    early = e.snapshot().pixel(5, 5)[0]
    time.sleep(0.4)
    early2 = e.snapshot().pixel(5, 5)[0]
    time.sleep(1.2)
    late = e.snapshot().pixel(5, 5)[0]
    assert 10 < early < 240, f"fade: partway through fade-in ({early}); not mid-fade at 0.4 s"
    assert early2 > early + 20, f"fade: fade-in brightens ({early} -> {early2}); fade-in does not brighten"
    assert late == 255, "fade: fully faded in"
    t0 = time.monotonic_ns()
    lock.signal(signal.SIGUSR1)
    time.sleep(0.6)
    mid = e.snapshot().pixel(5, 5)[0]
    assert lock.wait_exit() == 0, "fade: fade-out exits"
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert 10 < mid < 240, f"fade: partway through fade-out ({mid}); not mid-fade-out at 0.6 s"
    assert 1300 <= ms < 3500, f"fade: unlock waited for the fade-out ({ms} ms); unlock took {ms} ms"

    # Need a sample BEFORE locked, so cannot use _start's readiness wait here.
    lock = e.locker(FADE_CONFIG, env={"SWAYWARDLOCK_TEST_LATE_LOCKED": "1500"}, log=f"animation-{len(e.procs)}.log")
    time.sleep(1)
    before = e.snapshot().pixel(5, 5)[0]
    lock.wait_locked("fade: late locked")
    after = e.snapshot().pixel(5, 5)[0]
    assert before == 255, "fade: late locked, lock screen shown before locked"
    assert after == 255, "fade: late locked, no restart from black"
    _stop(lock)

    lock = _start(e, "fade: mid", FADE_CONFIG.replace("fade-in { duration-ms 1500", "fade-in { duration-ms 3000"), no_fade=False)
    time.sleep(1.2)
    before = e.snapshot().pixel(5, 5)[0]
    lock.signal(signal.SIGUSR1)
    maximum = 0
    for _ in range(5):
        maximum = max(maximum, e.snapshot().pixel(5, 5)[0])
        time.sleep(0.1)
    after = e.snapshot().pixel(5, 5)[0]
    assert maximum <= before + 25 and after < before, f"fade: an unlock mid fade-in darkens from there ({before} -> max {maximum} -> {after}); an unlock mid fade-in brightened"
    assert lock.wait_exit() == 0, "fade: mid fade-in unlock exits"

    lock = _start(e, "fade: --no-fade-in", 'background { color "#ffffff"; }\nanimations { fade-in { duration-ms 3000; }; }')
    time.sleep(0.5)
    assert e.snapshot().pixel(5, 5) == WHITE, "fade: --no-fade-in shows full colour at once"
    _stop(lock)
    lock = _start(e, "fade: off", 'background { color "#ffffff"; }\nanimations { off; }', no_fade=False)
    e.expect_colour("fade: animations off shows full colour", WHITE, 5, 5)
    t0 = time.monotonic_ns()
    _stop(lock)
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert ms < 1000, f"fade: animations off unlocks at once ({ms} ms); animations off took {ms} ms to unlock"
    lock = _start(e, "fade: zero", 'animations { fade-out { duration-ms 0; }; }', no_fade=False)
    time.sleep(1)  # Default fade-in finishes; its ticker must not rescue the zero fade-out.
    lock.signal(signal.SIGUSR1)
    assert lock.wait_exit() == 0, "fade: duration-ms 0 fade-out unlocks"
    lock = _start(e, "fade: wedge config", '''background { color "#ffffff"; }
animations { fade-in { duration-ms 3000; curve "linear"; }; fade-out { off; }; }
''', no_fade=False)
    e.wtype("swaywardlock-test", "-k", "Return")
    assert lock.wait_exit() == 0, "fade: password during the fade-in exits"
    lock = _start(e, "fade: long fade-out", '''background { color "#ffffff"; }
animations { fade-in { off; }; fade-out { duration-ms 3000; curve "linear"; }; }
''', no_fade=False)
    lock.signal(signal.SIGUSR1)
    time.sleep(0.3)
    t0 = time.monotonic_ns()
    lock.signal(signal.SIGUSR1)
    assert lock.wait_exit() == 0, "fade: second unlock request exits"
    ms = (time.monotonic_ns() - t0) // 1_000_000
    assert ms < 1500, f"fade: a second request cuts the fade-out short ({ms} ms); the second request waited for the fade-out ({ms} ms)"
