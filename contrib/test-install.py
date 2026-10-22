#!/usr/bin/env python3
"""Installer boundary tests: temporary HOME, stub tools, isolated root/PAM reads."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("install")


def load():
    loader = importlib.machinery.SourceFileLoader("installer", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(SCRIPT.is_file(), "user-local installer is missing")
        self.installer = load()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.prefix = self.home / ".local"
        self.tools = self.home / "tools"
        self.tools.mkdir()
        cwd = Path.cwd()
        os.chdir(self.home)
        self.addCleanup(os.chdir, cwd)
        self.log = self.home / "executed"
        self.binary = self.home / "trusted-binary"
        self.binary.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$EXEC_LOG\"\n"
                               "case \"$1\" in\ncompletions) echo \"# completion $2\";;\n"
                               "validate) exit \"${VALIDATE_RC:-0}\";;\n*) exit 99;;\nesac\n")
        self.binary.chmod(0o700)
        self.tool("ldd", "echo 'libc.so => /lib/libc.so (0x1234)'\n")
        self.tool("scdoc", "echo '.TH swaywardlock 1'\n")
        stack = contextlib.ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(mock.patch.dict(os.environ, {
            "HOME": str(self.home), "XDG_CONFIG_HOME": str(self.home / "xdg"),
            "PATH": str(self.tools), "EXEC_LOG": str(self.log), "VALIDATE_RC": "0"}))
        # Real root rejection is exercised separately; tests execute as the container uid.
        self.uid = stack.enter_context(mock.patch.object(self.installer.os, "geteuid", return_value=1000))
        stack.enter_context(mock.patch.object(self.installer, "in_container", return_value=False))
        stack.enter_context(mock.patch.object(self.installer, "PAM_PATHS", (
            self.home / "pam/etc/swaywardlock", self.home / "pam/usr/swaywardlock")))
        # No writes to real /tmp locks: each test has an isolated lock directory.
        stack.enter_context(mock.patch.object(self.installer.tempfile, "gettempdir", return_value=str(self.home)))
        # os.fstat ownership follows the real container uid; leave the real flock/syscalls intact.
        actual_fstat = os.fstat
        def owned(fd):
            value = actual_fstat(fd)
            fields = list(value)
            fields[4] = 1000
            return os.stat_result(fields)
        stack.enter_context(mock.patch.object(self.installer.os, "fstat", side_effect=owned))

    def tool(self, name, body):
        path = self.tools / name
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(0o700)

    def invoke(self, *args):
        self.stdout, self.stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(self.stdout), contextlib.redirect_stderr(self.stderr):
            return self.installer.main(list(args))

    def install(self, *args):
        return self.invoke("--no-build", "--binary", str(self.binary), *args)

    def test_cli_requires_explicit_trusted_binary(self):
        for args in (("--no-build",), ("--binary", str(self.binary)),
                     ("--config-file", "file"), ("--config", "other"),
                     ("--uninstall", "--no-build"), ("--uninstall", "--binary", "file"),
                     ("--uninstall", "--config", "swaylock"), ("--unknown",)):
            with self.subTest(args=args):
                self.assertEqual(self.invoke(*args), 2)
                self.assertFalse(self.prefix.exists())
        self.assertEqual(self.invoke("--help"), 0)
        self.assertIn("ldd can execute untrusted code", self.stdout.getvalue())

    def test_root_rejected(self):
        self.uid.return_value = 0
        self.assertEqual(self.install(), 1)
        self.assertEqual(self.invoke("--uninstall"), 1)
        self.assertFalse(self.prefix.exists())

    def test_test_strings_rejected_before_any_execution(self):
        for needle in (b"swaywardlock-test", b"SWAYWARDLOCK_TEST_DELAY"):
            self.binary.write_bytes(b"#!/bin/sh\n# " + needle)
            self.tool("ldd", 'echo ldd >> "$EXEC_LOG"\n')
            self.assertEqual(self.install(), 1)
            self.assertFalse(self.log.exists())
            self.assertFalse(self.prefix.exists())

    def test_binary_read_error_fatal(self):
        self.binary.unlink()
        self.assertEqual(self.install(), 1)
        self.assertFalse(self.log.exists())
        self.assertFalse(self.prefix.exists())

    def test_ldd_failure_and_loader_errors(self):
        for body in ("exit 7", "echo 'libfoo => not found'", "echo 'loader error' >&2",
                     "echo 'undefined symbol: foo' >&2", "echo 'not a dynamic executable'"):
            with self.subTest(body=body):
                self.tool("ldd", body)
                self.assertEqual(self.install(), 1)
                self.assertFalse(self.prefix.exists())
                self.assertFalse(self.log.exists())

    def test_missing_host_tools(self):
        for tool in ("ldd", "scdoc"):
            with self.subTest(tool=tool):
                path = self.tools / tool
                data = path.read_bytes()
                path.unlink()
                self.assertEqual(self.install(), 1)
                self.assertFalse(self.prefix.exists())
                path.write_bytes(data)
                path.chmod(0o700)

    def test_generation_failure_before_replacement(self):
        self.tool("scdoc", "exit 4")
        self.assertEqual(self.install(), 1)
        self.assertFalse(self.prefix.exists())

    def test_exact_bytes_docs_completions_and_no_default_config(self):
        self.assertEqual(self.install(), 0, self.stderr.getvalue())
        self.assertEqual((self.prefix / "bin/swaywardlock").read_bytes(), self.binary.read_bytes())
        for shell in ("bash", "fish", "zsh"):
            self.assertIn("completions " + shell, self.log.read_text())
        for path in ("README.md", "docs/USING.md", "docs/screenshots/README.md", "docs/test-health/2026-10-22.md"):
            self.assertEqual((self.prefix / "share/doc/swaywardlock" / path).read_bytes(),
                             (self.installer.REPO / path).read_bytes())
        self.assertFalse((self.home / "xdg").exists())
        self.assertIn("As root, run manually", self.stdout.getvalue())

    def test_existing_pam_left_alone(self):
        path = self.installer.PAM_PATHS[1]
        path.parent.mkdir(parents=True)
        path.write_text("local policy")
        self.assertEqual(self.install(), 0)
        self.assertEqual(path.read_text(), "local policy")
        self.assertNotIn("As root", self.stdout.getvalue())

    def test_repeat_install_and_modified_uninstall_preservation(self):
        self.assertEqual(self.install(), 0)
        self.assertEqual(self.install(), 0)
        modified = self.prefix / "bin/swaywardlock"
        modified.write_text("user edit")
        unrecorded = self.prefix / "share/doc/swaywardlock/keep"
        unrecorded.write_text("keep me")
        self.assertEqual(self.install(), 1)
        self.assertEqual(self.invoke("--uninstall"), 0)
        self.assertEqual(modified.read_text(), "user edit")
        self.assertEqual(unrecorded.read_text(), "keep me")
        self.assertFalse((self.prefix / "bin/swaywardlock-swaylock-to-kdl").exists())
        self.assertTrue((self.prefix / self.installer.MANIFEST).exists())

    def test_unrecorded_identical_collision_not_silently_adopted(self):
        path = self.prefix / "bin/swaywardlock"
        path.parent.mkdir(parents=True)
        path.write_bytes(self.binary.read_bytes())
        self.assertEqual(self.install(), 1)
        self.assertIn("not adopted", self.stderr.getvalue())
        self.assertFalse((self.prefix / self.installer.MANIFEST).exists())

    def test_clean_uninstall_no_recursive_deletion(self):
        self.assertEqual(self.install(), 0)
        unrelated = self.prefix / "bin/unrelated"
        unrelated.write_text("keep")
        self.assertEqual(self.invoke("--uninstall"), 0)
        self.assertTrue(unrelated.exists())
        self.assertFalse((self.prefix / self.installer.MANIFEST).exists())
        self.assertFalse((self.prefix / "bin/swaywardlock").exists())

    def test_symlink_destination_and_ancestors_including_dangling(self):
        self.prefix.mkdir()
        for name in ("bin", "share", "bin/swaywardlock"):
            for dangling in (False, True):
                with self.subTest(name=name, dangling=dangling):
                    path = self.prefix / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    target = self.home / "target"
                    if not dangling:
                        target.mkdir(exist_ok=True)
                    path.symlink_to(target if not dangling else self.home / "absent")
                    self.assertEqual(self.install(), 1)
                    path.unlink()
        (self.prefix / "bin/swaywardlock").mkdir()
        self.assertEqual(self.install(), 1)

    def test_manifest_traversal_rejected_before_uninstall(self):
        manifest = self.prefix / self.installer.MANIFEST
        manifest.parent.mkdir(parents=True)
        outside = self.home / "outside"
        outside.write_text("preserve")
        for name in ("../outside", "/etc/pam.d/swaywardlock", "bin/../../outside", "bin/./file", "bin//file"):
            with self.subTest(name=name):
                manifest.write_text(json.dumps({"version": 1, "files": {name: "0" * 64}}))
                self.assertEqual(self.invoke("--uninstall"), 1)
                self.assertEqual(outside.read_text(), "preserve")

    def config(self):
        source = self.home / "swaylock.conf"
        source.write_text("color=123456\n")
        dest = self.home / "xdg/swaywardlock/config.kdl"
        dest.parent.mkdir(parents=True)
        dest.write_text("old config")
        return source, dest

    def test_conversion_validate_failure_preserves_config(self):
        source, dest = self.config()
        os.environ["VALIDATE_RC"] = "5"
        self.assertEqual(self.install("--config", "swaylock", "--config-file", str(source)), 1)
        self.assertEqual(dest.read_text(), "old config")
        self.assertFalse(self.prefix.exists())
        self.assertEqual(list(dest.parent.glob("*.bak.*")), [])

    def test_conversion_input_failure_preserves_config(self):
        source, dest = self.config()
        source.unlink()
        self.assertEqual(self.install("--config", "swaylock", "--config-file", str(source)), 1)
        self.assertEqual(dest.read_text(), "old config")
        self.assertFalse(self.prefix.exists())

    def test_config_backup_and_private_atomic_replacement(self):
        source, dest = self.config()
        for _ in range(2):
            self.assertEqual(self.install("--config", "swaylock", "--config-file", str(source)), 0, self.stderr.getvalue())
        backups = sorted(dest.parent.glob("*.bak.*"))
        self.assertEqual(len(backups), 2)
        self.assertIn("old config", [path.read_text() for path in backups])
        self.assertNotEqual(dest.read_text(), "old config")
        self.assertEqual(dest.stat().st_mode & 0o777, 0o600)
        self.assertFalse(list(dest.parent.glob(".swaywardlock-*")))
        self.assertEqual(self.invoke("--uninstall"), 0)
        self.assertTrue(dest.exists())
        self.assertTrue(all(path.exists() for path in backups))

    def test_backup_failure_aborts_before_replacements(self):
        source, dest = self.config()
        actual = tempfile.mkstemp
        def fail_backup(*args, **kwargs):
            if kwargs.get("prefix", "").startswith("config.kdl.bak."):
                raise OSError("backup failure")
            return actual(*args, **kwargs)
        with mock.patch.object(self.installer.tempfile, "mkstemp", side_effect=fail_backup):
            self.assertEqual(self.install("--config", "swaylock", "--config-file", str(source)), 1)
        self.assertEqual(dest.read_text(), "old config")
        self.assertFalse(self.prefix.exists())

    @contextlib.contextmanager
    def host_build(self):
        # Only distrobox and cargo are fixtures: execute the actual inner shell,
        # flock, timeout, rm, env and receipt writer through main's host route.
        repo = self.home / "repo with 'quotes'"
        target = repo / "target/install/release/swaywardlock"
        target.parent.mkdir(parents=True)
        target.write_bytes(self.binary.read_bytes())
        for name in ("README.md", "LICENSE", "docs", "examples", "contrib"):
            (repo / name).symlink_to(self.installer.REPO / name)
        for tool in ("sh", "rm", "env", "flock", "timeout", "cp", "mkdir", "python3"):
            real = shutil.which(tool, path=os.defpath)
            self.assertIsNotNone(real, f"fixture requires {tool}")
            (self.tools / tool).symlink_to(real)
        self.tool("cargo", 'mkdir -p "$CARGO_TARGET_DIR/release"; cp "$BUILD_BINARY" "$CARGO_TARGET_DIR/release/swaywardlock"')
        self.tool("distrobox", 'case "$1" in list) echo "id | swayward-dev | running";; enter) shift 3; exec "$@";; *) exit 8;; esac')
        with mock.patch.object(self.installer, "REPO", repo), \
                mock.patch.dict(os.environ, {"CONTAINER_ID": "", "BUILD_BINARY": str(self.binary)}):
            try:
                yield target
            finally:
                self.assertFalse(list(target.parents[1].glob(".build-receipt-*")))

    def test_declined_zero_enter_never_installs_stale_artifact(self):
        with self.host_build() as target:
            self.tool("distrobox", 'case "$1" in list) echo "id | swayward-dev | running";; enter) exit 0;; esac')
            self.assertEqual(self.invoke(), 1)
            self.assertTrue(target.exists())  # Inner stale-output removal never ran.
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.log.exists())

    def test_inner_build_receipt_installs_matching_bytes(self):
        with self.host_build():
            self.assertEqual(self.invoke(), 0, self.stderr.getvalue())
        self.assertEqual((self.prefix / "bin/swaywardlock").read_bytes(), self.binary.read_bytes())

    def test_failed_build_never_installs_stale_artifact(self):
        with self.host_build() as target:
            self.tool("cargo", "exit 9")
            self.assertEqual(self.invoke(), 1)
            self.assertFalse(target.exists())
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.log.exists())

    def test_inner_timeout_never_installs_stale_artifact(self):
        with self.host_build() as target:
            (self.tools / "timeout").unlink()
            self.tool("timeout", "exit 124")
            self.assertEqual(self.invoke(), 1)
            self.assertTrue(target.exists())
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.log.exists())

    def test_receipt_identity_hash_and_type_must_match(self):
        actual_run = subprocess.run
        for mutation in ("prior-invocation", "hash", "artifact", "invalid-json", "list", "symlink"):
            with self.subTest(mutation=mutation), self.host_build() as target:
                def corrupt(args, **kwargs):
                    result = actual_run(args, **kwargs)
                    if args[:2] == ["distrobox", "enter"]:
                        self.assertEqual(result.returncode, 0)
                        receipt, = target.parents[1].glob(".build-receipt-*/completed.json")
                        value = json.loads(receipt.read_text())
                        if mutation == "artifact":
                            target.write_bytes(b"changed after receipt")
                        elif mutation == "symlink":
                            receipt.unlink()
                            receipt.symlink_to(self.binary)
                        else:
                            if mutation == "prior-invocation":
                                value["invocation"] = str(receipt.parent.parent / ".build-receipt-previous/completed.json")
                            elif mutation == "hash":
                                value["sha256"] = "0" * 64
                            elif mutation == "list":
                                value = []
                            receipt.write_text("{" if mutation == "invalid-json" else json.dumps(value))
                    return result
                with mock.patch.object(self.installer.subprocess, "run", side_effect=corrupt):
                    self.assertEqual(self.invoke(), 1)
                self.assertFalse(self.prefix.exists())
                self.assertFalse(self.log.exists())
            # Each subcase has independent real shell tools and repo paths.
            shutil.rmtree(self.home / "repo with 'quotes'")
            for tool in ("sh", "rm", "env", "flock", "timeout", "cp", "mkdir", "python3"):
                (self.tools / tool).unlink()

    def test_orphan_cannot_recreate_removed_receipt_directory(self):
        with self.host_build():
            self.tool("cargo", 'rm -rf "$CARGO_TARGET_DIR"/.build-receipt-*; '
                      'cp "$BUILD_BINARY" "$CARGO_TARGET_DIR/release/swaywardlock"')
            self.assertEqual(self.invoke(), 1)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.log.exists())

    def test_outer_timeout_rejects_even_completed_receipt(self):
        actual_run = subprocess.run
        with self.host_build():
            def timeout(args, **kwargs):
                result = actual_run(args, **kwargs)
                if args[:2] == ["distrobox", "enter"]:
                    self.assertEqual(result.returncode, 0)
                    raise subprocess.TimeoutExpired(args, 930)
                return result
            with mock.patch.object(self.installer.subprocess, "run", side_effect=timeout):
                self.assertEqual(self.invoke(), 1)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.log.exists())

    def test_other_container_uses_named_existing_dev_container(self):
        self.tool("distrobox", "exit 8")
        with mock.patch.object(self.installer, "REPO", self.home / "repo"), \
                mock.patch.dict(os.environ, {"CONTAINER_ID": "other-container"}):
            (self.home / "repo").mkdir()
            self.assertEqual(self.invoke(), 1)
        self.assertIn("command failed (8)", self.stderr.getvalue())
        self.assertFalse(self.prefix.exists())

    def test_empty_or_relative_xdg_uses_home(self):
        source = self.home / "swaylock.conf"
        source.write_text("color=123456\n")
        for value in ("", "relative"):
            with self.subTest(value=value), mock.patch.dict(os.environ, {"XDG_CONFIG_HOME": value}):
                self.assertEqual(self.install("--config", "swaylock", "--config-file", str(source)), 0)
                self.assertTrue((self.home / ".config/swaywardlock/config.kdl").exists())
        self.assertFalse((self.installer.REPO / "relative/swaywardlock/config.kdl").exists())

    def test_missing_container_never_enters_or_creates(self):
        self.tool("distrobox", 'case "$1" in list) echo "id | other | running";; *) echo enter >> "$EXEC_LOG";; esac')
        with mock.patch.object(self.installer, "REPO", self.home / "repo"), \
                mock.patch.dict(os.environ, {"CONTAINER_ID": "other"}):
            (self.home / "repo").mkdir()
            self.assertEqual(self.invoke(), 1)
        self.assertFalse(self.log.exists())
        self.assertFalse(self.prefix.exists())

    def test_container_without_host_spawn_fails_without_local_writes(self):
        with mock.patch.object(self.installer, "in_container", return_value=True):
            self.assertEqual(self.install(), 1)
        self.assertIn("host-spawn", self.stderr.getvalue())
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.log.exists())

    def test_container_host_delegation_never_executes_binary_locally(self):
        self.tool("host-spawn", 'printf "%s\\n" "$@" > "$EXEC_LOG"; exit 7')
        with mock.patch.object(self.installer, "in_container", return_value=True):
            self.assertEqual(self.install(), 7)
        forwarded = self.log.read_text()
        self.assertIn("--no-pty", forwarded)
        self.assertIn("CONTAINER_ID", forwarded)
        self.assertIn("--no-build", forwarded)
        self.assertIn(str(self.binary), forwarded)
        self.assertFalse(self.prefix.exists())

    def test_timeout_no_install_and_staging_cleanup(self):
        with mock.patch.object(self.installer.subprocess, "run", side_effect=subprocess.TimeoutExpired("ldd", 60)):
            self.assertEqual(self.install(), 1)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(list(self.home.glob("swaywardlock-stage-*")))


if __name__ == "__main__":
    unittest.main()
