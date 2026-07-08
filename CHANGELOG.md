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
- `docs/` directory with self-contained user documentation: installation, getting started, how it works (architecture), CLI reference, configuration reference, and a differences-from-Batect page — linked from `README.md`. Documents current gaps found in the process: `-- ADDITIONAL_ARGS` are parsed but not forwarded to the running command, `build_directory`/container `dependencies` are parsed but unimplemented, a container with neither `image` nor `build_directory` is a silent no-op, a missing config file doesn't fail the process, and container exit codes aren't currently checked.
- Itemized field-by-field and flag-by-flag comparison tables in `docs/differences-from-batect.md`, verified directly against Batect's own reference documentation (its config `overview`/`containers`/`tasks` and `cli` pages) rather than assumption — this is the detail behind the roadmap's "Full Configuration Parity" and "Full CLI Options Parity" items, and it also surfaced that Ratect silently ignores unsupported config keys instead of rejecting them.
- Sidecar/dependency container support (`Container.dependencies`, previously parsed but unused): dependencies are started (recursively, for nested dependencies) before a task's own container, reachable by name over a Docker network created and torn down for that single task execution. Deduped within one task's dependency resolution; not shared across tasks — each task execution gets its own instance and network, matching Batect's documented behavior. `ContainerRuntime` gained `create_network`, `remove_network`, `start_background_container`, and `stop_and_remove_container`; `run_container` now takes a `name`/`network` pair so a task's own container can join its dependencies' network. New `uuid` dependency for collision-resistant network naming (process ID was considered and rejected — it's frequently `1` when `ratect` runs inside a container, e.g. CI). No `health_check`/`setup_commands` support, so a dependency counts as ready as soon as it starts; see `docs/task-lifecycle.md` (new) for the full model with diagrams, and `docs/differences-from-batect.md` for what's simplified relative to Batect. New `tests/fixtures/sidecar.yml` fixture (two sibling dependencies plus one nested behind them) and ignored Docker integration test prove real cross-container DNS resolution for both siblings and nesting together, not just that the right calls were made. Unit tests cover nesting to four levels deep, within-task dedup of a dependency shared by multiple siblings (asserting each sibling itself started, not just the shared one), cross-task isolation, and circular-dependency detection.

### Fixed

- `ROADMAP.md` incorrectly listed `--project-name` as an example Batect CLI flag; it's actually a `batect.yml` config field, not a CLI option. Corrected and cross-linked to the itemized flag table in `docs/differences-from-batect.md`.
- Fatal errors (malformed config, missing task/container, dependency cycle) previously bypassed `tracing` entirely, propagating to `main`'s default `Result` handler and printing via `anyhow`'s raw `Debug` formatting — inconsistent with every other diagnostic message, and unaffected by `RUST_LOG`. `main` now returns `ExitCode` and routes the final error through `tracing::error!` like everything else.
- Container exit codes weren't checked: a task whose command exited non-zero was still reported as successful, and dependent tasks still ran. `run_container` now waits for the container and checks its exit status (via `wait_container`, falling back to bollard's `DockerContainerWaitError` for non-zero codes); a non-zero exit raises a new `ContainerExitedNonZero` error, and `main` propagates the *exact* exit code as `ratect`'s own process exit code (matching `docker run`'s convention), rather than a generic failure code. This also means a failing prerequisite now correctly stops the rest of the chain, matching Batect's documented behavior. New ignored Docker integration tests (`tests/fixtures/exit-code.yml`) prove both the exact-code propagation and the prerequisite-chain-stops behavior against a real daemon; a new unit test proves dependency/network cleanup still happens even when the main container fails.

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
