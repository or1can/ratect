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

"""Tests for `mdbook-ansi.py`.

    python3 -m unittest discover -s tools -p 'test_*.py'

The preprocessor's output is what the published site shows for every
captured transcript, and nothing downstream checks it: mdBook passes the
HTML through, and `lychee` only follows links. A wrong answer here is a
transcript that renders as escape-code garbage or, worse, one that renders
plausibly but not as the terminal showed it (an overwritten spinner frame
left standing, a colour on the wrong span). The overwrite tests pin the
real byte patterns `docs/captures/*.ansi` contain — npm's `\\x1b[1G\\x1b[0K`
spinner and the pty's `\\r\\n` line endings — not hypothetical ones.
"""

import importlib.util
import io
import json
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

TOOLS = Path(__file__).resolve().parent


def load(path):
    """Imports a `mdbook-ansi.py` by path.

    The hyphen makes it an invalid module name, so it cannot be imported by
    name.
    """
    spec = importlib.util.spec_from_file_location("mdbook_ansi_under_test", path)
    assert spec and spec.loader, path
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


mdbook_ansi = load(TOOLS / "mdbook-ansi.py")

ESC = "\x1b"


class RenderAnsiTests(unittest.TestCase):
    def test_plain_text_is_wrapped_and_html_escaped(self):
        html = mdbook_ansi.render_ansi('a <b> & {"c":1}\n')

        self.assertEqual(
            html,
            '<pre><code class="nohighlight ansi">a &lt;b&gt; &amp; {"c":1}</code></pre>',
        )

    def test_a_basic_foreground_colour_becomes_a_themed_class(self):
        html = mdbook_ansi.render_ansi(f"exit code {ESC}[32m0{ESC}[0m done\n")

        self.assertIn('exit code <span class="ansi-fg-green">0</span> done', html)

    def test_bold_and_colour_combine_on_one_span(self):
        # npm's `found 0 vulnerabilities`: green, then bold, then each
        # reset by its own code (22 = bold off, 39 = default foreground).
        html = mdbook_ansi.render_ansi(f"found {ESC}[32m{ESC}[1m0{ESC}[22m{ESC}[39m vulns\n")

        self.assertIn('found <span class="ansi-bold ansi-fg-green">0</span> vulns', html)

    def test_bright_and_background_colours(self):
        html = mdbook_ansi.render_ansi(f"{ESC}[91;44mx{ESC}[0m\n")

        self.assertIn('<span class="ansi-fg-bright-red ansi-bg-blue">x</span>', html)

    def test_256_and_truecolour_are_inline_styles(self):
        html = mdbook_ansi.render_ansi(f"{ESC}[38;5;196ma{ESC}[0m{ESC}[48;2;1;2;3mb{ESC}[0m\n")

        self.assertIn('<span style="color:#ff0000">a</span>', html)
        self.assertIn('<span style="background-color:#010203">b</span>', html)

    def test_pty_crlf_line_endings_are_plain_newlines(self):
        html = mdbook_ansi.render_ansi("one\r\ntwo\r\n")

        self.assertIn(">one\ntwo<", html)

    def test_a_spinner_overwritten_in_place_shows_only_its_final_state(self):
        # npm's progress spinner under a TTY: cursor to column 1, erase to
        # end of line, draw a frame — repeated — then the real line.
        frames = "".join(f"{ESC}[1G{ESC}[0K{c}" for c in "\\|/-")
        html = mdbook_ansi.render_ansi(f"{frames}{ESC}[1G{ESC}[0K\r\nadded 7 packages\r\n")

        self.assertIn(">\nadded 7 packages<", html)
        for frame in "\\|/-":
            self.assertNotIn(f">{frame}", html)

    def test_a_carriage_return_overwrites_from_the_start_of_the_line(self):
        html = mdbook_ansi.render_ansi("progress 10%\rprogress 100%\r\n")

        self.assertIn(">progress 100%<", html)

    def test_a_shorter_overwrite_leaves_the_tail_standing(self):
        # What a real terminal shows: `\r` only moves the cursor; the old
        # characters past the new text's end stay.
        html = mdbook_ansi.render_ansi("abcdef\rXY\r\n")

        self.assertIn(">XYcdef<", html)

    def test_erase_to_end_of_line_after_a_carriage_return_drops_the_tail(self):
        html = mdbook_ansi.render_ansi(f"abcdef\rXY{ESC}[K\r\n")

        self.assertIn(">XY<", html)

    def test_backspace_moves_the_cursor_back(self):
        html = mdbook_ansi.render_ansi("^D\b\bRunning\r\n")

        self.assertIn(">Running<", html)

    def test_cursor_up_rewrites_an_earlier_line(self):
        html = mdbook_ansi.render_ansi(f"first\r\nsecond\r\n{ESC}[2A{ESC}[2Kchanged\r\n")

        self.assertIn(">changed\nsecond<", html)

    def test_trailing_blank_lines_are_trimmed(self):
        html = mdbook_ansi.render_ansi("done\r\n\r\n\r\n")

        self.assertIn(">done<", html)

    def test_a_blank_line_in_the_middle_is_kept(self):
        html = mdbook_ansi.render_ansi("journey test passed\r\n\r\nCleaning up...\r\n")

        self.assertIn(">journey test passed\n\nCleaning up...<", html)

    def test_unknown_escape_sequences_are_dropped_not_shown(self):
        # Cursor hide/show (private mode) and an OSC window title: neither
        # has a visible rendering.
        html = mdbook_ansi.render_ansi(f"{ESC}[?25l{ESC}]0;title\x07text{ESC}[?25h\r\n")

        self.assertIn(">text<", html)

    def test_a_charset_designation_leaves_no_stray_character(self):
        # terminfo's `sgr0` for xterm-256color is `ESC ( B ESC [ m`: every
        # ncurses program in a container ends a styled run with it.
        html = mdbook_ansi.render_ansi(f"{ESC}[1mbold{ESC}(B{ESC}[m plain\r\n")

        self.assertIn('<span class="ansi-bold">bold</span> plain<', html)

    def test_a_private_csi_sequence_is_dropped(self):
        html = mdbook_ansi.render_ansi(f"{ESC}[>4;2mtext{ESC}[?1h\r\n")

        self.assertIn(">text<", html)

    def test_colon_separated_sgr_sub_parameters_are_accepted(self):
        html = mdbook_ansi.render_ansi(f"{ESC}[38:5:196mred{ESC}[0m\r\n")

        self.assertIn('<span style="color:#ff0000">red</span>', html)

    def test_cursor_addressing_is_relative_to_the_viewport_not_the_transcript(self):
        # 30 lines, then `clear` (ED 2 + CUP 1;1) and a line. The cursor is
        # on row 31, so the viewport is rows 8-31: a terminal keeps lines
        # 1-7 scrolled off above, blanks the visible 24, and writes at the
        # viewport's top — never over line 1.
        lines = "".join(f"line {n}\r\n" for n in range(1, 31))
        html = mdbook_ansi.render_ansi(f"{lines}{ESC}[2J{ESC}[1;1Hfresh\r\n")

        self.assertIn(">line 1\n", html)
        self.assertIn("line 7\nfresh<", html)
        self.assertNotIn("line 8", html)

    def test_a_style_spanning_a_line_break_is_split_per_line(self):
        html = mdbook_ansi.render_ansi(f"{ESC}[31ma\r\nb{ESC}[0m\r\n")

        self.assertIn(
            '<span class="ansi-fg-red">a</span>\n<span class="ansi-fg-red">b</span>',
            html,
        )


