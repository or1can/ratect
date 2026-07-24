# Ratect Roadmap

This document outlines the planned journey for Ratect, from achieving parity with Batect to implementing Rust-specific enhancements and future innovations.

## Batect Parity

The primary goal is to support the core features of Batect to ensure a seamless transition for existing users. This work targets the [`ratect-compat` binary](#two-binaries-ratect-and-ratect-compat) specifically — the `ratect` binary is not expected to maintain 1:1 Batect parity.

- **Image Building**: Building a Docker image from a `build_directory`, including `build_args` and `.dockerignore` support (0.3.0), custom Dockerfile naming/location (`dockerfile`), a multi-stage build target (`build_target`), secrets (`build_secrets`), and SSH agent forwarding (`build_ssh`, a single default host agent only — see [Differences from Batect](docs/differences-from-batect.md#container-fields)) (0.11.0), building with the builder the daemon advertises as its default — BuildKit on any modern daemon, matching Batect, with `DOCKER_BUILDKIT` honored as the force-on/off override (0.12.0) — see [config reference](docs/config-reference.md#image-building). Cross-invocation build caching and automatic image cleanup are not implemented.
- **Full Docker Networking**: Every task execution gets its own isolated network (see [the task lifecycle](docs/task-lifecycle.md)), `--use-network` reuses an existing one instead, `additional_hostnames`/`additional_hosts` add extra aliases/`/etc/hosts` entries, and `ports`/`--disable-ports` publish container ports to the host, including port ranges and the expanded object form, plus additional per-task `run.ports` (0.6.0) — see [config reference](docs/config-reference.md#port-mappings) and [CLI reference](docs/cli-reference.md).
- **Interactive Mode**: A task's own container gets a real Docker TTY, automatically, when both Ratect's own stdin and stdout are real terminals (0.4.0); its stdin forwarding and the host's `TERM` propagation both apply more broadly than that (whenever the task is interactive-eligible, not gated on a real TTY), and a real TTY's terminal size stays in sync for the whole session, not just once at attach (0.10.0) — see [Interactive mode](docs/config-reference.md#interactive-mode). One known, deliberate divergence from Batect remains — see [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps).
- **Full Environment Variable Interpolation & Batect Expressions**: `environment` on containers/tasks, `config_variables` (including Batect's one built-in, `batect.project_directory`), and `$VAR`/`${VAR}`/`${VAR:-default}`/`<name`/`<{name}` expressions are implemented for `environment` values, volume host paths, `build_directory`, `build_args`, and a `build_secrets` entry's `path` (0.11.0) — every already-supported field that could meaningfully take one; `build_ssh.paths` stays moot while explicit key files are unsupported — see [Expressions](docs/differences-from-batect.md#expressions).
- **Dependency Readiness**: A started dependency isn't treated as ready until it
  reports healthy (its image's own Docker health check, or the `health_check`
  override) and completes its `setup_commands` — only then do its dependents start
  (0.9.0) — see [config reference](docs/config-reference.md#dependency-readiness).
  The task's *own* container goes through this same gate too, concurrently with its
  main command rather than gating anything on it (0.21.0) — see [task
  lifecycle](docs/task-lifecycle.md#known-simplifications-relative-to-batect) for the
  one residual race this still shares with Batect, and [Differences from
  Batect](docs/differences-from-batect.md#container-fields).
- **Includes**: Local file includes — splitting one project's configuration across multiple files via the top-level `include` directive, resolved relative to each declaring file's own directory and merged into one flat `containers`/`tasks`/`config_variables` set (0.7.0) — and Git includes/bundles — importing shared tasks/containers from a separate repository, cloned once and cached forever at `~/.ratect/incl` (0.8.0), with a 30-day automatic cache eviction sweep matching Batect's own (0.19.0) — see [config reference](docs/config-reference.md#includes) and [Differences from Batect](docs/differences-from-batect.md#top-level-fields).
- **Full Configuration Parity**: Support for all available Batect configuration options and standard YAML structures. See [Differences from Batect](docs/differences-from-batect.md#configuration-format) for the itemized current status of every field.
- **Volume Mounts**: `volumes` supports all three of Batect's mount kinds — `local` (`local:container[:options]`), `cache` (a named volume that persists between separate `ratect` invocations — a Docker named volume by default, or a host directory under `--cache-type=directory`, plus `--clean`/`--clean-cache` to clear them out, [0.18.0](#ratect-compat)) — see [Cache volumes](docs/config-reference.md#cache-volumes) — and `tmpfs` (an in-memory, ephemeral mount, lost when the container exits, [0.21.0](#ratect-compat)) — see [Tmpfs mounts](docs/config-reference.md#tmpfs-mounts).
- **Config Schema**: A JSON schema describing Ratect's actual accepted `batect.yml` shape, for editor autocompletion/validation — generated from `ratect-core/src/config.rs`'s own types via `schemars` rather than hand-maintained separately, and committed at [`schema/batect-config.schema.json`](schema/batect-config.schema.json) ([0.21.0](#ratect-compat)) — see [Editor autocompletion and validation](docs/config-reference.md#editor-autocompletion-and-validation). Deliberately **not** Batect's own published schema (listed in [SchemaStore's catalog](https://www.schemastore.org/api/json/catalog.json) for `batect.yml`/`batect-bundle.yml`, hosted at `ide-integration.batect.dev`) — that reflects Batect's full field set, not Ratect's subset, so it would either validate fields Ratect doesn't actually support (a false pass in the editor) or reject a future Ratect-only extension as invalid (a false failure). Not submitted to SchemaStore itself — that's a separate, later decision.
- **Full CLI Options Parity**: Support for all standard Batect CLI flags and options (e.g., `--config-file`, `--override-image`, cleanup control flags, etc.). See [Differences from Batect](docs/differences-from-batect.md#cli-flags) for the itemized current status of every flag.
- **User Mapping**: A container can run as the host's own user/group (`run_as_current_user`) instead of the image's default, so files it writes to a mounted volume aren't root-owned (0.5.0) — see [User mapping](docs/config-reference.md#user-mapping). Host-side uid/gid lookup is Unix-only — see [Differences from Batect](docs/differences-from-batect.md#container-fields).
- **Proxy Support**: `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` are detected from the host environment and propagated into containers and image builds automatically, `--no-proxy-vars` to disable (0.6.0) — see [Proxy environment variables](docs/config-reference.md#proxy-environment-variables). `localhost` rewriting only works on macOS/Windows, and there's no Docker-version-gated hostname fallback chain — see [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps).

## Two Binaries: `ratect` and `ratect-compat`

Rather than one binary evolving through phases and eventually deprecating Batect
compatibility, the plan is a Cargo workspace with a shared core library
(config parsing, task engine, `ContainerRuntime`/Docker integration) and two thin
binary crates built on top of it:

- **`ratect-compat`**: A strict, literal, flag-for-flag and field-for-field match for
  Batect's CLI and `batect.yml` format. This is where all of the [Batect Parity](#batect-parity)
  work lands, scoped precisely by the itemized tables in
  [Differences from Batect](docs/differences-from-batect.md). Its only job is being a
  boring, reliable drop-in replacement for the (now-unmaintained) `batect` binary — it
  is not the place for new ideas.

  Ratect deliberately does **not** ship a binary literally named `batect` (that would be
  confusing, and edges toward a trademark/naming concern). Anyone who wants their
  existing `./batect` wrapper script or `PATH` entry to keep working symlinks or renames
  `ratect-compat` to `batect` themselves.

- **`ratect`**: The forward-looking CLI, free to diverge from Batect's interface —
  subcommands (`ratect tasks list`, `ratect run <task>`), better shell completions, and
  other modern-Rust-CLI conventions — without being constrained by parity concerns. This
  is also the binary that would adopt any future alternative configuration format (see
  [Future Vision](#future-vision)); `ratect-compat` stays YAML-only, permanently, since
  that's what Batect compatibility requires.

Because both binaries share the same core, an eventual migration/upgrade path from a
`ratect-compat`-managed project (Batect-format config) to a `ratect`-managed one is a
roadmap goal in its own right, not just a side effect of the split.

## Versioning & Releases

`ratect-compat` and `ratect` are versioned **independently** — they're on different
maturity clocks, and forcing one number to serve both meanings breaks the moment they
diverge (which they will, since `ratect-compat` has a head start). What *is* shared is
the release **process**: a fix in the shared core crate gets released for both binaries
at the same time (one PR/tag/CI run), each bumping its own patch version independently
— not the same version number, just released together, so nobody is left running a
stale, unpatched core. The core crate itself isn't published or meaningfully versioned
on its own; it's an internal implementation detail, not something either binary's users
interact with directly.

Mechanically, **every** `Cargo.toml` in the workspace sits at `X.Y.Z-dev` between
releases — both binaries and `ratect-core`, whichever binary a given cycle is
actually about, so a build from `main` never claims to be a released version.
Cutting a release is one isolated `chore:` commit that bumps the crates being
released to the plain `X.Y.Z` and moves `CHANGELOG.md`'s accumulated `Unreleased`
entries under a new dated heading naming every version in that release — e.g.
`## [ratect-compat 0.21.1 · ratect 0.2.0]`, or just the one binary when it's
released on its own. That commit is tagged and published as a GitHub Release
(`prerelease: true` until a binary's own 1.0.0 — see below — with that
`CHANGELOG.md` section as its body; a joint release uses that same section for
both, which is correct, because it *is* the same set of changes). The next
commit — starting the following version's development, also isolated, also
`chore:` — bumps them back to the next `X.Y.Z-dev`. Neither bump is ever folded
into a feature commit.

Three mechanics that only became concrete once `ratect` started its own release
cycle (0.2.0, the first one not about `ratect-compat`):

- **Tags are prefixed with the binary they release** — `ratect/v0.2.0`,
  `ratect-compat/v0.21.1` — because the two version lines will collide otherwise:
  `v0.2.0` is already taken, by `ratect-compat`'s own 0.2.0 back when it was the
  only binary. Bare `vX.Y.Z` tags (`v0.1.0` through `v0.21.0`) are that history and
  stay exactly as they are; nothing renames them. Everything from here on is
  prefixed, `ratect-compat` included, rather than leaving one binary on a legacy
  scheme.
- **One shared `CHANGELOG.md`, not one per binary.** Most substantive work is in
  `ratect-core` and so reaches both binaries — the anonymous-volume fix
  ([0.21.1](#ratect-compat)) is the pattern, not the exception — so two files
  would be largely the same prose under different headings, drifting apart on
  every core change. (That's the opposite of the CLI reference docs, which *are*
  split per binary: those overlap by almost nothing, since they document
  different flags. Split where the content differs, share where it doesn't —
  which is also why `config-reference.md` and `task-lifecycle.md` are shared.)
  An entry with no binary named applies to both; one that doesn't says
  `(ratect only)`/`(ratect-compat only)`, so the annotation cost falls on the
  rarer case. Revisit only if `ratect` diverges far enough that shared-core
  changes stop being the bulk of the work — 0.3.0's own config format is a step
  that way — since cutting one file in two later is easy, and merging two back
  into one isn't.
- **A cycle bumps the crates it actually changes.** A `ratect`-only cycle still
  moves `ratect-core` (it's the same shared crate, and its number has always run
  with the release cadence rather than standing still) and still leaves
  `ratect-compat` on a `-dev` of its own — a patch bump if nothing but the shared
  core moved underneath it, a minor one if it gained anything itself. Which of the
  two it turns out to be is decided at release time; the `-dev` number in between
  is a statement of intent, not a commitment.

### `ratect-compat`

- **0.1.0** — not a features milestone, an *honesty* milestone: the engine (prerequisites,
  cycle detection, dedup, sidecars) is already solid, but a few known gaps in
  [Differences from Batect](docs/differences-from-batect.md) currently make the tool's
  output untrustworthy rather than just incomplete, and should be fixed before anything
  is tagged:
  - ~~Container exit codes aren't checked~~ — fixed: a task whose command exits
    non-zero now fails `ratect` itself with that exact exit code, and stops the rest
    of a prerequisite chain, matching Batect.
  - ~~A missing config file exits `0` instead of failing~~ — fixed: it now fails fast
    with a non-zero exit, for both `--list-tasks` and running a task.
  - ~~`-- ADDITIONAL_ARGS` is parsed but silently dropped~~ — fixed: forwarded as
    `sh -c`'s positional parameters (`$1`, `$2`, `$@`), scoped to only the
    explicitly-requested task, never its prerequisites.
  - ~~Unsupported config keys are silently ignored rather than rejected~~ — fixed:
    every config struct now denies unknown fields, so a config using a field Ratect
    doesn't yet support fails to load with an error naming the field, instead of
    silently loading with that field ignored.
- **0.2.0** — ~~**Environment Variables** (the `environment` field on containers/tasks)
  together with **Batect Expressions**/config variables (`$VAR`, `${VAR:-default}`,
  config variables via `<name`)~~ — done: `environment` on both containers and task
  `run`s (merged, with `run.environment` winning on a key collision), a
  `config_variables` top-level field (`default:` only), `$VAR`/`${VAR}`/`${VAR:-default}`
  and `<name`/`<{name}` expressions resolved within `environment` values and volume
  host paths, `--config-var`/`--config-vars-file` CLI flags to supply config variable
  values, and Batect's one built-in config variable, `batect.project_directory`. See
  [config reference](docs/config-reference.md#expressions). Bundled deliberately, not a
  grab-bag: interpolation is the one shared mechanism both environment variables and
  config variables need to be useful, and later fields like `build_args` (0.3.0) depend
  on it too.
- **0.3.0** — ~~**Image Building** (`build_directory` currently just warns and no-ops),
  including `build_args` interpolation from 0.2.0~~ — done: `build_directory` builds a
  real image (Dockerfile always named `Dockerfile`, at its own root), `build_args`
  interpolated and passed through as `docker build`'s own `--build-arg` mechanism,
  `.dockerignore` support (a from-scratch port of Docker's actual matching rules — not
  `.gitignore`-compatible — new `dockerignore/` workspace crate, see
  [config reference](docs/config-reference.md#dockerignore-semantics)), and dependency
  containers gained `build_directory` support too (previously image-only). Known gaps,
  candidates for later work rather than blocking this release:
  - No cross-invocation build caching — each `ratect` run rebuilds fresh, tagged
    `<project_name>-<container_name>` (matching Batect's own convention, so the image
    is identifiable in `docker images`), but that tag is reused/overwritten on every
    run rather than cached against. Running containers doesn't depend on the tag
    staying put, though — `resolve_image` uses the image *ID* Docker reports back from
    the build, not the tag, specifically so two overlapping `ratect` invocations
    retagging the same name can't race each other into running the wrong image (see
    the `resolve_image` design comment in `ratect-core/src/engine.rs`). A future
    cache-aware scheme would need to reuse a previous build's output safely (staleness
    detection), which is separate from the naming/identification problem solved here.
  - Built images aren't cleaned up automatically — since each run retags
    `<project_name>-<container_name>` to point at its fresh build, the image it
    replaces becomes a dangling (`<none>`) image rather than disappearing, and
    accumulates until manually pruned (`docker image prune`), same as repeatedly
    running a plain `docker build -t ... .` would leave behind.
  - ~~Build output isn't captured or persisted anywhere~~ — fixed: every streamed
    build log line is now logged at `debug` level (`RUST_LOG=debug` for a live
    transcript), and — more importantly — a build failure's error now includes the
    *entire* accumulated transcript, not just Docker's one-line `error_detail.message`,
    via a new `build_output_suffix` helper (`ratect-core/src/docker.rs`). Deliberately
    not a `--output` mode (Ratect has none yet — see below) — piggybacking on the
    existing `tracing`-based logging/error-reporting Ratect already has was the honest
    "for now" answer, not a new UI concept.
  - The `dockerignore` crate has zero dependency on any ratect-specific type and was
    deliberately kept as its own workspace crate (not a `ratect-core` module)
    specifically so it *could* be extracted and published as a standalone crate later —
    no existing one implements Docker's actual `.dockerignore` semantics faithfully.
    Not committed to yet (no public API stability promise, no external docs, not on
    crates.io) — a candidate for later, not a plan.
- **0.4.0** — ~~**Interactive Mode** (TTY/STDIN attachment for tasks that need user
  input)~~ — done: a task's own container gets a real Docker TTY and its stdin
  forwarded whenever the invoked task's own container is running and Ratect's own
  stdin/stdout are both real terminals — fully automatic, matching Batect, no config
  field or CLI flag. Never applies to a prerequisite's, dependency's, or sidecar's
  container. Known gaps, candidates for later work rather than blocking this release:
  - No live terminal-resize forwarding — the container's TTY size is synced to the
    local terminal's once, at attach time, not tracked for the rest of the session.
  - Stdin forwarding isn't decoupled from TTY allocation the way Batect's is (Batect
    can pipe input into a task without allocating a TTY). Ratect gates both together —
    no support yet for piping input into a task that isn't otherwise interactive.
  - Windows terminal handling (raw mode, resize) is implemented via `crossterm`
    (cross-platform) but hasn't been verified there — Ratect's own testing has been
    Unix-only so far, consistent with [First-class Cross-platform
    Support](#rust-enhancements) not having started yet.
- **0.5.0** — ~~**User Mapping** (`run_as_current_user`)~~ — done: a container runs
  as the host's own user/group, matching Batect's already-shipped mechanism (not
  just `--user`): host-side volume directories are pre-created (as the current
  user, before the container exists, so Docker's daemon doesn't auto-create them as
  `root:root`), the container's `User` is set to the mapped `uid:gid`, and — since
  an arbitrary host uid/gid has no entry in the image's own passwd/group — minimal
  synthetic `/etc/passwd`/`/etc/shadow`/`/etc/group` and the declared home
  directory are uploaded into it before it starts. Applies per-container (a task's
  own container and each dependency set it independently), matching Batect. Known
  gaps, candidates for later work rather than blocking this release:
  - No equivalent to Batect's "cache mounts" — Ratect has no such config concept at
    all, so the corresponding provisioning step doesn't apply here.
  - Host-side `uid`/`gid` lookup (`ratect-core/src/user.rs`, via the `nix` crate)
    is Unix-only — errors clearly on other platforms rather than guessing, same
    caveat as 0.4.0's `crossterm` usage. Windows containers were never in scope for
    Ratect regardless.
- **0.6.0** — ~~**Full Docker Networking** and **Proxy Support**~~ — done: `--use-network`
  reuses an existing Docker network instead of a fresh one per task;
  `additional_hostnames`/`additional_hosts` add extra network aliases/`/etc/hosts`
  entries; `ports`/`--disable-ports` publish container ports to the host — both of
  Batect's forms (`"local:container[/protocol]"` strings, including ranges, and the
  expanded `{local, container, protocol}` object form), validated at config-load time,
  plus a task run's own additional `ports`, combined with the container's as a union;
  every container's Docker hostname is now set to its own container name, matching
  Batect, rather than left as a random container ID; and
  `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` are detected from the host
  environment and propagated into every container and build automatically
  (`--no-proxy-vars` to disable). Known gaps, candidates for later work rather than
  blocking this release:
  - No custom network driver support for a network Ratect creates itself — the only
    way to get a different driver is to pre-create the network yourself and point
    `--use-network` at it, same as Batect.
  - The proxy `localhost`/`127.0.0.1`/`::1` rewrite (to `host.docker.internal`) only
    works on macOS/Windows — no automatic Docker-reachable hostname on Linux, and no
    Docker-version-gated hostname fallback chain the way Batect has for very old
    Docker installs (not worth chasing for any actively-maintained daemon today).
- **0.7.0** — ~~**Includes**: local file includes, splitting one project's
  configuration across multiple files~~ — done: a config file's top-level `include`
  list (bare string path or expanded `{path, type: file}` form) is resolved relative to
  the directory of the file that declares it, recursively, de-duplicated by resolved
  path (an include cycle or a file included from two places is harmless); every loaded
  file's `containers`/`tasks`/`config_variables` merge into one flat set (a name
  defined in more than one file is a hard error naming both files), only the root file
  may declare `project_name`, and each container's relative paths resolve against its
  own origin file's directory while `<batect.project_directory` still always resolves
  to the root's. Git bundle includes (importing shared tasks/containers from a
  separate repository) remain deferred to a later, undecided release — a materially
  larger feature (remote fetch, caching) that shouldn't block the simpler
  file-splitting case; a `type: git` include entry is rejected with a clear
  "not supported yet" error rather than silently ignored.
- **0.8.0** — ~~**Git Includes**: the `type: git` include entry 0.7.0 rejects ("not
  supported yet") — importing shared tasks/containers from a separate Git repository (a
  "bundle"), the way real-world Batect projects actually rely on this, not just the
  simpler local-file-splitting case~~ — done, design validated against Batect's own
  implementation (`libs/git-client/`, `app/.../config/includes/` in the local `batect`
  checkout):
  - Shells out to the system `git` binary (`clone --quiet --no-checkout` into a temp
    dir, then `checkout --recurse-submodules <ref>`, then an atomic rename into
    place) — no embedded Git library, matching Batect's own approach and keeping this
    dependency-light.
  - A repo/ref is cloned **once and never re-fetched** — the cache key is a SHA-256
    hash of `(remote, ref)`; if that directory already exists, it's reused forever.
    This is *why* users are expected to pin immutable tags, not a corner Ratect is
    cutting relative to Batect. Cache lives at `~/.ratect/incl/<hash>` (Batect:
    `~/.batect/incl/<hash>`).
  - A lock file per cache entry (create-exclusive + poll + timeout, 5 minutes) makes
    concurrent `ratect` invocations targeting the same repo/ref safe — guards the clone
    step only, matching Batect.
  - Each cached repo gets a small TOML sidecar (`<hash>.toml`: `type`, `repo.remote`,
    `repo.ref`, `cloned_with_version`, `last_used`) — TOML rather than matching
    Batect's own JSON, since there's no compatibility requirement (this directory is
    ratect-specific, never read by Batect). `last_used` is a Unix timestamp (seconds)
    rather than filesystem `atime`/`mtime`, since `atime` is unreliable across platforms
    and especially on CI (`relatime`/`noatime` defaults), `mtime` reflects clone time,
    not last-used time, and an explicit field is trivially mockable in tests via an
    injected clock — same reasoning as Batect's own `TimeSource` parameter. Written
    via write-to-temp-then-atomic-rename (same trick already needed for the clone
    destination) so it can never be torn/corrupted under concurrent writers, without
    needing its own lock; a concurrent `last_used` bump can still be lost to a
    last-write-wins race, same as Batect accepts — low-stakes, since it only feeds
    cleanup, not correctness.
  - A git-included file's containers resolve their relative paths against the cloned
    repo's directory — already covered for free by 0.7.0's `container_base_paths`
    mechanism (a clone directory is just another "origin directory"), no new
    resolution logic needed.
  - `Config::load_from_file` is now `async` (`Config::load_from_file_with_git_cache`
    is the underlying generic entry point, parameterized over a new `GitClient` trait —
    mirroring `docker.rs`'s `ContainerRuntime`/`FakeContainerRuntime` split — so tests
    inject a `FakeGitClient` instead of needing a real network or `git` binary; a
    `SystemGitClient`-backed test suite exercises the real `git` binary too, against a
    local repository, needing no network).
  - `repo`/`ref` and every `path` reached through a Git include are treated as
    untrusted (they're config-file-supplied, possibly transitively from a bundle
    outside the caller's own control): a leading `-` on `repo`/`ref` is rejected
    (argv flag smuggling into `git clone`/`git checkout`), `GIT_ALLOW_PROTOCOL` is
    restricted to `file:git:http:https:ssh` on both commands (blocks the `ext::`
    transport's arbitrary-shell-command execution, including via a submodule URL
    reached through `--recurse-submodules`), and a `GitBoundary` (`config.rs`) enforces
    that a Git include's own `path`, and every `include` it transitively declares, stays
    within that repository's own clone directory — both lexically (rejects an absolute
    path or `../..` before ever touching the filesystem) and, once the target is
    confirmed to exist, against the *canonicalized* form of both paths (rejects a
    symlink planted inside the clone that points back out). A nested `type: git`
    include still works — it establishes its own fresh boundary rather than inheriting
    (or being rejected by) its parent's. Found via automated security review of the
    initial implementation, not part of the original design pass.
  - Known gap, deferred as follow-on work rather than blocking this release: no
    30-day cache eviction sweep — Batect runs one automatically, unconditionally,
    on every invocation via a background thread (`GitRepositoryCacheCleanupTask`),
    deleting any cached repo unused for 30+ days; tracked separately, not part of
    this item — `~/.ratect/incl` grows unbounded until removed by hand. (Corrected
    later: an earlier draft of this entry also claimed Batect has a manual
    cache-clear CLI command for this — it doesn't; only the automatic sweep is
    real, so there's no matching CLI surface to add.)
- **0.9.0** — ~~**Dependency Readiness**: `health_check` and `setup_commands`, replacing
  today's "started = ready" simplification (see
  [Container fields](docs/differences-from-batect.md#container-fields)) with Batect's
  real readiness gate before a container's dependents start~~ — done, design validated
  against Batect's own implementation (`WaitForContainerToBecomeHealthyStepRunner`,
  `RunContainerSetupCommandsStepRunner`, `RunStagePlanner` in the local `batect`
  checkout):
  - `health_check` (`command`, `interval`, `retries`, `start_period`, `timeout` —
    Batect's Go-style duration strings) overrides the image's own `HEALTHCHECK` at
    container creation; `command` maps to Docker's `CMD-SHELL` form, and an omitted
    field inherits the image's own value.
  - After a dependency starts, Ratect waits on Docker's own event stream
    (`health_status`/`die`, replayed from the beginning of time, matching Batect, so
    a verdict emitted before the stream opened still counts). A container with no
    health check at all is immediately healthy — the old "started = ready" behavior
    is now just that special case. Unhealthy fails the task with the last
    health-check run's exit code and output (Batect's message shape, including its
    "exited 0 just after the timeout expired" special case); exiting before a verdict
    fails too. No Ratect-side timeout, matching Batect — Docker's own
    `interval`/`retries` bound the wait.
  - `setup_commands` then run via Docker's `exec` mechanism, one at a time in
    declared order, with the container's own environment and (under
    `run_as_current_user`) the container's own `uid:gid`, each command via `sh -c`
    (the same shell treatment a task's `command` gets). A non-zero exit fails the
    task with the command's output. Only after all of them is the dependency "ready"
    and its dependents allowed to start.
  - Known gaps, candidates for later work rather than blocking this release: the
    task's *own* container's `setup_commands` don't run (Batect runs every container
    through the same per-container steps, so its task container's setup commands run
    concurrently with the task's command — Ratect's sequential engine has no
    concurrent exec path until [0.15.0](#ratect-compat)'s parallelism work); its
    `health_check`, while applied, never gates the task's outcome (in Batect an
    unhealthy task container can fail the task even as its command runs); and a
    setup command's omitted `working_directory` falls back straight to the image's
    default, since the container-level `working_directory` field it should fall back
    to first doesn't exist until [0.13.0](#ratect-compat).
- **0.10.0** — ~~**Interactive Mode Completeness**: closes the known gaps left by 0.4.0 —
  live terminal-resize forwarding for the rest of an interactive session (not just
  synced once at attach time), decoupling stdin forwarding from TTY allocation (piping
  input into a non-interactive task), and propagating the host's `TERM` into the
  container's environment alongside proxy variables~~ — done, design validated against
  Batect's own implementation (`ConsoleInfo`, `TaskContainerOnlyIOStreamingOptions`,
  `DockerContainerEnvironmentVariableProvider` in the local `batect` checkout):
  - Stdin forwarding (`open_stdin`/`attach_stdin`) and `TERM` propagation are now both
    gated on the task being interactive-eligible alone, independent of whether a real
    TTY is actually allocated — matching Batect's own unconditional behavior for both,
    confirmed by reading its source rather than guessing.
  - A local terminal resize is now forwarded for the whole session via a `SIGWINCH`
    listener (`tokio::signal::unix`, Unix-only — a plain OS signal, not crossterm's
    `event`/`EventStream` API, which stays off-limits per the existing `crossterm`
    entry in `CLAUDE.md`), not just synced once at attach.
  - Known gap, deliberately not changed as part of this release: Batect's real-TTY
    gate (`useTTYForContainer`) checks only whether its output is a real terminal;
    Ratect's (`should_use_tty`) still requires *both* stdin and stdout to be real
    terminals. Also carried forward from 0.4.0: Windows terminal handling is
    implemented but hasn't been verified, and live resize forwarding is additionally
    Unix-only (non-Unix keeps the once-at-attach-only sync instead of erroring).
- **0.11.0** — ~~**Build Customization**: `build_target`, custom `dockerfile`
  naming/location, `build_secrets`, `build_ssh` — extends 0.3.0's image-building
  support~~ — done:
  - `dockerfile` (a path relative to `build_directory`'s own root, defaulting to
    `Dockerfile` there) and `build_target` (Docker's own `--target`) both land on
    Docker's classic (non-BuildKit) build API unchanged — no new dependency, no
    behavior change for any container that doesn't use them.
  - `build_secrets` (either `{environment: NAME}` or `{path: ...}`, the latter
    resolved/containment-checked the same way as `build_directory`) and `build_ssh`
    switch that specific build to a BuildKit gRPC session instead (`bollard`'s `Moby`
    driver, upgrading the *existing* Docker daemon's own `/session`+`/grpc`
    endpoints — no separate persistent builder container, unlike `bollard`'s other
    drivers) — every build using neither field stays on the classic path, completely
    unaffected. `build_secrets` additionally disables that build's cache: BuildKit
    deliberately excludes a secret's value from its cache key, which would otherwise
    silently serve a stale secret from a cached layer after only the secret's value
    changed (found and fixed during this release's own integration testing, not part
    of the original design).
  - **Known, deliberate divergence from Batect**: `build_ssh` only supports
    forwarding the host's running `ssh-agent` (via `SSH_AUTH_SOCK`) under BuildKit's
    implicit `default` agent id — at most one entry; a non-`default` id or explicit
    key `paths` is rejected with a clear error rather than silently ignored. Batect
    supports multiple named agents and forwarding explicit private key files instead
    of a running agent (confirmed by reading Batect's own `BuildImageStepRunner`
    and its `docker-client`'s `sshAgentsFromRequest`, which forward straight through
    to upstream BuildKit's own `sshprovider.AgentConfig{ID, Paths}` — not assumed
    from Batect's docs alone); `bollard`, the Docker client `ratect` is built on,
    only exposes a single on/off toggle for the default agent, not either of those.
  - `#[async_trait]`'s `Send` bound on every `ContainerRuntime` method (needed since
    `bollard`'s own BuildKit session machinery isn't `Send`) is worked around by
    driving the BuildKit build to completion on a dedicated `spawn_blocking` thread
    via `Handle::block_on`, Tokio's own documented escape hatch for exactly this —
    see the `bollard` entry in `AGENTS.md`.
  - Known gaps, candidates for later work rather than blocking this release: a
    BuildKit-session build's output isn't captured (no `RUST_LOG=debug` transcript;
    a failure's error names the failing instruction and its exit code but not what
    that step printed — `bollard`'s session API exposes no log stream), and Ratect's
    classic-builder-by-default quietly diverges from Batect, which defaults to the
    daemon's ping-advertised builder — BuildKit on any modern daemon, making its
    `--enable-buildkit` flag a force-override rather than the primary switch. Both
    documented in
    [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps),
    and both expected to be closed by [0.12.0](#ratect-compat).
- **0.12.0** — ~~**BuildKit by Default**: build with the builder the daemon's own ping
  response advertises (BuildKit on any modern daemon), exactly Batect's rule
  (`DockerConnectivity.kt`) — closing 0.11.0's two known gaps and a live
  incompatibility: the classic builder rejects modern Dockerfile syntax (heredocs,
  `COPY --link`, `RUN --mount=type=cache`) that real Batect projects, which have
  been building under BuildKit for years, may already use~~ — done, exactly per the
  plan validated against `bollard` 0.21.0's source before implementation began:
  - All BuildKit builds unified onto bollard's classic-endpoint+session path
    (`build_image` with `BuilderBuildKit` + a per-build session), whose response
    stream carries full BuildKit progress/logs (`BuildInfoAux::BuildKit`) *and* the
    built image ID — restoring 0.11.0's missing `RUST_LOG=debug` transcript and
    transcript-in-error behavior for `build_secrets`/`build_ssh` builds, replacing
    both the gRPC-driver path's `spawn_blocking` non-`Send` workaround (the new
    path's future is `Send`) and its post-build `inspect_image` ID lookup.
  - The one missing piece in bollard 0.21.0 — `build_image`'s internal session only
    registers auth/file-send providers, with no way for a caller to supply the
    secrets/ssh session services — plus `ping_info` (the `/_ping` response's
    `Builder-Version` header, which plain `ping()` discards) are carried by a fork
    (`or1can/bollard`, branch `ratect/session-providers-0.21`) consumed through
    `[patch.crates-io]` and commit-pinned via `Cargo.lock`, with both changes PR'd
    upstream ([bollard#731](https://github.com/fussybeaver/bollard/pull/731),
    [bollard#732](https://github.com/fussybeaver/bollard/pull/732)) (a third, separable upstream contribution remains open: named
    agents/explicit key files for full `build_ssh` parity, which needs an
    in-process ssh keyring agent). Deliberately *not* a build-library switch: no
    other Rust BuildKit client supports the Docker daemon's own `/session`+`/grpc`
    upgrade path or session providers at all (the young `buildkit-client` crate was
    evaluated and ruled out — standalone-`buildkitd` oriented, no secrets/ssh
    providers, single-maintainer 0.1.x).
  - Builder selection matches Batect's, including its env override: `DOCKER_BUILDKIT`
    (`1`/`true`/`0`/`false`, anything else a hard error) wins over the daemon's
    advertised default; a daemon advertising no default builder falls back to the
    classic builder. The classic path stays for exactly those two cases — and
    `DOCKER_BUILDKIT=0` is what keeps it exercised in CI now that the default is
    BuildKit everywhere. [0.17.0](#ratect-compat)'s `--enable-buildkit` flag becomes
    pure CLI surface over this already-shipped selection logic.
  - `build_secrets`/`build_ssh` under a forced (or daemon-imposed) classic builder
    fail with a clear "requires BuildKit" error rather than silently building
    without the secret/agent.
- **0.13.0** — ~~**Container Runtime Options**: `entrypoint` (container and `run`),
  `working_directory` (container and `run`), `labels`,
  `capabilities_to_add`/`capabilities_to_drop`, `privileged`, `shm_size`, `devices`,
  `enable_init_process`, `image_pull_policy` — the remaining container/run fields,
  each largely a direct pass-through to the Docker API~~ — done, all nine fields,
  each proven against a real Docker daemon (not just unit tests against the fake
  runtime):
  - `working_directory` and `entrypoint` support both a container-level default and
    a task-level `run` override; the other seven are container level only, matching
    Batect exactly (none of them exist on Batect's own `TaskRunConfiguration` either).
  - Landing `entrypoint` exposed a real bug in Ratect's own pre-existing `command`
    handling: `command` had always run via an implicit `sh -c` wrap (a divergence
    from Batect's own tokenizer, `Command.parse`), which would have silently
    double-wrapped once a container also set `entrypoint` (the classic Batect idiom
    `entrypoint: /bin/sh -c` + a single-quoted `command: 'some command'`). Fixed
    ahead of `entrypoint` by porting Batect's tokenizer (`tokenize_command_line`,
    `docker.rs`) and dropping the `sh -c` wrap entirely — a breaking change to
    `command`'s existing behavior, acceptable this early (no external configs yet to
    break): `command: sh -c "..."` now needs to be written explicitly for shell
    operators. `setup_commands` is intentionally left on `sh -c` for now, a narrower,
    separate, still-open divergence.
  - `capabilities_to_add`/`capabilities_to_drop` validate against a fixed capability
    list based on Batect's own `Capability` enum — extended with `BPF`/
    `CHECKPOINT_RESTORE`/`PERFMON`, which Batect's own (unmaintained) list predates
    ([moby#41563](https://github.com/moby/moby/pull/41563)); a superset, not a
    divergence.
  - `devices` uncovered a genuine bollard/Docker API gap during its own
    integration test: an omitted `options` (cgroup permissions) makes `runc` fail
    outright, since Docker's raw API — unlike the `docker` CLI — applies no default;
    `build_devices` now applies the CLI's own `"rwm"` default itself.
  - `image_pull_policy` changes an existing default, not just adds a field:
    `IfNotPresent` (Batect's own default) now skips a container's pull entirely when
    the image already exists locally (new `ContainerRuntime::image_exists_locally`),
    replacing Ratect's previous unconditional-pull behavior for every `image`
    container, not just ones that opt into the field. Scoped to `image` containers
    only — Ratect doesn't implement Batect's separate use of this same field to
    force-pull a `build_directory` build's base image.
- **0.14.0** — ~~**Task Model Completeness**: task-level `dependencies` (sidecars
  scoped to a task, distinct from the container-level field shipped in 0.6.0),
  `description`/`group` (plus corresponding `--list-tasks` output), and
  `customise`.~~ — done, plus one addition found while scoping the work:
  - `run` is no longer required on a task — a task with only `prerequisites` and
    no `run` is now valid, matching Batect (previously rejected outright); its
    prerequisites still execute, then Ratect stops there, since there's no
    container of the task's own left to run.
  - Task-level `dependencies`: sidecars scoped to one task specifically, unioned
    with the task's own container's `dependencies` (and folded into the same
    `no_proxy` exemption list) when resolving what to start alongside it.
    Requires `run`, and can't name `run.container` itself.
  - `description`/`group`: shown in `--list-tasks` output — grouped under a
    `{group}:` heading (plus a trailing `Ungrouped tasks:` bucket) once *any*
    task in the project declares a `group`; otherwise the pre-0.14.0 flat list
    stays unchanged.
  - `customise`: per-task `environment`/`ports`/`working_directory` overrides
    for a *non-main* container used anywhere in the task's own container graph
    (at any depth). Can't target the main task container itself (set the
    equivalent property on `run` instead) or a container outside the task's
    graph — both validated at config-load time, matching Batect's own
    `Task`/`ContainerDependencyGraph` checks.
- **0.15.0** — ~~**Parallel Task Execution**: independent prerequisites and tasks run
  concurrently via `tokio`, rather than sequentially~~ — done, scoped down from this
  headline after checking the real Batect implementation (`ContainerDependencyGraph`/
  `RunStagePlanner`/`ParallelExecutionManager`, plus `TaskExecutionOrderResolver`/
  `SessionRunner` in the local `batect` checkout) before building: Batect itself never
  parallelizes independent `prerequisites` — only *within one task's own container
  graph* (image pulls/builds, container starts, health-check waits, setup commands).
  That's exactly what shipped:
  - `ratect-core/src/engine.rs` gained a static, up-front phase
    (`build_dependency_graph`) that builds one task's deduplicated container
    dependency graph and detects a circular container dependency via DFS ancestor
    path — mirroring Batect's `ContainerDependencyGraph`, and replacing the old
    *dynamic* `resolving`/`running` cycle check, which would have falsely flagged a
    diamond dependency as circular once siblings could start concurrently.
  - `start_dependency` became `ensure_container_ready`: memoized per task execution
    via `Arc<tokio::sync::OnceCell<...>>` (`ReadyCell`) keyed by container name, fanning
    out to a container's own dependencies concurrently (`futures::try_join_all`) before
    doing its own work. Two concurrent branches reaching the same node (a diamond)
    converge on one `OnceCell` instead of double-starting it — Rust's async/await
    achieving the same per-container dedup Batect gets from
    `ParallelExecutionManager`'s thread-pool/event-bus, without porting that machinery.
  - Fixed a latent race this also exposed: `pulled_images`/`built_images`'s
    check-then-act dedup was only safe under fully sequential execution. Both now use
    the same `ReadyCell` memoization, so two containers concurrently resolving the same
    image share one in-flight pull/build instead of racing to do it twice.
  - `prerequisites` staying strictly sequential is a deliberate scope decision, not a
    shortfall — it matches Batect's own behavior exactly. Running independent
    prerequisites concurrently remains a possible Rust-specific enhancement beyond
    Batect for later (see [Rust Enhancements](#rust-enhancements)), not committed to.
  - Closes the container-startup half of the
    [runtime behavior gap](docs/differences-from-batect.md#runtime-behavior-gaps)
    against Batect (see docs/task-lifecycle.md's "Dependency resolution" and "Known
    simplifications" sections for the full behavior).
  - This release also carries four unrelated fixes found while hand-testing the above
    (see `CHANGELOG.md` for full detail on each): a container's own `command` field
    (previously only reachable via a task's `run.command`, missed alongside 0.13.0's
    other container runtime options); `setup_commands.command` tokenized into literal
    argv instead of running via `sh -c` (closing a divergence left open since that same
    0.13.0 work); wildcard (`*`) `prerequisites` (missed alongside 0.14.0's Task Model
    Completeness); and task name suggestions ("Did you mean...?", ported from Batect's
    `TaskSuggester`).
- **0.16.0** — ~~**Output Modes**: `--output`/`-o` (Batect's `fancy`/`simple`/`quiet`/
  `all` modes) together with automatic default-mode selection based on terminal
  capabilities — the two can't ship separately, since auto-detection is the logic for
  picking between modes that don't otherwise exist. Also closes `--no-color` (color is
  one axis of the fancy/simple distinction)~~ — done, design validated against Batect's
  own implementation (`OutputStyle`, `EventLogger`/`EventLoggerProvider`,
  `ui/{quiet,simple,fancy,interleaved,containerio}/`, `ConsoleInfo`, `Console`, and
  `CommandLineOptionsParser` in the local `batect` checkout) before building, exactly
  Batect's four styles:
  - Landed the seam every style plugs into first: `ratect-core/src/ui/`, a port of
    Batect's `TaskEventSink`/`EventLogger` design. `engine.rs` and `docker.rs` post
    typed `TaskEvent`s to an injected `EventSink` instead of printing directly, and the
    selected logger decides what each event renders as — replacing the previous
    `indicatif` spinner entirely (no Batect style uses one) and, as a side effect,
    fixing concurrent pulls/builds (possible since 0.15.0) racing their uncoordinated
    spinners onto the same terminal line.
  - `simple` (the non-interactive default) and `quiet` (stdout is exactly the
    containers' own output; also a machine-readable `--list-tasks`) landed first,
    followed by `fancy` (a live per-container status block, cursor-movement repainted
    in place — no spinner, Batect doesn't use one either) and `all` (every container's
    output, dependencies included, line-prefixed and interleaved — the only mode that
    shows dependency/setup-command/build output at all). See `CHANGELOG.md`'s Added
    entries for each style's full behavior.
  - `--output`'s auto-selection and `--no-color` match Batect's own rule exactly
    (`EventLoggerProvider`/`ConsoleInfo.supportsInteractivity`, confirmed by reading
    its source), minus Batect's mintty and legacy `TRAVIS` special cases — deliberately
    skipped, since Windows is untested here and modern CI doesn't allocate a TTY, so
    the terminal check already covers it.
  - Two deliberate divergences from Batect, both documented in
    [Differences from Batect](docs/differences-from-batect.md#cli-flags): an explicit
    `-o fancy` on a non-interactive console fails with a clear error up front, where
    Batect accepts it and crashes on the first repaint; and `-o fancy --no-color`
    renders colorless fancy (the live repaint stays, only bold/color drop), where
    Batect's console couples color and cursor movement into one flag and rejects the
    combination outright.
  - A pre-release review of this release's own diff (found via the project's own
    review tooling, not hand-testing) turned up thirteen further issues, all fixed as
    separate commits before release rather than folded into the feature commits above:
    two genuine display bugs (fancy's cleanup line could overwrite a task's final
    unterminated output line; a `--use-network` failure could end the event stream
    with no `TaskFailed` ever posted, leaving `all` mode's preamble stuck unprinted), a
    silent-failure gap (a task's fatal error reached stderr solely via
    `tracing::error!`, which `RUST_LOG` can suppress — a failure under `-o quiet` plus
    `RUST_LOG=off` could exit non-zero with no visible output anywhere), a `char`-count
    vs. real terminal-display-width bug in fancy/`all` (new `unicode-width`
    dependency), a dropped-output bug in `all` mode's log streaming plus the
    duplicated implementation that caused it, and a background log-follower race that
    could let `all` mode's dependency output arrive after that container's own
    removal was reported. The remaining fixes were internal hardening/simplification
    with no user-visible behavior change (see `git log` and `TODO.md`, which also
    tracks what was investigated and deliberately left as-is — e.g. `LineBuffer`'s
    CR-only line buffering, confirmed faithful to Batect's own identical behavior).
- **0.17.0** — ~~**Remaining CLI Parity**: `--skip-prerequisites`, `--override-image`,
  `--no-cleanup`/`--no-cleanup-after-failure`/`--no-cleanup-after-success`,
  `--tag-image`, `--enable-buildkit` (just the tristate flag surface — the
  underlying builder-version selection, including the daemon's ping-advertised
  default, ships in [0.12.0](#ratect-compat)), `--docker-host`/
  `--docker-context`/`--docker-config`/`--docker-cert-path`/`--docker-tls*`
  (verified-only — see [Differences from
  Batect](docs/differences-from-batect.md#cli-flags) for why there's
  deliberately no skip-verification mode), and `--max-parallelism` (folded in
  after the fact, not part of the original scope above — caps image
  pulls/builds, dependency starts, and setup-command execution; narrower than
  Batect's own flag, which also caps health-check waits and container
  stop/removal — see the differences doc for the full reasoning)~~ — done, all
  six flag groups above shipped, each with unit tests (several using
  `#[tokio::test(start_paused = true)]` to prove concurrency/ordering
  deterministically) and a real-Docker smoke test, plus the full `--ignored`
  end-to-end suite passing throughout with no regressions.
  - `--docker-tls*` took the verified-only stance a step further than first
    scoped: rather than mirroring Batect's bare `--docker-tls` (Go's
    `InsecureSkipVerify`, which disables the whole chain/expiry/hostname check,
    not just hostname matching), Ratect's `--docker-tls` and
    `--docker-tls-verify` behave identically — always fully verified, matching
    `rustls`'s own design (no boolean toggle for this at all; skipping
    verification means implementing `ServerCertVerifier` from scratch). Comes
    with a worked example ([CLI
    reference](docs/cli-reference.md#tls-with-a-private-certificate-authority))
    for the private-CA setup this pushes users toward instead of skipping
    verification, and a hermetic `rcgen`-generated (not hand-embedded, so
    nothing sits around waiting to expire years from now) + `tokio-rustls`
    in-process handshake test proving both the accept and reject paths for
    real, not just against mocks.
  - `--max-parallelism`'s scope was widened mid-implementation: initially just
    image pulls/builds, extended to also cover dependency container starts and
    setup-command execution (CPU/disk-intensive, unlike the deliberately
    ungated health-check poll and stop/removal cleanup) via one invocation-wide
    semaphore, permits acquired/released around each individual operation and
    never nested across a whole container's readiness sequence (would deadlock
    at cap 1).
  - A documentation audit (prompted by "anything we've missed?" against
    [Differences from Batect](docs/differences-from-batect.md#cli-flags))
    found and fixed several real gaps beyond the flags themselves: Batect's
    `--log-file`, `--no-update-notification`, `--upgrade`, and
    `--no-wrapper-cache-cleanup` were previously hard `clap` parse errors
    (exit code 2, killing the *entire* invocation before anything ran,
    including `--list-tasks`) — a real risk for any Batect script/CI pipeline
    migrating to Ratect with one already baked in. `--log-file` is now
    genuinely implemented (tees Ratect's own logs to a file in addition to
    stderr); the other three are recognized but inert, since there's no
    self-updating wrapper script here to act on — except `--upgrade`, which
    prints a one-line notice and exits `0`, since a user invoking it is likely
    expecting some visible response. `docs/cli-reference.md`'s Environment
    Variables table and `docs/task-lifecycle.md`'s cleanup/concurrency
    descriptions were also updated to stay accurate against the new flags.
  - `--cache-type`/`--clean`/`--clean-cache` were deliberately scoped out —
    they need the still-unimplemented `volumes` `cache` mount type to do
    anything, so they're tracked as their own new [0.18.0](#ratect-compat)
    entry instead of being forced into this release.
- **0.18.0** — ~~**Cache Volumes**: `volumes`' `cache` mount type (a named
  volume that persists between separate `ratect` invocations — a Docker named
  volume by default, or a host directory under `--cache-type=directory`),
  plus the `--cache-type`/`--clean`/`--clean-cache` CLI flags this needs to
  actually do anything (currently unimplemented — see [Differences from
  Batect](docs/differences-from-batect.md#cli-flags)). Needs a project-scoped
  cache volume naming convention (Batect's own
  `batect-cache-<project-key>-<name>`) to avoid colliding across unrelated
  projects that happen to share a cache name.~~ — done: `cache` mounts
  (object form only — `type: cache`, `name`, `container`, `options`) resolve
  to a Docker named volume (`batect-cache-<project-key>-<name>`, Batect's own
  literal naming, deliberately, for drop-in cache reuse when migrating from
  real Batect) or a host directory under `.batect/caches/<name>/`, selected
  by `--cache-type` (new `ratect-core/src/cache.rs` module). The project key
  itself is a full UUID rather than Batect's 6-char id when freshly
  generated — an existing Batect-created key file is read and reused
  byte-for-byte instead, since nothing depends on matching the generation
  format, only the file's path and read-compatible layout. `--clean`/
  `--clean-cache <NAME>` clear out a project's cache volumes/directories
  (new `ContainerRuntime::list_volumes`/`remove_volume`), matching Batect's
  own `CleanupCachesCommand` exactly, including never needing the task
  config to exist. `tmpfs` mounts remain a separate, still-unscheduled gap —
  see [Differences from Batect](docs/differences-from-batect.md#container-fields).
- **0.19.0** — ~~**Parity Mop-Up**: closes several smaller gaps left over from
  earlier releases, ahead of 1.0.0~~ — done:
  - `forbid_telemetry` and `config_variables.<name>.description` are now
    recognized but inert, the same "no effect" treatment already given
    `--upgrade`/`--no-update-notification`/`--no-wrapper-cache-cleanup`
    (0.17.0). Both are purely informational in Batect itself (no runtime
    behavior to diverge from — Ratect has no telemetry to forbid and no help
    output to show a description in), unlike a field such as `log_driver`
    below, where silently ignoring it would mean actually doing something
    other than what the config asked for — see the note on unsupported
    fields at the top of [Differences from
    Batect](docs/differences-from-batect.md). Previously, a real Batect
    project using either field failed to load at all under Ratect's
    `deny_unknown_fields` parsing.
  - **Git-include cache eviction**: a 30-day automatic sweep for
    `~/.ratect/incl`, matching Batect's own `GitRepositoryCacheCleanupTask`
    exactly — an unconditional, fire-and-forget background task
    (`GitIncludeCache::cleanup_stale`, `tokio::spawn`, not an OS thread —
    Batect's own JVM daemon thread is the equivalent, not a literal port)
    started on every "run a task" invocation (not `--list-tasks`), deleting
    any cached repo not used in the last 30 days. Not a CLI feature — Batect
    has no manual clean command for this either, only the automatic sweep
    (see [Differences from
    Batect](docs/differences-from-batect.md#top-level-fields), corrected
    after an earlier draft of the 0.8.0 entry above overstated this gap).
  - `log_driver`/`log_options` (`Container`) now reach a real container's
    `HostConfig.LogConfig` (Docker's `--log-driver`/`--log-opt`), verified
    end-to-end against a live daemon — previously zero support at all (a
    hard config-load error), not partial.
  - `image_pull_policy`'s second use on a `build_directory` container:
    `Always` now force-pulls the build's own base image (`docker build
    --pull`) before building, distinct from the already-supported
    `image`-container pull decision. `ContainerRuntime::build_image` gained
    a `force_pull` parameter.
- **0.20.0** — **Two-Binary Split**: ~~the workspace actually splits as described
  in [Two Binaries](#two-binaries-ratect-and-ratect-compat), as its own
  dedicated structural change — not folded into a feature release or into
  1.0.0's own version-bump commit. The root package (still plainly named
  `ratect` today, even though everything shipped under it so far, 0.1.0
  through 0.19.0, is `ratect-compat`'s own work) moves to a dedicated
  `ratect-compat` crate, freeing up the `ratect` name for a placeholder crate
  for the forward-looking binary. At this point it's purely
  structural/naming — a placeholder, not yet `ratect`-specific functionality
  — proving the workspace mechanics (Cargo.toml layout, CI, `tests/cli.rs`'s
  `CARGO_BIN_EXE_ratect` references, `docs/installation.md`) actually work
  with two binaries before real `ratect`-only feature work starts on top.~~ —
  done: root `Cargo.toml` is now a virtual workspace manifest; the former root
  package moved to `ratect-compat/` (same behavior, same 0.20.0-dev version
  history) and a genuinely empty placeholder `ratect/` crate (fresh
  `0.1.0-dev`) was added alongside it. `ratect-compat/tests/cli.rs` now
  targets `CARGO_BIN_EXE_ratect-compat` (Cargo keeps the literal binary name,
  hyphen included, rather than underscoring it). CI needed no changes — every
  job already ran `--workspace`. Docs (`README.md`, `docs/installation.md`,
  and the CLI/config-reference/getting-started/how-it-works/task-lifecycle
  invocation examples) updated to name `ratect-compat` explicitly.
- **0.21.0** (planned) — **Parity Mop-Up II**: closes known Batect-parity gaps
  left open since before 0.19.0 — see [Differences from Batect](docs/differences-from-batect.md)
  for the itemized status of each. One `feat:` commit per item, matching the
  repo's own commit-packaging convention for a release bundling several
  separable behaviors:
  - ~~**`tmpfs` volumes**: Batect's third `volumes` mount kind (in-memory,
    ephemeral, lost on container exit) — currently entirely unsupported,
    alongside the already-supported `local`/`cache` kinds.~~ — done: a new
    `VolumeMount::Tmpfs` variant (object form only — no compact string form,
    matching Batect), mapped onto Docker's own `HostConfig.Tmpfs` map
    (`container_path` → an opaque `options` string forwarded verbatim,
    unparsed — matching Batect's own `VolumeMountResolver`, which also
    normalizes a missing `options` to `""`). Threaded through as a new
    `ContainerOptions.tmpfs` field (not folded into the existing `volumes`
    bind-string parameter, since a tmpfs mount can't be expressed as a bind
    string at all) — resolved synchronously, unlike `local`/`cache`, since it
    needs no cache-key lookup.
  - ~~**The task's own container's `setup_commands`**: currently only run for
    dependency containers, not the task's own — Batect runs them concurrently
    with the task's command; closing this needs the engine's first concurrent
    exec path for a single container's own readiness-then-run sequence.~~ —
    done: the task's own container now goes through the same readiness gate a
    dependency always has (health-check wait, then `setup_commands`, in
    order) — the engine's first concurrent-exec path
    (`TaskEngine::run_task_container_readiness`, run via `tokio::join!`
    alongside `ContainerRuntime::run_container`'s own attach-and-wait-for-
    exit). `run_container` gained two new parameters: `started` (a
    `oneshot::Sender` signaled with the container's id right after Docker's
    own `start` call, letting the readiness gate begin) and `readiness` (a
    `oneshot::Receiver` `run_container` itself awaits, for the readiness
    gate's own outcome, *before* removing the container — without this, a
    fast-exiting main command would routinely race the still-in-flight
    readiness gate against the container's own removal). One race this still
    shares with Batect (confirmed against Batect's own source — its
    `RunStage` completion is driven purely by the container's exit event, not
    its readiness either): a main command that exits very quickly — with no
    `health_check` configured especially, since the readiness gate then
    starts its `setup_commands` almost immediately — can still finish before
    a setup command gets a chance to `docker exec` into it, surfacing
    Docker's own "container is not running" error instead of that command's
    real outcome; anything taking more than a few tens of milliseconds is
    unaffected. Also unlike Batect (a deliberate simplification, not a bug):
    the main command is never cancelled early just because the readiness
    gate fails first — it always runs to completion, and the task is still
    reported as failed overall either way. See [task
    lifecycle](docs/task-lifecycle.md#known-simplifications-relative-to-batect).
  - ~~**Config Schema**: a JSON schema for `batect.yml`'s actual Ratect-accepted
    shape (editor autocompletion/validation), likely generated from
    `ratect-core/src/config.rs`'s own `Serialize`/`Deserialize` structs — see
    the [Batect Parity](#batect-parity) headline entry for the full reasoning
    on why this can't just be Batect's own published schema.~~ — done:
    generated from those types via `schemars` (`ratect-core/src/schema.rs`)
    and committed at [`schema/batect-config.schema.json`](schema/batect-config.schema.json),
    described in [config reference](docs/config-reference.md#editor-autocompletion-and-validation).
    Generation lives behind a non-default `schema` feature, so neither
    shipped binary carries the derived `JsonSchema` impls; CI's
    `--all-features` test run is what keeps the committed file honest.
    Draft-07 rather than schemars' own default (2020-12), because
    `yaml-language-server` — what VS Code and JetBrains actually run — only
    implements draft-07 fully; under 2020-12 a `$ref` with sibling keywords
    silently drops the siblings, which is every description on a `$ref`'d
    field. The string-or-object types (`ports`, `devices`, `volumes`,
    `build_secrets`, `include`, and `PortRange` itself) have hand-written
    `JsonSchema` impls, since their hand-written `Deserialize` impls are
    exactly the ones a derive can't see through. Field documentation comes
    from the config types' own doc comments (first paragraph, reflowed,
    rustdoc link syntax stripped) rather than a second hand-maintained copy
    per field. Tests: the committed file is regenerated and compared, and
    every fixture in the repository that parses as config is validated
    against the schema — the direction that matters, since a schema that's
    too strict puts a red squiggle under working configuration. Not
    submitted to SchemaStore; that stays a separate, later decision.
  - **Explicitly excluded**: `build_ssh` full parity (multiple named agents,
    explicit key-file paths — see [issue #1](https://github.com/or1can/ratect/issues/1))
    stays out of scope here. It's blocked on `bollard`'s session-provider
    surface growing beyond what the current fork exposes, and that's deferred
    until the two open upstream PRs (#731, #732 — see
    [Key Dependencies](AGENTS.md#key-dependencies)) land, rather than piling
    further changes onto the fork ahead of them.
- **0.23.0 → 1.0.0 (planned) — Batect conformance**: not one release but the *phase*
  between here and 1.0.0, shipped across the next few minors — porting ~30 journey
  projects surfaces parity bugs to fix, so each release turns more of the corpus
  green, and 1.0.0 is earned once it all is (this is what "verified against real
  Batect projects" in the 1.0.0 gate below actually means, made executable). Starts
  at 0.23.0 on the assumption the current cycle cuts as 0.22.0 — `ratect-compat`
  gained a feature this cycle (ownership labels), so it's a minor, not the
  `0.21.1-dev` placeholder the `Cargo.toml`s still carry. Our own tests encode *our*
  reading of Batect (careful, but confirmation-bias-prone: we test the paths we
  thought of); this closes that gap by running `ratect-compat` against Batect's
  *own* acceptance corpus:
  - **Vendor Batect's journey-test projects** (`app/src/journeyTest/resources/` —
    ~30 complete projects covering dependencies, caches, includes,
    `run_as_current_user`, health checks, customisation, log drivers, …) verbatim
    under `ratect-compat/tests/conformance/batect-journey/`, and assert the same
    *observable* behaviour Batect's own journey tests do. Batect is Apache-2.0
    (as is Ratect), so this is vendored with attribution (`NOTICE`, the
    `dockerignore` precedent) — and vendoring *frozen, archived* fixtures is the
    mitigation for "depending on a deprecated resource", not the risk: they never
    change, which is exactly what a conformance corpus wants. The spike landed a
    working harness + the first project (`simple-task-using-image`); 0.23.0 grew
    the harness (extra CLI args, host env vars, combined-output assertions) and
    ported the first batch of ~11 projects — prerequisites, mounts, Dockerfile
    builds, custom Dockerfile names, config variables, host environment, setup
    commands (on both a dependency and the task container), a health-checked
    dependency, and `--list-tasks`. That batch immediately earned its keep: it
    surfaced the missing `batect.local.yml` default for `--config-vars-file`
    (now fixed). Still to port: caches (needs a two-run harness), proxy
    variables (build-time propagation), `run_as_current_user` (needs its output
    mount adapted from Batect's own build tree), image/parallelism/customise
    flags, and `--override-image`/`--tag-image`; git includes and the
    Windows-container project stay out of this local-corpus scope.
  - **Assert behaviour, not Batect's transcript.** Batect's own assertions often
    check its exact output wording, which `ratect-compat` deliberately diverges
    from ([`docs/differences-from-batect.md`](docs/differences-from-batect.md)).
    The harness pins exit codes and the task command's own output instead. Where
    behaviour diverges *on purpose* (a documented simplification), it's recorded
    as an explicit `divergence` expectation — which turns the differences doc from
    prose into an executable report, arguably the most valuable by-product.
  - **A focused slice of real bundles** (git includes against a couple of real
    published Batect bundles) — the strongest real-world exercise of the includes
    feature specifically.
  - **Dogfood**: Ratect builds Ratect. The repository-root `batect.yml` runs the
    project's own `build`/`test`/`lint`/`fmt` in a pinned Rust container, with the
    Cargo registry and build output as `cache` volumes and the toolchain image
    built via `build_directory` — so one real project exercises task running,
    image building, and caching at once. (Landed now, ahead of the rest.) Batect
    never did this for its own Gradle build; a native tool building itself is
    worth showing.
  - **Explicitly *not* the strategy: waiting for user reports.** For a drop-in
    replacement the failure mode is silent abandonment ("it didn't work, I went
    back to compose, I filed nothing"), so reactive feedback can't earn pre-1.0
    confidence. It's a cheap *supplement* (a clear issue path, a user-facing "does
    feature X work?" matrix), not the proof.
  - Deliverable framing: **the value is the bugs this finds**, not green
    checkmarks — the scenarios are Batect's, so they exercise cases our own tests
    didn't. Skipped deliberately: Batect's Kotlin *unit* tests (internal
    implementation, JVM-bound) and its *completion* tests (shell completion, a
    feature `ratect-compat` doesn't ship).
- **1.0.0** — the [Batect Parity](#batect-parity) section above substantially checked
  off (all of the above, including 0.7.0–0.19.0, not just the items shipped through
  0.6.0), and verified against real Batect projects — the conformance corpus above
  green, not just the itemized field/flag tables passing in isolation. Not tagged
  early for appearances — earned once `ratect-compat` can honestly replace `batect`
  on real projects.

### `ratect`

- **0.1.0-dev** — ~~a placeholder crate exists (`ratect/`, added alongside
  `ratect-compat` in 0.20.0's [Two-Binary Split](#two-binaries-ratect-and-ratect-compat))
  but real feature work hasn't started yet.~~ — superseded: never released as
  0.1.0, since there was nothing in it to release. The crate went straight to
  `0.2.0-dev` when work on the subcommand skeleton below opened, right after
  `ratect-compat` 0.21.0 shipped.
- **0.2.0** (in development) — **CLI subcommand skeleton**: `ratect run <task>` and
  `ratect tasks list` (replacing `ratect-compat`'s flat `<task-name>` positional
  and `--list-tasks`), settled as subcommands rather than a flat CLI so later verbs
  (`ratect doctor`, git-include cache management — see
  [UX & Tooling](#ux--tooling)) have somewhere to live without a breaking
  restructure later. `run` is always explicit — no bare `ratect <task>` sugar —
  since that ambiguity (is `doctor` a task name or the `doctor` subcommand?) only
  gets worse as more subcommands land, and "always explicit" is a simpler rule to
  hold once 1.0.0's interface-stability promise applies. Deliberately still wired
  onto `ratect-core`'s existing engine and its current YAML `Config`, completely
  unchanged — no new parser, no new schema — so this proves the subcommand surface
  end-to-end in isolation before any config-format work lands on top of it, the
  same "mechanics before features" sequencing 0.20.0 used for the workspace split
  itself. Docker-connection flags and output-mode selection are reused as-is from
  `ratect-core`/`ui::create_event_sink` — `ratect-compat` already proved that
  surface, nothing to reinvent.

  Landed so far: both subcommands, on the shared glue lifted into `ratect-core`
  for them (`config::load_project`, `engine::TaskEngineSettings`/`with_settings`
  — `ratect-compat` moved onto both in the same change, so the seam is proven
  rather than declared). Two interface decisions worth recording, since they're
  the first places `ratect` deliberately differs rather than merely being newer:
  the Docker-connection options are `run`'s, not global (`tasks list` never
  reaches a daemon, and an accepted-but-ignored flag is worse than one that
  isn't offered) — carried in their own `#[command(flatten)]` struct so a later
  daemon-using verb picks up the identical surface, a rule since applied to the
  config-variable options too (`run`/`tasks list` only); and there's no
  `--log-file`, Batect's own, since redirecting stderr covers it. Documented in
  [`docs/ratect-cli.md`](docs/ratect-cli.md), which is deliberately separate
  from `ratect-compat`'s own CLI reference — two interfaces, not two spellings
  of one.

  `caches list`/`caches clean [NAME...]` replaces `--clean`/`--clean-cache`, with
  two deliberate improvements on the flags it replaces: listing exists at all
  (neither Batect nor `ratect-compat` can say what's there, which makes removing
  one *by name* guesswork against the config file), and a name matching nothing
  warns instead of passing silently. `clean` with names and without are the same
  verb separated by whether anything was named — rather than Batect's `--clean`
  meaning "everything" while `--clean-cache` silently overrides it when both are
  given. Both report a cache by its *config* name, never the
  `batect-cache-<key>-<name>` volume it happens to live in. Like `--clean` it
  never reads the config file, so it works on a project whose configuration is
  broken or absent — which is when clearing a cache is most likely wanted. New
  `cache::list_volume_caches`/`list_directory_caches` in `ratect-core`.

  Two questions deliberately deferred to 0.3.0, when the config work forces
  answers rather than guesses: whether `-f batect.yml` stays the default file name
  once `ratect` has its own format, and (below) whether a `config` verb is how a
  project moves between formats. Neither is worth deciding while `ratect` still
  reads today's YAML unchanged — a default file name that has to move again in one
  release, or a `config convert` with only one format to convert, would both be
  churn.
- **0.3.0** (planned) — **A `ratect`-native config format**, replacing YAML for
  this binary only (`ratect-compat` stays YAML-only, permanently, for Batect
  compatibility). TOML is the leading candidate — more idiomatic for a Rust
  tool, and translates cleanly for most of today's schema (named
  containers/tasks map naturally onto dotted table headers; scalars, string
  maps, and scalar lists are a 1:1 fit) — but the exact syntax is secondary to
  the schema redesign this version is actually about, which holds regardless of
  which concrete format is chosen:
  - **Replacing YAML anchors/aliases/merge keys** (`&name`/`*name`/`<<:`, free
    today via `noyalib` — see [config reference](docs/config-reference.md)) with
    an explicit `extends: <name>` field on `Container`, resolved as its own pass
    after `include`s are merged (so, unlike anchors — scoped to a single parsed
    YAML document — it can reach across `include` boundaries) and reusing the
    same ancestor-path cycle check `engine.rs` already applies to
    `dependencies`/`prerequisites`. Field-level shallow override (a child field
    fully replaces the parent's, no recursive merging into nested maps),
    matching the mental model `<<:` already has today rather than inventing a
    new one.
  - **Standardizing `volumes`/`ports`/`devices`/`include` on one object shape per
    entry**, dropping today's compact string shorthand (`"8080:80"`,
    `.:/code`, a bare include path) — not strictly required by TOML itself
    (heterogeneous arrays are legal), but keeping a list's `[[...]]`
    array-of-tables shorthand usable requires every entry be a table, and
    `include`'s three current shapes (bare string / `{path, type: file}` /
    `{type: git, ...}`) are the worst fit for a mixed-type array of any format.
  - Reuses `ratect-core`'s already-resolved `Config`/`Container`/`Task` types
    unchanged past parsing — only the deserialization front-end and the
    `extends`-resolution pass are new, so `engine.rs`/`docker.rs`/`ui/` need no
    changes at all, the same way `include` resolution is already invisible past
    `Config::load_from_file`. `toml` is already a `ratect-core` dependency
    (`cache.rs`'s sidecar file) — no new crate needed.
- Migration tooling — converting an existing `ratect-compat`-managed project's
  `batect.yml` into `ratect`'s new format — remains a named goal (see
  [Two Binaries](#two-binaries-ratect-and-ratect-compat)), landing with or just
  after 0.3.0, since there's nothing to convert until the second format exists.
  The likely shape is a **`ratect config` verb** — `config convert` to migrate a
  `batect.yml`, probably `config validate` (`doctor`'s config half as its own
  CI-friendly command) alongside it — sitting next to `run`/`tasks`/`caches`/
  `resources`/`doctor`/`includes` as its own noun, the same subcommand structure
  0.2.0 established. Deliberately not built in 0.2.0: a `config convert` with only
  one format to convert between is a no-op that invites "convert to what?", and
  the verb's shape is better decided against the real target format than ahead of
  it.

Its **1.0.0** means something different from `ratect-compat`'s: interface stability
(the subcommand structure and config format won't break), not feature-completeness
against Batect.

## Rust Enhancements

Leveraging Rust's strengths to provide a superior experience compared to the original JVM-based implementation.

- **Parallel Task Execution**: within-task container startup (image pulls/builds, health-check waits, setup commands for independent branches of one task's dependency graph) now runs concurrently via `tokio` — shipped as `ratect-compat` [0.15.0](#ratect-compat), since it also closed a Batect parity gap (Batect does exactly this, just not more). Running independent *prerequisite tasks* concurrently too — which Batect itself doesn't do — remains a possible Rust-specific enhancement for later, not currently scheduled.
- **Static Binaries**: Distribution as zero-dependency static binaries (`ratect` and `ratect-compat`) for easy installation and portability.
- **First-class Cross-platform Support**: Providing a high-performance, native experience across macOS, Linux, and Windows without the overhead or startup latency of a JVM.
- **Precise Error Reporting**: Utilizing Rust's type system and error handling to provide clear, actionable feedback on configuration errors and execution failures.

## UX & Tooling

Improving the developer experience through better tools and feedback.

- **`ratect doctor`**: ~~A built-in linter and diagnostic tool to validate configuration and environment setup. This will include checks for `latest` image tags, missing health checks on dependencies, and host-container permission issues. Should also report anything the orphaned-resource work below finds.~~ — shipped ([0.2.0](#ratect)) with the daemon-reachability, config-loads, `build_directory`/Dockerfile, floating-tag, dependency-without-`health_check` and leftover-resource checks; exits non-zero for problems but not warnings, so it works as a CI step. A leftover `batect`/`batect.cmd` wrapper script that still runs the JVM binary is flagged too (matched by content, so a wrapper repointed at Ratect isn't), as migration assistance. Host-container permission issues (`run_as_current_user` against the actual uid/gid of a mounted path) are the obvious next check and aren't done — they need a real filesystem probe rather than a config read. Container-level checks that need the *image* (whether it defines its own `HEALTHCHECK`, whether an `entrypoint` exists) would need a pull to answer, so they'd belong behind a flag rather than in the default run.
- **Orphaned-resource discovery** (`ratect resources list`/`clean`, working title):
  what's still on this machine from a previous run — after a crash, a `docker
  kill`, a `--no-cleanup`/`--no-cleanup-after-failure` run, or Ratect itself
  failing to tear down. Today answering "what should I remove?" means reading
  `docker ps -a` and guessing, which is precisely the complaint.

  **The blocker is that nothing is marked on the way in**, so this is mostly
  groundwork, not a verb. Containers are created via `create_container(None,
  config)` — no name, and `labels` carries only what the *user* configured — so a
  leftover container is identifiable at best by inference (it's attached to a
  `ratect-<uuid>` network), and under `--use-network` not even that. Batect is no
  better: `DockerContainerCreationSpecFactory` applies `container.labels` and
  nothing of its own, and Batect has no cleanup command at all, which is why this
  has never been answerable. Networks are the one thing that's greppable today,
  purely by their `ratect-` name prefix — and even they can't be attributed to a
  project or a task.

  So the work is, in order:
  1. ~~**Label every resource Ratect creates**~~ — done ([0.21.1](#ratect-compat)
     /[0.2.0](#ratect)), in the shape Docker Compose's own
     `com.docker.compose.*` labels have — runtime *ownership*, which is a
     different thing from OCI image annotations (see below):

     | Label | On | Value |
     | --- | --- | --- |
     | `eu.orican.ratect.project` | containers, networks | `project_name` |
     | `eu.orican.ratect.task` | containers, networks | the task being run |
     | `eu.orican.ratect.run` | containers, networks | the per-run id — the `Uuid` that already names the per-task network, reused rather than minting a second |
     | `eu.orican.ratect.container` | containers | the *config* container name (`build-env`), since Docker's own name is random |
     | `eu.orican.ratect.role` | containers | `task` or `dependency` — derivable from the config, but the point is to work without it |
     | `eu.orican.ratect.version` | containers, networks | the Ratect version that created it, for when the label set itself changes |

     Creation time needs no label: Docker records its own, for both objects.
     These are *additive* to the user's own `labels` — but on an exact key
     collision Ratect's win, because they're load-bearing for cleanup and a
     config that (accidentally or otherwise) set `eu.orican.ratect.run` would
     otherwise make its own resources unfindable. Namespace:
     **`eu.orican.ratect.*`** — reverse-DNS of a
     domain the project already owns, rather than a new `ratect.dev`-style one.
     Reverse-DNS here is purely a collision-avoidance convention (nothing ever
     resolves it), so a new domain would buy nothing functional while adding a
     renewal obligation that a namespace in every `docker inspect` output would
     then depend on. The one thing that would have justified one — a durable
     public URL for the committed [JSON schema](schema/batect-config.schema.json),
     the way Batect used `ide-integration.batect.dev` — doesn't need it either:
     roughly 35–40% of SchemaStore's own catalog entries are
     `raw.githubusercontent.com` URLs. A docs site, if Ratect ever gets one, is
     planned for `ratect.orican.eu` — whose own reverse-DNS is exactly this
     namespace, so that doesn't reopen the question either, and a schema URL
     could move there later without touching a single label. Sticky rather than
     irreversible regardless: the only reader that matters is Ratect
     itself, so a later version can match a legacy namespace alongside a new one
     for a release or two and still find older orphans.

     As shipped (`ratect-core/src/labels.rs`): `RunLabels` is built once per
     task execution in `run_task_internal` and threaded down through
     `ensure_container_ready`, so a task's containers and its network agree on
     one run id. That id is generated there rather than taken from the network's
     own name, deliberately — `--use-network` creates no network to take it
     from, and the containers still have to agree. The version comes from the
     *binary* (`TaskEngineSettings::ratect_version`), not `ratect-core`, whose
     version isn't what `--version` reports; since the two binaries are on
     independent version lines, it also identifies which one created the
     resource.
  2. ~~**`ContainerRuntime` gains `list_containers`/`list_networks`**~~ — done
     ([0.2.0](#ratect)), with label filtering (Docker supports `label=key=value`
     filters natively), alongside today's `list_volumes`. Both return one
     `LabelledResource`, since what's worth saying about a leftover container and
     a leftover network is the same; `list_containers` passes `all: true`,
     because a leftover has usually exited and Docker's default hides those.
  3. ~~**The verb itself**~~ — done ([0.2.0](#ratect)), shaped like `caches`:
     `resources list` shows what's there — grouped by run, with task name and
     age, so "these four containers and a network are from `integration-test`,
     three days ago" is readable at a glance — and `resources clean` removes it.
     Scoped to the current project by default, with `--all-projects` for the
     machine-wide sweep, which is the case the complaint is really about. Also
     `--older-than`, which turned out to matter more than expected — see below.
     Removal takes containers before networks (a network still holding an
     endpoint can't be removed) and a single failure is reported rather than
     abandoning the rest.

  One thing labels can't resolve: a *concurrently running* task's containers are
  labelled identically to an orphan, because they are the same thing until the
  run ends. `list` reporting age, and `clean` taking `--older-than`, is the
  honest mitigation; claiming to detect liveness would not be — the daemon can't
  say whether some other `ratect` process still cares about a container. This is
  documented prominently for `clean`, since a bare sweep on a shared machine can
  take an in-flight run with it. If that turns out to bite in practice, the next
  step would be a heartbeat (a running invocation touching its own resources
  periodically) rather than any attempt to infer liveness after the fact.

  Two safety measures considered and **deliberately not built**, recorded so
  they aren't re-litigated from scratch:

  - **A `--dry-run` for `clean`.** Unnecessary: `list` and `clean` take the same
    options and select through the same code, so `list` already shows exactly
    what `clean` would remove. A flag would be a second spelling of an existing
    command and a second thing to keep in step with it. Both are a snapshot
    either way — a run can start, or a resource age into `--older-than` scope,
    between the two — and a `--dry-run` followed by the real command has the
    identical window.
  - **A confirmation prompt on `clean --all-projects`.** The one thing a dry run
    can't help with: typing the dangerous command by accident, which only a
    prompt catches, since a dry run helps only if you remembered to use it.
    Deferred rather than rejected — it would be the first interactive prompt in
    either binary (Batect has none, so there's no precedent to follow), it needs
    a `--yes` escape for CI, and the two-layer guard on what `--all-projects`
    can even reach ([0.2.0](#ratect)) already removes the catastrophic version
    of the mistake. Worth revisiting on the first report of a near-miss.

  Cache volumes stay outside this: they're deliberate, not leftovers, and
  `caches` already finds them by name prefix. (They also *can't* carry labels
  today without creating them explicitly rather than letting a bind mount
  auto-create them — a separate change, only worth making if it buys something
  else.)

  **Anonymous volumes** were the one genuinely invisible leftover, and are fixed
  at source rather than by this verb: containers are now removed with Docker's
  `v` option ([0.21.1](#ratect-compat)), so a `VOLUME`-declaring image no longer leaves a dangling
  volume per container per run. That had to be a fix rather than a feature —
  Docker names anonymous volumes with a random hash and they can carry no labels
  (Docker creates them implicitly, so Ratect never sees a point at which to mark
  one), which makes them the one resource `resources list` could never have
  identified. The complete inventory this verb covers, then: **containers** and
  **networks** (labelled, above); **cache volumes** and **cache directories**
  (`caches`, already shipped); **built images**, which are tagged
  `<project>-<container>` and are a deliberate cache rather than a leftover —
  worth *reporting* eventually, never worth deleting by default; **anonymous
  volumes**, no longer created; **tmpfs mounts** and **exec instances**, which
  die with their container; and the **Git include cache** under `~/.ratect/incl`,
  which is host filesystem rather than Docker and has its own sweep plus the
  management command below.

  **Not OCI annotations, deliberately.** `org.opencontainers.image.*` is a fixed
  vocabulary describing an *image's provenance* — `source`, `revision`,
  `created`, `licenses`, `title` — and none of it means "the task that started
  this container" or "the run it belonged to". There's no OCI key for runtime
  ownership because OCI doesn't model runtime objects at all; Docker networks
  aren't OCI objects in the first place, so half of what needs labelling here
  couldn't carry them regardless. Bending `image.title` to hold a task name
  would be a misuse of a spec'd key, and the collision risk that reverse-DNS
  namespacing exists to prevent is precisely what it would create. Docker
  Compose reached the same conclusion with `com.docker.compose.project`/
  `.service`, as did Podman with `io.podman.*` — vendor-namespaced ownership
  labels, alongside OCI annotations rather than instead of them.

  The complementary half is real, though, and stays a separate idea: OCI
  annotations belong on the images a `build_directory` container *builds*, as
  the project's own provenance (`source`, `revision`, `created`). Ratect
  shouldn't invent those — only the project knows its own repository and commit,
  and guessing by shelling out to `git` in the build context would be wrong as
  often as right. Today that's a Dockerfile `LABEL`, which already works and
  needs nothing from Ratect. A config field for build-time image labels (as
  distinct from `Container.labels`, which applies to the *container*) would be
  the way to make it ergonomic — `ratect`-only, since Batect has no such field,
  and worth doing only if someone actually wants it.

  **Both binaries label**, decided: the labelling lives in the shared core, and
  the difficulty this solves is `ratect-compat` users' difficulty today, since
  that's the binary anyone actually runs. It's a parity divergence — Batect
  writes no labels of its own — but a strictly additive one that changes no
  behavior and can't break a task that starts using `ratect-compat`, in the same
  family as the `Capability` superset and the UUID cache key. Needs documenting
  in [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps)
  as visible-in-`docker inspect` rather than internal, which is the one way it
  differs from those two.
- **Improved Progress UI**: Output-mode selection with terminal-capability auto-detection and a live per-container progress display shipped as `ratect-compat` [0.16.0](#ratect-compat) (they were Batect parity work); what remains here is going *beyond* Batect — e.g. build context upload progress, richer pull progress (per-layer byte counts), and any `ratect`-binary-specific presentation ideas.
- **Watch Mode**: Automatically re-running tasks when source files change.
- **Git-include cache management** — ~~shipped ([0.2.0](#ratect)) as
  `ratect includes list`/`clean`/`refresh`:~~ a manual command to list/evict entries from
  `~/.ratect/incl` on demand, beyond 0.19.0's automatic 30-day sweep — e.g. force
  a re-clone of one repo without waiting on the sweep, or free disk space
  immediately. **`ratect`-only**, same reasoning as "Restrict Nested Git
  Includes" below (see [Future Vision](#future-vision)) — Batect has no
  equivalent CLI surface at all for this (only the automatic sweep), so there's
  no parity obligation pulling it into `ratect-compat`, and ROADMAP's own [Two
  Binaries](#two-binaries-ratect-and-ratect-compat) principle is that
  `ratect-compat` isn't the place for new ideas.

## Future Vision

Exploring innovative features that go beyond the original Batect, as well as planned improvements from the Batect roadmap.

- ~~**Alternative Configuration Format (TOML)**: Undecided, exploratory. TOML is a more typical configuration format for Rust projects than YAML. If pursued, this would apply only to the [`ratect` binary](#two-binaries-ratect-and-ratect-compat) — `ratect-compat` stays YAML-only for Batect compatibility — and would need a migration path for projects moving from `ratect-compat`'s YAML config.~~ — scoped into `ratect` [0.3.0](#ratect), including the schema redesign (an `extends` field replacing YAML anchors, one object shape per `volumes`/`ports`/`devices`/`include` entry) needed regardless of the exact format chosen. Migration tooling from `ratect-compat`'s YAML remains unscheduled — see the `### ratect` versioned list.

  **Scope, settled before building:**

  - **`refresh` is the valuable one, not `list`.** `ensure_cached`'s
    `clone_if_missing` returns early when the working copy exists, so a
    `(remote, ref)` pair is cloned once and then frozen — permanently. If `ref`
    is a branch, a project silently keeps using whatever that branch pointed at
    the first time, and the 30-day sweep never rescues it, because the sweep
    removes entries that go *unused* and an actively-used include never goes
    stale. Today's only remedy is deleting a hashed directory by hand. Batect is
    identical here (`cloneRepoIfMissing` checks `Files.exists` and nothing else),
    so this is an enhancement rather than a parity gap — consistent with this
    whole bullet being `ratect`-only.
  - **It's a *global* cache, unlike `caches`/`resources`.** `~/.ratect/incl` is
    shared by every project on the machine, so there's no project scoping to
    offer and no `--all-projects` to add: `clean` here necessarily affects other
    projects' includes. That cuts both ways — wider reach than anything else
    Ratect removes, but everything in it is re-cloneable, so the worst case is a
    network fetch rather than lost work. No confirmation prompt for that reason,
    unlike the one deferred for `resources clean`.
  - **The lock is a requirement, not a nicety.** `ensure_cached` takes a
    per-entry lock file around cloning; `clean`/`refresh` have to take the same
    one, or they can delete a directory another `ratect` process is cloning into
    or reading. This is the fiddly part of the work, and the reason the removal
    logic belongs in `git_include.rs` beside the lock rather than in the binary.
  - **Shape**, mirroring `caches`: `includes list` (remote, ref, path, last used,
    size on disk), `includes clean [--older-than <age>]`, `includes refresh
    [<remote>...]`. Named `includes` after the `include:` config field — what a
    user actually types — rather than Batect's "bundles" or the `incl` directory
    name. Core owns listing/removal/refresh (like `cache.rs` does for caches);
    the binary owns presentation.

  **As built** (the decisions below all held; the one thing that changed on
  contact was that `refresh` needed no remote filter to be useful, so it still
  has none):

  - **`clean` with no arguments removes only *stale* entries** — the same 30-day
    threshold the automatic sweep uses — with `--all` for everything and
    `--older-than <age>` for a different threshold. Docker's own `prune` versus
    `prune -a` precedent, and the right default given "everything" here is
    machine-wide rather than this project's. `--all` is really `--older-than 0`,
    kept as its own flag because it's what someone reaches for.
  - **`refresh` does the lot**, with no remote filter to start with. Simpler, and
    the cache is small enough that re-cloning all of it is not the imposition it
    would be for, say, images.
  - **`list` always shows each entry's size**, no flag. Measured rather than
    assumed: a realistic bundle-sized clone (5.7 MB, ~1,000 files) walks in about
    10 ms, and sizing each entry concurrently keeps a whole cache at roughly the
    cost of one. That's what makes `list` an answer to "why is my disk full"
    rather than merely informative.
- **Restrict Nested Git Includes**: **`ratect`-only** — `ratect-compat` must keep Batect's own unrestricted behavior for parity (its `ConfigurationLoader`/`IncludeResolver` have the identical gap: any file, root or reached transitively through a Git include, can declare a further `type: git` include with no restriction on remote). Currently a nested include gets the exact same trust as one the project owner declared themselves — no allowlist, and (post-0.10.0's `container_git_boundaries` fix) a rogue nested include's own containers are at least bounded to its clone directory or the project directory, but the include mechanism itself will still fetch from whatever remote a third-party bundle names. Worth an opt-in gate for `ratect` (e.g. `allow_nested_git_includes`, defaulting `false`) requiring the project owner to consciously accept that a Git-included bundle may itself redirect the process to further remotes. Relatedly worth reconsidering alongside it: whether a nested (non-root-declared) include's clone/checkout failure should keep surfacing git's raw stderr, since the specific transport error (host unreachable vs. connection refused vs. repository-not-found vs. auth-failed) lets repeated attempts fingerprint an internal network — most relevant when `ratect` runs in CI against a bundle whose nested includes a less-trusted contributor can influence, and whose CI logs are visible back to them. Deferred rather than implemented immediately: real projects (including ones outside this one) depend on nested git includes working by default today, and `ratect-compat` has to default this open regardless — squarely a `ratect`-only divergence, not a blocking gap.
- **Wildcard Includes**: Support for including multiple files using glob patterns (e.g., `include: containers/*.yaml`).
- **Configuration Merging/Replacement**: Ability to merge or override containers and tasks when including files.
- **Init Containers**: Support for containers that must start, run, and complete before other containers can start (e.g., for database initialization).
- **External Health Checks**: Support for external health checks (e.g., HTTP) that don't require specialized tools like `curl` to be installed within the container.
- **Image Lifecycle Management**: Tools for building and pushing images independently of task execution, and cleaning up unused images.
- **`ulimit` Support**: Support for setting `ulimit` values for containers.
- **Secrets Management**: Integrated support for securely handling sensitive information like API keys and credentials.
- **Plugin System**: A flexible architecture to allow users to extend Ratect's functionality with custom logic.
