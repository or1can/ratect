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

"""Tests for `claims.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

What is tested is the rev it reads from `.pre-commit-config.yaml`, the one
place CI's Claims job reads it from too, and the command it builds from
that rev. Running that command needs `uv` and the network, so it isn't run
here.
"""

import importlib.util
import sys
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent


def load(path):
    """Imports `claims.py` by path, so it isn't confused with the `claims` package."""
    spec = importlib.util.spec_from_file_location("claims_wrapper_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


wrapper = load(TOOLS / "claims.py")

CONFIG = """\
repos:
  - repo: https://github.com/example/other
    rev: v9.9.9
    hooks:
      - id: other
  - repo: https://github.com/or1can/claims
    rev: {rev}
    hooks:
      - id: claims
"""


class RevTest(unittest.TestCase):
    def test_reads_the_claims_repos_rev_not_another_repos(self):
        self.assertEqual(wrapper.rev(CONFIG.format(rev="v0.28.1")), "v0.28.1")

    def test_strips_quotes_and_a_trailing_comment(self):
        for written in ('"v0.28.1"', "'v0.28.1'", "v0.28.1  # frozen: v0.28.1"):
            with self.subTest(written=written):
                self.assertEqual(wrapper.rev(CONFIG.format(rev=written)), "v0.28.1")

    def test_accepts_a_quoted_repo_url_and_a_git_suffix(self):
        for repo in ('"https://github.com/or1can/claims"', "https://github.com/or1can/claims.git"):
            with self.subTest(repo=repo):
                config = CONFIG.format(rev="v0.28.1").replace("https://github.com/or1can/claims", repo)
                self.assertEqual(wrapper.rev(config), "v0.28.1")

    def test_a_config_without_the_claims_repo_is_an_error(self):
        with self.assertRaises(ValueError):
            wrapper.rev("repos:\n  - repo: https://github.com/example/other\n    rev: v1\n")

    def test_a_claims_repo_without_a_rev_is_an_error(self):
        config = "repos:\n  - repo: https://github.com/or1can/claims\n    hooks: []\n  - repo: x\n    rev: v1\n"
        with self.assertRaises(ValueError):
            wrapper.rev(config)

    def test_reads_this_repositorys_own_config(self):
        text = (ROOT / ".pre-commit-config.yaml").read_text()
        self.assertRegex(wrapper.rev(text), r"^v\d+\.\d+\.\d+$")


class CommandTest(unittest.TestCase):
    def test_runs_the_cli_from_that_rev_with_the_arguments_given(self):
        self.assertEqual(
            wrapper.command("v0.28.1", ["mark-checked", "--by", "x"]),
            [
                "uv", "run", "--no-project", "--python", sys.executable,
                "--with", "git+https://github.com/or1can/claims@v0.28.1",
                "python", "-m", "claims.cli", "mark-checked", "--by", "x",
            ],
        )


if __name__ == "__main__":
    unittest.main()
