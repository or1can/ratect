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

"""Tests for `echoed-claims.py`.

    python3 -m unittest discover -s tools -p 'test_*.py'

`tools/`'s convention is that a *decider* is tested and a candidate list is
not, because a bad ranking costs a skim. `echoed-claims.py` exits 0 like the
other candidate lists, and is tested anyway, because its failure mode is not a
bad ranking — it is **silence**. Reporting nothing is indistinguishable from
having nothing to report, so a defect in it looks exactly like a clean sweep
and gets recorded as one.

That is not hypothetical. Each of the three cases marked *regression* below is
a bug this tool actually shipped with during the hour it was written, and the
worst of them printed "Nothing you corrected is still said elsewhere" for a
diff containing the very stale sentence the tool exists to find. All three were
caught by running it against real history and none by reading it.
"""

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports an `echoed-claims.py` by path — see `test_verify_docs.load`."""
    spec = importlib.util.spec_from_file_location("echoed_claims_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


echoed_claims = load(TOOLS / "echoed-claims.py")


class Repository:
    """A throwaway git repository, so the tool runs against a real `git diff`.

    The pipeline's whole job is reading one, and every bug it shipped with was
    in that reading rather than in the matching — so these tests drive it the
    way it is actually used instead of hand-feeding it word lists.
    """

    def __init__(self, directory):
        self.path = Path(directory)
        self._git("init", "-q")
        self._git("config", "user.email", "test@example.com")
        self._git("config", "user.name", "Test")

    def _git(self, *arguments):
        subprocess.run(
            ["git", *arguments], cwd=self.path, check=True, capture_output=True
        )

    def write(self, name, text):
        (self.path / name).write_text(text, encoding="utf-8")

    def commit(self, message="wip"):
        self._git("add", "-A")
        self._git("commit", "-q", "-m", message)

    def echoes(self, length=6):
        """Every surviving run in the working tree, as `(run, file, line)`."""
        return echoed_claims.echoes(self.path, "", length)


class EchoedClaimsTests(unittest.TestCase):
    def repository(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return Repository(directory.name)

    def test_a_corrected_sentence_still_said_elsewhere_is_reported(self):
        """The whole point: one file is fixed, its twin is not."""
        repository = self.repository()
        repository.write("a.md", "The bridge interface differs for every network.\n")
        repository.write("b.md", "The bridge interface differs for every network.\n")
        repository.commit()
        repository.write("a.md", "Ratect containers are never on the default bridge.\n")

        found = repository.echoes()

        self.assertEqual(
            [(name, line) for _, name, line in found],
            [("b.md", 1)],
            "the untouched twin should be the one hit",
        )

    def test_nothing_is_reported_when_no_twin_survives(self):
        repository = self.repository()
        repository.write("a.md", "The bridge interface differs for every network.\n")
        repository.commit()
        repository.write("a.md", "Ratect containers are never on the default bridge.\n")

        self.assertEqual(repository.echoes(), [])

    def test_a_claim_split_across_a_line_wrap_is_still_found(self):
        """Regression. Prose here is hard-wrapped, and a claim does not respect
        the wrap. Matching each removed *line* on its own found nothing at all
        for the real case this tool was written for, because the phrase
        straddled a newline in both the diff and the surviving file."""
        repository = self.repository()
        wrapped = "and that one Ratect can't detect: the bridge\ninterface differs for every network it creates, so check\n"
        repository.write("a.md", wrapped)
        repository.write("b.md", "the bridge interface differs\nfor every network Ratect creates\n")
        repository.commit()
        repository.write("a.md", "Ratect containers are never on the default bridge.\n")

        found = repository.echoes()

        self.assertTrue(
            any(name == "b.md" for _, name, _ in found),
            f"a run spanning a wrap should still match: {found}",
        )

    def test_text_the_diff_also_re_added_is_not_reported(self):
        """Regression. Editing a paragraph reflows it, so most "removed" lines
        come straight back in the additions. Without subtracting them the tool
        reports what the commit *kept* — its first run produced three such hits
        and missed the real one entirely."""
        repository = self.repository()
        kept = "binding a proxy to 0.0.0.0 exposes it to everything on the network\n"
        repository.write("a.md", kept)
        repository.write("b.md", kept)
        repository.commit()
        # Rewrapped, same words: nothing was retracted.
        repository.write(
            "a.md", "binding a proxy to 0.0.0.0 exposes it\nto everything on the network\n"
        )

        self.assertEqual(
            repository.echoes(),
            [],
            "rewrapping is not retraction, so its twin is not a stale echo",
        )

    def test_a_short_restatement_is_matched_at_the_default_run_length(self):
        """Regression. At eight words this missed "for every network *it*
        creates" against "for every network *Ratect* creates" — one substituted
        word, which is what a restatement is. Six catches it, and the default
        is six for this reason rather than by taste."""
        repository = self.repository()
        repository.write("a.md", "the bridge interface differs for every network it creates\n")
        repository.write("b.md", "the bridge interface differs for every network Ratect creates\n")
        repository.commit()
        repository.write("a.md", "unrelated wording entirely\n")

        self.assertTrue(repository.echoes(length=6), "six words should match")
        self.assertFalse(
            repository.echoes(length=8),
            "eight should not — the regression this default guards against",
        )

    def test_markup_and_links_do_not_defeat_a_match(self):
        """The same claim with different emphasis, or a re-pointed link, is the
        same claim."""
        repository = self.repository()
        repository.write("a.md", "the proxy is bound to loopback only and cannot be reached\n")
        repository.write(
            "b.md",
            "the **proxy** is [bound to loopback](https://example.com/x) only and cannot be reached\n",
        )
        repository.commit()
        repository.write("a.md", "unrelated wording entirely\n")

        self.assertTrue(repository.echoes())

    def test_a_staged_correction_is_still_seen(self):
        """Regression, and the worst one. A bare `git diff` compares the working
        tree against the *index*, so staging the corrected file made the tool
        report nothing — while AGENTS.md instructs running it before committing
        and staging explicit paths. The documented workflow guaranteed silence."""
        repository = self.repository()
        repository.write("a.md", "The bridge interface differs for every network.\n")
        repository.write("b.md", "The bridge interface differs for every network.\n")
        repository.commit()
        repository.write("a.md", "Ratect containers are never on the default bridge.\n")
        repository._git("add", "a.md")

        self.assertEqual(
            [(name, line) for _, name, line in repository.echoes()],
            [("b.md", 1)],
            "staging the fix must not hide it",
        )

    def test_a_deleted_page_still_reports_its_surviving_claims(self):
        """Regression. A deleted file's hunk header is `+++ /dev/null`, so
        deciding Markdown-ness from that line alone dropped every line it
        removed — the case most worth reporting, since a deleted page's claims
        are the likeliest to live on somewhere else."""
        repository = self.repository()
        repository.write("a.md", "The bridge interface differs for every network.\n")
        repository.write("b.md", "The bridge interface differs for every network.\n")
        repository.commit()
        (repository.path / "a.md").unlink()

        self.assertEqual(
            [(name, line) for _, name, line in repository.echoes()],
            [("b.md", 1)],
            "deleting a page still retracts what it said",
        )

    def test_only_markdown_is_read(self):
        """A deleted line of source is not a retracted claim."""
        repository = self.repository()
        repository.write("a.rs", "// the bridge interface differs for every network\n")
        repository.write("b.md", "the bridge interface differs for every network\n")
        repository.commit()
        repository.write("a.rs", "// something else entirely\n")

        self.assertEqual(repository.echoes(), [])

    def test_one_line_is_reported_once_with_its_longest_run(self):
        """Overlapping runs of one sentence all match; six hits for one claim
        reads as six problems."""
        repository = self.repository()
        sentence = "the bridge interface differs for every network that Ratect creates today\n"
        repository.write("a.md", sentence)
        repository.write("b.md", sentence)
        repository.commit()
        repository.write("a.md", "unrelated wording entirely\n")

        found = repository.echoes()

        self.assertEqual(len(found), 1, f"one line, one hit: {found}")
        self.assertGreaterEqual(len(found[0][0].split()), 6)


if __name__ == "__main__":
    unittest.main()
