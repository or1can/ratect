# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`--use-network`**: reuses an existing Docker network for every task in an
  invocation instead of creating (and removing) a fresh one per task, matching
  Batect's flag of the same name.
  - New `ContainerRuntime::network_exists` (`ratect-core/src/docker.rs`), backed by
    `bollard`'s `inspect_network`, validates the named network up front with a clear
    error (`"The network '{name}' does not exist."`) rather than failing later with an
    unrelated Docker API error when trying to join it.
  - `TaskEngine::with_existing_network` (`ratect-core/src/engine.rs`) opts a task
    engine into reusing a network; when set, `run_task_internal` skips both
    `create_network` and `remove_network` for that network — Ratect didn't create it,
    so cleanup never removes it either, matching Batect (which only ever tears down
    networks it created itself).
- **`additional_hostnames` and `additional_hosts`**: two new per-container fields —
  `additional_hostnames` adds extra network aliases beyond a container's own name;
  `additional_hosts` adds extra `/etc/hosts` entries (Docker's own `--add-host`
  mechanism). Neither takes [expressions](docs/config-reference.md#expressions),
  matching Batect (which types both as plain strings, not `Expression`, itself).
  - New `NetworkOptions` (`ratect-core/src/docker.rs`) bundles both, passed as one
    trailing parameter to `ContainerRuntime::run_container`/
    `start_background_container` rather than two more flat ones — both methods were
    already at `#[allow(clippy::too_many_arguments)]`.
  - Also fixes a related gap found while implementing this: every container's Docker
    `hostname` is now always set to its own container name (matching Batect), not
    left as Docker's default random short container ID — previously a container was
    reachable *by* its name on the network, but `hostname`/`$HOSTNAME` *inside* it
    resolved to something unrelated.
- **`ports`, `run.ports`, and `--disable-ports`**: publishes container ports to the
  host, Docker's own `-p`/`--publish` mechanism.
  - New `ports: Option<Vec<PortMapping>>` on `Container`, accepting both of Batect's
    forms: a `"local:container[/protocol]"` string (protocol defaults to `tcp`,
    including port ranges, `"from-to:from-to[/protocol]"`) or the expanded
    `{local, container, protocol}` object form. New `config.rs::PortRange`/
    `PortMapping` types with hand-written `Deserialize` impls (accepting either form)
    validate `local`/`container` cover the same number of ports at config-load time —
    unlike `volumes`, which is never format-checked.
  - New `TaskRun.ports`: *additional* port mappings for a specific task's run, added
    to the container's own `ports` as a union (not an override — matching Batect,
    which combines these as a `Set`), via the new `engine.rs::merged_ports`.
  - `--disable-ports` suppresses publishing of every container's `ports` (both
    `Container.ports` and any `TaskRun.ports`) regardless of config, matching Batect's
    flag of the same name; `NetworkOptions` (added for `additional_hostnames`/
    `additional_hosts` above) gained a `ports` field — already-expanded
    `(local_port, container_port, protocol)` triples, via `PortMapping::expand` — so
    this stays one bundled parameter rather than a fourth flat one, and `docker.rs`
    itself never needs to parse or validate a `ports` entry.
- **Proxy environment variable propagation** (`--no-proxy-vars` to disable): detects
  `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` (either case) from the host
  environment and injects them into every container's environment and every image
  build's `build_args`, matching Batect's automatic behavior.
  - New `ratect-core/src/proxy.rs` module ports `ProxyEnvironmentVariablesProvider`/
    `ProxyEnvironmentVariablePreprocessor` in spirit: case-insensitive host lookup,
    `localhost`/`127.0.0.1`/`::1` URLs rewritten to `host.docker.internal` (macOS/
    Windows only — no automatic equivalent on Linux, and no Docker-version-gated
    hostname fallback chain the way Batect has, both accepted gaps), and every other
    container name sharing a task's network auto-appended to `no_proxy`/`NO_PROXY`.
  - Injected as the lowest-precedence layer — a container's own `environment`/
    `run.environment`, or explicit `build_args`, always override a proxy-derived value
    on a key collision.
  - New `url` dependency (`ratect-core`) for the `localhost`-rewriting URL parsing —
    already resolved transitively via `bollard`'s own dependency tree.

