"""A headless sway per test, the processes a test starts, and the locker."""

import contextlib
import mmap
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from . import DESKTOP
from .poll import alive, poll
from .screen import TOLERANCE, grab

ROOT = Path(__file__).resolve().parents[3]
CONTRIB = ROOT / "contrib"
DIR_PREFIX = "swl-e2e."
# Signals held while a test's processes are stopped (Ctrl-C, timeout(1), pytest-timeout).
CLEANUP_SIGNALS = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP, signal.SIGALRM)
# The variable that hands the reaper (reaper.py) its runtime dir. Not XDG_RUNTIME_DIR, so the
# reaper's own environment never names the dir and cleanup does not count it as a stray.
REAP_ENV = "SWL_E2E_REAP_DIR"
# Variables that would point a child at the caller's session instead of this test's sway.
FOREIGN_ENV = ("XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "SWAYSOCK", "DISPLAY")


def check_binary(path):
    """Fail early, with a reason, if `path` is not a `--features test-auth` build."""
    path = Path(path)
    if not path.is_file():
        return f"{path} does not exist: run cargo build --features test-auth"
    with open(path, "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
        # A hook only test-auth builds have (contrib/gate checks release builds lack it).
        if m.find(b"SWAYWARDLOCK_TEST_AUTH_DELAY_MS") < 0:
            return f"{path} is not a --features test-auth build"
    return None


class LockEnv:
    """One headless sway in a fresh runtime dir, and every process a test starts on it.

    `close` stops all of them and removes the dir; the `lock_env` fixture calls it however the
    test ends (pass, failure, timeout, KeyboardInterrupt).
    """

    def __init__(self, binary, resolution="320x240"):
        self.binary = str(binary)
        self.procs = []
        self.dir = Path(tempfile.mkdtemp(prefix=DIR_PREFIX, dir="/tmp"))
        # Lets a later run clean up after one that was killed before its teardown ran.
        (self.dir / "owner").write_text(str(os.getpid()))
        self._reaper = None
        env = {k: v for k, v in os.environ.items() if k not in FOREIGN_ENV}
        env["XDG_RUNTIME_DIR"] = str(self.dir)
        # Keep the user's own config out of tests that rely on defaults.
        env["XDG_CONFIG_HOME"] = str(self.dir / "config-home")
        self.env = env
        self.sway = None
        try:
            self._start_reaper()
            self._start_sway(resolution)
        except BaseException:
            self.close()
            raise

    def _start_reaper(self):
        """Start reaper.py, which cleans up this dir if this process dies before `close`.

        A SIGKILLed pytest runs no teardown, and PR_SET_PDEATHSIG cannot bound sway: exec of a
        binary with file capabilities (sway has cap_sys_nice) clears it. The reaper holds the
        read end of a pipe whose only write end is ours (os.pipe fds are not inherited), so it sees
        EOF exactly when this process is gone.
        """
        r, w = os.pipe()
        try:
            self._reaper = subprocess.Popen(
                [sys.executable, str(Path(__file__).with_name("reaper.py"))],
                env=dict(os.environ, **{REAP_ENV: str(self.dir)}),
                stdin=r,
                stdout=subprocess.DEVNULL,
                # Out of pytest's process group, so a group-wide Ctrl-C or kill leaves it to clean.
                start_new_session=True,
            )
        except BaseException:
            os.close(w)
            raise
        finally:
            os.close(r)
        self._reaper_pipe = w
        # sweep_stale_dirs leaves a dir to its reaper while that runs.
        (self.dir / "reaper").write_text(str(self._reaper.pid))

    def _stop_reaper(self):
        """Tell the reaper the dir is clean (one byte, then EOF) and wait for it to exit."""
        if self._reaper is None:
            return
        try:
            os.write(self._reaper_pipe, b"x")
        except OSError:
            pass
        os.close(self._reaper_pipe)
        try:
            self._reaper.wait(5)
        except subprocess.TimeoutExpired:
            self._reaper.kill()
            self._reaper.wait()
        self._reaper = None

    def _start_sway(self, resolution):
        conf = self.dir / "sway.conf"
        conf.write_text(f"output HEADLESS-1 resolution {resolution}\n")
        self.sway = self.spawn(
            ["sway", "-c", str(conf)],
            log="sway.log",
            env={"WLR_BACKENDS": "headless", "WLR_LIBINPUT_NO_DEVICES": "1", "WLR_RENDERER": "pixman"},
        )

        def display():
            names = sorted(p.name for p in self.dir.glob("wayland-*") if p.name[8:].isdigit())
            return names[0] if names else None

        name = poll(display, timeout=5)
        if not name:
            raise RuntimeError(f"sway did not start:\n{self.log_text('sway.log')}")
        self.env["WAYLAND_DISPLAY"] = name

    def path(self, name):
        return self.dir / name

    def log_text(self, name):
        try:
            return self.path(name).read_text(errors="replace")
        except OSError:
            return ""

    def spawn(self, argv, log=None, env=None, **kw):
        """Start a process on this sway, in its own process group; `close` kills it.

        Output goes to `log` (a file name in the runtime dir) or is discarded.
        """
        full_env = dict(self.env, **(env or {}))
        out = open(self.path(log), "ab") if log else subprocess.DEVNULL
        try:
            p = subprocess.Popen(
                [str(a) for a in argv],
                env=full_env,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                **kw,
            )
        finally:
            if log:
                out.close()
        self.procs.append(p)
        return p

    def run(self, argv, timeout=10, check=True, **kw):
        """Run a short command on this sway to completion."""
        return subprocess.run(
            [str(a) for a in argv],
            env=self.env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=timeout,
            check=check,
            **kw,
        )

    def contrib(self, name):
        return CONTRIB / name

    def swaymsg(self, *args):
        """swaymsg against this test's sway (never through an exported SWAYSOCK)."""
        sock = self.path(f"sway-ipc.{os.getuid()}.{self.sway.pid}.sock")
        return self.run(["swaymsg", "--socket", sock, *args]).stdout.decode()

    def wtype(self, *args):
        """wtype with a 300 ms delay between keys. Each run creates a fresh virtual keyboard."""
        self.run(["wtype", "-s", "300", *args])

    # Screen. Every read polls until its condition holds on a settled screen; `snapshot` is the
    # one way to take a single sample.

    def snapshot(self):
        """One grim screenshot, no waiting. sway sends `locked` before the locker's first frame is
        on screen, so a single sample can show the compositor's lock colour or a half-drawn frame:
        use the `expect*` reads unless the point is not to wait (a value that must not change,
        read after a sleep).
        """
        return grab(self.env)

    def expect(self, name, read, until, timeout=3.0, settled=True, want=None):
        """Poll `read(screenshot)` until its value meets `until`, and return that value.

        `until` is the expected value or a predicate. With `settled`, two samples in a row must
        agree (and both meet `until`), so a half-drawn first frame does not count. On a timeout
        the AssertionError names `want` (default: `until`), the last value, the last few distinct
        values before it and the wait.
        """
        check = until if callable(until) else (lambda v: v == until)
        start = time.monotonic()
        seen = []  # distinct values in order; seen[-1] is always the latest sample

        def ok():
            value = read(self.snapshot())
            stable = bool(seen) and value == seen[-1]
            if not stable:
                seen.append(value)
            return check(value) and (stable or not settled)

        if not poll(ok, timeout=timeout):
            if want is None:
                want = "a value meeting the condition" if callable(until) else repr(until)
            history = ", ".join(map(str, seen[-6:]))
            raise AssertionError(
                f"{name}: want {want}, last saw {seen[-1]} after "
                f"{time.monotonic() - start:.1f} s (seen: {history})"
            )
        return seen[-1]

    def expect_colour(self, name, want, x=None, y=None, timeout=3.0, settled=True):
        """Poll the pixel at (x, y) (default the centre) until it is `want`."""
        want = tuple(want)
        where = "the centre" if x is None else f"({x}, {y})"
        return self.expect(
            name,
            lambda shot: shot.pixel(x, y),
            want,
            timeout=timeout,
            settled=settled,
            want=f"{want} at {where}",
        )

    def expect_width(self, name, colour, box, until=None, timeout=3.0, tolerance=TOLERANCE):
        """Poll the width of the region near `colour` in `box` (x0, y0, x1, y1, end exclusive)
        until two samples agree and meet `until` (an exact width or a predicate; default above
        0), and return it. This is bash `settled_width` with a check.
        """
        colour = tuple(colour)
        if until is None:
            until, want = (lambda w: w > 0), "a width above 0"
        else:
            want = None if callable(until) else f"width {until}"
        return self.expect(
            name,
            lambda shot: shot.colour_width(colour, box, tolerance),
            until,
            timeout=timeout,
            want=f"{want or 'a width meeting the condition'} of {colour} in box {box}",
        )

    def start_desktop(self):
        """A green desktop (swaybg) behind the lock, so a test can see the compositor unlock.

        The locker exiting 0 is not enough: if unlock_and_destroy never reaches sway, it stays
        locked and shows its own red.
        """
        self.spawn(["swaybg", "-c", "#00ff00"])
        self.expect_colour("swaybg drew the desktop", DESKTOP, 5, 5)

    def expect_unlocked(self, name):
        """After an unlock: the desktop is visible again."""
        self.expect_colour(f"{name}: the compositor unlocked (desktop visible)", DESKTOP, 5, 5)

    # The locker.

    def locker(self, config=None, args=(), env=None, log="lock.log"):
        """Start the locker; `config` is KDL text written to lock.kdl and passed with -c."""
        argv = [self.binary]
        if config is not None:
            conf = self.path("lock.kdl")
            conf.write_text(config)
            argv += ["-c", conf]
        return Locker(self, self.spawn([*argv, *args], log=log, env=env), log)

    # Cleanup.

    def close(self):
        """Stop every process this test started, then sway, then remove the runtime dir."""
        with hold_signals():
            clean_dir(self.dir, self.procs, self.sway)
            self._stop_reaper()


_held = threading.local()


@contextlib.contextmanager
def hold_signals():
    """Hold Ctrl-C, SIGTERM, SIGHUP and the pytest-timeout alarm for the duration of the block,
    then re-raise them in arrival order (the first whose handler raises ends the rest).

    Each signal's handler is swapped for one that only records it, which works process wide:
    Python runs handlers in the main thread whichever thread the kernel picked (an xdist worker
    has an execnet thread, so a pthread_sigmask on the main thread alone does not hold them).
    Only the main thread can swap handlers; elsewhere no handler can interrupt the block anyway.
    Nested blocks hold until the outermost one ends.
    """
    if threading.current_thread() is not threading.main_thread() or getattr(_held, "on", False):
        yield
        return
    pending = []
    old = {
        sig: signal.signal(sig, lambda signum, frame: pending.append(signum))
        for sig in CLEANUP_SIGNALS
    }
    _held.on = True
    try:
        yield
    finally:
        _held.on = False
        for sig, handler in old.items():
            signal.signal(sig, handler)
        for sig in pending:
            signal.raise_signal(sig)


def clean_dir(d, procs, sway=None):
    """Stop `procs` and everything else running on runtime dir `d`, sway last, then remove `d`.

    Besides `procs` that is cmd[] children in their own process groups, a daemonized locker and
    helpers a tool started: every process of this user whose environment names `d`. None of them
    can belong to the caller, whose environment never names this dir.
    """
    # A second Ctrl-C, SIGTERM or the pytest-timeout alarm must not cut the cleanup short.
    with hold_signals():
        _stop([p for p in procs if p is not sway and p.poll() is None])
        keep = {sway.pid} if sway is not None and sway.poll() is None else set()
        _kill_pids([pid for pid in _strays(d) if pid not in keep])
        if keep:
            _stop([sway])
        _kill_pids(_strays(d))
        if d.parent == Path("/tmp") and d.name.startswith(DIR_PREFIX):
            shutil.rmtree(d, ignore_errors=True)


def _strays(d):
    """Live processes of this user on runtime dir `d`: by environment, or by command line (sway
    has file capabilities, so its environ is not readable)."""
    env_needle = f"XDG_RUNTIME_DIR={d}\0".encode()
    arg_needle = f"{d}/".encode()
    me, uid, found = os.getpid(), os.getuid(), []
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit() or int(entry.name) == me:
            continue
        pid = int(entry.name)
        try:
            if entry.stat().st_uid != uid:
                continue
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                hit = arg_needle in f.read()
            if not hit:
                with open(f"/proc/{pid}/environ", "rb") as f:
                    hit = env_needle in f.read()
        except OSError:
            continue
        if hit and alive(pid):
            found.append(pid)
    return found


def _stop(procs, grace=2.0):
    """SIGTERM each process group, SIGKILL what is left after `grace` seconds, reap them all."""
    for p in procs:
        _signal_group(p.pid, signal.SIGTERM)
    deadline = time.monotonic() + grace
    for p in procs:
        try:
            p.wait(max(0.0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            pass
    for p in procs:
        if p.poll() is None:
            _signal_group(p.pid, signal.SIGKILL)
            p.wait()


def _signal_group(pid, sig):
    try:
        os.killpg(pid, sig)
    except (ProcessLookupError, PermissionError):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            pass


def _kill_pids(pids):
    for pid in pids:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    poll(lambda: not any(alive(pid) for pid in pids), timeout=2)


class Locker:
    """A running swaywardlock."""

    def __init__(self, env, proc, log):
        self.env = env
        self.proc = proc
        self.pid = proc.pid
        self.log = log

    def log_text(self):
        return self.env.log_text(self.log)

    def wait_log(self, text, timeout=5.0):
        """Wait until the log contains `text`."""
        return poll(lambda: text in self.log_text(), timeout=timeout)

    def wait_locked(self, name, timeout=5.0):
        if not self.wait_log("session locked", timeout):
            raise AssertionError(f"{name}: never locked\n{self.log_text()}")

    def alive(self):
        return self.proc.poll() is None and alive(self.pid)

    def signal(self, sig):
        self.proc.send_signal(sig)

    def type(self, text, enter=True):
        self.env.wtype(text, *(["-k", "Return"] if enter else []))

    def wait_exit(self, timeout=5.0):
        """The exit status once the locker exits (128 + N for signal N), or None on a timeout."""
        try:
            code = self.proc.wait(timeout)
        except subprocess.TimeoutExpired:
            return None
        return code if code >= 0 else 128 - code
