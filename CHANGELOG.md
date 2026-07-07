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

### Changed

- Updated project version to `0.1.0-dev` to reflect pre-release status.
- Migrated YAML parsing from `serde_yaml` to `noyalib` for improved safety and maintenance.
- Upgraded core dependencies to their latest stable versions.
- `Cargo.lock` is now committed to the repository (previously gitignored), following the convention for binary crates to ensure reproducible builds and accurate dependency audits.
- Applied `cargo fmt` formatting across `src/`.
