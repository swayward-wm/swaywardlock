"""Process/protocol contracts: t_* counterparts retained until parity review.

These need a real compositor: an exit code alone cannot prove protocol ordering,
fd ownership or command-process cleanup. Cleanup checks run before LockEnv.close.
"""

import contextlib
import os
import pty
import re
import select
import signal
import socket
import subprocess
import time
from pathlib import Path

from lib.lockenv import Locker, ROOT
from lib.poll import alive, poll

GREY = (163, 163, 163)


def _launch(env, argv, log, **kw):
    """Narrow launcher for descriptors not supported by LockEnv.spawn."""
    with env.path(log).open("wb") as out:
        proc = subprocess.Popen(
            [str(a) for a in argv], env=env.env, stdin=subprocess.DEVNULL,
            stdout=out, stderr=subprocess.STDOUT, start_new_session=True, **kw,
        )
    env.procs.append(proc)
    return Locker(env, proc, log)


def _exit(lock, code, name, timeout=5):
    got = lock.wait_exit(timeout)
    assert got == code, f"{name} (got {got}, want {code})\n{lock.log_text()}"


def _pid(env, name):
    def read():
        try:
            return int(env.path(name).read_text())
        except (OSError, ValueError):
            return None
    pid = poll(read)
    assert pid, f"command not started: {name}"
    return pid


def _gone(pid, name, timeout=.3):
    assert poll(lambda: not alive(pid), timeout=timeout), f"{name}: child {pid} left running"


def _commands(env, prefix, background=False):
    d = env.dir
    config = f'animations {{ off; }}\nlabel {{ text "cmd[] echo $$ > {d}/{prefix}-child; exec sleep 300"; }}\n'
    if background:
        config += f'label {{ text "cmd[] sleep 302 >/dev/null & echo $! > {d}/{prefix}-bg; echo started"; }}\n'
    path = env.path(prefix + ".kdl")
    path.write_text(config)
    return path


@contextlib.contextmanager
def _proxy(env, name, *args):
    log = name + "-proxy.log"
    env.path("wayland-inject-0").unlink(missing_ok=True)
    proc = env.spawn([env.contrib("wl-inject-finished"), *args, env.path(log)],
                     log=name + "-launcher.log")
    try:
        assert poll(lambda: "ready" in env.log_text(log)), f"proxy never ready: {env.log_text(log)}"
        yield log
    finally:
        if proc.poll() is None:
            proc.terminate()
        proc.wait(5)


def test_validate(lock_env):
    env = lock_env
    bad = env.path("bad.kdl")
    bad.write_text('ring-indicator { radius 0; }')
    lock = _launch(env, [env.binary, "validate", "-c", bad], "validate-bad.log")
    _exit(lock, 1, "validate: bad config exit code")
    assert re.search("radius|between", lock.log_text()), "validate: error explains the value"
    for name, text, message in (
        ("good", 'ring-indicator { radius 20; }', "validate: good config exit code"),
        ("pamoff", 'auth { pam { off; }; }', "validate: pam { off; } is accepted"),
    ):
        path = env.path(name + ".kdl")
        path.write_text(text)
        _exit(_launch(env, [env.binary, "validate", "-c", path], name + ".log"), 0, message)
    blocks = re.findall(r"```kdl\n(.*?)```", (ROOT / "docs/specs/2026-10-19-swaywardlock.md").read_text(), re.S)
    failures = []
    paths = []
    for i, block in enumerate(blocks):
        path = env.path(f"spec{i}.kdl")
        path.write_text(block)
        paths.append(path)
    paths += sorted((ROOT / "examples").glob("*.kdl"))
    for i, path in enumerate(paths):
        lock = _launch(env, [env.binary, "validate", "-c", path], f"example-{i}.log")
        if lock.wait_exit() != 0:
            failures.append(f"{path}: {lock.log_text()}")
    assert len(blocks) >= 7 and not failures, f"validate: the spec's {len(blocks)} examples and the example configs: {failures}"
    path = env.path("glow.kdl")
    path.write_text('ring-indicator { glow 4; }')
    _exit(_launch(env, [env.binary, "validate", "-c", path], "glow.log"), 1,
          "validate: glow with the swaylock style is refused")


def test_no_wayland_egl(desktop):
    env = desktop
    lib = env.path("fake-egl")
    lib.mkdir()
    result = env.run(["cc", "-shared", "-o", lib / "libwayland-egl.so.1", "-x", "c", "/dev/null"], check=False)
    assert result.returncode == 0, f"no wayland-egl: needs a C compiler: {result.stderr!r}"
    lock = env.locker(env={"LD_LIBRARY_PATH": str(lib)}, log="fake-egl.log")
    lock.wait_locked("no wayland-egl")
    env.expect_colour("no wayland-egl: a solid frame covers the desktop", GREY)
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "no wayland-egl: SIGUSR1 unlocks")
    env.expect_unlocked("no wayland-egl")


