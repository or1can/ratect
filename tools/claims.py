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

"""Runs the `claims` CLI at the release the pre-commit hook pins.

    uv run tools/claims.py <claims.cli arguments>
    uv run tools/claims.py mark-checked --by "..." --diff-range origin/main < entries
    uv run tools/claims.py --print-rev

Reads `rev` for `or1can/claims` from `.pre-commit-config.yaml`, then runs
`python -m claims.cli` with the given arguments from that release, through
`uv run --with git+https://github.com/or1can/claims@<rev>`, on the Python
running this script, the 3.11 or later its header requires. `--print-rev`
prints the rev and nothing else; CI's Claims job reads it that way, so the
hook, CI and this command are always one release, and a Renovate bump of
the hook moves all three. Run it from the repository's root: `claims` finds
`claims.toml` and `claims.checked.toml` from the current directory, and a
relative path given to it is read from there too.

# Why this exists

`claims` is installed only inside pre-commit's environment for the hook, so
`python3 -m claims.cli` fails in a fresh shell with `No module named
'claims'` (ratect#905). Writing marks needs the CLI outside the hook.

Needs `uv` and the network, which uv uses to resolve the release's tag.
"""

import sys

# Before any other import: the PEP 723 header above is what `uv run` reads,
# and this is what a plain `python3` older than it meets instead.
if sys.version_info < (3, 11):
    sys.exit(
        "tools/claims.py needs Python 3.11 or later, and this is %d.%d. "
        "Run it as `uv run tools/claims.py`, which fetches one." % sys.version_info[:2]
    )

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "https://github.com/or1can/claims"


def rev(config):
    """The `rev` of the `or1can/claims` entry in `.pre-commit-config.yaml`'s text."""
    in_claims = False
    for line in config.splitlines():
        words = line.split("#", 1)[0].split()
        if words[:2] == ["-", "repo:"]:
            in_claims = [w.strip("\"'").removesuffix(".git") for w in words[2:]] == [REPO]
        elif in_claims and words[:1] == ["rev:"] and len(words) == 2:
            return words[1].strip("\"'")
    raise ValueError(f"no `rev:` for {REPO} in .pre-commit-config.yaml")


def command(rev, args):
    """The `uv` command that runs `claims.cli` at `rev` with `args`."""
    return [
        "uv", "run", "--no-project", "--python", sys.executable,
        "--with", f"git+{REPO}@{rev}",
        "python", "-m", "claims.cli", *args,
    ]


def main(argv):
    try:
        pinned = rev((ROOT / ".pre-commit-config.yaml").read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    if argv == ["--print-rev"]:
        print(pinned)
        return 0
    try:
        return subprocess.call(command(pinned, argv))
    except FileNotFoundError:
        print("error: `uv` is not on PATH; see https://docs.astral.sh/uv/", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
