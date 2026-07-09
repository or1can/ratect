# How It Works

This describes Ratect's internal pipeline, for anyone extending Ratect or trying to
understand its behavior in detail. For the code itself, see [`AGENTS.md`](../AGENTS.md)
for a map of the source layout.

## 1. CLI parsing

`src/main.rs` parses arguments with [`clap`](https://docs.rs/clap). See the
[CLI reference](cli-reference.md) for the full flag list. This has to happen before
config resolution (step 2 below) can finish, since `--config-var`/`--config-vars-file`
feed into it.

## 2. Config loading and resolution (`ratect-core/src/config.rs`)

This is two separate steps, not one, because the second depends on CLI flags that
aren't known at the first:

1. **`Config::load_from_file`**: the YAML file (`batect.yml` by default) is parsed into
   `Config`/`Container`/`Task`/`TaskRun`/`ConfigVariable` structs using
   [`noyalib`](https://docs.rs/noyalib). Nothing else — no path resolution, no
   expression interpolation.
2. **`Config::resolve_expressions`**: called once from `main.rs`, after `--config-var`/
   `--config-vars-file` have been parsed and merged into an overrides map. In one pass:
   - Resolves [expressions](config-reference.md#expressions) (`$VAR`, `${VAR:-default}`,
     `<name`, `<{name}`, plus the built-in `batect.project_directory`) within every
     `environment` value (container and task `run`) and every volume's host path.
   - **Volume path resolution**: *after* interpolating a volume's host path, if the
     result is relative, it's resolved to an absolute path relative to the directory
     containing the config file (not the current working directory) — done in this
     order (interpolate, then resolve) because an expression can itself resolve to an
     absolute path, which mustn't be treated as a relative fragment.

   See the [configuration reference](config-reference.md#expressions) for the full
   expression syntax, precedence, and error rules.

## 3. Task engine (`ratect-core/src/engine.rs`)

`TaskEngine::run_task(name)` is a recursive async function:

1. **Already executed?** If this task has already run successfully in this invocation,
   return immediately (no-op). This is what makes shared prerequisites run only once.
2. **Cycle detection**: if this task is already in the middle of being run (i.e. it's
   an ancestor of itself in the current call stack), return an error immediately
   instead of recursing forever.
3. **Run prerequisites**: each entry in the task's `prerequisites` list is run (via the
   same `run_task` function) before the task's own container step.
4. **Create the task's network**: every task execution gets its own Docker network,
   whether or not its container declares `dependencies` — a task's container is
   never left running on Docker's shared default bridge network. If the container
   *does* declare `dependencies`, those containers are started on that network
   (recursively, for nested dependencies) before the task's own container, so it can
   reach them by name. This is scoped to just this one task execution and torn down
   afterward — see [the task lifecycle](task-lifecycle.md) for the full step-by-step
   detail and diagrams.
5. **Run the container step**:
   - If the container has an `image`, pull it (unless it's already been pulled once
     this run) and run the container with the task's `command`, joined to the task's
     own network, with its `environment` merged with the task's own `run.environment`
     (which wins on a key collision).
   - If the container only has a `build_directory`, Ratect currently just logs a
     warning and does nothing further — image building isn't implemented yet (see
     [differences from Batect](differences-from-batect.md)).
   - If neither `image` nor `build_directory` is set, nothing happens and no error is
     raised.

The "run once" and "pull once" guarantees are tracked with in-memory sets
(`executed_tasks`, `pulled_images`, `in_progress_tasks`) scoped to a single `ratect`
invocation — nothing persists between runs. Dependency/network state, by contrast, is
scoped to a single *task* execution, not the whole invocation — see
[the task lifecycle](task-lifecycle.md).

Task execution is currently **sequential**: prerequisites run one after another, not in
parallel, even when they're independent of each other. Parallel execution is on the
[roadmap](../ROADMAP.md#rust-enhancements).

### Testability

The engine talks to Docker through a `ContainerRuntime` trait (defined in
`ratect-core/src/docker.rs`) rather than depending on the concrete Docker client
directly. This is what lets the engine's prerequisite/cycle/dedup logic be
unit-tested with a fake implementation instead of a real Docker daemon.

## 4. Docker integration (`ratect-core/src/docker.rs`)

`DockerClient` wraps [`bollard`](https://docs.rs/bollard), Ratect's async Docker API
client, and implements `ContainerRuntime`:

- **`pull_image`**: streams `docker create-image` progress and displays it via a
  spinner (using [`indicatif`](https://docs.rs/indicatif)).
- **`run_container`**: creates a container (attaching stdout/stderr, any resolved
  volume binds, and any resolved `environment` variables), joins it to the task's own
  network, starts it, streams its logs live to Ratect's own stdout, then removes the
  container once it exits.
- **`create_network` / `remove_network`**: thin wrappers over Docker's network API,
  used for the per-task network every task execution gets (see
  [task lifecycle](task-lifecycle.md)).
- **`start_background_container` / `stop_and_remove_container`**: create+start (or
  stop+remove) a container without streaming its logs or waiting for it to exit —
  used for dependency/sidecar containers, which run alongside the task rather than
  being the thing the task is waiting on.

Container creation/start/removal events are logged at `debug` level via `tracing` (see
below) — not shown by default, but useful with `RUST_LOG=debug`.

## 5. Logging vs. output

Ratect keeps two channels deliberately separate:

- **stdout**: the actual result of the command you asked for — container log output
  and `--list-tasks` listings. Safe to pipe.
- **stderr**: Ratect's own diagnostics, via [`tracing`](https://docs.rs/tracing) /
  [`tracing-subscriber`](https://docs.rs/tracing-subscriber), filtered by `RUST_LOG`
  (defaults to `info`).

This split is why running a task doesn't pollute its output with Ratect's own status
messages, and why `--list-tasks` output can be parsed directly.
