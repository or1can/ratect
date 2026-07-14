# Ratect Roadmap

This document outlines the planned journey for Ratect, from achieving parity with Batect to implementing Rust-specific enhancements and future innovations.

## Batect Parity

The primary goal is to support the core features of Batect to ensure a seamless transition for existing users. This work targets the [`ratect-compat` binary](#two-binaries-ratect-and-ratect-compat) specifically — the `ratect` binary is not expected to maintain 1:1 Batect parity.

- **Image Building**: Building a Docker image from a `Dockerfile` via `build_directory` (always named `Dockerfile`, at `build_directory`'s own root) is implemented, including `build_args` and `.dockerignore` support (0.3.0) — see [config reference](docs/config-reference.md#image-building). Custom Dockerfile naming/location (`dockerfile`), `build_target`, `build_secrets`, `build_ssh`, cross-invocation build caching, and automatic image cleanup are not — see [Differences from Batect](docs/differences-from-batect.md#container-fields).
- **Full Docker Networking**: Every task execution gets its own isolated network (see [the task lifecycle](docs/task-lifecycle.md)), `--use-network` reuses an existing one instead, `additional_hostnames`/`additional_hosts` add extra aliases/`/etc/hosts` entries, and `ports`/`--disable-ports` publish container ports to the host, including port ranges and the expanded object form, plus additional per-task `run.ports` (0.6.0) — see [config reference](docs/config-reference.md#port-mappings) and [CLI reference](docs/cli-reference.md).
- **Interactive Mode**: A task's own container gets a real Docker TTY and its stdin forwarded, automatically, when both Ratect's own stdin and stdout are real terminals (0.4.0) — see [Interactive mode](docs/config-reference.md#interactive-mode). Live terminal-resize forwarding and Batect's decoupled stdin-without-TTY support are not — see [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps).
- **Full Environment Variable Interpolation & Batect Expressions**: `environment` on containers/tasks, `config_variables` (including Batect's one built-in, `batect.project_directory`), and `$VAR`/`${VAR}`/`${VAR:-default}`/`<name`/`<{name}` expressions are implemented for `environment` values, volume host paths, `build_directory`, and `build_args` — every already-supported field that could meaningfully take one; `build_secrets.path`/`build_ssh.paths` remain moot until those fields themselves exist — see [Expressions](docs/differences-from-batect.md#expressions).
- **Includes**: Local file includes — splitting one project's configuration across multiple files via the top-level `include` directive, resolved relative to each declaring file's own directory and merged into one flat `containers`/`tasks`/`config_variables` set (0.7.0) — and Git includes/bundles — importing shared tasks/containers from a separate repository, cloned once and cached forever at `~/.ratect/incl` (0.8.0) — see [config reference](docs/config-reference.md#includes). No cache eviction sweep or manual cache-clear command yet — see [Differences from Batect](docs/differences-from-batect.md#top-level-fields).
- **Full Configuration Parity**: Support for all available Batect configuration options and standard YAML structures. See [Differences from Batect](docs/differences-from-batect.md#configuration-format) for the itemized current status of every field.
- **Full CLI Options Parity**: Support for all standard Batect CLI flags and options (e.g., `--config-file`, `--override-image`, cleanup control flags, etc.). See [Differences from Batect](docs/differences-from-batect.md#cli-flags) for the itemized current status of every flag.
- **User Mapping**: A container can run as the host's own user/group (`run_as_current_user`) instead of the image's default, so files it writes to a mounted volume aren't root-owned (0.5.0) — see [User mapping](docs/config-reference.md#user-mapping). No equivalent to Batect's "cache mounts", and host-side uid/gid lookup is Unix-only — see [Differences from Batect](docs/differences-from-batect.md#container-fields).
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

Mechanically, each `Cargo.toml` (`ratect`'s and `ratect-core`'s) sits at `X.Y.Z-dev`
between releases. Cutting a release is one isolated `chore:` commit that bumps both to
the plain `X.Y.Z` being released and moves `CHANGELOG.md`'s accumulated `Unreleased`
entries under a new dated `## [X.Y.Z]` header. That commit is tagged `vX.Y.Z` and
published as a GitHub Release (`prerelease: true` until a binary's own 1.0.0 — see
below — with that `CHANGELOG.md` section as its body). The next commit — starting the
following version's development, also isolated, also `chore:` — bumps both
`Cargo.toml`s back to the next `X.Y.Z-dev`.
Neither bump is ever folded into a feature commit.

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
  - Known gaps, deferred as follow-on work rather than blocking this release: no
    30-day cache eviction sweep and no manual cache-clear CLI surface (Batect has
    both; Ratect has no subcommand structure yet to hang a cleanup command off of —
    tracked separately, not part of this item) — `~/.ratect/incl` grows unbounded
    until removed by hand.
- **0.9.0** — **Dependency Readiness**: `health_check` and `setup_commands`, replacing
  today's "started = ready" simplification (see
  [Container fields](docs/differences-from-batect.md#container-fields)) with Batect's
  real readiness gate before a container's dependents start.
- **0.10.0** — **Interactive Mode Completeness**: closes the known gaps left by 0.4.0 —
  live terminal-resize forwarding for the rest of an interactive session (not just
  synced once at attach time), decoupling stdin forwarding from TTY allocation (piping
  input into a non-interactive task), and propagating the host's `TERM` into the
  container's environment alongside proxy variables.
- **0.11.0** — **Build Customization**: `build_target`, custom `dockerfile`
  naming/location, `build_secrets`, `build_ssh` — extends 0.3.0's image-building
  support.
- **0.12.0** — **Container Runtime Options**: `entrypoint` (container and `run`),
  `working_directory` (container and `run`), `labels`,
  `capabilities_to_add`/`capabilities_to_drop`, `privileged`, `shm_size`, `devices`,
  `enable_init_process`, `image_pull_policy` — the remaining container/run fields,
  each largely a direct pass-through to the Docker API.
- **0.13.0** — **Task Model Completeness**: task-level `dependencies` (sidecars scoped
  to a task, distinct from the container-level field shipped in 0.6.0),
  `description`/`group` (plus corresponding `--list-tasks` output), and `customise`.
- **0.14.0** — **Parallel Task Execution**: independent prerequisites and tasks run
  concurrently via `tokio`, rather than sequentially — closes the last
  [runtime behavior gap](docs/differences-from-batect.md#runtime-behavior-gaps)
  against Batect and delivers the [Parallel Task Execution](#rust-enhancements) item
  ahead of the rest of that section. The single biggest architectural change in this
  list — it touches the `Mutex`-based shared execution state in
  `ratect-core/src/engine.rs` (see `AGENTS.md`), and dependency-cycle detection has to
  stay correct under concurrency.
- **0.15.0** — **Output Modes**: `--output`/`-o` (Batect's `fancy`/`simple`/`quiet`/
  `all` modes) together with automatic default-mode selection based on terminal
  capabilities — the two can't ship separately, since auto-detection is the logic for
  picking between modes that don't otherwise exist. Also closes `--no-color` (color is
  one axis of the fancy/simple distinction).
- **0.16.0** — **Remaining CLI Parity**: `--skip-prerequisites`, `--override-image`,
  `--no-cleanup`/`--no-cleanup-after-failure`/`--no-cleanup-after-success`,
  `--tag-image`, `--enable-buildkit`, `--docker-host`/`--docker-context`/
  `--docker-config`/`--docker-cert-path`/`--docker-tls*`.
- **1.0.0** — the [Batect Parity](#batect-parity) section above substantially checked
  off (all of the above, including 0.7.0–0.16.0, not just the items shipped through
  0.6.0), and verified against a handful of real Batect projects, not just the
  itemized field/flag tables passing in isolation. Not tagged early for appearances —
  earned once `ratect-compat` can honestly replace `batect` on real projects.

### `ratect`

Hasn't started yet — see [Two Binaries](#two-binaries-ratect-and-ratect-compat). Its
**1.0.0** means something different from `ratect-compat`'s: interface stability (the
subcommand structure and config format won't break), not feature-completeness against
Batect.

## Rust Enhancements

Leveraging Rust's strengths to provide a superior experience compared to the original JVM-based implementation.

- **Parallel Task Execution**: Utilizing `tokio` to execute independent tasks and prerequisites in parallel, significantly reducing execution time. Scheduled as `ratect-compat` [0.14.0](#ratect-compat) rather than left indefinite, since it also closes a Batect parity gap.
- **Static Binaries**: Distribution as zero-dependency static binaries (`ratect` and `ratect-compat`) for easy installation and portability.
- **First-class Cross-platform Support**: Providing a high-performance, native experience across macOS, Linux, and Windows without the overhead or startup latency of a JVM.
- **Precise Error Reporting**: Utilizing Rust's type system and error handling to provide clear, actionable feedback on configuration errors and execution failures.

## UX & Tooling

Improving the developer experience through better tools and feedback.

- **`ratect doctor`**: A built-in linter and diagnostic tool to validate configuration and environment setup. This will include checks for `latest` image tags, missing health checks on dependencies, and host-container permission issues.
- **Automatic Output Mode Detection**: Automatically enabling or disabling color and fancy output based on terminal capabilities and TTY detection.
- **Improved Progress UI**: A more descriptive and visually appealing progress interface for task execution and image management, including build context upload progress.
- **Watch Mode**: Automatically re-running tasks when source files change.

## Future Vision

Exploring innovative features that go beyond the original Batect, as well as planned improvements from the Batect roadmap.

- **Alternative Configuration Format (TOML)**: **Undecided, exploratory.** TOML is a more typical configuration format for Rust projects than YAML. If pursued, this would apply only to the [`ratect` binary](#two-binaries-ratect-and-ratect-compat) — `ratect-compat` stays YAML-only for Batect compatibility — and would need a migration path for projects moving from `ratect-compat`'s YAML config.
- **Wildcard Includes**: Support for including multiple files using glob patterns (e.g., `include: containers/*.yaml`).
- **Configuration Merging/Replacement**: Ability to merge or override containers and tasks when including files.
- **Init Containers**: Support for containers that must start, run, and complete before other containers can start (e.g., for database initialization).
- **External Health Checks**: Support for external health checks (e.g., HTTP) that don't require specialized tools like `curl` to be installed within the container.
- **Image Lifecycle Management**: Tools for building and pushing images independently of task execution, and cleaning up unused images.
- **`ulimit` Support**: Support for setting `ulimit` values for containers.
- **Secrets Management**: Integrated support for securely handling sensitive information like API keys and credentials.
- **Plugin System**: A flexible architecture to allow users to extend Ratect's functionality with custom logic.
