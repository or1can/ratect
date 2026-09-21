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

"""mdBook preprocessor: renders a ```ansi fenced block as coloured HTML.

Wired up in `book.toml` as `[preprocessor.ansi]`, and never run by hand.
In a page:

    ```ansi
    {{#include captures/output-styles-simple.ansi}}
    ```

mdBook's own `links` preprocessor expands the `{{#include}}` first (hence
`after = ["links"]` in `book.toml`), so by the time this runs the fence
holds the raw bytes `tools/capture-output.py` wrote: what the binary sent
to a real terminal, ANSI escapes and all. This turns that into

    <pre><code class="nohighlight ansi">...<span class="ansi-fg-green">0</span>...</code></pre>

The class names are what `tools/mdbook-ansi.css` styles, with one palette
for mdBook's light-background themes and one for its dark ones, so the
green of a `finished with exit code 0` reads on both.

# Why a small terminal emulator, not a regex over SGR codes

A capture is what the terminal *received*, not what it *showed*. A
program with a TTY draws progress in place — npm's spinner is `\\x1b[1G`
(cursor to column 1), `\\x1b[0K` (erase to end of line), one frame
character, repeated — and a pty turns every `\\n` into `\\r\\n`. Converting
only the colour codes would leave every spinner frame standing in a row.
So this keeps a grid of styled cells and a cursor, and applies the cursor
and erase controls a static transcript actually contains (`\\r`, `\\b`,
CHA/CUP/CUU/CUD/CUF/CUB, EL, ED), rendering the grid's final state. A
block that needs more than that — anything whose output changes over time
rather than settling — is the animated case, and belongs in an asciinema
recording, not here (AGENTS.md's captured-output rules).

# What the HTML has to satisfy

- `nohighlight` keeps mdBook's bundled highlight.js from re-highlighting
  the block: without a `language-*` class it looks for exactly that class
  and returns before touching `innerHTML`, which would otherwise flatten
  the spans back to text. mdBook's `book.js` then adds `hljs` itself, which
  is what gives a code block its theme background and padding, and its
  copy button (which copies `textContent` — the plain transcript).
- The 16 named colours are classes, not inline styles, so the theme can
  choose them. 256-colour and truecolour codes have no theme-independent
  meaning, so those are inline `style` attributes as-is; the binary never
  emits them, only a container's own output can.
"""

import json
import re
import sys
from dataclasses import dataclass, replace
from html import escape

COLOUR_NAMES = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"]

# The classes a 16-colour index maps to; anything else is a `#rrggbb`.
FG_CLASS = {i: f"ansi-fg-{name}" for i, name in enumerate(COLOUR_NAMES)}
FG_CLASS.update({i + 8: f"ansi-fg-bright-{name}" for i, name in enumerate(COLOUR_NAMES)})
BG_CLASS = {i: f"ansi-bg-{name}" for i, name in enumerate(COLOUR_NAMES)}
BG_CLASS.update({i + 8: f"ansi-bg-bright-{name}" for i, name in enumerate(COLOUR_NAMES)})

# A CSI sequence: ESC [ parameters (a leading `<=>?` marks a private one),
# optional intermediates, one final byte.
CSI_RE = re.compile(r"\x1b\[([0-9;:<=>?]*)([ -/]*)([@-~])")
# An OSC sequence (window title and the like), ended by BEL or ESC \.
OSC_RE = re.compile(r"\x1b\].*?(?:\x07|\x1b\\)", re.DOTALL)
# Any other escape: a charset designation (`ESC ( B`, which terminfo's
# `sgr0` for xterm-256color starts with — so every ncurses program emits
# it) takes one argument byte; the rest (`ESC 7`, `ESC M`, ...) take none.
OTHER_ESC_RE = re.compile(r"\x1b(?:[()*+\-./].|.)", re.DOTALL)

# The rows a terminal shows at once — `tools/capture-output.py`'s pty height.
# Only cursor addressing and screen clearing need it: both are relative to
# the viewport, which for a capture longer than the screen is its tail.
VIEWPORT_ROWS = 24

FENCE_OPEN_RE = re.compile(r"^([ \t]*)(`{3,}|~{3,})[ \t]*(\S*)[ \t]*$")


