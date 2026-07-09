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
| `config_variables` | map of name → [ConfigVariable](#configvariable) | no | Declares the config variables usable via `<name`/`<{name}` [expressions](#expressions) in `environment` values. A name must be declared here before it can be referenced — see [Expressions](#expressions). |

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
| `build_directory` | string | one of `image`/`build_directory` | **Parsed but not yet implemented.** Intended to build an image from a `Dockerfile` in this directory (see [Image Building](../ROADMAP.md#batect-parity) on the roadmap). Running a task against a container that only has `build_directory` set currently just logs a warning and does nothing. |
| `volumes` | list of strings | no | Bind mounts in `host_path:container_path` form. See [Volume path resolution](#volume-path-resolution) below. |
| `dependencies` | list of strings | no | Names of other containers to start (recursively, if they themselves have dependencies) before this one, reachable by name over a Docker network created for the duration of the task. No health-check waiting — a dependency is considered ready as soon as it's started. See [the task lifecycle](task-lifecycle.md) for the full model, and [Differences from Batect](differences-from-batect.md#container-fields) for what's simplified relative to Batect. |
| `environment` | map of string → string | no | Environment variables to set in the container, e.g. `FOO: bar`. Values support [expressions](#expressions) (`$VAR`, `${VAR:-default}`, `<name`). A dependency container only ever gets its own `environment` — see [TaskRun](#taskrun) for how a task's own container's `environment` combines with `run.environment`. |

> **Note:** if a container has *neither* `image` nor `build_directory` set, running a
> task against it currently does nothing and reports success — no error is raised.
> This does **not** apply to a container listed in `dependencies`: a dependency without
> an `image` fails with an error, since it needs to actually run to serve its purpose.

### Volume path resolution

Each entry in `volumes` is split on `:`. Only entries with **exactly two** colon-separated
parts (`host_path:container_path`) are resolved:

- If `host_path` is relative, it's resolved to an absolute path **relative to the
  directory containing the config file** (not the current working directory), at load
  time — once, before any task runs.
- If `host_path` is already absolute, it's left unchanged.
- Entries that don't split into exactly two parts — e.g. a three-part spec like
  `host:container:ro` (Docker's read-only mount flag), or a Windows drive-letter path
  like `C:/data:/code` — are **left completely unresolved**, including the host path.
  Use an absolute host path if you need one of these forms today.

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

`environment` values (on both [Container](#container) and [TaskRun](#taskrun)) support
two kinds of expression, resolved once when the config is loaded — everywhere else in
the config (volume paths, `build_directory`, etc.), a string is used exactly as
written, with no substitution. Literal text around an expression is left untouched
(`"prefix-$VAR-suffix"` interpolates just `$VAR`), and a `$`/`<` not followed by a
valid identifier (or an unterminated `${`/`<{`) is treated as a literal character
rather than an error.

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
