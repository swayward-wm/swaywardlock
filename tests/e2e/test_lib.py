"""The screenshot and polling helpers, on hand-made images, and the signal hold (no sway)."""

import signal
import subprocess
import sys
from pathlib import Path

import pytest

from lib.lockenv import LockEnv
from lib.poll import alive, poll
from lib.screen import parse_ppm


def ppm(w, h, pixels, header=None):
    return (header or f"P6\n{w} {h}\n255\n".encode()) + bytes(c for p in pixels for c in p)


def test_pixel_and_default_centre():
    img = parse_ppm(ppm(3, 2, [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12), (13, 14, 15), (16, 17, 18)]))
    assert (img.width, img.height) == (3, 2)
    assert img.pixel(0, 0) == (1, 2, 3)
    assert img.pixel(2, 1) == (16, 17, 18)
    assert img.pixel() == (13, 14, 15)
    with pytest.raises(ValueError, match="outside"):
        img.pixel(3, 0)


def test_header_whitespace():
    # grim writes "P6\nW H\n255\n"; any whitespace between the fields is valid.
    img = parse_ppm(ppm(1, 1, [(9, 8, 7)], header=b"P6 1\t1\n255 "))
    assert img.pixel(0, 0) == (9, 8, 7)


@pytest.mark.parametrize("data", [b"", b"P6\n2 2\n255\n" + bytes(11), b"P3\n1 1\n255\n000"])
def test_rejects_bad_ppm(data):
    with pytest.raises(ValueError):
        parse_ppm(data)


def test_colour_width():
    red, blue = (255, 0, 0), (0, 0, 255)
    # A 6x2 image: row 0 red at x 1..3, row 1 red at x 2..4 (one channel off by 50).
    row0 = [blue, red, red, red, blue, blue]
    row1 = [blue, blue, red, red, (205, 0, 0), blue]
    img = parse_ppm(ppm(6, 2, row0 + row1))
    assert img.colour_width(red, (0, 0, 6, 1)) == 3
    assert img.colour_width(red, (0, 0, 6, 2)) == 4
    assert img.colour_width(red, (0, 0, 6, 2), tolerance=10) == 3
    assert img.colour_width((0, 255, 0), (0, 0, 6, 2)) == 0


def test_poll_returns_last_value():
    seen = iter([0, 0, 5])
    assert poll(lambda: next(seen), timeout=2, interval=0) == 5
    assert poll(lambda: None, timeout=0.05, interval=0.01) is None


def test_alive():
    import os

    assert alive(os.getpid())
    assert not alive(2**22 + 1)  # above the default pid_max


class FakeScreen(LockEnv):
    """A LockEnv whose screen is a list of frames (the last repeats), without sway."""

    def __init__(self, frames):
        self.frames = list(frames)

    def snapshot(self):
        frame = self.frames.pop(0) if len(self.frames) > 1 else self.frames[0]
        return parse_ppm(ppm(len(frame), 1, frame))


RED, BLUE = (255, 0, 0), (0, 0, 255)


def bar(width, total=8):
    return [RED] * width + [BLUE] * (total - width)


def test_expect_width_waits_for_a_settled_frame():
    # sway shows the lock colour, then a half-drawn frame, then the whole one.
    env = FakeScreen([bar(0), bar(3), bar(6), bar(6), bar(6)])
    assert env.expect_width("w", RED, (0, 0, 8, 1), timeout=2) == 6


def test_expect_width_exact_and_predicate():
    env = FakeScreen([bar(2), bar(4), bar(4), bar(5), bar(5)])
    assert env.expect_width("w", RED, (0, 0, 8, 1), 4, timeout=2) == 4
    assert env.expect_width("w", RED, (0, 0, 8, 1), lambda w: w > 4, timeout=2) == 5


def test_expect_width_failure_names_box_want_and_history():
    env = FakeScreen([bar(1), bar(2)])
    with pytest.raises(AssertionError) as e:
        env.expect_width("labels: grew", RED, (0, 0, 8, 1), 7, timeout=0.3)
    msg = str(e.value)
    assert msg.startswith("labels: grew: want width 7 of (255, 0, 0) in box (0, 0, 8, 1)")
    assert "last saw 2 after 0." in msg and "(seen: 1, 2)" in msg


def test_expect_colour_needs_two_samples_unless_unsettled():
    env = FakeScreen([[BLUE], [RED], [BLUE]])
    # One red frame between blue ones is not a settled red.
    msg = r"want \(255, 0, 0\) at the centre, last saw \(0, 0, 255\)"
    with pytest.raises(AssertionError, match=msg):
        env.expect_colour("c", RED, timeout=0.5)
    env = FakeScreen([[BLUE], [RED], [BLUE]])
    assert env.expect_colour("c", RED, timeout=0.5, settled=False) == RED


def test_expect_generic_read():
    env = FakeScreen([bar(1), bar(3), bar(3)])
    got = env.expect("n", lambda shot: len(shot.matching(RED, (0, 0, 8, 1))), lambda n: n >= 3)
    assert got == 3


def test_no_public_single_sample_reads():
    # Every screen read on LockEnv polls; the one single sample is spelled snapshot().
    reads = {
        name
        for name in dir(LockEnv)
        if not name.startswith("_")
        and any(k in name for k in ("pixel", "colour", "width", "screen", "shot"))
    }
    assert reads == {"expect_colour", "expect_width", "snapshot"}


HOLD_PROBE = r"""
import os, signal, sys, threading, time
sys.path.insert(0, sys.argv[1])
from lib.lockenv import hold_signals

def interrupt(signum, frame):
    raise KeyboardInterrupt(signum)

signal.signal(signal.SIGTERM, interrupt)
# A second thread, like an xdist worker's execnet thread: the kernel may deliver signals there.
threading.Thread(target=time.sleep, args=(5,), daemon=True).start()
done = []
try:
    with hold_signals():
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGTERM):
            os.kill(os.getpid(), sig)
            time.sleep(0.1)
        done.append("cleanup finished")
    print("not raised")
except KeyboardInterrupt as e:
    print(done, "then", e.args)
"""


def test_hold_signals_with_a_second_thread():
    out = subprocess.run(
        [sys.executable, "-c", HOLD_PROBE, str(Path(__file__).parent)],
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    # The block finished; then the first held signal acted.
    assert out.stdout.strip() == f"['cleanup finished'] then ({int(signal.SIGTERM)},)", out.stderr
