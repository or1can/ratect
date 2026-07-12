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
   same recursive function, `run_task_scoped`) before the task's own container step —
   but with `top_level: false`, unlike the task actually named on the command line
   (`top_level: true`). That flag is what decides interactive-TTY eligibility in step 5
   below — a prerequisite chain isn't the thing being "run" interactively, so only the
   originally-requested task's own container is ever eligible, however deeply nested
   its prerequisites are.
4. **Create the task's network**: every task execution gets its own Docker network,
   whether or not its container declares `dependencies` — a task's container is
   never left running on Docker's shared default bridge network. If the container
   *does* declare `dependencies`, those containers are started on that network
   (recursively, for nested dependencies) before the task's own container, so it can
   reach them by name. This is scoped to just this one task execution and torn down
   afterward — see [the task lifecycle](task-lifecycle.md) for the full step-by-step
   detail and diagrams.
5. **Resolve and run the image**: `TaskEngine::resolve_image` turns the container's
   `image` or `build_directory` into the image reference to actually run — pulling
   (unless already pulled once this run) if `image` is set, or building (unless already
   built once this run — see below) if `build_directory` is set, or erroring if neither
   is. The same method is used for a task's own container and for dependency
   containers (in step 4), so both support either form identically.
   `TaskEngine::resolve_user_mapping` similarly turns a container's
   `run_as_current_user` (if enabled) into a `UserMapping` — also called for both a
   task's own container and each dependency independently (a dependency's own
   `run_as_current_user` doesn't depend on the task's), unlike `interactive` below,
   which only ever applies to the task's own container — see
   [User mapping](config-reference.md#user-mapping). Then the container runs with the
   task's `command`, joined to the task's own network, with its `environment` merged
   with the task's own `run.environment` (which wins on a key collision), and
   `interactive` set to `top_level` from step 3 — eligibility only; see
   [Interactive mode](config-reference.md#interactive-mode) and the Docker integration
   section below for what actually decides whether a TTY gets used.

The "run once", "pull once", and "build once" guarantees are tracked with in-memory
maps/sets (`executed_tasks`, `pulled_images`, `built_images`, `in_progress_tasks`)
scoped to a single `ratect` invocation — nothing persists between runs (a
`build_directory` container is rebuilt fresh every invocation, retagging
`<project_name>-<container_name>` each time — see
[config reference](config-reference.md#image-building)).
Dependency/network state, by contrast, is scoped to a single *task* execution, not the
whole invocation — see [the task lifecycle](task-lifecycle.md).

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
- **`build_image`**: builds an in-memory tar of the build directory (via the
  `build_context_tar` free function, `.dockerignore`-aware — see the
  [`dockerignore`](../dockerignore) crate — and unit-testable on its own, with no
  Docker involved), then streams `docker build` progress the same way `pull_image`
  does.
- **`run_container`**: creates a container (attaching stdout/stderr, any resolved
  volume binds, and any resolved `environment` variables), joins it to the task's own
  network, starts it, streams its output, then removes the container once it exits.
  Takes one of two paths depending on `should_use_tty` (ANDing the `interactive`
  eligibility the engine passed in against whether Ratect's own stdin *and* stdout are
  real terminals — see [Interactive mode](config-reference.md#interactive-mode)): the
  default path creates the container without a TTY and streams `docker logs
  --follow`; the interactive path additionally sets `tty`/`open_stdin`/`attach_stdin`
  on the container, attaches to it (`docker.attach_container`, before starting it, so
  no early output is missed) instead of using `logs`, puts the local terminal into raw
  mode for the session's duration (restored via a guard's `Drop`, even on an error
  return), and pumps stdin/stdout between the local terminal and the container
  concurrently until the attach stream ends. When `user_mapping` is `Some` (see
  [User mapping](config-reference.md#user-mapping)), missing host-side volume
  directories are created first, the container's `User` is set to the mapped
  `uid:gid`, and — after creation, before starting — synthetic
  `/etc/passwd`/`/etc/shadow`/`/etc/group` entries and the declared home directory are
  uploaded into it (`docker.upload_to_container`, via the `build_user_mapping_tar`/
  `build_home_directory_tar` free functions, both unit-testable on their own).
- **`create_network` / `remove_network`**: thin wrappers over Docker's network API,
  used for the per-task network every task execution gets (see
  [task lifecycle](task-lifecycle.md)).
- **`start_background_container` / `stop_and_remove_container`**: create+start (or
  stop+remove) a container without streaming its logs or waiting for it to exit —
  used for dependency/sidecar containers, which run alongside the task rather than
  being the thing the task is waiting on. Applies `user_mapping` the same way
  `run_container` does, if that dependency's own `run_as_current_user` is enabled —
  independent of whether the task's own container has it enabled.

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

### Filtering `RUST_LOG`

`RUST_LOG` isn't just an on/off level switch — `tracing-subscriber`'s
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
syntax lets you scope it to specific modules (`target=level` directives, comma-separated).
This matters in practice once you turn on `debug` for anything build-related (e.g. to see
a live [image build](config-reference.md#image-building) transcript): `bollard` (the Docker
API client Ratect is built on) also logs at `debug`, and a bare `RUST_LOG=debug` includes
*all* of its raw API traffic — usually far more noise than signal.

A directive with no target (e.g. `RUST_LOG=debug`) applies everywhere, including
dependencies like `bollard`. Scoping to a specific target instead — `ratect_core` covers
everything Ratect itself logs — excludes anything not matched, including `bollard`,
without needing to name it:

```sh
# Only ratect_core's own logs, at debug — no bollard noise at all.
RUST_LOG=ratect_core=debug ratect -f batect.yml build

# Keep the normal `info` default everywhere else, but add ratect_core's debug-level
# output on top (e.g. build transcripts) — usually the more useful combination.
RUST_LOG=info,ratect_core=debug ratect -f batect.yml build

# Narrower still: just the Docker/build/container-runtime module, not task
# orchestration (`ratect_core::engine`) as well.
RUST_LOG=ratect_core::docker=debug ratect -f batect.yml build
```

If you do want a blanket `debug` sweep across everything (including `bollard`) but need to
silence one specific dependency, add it as its own `=off` directive instead:
`RUST_LOG=debug,bollard=off`.
