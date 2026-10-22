#!/usr/bin/env python3
"""Staged CLI regression checks, not package-manager or published-artifact verification.

Run inside swayward-dev with a default-feature binary:
    python3 contrib/test-verify-installed.py target/gate-normal/debug/swaywardlock
Only the installed absolute prefixes are redirected; the verifier and its inputs are real.
"""

import gzip
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(sys.argv.pop(1)).resolve()


class Installed(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.tmp.cleanup)
        cls.stage = Path(cls.tmp.name)
        checkout = cls.stage / "checkout"
        checkout.mkdir()
        for name in ("README.md", "docs", "examples", "pam"):
            (checkout / name).symlink_to(ROOT / name)
        (checkout / "contrib").mkdir()
        verifier = (ROOT / "contrib/verify-installed").read_text()
        # Preserve the real verifier's relative checkout lookup and all checks.
        verifier = verifier.replace("/usr/share/", f"{cls.stage}/usr/share/")
        verifier = verifier.replace("/etc/pam.d/", f"{cls.stage}/etc/pam.d/")
        cls.verifier = checkout / "contrib/verify-installed"
        cls.verifier.write_text(verifier)
        doc = cls.stage / "usr/share/doc/swaywardlock"
        doc.mkdir(parents=True)
        shutil.copy2(ROOT / "README.md", doc)
        for name in ("docs", "examples"):
            shutil.copytree(ROOT / name, doc / name)
        # verify-installed also checks the historical installed spec alias.
        shutil.copytree(ROOT / "docs/specs", doc / "specs")
        pam = cls.stage / "etc/pam.d/swaywardlock"
        pam.parent.mkdir(parents=True)
        shutil.copy2(ROOT / "pam/swaywardlock", pam)
        bindir = cls.stage / "bin"
        bindir.mkdir()
        (bindir / "swaywardlock").symlink_to(BIN)
        for converter in ("swaylock-to-kdl", "hyprlock-to-kdl"):
            (bindir / f"swaywardlock-{converter}").symlink_to(ROOT / "contrib" / converter)
        cls.env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
        for shell, relative in (
            ("bash", "bash-completion/completions/swaywardlock"),
            ("fish", "fish/vendor_completions.d/swaywardlock.fish"),
            ("zsh", "zsh/site-functions/_swaywardlock"),
        ):
            file = cls.stage / "usr/share" / relative
            file.parent.mkdir(parents=True, exist_ok=True)
            result = subprocess.run([BIN, "completions", shell], capture_output=True,
                                    check=True, timeout=10)
            file.write_bytes(result.stdout)
            if shell == "zsh":
                cls.zsh = result.stdout
        cls.man = cls.stage / "usr/share/man/man1/swaywardlock.1"
        cls.man.parent.mkdir(parents=True)
        result = subprocess.run(["scdoc"], input=(ROOT / "docs/swaywardlock.1.scd").read_bytes(),
                                capture_output=True, check=True, timeout=10)
        cls.roff = result.stdout
        cls.site = cls.stage / "usr/share/zsh/site-functions/_swaywardlock"
        cls.vendor = cls.stage / "usr/share/zsh/vendor-completions/_swaywardlock"
        cls.vendor.parent.mkdir(parents=True)

    def setUp(self):
        for file in (self.site, self.vendor, self.man, self.man.with_suffix(".1.gz")):
            file.unlink(missing_ok=True)
        self.site.write_bytes(self.zsh)
        self.man.write_bytes(self.roff)

    def verify(self, error=None):
        result = subprocess.run(["bash", self.verifier], env=self.env, capture_output=True,
                                text=True, timeout=20)
        output = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0 if error is None else 1, output)
        self.assertIn("verify-installed: ok" if error is None else error, output)

    def test_site_functions_plain_man(self):
        self.verify()

    def test_vendor_completions_gzip_man(self):
        self.site.rename(self.vendor)
        self.man.unlink()
        self.man.with_suffix(".1.gz").write_bytes(gzip.compress(self.roff))
        self.verify()

    def test_missing_zsh(self):
        self.site.unlink()
        self.verify("no nonempty zsh completion")

    def test_empty_zsh(self):
        self.site.write_bytes(b"")
        self.verify("no nonempty zsh completion")

    def test_garbage_plain_man(self):
        self.man.write_text("not a man page\n")
        self.verify("invalid man page")

    def test_garbage_gzip_man(self):
        self.man.unlink()
        self.man.with_suffix(".1.gz").write_text("not gzip\n")
        self.verify("invalid man page")

    def test_missing_man_section(self):
        self.man.write_bytes(self.roff.replace(b'.SH NAME', b'.SH OMITTED'))
        self.assertNotEqual(self.man.read_bytes(), self.roff, "fixture must remove NAME")
        self.verify("invalid man page")


if __name__ == "__main__":
    unittest.main()
