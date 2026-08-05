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
same `<name` / `<{name}` expression syntax as `batect.yml` — expressions are
values inside strings, so they're format-independent and carry across verbatim.
See [ConfigVariable](config-reference.md#configvariable) and
[Expressions](config-reference.md#expressions).

```toml
[config_variables.tag]
default = "latest"
description = "The image tag to run."

[containers.app]
image = "myapp:<{tag}>"                 # a config variable
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

## Field reference

Every container and task field from [`config-reference.md`](config-reference.md)
applies, with the same meaning. Scalars, string maps (`environment`, `labels`,
`build_args`, …) and scalar lists (`capabilities_to_add`, `additional_hostnames`,
…) are a direct 1:1 spelling; the only fields whose *shape* differs are the
object-per-entry lists above. The container fields, by area:

| Area | Fields | Semantics |
| --- | --- | --- |
| Image | `image`, `image_pull_policy`, `build_directory`, `dockerfile`, `build_target`, `build_args`, `build_secrets`, `build_ssh` | [Image building](config-reference.md#image-building) |
| Mounts | `volumes` (host / `cache` / `tmpfs`) | [Volumes](config-reference.md#volume-path-resolution), [caches](config-reference.md#cache-volumes), [tmpfs](config-reference.md#tmpfs-mounts) |
| Runtime | `command`, `entrypoint`, `working_directory`, `environment`, `enable_init_process`, `privileged`, `shm_size`, `capabilities_to_add`, `capabilities_to_drop`, `devices`, `labels`, `log_driver`, `log_options` | [Container](config-reference.md#container) |
| Networking | `ports`, `additional_hostnames`, `additional_hosts`, `dependencies` | [Ports](config-reference.md#port-mappings), [readiness](config-reference.md#dependency-readiness) |
| Readiness | `health_check`, `setup_commands` | [Dependency readiness](config-reference.md#dependency-readiness) |
| User | `run_as_current_user` | [User mapping](config-reference.md#user-mapping) |
| Inheritance | `extends` | [above](#extends-inheritance-instead-of-yaml-anchors) *(native only)* |

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
| List entries | string shorthand *or* object | object (inline table or `[[...]]`) |
| Local overrides | `batect.local.yml` | `ratect.local.toml` |
| Git bundle default | `batect-bundle.yml` | `ratect-bundle.toml`, then `batect-bundle.yml` |
| Includes | YAML | TOML or YAML, by extension |

Field *meanings* are unchanged; only the spelling and these format-level rules
differ.
