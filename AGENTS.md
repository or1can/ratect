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
    context is the stand-in.
  - **`ratect-core/src/engine.rs`**: The core execution logic. It manages the task
    lifecycle, handles prerequisites, detects dependency cycles, resolves and starts a
    task's dependency/sidecar containers (recursively, deduped and cleaned up within
    that one task's execution — see [`docs/task-lifecycle.md`](docs/task-lifecycle.md)),
    and ensures that each task, image pull, and image build occurs only once per
    session. `TaskEngine::resolve_image` is the single place that turns a container's
    `image`/`build_directory` into the image reference to actually run (pulling or
    building as needed, deduped via `pulled_images`/`built_images`), shared by both a
    task's own container and its dependency containers. `TaskEngine` is generic over
    `ContainerRuntime`.
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

Dependencies are split across the three `Cargo.toml`s along CLI-vs-core lines: `clap`
and `tracing-subscriber` are `ratect`-only; `serde`, `noyalib`, `bollard`, `futures`,
`indicatif`, `async-recursion`, `async-trait`, `uuid`, `tar`, `path-clean`, and the local
`dockerignore` crate are `ratect-core`-only (`dockerignore` itself depends on `regex`
and `path-clean` too); `anyhow`, `tracing`, and `tokio` are needed by both. `tokio`
is a normal dependency in both crates now — `ratect-core`'s non-test code needs it too,
for `build_context_tar`'s `tokio::task::spawn_blocking` (it used to be a `ratect-core`
dev-dependency only, for `#[tokio::test]` in its unit tests).

## Tooling & CI

- **Formatting/Linting**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored). One shared lockfile covers both crates.
- **Tests**: `cargo test --workspace` runs in CI, covering pattern matching (`dockerignore/src/pattern.rs`, verified against upstream `moby/patternmatcher`'s own test table), config parsing/resolution (`ratect-core/src/config.rs`, including `resolve_expressions`'s merge/precedence/error cases and `build_directory`/`build_args`), expression interpolation (`ratect-core/src/expressions.rs`), build-context tar construction (`ratect-core/src/docker.rs`'s `build_context_tar` — `.dockerignore` inclusion/exclusion, including the root-only-for-bare-patterns behavior, and the always-included `Dockerfile`/`.dockerignore` special case), task engine logic including dependency-cycle detection, prerequisite dedup, sidecar/dependency-container resolution (nesting, within-task dedup, cross-task isolation, circular-dependency detection), environment variable merging (container vs. task `run`, dependency containers), and image resolution (pull vs. build, build dedup, `build_args` reaching the build — `ratect-core/src/engine.rs`, via a fake `ContainerRuntime`), and CLI argument/behavior (`src/main.rs`, `tests/cli.rs`). `tests/cli.rs` also has end-to-end tests (`#[ignore]`d by default) that run against a real Docker daemon — the sample `batect.yml`, `tests/fixtures/sidecar.yml` (proves real cross-container DNS resolution), `tests/fixtures/environment.yml`/`config-vars.yml`/`project-directory.yml` (prove `environment`/config variable/`batect.project_directory` values reach a real container), `tests/fixtures/build.yml`/`build/Dockerfile` (proves `build_directory`/`build_args` reach a real `docker build`), `tests/fixtures/build-with-dockerignore.yml` (proves `.dockerignore` semantics hold against real Docker), and `tests/fixtures/build-failure.yml`/`build-failure/Dockerfile` (proves a real failing build's full transcript, not just Docker's one-line summary, reaches Ratect's own error output) — not just that the right calls were made — run them explicitly with `cargo test --workspace --test cli -- --ignored`; they also run as their own `docker-integration` CI job.
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
