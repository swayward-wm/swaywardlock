"""Fixtures for the end-to-end tests: a headless sway per test, and the locker on it.

Run inside the dev container after cargo build --features test-auth:

    pytest tests/e2e -n auto              # everything, in parallel
    pytest tests/e2e -k password          # one area or test
    pytest tests/e2e --keep-logs /tmp/l   # keep every test's logs

The binary is --swaywardlock PATH, else $SWAYWARDLOCK, else target/debug/swaywardlock.
"""

import os
import shutil
import signal
from pathlib import Path

import pytest

from lib import lockenv
from lib import tools
from lib.poll import alive


def pytest_addoption(parser):
    parser.addoption("--swaywardlock", help="the test-auth build to test")
    parser.addoption("--keep-logs", metavar="DIR", help="copy each test's logs to DIR/<test>/")


def pytest_configure(config):
    # SIGTERM (timeout(1) around the gate, a CI cancel) and SIGHUP (a closed terminal or
    # container session) unwind like Ctrl-C, so every fixture's teardown still stops its sway and
    # removes its dir.
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, _interrupt)


def _interrupt(signum, frame):
    raise KeyboardInterrupt(f"signal {signum}")


def pytest_sessionstart(session):
    # A backstop for the reaper (lib/reaper.py): clean up after a run or worker that died before
    # its teardown, if its reaper died too.
    sweep_stale_dirs()


def sweep_stale_dirs():
    """Remove runtime dirs whose pytest process and reaper are gone, with anything still running
    on them."""
    for d in Path("/tmp").glob(lockenv.DIR_PREFIX + "*"):
        try:
            owner = int((d / "owner").read_text())
        except (OSError, ValueError):
            continue
        try:
            reaper = int((d / "reaper").read_text())
        except (OSError, ValueError):
            reaper = None
        if not alive(owner) and not (reaper and alive(reaper)):
            lockenv.clean_dir(d, [])


@pytest.fixture(scope="session")
def binary(request):
    """The swaywardlock under test; the run fails at once if it is not a test-auth build."""
    path = (
        request.config.getoption("--swaywardlock")
        or os.environ.get("SWAYWARDLOCK")
        or lockenv.ROOT / "target/debug/swaywardlock"
    )
    problem = lockenv.check_binary(path)
    if problem:
        pytest.fail(problem, pytrace=False)
    return Path(path).resolve()


@pytest.fixture
def lock_env(binary, request):
    """A headless sway at 320x240 in a fresh runtime dir; see lib/lockenv.py LockEnv.

    Whatever ends the test (failure, pytest-timeout, KeyboardInterrupt, a second signal during
    teardown), teardown stops every process it started and removes the dir; if pytest itself is
    SIGKILLed, the dir's reaper (lib/reaper.py) does. A test that needs another size uses
    @pytest.mark.resolution("WxH").
    """
    mark = request.node.get_closest_marker("resolution")
    env = lockenv.LockEnv(binary, *(mark.args if mark else ()))
    try:
        yield env
    finally:
        # A second Ctrl-C or SIGTERM waits until the dir is gone, then acts.
        with lockenv.hold_signals():
            keep = request.config.getoption("--keep-logs")
            if keep:
                dest = Path(keep) / request.node.name
                dest.mkdir(parents=True, exist_ok=True)
                for log in env.dir.glob("*.log"):
                    shutil.copy(log, dest)
            env.close()


@pytest.hookimpl(wrapper=True)
def pytest_runtest_makereport(item, call):
    """On a failure, attach every log in the test's runtime dir to the report."""
    report = yield
    env = getattr(item, "funcargs", {}).get("lock_env")
    if report.failed and isinstance(env, lockenv.LockEnv) and env.dir.exists():
        for log in sorted(env.dir.glob("*.log")):
            report.sections.append((f"log {log.name}", env.log_text(log.name)))
    return report


@pytest.fixture
def desktop(lock_env):
    """A green swaybg desktop behind the lock; see LockEnv.start_desktop."""
    lock_env.start_desktop()
    return lock_env


def _not_ported(tool):
    def fixture():
        tools.not_ported(tool, tools.NOT_PORTED[tool])

    fixture.__doc__ = (
        f"contrib/{tool}: arrives with the {tools.NOT_PORTED[tool]} tests; asking for it fails."
    )
    return pytest.fixture(name=_fixture_name(tool))(fixture)


def _fixture_name(tool):
    return tool.removesuffix(".c").replace("-", "_")


for _tool in tools.NOT_PORTED:
    globals()[_fixture_name(_tool)] = _not_ported(_tool)
