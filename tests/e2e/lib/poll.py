"""Waiting and process state."""

import time


def poll(cond, timeout=3.0, interval=0.1):
    """Call `cond` until it returns something truthy or `timeout` seconds pass.

    Returns the last value `cond` returned, so a caller can report what it saw last.
    """
    deadline = time.monotonic() + timeout
    while True:
        value = cond()
        if value or time.monotonic() >= deadline:
            return value
        time.sleep(interval)


def alive(pid):
    """True if `pid` is a live process. A zombie counts as gone.

    In a container whose pid 1 is not an init (CI without --init), orphaned children of the
    locker are reparented to a process that never reaps them and stay zombies.
    """
    try:
        with open(f"/proc/{pid}/stat") as f:
            stat = f.read()
    except OSError:
        return False
    # The state follows the command name, which is in parentheses and may contain spaces.
    return stat.rpartition(")")[2].split()[0] != "Z"
