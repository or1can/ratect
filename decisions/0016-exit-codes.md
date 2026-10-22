# 0016 — What an invocation exits with

**Status:** Accepted — a run's three rules implemented (ratect-compat 0.33.0 ·
ratect 0.12.0, [#540](https://github.com/or1can/ratect/issues/540)). Refined
on 2026-10-22, before that release shipped, into two paths. A usage error or
an invalid option value exits `125` on a run, where a `ratect-compat` line
naming no task now counts as one. On every other verb it exits `2`, except
that a verb whose job is to diagnose reports an invalid value as a finding.
That rework is planned in the
same release
([#654](https://github.com/or1can/ratect/issues/654)). As first written, a
usage error was `2` everywhere, and a value that failed its check was the
command's ordinary failure.

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

Implementing the first version of this record raised a case it didn't decide.
An invalid `BATECT_CACHE_TYPE` is rejected before anything runs, as in Batect
([#441](https://github.com/or1can/ratect/issues/441),
[#450](https://github.com/or1can/ratect/issues/450)). One brief gave it
`clap`'s `2`, the other "the ordinary fatal error". The record as written
allowed both: `--cache-type bogus` exits `2` because `clap` rejects it, while
the same value from the environment, checked by hand, would have exited `125`
or `1`. The code depended on which library did the check. And on a run, `2`
breaks the same rule `1` did: it is a common task code (`make` exits `2` on
failure), so a `2` from a run could mean either.

## Decision

An exit code comes from one of two paths, with a little shared on either side.

**Shared, before either path.** `--help` and `--version` exit `0`, on either
path, and even when a default they would otherwise check is invalid
([#441](https://github.com/or1can/ratect/issues/441),
[#450](https://github.com/or1can/ratect/issues/450),
[#539](https://github.com/or1can/ratect/issues/539)). Each acts as soon as
`clap` reaches it. An argument `clap` rejects *before* reaching it is still a
usage error on its path, so `ratect-compat --bogus --help` exits `125` and
`ratect --bogus --help` exits `2`.

### Path 1 — a run

`ratect run`, and a `ratect-compat` line meant to run a task. A run stays out
of the way of the task's own code, so that a script can tell "the task
failed" from "Ratect couldn't run it". The exit code answers one question:
**did the task's own container run to an exit code of its own?** A run is a
chain — the task's prerequisites,
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
   code: an argument `clap` rejects, an invalid option value (below), config
   that doesn't load, unknown task, daemon unreachable, image pull or build
   failed, a dependency that exited or never became ready, a setup command
   that failed, a `--log-file` that can't be used. Whose fault it is, and what
   to do about it, reaches stderr — users are told *why* it failed rather than
   asked to decode a taxonomy.

`125` is `docker run`'s own code for "the `docker` command itself failed, the
contained command never ran", which is in turn GNU coreutils' convention for
any command-running wrapper (`env`, `nohup`, `timeout`, `chroot`). The
passthrough in rule 1 already cites that convention; `125` completes it, and a
Docker user already knows what it means. Docker's `126` and `127` are not
Ratect's to produce: the runtime reports them for a contained command that
isn't executable or isn't found, and they pass through under rule 1. `docker
run` gives a bad flag `125` too, rather than `2`. So on a run, every failure of
Ratect's own to run the task is `125`. Its only other codes there are `0`
(for `--help` and `--version`), `128 + n`, and a crash's `101`.

### Path 2 — everything else

`tasks list`, `caches`, `includes`, `resources`, `config validate`/`convert`,
`doctor`, `completions`; `ratect-compat --list-tasks`, `--clean`,
`--clean-cache` and `--upgrade`. These run nothing, so `125` would say
something false of them, and nothing passes through, so they can use the
conventional three:

- `0` — it succeeded;
- `1` — the command failed: the daemon is down, a config file doesn't parse,
  `doctor` found a problem;
- `2` — **it was invoked wrong**: an argument `clap` rejects, or an invalid
  option value.

That is Docker's own split (`docker ps` with the daemon down exits `1`,
`docker run` exits `125`) and the POSIX utilities' `2`. There are no further
codes, for the same reason a run has only `125`: the message on stderr says
what went wrong, and a taxonomy of codes is a surface every new failure would
need classifying into.

### An invalid option value

An invalid option value is a value an option gets that fails a check *on the
value itself*: a word that isn't one of the accepted ones, or a path that
doesn't exist or isn't the kind of thing the option needs. For a path the
option creates, such as `--log-file`, that means a path whose directory
doesn't exist. The source doesn't
matter: the command line, an environment variable, a default file such as
`batect.local.yml`, or the option's built-in default (`-f`'s `ratect.toml` or
`batect.yml`). A failure found once the command reads or uses a valid value is
the command's own failure, not this one: a config file that exists but doesn't
parse, a log file that exists but can't be opened, a connection that's
refused.

**The code depends on the kind of failure; the timing belongs to each
option.** An invalid option value takes its path's invocation code (`125` on
a run, `2` otherwise) *whenever it is found*. When it is checked is each
option's own business, and it is not settled here. Some values are checked
up front, before any command starts. That covers every value `clap` itself
parses (`--cache-type`, `-o`, `--tag-image`) and, in `ratect-compat`, an
explicit `--config-vars-file` and `--log-file`. The same will hold for the
values whose issues decided so under
[0013](0013-when-ratect-compat-copies-batect.md)'s point-of-failure rule:
`batect.local.yml` and `BATECT_CACHE_TYPE`
([#441](https://github.com/or1can/ratect/issues/441),
[#450](https://github.com/or1can/ratect/issues/450)), and the Docker
configuration and certificate directories in `ratect-compat`
([#539](https://github.com/or1can/ratect/issues/539)). Others are checked
when first used: `-f` when the config loads, `DOCKER_TLS_VERIFY` when Ratect
connects ([#461](https://github.com/or1can/ratect/issues/461)), and
`DOCKER_BUILDKIT` when a build first needs it
([#460](https://github.com/or1can/ratect/issues/460)). So a command that
doesn't use a value checked lazily never sees its error: `ratect-compat
--clean` doesn't need its config file, and `ratect config convert` in a
project with only a `batect.yml` never looks for `ratect.toml`. Moving a check
earlier or later changes which commands notice a bad value. It never changes
the code they exit with.

**A verb whose job is to diagnose is the exception**: `doctor` and `config
validate`. A bad value they can name (a `-f` that doesn't load, a
`DOCKER_CONFIG` that isn't a directory) is a finding, and they exit `1` for a
problem found, as for any other. A usage error in their own arguments is
still `2`.

### Which path an invocation is on

The path is decided by what the invocation asked for, so it is known even when
the arguments fail to parse. For `ratect` it is the subcommand: `ratect run`
is a run (`ratect run --bogus` and `ratect -o bogus run test` both exit `125`);
`ratect --bogus`, `ratect tasks list --bogus` and an unknown or missing
subcommand are not (`2`). `ratect-compat` is flat, so a line is a run unless
one of the flags that replace a task appears in it: `--list-tasks`/`-T`,
`--clean`, `--clean-cache` or `--upgrade`. That holds whether or not the
arguments parsed and whether or not a task was named. So a parse failure on a
line meant to run a task exits `125`, and so does a line naming no task at all
([#434](https://github.com/or1can/ratect/issues/434)), the safe answer for a
CI step that was meant to run one. In both binaries the subcommand or flag
counts only where `clap` would read it as one. It doesn't count after `--`,
where it belongs to the task, or as the value of an option that takes one.
An option `clap` doesn't recognise is taken to take no value, the one reading
that doesn't guess at an unknown flag. So `ratect-compat build --bogus --
--clean` and `ratect --bogus run test` are runs (`125`), and `ratect-compat
--bogus --clean` is not (`2`).

### Shared, anywhere

A crash exits `101` (Rust's panic default; the panic hook says where to report
it). A path 2 command installs no signal handler, so a signal ends it with
the default action and the shell reports `128 + n` anyway. That is a fact
about the process, not part of path 2's own codes.

### Both binaries, one mapping

**Both binaries share the mapping, unchanged by dialect.** `ratect-compat`'s
`125` where Batect returns `255`, `0` for `--help` where Batect returns `1`,
`125` or `2` for a usage error or an invalid option value, given or defaulted, where Batect returns
`255`, `0` for `--help`/`--version` with an invalid default where Batect
returns `255`, and `128 + n` for a signal are divergences, recorded on
[`docs/differences-from-batect.md`](../docs/differences-from-batect.md) — each
row with the implementation that makes it true — under
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
  classification, and the message on stderr already says which it was. Path
  2's `2` for an invocation error is as far as the split goes.
- **`2` for a usage error on both paths** (this record as first written).
  Rejected: on a run it collides with the task's own codes, the collision
  `125` was chosen over `1` to avoid. A reason that rules out `1` rules out
  `2` as well.
- **An invalid option value as the command's ordinary failure** (`125` on a
  run, `1` otherwise), with only `clap`'s own rejections as `2`. Rejected:
  the same bad value then exits `2` from a flag and `1` from the environment,
  and moving a check between `clap` and hand-written code changes a code
  users can see.
- **The code decided by when the value is checked**: `2` for a value checked
  before the command starts, the command's failure for one checked when used.
  Rejected, though it was briefly the plan: it makes the code depend on check
  timing, which is the same fault one level down. Checking *every* value up
  front instead, to make the timing uniform, was rejected too: `doctor`
  couldn't start to report a bad value, and `--clean` would fail on a config
  file it doesn't read.
- **For `ratect-compat`, a run only when a task name parsed**, so every parse
  failure is `2`. Rejected: it brings the collision back on exactly the lines
  most likely to be CI steps. Deciding by whatever `clap` reached before it
  failed was rejected too, because the code would then depend on argument
  order.
- **A new record superseding this one.** Rejected because nothing here had
  been released. Append-only protects a decision once the release
  implementing it ships ([README](README.md#format)); before that, a
  refinement found while implementing it is folded in, with the change noted
  in the status line and its rework planned in the same release.
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
for a cancel, which on a run would collide with a task's own `2`, and on
path 2 with an invocation error. The exception is
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
  reaffirmed, not reopened. The differences page carries a row for each
  divergence the Decision's "Both binaries, one mapping" paragraph lists.
- The refinement adds these breaking changes against the first
  implementation, all in [#654](https://github.com/or1can/ratect/issues/654)
  and in `CHANGELOG.md`:
  - **On a run**, an argument `clap` rejects exits `125`, not `2`. That
    includes `ratect run --bogus`, `ratect-compat --bogus build`, and a bare
    `ratect-compat --bogus`, which names no task and so is now a run.
  - **On path 2**, outside `doctor` and `config validate`, an invalid option
    value exits `2`, not `1`. That covers
    `-f` naming a missing file or a directory, whether given or defaulted (so
    `ratect tasks list` with no `ratect.toml`). In `ratect-compat` it also
    covers `--config-vars-file` naming a missing file or a directory, and
    `--log-file` in a missing directory. `ratect` has no `--log-file`, and
    reads `--config-vars-file` only on a run or in the two diagnosing verbs.
  - **Once the issues that add each check land**, an invalid `DOCKER_*` value
    found by a path 2 command exits `2` (#461's brief had `1`).
- An invalid default from [#441](https://github.com/or1can/ratect/issues/441),
  [#450](https://github.com/or1can/ratect/issues/450) or
  [#539](https://github.com/or1can/ratect/issues/539) exits `125` on a run and
  `2` otherwise. A `ratect-compat` line naming no task
  ([#434](https://github.com/or1can/ratect/issues/434)) exits `125` once it
  fails. Each of those issues' briefs, and #461's, was written against the
  first version of this record, and is corrected alongside this refinement.
- [#654](https://github.com/or1can/ratect/issues/654) also rewrites both CLI
  reference pages' exit-code sections around the two paths, and the
  differences page's rows. Until then those pages describe the first version.
- `ratect_core::exit_code` gains a code for an invocation error and still
  keeps one mapping for both binaries. Both `main`s handle a `clap` error
  themselves rather than letting `clap` exit, so that its code can depend on
  the path.