def test_bad_config_still_locks(desktop):
    env = desktop
    lock = env.locker('ring-indicator { radius 0; }', log="bad-lock.log")
    lock.wait_locked("bad config")
    assert "warn" in lock.log_text().lower() and "default" in lock.log_text(), "bad config: the log warns about the defaults"
    env.expect_colour("bad config: default look covers the desktop", GREY)
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "bad config: SIGUSR1 unlocks")
    env.expect_unlocked("bad config")
    lock = env.locker(args=["-c", env.path("does-not-exist.kdl")], log="missing-config.log")
    lock.wait_locked("missing config file")
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "missing config file: SIGUSR1 unlocks")


def test_sigterm(lock_env):
    env = lock_env
    config = _commands(env, "term", background=True)
    lock = _launch(env, [env.binary, "-c", config], "term.log")
    lock.wait_locked("sigterm")
    child, bg = _pid(env, "term-child"), _pid(env, "term-bg")
    # Ensure the background command's shell has exited, not just that its pid file exists.
    assert alive(bg), "sigterm: background job not started"
    time.sleep(.3)
    assert alive(child), "sigterm: label command running"
    lock.signal(signal.SIGTERM)
    _exit(lock, 143, "sigterm: default action (killed by SIGTERM)")
    _gone(child, "sigterm: cmd[] children killed")
    _gone(bg, "sigterm: background job of a cmd[] label killed")

    config = _commands(env, "nohup", background=True)
    lock = _launch(env, ["sh", "-c", 'trap "" HUP; exec "$@"', "sh", env.binary, "-c", config], "nohup.log")
    lock.wait_locked("sigterm: nohup run")
    hup_child, hup_bg = _pid(env, "nohup-child"), _pid(env, "nohup-bg")
    lock.signal(signal.SIGHUP)
    # A negative observation window: do not replace with eventual-alive polling.
    time.sleep(.5)
    assert lock.alive(), "sigterm: SIGHUP ignored under nohup stays ignored"
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "sigterm: SIGUSR1 unlocks after the ignored SIGHUP")
    _gone(hup_child, "sigterm: nohup cmd[] children killed")
    _gone(hup_bg, "sigterm: nohup background job killed")

    config = _commands(env, "preview", background=True)
    lock = _launch(env, ["env", "--default-signal=INT", env.binary, "--preview", "-c", config], "preview-int.log")
    child, bg = _pid(env, "preview-child"), _pid(env, "preview-bg")
    lock.signal(signal.SIGINT)
    _exit(lock, 130, "sigterm: preview killed by SIGINT")
    _gone(child, "sigterm: preview's cmd[] children killed on SIGINT")
    _gone(bg, "sigterm: preview's background job killed on SIGINT")


def test_sigusr1(desktop):
    env = desktop
    lock = env.locker(log="usr1.log")
    lock.wait_locked("sigusr1")
    env.expect_colour("sigusr1: the lock covers the desktop", (163, 163, 163), 5, 5)
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "sigusr1: exit code")
    env.expect_unlocked("sigusr1")
    for i in range(1, 4):
        lock = env.locker(log=f"storm-{i}.log")
        lock.wait_locked(f"sigusr1: storm run {i}")
        # Signal continuously through teardown; poll() reaps before any pid can be reused.
        deadline = time.monotonic() + 5
        while lock.proc.poll() is None and time.monotonic() < deadline:
            try:
                lock.signal(signal.SIGUSR1)
                lock.signal(signal.SIGUSR2)
            except ProcessLookupError:
                break
        _exit(lock, 0, f"sigusr1: exit code with signals during teardown (run {i})", timeout=.1)


def test_early_unlock(desktop):
    env = desktop
    trigger = env.path("release-locked")
    with _proxy(env, "hold", "--hold-locked", trigger) as plog:
        lock = env.locker('animations { off; }', env={"WAYLAND_DISPLAY": "wayland-inject-0"}, log="early.log")
        assert poll(lambda: "held locked" in env.log_text(plog), timeout=5), "early unlock: the proxy never saw locked"
        lock.signal(signal.SIGUSR1)
        assert lock.wait_log("deferring", timeout=3), "early unlock: SIGUSR1 before locked is deferred"
        time.sleep(.3)  # Must stay alive throughout the withheld-locked window.
        assert lock.alive(), "early unlock: still running until locked"
        trigger.touch()
        _exit(lock, 0, "early unlock: exit code")
        assert "session locked" in lock.log_text(), "early unlock: locked first"
        assert poll(lambda: "request unlock_and_destroy" in env.log_text(plog).splitlines()), "early unlock: answered locked with unlock_and_destroy"
        env.expect_unlocked("early unlock")


