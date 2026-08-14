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

"""Ranks prose claims by how much the code they describe has moved since the
claim was last touched.

    python3 tools/stale-claims.py [repo-root] [top-N]

**A candidate list, not a verdict.** It measures churn in a claim's subject,
which correlates only weakly with the claim being wrong: a hot file makes an
accurate claim look suspicious, and a claim can rot while its subject sits
still. Whether a flagged claim is actually false still needs the claim
*executed* — AGENTS.md guideline 15. The value is turning "re-read all the
prose before a release" into "re-read these six".

The motivating case is ADR 0006, whose empirical detector ("every module with
a `//!` doc comment is under 800 lines") was true when written and was
destroyed by the ADR's own Rule 1 mandating module docs everywhere. Four
modules now violate it and the ADR still reads as current. Nothing in the
repo looked for that.

How it works, and why:

  claim    a Markdown *section* (heading to next heading), not a file —
           `config-reference.md` changes constantly and would otherwise
           drown every claim inside it
  touched  newest committer-time across that section, via `git blame`
  subject  the code each section names. An explicit path
           (`ratect-core/src/config.rs`) counts anywhere. A backticked bare
           module name (`config`) counts only in `decisions/` and
           `AGENTS.md`, which is where this repo discusses modules by name;
           in `docs/` the same words mean the Docker CLI, a config field or
           a JSON schema, and matching them there put six false positives
           above the one real finding
  score    the largest fraction of any subject's history that happened
           *after* the claim — normalised, so a claim predating 90% of a
           file's life outranks one predating 5% of a busier file's

Append-only history is excluded: CHANGELOG entries and ROADMAP's versioned
lists describe a release as it shipped, so their subjects moving afterwards
is expected rather than suspicious. Left in, they were 7 of the first 10.
"""
import bisect
import re
import subprocess
import sys
from pathlib import Path

PATH_RE = re.compile(r"\b((?:[a-z][a-z0-9-]*/)+src/[a-z_0-9]+\.rs)\b")
MODULE_RE = re.compile(r"`([a-z_][a-z_0-9]*)(?:\.rs)?`")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.*)")

# Headings whose sections are append-only history (AGENTS.md guideline 9).
APPEND_ONLY_HEADINGS = ("### `ratect-compat`", "### `ratect`")


def git(root, *args):
    return subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True
    ).stdout


def main(root: Path, top: int) -> int:
    modules = {p.stem: str(p.relative_to(root)) for p in root.glob("*/src/*.rs")}
    # A module's own tests live beside it and are not what a claim describes.
    modules = {k: v for k, v in modules.items() if not k.endswith("_tests")}

    history: dict[str, list[int]] = {}

    def commits(path):
        if path not in history:
            history[path] = sorted(
                int(t) for t in git(root, "log", "--format=%ct", "--", path).split()
            )
        return history[path]

    # Tracked files only, via git rather than a directory walk: an untracked
    # tree has no history to blame, so it could never be ranked anyway, and
    # asking git means no skip-list of generated directories to keep current.
    docs = sorted(
        root / rel
        for rel in git(root, "ls-files", "*.md").split()
        if Path(rel).name != "CHANGELOG.md"
    )

    findings = []
    for doc in docs:
        rel = str(doc.relative_to(root))
        discusses_modules = rel.startswith("decisions/") or rel == "AGENTS.md"
        lines = doc.read_text(encoding="utf-8", errors="replace").splitlines()
        starts = [i for i, l in enumerate(lines) if HEADING_RE.match(l)] or [0]
        append_only_from = None
        for start, end in zip(starts, starts[1:] + [len(lines)]):
            matched = HEADING_RE.match(lines[start])
            heading = lines[start] if matched else ""
            level = len(matched.group(1)) if matched else 99
            # Everything nested under an append-only heading is history too.
            if append_only_from is not None and level > append_only_from:
                continue
            append_only_from = None
            if heading.strip() in APPEND_ONLY_HEADINGS:
                append_only_from = level
                continue

            body = "\n".join(lines[start:end])
            subjects = {m for m in PATH_RE.findall(body) if (root / m).exists()}
            if discusses_modules:
                subjects |= {modules[m] for m in MODULE_RE.findall(body) if m in modules}
            if not subjects:
                continue

            blame = git(
                root, "blame", "-L", f"{start + 1},{end}", "--line-porcelain", "--", rel
            )
            stamps = [
                int(l.split()[1])
                for l in blame.splitlines()
                if l.startswith("committer-time ")
            ]
            if not stamps:
                continue
            touched = max(stamps)

            moved = {}
            for path in sorted(subjects):
                times = commits(path)
                if not times:
                    continue
                since = len(times) - bisect.bisect_right(times, touched)
                if since:
                    moved[path] = (since, since / len(times))
            if moved:
                score = max(fraction for _, fraction in moved.values())
                label = matched.group(2) if matched else rel
                findings.append((score, label, rel, start + 1, moved))

    findings.sort(reverse=True)
    print(
        f"{len(findings)} claims name code that changed after them. "
        "Churn, not wrongness — verify by executing the claim.\n"
    )
    for score, label, rel, line, moved in findings[:top]:
        detail = ", ".join(
            f"{p.split('/')[-1]} {n} commits ({f:.0%} of its history)"
            for p, (n, f) in sorted(moved.items(), key=lambda kv: -kv[1][1])[:3]
        )
        print(f"{score:5.0%}  {label[:56]:56}  {rel}:{line}")
        print(f"       {detail}")
    return 0


if __name__ == "__main__":
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    sys.exit(main(root, int(sys.argv[2]) if len(sys.argv) > 2 else 15))