## [0.5.0] - 2026-07-13

### Added

- **User mapping** (`run_as_current_user`): a container can now run as the host's own
  user/group instead of the image's default (often root), so files a task writes to a
  bind-mounted volume come back owned by you, not root.
  - New `run_as_current_user: { enabled: bool, home_directory: string }` field on
    `Container` (`ratect-core/src/config.rs`), mirroring Batect's own shape exactly.
    `home_directory` is required whenever `enabled` is `true` (and rejected if given
    without it) — Ratect never guesses one. Interpolated through the existing
    expression machinery, but — unlike `build_directory` or volume host paths — *not*
    resolved against `base_path`: it's a path inside the container, validated to start
    with `/` instead.
  - This isn't just `--user uid:gid`: an arbitrary host uid/gid has no entry in the
    image's own `/etc/passwd`/`/etc/group`, which many programs need to function at
    all (no `$HOME`, no username resolution). New `ratect-core/src/user.rs` looks up
    the real host user (`nix`'s `Uid`/`Gid`/`User`/`Group`, new dependency, Unix-only)
    and generates minimal synthetic `/etc/passwd`/`/etc/shadow`/`/etc/group` content —
    ported from Batect's own `RunAsCurrentUserConfigurationProvider`, including its
    `uid == 0`/`gid == 0` special-casing so running as the current user doesn't
    produce a duplicate, conflicting `root` entry. New `docker.rs` functions
    (`build_user_mapping_tar`, `build_home_directory_tar`, both pure and
    unit-tested) build the tars uploaded into the container — via `bollard`'s
    `upload_to_container` — after it's created but before it starts.
  - Host-side bind-mount directories that don't exist yet are created *before* the
    container is even created (`ensure_host_volume_directories_exist`), as the
    current host user — otherwise Docker's daemon (running as root) would
    auto-create them as `root:root` on first use, defeating the point for the common
    "mount my code directory, get build artifacts back with sane ownership" case.
  - `ContainerRuntime::run_container`/`start_background_container` both gained a
    `user_mapping: Option<&UserMapping>` parameter; applies per-container (not
    per-task) — a task's own container and each of its dependencies can set
    `run_as_current_user` independently, matching Batect.
    `TaskEngine::resolve_user_mapping` (`ratect-core/src/engine.rs`) is the shared
    entry point, called from both `run_task_internal` and `start_dependency`.
  - New `#[ignore]`d Docker-backed test
    (`run_as_current_user_maps_the_container_onto_the_host_user`, `tests/cli.rs`) —
    writes its own temporary config at test time (rather than a static fixture,
    since it needs a *missing* host directory to exist beforehand to exercise
    pre-creation) and proves the container actually runs as the host's real uid/gid
    (compared against the test process's own `id -u`/`id -g`), and that a file it
    writes to the mounted volume comes back host-user-owned on disk, not root — the
    actual practical point of the feature, not just that the right calls were made.

## [0.4.0] - 2026-07-11

### Added

- **Interactive mode**: a task's own container now gets a real Docker TTY and its
  stdin forwarded when it's actually being run interactively (e.g. `command: sh` drops
  you into a working shell), instead of always running non-interactively with no
  stdin.
  - Fully automatic, matching Batect: no new config field, no new CLI flag. Applies
    whenever the invoked task's own container is running and Ratect's own stdin *and*
    stdout are both real terminals — falls back to today's `docker logs --follow`
    streaming otherwise (piped output, CI, redirected non-terminals). Never applies to
    a prerequisite's container, a dependency's, or a sidecar's — even though
    prerequisites are themselves full recursive task runs here, only the task actually
    named on the command line is eligible, via a new `top_level: bool` threaded
    through `TaskEngine::run_task`/the new private `run_task_scoped`
    (`ratect-core/src/engine.rs`).
  - `ContainerRuntime::run_container` (`ratect-core/src/docker.rs`) gained an
    `interactive: bool` parameter (eligibility, decided by the engine) and a new
    `should_use_tty` helper (its own unit tests) that further gates it on real
    `IsTerminal` checks. When true: the container is created with
    `tty`/`open_stdin`/`attach_stdin`/`stdin_once` set, attached to via `bollard`'s
    `attach_container` (before starting it, so no early output is lost) instead of
    `docker logs`, the local terminal is put into raw mode for the session (restored
    via a `Drop` guard, even on an error return), and stdin/stdout are pumped
    concurrently between the local terminal and the container until the session ends.
    The container's TTY size is synced to the local terminal's once, at attach time —
    not tracked live if the terminal is resized mid-session (known gap).
  - New `crossterm` dependency (`ratect-core`) for raw-mode enable/disable and
    terminal size; `std::io::IsTerminal` (stable stdlib) covers the "is this actually
    a terminal" checks, no crate needed for that part.
  - **Fixed a real hang found along the way**: `main` previously returned `ExitCode`
    from `#[tokio::main]`, which drops (and blocking-shuts-down) the Tokio runtime
    before the process actually exits — including waiting for the interactive
    session's abandoned `tokio::io::stdin()`-backed blocking read task, which never
    completes on its own (a real terminal's stdin has no natural EOF). Every
    interactive session would have hung the whole process afterward. `main` now calls
    `std::process::exit` explicitly once its own cleanup (raw-mode restoration,
    container/network teardown) has already run via ordinary `Drop`/`?`-propagation,
    bypassing that wait entirely.
  - New `portable-pty` dev-dependency and `#[ignore]`d `tests/cli.rs` test
    (`interactive_session_forwards_stdin_and_stdout`, `tests/fixtures/interactive.yml`)
    spawning `ratect` attached to a real (emulated) pseudo-terminal, scripting input,
    and asserting it round-trips through stdin → container → stdout and the process
    exits cleanly — proves the actual attach/raw-mode/pump path end-to-end (this is
    what caught the hang above), not just that the eligibility policy computes the
    right bool. Works in headless CI; no real terminal required.

## [0.3.0] - 2026-07-10

### Added

- Image building: a container with `build_directory` set now actually builds an image from a `Dockerfile` (always that name, at `build_directory`'s own root) via `bollard`'s classic (non-BuildKit) build API, instead of logging a warning and no-op'ing. New `ContainerRuntime::build_image` and free function `build_context_tar` (`ratect-core/src/docker.rs`) build an in-memory tar of the build directory, respecting a `.dockerignore` if present. Dependency containers now support `build_directory` too (previously only a task's own container could use it) — `TaskEngine::run_task_internal` and `start_dependency` (`ratect-core/src/engine.rs`) both now go through a single shared `TaskEngine::resolve_image`, which pulls or builds as needed and dedupes both (a container is only ever pulled/built once per `ratect` invocation, keyed by image name or container name respectively, via new `built_images: Mutex<HashMap<String, String>>`). Built images are tagged `<project_name>-<container_name>`, matching Batect's own convention, so they're identifiable in `docker images` instead of showing up as an opaque generated name. That tag isn't unique, though (retagged on every run) — `ContainerRuntime::build_image` now returns the image *ID* Docker's build reports back, and `resolve_image` runs/caches that ID rather than the tag, so two overlapping `ratect` invocations retagging the same name can't race each other into running the wrong image.
- `build_args` field on `Container` (`ratect-core/src/config.rs`), passed to the build as Docker's own `--build-arg` mechanism. Values support the same expression syntax as `environment` (interpolated in `resolve_expressions_with`, alongside a new `build_directory` resolution that reuses the same interpolate-then-resolve-to-absolute logic as volume host paths, now factored into a shared `resolve_path` helper).
- `.dockerignore` support: a new workspace crate, `dockerignore/`, is a from-scratch Rust port of Docker's own `.dockerignore` matching (`github.com/moby/patternmatcher`, which Docker's documentation cites as the reference implementation) — deliberately not a `.gitignore`-compatible matcher, since Docker's actual rules differ in ways confirmed against upstream's own source and test suite: most notably, a bare pattern with no wildcard (e.g. `node_modules`) only excludes it at the build context root, not at every depth, unlike `.gitignore`. No existing Rust crate implements this faithfully (two candidates checked: one's matcher is an unfinished, uncompiled stub; the other is an unmaintained 0.0.1 "primitive" from an unfamiliar publisher). Kept as its own crate (zero dependency on any ratect-specific type) rather than a `ratect-core` module, so it could be extracted and published independently later without that being decided now. Ported and verified against upstream's own ~70-case test table (`patternmatcher_test.go`'s `TestMatches`) plus its `ignorefile.ReadAll` parsing tests, both carried over as this crate's own tests. `Dockerfile` and `.dockerignore` themselves are always included in the build context regardless of exclusion patterns, matching Docker's own special-casing. `moby/patternmatcher` is Apache-2.0 licensed (same as Ratect) — new root `NOTICE` file and attribution doc comments in `dockerignore/src/lib.rs`/`pattern.rs` carry forward its own copyright/attribution notice.
- New unit tests across `dockerignore/src/pattern.rs` (the ported upstream test table, negation, root-only-for-bare-patterns behavior), `ratect-core/src/config.rs` (`build_directory`/`build_args` resolution and interpolation), `ratect-core/src/docker.rs` (`build_context_tar` — its first unit tests, since everything else there was previously only covered indirectly), and `ratect-core/src/engine.rs` (build-then-run, build dedup across tasks, `build_args` reaching the build, dependency containers with `build_directory`, the `<project_name>-<container_name>` tag format). New Docker-backed end-to-end test (`tests/fixtures/build.yml`, `tests/fixtures/build/Dockerfile`) proves `build_directory` and `build_args` reach a real `docker build`, not just that the right calls were made.
- Image build output is no longer silently lost: previously each streamed build log line only updated an ephemeral `indicatif` spinner message (never rendered on a non-TTY, e.g. CI) and a failure surfaced only Docker's own one-line `error_detail.message` — not the `RUN` step output that actually explains the failure. `DockerClient::build_image` (`ratect-core/src/docker.rs`) now logs every build log line at `debug` level as it streams (`RUST_LOG=info,ratect_core=debug` for a live transcript without unrelated `bollard` noise — see [filtering `RUST_LOG`](docs/how-it-works.md#filtering-rust_log)), and on failure folds the *entire* accumulated transcript into the returned error via a new `build_output_suffix` helper (its own unit tests), so a failing build is diagnosable without any extra flags. Ratect has no `--output` mode to stream build progress to instead, so this is deliberately the "for now" answer via the logging/error-reporting Ratect already has, not a new UI concept. New `#[ignore]`d Docker-backed test (`tests/fixtures/build-failure.yml`, `tests/fixtures/build-failure/Dockerfile`) proves a real failing build's transcript reaches Ratect's own error output, not just that `build_output_suffix` formats a string correctly in isolation.

### Fixed

- A task whose container has no `dependencies` was left running on Docker's shared default bridge network instead of an isolated one, since `TaskEngine::run_task_internal` (`ratect-core/src/engine.rs`) only created a per-task network when `dependencies` was non-empty — meaning such a task's container was reachable from, and could reach, anything else on that bridge (other unrelated containers on the host, other concurrent `ratect` runs' non-dependency containers), contrary to the isolation `docs/task-lifecycle.md` otherwise describes and to Batect's own behavior of always scoping a network per task. Every task execution now creates (and tears down) its own network unconditionally; dependency containers still only start if `dependencies` is set. `ContainerRuntime::run_container`'s `network` parameter changed from `Option<&str>` to `&str`, since a network is now always present by the time it's called.
- `resolve_path` (`ratect-core/src/config.rs`, used for `build_directory` and volume host paths) and the built-in `batect.project_directory` config variable left stray `.`/trailing-slash artifacts in resolved paths, since joining paths with `PathBuf::join` doesn't lexically normalize the result. Most visibly, running `ratect` with a bare `-f batect.yml` (no directory prefix — the common case) made `batect.project_directory` resolve to the project directory *with a trailing slash* (`/project/` instead of `/project`), since `base_path` becomes `""` in that case and `cwd.join("")` preserves it. Both now run the joined path through `path-clean`'s `.clean()` before returning it. Purely cosmetic (the paths still resolved correctly on disk either way), but user-visible in interpolated `environment`/`build_args` values and error messages. The `base_path` computation itself (`src/main.rs`) is now a small named `base_path_for` function with its own unit tests, covering the bare-filename (`""`), `./`-relative, subdirectory, and absolute cases — previously untested inline logic.

## [0.2.0] - 2026-07-09

### Added

- `environment` field on both containers and task `run`s (`ratect-core/src/config.rs`), merged when a task's own container runs (the container's values apply first, `run.environment` overrides them on a key collision) and passed through to Docker as real container environment variables. A dependency/sidecar container only ever gets its own container-level `environment`, since it has no task `run` of its own. `ContainerRuntime::run_container`/`start_background_container` gained an `environment` parameter, mapped to bollard's `ContainerCreateBody.env` via a new `build_env` helper in `ratect-core/src/docker.rs`.
- Batect expression syntax (`$VAR`, `${VAR}`, `${VAR:-default}` for host environment variables; `<name`, `<{name}` for config variables) — new `ratect-core/src/expressions.rs` module, with host-env and config-variable lookups injected as parameters rather than reading the real process environment, so resolution is deterministic and testable. An unset host variable with no `:-default` fallback, an undeclared config variable, or a declared config variable with no value from any source, are all hard errors naming the variable. Resolved within `environment` values (both containers and task `run`s) and, separately, within a volume's `host_path` — see next entry.
- Volume `host_path` interpolation: a container's `volumes` entries now run through the same expression syntax, before the existing relative-to-absolute path resolution rather than after — an expression resolving to an absolute path (e.g. a `<project_root` config variable) is used as-is rather than wrongly treated as a literal relative fragment of the config file's directory. This required moving path resolution out of `Config::load_from_file` (which runs before CLI-supplied config variable overrides are known) into the new combined `Config::resolve_expressions`/`resolve_expressions_with`, called explicitly from `main.rs` after `load_from_file`; the old separate `resolve_environment`/`resolve_paths` methods are gone, folded into this one pass (plus a new standalone `resolve_volume` helper in `ratect-core/src/config.rs`).
- `config_variables` top-level field (`ratect-core/src/config.rs`), declaring which names are resolvable via `<name`/`<{name}` and their optional `default:`. `Config::resolve_expressions` merges CLI-supplied overrides over each declared variable's `default` and runs every expression-bearing value (as above) through the expressions module.
- `--config-var NAME=VALUE` (repeatable) and `--config-vars-file PATH` CLI flags to supply config variable values, highest-precedence first: `--config-var` over `--config-vars-file` over a variable's own `default`. New `Config::load_config_vars_file` (a flat YAML map, parsed via the existing `noyalib` dependency) lives in `ratect-core`, not `main.rs`, keeping the CLI crate a thin parsing/orchestration layer per its documented architecture split.
- New `docs/config-reference.md#expressions`/`#configvariable` sections (plus an updated Volume path resolution section) and `docs/cli-reference.md` entries for the above; `docs/differences-from-batect.md` and `ROADMAP.md` updated to reflect `environment`, volume host paths, `config_variables`, and the two CLI flags as supported — `build_directory`/`build_args`/etc. remain literal-only, moot until image building itself exists.
- `batect.project_directory`, Batect's one built-in config variable (the absolute path of the directory containing the config file), resolvable via `<batect.project_directory`/`<{batect.project_directory}` without being declared under `config_variables` — in fact declaring it there, or supplying it via `--config-var`/`--config-vars-file`, is now a hard error, since it isn't meant to be overridable. Required allowing `.` in config-variable identifiers (but not host environment variable ones, which never contain dots) in `ratect-core/src/expressions.rs`'s identifier parsing, now parameterized per-sigil.
- New unit tests across `ratect-core/src/config.rs` (parsing, `resolve_expressions` merge/precedence/error cases including volume paths and the built-in variable's guard rails, `resolve_volume`'s relative-vs-absolute-after-interpolation behavior, `load_config_vars_file`), `ratect-core/src/expressions.rs` (token parsing, defaults, literal passthrough, error messages, dotted identifiers), `ratect-core/src/engine.rs` (environment reaching a task's own container vs. a dependency container, run-level override), and `src/main.rs` (the two new CLI flags). New Docker-backed end-to-end tests (`tests/fixtures/environment.yml`, `tests/fixtures/config-vars.yml`, `tests/fixtures/project-directory.yml`) prove `environment`/volume-path values — including both CLI flags' precedence and `batect.project_directory` in both bare and braced form — reach a real container's real environment, not just that the right calls were made; two new fast (non-`#[ignore]`d) `tests/cli.rs` tests cover `batect.project_directory`'s two guard-rail errors, which fail during config resolution before any Docker interaction.

### Fixed

- `unique_temp_dir()` (a test helper in `ratect-core/src/config.rs`) named scratch directories from just the process ID and a nanosecond timestamp, which could collide between tests running in parallel on platforms with coarser clock resolution, occasionally causing one test's scratch file to race with another's. Added a monotonic counter alongside them; confirmed clean across 20 repeated full-suite runs after the fix, versus an intermittent failure before it.

### Changed

- Both workspace crates (`ratect`, `ratect-core`) now sit at `0.2.0-dev`, the first commit of the 0.2.0 development cycle now that 0.1.0 is tagged. The `X.Y.Z-dev` ↔ `X.Y.Z` version bump convention itself is now documented in `ROADMAP.md`'s Versioning & Releases section and `AGENTS.md`, rather than only existing as an inferable pattern in the 0.1.0 release commit.

## [0.1.0] - 2026-07-09

### Added

- `ROADMAP.md` file outlining the path to Batect parity and future enhancements.
- Guideline in `AGENTS.md` for maintaining the changelog.
- `AGENTS.md` file providing context and instructions for AI agents working on the project.
- Initial Rust implementation of Batect core functionality.
- Support for `batect.yml` configuration parsing.
- Task execution engine with support for prerequisites and dependency cycle detection.
- Docker integration using the `bollard` library.
- Container execution with real-time log streaming.
- Automated image pulling with progress indicators.
- Support for volume mounting, including relative path resolution.
- Command-line interface with task listing (`--list-tasks`) and execution.
- Project documentation and Apache 2.0 license.
- GitHub Actions CI workflow running `cargo fmt --check`, `cargo clippy`, `cargo build`/`cargo test`, and `cargo audit` on every push and pull request.
- Unit tests for config parsing and volume path resolution (`src/config.rs`), task engine dependency-cycle detection, prerequisite dedup, and error handling via a fake `ContainerRuntime` (`src/engine.rs`), and CLI argument parsing (`src/main.rs`).
- `tests/cli.rs` integration tests covering `--list-tasks`, missing-config and no-task-name behavior, plus a Docker-backed end-to-end test (`#[ignore]`d by default, runnable via `cargo test -- --ignored`) that exercises the full sample `batect.yml` against a real daemon; wired into CI as its own `docker-integration` job.
- `ContainerRuntime` trait in `src/docker.rs` (via `async-trait`), implemented by `DockerClient`, so `TaskEngine` can be tested against a fake instead of a live Docker daemon.
- Coverage tooling via `cargo-llvm-cov`; CI generates an HTML report and uploads it as a `coverage-report` artifact for spotting untested code, without gating on a percentage.
- `docs/` directory with self-contained user documentation: installation, getting started, how it works (architecture), CLI reference, configuration reference, and a differences-from-Batect page — linked from `README.md`. Documents current gaps found in the process: `-- ADDITIONAL_ARGS` are parsed but not forwarded to the running command, `build_directory`/container `dependencies` are parsed but unimplemented, a container with neither `image` nor `build_directory` is a silent no-op, a missing config file doesn't fail the process, and container exit codes aren't currently checked.
- Itemized field-by-field and flag-by-flag comparison tables in `docs/differences-from-batect.md`, verified directly against Batect's own reference documentation (its config `overview`/`containers`/`tasks` and `cli` pages) rather than assumption — this is the detail behind the roadmap's "Full Configuration Parity" and "Full CLI Options Parity" items, and it also surfaced that Ratect silently ignores unsupported config keys instead of rejecting them.
- Sidecar/dependency container support (`Container.dependencies`, previously parsed but unused): dependencies are started (recursively, for nested dependencies) before a task's own container, reachable by name over a Docker network created and torn down for that single task execution. Deduped within one task's dependency resolution; not shared across tasks — each task execution gets its own instance and network, matching Batect's documented behavior. `ContainerRuntime` gained `create_network`, `remove_network`, `start_background_container`, and `stop_and_remove_container`; `run_container` now takes a `name`/`network` pair so a task's own container can join its dependencies' network. New `uuid` dependency for collision-resistant network naming (process ID was considered and rejected — it's frequently `1` when `ratect` runs inside a container, e.g. CI). No `health_check`/`setup_commands` support, so a dependency counts as ready as soon as it starts; see `docs/task-lifecycle.md` (new) for the full model with diagrams, and `docs/differences-from-batect.md` for what's simplified relative to Batect. New `tests/fixtures/sidecar.yml` fixture (two sibling dependencies plus one nested behind them) and ignored Docker integration test prove real cross-container DNS resolution for both siblings and nesting together, not just that the right calls were made. Unit tests cover nesting to four levels deep, within-task dedup of a dependency shared by multiple siblings (asserting each sibling itself started, not just the shared one), cross-task isolation, and circular-dependency detection.

### Fixed

- `ROADMAP.md` incorrectly listed `--project-name` as an example Batect CLI flag; it's actually a `batect.yml` config field, not a CLI option. Corrected and cross-linked to the itemized flag table in `docs/differences-from-batect.md`.
- Fatal errors (malformed config, missing task/container, dependency cycle) previously bypassed `tracing` entirely, propagating to `main`'s default `Result` handler and printing via `anyhow`'s raw `Debug` formatting — inconsistent with every other diagnostic message, and unaffected by `RUST_LOG`. `main` now returns `ExitCode` and routes the final error through `tracing::error!` like everything else.
- Container exit codes weren't checked: a task whose command exited non-zero was still reported as successful, and dependent tasks still ran. `run_container` now waits for the container and checks its exit status (via `wait_container`, falling back to bollard's `DockerContainerWaitError` for non-zero codes); a non-zero exit raises a new `ContainerExitedNonZero` error, and `main` propagates the *exact* exit code as `ratect`'s own process exit code (matching `docker run`'s convention), rather than a generic failure code. This also means a failing prerequisite now correctly stops the rest of the chain, matching Batect's documented behavior. New ignored Docker integration tests (`tests/fixtures/exit-code.yml`) prove both the exact-code propagation and the prerequisite-chain-stops behavior against a real daemon; a new unit test proves dependency/network cleanup still happens even when the main container fails.
- A missing config file exited `0` instead of failing, for both `--list-tasks` and running a task. `run()` now checks for the config file up front and fails fast with a non-zero exit before branching into either mode, which also removed the duplicated `Option<Config>` handling that existed to work around the old behavior. (Running with no task name at all, unrelated to this fix, still intentionally exits `0` with a warning — see `docs/cli-reference.md`.)
- `-- ADDITIONAL_ARGS` was parsed but silently dropped, never reaching the task's command. `TaskEngine::run_task` and `ContainerRuntime::run_container` now thread the args through, scoped to only the explicitly-requested task (never its prerequisites, matching Batect). Since `command` always runs via `sh -c`, args are appended as `sh -c`'s own positional parameters (`$1`, `$2`, `$@` — `sh -c '<command>' sh arg1 arg2 ...`) rather than concatenated into the command string, so they're never re-parsed as shell syntax regardless of what characters they contain (verified with an arg containing `;`, `&&`, and backticks against a real daemon). If a container has no `command` at all, non-empty additional args are passed directly as its argv instead, matching plain `docker run <image> <args>`. New `tests/fixtures/additional-args.yml` and an ignored Docker integration test prove real forwarding end to end; a new unit test proves prerequisites never receive the args.
- Unsupported `batect.yml` keys were silently ignored instead of raising an error — a typo'd or not-yet-implemented field (e.g. `environment` on a container) would load without complaint and just silently do nothing. `Config`, `Container`, `Task`, and `TaskRun` now derive `#[serde(deny_unknown_fields)]`, so any unrecognized key fails config loading with an error naming the field. This closes the last of the four `ratect-compat` 0.1.0 correctness gaps in `ROADMAP.md`. Deliberately implemented via `#[serde(deny_unknown_fields)]` on plain `noyalib::from_reader`, not `noyalib::from_reader_strict`/`from_str_strict`: `noyalib` 0.0.13's strict-mode path deserializes through its `Value` type, whose `Deserializer` impl forwards `deserialize_option` straight to `deserialize_any` — which breaks (`invalid type: string "...", expected option`) on every *populated* `Option` field, i.e. almost every field in this schema. `deny_unknown_fields` on the regular streaming deserializer doesn't hit that path and works correctly. New unit test `load_from_file_unsupported_key_errors` proves it, plus a `tests/fixtures/unsupported-key.yml` fixture and `unsupported_config_key_reports_error` CLI test proving the end-to-end process behavior (non-zero exit, field name on stderr).
- A task's own container with neither `image` nor `build_directory` set silently did nothing and still exited `0`, as if the task had succeeded — unlike dependency/sidecar containers, which already errored in this situation via `start_dependency`. `run_task_internal` now raises the same class of error for the main task container, naming the container. New unit test `container_without_image_or_build_directory_errors`, plus `tests/fixtures/no-image.yml` and `container_without_image_or_build_directory_reports_error` CLI test.

### Changed

- Added a "Versioning & Releases" section to `ROADMAP.md`: `ratect-compat` and `ratect` are versioned independently (different maturity clocks), but shared-core bug fixes/security patches get a coordinated release for both regardless. Defines `ratect-compat`'s 0.1.0 as an honesty milestone (fix the known correctness gaps in `docs/differences-from-batect.md` — exit codes not checked, missing config exits `0`, dropped `-- ADDITIONAL_ARGS`, silently-ignored unsupported keys) rather than a features milestone, plans 0.2.0 through 0.6.0 (environment variables/expressions, image building, interactive mode, user mapping, then networking/proxy support — sequenced so later items can reuse earlier ones, e.g. proxy support building on 0.2.0's environment variable support), notes what's left beyond that (includes, the long tail of smaller config/CLI fields) isn't optional for 1.0.0 even though it's not release-planned yet, and ties 1.0.0 for each binary to its own definition of "done" (Batect parity vs. interface stability).
- Converted the project into a Cargo workspace: extracted `config.rs`/`docker.rs`/`engine.rs` (and their tests) into a new `ratect-core` library crate, leaving the `ratect` binary crate as thin CLI glue (`src/main.rs` only) over `ratect-core`'s public API. Pure refactor, no behavior change — sets up the [two-binary plan](ROADMAP.md#two-binaries-ratect-and-ratect-compat) (a future `ratect-compat` and `ratect` sharing this same core) without committing to the rename or building the second binary yet. CI now runs `--workspace` variants of build/test/clippy/coverage.
- Restructured `ROADMAP.md`'s CLI plan from a two-phase single-binary evolution (with eventual deprecation of Batect-compatible flags) into two permanent binaries sharing one core: `ratect-compat` (strict Batect CLI/YAML parity, the target for all "Batect Parity" roadmap items) and `ratect` (a free-to-diverge modern CLI, not required to maintain Batect parity). Ratect will not ship a binary literally named `batect`, to avoid confusion/trademark concerns; a drop-in `./batect` replacement is achieved by the user symlinking or renaming `ratect-compat` themselves. Also added an undecided/exploratory TOML-as-alternative-config-format item to "Future Vision", scoped to the `ratect` binary only.
- Updated project version to `0.1.0-dev` to reflect pre-release status.
- Migrated YAML parsing from `serde_yaml` to `noyalib` for improved safety and maintenance.
- Upgraded core dependencies to their latest stable versions.
- `Cargo.lock` is now committed to the repository (previously gitignored), following the convention for binary crates to ensure reproducible builds and accurate dependency audits.
- Applied `cargo fmt` formatting across `src/`.
- Wired up `tracing`/`tracing-subscriber`: task lifecycle, unimplemented-feature, and config-error diagnostics now go through leveled, `RUST_LOG`-filterable log events on stderr, while command output (task listing, container log streaming) remains on stdout via `println!`/`print!`.
