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

## Current Status & Roadmap

Ratect is currently a **Work in Progress**.

### Supported Features:
- Parsing of core `batect.yml` structure (project name, containers, tasks).
- Task execution with prerequisite support.
- Image pulling with progress indicators.
- Basic container execution with real-time log streaming.
- Host-to-container volume mounting with relative path resolution.
- Dependency cycle detection in tasks.

### Planned / Missing Features:
- Support for building images from `Dockerfile` (build directory).
- Docker network management for inter-container communication.
- Container dependency management (starting sidecar containers).
- Parallel task/prerequisite execution.
- Interactive terminal support (TTY/STDIN).
- Comprehensive health check support.

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
