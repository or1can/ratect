# Worked Examples

A real, minimal hello-world project for five common ecosystems — Rust, Go,
Node.js, Python, and the JVM — each with `build`, `test`, `run`, `lint`, and
`shell`
tasks using that ecosystem's own usual tooling and base image. `shell` starts an
interactive shell in the build environment — no config needed beyond the task
itself, since [interactive mode](ratect-compat-config-reference.md#interactive-mode) is
automatic. It's the fastest way to poke around when a build environment isn't
behaving as expected. The config below each intro is included directly from
the actual project, checked into this repository under
[`examples/`](https://github.com/or1can/ratect/tree/main/examples) — what you
read here is exactly what a CI job in this repository runs against a real
Docker daemon on every change, so it can't silently drift out of date. Clone
the whole directory to run one yourself, or copy the config and adjust the
commands to your own scripts.

Four of the five are `ratect.toml`; the same containers and tasks work
identically as `batect.yml` with `ratect-compat` (see [`ratect config
convert`](ratect-cli.md#config) to translate one to the other) — the JVM
example below is `batect.yml` itself, for reasons its own section explains.
See the
[configuration reference](ratect-config-reference.md) for what each field
does, and [Getting Started](getting-started.md) if you haven't run a task
before.

Every `command` below that chains two steps with `&&` wraps them in `sh -c
'...'`. `command` is [tokenized into literal argv, with no shell
involved](ratect-compat-config-reference.md#taskrun) — an unwrapped `&&` doesn't fail
loudly, it's just handed to the first program as a literal extra argument,
which several package managers silently ignore rather than reject. The
[FAQ](faq.md#why-doesnt--or-var-work-in-my-command) walks through it, including
the config-time `$VAR` syntax that looks the same and isn't.

## Try it

Cloning [`examples/rust`](https://github.com/or1can/ratect/tree/main/examples/rust)
and running `ratect tasks list` there prints:

<!-- verify: cargo run -q -p ratect -- tasks list -f examples/rust/ratect.toml -->
```
Tasks in example-rust:

Checks:
- lint: Run clippy with warnings denied

Development:
- build: Compile the project
- run: Run the application
- test: Run the test suite

Utilities:
- shell: Start a shell in the build environment
```

`ratect run` followed by any task name (`build`, `test`, `run`, `lint`, or `shell`)
then actually runs it — see [Getting
Started](getting-started.md#2-run-a-task) for what that output looks like
against a real Docker daemon.

## Rust

[`examples/rust`](https://github.com/or1can/ratect/tree/main/examples/rust).
The official `rust` image doesn't ship the `clippy` component by default, so
`lint` adds it before running; the Cargo registry and the `target` build
output are each cached separately, so only the first run is cold.

```toml
{{#include ../examples/rust/ratect.toml}}
```

## Go

[`examples/go`](https://github.com/or1can/ratect/tree/main/examples/go).
`lint` uses `go vet`, part of the standard toolchain already in the `build-env`
image — no second tool or container needed for a minimal project like this
one.

```toml
{{#include ../examples/go/ratect.toml}}
```

## Node.js

[`examples/node`](https://github.com/or1can/ratect/tree/main/examples/node).
A TypeScript project with `build`/`test`/`lint` npm scripts (`tsc`, `vitest`,
`eslint`). Each task runs `npm ci` itself rather than relying on a separate
install step — a task's own container is recreated from scratch every run,
so nothing installed by one task's container is there for the next one's;
only what lands in the bind-mounted project directory or a `cache` mount
survives. The npm cache mount is what keeps a repeated `npm ci` fast.

```toml
{{#include ../examples/node/ratect.toml}}
```

## Python

[`examples/python`](https://github.com/or1can/ratect/tree/main/examples/python).
A plain script rather than a packaged library, kept deliberately simple;
`build` byte-compiles it as a minimal stand-in for a real build step (there
isn't a natural one for a script with no compiled artifact). Same reasoning
as the Node example above for why `test`/`lint` each reinstall: the `pip`
cache mount is what makes that cheap.

```toml
{{#include ../examples/python/ratect.toml}}
```

## JVM (Gradle)

[`examples/jvm`](https://github.com/or1can/ratect/tree/main/examples/jvm).
Written as `batect.yml` rather than `ratect.toml`, deliberately: Batect
itself is a Gradle/Kotlin project, and JVM/Gradle is where its own
conventions and sample projects skew, so an existing Batect-using JVM shop
is likely migrating a `batect.yml` that already looks close to this one —
`ratect-compat` reads it unchanged. The task set relies on the project's own
checked-in Gradle wrapper (`./gradlew`), so the image itself needs nothing
but a JDK; `lint` runs [Checkstyle](https://checkstyle.org), the traditional
choice, via its own Gradle task rather than a separate tool.

```yaml
{{#include ../examples/jvm/batect.yml}}
```

Only `batect.yml` ships in `examples/jvm/` — the same containers and tasks
would re-spell directly into `ratect.toml` with no behavior change (run
[`ratect config convert`](ratect-cli.md#config) to generate one), see
[`extends`/list-entry shape](ratect-config-reference.md#one-shape-per-list-entry)
for the syntax differences you'd see in the result.
