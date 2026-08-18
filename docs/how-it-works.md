# How It Works

This describes Ratect's internal pipeline, for anyone extending Ratect or trying to
understand its behavior in detail. For the code itself, see [`AGENTS.md`](../AGENTS.md)
for a map of the source layout.

## 1. CLI parsing

Each binary has its own `main.rs`, and both parse arguments with
[`clap`](https://docs.rs/clap): `ratect-compat/src/main.rs` for Batect's flat flag
interface ([CLI reference](cli-reference.md)), and `ratect/src/main.rs` for the
subcommand interface ([`ratect` CLI reference](ratect-cli.md)). Everything below
this step is shared — both call into `ratect-core`, which is where the rest of
this document lives. Parsing has to happen before config resolution (step 2)
can finish, since `--config-var`/`--config-vars-file` feed into it.

## 2. Config loading and resolution (`ratect-core/src/config.rs`)

This is two separate steps, not one, because the second depends on CLI flags that
aren't known at the first:

1. **`Config::load_from_file`** (or `load_from_file_native`, for `ratect.toml`):
   the root file is parsed into `Config`/`Container`/`Task`/`TaskRun`/`ConfigVariable`
   structs — YAML via [`noyalib`](https://docs.rs/noyalib), TOML via
   [`toml`](https://docs.rs/toml), one struct set for both formats — and its
   top-level `include` list (if any) is resolved, with every loaded file's
   `containers`/`tasks`/`config_variables` merged into one `Config` (see
   [Includes](config-reference.md#includes)). No expression interpolation yet.

   Includes are walked breadth-first, so every entry in the root file is reached
   before any included file's own, and each file is loaded exactly once however
   many entries name it. A `type: git` entry clones its repository into
   `~/.ratect/incl` first (`ratect-core/src/git_include.rs`), and everything
   reached through one is confined to that clone and may do only what the entry
   granted it — see [Git includes](config-reference.md#git-includes) for the
   rules, and [`CONTEXT.md`](../CONTEXT.md) for what *bundle*, *grant* and
   *boundary* each denote.

   The result is a `LoadedConfig`: the merged `Config`, plus two maps step 2
   needs — `container_base_paths`, recording which directory each container came
   from, and `container_git_boundaries`, recording the clone each container
   reached through a Git include must resolve its host paths within.
2. **`LoadedConfig::resolve_expressions`**: called once, after
   `--config-var`/`--config-vars-file` have been parsed and merged into an overrides
   map — from `load_project`/`load_project_native` in `config.rs`, which run both
   steps in order so neither binary has to know the order. In one pass:
   - Resolves [expressions](config-reference.md#expressions) (`$VAR`, `${VAR:-default}`,
     `<name`, `<{name}`, plus the built-in `batect.project_directory`) in every field
     that takes one: `environment` values (container and task `run`), `local` volume
     mount host paths, `build_directory`, `build_args`, a `build_secrets` entry's
     `path`, a `build_ssh` entry's `paths`, `run_as_current_user.home_directory`, and
     — `ratect.toml` only — `image`. A `cache` mount's `name`/`container` are plain
     strings, matching Batect: nothing to interpolate (see
     [Cache volumes](config-reference.md#cache-volumes)).
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

     The resolved path is then checked against `container_git_boundaries`: a
     container that came from a Git-included file may only reach inside its own
     clone or your project directory, unless that include was granted
     [`allow_host_paths`](config-reference.md#git-includes). Both the check and
     the path it validates are lexically normalized first, since the comparison
     comes down to `Path::starts_with`, which does not interpret `..`.

   See the [configuration reference](config-reference.md#expressions) for the full
   expression syntax, precedence, and error rules.

## 3. Task engine (`ratect-core/src/engine.rs`)

`TaskEngine::run_task(name)` is a recursive async function. The order of its five
steps is the part worth knowing here; for what each one does in detail, read
`engine.rs`'s own module comment (`cargo doc --open -p ratect-core`), which is
where that lives and stays current.

1. **Already executed?** If this task has already run successfully in this
   invocation, return immediately. This is what makes shared prerequisites run
   only once.
2. **Cycle detection**: a task already in the middle of being run — an ancestor of
   itself in the current call stack — errors immediately rather than recursing
   forever.
3. **Run prerequisites**, each through the same recursive function, before the
   task's own container step. They run with `top_level: false`, unlike the task
   actually named on the command line, and that flag is the whole of
   interactive-TTY eligibility: a prerequisite chain isn't the thing being run
   interactively, so only the originally-requested task's own container is ever
   eligible, however deeply nested its prerequisites are. A task with no `run` of
   its own stops here and succeeds — it exists purely to chain prerequisites (see
   [config reference](config-reference.md#task)), matching Batect's own
   `TaskRunner`.
4. **Create the task's network** and start everything in the task's container
   graph on it, before the task's own container, so it can reach them by name. The
   graph is the container's own `dependencies` unioned with the task's, resolved
   recursively, with any [`customise`](config-reference.md#taskcontainercustomisation)
   overrides applied to whichever container they target at whatever depth. Every
   task execution gets its own network — a task's container is never left on
   Docker's shared default bridge — and it is torn down afterwards. With
   `--use-network` an existing network is validated and reused instead, and never
   removed, since Ratect didn't create it. See [the task
   lifecycle](task-lifecycle.md) for the step-by-step and diagrams.
5. **Resolve and run the image.** `resolve_image` turns a container's `image` or
   `build_directory` into something runnable — pulling (per
   [`image_pull_policy`](config-reference.md#container)) or building, or
   erroring if neither is set — and is used identically for the task's own
   container and for dependencies. The container then runs with the task's
   `command`, joined to the task's network, its environment layered host `TERM` →
   [proxy variables](config-reference.md#proxy-environment-variables) → the
   container's `environment` → the task's `run.environment`, each winning over the
   last. Everything else on the container — ports, hostnames, working directory,
   entrypoint, capabilities, devices, and the rest — is assembled here from the
   config and handed to `docker.rs` as plain values; the [config
   reference](config-reference.md) is the list of what those fields mean, and which
   accept a task-level `run` override.

The "run once", "pull once" and "build once" guarantees are in-memory and scoped to
a **single invocation** — nothing persists between runs, so a `build_directory`
container is rebuilt every time. Pulls and builds are memoized as shareable futures
rather than plain sets, so two containers resolving the same image share one
in-flight operation instead of racing. Dependency readiness, by contrast, is scoped
to a single **task execution** and discarded when it finishes.

Concurrency follows Batect exactly: **prerequisites run sequentially**, one to
completion after another, even when independent — while **one task's dependency
startup is concurrent**, with independent branches of its graph pulling, building,
starting and health-waiting at the same time, gated only on each container's own
`dependencies` being ready. Running independent prerequisites concurrently too is
a possible Rust-specific enhancement beyond Batect — see the
[roadmap](../ROADMAP.md#rust-enhancements) — and
[task lifecycle](task-lifecycle.md#dependency-resolution) has the detail.

### Testability

The engine talks to Docker through a `ContainerRuntime` trait (defined in
`ratect-core/src/docker.rs`) rather than depending on the concrete Docker client
directly. This is what lets the engine's prerequisite/cycle/dedup logic be
unit-tested with a fake implementation instead of a real Docker daemon.

## 4. Docker integration (`ratect-core/src/docker.rs`)

`DockerClient` wraps [`bollard`](https://docs.rs/bollard) and implements
`ContainerRuntime`. What each method is *for*:

- **`pull_image`** and **`build_image`**: fetch or build the image a container
  needs, streaming the daemon's progress to the output layer as events — what, if
  anything, those render as is the selected output style's decision, not
  `docker.rs`'s (see [Logging vs. output](#5-logging-vs-output)). `build_image`
  first packs the build directory into an in-memory tar, honouring
  `.dockerignore` via the [`dockerignore`](../dockerignore) crate.
- **`run_container`**: creates, starts and streams the task's own container until
  it exits. Three start/attach paths sit behind it — fully non-interactive,
  stdin-forwarding, and a real TTY with raw mode and live resize — chosen by
  whether the task is [interactive](config-reference.md#interactive-mode)-eligible
  and whether Ratect's own stdin *and* stdout are terminals. It does **not** remove
  the container: the engine's cleanup stage removes everything a task created, its
  own container included, so `--no-cleanup-*` is interpreted in exactly one place.
- **`create_network` / `remove_network` / `network_exists`**: the per-task network,
  plus the up-front validation that makes `--use-network` fail with a clear error
  rather than an unrelated API failure later.
- **`start_background_container` / `stop_and_remove_container`**: the same, for a
  dependency or sidecar — started and left running alongside the task rather than
  waited on, so no logs are streamed and no task `command` applies.
- **`wait_for_container_healthy` / `exec_in_container`**: the two halves of the
  [dependency readiness gate](config-reference.md#dependency-readiness). The first
  blocks on Docker's own event stream, replayed from the beginning so a verdict
  that arrived before the stream opened still counts, and turns an *unhealthy*
  verdict into an error carrying the last health check's exit code and output. The
  second runs one `setup_commands` entry in the running container and returns its
  exit code and output for the engine to judge.

Nothing here depends on `config` types: `engine.rs` converts config into plain
values first, which is why the same options struct serves both container methods.
The module's own comment carries the gotchas — where each path calls Docker's
`start` relative to attaching, why cleanup ownership is not to be split again, how
`build_ssh` paths are classified — and is the thing to read before changing any of
this.

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
RUST_LOG=ratect_core=debug ratect-compat -f batect.yml build

# Keep the normal `info` default everywhere else, but add ratect_core's debug-level
# output on top (e.g. build transcripts) — usually the more useful combination.
RUST_LOG=info,ratect_core=debug ratect-compat -f batect.yml build

# Narrower still: just the Docker/build/container-runtime module, not task
# orchestration (`ratect_core::engine`) as well.
RUST_LOG=ratect_core::docker=debug ratect-compat -f batect.yml build
```

If you do want a blanket `debug` sweep across everything (including `bollard`) but need to
silence one specific dependency, add it as its own `=off` directive instead:
`RUST_LOG=debug,bollard=off`.
