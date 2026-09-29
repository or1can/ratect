# 0012 — A task's own container has no readiness gate (native dialect)

**Status:** Accepted — implemented (ratect 0.10.0,
[#173](https://github.com/or1can/ratect/issues/173), which also closed
[#240](https://github.com/or1can/ratect/issues/240) as superseded). This decision covers
the native dialect (`ratect`) only: `ratect-compat` keeps Batect's
behaviour byte for byte.

## Context

A container's *readiness gate* is what it must pass before anything that
depends on it starts: its health check, then its `setup_commands`. For a
dependency that is the whole point — the task container needs its database
up and migrated before it runs.

Batect runs every container through the same per-container steps, the
task's own container included, and Ratect ported that: the task container's
health wait and `setup_commands` run concurrently with its main command, and
a failure in either fails the task even when the main command succeeded.
Nothing depends on the task container's readiness, so the gate there
protects nothing and only adds ways to fail. The common case is a one-off
command — `psql`, a migration script — run in an image built as a
long-running service: its `HEALTHCHECK` was written for that other role, and
a short command exiting before the first probe fails the task with "The
container exited before becoming healthy." almost every time.

The native format had already settled the question for one field:
`external_health_check` is inert on a task's own container, because running
it *is* the task. `health_check` and `setup_commands` were left behind in
that change. #240 then asked for a per-task way to override or clear them,
which reopened the question underneath: whether the gate should exist there
at all.

## Decision

Under the native dialect, a task's own container has no readiness gate:

- Ratect doesn't wait for its health — from a declared `health_check` or the
  image's own `HEALTHCHECK` — and posts no health events for it. Docker's own
  probe still runs; Ratect simply doesn't watch it.
- Its `setup_commands` don't run.
- The task's result comes only from the main command's exit code and from
  errors running it.

**The project's dialect decides**, not the file syntax a container is
declared in: a task container declared in a `.yml` include of a native
project gets the native behaviour. The dialect is derived once per run from
how the project was loaded (`config::Dialect`, carried on `LoadedProject`
into `TaskEngineSettings`), and the engine turns it into one value the
task-container path consumes, rather than checking the dialect at each call
site.

**Config is accepted silently.** `health_check`/`setup_commands` on a
container used as a task container are neither rejected nor reported by
`doctor`/`config validate`: the same container can be one task's main
container and another task's dependency, as with `external_health_check`.
An `extends` child can set `setup_commands = []` to clear an inherited list.

Dependencies are unchanged in both dialects: their gate still runs, and a
failure still fails the task.

## Alternatives considered

- **Keep the gate in both dialects and report its failures as warnings**
  (#173's first brief). Removes the false failures, but keeps a check whose
  result nobody consumes, and adds a warning for the fast-command race — a
  report about an outcome that doesn't matter. Withdrawn.
- **Keep the gate and add a per-task override or clear of
  `health_check`/`setup_commands`** (#240). Treats the symptom per task,
  leaves the default wrong, and adds a field to fix a behaviour nobody wants
  by default. Closed as superseded.
- **Change `ratect-compat` too.** `ratect-compat` is a drop-in replacement
  for Batect, which fails the task here; changing it is a compatibility
  question of its own. Making its gate failures warnings is a possible bug
  fix, left open as a follow-up
  ([#248](https://github.com/or1can/ratect/issues/248)) rather than decided
  here.

## Consequences

- Breaking for `ratect`: a native task's own container no longer waits on
  its health check or runs its setup commands, so their failure no longer
  fails the task. A project that relied on a task container's
  `setup_commands` running must move them into the main command or into a
  dependency.
- `external_health_check`'s "inert, exactly as `health_check` is" is now
  true of both fields.
- The fast-main-command race (a near-instant command exiting before a
  `setup_commands` entry's `docker exec`) is a `ratect-compat`-only
  limitation.
- Two dialects now differ at run time, not only at load time. The engine
  learns the dialect through `TaskEngineSettings::dialect`; the next
  difference of this kind derives from the same value rather than adding a
  new setting.
