# Configuration Reference

Ratect reads a YAML file (`batect.yml` by default) describing containers and tasks.
This documents the schema Ratect actually parses today (`ratect-core/src/config.rs`) — it is a
**subset** of Batect's configuration format. See
[differences from Batect](differences-from-batect.md) for what's not yet supported.

## Top level

```yaml
project_name: my-project
containers:
  <name>: <Container>
tasks:
  <name>: <Task>
config_variables:
  <name>: <ConfigVariable>
```

| Field | Type | Required | Description |
|---|---|---|---|
| `project_name` | string | yes | Used only for display (e.g. in `--list-tasks` output). |
| `containers` | map of name → [Container](#container) | yes | Container definitions, keyed by name. Referenced from tasks via `run.container`. |
| `tasks` | map of name → [Task](#task) | yes | Task definitions, keyed by name. Run by name via `ratect <task-name>`. |
| `config_variables` | map of name → [ConfigVariable](#configvariable) | no | Declares the config variables usable via `<name`/`<{name}` [expressions](#expressions). A name must be declared here before it can be referenced — see [Expressions](#expressions). |

## Container

```yaml
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - .:/code
```

| Field | Type | Required | Description |
|---|---|---|---|
| `image` | string | one of `image`/`build_directory` | A Docker image reference to pull and run (e.g. `alpine:3.18`). |
| `build_directory` | string | one of `image`/`build_directory` | Builds an image from a `Dockerfile` in this directory (see [Image building](#image-building) below) instead of pulling a pre-built one. Supports [expressions](#expressions) and is resolved to an absolute path the same way a volume's `host_path` is — see [Volume path resolution](#volume-path-resolution). |
| `build_args` | map of string → string | no | Build-time variables passed to `docker build` (Docker's own `--build-arg` mechanism), e.g. `VERSION: "1.2.3"`. Only meaningful alongside `build_directory`. Values support [expressions](#expressions). |
| `volumes` | list of strings | no | Bind mounts in `host_path:container_path` form. `host_path` supports [expressions](#expressions). See [Volume path resolution](#volume-path-resolution) below. |
| `dependencies` | list of strings | no | Names of other containers to start (recursively, if they themselves have dependencies) before this one, reachable by name over a Docker network created for the duration of the task. No health-check waiting — a dependency is considered ready as soon as it's started. See [the task lifecycle](task-lifecycle.md) for the full model, and [Differences from Batect](differences-from-batect.md#container-fields) for what's simplified relative to Batect. |
| `environment` | map of string → string | no | Environment variables to set in the container, e.g. `FOO: bar`. Values support [expressions](#expressions) (`$VAR`, `${VAR:-default}`, `<name`). A dependency container only ever gets its own `environment` — see [TaskRun](#taskrun) for how a task's own container's `environment` combines with `run.environment`. |
| `run_as_current_user` | object (`enabled`, `home_directory`) | no | Runs this container as the host's own user/group instead of the image's default (see [User mapping](#user-mapping) below). |
| `additional_hostnames` | list of strings | no | Extra network aliases this container is reachable by, beyond its own name. No [expression](#expressions) support. |
| `additional_hosts` | map of string → string | no | Extra `/etc/hosts` entries in this container, `hostname: ip`, Docker's own `--add-host` mechanism. No expression support. |
| `ports` | list of strings | no | Publishes container ports to the host, `"local:container[/protocol]"` (protocol defaults to `tcp`), e.g. `"8080:80"`. Only single ports — no ranges, and no expression support. Suppressed entirely by `--disable-ports`, regardless of this field. See [CLI reference](cli-reference.md). |

> **Note:** if a container has *neither* `image` nor `build_directory` set, running a
> task against it is an error naming the container. A dependency container without
> either is also an error, since it needs to actually run to serve its purpose —
> `build_directory` works for dependency containers too, not just a task's own.

Every container's Docker hostname is always set to its own container name (matching
Batect) — not just its network alias. Without this, a container is reachable *by*
its name on the network, but `hostname`/`$HOSTNAME` *inside* it would resolve to
Docker's random short container ID instead, which is easy to be surprised by if
anything logs or checks its own hostname.

### Image building

A container with `build_directory` set is built (not pulled) the first time it's
needed, and reused for the rest of that `ratect` invocation if referenced again (as a
task's own container, as a dependency, or by more than one task) — but never reused
*across* separate `ratect` invocations; each run builds fresh. A few things to know:

- The Dockerfile is always named `Dockerfile`, at `build_directory`'s own root — there's
  no way yet to point at a differently-named or differently-located one.
- A `.dockerignore` file at `build_directory`'s root, if present, excludes matching
  files from the build context — see [`.dockerignore` semantics](#dockerignore-semantics)
  below for the (non-obvious) matching rules. No `.dockerignore` means the whole
  directory tree becomes the build context, unchanged from before this existed.
- `build_target`, `build_secrets`, and `build_ssh` aren't supported yet — see
  [Differences from Batect](differences-from-batect.md).
- The built image is tagged `<project_name>-<container_name>` (matching Batect's own
  default), so it's identifiable in `docker images` rather than showing up as an
  opaque generated name. That tag is reused/overwritten on every run, though — it's
  for identification only, not caching or correctness (Ratect always runs the image
  it just built, regardless of what the tag currently points to by the time the
  container starts).
- Built images aren't cleaned up automatically — since the tag is reused, the image a
  build replaces becomes a dangling (`<none>`) image rather than disappearing, and
  accumulates until manually pruned (`docker image prune`), same as repeatedly running
  a plain `docker build -t ... .` would leave behind.
- Ratect has no `--output` mode yet, so build progress is logged rather than
  streamed to the console: each build log line is emitted at `debug` level (set
  `RUST_LOG=info,ratect_core=debug` for a live transcript without unrelated
  dependency noise — see [filtering `RUST_LOG`](how-it-works.md#filtering-rust_log)),
  and if the build fails, the *entire* transcript is included in the error Ratect
  reports — not just Docker's one-line failure summary — so a failing `RUN` step's
  own output is always visible without needing `RUST_LOG` set.

#### `.dockerignore` semantics

Ratect's `.dockerignore` handling is a from-scratch reimplementation of Docker's own
matching rules (`github.com/moby/patternmatcher`, which Docker's documentation cites as
the reference implementation), **not** a `.gitignore`-compatible matcher — the two are
not the same, and the difference is easy to get surprised by:

- **A bare pattern with no wildcard only excludes at the build context root.**
  `node_modules` excludes a top-level `node_modules` directory, but *not* a nested one
  like `packages/foo/node_modules` — unlike `.gitignore`, where a slash-free pattern
  matches at any depth by default. Use `**/node_modules` for that.
- `**` matches any number of directories (including zero), usable as a prefix, suffix,
  or standalone segment (`**/dir2/*`, `dir/**`, `**`).
- Later lines take precedence over earlier ones — a `!`-prefixed line re-includes a path
  an earlier pattern excluded.
- Leading and trailing slashes are no-ops (`/foo/bar`, `foo/bar/`, and `foo/bar` are all
  equivalent) — including a trailing slash not restricting a match to directories only,
  unlike `.gitignore`.
- `Dockerfile` and `.dockerignore` themselves are always included in the build context
  regardless of exclusion patterns, matching Docker's own special-casing (otherwise a
  broad `*` pattern would exclude the file the build needs).

### Volume path resolution

Each entry in `volumes` is split on `:`. Only entries with **exactly two** colon-separated
parts (`host_path:container_path`) are resolved. `build_directory` is resolved the same
way (it has no `:container_path` part to split off, obviously, but otherwise follows
identical rules):

- The path is interpolated first (see [Expressions](#expressions)) — so a config
  variable that itself resolves to an absolute path is used as-is, not treated as a
  literal relative fragment.
- *After* interpolation, if the result is relative, it's resolved to an absolute path
  **relative to the directory containing the config file** (not the current working
  directory). If it's already absolute (whether literally written that way, or because
  that's what an expression resolved to), it's left unchanged.
- This all happens once, after CLI-supplied config variable overrides
  (`--config-var`/`--config-vars-file`) are known — not at config-parse time.
- Volume entries that don't split into exactly two parts — e.g. a three-part spec like
  `host:container:ro` (Docker's read-only mount flag), or a Windows drive-letter path
  like `C:/data:/code` — are **left completely unresolved**, including no interpolation
  and no path resolution. Use an absolute host path if you need one of these forms today.

### User mapping

```yaml
containers:
  build-env:
    image: alpine:3.18
    run_as_current_user:
      enabled: true
      home_directory: /home/container-user
```

By default, a container runs as whatever user the image defaults to — often root — so
files a task writes to a bind-mounted volume come back host-root-owned. Setting
`run_as_current_user.enabled: true` runs the container as the *host's own* user and
group instead.

> **Note:** `enabled` and `home_directory` are only ever valid *together* — this
> matches Batect's own behavior exactly, not a Ratect-specific restriction. Setting
> `enabled: true` with no `home_directory` is an error (Ratect never guesses one,
> since the container's own image has no home directory prepared for an arbitrary
> host uid/gid). The reverse is *also* an error: `enabled: false` (or omitted) with
> `home_directory` still set — e.g. simply flipping `enabled` back to `false` without
> also deleting `home_directory` fails config loading. Remove `home_directory`
> entirely to disable user mapping, not just `enabled`.

A few things happen automatically to make this actually work, not just set `--user`:

- Any `volumes` entries whose host path doesn't exist yet are created **before** the
  container is even created, as the current host user. Otherwise Docker's daemon
  (running as root) would auto-create them as `root:root` on first use, defeating the
  point for the common "mount my code directory, get build artifacts back with sane
  ownership" case.
- The container's own image has no `/etc/passwd`/`/etc/group` entry for an arbitrary
  host uid/gid — many programs misbehave or refuse to run at all without one (no
  `$HOME`, no username resolution). Minimal synthetic `/etc/passwd`, `/etc/shadow`,
  and `/etc/group` entries are uploaded into the container before it starts.
- `home_directory` itself is created inside the container (owned by the mapped
  uid/gid) before it starts — it's a path inside the container's own filesystem, not
  host-mounted, so it doesn't persist across runs, matching Ratect's existing
  ephemeral-container model.

Applies per-container, independently — a task's own container and each of its
dependencies can each set `run_as_current_user` on their own; it isn't inherited or
shared task-wide.

Not supported yet: an equivalent to Batect's "cache mounts" (Ratect has no such config
concept at all — see [Differences from Batect](differences-from-batect.md)), and
host-side uid/gid lookup is Unix-only (this errors clearly on other platforms rather
than guessing).

## Task

```yaml
tasks:
  test:
    run:
      container: build-env
      command: echo "hello"
    prerequisites:
      - build
```

| Field | Type | Required | Description |
|---|---|---|---|
| `run` | [TaskRun](#taskrun) | yes | What to actually execute for this task. |
| `prerequisites` | list of strings | no | Names of other tasks to run first, in order. Each prerequisite (and its own prerequisites, transitively) runs at most once per `ratect` invocation, even if shared by multiple tasks. A circular dependency is detected and reported as an error. |

## TaskRun

| Field | Type | Required | Description |
|---|---|---|---|
| `container` | string | yes | Name of a container defined under `containers`. |
| `command` | string | no | Shell command to run inside the container (executed as `sh -c "<command>"`). If omitted, the container's own default `CMD`/`ENTRYPOINT` runs instead. Any `-- ADDITIONAL_ARGS` from the CLI become this shell's positional parameters (`$1`, `$2`, `$@`) — see [CLI reference](cli-reference.md#using-additional_args-in-a-task-command). |
| `environment` | map of string → string | no | Environment variables to set for this task's run specifically. Merged with the container's own `environment` (see [Container](#container)): the container's values apply first, and `run.environment` overrides them on a key collision. Values support the same [expressions](#expressions) as `environment` does. |

## Interactive mode

There's no config field for this — it's automatic, matching Batect's own behavior:
running a task whose command drops you into a shell or otherwise needs your input
(`command: sh`, for example) just works, with no `interactive: true` to remember to
set anywhere.

A container gets a real Docker TTY and its stdin forwarded when *all* of the following
hold:

- It's the invoked task's own container — never a prerequisite's, a dependency's, or
  a sidecar's. Only the task actually named on the command line is ever eligible;
  running it via a prerequisite chain doesn't count, since stdin can only usefully
  attach to one container at a time.
- Ratect's own stdin *and* stdout are both genuinely connected to a real terminal.
  Piped output, a redirected non-terminal, or running in CI all fall back to the
  normal (non-interactive) streamed output, exactly as if the task didn't need a TTY
  at all — nothing extra to configure for that case either.

A few things to know about what this doesn't do yet:

- The container's TTY size is synced to the local terminal's size once, at the start
  of the session — it isn't tracked live if the local terminal is resized mid-session.
- Stdin forwarding and TTY allocation aren't decoupled the way Batect's are (Batect can
  forward stdin into a task even without allocating a TTY, e.g. when piping input into
  a non-interactive run). Ratect gates both together — no support yet for piping input
  into a task that isn't otherwise running interactively.

## ConfigVariable

```yaml
config_variables:
  environment_name:
    default: dev
```

| Field | Type | Required | Description |
|---|---|---|---|
| `default` | string | no | The value used when nothing else provides one — see [Expressions](#expressions) for the full precedence order (CLI `--config-var` > `--config-vars-file` > this `default`). Referencing a declared variable that has no `default` and no override from either CLI source is an error. |

## Expressions

`environment` values (on both [Container](#container) and [TaskRun](#taskrun)), a
volume's `host_path` (see [Volume path resolution](#volume-path-resolution)),
`build_directory`, and `build_args` values support two kinds of expression, resolved
once — after CLI-supplied config variable overrides (`--config-var`/`--config-vars-file`)
are known, so before any task runs but not at config-parse time itself. Everywhere else
in the config, a string is used exactly as written, with no substitution — expression
support is scoped to fields that can meaningfully take one; it'll extend to more fields
as they themselves get built (e.g. `build_secrets.path`), not automatically. Literal
text around an expression is left untouched (`"prefix-$VAR-suffix"` interpolates just
`$VAR`), and a `$`/`<` not followed by a valid identifier (or an unterminated `${`/`<{`)
is treated as a literal character rather than an error.

| Form | Resolves against | Example |
|---|---|---|
| `$NAME` | `ratect`'s own host environment | `$HOME` |
| `${NAME}` | `ratect`'s own host environment | `${HOME}` |
| `${NAME:-default}` | `ratect`'s own host environment, falling back to `default` if unset | `${LOG_LEVEL:-info}` |
| `<name` | A [`config_variables`](#configvariable) entry | `<environment_name` |
| `<{name}` | A [`config_variables`](#configvariable) entry | `<{environment_name}` |

A host variable referenced without a `:-default` fallback, and unset when `ratect`
runs, is a hard error naming the variable — there's no silent empty-string fallback. A
config variable referenced via `<name`/`<{name}` must be declared under
`config_variables`; an undeclared name is a hard error, and so is a declared one with
no value from any source (see [ConfigVariable](#configvariable)'s precedence order).
Config variable values themselves come from, highest precedence first: `--config-var
NAME=VALUE` (repeatable), `--config-vars-file` (a flat YAML map), then the variable's
own `default` — see [CLI reference](cli-reference.md).

### Built-in config variable: `batect.project_directory`

`<batect.project_directory`/`<{batect.project_directory}` always resolves to the
absolute path of the directory containing the config file — Batect's one built-in
config variable, so Ratect supports it without requiring (or allowing) it to be
declared under `config_variables`. Declaring a `config_variables` entry named
`batect.project_directory`, or supplying one via `--config-var`/`--config-vars-file`,
is a hard error — it isn't overridable.

## Full example

This mirrors the sample config used in the test suite (`batect.yml` in the repo root):

```yaml
project_name: ratect-test
containers:
  build-env:
    image: alpine:3.18.2
    volumes:
      - .:/code
tasks:
  shared-prereq:
    run:
      container: build-env
      command: echo "I should only run once"
  prereq-task:
    run:
      container: build-env
      command: echo "I am a prerequisite"
    prerequisites:
      - shared-prereq
  list-volume-task:
    run:
      container: build-env
      command: ls /code
    prerequisites:
      - shared-prereq
  test-task:
    run:
      container: build-env
      command: echo "Hello from ratect!"
    prerequisites:
      - prereq-task
      - list-volume-task
```

Running `ratect test-task` runs `shared-prereq` once (even though both `prereq-task`
and `list-volume-task` depend on it), then `prereq-task` and `list-volume-task`, then
`test-task` itself.
