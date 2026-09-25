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

"""Fails if `README.md`'s Documentation list has drifted from `docs/SUMMARY.md`.

    python3 tools/check-readme-docs-list.py [readme] [summary]

`readme` defaults to `README.md` and `summary` to `docs/SUMMARY.md`, both
relative to the current directory. Exits 0 when the two agree, 1 with a
unified diff (SUMMARY.md as the expected side) when they don't.

# Why this exists

`README.md`'s Documentation section promises "the same pages, grouped as the
site groups them" — a hand-maintained mirror of `docs/SUMMARY.md`, mdBook's
sidebar. It fell behind twice (ratect#168, ratect#193), each time because a
new page went into the sidebar and not into the README. This script states
the invariant so CI catches the third time instead of a reader.

# What is compared

Each file is reduced to one line per group heading and one per page, in
document order, and the two sequences must be identical:

- `docs/SUMMARY.md`: every `# Group` heading after the file's own `# Summary`
  title, and every top-level `- [title](path)` entry. The unlisted
  `[Introduction](index.md)` line is the site's landing page, which the
  README links from its prose rather than its list.
- `README.md`: the `## Documentation` section up to the next `## ` heading —
  every bold `**Group**` line and every `- [title](docs/path)` entry, with
  the `docs/` prefix stripped so the two files' differing link roots don't
  count as drift.

A nested sidebar entry (indented `  - [...]`) is not something either list has
today, so it isn't parsed; if one appears it shows up here as a README page
with no SUMMARY.md counterpart, which is the right time to decide how the
README should render it.
"""

import argparse
import difflib
import sys
from pathlib import Path


def summary_lines(text):
    """`docs/SUMMARY.md` reduced to its group headings and page entries."""
    lines = []
    for line in text.splitlines():
        if line.startswith("# ") and line != "# Summary":
            lines.append(line)
        elif line.startswith("- ["):
            lines.append(line)
    return lines


def readme_lines(text):
    """`README.md`'s Documentation section, in `SUMMARY.md`'s own shape."""
    lines = []
    in_section = False
    for line in text.splitlines():
        if line == "## Documentation":
            in_section = True
            continue
        if not in_section:
            continue
        if line.startswith("## "):
            break
        if line.startswith("**") and line.endswith("**") and len(line) > 4:
            lines.append("# " + line[2:-2])
        elif line.startswith("- ["):
            lines.append(line.replace("](docs/", "](", 1))
    return lines


def drift(readme_text, summary_text):
    """The unified diff from README to SUMMARY, or `""` when they agree."""
    expected = summary_lines(summary_text)
    actual = readme_lines(readme_text)
    if actual == expected:
        return ""
    return "".join(
        difflib.unified_diff(
            [line + "\n" for line in actual],
            [line + "\n" for line in expected],
            fromfile="README.md (Documentation section)",
            tofile="docs/SUMMARY.md",
        )
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("readme", nargs="?", default="README.md")
    parser.add_argument("summary", nargs="?", default="docs/SUMMARY.md")
    args = parser.parse_args(argv)

    try:
        readme_text = Path(args.readme).read_text(encoding="utf-8")
        summary_text = Path(args.summary).read_text(encoding="utf-8")
    except OSError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if not summary_lines(summary_text):
        print(f"error: parsed no groups or pages from {args.summary}", file=sys.stderr)
        return 1

    diff = drift(readme_text, summary_text)
    if diff:
        print(f"error: {args.readme}'s Documentation list has drifted from {args.summary}:\n")
        print(diff, end="")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
