# 0016 — What a run exits with

**Status:** Accepted — implemented (ratect-compat 0.33.0 · ratect 0.12.0,
[#540](https://github.com/or1can/ratect/issues/540)).

## Context

Ratect's exit codes grew one at a time. A task's own command's code passes
through, as `docker run`'s does; a termination signal exits 128 + its number;
a crash exits 101, Rust's default; `clap` exits 2 for a usage error and 0 for
`--help`; and every other failure — a config that doesn't load, an unknown
task, an unreachable daemon, a pull or build that fails, a dependency that
never becomes ready — exits 1. Both binaries share the one mapping, and
[`docs/ratect-cli.md`](../docs/ratect-cli.md) promises they always will.

Two things are wrong with that. First, `1` is also the commonest code a task's
own command exits with, so a CI job cannot tell "the tests failed" from "the
daemon was down" — the one distinction an exit code exists to make. Second,
the signal rule has a special case: a run that *passed* and was then signalled
during its cleanup reports the signal (`130`), while a run that *failed* and
was then signalled keeps its own code. That is not "the task's code wins"; it
is "non-zero wins", and it drops a `0` that was earned.

Batect, for comparison, has three codes: the task's own, `-1` (255) for
everything else — infrastructure, a usage error, an interrupt, an uncaught
exception, and `--no-cleanup-after-success` even on success — and `1` for
`--help`. Its documentation never names `255`; it only ever says "non-zero".
The parity sweep ([#284](https://github.com/or1can/ratect/issues/284)) filed
each difference as its own issue ([#338](https://github.com/or1can/ratect/issues/338),
[#339](https://github.com/or1can/ratect/issues/339),
[#458](https://github.com/or1can/ratect/issues/458); [#330](https://github.com/or1can/ratect/issues/330)
already decided), and [0013](0013-when-ratect-compat-copies-batect.md) set them
aside to be decided together rather than one at a time.

## Decision

The exit code answers one question: **did the task's own container run to
an exit code of its own?** A run is a chain — the task's prerequisites,
then the task — and the first task in it whose container exits non-zero
ends the run with that code, as today; "the task's own container" below is
the one that ended the run, or the requested task's when every container
exited `0`.

1. **It did → that exit code is Ratect's, `0` included.** Nothing that
   happens afterwards changes it, with one exception: if Ratect then fails to
   clean up — the daemon refuses a removal, or the cleanup ladder's abandon
   rung is reached with something still to remove — a run that would have
   exited `0` exits **`125`** instead. A cleanup failure is Ratect failing to
   remove something it set out to remove; what `--no-cleanup`,
   `--no-cleanup-after-success` or `--no-cleanup-after-failure` leaves in
   place on purpose is not one. A task's own non-zero code is never
   overridden: the failure is the thing the user needs to see, and a leak is
   reported on stderr either way. Forced removal that completes is a faster
   cleanup, not a failed one. A *prerequisite* whose cleanup fails this way
   ends the chain there, like any other failure of Ratect's own: the tasks
   after it never run, so rule 3 decides the run, and it exits `125`.
2. **A termination signal ended it → `128` + that signal's number** (`130`
   `SIGINT`, `143` `SIGTERM`, `129` `SIGHUP`), whatever cleanup does after.
   Only a signal that ended the command earns this: one that arrives during
   the cleanup of a finished run didn't end anything, so rule 1 applies. One
   that arrives during a *prerequisite's* cleanup ends the run before the
   next task starts, so it earns this code. The signal is still reported on
   stderr, and still escalates or abandons the cleanup as the ladder says.
3. **It never ran, or never finished, for any other reason → `125`.** One
   code: config that doesn't load, unknown task, daemon unreachable, image pull
   or build failed, a dependency that exited or never became ready, a setup
   command that failed. Whose fault it is, and what to do about it, reaches
   stderr — users are told *why* it failed rather than asked to decode a
   taxonomy.

`125` is `docker run`'s own code for "the `docker` command itself failed, the
contained command never ran", which is in turn GNU coreutils' convention for
any command-running wrapper (`env`, `nohup`, `timeout`, `chroot`). The
passthrough in rule 1 already cites that convention; `125` completes it, and a
Docker user already knows what it means. Docker's `126` and `127` are not
Ratect's to produce: the runtime reports them for a contained command that
isn't executable or isn't found, and they pass through under rule 1.

**The three rules are a run's** — `ratect run` and `ratect-compat <task>`.
Every other verb (`tasks list`, `caches`, `includes`, `resources`, `config
validate`/`convert`, `doctor`, `completions`; `ratect-compat --list-tasks`,
`--clean` and `--clean-cache`) runs nothing,
so `125` would say something false of it; each exits `1` on its own failure
and `0` otherwise, as today. That is Docker's own split — `docker ps` with
the daemon down exits `1`, `docker run` exits `125` — and it keeps `125`
meaning one thing. The mapping is the run paths' alone; a verb that isn't a
run doesn't consult it.

Outside the mapping, three codes stay what their conventions make them:
**`2`** for a usage error (`clap`'s, and the POSIX utilities'), **`0`** for
`--help`, and **`101`** for a crash (Rust's panic default; the panic hook says
where to report it). A failure on the way *into* whatever was asked for that
today exits by a path of its own — a `--log-file` that can't be opened — is
that invocation's failure: `125` for a run, `1` for anything else.

**Both binaries share the mapping, unchanged by dialect.** `ratect-compat`'s
`125` where Batect returns `255`, `0` for `--help` where Batect returns `1`,
`2` for a usage error where Batect returns `255`, and `128 + n` for a signal
are divergences, recorded on
[`docs/differences-from-batect.md`](../docs/differences-from-batect.md) — the
`--help` and usage-error rows alongside this record, the `125` rows with the
implementation that makes them true — under
[0013](0013-when-ratect-compat-copies-batect.md)'s rule 3 for the cases where
Batect's code reports something untrue (a passing run under
`--no-cleanup-after-success` exits `255`; so does a cancelled one, saying
nothing about the cancel) and its rule 4 for the rest: a script branches on
zero or non-zero, and Batect never documented `255` as something to branch on.

Named constants in `ratect_core::exit_code` are the one spelling of each code,
and tests pin the precedence above case by case.

## Alternatives considered

- **Batect's `255` for everything but the task's code**, in `ratect-compat`
  only or in both. Rejected. In one binary it breaks the shared mapping — two
  spellings of one contract, kept identical by hand — and forces a dialect
  gate into the one module that never needed one. In both it carries Batect's
  conflation of "cancelled" with "failed" into the native binary, for a number
  that means `ssh` to the wider world and nothing to a Docker user. The parity
  argument is weak: Batect's docs promise "non-zero" and no more.
- **Keep `1`.** Rejected: it is the one code a task's command is likeliest to
  exit with, so it is the one code that cannot mark Ratect's own failure.
- **A small taxonomy** — config invalid vs. daemon vs. image vs. dependency,
  or at least "fix your file" vs. "maybe retry". Rejected. Every new failure
  path would need classifying into it, scripts would start depending on the
  classification, and the message on stderr already says which it was. A usage
  error's `2` is as far as the split goes.
- **A fresh small number** (`3`, say) instead of `125`. Rejected: it means
  nothing to anyone, where `125` already has a meaning in the convention the
  passthrough rule borrows from. The collision with a task that itself exits
  `125` is accepted exactly as `docker run` accepts it.
- **The signal always wins** — report `128 + n` whenever one arrived, even
  during the cleanup of a finished run. Rejected: it drops a `0` the command
  earned (today's behaviour for a passing run), and under the rule that the
  task's non-zero code wins it would be a third rule rather than a consequence
  of the first.
- **Cleanup failure stays a warning** with the exit code untouched. Rejected:
  a leaked container or network is Ratect failing at its own job, and CI
  should see it. Limiting the override to runs that would otherwise exit `0`
  keeps the task's own failure visible.

A survey of comparable command-running tools (coreutils' wrappers, `docker
run`, `podman run`, `docker compose`, `kubectl`, `earthly`, `dagger`, `act`,
`cargo run`, `go run`, `npm run`, `just`, `make`, `nix run`, `devbox run`,
`tilt`; primary sources, October 2026) bears the choices out, with one
exception. Every tool that distinguishes its own failure from the command's
uses a reserved code for it — `125` for the coreutils/Docker/Podman family,
`101` for Cargo, a small set for Earthly — and every tool that exits `1` for
its own failure doesn't distinguish at all; none uses `1` *as* the
distinction. `128 + n` for a signal is what `just` and `timeout` do and what
`docker compose run` returns for `SIGINT` (`130`); Earthly and Dagger use `2`
for a cancel, which here would collide with a usage error. The exception is
rule 1's cleanup override: no surveyed tool documents any code for a
teardown that fails after the command succeeded (`docker compose up` exits
`0` after a graceful teardown; Earthly force-exits `9` only when cleanup
exceeds a timeout). That part is Ratect's own, chosen because a leaked
container is a failure CI should see, not borrowed from anywhere.

## Consequences

- Three observable changes, each a breaking change in `CHANGELOG.md` for both
  binaries (a script may check the old numbers; no `ratect.toml` meaning
  changes, so no `written_for` row): every failure of a *run* that exited
  `1` exits `125` (the other verbs' `1` is unchanged); a passing run
  signalled during its cleanup exits `0` (or `125` if the
  cleanup is then abandoned or fails) rather than `128 + n`; a passing run
  whose cleanup fails exits `125` rather than `0`.
- The cleanup ladder's abandon rung now has an exit-code consequence;
  `CONTEXT.md`'s entry for it defers here for what a signal during cleanup
  reports.
- [#339](https://github.com/or1can/ratect/issues/339) and
  [#458](https://github.com/or1can/ratect/issues/458) were closed against
  this record as it was written — both keep today's behaviour;
  [#338](https://github.com/or1can/ratect/issues/338) closes with the
  implementation ([#540](https://github.com/or1can/ratect/issues/540));
  [#330](https://github.com/or1can/ratect/issues/330)'s decision is
  reaffirmed, not reopened. The exit-code sections of both CLI reference pages
  are rewritten from the three rules above, and the differences page gains the
  rows the Decision names.
- A run with no task name exiting `0`
  ([#434](https://github.com/or1can/ratect/issues/434)) is a usage error, and
  gets `2` when it is fixed; that fix is its own issue, not this one.
