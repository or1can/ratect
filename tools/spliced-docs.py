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

"""Finds doc comments that document an item other than the one they sit on.

    python3 tools/spliced-docs.py [repo-root]

Inserting a Rust item anchored on the *following* item's `///` lines splices
the new item into its neighbour's documentation. Nothing catches it: the
compiler is happy, rustdoc renders the merged text under whichever item ends
up last, and the item the text was written for is left bare. AGENTS.md
guideline 15 has a rule against it, written after four occurrences; a fifth
(`load_project`'s documentation, stranded on `to_native_toml`) survived
untouched on `main` for long enough to be found by a reviewer rather than by
the rule. A rule you can only follow by remembering it is worth a check.

A candidate list, not a verdict — the same contract
[`stale-claims.py`](stale-claims.py) sets. On this repo it reports four, of
which two were real: `resize_tty`'s documentation stranded on
`stream_logs_as_interleaved_events`, and `labels_for`'s on `network_labels`.
The other two are a long doc whose paragraphs happen to start with a summary
verb. Read each, and exit status stays 0 either way.

How it works, and why:

  run       one contiguous block of `///` lines, plus the attributes and item
            beneath it. `//!` is not considered: a module comment has no item
            under it to be separated from
  break     inside a run, a line ending a sentence followed with no blank
            `///` between by a line reading like a fresh rustdoc summary
            (`Loads ...`, `The ...`). This repo writes a summary, a blank
            `///`, then detail — so a summary mid-run means two documents
  evidence  a break alone is far too noisy: 14 across this workspace, 13 of
            them an ordinary second sentence. What distinguishes a splice is
            that the stranded half belongs to an item that is now bare, and
            that item is always in the same file, since splicing is what an
            insertion between a doc and its item does. So a break is reported
            only when the text above it names — in backticks — the name or a
            parameter of some *undocumented* item in that same file

That evidence rule is what takes fourteen breaks down to four candidates. It
is also the blind spot: a splice whose stranded half names nothing about its
own item is invisible here, and so is one whose victim is documented
elsewhere in the file. This narrows the rule's surface, it does not replace
reading the diff — a hand sweep of the fourteen breaks cleared two of the
splices this finds, which is the whole argument for having it.
"""

import re
import subprocess
import sys
from pathlib import Path

DOC_RE = re.compile(r"^\s*///")
ATTR_RE = re.compile(r"^\s*#\[")
ITEM_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+|unsafe\s+|const\s+)*"
    r"(fn|struct|enum|trait|type|mod|impl)\s+([A-Za-z_][A-Za-z_0-9]*)"
)
# A field is an item too, and is spliced the same way.
FIELD_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z_0-9]*)\s*:")
SUMMARY_RE = re.compile(
    r"^\s*/// (?:The|A|An) \S|^\s*/// [A-Z][a-z]+(?:s|es) [`\w]"
)
BACKTICKED = re.compile(r"`([A-Za-z_][A-Za-z_0-9]*)`")
PARAM_RE = re.compile(r"^\s*(?:mut\s+)?([a-z_][a-z_0-9]*)\s*:")


def git(root, *args):
    return subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True
    ).stdout


def runs(lines):
    """Every `(doc_start, doc_end, item_line)` in a file, doc lines only."""
    i = 0
    while i < len(lines):
        if not DOC_RE.match(lines[i]):
            i += 1
            continue
        start = i
        while i < len(lines) and DOC_RE.match(lines[i]):
            i += 1
        end = i
        while i < len(lines) and ATTR_RE.match(lines[i]):
            i += 1
        yield start, end, (i if i < len(lines) else None)


def bare_items(lines):
    """Items with no doc comment of their own, as `name -> {identifiers}`.

    The identifiers are the item's own name plus, for a function, its
    parameter names — which is what a stranded doc comment tends to talk
    about, since it was written to describe them.
    """
    documented_at = {end for _, end, _ in runs(lines)}
    # By *name*, not by position: a trait method is documented once on the
    # trait and appears bare on every impl, which is not a stranding. Only a
    # name documented nowhere in the file can have lost its doc comment.
    documented_names = set()
    for i, line in enumerate(lines):
        matched = ITEM_RE.match(line)
        if not matched:
            continue
        attrs = i
        while attrs > 0 and ATTR_RE.match(lines[attrs - 1]):
            attrs -= 1
        if attrs in documented_at or (attrs > 0 and DOC_RE.match(lines[attrs - 1])):
            documented_names.add(matched.group(2))

    bare = {}
    for i, line in enumerate(lines):
        matched = ITEM_RE.match(line)
        if not matched:
            continue
        name = matched.group(2)
        if name in documented_names:
            continue
        attrs = i
        while attrs > 0 and ATTR_RE.match(lines[attrs - 1]):
            attrs -= 1
        if attrs in documented_at or (attrs > 0 and DOC_RE.match(lines[attrs - 1])):
            continue
        idents = {name}
        # A multi-line signature's parameters, up to the closing paren.
        for follow in lines[i : i + 12]:
            param = PARAM_RE.match(follow)
            if param:
                idents.add(param.group(1))
            if ")" in follow and follow.strip() != "(":
                break
        bare[name] = idents
    return bare


def main(root: Path) -> int:
    findings = []
    for rel in git(root, "ls-files", "*.rs").split():
        lines = (root / rel).read_text(encoding="utf-8", errors="replace").splitlines()
        if not lines:
            continue
        bare = bare_items(lines)
        if not bare:
            continue
        for start, end, item in runs(lines):
            for i in range(start + 1, end):
                previous = lines[i - 1].rstrip()
                if not previous.endswith(".") or not SUMMARY_RE.match(lines[i]):
                    continue
                stranded = " ".join(lines[start:i])
                named = set(BACKTICKED.findall(stranded))
                owners = sorted(n for n, idents in bare.items() if named & idents)
                if not owners:
                    continue
                attached = "?"
                if item is not None:
                    as_item = ITEM_RE.match(lines[item])
                    as_field = FIELD_RE.match(lines[item])
                    if as_item:
                        attached = as_item.group(2)
                    elif as_field:
                        attached = as_field.group(1)
                findings.append((rel, i + 1, attached, owners, lines[i].strip()))

    for rel, line, attached, owners, text in findings:
        print(f"{rel}:{line}")
        print(f"  documents:   {attached}")
        print(f"  text above names: {', '.join(owners)} — undocumented in this file")
        print(f"  break at:    {text[:70]}")
        print()
    if findings:
        print(
            f"{len(findings)} doc comment(s) appear to document an item other than "
            "the one they sit on."
        )
    else:
        print("No doc comment appears to document an item other than its own.")
    return 0


if __name__ == "__main__":
    sys.exit(main(Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()))
