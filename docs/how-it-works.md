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

1. **`Config::load_from_file`**: the root YAML file (`batect.yml` by default) is parsed
   into `Config`/`Container`/`Task`/`TaskRun`/`ConfigVariable` structs using
   [`noyalib`](https://docs.rs/noyalib), and its top-level `include` list (if any) is
   resolved — recursively, relative to each declaring file's own directory — with
   every loaded file's `containers`/`tasks`/`config_variables` merged into one `Config`
   (see [Includes](config-reference.md#includes)). No expression interpolation yet.
   The result is a `LoadedConfig`: the merged `Config`, plus a `container_base_paths`
   map recording which directory each container came from (needed by step 2 below).
2. **`LoadedConfig::resolve_expressions`**: called once from `main.rs`, after
   `--config-var`/`--config-vars-file` have been parsed and merged into an overrides
   map. In one pass:
   - Resolves [expressions](config-reference.md#expressions) (`$VAR`, `${VAR:-default}`,
     `<name`, `<{name}`, plus the built-in `batect.project_directory`) within every
     `environment` value (container and task `run`) and every `local` volume mount's
     host path (a `cache` mount's `name`/`container` are plain strings, matching
     Batect — nothing to interpolate; see [Cache volumes](config-reference.md#cache-volumes)).
   - **Volume path resolution**: *after* interpolating a `local` mount's host path, if
     the result is relative, it's resolved to an absolute path relative to *that
     container's own origin file's* directory (via `container_base_paths` — the root
     config's directory when there's no `include` involved), not the current working
     directory — done in this order (interpolate, then resolve) because an expression
     can itself resolve to an absolute path, which mustn't be treated as a relative
     fragment. `batect.project_directory` itself always resolves to the root config's
     directory regardless of which file a container came from. A `cache` mount's
     Docker volume name/host directory is resolved later instead (`ratect-core/src/cache.rs`,
     via `engine.rs`'s `resolve_volumes`), once `--cache-type` and the project's own
     cache key are known — neither available at this stage.

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
   its prerequisites are. If the task itself has no `run` (valid since 0.14.0 — a task
   can exist purely to chain `prerequisites` together, see [config
   reference](config-reference.md#task)), `run_task_internal` stops right here and
   returns success: there's no container of the task's own left to run, matching
   Batect's own `TaskRunner`.
4. **Create the task's network**: every task execution gets its own Docker network,
   whether or not its container declares `dependencies` — a task's container is
   never left running on Docker's shared default bridge network. Everything in the
   task's own container graph is started on that network before the task's own
   container, so it can reach them by name: the container's own `dependencies`,
   unioned with the task's own `dependencies` (0.14.0 — sidecars scoped to this one
   task specifically, distinct from the container-level field — see [config
   reference](config-reference.md#task)), resolved recursively for nested
   dependencies via `container_names_in_task`/`start_dependency`. A task's
   `customise` map (0.14.0 — per-task `environment`/`ports`/`working_directory`
   overrides for a non-main container somewhere in that graph, at any depth — see
   [config reference](config-reference.md#taskcontainercustomisation)) is threaded
   through this same resolution and applied to whichever dependency it targets, on
   top of that container's own base config. This is scoped to just this one task
   execution and torn down afterward — see [the task lifecycle](task-lifecycle.md)
   for the full step-by-step detail and diagrams. With `--use-network`, an existing
   network is validated to exist (`ContainerRuntime::network_exists`) and reused
   instead — never created, and never removed at cleanup, since Ratect didn't
   create it.
5. **Resolve and run the image**: `TaskEngine::resolve_image` turns the container's
   `image` or `build_directory` into the image reference to actually run — pulling (per
   `image_pull_policy`: `IfNotPresent`, the default, skips the pull if
   `ContainerRuntime::image_exists_locally` already says yes; `Always` never checks —
   either way, decided once per image name per run) if `image` is set, or building
   (unless already built once this run — see below) if `build_directory` is set, or
   erroring if neither is. The same method is used for a task's own container and for
   dependency containers (in step 4), so both support either form identically.
   `TaskEngine::resolve_user_mapping` similarly turns a container's
   `run_as_current_user` (if enabled) into a `UserMapping` — also called for both a
   task's own container and each dependency independently (a dependency's own
   `run_as_current_user` doesn't depend on the task's), unlike `interactive` below,
   which only ever applies to the task's own container — see
   [User mapping](config-reference.md#user-mapping). Then the container runs with the
   task's `command`, joined to the task's own network, with its `environment` built
   from four layers — the host's `TERM` (lowest precedence, gated on `interactive`
   below rather than a real TTY — see [`TERM`
   propagation](config-reference.md#term-propagation)), then [proxy environment
   variables](config-reference.md#proxy-environment-variables) (with every container
   name in this task appended to `no_proxy`, computed once per task via
   `container_names_in_task`), then the container's own `environment`, then the
   task's `run.environment` (each winning over the last on a key collision) — its
   `NetworkOptions` (`additional_hostnames`/`additional_hosts`, plus `ports` merged
   with any `run.ports` and expanded to concrete port triples via `merged_ports`,
   unless `--disable-ports`), its `ContainerOptions` (`working_directory` and
   `entrypoint` — each the task-level `run` override if set, else the container's
   own; `labels`, `capabilities_to_add`/`capabilities_to_drop` (converted from
   `config::Capability` to plain Docker capability names via `capability_names`),
   `privileged`, `shm_size`, `devices` (converted from `config::DeviceMapping` via
   `device_triples`), and `enable_init_process` — these eight are container level
   only, no `run` override, matching Batect), and `interactive` set to `top_level`
   from step 3 — eligibility only; see [Interactive mode](config-reference.md#interactive-mode)
   and the Docker integration section below for what this eligibility actually
   unlocks (stdin forwarding and `TERM`, unconditionally) versus what additionally
   requires a real TTY. Proxy variables (gated by the same `--no-proxy-vars` flag,
   `TERM` never applies here at all) are also merged underneath `build_args` when
   `resolve_image` builds an image.

The "run once", "pull once", and "build once" guarantees are tracked with in-memory
maps/sets (`executed_tasks`, `pulled_images`, `built_images`, `in_progress_tasks`)
scoped to a single `ratect` invocation — nothing persists between runs (a
`build_directory` container is rebuilt fresh every invocation, retagging
`<project_name>-<container_name>` each time — see
[config reference](config-reference.md#image-building)). `pulled_images`/
`built_images` cache a memoized, shareable future per key (`Arc<tokio::sync::OnceCell<...>>`,
see `engine.rs`'s `ReadyCell`) rather than a plain set/map, so two containers that
concurrently resolve the same image (0.15.0) share one in-flight pull/build instead of
racing to do it twice.
Dependency/network state, by contrast, is scoped to a single *task* execution, not the
whole invocation — a task's own dependency-readiness cache (the same `ReadyCell`
mechanism, keyed by container name this time) is built fresh per task execution and
discarded once it finishes — see [the task lifecycle](task-lifecycle.md).

Task execution is a mix of the two, matching Batect exactly: **prerequisites run
sequentially** — one after another, to completion, even when they're independent of
each other (Batect doesn't parallelize these either — see
[task lifecycle](task-lifecycle.md#known-simplifications-relative-to-batect)) —
while **a single task's own dependency startup is concurrent** (0.15.0): independent
branches of one task's container dependency graph pull/build/start/wait-healthy at
the same time, gated only by each container's own `dependencies` actually being
ready — see [task lifecycle](task-lifecycle.md#dependency-resolution). Running
independent prerequisites concurrently too remains a possible Rust-specific
enhancement beyond Batect's own behavior — see the [roadmap](../ROADMAP.md#rust-enhancements).

### Testability

The engine talks to Docker through a `ContainerRuntime` trait (defined in
`ratect-core/src/docker.rs`) rather than depending on the concrete Docker client
directly. This is what lets the engine's prerequisite/cycle/dedup logic be
unit-tested with a fake implementation instead of a real Docker daemon.

## 4. Docker integration (`ratect-core/src/docker.rs`)

`DockerClient` wraps [`bollard`](https://docs.rs/bollard), Ratect's async Docker API
client, and implements `ContainerRuntime`:

- **`pull_image`**: streams `docker create-image` progress, forwarding each status
  line as a progress event to the output layer (see
  [Logging vs. output](#5-logging-vs-output) — what, if anything, a progress event
  renders as is the selected output style's decision, not `docker.rs`'s).
- **`build_image`**: builds an in-memory tar of the build directory (via the
  `build_context_tar` free function, `.dockerignore`-aware — see the
  [`dockerignore`](../dockerignore) crate — and unit-testable on its own, with no
  Docker involved), then streams `docker build` progress the same way `pull_image`
  does.
- **`run_container`**: creates a container (attaching stdout/stderr, any resolved
  volume binds, any resolved `environment` variables, its Docker `hostname` set to
  its own container name, plus its `NetworkOptions` — `additional_hosts` as
  `HostConfig.extra_hosts` via the pure `build_extra_hosts`, and already-expanded
  port triples as `exposed_ports`/`port_bindings` via the pure `build_port_config`),
  joins it to the task's own network (with `additional_hostnames` as extra aliases
  beyond its name), starts it, streams its output, then removes the container once
  it exits.
  `cmd` is `command` tokenized into literal argv via `tokenize_command_line` (a
  from-scratch port of Batect's own `Command.parse`) with `additional_args`
  appended, no shell involved at all — same for `Config.entrypoint`, from
  `ContainerOptions::entrypoint`. The rest of `ContainerOptions` maps onto
  `HostConfig`/`Config` directly: `cap_add`/`cap_drop` (via `capability_names`),
  `privileged`, `shm_size`, `devices` (via the pure `build_devices` — which fills in
  Docker's `"rwm"` cgroup-permissions default itself, since the raw API, unlike the
  `docker` CLI, applies none), and `init`.
  `open_stdin`/`attach_stdin` on the container are set whenever `interactive`
  (eligibility) is true, independent of whether a real TTY ends up being used; `tty`
  itself is set only when `should_use_tty` (ANDing that same `interactive` eligibility
  against whether Ratect's own stdin *and* stdout are real terminals — see [Interactive
  mode](config-reference.md#interactive-mode)) is true. This gives three paths, not
  two: the fully non-interactive path creates the container without stdin/a TTY and
  streams `docker logs --follow`; the `interactive`-but-no-real-TTY path (piping input
  into a task whose output isn't a real terminal) attaches for stdin only
  (`docker.attach_container`, before starting, so nothing written early is lost),
  pumps it into the container, and otherwise streams output the same way the plain
  path does; the real-TTY path additionally attaches for stdout/stderr too instead of
  using `logs`, puts the local terminal into raw mode for the session's duration
  (restored via a guard's `Drop`, even on an error return), pumps stdin/stdout between
  the local terminal and the container concurrently until the attach stream ends, and
  keeps the container's TTY size in sync with the local terminal for the *whole*
  session (not just once, at attach) via a `SIGWINCH` listener (`tokio::signal::unix`,
  Unix-only — a plain OS signal, unlike a structured terminal-event API, which would
  consume/interpret stdin bytes instead of passing them through raw). When
  `user_mapping` is `Some` (see
  [User mapping](config-reference.md#user-mapping)), any missing host-side
  directory among the container's bind mounts is created first (a `local`
  mount's host path, or a `cache` mount's own directory under
  `--cache-type=directory` — never a bare Docker volume name, which
  `CacheType::Volume` resolves to instead and which `ensure_host_volume_directories_exist`
  explicitly skips), the container's `User` is set to the mapped
  `uid:gid`, and — after creation, before starting — synthetic
  `/etc/passwd`/`/etc/shadow`/`/etc/group` entries and the declared home directory are
  uploaded into it (`docker.upload_to_container`, via the `build_user_mapping_tar`/
  `build_home_directory_tar` free functions, both unit-testable on their own).
- **`create_network` / `remove_network` / `network_exists`**: thin wrappers over
  Docker's network API — create/remove for the per-task network every task execution
  gets (see [task lifecycle](task-lifecycle.md)), and `network_exists` (backed by
  `inspect_network`, treating a 404 as `false`) to validate `--use-network`'s target
  up front with a clear error rather than an unrelated API failure later.
- **`start_background_container` / `stop_and_remove_container`**: create+start (or
  stop+remove) a container without streaming its logs or waiting for it to exit —
  used for dependency/sidecar containers, which run alongside the task rather than
  being the thing the task is waiting on. Applies `user_mapping`, `NetworkOptions`
  (hostname, aliases, extra hosts, ports), and `ContainerOptions` the same way
  `run_container` does (no `cmd` here, though — a dependency has no task `command` of
  its own; its image's own `CMD`/`ENTRYPOINT` runs, unless `ContainerOptions::entrypoint`
  overrides it), from that dependency's own config — independent of the task's own
  container's settings.
- **`wait_for_container_healthy` / `exec_in_container`**: the two halves of the
  [dependency readiness gate](config-reference.md#dependency-readiness) the engine
  runs after starting each dependency. The first inspects the container (no health
  check at all means immediately healthy) and otherwise blocks on Docker's own event
  stream — filtered to that container's `health_status`/`die` events, replayed from
  the beginning of time so a verdict that arrived before the stream opened still
  counts — turning an *unhealthy* verdict into an error carrying the last
  health-check run's exit code and output (from `.State.Health.Log`). The second
  runs one `setup_commands` entry inside the running container via Docker's `exec`
  API (`sh -c`, the container's own environment and mapped user), returning the exit
  code and combined output for the engine to judge. A container's `health_check`
  config override itself is applied earlier, at creation time, by both
  `run_container` and `start_background_container` (via the pure
  `build_health_config`).

Container creation/start/removal events are logged at `debug` level via `tracing` (see
below) — not shown by default, but useful with `RUST_LOG=debug`. This includes each
`setup_commands` exec's raw output, which is whatever the command itself printed — so
if a setup command's own output could include something sensitive (a failed connection
string, a verbose HTTP client dumping request headers), that ends up in the debug log
too. Treat `RUST_LOG=debug` (or narrower `ratect_core=debug`) output with the same care
you'd give the command's own output before pasting it into a support ticket, chat
message, or CI log.

## 5. Logging vs. output

Ratect keeps two channels deliberately separate:

- **stdout**: the task's user-facing output — container log output, `--list-tasks`
  listings, and Ratect's own progress lines ("Running build...", "Pulling
  alpine:3.18...", "build finished with exit code 0 in 2.3s."), matching where
  Batect puts them. Internally these progress lines are typed events
  (`ratect-core/src/ui/`): `engine.rs` and `docker.rs` post task-execution
  milestones to an event sink instead of printing, and the selected
  [output style](cli-reference.md#output-styles) (`--output`/`-o`) decides what
  each event renders as — `fancy`'s live per-container status block on an
  interactive terminal, `simple`'s plain append-only lines otherwise, nothing at
  all under `quiet` (whose stdout is then exactly the containers' own output,
  safe to pipe), or `all`'s per-container prefixed lines (the one style where
  even container stdout routes through the event sink, line-buffered, instead of
  streaming to stdout directly).
- **stderr**: Ratect's own diagnostics, via [`tracing`](https://docs.rs/tracing) /
  [`tracing-subscriber`](https://docs.rs/tracing-subscriber), filtered by `RUST_LOG`
  (defaults to `info`) — except a *fatal* error (the reason the process is about to
  exit non-zero), which `main.rs` prints directly (`Error: <message>`) rather than
  through `tracing::error!`: it must stay visible even when `RUST_LOG` suppresses
  everything else, since there'd otherwise be no visible explanation at all for the
  failure under `RUST_LOG=off` combined with [`-o quiet`](cli-reference.md#output-styles).

Colors (e.g. the exit code in the task summary line) are only emitted when stdout is
actually a terminal — piped or redirected output gets plain text.

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
