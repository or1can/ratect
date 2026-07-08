# Configuration Reference

Ratect reads a YAML file (`batect.yml` by default) describing containers and tasks.
This documents the schema Ratect actually parses today (`src/config.rs`) — it is a
**subset** of Batect's configuration format. See
[differences from Batect](differences-from-batect.md) for what's not yet supported.

## Top level

```yaml
project_name: my-project
containers:
  <name>: <Container>
tasks:
  <name>: <Task>
```

| Field | Type | Required | Description |
|---|---|---|---|
| `project_name` | string | yes | Used only for display (e.g. in `--list-tasks` output). |
| `containers` | map of name → [Container](#container) | yes | Container definitions, keyed by name. Referenced from tasks via `run.container`. |
| `tasks` | map of name → [Task](#task) | yes | Task definitions, keyed by name. Run by name via `ratect <task-name>`. |

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
| `dependencies` | list of strings | no | **Parsed but not yet used by the engine.** In Batect, this starts other containers and waits for them to become healthy before this one starts (sidecar containers, e.g. a database). Ratect parses the field but doesn't start or wait for anything — see [Differences from Batect](differences-from-batect.md#container-fields). |

> **Note:** if a container has *neither* `image` nor `build_directory` set, running a
> task against it currently does nothing and reports success — no error is raised.

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
| `command` | string | no | Shell command to run inside the container (executed as `sh -c "<command>"`). If omitted, the container's own default `CMD`/`ENTRYPOINT` runs instead. |

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
