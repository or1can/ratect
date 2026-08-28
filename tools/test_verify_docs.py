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

"""Tests for `verify-docs.py`.

    python3 -m unittest discover -s tools -p 'test_*.py'

`verify-docs.py` is the only check in `tools/` that *decides* something — it
exits non-zero, so a wrong answer from it either blocks a release or, worse,
passes one. The other two rank candidates for a human to read, where a bad
ranking costs a skim. That asymmetry is why this file exists and they have no
equivalent yet.

Two of the cases below are the defects a review found in it (`7d5f6bd`): one
unrunnable marker abandoning the whole sweep, and stdout being welded onto
stderr. Both are regressions a test catches and a reading did not.
"""

import importlib.util
import shlex
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports a `verify-docs.py` by path.

    The hyphen makes it an invalid module name, so it cannot be imported by
    name. Taking a path also lets the regression tests below load a *previous*
    revision of the tool and show these tests failing against it.
    """
    spec = importlib.util.spec_from_file_location("verify_docs_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


verify_docs = load(TOOLS / "verify-docs.py")


class Repo:
    """A throwaway git repository — `verify-docs.py` finds files with
    `git ls-files`, so an unversioned directory has nothing to check."""

    def __enter__(self):
        self._temp = tempfile.TemporaryDirectory()
        self.root = Path(self._temp.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        return self

    def __exit__(self, *_exc):
        self._temp.cleanup()

    def write(self, name, text):
        (self.root / name).write_text(text, encoding="utf-8")
        subprocess.run(["git", "-C", str(self.root), "add", name], check=True)


def run(module, root):
    """`main`'s exit code and everything it printed."""
    out = StringIO()
    with redirect_stdout(out):
        code = module.main(root)
    return code, out.getvalue()


def marked(command, body):
    return f"<!-- verify: {command} -->\n```\n{body}\n```\n"


def echo(text):
    """A command printing `text` verbatim, without depending on a shell or on
    which `echo` is on PATH."""
    return python(f"import sys; sys.stdout.write({text!r})")


def python(source):
    """`sys.executable -c source`, quoted so `shlex.split` gives it back
    unchanged — the tool splits a marker exactly that way."""
    return f"{shlex.quote(sys.executable)} -c {shlex.quote(source)}"


class Parsing(unittest.TestCase):
    def test_finds_the_command_and_body_of_a_marked_block(self):
        lines = marked("some-command --flag", "first\nsecond").splitlines()
        found = list(verify_docs.blocks(lines))
        self.assertEqual(found, [(1, "some-command --flag", ["first", "second"])])

    def test_reports_a_marker_that_is_not_above_a_fenced_block(self):
        lines = ["<!-- verify: some-command -->", "", "ordinary prose"]
        self.assertEqual(list(verify_docs.blocks(lines)), [(1, "some-command", None)])

    def test_strips_the_indentation_a_nested_block_carries(self):
        self.assertEqual(
            verify_docs.dedent(["    indented", "", "    also indented"]),
            ["indented", "", "also indented"],
        )

    def test_trim_drops_trailing_whitespace_and_edge_blank_lines(self):
        self.assertEqual(verify_docs.trim(["", "kept   ", "  also kept", ""]), "kept\n  also kept")


class Checking(unittest.TestCase):
    def test_a_block_matching_its_command_is_up_to_date(self):
        with Repo() as repo:
            repo.write("doc.md", marked(echo("hello\n"), "hello"))
            code, output = run(verify_docs, repo.root)
        self.assertEqual(code, 0, output)
        self.assertIn("1 documented command(s) checked, 0 out of date.", output)

    def test_a_block_that_no_longer_matches_is_reported_with_its_location(self):
        with Repo() as repo:
            repo.write("doc.md", marked(echo("actual\n"), "documented"))
            code, output = run(verify_docs, repo.root)
        self.assertEqual(code, 1, output)
        self.assertIn("doc.md:1: output no longer matches", output)

    def test_the_prompt_lines_of_a_transcript_are_not_compared(self):
        with Repo() as repo:
            repo.write("doc.md", marked(echo("hello\n"), "$ some-command\nhello"))
            code, output = run(verify_docs, repo.root)
        self.assertEqual(code, 0, output)


class Regressions(unittest.TestCase):
    """The two defects a review found in this tool. Each is checked against the
    current code here, and against the revision that had the defect in
    `test_these_fail_against_the_revision_that_had_them` below."""

    MISSING_PROGRAM = "definitely-not-a-real-program-xyz"

    def a_missing_program_does_not_abandon_the_sweep(self, module):
        """One unrunnable marker used to raise `FileNotFoundError` out of
        `main`, so every later block went unchecked and the summary — the only
        line saying the run happened — never printed."""
        with Repo() as repo:
            repo.write("a.md", marked(self.MISSING_PROGRAM, "irrelevant"))
            repo.write("b.md", marked(echo("hello\n"), "hello"))
            code, output = run(module, repo.root)
        self.assertEqual(code, 1, output)
        self.assertIn(self.MISSING_PROGRAM, output)
        self.assertIn("documented command(s) checked", output)

    def stdout_is_not_welded_onto_stderr(self, module):
        """Compared as one string, a stdout not ending in a newline joined
        stderr's first line to its own last one and reported the seam as
        drift."""
        command = python(
            'import sys; sys.stdout.write("on-stdout"); sys.stderr.write("on-stderr\\n")'
        )
        with Repo() as repo:
            repo.write("doc.md", marked(command, "on-stdout\non-stderr"))
            code, output = run(module, repo.root)
        self.assertEqual(code, 0, output)

    def test_a_missing_program_does_not_abandon_the_sweep(self):
        self.a_missing_program_does_not_abandon_the_sweep(verify_docs)

    def test_stdout_is_not_welded_onto_stderr(self):
        self.stdout_is_not_welded_onto_stderr(verify_docs)

    def test_these_fail_against_the_revision_that_had_them(self):
        """A regression test that passes against the buggy code is not one.

        Loads the tool as it was before `7d5f6bd` fixed both, and asserts each
        case above fails against it — so this file's value is demonstrated
        rather than asserted. Skipped where that revision isn't reachable (a
        shallow clone, or a source tree outside git).
        """
        before = git_show("7d5f6bd^:tools/verify-docs.py")
        if before is None:
            self.skipTest("pre-fix revision of verify-docs.py is not reachable")
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "verify-docs.py"
            path.write_text(before, encoding="utf-8")
            buggy = load(path)
            for case in (
                self.a_missing_program_does_not_abandon_the_sweep,
                self.stdout_is_not_welded_onto_stderr,
            ):
                with self.subTest(case=case.__name__):
                    with self.assertRaises((AssertionError, OSError)):
                        case(buggy)


def git_show(revision):
    done = subprocess.run(
        ["git", "-C", str(TOOLS.parent), "show", revision],
        capture_output=True,
        text=True,
    )
    return done.stdout if done.returncode == 0 else None


if __name__ == "__main__":
    unittest.main()
