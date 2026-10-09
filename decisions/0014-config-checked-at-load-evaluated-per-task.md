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
  `run_as_current_user` `home_directory` that isn't absolute, a relative cache
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

## Consequences

- Loading splits into a check of the whole file and a per-run resolution of what
  the task uses. That restructure is
  [#314](https://github.com/or1can/ratect/issues/314)'s design, and touches
  [#297](https://github.com/or1can/ratect/issues/297),
  [#373](https://github.com/or1can/ratect/issues/373),
  [#379](https://github.com/or1can/ratect/issues/379) and
  [#462](https://github.com/or1can/ratect/issues/462).
- Which of several load errors is reported must be deterministic, and match
  the order Batect reports that case in: the order the file declares things in,
  for a file's own mistakes
  ([#405](https://github.com/or1can/ratect/issues/405),
  [#433](https://github.com/or1can/ratect/issues/433)), and tasks, then
  containers, then config variables for a name defined in more than one file
  ([#432](https://github.com/or1can/ratect/issues/432)).
- `docs/ratect-compat-config-reference.md` documents resolve-once-at-load today;
  it changes with #314.
- The native `ratect` binary shares the loader, so it follows the per-task
  evaluation by default ([0013](0013-when-ratect-compat-copies-batect.md));
  #314's design can gate it on the dialect if there's a reason. The native
  format's own rules are [0003](0003-ratect-native-config-format.md)'s.