def test_ready_fd(lock_env):
    env = lock_env
    # Shell redirection fixes fd 9 without mutating the threaded pytest parent's fd table.
    for name, config in (("ready", None), ("ready-child", f'label {{ text "cmd[] ls /proc/$$/fd > {env.dir}/child-fds; echo x"; }}\nanimations {{ off; }}')):
        argv = [env.binary]
        if config:
            path = env.path("ready-child.kdl")
            path.write_text(config)
            argv += ["-c", path]
        argv += ["--ready-fd", "9"]
        ready = env.path(name)
        lock = _launch(env, ["sh", "-c", 'exec 9>"$1"; shift; exec "$@"', "sh", ready, *argv], name + ".log")
        lock.wait_locked("ready-fd" if not config else "ready-fd: cmd run")
        if not config:
            assert poll(lambda: ready.read_bytes() == b"\n"), "ready-fd: newline written"
        else:
            fds = env.path("child-fds")
            assert poll(lambda: fds.exists() and fds.stat().st_size), "ready-fd: the cmd[] child never ran"
            assert "9" not in fds.read_text().splitlines(), "ready-fd: not inherited by cmd[] children"
        lock.signal(signal.SIGUSR1)
        _exit(lock, 0, "ready-fd: unlocks")
    lock = _launch(env, ["sh", "-c", 'exec 4>&-; exec "$@"', "sh", env.binary, "--ready-fd", "4"], "closed-fd.log")
    _exit(lock, 1, "ready-fd: a closed descriptor exits 1")
    assert "session locked" not in lock.log_text(), "ready-fd: a closed descriptor is refused before locking"


def test_fd_limit(lock_env):
    env = lock_env
    lock = _launch(env, ["bash", "-c", 'ulimit -Sn 16; exec "$0"', env.binary], "soft-limit.log")
    lock.wait_locked("fd limit: soft 16")
    time.sleep(.5)  # Allow rendering/descriptor use after locked at the raised limit.
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "fd limit: a soft limit of 16 is raised and the lock works")
    lock = _launch(env, ["bash", "-c", 'ulimit -n 16; exec "$0"', env.binary], "hard-limit.log")
    lock.wait_locked("fd limit: hard 16")
    assert "open file limit" in lock.log_text(), "fd limit: the log warns"
    lock.signal(signal.SIGUSR1)
    _exit(lock, 0, "fd limit: a hard limit of 16 still locks")


def test_compositor_death(lock_env):
    env = lock_env
    lock = env.locker(log="compositor-death.log")
    lock.wait_locked("compositor death")
    env.sway.terminate()  # Only this fixture's compositor, never a pgrep match.
    env.sway.wait(5)
    _exit(lock, 3, "compositor death: exit code after locking")


def test_finished_after_locked(lock_env):
    env = lock_env
    with _proxy(env, "finished") as plog:
        lock = env.locker(env={"WAYLAND_DISPLAY": "wayland-inject-0"}, log="finished-lock.log")
        _exit(lock, 2, "finished after locked: exit code")
        assert "injected finished" in env.log_text(plog), "finished after locked: finished injected"
        assert poll(lambda: "request unlock_and_destroy" in env.log_text(plog).splitlines()), "finished after locked: answered with unlock_and_destroy"
        time.sleep(.3)  # Retain upstream protocol-error observation window.
        lines = env.log_text(plog).splitlines()
        assert "request destroy" not in lines, f"finished after locked: requests: {lines}"
        assert not any(line.startswith("error") for line in lines), "finished after locked: no protocol error"


def test_protocol_error(lock_env):
    env = lock_env
    for mode in ("close", "keep-open"):
        args = ["--error"] + (["--keep-open"] if mode == "keep-open" else [])
        with _proxy(env, mode, *args) as plog:
            lock = env.locker(env={"WAYLAND_DISPLAY": "wayland-inject-0"}, log=f"error-{mode}-lock.log")
            assert poll(lambda: "injected error" in env.log_text(plog), timeout=5), f"protocol error ({mode}): never injected"
            start = time.monotonic()
            code = lock.wait_exit(timeout=2)
            ms = (time.monotonic() - start) * 1000
            assert code is not None, f"protocol error ({mode}): still running 2 s after the error ({lock.log_text().count('Protocol error')} log lines)"
            assert code == 3, f"protocol error ({mode}): exit code (got {code}, want 3)"
            assert ms <= 2000, f"protocol error ({mode}): exited after {ms:.0f} ms"
            assert lock.log_text().count("Protocol error") < 10, f"protocol error ({mode}): no busy loop"


