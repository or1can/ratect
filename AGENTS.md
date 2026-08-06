# Ratect AI Agent Guide

This file provides context, instructions, and guidelines for AI agents working on the Ratect project.

## Project Overview

Ratect is a Rust-based implementation of the [Batect](https://github.com/batect/batect) task execution engine. Its goal is to provide a fast, lightweight CLI for running development tasks in Docker containers, defined in configuration: `batect.yml` for the Batect-compatible `ratect-compat` binary, and a native `ratect.toml` for `ratect` (since its 0.3.0).

## Architecture

Ratect is a **Cargo workspace** with four crates (the
[two-binary split](ROADMAP.md#two-binaries-ratect-and-ratect-compat) landed in
0.20.0):

- **`ratect-compat`** (`ratect-compat/src/main.rs` only): the CLI binary that
  implements all of the [Batect Parity](ROADMAP.md#batect-parity) work — a strict,
  flag-for-flag and field-for-field drop-in replacement for the (now-unmaintained)
  `batect` binary. Handles argument parsing (via `clap`) and orchestrates the
  high-level flow (loading config, initializing the Docker client, starting the
  engine) by calling into `ratect-core`. Nothing else lives here — this crate is
  deliberately thin, since `ratect-core` is what any other binary (namely `ratect`,
  below) shares too.
- **`ratect`** (`ratect/src/main.rs` only): the forward-looking CLI, free to diverge
  from Batect's interface — subcommands (`ratect run <task>`, `ratect tasks list`)
  since 0.2.0, and since 0.3.0 its **own `ratect.toml` config format** (`-f`
  defaults to it; `config::load_project_native`, not `load_project`) — see
  [decisions/0003](decisions/0003-ratect-native-config-format.md). Thin for the
  same reason `ratect-compat` is: argument parsing, then `config::load_project_native`
  + `TaskEngineSettings` + `ui::create_event_sink`. Conventions to keep when adding
  a verb — Docker-connection options live in the flattened `DockerArgs` struct and
  attach to the subcommands that actually connect (never globally, so no verb accepts
  a flag it ignores); `OutputStyleArg`/`CacheTypeArg` are *this binary's own*
  mirrors of the `ratect-core` enums, intentionally duplicated from `ratect-compat`'s
  rather than shared (each binary's accepted value names are part of its own
  interface, and `clap` stays out of `ratect-core`); and the `Command` enum's
  variant order *is* the `--help` order, grouped by purpose (run/tasks ·
  caches/includes/resources · config/doctor · completions) — `clap` can't render
  group headings for subcommands, so only the docs carry the labels and the order
  alone conveys them here. Beyond `run`/`tasks`: `caches`, `includes`, `resources`,
  `doctor`, `config` (`validate`, and `convert` for migrating a `batect.yml` —
  which defaults its *source* to `batect.yml` rather than `-f`'s `ratect.toml`
  default, since that's what it writes, and writes no-clobber via `create_new`
  plus a temp-file rename so an interrupted `--force` can't truncate a
  hand-edited file), and `completions` (see the `clap_complete` dependency note).
  User docs are [`docs/ratect-cli.md`](docs/ratect-cli.md) plus the format's own
  [`docs/ratect-config-reference.md`](docs/ratect-config-reference.md), both
  separate from `ratect-compat`'s [`docs/cli-reference.md`](docs/cli-reference.md)
  and [`docs/config-reference.md`](docs/config-reference.md) — two interfaces and
  two formats, not two spellings of one, so a change to either only ever touches
  its own page.
- **`ratect-core`** (library crate, `ratect-core/src/`): all the reusable logic, with
  no CLI-specific code. This is what any future second binary would also depend on.
  See [`docs/how-it-works.md`](docs/how-it-works.md) for the full request-to-container
  pipeline; the notes below are per-module gotchas, not a full walkthrough.
  Each module carries its own notes as a `//!` doc comment — what it does, the
  gotchas, and the decisions that are easy to undo by accident. Read the module,
  not this file; `cargo doc --open -p ratect-core` renders them all. What lives
  where:

  | Module | Responsibility |
  | --- | --- |
  | `config.rs` | Two text formats, one model: `batect.yml` (YAML) and `ratect.toml` (TOML), includes, expression/path resolution, `extends` |
  | `git_include.rs` | `type: git` includes — the `~/.ratect/incl` clone cache and its staleness sweep |
  | `cache.rs` | `volumes` `cache` mounts → a named volume or host directory, and `--clean`/`--clean-cache` |
  | `expressions.rs` | Batect's `$VAR`/`${VAR:-default}`/`<name` expression syntax |
  | `docker.rs` | All `bollard`/daemon interaction, behind the fakeable `ContainerRuntime` trait |
  | `ssh_agent.rs` | `build_ssh`'s in-process ssh-agent (RFC 9987) — kept extractable, see [0005](decisions/0005-build-ssh-keyring-placement.md) |
  | `user.rs` | Host user lookup and the `/etc/passwd` generators for `run_as_current_user` |
  | `proxy.rs` | Proxy variable detection and propagation |
  | `interrupt.rs` | Ctrl+C tracking — the signal half only; the engine decides what it means |
  | `engine.rs` | Task lifecycle, prerequisites, dependency graph, cleanup |
  | `labels.rs` | The `eu.orican.ratect.*` ownership labels, see [0002](decisions/0002-runtime-ownership-labels.md) |
  | `ui.rs` (+ `ui/`) | The output layer: typed events in, four output styles out |
  | `schema.rs` | The two committed JSON schemas (non-default `schema` feature) |

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

- **`bollard`** (`features = ["buildkit_providerless", "chrono", "ssl"]`, **consumed via a `[patch.crates-io]` fork** — see the root `Cargo.toml`): Asynchronous Docker API client. The fork (`or1can/bollard`, branch `feat/ssh-named-agents`) is upstream `master` plus one commit of ours that hasn't landed yet: sshforward dispatch to *named* ssh agents, which `build_ssh`'s multi-agent and `paths` support builds on ([decisions/0005](decisions/0005-build-ssh-keyring-placement.md)). The two changes the patch originally existed for — session providers on `build_image` ([#731](https://github.com/fussybeaver/bollard/pull/731)) and `ping_info` ([#732](https://github.com/fussybeaver/bollard/pull/732)) — **have merged upstream** and are on `master` (0.22.0); the patch remains only because the latest crates.io release is still 0.21.0. Dropping it needs *both* 0.22 published and the named-agent change landed, since 0.22 alone won't carry `set_ssh_agent`. **Keep the pin on a branch cut from upstream `master`, and rebase it — never merge `master` into it.** The predecessor branch (`feat/build-image-session-providers`) was the #731 PR branch itself; upstream squash-merged and reshaped that work, so branch and master came to express the same feature as different commits touching the same lines. It could then neither raise a pull request (GitHub reported an unmergeable comparison) nor pick up later upstream fixes — a dead end reached without anything visibly breaking, which is why the rule above is a rule. See `ROADMAP.md`'s 0.12.0 entry for the full fork mechanics and PR links, and its 0.25.0 entry for what the named-agent commit unlocked. `chrono` is required transitively once `buildkit_providerless` is on (BuildKit OAuth token expiry needs a date/time type) — bollard won't compile without it or the `time` feature. `ssl` (added for `--docker-tls`/`-verify`, `ratect-core/src/docker.rs`'s `connect`) turns on `rustls`'s `ring` cryptographic provider feature on top of `ssl_providerless` (already pulled in by `buildkit_providerless`) — `Docker::connect_with_ssl` panics if asked to build a TLS connection before a provider is installed, so `ensure_crypto_provider_installed` calls `rustls::crypto::ring::default_provider().install_default()` once, guarded by a `std::sync::Once`.
- **`rustls`** (`default-features = false`, matching `bollard`'s own dependency line so no extra features get pulled in beyond what `bollard`'s `ssl` feature already requests): declared directly so `ratect-core` can call `rustls::crypto::ring::default_provider().install_default()` itself (see the `bollard` entry above) — Rust's strict-deps rule means a crate can't `use` another crate's items unless it's a direct dependency of its own, even when (as here) that crate is already fully resolved transitively.
- **`noyalib`**: Safe, pure-Rust YAML parser (used as a modern alternative to `serde_yaml`).
- **`tokio`**: The asynchronous runtime.
- **`clap`**: Command-line argument parsing with derive support.
- **`clap_complete`** (`ratect`-only, `features = ["unstable-dynamic"]`): shell completion for `ratect completions <shell>`. `ratect`-only because the completion surface is that binary's own, and `ratect-compat` deliberately matches Batect's flat flag interface instead. The subcommand emits the *dynamic* registration script (via `env::EnvCompleter::write_registration`), so there's one mechanism rather than a static script plus a separate `COMPLETE=…` env dance; at `<TAB>` the shell re-invokes `ratect`, a `CompleteEnv` hook in `main` handles it and exits before any normal work, and an `ArgValueCompleter` on `run`'s task argument produces task names. Note the feature is explicitly **unstable** — treat occasional API churn as the cost of the one thing that makes completion worth having on a task runner. Two shape constraints worth knowing: a value completer is handed only the word being completed (which is why `-f` awareness reads the completion process's own args after `--`), and zsh's script ends in `compdef`, so it must be sourced after `compinit`.
- **`anyhow`**: Simplified error handling with context.
- **`tracing` / `tracing-subscriber`**: Structured, leveled logging. The subscriber is initialized in `main.rs`, filtered via `RUST_LOG` (defaults to `info`), and writes to stderr.
- **`async-trait`**: Used for the `ContainerRuntime` trait in `ratect-core/src/docker.rs`, so it can have async methods and be implemented by both the real `DockerClient` and test fakes.
- **`uuid`**: Generates collision-resistant per-task Docker network names (`ratect-<uuid>`) in `ratect-core/src/engine.rs`. Deliberately not `std::process::id()` — that's frequently `1` when `ratect` itself runs inside a container (e.g. CI), which would collide across concurrent runs. Built images are tagged `<project_name>-<container_name>` instead (human-readable, matching Batect's convention) — `resolve_image` avoids the same collision hazard for these not via a random name but by running the image *ID* Docker's build reports back, not the (non-unique) tag. Also generates a freshly-created project cache key (0.18.0, `ratect-core/src/cache.rs`'s `project_cache_key`) — a full UUID rather than Batect's own shorter 6-char id, deliberately: nothing depends on matching Batect's generation format for a *new* key (an existing Batect-created one is read back byte-for-byte instead), and Batect's own alphabet is meaningfully more collision-prone across many projects sharing one machine.
- **`tar`**: Builds the in-memory build-context tarball `docker.rs`'s `build_context_tar` hands to `bollard`'s `build_image`.
- **`dockerignore`** (local workspace crate, not external): `.dockerignore` pattern matching — see the Architecture section above.
- **`path-clean`**: Lexically normalizes (`.`/`..`/trailing-slash) resolved paths in `ratect-core/src/config.rs` (`resolve_path`, and the built-in `batect.project_directory` config variable) — `PathBuf::join` alone doesn't do this, so without it a `base_path` like `""` or `"."` (both common — see `ratect-compat/src/main.rs`'s `-f`
handling) would leave a stray `.` or trailing slash in every path/expression derived from it. Already a `dockerignore` dependency; reused here rather than hand-rolling the same normalization twice.
- **`crossterm`**: Raw-mode terminal enable/disable and terminal size queries for interactive mode's attach path (`ratect-core/src/docker.rs`). Deliberately not used for its structured `event`/`EventStream` API — that's for TUI-style key/mouse/resize events and would consume/interpret stdin bytes instead of passing them through raw. `std::io::IsTerminal` (stable stdlib) covers the separate "is this actually a terminal" checks; no crate needed for that part. Live terminal-resize forwarding (0.10.0) is built on `tokio::signal::unix`'s `SIGWINCH` listener instead of crossterm's `event`/`EventStream` — a plain OS signal, not a stdin-consuming abstraction, so it doesn't reintroduce the problem this entry warns off; `crossterm::terminal::size()` is still what's actually queried on each signal.
- **`portable-pty`** (dev-dependency, `ratect-compat/tests/cli.rs` only): creates a real (emulated) pseudo-terminal pair in-process, so an integration test can spawn `ratect-compat` attached to something that genuinely passes `IsTerminal` checks and actually drive an interactive session — no existing test infrastructure here could otherwise exercise that path at all. Works in headless CI; no real terminal required. A reusable pattern worth reaching for again for any other feature that's only meaningfully testable from a real terminal.
- **`nix`** (`features = ["user"]`): looks up the real host user (`Uid`/`Gid::current`, `User`/`Group::from_uid`/`from_gid`) for `run_as_current_user` (`ratect-core/src/user.rs`) — Unix-only, matching Ratect's own Unix-only testing so far. Already resolved in `Cargo.lock` transitively (via `portable-pty`'s own dependency graph in `ratect-compat`'s dev-dependencies); adding it directly to `ratect-core` was a low-risk addition, not a new unknown quantity.
- **`url`**: parses/rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal` in `ratect-core/src/proxy.rs`. Already resolved in `Cargo.lock` transitively (via `bollard`'s own dependency graph) — same low-risk-addition reasoning as `nix` above.
- **`unicode-width`**: real terminal display-column widths (CJK wide characters count as 2, zero-width/combining marks count as 0) for `ratect-core/src/ui/fancy.rs`'s repaint-width clipping and `ratect-core/src/ui/interleaved.rs`'s prefix-column padding — a plain `char`/byte count under-measures exactly those characters, which let a rendered line silently wrap onto more terminal rows than the fancy logger's own cursor-movement math accounts for. Zero transitive dependencies; the same crate ripgrep/bat/etc. use for this.
- **`bytes`** (`ratect-core` dev-dependency only): constructs `bollard::container::LogOutput` values directly in `docker.rs`'s own unit tests (`drain_interleaved_log_stream`'s tests, which feed it a synthetic log stream via `futures::stream::iter` instead of needing a live daemon) — `LogOutput`'s variants wrap a `bytes::Bytes` message, which bollard itself doesn't re-export. Already resolved in `Cargo.lock` transitively (via `bollard`/`hyper`'s own dependency graph) — same low-risk-addition reasoning as `nix`/`url` above.
- **`serde_json`**: parses the Docker CLI's own context-store JSON files for `--docker-context` (`ratect-core/src/docker.rs`'s `docker_context_host`/`active_docker_context`) — `<config_directory>/contexts/meta/<sha256(name)>/meta.json`'s `Endpoints.docker.Host`, and `<config_directory>/config.json`'s `currentContext`. `serde` itself was already a dependency (for `noyalib`'s `compat-serde-yaml`); `serde_json` is the standard, ubiquitous choice for the same derive-based approach applied to actual JSON.
- **`rcgen`**, **`tokio-rustls`**, **`time`** (`ratect-core` dev-dependencies only): generate a throwaway self-signed CA + leaf certificate/key pair at test run time and run a real in-process TLS server against it, for `docker.rs`'s `--docker-tls`/`-verify` tests (`connect_over_tls_completes_a_real_handshake_against_a_valid_certificate`/`_rejects_an_expired_certificate`) — proving an actual `rustls` handshake succeeds against a valid certificate and fails against an expired one, through Ratect's own `connect` path, not just that a client object builds. Generating at test time (rather than a fixed PEM committed to the repo) means validity is always computed relative to "now" — a static embedded certificate would eventually expire on its own and fail with a stale, disconnected-looking failure years later, unrelated to whatever change actually triggered it; `rcgen` also makes it trivial to generate a deliberately-already-expired certificate on demand for the rejection test. All three already resolved transitively (via `bollard`'s `ssl` feature and its own `rustls` dependency) — low-risk additions, not new unknown quantities.

- **`ssh-key`** (`default-features = false`, features `std`/`ed25519`/`rsa`/`p256`/`p384`/`p521`/`getrandom`), plus **`ssh-encoding`**, **`signature`** and **`rsa`**: parse OpenSSH private key files and sign with them, for `build_ssh`'s `paths` (`ratect-core/src/ssh_agent.rs`, 0.25.0). Ratect's **only cryptographic dependency**, taken knowingly — [decisions/0005](decisions/0005-build-ssh-keyring-placement.md) chose owning that choice over pushing it onto the `bollard` fork, and it deserves proportionate scrutiny plus an eye on advisories (`cargo audit` already runs in CI). Feature selection is the whole cost control: this is the RustCrypto tree, so it's granular by design (~45 crates), and each flag is a deliberate call — `encryption` is **off**, which alone saves ~16 crates and matches Go BuildKit's own inability to use a passphrase-protected key; `p384`/`p521` cost only two crates on top of `p256`, so all of ECDSA is covered rather than an arbitrary slice of it; `dsa` is off (removed from OpenSSH entirely). `std` is what provides `PrivateKey::read_openssh_file`. **`getrandom` is load-bearing and looks redundant** — it is what puts `OsRng` in `rand_core`, and `OsRng` is what `rsa_signature` passes to `try_sign_with_rng`, the blinded path `.cargo/audit.toml` names as the sole mitigation for RUSTSEC-2023-0071. Removing the flag compiles today only because `p256`/`p384`/`p521` happen to enable `rand_core/getrandom` themselves; the day the curve features change, blinding would go with them and nothing would say so. Declared explicitly for that reason, not because the build currently needs it. The other three add **no** crates — all are already in `ssh-key`'s own tree — and exist only because Rust's strict-deps rule won't let us `use` a transitive dependency's items: `ssh-encoding` for the `Encode`/`Decode` traits, `signature` for the `Signer`/`Verifier` traits (`ssh-key` re-exports neither), and `rsa` to sign under `rsa-sha2-256` and to work around 0.6.7's broken RSA key conversion. **Take SHA-2 from `ssh_key::sha2`, not from `ratect-core`'s own direct `sha2`**: the two are different major versions (0.10 vs 0.11) and only the re-export matches what `rsa` expects. Not `ssh-agent-lib`: it would have added ~5 more crates including a socket-activation tree (`service-binding`/`raunch`) for a use case we don't have, while still leaving the per-algorithm signing to us — and this module has to own socket creation anyway, to make it `0700`.
- **`rand_chacha`** (`ratect-core` dev-dependency only): a deterministic RNG for generating Ed25519 test keys in `ssh_agent.rs`'s unit tests, so a failure is always the same failure. Already resolved transitively (via `ssh-key`'s own tree) — same low-risk-addition reasoning as `nix`/`url`/`bytes` above. RSA test keys are *not* generated this way: a 2048-bit keygen takes tens of seconds in an unoptimized test build, so those tests use a throwaway key embedded in the test module instead.
- **`schemars`**, **`jsonschema`** (`ratect-core`, both optional — enabled only by its non-default `schema` feature): generate the committed JSON schema for `batect.yml` (`ratect-core/src/schema.rs`) and, in that module's own tests, validate the repository's fixtures against it. Optional so neither shipped binary carries the derived `JsonSchema` impls or a JSON-schema validator; `jsonschema` is declared as an optional *normal* dependency despite being test-only because Cargo doesn't allow optional `[dev-dependencies]`, and `default-features = false` keeps `reqwest` and a TLS stack out of the tree (nothing here ever resolves a remote `$ref`). Ratect's first optional dependencies — every other entry above is unconditional.

Dependencies are split across the four `Cargo.toml`s along CLI-vs-core lines: `clap`
and `tracing-subscriber` are `ratect-compat`-only; `serde`, `serde_json`, `noyalib`,
`bollard`, `futures`, `async-recursion`, `async-trait`, `uuid`, `tar`, `path-clean`,
`crossterm`, `nix`, `url`, `sha2`, `toml`, `regex`, `unicode-width`, `rustls`,
`ssh-key`/`ssh-encoding`/`signature`/`rsa`,
`schemars`/`jsonschema` (optional, `schema` feature), and the
local `dockerignore` crate are `ratect-core`-only (`dockerignore` itself depends on
`regex` and `path-clean` too); `anyhow`, `tracing`, and `tokio` are needed by both
`ratect-compat` and `ratect-core`. `tokio` is a normal dependency in both crates now —
`ratect-core`'s non-test code needs it too, for `build_context_tar`'s
`tokio::task::spawn_blocking` (it used to be a `ratect-core` dev-dependency only, for
`#[tokio::test]` in its unit tests). `portable-pty` is `ratect-compat`'s first
`[dev-dependencies]` entry. The placeholder `ratect` crate has no dependencies of its
own yet.

## Tooling & CI

- **Formatting/Linting**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored). One shared lockfile covers the whole workspace. Accepted advisories live in [`.cargo/audit.toml`](.cargo/audit.toml) — currently one, RUSTSEC-2023-0071 (the Marvin timing attack in `rsa` 0.9.x, which has no fixed release). **Every entry there carries a written justification**: what the advisory covers, why it's accepted rather than fixed, what mitigates it meanwhile, and what would let it be removed — an ignore without that is indistinguishable from silencing the check, and `cargo audit` prints nothing about what it skipped. An advisory with a fixed release available never belongs there; upgrade instead.
- **Tests**: `cargo test --workspace` runs in CI, covering unit tests per module (pattern matching in `dockerignore`, config parsing/resolution, expression interpolation, build-context tar construction, interactive-TTY eligibility, user-mapping generation, and task engine logic — dependency cycles, prerequisite dedup, sidecar/dependency resolution, dependency readiness (health-wait/setup-command ordering and failure paths), environment merging, image resolution — via a fake `ContainerRuntime`) and CLI argument/behavior tests in `ratect-compat/src/main.rs`/`ratect-compat/tests/cli.rs`. `ratect-compat/tests/cli.rs` also has end-to-end tests (`#[ignore]`d by default, run explicitly via `cargo test --workspace --test cli -- --ignored`) that exercise a real Docker daemon against the fixtures under `ratect-compat/tests/fixtures/` — one per feature (sidecars, dependency readiness, environment/config variables, image building, `.dockerignore`, interactive mode, user mapping, hostnames/ports, proxy, `--use-network`). These also run as their own `docker-integration` CI job (`--workspace --test cli` picks up `ratect`'s own `ratect/tests/cli.rs` too, against its own `ratect/tests/fixtures/`). See the fixture files themselves for what each one proves.
- **Where a fixture lives — by *layer*, not by binary.** The two fixture sets look lopsided (~36 under `ratect-compat/tests/fixtures/`, a handful under `ratect/tests/fixtures/`), but that's correct, not debt. Most of `ratect-compat`'s fixtures don't test the *flat CLI* at all — they drive `ratect-core`'s engine/Docker behaviour (sidecars, readiness, build options, devices, proxy, tmpfs, the SSH-agent socket, …) against a real daemon, with the CLI as a mere harness. That behaviour is proven *once*: exhaustively in `ratect-core`'s own fake-`ContainerRuntime` unit tests, and end-to-end here. So the rules:
  - **Core engine/Docker behaviour → a `ratect-compat` fixture.** It's the permanent home because `ratect-compat` is permanently `batect.yml`-format (compatibility requires it — see `ROADMAP.md`), so those fixtures never have to change format, and its thin CLI exercises the whole stack. Don't re-prove the same behaviour through `ratect`'s CLI — that re-tests the engine via a second driver for no added confidence; `ratect` proves *its* CLI reaches the engine with one representative e2e (`run_executes_a_task_via_docker`) and inherits the rest.
  - **A binary's own CLI surface → that binary's fixtures.** `ratect`'s set is small *because it should be*: its subcommand surface (`tasks.yml`) and its own verbs (`caches`/`resources`/`labels`, which `ratect-compat` doesn't have). From 0.3.0 it also has fixtures in `ratect`'s *own* config format — which is the deeper reason the two sets are never merged into a shared directory: a file can't be both a valid `batect.yml` and a valid `ratect`-native config, so a "common" fixtures dir would have to fork exactly when that format lands (the very next release). The fixtures belong to the format, and the format is `ratect-compat`'s permanent territory. CI runs the non-Docker suite as `cargo test --workspace --all-targets --all-features` — the `--all-features` part is what runs `ratect-core`'s `schema` module tests (see the module list above); plain `cargo test --workspace` skips them, so run `cargo test -p ratect-core --features schema` after touching anything in `config.rs`. When a config type changes, regenerate *both* committed schemas (`batect.yml`'s and `ratect.toml`'s) with `RATECT_UPDATE_SCHEMA=1 cargo test -p ratect-core --features schema schema::` and commit the result alongside — the test fails, with that same command in its message, if you don't.
- **Coverage**: `cargo llvm-cov --workspace --show-missing-lines --summary-only` (requires `rustup component add llvm-tools-preview` and `cargo install cargo-llvm-cov`) reports exact uncovered lines per file — use it to find gaps, not to chase a percentage. `cargo llvm-cov --workspace --html` opens a browsable report at `target/llvm-cov/html`. CI runs this and uploads the HTML report as a `coverage-report` artifact (non-gating).

## Current Status & Roadmap

Ratect is currently a **Work in Progress**. For a detailed list of supported features and our future plans, please refer to the [ROADMAP.md](ROADMAP.md) file.

## User Documentation

The `docs/` directory is user-facing documentation (installation, getting started, architecture, CLI reference, config reference, differences from Batect) — **not** ROADMAP.md/AGENTS.md/CHANGELOG.md/`decisions/`, which are project-management/contributor docs. `docs/` deliberately does not assume familiarity with Batect's own documentation, since Ratect's behavior is a subset of and sometimes diverges from it.

**`docs/ratect-config-reference.md` defers to `docs/config-reference.md` for most
field semantics, and that deferral is only safe while the differences are about
*shape*.** It says every field "applies, with the same meaning" and then links
into the `batect.yml` reference from ~19 places, so any behaviour that is
`ratect-compat`-only silently falsifies it for native readers who followed a
link. The first semantic divergence (0.25.0's image-source validation, which
`extends` requires the native format not to have) is why there is now a **Where
the semantics differ** table in the native reference: a new behavioural
divergence needs a row there *and* a marker at the compat end of the link, not
just a sentence wherever it was implemented.

The [`decisions/`](decisions/) directory holds Architecture Decision Records — the **cross-cutting** decisions that get referenced from more than one place (the two-binary split, the runtime-ownership labels, the native config format, trusting a Git include's host paths). Its [`README.md`](decisions/README.md) states the convention; see guideline 14 below for when to write one.

## Guidelines for AI Agents

1.  **Idiomatic Rust**: Always strive for idiomatic and safe Rust. Use `anyhow::Context` to provide meaningful error messages.
2.  **Async/Await**: The codebase is heavily asynchronous. Ensure new I/O or Docker-related code uses `await` and integrates with the `tokio` runtime.
3.  **Dependency Management**: Keep each `Cargo.toml` clean and dependencies updated — and in the right crate (CLI-only deps in `ratect`'s `Cargo.toml`, everything else in `ratect-core`'s). If a library becomes deprecated or unmaintained, propose a migration to a better alternative.
4.  **Configuration Consistency**: When extending the `batect.yml` parser in `ratect-core/src/config.rs`, try to maintain compatibility with the original Batect configuration format.
5.  **State Management**: In `ratect-core/src/engine.rs`, state (like executed tasks) is shared using `Mutex` to ensure thread safety across async tasks. Be mindful of locking logic.
6.  **Verification**: After making changes, verify them by:
    -   Running `cargo build --workspace` to ensure compilation.
    -   Executing `cargo run -p ratect-compat -- -f ratect-compat/tests/fixtures/smoke.yml --list-tasks` to check config parsing.
    -   Running a sample task (e.g., `cargo run -p ratect-compat -- -f ratect-compat/tests/fixtures/smoke.yml test-task`) to verify the execution engine and Docker integration. (The repository root's `batect.yml` is Ratect's *own* dev-task config — we build Ratect with Ratect, dogfooding the tool: `cargo run -p ratect-compat -- build`/`test`/`lint`/`fmt` run each in a pinned Rust container with the Cargo registry and build output as `cache` volumes. A root `ratect.toml` mirrors it in the native format, so the same tasks also run through the `ratect` binary (`cargo run -p ratect -- run build`), dogfooding *both* binaries and their two config formats; `ratect-core`'s `the_two_root_dev_configs_agree` test resolves both files and fails if they drift, so an edit to one must be mirrored in the other. That's precisely what the root path *should* hold — this project's own dev tasks — which is why test fixtures deliberately live under `tests/fixtures/` instead, never at the root, so the two are never confused.)
7.  **Changelog Maintenance**: After completing a task that changes the project's features, dependencies, or structure, ensure that `CHANGELOG.md` is updated in the "Unreleased" section, following the "Keep a Changelog" standard.
8.  **Version Lifecycle**: When cutting a release, it's not just a version bump — follow the full process documented in [ROADMAP.md](ROADMAP.md#versioning--releases): the `X.Y.Z-dev` → `X.Y.Z` bump commit, tagging it `<binary>/vX.Y.Z` (prefixed since `ratect` and `ratect-compat` are on independent version lines that would otherwise collide — bare `vX.Y.Z` tags are pre-split history), and publishing it as a GitHub Release (body = that release's `CHANGELOG.md` section). There's one shared `CHANGELOG.md`, whose release headings name every version in that release (`## [ratect-compat 0.21.1 · ratect 0.2.0]`) and whose entries name a binary only when they don't apply to both — see ROADMAP.md for why it isn't split per binary. Starting the next version's development is a separate, later commit that bumps every crate back to a `X.Y.Z-dev`. Neither bump is ever folded into a feature commit.
9.  **ROADMAP.md Maintenance**: a `~~strikethrough~~` marking completed scope is
    **inline markdown and cannot cross a blank line** — a multi-paragraph entry
    needs the `~~` closed at the end of each paragraph and reopened at the start
    of the next, or the markers render literally and the entry still reads as
    outstanding. Check a struck entry rendered, not just that the markers are
    balanced; GitHub's own `/markdown` API answers it in one call. Beyond that, its `## Batect Parity` headline list and its versioned `### ratect-compat` list follow different edit rules. The headline list is a living summary — freely edit, merge, or delete bullets as scope changes or ships (e.g. "Sidecar Containers" and "Docker Networking" were merged into "Full Docker Networking" once shipped). The versioned list is append-only history — never delete an entry; mark completed scope with `~~strikethrough~~` plus a done-summary of what actually shipped.
10. **User Docs Maintenance**: When a change affects user-visible behavior (CLI flags, config schema, runtime behavior, Batect parity), update the relevant file(s) under `docs/` in the same change — don't let them drift from the code. If you find the code doesn't match what's documented, fix whichever one is wrong rather than leaving the mismatch.
11. **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, non-fatal error conditions like a best-effort cleanup failure) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout. One deliberate exception: `main.rs`'s single top-level fatal error (the reason the process is about to exit non-zero) is `eprintln!`ed directly, *not* through `tracing::error!` — it must stay visible even under `RUST_LOG=off`, since every output mode (including `-o quiet`, whose whole contract is "only error messages") otherwise has nowhere else to show it. Found and fixed during 0.16.0's output-modes review — don't revert it back to `tracing::error!`.
12. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff. Every commit is signed off (`git commit -s`) — the [DCO](https://developercertificate.org) attestation CONTRIBUTING.md describes and CI enforces on pull requests; direct commits to `main` follow the same convention for consistency.
13. **Commit Packaging**: a release that's one theme (like most 0.x releases so far) lands as a single `feat:` commit. A release bundling several genuinely separable behaviors (e.g. 0.6.0's networking + proxy work) should instead split into one `feat:` commit per behavior, each with its own tests and doc updates — easier to review and to `git bisect`/`git revert` than one large commit. The version bump and any docs-only release summary stay separate commits either way (see 8).
14. **Architecture Decision Records** ([`decisions/`](decisions/)): the home for a decision's rationale is decided by whether it's **cross-cutting or version-scoped**. A decision referenced from more than one place — the two-binary split, the labels namespace, the native config format — becomes an ADR (`decisions/NNNN-slug.md`, `Status`/`Context`/`Decision`/`Alternatives considered`/`Consequences`), and its ROADMAP.md entry shrinks to a summary plus a `decisions/NNNN` pointer. A decision that belongs to one release stays **inline** in that release's ROADMAP.md entry, using the existing "Scope, settled before building:" / "As built:" subsection pattern — don't extract it. Practical trigger: a decision earns an ADR the moment it's about to be referenced from a *second* place; most never cross that line. ADRs are append-only like the versioned lists — supersede and link forward, never delete. See [`decisions/README.md`](decisions/README.md) for the full convention.
15. **Review before committing, not after.** Run a review pass over the working diff (`/code-review`) *before* each commit rather than over a run of commits afterwards. Adopted after 0.25.0's interrupt work, where a post-hoc review found six issues that all existed at commit time — one of them a behaviour bug, not a slip. Four checks earned their place there, each having actually missed something:
    - **Anchor an inserted item on the preceding item's closing brace, never on the new one's attributes.** A Rust item's doc comment sits *above* its `#[test]`/`#[derive]` attributes, so anchoring an insertion there splices the new item into the previous one's documentation — silently, and the compiler is happy. This is how a new e2e test ended up wearing its neighbour's doc comment.
    - **Re-read every string you added, in its final control-flow position.** Log and error messages are correct when written and quietly become wrong as the code around them moves; nothing type-checks them. Two messages shipped claiming work that a flag had disabled, and naming a `ratect` verb from shared core that `ratect-compat` doesn't have. A
      related check for *errors* specifically: an error has to name something the
      user actually wrote. A lower layer speaks its own vocabulary — `docker.rs`
      knows an ssh agent id, not which container declared it — so an error
      crossing up from one needs the caller to attach that. `classify_ssh_agent_paths`
      shipped naming only the agent, in a codebase where every other config error
      names its container.
    - **Watch for coverage shaped by the test harness rather than the behaviour.** If the fake can only express one ordering of something inherently timing-dependent, the untestable orderings are where the bug will be — extend the harness instead of concluding the cases are covered. Every interrupt test could only pre-record interrupts *before* a run, and the broken case was an interrupt arriving mid-cleanup.
    - **A real-daemon test can mask a missing unit test.** The `#[ignore]`d Docker tests don't run in the default suite, so a path they cover can be entirely unprotected in `cargo test --workspace`. Assert each effect separately — one assertion per thing removed, not one for "cleanup happened".
