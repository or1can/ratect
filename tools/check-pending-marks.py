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

# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///

"""Fails while `claims.checked.toml` holds a mark still pending review.

    uv run tools/check-pending-marks.py [--repo-root R]
    uv run tools/check-pending-marks.py [--repo-root R] --list
    uv run tools/check-pending-marks.py [--repo-root R] --approve SHA

`R` defaults to the repository this script is in. With no option, exits 0
when no mark's `by` says `pending review by` (in any case), and 1 listing
each one that does. `--list` prints the marks pending in the committed file
at `HEAD`, headed by that commit's SHA, for a pull request's body, and
exits 0. `--approve SHA`, a full commit SHA, rewrites `pending review by`
to `approved by` in the working tree's file, for each mark whose keys are
unchanged since it was pending at `SHA`; it exits 0 once it has approved
one or more, and 1 when it approved none. Every mode exits 2 when the file
doesn't parse as TOML, holds anything but `[[mark]]` tables, or has a `by`
that isn't a string, or when `--approve` can't find a mark's `by` line to
rewrite.

# Why this exists

An agent writes its `claims` marks as pending the maintainer's review, and
carries on rather than waiting for approval (ratect#905; `agents/claims.md`
has the flow). A mark's attribution has to be true once it reaches `main`,
so CI's Claims Marks job runs this check, and a pending mark fails it until
the maintainer has approved it and the mark has been rewritten.

# What `--approve` changes

Only the `by` line, never by re-running `mark-checked`. The `claims` plugin
reads nothing from `by`, so editing it leaves the mark applying exactly as
before, while re-marking would re-record the mark's subjects from whatever
is on disk at approval, which nobody may have read.

And only a mark whose keys are all equal to a pending mark's at the
approved commit; comments and layout don't count. A mark added after that
commit, or re-marked since (with a new note or subjects), is not what was
approved, so it stays pending until a later commit's approval covers it.
"""

import sys

# Before any other import: the PEP 723 header above is what `uv run` reads,
# and this is what a plain `python3` older than it meets instead.
if sys.version_info < (3, 11):
    sys.exit(
        "tools/check-pending-marks.py needs Python 3.11 or later, and this is %d.%d. "
        "Run it as `uv run tools/check-pending-marks.py`, which fetches one." % sys.version_info[:2]
    )

import argparse
import json
import re
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILE = "claims.checked.toml"
PENDING = re.compile(r"pending review by", re.IGNORECASE)
APPROVED = "approved by"
# The header `claims` itself recognises, comment and spacing included: a
# copy of `_MARK_HEADER_RE` in `claims/checked.py`. CI's Claims job compares
# it with the pinned release's; `lines` and the `match` in `entries` copy
# how v0.28.1's `claims` applies it, which nothing compares: on a bump,
# re-read them and update that version.
HEADER = re.compile(r"^\s*\[\[\s*mark\s*\]\]\s*(#.*)?$")
BY = re.compile(r"^\s*by\s*=")


def lines(text):
    """`text` split after each `\n` only, as `claims` numbers its lines.

    Not `str.splitlines`, which also splits at characters a note may hold
    unescaped, such as U+2028.
    """
    return [line for line in re.split(r"(?<=\n)", text) if line]


def entries(text):
    """`text` split at each `[[mark]]` header: the preamble, then one string per entry."""
    parts = [""]
    for line in lines(text):
        if HEADER.match(line.rstrip("\n")):
            parts.append("")
        parts[-1] += line
    return parts


def marks(text):
    """Every mark in `text`, as its parsed keys; `ValueError` on a file `claims` would refuse.

    As `claims` itself requires, the file holds only `[[mark]]` tables, one
    per header line, so each mark is the one under the matching header and
    `approve` can rewrite its own text.
    """
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"{FILE} does not parse: {error}") from error
    found = data.get("mark", [])
    if set(data) - {"mark"} or not isinstance(found, list) or not all(isinstance(m, dict) for m in found):
        raise ValueError(f"{FILE} holds something other than [[mark]] tables")
    if len(found) != len(entries(text)) - 1:
        raise ValueError(f"{FILE}'s marks are not each written as a [[mark]] table")
    for mark in found:
        if not isinstance(mark.get("by", ""), str):
            raise ValueError(f"a mark's `by` is not a string: {describe(mark)}")
    return found


def is_pending(mark):
    return bool(PENDING.search(mark.get("by", "")))


def pending(text):
    """Each pending mark in `text`, as its parsed keys."""
    return [mark for mark in marks(text) if is_pending(mark)]


def fingerprint(mark):
    """A mark compared as its keys, not its text, so comments and layout don't count."""
    return json.dumps(mark, sort_keys=True, default=str)


def approve(text, at_commit):
    """`text` with each mark pending at `at_commit` approved.

    Returns the new text, how many marks were approved, and how many stay
    pending.
    """
    approvable = {fingerprint(mark) for mark in pending(at_commit)}
    parts = entries(text)
    approved = still_pending = 0
    for i, mark in enumerate(marks(text), 1):
        if not is_pending(mark):
            continue
        if fingerprint(mark) not in approvable:
            still_pending += 1
            continue
        entry = lines(parts[i])
        by = [n for n, line in enumerate(entry) if BY.match(line) and PENDING.search(line)]
        if len(by) != 1:
            raise ValueError(f"no `by = ` line saying 'pending review by' to rewrite for {describe(mark)}")
        entry[by[0]] = PENDING.sub(APPROVED, entry[by[0]], count=1)
        parts[i] = "".join(entry)
        approved += 1
    return "".join(parts), approved, still_pending


def describe(mark):
    return f"{mark.get('file')} {mark.get('mode')} (unit {mark.get('unit')}): {mark.get('note')}"


def git(root, *args):
    return subprocess.run(
        ["git", *args], cwd=root, check=True, capture_output=True, encoding="utf-8"
    ).stdout


def full_sha(value):
    """A full commit SHA, so an approval names the commit approved, not `HEAD`."""
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value):
        raise argparse.ArgumentTypeError(f"not a full commit SHA: {value!r}")
    return value


def parser():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--list", action="store_true", help="Print the marks pending at HEAD.")
    mode.add_argument("--approve", metavar="SHA", type=full_sha, help="Approve the marks pending at SHA.")
    return parser


def main(argv=None):
    args = parser().parse_args(argv)
    path = args.repo_root / FILE

    try:
        if args.list:
            sha = git(args.repo_root, "rev-parse", "HEAD").strip()
            committed = git(args.repo_root, "ls-tree", "--name-only", "HEAD", FILE).strip()
            found = pending(git(args.repo_root, "show", f"HEAD:./{FILE}") if committed else "")
            print(f"claims marks pending review at {sha}: {len(found)}")
            for mark in found:
                print(f"- {describe(mark)}")
            return 0

        if args.approve is not None:
            at_commit = git(args.repo_root, "show", f"{args.approve}:./{FILE}")
            text, approved, left = approve(path.read_text(encoding="utf-8"), at_commit)
            path.write_text(text, encoding="utf-8")
            print(f"{approved} approved, {left} still pending review")
            return 0 if approved else 1

        found = pending(path.read_text(encoding="utf-8") if path.exists() else "")
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        detail = getattr(error, "stderr", None) or error
        print(f"error: {detail}", file=sys.stderr)
        return 2

    if found:
        print(
            f"error: claims marks pending review: {len(found)}; "
            f"see agents/claims.md for approving them:",
            file=sys.stderr,
        )
        for mark in found:
            print(f"- {describe(mark)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
