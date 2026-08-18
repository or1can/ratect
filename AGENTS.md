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
  | `include_trust.rs` | What a Git-included bundle may do: the grant rule, its native-only gate, and every refusal that cites them |
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

**Every dependency's rationale is a comment above its own declaration**, in the
`Cargo.toml` that declares it — why it is there, which features are on and why,
what was rejected instead, and what would let it be dropped. That is the file
you are in when adding, removing, or re-featuring one, which is exactly when the
reasoning matters and the moment it was previously missed (`ssh-key`'s feature
list drifted from this file's prose in the same release that added it). See
[decisions/0006](decisions/0006-code-and-documentation-locality.md).

Two things worth knowing without opening a `Cargo.toml`:

- **`bollard` is consumed through a `[patch.crates-io]` fork** pinned in the root
  `Cargo.toml`, whose comment explains the branch rules — cut from upstream
  `master`, rebased and never merged into. Getting that wrong dead-ended a
  branch once already.
- **`ssh-key` and its companions are Ratect's only cryptographic dependency**,
  taken knowingly ([decisions/0005](decisions/0005-build-ssh-keyring-placement.md)),
  and the one accepted `cargo audit` advisory belongs to that tree — see
  [`.cargo/audit.toml`](.cargo/audit.toml), which carries its own justification.


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

[`CONTEXT.md`](CONTEXT.md) at the root is the **glossary**: what each term in
the configuration model denotes, and nothing else — no implementation detail,
no decisions, no behaviour. It exists because several of this project's bugs
have been one word covering two concepts (a *grant* is written on an include
entry, an *effective boundary* is what a file ends up with; `ConfigFormat` is a
*project's* dialect, not a *file's* syntax). Add a term when settling one
resolves an ambiguity, not to catalogue vocabulary that was never in doubt.

So: glossary at the root, cross-cutting rationale in `decisions/`, user-facing
behaviour in `docs/`, contributor process here. `decisions/` deliberately is
*not* `docs/adr/` — `docs/` is the user-facing tree, and ADRs are for
contributors. Moving them would also break links from already-released
CHANGELOG sections, which are append-only.

## Guidelines for AI Agents

### Working principles

Reproduced verbatim (headings demoted to fit this document) from
[andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills/blob/main/CLAUDE.md),
MIT-licensed — see [`NOTICE`](NOTICE) for the attribution — so anyone working in
this repo has them without installing anything. Unlike the numbered guidelines below, none of these was written after
something went wrong in Ratect — they are general habits, and where the two ever
disagree the specific one wins. Two places they meet are noted after the
principles; don't read either as an exception to them.

Behavioral guidelines to reduce common LLM coding mistakes. Merge with
project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial
tasks, use judgment.

#### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

#### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

#### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

#### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.

#### Where these meet Ratect's own rules

Two of the principles above sit next to a repo rule that reads like its
opposite. Neither is an exception:

- **Simplicity first** governs *code*. Explanatory prose is governed by
  [0006](decisions/0006-code-and-documentation-locality.md), which asks for more
  of it deliberately — a module here is often mostly doc comment, and that is
  not the 200-lines-could-be-50 case.
- **Surgical changes** and guideline 16's "fix the class, not the instance"
  agree: when the class is the defect, the class is what you must touch, and a
  sweep that comes back clean changes nothing. What neither licenses is
  improving code you happened to read on the way.

### Repo-specific guidelines

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

      `python3 tools/spliced-docs.py` finds the ones that get through
      anyway — which is how `load_project`, `resize_tty` and `labels_for`
      turned out to have lost theirs, the first of them long enough for a
      reviewer to find it on `main`. Four candidates on this repo and a
      hand sweep over the same list missed two of them, so read all four
      rather than trusting either. Not a gate: two are long docs whose
      paragraphs open with a summary verb, and it exits 0 regardless.
    - **Re-read every string you added, in its final control-flow position.** Log and error messages are correct when written and quietly become wrong as the code around them moves; nothing type-checks them. Two messages shipped claiming work that a flag had disabled, and naming a `ratect` verb from shared core that `ratect-compat` doesn't have. A
      related check for *errors* specifically: an error has to name something the
      user actually wrote. A lower layer speaks its own vocabulary — `docker.rs`
      knows an ssh agent id, not which container declared it — so an error
      crossing up from one needs the caller to attach that. `classify_ssh_agent_paths`
      shipped naming only the agent, in a codebase where every other config error
      names its container.
    - **When a change alters observable behaviour, re-read that behaviour's whole
      doc section and *run* each claim against the binary.** Not grep — execute.
      Every claim: the example output, the flag descriptions, the "this does X"
      sentences. Capture real output and `diff` it rather than editing what looks
      wrong, since what looks wrong is exactly the set you already believe.

      This is a rewrite of a check that said to *grep for strings naming the old
      scope*, which failed on its first outing. `ratect caches list` widened from
      a project's caches to the machine's shared ones as well, and the grep — for
      "this project's caches" and friends — missed `"This project has no
      caches."`, `--scope`'s "Listing shows both by default", and a paragraph
      describing deduplication, because none of them contained the words expected.
      Worse, it reported clean, so the sweep was recorded as done. Executing the
      section's claims caught all three, and had already caught a stale example
      block the grep also missed.

      Grep is the fallback for claims nothing can be run against (a roadmap
      entry, a design note). It is not the check.

      For *finding* which claims to re-read — as opposed to checking one you
      already suspect — `python3 tools/stale-claims.py` ranks prose by how
      much the code it names has moved since the claim was last touched.
      Roughly 30 candidates on this repo, so it is a skim, not a report. It
      measures churn rather than wrongness, so expect false positives (a
      claim about a hot file looks suspicious while staying true) and treat
      a hit as "re-read this", never as "this is wrong". Its motivating case
      is [0006](decisions/0006-code-and-documentation-locality.md), whose
      empirical detector was true when written and was invalidated by the
      ADR's own Rule 1 — nothing else in the repo looks for that shape.

      The stakes are not cosmetic: a stale heading claiming shared caches were
      "Caches for this project:" made a documented idiom
      (`caches list -o quiet | xargs ratect caches clean`) delete another
      project's cache. Across two rounds this one habit accounted for eight
      findings.
    - **Verify a claim before writing it, not after a reviewer questions it.**
      "No name that worked before stops working" was written into a CHANGELOG
      entry and two doc pages, and was false — the new validation had been
      enforced by Docker for volume caches only, so directory caches silently
      became a breaking change. Disabling the check and running it took a
      minute. An unverified claim about past behaviour is a guess with a
      citation's confidence.
    - **A behaviour that depends on which format/mode you're in needs one
      derived value, not a guard per call site.** `allow_nested_git_includes`
      got this wrong twice in one change: the gate was written unguarded (the
      existing compat tests caught it), and then the *error redaction* beside
      it was left unguarded too (review caught it, because its own tests were
      all native). Each site guarded correctly in isolation; the invariant —
      "these behaviours are native-only, together" — was stated nowhere. Deriving
      `restricted: Option<&Bundle>` once (`include_trust::restricting`), and
      having every site consume it, makes the divergence unrepresentable.
      Guarding site-by-site means the next site added is unguarded by default.
    - **Never spell config syntax in an error message — name the field.**
      `set 'x' to true`, not `add 'x: true'` or `'x = true'`. `allow_host_paths`
      shipped the YAML spelling in shared core, where `ratect.toml` readers hit
      it, and the new nested-include gate repeated the mistake in TOML.

      This started as a weaker rule — *"or prove the message fires in exactly
      one format"* — which is what the gate's author (me) did, correctly, and
      still got wrong: `ConfigFormat::Native` is the **project's** format, not
      the **file's**. A native project can locally include a `.yml`, so the very
      entry the message tells you to edit may be YAML. Any rule of the form
      "prove which syntax the reader is using" fails on mixed-format includes,
      which is why the rule is now unconditional. The first version of this rule
      survived one review and was refuted by the next.
    - **Watch for coverage shaped by the test harness rather than the behaviour.** If the fake can only express one ordering of something inherently timing-dependent, the untestable orderings are where the bug will be — extend the harness instead of concluding the cases are covered. Every interrupt test could only pre-record interrupts *before* a run, and the broken case was an interrupt arriving mid-cleanup.
    - **A test that cleans up after itself has to be run twice.** A single run
      passes whether or not the cleanup matched anything — the first run starts
      clean by definition. Extracting `remove_cache_mount_volumes` into a
      parameterised helper broke its match string, every conformance run leaked a
      Docker volume, and the suite stayed green because a *sibling* test was
      deleting the project's cache key and so handing each run a fresh volume
      name. Two defects concealing each other, both invisible to one run. Run it
      twice and assert the external state (`docker volume ls`), not just the exit
      code.
    - **Two tests sharing a fixture directory need a lock, not luck.** `cargo
      test` runs a binary's tests on several threads. `cache-mount` is driven by
      two conformance cases; they contend for `.batect/caches/key` even though
      they use different cache *types*, which surfaces as "the cache did not
      persist" rather than as a race. A `static Mutex` around the shared project
      is the cheap fix — see `CACHE_MOUNT_PROJECT`.
    - **A real-daemon test can mask a missing unit test.** The `#[ignore]`d Docker tests don't run in the default suite, so a path they cover can be entirely unprotected in `cargo test --workspace`. Assert each effect separately — one assertion per thing removed, not one for "cleanup happened".

16. **Fix the class, not the instance — and say what you swept.** A review finding
    is a *sample*, not the defect. Before fixing it, ask what else in the codebase
    has that shape and go and look; then report the sweep, including when it comes
    back clean. A negative result is information, and it saves the next reviewer
    re-deriving it.

    The cost of skipping this is measured in review rounds. Error attribution took
    two: one round found `classify_ssh_agent_paths` reporting an agent id with no
    container, the fix added context at that one call site, and the next round
    found `Keyring::start` beside it still bare. The structural fix — one wrapper
    attributing everything the build can fail on — was available the first time
    and would have made the second finding impossible. A review that only surfaces
    siblings of something already fixed is **failure demand**: work created by not
    having finished the job the first time.

    Three corollaries, each with a scar behind it:

    - **Prefer the structural fix when the local one leaves the invariant
      unstated.** 0.25.0's cleanup ownership is the precedent: three consecutive
      rounds each found a *distinct* bug in the same split between `run_container`
      and the engine, and retiring the split ended it — not a fourth correction.
      When one area keeps producing findings, the design is the finding.
    - **A repeated process error is a defect in the process, not in the attempt.**
      Resolving to be more careful is not a fix. Three git-staging errors in one
      day — `git add -A` merging two intended commits twice, then `commit --amend`
      landing on the wrong one — so the method changes: **stage explicit paths,
      never `-A`, whenever more than one commit is planned, and rebuild a
      mis-split pair with `reset --soft` rather than reaching for `--amend` or an
      interactive rebase.** (An earlier `rebase -i` fixup to correct one of these
      squashed the two commits it was meant to separate.) Four doc-comment splices
      produced the anchoring rule in 15. When something recurs, change the method,
      then write it down here.
    - **Not every finding is a class, and saying so is part of the job.** The
      sweeps that came back clean this cycle — ROADMAP/CHANGELOG overlap at 2%,
      appended module docs at 0–9%, exactly one stale "no such concept" claim in
      `docs/` — were each worth the minutes it took to know rather than assume,
      and worth reporting so nobody checks them again.
