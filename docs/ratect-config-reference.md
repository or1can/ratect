# `ratect.toml` Configuration Reference

This documents **`ratect.toml`**, the native configuration format the
[`ratect`](ratect-cli.md) binary reads by default (from 0.3.0). It is the same
schema the [Configuration Reference](config-reference.md) documents — the same
containers, tasks, and fields, with the same meanings — re-spelled in TOML, with
a few native additions (`extends`, an auto-discovered local overrides file) and a
few YAML-isms removed (anchors, the compact string shorthands).

Because the *field semantics* are identical across both formats, this reference
does not repeat them: for what a given field actually does, follow the links into
[`config-reference.md`](config-reference.md). What's covered here is the parts
that are genuinely different — the TOML spelling, and the native-only rules.

> The native format is `ratect`'s alone. `ratect-compat` reads `batect.yml`
> (YAML) permanently, for Batect compatibility — see
> [Two Binaries](../ROADMAP.md#two-binaries-ratect-and-ratect-compat). To migrate
> an existing `batect.yml`, run [`ratect config convert`](ratect-cli.md#config).

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
[Top level](config-reference.md#top-level)). `ratect` defaults `-f` to
`ratect.toml`; point it at a differently-named file, or a `batect.yml`, with
`-f`.

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
  named [cache volume](config-reference.md#cache-volumes)
  (`{ type = "cache", name = "...", container = "..." }`), or a
  [tmpfs mount](config-reference.md#tmpfs-mounts)
  (`{ type = "tmpfs", container = "...", options = "..." }`).
- A **`ports`** entry is `{ local, container }` [+ `protocol`], with port ranges
  written as `"6000-6010"` — see [Port mappings](config-reference.md#port-mappings).
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
see [ConfigVariable](config-reference.md#configvariable) and
[Expressions](config-reference.md#expressions).

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
and Batect at once. See [Includes](config-reference.md#includes) for how paths
resolve, the containment rules for Git bundles, and the shared
`~/.ratect/incl` cache ([`ratect includes`](ratect-cli.md#includes-options)
manages it).

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
registry or npm cache across projects has, until now, had to spell it as a
host path (`local = "~/.cache/cargo"`), which means granting the bundle access
to your home directory — the thing
[`allow_host_paths`](config-reference.md#git-includes) exists to permit and
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

A container's `image` takes [expressions](config-reference.md#expressions), so a
pipeline can choose its image per run without a flag:

```toml
[config_variables.tag]
default = "latest"

[containers.app]
image = "my-repo/my-image:<{tag}"

[containers.tools]
image = "my-repo/tools:${IMAGE_TAG:-latest}"
```

Both forms work: `<{tag}` reads a [`config_variables`](#config-variables) entry
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

A [Git include](config-reference.md#git-includes) fetches configuration from a
repository and merges it into yours. That bundle can declare `include` entries
of its own — and in a `batect.yml` those may be further `type: git` entries,
naming any remote, with the same trust your own includes get.

In `ratect.toml` that is **refused by default**:

```
The bundle 'https://github.com/my-org/infra-bundle.git' at '1.2.3' declares a
Git include of its own ('https://elsewhere.example/other.git'), which would
fetch and run configuration from a remote you have not named. Set
'allow_nested_git_includes' to true on that bundle's own include entry to
accept this.
```

You chose the bundle; you did not choose whatever it decides to pull in next,
and that choice can change under you the next time the ref moves. Opt in per
bundle:

```toml
[[include]]
type = "git"
repo = "https://github.com/my-org/infra-bundle.git"
ref = "1.2.3"
allow_nested_git_includes = true
```

The entry doesn't have to be in the `ratect.toml` itself — a native project can
[include](#includes) a local `.yml`, and an entry declared there is just as much
your own configuration, spelled `allow_nested_git_includes: true`. What makes a
file yours is that it was not reached through a Git include, not its extension.

**The grant is one level deep.** It admits that bundle's own Git includes; it
does not let *those* bundles declare further ones. Like
[`allow_host_paths`](config-reference.md#git-includes), it counts only in
configuration you control — written inside a Git-included file it is ignored,
so a bundle can neither grant itself the permission nor pass on the one you
gave it. If a bundle genuinely needs a chain deeper than that, include the
second repository yourself, where you can see it.

**Declare it in your root file.** A repository is read once however many entries
name it, so if another bundle also pulls in the same repository, the first entry
reached decides what it may do — and this grant, on a losing entry, would do
nothing at all. Root-file entries are always reached first. Where two entries
name the same repository and the losing one carries a grant, Ratect refuses to
load and names the repository, rather than dropping it silently; the same rule
covers [`allow_host_paths`](config-reference.md#git-includes).

That is a different case from the paragraph above, which two words could easily
blur. A grant written *inside* a bundle is **ignored** — accepted by the parser
and worth nothing, because honouring it would let a bundle grant itself. A
grant written in your own configuration that loses the race above is
**refused** — the load stops, because you wrote something that cannot take
effect and nothing else would tell you.

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

## Field reference

Every container and task field from [`config-reference.md`](config-reference.md)
applies, with the same meaning except where [Where the semantics
differ](#where-the-semantics-differ) says otherwise. Scalars, string maps
(`environment`, `labels`, `build_args`, …) and scalar lists
(`capabilities_to_add`, `additional_hostnames`, …) are a direct 1:1 spelling;
the only fields whose *shape* differs are the object-per-entry lists above.
The container fields, by area:

| Area | Fields | Semantics |
| --- | --- | --- |
| Image | `image`, `image_pull_policy`, `build_directory`, `dockerfile`, `build_target`, `build_args`, `build_secrets`, `build_ssh` | [Image building](config-reference.md#image-building) |
| Mounts | `volumes` (host / `cache` / `tmpfs`) | [Volumes](config-reference.md#volume-path-resolution), [caches](config-reference.md#cache-volumes), [tmpfs](config-reference.md#tmpfs-mounts). A cache also takes [`scope`](#shared-caches) *(native only)* — the linked section describes project-keyed storage, which `scope = "shared"` deliberately does not use. |
| Runtime | `command`, `entrypoint`, `working_directory`, `environment`, `enable_init_process`, `privileged`, `shm_size`, `capabilities_to_add`, `capabilities_to_drop`, `devices`, `labels`, `log_driver`, `log_options` | [Container](config-reference.md#container) |
| Networking | `ports`, `additional_hostnames`, `additional_hosts`, `dependencies` | [Ports](config-reference.md#port-mappings), [readiness](config-reference.md#dependency-readiness) |
| Readiness | `health_check`, `setup_commands` | [Dependency readiness](config-reference.md#dependency-readiness) |
| User | `run_as_current_user` | [User mapping](config-reference.md#user-mapping) |
| Inheritance | `extends` | [above](#extends-inheritance-instead-of-yaml-anchors) *(native only)* |

### Where the semantics differ

Almost nothing: the two formats parse into the same model, so a field means
what [`config-reference.md`](config-reference.md) says it means. The
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
| `image` alongside a build-only field (`build_args`, `build_target`, `dockerfile`, `build_secrets`, `build_ssh`) | Rejected when the file loads | Allowed and **ignored**, for the same inheritance reason — a child overriding a build with an `image` still carries the parent's build fields |

The last row is the one to watch: setting `build_secrets` or `build_ssh` on a
container that also has an `image` does nothing at all, and the native format
cannot tell you so without forbidding the override above. If a build field
looks like it is being ignored, check whether the container resolves to an
`image`.

Task fields: `run` (a [`TaskRun`](config-reference.md#taskrun) table —
`container`, `command`, `entrypoint`, `environment`, `ports`,
`working_directory`), `prerequisites`, `dependencies`, `description`, `group`,
and `customise` (see [Task](config-reference.md#task)). A task needs at least one
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
schema](config-reference.md#editor-autocompletion-and-validation): the same
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
| List entries | string shorthand *or* object | object (inline table or `[[...]]`) |
| Local overrides | `batect.local.yml` | `ratect.local.toml` |
| Git bundle default | `batect-bundle.yml` | `ratect-bundle.toml`, then `batect-bundle.yml` |
| Includes | YAML | TOML or YAML, by extension |

Most field *meanings* are unchanged; the spelling and the format-level rules
above are the bulk of the difference. The exceptions are the native-only
fields (`extends`, a cache's `scope`) and the handful of behaviours in [Where
the semantics differ](#where-the-semantics-differ), which exist because
`extends` gives some combinations a meaning `batect.yml` has no way to
express.
