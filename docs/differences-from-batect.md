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
| `config_variables` | Supported | Both `default:` and `description:` — the latter is recognized but inert, since Ratect has no help/usage output to show one in. See [config reference](config-reference.md#configvariable) and [Expressions](#expressions) below. |
| `include` | Supported, more restrictive | Both local **file includes** (splitting one project's config across files) and Git **includes/bundles** (importing shared tasks/containers from a separate Git repository, e.g. a team-wide `bundle.yml`) are supported — see [config reference](config-reference.md#includes). Any other `type` is rejected with a clear "not supported yet" error rather than silently ignored. Git includes get a 30-day cache eviction sweep (0.19.0), matching Batect's own `GitRepositoryCacheCleanupTask` exactly: an unconditional, fire-and-forget background task started on every "run a task" invocation (not `--list-tasks`), deleting any `~/.ratect/incl` entry unused for 30+ days. (Neither Batect nor Ratect has a manual cache-clear CLI command for this — only the automatic sweep.) **Deliberate divergence**: Ratect enforces that a Git include's `path`, and every `include` it transitively declares, stays within that repository's own clone directory (rejecting an absolute path, a `../..` traversal, or a symlink pointing back out) — see [config reference](config-reference.md#git-includes). Batect has no equivalent containment check (`IncludeResolver`/`PathResolver` both resolve an absolute `path` by discarding the base entirely, matching `java.nio.file.Path.resolve`'s own documented behavior, with no validation afterward beyond existence), so the same bundle that's rejected here would, in Batect, silently pull in an arbitrary file from the host running it. Local (non-Git) file includes remain unrestricted in both, by design, since those are always the project owner's own files. A bundle that legitimately needs a path outside both roots (typically a shared cache under your home directory) can be vouched for with `allow_host_paths: true` on the include entry — applying only to that bundle, never to bundles it includes, and honoured only in your own configuration, so a bundle can't grant it to itself; see [config reference](config-reference.md#git-includes). **A second deliberate divergence, `ratect`-only**: in a `ratect.toml`, a Git-included bundle may not declare `type: git` includes of its own unless its include entry sets `allow_nested_git_includes` — Batect places no restriction on that, and neither does `ratect-compat`, where a bundle can redirect the load to any remote exactly as Batect allows. Also `ratect`-only: a *nested* include's clone failure reports that it failed without `git`'s own transport error, which for a remote the bundle named rather than you is a readout on a network; the detail moves behind `RUST_LOG=debug`. Both are native-only because `ratect-compat` has to keep Batect's behaviour for parity — see the [`ratect.toml` reference](ratect-config-reference.md#nested-git-includes). |
| `forbid_telemetry` | Recognized, no effect | Ratect doesn't collect telemetry, so there's nothing to forbid. |

### Expressions

Not a single field — Batect supports an
[expression syntax](https://github.com/batect/batect.dev/blob/main/docs/reference/config/expressions.md)
(`$VAR`, `${VAR}`, `${VAR:-default}` for host environment variables; `<name`, `<{name}`
for config variables) usable *within* several fields: `environment`, `build_args`,
`build_directory`, `build_secrets.path`, `build_ssh.paths`, and volume local paths.

**Ratect implements this within `environment`, volume local paths, `build_directory`,
`build_args`, a `build_secrets` entry's `path`, and a `build_ssh` entry's `paths`** (see
[config reference](config-reference.md#expressions) for the full syntax, precedence,
and error rules, and [Volume path resolution](config-reference.md#volume-path-resolution)
for how an interpolated host path — or `build_directory`/`build_secrets.path`/
`build_ssh.paths` — is then resolved relative to the config file). Every other field's
YAML string value is still used exactly as written, with no host-side substitution step:

- **`image` is the one field where Ratect goes further than Batect, and only in
  `ratect.toml`.** Batect resolves nothing there
  ([batect#974](https://github.com/batect/batect/issues/974) asked for it and was
  never built), so a pipeline can't set a tag per run from config alone. The native
  format resolves it on the same rules as every other field; a `batect.yml` using one
  is **rejected when the file loads** rather than resolved, since a file that worked
  here and failed under `batect` would break the compat binary's whole proposition —
  `--override-image` is the equivalent there. See the [`ratect.toml`
  reference](ratect-config-reference.md#expressions-in-image).

- `build_secrets.environment` is the source environment variable's *name*, not its
  value, so it isn't an expression — matching Batect's own typing for that field.
- `command`/`entrypoint`/`run.command`/`run.entrypoint`/`setup_commands.command`
  are all tokenized into literal argv (matching Batect's own tokenizer — see
  [config reference](config-reference.md#taskrun)), with no shell involved at
  all, so a literal `$VAR` in one of these is never expanded by Ratect either —
  unrelated to, and not to be confused with, Batect's own expression syntax,
  which substitutes values from the **host** before the container even starts.
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
| `image` | Supported | No [expression](#expressions) support in `batect.yml`, matching Batect — one there is rejected when the file loads rather than resolved. `ratect.toml` does resolve them, the one field where Ratect goes beyond Batect; see [Expressions](#expressions) below. |
| `volumes` | Supported | `local` bind mounts (string or expanded object form — see [config reference](config-reference.md#volume-path-resolution)), `cache` volumes (object form only, `--cache-type` selects Docker-named-volume vs. host-directory storage — see [Cache volumes](config-reference.md#cache-volumes)), and `tmpfs` mounts (object form only — see [Tmpfs mounts](config-reference.md#tmpfs-mounts)) are all supported. A `local` mount's host path supports [expressions](#expressions); a `cache` mount's `name` and a `tmpfs` mount's `options` don't, matching Batect's own typing. **Deliberate divergence**: a `cache` mount's `name` must be Docker's own volume-name character set (letters, digits, `_`, `.`, `-`, starting alphanumeric). The name becomes a host directory under `--cache-type=directory`, and Batect validates it not at all, so `name: /etc` or `name: ../../.ssh` gets an arbitrary host directory bind-mounted into the container there — reachable from a Git-included bundle, which is configuration the project owner may not have written. `--cache-type=volume` already enforced this via Docker itself; directory caches accepted any name, so this is a breaking change for them (`name: my cache` loaded before and now fails). Same safer-direction call as the Git-include containment check above. |
| `dependencies` | Supported | Starts recursively (nested dependencies too), on a network scoped to one task execution — see [the task lifecycle](task-lifecycle.md). Each dependency must become ready (healthy, `setup_commands` completed — see `health_check`/`setup_commands` below) before its dependents start, matching Batect's real readiness gate. Works for dependency containers too, not just a task's own — see `build_directory` below. |
| `build_directory` | Supported (simplified) | Builds an image from `dockerfile` (a path relative to `build_directory`'s own root, defaulting to `Dockerfile` there) — see [config reference](config-reference.md#image-building). A `.dockerignore` at the root is respected, with real Docker's actual matching rules (not `.gitignore`'s — see [`.dockerignore` semantics](config-reference.md#dockerignore-semantics)). No cross-invocation build caching or automatic image cleanup yet. |
| `additional_hostnames` | Supported | Extra network aliases beyond the container's own name — see [config reference](config-reference.md#container). No expression support (matching Batect, which doesn't support it here either). |
| `additional_hosts` | Supported | Extra `/etc/hosts` entries — see [config reference](config-reference.md#container). No expression support. |
| `build_args` | Supported | Values support [expressions](#expressions). |
| `build_target` | Supported | The build stage to stop at, for a multi-stage `FROM ... AS <name>` Dockerfile — Docker's own `--target` mechanism. No expression support (matching Batect's own `String`, not `Expression`, typing for this field). |
| `build_secrets` | Supported | Exposes secrets to the build via BuildKit's secret-mount mechanism, without persisting them into the built image's layers — either `{environment: NAME}` (a host env var, read at build time) or `{path: ...}` (a file on the host; supports [expressions](#expressions)), exactly one required per entry. Switches that build to a BuildKit gRPC session and disables its build cache (BuildKit excludes a secret's value from its cache key, which would otherwise let an unrelated change reuse a cached layer built with a stale secret) — see [config reference](config-reference.md#image-building). |
| `build_ssh` | Supported | Makes SSH keys available to a build, for a Dockerfile's `RUN --mount=type=ssh`. Multiple named agents, forwarding the host's running `ssh-agent` (an entry with no `paths`), forwarding another agent by its socket path, and serving explicit private key files with no agent running at all — Batect's full feature set, following BuildKit's own `sshprovider` rules for how `paths` are interpreted. Key files are served by an ssh-agent Ratect runs in-process, so the keys stay in the `ratect` process and only signatures cross into the build. Two deliberate narrowings, both cases Batect inherits from Go's `x/crypto` rather than chooses: a passphrase-protected key is rejected (Go BuildKit can't use one either — its own source carries a `TODO: prompt passphrase?` — and a build has no terminal to prompt on), and RSA keys are signed only under `rsa-sha2-256`/`rsa-sha2-512`, not the legacy SHA-1 `ssh-rsa` that OpenSSH has disabled by default since 8.8 (2021). DSA keys are likewise not supported, having been removed from OpenSSH entirely. See [config reference](config-reference.md#image-building). |
| `capabilities_to_add` / `capabilities_to_drop` | Supported (extended) | Validated at config-load time against a fixed list — an unknown name is rejected with a clear error rather than reaching Docker's API to fail there. Based on Batect's own `Capability` enum, but not a strict port: Batect's last release predates `BPF`/`CHECKPOINT_RESTORE`/`PERFMON` (added to Docker in 20.10, briefly reverted, permanently supported since — [moby#41563](https://github.com/moby/moby/pull/41563)), so Ratect's list adds all three rather than inheriting that gap. A superset, not a divergence — every config Batect accepts here still parses identically. Container level only, matching Batect. No expression support. |
| `command` | Supported | Overrides the image's own default `CMD`. Tokenized into literal argv the same way `entrypoint` is — no expression support (matching Batect's own `Command`, not `Expression`, typing for this field). Applies as-is to a dependency/sidecar container; for a task's own container, overridden by the task-level `run.command`, when set — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). Symmetric with `entrypoint`, but missed when `entrypoint` and the rest of 0.13.0's container runtime options landed — closed afterward, once noticed. |
| `devices` | Supported | Both of Batect's forms — `"local:container[:options]"` string and `{local, container, options}` object — see [config reference](config-reference.md#container). No path resolution or expression support, matching Batect (unlike `volumes`' host path). `options` defaults to `"rwm"` when omitted, matching the `docker` CLI's own client-side default — Docker's raw API has none, and omitting it entirely makes `runc` fail outright. Container level only, matching Batect. |
| `dockerfile` | Supported | A path relative to `build_directory`'s own root, defaulting to `Dockerfile` there. No expression support (matching Batect's own `String`, not `Expression`, typing for this field). |
| `enable_init_process` | Supported | Runs Docker's own init process as PID 1 ahead of the actual command. Defaults to `false`, matching Batect. Container level only. No expression support. |
| `entrypoint` | Supported | Overrides the image's own `ENTRYPOINT`. Tokenized into literal argv the same way `command` is — no expression support (matching Batect's own `Command`, not `Expression`, typing for this field). Overridden by the task-level `run.entrypoint` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |
| `environment` | Supported | Values support [expressions](#expressions) (host env vars and config variables). A non-string scalar value (`8080`, `true`) is coerced to its string form, matching Batect. Combines with the equivalent task-level `run.environment` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |
| `health_check` | Supported | Overrides the image's own health check configuration (`command`, `interval`, `retries`, `start_period`, `timeout`) — see [Dependency readiness](config-reference.md#dependency-readiness). A dependency with a health check (from config or image) must report healthy before its dependents start. The task's own container's `health_check` is waited on too (0.21.0), concurrently with its main command — matching Batect, a task container reporting *unhealthy* fails the task even if its own command already succeeded. |
| `image_pull_policy` | Supported | `IfNotPresent` (the default, matching Batect) skips the pull entirely when the image already exists locally; `Always` never checks, matching Ratect's own pre-0.13.0 behavior. On a `build_directory` container, the same field instead controls whether the build's own base image is force-pulled before building (`docker build --pull`, 0.19.0) — matching Batect's own second, distinct use of this field. |
| `labels` | Supported | Docker labels applied to the container. Container level only, matching Batect (no equivalent task-level `run` override in either). No expression support. |
| `log_driver` / `log_options` | Supported | Docker's logging driver (Docker's `--log-driver`/`--log-opt`) — `None`/absent leaves the daemon's own configured default alone, rather than baking in a literal `"json-file"` default the way Batect's own config model does (immaterial in practice — that's also Docker's own out-of-the-box default). Container level only, matching Batect. No expression support. |
| `ports` | Supported | Both the `local:container[/protocol]` string form (including port ranges) and the expanded `{local, container, protocol}` object form — see [Port mappings](config-reference.md#port-mappings). Validated (matching ranges, positive ports) at config-load time. |
| `privileged` | Supported | Runs the container with extended (nearly all host) privileges. Defaults to `false`, matching Batect. Container level only. No expression support. |
| `run_as_current_user` | Supported | Runs the container as the host user's UID/GID instead of root, so files written to mounted volumes aren't root-owned — see [User mapping](config-reference.md#user-mapping). Host-side uid/gid lookup is Unix-only. |
| `setup_commands` | Supported | Run inside a started dependency after it becomes healthy, before its dependents start — see [Dependency readiness](config-reference.md#dependency-readiness). A `working_directory`-less entry falls back to the container's own `working_directory`, then the image's own default, matching Batect. The task's own container now runs its `setup_commands` too (0.21.0), concurrently with its main command — matching Batect, including that a failure fails the task even if the main command already succeeded, except the main command itself is never cancelled early because of it (see [task lifecycle](task-lifecycle.md#known-simplifications-relative-to-batect) for the one residual race this still shares with Batect). |
| `shm_size` | Supported | Accepts Batect's own size-string format (`"128m"`, etc. — see [config reference](config-reference.md#container)) or a plain YAML integer (also bytes). Container level only, matching Batect. No expression support. |
| `working_directory` | Supported | Overrides the image's own `WORKDIR`. No expression support (matching Batect's own `String`, not `Expression`, typing for this field). Overridden by the task-level `run.working_directory` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |

### Task fields

| Field | Status | Notes |
|---|---|---|
| `run` | Supported | A task with only `prerequisites` and no `run` is valid, matching Batect — see [config reference](config-reference.md#task). |
| `prerequisites` | Supported | Including wildcard (`*`) matching, expanded against every task name at run time — see [config reference](config-reference.md#wildcard-prerequisites). Ported directly from Batect's own `TaskExecutionOrderResolver` (`resolveWildcards`/`toWildcardRegex`): `*` matches zero or more characters, case-sensitive, anchored to the whole name; multiple matches run in alphabetical order; a wildcard matching zero tasks isn't an error. |
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
| `--list-tasks` / `-T` | Supported | Grouping and descriptions supported (see [task fields](#task-fields)); `--output=quiet` switches to Batect's machine-parsable `name<TAB>description` format — see [CLI reference](cli-reference.md#output-styles). |
| `--help` / `-h` | Supported | Auto-generated by `clap`. |
| `--version` | Supported | Auto-generated by `clap` (also gets a `-V` short form Batect doesn't have). |
| `<task-name> -- <args>` | Supported | Appended as literal argv entries after the task's own tokenized `command` — matching Batect's own mechanism exactly. See [CLI reference](cli-reference.md#using-additional_args-in-a-task-command). |
| `--skip-prerequisites` | Supported | Only ever scopes to the task actually named on the command line — a task reached as someone else's prerequisite always runs its own prerequisites regardless. See [CLI reference](cli-reference.md). |
| `--override-image` | Supported | Wholesale replaces the container's `imageSource` (`image` *or* `build_directory`, plus that container's own `image_pull_policy`) with a pull of the override value under the default `IfNotPresent` policy — matching Batect's own `TaskSpecialisedConfigurationFactory` exactly, including its eager "container does not exist" validation. See [CLI reference](cli-reference.md). |
| `--output` / `-o` | Supported | All four styles (`fancy`/`simple`/`quiet`/`all`), with Batect's own auto-selection rule when the flag is unset (fancy-if-interactive, else simple — minus Batect's mintty and legacy `TRAVIS` special cases, deliberately skipped: Windows is untested here anyway, and modern CI doesn't allocate a TTY, so the terminal check already covers it). Two deliberate divergences: an explicit `-o fancy` on a non-interactive console fails up front with a clear error, where Batect accepts it and crashes with an unhandled exception on the first repaint; and `all`'s Ratect-status lines drop Batect's inner `Batect \| ` prefix (`build \| Batect \| Running build...` there is `build \| Running build...` here) — the outer prefix already says whose line it is. See [CLI reference](cli-reference.md#output-styles). |
| `--no-color` | Supported | One deliberate divergence, a superset rather than a gap: Batect rejects `--output=fancy --no-color` at parse time (its console couples color and cursor movement under one flag, so its fancy mode can't run colorless); Ratect's console keeps the two independent, so `-o fancy --no-color` renders colorless fancy — the live repaint stays, bold/color go. Every combination Batect *accepts* behaves the same — including `--no-color` making `simple` the auto-selected default. |
| `--no-cleanup`, `--no-cleanup-after-failure`, `--no-cleanup-after-success` | Supported | Same success/failure split as Batect: a task's own container exiting non-zero is still "success" for cleanup-gating purposes — only a genuine infrastructure failure (build/pull/health-check/setup-command, or anything else before the task's own container gets to run) counts as "failure". One deliberate simplification against Batect: Batect's own `DontCleanup` still stops a started container (just skips removing it and the network); Ratect skips both, leaving every container genuinely running (not just present-but-stopped) for investigation. See [CLI reference](cli-reference.md). |
| `--disable-ports` | Supported | Disables publishing of any container's `ports` to the host, regardless of config. |
| `--use-network` | Supported | Reuses an existing Docker network for every task in the invocation instead of creating a fresh one per task; never removed at cleanup, since Ratect didn't create it. See [task lifecycle](task-lifecycle.md). |
| `--enable-buildkit` | Supported | Forces BuildKit on, taking precedence over the `DOCKER_BUILDKIT` environment variable — matching Batect's own `TristateFlagOption` (whose default value provider *is* that environment variable, so an explicit flag always wins). No `--disable-buildkit` counterpart, matching Batect exactly — forcing the classic builder is only ever done via `DOCKER_BUILDKIT=0`/`false`. See [config reference](config-reference.md#image-building). |
| `--tag-image` | Supported | Additional tags applied to the same image ID once the build completes (both bollard's classic and BuildKit build options only ever accept one `t` value each, unlike Batect's own client, which can request every tag directly as part of the build). Same validation as Batect: errors immediately if the named container ends up using a pulled image, and once the task and its prerequisites finish if the named container never actually ran. See [CLI reference](cli-reference.md). |
| `--config-vars-file`, `--config-var` | Supported | `--config-vars-file` defaults to `batect.local.yml` in the current directory when that file exists (absent → no file overrides, not an error), matching Batect's own `FileDefaultValueProvider`. See [CLI reference](cli-reference.md) and [Expressions](#expressions). |
| `--docker-host`, `--docker-context`, `--docker-config` | Supported | `--docker-host` also fixes a real gap: Ratect previously always connected via the platform default (a Unix socket/named pipe), ignoring `DOCKER_HOST` entirely even with no flags at all — it's now honored, matching Batect. `--docker-context` reads the Docker CLI's own context store (`~/.docker/contexts/meta/<sha256(name)>/meta.json`) for that context's host, matching Batect's own context resolution precedence (`CommandLineOptionsParser.resolveDockerContext`) exactly: an explicit `--docker-context` wins; otherwise an explicit `--docker-host` bypasses the context store entirely; otherwise `DOCKER_CONTEXT`; otherwise the store's own active context (`~/.docker/config.json`'s `currentContext`). See [CLI reference](cli-reference.md). |
| `--docker-cert-path`, `--docker-tls`, `--docker-tls-verify`, `--docker-tls-ca-cert`, `--docker-tls-cert`, `--docker-tls-key` | Supported (one deliberate divergence) | `--docker-tls` and `--docker-tls-verify` behave identically in Ratect — the daemon's certificate is always fully verified. Batect's own bare `--docker-tls` (without `-verify`) instead sets Go's `tls.Config.InsecureSkipVerify`, which disables *all* server certificate verification (chain of trust, expiry, *and* hostname matching, not just hostname matching) while still doing the TLS handshake and any configured client-certificate auth. Ratect deliberately doesn't support that mode at all, adopting the same stance as `rustls` itself (the library Ratect's TLS support is built on): `rustls` has no boolean toggle for this either — disabling verification requires implementing its own `ServerCertVerifier` trait from scratch, a deliberate hurdle against careless misuse, not a config flag. See [CLI reference](cli-reference.md#tls-with-a-private-certificate-authority) for the supported (verified) alternative: run your own CA and point `--docker-tls-ca-cert` at it, rather than skip verification. |
| `--cache-type` | Supported | Selects `volume` (a Docker named volume, the default) or `directory` (a host directory under `.batect/caches/`) as the storage mechanism for a `cache` volume mount — see [Cache volumes](config-reference.md#cache-volumes). No effect on a config with no `cache` mounts. Unlike Batect, not forced to `directory` for Windows containers — Ratect has no Windows support to special-case yet. |
| `--clean`, `--clean-cache` | Supported | Clears out this project's existing cache volumes/directories (per `--cache-type`) and exits — matching Batect's own `CleanupCachesCommand` exactly, including never needing the task config itself, and `--clean-cache <NAME>`'s explicit allowlist always winning over plain `--clean`'s "everything" default when both are given. Not a build-performance feature either way (the Docker build cache itself is unaffected) — these govern Batect's own cache *volumes*, a distinct mechanism. |
| `--max-parallelism` | Supported (narrower) | Batect's own flag caps *every* setup/cleanup step (image pulls/builds, container starts, health-check waits, setup commands, stops, removals) via a step-scheduling model (`ParallelExecutionManager`) Ratect doesn't have. Ratect's version caps image pulls/builds, a dependency's own create+start, and setup-command execution — the CPU/disk/network-intensive operations — via a single invocation-wide semaphore, one permit held only for the duration of each individual operation (never nested across a whole container's readiness sequence). Two deliberate exclusions: health-check waits are a polling wait, not resource-intensive work, so gating them would only slow down convergence for no benefit; and stop/removal (cleanup teardown) isn't resource-intensive in practice either. The task's own container's run is also never gated, matching Batect's own `RunContainerStep` exemption — it's the actual task work, not setup, and often long-running by design. |
| `--no-proxy-vars` | Supported | Disables proxy environment variable propagation entirely — see [Proxy environment variables](config-reference.md#proxy-environment-variables). |
| `--log-file` | Supported | Tees Ratect's own internal logs (governed by `RUST_LOG` as always) into the given file, in addition to stderr, not instead of it — Batect's own default (no `--log-file`) is a silent `NullLogSink`, nothing anywhere, whereas Ratect always logs to stderr regardless. Plain text, no ANSI color codes, even if stderr's own output has them. See [CLI reference](cli-reference.md#options). |
| `--no-update-notification`, `--upgrade`, `--no-wrapper-cache-cleanup` | Recognized, no effect | Permanently inapplicable — Ratect is a single native binary with no self-updating wrapper script to disable notifications for, clean caches for, or upgrade. Recognized (hidden from `--help`) so an existing Batect invocation carrying one of these doesn't hard-fail outright — before this, any of them was a `clap` parse error that killed the entire invocation before anything ran. `--upgrade` prints a one-line notice and exits `0`; the other two are silently accepted with no message, since there's nothing to disable in the first place. See [CLI reference](cli-reference.md#recognized-for-batect-compatibility-no-effect). |

## Runtime behavior gaps

Batect behavior not implemented in task execution, beyond what's covered by the field
tables above:

- **Cleanup on a termination signal (Ctrl+C, `SIGTERM`, `SIGHUP`)**: matches Batect for
  Ctrl+C — it abandons the run and then cleans up after it, rather than killing Ratect
  where it stands and leaving every container and the task's network behind. As in
  Batect, this counts as a task *failure*, so `--no-cleanup-after-failure` (and
  `--no-cleanup`) suppresses the cleanup for it exactly as for a build or health-check
  failure, leaving everything in place for investigation. Three deliberate differences:

  - **Batect traps `SIGINT` only; Ratect traps `SIGTERM` and `SIGHUP` too**, down the
    same path. A task runner is stopped by more than a keystroke — an editor running
    Ratect as a subprocess sends `SIGTERM` when it closes or restarts it, as do
    `docker stop`, `systemd` and most CI cancel buttons — and every one of those used
    to leak a container *and* a network. Networks leak faster than containers, because
    a run that fails during startup leaks one too; a machine that reaches Docker's
    default limit of roughly 31 networks then fails **every** Ratect run with `all
    predefined address pools have been fully subnetted`, not just the ones that leaked.
  - **The exit code names the signal**: 128 + the signal's own number, so **130** for
    Ctrl+C, **143** for `SIGTERM` and **129** for `SIGHUP`. Batect returns `-1`/255 for
    every failure alike and so says nothing about which it was.
  - **A second signal during the cleanup stops the cleanup itself**, since cleanup talks
    to the daemon and a container ignoring `SIGTERM` waits out Docker's full kill
    timeout. That means a second press after an interrupted run, or a first press while
    a normally-finished run is still tidying up — the rule is about when the signal
    lands, not how many there have been. Batect ends up in the same place by a different
    route — a second interrupt during its cleanup stage switches it to printing manual
    cleanup commands — whereas Ratect just stops, because anything left carries the
    ownership labels above and `ratect resources list`/`clean` finds it (see the
    [`ratect` CLI reference](ratect-cli.md#managing-resources)).

  Two cases a trap cannot cover. `SIGKILL` (`kill -9`, a container runtime's own
  hard stop, the OOM killer) cannot be caught by any process, so it still leaks
  everything the run had created — which is what the ownership labels below are
  for: [`ratect resources clean`](ratect-cli.md#managing-resources) removes what
  they identify. That verb lives in the `ratect` binary only; `ratect-compat` has
  no equivalent, so from it the sweep is `docker` itself, filtering on the same
  labels (`docker ps -a --filter label=eu.orican.ratect.project=<name>`, and
  `docker network ls` likewise). And an
  [interactive](config-reference.md#interactive-mode) task puts the terminal in raw
  mode, where Ctrl+C isn't turned into a signal at all but forwarded to the
  container as a keystroke — matching `docker run -it`, where it belongs to the
  program you're talking to. That is a property of the terminal driver, not of this
  trap: raw mode only stops `^C` becoming a `SIGINT`, so a signal sent by anything
  other than the keyboard is delivered as usual.
- **Ownership labels**: every container and network Ratect creates carries
  `eu.orican.ratect.*` labels recording the project, task, run, and (for a
  container) which configured container it is and whether it was the task's own or
  a dependency. Batect labels nothing of its own, so this is an **additive
  divergence** — it changes no behavior, and a container's own configured `labels`
  are passed through untouched alongside. Visible in `docker inspect` output, and
  usable as a `docker ps --filter label=...` filter. Groundwork for finding
  leftovers from a `--no-cleanup` run, a crash, or a run killed outright with
  `SIGKILL` — Ctrl+C, `SIGTERM` and `SIGHUP` each clean up after themselves
  (see above).
- **Anonymous volume cleanup**: matches Batect — a container is removed with Docker's
  `v`/`force` options set, so any anonymous volume it created (from a `VOLUME`
  instruction in its image) goes with it. This was a divergence until 0.21.1: Ratect
  removed containers with Docker's defaults, leaking one dangling volume per such
  container per run.
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
  [Proxy environment variables](config-reference.md#proxy-environment-variables). Two
  deliberate differences:

  - **A `localhost` proxy is rewritten on Linux too, and Ratect makes the name
    resolve.** Batect rewrites on macOS/Windows only, and on Linux propagates
    `http_proxy=http://localhost:3333` into the container verbatim — where
    `localhost` is the container itself, so it fails silently or reaches something
    unrelated. This is [Batect's oldest open
    issue](https://github.com/batect/batect/issues/10), eight years old, and its
    own recipe for it (read the gateway address out of `docker network inspect`,
    then add an `iptables` rule) predates the fix: Docker Engine 20.10 (December
    2020) added `--add-host host.docker.internal:host-gateway`, which does the
    same job as one documented flag. Ratect rewrites everywhere and adds that
    entry to every container **and every image build** — but only when a URL was
    actually rewritten, so a run whose proxy already names a routable host gets no
    name it didn't ask for. Taking that flag as given means this feature needs
    Docker 20.10 or newer — comfortably below the Engine API version Ratect
    already requires of the daemon for everything else, so it constrains nobody
    in practice; see [Prerequisites](installation.md#prerequisites).
  - **A proxy that's bound to loopback only is diagnosed, not left to fail.**
    Rewriting the URL can't make such a proxy reachable — `cntlm` and friends
    typically bind `127.0.0.1`, and a container connecting through the host
    gateway never arrives there. On Linux, Ratect reads `/proc/net/tcp`/`tcp6`,
    sees that the port is loopback-bound, and says so, naming the remedy
    (rebinding to `0.0.0.0`) **and its security cost** — that doing so exposes the
    proxy to everything else that can reach the machine. Batect's own roadmap
    notes the same warning is needed and never shipped one. It's a warning rather
    than a failure: a run may not need the proxy, and `--no-proxy-vars` already
    turns propagation off. Host firewall rules are the one case left undiagnosed,
    since there's no reliable interface to check for: Ratect's containers are
    always on a user-defined network, whose bridge interface Docker doesn't name
    in `docker network inspect`. What to do about that is documented rather than
    detected — see [Proxy environment
    variables](config-reference.md#proxy-environment-variables).

  What stays an accepted gap is Batect's Docker-version-gated hostname fallback
  chain, which reaches back to Docker 17.06. It isn't worth chasing for any
  actively-maintained daemon.
- **Private registry credentials**: **not supported.** Batect reads your Docker
  configuration (`~/.docker/config.json` by default, or `DOCKER_CONFIG`, or its
  own `--docker-config`), resolves the credential store or helper, and sends
  those credentials when it pulls an image or builds one. Ratect reads that same
  file — it has a `--docker-config` of its own, with the same defaults — but only
  for the Docker context; it ignores the credential sections and sends no
  registry credentials at all.

  What that means in practice: with Batect, running `docker login` once was
  enough, and every later run could pull from your private registry. With Ratect,
  a container whose `image` lives in a private registry fails to pull, whether or
  not you have logged in — unless that exact image is already in the daemon's
  local store, in which case nothing needs pulling and it works, which makes the
  failure look intermittent.

  Workaround until this is closed: `docker pull` the image yourself before the
  run, so the daemon already has it — your existing `docker login` applies, since
  that pull is the Docker CLI's, not Ratect's. Tracked in
  [ROADMAP.md](../ROADMAP.md#batect-parity) and blocking 1.0.0.

## What Ratect *does* support today

For the positive list — what's actually implemented and working — see:

- [Getting started](getting-started.md) for a walkthrough
- [Configuration reference](config-reference.md) for the supported schema
- [CLI reference](cli-reference.md) for the supported flags
- [How it works](how-it-works.md) for the execution model (prerequisites, dependency
  cycle detection, once-per-run dedup of tasks and image pulls, and — as of 0.15.0 —
  concurrent startup of independent branches of one task's own dependency graph)
