# Ratect AI Agent Guide

This file provides context, instructions, and guidelines for AI agents working on the Ratect project.

## Project Overview

Ratect is a Rust-based implementation of the [Batect](https://github.com/batect/batect) task execution engine. Its goal is to provide a fast, lightweight CLI for running development tasks in Docker containers, defined via a `batect.yml` file.

## Architecture

Ratect is a **Cargo workspace** with two crates today, and a third planned (see the
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
    Uses `noyalib` for YAML parsing and includes logic for resolving relative paths in
    volume mounts.
  - **`ratect-core/src/docker.rs`**: A wrapper around the `bollard` library. It manages
    interactions with the Docker daemon: pulling images, creating/starting/streaming/
    removing the task's own container, and creating/removing a per-task network plus
    starting/stopping background (sidecar/dependency) containers on it. Exposes a
    `ContainerRuntime` trait (implemented by `DockerClient`) so the engine can be
    tested against a fake instead of a live daemon.
  - **`ratect-core/src/engine.rs`**: The core execution logic. It manages the task
    lifecycle, handles prerequisites, detects dependency cycles, resolves and starts a
    task's dependency/sidecar containers (recursively, deduped and cleaned up within
    that one task's execution — see [`docs/task-lifecycle.md`](docs/task-lifecycle.md)),
    and ensures that each task and image pull occurs only once per session.
    `TaskEngine` is generic over `ContainerRuntime`.

## Key Dependencies

- **`bollard`**: Asynchronous Docker API client.
- **`noyalib`**: Safe, pure-Rust YAML parser (used as a modern alternative to `serde_yaml`).
- **`tokio`**: The asynchronous runtime.
- **`clap`**: Command-line argument parsing with derive support.
- **`indicatif`**: Used for displaying progress bars during image pulls.
- **`anyhow`**: Simplified error handling with context.
- **`tracing` / `tracing-subscriber`**: Structured, leveled logging. The subscriber is initialized in `main.rs`, filtered via `RUST_LOG` (defaults to `info`), and writes to stderr.
- **`async-trait`**: Used for the `ContainerRuntime` trait in `ratect-core/src/docker.rs`, so it can have async methods and be implemented by both the real `DockerClient` and test fakes.
- **`uuid`**: Generates collision-resistant per-task Docker network names (`ratect-<uuid>`) in `ratect-core/src/engine.rs`. Deliberately not `std::process::id()` — that's frequently `1` when `ratect` itself runs inside a container (e.g. CI), which would collide across concurrent runs.

Dependencies are split across the two `Cargo.toml`s along CLI-vs-core lines: `clap` and
`tracing-subscriber` are `ratect`-only; `serde`, `noyalib`, `bollard`, `futures`,
`indicatif`, `async-recursion`, `async-trait`, and `uuid` are `ratect-core`-only;
`anyhow`, `tracing`, and `tokio` are needed by both (`tokio` is a normal dependency in
`ratect`, for `#[tokio::main]`, but only a dev-dependency in `ratect-core`, for
`#[tokio::test]` in its unit tests).

## Tooling & CI

- **Formatting/Linting**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored). One shared lockfile covers both crates.
- **Tests**: `cargo test --workspace` runs in CI, covering config parsing/path resolution (`ratect-core/src/config.rs`), task engine logic including dependency-cycle detection, prerequisite dedup, and sidecar/dependency-container resolution (nesting, within-task dedup, cross-task isolation, circular-dependency detection — `ratect-core/src/engine.rs`, via a fake `ContainerRuntime`), and CLI argument/behavior (`src/main.rs`, `tests/cli.rs`). `tests/cli.rs` also has end-to-end tests (`#[ignore]`d by default) that run against a real Docker daemon — the sample `batect.yml` and `tests/fixtures/sidecar.yml` (which proves real cross-container DNS resolution, not just that the right calls were made) — run them explicitly with `cargo test --workspace --test cli -- --ignored`; they also run as their own `docker-integration` CI job.
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
8.  **User Docs Maintenance**: When a change affects user-visible behavior (CLI flags, config schema, runtime behavior, Batect parity), update the relevant file(s) under `docs/` in the same change — don't let them drift from the code. If you find the code doesn't match what's documented, fix whichever one is wrong rather than leaving the mismatch.
9.  **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, error conditions) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout.
10. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff.
