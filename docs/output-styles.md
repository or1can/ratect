# Output Styles

`--output`/`-o` controls how much Ratect says about its own progress — never
what the task's command itself prints, which always streams through unmodified,
and never *where* anything goes. Both binaries take it (`ratect` on every
command, `ratect-compat` on every invocation) and render each style with the
same code
([`ui.rs`](https://github.com/or1can/ratect/blob/main/ratect-core/src/ui.rs)),
so this page is the one
description of the four styles; the [`ratect`](ratect-cli.md#global-options)
and [`ratect-compat`](ratect-compat-cli.md#output) CLI references only link
here.

## Streams

Stdout is the result; stderr is everything said about producing it
([decisions/0017](https://github.com/or1can/ratect/blob/main/decisions/0017-output-streams-and-styles.md)).
For a run, the result is the task container's own output. Everything else —
progress, the summary line, notices, warnings and errors — is about the run,
and `-o` shapes how much of it is said, not which stream it is said on.

The two binaries differ in one thing: where their **progress** goes.

- **`ratect`** writes its progress — the milestones, the summary line, and
  under `all` the prefixed lines of every container but the task's own — to
  **stderr**. Its stdout is the task container's output and nothing else, in
  every style, so `ratect run test > out.txt` captures the result alone and
  `2>/dev/null` silences the conversation without touching it.
- **`ratect-compat`** writes its progress to **stdout**, where Batect does,
  because a Batect project's scripts may read it. Under `simple` those lines
  share stdout with the task's own output, unmarked, exactly as under Batect;
  `-o quiet` is the escape for a script that needs stdout to be the
  container's output alone, and `-o all` prefixes every line.

Three kinds of line reach stderr in **every** style, `quiet` included, from
both binaries, whatever `RUST_LOG` is set to: an error (`Error: ` in bold red,
then the message), a warning (`Warning: ` in bold yellow — a deprecated flag,
a proxy bound to loopback that containers can't reach, a credential helper
that failed), and a notice (unmarked: the interrupt notice with its "press
Ctrl+C again" instructions, the `Cleanup complete.` that follows it once
cleanup is over, and what `--no-cleanup*` left in place and how to remove it).
Only the fourth kind, progress, is shaped by `-o`.
`RUST_LOG` filters Ratect's internal diagnostics
([Filtering `RUST_LOG`](troubleshooting.md#filtering-rust_log)), which are a
separate stream of breadcrumbs on stderr; nothing a user needs to act on is
behind it.

A task's own non-zero exit is reported by the exit code and, outside `quiet`,
by the summary line — never by an `Error:` line; a run ended by a signal is
reported by the interrupt notice and its exit code, and ends with `Cleanup
complete.` (or the error saying what cleanup left behind) rather than a
trailing `Error: Interrupted`. `Error:` is reserved for Ratect's own failures: it could
not run the task, or could not clean up after it. A failure to run a task that
has started — an image that won't pull, a dependency that never becomes
healthy — is reported when it happens, before any cleanup line, and outside
`quiet` the run then ends with the summary line's other form, `<task> failed
after <duration>.`, where `finished with exit code N` would have gone. So
does a run whose command exited `0` but whose cleanup then failed, after the
error saying what was left behind; a command that exited non-zero keeps its
`finished with exit code N` line whatever cleanup does. A
failure before any task starts, such as a config that doesn't load, is printed
as the run exits, with no summary line, as is a prerequisite's failure for the
task waiting on it: the prerequisite's own line is the one printed.

In every style, and in every command's own output, a control character in a
name or value from your configuration (a task, container, image or cache
name, a command, a Git include's `repo` or `ref`), or in image build output,
is printed escaped: `\u{1b}` for ESC, `\r` for a carriage return. A Git
bundle or a config file can't send your terminal escape sequences that way. A
name's line breaks and tabs are escaped too (`\n`, `\t`), except inside an
error message, which keeps both. A command and image build output
keep theirs. A container's own output is printed as it was written.

Each style is shown below from the *same* real run — `ratect run journey-test`
against
[`examples/full-stack`](https://github.com/or1can/ratect/tree/main/examples/full-stack)
— so you compare styles rather than projects.

When `--output` isn't given, Ratect auto-selects: `fancy` on an interactive
console (the stream its progress goes to — stderr for `ratect`, stdout for
`ratect-compat` — is a real terminal, `TERM` is set and not `dumb`, the
terminal size is queryable, and there is no `--no-color`); `simple`
otherwise. `quiet` and `all` are never auto-selected. So `ratect run test >
out.txt` on a terminal still gets `fancy` — the pipe took the result, not the
conversation — where `ratect-compat test > out.txt` gets `simple`.

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
purely rewriting the block's lines in place, as Batect's is — and lines are
clipped to the terminal's current width. After that width changes, the block is
drawn afresh below the old one rather than over it, since the terminal has
re-wrapped the old lines. Because it repaints, it needs a terminal: an explicit
`-o fancy` asks less than auto-selection does — `TERM` may be unset, but not
`dumb` — and fails up front, with an error naming the reason, when the stream it
checked isn't a terminal, `TERM` is `dumb`, or the terminal size can't be
determined (Batect instead accepts it, then crashes on the first repaint when
stdout isn't a terminal, or garbles a `TERM=dumb` terminal with cursor movements
it can't perform). Works with
[`--no-color`](#colour) — the repaint stays; bold/color go — a combination
Batect rejects.

## `simple`

`ratect run journey-test -o simple`, captured on a terminal in
`examples/full-stack`:

```ansi
{{#include captures/output-styles-simple.ansi}}
```

Append-only, with no live-updating progress detail at all, so it is safe for
CI logs and redirected output. A dependency that exits on its own while the
task runs is reported as a milestone (`db exited unexpectedly with exit code
137.`), not a warning — the run's failure, if one follows, is what explains
it. The health/setup-command milestones are shown
for *dependency* containers only. Under `ratect` the task's own container has
no readiness gate to report (see [Dependency
Readiness](dependency-readiness.md#the-tasks-own-container)); under
`ratect-compat` its readiness runs concurrently with its command (see [task
lifecycle](task-lifecycle.md#known-limitations)), so
printing them would drop a line into the middle of that command's own output
— [`all`](#all) shows them. A readiness *failure* is still reported, on stderr,
in every style.

Cleanup opens with `Cleaning up...`, or, when it follows a failure, names it —
`Cleaning up after journey-test exited with code 3...`, or, when the task
couldn't run, the step and container that failed: `Cleaning up after db did not
become healthy...`, `Cleaning up after db's setup command 1 exited with code
3...`, `Cleaning up after cache's image failed to pull...`. The `Error:` above
it says why. `all` opens it the same way.

## `quiet`

`ratect run journey-test -o quiet`, captured the same way:

```ansi
{{#include captures/output-styles-quiet.ansi}}
```

No progress at all: stdout is exactly the task container's own output (under
`ratect` it is in every style; `quiet` is where `ratect-compat` matches it),
and stderr carries only [errors, warnings and notices](#streams). Nothing
from `app`/`db`/`cache` — a dependency's own stdout is never shown outside
`all`, whatever the style.

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
everything the other styles discard. In exchange, no container is
[interactive](interactive-mode.md) in this mode: the task container gets no
TTY and no stdin, and every container gets `TERM=dumb` (matching Batect — a
full-screen program can't render into line-prefixed output). That is why, next to `simple`'s capture, npm draws no
spinner here.

Nothing in `examples/` builds from a `Dockerfile` (every container uses a
prebuilt `image`), so `Image build | ...` output has no real capture to
point at here.

## Colour

Colour in Ratect's own output (the exit code in the summary line, `all`'s
per-container prefixes, the `Error:`/`Warning:` markers, and the bold that
picks out task, container and image names and commands in `simple`
and `fancy` progress) is, by default,
emitted only when the stream being written to is a terminal — each stream
decides for itself, so with `ratect run test > out.txt` the progress on stderr
is still coloured while a `2>err.txt` gets plain text, `RUST_LOG` lines
included — and a task command's own output is never touched either way. No
message depends on its colour to be read: colour only distinguishes one
container's prefix from another's or emphasises a marker word, a name or the
exit code, and Ratect never paints text in the terminal's own white or black,
which some themes render invisible. Three controls adjust that, and their
precedence is fixed:

| Control | Effect |
|---|---|
| `--no-color` | Disables colour in Ratect's own output, on both streams. Since colour is already skipped when a stream isn't a terminal, this only matters on an interactive console — unless `CLICOLOR_FORCE` is also set, which it overrides. Also makes `simple`, not `fancy`, the auto-selected style; an explicit `-o fancy --no-color` still gets a colourless `fancy`. |
| `NO_COLOR` | If set — to anything, even empty, per [no-color.org](https://no-color.org) — exactly the same effect as `--no-color`. |
| `CLICOLOR_FORCE` | If set to anything other than `0`, forces colour even when a stream isn't a terminal (a CI log viewer that renders ANSI despite the pipe). Never affects which style is auto-selected. |

`--no-color`/`NO_COLOR` always win: if either is set, `CLICOLOR_FORCE` is
ignored.
