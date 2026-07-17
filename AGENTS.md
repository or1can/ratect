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
  See [`docs/how-it-works.md`](docs/how-it-works.md) for the full request-to-container
  pipeline; the notes below are per-module gotchas, not a full walkthrough.
  - **`ratect-core/src/config.rs`**: Data models for the configuration (`batect.yml`),
    parsed via `noyalib`. `Config::load_from_file` parses the root file and resolves
    `include` (local file includes only so far — see
    [config reference](docs/config-reference.md#includes)), merging every loaded
    file's `containers`/`tasks`/`config_variables` into one `Config`, returned inside a
    `LoadedConfig` alongside a `container_base_paths` map (each container name → its
    own origin file's directory). A separate `LoadedConfig::resolve_expressions` call
    (needs CLI-supplied `--config-var`/`--config-vars-file` overrides, so it can't
    happen inside `load_from_file`) interpolates and resolves paths — per-container,
    against `container_base_paths` rather than a single shared directory, so an
    included file's relative paths resolve against *its own* directory while
    `batect.project_directory` still always resolves to the root's (`Config`'s own
    `resolve_expressions` stays available too, unchanged, for a `Config` built without
    going through `load_from_file`). `run_as_current_user.home_directory` is
    interpolated but *not* resolved against a base path — it's a container-side path,
    validated to start with `/` instead. `PortRange`/`PortMapping` and
    `DeviceMapping` (`devices`) have hand-written `Deserialize` impls so an entry can
    be either Batect's string form (`"local:container[/protocol]"` /
    `"local:container[:options]"`) or the expanded object form. `Capability`
    (`capabilities_to_add`/`capabilities_to_drop`) and `ImagePullPolicy` are fixed
    enums validated at parse time — `Capability`'s list is a deliberate *superset* of
    Batect's own (unmaintained) one, not a strict port, see its doc comment.
  - **`ratect-core/src/expressions.rs`**: Batect's expression syntax (`$VAR`,
    `${VAR:-default}`, `<name`/`<{name}` for config variables, including the built-in
    `batect.project_directory`). Host environment and resolved config variable values
    are injected as parameters rather than read from the real process environment, so
    resolution stays deterministic and unit-testable.
  - **`ratect-core/src/docker.rs`**: Wraps `bollard` for all Docker daemon interaction
    — pulling/building images, running a task's own container, per-task networks,
    sidecar/dependency containers, the interactive-mode TTY attach path
    ([docs](docs/config-reference.md#interactive-mode)), and the user-mapping upload
    path ([docs](docs/config-reference.md#user-mapping)). Exposes a `ContainerRuntime`
    trait so the engine can be tested against a fake instead of a live daemon. Gotchas
    worth knowing before touching it: the interactive path's `RawModeGuard` restores
    the terminal on `Drop`, even on an error return; since Ratect has no `--output`
    streaming mode, a failed build's full log transcript (not just Docker's one-line
    summary) is folded into the returned error instead; `command`/`entrypoint` are
    tokenized into literal argv by `tokenize_command_line` (a from-scratch port of
    Batect's own `Command.parse`) rather than run via a shell — `setup_commands` is
    the one remaining `sh -c` exception, a known, narrower, still-open divergence (see
    `config::SetupCommand`'s doc comment); and `ContainerOptions` bundles the
    still-growing set of per-container Docker options shared by `run_container`/
    `start_background_container` (0.13.0's `working_directory` through
    `enable_init_process`) — add new container-level fields there rather than as more
    flat parameters, converting from config types to plain values in `engine.rs`
    (`docker.rs` deliberately never depends on `config` types directly).
  - **`ratect-core/src/user.rs`**: Host user lookup (`current_user`, via the `nix`
    crate — Unix-only) and the pure `/etc/passwd`/`/etc/shadow`/`/etc/group` content
    generators `docker.rs` uses — ported from Batect's
    `RunAsCurrentUserConfigurationProvider`, including its `uid == 0`/`gid == 0`
    special-casing so running as the current user doesn't produce a duplicate
    conflicting `root` entry.
  - **`ratect-core/src/proxy.rs`**: Proxy environment variable detection/propagation
    (`--no-proxy-vars` to disable) — ported from Batect's
    `ProxyEnvironmentVariablesProvider`/`ProxyEnvironmentVariablePreprocessor`.
    Rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal`
    (macOS/Windows only — `None` on Linux).
  - **`ratect-core/src/engine.rs`**: The core execution logic — task lifecycle,
    prerequisites, dependency-cycle detection, sidecar/dependency container resolution
    (see [`docs/task-lifecycle.md`](docs/task-lifecycle.md)), and once-per-session
    dedup of image pulls/builds/task runs. `TaskEngine` is generic over
    `ContainerRuntime`. Worth knowing: opt-in settings (`existing_network`,
    `publish_ports`, etc.) are builder methods rather than `TaskEngine::new`
    parameters, so each new one lands without a mass-edit of the ~30 existing call
    sites; and only the task actually named on the command line (never a
    prerequisite) is ever eligible for interactive-TTY mode.
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

- **`bollard`** (`features = ["buildkit_providerless", "chrono"]`, **consumed via a `[patch.crates-io]` fork** — see the root `Cargo.toml`): Asynchronous Docker API client. Both build paths go through the same classic `/build` endpoint (`Docker::build_image`): the non-BuildKit path as before, and the BuildKit path (`build_image_via_buildkit`, `ratect-core/src/docker.rs`) by additionally setting `BuilderVersion::BuilderBuildKit` plus a per-build session — the channel the daemon calls back over to have `build_secrets`/`build_ssh` served mid-build. The session upgrades the *existing* daemon's own `/session`+`/grpc` endpoints (no separate persistent builder container), and the endpoint's response stream carries BuildKit's structured progress (`BuildInfoAux::BuildKit` — vertexes/logs, accumulated into the same transcript-in-error the classic path keeps) *and* the built image ID (`BuildInfoAux::Default`). The fork (`or1can/bollard`, branch `ratect/session-providers-0.21`; both commits PR'd upstream) carries the two pieces 0.21.0 is missing: `build_image_with_session_providers` (upstream `build_image`'s internal session only registers auth/file-send services — no way to supply the secrets/ssh providers; its gRPC-driver path has the providers but drops the log stream and isn't `Send`-compatible with `#[async_trait]`, which is why Ratect no longer uses it) and `ping_info` (exposes the `/_ping` response's `Builder-Version` header — the daemon's advertised default builder, which `ping()` discards). `chrono` is required transitively (BuildKit OAuth token expiry needs a date/time type; bollard fails to compile without either it or the `time` crate feature once `buildkit_providerless` is on). One remaining limitation worth knowing: bollard's ssh forwarding only serves the host's own `SSH_AUTH_SOCK` agent under BuildKit's implicit `default` id — no equivalent to Batect's multiple named agents or forwarding explicit private key files instead of a running agent (a second, separable upstream contribution) — see the `build_ssh` config field's docs and `docs/differences-from-batect.md#container-fields`.
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
- **`crossterm`**: Raw-mode terminal enable/disable and terminal size queries for interactive mode's attach path (`ratect-core/src/docker.rs`). Deliberately not used for its structured `event`/`EventStream` API — that's for TUI-style key/mouse/resize events and would consume/interpret stdin bytes instead of passing them through raw. `std::io::IsTerminal` (stable stdlib) covers the separate "is this actually a terminal" checks; no crate needed for that part. Live terminal-resize forwarding (0.10.0) is built on `tokio::signal::unix`'s `SIGWINCH` listener instead of crossterm's `event`/`EventStream` — a plain OS signal, not a stdin-consuming abstraction, so it doesn't reintroduce the problem this entry warns off; `crossterm::terminal::size()` is still what's actually queried on each signal.
- **`portable-pty`** (dev-dependency, `tests/cli.rs` only): creates a real (emulated) pseudo-terminal pair in-process, so an integration test can spawn `ratect` attached to something that genuinely passes `IsTerminal` checks and actually drive an interactive session — no existing test infrastructure here could otherwise exercise that path at all. Works in headless CI; no real terminal required. A reusable pattern worth reaching for again for any other feature that's only meaningfully testable from a real terminal.
- **`nix`** (`features = ["user"]`): looks up the real host user (`Uid`/`Gid::current`, `User`/`Group::from_uid`/`from_gid`) for `run_as_current_user` (`ratect-core/src/user.rs`) — Unix-only, matching Ratect's own Unix-only testing so far. Already resolved in `Cargo.lock` transitively (via `portable-pty`'s own dependency graph in the root crate's dev-dependencies); adding it directly to `ratect-core` was a low-risk addition, not a new unknown quantity.
- **`url`**: parses/rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal` in `ratect-core/src/proxy.rs`. Already resolved in `Cargo.lock` transitively (via `bollard`'s own dependency graph) — same low-risk-addition reasoning as `nix` above.

Dependencies are split across the three `Cargo.toml`s along CLI-vs-core lines: `clap`
and `tracing-subscriber` are `ratect`-only; `serde`, `noyalib`, `bollard`, `futures`,
`indicatif`, `async-recursion`, `async-trait`, `uuid`, `tar`, `path-clean`, `crossterm`,
`nix`, `url`, and the local `dockerignore` crate are `ratect-core`-only (`dockerignore`
itself depends on `regex` and `path-clean` too); `anyhow`, `tracing`, and `tokio` are
needed by both. `tokio` is a normal dependency in both crates now — `ratect-core`'s
non-test code needs it too, for `build_context_tar`'s `tokio::task::spawn_blocking` (it
used to be a `ratect-core` dev-dependency only, for `#[tokio::test]` in its unit tests).
`portable-pty` is `ratect`'s (root crate's) first `[dev-dependencies]` entry.

## Tooling & CI

- **Formatting/Linting**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored). One shared lockfile covers both crates.
- **Tests**: `cargo test --workspace` runs in CI, covering unit tests per module (pattern matching in `dockerignore`, config parsing/resolution, expression interpolation, build-context tar construction, interactive-TTY eligibility, user-mapping generation, and task engine logic — dependency cycles, prerequisite dedup, sidecar/dependency resolution, dependency readiness (health-wait/setup-command ordering and failure paths), environment merging, image resolution — via a fake `ContainerRuntime`) and CLI argument/behavior tests in `src/main.rs`/`tests/cli.rs`. `tests/cli.rs` also has end-to-end tests (`#[ignore]`d by default, run explicitly via `cargo test --workspace --test cli -- --ignored`) that exercise a real Docker daemon against the fixtures under `tests/fixtures/` — one per feature (sidecars, dependency readiness, environment/config variables, image building, `.dockerignore`, interactive mode, user mapping, hostnames/ports, proxy, `--use-network`). These also run as their own `docker-integration` CI job. See the fixture files themselves for what each one proves.
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
    -   Executing `cargo run -- -f tests/fixtures/smoke.yml --list-tasks` to check config parsing.
    -   Running a sample task (e.g., `cargo run -- -f tests/fixtures/smoke.yml test-task`) to verify the execution engine and Docker integration. (There is deliberately no `batect.yml` at the repository root — that path reads as *this project's own* dev-task config by the very convention ratect implements; the smoke fixture lives with the other fixtures instead.)
7.  **Changelog Maintenance**: After completing a task that changes the project's features, dependencies, or structure, ensure that `CHANGELOG.md` is updated in the "Unreleased" section, following the "Keep a Changelog" standard.
8.  **Version Lifecycle**: When cutting a release, it's not just a version bump — follow the full process documented in [ROADMAP.md](ROADMAP.md#versioning--releases): the `X.Y.Z-dev` → `X.Y.Z` bump commit, tagging it `vX.Y.Z`, and publishing it as a GitHub Release (body = that version's `CHANGELOG.md` section). Starting the next version's development is a separate, later commit that bumps back to the next `X.Y.Z-dev`. Neither bump is ever folded into a feature commit.
9.  **ROADMAP.md Maintenance**: its `## Batect Parity` headline list and its versioned `### ratect-compat` list follow different edit rules. The headline list is a living summary — freely edit, merge, or delete bullets as scope changes or ships (e.g. "Sidecar Containers" and "Docker Networking" were merged into "Full Docker Networking" once shipped). The versioned list is append-only history — never delete an entry; mark completed scope with `~~strikethrough~~` plus a done-summary of what actually shipped.
10. **User Docs Maintenance**: When a change affects user-visible behavior (CLI flags, config schema, runtime behavior, Batect parity), update the relevant file(s) under `docs/` in the same change — don't let them drift from the code. If you find the code doesn't match what's documented, fix whichever one is wrong rather than leaving the mismatch.
11. **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, error conditions) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout.
12. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff. Every commit is signed off (`git commit -s`) — the [DCO](https://developercertificate.org) attestation CONTRIBUTING.md describes and CI enforces on pull requests; direct commits to `main` follow the same convention for consistency.
13. **Commit Packaging**: a release that's one theme (like most 0.x releases so far) lands as a single `feat:` commit. A release bundling several genuinely separable behaviors (e.g. 0.6.0's networking + proxy work) should instead split into one `feat:` commit per behavior, each with its own tests and doc updates — easier to review and to `git bisect`/`git revert` than one large commit. The version bump and any docs-only release summary stay separate commits either way (see 8).
