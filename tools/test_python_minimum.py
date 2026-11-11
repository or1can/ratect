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

"""Tests that every `tools/` script declares, and enforces, one minimum Python.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

The minimum is the oldest Python still supported upstream (ratect#858). Each
script states it twice: in a PEP 723 header, which is what lets `uv run`
fetch a matching interpreter, and in a check at the top of the script, which
is what a plain `python3` older than that (from 3.9, which CI checks) meets:
a message naming the minimum and `uv run`, rather than whatever unrelated
error the first newer syntax or library call would raise. These tests keep the two, across every
script, saying the same thing.

The version is faked on the interpreter running these tests, so a script
using syntax newer than a real old Python still passes here while that
Python stops at a `SyntaxError` before reaching the check. CI's Tools Tests
job covers that by running each script on a real 3.9 as well.

Raising the minimum when it reaches end of life is part of cutting a
release: `AGENTS.md`'s Version Lifecycle guideline says what to change.

The check is pasted into each script rather than imported from a shared
module because it has to run before the script's first import, and each
script is run on its own; these tests are what keep the copies agreeing.

`.claude/hooks/` is left out, a choice ratect#858's brief left to whoever
implemented it. Claude Code runs its hook under
whichever `python3` is on the contributor's PATH, never through `uv`, so a
check there would only turn a working nudge into an error on stock macOS's
3.9.
"""

import re
import subprocess
import sys
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent

MINIMUM = (3, 11)

SCRIPTS = sorted(p for p in TOOLS.glob("*.py") if not p.name.startswith("test_"))

# Arguments that keep a script from doing its work if it gets past its version
# check, for a script whose argument-free run is too slow or too destructive
# to make here: the guard comes before argument parsing, so it still runs.
PAST_THE_CHECK = {
    # A cold doc build of the whole workspace.
    "check-doc-links.py": ["--help"],
}

HEADER = re.compile(
    r"^# /// script\n((?:#(?: .*)?\n)*?)# ///$",
    re.MULTILINE,
)


def run_as(version, script, *args):
    """Runs `script` as `__main__` with `sys.version_info` reporting `version`.

    A named tuple, like the real one, so a check written as
    `sys.version_info.minor` reads the faked version too.
    """
    stub = (
        "import collections, runpy, sys\n"
        "VersionInfo = collections.namedtuple("
        "'version_info', 'major minor micro releaselevel serial')\n"
        f"sys.version_info = VersionInfo(*{version!r})\n"
        f"sys.argv = {[str(script), *args]!r}\n"
        f"runpy.run_path({str(script)!r}, run_name='__main__')\n"
    )
    return subprocess.run(
        [sys.executable, "-c", stub],
        cwd=ROOT,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=30,
    )


class PythonMinimumTest(unittest.TestCase):
    def test_there_are_scripts_to_check(self):
        # A glob that matched nothing would pass every test below.
        self.assertGreaterEqual(len(SCRIPTS), 4, SCRIPTS)

    def test_every_script_declares_the_minimum_in_a_pep_723_header(self):
        expected = f'requires-python = ">={MINIMUM[0]}.{MINIMUM[1]}"'
        for script in SCRIPTS:
            with self.subTest(script=script.name):
                headers = HEADER.findall(script.read_text())
                self.assertEqual(len(headers), 1, "one `# /// script` block")
                lines = [line[2:] for line in headers[0].splitlines()]
                self.assertIn(expected, lines)
                self.assertIn("dependencies = []", lines)

    def test_every_script_refuses_an_older_python_and_names_uv_run(self):
        older = (MINIMUM[0], MINIMUM[1] - 1, 0, "final", 0)
        for script in SCRIPTS:
            with self.subTest(script=script.name):
                result = run_as(older, script, *PAST_THE_CHECK.get(script.name, ()))
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertIn(f"Python {MINIMUM[0]}.{MINIMUM[1]} or later", result.stderr)
                self.assertIn(f"{older[0]}.{older[1]}", result.stderr)
                self.assertIn(f"uv run tools/{script.name}", result.stderr)

    def test_no_script_refuses_the_minimum_itself(self):
        # A check stricter than the header would refuse a Python the header
        # admits, and the test above couldn't tell.
        minimum = (*MINIMUM, 0, "final", 0)
        for script in SCRIPTS:
            with self.subTest(script=script.name):
                result = run_as(minimum, script, *PAST_THE_CHECK.get(script.name, ()))
                self.assertNotIn("needs Python", result.stderr)
                self.assertNotIn("AttributeError", result.stderr)

    def test_mdbook_ansi_answers_the_supports_probe_on_an_older_python(self):
        # mdBook skips a preprocessor whose `supports` probe fails and still
        # builds green, so refusing there would publish raw ```ansi blocks.
        # Answering it gets the real run, which then refuses loudly.
        older = (MINIMUM[0], MINIMUM[1] - 1, 0, "final", 0)
        script = TOOLS / "mdbook-ansi.py"
        supports = run_as(older, script, "supports", "html")
        self.assertEqual(supports.returncode, 0, supports.stderr)
        # Any other renderer gets the answer the real run gives it.
        for version in (older, (*MINIMUM, 0, "final", 0)):
            with self.subTest(version=version):
                other = run_as(version, script, "supports", "markdown")
                self.assertEqual(other.returncode, 1, other.stderr)
        run = run_as(older, script)
        self.assertNotEqual(run.returncode, 0)
        self.assertIn("or later, and this is", run.stderr)

    def test_the_workflows_and_contributing_state_the_same_minimum(self):
        minimum = f"{MINIMUM[0]}.{MINIMUM[1]}"
        workflows = ROOT / ".github" / "workflows"
        pins = r"python-version:\s*[\"']?(\d+)\.(\d+)\b"
        # Only the Tools Tests job in ci.yml, up to the next job's key: another
        # job may pin a Python for its own reasons. Pins older than the
        # minimum there are the refusal step's; newer ones are a minimum left
        # behind.
        ci = (workflows / "ci.yml").read_text()
        tools_job = re.split(r"\n  [\w-]+:\n", ci.split("\n  tools:\n", 1)[1], 1)[0]
        pinned = [(int(a), int(b)) for a, b in re.findall(pins, tools_job)]
        self.assertIn(MINIMUM, pinned)
        self.assertEqual([v for v in pinned if v > MINIMUM], [])
        # release.yml has no refusal step, so every pin is the minimum.
        release = (workflows / "release.yml").read_text()
        pinned = [(int(a), int(b)) for a, b in re.findall(pins, release)]
        self.assertEqual(sorted(set(pinned)), [MINIMUM])
        tools_section = (ROOT / "CONTRIBUTING.md").read_text().split(
            "## The `tools/` scripts", 1
        )[1].split("\n## ", 1)[0]
        tools_section = " ".join(tools_section.split())
        self.assertIn(f"Python {minimum} or later", tools_section)
        self.assertIn(f"on {minimum} or later", tools_section)
        self.assertIn(f"--python '>={minimum}'", tools_section)


if __name__ == "__main__":
    unittest.main()