@dataclass(frozen=True)
class Style:
    bold: bool = False
    dim: bool = False
    italic: bool = False
    underline: bool = False
    # An int 0-15 (a themed class) or a `#rrggbb` string (an inline style).
    fg: object = None
    bg: object = None

    def span_open(self):
        classes = []
        styles = []
        if self.bold:
            classes.append("ansi-bold")
        if self.dim:
            classes.append("ansi-dim")
        if self.italic:
            classes.append("ansi-italic")
        if self.underline:
            classes.append("ansi-underline")
        for value, class_table, property_name in (
            (self.fg, FG_CLASS, "color"),
            (self.bg, BG_CLASS, "background-color"),
        ):
            if isinstance(value, int):
                classes.append(class_table[value])
            elif value is not None:
                styles.append(f"{property_name}:{value}")
        attributes = ""
        if classes:
            attributes += f' class="{" ".join(classes)}"'
        if styles:
            attributes += f' style="{";".join(styles)}"'
        return f"<span{attributes}>"


DEFAULT = Style()


def colour_256(index):
    """The `#rrggbb` of an xterm 256-colour index at or above 16 (the 6x6x6
    cube, then the 24-step grey ramp)."""
    if index < 232:
        index -= 16
        steps = (index // 36, (index // 6) % 6, index % 6)
        return "#" + "".join(f"{0 if s == 0 else 55 + 40 * s:02x}" for s in steps)
    grey = 8 + 10 * (index - 232)
    return f"#{grey:02x}{grey:02x}{grey:02x}"


def apply_sgr(style, params):
    """`style` after one SGR sequence's parameters (already split on `;`)."""
    i = 0
    while i < len(params):
        code = params[i]
        if code == 0:
            style = DEFAULT
        elif code == 1:
            style = replace(style, bold=True)
        elif code == 2:
            style = replace(style, dim=True)
        elif code == 3:
            style = replace(style, italic=True)
        elif code == 4:
            style = replace(style, underline=True)
        elif code == 22:
            style = replace(style, bold=False, dim=False)
        elif code == 23:
            style = replace(style, italic=False)
        elif code == 24:
            style = replace(style, underline=False)
        elif 30 <= code <= 37:
            style = replace(style, fg=code - 30)
        elif 90 <= code <= 97:
            style = replace(style, fg=code - 90 + 8)
        elif code == 39:
            style = replace(style, fg=None)
        elif 40 <= code <= 47:
            style = replace(style, bg=code - 40)
        elif 100 <= code <= 107:
            style = replace(style, bg=code - 100 + 8)
        elif code == 49:
            style = replace(style, bg=None)
        elif code in (38, 48):
            field = "fg" if code == 38 else "bg"
            if i + 2 < len(params) and params[i + 1] == 5:
                index = params[i + 2]
                value = index if index < 16 else colour_256(index)
                style = replace(style, **{field: value})
                i += 2
            elif i + 4 < len(params) and params[i + 1] == 2:
                r, g, b = params[i + 2 : i + 5]
                style = replace(style, **{field: f"#{r:02x}{g:02x}{b:02x}"})
                i += 4
        i += 1
    return style


class Screen:
    """A grid of `(char, Style)` cells and a cursor — just enough terminal
    to settle the in-place drawing a static capture contains."""

    def __init__(self):
        self.rows = [[]]
        self.row = 0
        self.col = 0
        self.style = DEFAULT

    def _line(self):
        while len(self.rows) <= self.row:
            self.rows.append([])
        return self.rows[self.row]

    def put(self, char):
        line = self._line()
        while len(line) < self.col:
            line.append((" ", DEFAULT))
        if self.col < len(line):
            line[self.col] = (char, self.style)
        else:
            line.append((char, self.style))
        self.col += 1

    def feed(self, text):
        i = 0
        while i < len(text):
            char = text[i]
            if char == "\x1b":
                match = CSI_RE.match(text, i) or OSC_RE.match(text, i) or OTHER_ESC_RE.match(text, i)
                if match and match.re is CSI_RE:
                    self.csi(match.group(1), match.group(3))
                i = match.end() if match else i + 1
                continue
            i += 1
            if char == "\r":
                self.col = 0
            elif char == "\n":
                # A pty sends `\r\n`, so `\r` has already done the column;
                # resetting it here too just means a capture made without a
                # pty still renders line by line rather than as a staircase.
                self.row += 1
                self.col = 0
                self._line()
            elif char == "\b":
                self.col = max(0, self.col - 1)
            elif char == "\t":
                self.col += 8 - self.col % 8
            elif char >= " ":
                self.put(char)

    def _top(self):
        """The transcript row at the top of the viewport."""
        return max(0, len(self.rows) - VIEWPORT_ROWS)

    def csi(self, raw_params, final):
        if raw_params[:1] in ("<", "=", ">", "?"):
            return  # A private sequence (cursor visibility etc.): nothing to draw.
        # `38:5:196` (colon sub-parameters) means the same as `38;5;196`.
        params = [int(p) if p else 0 for p in raw_params.replace(":", ";").split(";")] if raw_params else []
        n = params[0] if params and params[0] else 1
        if final == "m":
            self.style = apply_sgr(self.style, params or [0])
        elif final == "G":
            self.col = n - 1
        elif final == "A":
            self.row = max(self._top(), self.row - n)
        elif final == "B":
            self.row += n
        elif final == "C":
            self.col += n
        elif final == "D":
            self.col = max(0, self.col - n)
        elif final == "E":
            self.row, self.col = self.row + n, 0
        elif final == "F":
            self.row, self.col = max(self._top(), self.row - n), 0
        elif final in "Hf":
            self.row = self._top() + n - 1
            self.col = (params[1] if len(params) > 1 and params[1] else 1) - 1
        elif final == "K":
            mode = params[0] if params else 0
            line = self._line()
            if mode == 0:
                del line[self.col :]
            elif mode == 1:
                for c in range(min(self.col + 1, len(line))):
                    line[c] = (" ", DEFAULT)
            elif mode == 2:
                line.clear()
        elif final == "J":
            mode = params[0] if params else 0
            if mode == 0:
                del self._line()[self.col :]
                del self.rows[self.row + 1 :]
            elif mode in (2, 3):
                for row in self.rows[self._top() :]:
                    row.clear()

    def html(self):
        rows = list(self.rows)
        while rows and not any(char != " " or style != DEFAULT for char, style in rows[-1]):
            rows.pop()
        return "\n".join(self._row_html(row) for row in rows)

    @staticmethod
    def _row_html(row):
        # Trailing unstyled spaces are invisible on a terminal; drop them so
        # the copy button doesn't copy them either.
        while row and row[-1] == (" ", DEFAULT):
            row.pop()
        out = []
        current = DEFAULT
        for char, style in row:
            if style != current:
                if current != DEFAULT:
                    out.append("</span>")
                if style != DEFAULT:
                    out.append(style.span_open())
                current = style
            out.append(escape(char, quote=False))
        if current != DEFAULT:
            out.append("</span>")
        return "".join(out)


def render_ansi(text):
    """The HTML for one captured transcript."""
    screen = Screen()
    screen.feed(text)
    return f'<pre><code class="nohighlight ansi">{screen.html()}</code></pre>'


def transform_markdown(markdown):
    """`markdown` with every ```ansi fence (bar one shown inside another
    fence) replaced by its HTML."""
    out = []
    lines = markdown.split("\n")
    i = 0
    while i < len(lines):
        match = FENCE_OPEN_RE.match(lines[i])
        if not match:
            out.append(lines[i])
            i += 1
            continue
        indent, fence, info = match.groups()
        close_re = re.compile(rf"^[ \t]*{re.escape(fence[0])}{{{len(fence)},}}[ \t]*$")
        end = i + 1
        while end < len(lines) and not close_re.match(lines[end]):
            end += 1
        if info == "ansi":
            # As CommonMark reads a fence: content is relative to its indent.
            body = "\n".join(line.removeprefix(indent) for line in lines[i + 1 : end])
            if "{{#include" in body:
                raise ValueError(
                    "an ```ansi block still contains an unexpanded {{#include}} — "
                    "either the capture file it names doesn't exist (mdBook leaves the "
                    "marker in place), or the `links` preprocessor didn't run first "
                    '(book.toml: after = ["links"])'
                )
            html = render_ansi(body)
            if indent:
                # Inside a list item: the HTML block has to stay indented to
                # stay in the item, and an indented line inside <pre> would
                # be part of the text — so the block goes out as one line.
                html = indent + html.replace("\n", "&#10;")
            out.append(html)
        else:
            out.extend(lines[i : end + 1])
        i = end + 1
    return "\n".join(out)


def transform_items(items, errors):
    for item in items:
        if not isinstance(item, dict) or "Chapter" not in item:
            continue
        chapter = item["Chapter"]
        try:
            chapter["content"] = transform_markdown(chapter["content"])
        except ValueError as error:
            errors.append(f"{chapter.get('name', '?')}: {error}")
        transform_items(chapter.get("sub_items", []), errors)


def main(argv=None, stdin=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ["supports"]:
        return 0 if argv[1:] == ["html"] else 1
    book = json.load(stdin or sys.stdin)[1]  # [context, book]
    errors = []
    # `items` since mdBook 0.5 (what CI installs); 0.4 called it `sections`.
    transform_items(book.get("items", book.get("sections", [])), errors)
    for error in errors:
        print(f"mdbook-ansi: {error}", file=sys.stderr)
    if errors:
        return 1
    json.dump(book, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
