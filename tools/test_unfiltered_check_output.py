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

"""Tests for `.claude/hooks/unfiltered-check-output.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

The hook lives beside Claude Code's settings rather than under `tools/`, but
its tests run here so the Tools Tests CI job covers it. Each refused command
is a check piped into a filter. Each allowed one keeps the full output,
only mentions a check (in a search, a quoted message or a heredoc), or runs
a command whose name a check's starts (`git commit-graph`).
"""

import importlib.util
import unittest
from pathlib import Path

HOOK = Path(__file__).resolve().parent.parent / ".claude" / "hooks" / "unfiltered-check-output.py"
_spec = importlib.util.spec_from_file_location("unfiltered_check_output", HOOK)
assert _spec is not None and _spec.loader is not None
hook = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(hook)


class FiltersACheck(unittest.TestCase):
    def test_a_check_piped_into_a_filter_is_refused(self):
        for command in [
            "cargo test --workspace 2>&1 | tail -10",
            "git commit -s -q -F msg | tail -60",
            'cd x && cargo test 2>&1 | grep -E "test result"',
            "cargo test |& tail",
            "cargo test | sed -n 1,5p",
            "RUST_LOG=off cargo test | wc -l",
            "python3 -m claims.cli --diff-range x | tail -3",
            "pre-commit run --all-files | tail",
            "git -C ../wt commit -s -F msg | tail",
            "git commit -m \"$(cat <<'EOF'\nfix: x\nEOF\n)\" | tail",
            "cat > /tmp/m <<'X'\nit's\nX\ncargo test | tail",
        ]:
            with self.subTest(command=command):
                self.assertTrue(hook.filters_a_check(command))

    def test_output_kept_in_full_is_allowed(self):
        for command in [
            "cargo test 2>&1 | tee /tmp/t.log | tail -5",
            "cargo test > /tmp/t.log 2>&1; tail -5 /tmp/t.log",
        ]:
            with self.subTest(command=command):
                self.assertFalse(hook.filters_a_check(command))

    def test_a_command_that_only_mentions_a_check_is_allowed(self):
        for command in [
            'grep -rn "cargo test" AGENTS.md | head',
            'git log --grep="git commit" | head -5',
            'git commit -m "a | head of list"',
            'python3 - <<EOF\nx = ["grep \\"cargo test\\" x | head"]\nEOF',
            "git log --oneline | head",
            "cargo build && git status | head",
            "git commit-graph write | head",
        ]:
            with self.subTest(command=command):
                self.assertFalse(hook.filters_a_check(command))


if __name__ == "__main__":
    unittest.main()
