# 0014 — Config is checked at load, and evaluated per task

**Status:** Accepted. Checking at load is implemented and recorded as a
divergence. Evaluating per task is planned for ratect-compat 0.32.0 · ratect
0.11.0 ([#314](https://github.com/or1can/ratect/issues/314), designed with
[#149](https://github.com/or1can/ratect/issues/149)); until it lands, expressions
are still evaluated at load.

## Context

Batect resolves most of a configuration lazily: beyond the file's structure and
a few checks it makes at load (names, a `customise` targeting the task's own
container), it builds the invoked task's container graph, then checks and
evaluates only what that graph uses — a `customise` naming a container outside
the graph, a `home_directory` that isn't absolute, an expression naming an unset
environment variable, all surface only when a task reaches them. So most
mistakes in a container no task runs never stop Batect.

`ratect-compat` loads the whole file up front: every container and every task is
checked and every expression evaluated for any command that loads the file,
`--list-tasks` included. The parity sweep ([#284](https://github.com/or1can/ratect/issues/284))
filed this as several gaps ([#331](https://github.com/or1can/ratect/issues/331),
[#333](https://github.com/or1can/ratect/issues/333),
[#335](https://github.com/or1can/ratect/issues/335),
[#314](https://github.com/or1can/ratect/issues/314),
[#355](https://github.com/or1can/ratect/issues/355)), but they are two different
kinds of thing:

- **Mistakes in the file** — a reference to an undeclared container, a
  `customise` for a container the task never starts. The file is wrong for
  everyone who runs it.
- **Values from the machine running it** — `$VAR` and `<var` expressions,
  config-variable overrides. The file can be right and still fail on one
  machine: a `deploy` container reading `$AWS_PROFILE` is fine on the machine
  that deploys, and shouldn't stop `test` running on one that doesn't.

## Decision

- **Checks on the file's own content run at load, for every command that loads
  the file**, and fail fast: undeclared references, a `customise` outside the
  task's graph, a task container also listed in its own `dependencies`, a
  literal `run_as_current_user` `home_directory` that isn't absolute, a relative cache
  destination under `run_as_current_user`. This is a divergence, recorded under
  "Load errors" in
  [`docs/differences-from-batect.md`](../docs/differences-from-batect.md). The
  related `--tag-image`-on-a-pulled-image check runs when that container's
  image is resolved, still before anything runs in that container.
- **Values that depend on the machine are evaluated only for what the invoked
  task uses**, after its graph is built, as in Batect: expressions in a
  container's environment, volume paths and build settings, and in a task's
  `run.environment`. (In `batect.yml`, `image` isn't an expression at all — see
  the differences page; only `ratect.toml` resolves one.) `--list-tasks` reads no config-variable overrides at all
  ([#355](https://github.com/or1can/ratect/issues/355)).
- In `batect.yml`, `run_as_current_user.home_directory` is not an expression at
  all, as in Batect ([#462](https://github.com/or1can/ratect/issues/462)).
- **Evaluation happens per task**: when each task in the execution order is
  reached — after its prerequisites have run, before any of its own containers
  is built, created or started — every container in that task's graph, and the
  task's own `run.environment`, is evaluated. So a prerequisite that fails
  stops the run with its own failure, as in Batect, before a later task's bad
  value is ever looked at. Within one task this is earlier than Batect, which
  evaluates each container as it creates or builds it, possibly after starting
  the task's dependencies; that is a divergence, recorded on the differences
  page.
- **A check on a field that may hold an expression judges a literal at load
  and an expression when it's evaluated.** That covers `home_directory`'s shape
  (absolute, no `:` or control character, a final path component) in
  `ratect.toml`, and a Git-included container's host paths staying inside its
  boundary. A literal mistake is a mistake in the file; a value from the
  machine waits for evaluation, which still precedes Docker touching that
  container, so no escaping path ever reaches the daemon.
- **An inherited path keeps its supplier's anchoring.** `extends` (native only)
  takes each field whole from one container in the chain, so loading records
  which container supplied each path-bearing field — `volumes`,
  `build_directory`, `build_secrets`, `build_ssh` — and evaluation resolves it
  against that container's file and boundary, as it did when evaluation
  preceded `extends`.
- **`ratect config validate` and `ratect doctor` evaluate the whole project**
  against the machine they run on, reporting each value that can't be
  evaluated, or that fails a check above, as a warning naming the field and the
  expression. A missing CI-only variable is expected on a laptop, so it never
  fails validation; this is where a value only one task would trip over is
  found without running that task.

## Alternatives considered

- **Everything lazy, as Batect does.** Rejected: a mistake in the file then
  waits silently until someone runs the one task that reaches it, often in CI or
  on a colleague's machine. Failing at load on every command catches it the
  first time anyone touches the project.
- **Everything eager, as `ratect-compat` did.** Rejected: a value that only one
  machine lacks breaks every command on every other machine, including
  `--list-tasks`. That is a gap under
  [0013](0013-when-ratect-compat-copies-batect.md)'s first rule — a config Batect
  runs fails under `ratect-compat`.
- **Evaluating once per run, before the first prerequisite.** Rejected: both
  this and per-task evaluation run less than Batect before failing, but this
  one also changes which failure is reported. Where a prerequisite would fail
  on its own, Batect reports that failure and its exit code; evaluating
  everything first reports a later task's bad value instead, and the broken
  prerequisite goes unnoticed.
- **Evaluating per container, as Batect does.** Rejected: exact within a task,
  but it spreads evaluation across the engine's build and create steps for a
  difference no outcome depends on — Batect already creates a task's
  containers concurrently, so which of two failures inside one task comes
  first is a race there anyway.
- **Keeping the un-extended containers and re-applying `extends` per task**, so
  each ancestor's paths are evaluated against its own file. Rejected for
  recording each field's supplier: the same anchoring, without redoing
  inheritance on every run.

## Consequences

- Loading splits into a check of the whole file and a per-run resolution of what
  the task uses. That restructure is
  [#314](https://github.com/or1can/ratect/issues/314)'s design, and touches
  [#297](https://github.com/or1can/ratect/issues/297),
  [#373](https://github.com/or1can/ratect/issues/373),
  [#379](https://github.com/or1can/ratect/issues/379) and
  [#462](https://github.com/or1can/ratect/issues/462).
- Where a config has several errors, a run reports the first, and which is
  first follows Batect only where it changes what executes. Errors at
  different points of failure — at load against during a task, or one task
  against a later one — surface in Batect's order, since that decides what has
  run and which exit code you get. Errors at the same point of failure — two
  bad containers in one file, two load-time rule violations — are reported in
  a deterministic order of Ratect's own, the order the file declares them in:
  the file is rejected either way, so which one is named first is presentation,
  [0013](0013-when-ratect-compat-copies-batect.md)'s fourth rule
  ([#405](https://github.com/or1can/ratect/issues/405),
  [#433](https://github.com/or1can/ratect/issues/433),
  [#432](https://github.com/or1can/ratect/issues/432)).
- `ratect config validate` still stops at the first load-time error; only its
  evaluation pass reports everything it finds. Collecting every load-time
  error is a separate change.
- `docs/ratect-compat-config-reference.md` documents resolve-once-at-load today;
  it changes with #314.
- The native `ratect` binary shares the loader, so it follows the per-task
  evaluation by default ([0013](0013-when-ratect-compat-copies-batect.md));
  #314's design can gate it on the dialect if there's a reason. The native
  format's own rules are [0003](0003-ratect-native-config-format.md)'s.
