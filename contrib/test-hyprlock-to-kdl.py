#!/usr/bin/env python3
"""Tests for contrib/hyprlock-to-kdl.

Each case converts a hyprlock config, checks the KDL and the warnings, and, when a swaywardlock
binary is available ($SWAYWARDLOCK, default target/debug/swaywardlock), checks that
`swaywardlock validate` accepts the output. The converter must never drop a key silently and its
output must always validate.
"""

import os
import random
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).parents[1]
SCRIPT = Path(__file__).with_name("hyprlock-to-kdl")
BIN = Path(os.environ.get("SWAYWARDLOCK", ROOT / "target/debug/swaywardlock"))
EXAMPLE = Path(__file__).with_name("fixtures") / "hyprlock-example.conf"

# Characters KDL 1.0 (knuffel 3.2 grammar.rs newline()) ends a line on, besides LF; VT is a newline
# in KDL 2.0. One of them inside a // comment ends the comment and starts a new node.
KDL_NEWLINES = ["\r", "\x0b", "\x0c", "\x85", "\u2028", "\u2029"]
BIDI = ["\u061c", "\u200e", "\u200f", "\u202a", "\u202b", "\u202c", "\u202d", "\u202e",
        "\u2066", "\u2067", "\u2068", "\u2069"]
TOP_NODES = {"general", "auth", "animations", "background", "shape", "image", "input-field", "label"}


def top_nodes(kdl):
    """Top-level node names, read line by line (sound once no_raw_newlines holds)."""
    names = []
    for line in kdl.split("\n"):
        m = re.match(r"([^\s/}][^\s{;]*)", line)
        if m:
            names.append(m.group(1))
    return names


def injection_free(kdl):
    """Is the only line break in KDL a plain LF, and no bidi control raw in the text?"""
    return not any(c in kdl for c in KDL_NEWLINES + BIDI)


def warnings(r, widgets=False):
    """The warning lines; the "no widgets" one only when asked for (many cases have no widgets)."""
    return [l for l in r.stderr.splitlines()
            if l.startswith("warning:") and (widgets or "no widgets" not in l)]


def mentions(r, key):
    """True if a warning names KEY (a hyprlock key, as `section:key` or bare)."""
    return any(re.search(rf"(^|[ :]){re.escape(key)}:", w) for w in warnings(r))


