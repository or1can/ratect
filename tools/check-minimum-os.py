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

"""Fails if a built release binary needs a newer OS than its target records.

    uv run tools/check-minimum-os.py <target-triple> <binary>

Prints what the binary needs and exits 0 when that is within `RECORDED`'s
minimum for its target, adding a note when the record could be lowered. It
exits 1 naming both when the binary needs more, when it is for another
platform or architecture than its target, or when it needs a different kind
of thing than its target records, such as a `musl` build that links a shared
libc; and exits 1 saying it can't measure a file it can't read. The release
workflow's `Smoke test` step runs it on every binary before upload
(ratect#855).

# Why this exists

A binary's minimum OS is set by how it was built, not by anything in this
repository's source. The GNU Linux binary needs the newest version of each
glibc symbol it uses that the runner image's glibc provides, so moving the
build to a newer image can raise its minimum as far as that image's glibc,
for every user, and nothing else would notice. The macOS minimum comes from
the Rust toolchain's deployment target, which a toolchain update can raise
the same way. `RECORDED` is what `docs/installation.md` promises; raising
an entry is a breaking change, made on purpose with a `### Breaking`
`CHANGELOG.md` entry, never by a runner change.

# What it reads

- **Both**: the architecture, from ELF's `e_machine` or Mach-O's `cputype`,
  and the platform, from the format, which must match the target triple's.
- **Mach-O**: the `minos` of `LC_BUILD_VERSION`, or the `version` of the
  older `LC_VERSION_MIN_MACOSX`. The linker writes one or the other: the
  Intel binary carries the older command and the Apple Silicon one the
  newer, so both are read.
- **ELF**: a binary with a `PT_INTERP` segment or a `DT_NEEDED` entry is
  dynamically linked; one with neither is static, which is what a `musl`
  target records, and is unexpected for a GNU one. A static-pie binary has
  a `PT_DYNAMIC` segment with no `DT_NEEDED`, so the segment's presence
  alone decides nothing. A dynamic binary's requirement is the highest
  `GLIBC_x.y` its `.gnu.version_r` section asks for, not counting a weak
  reference, which the loader doesn't insist on. One whose section asks
  for no such version, or only weak ones, is unexpected for any target: it
  says nothing a minimum could record. One with no such section can't be
  measured. Nor can one that asks for a glibc version with no number, such
  as `GLIBC_ABI_DT_RELR`: refusing it, rather than skipping it, can't miss
  a newer minimum.

Only the standard library parses them, so it reads the same on every runner
and its tests need no platform tools. `dist build --print=linkage`, which
the release workflow already runs, answers only part of this: by its own
`--help`, it "prints information on dynamic libraries used by build
artifacts", which bears on the `musl` case but says nothing of the symbol
versions a binary needs from those libraries, or of its deployment target.
The ELF has to be parsed for the glibc case anyway, so the `musl` case is
read from it too.
"""

import sys

# Before any other import: the PEP 723 header above is what `uv run` reads,
# and this is what a plain `python3` older than it meets instead.
if sys.version_info < (3, 11):
    sys.exit(
        "tools/check-minimum-os.py needs Python 3.11 or later, and this is %d.%d. "
        "Run it as `uv run tools/check-minimum-os.py`, which fetches one." % sys.version_info[:2]
    )

import argparse
import re
import struct
from dataclasses import dataclass
from pathlib import Path

# Each release target's minimum version: what `docs/installation.md`'s table
# says, and for glibc what `dist-workspace.toml`'s `min-glibc-version` makes
# the shell installer require; tests hold both to it. What kind of version it
# is comes from the target triple (`expectation`), and a `musl` target's is
# `None`: it is statically linked, and needs no shared libc.
RECORDED = {
    "x86_64-unknown-linux-gnu": (2, 34),
    "x86_64-unknown-linux-musl": None,
    "aarch64-unknown-linux-musl": None,
    "x86_64-apple-darwin": (10, 12),
    "aarch64-apple-darwin": (11, 0),
}

GLIBC_NUMBERED = re.compile(rb"GLIBC_(\d+)\.(\d+)(?:\.(\d+))?")

# Architectures by each format's number for them, named as the target
# triples name them.
ELF_MACHINES = {62: "x86_64", 183: "aarch64"}
MACHO_CPUS = {0x01000007: "x86_64", 0x0100000C: "aarch64"}


def dotted(version):
    return ".".join(str(part) for part in version)


@dataclass(frozen=True)
class Static:
    """Needing no shared libc: a statically linked ELF binary, or what a
    `musl` target records."""

    def __str__(self):
        return "no shared libc"


@dataclass(frozen=True)
class Glibc:
    """Needing glibc `version` or later: the highest a dynamic ELF binary
    requires, or what a GNU target records."""

    version: tuple

    def __str__(self):
        return f"glibc {dotted(self.version)}"


