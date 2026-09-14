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

"""Prints the `CHANGELOG.md` section for one released version.

    python3 tools/changelog-section.py <version> [changelog]

`changelog` defaults to `CHANGELOG.md` in the current directory.

# Why this exists

`CHANGELOG.md`'s release headings name every version in that release at once
— `## [ratect-compat 0.27.0 · ratect 0.6.0]` — since `ratect-compat` and
`ratect` share a core and are usually released together, each bumping its own
independent version number (see `ROADMAP.md`'s versioning section). That is a
deliberate, documented convention, not a format `cargo-dist`'s own changelog
extraction understands: it matches a heading against the *exact* version
string being released, with no support for a heading naming more than one
version. Pointed at this changelog it finds nothing and silently skips release
notes.

This script is the workaround: given the version being released, it finds the
heading that names it — whichever position in the `·`-separated list it's
in — and prints that section's body, for a release-automation step to hand to
`gh release create --notes-file` (or equivalent) instead of relying on
`cargo-dist`'s own extraction.

# How matching works

A naive substring search on the version string is wrong: `"7.0.0"` is a
substring of `"17.0.0"`, so it would match a heading for a version that was
never released. Instead, each `·`-separated fragment of a heading's bracketed
content is checked for a trailing version token of its own — `"ratect
0.6.0"`'s token is `"0.6.0"`, `"0.21.0"`'s token is `"0.21.0"` — and that token
must equal the requested version exactly.
"""

import argparse
import re
import sys
from pathlib import Path

# A release heading's bracketed content, e.g. "ratect-compat 0.27.0 · ratect
# 0.6.0" or the pre-two-binary-split "0.20.0". The date suffix (`- 2026-08-01`)
# is optional so an in-progress `## [Unreleased]` heading also parses.
HEADING_RE = re.compile(r"^## \[(?P<bracket>.+?)\](?:\s*-.*)?\s*$")

# The version token trailing one `·`-separated fragment of a heading's
# bracketed content — preceded by whitespace (a named package) or nothing at
# all (a bare pre-split version), and anchored to the end of the fragment so
# it can't match part of a longer version number.
VERSION_TOKEN_RE = re.compile(r"(?:^|\s)(\d+\.\d+\.\d+)$")


def heading_versions(bracket_content):
    """Every version named in one heading's bracketed content."""
    versions = []
    for fragment in bracket_content.split(" · "):
        match = VERSION_TOKEN_RE.search(fragment.strip())
        if match:
            versions.append(match.group(1))
    return versions


def changelog_section(text, version):
    """The body of the `## [...]` heading naming `version`, up to the next
    such heading (or end of file). Raises `ValueError` if no heading names it.
    """
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        match = HEADING_RE.match(line)
        if match and version in heading_versions(match.group("bracket")):
            start = index + 1
            break
    if start is None:
        raise ValueError(f"no changelog heading names version {version!r}")

    end = len(lines)
    for index in range(start, len(lines)):
        if HEADING_RE.match(lines[index]):
            end = index
            break

    return "\n".join(lines[start:end]).strip("\n") + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version", help="the released version to extract, e.g. 0.28.0")
    parser.add_argument(
        "changelog",
        nargs="?",
        default="CHANGELOG.md",
        help="path to the changelog (default: CHANGELOG.md)",
    )
    args = parser.parse_args(argv)

    path = Path(args.changelog)
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        print(f"error: cannot read {path}: {error}", file=sys.stderr)
        return 1

    try:
        section = changelog_section(text, args.version)
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(section, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
