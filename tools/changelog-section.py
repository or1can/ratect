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

    python3 tools/changelog-section.py [--binary <package>] <version> [changelog]

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

Given `--binary`, a fragment must name that binary and that version, so a bare
pre-split heading never matches. The two binaries' version lines are
independent and older than the split: `ratect 0.13.0` is not the `0.13.0` of
July 2026, which was `ratect-compat`'s, and a release with no heading of its own
has to fail rather than publish that one's notes.

# How filtering works

Given `--binary`, an entry marked for the other binary is omitted, along with
whatever is indented under it, and every other entry stays exactly as written,
marker included. Only a marker in `ENTRY_MARKERS`, at the very start of a
top-level `- ` entry, counts: a "(ratect only)" further along often qualifies
one flag or clause rather than the entry, so it is prose and the entry reaches
both binaries' notes. A wrongly omitted entry could drop a breaking change from
a binary's notes, where a wrongly kept one only adds a line.

A subsection left with no entries goes entirely, heading and any prose in it. A
release with no entries left has an empty body, which is not an error, whatever
theme paragraph it opened with.
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
# it can't match part of a longer version number. The optional `-rc.1`-style
# suffix isn't for a real release (guideline 8's process only ever tags a
# plain bumped version) — it's so a heading can name the throwaway rc tag
# ratect#36's rollout validation pushes, which is a real SemVer prerelease
# and needs its own exact-match heading the same way a real version does.
VERSION_TOKEN_RE = re.compile(r"(?:^|\s)(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$")

# The leading text marking an entry as applying to one binary only (AGENTS.md
# guideline 7), and that binary. `**ratect.toml:**` also carries the written-for
# check's meaning; a `ratect.toml` is only ever read by `ratect`.
ENTRY_MARKERS = {
    "- **ratect only:**": "ratect",
    "- **ratect-compat only:**": "ratect-compat",
    "- **ratect.toml:**": "ratect",
}


def heading_versions(bracket_content):
    """Every version named in one heading's bracketed content."""
    versions = []
    for fragment in bracket_content.split(" · "):
        match = VERSION_TOKEN_RE.search(fragment.strip())
        if match:
            versions.append(match.group(1))
    return versions


def heading_matches(bracket_content, version, binary):
    """Whether one heading's bracketed content names `version` — of `binary`,
    if one is given."""
    if binary is None:
        return version in heading_versions(bracket_content)
    fragments = [fragment.strip() for fragment in bracket_content.split(" · ")]
    return f"{binary} {version}" in fragments


def marked_for_another_binary(line, binary):
    """Whether `line` starts an entry marked for a binary other than `binary`."""
    return any(
        line.startswith(marker) and marked != binary for marker, marked in ENTRY_MARKERS.items()
    )


def has_entries(lines):
    return any(line.startswith("- ") for line in lines)


def filtered_subsection(lines, binary):
    """One `###` subsection's lines without the entries marked for the other
    binary, or none if it had entries and none of them are left."""
    kept = []
    omitting = False
    for line in lines:
        if line.startswith("- "):
            omitting = marked_for_another_binary(line, binary)
        elif line and not line[0].isspace():
            omitting = False
        # An omitted entry's blank line still separates what's on either side
        # of it, unless one already does.
        if not omitting or (not line and kept and kept[-1]):
            kept.append(line)
    return kept if has_entries(kept) or not has_entries(lines) else []


def filtered_section(lines, binary):
    """A release section's body, keeping only what applies to `binary`."""
    subsections = [[]]
    for line in lines:
        if line.startswith("### "):
            subsections.append([])
        subsections[-1].append(line)

    kept = [filtered_subsection(subsection, binary) for subsection in subsections]
    if has_entries(lines) and not any(has_entries(subsection) for subsection in kept):
        return ""
    texts = ["\n".join(subsection).strip("\n") for subsection in kept]
    return "\n\n".join(text for text in texts if text) + "\n"


def changelog_section(text, version, binary=None):
    """The body of the `## [...]` heading naming `version` (of `binary`, if one
    is given), up to the next such heading (or end of file), without the entries
    marked for the other binary. Raises `ValueError` if no heading names it.
    """
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        match = HEADING_RE.match(line)
        if match and heading_matches(match.group("bracket"), version, binary):
            start = index + 1
            break
    if start is None:
        named = f"{binary} version" if binary else "version"
        raise ValueError(f"no changelog heading names {named} {version!r}")

    end = len(lines)
    for index in range(start, len(lines)):
        if HEADING_RE.match(lines[index]):
            end = index
            break

    if binary is not None:
        return filtered_section(lines[start:end], binary)
    return "\n".join(lines[start:end]).strip("\n") + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--binary",
        choices=("ratect", "ratect-compat"),
        help="the binary being released; only a heading naming it matches, "
        "and entries marked for the other binary are omitted",
    )
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
        section = changelog_section(text, args.version, args.binary)
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(section, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