@dataclass(frozen=True)
class NoGlibcVersion:
    """A dynamic ELF binary requiring no glibc version, or only weak ones.
    No target records this."""

    def __str__(self):
        return "shared libraries with no required glibc symbol version"


@dataclass(frozen=True)
class MacOS:
    """Needing macOS `version` or later: a Mach-O binary's deployment
    target, or what a macOS target records."""

    version: tuple

    def __str__(self):
        return f"macOS {dotted(self.version)}"


@dataclass(frozen=True)
class Measurement:
    """What a binary is for, and what it needs there; or, from
    `expectation`, what a target promises."""

    platform: str
    arch: str
    needs: Static | Glibc | NoGlibcVersion | MacOS


@dataclass(frozen=True)
class Verdict:
    """Whether a binary keeps its target's promise, in words for the user."""

    passed: bool
    message: str
    note: str | None = None


class Unreadable(Exception):
    """The binary isn't one this script can measure."""


def elf_measurement(data):
    """What an ELF binary needs: see "What it reads"."""
    if data[4:6] != b"\x02\x01":
        raise Unreadable("not a 64-bit little-endian ELF file")
    (machine,) = struct.unpack_from("<H", data, 18)
    arch = ELF_MACHINES.get(machine, f"machine {machine}")
    (phoff, shoff) = struct.unpack_from("<QQ", data, 32)
    (phentsize, phnum, shentsize, shnum) = struct.unpack_from("<HHHH", data, 54)
    dynamic = False
    for i in range(phnum):
        p_type, _, p_offset, _, _, p_filesz = struct.unpack_from("<IIQQQQ", data, phoff + i * phentsize)
        if p_type == 3:  # PT_INTERP
            dynamic = True
        elif p_type == 2:  # PT_DYNAMIC
            for entry in range(p_offset, p_offset + p_filesz, 16):
                (tag,) = struct.unpack_from("<q", data, entry)
                if tag == 0:  # DT_NULL
                    break
                dynamic |= tag == 1  # DT_NEEDED
    if not dynamic:
        return Measurement("Linux", arch, Static())
    sections = [struct.unpack_from("<IIQQQQII", data, shoff + i * shentsize) for i in range(shnum)]
    highest = None
    found_table = False
    for _, sh_type, _, _, offset, _, link, count in sections:
        if sh_type != 0x6FFFFFFE:  # SHT_GNU_verneed
            continue
        found_table = True
        strings = sections[link][4]
        for _ in range(count):
            _, aux_count, _, aux, following = struct.unpack_from("<HHIII", data, offset)
            entry = offset + aux
            for _ in range(aux_count):
                _, flags, _, name_at, aux_following = struct.unpack_from("<IHHII", data, entry)
                name = data[strings + name_at : data.index(b"\0", strings + name_at)]
                entry += aux_following
                if not name.startswith(b"GLIBC_"):
                    continue
                match = GLIBC_NUMBERED.fullmatch(name)
                if not match:
                    # GLIBC_ABI_DT_RELR, say, which only glibc 2.36 and
                    # later provide: the name alone can't be compared.
                    # Refused even if weak, since an older glibc would then
                    # load a binary whose relocations it can't apply.
                    raise Unreadable(
                        f"it needs {name.decode()}, a glibc version with no number; "
                        "find the glibc release that added it and teach this script"
                    )
                # VER_FLG_WEAK: the loader runs a binary without it. ratect
                # 0.13.0's GNU binary marks libc.so.6's GLIBC_2.18, 2.25,
                # 2.28, 2.29 and 2.30 this way.
                if flags & 0x2:
                    continue
                version = tuple(int(part) for part in match.groups() if part is not None)
                highest = max(highest or version, version)
            offset += following
    if not found_table:
        # The loader reads the same table through DT_VERNEED, but the
        # release binaries keep their section headers, so this is the one
        # place read.
        if not shnum:
            raise Unreadable(
                "a dynamically linked ELF file with its section headers stripped, "
                "which this script reads its version table from"
            )
        raise Unreadable(
            "a dynamically linked ELF file with no .gnu.version_r section, "
            "so nothing says which libc version it needs"
        )
    return Measurement("Linux", arch, Glibc(highest) if highest else NoGlibcVersion())