class TransformMarkdownTests(unittest.TestCase):
    def test_an_ansi_fence_becomes_html_and_other_fences_are_untouched(self):
        markdown = (
            "Before.\n\n"
            "```ansi\n"
            f"{ESC}[32mok{ESC}[0m\r\n"
            "```\n\n"
            "```console\n"
            "$ echo unchanged\n"
            "```\n"
        )

        result = mdbook_ansi.transform_markdown(markdown)

        self.assertIn('<pre><code class="nohighlight ansi"><span class="ansi-fg-green">ok</span></code></pre>', result)
        self.assertIn("```console\n$ echo unchanged\n```\n", result)
        self.assertNotIn("```ansi", result)

    def test_an_ansi_fence_inside_another_fence_is_left_alone(self):
        # A page *showing* the syntax (AGENTS.md-style) must not have its
        # example rendered.
        markdown = "````markdown\n```ansi\n{{#include x.ansi}}\n```\n````\n"

        self.assertEqual(mdbook_ansi.transform_markdown(markdown), markdown)

    def test_an_unexpanded_include_marker_is_an_error(self):
        # mdBook leaves the marker in place when the file it names doesn't
        # exist, and it's still there if `links` didn't run first; either
        # way the site would otherwise ship the marker text as the
        # transcript.
        markdown = "```ansi\n{{#include captures/x.ansi}}\n```\n"

        with self.assertRaisesRegex(ValueError, "doesn't exist.*links"):
            mdbook_ansi.transform_markdown(markdown)

    def test_a_fence_indented_inside_a_list_item_stays_in_the_item(self):
        # A tutorial step's transcript. The HTML block must keep the item's
        # indent, and can't span lines (an indented continuation line would
        # land inside the <pre> as text), so it goes out as one line.
        markdown = "1. Run it:\n\n   ```ansi\n   one\r\n   two\r\n   ```\n"

        result = mdbook_ansi.transform_markdown(markdown)

        self.assertEqual(
            result,
            '1. Run it:\n\n   <pre><code class="nohighlight ansi">one&#10;two</code></pre>\n',
        )

    def test_a_fence_whose_info_string_has_attributes_is_still_one_fence(self):
        # Otherwise its closer reads as an opener and swallows the ansi
        # block after it, which then ships as raw escape bytes.
        markdown = "```rust ignore\nfn f() {}\n```\n\n```ansi\nhi\n```\n"

        result = mdbook_ansi.transform_markdown(markdown)

        self.assertIn("```rust ignore\nfn f() {}\n```\n", result)
        self.assertIn('<pre><code class="nohighlight ansi">hi</code></pre>', result)

    def test_a_tilde_fence_works_too(self):
        markdown = "~~~ansi\nhi\n~~~\n"

        self.assertIn('<pre><code class="nohighlight ansi">hi</code></pre>', mdbook_ansi.transform_markdown(markdown))


