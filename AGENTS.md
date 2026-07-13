# Ratect AI Agent Guide

This file provides context, instructions, and guidelines for AI agents working on the Ratect project.

## Project Overview

Ratect is a Rust-based implementation of the [Batect](https://github.com/batect/batect) task execution engine. Its goal is to provide a fast, lightweight CLI for running development tasks in Docker containers, defined via a `batect.yml` file.

## Architecture

Ratect is a **Cargo workspace** with three crates today, and a fourth planned (see the
[two-binary roadmap item](ROADMAP.md#two-binaries-ratect-and-ratect-compat)):

- **`ratect`** (root package, `src/main.rs` only): the CLI binary. Handles argument
  parsing (via `clap`) and orchestrates the high-level flow (loading config,
  initializing the Docker client, starting the engine) by calling into `ratect-core`.
  Nothing else lives here — this crate is deliberately thin, since it's the part that
  will eventually be forked into two different CLIs (`ratect-compat` and `ratect`)
  sharing the same `ratect-core`.
- **`ratect-core`** (library crate, `ratect-core/src/`): all the reusable logic, with
  no CLI-specific code. This is what any future second binary would also depend on.
  - **`ratect-core/src/config.rs`**: Data models for the configuration (`batect.yml`).
    Uses `noyalib` for YAML parsing. `Config::load_from_file` only parses; a separate
    `Config::resolve_expressions` call (needs CLI-supplied `--config-var`/
    `--config-vars-file` overrides, so it can't happen inside `load_from_file`)
    interpolates `environment` values, volume host paths, `build_directory`, and
    `build_args` via `expressions.rs`, then resolves relative volume host paths and
    `build_directory` to absolute ones (shared `resolve_path` helper).
    `run_as_current_user.home_directory` is interpolated too, but *not* resolved
    against `base_path` (it's a path inside the container, not the host) — validated
    to start with `/` instead, and required whenever `run_as_current_user.enabled` is
    `true` (rejected if given without it), matching Batect's own two validation
    errors.
  - **`ratect-core/src/expressions.rs`**: Batect's expression syntax (`$VAR`, `${VAR}`,
    `${VAR:-default}` for host environment variables; `<name`, `<{name}` for config
    variables, including the built-in `batect.project_directory`). A single
    `interpolate` function, with the host environment and resolved config variable
    values injected as parameters rather than read from the real process environment,
    so resolution is deterministic and unit-testable.
  - **`ratect-core/src/docker.rs`**: A wrapper around the `bollard` library. It manages
    interactions with the Docker daemon: pulling images, building images from a
    `build_directory` (via `build_context_tar`, `.dockerignore`-aware using the
    `dockerignore` crate — its own unit tests, since everything else here is only
    covered indirectly via the `ContainerRuntime` fake or real Docker in
    `tests/cli.rs`), creating/starting/streaming/removing the task's own container, and
    creating/removing a per-task network plus starting/stopping background
    (sidecar/dependency) containers on it. Exposes a `ContainerRuntime` trait
    (implemented by `DockerClient`) so the engine can be tested against a fake instead
    of a live daemon. A build's streamed log lines are logged at `debug` level as they
    arrive and, on failure, the full accumulated transcript (not just Docker's one-line
    `error_detail.message`) is folded into the returned error via `build_output_suffix`
    — Ratect has no `--output` mode to stream them to otherwise, so logging/error
    context is the stand-in. `run_container` takes one of two paths depending on
    `should_use_tty` (an `interactive` eligibility bool the engine decided, ANDed
    against real `IsTerminal` checks on Ratect's own stdin/stdout): the default path is
    unchanged (`docker logs --follow`); the interactive path (`run_container_interactively`)
    attaches to the container (`bollard`'s `attach_container`, before starting it),
    puts the local terminal into raw mode via a `RawModeGuard` (restored on `Drop`,
    even on an error return), and pumps stdin/stdout between the local terminal and the
    container concurrently until the session ends — see
    [Interactive mode](docs/config-reference.md#interactive-mode). When the engine
    passes a `user_mapping: Some(UserMapping)` (`run_as_current_user` enabled), both
    `run_container` and `start_background_container` pre-create missing host volume
    directories, set the container's `User` to the mapped `uid:gid`, and — after
    creation, before starting — upload synthetic passwd/shadow/group files and the
    home directory via `apply_user_mapping`/`docker.upload_to_container` — see
    [User mapping](docs/config-reference.md#user-mapping). `network_exists` (backed by
    `inspect_network`) validates `--use-network`'s target up front. `NetworkOptions`
    bundles `additional_hostnames`/`additional_hosts`/`ports` — introduced as one
    trailing parameter to `run_container`/`start_background_container` rather than
    three more flat ones, since both were already at
    `#[allow(clippy::too_many_arguments)]`. Every container's Docker `hostname` is set
    to its own container name. `parse_port_mapping`/`build_port_config` are pure
    functions parsing `ports`' `"local:container[/protocol]"` strings into
    `Config.exposed_ports`/`HostConfig.port_bindings`.
  - **`ratect-core/src/user.rs`**: Host user lookup (`current_user`, via the `nix`
    crate's `Uid`/`Gid`/`User`/`Group` — Unix-only, `cfg(not(unix))` errors clearly
    rather than guessing) and the pure `/etc/passwd`/`/etc/shadow`/`/etc/group`
    content generators (`generate_passwd_file`/etc.) `docker.rs`'s
    `build_user_mapping_tar` uses — ported from Batect's
    `RunAsCurrentUserConfigurationProvider`, including its `uid == 0`/`gid == 0`
    special-casing so running as the current user doesn't produce a duplicate
    conflicting `root` entry.
  - **`ratect-core/src/proxy.rs`**: Proxy environment variable detection/propagation
    (`--no-proxy-vars` to disable) — ported from Batect's
    `ProxyEnvironmentVariablesProvider`/`ProxyEnvironmentVariablePreprocessor`.
    `proxy_environment_variables` detects `http_proxy`/`https_proxy`/`ftp_proxy`/
    `no_proxy` case-insensitively from a `host_env` closure (parameterized the same
    way `expressions.rs`/`config.rs` are, for deterministic unit tests) and appends an
    `extra_no_proxy_entries` set to `no_proxy`/`NO_PROXY`. `preprocess_proxy_value`
    rewrites `localhost`/`127.0.0.1`/`::1` URLs to `host.docker.internal`
    (`docker_host_name`, macOS/Windows only — `None` on Linux) via the `url` crate.
  - **`ratect-core/src/engine.rs`**: The core execution logic. It manages the task
    lifecycle, handles prerequisites, detects dependency cycles, resolves and starts a
    task's dependency/sidecar containers (recursively, deduped and cleaned up within
    that one task's execution — see [`docs/task-lifecycle.md`](docs/task-lifecycle.md)),
    and ensures that each task, image pull, and image build occurs only once per
    session. `TaskEngine::resolve_image` is the single place that turns a container's
    `image`/`build_directory` into the image reference to actually run (pulling or
    building as needed, deduped via `pulled_images`/`built_images`), shared by both a
    task's own container and its dependency containers. `TaskEngine::resolve_user_mapping`
    similarly turns a container's `run_as_current_user` into a `UserMapping` when
    enabled — also shared by both call sites, but *not* deduped/cached the way
    `resolve_image` is: it's called (and `user::current_user()` looked up) fresh per
    container, since there's only ever one real host user per process, so recomputing
    it is cheap and simpler than a memoization layer would be. `TaskEngine` is
    generic over `ContainerRuntime`. The public `run_task` is a thin wrapper fixing a
    private,
    recursive `run_task_scoped`'s `top_level: bool` to `true`; prerequisites recurse
    into it with `top_level: false` — this is what decides interactive-TTY eligibility
    (only the task actually named on the command line, never a prerequisite's
    container, is ever passed `interactive: true`). `existing_network`/`publish_ports`/
    `propagate_proxy_environment_variables`/`host_env` are opt-in settings set via
    builder methods (`with_existing_network`, `without_port_publishing`,
    `without_proxy_environment_variables`) rather than `TaskEngine::new` parameters —
    several were added across separate 0.6.0 commits, and a builder means each one
    lands without another mass-edit of the ~30 existing `TaskEngine::new` call sites
    (mostly tests). `container_names_in_task` computes the `no_proxy` exemption set
    (every container sharing one task's network) once per task, fixed for every
    container started within it — matching Batect's `allContainersInNetwork`.
- **`dockerignore`** (library crate, `dockerignore/src/`): a from-scratch Rust port of
  Docker's own `.dockerignore` matching (`github.com/moby/patternmatcher`, which
  Docker's documentation cites as the reference implementation) — deliberately **not**
  a `.gitignore`-compatible matcher, since Docker's actual rules differ in confirmed,
  non-obvious ways (e.g. a bare pattern with no wildcard only excludes at the build
  context root, not at every depth). No dependency on any ratect-specific type, kept as
  its own crate rather than a `ratect-core` module specifically so it could be
  extracted and published independently later — not committed to yet. Verified against
  upstream's own test suite, carried over as this crate's tests. `moby/patternmatcher`
  is Apache-2.0 licensed (same as Ratect) — see this repo's [`NOTICE`](NOTICE) file and
  the attribution doc comments at the top of `dockerignore/src/lib.rs` and
  `dockerignore/src/pattern.rs`.

## Key Dependencies

- **`bollard`**: Asynchronous Docker API client.
- **`noyalib`**: Safe, pure-Rust YAML parser (used as a modern alternative to `serde_yaml`).
- **`tokio`**: The asynchronous runtime.
- **`clap`**: Command-line argument parsing with derive support.
- **`indicatif`**: Used for displaying progress bars during image pulls.
- **`anyhow`**: Simplified error handling with context.
- **`tracing` / `tracing-subscriber`**: Structured, leveled logging. The subscriber is initialized in `main.rs`, filtered via `RUST_LOG` (defaults to `info`), and writes to stderr.
- **`async-trait`**: Used for the `ContainerRuntime` trait in `ratect-core/src/docker.rs`, so it can have async methods and be implemented by both the real `DockerClient` and test fakes.
- **`uuid`**: Generates collision-resistant per-task Docker network names (`ratect-<uuid>`) in `ratect-core/src/engine.rs`. Deliberately not `std::process::id()` — that's frequently `1` when `ratect` itself runs inside a container (e.g. CI), which would collide across concurrent runs. Built images are tagged `<project_name>-<container_name>` instead (human-readable, matching Batect's convention) — `resolve_image` avoids the same collision hazard for these not via a random name but by running the image *ID* Docker's build reports back, not the (non-unique) tag.
- **`tar`**: Builds the in-memory build-context tarball `docker.rs`'s `build_context_tar` hands to `bollard`'s `build_image`.
- **`dockerignore`** (local workspace crate, not external): `.dockerignore` pattern matching — see the Architecture section above.
- **`path-clean`**: Lexically normalizes (`.`/`..`/trailing-slash) resolved paths in `ratect-core/src/config.rs` (`resolve_path`, and the built-in `batect.project_directory` config variable) — `PathBuf::join` alone doesn't do this, so without it a `base_path` like `""` or `"."` (both common — see `main.rs`'s `-f` handling) would leave a stray `.` or trailing slash in every path/expression derived from it. Already a `dockerignore` dependency; reused here rather than hand-rolling the same normalization twice.
- **`crossterm`**: Raw-mode terminal enable/disable and terminal size queries for interactive mode's attach path (`ratect-core/src/docker.rs`). Deliberately not used for its structured `event`/`EventStream` API — that's for TUI-style key/mouse/resize events and would consume/interpret stdin bytes instead of passing them through raw. `std::io::IsTerminal` (stable stdlib) covers the separate "is this actually a terminal" checks; no crate needed for that part.
- **`portable-pty`** (dev-dependency, `tests/cli.rs` only): creates a real (emulated) pseudo-terminal pair in-process, so an integration test can spawn `ratect` attached to something that genuinely passes `IsTerminal` checks and actually drive an interactive session — no existing test infrastructure here could otherwise exercise that path at all. Works in headless CI; no real terminal required. A reusable pattern worth reaching for again for any other feature that's only meaningfully testable from a real terminal.
- **`nix`** (`features = ["user"]`): looks up the real host user (`Uid`/`Gid::current`, `User`/`Group::from_uid`/`from_gid`) for `run_as_current_user` (`ratect-core/src/user.rs`) — Unix-only, matching Ratect's own Unix-only testing so far. Already resolved in `Cargo.lock` transitively (via `portable-pty`'s own dependency graph in the root crate's dev-dependencies); adding it directly to `ratect-core` was a low-risk addition, not a new unknown quantity.
- **`url`**: parses/rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal` in `ratect-core/src/proxy.rs`. Already resolved in `Cargo.lock` transitively (via `bollard`'s own dependency graph); adding it directly to `ratect-core` was a low-risk addition, not a new unknown quantity — same reasoning as `nix` above.

Dependencies are split across the three `Cargo.toml`s along CLI-vs-core lines: `clap`
and `tracing-subscriber` are `ratect`-only; `serde`, `noyalib`, `bollard`, `futures`,
`indicatif`, `async-recursion`, `async-trait`, `uuid`, `tar`, `path-clean`, `crossterm`,
`nix`, `url`, and the local `dockerignore` crate are `ratect-core`-only (`dockerignore` itself
depends on `regex` and `path-clean` too); `anyhow`, `tracing`, and `tokio` are needed
by both. `tokio` is a normal dependency in both crates now — `ratect-core`'s non-test
code needs it too, for `build_context_tar`'s `tokio::task::spawn_blocking` (it used to
be a `ratect-core` dev-dependency only, for `#[tokio::test]` in its unit tests).
`portable-pty` is `ratect`'s (root crate's) first `[dev-dependencies]` entry.

## Tooling & CI

- **Formatting/Linting**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored). One shared lockfile covers both crates.
- **Tests**: `cargo test --workspace` runs in CI, covering pattern matching (`dockerignore/src/pattern.rs`, verified against upstream `moby/patternmatcher`'s own test table), config parsing/resolution (`ratect-core/src/config.rs`, including `resolve_expressions`'s merge/precedence/error cases and `build_directory`/`build_args`), expression interpolation (`ratect-core/src/expressions.rs`), build-context tar construction (`ratect-core/src/docker.rs`'s `build_context_tar` — `.dockerignore` inclusion/exclusion, including the root-only-for-bare-patterns behavior, and the always-included `Dockerfile`/`.dockerignore` special case), container `cmd` construction (`build_cmd` — command-with/without-additional-args, and the `command: None` entrypoint-passthrough case, which must stay `None` rather than an empty `Vec` so bollard/Docker falls back to the image's own default), interactive-TTY eligibility (`should_use_tty`'s four combinations in `docker.rs`; `only_the_top_level_tasks_own_container_run_is_interactive_eligible`/`prerequisite_tasks_own_container_is_never_interactive` in `engine.rs`, via a fake `ContainerRuntime` that captures the `interactive` bool a `run_container` call was given), host-user lookup and passwd/shadow/group generation (`ratect-core/src/user.rs`, both for a normal uid/gid and separately for uid/gid `0`, ported from Batect's own generator logic), user-mapping tar construction and host-directory pre-creation (`build_user_mapping_tar`/`build_home_directory_tar`/`ensure_host_volume_directories_exist` in `docker.rs`), `run_as_current_user` reaching a task's own container and independently reaching a dependency's (`engine.rs`, via a fake `ContainerRuntime` that captures the `(uid, gid, home_directory)` a call was given), task engine logic including dependency-cycle detection, prerequisite dedup, sidecar/dependency-container resolution (nesting, within-task dedup, cross-task isolation, circular-dependency detection), environment variable merging (container vs. task `run`, dependency containers), and image resolution (pull vs. build, build dedup, `build_args` reaching the build — `ratect-core/src/engine.rs`, via a fake `ContainerRuntime`), and CLI argument/behavior (`src/main.rs`, `tests/cli.rs`). `tests/cli.rs` also has end-to-end tests (`#[ignore]`d by default) that run against a real Docker daemon — the sample `batect.yml`, `tests/fixtures/sidecar.yml` (proves real cross-container DNS resolution), `tests/fixtures/environment.yml`/`config-vars.yml`/`project-directory.yml` (prove `environment`/config variable/`batect.project_directory` values reach a real container), `tests/fixtures/build.yml`/`build/Dockerfile` (proves `build_directory`/`build_args` reach a real `docker build`), `tests/fixtures/build-with-dockerignore.yml` (proves `.dockerignore` semantics hold against real Docker), `tests/fixtures/build-failure.yml`/`build-failure/Dockerfile` (proves a real failing build's full transcript, not just Docker's one-line summary, reaches Ratect's own error output), `tests/fixtures/interactive.yml` (`interactive_session_forwards_stdin_and_stdout` — spawns `ratect` attached to a `portable-pty`-emulated pseudo-terminal, scripts input, and asserts it round-trips through stdin → container → stdout and the process exits cleanly; this is the test that caught a real hang (`main` waiting forever on an abandoned `tokio::io::stdin()` read task after every interactive session — see the "Interactive mode" entry's own sub-bullet under `[Unreleased]` in `CHANGELOG.md`) before it shipped), and `run_as_current_user_maps_the_container_onto_the_host_user` (writes its own temporary config at test time, rather than a static fixture, since it needs a *missing* host directory to exist beforehand to exercise pre-creation — asserts the container's `id -u`/`id -g` match this test process's own, and that a file it writes to the mounted volume comes back host-user-owned on disk, not root), `tests/fixtures/additional-hostnames-and-hosts.yml` (proves a dependency's `additional_hostnames` alias, a container's `additional_hosts` entry, and the container-name-as-hostname fix all reach a real container), `tests/fixtures/ports.yml` (`ports_publishes_a_container_port_to_the_host`/`disable_ports_flag_suppresses_port_publishing` — spawns `ratect` rather than waiting on `.output()`, since the task's own `sleep 5` command needs to stay running long enough for the host-side test to poll the published port via a raw `TcpStream::connect_timeout`; uses separate containers/ports from each other so the two can run concurrently), `tests/fixtures/proxy.yml` (`proxy_environment_variables_are_propagated_into_the_container`/`no_proxy_vars_flag_disables_propagation` — sets `http_proxy`/`no_proxy` on `ratect`'s own spawned-process environment, not the container's, proving the host-env-to-container injection and the automatic `no_proxy` container-name exemption both reach a real container), and `use_network_reuses_an_existing_docker_network`/`use_network_errors_clearly_for_a_nonexistent_network` (`docker network create`d ahead of time via the `docker` CLI directly) — not just that the right calls were made — run them explicitly with `cargo test --workspace --test cli -- --ignored`; they also run as their own `docker-integration` CI job.
- **Coverage**: `cargo llvm-cov --workspace --show-missing-lines --summary-only` (requires `rustup component add llvm-tools-preview` and `cargo install cargo-llvm-cov`) reports exact uncovered lines per file — use it to find gaps, not to chase a percentage. `cargo llvm-cov --workspace --html` opens a browsable report at `target/llvm-cov/html`. CI runs this and uploads the HTML report as a `coverage-report` artifact (non-gating).

## Current Status & Roadmap

Ratect is currently a **Work in Progress**. For a detailed list of supported features and our future plans, please refer to the [ROADMAP.md](ROADMAP.md) file.

## User Documentation

The `docs/` directory is user-facing documentation (installation, getting started, architecture, CLI reference, config reference, differences from Batect) — **not** ROADMAP.md/AGENTS.md/CHANGELOG.md, which are project-management/contributor docs. `docs/` deliberately does not assume familiarity with Batect's own documentation, since Ratect's behavior is a subset of and sometimes diverges from it.

## Guidelines for AI Agents

1.  **Idiomatic Rust**: Always strive for idiomatic and safe Rust. Use `anyhow::Context` to provide meaningful error messages.
2.  **Async/Await**: The codebase is heavily asynchronous. Ensure new I/O or Docker-related code uses `await` and integrates with the `tokio` runtime.
3.  **Dependency Management**: Keep each `Cargo.toml` clean and dependencies updated — and in the right crate (CLI-only deps in `ratect`'s `Cargo.toml`, everything else in `ratect-core`'s). If a library becomes deprecated or unmaintained, propose a migration to a better alternative.
4.  **Configuration Consistency**: When extending the `batect.yml` parser in `ratect-core/src/config.rs`, try to maintain compatibility with the original Batect configuration format.
5.  **State Management**: In `ratect-core/src/engine.rs`, state (like executed tasks) is shared using `Mutex` to ensure thread safety across async tasks. Be mindful of locking logic.
6.  **Verification**: After making changes, verify them by:
    -   Running `cargo build --workspace` to ensure compilation.
    -   Executing `cargo run -- --list-tasks` to check config parsing.
    -   Running a sample task (e.g., `cargo run -- test-task`) to verify the execution engine and Docker integration.
7.  **Changelog Maintenance**: After completing a task that changes the project's features, dependencies, or structure, ensure that `CHANGELOG.md` is updated in the "Unreleased" section, following the "Keep a Changelog" standard.
7a. **Version Lifecycle**: When cutting a release, it's not just a version bump — follow the full process documented in [ROADMAP.md](ROADMAP.md#versioning--releases): the `X.Y.Z-dev` → `X.Y.Z` bump commit, tagging it `vX.Y.Z`, and publishing it as a GitHub Release (body = that version's `CHANGELOG.md` section). Starting the next version's development is a separate, later commit that bumps back to the next `X.Y.Z-dev`. Neither bump is ever folded into a feature commit.
8.  **User Docs Maintenance**: When a change affects user-visible behavior (CLI flags, config schema, runtime behavior, Batect parity), update the relevant file(s) under `docs/` in the same change — don't let them drift from the code. If you find the code doesn't match what's documented, fix whichever one is wrong rather than leaving the mismatch.
9.  **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, error conditions) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout.
10. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff.
