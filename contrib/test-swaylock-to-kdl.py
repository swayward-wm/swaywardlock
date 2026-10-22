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

    def test_unambiguous_long_option_prefixes_work_like_getopt_long(self):
        r = run("--args", "swaylock --daemon --indicator-idle --inside-color ff0000 --ignore-empty --show-f")
        self.assertIn("command: swaywardlock -f", r.stderr)
        self.assertNotIn("warning:", r.stderr)
        for needle in ["idle-visible", "ignore-empty-input", "show-failed-attempts", 'inside="#ff0000ff"']:
            self.assertIn(needle, r.stdout)
        self.validate(r.stdout)
        # The same in a config file, with values.
        r = self.config("indicator-idle\ninside-co=00ff00\nshow-failed\nindicator-rad=70\n")
        self.assertNotIn("warning:", r.stderr)
        for needle in ["idle-visible", "show-failed-attempts", 'inside="#00ff00ff"', "radius 70"]:
            self.assertIn(needle, r.stdout)
        self.validate(r.stdout)
        # An ambiguous prefix names the candidates and is skipped (its value too).
        r = run("--args", "swaylock --line-uses --ring 00ff00 -f")
        self.assertIn("warning: --line-uses: ambiguous (could be --line-uses-inside, --line-uses-ring)", r.stderr)
        self.assertIn("warning: --ring: ambiguous", r.stderr)
        self.assertIn("command: swaywardlock -f", r.stderr)
        self.assertNotIn("line-uses", r.stdout)
        self.validate(r.stdout)
        # An exact name wins over longer ones it prefixes.
        r = run("--args", "swaylock --font Mono --line-color 112233")
        self.assertNotIn("warning:", r.stderr)
        self.assertIn('font "Mono"', r.stdout)

    def test_swaylock_effects_options_name_the_swaywardlock_equivalent(self):
        r = run("--args", "swaylock -f -c 112233 --grace 2 --effect-blur 7x5 --fade-in 0.2 --clock -i /tmp/x.png")
        self.assertIn("command: swaywardlock -f -g 2\n", r.stderr)
        self.assertIn("--grace: a swaylock-effects option, carried over as -g 2", r.stderr)
        self.assertIn("blur {", r.stderr)
        self.assertIn("fade-in {", r.stderr)
        self.assertIn("$TIME", r.stderr)
        # The value of a skipped option is swallowed, not reported as a second warning.
        self.assertNotIn("unexpected argument", r.stderr)
        self.assertEqual(r.stderr.count("warning:"), 4, r.stderr)
        self.validate(r.stdout)
        # The config file form.
        r = self.config("grace=3\neffect-blur=7x5\n")
        self.assertIn("command: swaywardlock -g 3\n", r.stderr)

    def test_grace_too_large_for_the_command_line_is_not_carried_over(self):
        r = run("--args", "swaylock --grace 99999999999999999999")
        self.assertNotIn("command: swaywardlock -g", r.stderr)
        self.assertIn("too large", r.stderr)

    def test_unreadable_config_is_an_error_not_a_traceback(self):
        with tempfile.TemporaryDirectory() as d:
            bad = Path(d) / "latin1"
            bad.write_bytes(b"color=\xff\xfe\n")
            for target, reason in (("/nonexistent/config", "No such file"), (str(bad), "UTF-8")):
                r = subprocess.run([SCRIPT, target], text=True, capture_output=True)
                self.assertEqual(r.returncode, 1, r.stderr)
                self.assertEqual(r.stdout, "")
                self.assertIn(f"swaylock-to-kdl: cannot read {target}: ", r.stderr)
                self.assertIn(reason, r.stderr)
                self.assertNotIn("Traceback", r.stderr)

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

    def test_values_the_decoder_rejects_are_clamped_with_a_warning(self):
        # swaylock locks with all of these; swaywardlock refuses a config holding them.
        r = run("--args", "swaylock --indicator-idle-visible --indicator-radius 0 "
                          "--indicator-thickness 0 --font-size -5")
        self.assertIn("radius 1\n", r.stdout)
        self.assertIn("thickness 1\n", r.stdout)
        self.assertIn("font-size 0\n", r.stdout)
        for name in ("indicator-radius", "indicator-thickness", "font-size"):
            self.assertIn(f"warning: --{name}", r.stderr)
        self.validate(r.stdout)
        # A non-number is atoi/strtol 0, so it is clamped too.
        r = run("--args", "swaylock --indicator-radius abc --indicator-thickness 99999999")
        self.assertIn("radius 1\n", r.stdout)
        self.assertIn("thickness 10000\n", r.stdout)
        self.validate(r.stdout)
        # Huge positions would overflow the position range.
        r = run("--args", "swaylock --indicator-x-position 99999999 --indicator-y-position -99999999")
        self.assertIn("warning:", r.stderr)
        self.validate(r.stdout)
        # In-range values warn about nothing.
        r = run("--args", "swaylock --indicator-radius 100 --indicator-thickness 7 --font-size 0")
        self.assertNotIn("warning:", r.stderr)

    def test_relative_image_paths_are_made_absolute(self):
        with tempfile.TemporaryDirectory() as d:
            r = subprocess.run([SCRIPT, "--args", "swaylock -i wall.png -i DP-1:sub/b.jpg"],
                               text=True, capture_output=True, check=True, cwd=d)
            real = os.path.realpath(d)
            self.assertIn(f'path "{real}/wall.png"', r.stdout)
            self.assertIn(f'path "{real}/sub/b.jpg"', r.stdout)
            self.assertNotIn("warning:", r.stderr)

    def test_unsupported_image_extensions_warn(self):
        r = run("--args", "swaylock -i /a.svg -i DP-1:/b.JPG -i DP-2:/noext")
        self.assertIn("warning: --image: '/a.svg' does not have a supported image extension", r.stderr)
        self.assertIn("warning: --image: '/noext' does not have", r.stderr)
        self.assertNotIn("/b.JPG'", r.stderr)  # extension match is case-insensitive
        self.assertIn('path "/a.svg"', r.stdout)  # still translated: the decoder sniffs content

    def test_gdk_pixbuf_formats_swaywardlock_reads_do_not_warn(self):
        # Fedora's stock /etc/swaylock/config uses default.jxl.
        r = run("--args", "swaylock -i /a.jxl -i DP-1:/b.tif -i DP-2:/c.tiff -i DP-3:/d.ico -i DP-4:/e.avif")
        for ok in ("/a.jxl", "/b.tif", "/c.tiff", "/d.ico"):
            self.assertNotIn(f"'{ok}'", r.stderr)
        self.assertIn("warning: --image: '/e.avif' does not have", r.stderr)


class PpmPixel(unittest.TestCase):
    """contrib/ppm-pixel must fail, not hang, when grim gives it nothing usable."""

    TOOL = Path(__file__).with_name("ppm-pixel")

    def run_tool(self, data, *args):
        # A hang is a failure: the timeout raises subprocess.TimeoutExpired.
        return subprocess.run([self.TOOL, *args], input=data, capture_output=True, timeout=10)

    def test_reads_a_pixel(self):
        ppm = b"P6\n2 1\n255\n" + bytes([1, 2, 3, 4, 5, 6])
        self.assertEqual(self.run_tool(ppm, "1", "0").stdout.split(), [b"4", b"5", b"6"])

    def test_empty_and_truncated_input_exit_nonzero(self):
        for data in (b"", b"\n \n", b"P6", b"P6\n2 1", b"P6\n2 1\n255", b"P6\n2 1\n255\n\x01\x02"):
            r = self.run_tool(data, "1", "0")
            self.assertNotEqual(r.returncode, 0, data)
            self.assertIn(b"ppm-pixel:", r.stderr)


if __name__ == "__main__":
    unittest.main()
