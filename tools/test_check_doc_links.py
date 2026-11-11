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

"""Tests for `check-doc-links.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

These cover the comparison over fixture source and rendered-HTML trees, and
the guards that keep a broken search from passing, and the commands `main`
runs, with cargo replaced by a recorder. None of them runs cargo: whether
those commands build the right pages is checked by running the script
itself, as CI's Rustdoc job does.
"""

import importlib.util
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports a `check-doc-links.py` by path (hyphens, so not by name)."""
    spec = importlib.util.spec_from_file_location("check_doc_links_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check = load(TOOLS / "check-doc-links.py")


class Tree:
    """A temporary workspace: crate sources, and a `doc/` per rendered crate."""

    def __init__(self, root):
        self.root = Path(root)
        self.doc = self.root / "doc"

    def source(self, crate_dir, text):
        path = self.root / crate_dir / "src" / "lib.rs"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path.parent

    def rendered(self, crate, html):
        path = self.doc / crate / "index.html"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(html, encoding="utf-8")


class CheckTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.tree = Tree(temp.name)

    def run_check(self, sources, crates):
        written, error = check.written_links(sources)
        self.assertIsNone(error)
        return check.check(written, self.tree.doc, crates)

    def test_a_written_link_rendered_as_brackets_is_reported(self):
        src = self.tree.source("core", "/// See [`Hidden`] and [`Shown`].\npub fn f() {}\n")
        self.tree.rendered("core", "<p>See [<code>Hidden</code>] and <a><code>Shown</code></a>.</p>")
        self.assertEqual(self.run_check([src], ["core"]), (["Hidden"], None))

    def test_a_path_with_colons_is_reported_too(self):
        src = self.tree.source("core", "/// [`Boundary::check`]\npub fn f() {}\n")
        self.tree.rendered("core", "[<code>Boundary::check</code>]")
        self.assertEqual(self.run_check([src], ["core"]), (["Boundary::check"], None))

    def test_a_dependencys_dropped_link_is_not_ours(self):
        # Inlined into our pages through a trait impl, but not written here.
        src = self.tree.source("core", "/// [`Ours`]\npub fn f() {}\n")
        self.tree.rendered("core", "<a><code>Ours</code></a> [<code>Theirs</code>]")
        self.assertEqual(self.run_check([src], ["core"]), ([], None))

    def test_every_rendered_crate_is_searched(self):
        core = self.tree.source("core", "/// [`A`]\npub fn f() {}\n")
        cli = self.tree.source("cli", "/// [`B`]\nfn main() {}\n")
        self.tree.rendered("core", "")
        self.tree.rendered("cli", "[<code>A</code>] [<code>B</code>]")
        self.assertEqual(self.run_check([core, cli], ["core", "cli"]), (["A", "B"], None))

    def test_no_written_links_means_the_search_is_broken(self):
        empty = self.tree.root / "nothing"
        empty.mkdir()
        written, error = check.written_links([empty])
        self.assertEqual(written, set())
        self.assertIn("search is broken", error)

    def test_a_missing_source_directory_means_the_search_is_broken(self):
        _, error = check.written_links([self.tree.root / "renamed" / "src"])
        self.assertIn("renamed", error)

    def test_a_crate_with_no_rendered_docs_means_the_search_is_broken(self):
        # The doc build ran first, so a missing crate is a renamed crate or a
        # wrong target directory, either of which would otherwise pass.
        src = self.tree.source("core", "/// [`Hidden`]\npub fn f() {}\n")
        self.tree.rendered("core", "")
        _, error = self.run_check([src], ["core", "gone"])
        self.assertIn("gone", error)


METADATA = ["cargo", "metadata", "--format-version", "1", "--no-deps"]


class MainTest(unittest.TestCase):
    """What `main` runs and prints, with cargo replaced by a recorder.

    `run_main` returns `(commands, stderr)`: each command run, with the
    `RUSTDOCFLAGS` it was given, and what `main` printed.
    """

    def run_main(
        self, environ=None, fails=None, raises=None, written=({"A"}, None), result=([], None), status=0
    ):
        commands = []
        searched = []
        checked = []

        def run(command, **kwargs):
            self.assertEqual(kwargs.get("cwd"), check.ROOT, command)
            commands.append((command, kwargs.get("env", {}).get("RUSTDOCFLAGS")))
            if raises:
                raise raises
            if command == fails:
                raise subprocess.CalledProcessError(101, command)
            return subprocess.CompletedProcess(command, 0, stdout='{"target_directory": "/nowhere"}')

        def written_links(source_dirs):
            searched.append(source_dirs)
            return written

        def fake_check(*args):
            checked.append(args)
            return result

        stderr = StringIO()
        with (
            mock.patch.object(check.subprocess, "run", run),
            mock.patch.dict(check.os.environ, environ or {}, clear=True),
            mock.patch.object(check, "written_links", written_links),
            mock.patch.object(check, "check", fake_check),
            redirect_stderr(stderr),
        ):
            self.assertEqual(check.main([]), status)
        self.assertEqual(searched, [[check.ROOT / d for d in check.CRATES]])
        if checked:
            self.assertEqual(
                checked, [(written[0], Path("/nowhere/doc"), list(check.CRATES.values()))]
            )
        return commands, stderr.getvalue()

    def test_every_searched_source_directory_exists(self):
        for directory in check.CRATES:
            self.assertTrue((check.ROOT / directory).is_dir(), directory)

    def test_a_failing_cargo_command_passes_its_status_through(self):
        commands, stderr = self.run_main(fails=check.DOC_PASSES[0], status=101)
        self.assertEqual(len(commands), 2, "stops at the failing pass")
        self.assertIn(
            "error: `cargo doc --workspace --lib --all-features --no-deps` failed", stderr
        )

    def test_a_broken_source_search_fails_before_any_cargo_command(self):
        commands, stderr = self.run_main(written=(set(), "no doc links found at all"), status=1)
        self.assertEqual(commands, [])
        self.assertIn("error: no doc links found at all", stderr)

    def test_no_cargo_on_path_fails_with_a_message_not_a_traceback(self):
        commands, stderr = self.run_main(raises=FileNotFoundError("cargo"), status=1)
        self.assertEqual(len(commands), 1, "stops at the first command")
        self.assertIn("error: could not run cargo", stderr)

    def test_a_dropped_link_is_reported(self):
        _, stderr = self.run_main(result=(["Hidden"], None), status=1)
        self.assertIn(check.report(["Hidden"]), stderr)

    def test_an_untrustworthy_rendered_search_is_reported(self):
        _, stderr = self.run_main(result=([], "no rendered docs for x"), status=1)
        self.assertIn("error: no rendered docs for x", stderr)

    def test_it_cleans_documents_the_libraries_then_the_workspace_then_finds_the_docs(self):
        # Both passes select the whole workspace, so dependency features
        # resolve the same way and the second pass doesn't document the
        # libraries again alongside the binaries (ratect#838).
        commands, stderr = self.run_main()
        self.assertEqual(
            [command for command, _ in commands],
            [
                ["cargo", "clean", "--doc"],
                ["cargo", "doc", "--workspace", "--lib", "--all-features", "--no-deps"],
                ["cargo", "doc", "--workspace", "--all-features", "--no-deps"],
                METADATA,
            ],
        )
        self.assertEqual(stderr, "")

    def test_rustdocflags_default_to_ci_and_a_callers_are_kept(self):
        commands, _ = self.run_main()
        self.assertEqual({flags for command, flags in commands if command != METADATA}, {"-D warnings"})
        commands, _ = self.run_main({"RUSTDOCFLAGS": "--cfg docsrs"})
        self.assertEqual({flags for command, flags in commands if command != METADATA}, {"--cfg docsrs"})


class MessageTest(unittest.TestCase):
    def test_the_report_names_each_link_and_the_fix(self):
        report = check.report(["Hidden", "Other"])
        self.assertIn("Hidden\nOther\n", report)
        self.assertIn("Link a public path, or write it as a plain code span.", report)


if __name__ == "__main__":
    unittest.main()
