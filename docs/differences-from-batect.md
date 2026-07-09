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
| `include` | Not supported | No multi-file configuration — neither form: local **file includes** (splitting one project's config across files) nor Git **includes/bundles** (importing shared tasks/containers from a separate Git repository, e.g. a team-wide `bundle.yml`). |
| `forbid_telemetry` | N/A | Ratect doesn't collect telemetry, so there's nothing to forbid. |

### Expressions

Not a single field — Batect supports an
[expression syntax](https://github.com/batect/batect.dev/blob/main/docs/reference/config/expressions.md)
(`$VAR`, `${VAR}`, `${VAR:-default}` for host environment variables; `<name`, `<{name}`
for config variables) usable *within* several fields: `environment`, `build_args`,
`build_directory`, `build_secrets.path`, `build_ssh.paths`, and volume local paths.

**Ratect implements this within `environment` and volume local paths** (see
[config reference](config-reference.md#expressions) for the full syntax, precedence,
and error rules, and [Volume path resolution](config-reference.md#volume-path-resolution)
for how an interpolated host path is then resolved relative to the config file). Every
other field's YAML string value is still used exactly as written, with no host-side
substitution step:

- `build_args`, `build_directory`, `build_secrets.path`, and `build_ssh.paths` are
  moot anyway until image building itself exists — see `build_directory`'s "Parsed,
  not implemented" entry below.
- `run.command` is a field where you *will* see `$VAR`-style expansion happen — but
  that's ordinary POSIX shell variable expansion done by `sh -c` **inside the
  container**, using the container's own environment (including anything set via
  `environment`). It's unrelated to Batect's expression syntax, which substitutes
  values from the **host** before the container even starts.
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
| `dependencies` | Supported (simplified) | Starts recursively (nested dependencies too), on a network scoped to one task execution — see [the task lifecycle](task-lifecycle.md). No health-check waiting (`health_check` isn't parsed — see below) and no `setup_commands` support, so a dependency is "ready" as soon as it's started, unlike Batect's real readiness check. |
| `build_directory` | Parsed, not implemented | No image building. Roadmap: [Image Building](../ROADMAP.md#batect-parity). |
| `additional_hostnames` | Not supported | |
| `additional_hosts` | Not supported | |
| `build_args` | Not supported | (moot until image building exists) |
| `build_target` | Not supported | (moot until image building exists) |
| `build_secrets` | Not supported | (moot until image building exists) |
| `build_ssh` | Not supported | (moot until image building exists) |
| `capabilities_to_add` / `capabilities_to_drop` | Not supported | |
| `command` | Supported | Only at the container level via the equivalent task-level `run.command` — see [Task fields](#task-fields). |
| `devices` | Not supported | |
| `dockerfile` | Not supported | (moot until image building exists) |
| `enable_init_process` | Not supported | |
| `entrypoint` | Not supported | |
| `environment` | Supported | Values support [expressions](#expressions) (host env vars and config variables). Combines with the equivalent task-level `run.environment` — see [Task run fields](#run-fields) and [config reference](config-reference.md#taskrun). |
| `health_check` | Not supported | This is why `dependencies` (above) treats "started" as "ready" instead of waiting for real health. |
| `image_pull_policy` | Not supported | Ratect always pulls an image at most once per run, with no `Always`-equivalent. |
| `labels` | Not supported | |
| `log_driver` / `log_options` | Not supported | |
| `ports` | Not supported | No port publishing. |
| `privileged` | Not supported | |
| `run_as_current_user` | Not supported | In Batect, this runs the container as the host user's UID/GID (instead of root) so files written to mounted volumes aren't root-owned. Ratect always runs as whatever user the image defaults to — on Linux, that means volume-mounted files written by a task will typically come back owned by `root`. Roadmap: [User Mapping](../ROADMAP.md#batect-parity). |
| `setup_commands` | Not supported | See `health_check` above — this is the other half of Batect's real dependency-readiness check that Ratect doesn't implement. |
| `shm_size` | Not supported | |
| `working_directory` | Not supported | |

### Task fields

| Field | Status | Notes |
|---|---|---|
| `run` | Supported, but **required** | Batect allows a task with only `prerequisites` and no `run`; Ratect requires `run` on every task. |
| `prerequisites` | Supported | No wildcard (`*`) matching — each name must be listed explicitly. |
| `dependencies` (task-level sidecars) | Not supported | Distinct from the container-level `dependencies` field above; not parsed at all. |
| `description` | Not supported | Rejected — see the note at the top of this page; `--list-tasks` output has no description column. |
| `group` | Not supported | Rejected — see the note at the top of this page; `--list-tasks` output isn't grouped. |
| `customise` | Not supported | |

### `run` fields

| Field | Status | Notes |
|---|---|---|
| `container` | Supported | |
| `command` | Supported | |
| `entrypoint` | Not supported | |
| `environment` | Supported | Values support [expressions](#expressions). Overrides the container's own `environment` on a key collision — see [config reference](config-reference.md#taskrun). |
| `ports` | Not supported | |
| `working_directory` | Not supported | |

## CLI flags

Batect's full flag list, from its [CLI reference](https://github.com/batect/batect.dev/blob/main/docs/reference/cli.mdx):

| Flag | Status | Notes |
|---|---|---|
| `--config-file` / `-f` | Supported | |
| `--list-tasks` / `-T` | Supported | No grouping or descriptions (see [task fields](#task-fields)) and no `--output=quiet`-style machine-parsable variant. |
| `--help` / `-h` | Supported | Auto-generated by `clap`. |
| `--version` | Supported | Auto-generated by `clap` (also gets a `-V` short form Batect doesn't have). |
| `<task-name> -- <args>` | Supported | Forwarded as `sh -c`'s positional parameters (`$1`, `$2`, `$@`) rather than appended as literal argv the way Batect does it — since Ratect always runs commands via `sh -c` (see [CLI reference](cli-reference.md#using-additional_args-in-a-task-command)), this is the safe equivalent within that design, though the exact mechanism differs from Batect's. |
| `--skip-prerequisites` | Not supported | Prerequisites always run. |
| `--override-image` | Not supported | |
| `--output` / `-o` | Not supported | Ratect has one output mode; see [how it works](how-it-works.md#5-logging-vs-output) for the stdout/stderr split it uses instead. |
| `--no-color` | Not supported | Ratect currently has no colored output to disable. |
| `--no-cleanup`, `--no-cleanup-after-failure`, `--no-cleanup-after-success` | Not supported | Ratect always attempts to remove containers after running; there's no way to leave them for debugging. |
| `--disable-ports` | N/A | Moot — no port publishing exists to disable. |
| `--use-network` | Not supported | A minimal per-task network now exists (see `dependencies`) but there's no way to point it at an existing network instead. Roadmap: [Docker Networking](../ROADMAP.md#batect-parity). |
| `--enable-buildkit` | N/A | Moot — no image building exists yet. |
| `--tag-image` | N/A | Moot — no image building exists yet. |
| `--config-vars-file`, `--config-var` | Supported | See [CLI reference](cli-reference.md) and [Expressions](#expressions). |
| `--docker-host`, `--docker-context`, `--docker-config`, `--docker-cert-path`, `--docker-tls*` | Not supported | Ratect connects using Docker's local defaults only, with no CLI overrides. |
| `--cache-type`, `--clean`, `--clean-cache` | N/A | Moot — no cache concept exists (Batect's caches are for build performance, not implemented here). |
| `--max-parallelism` | N/A | Moot — Ratect doesn't run anything in parallel yet. Roadmap: [Parallel Task Execution](../ROADMAP.md#rust-enhancements). |
| `--no-proxy-vars` | N/A | Moot — no proxy propagation exists yet. Roadmap: [Proxy Support](../ROADMAP.md#batect-parity). |
| `--log-file` | Different mechanism | Ratect uses `RUST_LOG` + stderr instead of a dedicated log-file flag — see [CLI reference](cli-reference.md#environment-variables). |
| `--no-update-notification`, `--upgrade`, `--no-wrapper-cache-cleanup` | N/A | Moot — Ratect isn't distributed via a self-updating wrapper script. |

## Runtime behavior gaps

Batect behavior not implemented in task execution, beyond what's covered by the field
tables above:

- **Docker networking**: a minimal per-task network now exists (see
  [`dependencies`](#container-fields) and [the task lifecycle](task-lifecycle.md)), but
  only for containers involved in a dependency relationship — it's not Batect's fully
  configurable networking (custom drivers, `--use-network` to reuse an existing
  network, etc.).
- **Interactive mode**: no TTY/STDIN attachment for tasks that need user input.
- **Parallel execution**: prerequisites run sequentially, not in parallel — Batect runs
  independent setup/cleanup steps concurrently.

## What Ratect *does* support today

For the positive list — what's actually implemented and working — see:

- [Getting started](getting-started.md) for a walkthrough
- [Configuration reference](config-reference.md) for the supported schema
- [CLI reference](cli-reference.md) for the supported flags
- [How it works](how-it-works.md) for the execution model (prerequisites, dependency
  cycle detection, once-per-run dedup of tasks and image pulls)
