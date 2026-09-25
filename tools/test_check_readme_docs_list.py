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

"""Tests for `check-readme-docs-list.py`.

    python3 -m unittest discover -s tools -p 'test_*.py'

The fixture tests pin what counts as drift — a missing page, an extra one, a
reordered one, a renamed group — and what doesn't: the `docs/` link prefix,
prose inside the section, and links under a later heading.
`test_the_repository_itself_agrees` runs the check against the real files,
so the local test run catches drift too, not only the CI step.
"""

import importlib.util
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent


def load(path):
    """Imports a `check-readme-docs-list.py` by path (hyphens, so not by name)."""
    spec = importlib.util.spec_from_file_location("check_readme_docs_list_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load(TOOLS / "check-readme-docs-list.py")

SUMMARY = """\
# Summary

[Introduction](index.md)

# New to Ratect

- [Installation](installation.md)
- [Getting Started](getting-started.md)

# Reference

- [`ratect` CLI](ratect-cli.md)
"""

README = """\
# Ratect

## Documentation

Prose about the docs, with a [source](docs/index.md) link.

**New to Ratect**

- [Installation](docs/installation.md)
- [Getting Started](docs/getting-started.md)

**Reference**

- [`ratect` CLI](docs/ratect-cli.md)

## License

- [LICENSE](LICENSE)
"""


class ParsingTests(unittest.TestCase):
    def test_summary_keeps_groups_and_pages_in_order(self):
        self.assertEqual(
            check.summary_lines(SUMMARY),
            [
                "# New to Ratect",
                "- [Installation](installation.md)",
                "- [Getting Started](getting-started.md)",
                "# Reference",
                "- [`ratect` CLI](ratect-cli.md)",
            ],
        )

    def test_readme_section_matches_summary_shape(self):
        self.assertEqual(check.readme_lines(README), check.summary_lines(SUMMARY))

    def test_readme_list_ends_at_the_next_heading(self):
        self.assertNotIn("- [LICENSE](LICENSE)", check.readme_lines(README))


class DriftTests(unittest.TestCase):
    def test_no_drift_is_empty(self):
        self.assertEqual(check.drift(README, SUMMARY), "")

    def test_a_missing_page_is_drift(self):
        readme = README.replace("- [Getting Started](docs/getting-started.md)\n", "")
        diff = check.drift(readme, SUMMARY)
        self.assertIn("+- [Getting Started](getting-started.md)", diff)

    def test_an_extra_page_is_drift(self):
        readme = README.replace(
            "- [Getting Started](docs/getting-started.md)\n",
            "- [Getting Started](docs/getting-started.md)\n- [Extra](docs/extra.md)\n",
        )
        self.assertIn("-- [Extra](extra.md)", check.drift(readme, SUMMARY))

    def test_reordered_pages_are_drift(self):
        readme = README.replace(
            "- [Installation](docs/installation.md)\n- [Getting Started](docs/getting-started.md)\n",
            "- [Getting Started](docs/getting-started.md)\n- [Installation](docs/installation.md)\n",
        )
        self.assertNotEqual(check.drift(readme, SUMMARY), "")

    def test_a_renamed_group_is_drift(self):
        readme = README.replace("**Reference**", "**References**")
        diff = check.drift(readme, SUMMARY)
        self.assertIn("-# References", diff)
        self.assertIn("+# Reference", diff)


class MainTests(unittest.TestCase):
    def run_main(self, argv):
        out, err = StringIO(), StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = check.main(argv)
        return code, out.getvalue(), err.getvalue()

    def test_the_repository_itself_agrees(self):
        code, out, err = self.run_main([str(ROOT / "README.md"), str(ROOT / "docs/SUMMARY.md")])
        self.assertEqual((code, out, err), (0, "", ""))

    def test_drift_exits_one_with_the_diff_on_stdout(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            readme = Path(tmp, "README.md")
            summary = Path(tmp, "SUMMARY.md")
            readme.write_text(README.replace("- [Installation](docs/installation.md)\n", ""))
            summary.write_text(SUMMARY)
            code, out, err = self.run_main([str(readme), str(summary)])
        self.assertEqual(code, 1)
        self.assertIn("has drifted", out)
        self.assertIn("+- [Installation](installation.md)", out)

    def test_an_unparseable_summary_is_an_error_not_a_pass(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            readme = Path(tmp, "README.md")
            summary = Path(tmp, "SUMMARY.md")
            readme.write_text("# Ratect\n")
            summary.write_text("nothing here\n")
            code, _, err = self.run_main([str(readme), str(summary)])
        self.assertEqual(code, 1)
        self.assertIn("parsed no groups or pages", err)


if __name__ == "__main__":
    unittest.main()
