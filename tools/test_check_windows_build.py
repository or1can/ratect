#!/usr/bin/env python3
# Copyright 2026 Orican Ltd.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Tests for `check-windows-build.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

None of these runs the real `cargo check`, which takes minutes cold: they
run the stub toolchain as `cc` does, on the command lines `cc` was seen to
give it for `ring` (the compile and archive lines below are trimmed from
that run), hold the check's command to CI's Windows Check job, and pin
`main`'s handling of a missing target and of cargo's status, with `rustc`
and `cargo` replaced. Whether the real check gets past `ring` and reports
the Rust errors is shown by running it, as ratect#904's acceptance criteria
do.
"""

import importlib.util
import os
import re
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent


def load(path):
    """Imports a `check-windows-build.py` by path (hyphens, so not by name)."""
    spec = importlib.util.spec_from_file_location("check_windows_build_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load(TOOLS / "check-windows-build.py")


def compile_line(obj):
    return ["-nologo", "-MD", "-I", "/ring/include", "-W4", "/Gy", "-DNDEBUG", f"-Fo{obj}", "-c", "/ring/curve25519.c"]


def archive_line(lib, *objects):
    return [f"-out:{lib}", "-nologo", *objects]


class StubTest(unittest.TestCase):
    """The stub as `cc` runs it: the written file, as a subprocess."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        # A space in the path, which the stub's quoting has to survive, and
        # a directory `write_stub` has to create.
        self.dir = Path(directory.name) / "with space"
        self.stub = check.write_stub(self.dir)

    def run_stub(self, args):
        return subprocess.run(
            [self.stub, *map(str, args)], cwd=self.dir, capture_output=True, text=True, timeout=30
        )

    def created(self):
        return sorted(p.name for p in self.dir.iterdir() if p != self.stub)

    def test_rewriting_leaves_an_unchanged_stub_alone_and_replaces_a_changed_one(self):
        # A run sharing the target directory may be executing the stub.
        inode = self.stub.stat().st_ino
        self.assertEqual(check.write_stub(self.dir).stat().st_ino, inode)
        self.stub.write_text("#!/bin/sh\nexit 1\n")
        check.write_stub(self.dir)
        self.assertEqual(self.stub.read_text(), check.STUB)
        self.assertEqual(self.created(), [])

    def test_the_stub_is_executable(self):
        self.assertTrue(os.access(self.stub, os.X_OK))

    def test_a_compile_creates_its_object_and_exits_0(self):
        result = self.run_stub(compile_line(self.dir / "a.o"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.created(), ["a.o"])

    def test_an_archive_creates_its_library_and_not_its_inputs(self):
        result = self.run_stub(archive_line(self.dir / "libring_core.a", "a.o", "b.o"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.created(), ["libring_core.a"])

    def test_either_spelling_of_each_flag(self):
        args = [f"/Fo{self.dir}/a.o", f"/OUT:{self.dir}/b.lib", f"-OUT:{self.dir}/c.lib", f"/out:{self.dir}/d.lib"]
        result = self.run_stub(args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.created(), ["a.o", "b.lib", "c.lib", "d.lib"])

    def test_a_gnu_flag_starting_fo_names_no_file(self):
        # `CFLAGS` reaches the stub too; `cl.exe`'s `/Fo` is case-sensitive.
        result = self.run_stub(["-fomit-frame-pointer", "-fopenmp", "-FOx", "-c", "x.c"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.created(), [])

    def test_a_bare_flag_names_no_file(self):
        result = self.run_stub(["-Fo", "-out:"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.created(), [])

    def test_both_detection_runs_print_nothing_and_exit_0(self):
        # Exiting 0 on `-?` is what makes `cc` treat the stub as MSVC.
        for args in (["-E", self.dir / "detect.c"], ["-?"]):
            with self.subTest(args=args):
                result = self.run_stub(args)
                self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        self.assertEqual(self.created(), [])


class CheckEnvTest(unittest.TestCase):
    def test_points_the_targets_cc_and_ar_at_the_stub(self):
        env = check.check_env({}, Path("/tmp/stub"))
        self.assertEqual(env["CC_x86_64_pc_windows_msvc"], "/tmp/stub")
        self.assertEqual(env["AR_x86_64_pc_windows_msvc"], "/tmp/stub")

    def test_denies_warnings_as_windows_check_does(self):
        self.assertEqual(check.check_env({}, Path("/s"))["RUSTFLAGS"], "-D warnings")

    def test_replaces_rustflags_already_set_and_keeps_the_rest_of_the_environment(self):
        # As the job's `env:` does: `-A warnings` here must not pass a
        # check Windows Check fails.
        for rustflags in ("-A warnings", ""):
            with self.subTest(rustflags=rustflags):
                env = check.check_env({"RUSTFLAGS": rustflags, "PATH": "/bin"}, Path("/s"))
                self.assertEqual((env["RUSTFLAGS"], env["PATH"]), ("-D warnings", "/bin"))

    def test_removes_encoded_rustflags_which_cargo_would_read_instead(self):
        self.assertNotIn("CARGO_ENCODED_RUSTFLAGS", check.check_env({"CARGO_ENCODED_RUSTFLAGS": "-Awarnings"}, Path("/s")))

    def test_leaves_the_host_compiler_alone(self):
        # Build scripts and proc macros still compile for the host.
        self.assertNotIn("CC", check.check_env({}, Path("/s")))


class MainTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.sysroot = Path(directory.name)
        self.target = Path(directory.name) / "target"

    def install_target(self):
        (self.sysroot / "lib" / "rustlib" / check.TARGET / "lib").mkdir(parents=True)

    def run_main(self, cargo_status=0, cargo_error=None):
        cargo = mock.Mock(return_value=subprocess.CompletedProcess([], cargo_status), side_effect=cargo_error)
        stderr = StringIO()
        with (
            mock.patch.object(check, "sysroot", return_value=str(self.sysroot)),
            mock.patch.object(check, "target_directory", return_value=self.target),
            mock.patch.object(check.subprocess, "run", cargo),
            redirect_stderr(stderr),
        ):
            status = check.main([])
        return status, stderr.getvalue(), cargo

    def test_a_missing_target_names_the_rustup_command_and_runs_no_check(self):
        status, stderr, cargo = self.run_main()
        self.assertEqual(status, 1)
        self.assertIn(f"rustup target add {check.TARGET}", stderr)
        cargo.assert_not_called()

    def test_runs_ci_s_check_for_the_windows_target_with_the_stub(self):
        self.install_target()
        status, _, cargo = self.run_main()
        self.assertEqual(status, 0)
        (command,), kwargs = cargo.call_args
        self.assertEqual(
            command,
            ["cargo", "check", "--workspace", "--all-targets", "--all-features", "--target", "x86_64-pc-windows-msvc"],
        )
        self.assertTrue(kwargs["env"]["CC_x86_64_pc_windows_msvc"].endswith("c-toolchain-stub"))
        self.assertEqual(kwargs["cwd"], check.ROOT)

    def test_the_stub_is_at_one_path_in_the_target_directory(self):
        # A new path each run would rerun `ring`'s build script, and
        # recheck everything that depends on it, every time.
        self.install_target()
        paths = []
        for _ in range(2):
            _, _, cargo = self.run_main()
            paths.append(Path(cargo.call_args.kwargs["env"]["CC_x86_64_pc_windows_msvc"]))
        self.assertEqual(paths[0], paths[1])
        self.assertEqual(paths[0].parent, self.target / "check-windows-build")
        self.assertTrue(os.access(paths[0], os.X_OK))

    def test_a_failing_or_missing_rustc_is_an_error_not_a_traceback(self):
        for error in (subprocess.CalledProcessError(1, ["rustc"]), FileNotFoundError("rustc")):
            with self.subTest(error=error):
                stderr = StringIO()
                with mock.patch.object(check, "sysroot", side_effect=error), redirect_stderr(stderr):
                    self.assertEqual(check.main([]), 1)
                self.assertIn("rustc", stderr.getvalue())

    def test_a_failing_cargo_metadata_exits_with_its_status(self):
        self.install_target()
        stderr = StringIO()
        failure = subprocess.CalledProcessError(101, ["cargo", "metadata"])
        with (
            mock.patch.object(check, "sysroot", return_value=str(self.sysroot)),
            mock.patch.object(check, "target_directory", side_effect=failure),
            redirect_stderr(stderr),
        ):
            self.assertEqual(check.main([]), 101)
        self.assertIn("cargo metadata", stderr.getvalue())

    def test_exits_with_cargos_status(self):
        self.install_target()
        self.assertEqual(self.run_main(cargo_status=101)[0], 101)

    def test_a_missing_cargo_is_an_error_not_a_traceback(self):
        self.install_target()
        status, stderr, _ = self.run_main(cargo_error=FileNotFoundError("cargo"))
        self.assertEqual(status, 1)
        self.assertIn("could not run cargo", stderr)


class WindowsCheckJobTest(unittest.TestCase):
    """The check is CI's Windows Check job's, for the Windows target."""

    def setUp(self):
        ci = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        self.job = re.split(r"\n  [\w-]+:\n", ci.split("\n  windows-check:\n", 1)[1], maxsplit=1)[0]

    def test_the_command_is_the_jobs_plus_the_target(self):
        (run,) = re.findall(r"^\s*- run: (cargo check.*)$", self.job, re.MULTILINE)
        self.assertEqual(check.CHECK, [*run.split(), "--target", check.TARGET])

    def test_the_rustflags_are_the_jobs(self):
        (rustflags,) = re.findall(r"^\s*RUSTFLAGS: (.*)$", self.job, re.MULTILINE)
        self.assertEqual(check.RUSTFLAGS, rustflags.strip())


if __name__ == "__main__":
    unittest.main()