def macho_measurement(data):
    """What a Mach-O binary needs: see "What it reads"."""
    if data[:4] != b"\xcf\xfa\xed\xfe":
        raise Unreadable("not a thin 64-bit little-endian Mach-O file")
    (cputype,) = struct.unpack_from("<I", data, 4)
    arch = MACHO_CPUS.get(cputype, f"CPU type {cputype:#x}")
    (ncmds,) = struct.unpack_from("<I", data, 16)
    offset = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", data, offset)
        if cmd == 0x32:  # LC_BUILD_VERSION
            platform, minos = struct.unpack_from("<II", data, offset + 8)
            if platform == 1:  # PLATFORM_MACOS
                return Measurement("macOS", arch, MacOS(unpack_version(minos)))
        elif cmd == 0x24:  # LC_VERSION_MIN_MACOSX
            (version,) = struct.unpack_from("<I", data, offset + 8)
            return Measurement("macOS", arch, MacOS(unpack_version(version)))
        offset += cmdsize
    raise Unreadable("a Mach-O file with neither LC_BUILD_VERSION nor LC_VERSION_MIN_MACOSX for macOS")


def unpack_version(word):
    """A Mach-O `xxxx.yy.zz` version word, without a zero patch, so that
    `11.0.0` compares equal to a recorded `11.0`."""
    version = (word >> 16, word >> 8 & 0xFF, word & 0xFF)
    return version if version[2] else version[:2]


def measure(data):
    """What a binary is for, and what it needs there."""
    if data[:4] == b"\x7fELF":
        return elf_measurement(data)
    if data[:4] in (b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe"):
        return macho_measurement(data)
    raise Unreadable("neither an ELF nor a Mach-O file")


def expectation(target):
    """What `target` promises, from its triple and `RECORDED`."""
    arch = target.split("-", 1)[0]
    version = RECORDED[target]
    if target.endswith("-apple-darwin"):
        platform, recorded = "macOS", MacOS(version)
    elif target.endswith("-linux-gnu"):
        platform, recorded = "Linux", Glibc(version)
    elif target.endswith("-linux-musl"):
        platform, recorded = "Linux", Static()
    else:
        raise ValueError(f"no rule says what {target} runs on")
    if (version is None) != isinstance(recorded, Static):
        raise ValueError(f"RECORDED's {target} is {version!r}, not what its triple asks for")
    return Measurement(platform, arch, recorded)


def where_recorded(requirement):
    """Every place a minimum of `requirement`'s kind is written down."""
    if isinstance(requirement, Glibc):
        return (
            "RECORDED in tools/check-minimum-os.py, docs/installation.md's table "
            "and dist-workspace.toml's min-glibc-version"
        )
    return "RECORDED in tools/check-minimum-os.py and docs/installation.md's table"


def judge(target, measured):
    """Whether a binary measured as `measured` keeps `target`'s promise."""
    expected = expectation(target)
    recorded = expected.needs
    if (measured.platform, measured.arch) != (expected.platform, expected.arch):
        return Verdict(
            False,
            f"{target} is for {expected.platform} {expected.arch}, but this is a "
            f"{measured.platform} {measured.arch} binary.",
        )
    needs = measured.needs
    summary = f"{target} needs {needs}; recorded minimum {recorded}"
    # No `match`: CI runs this file on Python 3.9 to see its version check
    # refuse, and 3.9 would stop at a `match` with a SyntaxError first.
    if isinstance(needs, Static) and isinstance(recorded, Static):
        return Verdict(True, summary)
    if isinstance(needs, (Glibc, MacOS)) and type(needs) is type(recorded):
        if needs.version == recorded.version:
            return Verdict(True, summary)
        if needs.version < recorded.version:
            return Verdict(
                True,
                summary,
                f"note: {target}'s recorded minimum, {recorded}, could be lowered to "
                f"{needs}: change {where_recorded(needs)}.",
            )
        return Verdict(
            False,
            f"{target} needs {needs}, but its recorded minimum is {recorded}. If that is "
            f"intended, it is a breaking change: raise {where_recorded(needs)} to "
            "match, and say so in a `### Breaking` CHANGELOG.md entry.",
        )
    return Verdict(
        False,
        f"{target} needs {needs}, where its record says {recorded}: not a minimum "
        "to raise, since the build likely changed.",
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "target", choices=sorted(RECORDED), help="the target triple the binary was built for"
    )
    parser.add_argument("binary", type=Path)
    args = parser.parse_args(argv)
    try:
        measured = measure(args.binary.read_bytes())
    except OSError as error:
        print(f"{args.binary}: can't measure its minimum OS: {error.strerror}", file=sys.stderr)
        return 1
    except (Unreadable, struct.error, IndexError, ValueError) as error:
        print(f"{args.binary}: can't measure its minimum OS: {error}", file=sys.stderr)
        return 1
    verdict = judge(args.target, measured)
    if not verdict.passed:
        print(verdict.message, file=sys.stderr)
        return 1
    print(verdict.message)
    if verdict.note:
        print(verdict.note)
    return 0


if __name__ == "__main__":
    sys.exit(main())
