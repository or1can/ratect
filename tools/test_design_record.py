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

"""Tests for `design-record.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

Each test drives `main` against `FakeGitHub`, an in-process stand-in for
`gh api` holding one issue's body and comments, so nothing here reaches
GitHub. The fake answers only the calls the script is meant to make, in the
shape `gh api` takes them, so a wrong endpoint or method fails here too.
"""

import importlib.util
import json
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports `design-record.py` by path (hyphens, so not by name)."""
    spec = importlib.util.spec_from_file_location("design_record_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


record = load(TOOLS / "design-record.py")

REPO = "repos/{owner}/{repo}"


class FakeGitHub:
    """One issue, number 7, as `gh api` would serve it."""

    def __init__(self, body="", comments=(), association="OWNER"):
        self.body = body
        self.association = association
        self.comments = []
        self.calls = []
        self.clock = 0
        for text in comments:
            self.add(text)

    def add(self, text, association="OWNER"):
        self.clock += 1
        stamp = f"2026-11-17T00:00:{self.clock:02d}Z"
        comment = {
            "id": 100 + len(self.comments),
            "body": text,
            "created_at": stamp,
            "updated_at": stamp,
            "author_association": association,
            "html_url": f"https://github.com/o/r/issues/7#issuecomment-{100 + len(self.comments)}",
        }
        self.comments.append(comment)
        return comment

    def comment(self, comment_id):
        return next(c for c in self.comments if c["id"] == comment_id)

    def __call__(self, *args, input=None):
        self.calls.append(list(args))
        args = list(args)
        assert args[0] == "api", args
        method = "GET"
        if "-X" in args:
            method = args[args.index("-X") + 1]
        path = next(a for a in args[1:] if a.startswith("repos/"))
        assert path.startswith(REPO + "/"), path
        path = path[len(REPO) + 1 :]
        if method == "GET" and path == "issues/7":
            return json.dumps({"number": 7, "body": self.body, "author_association": self.association})
        if method == "GET" and path == "issues/7/comments":
            assert "--paginate" in args, args
            # Two pages, as `--paginate` writes them: arrays back to back.
            half = len(self.comments) // 2
            return json.dumps(self.comments[:half]) + "\n" + json.dumps(self.comments[half:])
        if method == "POST" and path == "issues/7/comments":
            assert args[-2:] == ["--input", "-"], args
            return json.dumps(self.add(json.loads(input)["body"]))
        if path.startswith("issues/comments/"):
            comment = self.comment(int(path.rsplit("/", 1)[1]))
            if method == "GET":
                return json.dumps(comment)
            if method == "PATCH":
                assert args[-2:] == ["--input", "-"], args
                self.clock += 1
                comment["body"] = json.loads(input)["body"]
                comment["updated_at"] = f"2026-11-17T00:00:{self.clock:02d}Z"
                return json.dumps(comment)
        raise AssertionError(f"unexpected gh call: {args}")


def run(github, *argv, stdin=""):
    """`main(argv)` against `github`; returns (exit code, stdout, stderr)."""
    out, err = StringIO(), StringIO()
    with (
        mock.patch.object(record, "gh", github),
        mock.patch.object(record, "today", lambda: "2026-11-17"),
        mock.patch("sys.stdin", StringIO(stdin)),
        redirect_stdout(out),
        redirect_stderr(err),
    ):
        code = record.main(list(argv))
    return code, out.getvalue(), err.getvalue()


def get(github):
    code, out, err = run(github, "get", "7")
    assert code == 0, err
    return json.loads(out)


class Get(unittest.TestCase):
    def test_an_issue_with_no_record_has_an_empty_one(self):
        found = get(FakeGitHub(body="## What\n\nA thing.\n", comments=["A comment."]))
        self.assertEqual(found["source"], "none")
        self.assertEqual(found["body"], "")
        self.assertEqual(found["pointers"], [])

    def test_a_record_comment_is_the_record(self):
        text = "> *Generated.*\n\n## Design record\n\n### Decisions\n\n**D1. One.** Because.\n"
        github = FakeGitHub(comments=["Unrelated.", text])
        found = get(github)
        self.assertEqual(found["source"], "comment")
        self.assertEqual(found["comment_id"], 101)
        self.assertEqual(found["body"], text)
        self.assertEqual(found["url"], github.comments[1]["html_url"])

    def test_the_heading_counts_only_as_a_line_of_its_own(self):
        found = get(FakeGitHub(comments=["See the `## Design record` on #9.", "### Design record\n"]))
        self.assertEqual(found["source"], "none")

    def test_one_legacy_record_serves_as_the_record(self):
        legacy = "## Agent Brief\n\nStuff.\n\n## settle-design record\n\nDecided.\n"
        found = get(FakeGitHub(comments=[legacy, "## settle-design record: amendment\n\nLater.\n"]))
        self.assertEqual(found["source"], "legacy")
        self.assertEqual(found["comment_id"], 100)
        self.assertEqual(found["body"], "## settle-design record\n\nDecided.\n")

    def test_a_record_outranks_a_legacy_record(self):
        found = get(FakeGitHub(comments=["## settle-design record\n", "## Design record\n"]))
        self.assertEqual((found["source"], found["comment_id"]), ("comment", 101))

    def test_several_legacy_records_must_be_merged_by_settle_design(self):
        github = FakeGitHub(comments=["## settle-design record\n\nA.\n", "## settle-design record\n\nB.\n"])
        code, out, err = run(github, "get", "7")
        self.assertEqual((code, out), (1, ""))
        self.assertIn("2 legacy", err)
        self.assertIn("settle-design", err)
        self.assertIn("merge", err)

    def test_several_records_are_refused(self):
        github = FakeGitHub(comments=["## Design record\n", "## Design record\n"])
        code, out, err = run(github, "get", "7")
        self.assertEqual((code, out), (1, ""))
        self.assertIn("2 design records", err)

    def test_with_no_comment_the_bodys_section_is_the_record(self):
        body = (
            "Part of #4.\n\n## Design record\n\n"
            "Governed by [#924's design record](https://github.com/o/r/issues/924#issuecomment-1): D5.\n\n"
            "## Scope\n\nThings.\n"
        )
        found = get(FakeGitHub(body=body, comments=["Hi."]))
        self.assertEqual(found["source"], "body")
        self.assertIsNone(found["comment_id"])
        self.assertEqual(
            found["body"],
            "## Design record\n\n"
            "Governed by [#924's design record](https://github.com/o/r/issues/924#issuecomment-1): D5.\n",
        )

    def test_a_pointer_to_another_issues_record_is_returned_as_one(self):
        text = (
            "## Design record\n\n### Mechanisms\n\n"
            "- **The lock.** governed by #31's record.\n"
            "- **The cache.** Actors: two.\n\n"
            "### Decisions\n\n**D1. Cache.** Mentions #40 but governs nothing there.\n"
        )
        found = get(FakeGitHub(comments=[text]))
        self.assertEqual(
            found["pointers"], [{"issue": 31, "line": "- **The lock.** governed by #31's record."}]
        )

    def test_its_calls_are_gh_api_reads(self):
        github = FakeGitHub(comments=["## Design record\n"])
        get(github)
        self.assertIn(["api", "--paginate", f"{REPO}/issues/7/comments"], github.calls)

    def test_repo_names_the_repository(self):
        github = mock.Mock(return_value="[]")
        github.side_effect = lambda *a, input=None: json.dumps({"body": ""}) if a[-1].endswith("/7") else "[]"
        code, _, err = run(github, "get", "7", "--repo", "o/r")
        self.assertEqual(code, 0, err)
        self.assertEqual(
            [c.args for c in github.call_args_list],
            [("api", "--paginate", "repos/o/r/issues/7/comments"), ("api", "repos/o/r/issues/7")],
        )


RECORD = (
    "## Design record\n\n"
    "### Mechanisms\n\n- **The lock.** Actors: two.\n\n"
    "### Decisions\n\n"
    "**D1. One lock.** Because.\n- *Rejected:* two locks.\n\n"
    "**D2. Shared.** Because too.\n\n"
    "### Open points\n\n- **Timeouts** (who settles it: #9).\n\n"
    "### Amendment log\n\n- 2026-11-01: created by settle-design.\n"
)


class Trust(unittest.TestCase):
    """Only a comment, or an issue body, from someone with write access to the repository counts."""

    def test_a_record_from_outside_the_repository_is_not_one(self):
        for association in ("NONE", "CONTRIBUTOR", "FIRST_TIME_CONTRIBUTOR", "FIRST_TIMER"):
            github = FakeGitHub(comments=[RECORD])
            github.add("## Design record\n\n**D1. Mine.**\n", association)
            github.add("## settle-design record\n\nNewer.\n", association)
            found = get(github)
            self.assertEqual((found["source"], found["comment_id"]), ("comment", 100), association)

    def test_a_body_record_from_outside_the_repository_is_not_one(self):
        for association in ("NONE", "CONTRIBUTOR", "FIRST_TIME_CONTRIBUTOR", "FIRST_TIMER"):
            github = FakeGitHub(body="## Design record\n\n**D1. Mine.**\n", association=association)
            self.assertEqual(get(github)["source"], "none", association)

    def test_an_append_does_not_copy_a_body_record_from_outside(self):
        github = FakeGitHub(body="## Design record\n\n**D1. Mine.**\n", association="NONE")
        code, _, err = run(github, "append", "7", "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(code, 0, err)
        self.assertNotIn("Mine", github.comments[0]["body"])

    def test_collaborators_and_members_count(self):
        for association in ("COLLABORATOR", "MEMBER"):
            github = FakeGitHub()
            github.add("## Design record\n", association)
            self.assertEqual(get(github)["source"], "comment", association)


class Create(unittest.TestCase):
    def test_settle_design_posts_a_record(self):
        github = FakeGitHub(comments=["Hi."])
        code, out, err = run(github, "create", "7", "--by", "settle-design", stdin=RECORD)
        self.assertEqual(code, 0, err)
        self.assertEqual(github.comments[-1]["body"], RECORD)
        self.assertEqual(json.loads(out)["comment_id"], github.comments[-1]["id"])
        self.assertIn(["api", "-X", "POST", f"{REPO}/issues/7/comments", "--input", "-"], github.calls)

    def test_it_replaces_legacy_records(self):
        github = FakeGitHub(comments=["## settle-design record\n\nA.\n", "## settle-design record\n\nB.\n"])
        code, _, err = run(github, "create", "7", "--by", "settle-design", stdin=RECORD)
        self.assertEqual(code, 0, err)
        self.assertEqual(get(github)["source"], "comment")

    def test_only_settle_design_creates_one(self):
        for tool in ("check-design", "ready"):
            github = FakeGitHub()
            code, _, err = run(github, "create", "7", "--by", tool, stdin=RECORD)
            self.assertEqual(code, 1, tool)
            self.assertIn("settle-design", err)
            self.assertEqual(github.comments, [])

    def test_an_issue_with_a_record_gets_no_second(self):
        github = FakeGitHub(comments=[RECORD])
        code, _, err = run(github, "create", "7", "--by", "settle-design", stdin=RECORD)
        self.assertEqual(code, 1)
        self.assertIn("already has", err)
        self.assertEqual(len(github.comments), 1)

    def test_a_record_needs_its_heading_and_log(self):
        for text in (RECORD.replace("## Design record", "## Record"), RECORD.split("### Amendment log")[0]):
            github = FakeGitHub()
            code, _, _ = run(github, "create", "7", "--by", "settle-design", stdin=text)
            self.assertEqual(code, 1)
            self.assertEqual(github.comments, [])

    def test_a_record_posted_meanwhile_is_reported(self):
        github = FakeGitHub()
        post = github.__call__

        def racing(*args, input=None):
            if "POST" in args:
                github.add(RECORD)
            return post(*args, input=input)

        code, _, err = run(racing, "create", "7", "--by", "settle-design", stdin=RECORD)
        self.assertEqual(code, 1)
        self.assertIn("2 design records", err)


class Append(unittest.TestCase):
    def append(self, github, *argv, stdin):
        code, out, err = run(github, "append", "7", *argv, stdin=stdin)
        self.assertEqual(code, 0, err)
        return json.loads(out)

    def test_a_gap_goes_before_the_log_and_is_logged(self):
        github = FakeGitHub(comments=[RECORD])
        result = self.append(github, "gap", "--by", "ready", stdin="Says nothing of a crash.\nOr a restart.\n")
        expected = RECORD.replace(
            "### Amendment log\n\n- 2026-11-01: created by settle-design.\n",
            "### Design gaps\n\n"
            "- **G1.** Says nothing of a crash.\n  Or a restart.\n"
            "  *(added at implementation by ready, 2026-11-17)*\n\n"
            "### Amendment log\n\n- 2026-11-01: created by settle-design.\n"
            "- 2026-11-17: ready: appended design gap G1.\n",
        )
        self.assertEqual(github.comments[0]["body"], expected)
        self.assertEqual(result["body"], expected)
        self.assertIn(["api", "-X", "PATCH", f"{REPO}/issues/comments/100", "--input", "-"], github.calls)

    def test_a_second_gap_follows_the_first(self):
        github = FakeGitHub(comments=[RECORD])
        self.append(github, "gap", "--by", "ready", stdin="First.")
        self.append(github, "gap", "--by", "check-design", stdin="Second.")
        body = github.comments[0]["body"]
        self.assertIn(
            "- **G1.** First.\n  *(added at implementation by ready, 2026-11-17)*\n"
            "- **G2.** Second.\n  *(added by check-design, 2026-11-17)*\n\n### Amendment log",
            body,
        )
        self.assertTrue(body.endswith("appended design gap G1.\n- 2026-11-17: check-design: appended design gap G2.\n"))

    def test_a_doubt_is_an_open_point_and_leaves_the_decision_alone(self):
        github = FakeGitHub(comments=[RECORD])
        self.append(github, "doubt", "D2", "--by", "check-design", stdin="#40 shares it now.")
        expected = RECORD.replace(
            "- **Timeouts** (who settles it: #9).\n",
            "- **Timeouts** (who settles it: #9).\n"
            "- **D2 doubted** by check-design, 2026-11-17: #40 shares it now.\n"
            "  Who settles it: the maintainer, through settle-design.\n",
        ) + "- 2026-11-17: check-design: doubted D2.\n"
        self.assertEqual(github.comments[0]["body"], expected)

    def test_ready_does_not_doubt(self):
        github = FakeGitHub(comments=[RECORD])
        code, _, err = run(github, "append", "7", "doubt", "D2", "--by", "ready", stdin="No.")
        self.assertEqual(code, 1)
        self.assertIn("ready may not", err)
        self.assertEqual(github.comments[0]["body"], RECORD)

    def test_a_doubt_names_a_decision_the_record_holds(self):
        github = FakeGitHub(comments=[RECORD])
        code, _, err = run(github, "append", "7", "doubt", "D9", "--by", "check-design", stdin="No.")
        self.assertEqual(code, 1)
        self.assertIn("no D9", err)

    def test_the_first_append_to_a_body_record_starts_the_comment(self):
        body = "## Design record\n\nGoverned by #924's design record: D5.\n\n## Scope\n\nThings.\n"
        github = FakeGitHub(body=body)
        result = self.append(github, "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(
            github.comments[0]["body"],
            "## Design record\n\nGoverned by #924's design record: D5.\n\n"
            "### Design gaps\n\n- **G1.** A gap.\n  *(added at implementation by ready, 2026-11-17)*\n\n"
            "### Amendment log\n\n"
            "- 2026-11-17: ready: started this comment from the issue body's design record.\n"
            "- 2026-11-17: ready: appended design gap G1.\n",
        )
        self.assertEqual(result["source"], "comment")
        self.assertEqual(github.body, body)

    def test_an_issue_with_no_record_gets_one(self):
        github = FakeGitHub()
        self.append(github, "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(get(github)["source"], "comment")
        self.assertTrue(github.comments[0]["body"].startswith("## Design record\n\n### Design gaps\n"))

    def test_a_legacy_record_takes_appends_at_its_end(self):
        legacy = "## settle-design record\n\nDecided.\n"
        github = FakeGitHub(comments=[legacy])
        self.append(github, "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(
            github.comments[0]["body"],
            legacy + "\n### Design gaps\n\n- **G1.** A gap.\n  *(added at implementation by ready, 2026-11-17)*\n\n"
            "### Amendment log\n\n- 2026-11-17: ready: appended design gap G1.\n",
        )

    def test_a_record_edited_since_it_was_read_is_not_overwritten(self):
        github = FakeGitHub(comments=[RECORD])
        serve = github.__call__

        def edited_meanwhile(*args, input=None):
            if args[-1].startswith(f"{REPO}/issues/comments/") and "-X" not in args:
                github.comment(100)["updated_at"] = "2026-11-17T09:00:00Z"
            return serve(*args, input=input)

        code, _, err = run(edited_meanwhile, "append", "7", "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(code, 1)
        self.assertIn("changed since", err)
        self.assertEqual(github.comments[0]["body"], RECORD)

    def test_a_write_that_did_not_land_as_written_is_reported(self):
        github = FakeGitHub(comments=[RECORD])
        serve = github.__call__

        def overwritten(*args, input=None):
            reply = serve(*args, input=input)
            if "PATCH" in args:
                github.comment(100)["body"] = "Someone else's."
            return reply

        code, _, err = run(overwritten, "append", "7", "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(code, 1)
        self.assertIn("not what was written", err)


class Amend(unittest.TestCase):
    def test_settle_design_replaces_a_decision_and_logs_why(self):
        github = FakeGitHub(comments=[RECORD])
        new = "**D1. Two locks.** Because #40.\n- *Rejected:* one lock, which #40 contends.\n"
        code, _, err = run(github, "amend", "7", "D1", "--by", "settle-design", "--why", "#40 shares it", stdin=new)
        self.assertEqual(code, 0, err)
        self.assertEqual(
            github.comments[0]["body"],
            RECORD.replace("**D1. One lock.** Because.\n- *Rejected:* two locks.\n", new)
            + "- 2026-11-17: settle-design: amended D1, because #40 shares it.\n",
        )

    def test_the_last_decision_ends_at_the_next_heading(self):
        github = FakeGitHub(comments=[RECORD])
        code, _, err = run(github, "amend", "7", "D2", "--by", "settle-design", "--why", "w", stdin="**D2. Not shared.**")
        self.assertEqual(code, 0, err)
        self.assertIn("**D2. Not shared.**\n\n### Open points", github.comments[0]["body"])

    def test_only_settle_design_amends(self):
        for tool in ("check-design", "ready"):
            github = FakeGitHub(comments=[RECORD])
            code, _, err = run(github, "amend", "7", "D1", "--by", tool, "--why", "w", stdin="**D1. X.**")
            self.assertEqual(code, 1, tool)
            self.assertEqual(github.comments[0]["body"], RECORD)

    def test_the_new_text_is_the_same_decision(self):
        github = FakeGitHub(comments=[RECORD])
        code, _, err = run(github, "amend", "7", "D1", "--by", "settle-design", "--why", "w", stdin="**D3. X.**")
        self.assertEqual(code, 1)
        self.assertIn("starts `**D1.`", err)

    def test_a_decision_the_record_lacks_is_refused(self):
        for github in (FakeGitHub(comments=[RECORD]), FakeGitHub(body="## Design record\n\n**D9. X.**\n")):
            code, _, err = run(github, "amend", "7", "D9", "--by", "settle-design", "--why", "w", stdin="**D9. Y.**")
            self.assertEqual(code, 1, err)
            self.assertFalse([c for c in github.calls if "-X" in c])


class Format(unittest.TestCase):
    """Where the record's Markdown ends and begins: fences, subheadings, sections, pointers."""

    def test_amend_keeps_a_subheading_after_the_decision(self):
        text = RECORD.replace("**D2. Shared.**", "#### The tools\n\n**D2. Shared.**")
        github = FakeGitHub(comments=[text])
        code, _, err = run(github, "amend", "7", "D1", "--by", "settle-design", "--why", "w", stdin="**D1. X.**")
        self.assertEqual(code, 0, err)
        self.assertIn("**D1. X.**\n\n#### The tools\n\n**D2. Shared.**", github.comments[0]["body"])

    def test_amend_text_is_one_decision(self):
        for extra in ("\n\n**D2. Again.**", "\n\n### Open points", "\n\n## Design record"):
            github = FakeGitHub(comments=[RECORD])
            code, _, err = run(
                github, "amend", "7", "D1", "--by", "settle-design", "--why", "w", stdin="**D1. X.**" + extra
            )
            self.assertEqual(code, 1, extra)
            self.assertIn("one decision", err)
            self.assertEqual(github.comments[0]["body"], RECORD)

    def test_a_fenced_record_is_not_a_record(self):
        quoted = "The format:\n\n```markdown\n## Design record\n\n### Decisions\n```\n"
        github = FakeGitHub(comments=[RECORD, quoted])
        self.assertEqual(get(github)["comment_id"], 100)

    def test_a_fenced_heading_does_not_end_a_section(self):
        text = RECORD.replace(
            "- **Timeouts** (who settles it: #9).\n",
            "- **Timeouts** (who settles it: #9).\n\n```sh\n# a comment\n```\n",
        )
        github = FakeGitHub(comments=[text])
        code, _, err = run(github, "append", "7", "doubt", "D1", "--by", "check-design", stdin="Why.")
        self.assertEqual(code, 0, err)
        self.assertIn("# a comment\n```\n- **D1 doubted**", github.comments[0]["body"])

    def test_a_legacy_record_is_its_section_of_the_comment(self):
        brief = (
            "## Agent Brief\n\nGoverned by #3's brief.\n\n"
            "## settle-design record\n\nDecided.\n\n"
            "## Acceptance criteria\n\n- [ ] Done.\n"
        )
        github = FakeGitHub(comments=[brief])
        found = get(github)
        self.assertEqual(found["body"], "## settle-design record\n\nDecided.\n")
        self.assertEqual(found["pointers"], [])
        code, _, err = run(github, "append", "7", "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(code, 0, err)
        self.assertEqual(
            github.comments[0]["body"],
            "## Agent Brief\n\nGoverned by #3's brief.\n\n"
            "## settle-design record\n\nDecided.\n\n"
            "### Design gaps\n\n- **G1.** A gap.\n  *(added at implementation by ready, 2026-11-17)*\n\n"
            "### Amendment log\n\n- 2026-11-17: ready: appended design gap G1.\n\n"
            "## Acceptance criteria\n\n- [ ] Done.\n",
        )

    def test_a_legacy_record_posted_after_the_record_must_be_merged(self):
        github = FakeGitHub(comments=[RECORD, "## settle-design record\n\nNewer.\n"])
        code, out, err = run(github, "get", "7")
        self.assertEqual((code, out), (1, ""))
        self.assertIn("rewrite", err)

    def test_pointers_are_only_lines_that_say_so(self):
        text = (
            "## Design record\n\nGoverned by [#924's design record](https://x/924): D5.\n\n"
            "### Mechanisms\n\n- **The lock.** governed by #31's record.\n- The cache: governed by #32's record.\n\n"
            "### Decisions\n\n**D1. Own.** Because.\n"
            "- *Rejected:* leaving the lock governed by #33's record, since it moved.\n"
        )
        found = get(FakeGitHub(comments=[text]))
        self.assertEqual([p["issue"] for p in found["pointers"]], [924, 31, 32])

    def test_a_missing_gh_or_unreadable_reply_exits_2(self):
        def missing(*args, input=None):
            raise FileNotFoundError("gh")

        def garbled(*args, input=None):
            return "To get started with GitHub CLI, please run: gh auth login"

        for github in (missing, garbled):
            code, _, err = run(github, "get", "7")
            self.assertEqual(code, 2, err)

    def test_a_body_stored_with_crlf_still_counts_as_written(self):
        github = FakeGitHub(comments=[RECORD])
        serve = github.__call__

        def crlf(*args, input=None):
            reply = serve(*args, input=input)
            if "PATCH" in args:
                github.comment(100)["body"] = github.comment(100)["body"].replace("\n", "\r\n")
            return reply

        code, _, err = run(crlf, "append", "7", "gap", "--by", "ready", stdin="A gap.")
        self.assertEqual(code, 0, err)


class Rewrite(unittest.TestCase):
    def test_settle_design_replaces_the_record_and_logs_why(self):
        github = FakeGitHub(comments=["## Design record\n\n### Design gaps\n\n- **G1.** X.\n"])
        code, _, err = run(github, "rewrite", "7", "--by", "settle-design", "--why", "settled at triage", stdin=RECORD)
        self.assertEqual(code, 0, err)
        self.assertEqual(
            github.comments[0]["body"], RECORD + "- 2026-11-17: settle-design: rewrote the record, because settled at triage.\n"
        )

    def test_it_merges_a_newer_legacy_record(self):
        github = FakeGitHub(comments=[RECORD, "## settle-design record\n\nNewer.\n"])
        code, _, err = run(github, "rewrite", "7", "--by", "settle-design", "--why", "merged", stdin=RECORD)
        self.assertEqual(code, 0, err)
        self.assertEqual(get(github)["comment_id"], 100)
        self.assertIn(github.comments[1]["html_url"], github.comments[0]["body"])

    def test_an_edit_does_not_count_as_merging_a_newer_legacy_record(self):
        github = FakeGitHub(comments=[RECORD, "## settle-design record\n\nNewer.\n"])
        github.comment(100)["updated_at"] = "2026-11-17T09:00:00Z"
        code, _, err = run(github, "get", "7")
        self.assertEqual(code, 1)
        self.assertIn("rewrite", err)

    def test_only_settle_design_rewrites(self):
        github = FakeGitHub(comments=[RECORD])
        code, _, _ = run(github, "rewrite", "7", "--by", "ready", "--why", "w", stdin=RECORD)
        self.assertEqual(code, 1)
        self.assertEqual(github.comments[0]["body"], RECORD)

    def test_it_needs_a_record_comment_and_a_whole_record(self):
        for github, text in (
            (FakeGitHub(body="## Design record\n\nGoverned by #1's record.\n"), RECORD),
            (FakeGitHub(comments=[RECORD]), "## Design record\n\nNo log.\n"),
        ):
            code, _, _ = run(github, "rewrite", "7", "--by", "settle-design", "--why", "w", stdin=text)
            self.assertEqual(code, 1)
            self.assertFalse([c for c in github.calls if "-X" in c])


if __name__ == "__main__":
    unittest.main()
