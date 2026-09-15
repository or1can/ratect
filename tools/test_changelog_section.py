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

"""Tests for `changelog-section.py`.

    python3 -m unittest discover -s tools -p 'test_*.py'

This script's output becomes a real published GitHub Release's notes body, so
a wrong answer from it is not a bad ranking a human skims past — it either
publishes a release with the wrong notes or an empty body. It is the one
script in `tools/` that *decides* something rather than ranking candidates
for a human to read (`tools/`'s other doc-integrity scripts are now the
`claims` Claude Code plugin instead — see `AGENTS.md`'s Tooling & CI section).

`test_a_short_target_is_not_matched_inside_a_longer_version` pins the bug a
naive substring search would ship with: `"7.0.0"` is a substring of
`"17.0.0"`, so `"17.0.0" in heading_text` would wrongly match a release that
was never cut.
"""

import importlib.util
import unittest
from contextlib import redirect_stdout, redirect_stderr
from io import StringIO
from pathlib import Path

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports a `changelog-section.py` by path.

    The hyphen makes it an invalid module name, so it cannot be imported by
    name.
    """
    spec = importlib.util.spec_from_file_location("changelog_section_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


changelog_section = load(TOOLS / "changelog-section.py")

COMBINED_CHANGELOG = """\
## [Unreleased]

## [ratect-compat 0.27.0 · ratect 0.6.0] - 2026-08-01

### Changed

- something ratect-compat specific
- **(ratect only)** something ratect specific

## [ratect-compat 0.26.0] - 2026-06-01

### Added

- older entry
"""

PRE_SPLIT_CHANGELOG = """\
## [0.20.0] - 2026-01-01

### Added

- the last release before the two-binary split
"""


class ChangelogSectionTests(unittest.TestCase):
    def test_extracts_the_section_for_one_of_two_versions_in_a_combined_heading(self):
        section = changelog_section.changelog_section(COMBINED_CHANGELOG, "0.27.0")
        self.assertIn("something ratect-compat specific", section)

    def test_extracts_the_same_section_for_the_other_named_version(self):
        section = changelog_section.changelog_section(COMBINED_CHANGELOG, "0.6.0")
        self.assertIn("something ratect specific", section)

    def test_stops_at_the_next_release_heading(self):
        section = changelog_section.changelog_section(COMBINED_CHANGELOG, "0.27.0")
        self.assertNotIn("older entry", section)

    def test_matches_a_bare_pre_two_binary_split_heading(self):
        section = changelog_section.changelog_section(PRE_SPLIT_CHANGELOG, "0.20.0")
        self.assertIn("the last release before the two-binary split", section)

    def test_no_matching_heading_raises_a_clear_error(self):
        with self.assertRaisesRegex(ValueError, "0.99.0"):
            changelog_section.changelog_section(COMBINED_CHANGELOG, "0.99.0")

    def test_a_short_target_is_not_matched_inside_a_longer_version(self):
        """Regression guard: a naive `"7.0.0" in heading` substring search
        matches inside `"17.0.0"`, which is a different, never-released
        version."""
        changelog = "## [ratect 17.0.0] - 2026-01-01\n\nwrong release\n"

        with self.assertRaises(ValueError):
            changelog_section.changelog_section(changelog, "7.0.0")

    def test_content_up_to_but_not_including_the_next_heading_is_returned_verbatim(self):
        changelog = "## [1.0.0] - 2026-01-01\n\nfirst line\nsecond line\n\n## [0.9.0] - 2026-01-01\nother\n"

        section = changelog_section.changelog_section(changelog, "1.0.0")

        self.assertEqual(section, "first line\nsecond line\n")

    def test_the_final_release_heading_runs_to_end_of_file(self):
        changelog = "## [1.0.0] - 2026-01-01\n\nonly section\n"

        section = changelog_section.changelog_section(changelog, "1.0.0")

        self.assertIn("only section", section)

    def test_matches_a_prerelease_suffixed_version_exactly(self):
        """A `PACKAGE/vX.Y.Z-rc.N`-style tag (ratect#36's rollout-validation
        rc tags, not a real release) needs a heading naming that exact
        suffixed version to resolve notes for it."""
        changelog = "## [ratect-compat 0.28.0-rc.1 · ratect 0.7.0-rc.1] - 2026-09-15\n\nrc notes\n"

        section = changelog_section.changelog_section(changelog, "0.28.0-rc.1")

        self.assertIn("rc notes", section)

    def test_a_bare_version_does_not_match_a_prerelease_suffixed_heading(self):
        changelog = "## [ratect-compat 0.28.0-rc.1] - 2026-09-15\n\nrc notes\n"

        with self.assertRaises(ValueError):
            changelog_section.changelog_section(changelog, "0.28.0")


class MainTests(unittest.TestCase):
    def run_main(self, argv):
        out, err = StringIO(), StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = changelog_section.main(argv)
        return code, out.getvalue(), err.getvalue()

    def write_changelog(self, tmp_path, text):
        path = tmp_path / "CHANGELOG.md"
        path.write_text(text, encoding="utf-8")
        return path

    def test_prints_the_matching_section_and_exits_zero(self):
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = self.write_changelog(Path(directory), COMBINED_CHANGELOG)

            code, out, _ = self.run_main(["0.27.0", str(path)])

            self.assertEqual(code, 0)
            self.assertIn("something ratect-compat specific", out)

    def test_missing_changelog_file_errors_clearly_on_stderr(self):
        code, out, err = self.run_main(["0.27.0", "/nonexistent/CHANGELOG.md"])

        self.assertEqual(code, 1)
        self.assertEqual(out, "")
        self.assertIn("cannot read", err)

    def test_unmatched_version_errors_clearly_on_stderr(self):
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = self.write_changelog(Path(directory), COMBINED_CHANGELOG)

            code, out, err = self.run_main(["9.9.9", str(path)])

            self.assertEqual(code, 1)
            self.assertEqual(out, "")
            self.assertIn("9.9.9", err)


if __name__ == "__main__":
    unittest.main()
