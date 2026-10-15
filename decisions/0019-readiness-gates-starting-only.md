# 0019 — A dependency's readiness gates only the start of what depends on it

**Status:** Accepted — image work shipped
([#312](https://github.com/or1can/ratect/issues/312), ratect-compat 0.33.0 ·
ratect 0.12.0); container creation planned
([#603](https://github.com/or1can/ratect/issues/603)).

## Context

A task's containers start concurrently, each branch of the dependency graph
proceeding on its own — but what each container does before it is *allowed*
to start has been gated on the wrong thing. Today a container's whole
sequence — resolve its image (pull or build), create it, start it, pass its
readiness gate — runs only once every container it depends on has passed its
readiness gate: started, healthy, setup commands done. The task's own
container is sequenced the same way before its command runs.

Batect gates less. Its `PullImageStepRule` and `BuildImageStepRule` are ready
the moment a run starts, so every image is pulled or built at once, alongside
network creation; its `CreateContainerStepRule` waits only for the task
network and the container's own image; and only `RunContainerStepRule` waits
for the container's dependencies to be ready. The readiness gate is on
*starting*, and nothing else.

The difference is observable, and the execution parity sweep
([#285](https://github.com/or1can/ratect/issues/285), by
[#284](https://github.com/or1can/ratect/issues/284)'s method) filed it as
[#312](https://github.com/or1can/ratect/issues/312), reproduced five times out
of five against a real daemon: a dependency's setup commands run — side
effects included — before the task container's unpullable image is even
tried; pulls and builds along a dependency chain are serialised, so a chain
of three waits for three pulls in turn where Batect runs them together; and
a create-time failure (an invalid mount or device, a name conflict) surfaces
only after every dependency is ready. `fancy`'s status lines inherit the
order — a container sits at "ready to build image" for the whole of its
dependencies' startup ([#500](https://github.com/or1can/ratect/issues/500)) —
and [`docs/how-it-works.md`](../docs/how-it-works.md) describes the branches as
"pulling, building, starting and health-waiting at the same time, gated only
on each container's own `dependencies` being ready", which is true of
starting and false of pulling and building.

[0013](0013-when-ratect-compat-copies-batect.md)'s first rule decides it:
where a config runs differently under `ratect-compat` than under Batect, that
is a gap, not a divergence. Running a dependency's setup commands before
discovering the task image doesn't exist is a different run.

One neighbouring difference is already decided the other way and stays so:
Ratect creates the task network (or, under `--use-network`, checks it exists)
*before* any image work, where Batect does both at once. A network problem
then fails the run in milliseconds instead of after draining in-flight pulls,
and the row on [`docs/differences-from-batect.md`](../docs/differences-from-batect.md)
records it. This record builds on that: "the network is ready" is the one
thing image work waits for.

## Decision

**A dependency's readiness gate gates the start of the containers that depend
on it — and nothing else.** Everything a container needs *before* it can be
started proceeds without regard to its dependencies:

1. **Image work starts when the network is ready.** Every image in the task's
   graph — a dependency's, an external health check's companion's, the task
   container's own —
   is pulled or built as soon as the task network exists, concurrently, each
   memoised as today (one pull per image, one build per container) so two
   containers sharing an image share one in-flight pull. `--max-parallelism`
   caps each pull and build as it does now. A pull or build that fails ends
   the run as soon as it fails — not once every dependency is ready, by
   which time its setup commands have run.
2. **A container is created once its own image and the network are ready**,
   whether or not its dependencies are. Creation and starting become two
   steps in the runtime, for dependencies and for the task's own container
   alike. A container that has been created and not yet started is registered
   for cleanup at creation — as the engine already does for every container
   it creates — so a run that fails between the two leaves nothing
   unaccounted for. Under `--max-parallelism`, a create and a start each hold
   one permit, released when that step ends; a permit is never held while
   waiting on a dependency, so a small limit can't deadlock a chain. This is
   Batect's every-step model, narrowed to the steps Ratect caps.
3. **A container starts only when every container it depends on has passed
   its readiness gate.** Unchanged, and the only gate there is. The task's
   own container's start is gated the same way; what its *own* gate does
   afterwards is [0012](0012-native-task-container-has-no-readiness-gate.md)'s
   and is not touched here.

What a failure during startup does is unchanged: a pull, build, create or
start that fails drops whatever else was starting alongside it at once and
cleanup begins — the divergence from Batect's cancel-then-drain that
[`docs/differences-from-batect.md`](../docs/differences-from-batect.md)
already records.

**Two phases**, because they are separable and the first carries the
correctness. Phase 1 — rule 1, image work — is an engine change alone: the
memoised pull and build cells exist, and only where they are awaited moves.
It lands with [#312](https://github.com/or1can/ratect/issues/312). Phase 2 —
rule 2, creation — splits create from start in the `ContainerRuntime` trait,
including `run_container`'s interactive and stdin-forwarding paths, and is
[#603](https://github.com/or1can/ratect/issues/603). Between the phases a
container is still created and started together, after its dependencies are
ready; `fancy`'s creation states ([#500](https://github.com/or1can/ratect/issues/500))
wait for phase 2, since a step that doesn't exist yet has no milestone to
render.

The lifecycle events a run posts follow the new order: image events for every
container arrive early, no longer after a dependency's readiness; a
dependency's "starting" milestone is posted when its start actually begins —
after its dependencies are ready and the permit is held (today it is posted
before the permit is acquired; moving it is in
[#505](https://github.com/or1can/ratect/issues/505)'s agent brief); phase 2
adds a creation
milestone for the display to render.

## Alternatives considered

- **Keep today's order and record it as a divergence.** Rejected by
  [0013](0013-when-ratect-compat-copies-batect.md)'s first rule: the run
  differs — setup commands with side effects run before a doomed pull fails —
  and it is slower for no benefit. The one argument for it, that a pull
  started early is wasted if a dependency then fails, is Batect's own
  trade-off too, and a pulled image is cached for the next attempt.
- **Hoist image work and creation in one change.** Rejected: creation needs
  the runtime trait to separate create from start, and `run_container`'s
  TTY and stdin paths come with it; image work needs none of that and
  delivers the correctness half alone. Two reviewable changes, one record.
- **Hoist image work only; keep creation gated on dependencies for good.**
  Rejected: it leaves the create-time failures late and `fancy` without
  Batect's creation states for no reason beyond effort, and the effort is
  phase 2's to spend when it is scheduled.
- **One `--max-parallelism` permit spanning create through start.** Rejected:
  a created container waiting for its dependencies would hold a permit the
  dependencies may need, and a chain longer than the limit would deadlock.
  **Capping the start alone** was also considered and rejected: creation is
  an operation like the others Ratect caps, and the recorded contract is
  that the flag caps the resource-intensive operations.
- **Create the network concurrently with the pulls**, as Batect does.
  Rejected, as already decided: failing fast on a network problem is worth
  more than the milliseconds the concurrency saves.

## Consequences

- The order of a run's lifecycle events changes, visibly under `-o all` and
  `fancy`: image lines come first, for every container, then dependencies
  start as their own dependencies become ready. A `CHANGELOG.md` entry for
  both binaries, not a breaking one — no flag, exit code or config meaning
  changes.
- Tests that pinned the old order (image events after a dependency's setup
  commands) are rewritten to the new one; the fake runtime gains whatever
  it lacks to express the new orderings — a build that takes time alongside
  a dependency's startup, a network creation that fails before any image
  work has begun.
- [`docs/how-it-works.md`](../docs/how-it-works.md)'s concurrency paragraph
  and [`docs/dependency-readiness.md`](../docs/dependency-readiness.md)'s
  resolution-order section say what the gate gates — starting — and that
  image work and, after phase 2, creation don't wait for it; `CONTEXT.md`'s
  readiness-gate entry says the same in one sentence.
- After phase 2, the `--max-parallelism` row on
  [`docs/differences-from-batect.md`](../docs/differences-from-batect.md)
  names a create and a start as separate capped steps.
- [#500](https://github.com/or1can/ratect/issues/500) depends on phase 2;
  [#505](https://github.com/or1can/ratect/issues/505)'s "starting" milestone
  position is consistent with both phases and can land before either.
