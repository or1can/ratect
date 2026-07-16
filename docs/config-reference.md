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
| `include` | list of string or [Include](#includes) | no | Splits configuration across multiple files — see [Includes](#includes) below. |

## Includes

```yaml
include:
  - some-include.yml
  - path: some-other-include.yml
    type: file
  - type: git
    repo: https://github.com/my-org/my-batect-bundle.git
    ref: v1.2.3
    path: bundle.yml
```

Each entry is either a bare string path (a local file include), or an object form —
`{path, type: file}` for another local file, or `{type: git, repo, ref, path}` for a
Git include (a "bundle": shared tasks/containers imported from a separate Git
repository). Any other `type` is rejected with a clear error.

An included file uses the same schema as the root file, with two differences:

- It must not declare `project_name` — that's root-only.
- `containers`, `tasks`, and `config_variables` may each be omitted entirely (they
  default to empty) — a file that exists only to `include` further files, or only to
  add one task, doesn't need to restate the others.

Every loaded file's `containers`, `tasks`, and `config_variables` are merged into one
flat set. A name defined in more than one file is a hard error naming the conflicting
files — it's never treated as one file overriding another.

Relative paths *within* a container (a volume's host path, `build_directory`, a
`build_secrets` entry's `path`) resolve against that container's *own* origin file's
directory, not the root project
directory. Use the built-in [`batect.project_directory`
config variable](#built-in-config-variable-batectproject_directory) — which always
resolves to the root's directory regardless of which file a container is defined in —
to reference the root project directory explicitly from an included file.

### Local file includes

A local include's path is resolved relative to the directory of the file that
declares the `include` — *not* the root project directory — so an included file
further down a subdirectory can itself `include` more files using paths relative to
its own location. An already-loaded file (by resolved absolute path) is skipped
rather than reloaded, so it's safe for two files to both include a common third file.

### Git includes

| Field | Type | Required | Description |
|---|---|---|---|
| `repo` | string | yes | A Git remote — anything `git clone` itself accepts (an HTTPS/SSH URL, or a local path). |
| `ref` | string | yes | The tag, branch, or commit to check out. **Must be a value that never changes** (in practice, an immutable tag or a pinned commit SHA, not a branch) — see below. |
| `path` | string | no, default `batect-bundle.yml` | The path, within the repository, of the file to include — resolved relative to the repository's own root, not the file that declared the `include`. |

A `(repo, ref)` pair is cloned **once and cached forever** at
`~/.ratect/incl/<hash>`, keyed by a hash of the pair — it is never re-fetched, even if
the remote's `ref` later moves (e.g. a branch, or a tag someone re-pushed). This is why
`ref` must be pinned to something immutable: Ratect has no update/refresh mechanism yet
(see [Differences from Batect](differences-from-batect.md#top-level-fields) for the
known gaps — no cache eviction sweep, no manual cache-clear command). If you need to
pick up a change made to a bundle, choose a new `ref` (e.g. bump the tag) or delete the
corresponding directory under `~/.ratect/incl` by hand.

The included file's own relative paths (a volume's host path, `build_directory`, a
`build_secrets` entry's `path`, and any further `include` entries it declares) resolve
against the *cloned repository's*
root, the same way a local include's relative paths resolve against its own directory
(see above) — just rooted at the clone instead of a directory in your project.

**Containment**: a Git include's `path`, and every `include` entry declared
(transitively) by the file it names, must resolve to somewhere *inside* that
repository's own clone — an absolute path, a `../..` traversal, or a symlink pointing
back out are all rejected with a clear error rather than silently reading a file
elsewhere on the machine running `ratect`. This matters because `repo`/`ref` may point
at a repository you don't fully control, unlike a local file include (which stays
unrestricted, since it's always something already in your own project checkout). A
Git-included bundle *can* still declare a further `type: git` include of its own —
that's a fresh repository with its own boundary, not an escape from this one.

The same containment applies to a `volumes` host path, `build_directory`, or
`build_secrets` entry's `path` declared by a *container* defined inside a
Git-included file: it must resolve to somewhere inside
that repository's own clone, **or** inside your project directory — an absolute path
or `../..` traversal escaping both is rejected the same way. The project directory is
allowed as a second root (rather than requiring pure containment within the clone)
because referencing it explicitly via
[`batect.project_directory`](#built-in-config-variable-batectproject_directory) (e.g.
`<{batect.project_directory}/output:/output`) is a legitimate, common thing for a
shared bundle to do — the project directory is your own fully-trusted tree, distinct
from the repository the container definition itself came from.

Cloning requires the system `git` binary to be installed and on `PATH` — Ratect shells
out to it (`git clone --quiet --no-checkout` followed by
`git checkout --recurse-submodules <ref>`) rather than embedding a Git library, so
submodules and any Git configuration (credentials, `.gitconfig` rewrites, etc.) that
your normal `git clone` already relies on work the same way here.

For example, given `containers/extra.yml` (included from the root `batect.yml`):

```yaml
containers:
  my-other-container:
    image: alpine:1.2.3
    volumes:
      # Resolves relative to containers/, not the root project directory.
      - ./data:/data
      # Always the root project directory, regardless of where this file lives.
      - <{batect.project_directory}/scripts:/scripts
```

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
| `dockerfile` | string | no | The Dockerfile to build, as a path relative to `build_directory`'s own root. Defaults to `Dockerfile` at `build_directory`'s root. Only meaningful alongside `build_directory`. No [expression](#expressions) support. |
| `build_target` | string | no | The build stage to stop at (Docker's own `--target` mechanism), for a multi-stage `FROM ... AS <name>` Dockerfile. Only meaningful alongside `build_directory`. No expression support. |
| `volumes` | list of strings | no | Bind mounts in `host_path:container_path` form. `host_path` supports [expressions](#expressions). See [Volume path resolution](#volume-path-resolution) below. |
| `dependencies` | list of strings | no | Names of other containers to start (recursively, if they themselves have dependencies) before this one, reachable by name over a Docker network created for the duration of the task. Each dependency must become *ready* — healthy, with all its `setup_commands` completed — before its dependents start; see [Dependency readiness](#dependency-readiness) below and [the task lifecycle](task-lifecycle.md) for the full model. |
| `environment` | map of string → string | no | Environment variables to set in the container, e.g. `FOO: bar`. Values support [expressions](#expressions) (`$VAR`, `${VAR:-default}`, `<name`). A dependency container only ever gets its own `environment` — see [TaskRun](#taskrun) for how a task's own container's `environment` combines with `run.environment`. |
| `run_as_current_user` | object (`enabled`, `home_directory`) | no | Runs this container as the host's own user/group instead of the image's default (see [User mapping](#user-mapping) below). |
| `additional_hostnames` | list of strings | no | Extra network aliases this container is reachable by, beyond its own name. No [expression](#expressions) support. |
| `additional_hosts` | map of string → string | no | Extra `/etc/hosts` entries in this container, `hostname: ip`, Docker's own `--add-host` mechanism. No expression support. |
| `ports` | list of strings/objects | no | Publishes container ports to the host (see [Port mappings](#port-mappings) below). No expression support. Suppressed entirely by `--disable-ports`, regardless of this field. See [CLI reference](cli-reference.md). |
| `health_check` | object | no | Overrides the health check configuration baked into the container's image (see [Dependency readiness](#dependency-readiness) below). No expression support. |
| `setup_commands` | list of objects (`command`, `working_directory`) | no | Commands run inside the started container after it becomes healthy but before its dependents start (see [Dependency readiness](#dependency-readiness) below). No expression support. |

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

- The Dockerfile built is `dockerfile` (a path relative to `build_directory`'s own
  root), defaulting to `Dockerfile` at `build_directory`'s own root when omitted.
- `build_target` stops the build at that stage, for a multi-stage `FROM ... AS <name>`
  Dockerfile — Docker's own `--target` mechanism.
- A `.dockerignore` file at `build_directory`'s root, if present, excludes matching
  files from the build context — see [`.dockerignore` semantics](#dockerignore-semantics)
  below for the (non-obvious) matching rules. No `.dockerignore` means the whole
  directory tree becomes the build context, unchanged from before this existed.
  `dockerfile` and `.dockerignore` itself are always included in the build context
  regardless of exclusion patterns, matching Docker's own special-casing.
- `build_secrets` exposes secrets to the build via BuildKit's secret-mount mechanism
  (a Dockerfile's `RUN --mount=type=secret,id=<key>`), without persisting them into
  the built image's layers — keyed by the `id` such a `RUN` instruction references.
  Each entry is either `{environment: NAME}` (read from *this* `ratect` process's own
  environment at build time) or `{path: ...}` (read from a file on the host, resolved
  like `build_directory`); exactly one of the two is required. Using `build_secrets`
  switches that build to a BuildKit gRPC session instead of Docker's classic build
  API, and disables the build cache for it entirely — BuildKit deliberately excludes
  a secret's *value* from its cache key (so it can't leak into one), which would
  otherwise let an unrelated Dockerfile change reuse a cached layer built with a
  now-stale secret value.
- `build_ssh` forwards an SSH agent from the host into the build, for a Dockerfile's
  `RUN --mount=type=ssh` instructions. **Ratect only supports forwarding the host's
  running `ssh-agent` (via its `SSH_AUTH_SOCK`) under the implicit `default` agent
  id** — at most one entry (`build_ssh: [{id: default}]`, or an empty/omitted `id`),
  and an entry with explicit key `paths` is rejected — see
  [Differences from Batect](differences-from-batect.md#container-fields) for why.
  Also switches that build to a BuildKit gRPC session (shared with `build_secrets`
  above if both are set on the same container) — see
  [Differences from Batect](differences-from-batect.md). The agent is proxied over
  that session, not mounted as a socket — so this works unchanged on macOS/Windows,
  where Docker Desktop's VM boundary otherwise blocks mounting host sockets into
  containers (no `/run/host-services/ssh-auth.sock` workaround involved).
- A BuildKit build captures output the same way a classic build does: every build
  step and its output is logged at `debug` level (`RUST_LOG=debug` for a live
  transcript), and a build failure's error includes the entire accumulated
  transcript — the failing step's own output included — not just BuildKit's
  one-line failing-instruction summary.
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

### Port mappings

```yaml
containers:
  web:
    image: nginx:alpine
    ports:
      - "8080:80"
      - "9000-9002:9100-9102/udp"
      - local: 8443
        container: 443
```

`ports` publishes container ports to the host — Docker's own `-p`/`--publish`
mechanism — and takes either form Batect itself supports, freely mixed within one
list:

- A string, `"local:container[/protocol]"` (`protocol` defaults to `tcp`), e.g.
  `"8080:80"` or `"8080:80/udp"`.
- A port *range* string, `"from-to:from-to[/protocol]"`, e.g. `"9000-9002:9100-9102"`
  — each local port maps to the corresponding container port by position (`9000` →
  `9100`, `9001` → `9101`, `9002` → `9102`); `local` and `container` must cover the
  same number of ports.
- The expanded object form, `{local, container, protocol}` — `local`/`container` each
  accept a single port or a range (`8443` or `"8000-8002"`), `protocol` is optional
  (defaults to `tcp`).

Validated at config-load time (unlike `volumes`, which is never format-checked):
a malformed entry, a non-positive port, or mismatched-size local/container ranges are
all rejected before anything runs. No [expression](#expressions) support.

`TaskRun.ports` (see [TaskRun](#taskrun)) adds *additional* port mappings for a
specific task's run — combined with the container's own `ports` as a union, not an
override; there's no concept of one replacing an entry from the other.

`--disable-ports` suppresses publishing of every container's `ports` — from both
`Container.ports` and any `TaskRun.ports` — regardless of what's configured. See
[CLI reference](cli-reference.md).

### Dependency readiness

```yaml
containers:
  database:
    image: postgres:16
    health_check:
      command: pg_isready -h localhost
      interval: 2s
      retries: 5
      start_period: 3s
      timeout: 1s
    setup_commands:
      - command: ./apply-migrations.sh
      - command: ./seed-data.sh
        working_directory: /setup
```

A dependency container being *started* doesn't mean it's *ready* — a database
accepts connections some time after its process launches. Matching Batect, a
dependency must pass two gates, in order, before anything that depends on it (another
dependency, or the task's own container) starts:

1. **It must report healthy.** If the container has a Docker health check — from its
   image's own `HEALTHCHECK`, from the `health_check` field, or both — Ratect waits
   for Docker's verdict: proceeds on *healthy*; fails the task on *unhealthy* (the
   error includes the last health-check run's exit code and output) or if the
   container exits first. A container with no health check at all is immediately
   considered healthy — the pre-0.9.0 "started = ready" behavior, now just the
   no-health-check special case.
2. **Its `setup_commands` must succeed.** Each runs inside the running container (via
   Docker's `exec` mechanism), one at a time in declared order, with the container's
   own `environment` and (under [User mapping](#user-mapping)) the same user/group
   the container runs as. A command exiting non-zero fails the task, with its output
   in the error.

`health_check` *overrides* the image's health check configuration — each field
replaces that one aspect, and any field left out inherits the image's own value:

| Field | Type | Description |
|---|---|---|
| `command` | string | The command to run to check the container's health, via the container's default shell (a Dockerfile `HEALTHCHECK CMD <string>`). Exit code 0 means healthy. |
| `interval` | duration | Time between health check runs. |
| `retries` | integer | Consecutive failures needed before the container is considered unhealthy. |
| `start_period` | duration | Time during which failing checks don't count against `retries` (a success during it still counts as healthy immediately). |
| `timeout` | duration | Time a single check may run before it's considered failed. |

Durations are strings in Batect's (Go-style) format: one or more `<number><unit>`
components — `ns`, `us`, `ms`, `s`, `m`, `h`, numbers optionally fractional — e.g.
`2s`, `500ms`, `1m30s`, `1.5h`, or a bare `0`.

#### How Docker reaches its verdict

This is Docker's own behavior, not Ratect's, but it's what actually determines how
long the gate waits and when it fails, so it's worth spelling out:

- A freshly started container with a health check isn't unhealthy — it's in a third
  state, **`starting`**, until Docker reaches a first verdict. Docker runs `command`
  every `interval`; the first success makes the container *healthy*, and only
  `retries` **consecutive** failures make it *unhealthy*. With the example above
  (`interval: 2s`, `retries: 5`), the earliest possible unhealthy verdict is about
  ten seconds in — a health check can't "fail fast" on its first bad run.
- Failures during `start_period` don't count toward `retries` at all — that's the
  grace period for slow-booting services — but a success during it still flips the
  container healthy immediately.
- Ratect waits for that first verdict, and **only** the first: matching Batect, a
  dependency's health is never re-checked once its dependents have started, even
  though Docker keeps running the check for the container's whole lifetime and the
  state can flip later.

While a task appears to hang on a dependency, `docker ps` shows each container's
health state in its `STATUS` column (`health: starting`, etc.), and
`docker inspect --format '{{.State.Health.Status}}' <container>` shows it directly —
`.State.Health.Log` keeps the last few check runs' exit codes and output, which is
also where the detail in Ratect's "did not become healthy" error comes from.

Each `setup_commands` entry takes:

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | The command to run, via `sh -c` — the same shell treatment a task's `command` gets. |
| `working_directory` | string | no | Directory to run it in. Falls back to the image's default working directory when omitted. |

Ratect imposes no timeout of its own on the health wait (matching Batect) — Docker's
own `interval`/`retries` bound how long a verdict can take, so a health check
configured to retry forever waits forever.

Two deliberate scope notes, both diverging only for the task's *own* container (see
[Differences from Batect](differences-from-batect.md#container-fields)): its
`health_check` is still applied — Docker records and runs it — but Ratect never waits
on its verdict (the task's own exit code alone decides the outcome), and its
`setup_commands` don't run at all (Batect runs them concurrently with the task's
command; Ratect's engine has no concurrent exec path yet).

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
| `ports` | list of strings/objects | no | Additional port mappings for this task's run specifically — see [Port mappings](#port-mappings). *Added* to the container's own `ports`, not an override — there's no concept of one replacing an entry from the other. |

## Interactive mode

There's no config field for this — it's automatic, matching Batect's own behavior:
running a task whose command drops you into a shell or otherwise needs your input
(`command: sh`, for example) just works, with no `interactive: true` to remember to
set anywhere.

The invoked task's own container — never a prerequisite's, a dependency's, or a
sidecar's; only the task actually named on the command line is ever eligible — always
gets its stdin forwarded and the host's `TERM` environment variable propagated into its
own environment (see [below](#term-propagation)), independent of whether Ratect's own
stdin/stdout are real terminals. A real Docker TTY (raw mode locally, live terminal
resizing) is additionally allocated when *both* Ratect's own stdin *and* stdout are
genuinely connected to a real terminal — piped output, a redirected non-terminal, or
running in CI fall back to plain (non-TTY) stdin forwarding and streamed output instead,
but stdin still reaches the container either way. Nothing extra to configure for either
case.

The container's TTY, when one is allocated, stays in sync with the local terminal's
size for the whole session (not just once at the start) — a local resize is forwarded
live via a `SIGWINCH` handler. This tracking is Unix-only; on other platforms the size
is still synced once, at the start of the session, but not tracked further (interactive
mode itself works cross-platform either way).

One known, deliberate divergence from Batect remains: Batect's own real-TTY gate checks
only whether its output is a real terminal; Ratect's requires *both* stdin and stdout to
be real terminals before allocating one.

### `TERM` propagation

Ratect's own `TERM` environment variable is copied into the invoked task's own
container's environment automatically, whenever that container is eligible (the
top-level task, as above) — not gated on a real TTY actually being allocated, matching
Batect's own unconditional behavior. Never applied to a prerequisite's, a dependency's,
or a sidecar's container, and never applied to an image build. A container's own
explicit `environment`, or a task's `run.environment`, both still override it on a key
collision (see [TaskRun](#taskrun) for how those two combine with each other) — `TERM`
is the lowest-precedence layer, the same tier [proxy environment
variables](#proxy-environment-variables) occupy.

## Proxy environment variables

There's no config field for this either — like [interactive mode](#interactive-mode),
it's automatic, matching Batect's own behavior. Whenever `http_proxy`, `https_proxy`,
`ftp_proxy`, or `no_proxy` (in either case, e.g. `HTTP_PROXY` too) are set in the
environment `ratect` itself runs in, they're injected into every container's
environment and every image build's `build_args` — so a task or a build that needs to
reach the network through a proxy just works, without repeating proxy settings in
`environment`/`build_args` by hand.

A few details worth knowing:

- **Precedence**: injected proxy variables are the lowest-precedence layer — a
  container's own `environment`, and a task's `run.environment`, both override a
  proxy-derived value on a key collision (see [TaskRun](#taskrun) for how those two
  combine with each other). `build_args` works the same way for builds.
- **`no_proxy` is extended automatically**: every container sharing a task's network
  (the task's own container and each of its dependencies) has its own name appended to
  `no_proxy`/`NO_PROXY`, so traffic between them isn't sent through the proxy. Not done
  for image builds — nothing's running yet during a build, so there's nothing to
  exempt.
- **`localhost` rewriting**: `http_proxy`/`https_proxy`/`ftp_proxy` values that point at
  `localhost`, `127.0.0.1`, or `::1` are rewritten to `host.docker.internal`, since
  `localhost` from *inside* a container refers to the container itself, not the host
  machine running a proxy. Only rewritten on macOS and Windows (where Docker Desktop
  provides `host.docker.internal` automatically) — left unchanged on Linux, where
  there's no automatic equivalent. A value that isn't a `http`/`https` URL, or doesn't
  refer to the local machine, is also left unchanged.
- **`--no-proxy-vars`** disables all of this. See [CLI reference](cli-reference.md).

See also: [`TERM` propagation](#term-propagation) — a similarly automatic,
lowest-precedence-layer environment injection for the invoked task's own container, but
gated on interactive eligibility rather than `--no-proxy-vars`.

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
`build_directory`, `build_args`, and a `build_secrets` entry's `path` (not its
`environment` — that's a literal host environment variable *name*, not itself
interpolated) support two kinds of expression, resolved once — after CLI-supplied
config variable overrides (`--config-var`/`--config-vars-file`) are known, so before
any task runs but not at config-parse time itself. Everywhere else in the config, a
string is used exactly as written, with no substitution — expression support is
scoped to fields that can meaningfully take one; it'll extend to more fields as they
themselves get built, not automatically. Literal
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
