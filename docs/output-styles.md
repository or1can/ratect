# Output Styles

`--output`/`-o` controls how Ratect reports its own progress on stdout — never
what the task's command itself prints, which always streams through unmodified.
Both binaries take it (`ratect` on every command, `ratect-compat` on every
invocation) and render each style with the same code
([`ui.rs`](https://github.com/or1can/ratect/blob/main/ratect-core/src/ui.rs)),
so this page is the one
description of the four styles; the [`ratect`](ratect-cli.md#global-options)
and [`ratect-compat`](ratect-compat-cli.md#output) CLI references only link
here.

Each style is shown below from the *same* real run — `ratect run journey-test`
against
[`examples/full-stack`](https://github.com/or1can/ratect/tree/main/examples/full-stack)
— so you compare styles rather than projects.

When `--output` isn't given, Ratect auto-selects: `fancy` on an interactive
console (stdout a real terminal, `TERM` set and not `dumb`, terminal size
queryable, no `--no-color`); `simple` otherwise. `quiet` and `all` are never
auto-selected.

## `fancy`

The same real recording as [the homepage](index.md), unedited — static text
can't convey an in-place repaint:

<div id="demo-player-cli"></div>
<script>
window.addEventListener('DOMContentLoaded', function () {
  AsciinemaPlayer.create('demo.cast', document.getElementById('demo-player-cli'), {
    autoPlay: false,
    cols: 80,
    rows: 24,
  });
});
</script>

What the recording can't tell you: there is no spinner — the animation is
purely rewriting changed lines, exactly like Batect — and lines are clipped to
the terminal's current width. Because it repaints, it requires an interactive
console: an explicit `-o fancy` without one fails up front with a clear error
(Batect instead accepts it and crashes on the first repaint). Works with
[`--no-color`](#colour) — the repaint stays; bold/color go — a combination
Batect rejects.

## `simple`

`ratect run journey-test -o simple`, captured on a terminal in
`examples/full-stack`:

```ansi
{{#include captures/output-styles-simple.ansi}}
```

Append-only, with no live-updating progress detail at all, so it is safe for
CI logs and redirected output. The health/setup-command milestones are shown
for *dependency* containers only: the task's own container's readiness runs
concurrently with its command (see [task
lifecycle](task-lifecycle.md#known-limitations)), so
printing them would drop a line into the middle of that command's own output
— [`all`](#all) shows them. A readiness *failure* is still reported, on stderr,
in every style.

## `quiet`

`ratect run journey-test -o quiet`, captured the same way:

```ansi
{{#include captures/output-styles-quiet.ansi}}
```

Stdout is exactly the containers' own output, so it's safe to pipe; error
reporting stays on stderr, unchanged. Nothing from `app`/`db`/`cache` — a
dependency's own stdout is never shown outside `all`, whatever the style.

`quiet` also switches a task listing (`ratect tasks list`, `ratect-compat
--list-tasks`) to a machine-readable format: one task per line, sorted by
name, as `name` alone or `name<TAB>description` — no header, no
[grouping](ratect-compat-config-reference.md#list-tasks-output). The `ratect`
commands with a listing of their own (`caches`, `includes`, `resources`,
`doctor`) each have a `quiet` form of the same kind, described with the
command in the [`ratect` CLI reference](ratect-cli.md#commands).

## `all`

`ratect run journey-test -o all`, captured the same way — long, because every
line `db`, `cache` and `app` wrote during the run is here too:

```ansi
{{#include captures/output-styles-all.ansi}}
```

The only style that shows *dependency* containers' stdout/stderr,
setup-command output, and full image-build output (`Image build | ...`) —
everything the other styles discard. In exchange, no container is interactive
in this mode: the task container gets no TTY and no stdin, and every container
gets `TERM=dumb` (matching Batect — a full-screen program can't render into
line-prefixed output). That is why, next to `simple`'s capture, npm draws no
spinner here.

Nothing in `examples/` builds from a `Dockerfile` (every container uses a
prebuilt `image`), so `Image build | ...` output has no real capture to
point at here.

## Colour

Colour in Ratect's own output (the exit code in the summary line, `all`'s
per-container prefixes) is, by default, emitted only when stdout is a terminal
— piped or redirected output gets plain text — and a task command's own output
is never touched either way. Three controls adjust that, and their precedence is fixed:

| Control | Effect |
|---|---|
| `--no-color` | Disables colour in Ratect's own output. Since colour is already skipped when stdout isn't a terminal, this only matters on an interactive console — unless `CLICOLOR_FORCE` is also set, which it overrides. Also makes `simple`, not `fancy`, the auto-selected style; an explicit `-o fancy --no-color` still gets a colourless `fancy`. |
| `NO_COLOR` | If set — to anything, even empty, per [no-color.org](https://no-color.org) — exactly the same effect as `--no-color`. |
| `CLICOLOR_FORCE` | If set to anything other than `0`, forces colour even when stdout isn't a terminal (a CI log viewer that renders ANSI despite the pipe). Never affects which style is auto-selected. |

`--no-color`/`NO_COLOR` always win: if either is set, `CLICOLOR_FORCE` is
ignored.
