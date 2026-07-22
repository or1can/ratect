# Getting Started

This walks through defining and running your first task with Ratect. It assumes you've
already [installed Ratect](installation.md) and have Docker running.

## 1. Create a `batect.yml`

Ratect reads its configuration from a `batect.yml` file in the current directory (or
wherever you point `-f`/`--config-file` — see the [CLI reference](cli-reference.md)).

```yaml
project_name: my-project
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - .:/code
tasks:
  test:
    run:
      container: build-env
      command: ls /code
```

This defines one container (`build-env`, based on the `alpine:3.18` image, with the
current directory mounted at `/code`) and one task (`test`, which runs `ls /code`
inside that container).

See the [configuration reference](config-reference.md) for the full schema.

## 2. List available tasks

```bash
ratect-compat --list-tasks
```

```
Tasks in my-project:
- test
```

## 3. Run a task

```bash
ratect-compat test
```

The first run pulls the `alpine:3.18` image (printing "Pulling alpine:3.18..." /
"Pulled alpine:3.18." around it), then creates, starts, and runs the container.
Whatever the container writes to stdout/stderr is streamed live and printed as-is —
that's the actual output of your task — framed by Ratect's own progress lines
("Running test...", then a "finished with exit code 0" summary).

## 4. Prerequisites

Tasks can depend on other tasks, which run first:

```yaml
tasks:
  build:
    run:
      container: build-env
      command: echo "building..."
  test:
    run:
      container: build-env
      command: echo "testing..."
    prerequisites:
      - build
```

Running `ratect-compat test` runs `build` first, then `test`. Within a single
`ratect-compat` invocation:

- Each task runs **at most once**, even if it's a prerequisite of more than one other
  task.
- Each container image is **pulled at most once**, even if multiple tasks use it.
- A prerequisite cycle (e.g. `a` depends on `b`, `b` depends on `a`) is detected and
  reported as an error rather than hanging.

See [how it works](how-it-works.md) for the details.

## 5. Environment variables and expressions

Containers and individual task runs can set environment variables, and their values
can pull in a host environment variable or a declared config variable instead of being
written as a literal:

```yaml
config_variables:
  environment_name:
    default: dev
containers:
  build-env:
    image: alpine:3.18
    environment:
      GREETING: "hello-${WHO:-world}"
tasks:
  test:
    run:
      container: build-env
      command: echo "$GREETING in $ENVIRONMENT_NAME"
      environment:
        ENVIRONMENT_NAME: <environment_name
```

Running `ratect-compat test` (with `WHO` unset in your shell) prints `hello-world in
dev`. Override the config variable from the command line instead of relying on its
`default`:

```bash
ratect-compat --config-var environment_name=staging test
```

See the [configuration reference](config-reference.md#expressions) for the full
expression syntax (including `batect.project_directory`, always available without
being declared) and the [CLI reference](cli-reference.md) for `--config-var`/
`--config-vars-file`.

## 6. Reading the output

Ratect separates two kinds of output:

- **stdout**: the actual output of your command — this is what your task produces, and
  what `--list-tasks` prints. Safe to pipe or redirect.
- **stderr**: Ratect's own diagnostics (task lifecycle messages, warnings, errors),
  logged via [`tracing`](https://docs.rs/tracing). Control verbosity with the
  `RUST_LOG` environment variable, e.g.:

```bash
RUST_LOG=debug ratect-compat test
```

`debug` also surfaces low-level Docker API activity (container create/start/remove),
which is useful when troubleshooting.
