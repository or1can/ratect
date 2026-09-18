# Worked Examples

A real `ratect.toml` for five common ecosystems — Rust, Go, Node.js, Python, and
the JVM — each with `build`, `test`, and `lint` tasks using that ecosystem's own
usual tooling and base image. Copy the one closest to your project and adjust
the commands to match your own scripts; everything else (the volume mounts, the
`cache` mounts, the task/container shape) carries over unchanged.

Every example below is `ratect.toml`; the same containers and tasks work
identically as `batect.yml` with `ratect-compat` (see [`ratect config
convert`](ratect-cli.md#config) to translate one to the other). See the
[configuration reference](ratect-config-reference.md) for what each field
does, and [Getting Started](getting-started.md) if you haven't run a task
before.

## Rust

A `Cargo.toml`-based project. The official `rust` image doesn't ship the
`clippy` component by default, so `lint` adds it before running; the Cargo
registry and the `target` build output are each cached separately, so only
the first run is cold.

```toml
project_name = "example-rust"

[containers.build-env]
image = "rust:1.90"
working_directory = "/code"
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "cargo-registry", container = "/usr/local/cargo/registry" },
    { type = "cache", name = "cargo-target", container = "/code/target" },
]

[tasks.build]
description = "Compile the project"
group = "Development"
run = { container = "build-env", command = "cargo build --workspace" }

[tasks.test]
description = "Run the test suite"
group = "Development"
run = { container = "build-env", command = "cargo test --workspace" }

[tasks.lint]
description = "Run clippy with warnings denied"
group = "Checks"
run = { container = "build-env", command = "rustup component add clippy --quiet && cargo clippy --workspace --all-targets -- -D warnings" }
```

## Go

A `go.mod`-based project. `lint` runs in its own container, the official
[`golangci-lint`](https://golangci-lint.run) image rather than a tool
installed into `build-env` — a plain example of a task reaching for exactly
the container it needs instead of one image accumulating every tool any task
might want. Both containers share the module cache, since both `go build` and
`golangci-lint` resolve the same dependencies.

```toml
project_name = "example-go"

[containers.build-env]
image = "golang:1.23"
working_directory = "/code"
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "go-mod", container = "/go/pkg/mod" },
    { type = "cache", name = "go-build", container = "/root/.cache/go-build" },
]

[containers.golangci-lint]
image = "golangci/golangci-lint:v1.61"
working_directory = "/code"
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "go-mod", container = "/go/pkg/mod" },
    { type = "cache", name = "golangci-lint", container = "/root/.cache/golangci-lint" },
]

[tasks.build]
description = "Build every package"
group = "Development"
run = { container = "build-env", command = "go build ./..." }

[tasks.test]
description = "Run the test suite"
group = "Development"
run = { container = "build-env", command = "go test ./..." }

[tasks.lint]
description = "Run golangci-lint"
group = "Checks"
run = { container = "golangci-lint", command = "golangci-lint run" }
```

## Node.js

A TypeScript project with `build`/`test`/`lint` npm scripts already defined in
`package.json` (`tsc`, your test runner, `eslint`). Each task runs `npm ci`
itself rather than relying on a separate install step — a task's own
container is recreated from scratch every run, so nothing installed by one
task's container is there for the next one's; only what lands in the
bind-mounted project directory or a `cache` mount survives. The npm cache
mount is what keeps a repeated `npm ci` fast.

```toml
project_name = "example-node"

[containers.build-env]
image = "node:22"
working_directory = "/code"
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "npm-cache", container = "/root/.npm" },
]

[tasks.build]
description = "Compile TypeScript"
group = "Development"
run = { container = "build-env", command = "npm ci && npm run build" }

[tasks.test]
description = "Run the test suite"
group = "Development"
run = { container = "build-env", command = "npm ci && npm test" }

[tasks.lint]
description = "Run eslint"
group = "Checks"
run = { container = "build-env", command = "npm ci && npm run lint" }
```

## Python

A packaged library (`pyproject.toml`, with `pytest` and `ruff` declared under
a `dev` optional-dependencies group) rather than a bare script — `build`
produces the actual distributable wheel/sdist, the shape most real Python
projects eventually need. Same reasoning as the Node example above for why
`test`/`lint` each reinstall: the `pip` cache mount is what makes that cheap.

```toml
project_name = "example-python"

[containers.build-env]
image = "python:3.12"
working_directory = "/code"
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "pip-cache", container = "/root/.cache/pip" },
]

[tasks.build]
description = "Build the distributable package"
group = "Development"
run = { container = "build-env", command = "pip install build && python -m build" }

[tasks.test]
description = "Run the test suite"
group = "Development"
run = { container = "build-env", command = "pip install -e '.[dev]' && pytest" }

[tasks.lint]
description = "Run ruff"
group = "Checks"
run = { container = "build-env", command = "pip install -e '.[dev]' && ruff check ." }
```

## JVM (Gradle)

Written as `batect.yml` rather than `ratect.toml`, deliberately: Batect
itself is a Gradle/Kotlin project, and JVM/Gradle is where its own
conventions and sample projects skew, so an existing Batect-using JVM shop is
likely migrating a `batect.yml` that already looks close to this one —
`ratect-compat` reads it unchanged. The task set relies on the project's own
checked-in Gradle wrapper (`./gradlew`), so the image itself needs nothing
but a JDK; `lint` runs [Checkstyle](https://checkstyle.org), the traditional
choice, via its own Gradle task rather than a separate tool.

```yaml
project_name: example-jvm

containers:
  build-env:
    image: eclipse-temurin:21-jdk
    working_directory: /code
    volumes:
      - local: .
        container: /code
      - type: cache
        name: gradle-cache
        container: /root/.gradle

tasks:
  build:
    description: Compile and assemble
    group: Development
    run:
      container: build-env
      command: ./gradlew assemble

  test:
    description: Run the test suite
    group: Development
    run:
      container: build-env
      command: ./gradlew test

  lint:
    description: Run Checkstyle
    group: Checks
    run:
      container: build-env
      command: ./gradlew checkstyleMain checkstyleTest
```

This one also has a `ratect.toml` form — the same containers and tasks, just
re-spelled — see [`extends`/list-entry
shape](ratect-config-reference.md#one-shape-per-list-entry) for the syntax
differences if you convert it.
