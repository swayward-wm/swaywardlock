"""The contrib test tools, one fixture each (see conftest.py).

The area tests use private wrappers around these tools. Shared fixture names remain explicit
placeholders; use the area wrapper or lock_env.spawn for tools without a shared wrapper.
Their command lines are in the tools' own docstrings.
"""


def not_ported(tool, area):
    raise NotImplementedError(
        f"contrib/{tool} has no shared pytest wrapper; the {area} tests use private wrappers. "
        f"Use lock_env.spawn([lock_env.contrib({tool!r}), ...]) or the area wrapper."
    )


# Tool name and the pytest areas that use it.
NOT_PORTED = {
    "vkbd": "keyboard, keyboard-layout, otp",
    "vptr": "pointer",
    "fake-fprintd": "fingerprint",
    "forge-fprint": "fingerprint",
    "wl-inject-finished": "finished-after-locked, early-unlock, protocol-error",
    "wl-drop-key-release": "keyboard-repeat",
    "clock-jump.c": "grace, clock-resume",
}
