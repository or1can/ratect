# Ratect AI Agent Guide

This file provides context, instructions, and guidelines for AI agents working on the Ratect project.

## Project Overview

Ratect is a Rust-based implementation of the [Batect](https://github.com/batect/batect) task execution engine. Its goal is to provide a fast, lightweight CLI for running development tasks in Docker containers, defined in configuration: `batect.yml` for the Batect-compatible `ratect-compat` binary, and a native `ratect.toml` for `ratect` (since its 0.3.0).

## Architecture

Ratect is a **Cargo workspace** with four crates (the
[two-binary split](decisions/0001-two-binaries.md) landed in 0.20.0):

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
  separate from `ratect-compat`'s [`docs/ratect-compat-cli.md`](docs/ratect-compat-cli.md)
  and [`docs/ratect-compat-config-reference.md`](docs/ratect-compat-config-reference.md) — two interfaces and
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
  | `include_trust.rs` | CONTEXT.md's whole **Boundary** — `Boundary` carries both the containment (which directory an include or a container's paths must stay inside) and the grants (`Bundle`'s `trust`) — plus the native-only nested-Git gate and the refusals that cite them. `check_may_declare_git`/`boundary_contains` are the two "may this be read at all" checks the loader and shell completion both call directly, so neither hand-combines the nested-Git and containment checks itself |
  | `cache.rs` | `volumes` `cache` mounts → a named volume or host directory (`resolve_cache_mount`), and — a separate concept, same module — the `CacheStore` behind `ratect caches`/`ratect-compat --clean`/`--clean-cache`: what cache storage exists, its project/shared scope rule, and how to list or remove it |
  | `expressions.rs` | Batect's `$VAR`/`${VAR:-default}`/`<name` expression syntax |
  | `container_spec.rs` | What a container's runtime spec is, and what a build's is — `ContainerSpec`/`derive_spec` (from a container's config plus a task's `run`/dependency's `customise` overlay) and `BuildSpec`/`derive_build_spec` (for `build_image`'s own call) — shared vocabulary between `engine.rs` and `docker.rs`, owned by neither |
  | `docker.rs` (+ `ratect-core/src/docker/connection.rs`) | All `bollard`/daemon interaction, behind the fakeable `ContainerRuntime` trait — connection selection (`--docker-host`/`-context`/`-tls*`) split into its own submodule, no reference to `ContainerRuntime` at all |
  | `ssh_agent.rs` | `build_ssh`'s in-process ssh-agent (RFC 9987) — kept extractable, see [0005](decisions/0005-build-ssh-keyring-placement.md) |
  | `user.rs` | Host user lookup and the `/etc/passwd` generators for `run_as_current_user` |
  | `proxy.rs` | Proxy variable detection and propagation, the host-gateway entry a rewritten URL needs, and the loopback-bound proxy it can't fix |
  | `interrupt.rs` | Termination-signal (Ctrl+C, `SIGTERM`, `SIGHUP`) tracking — the signal half only; the engine decides what it means |
  | `engine.rs` | Task lifecycle, prerequisites, dependency graph, cleanup |
  | `exit_code.rs` | What a failed run exits with — the one mapping both binaries share, since `docs/ratect-cli.md` promises their codes are identical |
  | `labels.rs` | The `eu.orican.ratect.*` ownership labels, see [0002](decisions/0002-runtime-ownership-labels.md) |
  | `registry_auth.rs` | Resolving private registry credentials via real Docker credential helpers — the `RegistryCredentialResolver` trait, its `DockerCredentialHelperResolver` production implementation, registry-hostname extraction (for pull), and `config.json` registry-key enumeration (for build, which resolves every configured registry rather than one). A filesystem-and-subprocess concern kept out of `docker.rs`, which wires both into `pull_image`/`build_image` |
  | `resources.rs` | What previous runs left behind and how it's removed — `ratect resources list`/`clean`'s selection and removal, generic over its own `ResourceInventory` (the container/network inventory slice of `ContainerRuntime`, which requires it as a supertrait) |
  | `diagnostics.rs` | `Finding` and every producer of one that needs no daemon connection of its own — `ratect doctor`/`config validate`'s shared checks |
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

- **Documentation checks**: the [`claims`](https://github.com/or1can/claims)
  Claude Code plugin (enabled in `.claude/settings.json`, configured for this
  repo in `claims.toml`) — not the four hand-rolled `tools/` scripts this
  repo used before it existed (`stale-claims.py`, `spliced-docs.py`,
  `echoed-claims.py`, `verify-docs.py`; see
  [decisions/0009](decisions/0009-adopt-claims-plugin.md) for the
  migration). Inside a Claude Code session with the plugin enabled, it runs
  automatically before every `git commit` via a `PreToolUse` hook — a gate
  finding blocks the commit, an advisory finding is reported alongside it —
  and on demand via the `check-claims` skill (ask an agent to check
  claims, any time mid-task, not only at commit time). Checks: `stale-claims`
  (advisory, ranks Markdown prose by how much the code it names has moved
  since the claim was last touched — generalized from this repo's own former
  stale-claims script), `spliced-docs` (advisory, doc comments that document
  an item other than the one they sit on — generalized from this repo's own
  former spliced-docs script to sweep every tracked `.rs` file rather than a
  narrower scan), `restatement` (advisory, prose a diff retracted that's
  still asserted verbatim elsewhere — generalized from this repo's own
  former echoed-claims script), `executable-claims` (gate, `<!-- verify: ...
  -->` markers — generalized from this repo's own former verify-docs
  script), plus three checks with no prior equivalent here: `check-links`
  (gate, dead internal Markdown links/anchors), `check-citations` (gate, a
  backticked name citing a symbol this repo once declared but no longer
  has, Markdown/Swift only — no Rust support yet), and
  `claim-words`/`judgment-agent` (advisory, totalising language and
  architecture-claim review, opt-in via `claims.toml`'s `[claim-words]
  files` — this repo opts in `AGENTS.md`, `CONTEXT.md`, `ROADMAP.md`,
  `RELEASES.md`, `TODO.md`, `decisions/*.md`, `docs/**/*.md`). The four
  former scripts' own paths and the full migration story are in
  [decisions/0009](decisions/0009-adopt-claims-plugin.md) — not repeated
  here now that they no longer exist to link to.

  `claims.toml` also widens `restatement`'s default `extensions` to add
  `.rs` — see its own comment for why.

  `claims.toml` also scopes `stale-claims`'s bare-name module matching to
  `decisions/`/`AGENTS.md` — see its own comment for why — and raises
  `executable-claims`'s timeout past this repo's own cold-build time.

  Every noise/timeout issue found comparing 0.2.2 against this repo's old
  tooling is now fixed upstream (`decisions/0009` has the detail and issue
  links). `executable-claims` also refuses to run a `<!-- verify: -->`
  marker containing shell chaining, redirection, substitution, or
  `sed`/`awk`/`grep` — a fixed, no-config-needed gate, not a per-project
  knob.

  **`executable-claims` denies execution by default, gated on a local,
  git-ignored grant** (`claims.local.toml`, sibling to `claims.toml` —
  never commit it; `.gitignore` already excludes it, see the plugin's own
  [deny-by-default ADR](https://github.com/or1can/claims/blob/main/docs/adr/0001-executable-claims-deny-by-default.md)).
  A marker's command
  with no exact-string entry in that file's `[executable-claims]` section
  is a **gate finding** naming the command and the exact TOML to add,
  regardless of whether it's actually safe — a fresh clone hits this
  immediately on this repo's markers (`AGENTS.md`'s smoke-test example
  above, and the task listings on
  [`docs/worked-examples.md`](docs/worked-examples.md),
  [`docs/getting-started.md`](docs/getting-started.md) and
  [`docs/ratect-compat-config-reference.md`](docs/ratect-compat-config-reference.md)).
  Add, once per machine:

  ```toml
  [executable-claims]
  allowed = [
      "cargo run -q -p ratect-compat -- -f ratect-compat/tests/fixtures/smoke.yml --list-tasks",
      "cargo run -q -p ratect-compat -- -f examples/jvm/batect.yml --list-tasks",
      "cargo run -q -p ratect-compat -- -f examples/jvm/batect.yml -o quiet --list-tasks",
      "cargo run -q -p ratect -- tasks list -f examples/rust/ratect.toml",
      "cargo run -q -p ratect -- tasks list -f examples/getting-started/ratect.toml",
  ]
  ```

  This closes the automatic-execution risk `decisions/0009` originally
  documented as an accepted, unresolved gap — filed upstream as
  [or1can/claims#15](https://github.com/or1can/claims/issues/15), now
  fixed and no longer merely mitigated by caution.

  **The hook is local, not CI-enforced** — a PR opened without Claude Code
  (a plain shell commit, another editor, or a bot like Renovate) never runs
  any of this. See `TODO.md`'s Maintainability section for why that isn't
  wired into CI yet.
- **Formatting/Linting**: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass; both are enforced in CI (`.github/workflows/ci.yml`).
- **Dependency Audit**: `cargo audit` runs in CI against `Cargo.lock`, which is committed to the repo (binary crate convention, not gitignored). One shared lockfile covers the whole workspace. Accepted advisories live in [`.cargo/audit.toml`](.cargo/audit.toml) — currently one, RUSTSEC-2023-0071 (the Marvin timing attack in `rsa` 0.9.x, which has no fixed release). **Every entry there carries a written justification**: what the advisory covers, why it's accepted rather than fixed, what mitigates it meanwhile, and what would let it be removed — an ignore without that is indistinguishable from silencing the check, and `cargo audit` prints nothing about what it skipped. An advisory with a fixed release available never belongs there; upgrade instead.
- **Tests**: `cargo test --workspace` runs in CI, covering unit tests per module (pattern matching in `dockerignore`, config parsing/resolution, expression interpolation, build-context tar construction, interactive-TTY eligibility, user-mapping generation, and task engine logic — dependency cycles, prerequisite dedup, sidecar/dependency resolution, dependency readiness (health-wait/setup-command ordering and failure paths), environment merging, image resolution — via a fake `ContainerRuntime`) and CLI argument/behavior tests in `ratect-compat/src/main.rs`/`ratect-compat/tests/cli.rs`. `ratect-compat/tests/cli.rs` also has end-to-end tests (`#[ignore]`d by default, run explicitly via `cargo test --workspace --test cli -- --ignored`) that exercise a real Docker daemon against the fixtures under `ratect-compat/tests/fixtures/` — one per feature (sidecars, dependency readiness, environment/config variables, image building, `.dockerignore`, interactive mode, user mapping, hostnames/ports, proxy, `--use-network`). These also run as their own `docker-integration` CI job (`--workspace --test cli` picks up `ratect`'s own `ratect/tests/cli.rs` too, against its own `ratect/tests/fixtures/`). See the fixture files themselves for what each one proves.

  A separate `worked-examples` CI job runs `build`/`test`/`run`/`lint`/`shell` against a real Docker daemon for every project under [`examples/`](examples/) (`shell`'s own invocation appends `-- -c '...'`, since it's interactive by design and would otherwise hang CI waiting on a TTY that never arrives; `full-stack` and `getting-started`, whose task names differ, each get their own section of the job running their own tasks) — the real, checked-in hello-world projects [`docs/worked-examples.md`](docs/worked-examples.md) `{{#include}}`s its config from directly, so the published page can't silently drift from something that no longer actually runs (as happened once, caught only by running these by hand — see that page's own note on `command` having no shell). Not `#[ignore]`d/opt-in like the fixture tests above: every push and PR runs it, since `examples/` is small and its whole point is staying provably current.
- **Where a fixture lives — by *layer*, not by binary.** The two fixture sets look lopsided (~36 under `ratect-compat/tests/fixtures/`, a handful under `ratect/tests/fixtures/`), but that's correct, not debt. Most of `ratect-compat`'s fixtures don't test the *flat CLI* at all — they drive `ratect-core`'s engine/Docker behaviour (sidecars, readiness, build options, devices, proxy, tmpfs, the SSH-agent socket, …) against a real daemon, with the CLI as a mere harness. That behaviour is proven *once*: exhaustively in `ratect-core`'s own fake-`ContainerRuntime` unit tests, and end-to-end here. So the rules:
  - **Core engine/Docker behaviour → a `ratect-compat` fixture.** It's the permanent home because `ratect-compat` is permanently `batect.yml`-format (compatibility requires it — see `ROADMAP.md`), so those fixtures never have to change format, and its thin CLI exercises the whole stack. Don't re-prove the same behaviour through `ratect`'s CLI — that re-tests the engine via a second driver for no added confidence; `ratect` proves *its* CLI reaches the engine with one representative e2e (`run_executes_a_task_via_docker`) and inherits the rest.
  - **A binary's own CLI surface → that binary's fixtures.** `ratect`'s set is small *because it should be*: its subcommand surface (`tasks.yml`) and its own verbs (`caches`/`resources`/`labels`, which `ratect-compat` doesn't have). From 0.3.0 it also has fixtures in `ratect`'s *own* config format — which is the deeper reason the two sets are never merged into a shared directory: a file can't be both a valid `batect.yml` and a valid `ratect`-native config, so a "common" fixtures dir would have to fork exactly when that format lands (the very next release). The fixtures belong to the format, and the format is `ratect-compat`'s permanent territory. CI runs the non-Docker suite as `cargo test --workspace --all-targets --all-features` — the `--all-features` part is what runs `ratect-core`'s `schema` module tests (see the module list above); plain `cargo test --workspace` skips them, so run `cargo test -p ratect-core --features schema` after touching anything in `config.rs`. When a config type changes, regenerate *both* committed schemas (`batect.yml`'s and `ratect.toml`'s) with `RATECT_UPDATE_SCHEMA=1 cargo test -p ratect-core --features schema schema::` and commit the result alongside — the test fails, with that same command in its message, if you don't.
  - **One documented exception: a fixture with no test at all.** `ratect-compat/tests/fixtures/manual-terminal-resize.yml` (ratect#73) backs a manual-only repro — a live terminal's own reflow rendering isn't something any headless harness here, `portable-pty` included, can reproduce or assert against. Its own header comment carries the steps; don't read its lack of a `#[ignore]`d test as orphaned or delete it as dead weight.
  - **[`examples/`](examples/) is neither fixture set — it's user-facing, not test-owned.** A real per-ecosystem project (Rust, Go, Node.js, Python, Gradle/JVM) that [`docs/worked-examples.md`](docs/worked-examples.md) `{{#include}}`s its config from — plus `getting-started/`, the tutorial's own project, which `docs/getting-started.md` includes slice by slice — so what's documented is always exactly what's on disk. It doesn't belong under either `tests/fixtures/` directory because it isn't proving engine/Docker behaviour or a binary's CLI surface — that's already covered by the two sets above — it's proving that a real, cloneable project actually works, which is a different claim with a different audience. Verified anyway, by its own CI job (see Tests below), because a doc that claims something runs and doesn't check it is exactly the gap that job was built to close.
- **Coverage**: `cargo llvm-cov --workspace --show-missing-lines --summary-only` (requires `rustup component add llvm-tools-preview` and `cargo install cargo-llvm-cov`) reports exact uncovered lines per file — use it to find gaps, not to chase a percentage. `cargo llvm-cov --workspace --html` opens a browsable report at `target/llvm-cov/html`. CI runs this and uploads the HTML report as a `coverage-report` artifact (non-gating).

## Current Status & Roadmap

Ratect is currently a **Work in Progress**. For a detailed list of supported features and our future plans, please refer to the [ROADMAP.md](ROADMAP.md) file.

## Guidelines for AI Agents

### Working principles

Reproduced verbatim (headings demoted to fit this document) from
[andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills/blob/main/CLAUDE.md),
MIT-licensed — see [`NOTICE`](NOTICE) for the attribution — so anyone working in
this repo has them without installing anything. Unlike the numbered guidelines below, none of these was written after
something went wrong in Ratect — they are general habits, and where the two ever
disagree the specific one wins. Everything after them is this repo's own — the
change loop, and two places they meet a repo rule; don't read any of it as an
exception to them.

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

#### The change loop

**For any change:** specification → suite red → code → suite green → write
prose.

Derive the red **and** the code from the specification, independently. When the
two disagree, the fault may be in the red, the code, or the specification.

Specification first, because otherwise there is nothing to derive from
independently — a red written from unstated intent is the code by another
route. Usually one sentence, not a document; where the handoff or an ADR
already settles it, cite that rather than restate it.

Write the prose from the specification and the code **as delivered** —
re-read and re-run, not remembered; it is also how you reload both. "The code"
includes whatever the prose *names*: a sentence about something is written with
that thing open.

Red is omitted only where the change alters no behaviour; say so out loud when
it is.

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

**Constant gardening.** Read strictly, "touch only what you must" says to leave
every defect you notice in passing, and things left that way rot: the
`load_project` doc comment sat on the wrong function on `main` until a reviewer
found it, and `TODO.md` still described behaviour a release had deleted. So the
rule here is the opposite of leaving it — when you are already working in an
area and you spot something wrong, fix it then, because that is the cheapest
this fix will ever be and nobody is coming back for it.

This does not conflict with surgical changes, because the thing that rule is
actually protecting is the **diff**, not the defect. Give the gardening its own
commit (guideline 13), so every changed line still traces to one intent and the
unrelated fix can be reviewed, bisected or reverted on its own. Fold it into the
feature commit and you have the problem the rule warns about; land it separately
and you have a tidier repo and a reviewable history. What stays out of scope is
work you cannot finish or verify to the same standard as the change you came
for — note that in `TODO.md` instead.

### Repo-specific guidelines

1.  **Idiomatic Rust**: Always strive for idiomatic and safe Rust. Use `anyhow::Context` to provide meaningful error messages.
2.  **Async/Await**: The codebase is heavily asynchronous. Ensure new I/O or Docker-related code uses `await` and integrates with the `tokio` runtime.
3.  **Dependency Management**: Keep each `Cargo.toml` clean and dependencies updated — and in the right crate (CLI-only deps in `ratect`'s `Cargo.toml`, everything else in `ratect-core`'s). If a library becomes deprecated or unmaintained, propose a migration to a better alternative.
4.  **Configuration Consistency**: When extending the `batect.yml` parser in `ratect-core/src/config.rs`, try to maintain compatibility with the original Batect configuration format.
5.  **State Management**: In `ratect-core/src/engine.rs`, state (like executed tasks) is shared using `Mutex` to ensure thread safety across async tasks. Be mindful of locking logic.
6.  **Verification**: After making changes, verify them by:
    -   Running `cargo build --workspace` to ensure compilation.
    -   Executing `cargo run -p ratect-compat -- -f ratect-compat/tests/fixtures/smoke.yml --list-tasks` to check config parsing, which should print:

        <!-- verify: cargo run -q -p ratect-compat -- -f ratect-compat/tests/fixtures/smoke.yml --list-tasks -->
        ```
        Tasks in ratect-test:
        - list-volume-task
        - prereq-task
        - prerequisites-only-task
        - shared-prereq
        - test-task
        ```

    -   Running a sample task (e.g., `cargo run -p ratect-compat -- -f ratect-compat/tests/fixtures/smoke.yml test-task`) to verify the execution engine and Docker integration. (The repository root's `batect.yml` is Ratect's *own* dev-task config — we build Ratect with Ratect, dogfooding the tool: `cargo run -p ratect-compat -- build`/`test`/`lint`/`fmt` run each in a pinned Rust container with the Cargo registry and build output as `cache` volumes. A root `ratect.toml` mirrors it in the native format, so the same tasks also run through the `ratect` binary (`cargo run -p ratect -- run build`), dogfooding *both* binaries and their two config formats; `ratect-core`'s `the_two_root_dev_configs_agree` test resolves both files and fails if they drift, so an edit to one must be mirrored in the other. That's precisely what the root path *should* hold — this project's own dev tasks — which is why test fixtures deliberately live under `tests/fixtures/` instead, never at the root, so the two are never confused.)
    -   **Touching a doc comment: `cargo doc` is not the gate.** rustdoc
        renders `` [`X`] `` as literal brackets and exits 0 when `X` isn't
        public from there, so `-D warnings` reports nothing — which is why CI
        greps the *rendered HTML* for the symptom instead (`.github/workflows/ci.yml`'s
        "No doc link renders as literal brackets" step, whose own comment
        explains why no lint covers it). Run that step's script, not just
        `cargo doc`, after editing doc comments: a link to a `pub(crate)`
        item otherwise ships as punctuation on a published page. Found by
        shipping ten of them in one sweep, green locally, red in CI.

7.  **Changelog Maintenance**: After completing a task that changes the project's features, dependencies, or structure, ensure that `CHANGELOG.md` is updated in the "Unreleased" section, following the "Keep a Changelog" standard — tersely: state *what* changed, from an upgrading user's perspective, not the rationale, prior state, or implementation detail behind it (that belongs in the change's own GitHub issue, or `RELEASES.md`'s short entry — see guideline 8). Don't repeat a pointer to either on every bullet; a reader who wants that context already knows where to look. A change that breaks existing behavior goes in its own `### Breaking` subsection, listed first under the heading it lands in, so it can be found without reading anything else.
8.  **Version Lifecycle**: A release's scope is tracked as GitHub Issues under one Milestone, named to match its eventual `CHANGELOG.md` heading (e.g. `ratect-compat 0.27.0 · ratect 0.6.0`) — see [decisions/0008](decisions/0008-tracking-release-scope-with-github-issues.md). When cutting a release, it's not just a version bump: every issue in the Milestone closed first, then the `X.Y.Z-dev` → `X.Y.Z` bump commit, moving `CHANGELOG.md`'s `Unreleased` entries under that version's own dated heading (the pipeline's release notes can't be extracted before this lands — see below), and tagging it `<binary>/vX.Y.Z` (prefixed since `ratect` and `ratect-compat` are on independent version lines that would otherwise collide — bare `vX.Y.Z` tags are pre-split history; full rationale in [decisions/0001](decisions/0001-two-binaries.md#consequences)). **Pushing that tag is what publishes the release** — see [decisions/0010](decisions/0010-release-binary-distribution.md) for the `cargo-dist`-based pipeline (`.github/workflows/release.yml`) this triggers: it builds every target's binaries, generates a CycloneDX SBOM, attests every binary/SBOM/checksums-manifest, and creates the GitHub Release itself (body = that release's `CHANGELOG.md` section, via `tools/changelog-section.py`) — no separate hand-creation step. There's one shared `CHANGELOG.md`, whose release headings name every version in that release (`## [ratect-compat 0.21.1 · ratect 0.2.0]`) and whose entries name a binary only when they don't apply to both — see [decisions/0001](decisions/0001-two-binaries.md#consequences) for why it isn't split per binary. Starting the next version's development is a separate, later commit that bumps every crate back to a `X.Y.Z-dev`. Neither bump is ever folded into a feature commit.
9.  **ROADMAP.md / RELEASES.md Maintenance**: the two files follow different edit rules, which is why they *are* two files. `ROADMAP.md` is forward-looking and freely rewritten as scope changes or ships — not just merging or deleting a bullet, but collapsing a whole section once it's actually done: [Batect Parity](ROADMAP.md#batect-parity) went from an itemized, version-numbered feature list to a two-paragraph pointer at [`docs/differences-from-batect.md`](docs/differences-from-batect.md) (the living itemized tracker) and the conformance corpus, once feature parity itself was reached and the per-field detail had a better home. The lesson generalizes: a section here describing something that's *shipped* is a bug, not documentation — link to where that's actually tracked (`docs/`, `decisions/`, a code doc comment) instead of restating it, the same way [Differences from Batect](docs/differences-from-batect.md) itself only lists real divergences now rather than a field-by-field "Supported" table. `ROADMAP.md` keeps each retired section's heading as a one-line stub, gathered under its own Retired headings section at the end of the file, so links written before that section shipped or moved — including ones in already-released `CHANGELOG.md`/`RELEASES.md` entries, which are append-only — still resolve.

    `RELEASES.md` itself is append-only history — never delete an entry — but since [decisions/0008](decisions/0008-tracking-release-scope-with-github-issues.md) a new entry is a short, one-paragraph retrospective pointer (the release's theme, and a link to its closed GitHub Milestone, which lists every issue that shipped), written once after the release ships. It is no longer a live document edited across the release (scope written before building, then struck through and summarised after) — that live tracking is now the Milestone's own open/closed issue state. Entries from before this change keep their old `~~strikethrough~~`-plus-done-summary form, untouched; don't rewrite them to match the new convention, and don't add scope to them going forward. The append-only guarantee protects a version once it *ships* (tagged, bumped off `-dev`) — not while it's still being written, so an in-progress `-dev` entry already under the old convention (see `decisions/0008`'s own Status for the current example) is expected to be rewritten once, wholesale, into the new short form when that release finally ships. (For the record, since these entries still exist in the file: a `~~strikethrough~~` is inline markdown and cannot cross a blank line — a multi-paragraph entry needed the `~~` closed at the end of each paragraph and reopened at the start of the next, or the markers rendered literally. GitHub's own `/markdown` API answers whether one renders correctly, in one call.)
10. **User Docs Maintenance**: When a change affects user-visible behavior (CLI flags, config schema, runtime behavior, Batect parity), update the relevant file(s) under `docs/` in the same change — don't let them drift from the code. If you find the code doesn't match what's documented, fix whichever one is wrong rather than leaving the mismatch.
11. **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, non-fatal error conditions like a best-effort cleanup failure) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout. One deliberate exception: `main.rs`'s single top-level fatal error (the reason the process is about to exit non-zero) is `eprintln!`ed directly, *not* through `tracing::error!` — it must stay visible even under `RUST_LOG=off`, since every output mode (including `-o quiet`, whose whole contract is "only error messages") otherwise has nowhere else to show it. Found and fixed during 0.16.0's output-modes review — don't revert it back to `tracing::error!`.
12. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff. Every commit is signed off (`git commit -s`) — the [DCO](https://developercertificate.org) attestation CONTRIBUTING.md describes and CI enforces on pull requests.

    **`main` requires a pull request** — a repository ruleset (`main branch protection`) blocks deletion, force-pushes, and every direct push, with no bypass for anyone including repo admins, and requires the full CI suite (including the DCO sign-off check) to pass before merge. The PR/CI-required rules landed alongside the scheduled Renovate workflow: once dependency-bump PRs are a routine, automated part of this repo's life, everything else goes through the same gate for consistency, not just for Renovate — work on a branch, open a PR, let CI run, then merge (no minimum reviewer count is required, so a solo change can merge as soon as its own checks pass). Earlier commits in this repo's history were made directly to `main`; that convention is retired going forward, not retroactively rewritten.

    **Squash merging is disabled**, both in the ruleset and at the repo level — it flattens a branch's own commit history into one, discarding whatever story separate commits told (see guideline 13's own reasoning for keeping genuinely separable behaviors as separate commits in the first place). Use a merge commit or rebase merge instead; squashing individual commits *on a branch* before opening a PR, to tell a cleaner story, is still fine and encouraged — the distinction is who does the squashing and when, not whether a tidy history matters.
13. **Commit Packaging**: a release that's one theme (like most 0.x releases so far) lands as a single `feat:` commit. A release bundling several genuinely separable behaviors (e.g. 0.6.0's networking + proxy work) should instead split into one `feat:` commit per behavior, each with its own tests and doc updates — easier to review and to `git bisect`/`git revert` than one large commit. The version bump and any docs-only release summary stay separate commits either way (see 8).

    **Atomic within a PR.** A PR is usually one change, so a commit inside it
    that *corrects* another commit inside it is not history — it is a draft
    left in the record. Fold it. The test: does understanding one change
    require reading a later commit that changes the story? A `feat:` describing
    a contract three later commits revise fails it; so does a doc commit
    asserting an invariant the next two correct. Across PRs and over time,
    corrections to earlier work are expected and stay separate — that is
    ordinary history, not a draft. What stays separate *within* a PR is
    genuinely discrete work: ratect#98 carried three unrelated pre-existing
    bugs found on the way, each its own commit, each independently revertable.
    A commit's `CHANGELOG.md` entry travels with it and has to stand alone — a
    bullet saying "same reason" breaks the moment the bullet it referred to
    lands in a different commit.

    **Every commit must build and pass its tests**, which is what `bisect`
    actually needs and what "easier to bisect" above silently assumes. The
    trap, met while restructuring ratect#98: folding a review fix into the
    feature pulled in a test calling a function a *later* commit introduced,
    so that commit compiled nowhere. Check it rather than assume it — a loop
    of `git checkout <sha> && cargo check --workspace --all-targets` over
    `git rev-list main..HEAD` is a minute's work, and it is how that one was
    caught before it shipped.

    **Restructuring is verifiable, so verify it.** Branch the old head first;
    the rewritten branch's final tree should then equal the pre-rewrite one
    (`git diff <backup> HEAD` empty), or differ only by changes you can
    enumerate and justify. `git rebase -i` drives fine non-interactively with
    a scripted `GIT_SEQUENCE_EDITOR` — a two-line script that `cat`s a
    prepared todo into `$1` — so reordering and folding need no terminal, and
    "interactive rebase isn't available" is not a reason to leave a history
    unsplit.

    **A scripted `reword` replaces the whole message, sign-off included.**
    `git commit -s` wrote that trailer; a prepared message file won't have it
    unless you put it there, and the result compiles, tests clean and fails
    the DCO check instead. Check trailers after any rewrite — `git log
    --format='%h %(trailers:only)' origin/main..HEAD` — rather than finding
    out from a required status.
14. **Architecture Decision Records** ([`decisions/`](decisions/)): the home for a decision's rationale is decided by whether it's **cross-cutting or version-scoped**. A decision referenced from more than one place — the two-binary split, the labels namespace, the native config format — becomes an ADR — a new `decisions/NNNN-slug.md`<!-- example --> file (`NNNN` the next number in sequence) with `Status`, `Context`, `Decision`, `Alternatives considered`, and `Consequences` sections; `RELEASES.md`'s own entry for that release then just links to it, same as it links to the release's Milestone. A decision that belongs to one release stays **inline** in that release's own GitHub issue (its "Implementation Decisions" section — see [decisions/0008](decisions/0008-tracking-release-scope-with-github-issues.md)) — don't extract it. Practical trigger: a decision earns an ADR the moment it's about to be referenced from a *second* place; most never cross that line. ADRs are append-only like the versioned lists — supersede and link forward, never delete. See [`decisions/README.md`](decisions/README.md) for the full convention.
15. **Review before committing, not after.** Run a review pass over the working
    diff (`/code-review`) *before* each commit, not over a run of commits
    afterwards. The checks below have each caught something a review missed:
    - **Anchor an inserted item on the preceding item's closing brace, never on
      the new one's attributes.** A Rust item's doc comment sits *above* its
      `#[test]`/`#[derive]` attributes, so anchoring there splices the new item
      into the previous one's documentation — silently, and the compiler is
      happy. The `claims` plugin's `spliced-docs` check finds the ones that
      get through; it's advisory (never blocks a commit), so read each
      candidate rather than treating a report as a defect.
    - **Re-read every string you added, in its final control-flow position.**
      Nothing type-checks a log or error message, so they stay as written while
      the code around them moves. For errors specifically: name something the
      user actually wrote. A lower layer speaks its own vocabulary — `docker.rs`
      knows an ssh agent id, not which container declared it — so an error
      crossing up from one needs the caller to attach that.
    - **When a change alters observable behaviour, re-read that behaviour's
      whole doc section and *run* each claim against the binary.** Not grep —
      execute. Every claim: the example output, the flag descriptions, the "this
      does X" sentences. Capture real output and `diff` it rather than editing
      what looks wrong, since what looks wrong is exactly the set you already
      believe. Grep is the fallback for claims nothing can be run against (a
      roadmap entry, a design note); it is not the check. An earlier version of
      this rule *was* grep — for strings naming the old scope — and it reported
      clean while missing three, so the sweep was recorded as done.

      To find which claims to re-read, the `claims` plugin's `stale-claims`
      check ranks prose by how much the code it names has moved since the
      claim was last touched. Treat a hit as "re-read this", never as "this
      is wrong" — it measures churn, so a true claim about a hot file looks
      suspicious. Don't record its candidate count anywhere: the number rises
      when code moves and falls when a doc is fixed, so it measures neither.
    - **Verify a claim before writing it, not after a reviewer questions it** —
      the review-time half of the change loop's write-prose step.
    - **A claim copied from the handoff is unverified.** The handoff is a
      *specification* — what to build, and why it is worth building. It is not
      a source for what exists. So every name in it that you carry into a doc
      comment, an error, or a page under `docs/` gets run first: a flag against
      `--help`, a mode against the code that would implement it, a path against
      the filesystem. This is the change loop's "the code includes whatever the
      prose *names*", applied to the one input that most looks like it has
      already been checked.

      Twice in one release, 0.26.0's scope was transcribed verbatim and was
      wrong both times: `ratect-compat --cleanup` is a flag that has never
      existed (and the nearest-looking one, `--clean`, destroys caches), and
      "run as an MCP server" names a mode neither binary has. Both were true of
      the author's *intent* and false of the binary, which is exactly the gap a
      specification cannot see. Correct the handoff where it is still live;
      where it is already struck through, record the correction in its
      done-summary rather than editing history.
    - **A behaviour that depends on which format/mode you're in needs one
      derived value, not a guard per call site.** Derive it once
      (`include_trust::restricting`, returning `Option<&Bundle>`) and have every
      site consume it, so the divergence is unrepresentable. Guarding
      site-by-site means the next site added is unguarded by default.
    - **Never spell config syntax in an error message — name the field.** `set
      'x' to true`, not `add 'x: true'` or `'x = true'`. Unconditional: the
      earlier form allowed the syntax where the message fires in one format
      only, and that was refuted — `ConfigFormat::Native` is the **project's**
      format, not the **file's**, so a native project can locally include a
      `.yml` and the entry the message names may be YAML.
    - **Watch for coverage shaped by the test harness rather than the
      behaviour.** If the fake can only express one ordering of something
      inherently timing-dependent, the untestable orderings are where the bug
      will be — extend the harness instead of concluding the cases are covered.
    - **Run a self-cleaning test twice, asserting external state (`docker volume
      ls`) rather than the exit code.** One run passes whether or not the
      cleanup matched anything — the first run starts clean by definition.
    - **Two tests sharing a fixture directory need a lock, not luck.** `cargo
      test` runs a binary's tests on several threads, and contention over a
      shared fixture surfaces as a behaviour failure ("the cache did not
      persist"), not as a race. A `static Mutex` around the project is the cheap
      fix — see `CACHE_MOUNT_PROJECT`.
    - **A real-daemon test can mask a missing unit test.** The `#[ignore]`d
      Docker tests don't run in the default suite, so a path they cover can be
      unprotected in `cargo test --workspace`. Assert each effect separately —
      one assertion per thing removed, not one for "cleanup happened".

16. **Fix the class, not the instance — and say what you swept.** A review
    finding is a *sample*. Before fixing it, ask what else in the codebase has
    that shape and go and look; then report the sweep, including when it comes
    back clean — a negative result is information, and it saves the next
    reviewer re-deriving it. A review that only surfaces siblings of something
    already fixed is **failure demand**: work created by not having finished the
    job the first time.

    For prose specifically, the `claims` plugin's `restatement` check runs
    automatically before each commit (or on demand via `check-claims`)
    rather than relying on sweeping from memory: it lists what some other
    file still says verbatim after you corrected it here — staged or not. It
    is one pass, not the sweep: it matches the words you deleted, so it finds
    quotation and misses paraphrase, and a summary paraphrases what it
    summarises. A clean run buys you the verbatim case and nothing more.

    Two things to hold on to. A range changes what is *diffed*, never what is
    *searched*, which is always the current checkout — so pointing it at
    `main..HEAD` answers "did that correction leave anything still asserted
    today". And **read the phrase it prints, not the line number**: on the
    one case this was measured against it reports the right file and line
    while matching a different, perfectly true phrase on it. A hit at the
    correct location for the wrong reason looks exactly like a catch.

    - **Prefer the structural fix when the local one leaves the invariant
      unstated.** When one area keeps producing findings, the design is the
      finding.
    - **A repeated process error is a defect in the process, not in the
      attempt.** Resolving to be more careful is not a fix: change the method,
      then write it down here. Standing method — **stage explicit paths, never
      `-A`, whenever more than one commit is planned, and rebuild a mis-split
      pair with `reset --soft` rather than `--amend` or an interactive rebase.**
    - **Not every finding is a class, and saying so is part of the job.**

## Agent skills

Contributor/agent-process reference docs (not user-facing, so not under
`docs/` — see [decisions/0007](decisions/0007-where-agent-process-docs-live.md))
live in [`agents/`](agents/).

### Issue tracker

GitHub Issues (via `gh`), on or1can/ratect. See `agents/issue-tracker.md`.

### Triage labels

Default canonical label strings, unchanged. See `agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the repo root; ADRs in `decisions/` (this
repo's own established location, not `docs/adr/`). See `agents/domain.md`.

### User docs

The `docs/` directory is the user-facing documentation tree, as distinct
from `ROADMAP.md`, `RELEASES.md`, `CHANGELOG.md`, `decisions/` and this
file, which are for contributors. How to write a page there — the two
config references' deferral rule, the three kinds of example, the
captured-output rules, the ownership guidelines — is `agents/docs.md`.
