"""Nothing outlives a test: a child pytest runs probes/probe_hang.py and is killed mid-test."""

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from lib.lockenv import _strays, clean_dir
from lib.poll import alive, poll

HERE = Path(__file__).parent


def _default_signals():
    """Runs in the child before exec. A pytest started as a background job of a non-interactive
    shell has SIGINT ignored, and the probe would inherit that and ignore the test's signals."""
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, signal.SIG_DFL)


class Probe:
    """A child pytest running probe_hang.py, in its own process group."""

    def __init__(self, tmp_path, binary, xdist):
        self.ready = tmp_path / "ready"
        self.reaped = tmp_path / "reaped"
        self.out = tmp_path / "pytest.out"
        argv = [sys.executable, "-m", "pytest", str(HERE / "probes/probe_hang.py")]
        argv += ["-p", "no:cacheprovider", "-q", "--timeout", "0", "--swaywardlock", str(binary)]
        argv += ["-n", "1"] if xdist else []
        # Not this run's xdist worker identity or current test.
        env = {k: v for k, v in os.environ.items() if not k.startswith(("PYTEST_", "SWL_E2E_"))}
        env.update(SWL_E2E_PROBE_READY=str(self.ready), SWL_E2E_REAP_LOG=str(self.reaped))
        with open(self.out, "wb") as out:
            self.proc = subprocess.Popen(
                argv,
                cwd=HERE,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=out,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                preexec_fn=_default_signals,
            )
        self.dir = None

    def wait_ready(self):
        assert poll(lambda: self.ready.exists() and self.ready.read_text(), timeout=30), self.output()
        self.dir = Path(self.ready.read_text())
        # The test's pytest process: the plain run itself, or the xdist worker.
        self.owner = int((self.dir / "owner").read_text())
        strays = _strays(self.dir)
        assert len(strays) >= 4, f"probe: sway, locker, cmd child and helper running ({strays})"

    def output(self):
        return self.out.read_text(errors="replace")

    def wait_exit(self):
        try:
            self.proc.wait(30)
        except subprocess.TimeoutExpired:
            os.killpg(self.proc.pid, signal.SIGKILL)
            self.proc.wait()
            raise AssertionError(f"the child pytest did not exit\n{self.output()}")

    def left(self):
        """What is still there of the probe's dir: live processes on it and the dir itself."""
        return self._procs() + ([str(self.dir)] if self.dir.exists() else [])

    def _procs(self):
        procs = []
        for p in _strays(self.dir):
            try:
                procs.append(f"pid {p}: {Path(f'/proc/{p}/cmdline').read_bytes()!r}")
            except (FileNotFoundError, ProcessLookupError):
                pass  # gone between the scan and the read: the reaper is killing it
        return procs

    def reaped_by_reaper(self):
        return self.reaped.exists() and str(self.dir) in self.reaped.read_text().split()

    def wait_reaped(self):
        """The reaper logs after it removed the dir, so the dir can be gone a moment before."""
        return poll(self.reaped_by_reaper, timeout=10)

    def close(self):
        if self.proc.poll() is None:
            os.killpg(self.proc.pid, signal.SIGKILL)
            self.proc.wait()
        if self.dir is not None:
            clean_dir(self.dir, [])


@pytest.fixture
def probe(tmp_path, binary):
    made = []

    def start(xdist):
        made.append(Probe(tmp_path, binary, xdist))
        return made[-1]

    yield start
    for p in made:
        p.close()


@pytest.mark.parametrize("xdist", [False, True], ids=["plain", "xdist"])
def test_sigkill_of_pytest_leaves_nothing(probe, xdist):
    p = probe(xdist)
    p.wait_ready()
    # Every pytest process of the run at once (plain: the run; xdist: main and worker), so no
    # teardown and no replacement worker runs.
    os.killpg(p.proc.pid, signal.SIGKILL)
    p.wait_exit()
    assert poll(lambda: not p.left(), timeout=10), f"left after SIGKILL: {p.left()}"
    assert p.wait_reaped(), "the reaper cleaned up"


@pytest.mark.parametrize("sig", [signal.SIGTERM, signal.SIGINT], ids=["SIGTERM", "SIGINT"])
def test_second_signal_during_teardown_in_an_xdist_worker(probe, sig):
    p = probe(xdist=True)
    p.wait_ready()
    threads = len(os.listdir(f"/proc/{p.owner}/task"))
    assert threads > 1, "the worker has a second thread (execnet), which the signal can land on"
    # The first signal ends the test; teardown waits 2 s on the helper that ignores SIGTERM, and
    # the second arrives during it.
    os.kill(p.owner, sig)
    time.sleep(0.5)
    assert alive(p.owner), "the worker is still in teardown"
    os.kill(p.owner, sig)
    p.wait_exit()
    assert not p.left(), f"left after a second {sig.name} during teardown: {p.left()}\n{p.output()}"
    assert not p.reaped_by_reaper(), "the worker's own teardown cleaned up, not the reaper"


def test_left_tolerates_a_process_that_vanishes_after_the_scan(tmp_path, monkeypatch):
    p = Probe.__new__(Probe)
    p.dir = tmp_path
    gone = subprocess.Popen(["true"])
    gone.wait()  # reaped: /proc/<pid> does not exist
    monkeypatch.setitem(globals(), "_strays", lambda d: [gone.pid, os.getpid()])
    left = p.left()
    assert [x for x in left if x.startswith(f"pid {gone.pid}:")] == []
    assert any(x.startswith(f"pid {os.getpid()}:") for x in left)


def test_probe_resets_ignored_signals():
    """The child of a process that ignores SIGINT, SIGTERM and SIGHUP is not ignoring them."""
    code = (
        "import signal;"
        "print([signal.getsignal(s) != signal.SIG_IGN for s in"
        " (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)])"
    )
    old = {s: signal.signal(s, signal.SIG_IGN) for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
    try:
        out = subprocess.run(
            [sys.executable, "-c", code], capture_output=True, text=True, preexec_fn=_default_signals
        ).stdout
    finally:
        for s, h in old.items():
            signal.signal(s, h)
    assert out.strip() == "[True, True, True]"
