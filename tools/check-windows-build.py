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

"""Type-checks the workspace for Windows from a macOS or Linux host.

    uv run tools/check-windows-build.py

Takes no arguments but `--help`; given the script's path, it runs from any
directory in the checkout. Runs CI's Windows Check command,

    cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-msvc

and exits with cargo's status, cargo's own output passing through
unchanged. Before running it, exits 1 when the Rust target isn't
installed, printing the `rustup target add` command that installs it; 1
when `rustc --print sysroot` fails or can't be run; `cargo metadata`'s
status when that fails, and 1 when cargo can't be run at all; 1 when the
stub can't be written; and 2 on an argument it doesn't take. The target is
looked for in the sysroot of `RUSTC`, or of `rustc` when that is unset.
`RUSTFLAGS` is set to `-D warnings`, as Windows Check sets it, replacing
any value in the environment (and `CARGO_ENCODED_RUSTFLAGS`, which cargo
would read instead, is removed), so a `cfg(unix)`-only helper that ungated
code leaves unused fails here as it does there.

# Why this exists

`AGENTS.md` says Unix-only code goes behind `cfg(unix)`, and CI's Windows
Check job is the only thing that compiles the other side of each gate. Over
0.14.0 that job failed on 4 of 17 PRs, each found after the PR opened
(ratect#904). Checking locally failed differently: `cargo check --target
x86_64-pc-windows-msvc` stops in `ring`'s build script, which compiles C
with the target's compiler, and no Mac or Linux machine has MSVC's `cl.exe`
or `lib.exe`.

# The stub C toolchain

`check` never links, so nothing ever reads what `ring`'s build script
compiles: the C only has to *appear* to build. The `cc` crate, which that
build script calls, takes its compiler and archiver for one target from
`CC_<target>` and `AR_<target>`. This script points both at a stub, a
`/bin/sh` script (`STUB`) written to `check-windows-build/` in cargo's
target directory, which creates an empty file at each output path its arguments name and exits 0:

- `-Fo<path>` or `/Fo<path>`: the object file a `cl.exe`-style compile
  writes. Matched in that case only, as `cl.exe` does, so a GNU flag such
  as `-fomit-frame-pointer` in `CFLAGS` names no file.
- `-out:<path>` or `/out:<path>`, either in lower or upper case: the
  library a `lib.exe`-style archive writes. `cc` writes `-out:`.

Every other invocation prints nothing and exits 0. That includes `cc`'s
two compiler-detection runs, on `-E` and on `-?`, and the second is what
makes `cc` treat the stub as MSVC: in `cc` 1.9's `tool.rs`, a compiler that
exits 0 on `-?` accepts `cl.exe`-style flags. Exiting non-zero there would
make `cc` treat the stub as GNU and pass `-o`, which names no file the stub
creates. With the outputs on disk, the build script finishes and cargo
goes on to check the Rust code, which is the point. Nothing it produces
could link or run, and none of it is meant to: this is a type check only.

The stub's path is the same on every run because `cc` asks cargo to rerun
`ring`'s build script whenever `CC_<target>` changes, and a rerun rechecks
everything that depends on `ring`, this workspace's crates included.

The stub is a shell script, so the host must be one with `/bin/sh`. On
Windows itself, plain `cargo check` needs none of this.
"""

import sys

# Before any other import: the PEP 723 header above is what `uv run` reads,
# and this is what a plain `python3` older than it meets instead.
if sys.version_info < (3, 11):
    sys.exit(
        "tools/check-windows-build.py needs Python 3.11 or later, and this is %d.%d. "
        "Run it as `uv run tools/check-windows-build.py`, which fetches one." % sys.version_info[:2]
    )

import argparse
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

TARGET = "x86_64-pc-windows-msvc"

# Windows Check's command and `RUSTFLAGS`, plus the target; the tests hold
# them to `.github/workflows/ci.yml`'s `windows-check` job.
CHECK = [
    "cargo",
    "check",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--target",
    TARGET,
]
RUSTFLAGS = "-D warnings"

# The stub C toolchain: see "The stub C toolchain".
STUB = """\
#!/bin/sh
# tools/check-windows-build.py's stub C compiler and archiver: see its
# "The stub C toolchain".
for arg in "$@"; do
    case "$arg" in
        -Fo?* | /Fo?*) : >"${arg#???}" ;;
        -out:?* | /out:?* | -OUT:?* | /OUT:?*) : >"${arg#?????}" ;;
    esac
done
exit 0
"""


def write_stub(directory):
    """Writes the stub into `directory`, creating it, and returns the stub's path.

    Leaves a stub that is already there untouched: another run sharing the
    target directory may be executing it, and `/bin/sh` reads a script as
    it runs it.
    """
    directory.mkdir(parents=True, exist_ok=True)
    stub = directory / "c-toolchain-stub"
    if not (stub.is_file() and stub.read_text() == STUB and os.access(stub, os.X_OK)):
        written = directory / f"c-toolchain-stub.{os.getpid()}"
        written.write_text(STUB)
        written.chmod(0o755)
        written.replace(stub)
    return stub


def target_installed(sysroot):
    """Whether the toolchain at `sysroot` has the Windows target's standard library."""
    return (Path(sysroot) / "lib" / "rustlib" / TARGET / "lib").is_dir()


def check_env(environ, stub_path):
    """The environment `cargo check` runs in: `environ` plus the stub and `RUSTFLAGS`."""
    env = dict(environ)
    variable = TARGET.replace("-", "_")
    env[f"CC_{variable}"] = str(stub_path)
    env[f"AR_{variable}"] = str(stub_path)
    env["RUSTFLAGS"] = RUSTFLAGS
    # Cargo reads this one instead of `RUSTFLAGS` when both are set.
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    return env


def sysroot():
    """The sysroot of `RUSTC`, or of the `rustc` on `PATH` from the checkout.

    Cargo also reads `CARGO_BUILD_RUSTC` and `build.rustc`, which this
    doesn't: with either set, cargo itself reports a missing target.
    """
    result = subprocess.run(
        [os.environ.get("RUSTC", "rustc"), "--print", "sysroot"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    return result.stdout.strip()


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

    try:
        rust_sysroot = sysroot()
    except subprocess.CalledProcessError as error:
        print(f"error: `{' '.join(error.cmd)}` failed", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"error: could not run rustc: {error}", file=sys.stderr)
        return 1
    if not target_installed(rust_sysroot):
        print(
            f"error: the Rust target {TARGET} isn't installed in {rust_sysroot}. "
            "With rustup, install it with:\n\n"
            f"    rustup target add {TARGET}",
            file=sys.stderr,
        )
        return 1

    try:
        directory = target_directory() / "check-windows-build"
    except subprocess.CalledProcessError as error:
        print(f"error: `{' '.join(error.cmd)}` failed", file=sys.stderr)
        return error.returncode
    except OSError as error:
        print(f"error: could not run cargo: {error}", file=sys.stderr)
        return 1
    try:
        stub = write_stub(directory)
    except OSError as error:
        print(f"error: could not write the stub C toolchain: {error}", file=sys.stderr)
        return 1
    try:
        return subprocess.run(CHECK, cwd=ROOT, env=check_env(os.environ, stub)).returncode
    except OSError as error:
        print(f"error: could not run cargo: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