class Convert(unittest.TestCase):
    def run_file(self, path, *args):
        return subprocess.run([SCRIPT, *args, str(path)], text=True, capture_output=True, check=True)

    def conf(self, text, files=None):
        """Convert TEXT as hyprlock.conf in a fresh directory (plus FILES: name -> text)."""
        with tempfile.TemporaryDirectory() as d:
            for name, body in (files or {}).items():
                (Path(d) / name).parent.mkdir(parents=True, exist_ok=True)
                (Path(d) / name).write_text(body)
            p = Path(d) / "hyprlock.conf"
            p.write_text(text)
            return self.run_file(p)

    def validate(self, kdl):
        if not BIN.exists():
            self.skipTest(f"{BIN} not built")
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "config.kdl"
            p.write_text(kdl)
            r = subprocess.run([BIN, "validate", "-c", p], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"swaywardlock rejected:\n{kdl}\n{r.stderr}{r.stdout}")

    def ok(self, text, **kw):
        """Convert, validate the output, and return the result."""
        r = self.conf(text, **kw)
        self.validate(r.stdout)
        return r

    def assert_not_injected(self, r, allowed=TOP_NODES - {"auth"}):
        """The output validates, has no line break but LF (in comments or strings), and every
        top-level node is one the converter writes; stderr has no raw line breaks either."""
        self.validate(r.stdout)
        self.assertTrue(injection_free(r.stdout), repr(r.stdout))
        self.assertTrue(injection_free(r.stderr), repr(r.stderr))
        extra = set(top_nodes(r.stdout)) - set(allowed)
        self.assertFalse(extra, f"injected top-level nodes {extra}:\n{r.stdout}")

    def test_example_conf_converts_and_validates(self):
        r = self.run_file(EXAMPLE)
        # Only onclick has no counterpart: the animations and the rest map to real KDL.
        self.assertEqual(len(warnings(r)), 1, r.stderr)
        self.assertIn("onclick", warnings(r)[0])
        for needle in [
            "hide-cursor false",
            "screenshot",
            "contrast 0.8917",
            "brightness 0.8172",
            "angle=135",
            "angle=210",
            "angle=130",
            "fail {",
            'text "$PAMFAIL$FPRINTFAIL"',
            'font "Sans 24"',
            'font "Monospace 90"',
            'align "left"',
            'fade-in { duration-ms 500; curve "linear"; }',
            'fade-out { duration-ms 500; curve "linear"; }',
            'input-field-dots { duration-ms 200; curve "linear"; }',
            "fade-on-empty false",
            "dots spacing=0.3",
            "// hyprlock: onclick = hyprctl switchxkblayout all next",
            "fail-timeout-ms 2000",
            "blur { passes 3; offset 8; noise 0.0117; saturation 1; }",
            "vibrancy 0.1686",
            "vibrancy-darkness 0.05",
        ]:
            self.assertIn(needle, r.stdout)
        self.assertNotIn("input-field-fade", r.stdout)  # no explicit line: ours keeps its default
        self.validate(r.stdout)

    def test_gradient_angle_plus_90(self):
        def border(value):
            r = self.ok(f"shape {{\n border_size = 2\n border_color = {value}\n}}")
            return r

        # CSS angle = hyprlock degrees + 90, mod 360; no angle is hyprlock's 0.
        for value, angle in [
            ("rgba(ff0000ff) rgba(0000ffff) 0deg", 90),
            ("rgba(ff0000ff) rgba(0000ffff) 45deg", 135),
            ("rgba(ff0000ff) rgba(0000ffff) 300deg", 30),
            ("rgba(ff0000ff) rgba(0000ffff) -45deg", 45),
            ("rgba(ff0000ff) rgba(0000ffff)", 90),
            ("0xFFFF0000 0xFF0000FF 270deg", 0),
        ]:
            r = border(value)
            self.assertIn(f"angle={angle}", r.stdout, value)
            self.assertIn('from="#ff0000ff" to="#0000ffff"', r.stdout)
            self.assertEqual(warnings(r), [])
        r = border("rgba(ff0000ff)")
        self.assertIn('border { width 2; color "#ff0000ff"; }', r.stdout)
        # More than two stops: the first and the last survive; the rest is reported twice.
        r = border("rgba(ff0000ff) rgba(00ff00ff) rgba(0000ffff) 0deg")
        self.assertIn('from="#ff0000ff" to="#0000ffff" angle=90', r.stdout)
        self.assertTrue(any("border_color" in w and "3" in w for w in warnings(r)), r.stderr)
        self.assertIn("// hyprlock: border_color = rgba(ff0000ff) rgba(00ff00ff) rgba(0000ffff) 0deg", r.stdout)

    def test_colors(self):
        for value, want in [
            ("rgba(33ccffee)", "#33ccffee"),
            ("rgba(0,0,0,0.0)", "#00000000"),
            ("rgba(255, 128, 0, 0.5)", "#ff800080"),
            ("rgb(143,143,143)", "#8f8f8fff"),
            ("rgb(aabbcc)", "#aabbccff"),
            ("0xEE00FF99", "#00ff99ee"),
            ("0xFF111111", "#111111ff"),
        ]:
            r = self.ok(f"label {{\n text = x\n color = {value}\n}}")
            self.assertIn(f'color "{want}"', r.stdout, value)
            self.assertEqual(warnings(r), [], value)
        # Components outside 0..255 warn and are clamped, so the output still validates.
        r = self.ok("label {\n text = x\n color = rgb(300, 0, 0)\n}")
        self.assertIn('color "#ff0000ff"', r.stdout)
        self.assertTrue(any("color" in w and "300" in w for w in warnings(r)), r.stderr)
        # A value hyprlock cannot parse is ignored there (default kept): warn, emit nothing.
        r = self.ok("label {\n text = x\n color = zzz\n}")
        self.assertTrue(any("color" in w for w in warnings(r)), r.stderr)
        self.assertNotIn('color "', r.stdout)
        # hyprlang reads anything starting with "no" as false, so "nothex" is a transparent 0.
        r = self.ok("label {\n text = x\n color = nothex\n}")
        self.assertIn('color "#00000000"', r.stdout)

    def test_default_differences_are_spelled_out(self):
        r = self.ok("label {\n}\nimage {\n path = /a.png\n}\nbackground {\n}\ninput-field {\n}\n")
        out = r.stdout
        for needle in [
            "hide-cursor false",  # ours defaults to true
            'color "#111111ff"',  # background: ours is #a3a3a3
            'align "left"',  # label: ours is centred
            'text "Sample Text"',
            'border { width 4; color "#ddddddff"; }',  # image: ours has no border
            'fade-in { duration-ms 800; curve "cubic-bezier" 0 0.75 0.15 1; }',
            'fade-out { duration-ms 800; curve "cubic-bezier" 0 0.75 0.15 1; }',
            "fail-timeout-ms 2000",  # general:fail_timeout: ours is 3000
        ]:
            self.assertIn(needle, out)
        # blur_passes defaults to 0: no blur, so no contrast/brightness/noise either.
        for absent in ("contrast", "brightness", "blur", "input-field-fade"):
            self.assertNotIn(absent, out)
        # Deliberately ours: hyprlock never clears a typed password (no KDL value turns ours off).
        self.assertNotIn("clear-input-after-ms", out)
        self.assertEqual(warnings(r), [])

    def test_blur_defaults_only_with_passes(self):
        r = self.ok("background {\n blur_passes = 0\n contrast = 1.2\n brightness = 0.5\n noise = 0.3\n blur_size = 4\n"
                    " vibrancy = 0.5\n vibrancy_darkness = 0.2\n}")
        for absent in ("contrast", "brightness", "blur {", "vibrancy"):
            self.assertNotIn(absent, r.stdout)
        for key in ("contrast", "brightness", "noise", "blur_size", "vibrancy", "vibrancy_darkness"):
            self.assertTrue(any(key in w and "blur_passes" in w for w in warnings(r)), (key, r.stderr))
        r = self.ok("background {\n blur_passes = 3\n}")
        # blur_size defaults to 8 in hyprlock, our offset to 3: spelled out with the blur.
        self.assertIn("blur { passes 3; offset 8; noise 0.0117; saturation 1; }", r.stdout)
        self.assertIn("contrast 0.8917", r.stdout)
        self.assertIn("brightness 0.8172", r.stdout)
        # hyprlock's vibrancy defaults, and saturation 1: hyprlock has no blur saturation.
        self.assertIn("vibrancy 0.1686\n", r.stdout)
        self.assertIn("vibrancy-darkness 0.05\n", r.stdout)
        self.assertEqual(warnings(r), [])
        r = self.ok("background {\n blur_passes = 2\n blur_size = 6\n noise = 0.05\n contrast = 1.5\n brightness = 1\n"
                    " vibrancy = 0.3\n vibrancy_darkness = 0\n}")
        self.assertIn("blur { passes 2; offset 6; noise 0.05; saturation 1; }", r.stdout)
        self.assertIn("vibrancy 0.3\n", r.stdout)
        self.assertIn("vibrancy-darkness 0\n", r.stdout)
        self.assertIn("contrast 1.5", r.stdout)
        self.assertIn("brightness 1\n", r.stdout)
        # Values the decoder rejects are clamped with a warning.
        r = self.ok("background {\n blur_passes = 999\n contrast = 99\n vibrancy = 20\n vibrancy_darkness = 2\n}")
        self.assertIn("passes 255", r.stdout)
        self.assertIn("contrast 10\n", r.stdout)
        self.assertIn("vibrancy 10\n", r.stdout)
        self.assertIn("vibrancy-darkness 1\n", r.stdout)
        self.assertEqual(len(warnings(r)), 4, r.stderr)
        self.validate(r.stdout)

    def test_animation_tree_and_beziers(self):
        def anim(lines):
            return self.ok("animations {\n" + "\n".join(lines) + "\n}")

        # A parent line applies to the leaves below it; an explicit leaf wins either order.
        r = anim(["bezier = fast, 0.1, 0.9, 0.2, 1", "animation = fade, 1, 3, fast", "animation = fadeOut, 1, 6, linear"])
        self.assertIn('fade-in { duration-ms 300; curve "cubic-bezier" 0.1 0.9 0.2 1; }', r.stdout)
        self.assertIn('fade-out { duration-ms 600; curve "linear"; }', r.stdout)
        r = anim(["animation = fadeOut, 1, 6, linear", "animation = fade, 1, 3, default"])
        self.assertIn('fade-out { duration-ms 600; curve "linear"; }', r.stdout)
        self.assertIn('fade-in { duration-ms 300; curve "cubic-bezier" 0 0.75 0.15 1; }', r.stdout)
        # Disabled maps to off; global reaches the fades but not inputFieldColors.
        r = anim(["animation = fadeIn, 0, 1, default", "animation = global, 1, 2, linear"])
        self.assertIn("fade-in { off; }", r.stdout)
        self.assertIn('fade-out { duration-ms 200; curve "linear"; }', r.stdout)
        self.assertIn('input-field-fade { duration-ms 200; curve "linear"; }', r.stdout)
        self.assertIn('input-field-width { duration-ms 200; curve "linear"; }', r.stdout)
        self.assertIn('input-field-dots { duration-ms 200; curve "linear"; }', r.stdout)
        self.assertNotIn("input-field-colors", r.stdout)
        r = anim(["animation = inputFieldColors, 1, 4, linear", "animation = inputFieldWidth, 0, 4, linear"])
        self.assertIn('input-field-colors { duration-ms 400; curve "linear"; }', r.stdout)
        self.assertIn("input-field-width { off; }", r.stdout)
        # The identity bezier is linear whatever its name.
        r = anim(["bezier = x, 1, 1, 0, 0", "animation = fade, 1, 5, x"])
        self.assertIn('fade-in { duration-ms 500; curve "linear"; }', r.stdout)
        self.assertEqual(warnings(r), [])
        # An unknown bezier falls back to default (as in hyprlock), with a warning.
        r = anim(["animation = fadeIn, 1, 5, nope"])
        self.assertIn('fade-in { duration-ms 500; curve "cubic-bezier" 0 0.75 0.15 1; }', r.stdout)
        self.assertTrue(any("nope" in w for w in warnings(r)), r.stderr)
        # x outside 0..1 is rejected by the decoder: clamp and warn.
        r = anim(["bezier = wild, 1.5, 0, -0.5, 1", "animation = fadeIn, 1, 5, wild"])
        self.assertIn('curve "cubic-bezier" 1 0 0 1;', r.stdout)
        self.assertTrue(any("wild" in w for w in warnings(r)), r.stderr)
        # More than 3 s of any animation is rejected by the decoder (a fade-out would hold the
        # session locked; a field animation keeps redrawing): clamp and warn.
        r = anim(["animation = fadeOut, 1, 50, linear"])
        self.assertIn("duration-ms 3000", r.stdout)
        self.assertTrue(any("fadeOut" in w for w in warnings(r)), r.stderr)
        r = anim(["animation = inputFieldColors, 1, 50, linear"])
        self.assertIn('input-field-colors { duration-ms 3000; curve "linear"; }', r.stdout)
        self.assertTrue(any("inputFieldColors" in w for w in warnings(r)), r.stderr)
        # y outside -2..3 is rejected by the decoder: clamp and warn.
        r = anim(["bezier = boing, 0, 1000, 1, -5", "animation = inputFieldDots, 1, 2, boing"])
        self.assertIn('input-field-dots { duration-ms 200; curve "cubic-bezier" 0 3 1 -2; }', r.stdout)
        self.assertTrue(any("boing" in w for w in warnings(r)), r.stderr)
        # A bad speed or an unknown name is an error in hyprlock: the line is ignored, with a warning.
        r = anim(["animation = fadeIn, 1, 0, linear", "animation = fadeSideways, 1, 5, linear"])
        self.assertIn("duration-ms 800", r.stdout)
        self.assertEqual(len(warnings(r)), 2, r.stderr)
        # animations:enabled = false turns them all off.
        r = self.ok("animations {\n enabled = false\n}")
        self.assertIn("animations {\n    off\n}", r.stdout)

    def test_rounding_sentinels(self):
        r = self.ok("input-field {\n rounding = -1\n dots_rounding = -2\n}")
        self.assertNotIn("corner-radius 15", r.stdout)
        self.assertNotRegex(r.stdout, r"^\s*corner-radius", "rounding -1 is ours by default")
        self.assertIn("dots corner-radius=-2", r.stdout)
        r = self.ok("input-field {\n rounding = 0\n}")
        self.assertIn("corner-radius 0", r.stdout)
        r = self.ok("shape {\n rounding = -1\n}\nshape {\n rounding = 12\n}\nshape {\n}")
        self.assertIn('corner-radius "full"', r.stdout)
        self.assertIn("corner-radius 12", r.stdout)
        self.assertEqual(r.stdout.count("corner-radius"), 2)
        r = self.ok("image {\n path = /a.png\n rounding = 8\n}\nimage {\n path = /b.png\n}")
        self.assertIn("corner-radius 8", r.stdout)
        self.assertEqual(r.stdout.count("corner-radius"), 1)  # -1 is the full circle in both
        r = self.ok("input-field {\n dots_rounding = 5000\n}")
        self.assertIn("corner-radius=1000", r.stdout)  # the decoder's cap
        self.assertEqual(len(warnings(r)), 1, r.stderr)
        r = self.ok("shape {\n border_size = 2\n border_color = rgba(ff0000ff) rgba(0000ffff) 100000deg\n zindex = 99999999999\n}")
        self.assertIn("angle=10;", r.stdout)  # (100000 + 90) mod 360, inside the decoder's i16
        self.assertIn("z-index 2147483647", r.stdout)
        r = self.ok("input-field {\n dots_rounding = -7\n}")
        self.assertTrue(any("dots_rounding" in w for w in warnings(r)), r.stderr)

    def test_hyprlang_syntax(self):
        # Variables: longest name first, plain substring replacement, chained.
        r = self.ok("$font = Mono\n$fontsize = 33\n$a = $font\n$b = $a Bold\nlabel {\n text = hi\n font_family = $b\n font_size = $fontsize\n}")
        self.assertIn('font "Mono Bold 33"', r.stdout)
        # A variable applies to later lines only.
        r = self.ok("label {\n text = $late\n}\n$late = x")
        self.assertIn('text "$late"', r.stdout)
        # Expressions, with variables and plain numbers.
        r = self.ok("$w = 100\nshape {\n size = {{ $w + 20 }}, {{ 10 * 3 }}\n}")
        self.assertIn("size 120 30", r.stdout)
        # Comments: trailing, escaped ##, continuation lines.
        r = self.ok("label {\n text = a ## b # comment\n color = rgba(ff00ff\\\nff) # c\n}")
        self.assertIn('text "a # b"', r.stdout)
        self.assertIn('color "#ff00ffff"', r.stdout)
        # Escapes after substitution.
        r = self.ok("label {\n text = \\{{ not an expression \\}}\n}")
        self.assertIn('text "{{ not an expression }}"', r.stdout)
        # Reopened and flat/nested names reach the same keys.
        r = self.ok("general {\n hide_cursor = true\n}\ngeneral {\n ignore_empty_input = true\n}\n"
                    "auth:fingerprint:enabled = true\nauth {\n fingerprint {\n retry_delay = 500\n }\n}")
        self.assertIn("hide-cursor true", r.stdout)
        self.assertIn("ignore-empty-input", r.stdout)
        self.assertIn("retry-delay-ms 500", r.stdout)
        # The last of two equal keys in a block wins.
        r = self.ok("label {\n text = one\n text = two\n}")
        self.assertIn('text "two"', r.stdout)
        self.assertNotIn("one", r.stdout)
        # # hyprlang if / endif use the environment, then config variables.
        os.environ["HL2KDL_TEST_FLAG"] = "1"
        r = self.ok("# hyprlang if HL2KDL_TEST_FLAG\nlabel {\n text = yes\n}\n# hyprlang endif\n"
                    "# hyprlang if !HL2KDL_TEST_FLAG\nlabel {\n text = no\n}\n# hyprlang endif\n")
        self.assertIn('text "yes"', r.stdout)
        self.assertNotIn('text "no"', r.stdout)
        # Quotes and backslashes in text survive KDL escaping.
        r = self.ok('label {\n text = say "hi" \\\\o/\n}')
        self.assertIn('text "say \\"hi\\" \\\\o/"', r.stdout)
        # Environment in paths.
        os.environ["HL2KDL_TEST_DIR"] = "/srv/walls"
        r = self.ok("image {\n path = $HL2KDL_TEST_DIR/a.png\n}")
        self.assertIn('path "/srv/walls/a.png"', r.stdout)
        # Runtime tokens stay, an unknown $NAME is reported.
        r = self.ok("label {\n text = $TIME $LAYOUT[en,ru] $ATTEMPTS $DESC\n}")
        self.assertIn('text "$TIME $LAYOUT[en,ru] $ATTEMPTS $DESC"', r.stdout)
        self.assertEqual(warnings(r), [])
        r = self.ok("label {\n text = $NOSUCHTHING\n}")
        self.assertTrue(any("$NOSUCHTHING" in w for w in warnings(r)), r.stderr)

    def test_source(self):
        files = {
            "a.conf": "$c = sourced\nlabel {\n text = $c\n}\nsource = sub/b.conf\n",
            "sub/b.conf": "shape {\n size = 7, 8\n}\nsource = ../hyprlock.conf\n",
            "g1.conf": "label {\n text = g1\n}\n",
            "g2.conf": "label {\n text = g2\n}\n",
            "self.conf": "source = self.conf\nlabel {\n text = selfdone\n}\n",
        }
        # Relative to the including file, nested, a self/loop include is skipped, globs expand.
        r = self.ok("source = self.conf\nsource = g*.conf\nsource = a.conf\nlabel {\n text = $c main\n}", files=files)
        for needle in ['text "selfdone"', 'text "g1"', 'text "g2"', 'text "sourced main"']:
            self.assertIn(needle, r.stdout)
        # b.conf includes the main file again: hyprlock would recurse forever; here it is cut and said.
        self.assertIn("size 7 8", r.stdout)
        self.assertEqual(len(warnings(r)), 1, r.stderr)
        self.assertIn("include loop", warnings(r)[0])
        # ~ expands to the home directory.
        with tempfile.TemporaryDirectory() as home:
            (Path(home) / "h.conf").write_text("label {\n text = from home\n}\n")
            env = dict(os.environ, HOME=home)
            with tempfile.TemporaryDirectory() as d:
                p = Path(d) / "hyprlock.conf"
                p.write_text("source = ~/h.conf\n")
                r = subprocess.run([SCRIPT, str(p)], text=True, capture_output=True, check=True, env=env)
        self.assertIn('text "from home"', r.stdout)
        # A missing file is an error in hyprlock; here a warning.
        r = self.ok("source = nothing-here.conf\n")
        self.assertTrue(any("nothing-here.conf" in w for w in warnings(r)), r.stderr)

    def test_widget_order_is_hyprlock_draw_order(self):
        r = self.ok("label {\n text = L\n}\nshape {\n size = 1, 2\n}\ninput-field {\n}\nimage {\n path = /a.png\n}\n"
                    "background {\n}\nshape {\n size = 3, 4\n}\nlabel {\n text = M\n zindex = 2\n}")
        order = re.findall(r"^(background|shape|image|input-field|label)\b", r.stdout, re.M)
        self.assertEqual(order, ["background", "shape", "shape", "image", "input-field", "label", "label"])
        self.assertLess(r.stdout.index("size 1 2"), r.stdout.index("size 3 4"))  # file order within a type
        self.assertIn("z-index 2", r.stdout)
        self.assertNotIn("z-index -1", r.stdout)  # the type default is not spelled out
        r = self.ok("background {\n zindex = 4\n}")
        self.assertIn("z-index 4", r.stdout)

    def test_pam_safety(self):
        # The default module name needs /etc/pam.d/hyprlock, which may not exist: keep our own file.
        r = self.ok("auth {\n pam {\n module = hyprlock\n }\n}")
        self.assertNotIn("service=", r.stdout)
        self.assertFalse([l for l in r.stdout.splitlines() if l.strip().startswith("pam")], r.stdout)
        self.assertIn("// hyprlock: auth:pam:module = hyprlock", r.stdout)
        self.assertIn("note:", r.stderr)
        r = self.ok("auth:pam:module = login\n")
        self.assertIn('pam service="login"', r.stdout)
        r = self.ok("auth {\n pam {\n enabled = false\n module = login\n }\n}")
        self.assertIn('pam "off"', r.stdout)
        self.assertNotIn("service=", r.stdout)
        # PAM off without a fingerprint still permits explicitly enabled CLI grace.
        self.assertTrue(any("PAM" in w and "grace" in w and "SIGUSR1" in w
                            for w in warnings(r)), r.stderr)
        self.assertNotIn("only SIGUSR1", r.stderr)
        r = self.ok("auth {\n pam {\n enabled = false\n }\n fingerprint {\n enabled = true\n }\n}")
        self.assertEqual(warnings(r), [])
        self.assertIn("fingerprint", r.stdout)
        # Nothing about auth by default.
        r = self.ok("")
        self.assertNotIn("auth", r.stdout)

    def test_fingerprint(self):
        r = self.ok("auth {\n fingerprint {\n enabled = true\n ready_message = Touch me\n present_message = Scanning...\n retry_delay = 300\n }\n}")
        for needle in ['ready-message "Touch me"', 'present-message "Scanning..."', "retry-delay-ms 300"]:
            self.assertIn(needle, r.stdout)
        # Sub-keys of a disabled fingerprint do nothing in hyprlock either; they stay visible.
        r = self.ok("auth {\n fingerprint {\n ready_message = Touch me\n }\n}")
        self.assertNotIn("fingerprint {", r.stdout)
        self.assertIn("// hyprlock: auth:fingerprint:ready_message = Touch me", r.stdout)

    def test_relative_paths_absolute(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "x").mkdir()
            p = Path(d) / "x" / "hyprlock.conf"
            p.write_text("background {\n path = wall.png\n}\nimage {\n path = sub/../me.png\n}\n"
                         "image {\n path = /abs/pic.png\n}\nimage {\n path = ~/pic.png\n}\n")
            env = dict(os.environ, HOME="/home/someone")
            r = subprocess.run([SCRIPT, str(p)], text=True, capture_output=True, check=True, env=env)
            real = os.path.abspath(Path(d) / "x")
        self.assertIn(f'path "{real}/wall.png"', r.stdout)
        self.assertIn(f'path "{real}/me.png"', r.stdout)
        self.assertIn('path "/abs/pic.png"', r.stdout)
        self.assertIn('path "/home/someone/pic.png"', r.stdout)
        self.assertEqual(warnings(r), [])
        r = self.ok("background {\n path = screenshot\n}")
        self.assertIn("screenshot", r.stdout)
        self.assertNotIn("path", r.stdout)

    def test_image_without_path_is_skipped_with_a_warning(self):
        r = self.ok("image {\n size = 100\n}")
        self.assertTrue(any("image" in w and "path" in w for w in warnings(r)), r.stderr)
        self.assertNotRegex(r.stdout, r"^image", "ours requires a path")

    def test_layout_values(self):
        r = self.ok("shape {\n size = 25%, 10\n position = 30, -20%\n halign = left\n valign = top\n}")
        for needle in ['size "25%" 10', 'position 30 "-20%"', 'halign "left"', 'valign "top"']:
            self.assertIn(needle, r.stdout)
        # A single comma only; garbage is ignored with a warning, as hyprlock logs an error.
        r = self.ok("shape {\n size = 1, 2, 3\n position = abc\n halign = sideways\n}")
        for key in ("size", "position", "halign"):
            self.assertTrue(any(f": {key}:" in w for w in warnings(r)), (key, r.stderr))
        # Sizes the decoder rejects are clamped.
        r = self.ok("shape {\n size = -5, 5000%\n position = 9999999, 0\n}")
        self.assertIn('size 0 "1000%"', r.stdout)
        self.assertEqual(len(warnings(r)), 3, r.stderr)

    def test_monitor(self):
        r = self.ok("label {\n text = a\n monitor = DP-1\n}\nlabel {\n text = b\n monitor = desc:Dell U2720\n}\n"
                    "label {\n text = c\n monitor = Dell U2720\n}\nlabel {\n text = d\n monitor =\n}")
        self.assertIn('label output="DP-1" {', r.stdout)
        self.assertIn('label output="desc:Dell U2720" {', r.stdout)
        self.assertEqual(r.stdout.count('output="desc:Dell U2720"'), 2)  # hyprlock matches a bare description prefix
        self.assertEqual(r.stdout.count("output="), 3)
        self.assertEqual(warnings(r), [])

    def test_input_field(self):
        r = self.ok(
            "input-field {\n outline_thickness = 0\n outer_color = rgba(ff0000ff)\n inner_color = 0xFF202020\n"
            " font_color = rgb(ffffff)\n font_family = Mono\n placeholder_text = Pw\n fail_text = Nope\n"
            " check_text = Wait\n dots_size = 0.4\n dots_spacing = -0.1\n dots_center = false\n dots_text_format = *\n"
            " dots_rounding = 3\n fade_on_empty = true\n fade_timeout = 1500\n hide_input = true\n"
            " hide_input_base_color = 0xFF112233\n capslock_color = rgba(ff0000ff) rgba(00ff00ff) 0deg\n numlock_color = 0xFF010203\n"
            " bothlock_color =\n invert_numlock = true\n swap_font_color = true\n check_color = 0xFF00FF00\n fail_color = 0xFFFF0000\n}")
        for needle in [
            'outline { width 0; color "#ff0000ff"; }',
            'inner-color "#202020ff"',
            'font-color "#ffffffff"',
            'font "Mono"',
            'placeholder-text "Pw"',
            'fail { color "#ff0000ff"; text "Nope"; }',
            'check { color "#00ff00ff"; text "Wait"; }',
            'dots size=0.4 spacing=-0.1 center=false corner-radius=3 text="*"',
            "fade-on-empty timeout-ms=1500",
            'hide-input "#112233ff"',
            'capslock-gradient from="#ff0000ff" to="#00ff00ff" angle=90',
            'numlock-color "#010203ff"',
            "invert-numlock",
            "swap-font-color",
        ]:
            self.assertIn(needle, r.stdout)
        self.assertNotIn("bothlock", r.stdout)  # empty value: unset
        self.assertEqual(warnings(r), [])
        # Dot sizes outside what hyprlock allows are clamped, with a warning.
        r = self.ok("input-field {\n dots_size = 5\n dots_spacing = 3\n}")
        self.assertIn("size=0.8", r.stdout)
        self.assertIn("spacing=1", r.stdout)
        self.assertEqual(len(warnings(r)), 2, r.stderr)
        # An outline colour alone, or a thickness alone, still gives a block the decoder accepts.
        r = self.ok("input-field {\n outline_thickness = 6\n}")
        self.assertIn('outline { width 6; color "#111111ff"; }', r.stdout)
        r = self.ok("input-field {\n fade_on_empty = false\n fade_timeout = 5\n}")
        self.assertIn("fade-on-empty false", r.stdout)
        self.assertNotIn("fade-on-empty timeout-ms", r.stdout)

    def test_shadows_on_boxes_are_approximated_with_a_warning(self):
        r = self.ok("shape {\n shadow_passes = 2\n shadow_size = 5\n shadow_color = 0xFF112233\n}")
        self.assertIn('shadow { offset x=0 y=0; spread 0; softness 10; color "#112233ff"; }', r.stdout)
        self.assertTrue(any("shadow" in w and "approximat" in w for w in warnings(r)), r.stderr)
        # No passes, no shadow in hyprlock: the other keys are reported, not translated.
        r = self.ok("input-field {\n shadow_size = 9\n}")
        self.assertNotIn("shadow {", r.stdout)
        self.assertTrue(any("shadow_size" in w for w in warnings(r)), r.stderr)

    def test_lock_colors_follow_hyprlock_an_empty_value_is_the_fallback(self):
        # hyprlock 0.9.6: the default is the empty string (the fallback); there is no special -1.
        # -1 is read as the integer 0xFFFFFFFF, opaque white, like any other colour number.
        r = self.ok("input-field {\n capslock_color = -1\n numlock_color = -1\n bothlock_color = -1\n}")
        for name in ("capslock", "numlock", "bothlock"):
            self.assertIn(f'{name}-color "#ffffffff"', r.stdout)
        self.assertEqual(warnings(r), [], r.stderr)
        self.assertNotIn("-1 means", r.stderr)
        # An empty value is the fallback: nothing is written.
        r = self.ok("input-field {\n capslock_color =\n numlock_color =\n bothlock_color =\n}")
        for name in ("capslock", "numlock", "bothlock"):
            self.assertNotIn(name, r.stdout)
        self.assertEqual(warnings(r), [], r.stderr)

    def test_old_general_options_point_at_the_swaywardlock_equivalent(self):
        r = self.ok("general {\n grace = 3\n disable_loading_bar = true\n}")
        grace = [w for w in warnings(r) if "general:grace" in w]
        self.assertEqual(len(grace), 1, r.stderr)
        self.assertIn("-g/--grace SECONDS", grace[0])
        self.assertTrue(any("general:disable_loading_bar" in w and "loading bar" in w for w in warnings(r)), r.stderr)
        self.assertIn("// hyprlock: general:grace = 3", r.stdout)  # still never silent

    def test_no_widgets_warns_about_the_default_look(self):
        r = self.ok("general {\n hide_cursor = true\n}")
        self.assertEqual(len(warnings(r, widgets=True)), 1, r.stderr)
        self.assertIn("no widgets", warnings(r, widgets=True)[0])

    def test_general(self):
        r = self.ok("general {\n hide_cursor = true\n ignore_empty_input = on\n fail_timeout = 1234\n immediate_render = yes\n}")
        for needle in ["hide-cursor true", "ignore-empty-input", "fail-timeout-ms 1234"]:
            self.assertIn(needle, r.stdout)
        self.assertIn("command: swaywardlock --immediate-render", r.stderr)
        r = self.ok("")
        self.assertIn("command: swaywardlock\n", r.stderr)

    # Every key hyprlock registers (ConfigManager.cpp init), set to a non-default value inside a
    # template that makes it meaningful. TEMPLATE has one {} where the "key = value" line goes. The
    # converter must either change its output or mention the key on stderr: never drop it silently.
    EVERY_KEY = [
        ("general {{\n{}\n}}", [
            ("text_trim", "0"), ("hide_cursor", "true"), ("ignore_empty_input", "true"),
            ("immediate_render", "true"), ("fractional_scaling", "1"), ("screencopy_mode", "1"),
            ("fail_timeout", "1500")]),
        ("auth {{\n pam {{\n{}\n }}\n}}", [("enabled", "false"), ("module", "login"), ("module", "hyprlock")]),
        ("auth {{\n fingerprint {{\n enabled = true\n{}\n }}\n}}", [
            ("ready_message", "x"), ("present_message", "y"), ("retry_delay", "300")]),
        ("auth {{\n fingerprint {{\n{}\n }}\n}}", [("enabled", "true"), ("ready_message", "x")]),
        ("animations {{\n{}\n}}", [("enabled", "false")]),
        ("background {{\n blur_passes = 2\n{}\n}}", [
            ("blur_size", "5"), ("noise", "0.1"), ("contrast", "1.1"), ("brightness", "0.9"),
            ("vibrancy", "0.3"), ("vibrancy_darkness", "0.2")]),
        ("background {{\n{}\n}}", [
            ("monitor", "DP-1"), ("path", "/a.png"), ("color", "rgba(10203040)"), ("blur_passes", "4"),
            ("blur_size", "5"), ("noise", "0.1"), ("contrast", "1.1"), ("brightness", "0.9"),
            ("vibrancy", "0.3"), ("vibrancy_darkness", "0.2"), ("zindex", "3"), ("reload_time", "5"),
            ("reload_cmd", "echo hi"), ("crossfade_time", "1.5")]),
        ("shape {{\n border_size = 2\n{}\n}}", [("border_color", "rgba(0000ffff)")]),
        ("shape {{\n{}\n}}", [
            ("monitor", "DP-1"), ("size", "20, 30"), ("rounding", "5"), ("border_size", "2"), ("color", "rgba(0000ffff)"),
            ("position", "5, 6"), ("halign", "left"), ("valign", "top"), ("rotate", "30"), ("xray", "true"),
            ("zindex", "3"), ("shadow_size", "5"), ("shadow_passes", "2"), ("shadow_color", "0xFF00FF00"),
            ("shadow_boost", "2"), ("onclick", "echo hi")]),
        ("image {{\n path = /a.png\n{}\n}}", [
            ("monitor", "DP-1"), ("size", "200"), ("rounding", "5"), ("border_size", "0"), ("border_color", "0xFF00FF00"),
            ("position", "5, 6"), ("halign", "left"), ("valign", "top"), ("rotate", "30"), ("reload_time", "5"),
            ("reload_cmd", "echo hi"), ("zindex", "3"), ("shadow_size", "5"), ("shadow_passes", "2"),
            ("shadow_color", "0xFF00FF00"), ("shadow_boost", "2"), ("onclick", "echo hi")]),
        ("input-field {{\n hide_input = true\n{}\n}}", [("hide_input_base_color", "0xFF112233")]),
        ("input-field {{\n fade_on_empty = false\n{}\n}}", [("fade_timeout", "1000")]),
        ("input-field {{\n{}\n}}", [
            ("monitor", "DP-1"), ("size", "20, 30"), ("inner_color", "0xFF202020"), ("outer_color", "0xFF202020"),
            ("outline_thickness", "2"), ("dots_size", "0.5"), ("dots_center", "false"), ("dots_spacing", "0.5"),
            ("dots_rounding", "3"), ("dots_text_format", "*"), ("fade_on_empty", "false"), ("fade_timeout", "1000"),
            ("font_color", "0xFF202020"), ("font_family", "Mono"), ("halign", "left"), ("valign", "top"),
            ("position", "5, 6"), ("placeholder_text", "Pw"), ("hide_input", "true"), ("rounding", "5"),
            ("check_color", "0xFF202020"), ("fail_color", "0xFF202020"), ("fail_text", "Nope"), ("check_text", "Wait"),
            ("capslock_color", "0xFF202020"), ("numlock_color", "0xFF202020"), ("bothlock_color", "0xFF202020"),
            ("invert_numlock", "true"), ("swap_font_color", "true"), ("zindex", "3"), ("shadow_size", "5"),
            ("shadow_passes", "2"), ("shadow_color", "0xFF00FF00"), ("shadow_boost", "2")]),
        ("label {{\n{}\n}}", [
            ("monitor", "DP-1"), ("position", "5, 6"), ("color", "0xFF202020"), ("font_size", "30"), ("text", "Hi"),
            ("font_family", "Mono"), ("halign", "left"), ("valign", "top"), ("rotate", "30"), ("text_align", "center"),
            ("zindex", "3"), ("shadow_size", "5"), ("shadow_passes", "2"), ("shadow_color", "0xFF00FF00"),
            ("shadow_boost", "2"), ("onclick", "echo hi")]),
    ]

    def test_every_hyprlock_key_is_translated_or_reported(self):
        checked = 0
        for template, keys in self.EVERY_KEY:
            base = self.conf(template.format("")).stdout
            for key, value in keys:
                r = self.ok(template.format(f" {key} = {value}"))
                reported = mentions(r, key) or f"{key} = {value}" in r.stdout
                self.assertTrue(r.stdout != base or reported, f"{key} = {value} was dropped silently")
                checked += 1
        self.assertGreater(checked, 100)

    # Keys with no KDL counterpart yet. Each must warn and leave a comment, and the output validates.
    # The units that add the KDL (shadows) move their keys out of this list.
    UNSUPPORTED = [
        ("general", "text_trim", "0"), ("general", "fractional_scaling", "1"), ("general", "screencopy_mode", "1"),
        ("shape", "onclick", "echo hi"), ("shape", "shadow_boost", "2"),
        ("image", "onclick", "echo hi"), ("image", "shadow_boost", "2"),
        ("input-field", "shadow_boost", "2"),
        ("label", "onclick", "echo hi"),
        ("label", "frobnicate", "1"),
    ]

    def test_reload_keys_become_kdl(self):
        """reload_time (seconds) -> reload-ms, reload_cmd -> reload-cmd, crossfade_time -> crossfade-ms."""
        r = self.ok("background {\n reload_time = 5\n reload_cmd = echo /a.png\n crossfade_time = 1.5\n}")
        self.assertIn('reload-cmd "echo /a.png"', r.stdout)
        self.assertIn("reload-ms 5000", r.stdout)
        self.assertIn("crossfade-ms 1500", r.stdout)
        self.assertNotIn("// hyprlock: reload", r.stdout)
        self.assertTrue(any("crossfade_time" in l and "ignores" in l for l in r.stderr.splitlines()), r.stderr)
        # 0 is "only on SIGUSR2"; -1 (the default) is off.
        r = self.ok("background {\n reload_time = 0\n reload_cmd = echo /a.png\n}")
        self.assertIn("reload-ms 0", r.stdout)
        r = self.ok("background {\n reload_time = -1\n reload_cmd = echo /a.png\n}")
        self.assertNotIn("reload-ms", r.stdout)
        self.assertIn("reload-cmd", r.stdout)  # runs once at start, as in hyprlock
        # Negative crossfade_time is off.
        r = self.ok("background {\n crossfade_time = -1\n}")
        self.assertNotIn("crossfade-ms", r.stdout)
        # An image reloads on a timer, with or without a command.
        r = self.ok("image {\n path = /a.png\n reload_time = 2\n reload_cmd = echo /b.png\n}")
        self.assertIn("reload-ms 2000", r.stdout)
        self.assertIn('reload-cmd "echo /b.png"', r.stdout)
        r = self.ok("image {\n path = /a.png\n reload_time = 0\n}")
        self.assertIn("reload-ms 0", r.stdout)
        self.assertNotIn("reload-cmd", r.stdout)

    def test_reload_keys_hyprlock_never_acts_on_are_left_out(self):
        for kind, extra, text, key in [
            # A background reloads only with a command; an image's command only runs on a timer.
            ("background", "", "reload_time = 5", "reload_time"),
            ("background", " path = /a.png\n", "reload_cmd = echo x", "reload_cmd"),
            ("image", " path = /a.png\n", "reload_cmd = echo x", "reload_cmd"),
        ]:
            r = self.ok(f"{kind} {{\n{extra} {text}\n}}")
            self.assertTrue(mentions(r, key), f"{kind} {key}: {r.stderr}")
            self.assertNotIn("reload-", r.stdout, kind)

    def test_unsupported_keys_become_comments(self):
        for section, key, value in self.UNSUPPORTED:
            extra = " path = /a.png\n" if section == "image" else ""
            r = self.ok(f"{section} {{\n{extra} {key} = {value}\n}}")
            self.assertTrue(mentions(r, key), f"no warning for {key}: {r.stderr}")
            self.assertRegex(r.stdout, rf"// hyprlock: [\w:-]*{key} = {re.escape(value)}", key)
        # The same for a section and a key hyprlock does not know, and for the default values
        # hyprlock itself treats as "off" (nothing to report).
        r = self.ok("frobnicator {\n level = 11\n}")
        self.assertTrue(any("frobnicator" in w for w in warnings(r)), r.stderr)
        self.assertTrue(any("level" in w for w in warnings(r)), r.stderr)
        self.assertIn("// hyprlock: frobnicator:level = 11", r.stdout)
        r = self.ok("label {\n text = a\n rotate = 0\n onclick =\n shadow_passes = 0\n shadow_boost = 1.2\n}\n"
                    "general {\n text_trim = 1\n fractional_scaling = 2\n screencopy_mode = 0\n}")
        self.assertEqual(warnings(r), [], r.stderr)

    def test_label_shadow_becomes_text_shadow(self):
        """hyprlock's label shadow_* is a text-shadow node; passes 0 (hyprlock's default) is none."""
        r = self.ok("label {\n text = a\n shadow_passes = 2\n}")
        self.assertIn("text-shadow { passes 2; }", r.stdout)
        self.assertEqual(warnings(r), [], r.stderr)
        r = self.ok("label {\n text = a\n shadow_passes = 3\n shadow_size = 5\n shadow_color = 0x80FF0000\n"
                    " shadow_boost = 2\n}")
        # hyprlock ignores the colour's alpha, so it is written as ff.
        self.assertIn('text-shadow { passes 3; size 5; color "#ff0000ff"; boost 2; }', r.stdout)
        self.assertEqual(warnings(r), [], r.stderr)
        # The defaults are left out.
        r = self.ok("label {\n text = a\n shadow_passes = 1\n shadow_size = 3\n shadow_color = 0xFF000000\n"
                    " shadow_boost = 1.2\n}")
        self.assertIn("text-shadow { passes 1; }", r.stdout)
        # No passes: no shadow in hyprlock, so none here; the other keys are reported.
        r = self.ok("label {\n text = a\n shadow_passes = 0\n shadow_size = 5\n}")
        self.assertNotIn("text-shadow", r.stdout)
        self.assertTrue(any("shadow_size" in w for w in warnings(r)), r.stderr)
        r = self.ok("label {\n text = a\n shadow_boost = 3\n}")
        self.assertNotIn("text-shadow", r.stdout)
        self.assertTrue(any("shadow_boost" in w for w in warnings(r)), r.stderr)
        # Out of range values are clamped with a warning (swaywardlock rejects them).
        r = self.ok("label {\n text = a\n shadow_passes = 20\n shadow_size = 500\n shadow_boost = 99\n}")
        self.assertIn("text-shadow { passes 4; size 20; boost 10; }", r.stdout)
        self.assertEqual(len(warnings(r)), 3, r.stderr)
        # swaywardlock caps passes at 4 and size at 20 (the blur's reach doubles per pass).
        r = self.ok("label {\n text = a\n shadow_passes = 5\n shadow_size = 21\n}")
        self.assertIn("text-shadow { passes 4; size 20; }", r.stdout)
        self.assertEqual(len(warnings(r)), 2, r.stderr)
        # Other widgets keep the box shadow, and boost stays a comment there.
        r = self.ok("shape {\n shadow_passes = 2\n}")
        self.assertNotIn("text-shadow", r.stdout)

    def test_rotate_becomes_kdl(self):
        """rotate on label, image and shape is real KDL, normalised to 0..360; hyprlock ignores
        it on an xray shape, so there it stays a comment."""
        for section, extra in (("label", " text = a\n"), ("image", " path = /a.png\n"), ("shape", "")):
            for value, want in (("30", "rotate 30"), ("-90", "rotate 270"), ("725.5", "rotate 5.5")):
                with self.subTest(section=section, value=value):
                    r = self.ok(f"{section} {{\n{extra} rotate = {value}\n}}")
                    self.assertIn(want, r.stdout)
                    self.assertFalse(mentions(r, "rotate"), r.stderr)
            r = self.ok(f"{section} {{\n{extra} rotate = 360\n}}")
            self.assertNotIn("rotate", r.stdout)
        r = self.ok("shape {\n xray = true\n rotate = 30\n}")
        self.assertTrue(mentions(r, "rotate"), r.stderr)
        self.assertIn("// hyprlock: rotate = 30 (ignored: hyprlock does not rotate an xray shape)", r.stdout)
        self.assertNotRegex(r.stdout, r"(?m)^\s*rotate ")

    def test_reviewer_injection_case(self):
        # inj4.conf from the review: U+2028 in a value used to end the // comment, so the KDL
        # gained a top-level `auth { pam "off"; }` that hyprlock.conf never had.
        ls = "\u2028"
        # (The review used rotate; it is real KDL now, so onclick, still a comment, carries it.)
        r = self.conf(f'label {{\n text = x\n onclick = 5{ls}}}{ls}auth {{ pam "off"; }}{ls}label {{{ls}text "y"{ls}//\n}}\n')
        self.assert_not_injected(r)
        self.assertEqual(top_nodes(r.stdout), ["general", "animations", "label"])
        self.assertIn("// hyprlock: onclick = 5\\u{2028}}\\u{2028}auth", r.stdout)

    def test_kdl_newlines_in_every_comment_path(self):
        """Each KDL line break, in each place that copies hyprlock text into a comment."""
        inj = 'auth {{ pam "off"; }}{n}general {{ hide-cursor true; }}{n}//'
        cases = {
            "unsupported key": "label {{\n text = x\n onclick = 5{n}}}{n}" + inj + "\n}}",
            "unknown key in a widget": "label {{\n text = x\n frob = 1{n}}}{n}" + inj + "\n}}",
            "unknown key name": "label {{\n text = x\n frob{n}}}{n}auth {{ pam \"off\"; }}{n}// = 1\n}}",
            "unknown section": "frob{n}auth {{\n level = 1\n}}",
            "top-level unknown": "frob = 1{n}" + inj,
            "gradient stops": "shape {{\n border_size = 2\n border_color = rgba(ff0000ff) rgba(00ff00ff) rgba(0000ffff) 0deg{n}}}{n}" + inj + "\n}}",
            "immediate_render": "general {{\n immediate_render = yes{n}}}{n}" + inj + "\n}}",
            "pam module off": "auth {{\n pam {{\n enabled = false\n module = x{n}}}{n}" + inj + "\n }}\n}}",
            "fingerprint off": "auth {{\n fingerprint {{\n ready_message = x{n}}}{n}" + inj + "\n }}\n}}",
            "skipped image": "image {{\n onclick = 5{n}}}{n}" + inj + "\n}}",
            "unsupported general": "general {{\n text_trim = 0{n}}}{n}" + inj + "\n}}",
        }
        for name, template in cases.items():
            for n in KDL_NEWLINES:
                with self.subTest(path=name, char=repr(n)):
                    r = self.conf(template.format(n=n))
                    allowed = TOP_NODES if name.startswith(("pam", "fingerprint")) else TOP_NODES - {"auth"}
                    self.assert_not_injected(r, allowed)
                    nodes = top_nodes(r.stdout)
                    self.assertEqual(nodes.count("general"), 1, r.stdout)
                    self.assertLessEqual(nodes.count("auth"), 1, r.stdout)

    def test_numbers_beyond_float_range_keep_the_default(self):
        # C++ std::stof throws past float range, so hyprlock keeps its default; Python's float()
        # gives inf, which used to come out as position "inf%".
        r = self.ok("label {\n text = x\n position = 1e999%, 5\n}\nshape {\n size = 1e39, 5\n}")
        self.assertNotIn("inf", r.stdout)
        self.assertNotIn("position", r.stdout)
        self.assertTrue(any(": position:" in w for w in warnings(r)), r.stderr)
        self.assertTrue(any(": size:" in w for w in warnings(r)), r.stderr)

    def test_lone_cr_does_not_split_a_line(self):
        # hyprlang splits lines on LF only: a CR stays inside the value, so this sets a variable
        # and nothing else. Splitting on it would turn PAM off, which hyprlock never does.
        r = self.conf("$x = a\rauth:pam:enabled = false\nlabel {\n text = $x\n}\n")
        self.assert_not_injected(r)
        self.assertNotIn("auth", top_nodes(r.stdout))
        self.assertIn('text "a\\u{d}auth:pam:enabled = false"', r.stdout)
        # CRLF line ends still work.
        r = self.conf("label {\r\n text = crlf\r\n}\r\n")
        self.assertIn('text "crlf"', r.stdout)
        self.assertEqual(warnings(r), [])

    def test_kdl_newline_in_a_sourced_file_name(self):
        # The skipped-image comment carries the file:line location.
        for n in KDL_NEWLINES + ["\n"]:
            name = f"a{n}auth {{ pam off; }}{n}x.conf"
            r = self.conf("source = a*.conf\n", files={name: "image {\n size = 10\n}\n"})
            self.assert_not_injected(r)
            self.assertIn("// hyprlock: image at", r.stdout)

    def test_strings_escape_line_breaks_and_controls(self):
        # Inside a KDL string these are legal, but written raw they hide what the string holds.
        for c in KDL_NEWLINES + BIDI + ["\x00", "\x1b", "\x7f"]:
            with self.subTest(char=repr(c)):
                r = self.conf(f"label {{\n text = a{c}b\n font_family = F{c}\n monitor = M{c}\n}}\n"
                              f"image {{\n path = /p{c}.png\n}}\ninput-field {{\n placeholder_text = p{c}\n"
                              f" dots_text_format = d{c}\n fail_text = f{c}\n}}\n")
                self.assert_not_injected(r)
                self.assertIn(f'text "a\\u{{{ord(c):x}}}b"', r.stdout)

    def conf_in(self, dirname, text, files=None, env=None):
        """Convert TEXT as hyprlock.conf in a directory called DIRNAME (a str, so a non-UTF-8
        name is spelled with surrogateescape), with FILES (name -> text) beside it, and the
        environment variables ENV added."""
        with tempfile.TemporaryDirectory() as d:
            base = Path(d) / dirname
            base.mkdir()
            for name, body in (files or {}).items():
                (base / name).write_text(body)
            p = base / "hyprlock.conf"
            p.write_text(text)
            return subprocess.run([SCRIPT, str(p)], text=True, capture_output=True, check=True,
                                  env={**os.environ, **(env or {})})

    def assert_no_surrogate_escape(self, r):
        # KDL has no surrogate code points: neither \u{d800}..\u{dfff} nor the raw character.
        self.assertIsNone(re.search(r"\\u\{d[89a-f][0-9a-f]{2}\}", r.stdout, re.I), r.stdout)
        self.assertIsNone(re.search(r"[\ud800-\udfff]", r.stdout + r.stderr), r.stdout)

    NON_UTF8_NAMES = ["bad\udcffdir", "\udc80", "a\udce9\udcb1b", "\udcc3(", "x\udcf0\udc9f\udc98y"]

    def test_path_that_is_not_utf8_is_refused(self):
        # A relative path is made absolute against the (non-UTF-8) directory of the conf file.
        for name in self.NON_UTF8_NAMES:
            with self.subTest(dir=name):
                r = self.conf_in(name, "image {\n path = rel.png\n}\nbackground {\n path = bg.png\n}\n")
                self.assert_not_injected(r)
                self.assert_no_surrogate_escape(r)
                self.assertNotIn("rel.png", "".join(l for l in r.stdout.splitlines() if not l.startswith("//")))
                self.assertNotIn("bg.png", "".join(l for l in r.stdout.splitlines() if not l.startswith("//")))
                w = [l for l in warnings(r) if "cannot be expressed in KDL" in l]
                self.assertEqual(len(w), 2, r.stderr)
                self.assertIn("rename it", w[0])
                self.assertRegex(w[0], r"\\x[0-9A-F]{2}")  # the bytes, as plain text
                self.assertIn("// hyprlock: image at", r.stdout)
                self.assertIn('color "#111111ff"', r.stdout)  # the background keeps its default look

    def test_path_from_a_non_utf8_environment_variable_is_refused(self):
        r = self.conf_in("d", "image {\n path = $X/a.png\n}\nimage {\n path = /ok.png\n}\n"
                         "background {\n path = ~/b.png\n}\n",
                         env={"X": "/a\udcffb", "HOME": "/h\udce9"})
        self.assert_not_injected(r)
        self.assert_no_surrogate_escape(r)
        self.assertIn('path "/ok.png"', r.stdout)
        self.assertEqual(len([w for w in warnings(r) if "cannot be expressed in KDL" in w]), 2, r.stderr)

    def test_non_utf8_names_in_comments_and_warnings_are_plain_text(self):
        # Sourced file with a bad name: its location lands in a skipped-image comment.
        r = self.conf_in("d", "source = ./a*.conf\n", files={"a\udcff.conf": "image {\n size = 10\n}\n"})
        self.assert_not_injected(r)
        self.assert_no_surrogate_escape(r)
        self.assertIn("a\\xFF.conf", r.stdout)
        # A source directory that does not exist, named with bad bytes.
        r = self.conf_in("d", "source = $X/*.conf\nlabel {\n text = t\n}\n", env={"X": "/n\udcfe"})
        self.assert_not_injected(r)
        self.assert_no_surrogate_escape(r)
        self.assertIn("no such file", r.stderr)

    def test_non_utf8_path_is_not_a_kdl_string_even_if_it_gets_that_far(self):
        # kdl_str is the last line of defence: it writes U+FFFD, never a surrogate.
        r = subprocess.run(
            ["python3", "-c", "import importlib.machinery as m, importlib.util as u, sys\n"
             f"l = m.SourceFileLoader('h', {str(SCRIPT)!r}); sp = u.spec_from_loader('h', l); h = u.module_from_spec(sp)\n"
             "l.exec_module(h); sys.stdout.write(h.kdl_str('a\\udcffb') + h.escape_unsafe('a\\udcffb\\ud800'))"],
            text=True, capture_output=True, check=True)
        self.assertEqual(r.stdout, '"a\ufffdb"a\\xFFb\\uD800')

    def random_text(self, rng, size):
        pool = (KDL_NEWLINES + BIDI + ["\x00", "\x1b", "\x7f", "\ufeff", "\u200d", "\xa0", "\u3000"]
                + list('{}"\\;=/*#$,%() ') + ['auth { pam "off"; }', "//", "/*", "*/", "r#\"", "{{", "}}"]
                + ["1e999", "-1e999%", "1e39", "99999999999999999999", "0x", "-", "."] * 3)
        out = []
        for _ in range(size):
            roll = rng.random()
            if roll < 0.4:
                out.append(rng.choice(pool))
            elif roll < 0.7:
                out.append(chr(rng.randrange(0x20, 0x7f)))
            else:  # any scalar value outside the surrogates
                cp = rng.randrange(0x80, 0x110000)
                out.append(chr(cp if not 0xD800 <= cp <= 0xDFFF else 0xFFFD))
        return "".join(out).replace("\n", " ")

    def test_random_unicode_never_breaks_or_injects(self):
        """Property: random text (all KDL line breaks, bidi and C0/C1 controls, KDL syntax, any
        Unicode) in every value position and as key and section names: the output always
        validates and never gains a node the converter did not write."""
        seed = int(os.environ.get("HL2KDL_SEED", "20261020"))
        rng = random.Random(seed)
        rounds = int(os.environ.get("HL2KDL_ROUNDS", "4"))
        for template, keys in self.EVERY_KEY:
            for i in range(rounds):
                lines = [f" {key} = {self.random_text(rng, rng.randrange(0, 24))}" for key, _ in keys]
                lines.append(f" {self.random_text(rng, 8)} = {self.random_text(rng, 8)}")
                text = (f"{self.random_text(rng, 6)} = {self.random_text(rng, 10)}\n"
                        + template.format("\n".join(lines)) + "\n"
                        + f"{self.random_text(rng, 6)} {{\n k = {self.random_text(rng, 10)}\n}}\n")
                with self.subTest(seed=seed, template=template.split("{")[0], round=i):
                    r = self.conf(text)
                    allowed = TOP_NODES if template.startswith("auth") else TOP_NODES - {"auth"}
                    self.assert_not_injected(r, allowed)

    def random_bad_name(self, rng):
        """A file name with lone surrogates (as os.fsdecode gives for non-UTF-8 bytes) among
        ordinary, unsafe and KDL-syntax characters. No NUL or slash: not valid in a name."""
        pool = ["\"", "\\", "{", "}", "$", "*", "?", "[", "#", " ", "\u2028", "\x85", "\u202e", "é", "\U0001f600"]
        return "".join(chr(0xDC80 + rng.randrange(128)) if rng.random() < 0.4 else
                       rng.choice(pool) if rng.random() < 0.5 else chr(rng.randrange(0x61, 0x7b))
                       for _ in range(rng.randrange(1, 8)))

    def test_random_non_utf8_names_and_environment_never_break_validate(self):
        """Property: lone surrogates from non-UTF-8 directory, file and environment names, in
        every place the converter reads a path: the output validates, has no surrogate in it,
        and gains no node."""
        rng = random.Random(int(os.environ.get("HL2KDL_SEED", "20261020")))
        for i in range(int(os.environ.get("HL2KDL_ROUNDS", "4")) * 6):
            d, f, x = (self.random_bad_name(rng) for _ in range(3))
            conf = ("source = ./*.conf\nsource = $X/*.conf\n"
                    "image {\n path = rel.png\n}\nimage {\n path = $X/y.png\n}\n"
                    "background {\n path = $X\n}\nbackground {\n path = ./b.png\n}\n"
                    "label {\n text = $X $FOO\n font_family = $X\n}\n")
            with self.subTest(dir=d, file=f, env=x):
                r = self.conf_in(d, conf, files={f + ".conf": "image {\n path = in.png\n}\nlabel {\n text = q\n}\n"},
                                 env={"X": "/" + x})
                self.assert_not_injected(r)
                self.assert_no_surrogate_escape(r)

    def test_invalid_utf8_file_and_include_are_not_rewritten(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "bad.conf"
            p.write_bytes(b"background {\n path = /tmp/\xff.png\n}\n")
            r = subprocess.run([SCRIPT, str(p)], text=True, capture_output=True, timeout=10)
            self.assertEqual(r.returncode, 1)
            self.assertEqual(r.stdout, "")
            self.assertIn("not valid UTF-8", r.stderr)
            self.assertNotIn("Traceback", r.stderr)
            main = Path(d) / "main.conf"
            main.write_text("source = bad.conf\nlabel {\n text = café\n}\n")
            r = self.run_file(main)
            self.assertIn("not valid UTF-8", r.stderr)
            self.assertNotIn("\ufffd", r.stdout)
            self.assertIn('text "café"', r.stdout)
            self.validate(r.stdout)

    def test_crossfade_is_clamped_to_three_seconds(self):
        for seconds, ms in [(0.001, 1), (3, 3000), (4, 3000), (1e30, 3000)]:
            with self.subTest(seconds=seconds):
                r = self.conf(f"background {{\n crossfade_time = {seconds}\n}}\n")
                self.assertIn(f"crossfade-ms {ms}", r.stdout)
                if seconds > 3:
                    self.assertTrue(mentions(r, "crossfade_time"), r.stderr)
                self.validate(r.stdout)

    def test_nul_reload_command_is_commented(self):
        for widget in ["background", "image"]:
            r = self.conf(f"{widget} {{\n path = /a.png\n reload_cmd = echo \x00x\n reload_time = 5\n}}\n")
            self.assertTrue(mentions(r, "reload_cmd"), r.stderr)
            self.assertNotRegex(r.stdout, r"(?m)^\s*reload-cmd ")
            self.assertIn("NUL", r.stderr)
            self.validate(r.stdout)

    def test_screenshot_reload_is_explicitly_unsupported(self):
        r = self.conf("background {\n path = screenshot\n reload_cmd = echo /a.png\n reload_time = 5\n crossfade_time = 1\n}\n")
        self.assertIn("screenshot", r.stdout)
        for key in ["reload_cmd", "reload_time", "crossfade_time"]:
            self.assertTrue(mentions(r, key), r.stderr)
        self.assertNotRegex(r.stdout, r"(?m)^\s*(reload-cmd|reload-ms|crossfade-ms) ")
        self.validate(r.stdout)

    def test_validate_warns_for_screenshot_keys_in_either_order(self):
        if not BIN.exists():
            self.skipTest(f"{BIN} not built")
        for children in [
            'screenshot; reload-ms 0; reload-cmd "echo x"; crossfade-ms 0;',
            'crossfade-ms 0; reload-cmd "echo x"; reload-ms 0; screenshot;',
        ]:
            with tempfile.TemporaryDirectory() as d:
                p = Path(d) / "config.kdl"
                p.write_text(f"background {{ {children} }}")
                r = subprocess.run([BIN, "validate", "-c", p], text=True,
                                   capture_output=True, timeout=10)
                self.assertEqual(r.returncode, 0, r.stderr)
                for key in ["reload-ms", "reload-cmd", "crossfade-ms"]:
                    self.assertIn(key, r.stderr)
                self.assertEqual(r.stderr.count("ignored with `screenshot`"), 1, r.stderr)

    def test_missing_input_and_bad_lines(self):
        r = self.ok("this line has no equals sign\nlabel {\n text = x\n}")
        self.assertTrue(any("this line" in w for w in warnings(r)), r.stderr)
        with tempfile.TemporaryDirectory() as d:
            r = subprocess.run([SCRIPT, str(Path(d) / "nothing.conf")], text=True, capture_output=True)
            self.assertEqual(r.returncode, 1)
            self.assertIn("nothing.conf", r.stderr)
        with tempfile.TemporaryDirectory() as d:
            # No config given and none found on the search path: exit 1 with a message.
            env = {k: v for k, v in os.environ.items() if k not in ("XDG_CONFIG_HOME", "XDG_CONFIG_DIRS")}
            env["HOME"] = d
            r = subprocess.run([SCRIPT], text=True, capture_output=True, env=env)
            if not Path("/etc/xdg/hypr/hyprlock.conf").exists():
                self.assertEqual(r.returncode, 1)
                self.assertIn("no hyprlock config", r.stderr)

    def test_default_config_path(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / ".config/hypr").mkdir(parents=True)
            (Path(d) / ".config/hypr/hyprlock.conf").write_text("label {\n text = found\n}\n")
            env = {k: v for k, v in os.environ.items() if k not in ("XDG_CONFIG_HOME", "XDG_CONFIG_DIRS")}
            env["HOME"] = d
            r = subprocess.run([SCRIPT], text=True, capture_output=True, check=True, env=env)
        self.assertIn('text "found"', r.stdout)


if __name__ == "__main__":
    unittest.main()
