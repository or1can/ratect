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
    validated to start with `/` instead. `PortRange`/`PortMapping`,
    `DeviceMapping` (`devices`), and `VolumeMount` (`volumes` — `Local`/`Cache`
    variants, 0.18.0) all have hand-written `Deserialize` impls so an entry can
    be either Batect's string form (`"local:container[/protocol]"` /
    `"local:container[:options]"` — `VolumeMount`'s string form is always
    `Local`; there's no compact string form for `Cache`) or the expanded object
    form. A `VolumeMount::Local`'s host path is resolved here (against
    `container_base_paths`, same as `build_directory`); a `Cache`'s `name`/
    `container` are plain strings, not `Expression`s, matching Batect — nothing
    to resolve here at all, since `--cache-type` and the project's own cache
    key (needed to actually resolve one) aren't known until `engine.rs`/
    `cache.rs`. `Capability`
    (`capabilities_to_add`/`capabilities_to_drop`) and `ImagePullPolicy` are fixed
    enums validated at parse time — `Capability`'s list is a deliberate *superset* of
    Batect's own (unmaintained) one, not a strict port, see its doc comment.
    `Task.run` is `Option<TaskRun>` (0.14.0, see docs/task-lifecycle.md) — still
    requires at least one of `run`/`prerequisites`. `dependencies` (task-level
    sidecars, distinct from `Container.dependencies`) requires `run` and is
    rejected without it; `customise` requires `run` too but is merely inert
    without it, matching Batect. `container_names_in_task` lives here (moved from
    `engine.rs`) since both the `no_proxy` exemption list and `customise`'s
    graph-membership check need the same transitive-dependency walk.
    `format_task_list` is the single source of `--list-tasks` formatting.
    `Container.command` (a container's own default `CMD` override, symmetric with
    `Container.entrypoint`) was missed when 0.13.0's container runtime options
    landed — `run.command` covered the task's own container, but a dependency had
    no way to set a command of its own at all, silently defaulting to the image's
    own `CMD` regardless. Closed once noticed, threading through
    `ContainerRuntime::start_background_container` (a new `command` parameter,
    reusing `docker.rs`'s existing `build_cmd`/`tokenize_command_line`) the same
    way `run_container`'s already did. `forbid_telemetry`
    (`Config`/`ConfigFile`) and `config_variables.<name>.description`
    (`ConfigVariable`) are recognized but inert (0.19.0), the same "no
    effect" treatment already given `--upgrade`/`--no-update-notification`/
    `--no-wrapper-cache-cleanup` (0.17.0, `main.rs`) — parsed and, for
    `forbid_telemetry`, carried onto the merged `Config` (root file only,
    same precedent as `project_name`), but never read anywhere else.
  - **`ratect-core/src/git_include.rs`**: Git includes (`type: git` entries
    in `include`) — `GitIncludeCache::ensure_cached`, driven by
    `config.rs`'s own include-resolution loop, clones a `(remote, ref)` pair
    once into `~/.ratect/incl/<sha256 key>/` and reuses it forever (0.8.0);
    a `<key>.toml` sidecar (`CacheInfo`) records `last_used` (a Unix
    timestamp, not `atime`/`mtime` — unreliable across platforms/CI),
    bumped on every `ensure_cached` call regardless of whether a clone
    actually happened. `GitIncludeCache::cleanup_stale` (0.19.0) sweeps that
    same cache: any entry whose `last_used` is more than 30 days old gets
    both its working copy and its `.toml` sidecar removed, matching
    Batect's own `GitRepositoryCacheCleanupTask` exactly except that it's a
    `tokio::spawn`ed async task, not a literal OS thread (Batect's own JVM
    daemon thread is the equivalent to port the *behavior* of — unconditional,
    fire-and-forget, never awaited — not literally a `std::thread::spawn`).
    Started unconditionally from `main.rs`'s "run a task" branch (not
    `--list-tasks`), before the Docker connectivity check, mirroring where
    Batect's own `BackgroundTaskManager` fires it. One stale entry failing
    to delete (unreadable/unparsable sidecar, filesystem error) is logged
    and skipped rather than aborting the whole sweep — same per-entry
    try/catch Batect's own cleanup task has.
  - **`ratect-core/src/cache.rs`** (0.18.0): Resolves a `VolumeMount::Cache`
    (`config.rs`) into an actual Docker bind-mount string — a named volume
    (`CacheType::Volume`, the default) or a host directory
    (`CacheType::Directory`, `--cache-type=directory`) — and implements
    `--clean`/`--clean-cache` (`clean_volume_caches`/`clean_directory_caches`),
    which remove them. Ported from Batect's own `CacheManager`/
    `VolumeMountResolver`/`CacheType`/`CleanupCachesCommand`, kept
    byte-for-byte compatible with Batect's own `.batect/caches/` location and
    `batect-cache-<project-key>-<name>` volume-naming convention *on purpose*
    — this is `ratect-compat`'s territory (see `ROADMAP.md`'s two-binaries
    section), so a project migrating from real `batect` should find its
    existing cache volumes/directories reused, not orphaned. The one
    deliberate divergence: a freshly generated `project_cache_key` is a full
    `uuid::Uuid::new_v4()`, not Batect's 6-char `a-z0-9` id — an existing
    Batect-created key file is still read and reused byte-for-byte (tolerant
    of its `#`-comment-header format), since nothing depends on matching the
    *generation* format, only the file's path and read-compatible layout, and
    Batect's own alphabet is meaningfully more collision-prone across many
    projects on one machine. The actual removal *decision* (which
    volumes/directories match this project's prefix, restricted to
    `--clean-cache`'s allowlist) is split into plain synchronous functions
    (`matching_cache_volumes`/`matching_cache_directories`), deliberately kept
    separate from the async I/O around them, so they're unit-testable against
    plain `Vec<String>`/tempdir fixtures without needing a fake
    `ContainerRuntime`.
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
    summary) is folded into the returned error instead; `command`/`entrypoint`/
    `setup_commands.command` are all tokenized into literal argv by
    `tokenize_command_line` (a from-scratch port of Batect's own `Command.parse`)
    rather than run via a shell — `setup_commands` used to be a `sh -c` exception
    (closed once noticed it was never actually deliberate; see
    `config::SetupCommand`'s doc comment); and `ContainerOptions` bundles the
    still-growing set of per-container Docker options shared by `run_container`/
    `start_background_container` (0.13.0's `working_directory` through
    `enable_init_process`) — add new container-level fields there rather than as more
    flat parameters, converting from config types to plain values in `engine.rs`
    (`docker.rs` deliberately never depends on `config` types directly).
    `log_driver`/`log_options` (0.19.0) followed the same pattern onto
    bollard's `HostConfig.log_config` (`build_log_config`, pure/unit-testable,
    same shape as `build_devices`) — `None`/absent leaves the daemon's own
    configured default alone rather than baking in a literal `"json-file"`
    default the way Batect's own config model does. `build_image` also
    gained a `force_pull: bool` parameter (0.19.0, both the classic and
    BuildKit paths' `BuildImageOptionsBuilder::pull("true")`) — Batect's
    second, distinct use of `image_pull_policy` on a `build_directory`
    container (`engine.rs`'s `resolve_image` computes it from
    `container_config.image_pull_policy == Always`, since `docker.rs` still
    doesn't depend on `config` types directly).
    `ensure_host_volume_directories_exist` (the `run_as_current_user` host-dir
    pre-creation step) only `mkdir -p`s a bind's *absolute* source segment —
    added when 0.18.0's `cache` mounts landed, since `CacheType::Volume`
    resolves to a bare (non-absolute) Docker volume name, which this would
    otherwise have tried to create as a relative directory under the current
    working directory. `list_volumes`/`remove_volume` (0.18.0, `--clean`/
    `--clean-cache`) are thin wrappers over bollard's own volume API — see
    `cache.rs` for the actual removal-decision logic built on top of them.
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
    prerequisite) is ever eligible for interactive-TTY mode. `run_task_internal`
    runs `prerequisites` first, then returns early (no error) if the task itself has
    no `run` (0.14.0) — everything after can assume `run` is present. `customise`
    threads through `start_dependency`'s own recursion unconditionally, so it
    reaches its target regardless of depth in the dependency graph.
    `resolve_volumes` (0.18.0) turns a container's `VolumeMount`s into the
    literal bind strings `docker.rs` expects — a `Local` mount's already fully
    resolved by `config.rs`, nothing left to do but reassemble the string; a
    `Cache` mount goes through `cache::resolve_cache_mount`, memoizing the
    project's own cache key in a `tokio::sync::OnceCell` field (computed at
    most once per invocation, and only if a `cache` mount is actually
    resolved — never eagerly). `with_cache_options` (`--cache-type` + the
    project directory) is `main.rs`'s own builder call, always made in
    practice despite being optional here, same convention as the other opt-in
    settings above.
  - **`ratect-core/src/ui/`**: The user-facing output layer (0.16.0's output-modes
    work) — a port of Batect's `TaskEventSink`/`EventLogger` design: `engine.rs`
    posts typed `TaskEvent` milestones and `docker.rs` posts fine-grained
    pull/build progress to an injected `EventSink` (both default to the silent
    `NullEventSink`; `main.rs` wires the real logger into both so one sink sees
    the whole stream), and the selected logger decides what each event renders
    as — never `println!` from `engine.rs`/`docker.rs` directly. Loggers must
    serialize rendering internally (events arrive concurrently since 0.15.0);
    `Console` keeps color and cursor movement as *independent* axes,
    deliberately unlike Batect's single `enableComplexOutput` flag — that
    coupling is the only reason Batect rejects `fancy` + `--no-color`, a
    combination Ratect supports instead (colorless fancy). Milestone events are
    keyed by container/task name (engine's vocabulary); progress events by
    image/tag (all `docker.rs` knows) — a logger maps one to the other via the
    `TaskGraphResolved` event's `TaskContainerInfo`s. The logger also *owns the
    container I/O policy* (`EventSink::container_io_streaming`, mirroring
    Batect's `EventLogger.ioStreamingOptions`): `engine.rs` and `docker.rs`
    consult it rather than being configured separately, which is how `all` mode
    line-buffers every container's output into `ContainerOutput` events (no
    TTY/stdin, `TERM=dumb` everywhere) while the other three modes stream the
    task container raw to stdout — add any future per-mode I/O behavior through
    that method, not a new engine/docker setting. Style selection and logger
    construction (including the explicit-`fancy`-without-an-interactive-console
    error) live in `ui::create_event_sink`, not `main.rs` — deliberately, so the
    planned `ratect-compat` binary (see `ROADMAP.md`'s two-binaries section)
    gets this for free instead of reimplementing the style→logger match itself;
    `main.rs` only gathers the terminal facts once and hands them to it (and to
    `select_output_style`, for `--list-tasks`'s own quiet-format decision).
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

- **`bollard`** (`features = ["buildkit_providerless", "chrono", "ssl"]`, **consumed via a `[patch.crates-io]` fork** — see the root `Cargo.toml`): Asynchronous Docker API client. The fork (`or1can/bollard`, branch `ratect/session-providers-0.21`, both commits PR'd upstream) adds session-provider support to `build_image` (needed for `build_secrets`/`build_ssh` mid-build) and `ping_info` (the daemon's advertised default builder) — see `ROADMAP.md`'s 0.12.0 entry for the full fork mechanics, PR links, and the build_ssh single-agent limitation. Remove the patch once both land in a bollard release. `chrono` is required transitively once `buildkit_providerless` is on (BuildKit OAuth token expiry needs a date/time type) — bollard won't compile without it or the `time` feature. `ssl` (added for `--docker-tls`/`-verify`, `ratect-core/src/docker.rs`'s `connect`) turns on `rustls`'s `ring` cryptographic provider feature on top of `ssl_providerless` (already pulled in by `buildkit_providerless`) — `Docker::connect_with_ssl` panics if asked to build a TLS connection before a provider is installed, so `ensure_crypto_provider_installed` calls `rustls::crypto::ring::default_provider().install_default()` once, guarded by a `std::sync::Once`.
- **`rustls`** (`default-features = false`, matching `bollard`'s own dependency line so no extra features get pulled in beyond what `bollard`'s `ssl` feature already requests): declared directly so `ratect-core` can call `rustls::crypto::ring::default_provider().install_default()` itself (see the `bollard` entry above) — Rust's strict-deps rule means a crate can't `use` another crate's items unless it's a direct dependency of its own, even when (as here) that crate is already fully resolved transitively.
- **`noyalib`**: Safe, pure-Rust YAML parser (used as a modern alternative to `serde_yaml`).
- **`tokio`**: The asynchronous runtime.
- **`clap`**: Command-line argument parsing with derive support.
- **`anyhow`**: Simplified error handling with context.
- **`tracing` / `tracing-subscriber`**: Structured, leveled logging. The subscriber is initialized in `main.rs`, filtered via `RUST_LOG` (defaults to `info`), and writes to stderr.
- **`async-trait`**: Used for the `ContainerRuntime` trait in `ratect-core/src/docker.rs`, so it can have async methods and be implemented by both the real `DockerClient` and test fakes.
- **`uuid`**: Generates collision-resistant per-task Docker network names (`ratect-<uuid>`) in `ratect-core/src/engine.rs`. Deliberately not `std::process::id()` — that's frequently `1` when `ratect` itself runs inside a container (e.g. CI), which would collide across concurrent runs. Built images are tagged `<project_name>-<container_name>` instead (human-readable, matching Batect's convention) — `resolve_image` avoids the same collision hazard for these not via a random name but by running the image *ID* Docker's build reports back, not the (non-unique) tag. Also generates a freshly-created project cache key (0.18.0, `ratect-core/src/cache.rs`'s `project_cache_key`) — a full UUID rather than Batect's own shorter 6-char id, deliberately: nothing depends on matching Batect's generation format for a *new* key (an existing Batect-created one is read back byte-for-byte instead), and Batect's own alphabet is meaningfully more collision-prone across many projects sharing one machine.
- **`tar`**: Builds the in-memory build-context tarball `docker.rs`'s `build_context_tar` hands to `bollard`'s `build_image`.
- **`dockerignore`** (local workspace crate, not external): `.dockerignore` pattern matching — see the Architecture section above.
- **`path-clean`**: Lexically normalizes (`.`/`..`/trailing-slash) resolved paths in `ratect-core/src/config.rs` (`resolve_path`, and the built-in `batect.project_directory` config variable) — `PathBuf::join` alone doesn't do this, so without it a `base_path` like `""` or `"."` (both common — see `main.rs`'s `-f` handling) would leave a stray `.` or trailing slash in every path/expression derived from it. Already a `dockerignore` dependency; reused here rather than hand-rolling the same normalization twice.
- **`crossterm`**: Raw-mode terminal enable/disable and terminal size queries for interactive mode's attach path (`ratect-core/src/docker.rs`). Deliberately not used for its structured `event`/`EventStream` API — that's for TUI-style key/mouse/resize events and would consume/interpret stdin bytes instead of passing them through raw. `std::io::IsTerminal` (stable stdlib) covers the separate "is this actually a terminal" checks; no crate needed for that part. Live terminal-resize forwarding (0.10.0) is built on `tokio::signal::unix`'s `SIGWINCH` listener instead of crossterm's `event`/`EventStream` — a plain OS signal, not a stdin-consuming abstraction, so it doesn't reintroduce the problem this entry warns off; `crossterm::terminal::size()` is still what's actually queried on each signal.
- **`portable-pty`** (dev-dependency, `tests/cli.rs` only): creates a real (emulated) pseudo-terminal pair in-process, so an integration test can spawn `ratect` attached to something that genuinely passes `IsTerminal` checks and actually drive an interactive session — no existing test infrastructure here could otherwise exercise that path at all. Works in headless CI; no real terminal required. A reusable pattern worth reaching for again for any other feature that's only meaningfully testable from a real terminal.
- **`nix`** (`features = ["user"]`): looks up the real host user (`Uid`/`Gid::current`, `User`/`Group::from_uid`/`from_gid`) for `run_as_current_user` (`ratect-core/src/user.rs`) — Unix-only, matching Ratect's own Unix-only testing so far. Already resolved in `Cargo.lock` transitively (via `portable-pty`'s own dependency graph in the root crate's dev-dependencies); adding it directly to `ratect-core` was a low-risk addition, not a new unknown quantity.
- **`url`**: parses/rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal` in `ratect-core/src/proxy.rs`. Already resolved in `Cargo.lock` transitively (via `bollard`'s own dependency graph) — same low-risk-addition reasoning as `nix` above.
- **`unicode-width`**: real terminal display-column widths (CJK wide characters count as 2, zero-width/combining marks count as 0) for `ratect-core/src/ui/fancy.rs`'s repaint-width clipping and `ratect-core/src/ui/interleaved.rs`'s prefix-column padding — a plain `char`/byte count under-measures exactly those characters, which let a rendered line silently wrap onto more terminal rows than the fancy logger's own cursor-movement math accounts for. Zero transitive dependencies; the same crate ripgrep/bat/etc. use for this.
- **`bytes`** (`ratect-core` dev-dependency only): constructs `bollard::container::LogOutput` values directly in `docker.rs`'s own unit tests (`drain_interleaved_log_stream`'s tests, which feed it a synthetic log stream via `futures::stream::iter` instead of needing a live daemon) — `LogOutput`'s variants wrap a `bytes::Bytes` message, which bollard itself doesn't re-export. Already resolved in `Cargo.lock` transitively (via `bollard`/`hyper`'s own dependency graph) — same low-risk-addition reasoning as `nix`/`url` above.
- **`serde_json`**: parses the Docker CLI's own context-store JSON files for `--docker-context` (`ratect-core/src/docker.rs`'s `docker_context_host`/`active_docker_context`) — `<config_directory>/contexts/meta/<sha256(name)>/meta.json`'s `Endpoints.docker.Host`, and `<config_directory>/config.json`'s `currentContext`. `serde` itself was already a dependency (for `noyalib`'s `compat-serde-yaml`); `serde_json` is the standard, ubiquitous choice for the same derive-based approach applied to actual JSON.
- **`rcgen`**, **`tokio-rustls`**, **`time`** (`ratect-core` dev-dependencies only): generate a throwaway self-signed CA + leaf certificate/key pair at test run time and run a real in-process TLS server against it, for `docker.rs`'s `--docker-tls`/`-verify` tests (`connect_over_tls_completes_a_real_handshake_against_a_valid_certificate`/`_rejects_an_expired_certificate`) — proving an actual `rustls` handshake succeeds against a valid certificate and fails against an expired one, through Ratect's own `connect` path, not just that a client object builds. Generating at test time (rather than a fixed PEM committed to the repo) means validity is always computed relative to "now" — a static embedded certificate would eventually expire on its own and fail with a stale, disconnected-looking failure years later, unrelated to whatever change actually triggered it; `rcgen` also makes it trivial to generate a deliberately-already-expired certificate on demand for the rejection test. All three already resolved transitively (via `bollard`'s `ssl` feature and its own `rustls` dependency) — low-risk additions, not new unknown quantities.

Dependencies are split across the three `Cargo.toml`s along CLI-vs-core lines: `clap`
and `tracing-subscriber` are `ratect`-only; `serde`, `serde_json`, `noyalib`, `bollard`,
`futures`, `async-recursion`, `async-trait`, `uuid`, `tar`, `path-clean`, `crossterm`,
`nix`, `url`, `sha2`, `toml`, `regex`, `unicode-width`, `rustls`, and the local
`dockerignore` crate are `ratect-core`-only (`dockerignore` itself depends on `regex`
and `path-clean` too); `anyhow`, `tracing`, and `tokio` are
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
11. **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, non-fatal error conditions like a best-effort cleanup failure) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout. One deliberate exception: `main.rs`'s single top-level fatal error (the reason the process is about to exit non-zero) is `eprintln!`ed directly, *not* through `tracing::error!` — it must stay visible even under `RUST_LOG=off`, since every output mode (including `-o quiet`, whose whole contract is "only error messages") otherwise has nowhere else to show it. Found and fixed during 0.16.0's output-modes review — don't revert it back to `tracing::error!`.
12. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff. Every commit is signed off (`git commit -s`) — the [DCO](https://developercertificate.org) attestation CONTRIBUTING.md describes and CI enforces on pull requests; direct commits to `main` follow the same convention for consistency.
13. **Commit Packaging**: a release that's one theme (like most 0.x releases so far) lands as a single `feat:` commit. A release bundling several genuinely separable behaviors (e.g. 0.6.0's networking + proxy work) should instead split into one `feat:` commit per behavior, each with its own tests and doc updates — easier to review and to `git bisect`/`git revert` than one large commit. The version bump and any docs-only release summary stay separate commits either way (see 8).
