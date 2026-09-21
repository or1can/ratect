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

"""Runs a command on a real terminal and keeps every byte it printed.

    tools/capture-output.py <name> -- <command> [args...]

Writes `docs/captures/<name>.ansi` (relative to this repo, whatever the
current directory), for a docs page to show as

    ```ansi
    {{#include captures/<name>.ansi}}
    ```

which `tools/mdbook-ansi.py` renders in colour at build time. Exits with
the command's own status — a capture of a failing run is a legitimate
block (a red exit code, a `doctor` finding), so the file is written either
way. Example, from the repo root:

    cargo build -q -p ratect
    tools/capture-output.py output-styles-simple -- \\
        target/debug/ratect run journey-test -f examples/full-stack/ratect.toml -o simple

# Why a pty, and why not `script`

The binary only colours its output when stdout is a terminal (see
`Console::stdout` in `ratect-core/src/ui.rs`), and a task's container only
gets a TTY when stdin and stdout both are — so a capture with either
redirected shows less than a user's terminal does. This allocates a pseudo-
terminal (80x24, `TERM=xterm-256color` — the size and `TERM` the homepage's
asciinema recording uses) and runs the command as its session leader, so
stdin, stdout and stderr are all that terminal, exactly as in a shell.

`script(1)` does the same, but the macOS and util-linux versions take
different flags, and both copy the calling process's stdin into the pty:
run from anything but a live terminal (an agent's shell, a CI step) that
stdin is at EOF, `script` forwards it as `^D`, and the capture starts with
the terminal's echo of it and two backspaces. Reading the pty directly and
forwarding nothing avoids both.
"""

import fcntl
import os
import pty
import re
import struct
import sys
import termios
from pathlib import Path

CAPTURES = Path(__file__).resolve().parent.parent / "docs" / "captures"
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
USAGE = "usage: tools/capture-output.py <name> -- <command> [args...]"


def capture(command, columns=80, rows=24, term="xterm-256color"):
    """Every byte `command` wrote to its terminal, and its exit status."""
    pid, fd = pty.fork()
    if pid == 0:  # The child: size the terminal, then become the command.
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.environ["TERM"] = term
        try:
            os.execvp(command[0], command)
        except OSError as error:
            print(f"cannot run {command[0]}: {error}", file=sys.stderr)
            os._exit(127)

    chunks = []
    while True:
        try:
            chunk = os.read(fd, 65536)
        except OSError:  # EIO: the command exited and the pty closed.
            break
        if not chunk:
            break
        chunks.append(chunk)
    os.close(fd)
    _, status = os.waitpid(pid, 0)
    return b"".join(chunks), os.waitstatus_to_exitcode(status)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) < 3 or argv[1] != "--" or not NAME_RE.match(argv[0]):
        print(USAGE, file=sys.stderr)
        return 2
    name, command = argv[0], argv[2:]

    output, exit_code = capture(command)

    CAPTURES.mkdir(parents=True, exist_ok=True)
    path = CAPTURES / f"{name}.ansi"
    path.write_bytes(output)
    shown = path.relative_to(Path.cwd()) if path.is_relative_to(Path.cwd()) else path
    print(f"wrote {shown} ({len(output)} bytes); command exited {exit_code}", file=sys.stderr)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
