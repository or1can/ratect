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

"""Claude Code `PreToolUse` hook on `Bash`: refuse a check whose output is
piped straight into a filter (`tail`, `head`, `grep`, `sed` and the like).

Twice in ratect 0.13.0 (#813) a filter chosen to save context hid what the
output was for: a test run cut to its totals lost a failing test's name,
and a commit hook cut to its last screen hid every finding but the last.
Writing the full output to a file first keeps it, and the file can be
filtered afterwards. `tee` before the filter keeps it too, so that passes.

This is a nudge, not a gate: it reads the command as typed, so a wrapped or
scripted command gets past it, and that's accepted. A gate built this way
fails, because no reading of a command string covers every spelling:
or1can/claims dropped its own `PreToolUse` commit gate for that reason (its
decisions/0007-git-decides-when-a-commit-happens.md), and a check here for
`git commit -n` was dropped on review for the same one. Here a miss only
falls back to the agent's own judgement.
"""

import json
import re
import sys

# A check in command position: the start of a pipeline, after any
# `VAR=value` assignments. Matching anywhere would refuse a search for the
# words, such as a grep for them piped into `head`.
CHECK = re.compile(
    r"^\s*(?:\w+=\S*\s+)*"
    r"(?:git\s+(?:-C\s+\S+\s+)*commit|cargo\s+(?:test|clippy|llvm-cov)"
    r"|pre-commit\s+run|python3?\s+-m\s+claims\.cli)(?![\w-])"
)
FILTER = re.compile(r"^\s*(?:tail|head|grep|rg|sed|awk|cut|wc)\b")
TEE = re.compile(r"^\s*tee\b")
QUOTED = re.compile(r"'[^']*'|\"(?:[^\"\\]|\\.)*\"")
HEREDOC = re.compile(r"<<-?\s*(['\"]?)(\w+)\1[^\n]*\n.*?^\s*\2\s*$", re.S | re.M)


def without_data(command: str) -> str:
    """`command` with each heredoc's body and quoted text dropped: a `|` or a
    check's name inside them is data, not shell. A heredoc goes first, since
    its body can hold an unbalanced quote."""
    command = HEREDOC.sub(lambda m: m.group(0).split("\n", 1)[0], command)
    return QUOTED.sub("''", command)


def filters_a_check(command: str) -> bool:
    """Whether some pipeline in `command` runs a check and pipes its output
    into a filter with no `tee` keeping it first."""
    for pipeline in re.split(r"&&|\|\||;|\n", without_data(command)):
        stages = re.split(r"\|&?", pipeline)
        if not CHECK.search(stages[0]):
            continue
        for stage in stages[1:]:
            if TEE.match(stage):
                break
            if FILTER.match(stage):
                return True
    return False


def main() -> int:
    command = json.load(sys.stdin).get("tool_input", {}).get("command", "")
    if not filters_a_check(command):
        return 0
    print(
        "This pipes a check's output into a filter, which can hide a failure "
        "or a finding. Write the full output to a file first (`… > /tmp/run.log 2>&1`, "
        "or `| tee /tmp/run.log | tail`), then read or filter the file.",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main())
