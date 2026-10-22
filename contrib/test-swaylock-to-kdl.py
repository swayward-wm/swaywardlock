#!/usr/bin/env python3
"""Tests for contrib/swaylock-to-kdl.

Each case translates a swaylock config or command line, checks the KDL, and, when a swaywardlock
binary is available ($SWAYWARDLOCK, default target/debug/swaywardlock), checks that
`swaywardlock validate` accepts the output.
"""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parents[1]
SCRIPT = Path(__file__).with_name("swaylock-to-kdl")
BIN = Path(os.environ.get("SWAYWARDLOCK", ROOT / "target/debug/swaywardlock"))

# Every swaylock styling option, in config-file form. A config that sets them all must translate
# with no warnings and validate.
EVERY_OPTION = """
# comment lines and blank lines are skipped

color=1e1e2e
image=/tmp/wall.png
scaling=fill
ignore-empty-password
show-failed-attempts
show-keyboard-layout
disable-caps-lock-text
indicator-caps-lock
indicator-idle-visible
line-uses-ring
font=JetBrains Mono
font-size=24
indicator-radius=100
indicator-thickness=7
indicator-x-position=200
indicator-y-position=300
bs-hl-color=db3300
caps-lock-bs-hl-color=db3300ff
caps-lock-key-hl-color=#33db00
key-hl-color=33db00
separator-color=00000000
layout-bg-color=000000c0
layout-border-color=00000000
layout-text-color=ffffff
inside-color=00000088
inside-clear-color=e5a445
inside-caps-lock-color=000000
inside-ver-color=0072ff
inside-wrong-color=fa0000
line-color=000000
line-clear-color=000000
line-caps-lock-color=000000
line-ver-color=000000
line-wrong-color=000000
ring-color=337d00
ring-clear-color=e5a445
ring-caps-lock-color=e5a445
ring-ver-color=3300ff
ring-wrong-color=7d3300
text-color=e5a445
text-clear-color=000000
text-caps-lock-color=e5a445
text-ver-color=000000
text-wrong-color=000000
daemonize
"""


def run(*args, stdin=None):
    return subprocess.run([SCRIPT, *args], text=True, capture_output=True, check=True, input=stdin)


class Translate(unittest.TestCase):
    def config(self, text):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "config"
            p.write_text(text)
            return run(str(p))

    def validate(self, kdl):
        if not BIN.exists():
            self.skipTest(f"{BIN} not built")
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "config.kdl"
            p.write_text(kdl)
            r = subprocess.run([BIN, "validate", "-c", p], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"swaywardlock rejected:\n{kdl}\n{r.stderr}{r.stdout}")

    def test_every_option_translates_and_validates(self):
        r = self.config(EVERY_OPTION)
        self.assertNotIn("warning:", r.stderr)
        self.assertIn("command: swaywardlock -f", r.stderr)
        kdl = r.stdout
        for needle in [
            'color "#1e1e2eff"',
            'path "/tmp/wall.png"',
            'fit "cover"',
            "ignore-empty-input",
            "radius 100",
            "thickness 7",
            'halign "left"',
            'valign "top"',
            # Centre at (200, 300) with radius+thickness 107 from the top-left corner.
            "position 93 -193",
            'line-uses "ring"',
            'show-keyboard-layout "always"',
            "show-caps-lock-text false",
            'state "input" inside="#00000088" line="#000000ff" ring="#337d00ff" text="#e5a445ff"',
            'state "verifying"',
            'caps-lock-key-highlight "#33db00ff"',
            'layout bg="#000000c0" border="#00000000" text="#ffffffff"',
        ]:
            self.assertIn(needle, kdl)
        self.validate(kdl)

    def test_command_line_with_short_flags(self):
        r = run("--args", "swaylock -fFe -c 000000 -s center -i DP-1:/a.png --ready-fd 3 -u")
        self.assertIn("command: swaywardlock -f --ready-fd 3", r.stderr)
        self.assertNotIn("warning:", r.stderr)
        self.assertIn('background output="DP-1"', r.stdout)
        self.assertIn('fit "center"', r.stdout)
        self.assertNotIn("ring-indicator", r.stdout)  # -u
        self.validate(r.stdout)

    def test_unknown_options_warn_and_are_never_silent(self):
        r = run("--args", "swaylock --frobnicate -Z --ring-color nothex")
        self.assertIn("warning: --frobnicate", r.stderr)
        self.assertIn("warning: -Z", r.stderr)
        self.assertIn("warning: --ring-color: invalid colour", r.stderr)
        self.validate(r.stdout)

    def test_empty_config_gives_the_default_look(self):
        r = self.config("")
        self.assertNotIn("warning:", r.stderr)
        self.validate(r.stdout)

    def test_bare_value_options_warn_instead_of_crashing(self):
        r = self.config("image\ncolor\nready-fd\n")
        for name in ("image", "color", "ready-fd"):
            self.assertIn(f"warning: --{name}: missing value", r.stderr)
        self.assertNotIn("--ready-fd", r.stderr.split("command:")[1].splitlines()[0])
        self.validate(r.stdout)

    def test_image_paths_get_swaylocks_expansion(self):
        os.environ["SWAYLOCK_TEST_DIR"] = "/srv/walls"
        r = run("--args", "swaylock -i $SWAYLOCK_TEST_DIR/a.png -i DP-2:~/b.png")
        self.assertIn('path "/srv/walls/a.png"', r.stdout)
        self.assertIn('path "~/b.png"', r.stdout)  # swaywardlock expands ~ itself
        self.assertNotIn("warning:", r.stderr)
        r = run("--args", "swaylock -i '*.png'")
        self.assertIn("warning: --image: '*.png' needs shell expansion", r.stderr)

    def test_c_number_parsing_and_flag_precedence(self):
        r = run("--args", "swaylock --indicator-radius 0x40 --indicator-thickness 010 -K -k")
        self.assertIn("radius 64", r.stdout)
        self.assertIn("thickness 8", r.stdout)
        # --hide-keyboard-layout wins whatever the order, as in swaylock.
        self.assertIn('show-keyboard-layout "never"', r.stdout)
        r = run("--args", "swaylock -i /a.png -s solid_color -s fit")
        self.assertIn('path "/a.png"', r.stdout)

    def test_solid_color_drops_images(self):
        r = run("--args", "swaylock -i /a.png --scaling solid_color -c 112233")
        self.assertNotIn("path", r.stdout)
        self.assertIn('color "#112233ff"', r.stdout)


if __name__ == "__main__":
    unittest.main()