class PreprocessorProtocolTests(unittest.TestCase):
    def test_supports_only_the_html_renderer(self):
        self.assertEqual(mdbook_ansi.main(["supports", "html"]), 0)
        self.assertEqual(mdbook_ansi.main(["supports", "markdown"]), 1)

    def test_rewrites_every_chapter_including_nested_ones(self):
        book = {
            "items": [
                {
                    "Chapter": {
                        "name": "Top",
                        "content": "```ansi\nx\n```\n",
                        "sub_items": [
                            {"Chapter": {"name": "Nested", "content": "```ansi\ny\n```\n", "sub_items": []}},
                            "Separator",
                        ],
                    }
                },
                {"PartTitle": "Part"},
                "Separator",
            ],
            "__non_exhaustive": None,
        }
        stdin = io.StringIO(json.dumps([{"root": "/book"}, book]))
        stdout = io.StringIO()

        with redirect_stdout(stdout):
            status = mdbook_ansi.main([], stdin=stdin)

        self.assertEqual(status, 0)
        output = json.loads(stdout.getvalue())
        self.assertIn('<pre><code class="nohighlight ansi">x</code></pre>', output["items"][0]["Chapter"]["content"])
        nested = output["items"][0]["Chapter"]["sub_items"][0]["Chapter"]["content"]
        self.assertIn('<pre><code class="nohighlight ansi">y</code></pre>', nested)
        self.assertEqual(output["items"][1], {"PartTitle": "Part"})

    def test_a_bad_block_fails_the_build_with_the_chapter_named(self):
        book = {"items": [{"Chapter": {"name": "Broken", "content": "```ansi\n{{#include x}}\n```\n", "sub_items": []}}]}
        stdin = io.StringIO(json.dumps([{}, book]))
        stderr = io.StringIO()

        with redirect_stderr(stderr), redirect_stdout(io.StringIO()):
            status = mdbook_ansi.main([], stdin=stdin)

        self.assertEqual(status, 1)
        self.assertIn("Broken", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
