# 0017 — Output: what each stream is for, and how a style shapes it

**Status:** Accepted — planned (ratect-compat 0.33.0 · ratect 0.12.0,
[#535](https://github.com/or1can/ratect/issues/535)). Refined by
[#473](https://github.com/or1can/ratect/issues/473): a task that fails to run
once started ends with a summary line of its own, so that route is listed below.

## Context

Ratect's output grew one event at a time, ported from Batect. Progress lines
(`Running test...`, `test finished with exit code 0 in 1.2s`) go to stdout,
shaped by `-o`; the task container's own output goes to stdout too; failures
go to stderr as an `Error:` line; and a third set of lines — the interrupt
notice with its "press Ctrl+C again" instructions, the `--no-cleanup` reminder,
a cleanup that left something behind, a deprecated flag — goes through
`tracing`, which `RUST_LOG=off` silences and which colours its text whether or
not stderr is a terminal. `-o all` cycles container prefixes through palette
colours and paints task-level lines in the palette's white, as Batect does;
`simple`'s own lines share stdout with the task's output, unmarked.

The parity sweep ([#284](https://github.com/or1can/ratect/issues/284)) filed
each difference from Batect's output as its own issue, and triage kept finding
that no single one could be decided alone: whether the interrupt notice should
be logged or printed ([#369](https://github.com/or1can/ratect/issues/369)),
which colours `all` may use ([#489](https://github.com/or1can/ratect/issues/489)),
what cleanup reports ([#488](https://github.com/or1can/ratect/issues/488)),
whether stderr is coloured at all ([#479](https://github.com/or1can/ratect/issues/479)),
and whether Batect's bold and white are policy or parity
([#492](https://github.com/or1can/ratect/issues/492),
[#493](https://github.com/or1can/ratect/issues/493),
[#506](https://github.com/or1can/ratect/issues/506),
[#507](https://github.com/or1can/ratect/issues/507)). Each asks the same
three questions — which stream, printed or logged, and how marked — and
parity answers none of them: Batect matching is not the same as good output,
and Batect's own choices are inconsistent (its `all` sends errors to stdout;
its white text is invisible on a light theme).

A survey of comparable tools (`docker compose`, BuildKit, `podman`, `earthly`,
`dagger`, `cargo`, `just`, `make`, `tilt`, `kubectl`, `gh`, `npm`, `go`,
`act`, `nix`; primary sources, October 2026) found one dominant convention:
the tool's own status goes to stderr and the wrapped command's output to
stdout, with every quiet mode converging on "stdout carries only the payload".
The exceptions are the older tools (`make`, legacy `docker build`, which
BuildKit moved) and Batect. No surveyed tool but `act` and Batect uses the
terminal's plain white or black as a foreground; `gh` is the only one with a
documented light-theme stance. Almost none gates text a user must act on
behind a verbosity knob — `podman`'s hidden shutdown notice is the one
counter-example, and the same defect as [#369](https://github.com/or1can/ratect/issues/369).

## Decision

### Stdout is the result; stderr is everything said about producing it

This is Unix's own rule for programs meant to be connected to other programs,
and every later decision here follows from it.

- For a run, the **result** is the task container's own output — and nothing
  else. A dependency's output is never the result (which is why only `all`
  shows it); nor is Ratect's progress, its summary line, a notice, a warning
  or an error. All of those are *about* the run, and go to stderr.
- For every other verb, the result is what the verb produces — the listing
  of `tasks list`, `caches list`, `resources list`, `doctor`'s report, a
  conversion — on stdout, with its errors on stderr.
- `-o` shapes **how much** of the conversation is said, never **where**.
  `quiet` means "no progress": stderr carries only errors, warnings and
  notices. It does not mean "keep stdout clean", because stdout is always
  clean.

So `fancy`'s repaint block, `simple`'s milestone lines and `all`'s prefixed
lines are stderr. A user who redirects `2>/dev/null` has opted out of the
conversation, not out of the result; a `ratect` user who pipes stdout gets
the container's output and nothing else, in every style (`ratect-compat`'s
stdout keeps more — next).

**`ratect-compat` keeps Batect's stream placement where a script could
depend on it**, under [0013](0013-when-ratect-compat-copies-batect.md)'s rule 1
(an outcome that differs is a gap, not a divergence). This is a closed list
of two, not to be widened — and being parity kept, not parity dropped, it
earns no row on
[`docs/differences-from-batect.md`](../docs/differences-from-batect.md):

1. Progress, milestones and the summary line go to stdout.
2. Under `all`, dependency containers' output, setup-command output and image
   build output go to stdout.

Errors, warnings and notices are stderr in both binaries, and unprefixed
under `all` — [#367](https://github.com/or1can/ratect/issues/367)'s
decision, already recorded on the differences page (its runtime-behaviour
bullet on failure messages), which stays.

The two binaries differ only in which file descriptor each `main` hands the
**progress console** — the stream Ratect's own progress is written to: stderr
for `ratect`, stdout for `ratect-compat`. The loggers in `ui.rs` write to that
console and never choose a stream themselves; no dialect gate lives inside
them.

### Each stream decides for itself whether it is a terminal

Colour, and `fancy`'s auto-selection, are decided by probing the stream being
written to, not stdout regardless. Colour on stderr is enabled when stderr is
a terminal (and `--no-color`/`NO_COLOR`/`CLICOLOR_FORCE` apply to it exactly
as to stdout); `fancy`'s interactivity, and the refusal when `-o fancy` is
asked for without it, are the progress console's — stderr's for `ratect`,
stdout's for `ratect-compat` — and the refusal names that stream.
Consequently `ratect run x > out.txt` on a terminal gets `fancy`, as `docker
compose` does: the pipe took the result, not the conversation.

The `tracing` subscriber is one more writer to stderr and follows the same
decision — it colours only when stderr is a terminal and no opt-out is set.

### Printed, or logged

One test decides it for every message: **is a user with `RUST_LOG` unset
worse off without this line?** If yes, it is output — printed, on its stream,
in every style that shows its class — and never routed through `tracing`.
`tracing` is for breadcrumbs (Docker API calls, what was resolved to what) and
for failures of no consequence to the run's outcome (a best-effort retry
that succeeded on the second try).

Under that test the interrupt notice and its "press Ctrl+C again"
instructions, a cleanup that left something behind, the `--no-cleanup*`
reminder, a deprecated flag's warning and the loopback-bound proxy warning
are all printed. The reminder is also rewritten on the way: today's line
names nothing it kept and gives no removal command, and the cleanup section
below says what it must say. An event is never both printed and logged above
`debug`: a printed message may have a `debug!` breadcrumb beside it, never a
`warn!` twin, or a `RUST_LOG=warn` user reads it twice.

### Stderr carries the same classes in every style

Four classes of line reach stderr. Three of them — errors, warnings and
notices — appear in every style, `quiet` included; only the fourth, progress,
is shaped by `-o`.

| Class | Marker | What it is |
| --- | --- | --- |
| Error | `Error: ` in bold red, message in the default colour; unprefixed under `all` ([#367](https://github.com/or1can/ratect/issues/367)) | Ratect failed to do what was asked: for a run, to run the task or to clean up after it ([0016](0016-exit-codes.md)'s rule 3 and rule 1's cleanup failure, exit `125`); for any other verb, that verb's own failure (exit `1`) or an invocation error (exit `2`). |
| Warning | `Warning: ` in bold yellow, message in the default colour | Something the user should change, though the run proceeds: a deprecated flag, a proxy bound to loopback. |
| Notice | none | Something about the run the user reads but need not act on: the interrupt notice, a completed cleanup after an interrupt, what `--no-cleanup*` left in place. |
| Progress | none | Milestones, status, the summary line; the only class `-o` shapes. |

A marker says "act on this"; a notice is read, not acted on, so it carries
none. By the same rule, the `Warning: <container> exited unexpectedly with
exit code N` line that `simple`, `fancy` and `all` each print today is a
milestone wearing a marker: a dependency exiting is progress — the event
the run's failure, if one follows, will explain — so it loses the
`Warning:` and is worded like any other milestone, on the progress console.

### Each class reports an event at most once, and which classes do is fixed

An event may reach the user by more than one route — a notice, a summary
line, the exit code — but no class reports it twice, and which classes report
it is fixed by the event, not by the style:

- A task's own non-zero exit is reported by the exit code, and in the
  non-quiet styles by the summary line. It is never an `Error:` line
  ([#454](https://github.com/or1can/ratect/issues/454)).
- A task that fails to run once it has started — an image that won't pull, a
  dependency that never becomes ready — is reported by the `Error:` line when
  it happens, and in the non-quiet styles by the summary line's other form,
  `<task> failed after <duration>.`, which says the run ended and never why
  ([#473](https://github.com/or1can/ratect/issues/473)).
- A termination signal is reported by the notice when it arrives, the cleanup
  outcome when cleanup ends, and the exit code. It is never a trailing `Error:
  Interrupted`.
- `Error:` is reserved for Ratect's own failures — for a run, the ones 0016
  gives `125` (it could not run the task, or could not clean up after it);
  for any other verb, the verb's failure or an invocation error (0016's `1`
  and `2`).

### The interrupt sequence

In every style, on stderr, printed:

1. On the signal: what ended the run, then the next step — today's
   `Interrupted; cleaning up. Press Ctrl+C again to force-remove what's left,
   and Ctrl+C after that to stop cleaning up.`, keeping the per-signal
   wording the code already chose (`TerminationSignal::ended_run` and
   `send_again`: `SIGINT` stays the bare `Interrupted`, since naming the
   signal would explain a keystroke to the person who pressed it; the
   escalate rung names the same signal again, `Send SIGTERM again`, while
   the abandon rung is `Ctrl+C` for every signal because only a keyboard
   sends an interrupt twice — the cleanup ladder's own rule). What changes
   is the route, not the words: printed, not logged.
2. On a further signal during cleanup: today's `… during cleanup;
   force-removing what's left. Press Ctrl+C to stop cleaning up.`, likewise.
3. When cleanup ends: `Cleanup complete.`, or the cleanup-failure error
   below if anything was left. This line is the interrupt sequence's own terminator —
   an interrupted run has no summary line to end on — and is not printed
   after an uninterrupted cleanup (next section).

The exit code is [0016](0016-exit-codes.md)'s and unchanged by this record.

### What cleanup says

- `quiet`: nothing on success.
- `simple`: `Cleaning up...` once, and nothing more on success — the summary
  line follows, and "complete" is the absence of a cleanup-failure error.
- The opening cleanup line names its trigger whenever that isn't the task
  simply finishing: a failure (`Cleaning up after <what failed>...`) or a
  signal (the interrupt notice is that line, and already says).
- After an *interrupted* run, every style, `quiet` included, ends with the
  interrupt sequence's `Cleanup complete.` (or the cleanup-failure error),
  since no summary line follows.
- `fancy`: its live countdown line, as today.
- `all`: each step on the prefix it belongs to — `<container> | Container
  stopped.`, `<container> | Container removed.`, and `Removing task
  network...`/`Task network removed.` on the task's prefix — because these
  lines matter most when cleanup is slow or fails, and per-step detail is
  `all`'s whole purpose.

In every style, a cleanup that leaves something behind is a cleanup failure
([0016](0016-exit-codes.md)'s term, and `CONTEXT.md`'s), so it is reported
as the error it is: `Error: Cleanup abandoned; left behind: <container>,
<network>. Remove with: <the real command>` when the ladder's abandon rung
was reached, or `Error: Cleanup failed: <what the daemon said>; left behind:
… Remove with: …` when a removal was refused — where the command is the one
that works from that binary (`ratect resources clean` where it exists;
`docker rm -f`/`docker network rm` with the names otherwise). The exit code
is 0016's. A `--no-cleanup*` flag is not a failure, so its line is a notice,
with the same tail and a reason in place of the marker: `Left in place
(--no-cleanup): <container>, <network>. Remove with: …` — "in place", not
"running", since a container created but never started is kept too. One
tail shape, so the eye learns it once.

Amended by ratect#308: the `docker rm` in either tail is `docker rm -f -v`,
removing anonymous volumes as Ratect's own cleanup does, and the
`--no-cleanup*` notice is followed by one line per kept container, in name
order, with Batect's investigation hints — `Investigate <container> with:
docker logs <id>, or docker exec -it <id> <command>`, the `exec` preceded by
`docker start <id>; ` for a container that isn't running. Lines of their own
after the notice rather than inside it, so the notice keeps the one tail
shape the failure error has.

### Colour never carries information alone

Every message reads the same with its colour stripped. Colour only ever
*distinguishes* (one container's prefix from another's) or *emphasises* (the
marker words above, bold on the user's own names). A red exit code is also the
words `exit code 42`; `fancy` never has a state that only a colour conveys.

### No foreground is ever the palette's white or black

The palette's white (SGR 37) and black (SGR 30), bright or not, are the
theme's own foreground and background: Batect's white status lines are light
grey on a dark theme and near-invisible on Solarized Light or macOS Terminal's
Basic profile. Ratect never sets either. Plain text is the default foreground
(no SGR at all); emphasis is bold; palette colours are used in exactly three
places — `all`'s container prefixes, the summary line's exit code (green `0`,
red otherwise) and the `Error:`/`Warning:` marker words — and nowhere else. Only the plain 31–36 range is used — the bright
variants (90–96) are remapped to greys by Solarized and are theme-dependent
in the same way.

### `all`'s prefix palette

Five colours, cycled in the order **cyan, yellow, green, magenta, blue** —
`docker compose`'s order, which keeps the two most-confused hues (blue and
cyan) from landing on neighbouring containers until the cycle wraps at a
sixth; red is reserved for `Error:`. Assigned by the containers'
sorted names, so a container's colour doesn't change between runs because a
dependency resolved in a different order. The task-level prefix is bold in
the default foreground. All three are divergences from Batect — which cycles
six including black, assigns by graph order, and paints the task prefix
white — recorded on the differences page by
[#489](https://github.com/or1can/ratect/issues/489).

### Emphasis is for the user's own names and values

Bold marks three things and nothing else: the user's own names and values
(a task, container, image, command or exit code — what they wrote or are
reading for); the `Error:`/`Warning:` marker words; and `all`'s task-level
prefix. No other word of Ratect's is bold, no underline or italic anywhere,
separators (` | `) unstyled. The summary line's
exit code is both coloured and bold, being the one value most people read
for. This makes the "match Batect's bold" fixes
([#492](https://github.com/or1can/ratect/issues/492),
[#493](https://github.com/or1can/ratect/issues/493),
[#506](https://github.com/or1can/ratect/issues/506),
[#507](https://github.com/or1can/ratect/issues/507)) a policy both binaries
share, not parity that only `ratect-compat` owes.

### One grammar for every milestone

Subject first, by the user's own name, bold; then what happened — past tense
for a milestone (`Started`, `Built`, `Removed`), progressive for in-flight
(`Starting…`, `Pulling…`); and the summary as `<task> finished with exit code
<N> in <t>`. Ratect's own words, chosen over Batect's where Ratect's are
clearer ([#505](https://github.com/or1can/ratect/issues/505),
[#507](https://github.com/or1can/ratect/issues/507)). `fancy`'s
per-container status line and `simple`'s milestone line say the same thing
for the same event, so the two styles differ in rendering only, and a test
can pin each event's wording once.

### `ratect-compat`'s `simple` lines stay unmarked

Under `ratect-compat`, progress stays on stdout and shares it with the task's
output, unmarked, as under Batect; `-o quiet` and `-o all` are the escapes,
and [`docs/output-styles.md`](../docs/output-styles.md) will say so when the
stream rule lands on it. A prefix
would change every CI log captured from a Batect project for a problem the
native binary no longer has, and bold doesn't survive a pipe. For `ratect`
the question doesn't arise.

### Out of scope

Decided elsewhere or not decided here, listed so silence isn't read as a
decision: the `Caused by:` chain's format
([#427](https://github.com/or1can/ratect/issues/427)); `fancy`'s repaint cost
(`.out-of-scope/cheaper-fancy-repaints.md`); the `tracing` line format and
`--log-file`; each listing verb's own format.

## Alternatives considered

- **Progress on stdout in both binaries** (today's behaviour, Batect's). Rejected
  for `ratect`: it is the convention of the older tools, it makes `simple`
  unsafe to pipe, it makes `quiet` a stream rule rather than a verbosity rule,
  and every comparable tool written in the last decade went the other way.
  Kept for `ratect-compat` because a Batect user's scripts may read stdout.
- **Progress on stderr in both binaries**, with `ratect-compat` diverging from
  Batect. Rejected: stream placement is an outcome a script sees, so it is
  0013's rule 1, not its rule 4.
- **A dialect gate inside the loggers** to choose the stream. Rejected: the
  binaries differ in one file descriptor, which `main` already owns; a gate in
  `ui.rs` would be the next place a third difference crept in.
- **Keep the interrupt notice in `tracing`.** Rejected: it is the one line a
  person at the keyboard must read, and `RUST_LOG=off` hides it. Compose
  routes the same hint through its progress channel for the same reason.
- **A `Note: ` marker for notices.** Rejected: a marker asks for action; a
  notice asks for none.
- **Report-once decided per style** (suppress `Error:` only where progress
  already said it). Rejected: it would make `quiet` the one style that *does*
  print `Error:` for a task's own exit — the inverse of what [#454](https://github.com/or1can/ratect/issues/454) decided.
- **Keep Batect's white, or use bright colours.** Rejected: both are
  theme-dependent; the default foreground is the only colour every theme
  renders readably.
- **Batect's prefix order and graph-order assignment.** Rejected: blue
  beside cyan on the first two containers, and a colour that changes between
  runs.
- **A marker on `ratect-compat`'s `simple` lines.** Rejected: changes every
  Batect user's captured log; the native binary doesn't need it.

## Consequences

- **A breaking change for `ratect`** in `CHANGELOG.md` (no `written_for`
  row — no `ratect.toml` meaning changes): progress, milestones and the
  summary line move from stdout to stderr, as does `all`'s dependency,
  setup-command and build output. A script that captured `ratect run`'s
  stdout now gets the task's output alone. `ratect-compat` is unchanged
  here.
- **Observable in both binaries:** the interrupt notice, the cleanup's
  completion or failure, the `--no-cleanup*` reminder (rewritten to name what was left and how to
  remove it), the deprecated-flag warning and the loopback-proxy warning are
  printed, not logged, and `Error: Interrupted` no longer follows an interrupted run;
  stderr is coloured only when stderr is a terminal (closing
  [#479](https://github.com/or1can/ratect/issues/479)); white and black
  leave the palette; `all`'s prefix order changes; `simple` and `fancy` share
  one wording per event.
- `fancy` is auto-selected for `ratect` whenever stderr is an interactive
  console (the same terminal facts as today — a terminal, `TERM` set and not
  `dumb`, a queryable size — asked of stderr instead of stdout), so a run
  with stdout redirected gets it where it used to get `simple`.
- [`docs/output-styles.md`](../docs/output-styles.md) gains the stream rule
  and a per-binary sentence; its captures are regenerated; the differences
  page's rows for what `ratect-compat` stops copying — the palette, the
  interrupt wording and the dropped `Error: Interrupted`, white text — land
  with the implementation that makes each true, as 0016's did;
  [`CONTEXT.md`](../CONTEXT.md) gains the output terms this record uses.
  AGENTS.md guideline 11's one `eprintln!` exception becomes the general
  rule above.
- [#369](https://github.com/or1can/ratect/issues/369),
  [#488](https://github.com/or1can/ratect/issues/488) and
  [#489](https://github.com/or1can/ratect/issues/489) implement their
  sections of this record under their own briefs; the stream move, the
  per-stream terminal decision, the printed-versus-logged sweep, the
  colour rule, the markers and the grammar land under
  [#535](https://github.com/or1can/ratect/issues/535). [#492](https://github.com/or1can/ratect/issues/492)/#493/#506/#507
  and [#505](https://github.com/or1can/ratect/issues/505) proceed as written, re-grounded on the emphasis and grammar
  sections. [#454](https://github.com/or1can/ratect/issues/454)'s brief
  proceeds too, with two of its asides superseded here: a signal-ended run
  no longer prints `Error: Interrupted`, and the guideline-11 qualification
  it asks for is this record's rewrite of that guideline. [#479](https://github.com/or1can/ratect/issues/479) closes
  against this record.
