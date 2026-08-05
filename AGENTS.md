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
  - **`ratect-core/src/config.rs`**: Data models for the configuration — **two text
    formats, one model**. Every entry point comes in a pair: `load_project`/
    `load_from_file` (Batect-compatible, YAML via `noyalib`) and their `_native`
    siblings (`ratect`'s `ratect.toml`, TOML via `toml`, with `.yml`/`.yaml`
    includes still parsed as YAML *by extension*). A binary picks a format by
    *which function it calls*, so the private `ConfigFormat` policy enum never
    leaks into the public API; `parse_config_file` is the single dispatch point,
    and both parsers feed the same `ConfigFile`, so nothing downstream knows which
    format a file came from. Three things ride on that policy — the native-only
    `extends` pass, the `ratect-bundle.toml`-before-`batect-bundle.yml` probe for a
    pathless git include, and the object-only *documented* schema (the parser
    itself stays string-tolerant, which is what lets one set of hand-written
    `Deserialize` impls serve both formats). **`extends`** (native only; a
    `batect.yml` using it is *rejected*, not ignored) is a final pass *after*
    expression/path resolution — mechanically `child.or(parent)` over the
    already-`Option` fields, so a set field replaces and an unset one inherits,
    single-parent, transitive, cycle-checked. `inherit_container_fields`
    destructures the parent exhaustively on purpose: a new `Container` field that
    forgets to inherit is a compile error, not a silent gap. Ordering is
    load-bearing — resolve *then* extend, so an inherited relative path stays
    anchored to the *parent's* own file rather than re-anchoring to the child's.
    See [decisions/0003](decisions/0003-ratect-native-config-format.md).
    Two Batect behaviours worth knowing, both found by running real-world bundles
    and both applying to *either* format's YAML: a top-level key starting with `.`
    is an **extension** (it exists only to hold a YAML anchor and is stripped
    before the schema sees it — which is why YAML is deserialized in two steps,
    text → `noyalib::Value` → `ConfigFile`, since anchors must resolve *before*
    the key is dropped), and a **leading `~`** in a host path expands to the home
    directory (component-wise, matching Batect's `PathResolver.resolveHomeDir`, so
    `~user/…` stays literal). `task_names_for_completion` is a deliberate
    *non*-load for shell completion: names only, follows local and
    already-cached-git includes, never clones or errors.
    `Config::load_from_file` parses the root file and resolves
    `include` (local files and Git bundles — see
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
    going through `load_from_file`). `load_project` (0.2.0-dev) wraps that whole
    sequence — existence check, `load_from_file`, `base_path_for`,
    `project_directory_path`, `resolve_expressions` — into the one call a binary
    actually wants, returning a `LoadedProject`; it exists so `ratect` and
    `ratect-compat` can't get the ordering (includes before expressions) or the
    missing-file error wording out of step with each other. Merging
    `--config-vars-file` with individually-supplied variables stays the caller's
    job — only the caller knows what its own flags are called.
    `run_as_current_user.home_directory` is
    interpolated but *not* resolved against a base path — it's a container-side path,
    validated to start with `/` instead. `PortRange`/`PortMapping`,
    `DeviceMapping` (`devices`), and `VolumeMount` (`volumes` — `Local`/`Cache`
    variants, 0.18.0, plus `Tmpfs`, 0.21.0) all have hand-written `Deserialize`
    impls so an entry can be either Batect's string form (`"local:container[/protocol]"` /
    `"local:container[:options]"` — `VolumeMount`'s string form is always
    `Local`; there's no compact string form for `Cache`/`Tmpfs`) or the expanded
    object form. A `VolumeMount::Local`'s host path is resolved here (against
    `container_base_paths`, same as `build_directory`); a `Cache`'s `name`/
    `container` are plain strings, not `Expression`s, matching Batect — nothing
    to resolve here at all, since `--cache-type` and the project's own cache
    key (needed to actually resolve one) aren't known until `engine.rs`/
    `cache.rs`. A `Tmpfs`'s `container`/`options` are likewise plain strings —
    nothing to resolve here either, matching Batect's own `TmpfsMount` typing.
    `Capability`
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
    try/catch Batect's own cleanup task has. `cached_working_copy` (0.3.0) is
    the read-only counterpart to `ensure_cached`: it computes the same
    `~/.ratect/incl` path and returns it only if the clone already exists —
    never cloning, locking, or touching the network — for offline callers
    (`config::task_names_for_completion`) that must not stall a shell `<TAB>`.
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
    worth knowing before touching it: `run_container`'s three actual start/attach
    paths (`run_container_interactively`/`run_container_forwarding_stdin`/
    `start_and_stream_logs`) each call Docker's own `start` at a different point
    relative to attaching (the TTY path attaches *before* starting, deliberately, so
    no early output is missed) — `run_container`'s own `started` parameter (0.21.0)
    is threaded into all three so each can signal it right after its own `start`
    call succeeds, regardless of which path actually ran, letting `engine.rs`'s
    concurrent readiness gate begin at the right moment however the container ends
    up attached; **`run_container` never removes the container it creates** —
    `engine.rs`'s cleanup stage removes everything, the task's own container
    included, so `--no-cleanup-after-success`/`-failure` are read in exactly one
    place (0.25.0, see `engine.rs`'s own note and `run_container`'s doc comment for
    why the split was retired rather than corrected again); the interactive path's `RawModeGuard`
    restores the terminal on `Drop`, even on an error return; since Ratect has no `--output`
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
    default the way Batect's own config model does. `tmpfs` (0.21.0)
    followed the same pattern again, onto bollard's `HostConfig.Tmpfs`
    (`build_tmpfs_mounts`, pure/unit-testable, same shape as `build_devices`/
    `build_log_config`) — unlike `devices`/`log_options`, its `(container_path,
    options)` pairs come from the same `volumes` config field `resolve_volumes`
    already handles, just pulled out separately (`engine.rs`'s `tmpfs_mounts`)
    since a tmpfs mount can't be expressed as a bind string. `build_image` also
    gained a `force_pull: bool` parameter (0.19.0, both the classic and
    BuildKit paths' `BuildImageOptionsBuilder::pull("true")`) — Batect's
    second, distinct use of `image_pull_policy` on a `build_directory`
    container (`engine.rs`'s `resolve_image` computes it from
    `container_config.image_pull_policy == Always`, since `docker.rs` still
    doesn't depend on `config` types directly).
    `classify_ssh_agent_paths` (0.25.0) turns one `build_ssh` entry's already
    resolved `paths` into a `SshAgentSource` (`HostAgent`/`Socket`/`Keys`) by
    the same rules Go BuildKit's own `sshprovider` uses — no paths means the
    host's `SSH_AUTH_SOCK`, a path that *is* a Unix socket forwards that agent
    and has to be the entry's only path, and anything else is a private key
    file. Deliberately a `stat` rather than a config-schema rule, matching
    where BuildKit draws the line: a nonexistent path is *not* a socket, so it
    goes down the key-file route and fails naming the file instead of failing
    to connect to something that was never there. `Keys` is what makes
    `build_image_via_buildkit` start a `crate::ssh_agent::Keyring` and hand
    bollard its socket — those keyrings are held in a local `Vec` and
    explicitly dropped *after* the build stream is drained, since dropping one
    stops the agent that every `RUN --mount=type=ssh` in the build depends on.
    `ensure_host_volume_directories_exist` (the `run_as_current_user` host-dir
    pre-creation step) only `mkdir -p`s a bind's source when it's *absolute*
    (a bare non-absolute source is a `CacheType::Volume` name, 0.18.0, not a
    host path), *doesn't already exist* (an existing directory Docker reuses,
    or a single file/socket the config bind-mounts like `~/.gitconfig` or an
    SSH agent socket — `mkdir` over a non-directory both fails and is wrong),
    and *isn't a special Docker Desktop path* (`/run/host-services`/
    `/run/guest-services`, which Docker injects into its own VM and don't
    exist on a macOS host — the SSH agent socket lives at
    `/run/host-services/ssh-auth.sock`). The latter two guards are a
    straight port of Batect's `!Files.exists && !isSpecialDockerDesktopPath`;
    0.10.0's `52e8d59` dropped the exists-check as "TOCTOU-prone" and the
    special-path one was never ported, which together broke mounting the SSH
    agent under `run_as_current_user` — don't re-simplify them away. `list_volumes`/`remove_volume` (0.18.0, `--clean`/
    `--clean-cache`) are thin wrappers over bollard's own volume API — see
    `cache.rs` for the actual removal-decision logic built on top of them.
    `list_containers`/`list_networks` (0.2.0-dev) are the equivalent pair for
    finding what a previous run left behind, both returning the same
    `LabelledResource` (a container and a network want reporting identically,
    so the reporting code isn't written twice) and both filtering *daemon-side*
    via `label_filters` — Docker ANDs the values under one `label` filter name,
    which is what makes "this project *and* this run" mean both rather than
    either. `list_containers` passes `all: true` deliberately: a leftover has
    usually exited, and Docker's default lists only running containers.
  - **`ratect-core/src/user.rs`**: Host user lookup (`current_user`, via the `nix`
    crate — Unix-only) and the pure `/etc/passwd`/`/etc/shadow`/`/etc/group` content
    generators `docker.rs` uses — ported from Batect's
    `RunAsCurrentUserConfigurationProvider`, including its `uid == 0`/`gid == 0`
    special-casing so running as the current user doesn't produce a duplicate
    conflicting `root` entry.
  - **`ratect-core/src/ssh_agent.rs`** (0.25.0): a minimal in-process ssh-agent
    (`Keyring`), serving a `build_ssh` entry's `paths` private keys over a Unix
    socket in a `0700` temporary directory — which is what lets `paths` work
    with no agent running on the host at all, the normal CI case. BuildKit's
    `sshforward` bridges each forwarded stream to *anything* speaking the agent
    protocol, so serving it ourselves is indistinguishable from forwarding a
    real agent (exactly what Go BuildKit's own `sshprovider` does with
    `x/crypto`'s `agent.NewKeyring`). Wire formats come from
    [RFC 9987](https://www.rfc-editor.org/rfc/rfc9987); only
    `REQUEST_IDENTITIES` and `SIGN_REQUEST` get real answers and everything
    else returns `SSH_AGENT_FAILURE`, which is all an SSH *client* doing
    public-key auth needs. Things to preserve when touching it, all from
    [decisions/0005](decisions/0005-build-ssh-keyring-placement.md): it stays
    **extractable** — no config/engine/Docker types, and no error message
    naming Ratect, so it could be lifted out or offered upstream to `bollard`
    as a copy rather than a rewrite; private keys never cross the socket, only
    signatures; and the socket's directory is both unpredictably named and
    `0700`, since the system temporary directory is world-writable and the
    socket grants signing to anything that can reach it. Two non-obvious
    details: **`ssh-key` 0.6.7's own RSA conversion is broken** (it passes the
    prime `p` twice instead of `p` and `q`, so *no* RSA key can be signed with
    through its `Signer` impl either) — `rsa_private_key` rebuilds the key from
    components to work around it, and the workaround goes when `ssh-key` 0.7 is
    published and adopted; and the socket path length is checked before binding,
    because `sun_path` is only 104 bytes on macOS and its per-user `TMPDIR`
    already spends about half of that. Its tests build every request from RFC
    9987's own numbers rather than from this module's constants — a test that
    derives its input from the code can't validate a constant that crosses a
    protocol boundary. `docker.rs`'s `classify_ssh_agent_paths` decides *when*
    a keyring is needed (see below); this module knows nothing about
    `build_ssh`.
  - **`ratect-core/src/proxy.rs`**: Proxy environment variable detection/propagation
    (`--no-proxy-vars` to disable) — ported from Batect's
    `ProxyEnvironmentVariablesProvider`/`ProxyEnvironmentVariablePreprocessor`.
    Rewrites `localhost`/`127.0.0.1`/`::1` proxy URLs to `host.docker.internal`
    (macOS/Windows only — `None` on Linux).
  - **`ratect-core/src/interrupt.rs`** (0.25.0): Ctrl+C tracking, so an
    interrupted run still cleans up instead of leaving its containers and
    network behind — a port of Batect's `InterruptionTrap`, whose
    `UserInterruptedExecutionEvent` is a `TaskFailedEvent`, which is why an
    interrupt takes the *ordinary failure* path here too (and so
    `--no-cleanup-after-failure` suppresses cleanup for it, exactly as Batect's
    `TaskStateMachine` does). Deliberately only the signal half: it *counts*
    interrupts and lets callers await one, and the engine decides what that
    means. Counting rather than latching is load-bearing — the count is the
    only thing distinguishing an interrupt that lands *during cleanup* ("stop
    cleaning up, now") from the one that started it, and cleanup is slow
    enough to need that answer. Note the engine's rule is *relative*: it
    compares against the count when cleanup started, not a fixed `>= 2`.
    That matters because arming the handler replaces the process's default
    `SIGINT` behaviour for the whole run, so any interrupt the engine doesn't
    act on is one it has silently swallowed — and a fixed threshold swallows
    the first Ctrl+C during the cleanup of a run that was never interrupted,
    which is the common case rather than an exotic one. Two things to
    preserve when touching it: `wait_for`'s `notified()`-before-check ordering
    (`Notify::notify_waiters` wakes only *existing* waiters, so checking first
    would let an interrupt land in the gap and hang the caller forever — it has
    its own regression test), and the fact that `listen` spawns, so it can only
    be called from inside a runtime — which is why both binaries arm it from
    their async path rather than in the synchronous `engine_settings` their
    flag-mapping tests call directly. Interactive tasks are deliberately
    untouched: a raw-mode terminal doesn't turn Ctrl+C into a signal at all,
    forwarding `0x03` to the container instead, matching `docker run -it`.
  - **`ratect-core/src/engine.rs`**: The core execution logic — task lifecycle,
    prerequisites, dependency-cycle detection, sidecar/dependency container resolution
    (see [`docs/task-lifecycle.md`](docs/task-lifecycle.md)), and once-per-session
    dedup of image pulls/builds/task runs. `TaskEngine` is generic over
    `ContainerRuntime`. Worth knowing: opt-in settings (`existing_network`,
    `publish_ports`, etc.) are builder methods rather than `TaskEngine::new`
    parameters, so each new one lands without a mass-edit of the ~30 existing call
    sites — with `TaskEngineSettings`/`with_settings` (0.2.0-dev) as the
    plain-data form of that same set, which is what the *binaries* use (both
    expose the same ~10 knobs behind differently-named flags, so neither
    duplicates the builder chain; a new setting needs adding in both places or a
    binary can't reach it, and this module's own tests keep using the builders,
    where naming one setting reads better than a mostly-default struct); and only
    the task actually named on the command line (never a
    prerequisite) is ever eligible for interactive-TTY mode. `run_task_internal`
    runs `prerequisites` first, then returns early (no error) if the task itself has
    no `run` (0.14.0) — everything after can assume `run` is present. `customise`
    threads through `start_dependency`'s own recursion unconditionally, so it
    reaches its target regardless of depth in the dependency graph. The task's own
    container goes through the same readiness gate a dependency always has too
    (0.21.0, `run_task_container_readiness`) — health-check wait, then
    `setup_commands`, in order — but run *concurrently* with
    `ContainerRuntime::run_container`'s own attach-and-wait-for-exit via
    `tokio::join!` (the engine's first concurrent-exec path), rather than gating
    anything on it, since nothing else in the graph depends on the task container's
    own readiness. `run_container` takes two `oneshot::Sender`s for this:
    `created` (the container's id, sent the moment `create_container` returns —
    *before* it's started, matching Batect's own `containersCreated` set, which is
    what its `CleanupStagePlanner` plans removals from) and `started` (a bare `()`,
    right after Docker's own `start` call, which is when the readiness gate may
    begin — both its health inspect and its `docker exec` need a *running*
    container). `created` firing that early is what lets `run_container` `?` freely
    on every subsequent line: from that instant the engine can remove the
    container, so no failure can strand it. This replaced (0.25.0) a scheme where
    `run_container` removed its own container and took a third `readiness` channel
    purely to order that removal after the gate; the cleanup flags then had to be
    interpreted identically in two modules, and keeping them in step by hand
    produced a distinct bug in each of three consecutive review rounds. Don't
    reintroduce a removal here. See [task
    lifecycle](docs/task-lifecycle.md#known-simplifications-relative-to-batect) for
    the one race this still shares with Batect (a near-instant main command with no
    `health_check` can still race past a `setup_commands` entry's own `docker exec`)
    and the one deliberate divergence (the main command is never cancelled early
    just because the readiness gate fails first, unlike Batect's own coroutine
    cancellation).
    `resolve_volumes` (0.18.0) turns a container's `VolumeMount`s into the
    literal bind strings `docker.rs` expects — a `Local` mount's already fully
    resolved by `config.rs`, nothing left to do but reassemble the string; a
    `Cache` mount goes through `cache::resolve_cache_mount`, memoizing the
    project's own cache key in a `tokio::sync::OnceCell` field (computed at
    most once per invocation, and only if a `cache` mount is actually
    resolved — never eagerly). `with_cache_options` (`--cache-type` + the
    project directory) is `main.rs`'s own builder call, always made in
    practice despite being optional here, same convention as the other opt-in
    settings above. `Tmpfs` mounts are deliberately *not* resolved by
    `resolve_volumes` at all (0.21.0) — a tmpfs mount can't be expressed as a
    bind string, and needs no async cache-key lookup either, so a separate,
    synchronous `tmpfs_mounts` helper (alongside `capability_names`/
    `device_triples`) pulls them out into a new `ContainerOptions.tmpfs` field
    instead, mapped onto Docker's own `HostConfig.Tmpfs` map by `docker.rs`'s
    `build_tmpfs_mounts`.
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
    that method, not a new engine/docker setting. That raw-streaming half is
    also why `simple.rs` *drops* the task container's own
    `ContainerBecameHealthy`/`RunningSetupCommand`/`SetupCommandsCompleted`
    lines (0.21.0, `is_task_container` — the same three `if (container ==
    taskContainer) return` guards Batect's own `SimpleEventLogger` has): since
    the task container's readiness gate now runs *concurrently* with its
    command (`engine.rs`), printing them would drop a line into the middle of
    that command's unframed output. `all` mode has no such collision (prefixed,
    line-buffered) and reports them for every container, matching Batect's own
    `InterleavedEventLogger`; `fancy` never sees them, since its block freezes
    the moment the task container starts. Any *new* milestone event for the
    task's own container needs the same decision made for it. Style selection and logger
    construction (including the explicit-`fancy`-without-an-interactive-console
    error) live in `ui::create_event_sink`, not `main.rs` — deliberately, so the
    `ratect` binary (see `ROADMAP.md`'s two-binaries section) gets this for free
    once it needs it, instead of reimplementing the style→logger match itself;
    `ratect-compat/src/main.rs` only gathers the terminal facts once and hands them
    to it (and to `select_output_style`, for `--list-tasks`'s own quiet-format
    decision).
  - **`ratect-core/src/labels.rs`** (0.2.0-dev): the `eu.orican.ratect.*` Docker
    labels stamped on every container and network the engine creates, so the
    planned `resources` verb can find what a previous run left behind (see
    `ROADMAP.md`). `RunLabels` is built once per task execution in
    `engine.rs`'s `run_task_internal` and threaded down through
    `ensure_container_ready`, so a task's containers and its network all agree
    on one run id — generated there rather than alongside the network,
    deliberately, since `--use-network` creates no network to take it from.
    Two things to preserve when touching it: Ratect's own keys must win over a
    container's configured `labels` on an exact collision (they're load-bearing
    for cleanup — a config setting `eu.orican.ratect.run` would otherwise make
    its own containers unfindable), and the version label comes from the
    *binary* (`TaskEngineSettings::ratect_version`, `env!("CARGO_PKG_VERSION")`
    at each `main.rs`) rather than `ratect-core`'s own version, which isn't
    what a user sees from `--version`. Not OCI annotations, deliberately — see
    the module's own doc comment.
  - **`ratect-core/src/schema.rs`** (0.21.0, behind the non-default `schema`
    feature): generates **two** JSON schemas from `config.rs`'s own types —
    `batect.yml`'s (`config_file_schema`, committed at
    `schema/batect-config.schema.json`) and, since 0.3.0, `ratect.toml`'s
    (`native_config_file_schema`, at `schema/ratect-config.schema.json`) — see
    [config reference](docs/config-reference.md#editor-autocompletion-and-validation)
    and the [`ratect.toml` reference](docs/ratect-config-reference.md#editor-support)
    for the user-facing halves. The native one is the same generated base put
    through `make_native`, which applies *exactly* the two differences that define
    the format: it drops the compact string form from the
    `volumes`/`ports`/`devices`/`include` `oneOf`s (object-only), and adds the
    native-only `extends` field the compat schema skips. Everything else is shared
    because both formats parse into one `Config`. One asymmetry that's deliberate:
    only the compat schema carries a `patternProperties` entry admitting top-level
    `.`-prefixed keys (YAML extensions — TOML has no anchors for one to hold). The
    same `RATECT_UPDATE_SCHEMA=1` run regenerates both, and a drift in either fails
    its own test. Things to know before touching it: the schema is
    generated from `ConfigFile` (`pub(crate)` for exactly this reason), not
    `Config` — one *file*'s shape, `include` and all, is what an editor has open,
    not the merged result; it's emitted as draft-07 rather than schemars' own
    default 2020-12, because `yaml-language-server` (what VS Code and JetBrains
    run) only implements draft-07 fully — under 2020-12 it drops keywords sitting
    beside a `$ref`, which is every description on a `$ref`'d field; every type
    with a hand-written `Deserialize` impl needs a hand-written `JsonSchema` impl
    to match, and they live here rather than in `config.rs` (`PortMapping`,
    `PortRange`, `DeviceMapping`, `VolumeMount`, `BuildSecret`, `IncludeEntry` —
    add one here whenever a new string-or-object config type lands, or the derive
    won't compile); and field documentation is the config types' own doc comments,
    run through `summarize` (first paragraph, reflowed, rustdoc link syntax
    stripped) rather than a second `schemars(description = ...)` copy per field,
    which would be free to drift. So a new config field needs a doc comment whose
    *first paragraph* stands alone as user-facing documentation — everything after
    it is for contributors and never reaches the schema. The `schema` feature also
    pulls in `jsonschema` (an optional normal dependency, not a dev-dependency —
    Cargo won't let those be optional) purely for this module's own tests, which
    validate every fixture in the repository against the generated schema.
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

- **`ssh-key`** (`default-features = false`, features `std`/`ed25519`/`rsa`/`p256`/`p384`/`p521`), plus **`ssh-encoding`**, **`signature`** and **`rsa`**: parse OpenSSH private key files and sign with them, for `build_ssh`'s `paths` (`ratect-core/src/ssh_agent.rs`, 0.25.0). Ratect's **only cryptographic dependency**, taken knowingly — [decisions/0005](decisions/0005-build-ssh-keyring-placement.md) chose owning that choice over pushing it onto the `bollard` fork, and it deserves proportionate scrutiny plus an eye on advisories (`cargo audit` already runs in CI). Feature selection is the whole cost control: this is the RustCrypto tree, so it's granular by design (~45 crates), and each flag is a deliberate call — `encryption` is **off**, which alone saves ~16 crates and matches Go BuildKit's own inability to use a passphrase-protected key; `p384`/`p521` cost only two crates on top of `p256`, so all of ECDSA is covered rather than an arbitrary slice of it; `dsa` is off (removed from OpenSSH entirely). `std` is what provides `PrivateKey::read_openssh_file`. The other three add **no** crates — all are already in `ssh-key`'s own tree — and exist only because Rust's strict-deps rule won't let us `use` a transitive dependency's items: `ssh-encoding` for the `Encode`/`Decode` traits, `signature` for the `Signer`/`Verifier` traits (`ssh-key` re-exports neither), and `rsa` to sign under `rsa-sha2-256` and to work around 0.6.7's broken RSA key conversion. **Take SHA-2 from `ssh_key::sha2`, not from `ratect-core`'s own direct `sha2`**: the two are different major versions (0.10 vs 0.11) and only the re-export matches what `rsa` expects. Not `ssh-agent-lib`: it would have added ~5 more crates including a socket-activation tree (`service-binding`/`raunch`) for a use case we don't have, while still leaving the per-algorithm signing to us — and this module has to own socket creation anyway, to make it `0700`.
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
9.  **ROADMAP.md Maintenance**: its `## Batect Parity` headline list and its versioned `### ratect-compat` list follow different edit rules. The headline list is a living summary — freely edit, merge, or delete bullets as scope changes or ships (e.g. "Sidecar Containers" and "Docker Networking" were merged into "Full Docker Networking" once shipped). The versioned list is append-only history — never delete an entry; mark completed scope with `~~strikethrough~~` plus a done-summary of what actually shipped.
10. **User Docs Maintenance**: When a change affects user-visible behavior (CLI flags, config schema, runtime behavior, Batect parity), update the relevant file(s) under `docs/` in the same change — don't let them drift from the code. If you find the code doesn't match what's documented, fix whichever one is wrong rather than leaving the mismatch.
11. **Logging vs. Output**: Use `tracing::{info,warn,error,debug}` for diagnostics and progress (task lifecycle, Docker API breadcrumbs, non-fatal error conditions like a best-effort cleanup failure) — these go to stderr and respect `RUST_LOG`. Reserve `println!`/`print!` for actual command output that the user is asking for (task listing, container log streaming) — this stays on stdout. One deliberate exception: `main.rs`'s single top-level fatal error (the reason the process is about to exit non-zero) is `eprintln!`ed directly, *not* through `tracing::error!` — it must stay visible even under `RUST_LOG=off`, since every output mode (including `-o quiet`, whose whole contract is "only error messages") otherwise has nowhere else to show it. Found and fixed during 0.16.0's output-modes review — don't revert it back to `tracing::error!`.
12. **Commit Messages**: Use the Conventional Commits format (`type: summary`, e.g. `feat:`, `fix:`, `chore:`). Keep the summary concise; add a body only when it clarifies non-obvious motivation, and focus the body on *why* the change was made rather than restating the diff. Every commit is signed off (`git commit -s`) — the [DCO](https://developercertificate.org) attestation CONTRIBUTING.md describes and CI enforces on pull requests; direct commits to `main` follow the same convention for consistency.
13. **Commit Packaging**: a release that's one theme (like most 0.x releases so far) lands as a single `feat:` commit. A release bundling several genuinely separable behaviors (e.g. 0.6.0's networking + proxy work) should instead split into one `feat:` commit per behavior, each with its own tests and doc updates — easier to review and to `git bisect`/`git revert` than one large commit. The version bump and any docs-only release summary stay separate commits either way (see 8).
14. **Architecture Decision Records** ([`decisions/`](decisions/)): the home for a decision's rationale is decided by whether it's **cross-cutting or version-scoped**. A decision referenced from more than one place — the two-binary split, the labels namespace, the native config format — becomes an ADR (`decisions/NNNN-slug.md`, `Status`/`Context`/`Decision`/`Alternatives considered`/`Consequences`), and its ROADMAP.md entry shrinks to a summary plus a `decisions/NNNN` pointer. A decision that belongs to one release stays **inline** in that release's ROADMAP.md entry, using the existing "Scope, settled before building:" / "As built:" subsection pattern — don't extract it. Practical trigger: a decision earns an ADR the moment it's about to be referenced from a *second* place; most never cross that line. ADRs are append-only like the versioned lists — supersede and link forward, never delete. See [`decisions/README.md`](decisions/README.md) for the full convention.
15. **Review before committing, not after.** Run a review pass over the working diff (`/code-review`) *before* each commit rather than over a run of commits afterwards. Adopted after 0.25.0's interrupt work, where a post-hoc review found six issues that all existed at commit time — one of them a behaviour bug, not a slip. Four checks earned their place there, each having actually missed something:
    - **Anchor an inserted item on the preceding item's closing brace, never on the new one's attributes.** A Rust item's doc comment sits *above* its `#[test]`/`#[derive]` attributes, so anchoring an insertion there splices the new item into the previous one's documentation — silently, and the compiler is happy. This is how a new e2e test ended up wearing its neighbour's doc comment.
    - **Re-read every string you added, in its final control-flow position.** Log and error messages are correct when written and quietly become wrong as the code around them moves; nothing type-checks them. Two messages shipped claiming work that a flag had disabled, and naming a `ratect` verb from shared core that `ratect-compat` doesn't have.
    - **Watch for coverage shaped by the test harness rather than the behaviour.** If the fake can only express one ordering of something inherently timing-dependent, the untestable orderings are where the bug will be — extend the harness instead of concluding the cases are covered. Every interrupt test could only pre-record interrupts *before* a run, and the broken case was an interrupt arriving mid-cleanup.
    - **A real-daemon test can mask a missing unit test.** The `#[ignore]`d Docker tests don't run in the default suite, so a path they cover can be entirely unprotected in `cargo test --workspace`. Assert each effect separately — one assertion per thing removed, not one for "cleanup happened".
