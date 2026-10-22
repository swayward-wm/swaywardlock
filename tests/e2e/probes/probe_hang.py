"""Not collected by `pytest tests/e2e`: test_cleanup.py runs it in a child pytest and kills it.

A test that hangs with sway, the locker, a cmd[] child and a helper that ignores SIGTERM (so a
teardown takes the 2 s grace and a second signal can arrive during it). It writes its runtime dir
to $SWL_E2E_PROBE_READY once everything is up.
"""

import os
import time


def test_hang(lock_env):
    marker = lock_env.path("cmd-started")
    locker = lock_env.locker(f'label {{ text "cmd[] touch {marker}; sleep 777"; }}\n')
    locker.wait_locked("probe")
    lock_env.spawn(["sh", "-c", "trap '' TERM; sleep 778 & wait"])
    for _ in range(50):
        if marker.exists():
            break
        time.sleep(0.1)
    with open(os.environ["SWL_E2E_PROBE_READY"], "w") as f:
        f.write(str(lock_env.dir))
    time.sleep(300)
