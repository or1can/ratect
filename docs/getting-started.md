# Getting Started

This walks through defining and running your first task with Ratect. It assumes you've
already [installed Ratect](installation.md) and have Docker running.

**Which binary?** Ratect ships two. `ratect` reads its own `ratect.toml` format and
is the one this page uses — if you're new here, it's the one to install. If you
already have a `batect.yml`, `ratect-compat` runs it unchanged: see [Migrating a
Batect Project to Ratect](migrating-batect-project.md) instead, since this page
assumes you're starting from nothing. Nothing below assumes a brand-new codebase
either: writing a `ratect.toml` doesn't touch any of your existing code, build
scripts, or CI — it's one new file describing how to run what you already have
in a container, alongside whatever else the project does today.

Every file and transcript on this page comes from a real project checked into this
repository, [`examples/getting-started`](https://github.com/or1can/ratect/tree/main/examples/getting-started)
— the page builds it up step by step, and a CI job runs every task in it against a
real Docker daemon on every change, so what you read here is what actually runs.
Clone that directory to follow along without typing anything.

## 1. Create a `ratect.toml`

`ratect` reads its configuration from a `ratect.toml` file in the current directory
(or wherever you point `-f`/`--config-file` — see the [CLI reference](ratect-cli.md)).
The smallest file that runs something:

```toml
{{#include ../examples/getting-started/ratect.toml:minimal}}
```

This defines one container (`build-env`, based on the `alpine:3.24` image, with the
current directory mounted at `/code`) and one task (`hello`, which runs `ls /code`
inside that container).

See the [configuration reference](ratect-config-reference.md) for the full schema.

## 2. Run a task

```bash
ratect run hello
```

`ratect run hello -o simple`, captured on a terminal in `examples/getting-started`:

```ansi
{{#include captures/getting-started-first-run.ansi}}
```

`README.md` and `ratect.toml` are `ls /code`'s actual output, streamed live as the
container produces it — in columns, because the container was given a terminal,
exactly as it would be in a shell. Everything else is Ratect's own framing. A second run skips the pull, since the image is already local.

On a terminal, `ratect` defaults to its `fancy` output style, which draws the same
milestones in place as they happen rather than one line at a time; the capture
above asked for `simple` so it could be shown on a page. See the `-o` option in the
[CLI reference](ratect-cli.md#global-options) for the other styles.

## 3. Prerequisites

Tasks can depend on other tasks, which run first:

```toml
{{#include ../examples/getting-started/ratect.toml:prerequisites}}
```

Running `ratect run test` runs `build` to completion, then `test`. Within a single
`ratect run` invocation:

- Each task runs **at most once**, even if it's a prerequisite of more than one other
  task.
- Each container image is **pulled at most once**, even if multiple tasks use it.
- A prerequisite cycle (e.g. `a` depends on `b`, `b` depends on `a`) is detected and
  reported as an error rather than hanging.

See [Task ordering](task-lifecycle.md#task-ordering) for the details.

## 4. Environment variables and expressions

A task's `run` (and a container) can set environment variables, and a value can pull
in a host environment variable or a declared config variable instead of being written
as a literal:

```toml
{{#include ../examples/getting-started/ratect.toml:expressions}}
```

Running `ratect run greet` (with `WHO` unset in your shell) prints `hello-world in
dev`. Override the config variable from the command line instead of relying on its
`default`:

```bash
ratect run greet --config-var environment_name=staging
```

which prints `hello-world in staging`.

Note the `sh -c`: `command` is split into arguments with no shell involved, so
`$GREETING` on its own would reach `echo` as literal text. Ratect's own `${WHO:-world}`
and `<environment_name` expressions are a different thing — resolved by Ratect before
the container starts, in `environment` values and a few other fields, never inside
`command`. See [the FAQ](faq.md#why-doesnt--or-var-work-in-my-command) for the three
`$`-syntaxes that look alike, the [configuration
reference](ratect-config-reference.md#config-variables-and-expressions) for the full
expression syntax (including `batect.project_directory`, always available without
being declared), and the [CLI reference](ratect-cli.md#global-options) for
`--config-var`/`--config-vars-file`.

## 5. List available tasks

With four tasks in the file now, list them:

```bash
ratect tasks list
```

<!-- verify: cargo run -q -p ratect -- tasks list -f examples/getting-started/ratect.toml -->
```
Tasks in getting-started:
- build
- greet
- hello
- test
```

A task's optional `description` and `group` fields turn this flat list into a grouped
one, with a line of help per task — see [Worked Examples](worked-examples.md#try-it)
for what that looks like.

## 6. Reading the output

Ratect separates two kinds of output:

- **stdout**: the actual output of your command — this is what your task produces, and
  what `tasks list` prints. Safe to pipe or redirect.
- **stderr**: Ratect's own diagnostics (task lifecycle messages, warnings, errors),
  logged via [`tracing`](https://docs.rs/tracing). Control verbosity with the
  `RUST_LOG` environment variable, e.g.:

```bash
RUST_LOG=debug ratect run hello
```

`debug` also surfaces low-level Docker API activity (container create/start/remove),
which is useful when troubleshooting.

## Next steps

See [Worked Examples](worked-examples.md) for a real `build`/`test`/`run`/`lint`/`shell`
task set in Rust, Go, Node.js, Python, or the JVM — a faster starting point than
building one up from scratch if your project is in one of those. Once you have a
container running your own toolchain, [Using Ratect With Language
Ecosystems](using-ratect-with.md) covers what to cache and a couple of correctness
gotchas per ecosystem.
