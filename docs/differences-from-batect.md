# Differences from Batect

Ratect is a from-scratch Rust implementation inspired by
[Batect](https://github.com/batect/batect) (which is itself no longer maintained — the
upstream repository was archived in October 2023), not a wrapper or fork. It does not
read Batect's documentation or source at runtime, and it does not (yet) support
everything Batect did. This page exists so you don't have to guess which Batect
behavior applies — if a field or flag isn't marked "Supported" below, or isn't in the
[config](config-reference.md)/[CLI](cli-reference.md) reference, assume Ratect doesn't
do it.

The tables below are the itemized detail behind the "Full Configuration Parity" and
"Full CLI Options Parity" entries in [`ROADMAP.md`](../ROADMAP.md) — that file describes
direction, this page describes exact current status per field/flag, verified against
Batect's own reference documentation.

> **A note on unsupported fields**: Ratect's YAML parsing rejects unknown keys — if you
> write a Batect config field that Ratect doesn't understand (e.g. `working_directory`
> on a container), config loading fails with an error naming the field, rather than
> silently ignoring it. This means a config using any not-yet-supported Batect field
> won't load at all until that field is supported, even for fields marked "Not
> supported" in the tables below — there's no partial/best-effort mode.

## Configuration format

### Top-level fields

| Field | Status | Notes |
|---|---|---|
| `project_name` | Supported | |
| `containers` | Supported | See [Container fields](#container-fields) below. |
| `tasks` | Supported | See [Task fields](#task-fields) below. |
| `config_variables` | Supported | Only `default:` — no `description` field (rejected, per the note above; Ratect has no help/usage output to show one in anyway). See [config reference](config-reference.md#configvariable) and [Expressions](#expressions) below. |
| `include` | Supported, more restrictive | Both local **file includes** (splitting one project's config across files) and Git **includes/bundles** (importing shared tasks/containers from a separate Git repository, e.g. a team-wide `bundle.yml`) are supported — see [config reference](config-reference.md#includes). Any other `type` is rejected with a clear "not supported yet" error rather than silently ignored. Two known gaps versus Batect for Git includes: no 30-day cache eviction sweep, and no manual cache-clear CLI subcommand (Ratect has no subcommand structure yet to hang one off of) — `~/.ratect/incl` grows unbounded until removed by hand. **Deliberate divergence**: Ratect enforces that a Git include's `path`, and every `include` it transitively declares, stays within that repository's own clone directory (rejecting an absolute path, a `../..` traversal, or a symlink pointing back out) — see [config reference](config-reference.md#git-includes). Batect has no equivalent containment check (`IncludeResolver`/`PathResolver` both resolve an absolute `path` by discarding the base entirely, matching `java.nio.file.Path.resolve`'s own documented behavior, with no validation afterward beyond existence), so the same bundle that's rejected here would, in Batect, silently pull in an arbitrary file from the host running it. Local (non-Git) file includes remain unrestricted in both, by design, since those are always the project owner's own files. |
| `forbid_telemetry` | N/A | Ratect doesn't collect telemetry, so there's nothing to forbid. |

### Expressions

Not a single field — Batect supports an
[expression syntax](https://github.com/batect/batect.dev/blob/main/docs/reference/config/expressions.md)
(`$VAR`, `${VAR}`, `${VAR:-default}` for host environment variables; `<name`, `<{name}`
for config variables) usable *within* several fields: `environment`, `build_args`,
`build_directory`, `build_secrets.path`, `build_ssh.paths`, and volume local paths.

**Ratect implements this within `environment`, volume local paths, `build_directory`,
`build_args`, and a `build_secrets` entry's `path`** (see
[config reference](config-reference.md#expressions) for the full syntax, precedence,
and error rules, and [Volume path resolution](config-reference.md#volume-path-resolution)
for how an interpolated host path — or `build_directory`/`build_secrets.path` — is then
resolved relative to the config file). Every other field's YAML string value is still
used exactly as written, with no host-side substitution step:

- `build_secrets.environment` (the source environment variable's *name*, not its
  value) and `build_ssh.paths` (unsupported at all — see the `build_ssh` entry below)
  are not expressions, matching Batect's own typing for the former and moot for the
  latter.
- `command`/`entrypoint`/`run.command`/`run.entrypoint` are tokenized into
  literal argv (matching Batect's own tokenizer — see [config
  reference](config-reference.md#taskrun)), with no shell involved at all, so a
  literal `$VAR` in one of these is never expanded by Ratect either — unrelated to,
  and not to be confused with, Batect's own expression syntax, which substitutes
  values from the **host** before the container even starts. `setup_commands` is the
  one remaining field that still runs via `sh -c` inside the container — see the
  [container fields table](#container-fields) below.
- Batect has exactly one implicit built-in variable, `batect.project_directory`
  (the absolute path of the directory containing the config file), and Ratect
  supports it too — resolvable via `<batect.project_directory`/
  `<{batect.project_directory}` without being declared under `config_variables` (in
  fact, declaring or `--config-var`/`--config-vars-file`-overriding that exact name is
  a hard error, since it isn't meant to be overridable) — see
  [config reference](config-reference.md#built-in-config-variable-batectproject_directory).
  No other implicit/built-in variables exist beyond this one.

### Container fields

| Field | Status | Notes |
|---|---|---|
| `image` | Supported | |
| `volumes` | Partially supported | Only the `local:container[:options]` string form — see [config reference](config-reference.md#volume-path-resolution). The local path supports [expressions](#expressions). The expanded map form, [caches](https://github.com/batect/batect.dev/blob/main/docs/reference/config/containers.md#volumes), and tmpfs mounts aren't supported. |
| `dependencies` | Supported | Starts recursively (nested dependencies too), on a network scoped to one task execution — see [the task lifecycle](task-lifecycle.md). Each dependency must become ready (healthy, `setup_commands` completed — see `health_check`/`setup_commands` below) before its dependents start, matching Batect's real readiness gate. Works for dependency containers too, not just a task's own — see `build_directory` below. |
| `build_directory` | Supported (simplified) | Builds an image from `dockerfile` (a path relative to `build_directory`'s own root, defaulting to `Dockerfile` there) — see [config reference](config-reference.md#image-building). A `.dockerignore` at the root is respected, with real Docker's actual matching rules (not `.gitignore`'s — see [`.dockerignore` semantics](config-reference.md#dockerignore-semantics)). No cross-invocation build caching or automatic image cleanup yet. |
| `additional_hostnames` | Supported | Extra network aliases beyond the container's own name — see [config reference](config-reference.md#container). No expression support (matching Batect, which doesn't support it here either). |
| `additional_hosts` | Supported | Extra `/etc/hosts` entries — see [config reference](config-reference.md#container). No expression support. |
| `build_args` | Supported | Values support [expressions](#expressions). |
| `build_target` | Supported | The build stage to stop at, for a multi-stage `FROM ... AS <name>` Dockerfile — Docker's own `--target` mechanism. No expression support (matching Batect's own `String`, not `Expression`, typing for this field). |
| `build_secrets` | Supported | Exposes secrets to the build via BuildKit's secret-mount mechanism, without persisting them into the built image's layers — either `{environment: NAME}` (a host env var, read at build time) or `{path: ...}` (a file on the host; supports [expressions](#expressions)), exactly one required per entry. Switches that build to a BuildKit gRPC session and disables its build cache (BuildKit excludes a secret's value from its cache key, which would otherwise let an unrelated change reuse a cached layer built with a stale secret) — see [config reference](config-reference.md#image-building). |
| `build_ssh` | Supported (simplified) | Forwards an SSH agent from the host for a Dockerfile's `RUN --mount=type=ssh`. **Ratect only supports forwarding the host's running `ssh-agent` (via `SSH_AUTH_SOCK`) under the implicit `default` agent id** — at most one entry, its `id` (if set) must be `"default"`, and its `paths` must be empty. Batect additionally supports multiple named agents and forwarding explicit private key files instead of a running agent (via BuildKit's own `sshprovider.AgentConfig`, `ID`/`Paths` — confirmed by reading Batect's `BuildImageStepRunner.resolveSSHAgent` and its `docker-client`'s `sshAgentsFromRequest`, not assumed from the docs alone); `bollard`, the Docker client this is built on, only exposes a single on/off toggle for the default agent, not either of those. Closing this is designed and tracked in [#1](https://github.com/or1can/ratect/issues/1) — see [config reference](config-reference.md#image-building). |
| `capabilities_to_add` / `capabilities_to_drop` | Supported (extended) | Validated at config-load time against a fixed list — an unknown name is rejected with a clear error rather than reaching Docker's API to fail there. Based on Batect's own `Capability` enum, but not a strict port: Batect's last release predates `BPF`/`CHECKPOINT_RESTORE`/`PERFMON` (added to Docker in 20.10, briefly reverted, permanently supported since — [moby#41563](https://github.com/moby/moby/pull/41563)), so Ratect's list adds all three rather than inheriting that gap. A superset, not a divergence — every config Batect accepts here still parses identically. Container level only, matching Batect. No expression support. |
| `command` | Supported | Only at the container level via the equivalent task-level `run.command` — see [Task fields](#task-fields). |
| `devices` | Supported | Both of Batect's forms — `"local:container[:options]"` string and `{local, container, options}` object — see [config reference](config-reference.md#container). No path resolution or expression support, matching Batect (unlike `volumes`' host path). `options` defaults to `"rwm"` when omitted, matching the `docker` CLI's own client-side default — Docker's raw API has none, and omitting it entirely makes `runc` fail outright. Container level only, matching Batect. |
| `dockerfile` | Supported | A path relative to `build_directory`'s own root, defaulting to `Dockerfile` there. No expression support (matching Batect's own `String`, not `Expression`, typing for this field). |
| `enable_init_process` | Supported | Runs Docker's own init process as PID 1 ahead of the actual command. Defaults to `false`, matching Batect. Container level only. No expression support. |
| `entrypoint` | Supported | Overrides the image's own `ENTRYPOINT`. Tokenized into literal argv the same way `command` is — no expression support (matching Batect's own `Command`, not `Expression`, typing for this field). Overridden by the task-level `run.entrypoint` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |
| `environment` | Supported | Values support [expressions](#expressions) (host env vars and config variables). Combines with the equivalent task-level `run.environment` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |
| `health_check` | Supported | Overrides the image's own health check configuration (`command`, `interval`, `retries`, `start_period`, `timeout`) — see [Dependency readiness](config-reference.md#dependency-readiness). A dependency with a health check (from config or image) must report healthy before its dependents start. The task's own container's `health_check` is applied (Docker records and runs it) but Ratect never waits on its verdict — a small divergence: Batect's uniform per-container steps mean a task container reporting *unhealthy* can fail the task even as its command runs; in Ratect the task's own exit code alone decides. |
| `image_pull_policy` | Supported (`image` only) | `IfNotPresent` (the default, matching Batect) skips the pull entirely when the image already exists locally; `Always` never checks, matching Ratect's own pre-0.13.0 behavior. Batect additionally uses this same field on a `build_directory` container to force-pull the build's base image (`docker build --pull`) — Ratect doesn't implement that second use, only the `image`-container pull decision. |
| `labels` | Supported | Docker labels applied to the container. Container level only, matching Batect (no equivalent task-level `run` override in either). No expression support. |
| `log_driver` / `log_options` | Not supported | |
| `ports` | Supported | Both the `local:container[/protocol]` string form (including port ranges) and the expanded `{local, container, protocol}` object form — see [Port mappings](config-reference.md#port-mappings). Validated (matching ranges, positive ports) at config-load time. |
| `privileged` | Supported | Runs the container with extended (nearly all host) privileges. Defaults to `false`, matching Batect. Container level only. No expression support. |
| `run_as_current_user` | Supported | Runs the container as the host user's UID/GID instead of root, so files written to mounted volumes aren't root-owned — see [User mapping](config-reference.md#user-mapping). No equivalent to Batect's "cache mounts" (Ratect has no such config concept), and host-side uid/gid lookup is Unix-only. |
| `setup_commands` | Supported (simplified) | Run inside a started dependency after it becomes healthy, before its dependents start — see [Dependency readiness](config-reference.md#dependency-readiness). A `working_directory`-less entry falls back to the container's own `working_directory`, then the image's own default, matching Batect. One remaining gap: the task's *own* container's `setup_commands` don't run at all (Batect runs them concurrently with the task's command; Ratect's sequential engine has no concurrent exec path yet). |
| `shm_size` | Supported | Accepts Batect's own size-string format (`"128m"`, etc. — see [config reference](config-reference.md#container)) or a plain YAML integer (also bytes). Container level only, matching Batect. No expression support. |
| `working_directory` | Supported | Overrides the image's own `WORKDIR`. No expression support (matching Batect's own `String`, not `Expression`, typing for this field). Overridden by the task-level `run.working_directory` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |

### Task fields

| Field | Status | Notes |
|---|---|---|
| `run` | Supported | A task with only `prerequisites` and no `run` is valid, matching Batect — see [config reference](config-reference.md#task). |
| `prerequisites` | Supported | No wildcard (`*`) matching — each name must be listed explicitly. |
| `dependencies` (task-level sidecars) | Supported | Distinct from the container-level `dependencies` field above — scoped to this task specifically, unioned with the task's own container's `dependencies` — see [config reference](config-reference.md#task). |
| `description` | Supported | Shown next to the task's name in `--list-tasks` output — see [config reference](config-reference.md#list-tasks-output). |
| `group` | Supported | Groups tasks under a heading in `--list-tasks` output, only once *some* task in the project declares one — see [config reference](config-reference.md#list-tasks-output). |
| `customise` | Supported | Per-task `environment`/`ports`/`working_directory` overrides for a non-main container in the task's own graph — see [config reference](config-reference.md#taskcontainercustomisation). |

### `run` fields

| Field | Status | Notes |
|---|---|---|
| `container` | Supported | |
| `command` | Supported | |
| `entrypoint` | Supported | Overrides the container's own `entrypoint` for this task's run specifically — see [config reference](config-reference.md#taskrun). Tokenized the same way. No expression support. |
| `environment` | Supported | Values support [expressions](#expressions). Overrides the container's own `environment` on a key collision — see [config reference](config-reference.md#taskrun). |
| `ports` | Supported | Additional port mappings for this task's run, added to the container's own `ports` as a union — see [config reference](config-reference.md#port-mappings). |
| `working_directory` | Supported | Overrides the container's own `working_directory` for this task's run specifically — see [config reference](config-reference.md#taskrun). No expression support. |

## CLI flags

Batect's full flag list, from its [CLI reference](https://github.com/batect/batect.dev/blob/main/docs/reference/cli.mdx):

| Flag | Status | Notes |
|---|---|---|
| `--config-file` / `-f` | Supported | |
| `--list-tasks` / `-T` | Supported | Grouping and descriptions supported (see [task fields](#task-fields)); no `--output=quiet`-style machine-parsable variant. |
| `--help` / `-h` | Supported | Auto-generated by `clap`. |
| `--version` | Supported | Auto-generated by `clap` (also gets a `-V` short form Batect doesn't have). |
| `<task-name> -- <args>` | Supported | Appended as literal argv entries after the task's own tokenized `command` — matching Batect's own mechanism exactly. See [CLI reference](cli-reference.md#using-additional_args-in-a-task-command). |
| `--skip-prerequisites` | Not supported | Prerequisites always run. |
| `--override-image` | Not supported | |
| `--output` / `-o` | Not supported | Ratect has one output mode; see [how it works](how-it-works.md#5-logging-vs-output) for the stdout/stderr split it uses instead. |
| `--no-color` | Not supported | Ratect currently has no colored output to disable. |
| `--no-cleanup`, `--no-cleanup-after-failure`, `--no-cleanup-after-success` | Not supported | Ratect always attempts to remove containers after running; there's no way to leave them for debugging. |
| `--disable-ports` | Supported | Disables publishing of any container's `ports` to the host, regardless of config. |
| `--use-network` | Supported | Reuses an existing Docker network for every task in the invocation instead of creating a fresh one per task; never removed at cleanup, since Ratect didn't create it. See [task lifecycle](task-lifecycle.md). |
| `--enable-buildkit` | Partially supported | The *behavior* matches Batect fully — images build with the builder the daemon advertises as its default (BuildKit on any modern daemon), and the `DOCKER_BUILDKIT` environment variable forces it on/off (`1`/`true`/`0`/`false`), which is exactly what Batect reads as this flag's default value. Only the flag itself doesn't exist yet — see [config reference](config-reference.md#image-building). |
| `--tag-image` | Not supported | Built images are tagged `<project_name>-<container_name>` (like Batect's own default) — no way to additionally tag one with a custom name. |
| `--config-vars-file`, `--config-var` | Supported | See [CLI reference](cli-reference.md) and [Expressions](#expressions). |
| `--docker-host`, `--docker-context`, `--docker-config`, `--docker-cert-path`, `--docker-tls*` | Not supported | Ratect connects using Docker's local defaults only, with no CLI overrides. |
| `--cache-type`, `--clean`, `--clean-cache` | N/A | Moot — no cache concept exists (Batect's caches are for build performance, not implemented here). |
| `--max-parallelism` | Not supported | Ratect's own within-task container concurrency (0.15.0) is unbounded, same as Batect's default when this flag isn't passed — there's just no CLI surface yet to cap it. |
| `--no-proxy-vars` | Supported | Disables proxy environment variable propagation entirely — see [Proxy environment variables](config-reference.md#proxy-environment-variables). |
| `--log-file` | Different mechanism | Ratect uses `RUST_LOG` + stderr instead of a dedicated log-file flag — see [CLI reference](cli-reference.md#environment-variables). |
| `--no-update-notification`, `--upgrade`, `--no-wrapper-cache-cleanup` | N/A | Moot — Ratect isn't distributed via a self-updating wrapper script. |

## Runtime behavior gaps

Batect behavior not implemented in task execution, beyond what's covered by the field
tables above:

- **Interactive mode**: supported for the invoked task's own container (never a
  prerequisite's, a dependency's, or a sidecar's) — see
  [Interactive mode](config-reference.md#interactive-mode). A real Docker TTY (raw mode,
  live terminal-resize forwarding) is only allocated when both Ratect's own stdin and
  stdout are real terminals; stdin forwarding and the host's `TERM` propagation are
  **not** gated on that — both apply whenever the invoked task's own container is
  eligible, matching Batect's own `attachStdinForContainer`/`stdinForContainer` and
  `ConsoleInfo.terminalType`/`terminalTypeForContainer`, all four confirmed (by reading
  Batect's own source) to be unconditional on any TTY check. One known, deliberate
  divergence remains: Batect's real-TTY gate (`useTTYForContainer`) checks only whether
  its output is a real terminal; Ratect's (`should_use_tty`) still requires *both* stdin
  and stdout to be real terminals — not changed as part of closing the other three gaps.
- **Proxy support**: `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` are detected from
  the host environment and propagated into containers and builds automatically — see
  [Proxy environment variables](config-reference.md#proxy-environment-variables). The
  `localhost`-rewriting half of this only works on macOS/Windows (no automatic
  Docker-reachable hostname on Linux), and there's no Docker-version-gated hostname
  fallback chain the way Batect has for very old Docker installs — both accepted gaps,
  not worth chasing for any actively-maintained Docker daemon.

## What Ratect *does* support today

For the positive list — what's actually implemented and working — see:

- [Getting started](getting-started.md) for a walkthrough
- [Configuration reference](config-reference.md) for the supported schema
- [CLI reference](cli-reference.md) for the supported flags
- [How it works](how-it-works.md) for the execution model (prerequisites, dependency
  cycle detection, once-per-run dedup of tasks and image pulls, and — as of 0.15.0 —
  concurrent startup of independent branches of one task's own dependency graph)
