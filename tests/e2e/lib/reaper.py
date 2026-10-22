"""Clean up a test's runtime dir if the pytest process that owns it dies first.

LockEnv starts this with stdin the read end of a pipe only the owner holds, and the dir in
$SWL_E2E_REAP_DIR. A byte before EOF means the owner cleaned up itself; EOF alone means the owner
is gone (SIGKILL, a crash) and the dir, sway, the locker and their children are removed here,
and the dir is appended to $SWL_E2E_REAP_LOG if that is set (test_cleanup.py checks who cleaned).
"""

import os
import signal
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from lib.lockenv import REAP_ENV, clean_dir


def main():
    # In its own session already; a stray terminal or group signal must not end it either.
    for sig in (signal.SIGINT, signal.SIGHUP, signal.SIGTERM):
        signal.signal(sig, signal.SIG_IGN)
    d = Path(os.environ[REAP_ENV])
    while True:
        try:
            data = os.read(0, 1)
            break
        except InterruptedError:
            continue
    if not data:
        clean_dir(d, [])
        log = os.environ.get("SWL_E2E_REAP_LOG")
        if log:
            with open(log, "a") as f:
                f.write(f"{d}\n")


if __name__ == "__main__":
    main()
