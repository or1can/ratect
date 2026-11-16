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

"""Tests for `.claude/hooks/settle-design-reminder.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

The hook lives beside Claude Code's settings rather than under `tools/`, but
its tests run here so the Tools Tests CI job covers it. A prompt that starts
by invoking the plugin's triage or implement skills, by either of their
names, gets the reminder for that moment; any other prompt, including one
that only mentions a skill, gets nothing.
"""

import importlib.util
import io
import json
import unittest
from contextlib import redirect_stdout
from pathlib import Path

HOOK = Path(__file__).resolve().parent.parent / ".claude" / "hooks" / "settle-design-reminder.py"
_spec = importlib.util.spec_from_file_location("settle_design_reminder", HOOK)
assert _spec is not None and _spec.loader is not None
hook = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(hook)


class Reminder(unittest.TestCase):
    def test_a_triage_prompt_gets_the_triage_reminder(self):
        for prompt in [
            "/mattpocock-skills:triage #902",
            "/triage",
            "  /mattpocock-skills:triage\n#902",
        ]:
            with self.subTest(prompt=prompt):
                self.assertEqual(hook.reminder(prompt), hook.AT_TRIAGE)

    def test_an_implement_prompt_gets_the_implementation_reminder(self):
        for prompt in [
            "/mattpocock-skills:implement #902",
            "/implement 902",
            "/mattpocock-skills:implement-spec .scratch/x/spec.md",
            "/implement-spec",
        ]:
            with self.subTest(prompt=prompt):
                self.assertEqual(hook.reminder(prompt), hook.AT_IMPLEMENTATION)

    def test_any_other_prompt_gets_nothing(self):
        for prompt in [
            "",
            "what's the next step for #902, is it triage?",
            "run /mattpocock-skills:implement on #902",
            "/mattpocock-skills:triaged",
            "/implementation",
            "/ready",
            "/mattpocock-skills:to-tickets",
        ]:
            with self.subTest(prompt=prompt):
                self.assertIsNone(hook.reminder(prompt))

    def test_main_prints_the_reminder_and_always_exits_zero(self):
        for payload, expected in [
            ({"prompt": "/triage #1"}, hook.AT_TRIAGE + "\n"),
            ({"prompt": "hello"}, ""),
            ({}, ""),
            ({"prompt": None}, ""),
            ({"prompt": ["/triage"]}, ""),
        ]:
            with self.subTest(payload=payload):
                out = io.StringIO()
                with redirect_stdout(out):
                    code = hook.main(io.StringIO(json.dumps(payload)))
                self.assertEqual(code, 0)
                self.assertEqual(out.getvalue(), expected)

    def test_each_reminder_names_the_skill(self):
        for text in [hook.AT_TRIAGE, hook.AT_IMPLEMENTATION]:
            with self.subTest(text=text):
                self.assertIn("settle-design", text)


if __name__ == "__main__":
    unittest.main()
