# Ratect AI Agent Guide

This file provides context, instructions, and guidelines for AI agents working on the Ratect project.

## Project Overview

Ratect is a Rust-based implementation of the [Batect](https://github.com/batect/batect) task execution engine. Its goal is to provide a fast, lightweight CLI for running development tasks in Docker containers, defined via a `batect.yml` file.

## Architecture

The project is modularized into several key components:

- **`src/main.rs`**: Handles CLI argument parsing (via `clap`) and orchestrates the high-level flow (loading config, initializing the Docker client, and starting the engine).
- **`src/config.rs`**: Contains the data models for the configuration (`batect.yml`). It uses `noyalib` for YAML parsing and includes logic for resolving relative paths in volume mounts.
- **`src/docker.rs`**: A wrapper around the `bollard` library. It manages interactions with the Docker daemon, including pulling images, creating containers, and streaming logs.
- **`src/engine.rs`**: The core execution logic. It manages the task lifecycle, handles prerequisites, detects dependency cycles, and ensures that each task and image pull occurs only once per session.

## Key Dependencies

- **`bollard`**: Asynchronous Docker API client.
- **`noyalib`**: Safe, pure-Rust YAML parser (used as a modern alternative to `serde_yaml`).
- **`tokio`**: The asynchronous runtime.
- **`clap`**: Command-line argument parsing with derive support.
- **`indicatif`**: Used for displaying progress bars during image pulls.
- **`anyhow`**: Simplified error handling with context.
- **`tracing` / `tracing-subscriber`**: Structured, leveled logging. The subscriber is initialized in `main.rs`, filtered via `RUST_LOG` (defaults to `info`), and writes to stderr.

## Tooling & CI

- **Formatting/Linting**: `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored).
- **Tests**: `cargo test` runs in CI. There is currently no test coverage — this is a known gap, not a passing baseline.

## Current Status & Roadmap

Ratect is currently a **Work in Progress**. For a detailed list of supported features and our future plans, please refer to the [ROADMAP.md](ROADMAP.md) file.

## Guidelines for AI Agents

1.  **Idiomatic Rust**: Always strive for idiomatic and safe Rust. Use `anyhow::Context` to provide meaningful error messages.
2.  **Async/Await**: The codebase is heavily asynchronous. Ensure new I/O or Docker-related code uses `await` and integrates with the `tokio` runtime.
3.  **Dependency Management**: Keep `Cargo.toml` clean and dependencies updated. If a library becomes deprecated or unmaintained, propose a migration to a better alternative.
4.  **Configuration Consistency**: When extending the `batect.yml` parser in `src/config.rs`, try to maintain compatibility with the original Batect configuration format.
5.  **State Management**: In `src/engine.rs`, state (like executed tasks) is shared using `Mutex` to ensure thread safety across async tasks. Be mindful of locking logic.
6.  **Verification**: After making changes, verify them by:
    -   Running `cargo build` to ensure compilation.
    -   Executing `cargo run -- --list-tasks` to check config parsing.
    -   Running a sample task (e.g., `cargo run -- test-task`) to verify the execution engine and Docker integration.
7.  **Changelog Maintenance**: After completing a task that changes the project's features, dependencies, or structure, ensure that `CHANGELOG.md` is updated in the "Unreleased" section, following the "Keep a Changelog" standard.
8.  **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, error conditions) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout.
9.  **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff.
