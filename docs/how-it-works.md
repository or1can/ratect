# How It Works

This describes Ratect's internal pipeline, for anyone extending Ratect or trying to
understand its behavior in detail. For the code itself, see [`AGENTS.md`](../AGENTS.md)
for a map of the source layout.

## 1. CLI parsing

Each binary has its own `main.rs`, and both parse arguments with
[`clap`](https://docs.rs/clap): `ratect-compat/src/main.rs` for Batect's flat flag
interface ([CLI reference](ratect-compat-cli.md)), and `ratect/src/main.rs` for the
subcommand interface ([`ratect` CLI reference](ratect-cli.md)). Everything below
this step is shared — both call into `ratect-core`, which is where the rest of
this document lives. Parsing has to happen before config evaluation (step 2)
can run, since `--config-var`/`--config-vars-file` feed into it.

## 2. Config loading and evaluation (`ratect-core/src/config.rs`)

This is two separate steps, not one: loading checks what the file says, and
evaluation reads what the machine running it supplies — see
[decisions/0014](../decisions/0014-config-checked-at-load-evaluated-per-task.md).

1. **`load_project`** (or `load_project_native`, for `ratect.toml`) starts with
   `Config::load_from_file` (or `load_from_file_native`): the root file is parsed
   into `Config`/`Container`/`Task`/`TaskRun`/`ConfigVariable` structs — YAML via
   [`noyalib`](https://docs.rs/noyalib), TOML via [`toml`](https://docs.rs/toml),
   one struct set for both formats — and its top-level `include` list (if any) is
   resolved, with every loaded file's `containers`/`tasks`/`config_variables`
   merged into one `Config` (see
   [how included files combine](includes.md#how-included-files-combine)).

   Includes are walked breadth-first, so every entry in the root file is reached
   before any included file's own, and each file is loaded exactly once however
   many entries name it. A `type: git` entry clones its repository into
   `~/.ratect/incl` first (`ratect-core/src/git_include.rs`), and everything
   reached through one is confined to that clone and may do only what the entry
   granted it — see [What a bundle may do](includes.md#what-a-bundle-may-do) for the
   rules, and [`CONTEXT.md`](../CONTEXT.md) for what *bundle*, *grant* and
   *boundary* each denote.

   It then checks the result — references, field combinations, and the shape of
   every value it can judge without the machine — applies `extends` (native
   only), and returns a `LoadedProject`. **No expression is evaluated**: a field
   that may hold one is judged here only when it holds a literal, so a container
   reading a variable this machine doesn't have stops nothing that doesn't use
   it. The `LoadedProject` keeps what evaluation needs: the directory each
   container was declared in, the clone a Git-included container must keep its
   host paths within, and which container in an `extends` chain supplied each
   path-bearing field. Listing tasks needs nothing more.
2. **`LoadedProject::evaluate`**: given the containers and tasks a caller needs,
   and the `--config-var`/`--config-vars-file` overrides it parsed:
   - Evaluates [expressions](ratect-compat-config-reference.md#expressions) (`$VAR`, `${VAR:-default}`,
     `<name`, `<{name}`, plus the built-in `batect.project_directory`) in every field
     that takes one: `environment` values (container and task `run`), `local` volume
     mount host paths, `build_directory`, `build_args`, a `build_secrets` entry's
     `path`, a `build_ssh` entry's `paths`, `run_as_current_user.home_directory`
     (not in a Batect-format file), and — `ratect.toml` only — `image`. A `cache`
     mount's `name`/`container` are plain strings, matching Batect: nothing to
     evaluate (see [Cache volumes](ratect-compat-config-reference.md#cache-volumes)).
     A value that can't be evaluated is an error naming the container or task,
     the field and the expression.
   - **Host path resolution**: *after* evaluating a host path, if the result is
     relative, it's resolved to an absolute path relative to the directory of the
     file that declared the value — the container's own, or for an inherited
     field the ancestor's that supplied it; the root config's directory when
     there's no `include` involved — not the current working directory. Done in
     this order because an expression can itself resolve to an absolute path,
     which mustn't be treated as a relative fragment. `batect.project_directory`
     itself always resolves to the root config's directory regardless of which
     file a container came from. A `cache` mount's Docker volume name/host
     directory is resolved later instead (`ratect-core/src/cache.rs`, via
     `engine.rs`'s `resolve_volumes`), once `--cache-type` and the project's own
     cache key are known — neither available at this stage.

     The resolved path is then checked against the container's Git boundary, if
     it has one: a container that came from a Git-included file may only reach
     inside its own clone or your project directory, unless that include was
     granted [`allow_host_paths`](includes.md#vouching-for-a-bundle). Checked
     twice: once lexically, with both the check and the path normalized first,
     since `Path::starts_with` does not interpret `..`; then against the real
     locations, with symlinks resolved as far as the path exists, since a bundle
     can commit one inside its own clone. A literal path gets the same check at
     load.

   The engine evaluates each task as it reaches it — see step 3 of the task
   engine below.

   See the [configuration reference](ratect-compat-config-reference.md#expressions) for the full
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
   actually named on the command line, and with no `-- ARGS`. The flag keeps
   `--skip-prerequisites` to the named task, and leaves a prerequisite's
   resources out of `--no-cleanup-after-success`. It plays no part in
   interactive eligibility: every task's own container is eligible, however
   deeply nested, as in Batect — see [Interactive Mode](interactive-mode.md).
   The task is then
   **evaluated**, before any Docker work of its own: every container in its graph,
   its `run.environment` and its `customise` environments go through
   `LoadedProject::evaluate`, and the rest of the task reads only those values. A
   container no task in the run uses is never evaluated. A task with no `run` of
   its own stops here and succeeds — it exists purely to chain prerequisites (see
   [config reference](ratect-compat-config-reference.md#task)), matching Batect's own
   `TaskRunner`.
4. **Create the task's network**, then pull or build every image and create every
   container in the task's container graph on it — the task's own included — as
   soon as each one's image is ready, and start each dependency once every
   container it depends on is ready, before the task's own container, so it can
   reach them by name. The
   graph is the container's own `dependencies` unioned with the task's, resolved
   recursively, with any [`customise`](ratect-compat-config-reference.md#taskcontainercustomisation)
   overrides applied to whichever container they target at whatever depth. Every
   task execution gets its own network — a task's container is never left on
   Docker's shared default bridge — and it is torn down afterwards. With
   `--use-network` an existing network is validated and reused instead, and never
   removed, since Ratect didn't create it. See [the task
   lifecycle](task-lifecycle.md) for the step-by-step and diagrams.
5. **Resolve and run the image.** `resolve_image` turns a container's `image` or
   `build_directory` into something runnable — pulling (per
   [`image_pull_policy`](ratect-compat-config-reference.md#container)) or building, or
   erroring if neither is set — and is used identically for the task's own
   container and for dependencies. The container then runs with the task's
   `command`, created on the task's network (unless its own
   [`network_mode`](ratect-config-reference.md#network_mode-leaving-the-tasks-network)
   says otherwise), its environment layered as
   [Environment precedence](ratect-compat-config-reference.md#environment-precedence)
   states. Everything else on the container — ports, hostnames, working directory,
   entrypoint, capabilities, devices, and the rest — is assembled here from the
   config and handed to `docker.rs` as plain values; the [config
   reference](ratect-compat-config-reference.md) is the list of what those fields mean, and which
   accept a task-level `run` override.

The "run once", "pull once" and "build once" guarantees are in-memory and scoped to
a **single invocation** — nothing persists between runs, so a `build_directory`
container is rebuilt every time. Pulls and builds are memoized as shareable futures
rather than plain sets, so two containers resolving the same image share one
in-flight operation instead of racing. Dependency readiness, by contrast, is scoped
to a single **task execution** and discarded when it finishes.

Concurrency follows Batect's in outline: **prerequisites run sequentially**, one to
completion after another, even when independent — while **one task's startup is
concurrent**. Every container's image — each dependency's and the task's own —
is pulled or built at once, the moment the task's network is ready, and each
container is created as soon as its own image is, so a pull, build or create
that fails ends the task as soon as it fails — not once every dependency is
ready, by which time their setup commands have run. Starting is what a
container's own `dependencies` gate: independent branches of the graph start
and health-wait at the same time, and a container — already created — starts
once every container it depends on has passed its readiness gate (see [decision
0019](https://github.com/or1can/ratect/blob/main/decisions/0019-readiness-gates-starting-only.md)).
Cleanup is concurrent the same way, in reverse: a container is stopped once every
container depending on it has been, and every container ready to stop is stopped at
once, so independent dependencies wait out their grace periods together.
The task's network is created before any of that
starts, where Batect creates it alongside — see [Differences from
Batect](differences-from-batect.md#runtime-behavior-gaps). Running independent prerequisites concurrently too is
a possible Rust-specific enhancement beyond Batect — tracked as
[ratect#102](https://github.com/or1can/ratect/issues/102) — and
[Dependency Readiness](dependency-readiness.md#resolution-order) has the detail.

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
- **`create_container`**: creates a container — a dependency's or the task's own
  — ready to be started: Docker's create, the user-mapping upload, the join to the
  task network. It hands the container's id back the moment that is done (inside
  its error, if the upload or join failed after Docker's create), and the engine
  registers it for cleanup there and then, so a container whose start later fails
  or is cut short by another dependency failing is still removed. Its own step,
  separate from starting, because it waits for less: a container is created once
  its image and the network are ready, and started only once the containers it
  depends on are — see [Dependency readiness](dependency-readiness.md#resolution-order).
- **`run_container`**: starts and streams the task's own container until it exits.
  Three attach-and-start paths sit behind it — fully non-interactive,
  stdin-forwarding, and a real TTY with raw mode and live resize — chosen by
  whether the task is [interactive](interactive-mode.md#which-container-is-eligible)-eligible
  and whether Ratect's own stdin *and* stdout are terminals. All three attach to the
  container before Docker's start and read its output from that attach, as Batect
  does, so nothing typed or printed early is lost and any log driver works. It does
  **not** remove the container: the engine's cleanup stage removes everything a task
  created, its own container included, so `--no-cleanup-*` is interpreted in exactly
  one place.
- **`create_network` / `remove_network` / `network_exists`**: the per-task network,
  plus the up-front validation that makes `--use-network` fail with a clear error
  rather than an unrelated API failure later.
- **`start_background_container` / `stop_and_remove_container`**: the same, for a
  dependency or sidecar — started and left running alongside the task rather than
  waited on, so no task `command` applies; its output is shown only under
  [`-o all`](output-styles.md#all), read from an attach made before it starts.
- **`wait_for_container_healthy` / `exec_in_container`**: the two steps of the
  [dependency readiness gate](dependency-readiness.md#the-readiness-gate). The first
  blocks on Docker's own event stream, replayed from the beginning so a verdict
  that arrived before the stream opened still counts, and turns an *unhealthy*
  verdict into an error carrying the last health check's exit code and output, when
  the container and daemon are still there to report them. The
  second runs one `setup_commands` entry in the running container and returns its
  exit code and output for the engine to judge.

Nothing here depends on `config` types: `engine.rs` converts config into plain
values first, which is why the same options struct serves both container methods.
The module's own comment carries the gotchas — where each path calls Docker's
`start` relative to attaching, why cleanup ownership is not to be split again, how
`build_ssh` paths are classified — and is the thing to read before changing any of
this.

Container creation/start/removal events are logged at `debug` level via `tracing` (see
below), as is each `setup_commands` exec's raw output — which is why
[Troubleshooting](troubleshooting.md#filtering-rust_log) warns about pasting a `debug`
log anywhere.

## 5. Logging vs. output

Ratect keeps two channels deliberately separate — and output itself follows
one rule: **stdout is the result, stderr is everything said about producing
it** ([decisions/0017](https://github.com/or1can/ratect/blob/main/decisions/0017-output-streams-and-styles.md)).

- **Output** is what a user with `RUST_LOG` unset would be worse off without,
  and never goes through `tracing`. The *result* — a run's task-container
  output, a `tasks list` listing — is stdout. Everything *about* the run is
  stderr: errors (`Error: `), warnings (`Warning: `), notices (the interrupt
  notice, what `--no-cleanup*` left in place), in every style including
  `quiet`; and progress ("Running build...", "Pulling alpine:3.18...", "build
  finished with exit code 0 in 2.3s."), the one class `--output`/`-o` shapes.
  Progress is the one place the binaries differ: `ratect` writes it to stderr,
  `ratect-compat` to stdout, where Batect puts it — see
  [Streams](output-styles.md#streams). Internally these lines are typed events
  (`ratect-core/src/ui/`): `engine.rs` and `docker.rs` post task-execution
  milestones, and the three stderr classes, to an event sink instead of
  printing, and the selected [output style](output-styles.md) decides what
  each renders as — `fancy`'s live per-container status block on an
  interactive terminal, `simple`'s plain append-only lines otherwise, no
  progress at all under `quiet`, or `all`'s per-container prefixed lines (the
  one style where even container output routes through the event sink,
  line-buffered, instead of streaming to stdout directly — the task container's
  own lines still landing on stdout). Each `main` names which stream its
  progress console is; nothing inside the loggers chooses.
- **Diagnostics** are breadcrumbs — Docker API calls, what resolved to what,
  a best-effort retry that then succeeded — via
  [`tracing`](https://docs.rs/tracing) /
  [`tracing-subscriber`](https://docs.rs/tracing-subscriber) on stderr,
  filtered by `RUST_LOG` (defaults to `info`). A fatal error is never one of
  these: `main.rs` prints it directly (`Error: <message>`), so it stays visible
  under `RUST_LOG=off` and `-o quiet` alike — except for a run's failure the
  run has already reported by another route (the task's own non-zero exit, a
  signal, a cleanup failure the engine reported as it happened), which gets no
  second line.

Colours (e.g. the exit code in the task summary line, the `Error:` marker) are,
by default, only emitted when the stream being written to is actually a
terminal — each stream decides for itself, `tracing`'s lines included — see
[Colour](output-styles.md#colour) for the controls that change that.

### Filtering `RUST_LOG`

Scoping `RUST_LOG` to Ratect's own targets (`ratect_core`, `ratect_core::docker`)
rather than every dependency's is on
[Troubleshooting](troubleshooting.md#filtering-rust_log), which owns it.
