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

"""Fails if a doc link this workspace writes renders as literal brackets.

    uv run tools/check-doc-links.py

Takes no arguments but `--help`; given the script's path, it runs from any
directory in the checkout. Rebuilds the workspace's documentation from
cold, then exits 0 when no written link rendered as brackets, 1 listing
those that did (or when the search can't be trusted: no links in the
sources, or a crate with no rendered docs), with cargo's own status when a
cargo command fails, 1 when cargo can't be run at all, and 2 on an
argument it doesn't take.
`RUSTDOCFLAGS` defaults to `-D warnings`, CI's setting; a value already in
the environment is kept.

# Why this exists

`cargo doc -D warnings` catches a link to a name that does not exist. It
does *not* catch a link to a name that exists but is not public from where
it's written: rustdoc renders `` [`x`] `` as literal brackets and exits 0, so
the reader sees punctuation where a link was meant. No lint covers it —
`private_intra_doc_links` does not fire here — so this looks for the symptom
in the rendered HTML instead of asking the compiler. That once shipped ten
such links in one sweep, green locally and red in CI.

It used to be an inline step of CI's Rustdoc job, which `AGENTS.md` told
contributors to run by hand. There was no script to run, so a hand run had
to rebuild the docs its own way, and one listed 16 links that weren't there
(ratect#838). The build is part of the script for that reason: the
comparison is only as good as the docs it reads, and the build is the part
that is easy to get wrong (see "How the docs are built").

# How the docs are built

`cargo clean --doc` first, so every run is a cold build like CI's, whatever
an earlier `cargo doc` left in the target directory. That removes the whole
`doc` directory, dependencies' pages included, not only this workspace's.

Then two passes: the library crates, then the workspace. A cross-crate link
(`ratect_core::x` from a binary) only resolves if the target crate's
documentation already exists, and one `--workspace` pass documents in
whatever order it likes, so seven such links flipped between renders. Two
passes make the result the same every time, which the comparison needs far
more than it needs to be one command.

The first pass selects the libraries with `--workspace --lib`, not `-p`. A
`-p` selection resolves dependency features over those packages alone, so
`regex-automata`, under `regex`, came out without the `dfa-build` feature
the second pass's selection gives it. The second pass then saw both
libraries as dirty and documented them again, alongside the binaries that
link into them. CI's own two passes were written that way, and a second
run of them back to back listed the same 16 links as the one in ratect#838
— likely, though not confirmed, the cause there too. Selecting the whole
workspace in both resolves the same features, so the second pass documents
only the binaries.

# What is compared

Link names written as `` [`X`] `` in the workspace's crate sources,
intersected with names rendered as `[<code>X</code>]` in those crates' own
generated pages. Restricted to names this workspace writes, so a
dependency's own dropped links (inlined into our pages through trait impls)
don't fail our build.

Paths with `::` are included, not skipped. An earlier version excluded them,
claiming the class was "always a bare name" — false:
`GitBoundary::check_path_allowed` named a private struct in the same crate.
The reason to exclude them was really the render nondeterminism, which the
two passes fix.

# What it cannot see

A link in the doc comment of a *private* item, measured by reintroducing
one. Those items get no page, so the link never renders and there is no
symptom to look for — which also means no reader of the published
documentation ever meets it. Covering them would take
`--document-private-items`, i.e. checking pages nobody publishes. Left
uncovered on purpose: the maintainers decided so when the CI step this
replaces measured the gap (dd0825ea), and ratect#838's brief kept it out of
scope.

# Why an empty search fails

A search that finds nothing finds no overlap and passes: a gate that
silently stops gating. This workspace always has doc links, so finding none
in its sources means the search broke (a renamed source directory, say), not
that the code got tidy; that is checked before the build, since it needs
none. A crate whose rendered docs are missing after the build fails the
same way, for the same reason.
"""

import sys

# Before any other import: the PEP 723 header above is what `uv run` reads,
# and this is what a plain `python3` older than it meets instead.
if sys.version_info < (3, 11):
    sys.exit(
        "tools/check-doc-links.py needs Python 3.11 or later, and this is %d.%d. "
        "Run it as `uv run tools/check-doc-links.py`, which fetches one." % sys.version_info[:2]
    )

import argparse
import json
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Each crate's source directory, and the directory its docs render into.
CRATES = {
    "ratect-core/src": "ratect_core",
    "ratect/src": "ratect",
    "ratect-compat/src": "ratect_compat",
    "dockerignore/src": "dockerignore",
}

# Libraries first: see "How the docs are built".
DOC_PASSES = [
    ["cargo", "doc", "--workspace", "--lib", "--all-features", "--no-deps"],
    ["cargo", "doc", "--workspace", "--all-features", "--no-deps"],
]

WRITTEN = re.compile(r"\[`([A-Za-z_][A-Za-z0-9_:]*)`\]")
RENDERED = re.compile(r"\[<code>([^<]*)</code>\]")


def names_in(directory, pattern):
    """Every name `pattern`'s group matches in any file under `directory`."""
    names = set()
    for path in directory.rglob("*"):
        if path.is_file():
            names.update(pattern.findall(path.read_text(encoding="utf-8", errors="replace")))
    return names


def written_links(source_dirs):
    """`(names, error)`: every link name written in `source_dirs`, or the
    reason the search can't be trusted (and `names` is then empty)."""
    for directory in source_dirs:
        if not Path(directory).is_dir():
            return set(), f"no source directory {directory}: this search is broken, not clean."
    written = set()
    for directory in source_dirs:
        written |= names_in(Path(directory), WRITTEN)
    if not written:
        return set(), "no doc links found at all: this search is broken, not clean."
    return written, None


def check(written, doc_root, crates):
    """`(dropped, error)`: the `written` links rendered as brackets in
    `crates`' pages, sorted, or the reason the search can't be trusted."""
    for crate in crates:
        if not (Path(doc_root) / crate).is_dir():
            return [], f"no rendered docs for {crate} under {doc_root}: this search is broken, not clean."
    rendered = set()
    for crate in crates:
        rendered |= names_in(Path(doc_root) / crate, RENDERED)
    return sorted(written & rendered), None


def report(dropped):
    """What a failing run prints."""
    return (
        "These doc links resolve to nothing public and render as literal brackets:\n"
        + "".join(f"{name}\n" for name in dropped)
        + "Link a public path, or write it as a plain code span."
    )


def target_directory():
    """Where cargo writes, which `CARGO_TARGET_DIR` or a config can move."""
    metadata = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return Path(json.loads(metadata.stdout)["target_directory"])


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.parse_args(argv)

    # Before the build, which is minutes of work a broken search would waste.
    written, error = written_links([ROOT / d for d in CRATES])
    if error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    env = dict(os.environ)
    env.setdefault("RUSTDOCFLAGS", "-D warnings")
    try:
        for command in [["cargo", "clean", "--doc"], *DOC_PASSES]:
            subprocess.run(command, cwd=ROOT, env=env, check=True)
        doc_root = target_directory() / "doc"
    except subprocess.CalledProcessError as error:
        print(f"error: `{' '.join(error.cmd)}` failed", file=sys.stderr)
        return error.returncode
    except OSError as error:
        print(f"error: could not run cargo: {error}", file=sys.stderr)
        return 1

    dropped, error = check(written, doc_root, list(CRATES.values()))
    if error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if dropped:
        print(report(dropped), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
