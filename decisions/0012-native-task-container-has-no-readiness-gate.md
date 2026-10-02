# 0012 — A task's own container's readiness gate

**Status:** Accepted — implemented. The native half in ratect 0.10.0
([#173](https://github.com/or1can/ratect/issues/173), which also closed
[#240](https://github.com/or1can/ratect/issues/240) as superseded, and
[#268](https://github.com/or1can/ratect/issues/268)); the Batect-compatible
half in ratect-compat 0.31.0
([#248](https://github.com/or1can/ratect/issues/248),
[#260](https://github.com/or1can/ratect/issues/260),
[#263](https://github.com/or1can/ratect/issues/263),
[#274](https://github.com/or1can/ratect/pull/274), and the refactor in
[#272](https://github.com/or1can/ratect/issues/272)). This record covers
both binaries: what a task's own container's readiness gate does under each
dialect, and why they differ.

## Context

A container's *readiness gate* is one gate with two steps: it must report
healthy, then its `setup_commands` must succeed, in order.
(`run_to_completion` and `external_health_check` change what the gate *is*
for a container, not how many it has.) For a dependency the gate is the
whole point — nothing that depends on it starts before it passes, so the task
container finds its database up and migrated.

Batect runs every container through the same per-container steps, the
task's own container included: its health wait and `setup_commands` run
concurrently with its main command, and a failure in either fails the task
even when the main command succeeded. Ratect ported that, in both binaries.
Nothing depends on the task container's readiness, so under the native
dialect — with no Batect behaviour to preserve — the gate there protects
nothing and only adds ways to fail. The Batect-compatible forces below are
why the same can't be said of `ratect-compat`.

**The native forces.** The common case is a one-off command — `psql`, a
migration script — run in an image built as a long-running service: its
`HEALTHCHECK` was written for that other role, and a short command exiting
before the first probe fails the task with "The container exited before
becoming healthy." almost every time. The native format had already settled
the question for one field: `external_health_check` is inert on a task's own
container, because running it *is* the task. `health_check` and
`setup_commands` were left behind in that change. #240 then asked for a
per-task way to override or clear them, which reopened the question
underneath: whether the gate should exist there at all.

**The Batect-compatible forces.** `ratect-compat` is a drop-in replacement,
and Batect's own `task-container-with-setup-command` journey test shows a
task container's setup commands can be load-bearing: its main command waits
for a file the setup command writes. A failure there has to fail the task,
or a CI task turns green with its setup never done. But the same fast main
command that troubles the native dialect troubles this one: it can exit
before the first health probe, before a setup command's `docker exec` can
start, or while one is running — killing it with the container — and then
the gate fails only *because* the container stopped. Batect fails the task
in every one of those cases, though none says anything about readiness.

## Decision

### Native dialect: no gate

Under the native dialect, a task's own container has no readiness gate:

- Ratect doesn't wait for its health — from a declared `health_check` or the
  image's own `HEALTHCHECK` — and posts no health events for it. Docker's own
  probe still runs; Ratect simply doesn't watch it.
- Its `setup_commands` don't run.
- The task's result comes only from the main command's exit code and from
  errors running it.

**The project's dialect decides**, not the file syntax a container is
declared in: a task container declared in a `.yml` include of a native
project, or in a YAML root file (`ratect -f batect.yml`), gets the native
behaviour. The dialect is derived once per run from how the project was
loaded (`config::Dialect`, carried on `LoadedProject` into
`TaskEngineSettings`), and the engine turns it into one value the
task-container path consumes, rather than checking the dialect at each call
site. This is a separate rule from the one deciding which *fields* a
container may use, which the container's own file decides
([`docs/includes.md`](../docs/includes.md#which-fields-a-file-may-use),
ratect#214): the file governs loading, the dialect governs running.

**Config is accepted silently.** `health_check`, `setup_commands` and
`run_to_completion` (#268) on a container used as a task container are
neither rejected nor reported by `doctor`/`config validate`: the same
container can be one task's main container and another task's dependency,
as with `external_health_check`. An `extends` child can set
`setup_commands = []` to clear an inherited list.

### Batect-compatible dialect: the gate runs, and the main command decides

Under the Batect-compatible dialect the task's own container goes through
the same gate a dependency does, concurrently with its main command, and:

1. **While the main command is running, a gate failure cancels it and fails
   the task.** As Batect does, because the main command may be waiting on
   the gate's work (#260); waiting for it instead can hang forever. Before
   cancelling, Docker is asked whether the container is still running: an
   exited one is awaited for its exit code, and a "running" or unanswered
   inspect waits up to two seconds (`TASK_CONTAINER_EXIT_GRACE`, #274) for
   the run's own result first, because a setup command killed by the main
   command's exit can report before Docker has marked the container
   stopped — CI hit exactly that.
2. **The run's end ends the gate**, whatever the exit code: a gate still in
   flight is dropped, not awaited (#263, and #272's refactor for a non-zero
   exit). After an exit 0, a failure the gate has already reported is
   forgiven (#248, #263), since none of those late failures says anything
   about readiness.
3. **After a main command exits non-zero, its exit code is the result.** It
   is the task's own verdict and the more useful thing to report, so it wins
   over a gate failure alongside it.
4. **Judged by the run's outcome, not the failure's cause.** A setup command
   killed with its container exits 137, which can't say why it died, and
   Docker doesn't report why an exec ended. So the rule looks at whether the
   main command has exited, never at what the gate's error was.

Dependencies are unchanged in both dialects: their gate still runs, and a
failure still fails the task.

## Alternatives considered

- **Keep the native gate and report its failures as warnings** (#173's first
  brief). Removes the false failures, but keeps a check whose result nobody
  consumes, and adds a warning for the fast-command race — a report about an
  outcome that doesn't matter. Withdrawn.
- **Keep the native gate and add a per-task override or clear of
  `health_check`/`setup_commands`** (#240). Treats the symptom per task,
  leaves the default wrong, and adds a field to fix a behaviour nobody wants
  by default. Closed as superseded.
- **Every compat gate failure as a warning** (#248's first proposal).
  Declined: task-container setup commands can be load-bearing, and a failure
  that turns a CI task green is the silent regression `ratect-compat` exists
  to avoid.
- **Forgive by cause** (#248 as built): a typed `docker::ContainerStopped`
  error, raised when the health wait saw the container die or an exec was
  refused, was forgiven and every other gate error failed the task. Replaced
  by #263, because it couldn't catch a setup command killed mid-run — that
  reports a plain exit code 137, indistinguishable from a command that
  failed on its own.
- **Never cancelling early** (before #260): the gate's failure was held
  until the main command exited. Removed, because a main command waiting on
  the gate's work then never exits.
- **Exact Batect behaviour** (any late gate failure fails the task).
  Declined: it makes a task whose main command succeeded fail for a reason
  that has nothing to do with it.
- **One inspect, no grace** (#263 as built): a single "is it running"
  answer decided whether to cancel. Replaced by #274's two-second wait after
  CI hit the race; the wait narrows it rather than closes it, since Docker
  never says why an exec ended.

## Consequences

- Breaking for `ratect`: a native task's own container no longer waits on
  its health check or runs its setup commands, so their failure no longer
  fails the task. A project that relied on a task container's
  `setup_commands` running must move them into the main command or into a
  dependency.
- `external_health_check`'s "inert, exactly as `health_check` is" is now
  true of every readiness field on a native task container.
- The cost under `ratect-compat`: a setup command genuinely failing in the
  same instant as an exit 0, or up to two seconds before it, is forgiven.
- The remaining race under `ratect-compat`: a setup command that fails more
  than two seconds before Docker reports the main command's exit cancels the
  run and reports the setup error, since a main command still running past
  the grace can't be told from one whose exit Docker is slow to record.
  [`docs/task-lifecycle.md`](../docs/task-lifecycle.md#known-limitations)
  carries the user-facing statement of both;
  [`docs/differences-from-batect.md`](../docs/differences-from-batect.md#container-fields)
  the row against Batect.
- Two dialects now differ at run time, not only at load time. The engine
  learns the dialect through `TaskEngineSettings::dialect`; the next
  difference of this kind derives from the same value rather than adding a
  new setting.
