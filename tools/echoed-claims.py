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

"""Finds prose you just corrected that is still said, unchanged, somewhere else.

    python3 tools/echoed-claims.py [revision-range] [repo-root]

With no range, reads the working tree against `HEAD`; with one (`main..HEAD`,
`abc123...HEAD`) it reads that instead.

**A candidate list, not a verdict** — like `stale-claims.py` and
`spliced-docs.py`, it exits 0 whatever it finds, and a hit means "you probably
meant to change this too", never "this is wrong". A phrase can legitimately
appear twice: a summary and the page it summarises may say the same thing on
purpose, and only a reader can tell that from a miss.

# Why this exists

The other two tools rank by *code* churn, so neither can see this at all. What
they miss is the claim that lives in five files at once. This repo says the
same thing in `README.md`, `ROADMAP.md`, `CHANGELOG.md`, `docs/` and a doc
comment, by design — a headline list summarising a versioned entry summarising
a reference page. When the behaviour changes, whoever fixes it fixes the file
they were looking at, and the other four keep asserting the old thing with
full confidence.

That is not hypothetical. A sentence about firewall detection was corrected in
`docs/config-reference.md` while its twin in `docs/differences-from-batect.md`
was left standing, and a human reviewer reading both pages is what found it.
Guideline 16 already says to fix the class rather than the instance; this is
the part of that sweep a machine can do, because the query is not "what else is
like this" — it is the exact words you just deleted.

# What it does not catch, measured rather than assumed

Restatement, which is most of them. In the same release a `ROADMAP.md` headline
bullet kept saying the proxy rewrite was macOS/Windows-only throughout the
change that made it work everywhere — and this tool does not find it at
`--words 6` or at 5, because the headline says "`localhost` rewriting only
works on macOS/Windows" where the entry it summarises says "the proxy
`localhost`/`127.0.0.1`/`::1` rewrite ... only works on macOS/Windows". Five
words in common, then they diverge. A summary paraphrases by nature, so the
files most likely to hold a stale echo are the ones this is weakest on.

Treat it as one cheap pass that catches verbatim duplication, not as the sweep.
The sweep is still yours.

Expect noise from released `CHANGELOG.md` sections: they say what was true when
written and are append-only, so a hit there is nearly always correct as it
stands.

# How

Any line the diff removed from a Markdown file is normalised (markup, links and
punctuation dropped, case folded) and cut into overlapping runs of
`--words`-many words. Runs the diff also *added* are dropped first — rewrapping
a paragraph removes and re-adds most of it, and a thing you still say is not a
thing you retracted. Whatever is left, if it still occurs in a tracked Markdown
file, is reported with the file and line it survives at.

Runs are short enough to survive the small edits a restatement makes ("for
every network *it* creates" against "for every network *Ratect* creates" share
six words but no more), and long enough that a match is a quotation rather than
a coincidence.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

# Inline markup, links and punctuation are dropped before matching, so that a
# sentence surviving with different emphasis or a re-pointed link still counts
# as the same claim. Only the words carry the assertion.
LINK_RE = re.compile(r"\[([^\]]*)\]\([^)]*\)")
MARKUP_RE = re.compile(r"[`*_~<>\[\]()#|]")
NON_WORD_RE = re.compile(r"[^a-z0-9]+")


def normalise(line):
    """One Markdown line as the words it asserts, in order."""
    line = LINK_RE.sub(r"\1", line)
    line = MARKUP_RE.sub(" ", line)
    return NON_WORD_RE.sub(" ", line.lower()).split()


def runs(words, length):
    """Every overlapping run of `length` words, as joined strings."""
    return {" ".join(words[i : i + length]) for i in range(0, len(words) - length + 1)}


def located_runs(located_words, length):
    """`{run: first_line_number}` over `(word, line_number)` pairs."""
    found = {}
    for i in range(0, len(located_words) - length + 1):
        window = located_words[i : i + length]
        found.setdefault(" ".join(word for word, _ in window), window[0][1])
    return found


def changed_markdown_lines(root, revision_range):
    """`(removed, added)` — the words this diff deletes from and writes to
    Markdown files, each as one stream in diff order.

    Both halves are needed, not just the removals. Editing a paragraph reflows
    it, so most "removed" lines come back nearly verbatim in the additions; if
    the diff still says a thing after rewrapping, it did not retract it, and
    reporting it as a surviving echo is noise that buries the real hits.

    One stream rather than a list of lines, because prose here is hard-wrapped
    and a claim does not respect the wrap: "the bridge interface differs for
    every network it creates" was split across two lines in the diff that
    removed it, so matching line-by-line found nothing at all.
    """
    command = ["git", "diff", "--unified=0"]
    if revision_range:
        command.append(revision_range)
    diff = subprocess.run(
        command, cwd=root, capture_output=True, text=True, check=True
    ).stdout

    removed, added = [], []
    markdown = False
    for line in (diff or "").splitlines():
        if line.startswith("+++ "):
            markdown = line.endswith(".md")
        elif not markdown or line.startswith(("---", "+++")):
            continue
        elif line.startswith("-"):
            removed.extend(normalise(line[1:]))
        elif line.startswith("+"):
            added.extend(normalise(line[1:]))
    return removed, added


def tracked_markdown(root):
    """Every tracked `.md` file as `(path, [(word, line_number), ...])`.

    Line numbers ride along with the words so a run matched across a wrap can
    still be reported at the line it starts on.
    """
    listed = subprocess.run(
        ["git", "ls-files", "*.md"], cwd=root, capture_output=True, text=True, check=True
    ).stdout
    for name in (listed or "").split():
        path = root / name
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        located = [
            (word, number)
            for number, line in enumerate(text.splitlines(), start=1)
            for word in normalise(line)
        ]
        yield name, located


def echoes(root, revision_range, length):
    """`(run, file, line_number)` for each deleted run still asserted somewhere."""
    removed, added = changed_markdown_lines(root, revision_range)
    wanted = runs(removed, length) - runs(added, length)
    if not wanted:
        return []

    found = []
    for name, located in tracked_markdown(root):
        surviving = located_runs(located, length)
        # Overlapping runs of one sentence all match, so report each surviving
        # line once, with its longest matching run — the same claim listed six
        # times reads as six problems.
        best = {}
        for run in sorted(wanted & surviving.keys()):
            line = surviving[run]
            if len(run) > len(best.get(line, "")):
                best[line] = run
        found.extend((run, name, line) for line, run in sorted(best.items()))
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "revision_range",
        nargs="?",
        default="",
        help="what to diff (default: the working tree against HEAD)",
    )
    parser.add_argument("root", nargs="?", default=".", help="repository root")
    parser.add_argument(
        "--words",
        type=int,
        default=6,
        help="how many consecutive words make a quotation rather than a coincidence",
    )
    args = parser.parse_args()

    found = echoes(Path(args.root).resolve(), args.revision_range, args.words)
    if not found:
        print("Nothing you corrected is still said elsewhere.")
        return 0

    print(
        f"{len(found)} surviving echo(es) of prose this diff removed. "
        "Each may be deliberate — read it, don't sweep it."
    )
    for run, name, number in found:
        print(f"\n  {name}:{number}\n    still says: ...{run}...")
    return 0


if __name__ == "__main__":
    sys.exit(main())
