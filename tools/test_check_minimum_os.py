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

"""Tests for `check-minimum-os.py`.

Run as `CONTRIBUTING.md`'s "The `tools/` scripts" section says.

The fixtures are built here, a few hundred bytes each, rather than checked
in: each holds only the headers and tables the script reads, laid out the
way the formats specify, so a test needs neither a real binary nor the
platform's own tools. The expected values are the ones `otool -l`, `file`,
`strings` and, for which version references are weak, `objdump -p`
reported for `ratect` 0.13.0's release assets (ratect#855).
"""

import importlib.util
import re
import struct
import tempfile
import tomllib
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
ROOT = TOOLS.parent


def load():
    """Imports `check-minimum-os.py` by path; the hyphens rule out a name."""
    path = TOOLS / "check-minimum-os.py"
    spec = importlib.util.spec_from_file_location("check_minimum_os_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# The architectures the release targets build for, as each format numbers
# them: ELF's `e_machine`, Mach-O's `cputype`.
X86_64_ELF, AARCH64_ELF = 62, 183
X86_64_MACHO, ARM64_MACHO = 0x01000007, 0x0100000C


def elf(*, machine=X86_64_ELF, interpreter=None, dynamic=False, needed=(), versions=(), section_headers=True):
    """A 64-bit little-endian ELF executable with only what the script reads.

    `versions` is `[(file, [version names])]`, written as a
    `.gnu.version_r` section the way the linker writes one: a chain of
    `Elf64_Verneed` entries, each followed by its chain of `Elf64_Vernaux`,
    with names in the `.dynstr` its `sh_link` names. `needed` becomes
    `DT_NEEDED` entries in a `PT_DYNAMIC` segment, which `dynamic` adds on
    its own for a static-pie binary, which has one with no `DT_NEEDED`.
    Without `section_headers`, `e_shoff` and `e_shnum` are 0, as `strip
    --strip-section-headers` leaves them.
    """
    strtab = bytearray(b"\0")

    def string(text):
        offset = len(strtab)
        strtab.extend(text.encode() + b"\0")
        return offset

    verneed = bytearray()
    for i, (file, names) in enumerate(versions):
        aux = bytearray()
        for j, name in enumerate(names):
            # A `(name, flags)` pair sets `vna_flags`: 0x2 is
            # `VER_FLG_WEAK`, which the real binary's libc.so.6 GLIBC_2.18,
            # 2.25, 2.28, 2.29 and 2.30 carry.
            name, flags = name if isinstance(name, tuple) else (name, 0)
            following = 16 if j < len(names) - 1 else 0
            aux += struct.pack("<IHHII", 0, flags, 0, string(name), following)
        following = 16 + len(aux) if i < len(versions) - 1 else 0
        verneed += struct.pack("<HHIII", 1, len(names), string(file), 16, following) + aux
    dyn = b"".join(struct.pack("<qQ", 1, string(name)) for name in needed)
    dyn += struct.pack("<qQ", 0, 0)
    interp = interpreter.encode() + b"\0" if interpreter else b""

    segments = []
    if interpreter:
        segments.append((3, interp))
    if dynamic or needed or interpreter:
        segments.append((2, dyn))
    sections = [(3, bytes(strtab), 0, 0)]
    if versions:
        sections.append((0x6FFFFFFE, bytes(verneed), 1, len(versions)))

    offset = 64 + 56 * len(segments)
    body = bytearray()
    phdrs = bytearray()
    for p_type, data in segments:
        phdrs += struct.pack("<IIQQQQQQ", p_type, 4, offset + len(body), 0, 0, len(data), len(data), 8)
        body += data
    shdrs = bytearray(64)  # SHN_UNDEF's all-zero entry
    for sh_type, data, link, info in sections:
        shdrs += struct.pack("<IIQQQQIIQQ", 0, sh_type, 0, 0, offset + len(body), len(data), link, info, 8, 0)
        body += data
    shoff, shnum = offset + len(body), len(sections) + 1
    if not section_headers:
        shdrs, shoff, shnum = b"", 0, 0
    header = struct.pack(
        "<16sHHIQQQIHHHHHH",
        b"\x7fELF\x02\x01\x01" + bytes(9),
        3, machine, 1, 0, 64, shoff, 0, 64, 56, len(segments), 64, shnum, 0,
    )
    return header + phdrs + bytes(body) + bytes(shdrs)


def nibbles(major, minor, patch=0):
    """A Mach-O version: `xxxx.yy.zz` packed into one 32-bit word."""
    return major << 16 | minor << 8 | patch


def macho(*commands, cputype=ARM64_MACHO):
    """A thin 64-bit Mach-O executable holding `commands`, after an
    `LC_UUID` the script has to step over."""
    commands = [struct.pack("<II16s", 0x1B, 24, bytes(16)), *commands]
    header = struct.pack(
        "<IiiIIIII", 0xFEEDFACF, cputype, 0, 2, len(commands), sum(map(len, commands)), 0, 0
    )
    return header + b"".join(commands)


def build_version(minos, sdk=(15, 5)):
    """`LC_BUILD_VERSION` for macOS, as `aarch64-apple-darwin`'s carries."""
    return struct.pack("<IIIIII", 0x32, 24, 1, nibbles(*minos), nibbles(*sdk), 0)


def version_min_macosx(version, sdk=(15, 5)):
    """`LC_VERSION_MIN_MACOSX`, as `x86_64-apple-darwin`'s carries."""
    return struct.pack("<IIII", 0x24, 16, nibbles(*version), nibbles(*sdk))


def glibc_binary(highest="GLIBC_2.34"):
    return elf(
        interpreter="/lib64/ld-linux-x86-64.so.2",
        needed=["libc.so.6", "libgcc_s.so.1"],
        versions=[
            ("libgcc_s.so.1", ["GCC_3.0"]),
            # The highest between two lower strong versions, so that
            # neither the first nor the last strong one read can pass for
            # it, and a weak one after them all that mustn't count.
            ("libc.so.6", ["GLIBC_2.2.5", highest, "GLIBC_2.17", ("GLIBC_2.30", 0x2)]),
        ],
    )


TARGETS = (
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)

# Each measurement, as a fixture binary.
BINARIES = {
    "static-pie x86_64": lambda: elf(dynamic=True),
    "static aarch64": lambda: elf(machine=AARCH64_ELF),
    "glibc 2.31": lambda: glibc_binary("GLIBC_2.31"),
    "glibc 2.34": lambda: glibc_binary("GLIBC_2.34"),
    "glibc 2.39": lambda: glibc_binary("GLIBC_2.39"),
    "only weak glibc versions": lambda: elf(
        interpreter="/lib64/ld-linux-x86-64.so.2",
        needed=["libc.so.6"],
        versions=[("libc.so.6", [("GLIBC_2.30", 0x2)])],
    ),
    "macOS 10.11 x86_64": lambda: macho(version_min_macosx((10, 11)), cputype=X86_64_MACHO),
    "macOS 10.12 x86_64": lambda: macho(version_min_macosx((10, 12)), cputype=X86_64_MACHO),
    "macOS 10.13 x86_64": lambda: macho(version_min_macosx((10, 13)), cputype=X86_64_MACHO),
    "macOS 10.15 arm64": lambda: macho(build_version((10, 15))),
    "macOS 11.0 arm64": lambda: macho(build_version((11, 0))),
    "macOS 12.0 arm64": lambda: macho(build_version((12, 0))),
    "macOS 11.0.1 arm64": lambda: macho(build_version((11, 0, 1))),
}

PASS, NOTE, RAISE, WRONG, UNEXPECTED = "pass", "note", "raise", "wrong file", "unexpected"

# Every measurement against every target, each verdict written by hand
# rather than derived, so that a wrong rule in the script disagrees with a
# cell here. Columns in TARGETS order.
VERDICTS = {
    #                           gnu         musl x86_64 musl aarch64 darwin x86_64 darwin arm64
    "static-pie x86_64":        (UNEXPECTED, PASS,       WRONG,       WRONG,        WRONG),
    "static aarch64":           (WRONG,      WRONG,      PASS,        WRONG,        WRONG),
    "glibc 2.31":               (NOTE,       UNEXPECTED, WRONG,       WRONG,        WRONG),
    "glibc 2.34":               (PASS,       UNEXPECTED, WRONG,       WRONG,        WRONG),
    "glibc 2.39":               (RAISE,      UNEXPECTED, WRONG,       WRONG,        WRONG),
    "only weak glibc versions": (UNEXPECTED, UNEXPECTED, WRONG,       WRONG,        WRONG),
    "macOS 10.11 x86_64":       (WRONG,      WRONG,      WRONG,       NOTE,         WRONG),
    "macOS 10.12 x86_64":       (WRONG,      WRONG,      WRONG,       PASS,         WRONG),
    "macOS 10.13 x86_64":       (WRONG,      WRONG,      WRONG,       RAISE,        WRONG),
    "macOS 10.15 arm64":        (WRONG,      WRONG,      WRONG,       WRONG,        NOTE),
    "macOS 11.0 arm64":         (WRONG,      WRONG,      WRONG,       WRONG,        PASS),
    "macOS 12.0 arm64":         (WRONG,      WRONG,      WRONG,       WRONG,        RAISE),
    "macOS 11.0.1 arm64":       (WRONG,      WRONG,      WRONG,       WRONG,        RAISE),
}

# What each failing verdict's message, and only its message, says.
FAILURE_WORDS = {RAISE: "### Breaking", WRONG: "but this is a", UNEXPECTED: "the build likely changed"}


class CheckTest(unittest.TestCase):
    def setUp(self):
        self.script = load()
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def run_check(self, target, binary):
        path = Path(self.dir.name) / "binary"
        path.write_bytes(binary)
        out, err = StringIO(), StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = self.script.main([target, str(path)])
        return code, out.getvalue(), err.getvalue()

    def test_every_measurement_against_every_target(self):
        self.assertEqual(set(VERDICTS), set(BINARIES))
        for measurement, verdicts in VERDICTS.items():
            for target, verdict in zip(TARGETS, verdicts, strict=True):
                with self.subTest(measurement=measurement, target=target, verdict=verdict):
                    code, out, err = self.run_check(target, BINARIES[measurement]())
                    if verdict in (PASS, NOTE):
                        self.assertEqual((code, err), (0, ""))
                        self.assertIn(f"{target} needs", out)
                        self.assertEqual("note:" in out, verdict == NOTE, out)
                    else:
                        self.assertEqual((code, out), (1, ""))
                        for kind, words in FAILURE_WORDS.items():
                            self.assertEqual(words in err, kind == verdict, err)

    def test_a_raise_names_the_measured_and_recorded_values(self):
        code, _, err = self.run_check("x86_64-unknown-linux-gnu", glibc_binary("GLIBC_2.39"))
        self.assertEqual(code, 1)
        self.assertIn("x86_64-unknown-linux-gnu needs glibc 2.39", err)
        self.assertIn("recorded minimum is glibc 2.34", err)

    def test_lowering_the_record_below_the_binary_fails(self):
        # Acceptance criterion's own example: the record lowered to 2.31.
        self.script.RECORDED["x86_64-unknown-linux-gnu"] = (2, 31)
        code, _, err = self.run_check("x86_64-unknown-linux-gnu", glibc_binary())
        self.assertEqual(code, 1)
        self.assertIn("needs glibc 2.34", err)
        self.assertIn("recorded minimum is glibc 2.31", err)

    def test_a_note_names_where_to_lower_the_record(self):
        _, out, _ = self.run_check("x86_64-unknown-linux-gnu", glibc_binary("GLIBC_2.31"))
        self.assertIn("could be lowered to glibc 2.31", out)
        self.assertIn("RECORDED in tools/check-minimum-os.py", out)
        _, out, _ = self.run_check("aarch64-apple-darwin", macho(build_version((10, 15))))
        self.assertIn("could be lowered to macOS 10.15", out)

    def test_a_wrong_file_names_both_platforms_and_architectures(self):
        code, _, err = self.run_check("aarch64-apple-darwin", elf(machine=AARCH64_ELF))
        self.assertEqual(code, 1)
        self.assertIn("aarch64-apple-darwin is for macOS aarch64", err)
        self.assertIn("a Linux aarch64 binary", err)

    def test_an_unknown_architecture_is_named_by_its_number(self):
        code, _, err = self.run_check("aarch64-unknown-linux-musl", elf(machine=40))
        self.assertEqual(code, 1)
        self.assertIn("a Linux machine 40 binary", err)

    def test_a_weak_version_reference_is_not_a_requirement(self):
        # glibc's loader runs a binary missing a weak version.
        binary = elf(
            interpreter="/lib64/ld-linux-x86-64.so.2",
            needed=["libc.so.6"],
            versions=[("libc.so.6", ["GLIBC_2.2.5", ("GLIBC_2.39", 0x2), "GLIBC_2.34"])],
        )
        code, out, err = self.run_check("x86_64-unknown-linux-gnu", binary)
        self.assertEqual((code, err), (0, ""))
        self.assertIn("needs glibc 2.34;", out)

    def test_a_needed_library_alone_makes_a_binary_dynamic(self):
        # No PT_INTERP, as a shared library would be: DT_NEEDED still counts.
        binary = elf(needed=["libgcc_s.so.1"], versions=[("libgcc_s.so.1", ["GCC_3.0"])])
        code, _, err = self.run_check("x86_64-unknown-linux-musl", binary)
        self.assertEqual(code, 1)
        self.assertIn("needs shared libraries with no required glibc symbol version", err)

    def test_a_glibc_version_with_no_number_is_refused_even_if_weak(self):
        # GLIBC_ABI_DT_RELR needs glibc 2.36, and its name doesn't say so.
        for flags in (0, 0x2):
            with self.subTest(flags=flags):
                binary = elf(
                    interpreter="/lib64/ld-linux-x86-64.so.2",
                    needed=["libc.so.6"],
                    versions=[("libc.so.6", ["GLIBC_2.34", ("GLIBC_ABI_DT_RELR", flags)])],
                )
                code, out, err = self.run_check("x86_64-unknown-linux-gnu", binary)
                self.assertEqual((code, out), (1, ""))
                self.assertIn("GLIBC_ABI_DT_RELR", err)

    def test_a_dynamic_binary_with_no_version_table_is_refused(self):
        # Both shapes: a table missing from the section headers, and the
        # section headers stripped altogether.
        for headers in (True, False):
            with self.subTest(section_headers=headers):
                binary = elf(
                    interpreter="/lib64/ld-linux-x86-64.so.2",
                    needed=["libc.so.6"],
                    section_headers=headers,
                )
                code, out, err = self.run_check("x86_64-unknown-linux-gnu", binary)
                self.assertEqual((code, out), (1, ""))
                self.assertIn(
                    "no .gnu.version_r section" if headers else "section headers stripped", err
                )

    def test_a_mach_o_with_neither_command_is_refused(self):
        code, _, err = self.run_check("aarch64-apple-darwin", macho())
        self.assertEqual(code, 1)
        self.assertIn("neither LC_BUILD_VERSION nor LC_VERSION_MIN_MACOSX", err)

    def test_a_file_in_neither_format_is_refused(self):
        code, _, err = self.run_check("aarch64-apple-darwin", b"#!/bin/sh\n")
        self.assertEqual(code, 1)
        self.assertIn("neither an ELF nor a Mach-O file", err)

    def test_a_missing_file_is_refused(self):
        out, err = StringIO(), StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = self.script.main(["aarch64-apple-darwin", str(Path(self.dir.name) / "absent")])
        self.assertEqual((code, out.getvalue()), (1, ""))
        self.assertIn("can't measure its minimum OS", err.getvalue())

    def test_a_target_with_no_record_is_refused(self):
        with self.assertRaises(SystemExit) as raised, redirect_stderr(StringIO()):
            self.script.main(["riscv64gc-unknown-linux-gnu", "binary"])
        self.assertEqual(raised.exception.code, 2)


class RecordTest(unittest.TestCase):
    def setUp(self):
        self.recorded = load().RECORDED

    def test_every_record_has_the_shape_its_triple_asks_for(self):
        script = load()
        for target in self.recorded:
            with self.subTest(target=target):
                script.expectation(target)

    def test_a_record_of_the_wrong_shape_is_refused(self):
        for target, version in (("x86_64-unknown-linux-gnu", None), ("x86_64-unknown-linux-musl", (2, 17))):
            with self.subTest(target=target):
                script = load()
                script.RECORDED[target] = version
                with self.assertRaises(ValueError):
                    script.expectation(target)

    def test_every_release_target_has_a_record_and_no_other(self):
        dist = tomllib.loads((ROOT / "dist-workspace.toml").read_text())
        self.assertEqual(sorted(self.recorded), sorted(dist["dist"]["targets"]))
        self.assertEqual(sorted(self.recorded), sorted(TARGETS))

    # This test says which targets are GNU or macOS from the triple itself,
    # rather than asking the script's `expectation`, so that a wrong rule
    # there disagrees with it instead of agreeing.

    def test_the_installation_page_states_each_recorded_minimum(self):
        page = (ROOT / "docs" / "installation.md").read_text()
        rows = dict(re.findall(r"^\| `([\w-]+)` \| [^|]+ \| ([^|]+?) \|$", page, re.MULTILINE))
        expected = {}
        for target, version in self.recorded.items():
            if version is None:
                expected[target] = "No shared libc: statically linked"
            else:
                system = "macOS" if target.endswith("-apple-darwin") else "glibc"
                expected[target] = f"{system} {'.'.join(map(str, version))} or later"
        self.assertEqual(rows, expected)

    def test_the_installation_page_names_distributions_by_the_recorded_glibc(self):
        # The paragraph under the table repeats the GNU minimum in prose.
        page = (ROOT / "docs" / "installation.md").read_text()
        for target, version in self.recorded.items():
            if target.endswith("-linux-gnu"):
                stated = re.findall(r"ship glibc (\d+\.\d+) or newer", page)
                self.assertEqual(stated, [".".join(map(str, version))])


if __name__ == "__main__":
    unittest.main()
