# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Changed

- Updated project version to `0.1.0-dev` to reflect pre-release status.
- Migrated YAML parsing from `serde_yaml` to `noyalib` for improved safety and maintenance.
- Upgraded core dependencies to their latest stable versions.
- `Cargo.lock` is now committed to the repository (previously gitignored), following the convention for binary crates to ensure reproducible builds and accurate dependency audits.
- Applied `cargo fmt` formatting across `src/`.
- Wired up `tracing`/`tracing-subscriber`: task lifecycle, unimplemented-feature, and config-error diagnostics now go through leveled, `RUST_LOG`-filterable log events on stderr, while command output (task listing, container log streaming) remains on stdout via `println!`/`print!`.
