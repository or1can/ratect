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

"""Claude Code `UserPromptSubmit` hook: remind the agent to run the
`settle-design` skill when a prompt starts triage, and to read the design
record when one starts implementation.

The `mattpocock-skills` plugin's `triage`, `implement` and `implement-spec`
skills can't be edited from this repo, and each sets
`disable-model-invocation`, so a typed prompt is the only way any of them
starts. That makes the prompt a deterministic signal for the two moments
design is checked at: the end of triage, where `settle-design` runs
(ratect#902), and before any code is written. `settle-design` runs at
triage only (ratect#927), so until ratect#930 retargets the implementation
reminder to `check-design`, it says to read the record instead.
AGENTS.md's "Settling design" section says why.

This is a reminder, not a gate: it adds a line to the agent's context and
never blocks the prompt. A skill started some other way (a paraphrase, a
prompt that names the skill mid-sentence) gets no reminder, and AGENTS.md
is the fallback there.
"""

import json
import re
import sys
from typing import Optional, TextIO

AT_TRIAGE = (
    "Before this triage moves an issue to ready-for-agent or ready-for-human, "
    "run the settle-design skill on it (AGENTS.md, \"Settling design\")."
)
AT_IMPLEMENTATION = (
    "Before writing any code for this ticket, read its design record with "
    "`uv run tools/design-record.py get <number>` and check that the brief's "
    "acceptance criteria agree with it (agents/issue-tracker.md). settle-design "
    "runs at triage only, so a ticket whose record is empty (`source` `none`) "
    "goes back to needs-triage, and on exit 1 you stop (AGENTS.md, "
    "\"Settling design\")."
)

# The skill's name, bare or plugin-qualified, at the very start of the
# prompt and ending at whitespace or the prompt's end, so `/triaged` and a
# prompt that only mentions `/implement` both pass untouched.
INVOCATION = re.compile(r"^\s*/(?:mattpocock-skills:)?(triage|implement|implement-spec)(?:\s|$)")


def reminder(prompt: str) -> Optional[str]:
    """The reminder for a prompt that starts one of the watched skills, or
    `None` for any other prompt."""
    match = INVOCATION.match(prompt)
    if match is None:
        return None
    return AT_TRIAGE if match.group(1) == "triage" else AT_IMPLEMENTATION


def main(stdin: TextIO = sys.stdin) -> int:
    prompt = json.load(stdin).get("prompt")
    text = reminder(prompt) if isinstance(prompt, str) else None
    if text is not None:
        # A `UserPromptSubmit` hook's stdout, on exit 0, is added to the
        # agent's context.
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
