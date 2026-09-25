# `ratect.toml` Configuration Reference

This documents **`ratect.toml`**, the native configuration format the
[`ratect`](ratect-cli.md) binary reads by default. It is the same
schema the [Configuration Reference](ratect-compat-config-reference.md) documents — the same
containers, tasks, and fields, with the same meanings — re-spelled in TOML, with
a few native additions (`extends`, an auto-discovered local overrides file) and a
few YAML-isms removed (anchors, the compact string shorthands).

Because the *field semantics* are identical across both formats, this reference
does not repeat them: for what a given field actually does, follow the links into
[`ratect-compat-config-reference.md`](ratect-compat-config-reference.md). What's covered here is the parts
that are genuinely different — the TOML spelling, and the native-only rules.

> The native format is `ratect`'s alone — `ratect-compat` reads `batect.yml`
> only (see [Two binaries, two formats](index.md#two-binaries-two-formats)). To
> migrate an existing `batect.yml`, run [`ratect config convert`](ratect-cli.md#config).

## The file

A `ratect.toml` describes a project's `containers` and `tasks`. Named containers
and tasks map onto TOML tables, so a container `build-env` is `[containers.build-env]`
and a task `build` is `[tasks.build]`:

```toml
project_name = "my-app"

[containers.build-env]
image = "rust:1.90"
working_directory = "/code"
volumes = [{ local = ".", container = "/code" }]
environment = { CARGO_TERM_COLOR = "always" }

[tasks.build]
description = "Compile the project"
group = "Development"
run = { container = "build-env", command = "cargo build" }
```

`project_name` is the only required top-level key (it's taken from the root file
only, and names the images and cache volumes the project creates — see
[Top level](ratect-compat-config-reference.md#top-level)). `ratect` defaults `-f` to
`ratect.toml`; point it at a differently-named file, or a `batect.yml`, with
`-f`.

This closely mirrors [`examples/rust/ratect.toml`](https://github.com/or1can/ratect/blob/main/examples/rust/ratect.toml)'s
own real `build-env` container and `build` task — same image, same command, same
task `description`/`group` — with an `environment` entry added here purely to
show the syntax; the real file doesn't set one.

## `extends`: inheritance instead of YAML anchors

`batect.yml` factors out a shared base container with YAML anchors/aliases/merge
keys (`&base`/`*base`/`<<:`). Those are YAML syntax and don't exist in TOML, so
`ratect.toml` replaces them with an explicit **`extends`** field:

```toml
[containers.base]
image = "rust:1.90"
environment = { CARGO_TERM_COLOR = "always" }

[containers.build-env]
extends = "base"
working_directory = "/code"      # added
environment = { RUSTFLAGS = "-D warnings" }  # replaces base's entirely
```

The rules:

- **Single parent.** `extends` names exactly one container.
- **Shallow, per field.** A field the child sets replaces the inherited one
  *outright* — there is no deep-merging into nested maps. Above, `build-env`'s
  `environment` is `{ RUSTFLAGS }` only; `base`'s `CARGO_TERM_COLOR` is **not**
  merged in. This matches how `<<:` already behaves, and Cargo's profile
  `inherits`. To keep an inherited map *and* add to it, restate the whole map.
- **Chains.** `a` may extend `b` which extends `c`; each level fills what the one
  below left unset. A cycle (including a container extending itself) is an error.
- **Base-only containers need no `image`.** Only a container a task actually runs
  is required to have an `image` or `build_directory`, so a `base` that exists
  purely to be extended can omit both.
- **Resolved after paths.** Inheritance happens *after* relative paths are made
  absolute, so an inherited `build_directory` or volume host path stays anchored
  to the file that *declared* it, not the child's location — this matters when
  the parent came from an [included file](#includes).
- **Overriding a build with an image.** Because inheritance is per-field with no
  way to *unset* one, setting `image` on a child is how you override a parent's
  `build_directory`: `image` wins, and the inherited `build_directory` is simply
  unused. `ratect-compat` rejects a container with both fields (Batect does, and
  has no `extends` that would need the override) — this is a deliberate
  difference, not an oversight. A container used only as an `extends` base
  likewise needs neither field; the requirement is enforced when a task actually
  runs a container, so no `abstract` marker is needed.
- **Containers only.** Tasks do not `extends` (compose task behaviour with
  `prerequisites`/`dependencies` instead).

## One shape per list entry

`volumes`, `ports`, and `devices` take **one object shape per entry** — not the
compact `"local:container"` strings `batect.yml` also accepts. Use inline tables
for the terse cases and `[[...]]` array-of-tables blocks for longer ones; they're
equivalent:

```toml
[containers.app]
image = "postgres:16"

# Inline tables — compact.
volumes = [
    { local = ".", container = "/code" },
    { local = "./secrets", container = "/run/secrets", options = "ro" },
]
ports = [{ local = 5432, container = 5432 }]

# Or the block form — readable when there are many fields.
[[containers.app.devices]]
local = "/dev/kvm"
container = "/dev/kvm"
```

- A **`volumes`** entry is a host bind (`local` + `container` [+ `options`]), a
  named [cache volume](ratect-compat-config-reference.md#cache-volumes)
  (`{ type = "cache", name = "...", container = "..." }`), or a
  [tmpfs mount](ratect-compat-config-reference.md#tmpfs-mounts)
  (`{ type = "tmpfs", container = "...", options = "..." }`).
- A **`ports`** entry is `{ local, container }` [+ `protocol`], with port ranges
  written as `"6000-6010"` — see [Port mappings](ratect-compat-config-reference.md#port-mappings).
- A **`devices`** entry is `{ local, container }` [+ `options`].

The parser itself still *accepts* the string forms (which is what lets a
`.yml` [include](#includes) keep using them), but the native schema, the docs,
and [`config validate`](ratect-cli.md#config) treat the object form as canonical.

## Config variables and expressions

Config variables are declared under `[config_variables]` and referenced with the
same `<name` / `<{name}` expression syntax as `batect.yml` — the *syntax* is
values inside strings, so it carries across verbatim. Which **fields** resolve
one is not identical: this format also resolves them in `image`, which a
`batect.yml` refuses — see [Expressions in `image`](#expressions-in-image), and
note that the `image` line in the example below is exactly that case. Otherwise
see [ConfigVariable](ratect-compat-config-reference.md#configvariable) and
[Expressions](ratect-compat-config-reference.md#expressions).

```toml
[config_variables.tag]
default = "latest"
description = "The image tag to run."

[containers.app]
image = "myapp:<{tag}"                 # a config variable
environment = { HOME = "${HOME}" }      # a host environment variable
```

### Local overrides

A **`ratect.local.toml`** beside the config file is loaded automatically when
present (no flag), supplying config-variable *values* for the current
developer/machine — the native default for `--config-vars-file`:

```toml
# ratect.local.toml — gitignore this.
tag = "dev"
```

It holds **values only**, not configuration: a flat `name = "value"` map, nothing
else. Anything you want to vary locally should be a config variable the tracked
config interpolates, keeping what varies declared and visible rather than hidden
in an untracked file. Precedence, lowest to highest: a variable's `default`, then
the config-vars file (`ratect.local.toml`, or whatever `--config-vars-file`
names), then `--config-var` on the command line.

## Includes

`include` is an array of entries, each a local file or a Git bundle. Formats may
mix: **each included file is parsed by its extension** — `.toml` as native,
`.yml`/`.yaml` as Batect-format YAML — so a native project can still pull in an
existing `batect.yml` fragment or bundle unchanged.

```toml
include = [
    { path = "ci/tasks.toml" },                              # local, native
    { path = "shared/legacy.yml" },                          # local, still YAML
    { type = "git", repo = "https://example.com/bundle.git", ref = "v2" },
]
```

A `type = "git"` entry with no `path` discovers its bundle file by looking for
**`ratect-bundle.toml` first, then `batect-bundle.yml`** — so an unmigrated Batect
bundle keeps working, and a bundle author can ship both files to support `ratect`
and Batect at once. How included files combine, where their paths resolve
and what a bundle may do are on [Includes](includes.md); the shared
`~/.ratect/incl` cache is in the [`batect.yml`
reference](ratect-compat-config-reference.md#git-includes)
([`ratect includes`](ratect-cli.md#includes-options) manages it); why you'd
share a bundle across projects at all is [Reusable Pipeline Building
Blocks](reusable-building-blocks.md).

An `extends` in a native file may inherit from a container defined in *any*
included file, including a YAML bundle — the container namespace is flat once
includes are merged.

## Shared caches

A `cache` mount is private to the project by default: the storage carries the
project's own key, so two projects declaring `cargo-registry` get two
different caches. `scope = "shared"` drops that key, so every project on the
machine naming it gets the *same* storage.

```toml
[[containers.build-env.volumes]]
type = "cache"
name = "cargo-registry"
container = "/usr/local/cargo/registry"
scope = "shared"        # or "project", the default

[[containers.build-env.volumes]]
type = "cache"
name = "build-output"
container = "/build"    # no scope: private to this project
```

This exists because the alternative is worse. A bundle that wants one Cargo
registry or npm cache across projects would otherwise have to spell it as a
host path (`local = "~/.cache/cargo"`), which means granting the bundle access
to your home directory — the thing
[`allow_host_paths`](includes.md#vouching-for-a-bundle) exists to permit and
[decisions/0004](https://github.com/or1can/ratect/blob/main/decisions/0004-git-include-host-path-trust.md)
would rather solve properly. A shared cache says the same thing directly,
grants no host filesystem access at all, and keeps the location under Ratect's
control.

**Where it is stored.** A shared cache is the Docker volume
`ratect-shared-cache-<name>`, or the directory `~/.ratect/caches/<name>` under
`--cache-type=directory` — beside `~/.ratect/incl`, where Git includes are
cloned, because both belong to the machine rather than to any one project. A
project cache remains `batect-cache-<project key>-<name>`.

**A name has one scope per project.** Declaring `cargo-registry` as `project`
in one container and `shared` in another is rejected when the file loads: one
name would mean two different pieces of storage. Two *containers* naming the
same cache is the ordinary way to share it between them, and is unaffected.

**Removing one takes naming it.** `ratect caches clean` with no arguments
sweeps this project's caches and never a shared one — discarding storage other
projects are still using should not be a side effect. See
[the `caches` options](ratect-cli.md#caches-options).

`batect.yml` has no equivalent, so `scope` is rejected there rather than
ignored — see [Differences](#differences-from-batectyml-at-a-glance) below.

## Expressions in `image`

A container's `image` takes [expressions](ratect-compat-config-reference.md#expressions), so a
pipeline can choose its image per run without a flag:

```toml
[config_variables.tag]
default = "latest"

[containers.app]
image = "my-repo/my-image:<{tag}"

[containers.tools]
image = "my-repo/tools:${IMAGE_TAG:-latest}"
```

Both forms work: `<{tag}` reads a [`config_variables`](#config-variables-and-expressions) entry
(settable with `--config-var tag=1.2.3`), and `${IMAGE_TAG:-latest}` reads the
host environment with a fallback. The same rules apply as everywhere else — an
unset host variable with no `:-default` is a hard error naming it, rather than a
silent empty string that would produce a puzzling image reference.

Resolution happens before `extends` is applied, so a container inheriting an
`image` inherits the *resolved* value, consistent with `build_directory` and
volume host paths.

**Resolution is eager and covers the whole file**, not just the containers your
task uses — again like every other expression-bearing field. So an unset
variable with no `:-default` fails *every* task in the file, including tasks
that never touch the container declaring it. Give a default where a variable is
genuinely optional. The error names the container, so you are not left hunting
for which one.

**Rejected in a `batect.yml`**, rather than resolved or ignored. Batect has no
expression support in `image`, so a file using one would load here and fail
under `batect` itself — and unlike an exotic capability, a parameterised image
tag is something a pipeline would use on every run, so the lock-in would be
routine rather than incidental. `ratect-compat` users have `--override-image`,
which covers most of the same ground; what it can't express is an in-config
default, which is what this adds.

The rejection is on what you wrote, not on what it would resolve to, and it
knows the difference between an expression and a literal `$`: `alpine:3.18` and
`repo/img:1.2.3` load exactly as before.

## Nested Git includes

A [Git include](includes.md#two-kinds-of-include) fetches configuration from a
repository and merges it into yours. That bundle can declare `include` entries
of its own — and in a `batect.yml` those may be further `type: git` entries,
naming any remote, with the same trust your own includes get. In `ratect.toml`
that is **refused by default**: you chose the bundle; you did not choose
whatever it decides to pull in next. Opt in per bundle by setting
`allow_nested_git_includes` on that bundle's include entry:

```toml
[[include]]
type = "git"
repo = "https://github.com/my-org/infra-bundle.git"
ref = "1.2.3"
allow_nested_git_includes = true
```

The grant is one level deep, counts only in configuration you control, and
has to sit on the entry that reaches the bundle's file first — the same rules
as `allow_host_paths`, stated once on [Includes](includes.md#nested-git-includes)
along with the refusal you see without the grant.

**A nested include's clone failure is reported without `git`'s own message.**
Whether a remote is unreachable, refusing connections, missing, or demanding
credentials is a readout on a network — and for a nested include the remote
was named by the bundle, not by you, so the answer is of more use to whoever
wrote it than to you. In CI, where the log is often visible to anyone who can
propose a change to that bundle, repeated attempts map an internal network one
include at a time. The failure is still reported and still names both
repositories; only the transport detail moves behind `RUST_LOG=debug`. An
include *you* declared keeps `git`'s message in full — it describes a remote
you wrote down, and hiding it would only make your own typo harder to find.

The field is rejected in a `batect.yml` rather than ignored, value and all:
setting `allow_nested_git_includes` to false there would claim a restriction
that format never applies.

## `run_in`: setup commands in another container

A `setup_commands` entry normally runs inside the container that declares it.
Sometimes the tooling isn't there: seeding a database means running a client,
and a `postgres:16` image is not where your migration tool lives. `run_in`
names another container to run the command in, while leaving *when* it runs
alone — it is still part of the declaring container's readiness gate, so
nothing that depends on that container starts until the command has succeeded.

```toml
[containers.db-seed-client]
image = "my-org/db-tools:latest"
# Stays alive, so there is a live process for the command to run alongside.
command = "sleep infinity"

[containers.db]
image = "postgres:16"
dependencies = ["db-seed-client"]
health_check = { command = "pg_isready -U postgres" }
setup_commands = [
  { command = "psql -h db -U postgres -f /seed/schema.sql", run_in = "db-seed-client" },
]

[containers.app]
image = "my-org/app:latest"
dependencies = ["db"]
```

`app` starts once `db` is healthy *and* the seeding command has exited 0 —
exactly as it would if that command ran inside `db` itself.

- **The target must be one of the declaring container's own `dependencies`**
  (or the declaring container itself, which is what leaving `run_in` out
  means). Anything else is rejected when the file loads, naming the container
  you asked for. This is the rule that makes the ordering safe rather than
  merely likely: a dependency has already been through its *own* full
  readiness gate before the container declaring it even starts, so it is
  running when the command arrives. A sibling has no ordering edge to this
  container at all, and a container that depends on *this* one structurally
  cannot have started yet — either would be a race with no fix, so neither is
  offered. A `dependencies` entry inherited via [`extends`](#extends-inheritance-instead-of-yaml-anchors)
  counts, since the check runs after inheritance resolves. A *task's* own
  `dependencies` does not, even for that task's main container: it orders the
  target ahead of this container for one task only, and the same container
  used by another task would silently lose that ordering — so the dependency
  has to be declared on the container, where it holds everywhere.
- **The command runs with the target container's environment, user and
  working directory**, not the declaring container's — it is running over
  there, so those are the ones that apply. That includes the fallback a
  setup command with no `working_directory` of its own gets: the *target's*
  `working_directory`, not the declaring container's. The entry's own
  `working_directory` still overrides. In the example above, that is why the
  command reaches the database over the network (`-h db`) rather than through
  a local socket.
- **The target must stay running.** It is `docker exec`, so there has to be a
  live container to exec into — hence `sleep infinity` above. A
  [`run_to_completion`](#run_to_completion-init-containers) dependency has
  already exited by the time it counts as ready, so naming one is rejected
  when the file loads rather than failing mid-run.
- **`ratect`-native only.** Batect has no equivalent —
  [batect#286](https://github.com/batect/batect/issues/286) asked for this in
  2018 and was never built — so a `batect.yml` using it is rejected when the
  file loads rather than quietly running the command in the declaring
  container instead.

## `run_to_completion`: init containers

A dependency normally starts detached, waits for a health check (immediate if
none is configured), then runs its `setup_commands` — see [Dependency
Readiness](dependency-readiness.md#the-two-gates). `run_to_completion` replaces that whole
gate with a simpler one: the dependency runs, and is ready once it exits with
status 0 — a non-zero exit fails the task run the same way an unhealthy
dependency or a failing setup command does. Kubernetes-style
init-container behavior, expressed as a plain node in the existing dependency
graph rather than a separate concept:

```toml
[containers.migrate]
image = "my-repo/migrate:latest"
run_to_completion = true

[containers.app]
image = "my-repo/app:latest"
dependencies = ["migrate"]
```

- **No health check, no `setup_commands`.** Neither concept applies once a
  dependency runs to completion — setting either alongside `run_to_completion`
  is rejected when the file loads.
- **Ordinary graph participation.** A `run_to_completion` dependency can depend
  on other dependencies (of either kind), other dependencies can depend on it,
  and it can be declared directly under a task's own `dependencies` or nested
  under another dependency's — no special-casing either way.
- **Concurrent when independent.** Two `run_to_completion` dependencies for one
  container that don't depend on each other run at the same time, gated purely
  by the graph, not a separate strictly-sequential list.
- **The task's own container is unaffected.** It already always runs to
  completion by definition — that's what running a task's command means. The
  field is meaningless there twice over: structurally, a task's own `run`
  block has no field to set it on in the first place; and on the container
  itself, setting it on whichever container a task names via `run.container`
  is rejected when the file loads (directly, or inherited via `extends`),
  rather than silently doing nothing. The same container can still be
  `run_to_completion` when used as a *dependency* by another task — only
  being a task's own main container while the flag is set is rejected.
- **`ratect`-native only**, like `extends`: `batect.yml` has no equivalent
  concept, so a container using it is rejected when the file loads rather than
  silently ignored.

**Not a substitute for `prerequisites`.** A `prerequisites` entry runs
as a fully separate task execution — own network, own containers, own
cleanup — strictly sequential relative to whatever named it, whether or not it
ran concurrently with anything else. That isolation is the point when the two
runs genuinely shouldn't share anything. `run_to_completion` solves a
different problem: staying *inside* one task's own dependency graph, sharing
its network with siblings (a `cache`, a long-running `app`) that must keep
running while it finishes.

## `external_health_check`: checking a container from outside it

A [`health_check`](dependency-readiness.md#the-two-gates) runs *inside* the
container it checks, so it needs a shell and a tool (`curl`, `wget`,
`pg_isready`) to be present in that image. A distroless or `scratch` image has
neither, and the usual workaround is to add tooling that exists for no reason
but to be health-checked with. `external_health_check` checks such a container
from outside instead.

It is a separate field because it is a separate concept, not a second
spelling of `health_check`. `health_check` *is* Docker's own `HEALTHCHECK` —
the daemon owns its schedule, its `starting`/`healthy`/`unhealthy` state and
its re-running for the container's whole lifetime. This is closer to a
Kubernetes *readiness* check: an external observer asking "can this be used
yet?", once, with no opinion about the container's health afterwards. (Ratect
has no equivalent of a *liveness* check in either form.)

```toml
[containers.api]
image = "my-repo/api:1.0.0"   # scratch-based: no shell, no curl

[containers.api.external_health_check]
type = "http"
port = 8080
path = "/healthz"

[containers.app]
image = "my-repo/app:1.0.0"
dependencies = ["api"]
```

`app` now starts only once `GET http://api:8080/healthz` answers `200`, with
nothing at all having run inside `api`.

Two kinds of check are supported:

| `type` | Checks | Fields |
| --- | --- | --- |
| `http` | The response status of a request to `path` on `port` | `port` (required), `path` (defaults to `/`), `expected_status` (defaults to `200`) |
| `tcp` | That a connection to `port` is accepted | `port` (required) |

Both also take `interval`, `retries` and `timeout`, meaning exactly what they
mean on a [`health_check`](ratect-compat-config-reference.md#dependency-readiness) —
how long to wait between attempts, how many attempts to make before failing
the task, and how long one attempt may take. The difference is what an omitted
one does: a `health_check` inherits the image's own value, and there is no
image to inherit from here, so Ratect's own defaults apply (`interval = "1s"`,
`retries = 30`, `timeout = "5s"`). The first attempt is made immediately
rather than after `interval` has elapsed. A `port`, `timeout` or `retries` of
zero is rejected when the file loads rather than taken literally — nothing
listens on port 0, zero attempts would never check at all, and the tools the
check is made with read a zero timeout as *no* timeout. So is an
`expected_status` that isn't three digits, which no HTTP response can carry.
A checked container's own name has to be reachable as a hostname, since the
check reaches it by that name: letters, digits, `-`, `_` and `.`, not
starting with a `-` or a `.`. (A leading `_` is fine — Docker resolves it and
both tools the check uses reach it.)

- **Over the project's own network, never a published port.** The check
  reaches the container by its container-config name — the same network alias
  its siblings use — so `port` is the port the container *listens* on, and
  nothing has to appear in `ports`. Two isolated instances of one project
  (concurrent CI jobs on a single host) therefore never contend over a host
  port to check each other.
- **Reported as an ordinary health check, because that is what it is.** The
  output says `api has become healthy.` when the check passes — the same line
  a `health_check` produces, at the same point in the run. A check that never
  passes fails it the same way too, naming the container you wrote:
  `Container 'api' did not become healthy: last status 000 from
  http://api:8080/healthz after 30 attempt(s), wanted 200`.
- **Ratect runs the check from a companion container, which you don't see.**
  Declaring one generates a container named `ratect-health-check-<container>`
  running `curlimages/curl` (pinned by digest, not by tag — you did not write
  that reference, so it must not resolve to something different later), which
  loops the check and exits 0 or non-zero — an ordinary
  [`run_to_completion`](#run_to_completion-init-containers) dependency, so the
  waiting happens in the dependency graph rather than anywhere new. It
  narrates nothing of its own in any output style: it is how the check runs,
  not something you declared. It is still a real container, so
  [`ratect resources`](ratect-cli.md#resources-options) and `docker ps` see
  it, and a failure names it so
  [`--no-cleanup-after-failure`](ratect-cli.md#run-options) leaves you
  something to inspect.
- **The generated name is reserved.** While `api` has an external check,
  anything in the project that declares or refers to a container called
  `ratect-health-check-api` — another container's `dependencies`, a task's
  `run.container` or `dependencies` — is rejected when the file loads, naming
  what referred to it. Nothing is silently replaced or quietly repurposed.
- **No `health_check`, no `run_to_completion`, no `setup_commands`.** All
  three are rejected alongside it when the file loads. The first would be two
  answers to one question; the second has already exited by the time anything
  could connect to it; and the third has no ordering available to it — the
  companion is a *sibling* of the checked container rather than a gate on it,
  so a setup command would run against exactly the service the check is
  waiting for. Put such a step in a container of its own that depends on the
  checked one, where it waits for the check like anything else.
- **Inert on a task's own container**, exactly as `health_check` is — nothing
  waits on a task's own container becoming ready, because running it *is* the
  task. Unlike `run_to_completion`, this isn't rejected: the field changes
  nothing about how the container runs, so the same container can be one
  task's main container and another task's checked dependency.
- **`ratect`-native only**, like `run_to_completion`: `batect.yml` has no
  equivalent concept, so a container using it is rejected when the file loads
  rather than silently ignored.

Because the two are separate concepts, `interval`, `retries` and `timeout` do
not quite mean the same thing in both, despite the shared names — which is why
they are not one field under a `type` tag. Docker's `retries` counts
*consecutive failures before flipping to unhealthy*, cushioned by a
`start_period` that has no meaning out here; this one counts *attempts*.

## `stop_signal`/`stop_grace_period`: graceful shutdown

Cleanup stops every container the same way by default — Docker's own
default signal (usually `SIGTERM`), then a fixed ~10-second timeout before
escalating to a forceful kill. That is fine for most containers, but not for
one where an abrupt stop can corrupt something, such as a database with data
shared between invocations. `stop_signal` and `stop_grace_period` — Docker
Compose's own field names for the same concept — let a container opt into
different behavior:

```toml
[containers.database]
image = "postgres:16"
stop_signal = "SIGINT"
stop_grace_period = "30s"
```

- **Purely additive.** A container that sets neither field behaves exactly
  as today — Docker's own default signal and timeout, unchanged. Setting one
  without the other only changes that half: `stop_signal` alone still waits
  Docker's own default timeout, and `stop_grace_period` alone still sends
  Docker's own default signal.
- **Only changes what a task's own cleanup sends.** The other place Ratect
  stops a container — [`ratect resources clean`](ratect-cli.md#resources-options),
  which removes what an interrupted run left behind — finds its containers
  by a label scan, with no configuration to read, so it keeps Docker's own
  default signal and timeout whatever a container asked for here.
- **Durations use Batect's Go-style string format**: `"2s"`, `"1m30s"`,
  `"0"` — the same format `health_check`'s `interval`/`timeout` use. Docker's
  own stop timeout is whole seconds, so anything finer is rounded *up* to the
  next second: `"500ms"` waits one second, never less than asked for.
- **A second interrupt during cleanup still abandons cleanup immediately.**
  `stop_grace_period` only bounds how long the first interrupt's cleanup
  waits on Docker; pressing Ctrl+C again abandons cleanup exactly as it does
  today, regardless of any container's configured grace period.

## `ulimits`: per-resource limits

A container can raise or lower the resource limits its processes run under —
Docker's own `--ulimit`, per container:

```toml
[containers.build-env]
image = "rust:1.90"
ulimits = [
    { name = "nofile", soft = 1024, hard = 2048 },
    { name = "core", soft = 0 },
]
```

- **`name` is one of the resources
  [`docker run --ulimit`](https://docs.docker.com/reference/cli/docker/container/run/#ulimit)
  documents**, without its `RLIMIT_` prefix: `core`, `cpu`, `data`, `fsize`,
  `locks`, `memlock`, `msgqueue`, `nice`, `nofile`, `nproc`, `rss`, `rtprio`,
  `rttime`, `sigpending`, `stack`. (Not `as`, which Docker documents as
  deprecated.) A name outside that list is rejected when the file loads,
  like an unknown
  [capability](ratect-compat-config-reference.md#container) — Docker's API
  checks none of this itself, so the alternative is a container that is
  created happily and then fails to *start*, as `wrong rlimit value:
  RLIMIT_<NAME>` out of the container runtime.
- **`hard` defaults to `soft`.** An entry that sets only `soft` sets both
  limits to that value, matching `docker run --ulimit name=limit`.
- **`-1` means unlimited.** A soft limit above the hard limit — or an
  unlimited soft limit under a finite hard one — is rejected when the file
  loads. That is the rule the `docker` CLI enforces for `--ulimit`, though
  its documentation doesn't state it; the daemon's API doesn't enforce it
  either, and a container that gets such a pair fails in the runtime with
  `error setting rlimit type 7: invalid argument`, which names a number
  rather than the field you wrote.
- **Per container, and only that container.** A dependency's `ulimits` apply
  to the dependency; a task's own container is unaffected by them, and there
  is no task-level or project-wide default.
- **Purely additive.** A container that sets no `ulimits` is created exactly
  as before — whatever the daemon's own defaults are.
- **`ratect`-native only**, like [`stop_signal`](#stop_signalstop_grace_period-graceful-shutdown):
  `batect.yml` has no equivalent field, so a container using it is rejected
  when the file loads rather than silently ignored.

Entries are objects, like every other native list entry. The parser also
accepts Docker's own compact `"nofile=1024:2048"` (or `"nofile=1024"`)
string, for the same reason [`devices`](#one-shape-per-list-entry) still
accepts its shorthand — a `.yml` [include](#includes) can keep using it —
but the object form is what this format, the schema and [`config
validate`](ratect-cli.md#config) treat as canonical.

## Field reference

Every container and task field from [`ratect-compat-config-reference.md`](ratect-compat-config-reference.md)
applies, with the same meaning except where [Where the semantics
differ](#where-the-semantics-differ) says otherwise. Scalars, string maps
(`environment`, `labels`, `build_args`, …) and scalar lists
(`capabilities_to_add`, `additional_hostnames`, …) are a direct 1:1 spelling;
the only fields whose *shape* differs are the object-per-entry lists above.
The container fields, by area:

| Area | Fields | Semantics |
| --- | --- | --- |
| Image | `image`, `image_pull_policy`, `build_directory`, `dockerfile`, `build_target`, `build_args`, `build_secrets`, `build_ssh` | [Image building](ratect-compat-config-reference.md#image-building) |
| Mounts | `volumes` (host / `cache` / `tmpfs`) | [Volumes](ratect-compat-config-reference.md#volume-path-resolution), [caches](ratect-compat-config-reference.md#cache-volumes), [tmpfs](ratect-compat-config-reference.md#tmpfs-mounts). A cache also takes [`scope`](#shared-caches) *(native only)* — the linked section describes project-keyed storage, which `scope = "shared"` deliberately does not use. |
| Runtime | `command`, `entrypoint`, `working_directory`, `environment`, `enable_init_process`, `privileged`, `shm_size`, `capabilities_to_add`, `capabilities_to_drop`, `devices`, `labels`, `log_driver`, `log_options` | [Container](ratect-compat-config-reference.md#container) |
| Graceful shutdown | `stop_signal`, `stop_grace_period` | [above](#stop_signalstop_grace_period-graceful-shutdown) *(native only)* |
| Resource limits | `ulimits` | [above](#ulimits-per-resource-limits) *(native only)* |
| Networking | `ports`, `additional_hostnames`, `additional_hosts`, `dependencies` | [Ports](ratect-compat-config-reference.md#port-mappings), [readiness](dependency-readiness.md) |
| Readiness | `health_check`, `setup_commands` | [Dependency Readiness](dependency-readiness.md). A setup command also takes [`run_in`](#run_in-setup-commands-in-another-container) *(native only)* |
| Init containers | `run_to_completion` | [above](#run_to_completion-init-containers) *(native only)* |
| External checks | `external_health_check` | [above](#external_health_check-checking-a-container-from-outside-it) *(native only)* |
| User | `run_as_current_user` | [User mapping](ratect-compat-config-reference.md#user-mapping) |
| Inheritance | `extends` | [above](#extends-inheritance-instead-of-yaml-anchors) *(native only)* |

### Where the semantics differ

Almost nothing: the two formats parse into the same model, so a field means
what [`ratect-compat-config-reference.md`](ratect-compat-config-reference.md) says it means. The
exceptions fall into three groups: places where `extends` gives a combination
a meaning it cannot have in a `batect.yml`, which has no inheritance; places
where this format is deliberately **stricter**, having no Batect
compatibility to preserve; and one place where it does **more** than Batect,
which `batect.yml` then has to refuse rather than quietly accept.

| Behaviour | `batect.yml` (`ratect-compat`) | `ratect.toml` (`ratect`) |
| --- | --- | --- |
| A Git-included bundle declaring a **`type: git` include of its own** | Always allowed, matching Batect | Refused unless the bundle's own include entry sets [`allow_nested_git_includes`](#nested-git-includes) |
| A **nested** Git include failing to clone | Reports `git`'s own error | Reports that it failed, with the transport detail behind `RUST_LOG=debug` — see [Nested Git includes](#nested-git-includes) |
| An **expression in `image`** | Rejected when the file loads — Batect resolves nothing there | Resolved like any other expression — see [Expressions in `image`](#expressions-in-image) |
| A container with **both** `image` and `build_directory` | Rejected when the file loads, matching Batect | Allowed — `image` wins, and this is the only way to override a `build_directory` inherited from an `extends` parent, since inheritance is per-field with no way to unset one |
| A container with **neither** `image` nor `build_directory` | Rejected when the file loads | Allowed — a container used only as an `extends` base needs neither; the requirement is enforced when a task actually runs a container, so no `abstract` marker is needed |
| Setting **`stop_signal`/`stop_grace_period`** on a container | Rejected when the file loads — Batect has no equivalent field | Overrides Docker's own default stop signal/timeout during cleanup — see [above](#stop_signalstop_grace_period-graceful-shutdown) |
| Setting **`ulimits`** on a container | Rejected when the file loads — Batect has no equivalent field | Sets that container's own resource limits — see [above](#ulimits-per-resource-limits) |
| `image` alongside a build-only field (`build_args`, `build_target`, `dockerfile`, `build_secrets`, `build_ssh`) | Rejected when the file loads | Allowed and **ignored**, for the same inheritance reason — a child overriding a build with an `image` still carries the parent's build fields |

The last row is the one to watch: setting `build_secrets` or `build_ssh` on a
container that also has an `image` does nothing at all, and the native format
cannot tell you so without forbidding the override above. If a build field
looks like it is being ignored, check whether the container resolves to an
`image`.

Task fields: `run` (a [`TaskRun`](ratect-compat-config-reference.md#taskrun) table —
`container`, `command`, `entrypoint`, `environment`, `ports`,
`working_directory`), `prerequisites`, `dependencies`, `description`, `group`,
and `customise` (see [Task](ratect-compat-config-reference.md#task)). A task needs at least one
of `run` or `prerequisites`.

```toml
[tasks.integration-test]
description = "Run the integration suite"
prerequisites = ["build"]
run = { container = "test-runner", command = "pytest tests/integration" }

[tasks.integration-test.run.environment]
DATABASE_URL = "postgres://db/test"
```

## Editor support

Ratect ships a JSON schema for the native format,
[`schema/ratect-config.schema.json`](../schema/ratect-config.schema.json)
(generated from the config types, so it can't drift). Pointing a TOML-aware
editor extension at it — [taplo](https://taplo.tamasfe.dev) / "Even Better TOML"
for VS Code, or JetBrains' TOML support — gives field-name autocompletion, hover
documentation, and a red squiggle under a misspelled or unsupported field. It's
the native counterpart of the [`batect.yml`
schema](ratect-compat-config-reference.md#editor-autocompletion-and-validation): the same
schema, adjusted to the native shape (object-only list entries, plus `extends`).

The simplest way to use it is a schema directive on the first line of your
config, which taplo honors:

```toml
#:schema https://raw.githubusercontent.com/or1can/ratect/main/schema/ratect-config.schema.json
project_name = "my-project"
```

Structural validation only catches what the schema can express; for the rules it
can't (a task needing `run` or `prerequisites`, an `extends` cycle, a container
with neither `image` nor `build_directory`), [`ratect config
validate`](ratect-cli.md#config) checks a `ratect.toml` without a Docker daemon,
so it also works as a CI gate.

## Differences from `batect.yml`, at a glance

| | `batect.yml` (`ratect-compat`) | `ratect.toml` (`ratect`) |
| --- | --- | --- |
| Format | YAML | TOML |
| Default file | `batect.yml` | `ratect.toml` |
| Reuse | anchors / aliases / merge keys | [`extends`](#extends-inheritance-instead-of-yaml-anchors) |
| Cross-project cache | — | [`scope = "shared"`](#shared-caches) on a `cache` mount |
| Init containers | — | [`run_to_completion`](#run_to_completion-init-containers) on a dependency |
| Health check from outside | — | [`external_health_check`](#external_health_check-checking-a-container-from-outside-it) on a container |
| Graceful shutdown | — | [`stop_signal`/`stop_grace_period`](#stop_signalstop_grace_period-graceful-shutdown) on a container |
| Resource limits | — | [`ulimits`](#ulimits-per-resource-limits) on a container |
| Setup command target | always the declaring container | [`run_in`](#run_in-setup-commands-in-another-container) on a setup command |
| List entries | string shorthand *or* object | object (inline table or `[[...]]`) |
| Local overrides | `batect.local.yml` | `ratect.local.toml` |
| Git bundle default | `batect-bundle.yml` | `ratect-bundle.toml`, then `batect-bundle.yml` |
| Includes | YAML | TOML or YAML, by extension |

Most field *meanings* are unchanged; the spelling and the format-level rules
above are the bulk of the difference. The exceptions are the native-only
fields (`extends`, a cache's `scope`, a dependency's `run_to_completion` or
`external_health_check`, a setup command's `run_in`, a container's
`stop_signal`/`stop_grace_period` or `ulimits`) and the handful of behaviours in [Where
the semantics differ](#where-the-semantics-differ), which exist because
`extends` gives some combinations a meaning `batect.yml` has no way to
express.