def test_daemonize(lock_env):
    env = lock_env
    lock = env.locker(args=["-f"], log="daemon-parent.log")
    _exit(lock, 0, "daemonize: parent exit code")
    assert "session locked" in lock.log_text(), "daemonize: locked before the parent exited"
    decoy = env.spawn(["bash", "-c", 'exec -a "$1" sleep 30', "bash", env.binary + " -f"],
                      env={"XDG_RUNTIME_DIR": "/nonexistent"}, log="daemon-decoy.log")

    def discover():
        found = []
        needle = f"XDG_RUNTIME_DIR={env.dir}".encode()
        for path in Path("/proc").iterdir():
            if not path.name.isdigit():
                continue
            try:
                args = (path / "cmdline").read_bytes().split(b"\0")
                if args[:2] == [os.fsencode(env.binary), b"-f"] and needle in (path / "environ").read_bytes().split(b"\0") and alive(int(path.name)):
                    found.append(int(path.name))
            except OSError:
                pass
        return found

    assert decoy.poll() is None, "daemonize: decoy did not start"
    children = poll(discover)
    assert len(children) == 1 and children[0] != decoy.pid, f"daemonize: child keeps running ({children})"
    child = children[0]  # Exact executable + private runtime identifies our detached child.
    assert os.getsid(child) == child, "daemonize: the daemon leads its own session"
    assert poll(lambda: [os.readlink(f"/proc/{child}/fd/{fd}") for fd in (0, 1, 2)] == ["/dev/null"] * 3), "daemonize: stdio on /dev/null"
    os.kill(child, signal.SIGUSR1)
    _gone(child, "daemonize: the daemon exits on SIGUSR1", timeout=5)
    assert decoy.poll() is None, "daemonize: the decoy was left alone"
    decoy.terminate()
    decoy.wait(5)


def test_second_locker(lock_env):
    env = lock_env
    first = env.locker(log="first-locker.log")
    first.wait_locked("second locker: first")
    second = env.locker(log="second-locker.log")
    _exit(second, 2, "second locker: exit code")
    assert first.alive(), "second locker: first stays locked"
    # Earlier session-locked markers cannot satisfy the second invocation's readiness.
    assert not second.wait_log("session locked", timeout=.1), "second locker: stale locked marker"
    first.signal(signal.SIGUSR1)
    _exit(first, 0, "second locker: first unlocks")


@contextlib.contextmanager
def _broken_stderr(env, mode):
    if mode == "fifo":
        r, w = os.pipe()
    elif mode == "sock":
        a, b = socket.socketpair()
        r, w = a.detach(), b.detach()
    else:
        r, w = pty.openpty()
    log = f"stderr-{mode}.log"
    try:
        proc = subprocess.Popen([env.binary, "-v"], env=env.env, stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=w, start_new_session=True)
        env.procs.append(proc)
    except BaseException:
        os.close(r)
        raise
    finally:
        os.close(w)
    lock = Locker(env, proc, log)
    try:
        deadline = time.monotonic() + 5
        with env.path(log).open("wb") as out:
            buf = b""
            while b"session locked" not in buf and time.monotonic() < deadline:
                if not select.select([r], [], [], max(0, deadline - time.monotonic()))[0]:
                    break
                try:
                    chunk = os.read(r, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                out.write(chunk)
                buf = buf[-64:] + chunk
        assert b"session locked" in buf, f"broken stderr ({mode}): never locked\n{lock.log_text()}"
        os.close(r)
        r = None
        yield lock
    finally:
        if r is not None:
            os.close(r)
        # Only fixture cleanup, after the test asserted the production exit/status.
        if proc.poll() is None:
            proc.kill()
        proc.wait(5)


def test_broken_stderr(lock_env):
    env = lock_env
    for mode in ("fifo", "sock", "pty"):
        with _broken_stderr(env, mode) as lock:
            time.sleep(.2)
            env.wtype("-s", "300", "wrong", "-k", "Return")
            time.sleep(1)  # Failed auth/log writes after the reader closes.
            lock.signal(signal.SIGUSR2)
            time.sleep(.3)  # Negative survival window, not eventual readiness.
            assert lock.alive(), f"broken stderr ({mode}): a wrong password and SIGUSR2 keep the locker alive"
            if mode == "pty":
                lock.signal(signal.SIGUSR1)
                what = "SIGUSR1"
            else:
                env.wtype("-s", "300", "swaywardlock-test", "-k", "Return")
                what = "password"
            _exit(lock, 0, f"broken stderr ({mode}): {what} unlocks, exit code")


def test_no_display(lock_env):
    lock = lock_env.locker(env={"WAYLAND_DISPLAY": "does-not-exist"}, log="no-display.log")
    _exit(lock, 1, "no display: exit code")
