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

"""Tests for `check-pending-marks.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

The check itself: a file holding a pending mark fails, and passes once the
mark is approved. `--approve` changes only the `by` line, and only for a
mark whose keys are unchanged since it was pending at the approved commit,
so a mark added or re-marked after that commit stays pending. There is
deliberately no test that the repository's own `claims.checked.toml` holds
no pending mark: a pull request carries pending marks until its approval,
and the required Tools Tests job would then fail on every one of them. CI's
Claims Marks job runs the check instead, and making it required is the
maintainer's call. """

import importlib.util
import os
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports `check-pending-marks.py` by path (hyphens, so not by name)."""
    spec = importlib.util.spec_from_file_location("check_pending_marks_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load(TOOLS / "check-pending-marks.py")

PREAMBLE = "# A comment mark-checked keeps.\n\n"


def mark(file, note, by):
    return (
        "[[mark]]\n"
        f'file = "{file}"\n'
        'mode = "claim-words-totalising"\n'
        'unit = "0123456789abcdef"\n'
        "copies = 1\n"
        f'note = "{note}"\n'
        f'by = "{by}"\n'
        "subjects = {}\n"
    )


PENDING = "Claude Code agent, pending review by Kevin Rogers"
APPROVED = "Claude Code agent, approved by Kevin Rogers"


def marks(*entries):
    return PREAMBLE + "\n".join(entries)


def run_main(*args):
    out, err = StringIO(), StringIO()
    with redirect_stdout(out), redirect_stderr(err):
        code = check.main(list(args))
    return code, out.getvalue(), err.getvalue()


class PendingTest(unittest.TestCase):
    def test_finds_only_pending_marks(self):
        text = marks(mark("a.md", "read a", APPROVED), mark("b.md", "read b", PENDING))
        self.assertEqual([m["file"] for m in check.pending(text)], ["b.md"])

    def test_no_marks_is_none_pending(self):
        self.assertEqual(check.pending(""), [])

    def test_reads_a_header_claims_accepts_with_spacing_and_a_comment(self):
        text = marks(mark("a.md", "read a", APPROVED), mark("b.md", "read b", PENDING))
        text = text.replace("[[mark]]\nfile = \"b.md\"", " [[ mark ]]  # re-read\nfile = \"b.md\"")
        self.assertEqual([m["file"] for m in check.pending(text)], ["b.md"])

    def test_pending_is_matched_whatever_its_case(self):
        text = marks(mark("a.md", "read a", PENDING.replace("pending", "Pending")))
        self.assertEqual(len(check.pending(text)), 1)

    def test_a_file_claims_would_refuse_is_an_error(self):
        inline = 'mark = [{file = "a.md", mode = "m", unit = "u", copies = 1, note = "n", by = "%s"}]\n' % PENDING
        for text in (
            "[[mark]\nfile=1\n",
            marks(mark("a.md", "read a", APPROVED)).replace('by = "', "by = 1 #"),
            inline,
            'mark = ["x"]\n',
        ):
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    check.pending(text)


class ApproveTest(unittest.TestCase):
    def test_approves_a_mark_pending_at_the_commit_by_editing_only_by(self):
        before = marks(mark("a.md", "read a", APPROVED), mark("b.md", "read b", PENDING))
        after, approved, left = check.approve(before, before)
        self.assertEqual(after, before.replace(PENDING, APPROVED))
        self.assertEqual((approved, left), (1, 0))

    def test_leaves_a_mark_added_after_the_commit_pending(self):
        at_commit = marks(mark("a.md", "read a", PENDING))
        now = marks(mark("a.md", "read a", PENDING), mark("b.md", "read b", PENDING))
        after, approved, left = check.approve(now, at_commit)
        self.assertEqual(after, marks(mark("a.md", "read a", APPROVED), mark("b.md", "read b", PENDING)))
        self.assertEqual((approved, left), (1, 1))

    def test_leaves_a_mark_re_marked_after_the_commit_pending(self):
        at_commit = marks(mark("a.md", "read a", PENDING))
        now = marks(mark("a.md", "read a again, reworded", PENDING))
        after, approved, left = check.approve(now, at_commit)
        self.assertEqual(after, now)
        self.assertEqual((approved, left), (0, 1))

    def test_a_comment_added_beside_an_entry_since_the_commit_changes_nothing(self):
        at_commit = marks(mark("a.md", "read a", PENDING), mark("b.md", "read b", APPROVED))
        now = at_commit.replace("[[mark]]\nfile = \"b.md\"", "# A note on b.\n[[mark]]\nfile = \"b.md\"")
        after, approved, left = check.approve(now, at_commit)
        self.assertEqual(after, now.replace(PENDING, APPROVED))
        self.assertEqual((approved, left), (1, 0))

    def test_a_note_holding_a_line_separator_is_one_entry(self):
        before = marks(mark("a.md", "read\u2028a", PENDING), mark("b.md", "read\x85b", PENDING))
        after, approved, left = check.approve(before, before)
        self.assertEqual(after, before.replace(PENDING, APPROVED))
        self.assertEqual((approved, left), (2, 0))

    def test_a_by_line_it_cannot_find_refuses_the_approval_naming_the_mark(self):
        before = marks(mark("a.md", "read a", PENDING)).replace("by = ", '"by" = ')
        with self.assertRaisesRegex(ValueError, "a.md"):
            check.approve(before, before)

    def test_a_value_json_has_no_form_for_is_still_compared(self):
        before = marks(mark("a.md", "read a", PENDING)).replace("subjects = {}", "subjects = {a = 1979-05-27}")
        after, approved, left = check.approve(before, before)
        self.assertEqual((approved, left), (1, 0))

    def test_a_mark_already_approved_at_the_commit_is_not_an_approval_of_one_pending_now(self):
        at_commit = marks(mark("a.md", "read a", APPROVED))
        now = marks(mark("a.md", "read a", PENDING))
        after, approved, left = check.approve(now, at_commit)
        self.assertEqual(after, now)
        self.assertEqual((approved, left), (0, 1))


class MainTest(unittest.TestCase):
    def setUp(self):
        # Not the repository a hook running these tests was called for.
        git_env = [k for k in os.environ if k.startswith("GIT_")]
        patcher = mock.patch.dict(os.environ)
        patcher.start()
        self.addCleanup(patcher.stop)
        for k in git_env:
            del os.environ[k]
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.dir = Path(temp.name)
        self.file = self.dir / "claims.checked.toml"

    def git(self, *args):
        return subprocess.run(
            [
                "git",
                "-c", "user.name=t",
                "-c", "user.email=t@example.com",
                "-c", "commit.gpgsign=false",
                *args,
            ],
            cwd=self.dir,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def test_a_pending_mark_fails_and_passes_once_approved(self):
        self.file.write_text(marks(mark("a.md", "read a", APPROVED), mark("b.md", "read b", PENDING)))
        self.git("init", "-q")
        self.git("add", "claims.checked.toml")
        self.git("commit", "-q", "-m", "marks")
        sha = self.git("rev-parse", "HEAD")

        code, out, err = run_main("--repo-root", str(self.dir))
        self.assertEqual(code, 1)
        self.assertIn("b.md claim-words-totalising", err)
        self.assertNotIn("a.md", err)

        code, out, err = run_main("--repo-root", str(self.dir), "--list")
        self.assertEqual(code, 0, err)
        self.assertIn(sha, out)
        self.assertIn("b.md claim-words-totalising (unit 0123456789abcdef): read b", out)

        code, out, err = run_main("--repo-root", str(self.dir), "--approve", sha)
        self.assertEqual(code, 0, err)
        self.assertIn("1 approved", out)
        self.assertNotIn(PENDING, self.file.read_text())

        code, out, err = run_main("--repo-root", str(self.dir))
        self.assertEqual(code, 0, err)

    def test_approve_needs_a_full_sha_and_fails_when_it_approved_nothing(self):
        self.file.write_text(marks(mark("a.md", "read a", PENDING)))
        self.git("init", "-q")
        self.git("add", "claims.checked.toml")
        self.git("commit", "-q", "-m", "marks")
        for rev in ("HEAD", "", self.git("rev-parse", "--short", "HEAD")):
            with self.subTest(rev=rev), self.assertRaises(SystemExit):
                run_main("--repo-root", str(self.dir), "--approve", rev)
        self.file.write_text(marks(mark("a.md", "read a, then again", PENDING)))
        code, out, err = run_main("--repo-root", str(self.dir), "--approve", self.git("rev-parse", "HEAD"))
        self.assertEqual(code, 1)
        self.assertIn("0 approved, 1 still pending review", out)

    def test_list_reads_the_committed_file_not_the_working_tree(self):
        self.file.write_text(marks(mark("a.md", "read a", PENDING)))
        self.git("init", "-q")
        self.git("add", "claims.checked.toml")
        self.git("commit", "-q", "-m", "marks")
        self.file.write_text(marks(mark("a.md", "read a", PENDING), mark("b.md", "read b", PENDING)))
        code, out, err = run_main("--repo-root", str(self.dir), "--list")
        self.assertEqual(code, 0, err)
        self.assertIn("a.md", out)
        self.assertNotIn("b.md", out)

    def test_list_at_a_commit_without_the_file_is_none_pending(self):
        self.git("init", "-q")
        self.git("commit", "-q", "--allow-empty", "-m", "empty")
        code, out, err = run_main("--repo-root", str(self.dir), "--list")
        self.assertEqual(code, 0, err)
        self.assertIn(": 0", out)

    def test_no_file_is_no_pending_mark(self):
        code, out, err = run_main("--repo-root", str(self.dir))
        self.assertEqual(code, 0, err)

    def test_an_unreadable_file_is_an_error_not_a_pending_mark(self):
        self.file.write_text("[[mark]]\nfile = \n")
        code, out, err = run_main("--repo-root", str(self.dir))
        self.assertEqual(code, 2)
        self.assertIn("error:", err)

    def test_the_repo_root_defaults_to_this_repository_not_the_current_directory(self):
        self.assertEqual(check.ROOT, TOOLS.parent)
        self.assertEqual(check.parser().parse_args([]).repo_root, TOOLS.parent)


if __name__ == "__main__":
    unittest.main()
