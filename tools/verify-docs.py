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

"""Runs the documented commands and diffs their real output against the docs.

    python3 tools/verify-docs.py [repo-root]

AGENTS.md guideline 15 says to *execute* a claim rather than grep for it, after
a grep-based check reported clean while three claims were false. This is the
mechanised half of that: any claim already written as a command and its output
can be checked by a machine, every time, instead of by hand at release.

It exists because the other two checks in `tools/` cannot see this repo's
actual failure mode. Both rank a claim by how much the code *underneath* it has
moved, so they detect neglect. What has produced most of the wrong claims here
is the opposite — prose written and then falsified by a later commit on the
same branch, where claim and code move together and every churn signal reads
zero.

Opt in per block, by putting a marker line directly above a fenced block:

    <!-- verify: cargo run -q -p ratect-compat -- --list-tasks -->
    ```
    $ ratect --list-tasks
    Available tasks:
    - build: Build the application
    ```

The command runs from the repo root with `NO_COLOR`, `RUST_LOG` and `COLUMNS`
pinned so colour, log level and terminal width cannot vary between machines —
the rest of the environment is inherited, so a marker depending on anything else
about yours will diff for everyone but you. Output is compared as stdout
followed by stderr, each split into lines on its own. It runs
without a shell — `shlex.split`, so a marker is one program and its arguments
and no metacharacter is expanded behind your back. **That is tidiness, not a
sandbox.** A marker naming `sh -c ...` runs exactly what it says, verified by
probe rather than assumed, and no parsing rule can prevent that: markers come
out of files, and a file can name any program on the machine.

So the boundary is trust, not syntax: running this on a branch is running that
branch's code, the same bargain as `cargo test`. Do not run it over a pull
request you would not build, and that is the reason it is not in CI, where it
would meet every branch automatically. Its
combined output is compared against the block with any leading `$ ...` command
lines stripped, so the block keeps reading as a transcript. Both sides are
compared with trailing whitespace removed and blank lines collapsed at the
edges; nothing else is normalised, because a check that ignores differences is
the failure this is meant to replace.

Deliberately opt-in, and deliberately not in CI yet. Most of this repo's
existing example blocks are *illustrative* — `ratect resources list` prints
what a particular machine had left over — and no marker can make those
reproducible. Marking a block is a claim that its output is deterministic;
finding out which ones aren't is half the value of running this.
"""

import difflib
import os
import re
import shlex
import subprocess
import sys
from pathlib import Path

MARKER_RE = re.compile(r"^\s*<!--\s*verify:\s*(.+?)\s*-->\s*$")
FENCE_RE = re.compile(r"^\s*```")
PROMPT_RE = re.compile(r"^\s*\$ ")


def blocks(lines):
    """Every `(marker_line, command, expected_lines)` in one file."""
    for i, line in enumerate(lines):
        marker = MARKER_RE.match(line)
        if not marker:
            continue
        j = i + 1
        while j < len(lines) and not lines[j].strip():
            j += 1
        if j >= len(lines) or not FENCE_RE.match(lines[j]):
            yield i + 1, marker.group(1), None
            continue
        end = j + 1
        while end < len(lines) and not FENCE_RE.match(lines[end]):
            end += 1
        yield i + 1, marker.group(1), lines[j + 1 : end]


def dedent(lines):
    """Strips the block's own indentation — a fenced block nested in a list
    item carries the list's, which is markdown's business and not the
    command's."""
    body = [line for line in lines if line.strip()]
    common = min((len(l) - len(l.lstrip()) for l in body), default=0)
    return [line[common:] if line.strip() else line for line in lines]


def trim(text):
    return "\n".join(line.rstrip() for line in text).strip("\n")


def main(root: Path) -> int:
    env = dict(os.environ, NO_COLOR="1", RUST_LOG="off", COLUMNS="80")
    failures = 0
    checked = 0
    # `CLAUDE.md` is a symlink to `AGENTS.md` and git tracks both, so without
    # this every marked block in it is checked, and reported, twice.
    seen = set()
    for rel in subprocess.run(
        ["git", "-C", str(root), "ls-files", "*.md"], capture_output=True, text=True
    ).stdout.split():
        real = (root / rel).resolve()
        if real in seen:
            continue
        seen.add(real)
        lines = (root / rel).read_text(encoding="utf-8").splitlines()
        for line_no, command, expected in blocks(lines):
            if expected is None:
                print(f"{rel}:{line_no}: verify marker is not above a fenced block")
                failures += 1
                continue
            checked += 1
            # `shlex.split` rather than `shell=True`: one named program and
            # its arguments, with nothing expanded, chained or redirected
            # behind the author's back. It is not a safety boundary — a marker
            # is free to name `sh` — but a marker that needs a pipe is pinning
            # something other than one command's own output, and this makes
            # that obvious rather than easy.
            try:
                argv = shlex.split(command)
            except ValueError as bad:
                print(f"{rel}:{line_no}: cannot parse command ({bad})")
                failures += 1
                continue
            try:
                done = subprocess.run(
                    argv, cwd=root, capture_output=True, text=True, env=env
                )
            except OSError as bad:
                # One unrunnable marker must not abandon the rest of the sweep:
                # the summary line is what says the run happened at all, and a
                # traceback would leave every later block silently unchecked.
                print(f"{rel}:{line_no}: cannot run {argv[0]!r} ({bad.strerror})")
                failures += 1
                continue
            # Kept apart. Concatenated, a stdout that does not end in a newline
            # welds its last line onto stderr's first and reports the seam as
            # drift.
            actual = trim(done.stdout.splitlines() + done.stderr.splitlines())
            want = trim([l for l in dedent(expected) if not PROMPT_RE.match(l)])
            if actual == want:
                continue
            failures += 1
            print(f"{rel}:{line_no}: output no longer matches")
            print(f"  $ {command}")
            for line in unified(want, actual):
                print(f"  {line}")
            print()

    print(f"{checked} documented command(s) checked, {failures} out of date.")
    return 1 if failures else 0


def unified(want, actual):
    return list(
        difflib.unified_diff(
            want.splitlines(),
            actual.splitlines(),
            fromfile="documented",
            tofile="actual",
            lineterm="",
        )
    )


if __name__ == "__main__":
    sys.exit(main(Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()))
