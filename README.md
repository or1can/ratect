# Ratect

[![CI](https://github.com/or1can/ratect/actions/workflows/ci.yml/badge.svg)](https://github.com/or1can/ratect/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

Ratect is a from-scratch Rust implementation of
[Batect](https://github.com/batect/batect): define your development tasks (build,
test, lint, a database to develop against, …) once in a `batect.yml`, and run them
identically on any machine that has Docker — no "works on my machine", no
per-developer setup drift.

Batect itself is no longer maintained (the upstream repository was archived in
October 2023). Ratect aims to become a drop-in replacement for its core feature set,
with the startup speed and footprint of a native binary instead of a JVM. It is an
independent project, not affiliated with or endorsed by the original Batect project.

## Status

**Experimental / Work in Progress** — pre-1.0, evolving quickly.

Ratect implements a substantial subset of Batect's features, including:

- Tasks, prerequisites, and dependency/sidecar containers with Batect's real
  readiness gates (health checks, setup commands), on an isolated per-task network.
- Image building — BuildKit by default, matching Batect — with `build_args`, custom
  `dockerfile`/`build_target`, `build_secrets`, `build_ssh`, and faithful
  `.dockerignore` semantics.
- Includes: splitting configuration across local files, and Git-hosted bundles
  shared between projects.
- Batect's expression syntax and config variables, environment variables, volumes,
  port publishing, proxy propagation, user mapping (`run_as_current_user`), and
  automatic interactive TTY attachment.

The itemized, per-field and per-flag status — including known divergences — lives in
[Differences from Batect](docs/differences-from-batect.md), and the direction and
release history in the [Roadmap](ROADMAP.md).

The destination is
[two binaries sharing one core](ROADMAP.md#two-binaries-ratect-and-ratect-compat):
**`ratect-compat`**, a strict, flag-for-flag and field-for-field drop-in replacement
for the (now-unmaintained) `batect` binary, and **`ratect`**, a forward-looking CLI
free to diverge from Batect's interface. Today's single `ratect` binary is where the
parity work lands ahead of that split. Ratect deliberately does not ship a binary
literally named `batect` — anyone who wants their existing `./batect` wrapper script
to keep working symlinks or renames `ratect-compat` themselves.

## Getting Started

### Prerequisites

- [Docker](https://www.docker.com/)
- [Rust](https://www.rust-lang.org/) (stable)

### Building

```bash
cargo build
```

> Note: the workspace currently carries a `[patch.crates-io]` override for
> [bollard](https://github.com/fussybeaver/bollard) (a
> [fork](https://github.com/or1can/bollard) adding BuildKit session support, which
> is being contributed upstream). Cargo resolves it automatically — no extra setup.

### Running

To list available tasks:
```bash
cargo run -- --list-tasks
```

To run a specific task:
```bash
cargo run -- <task-name>
```

## Example `batect.yml`

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

## Documentation

Ratect is not a Batect wrapper — its documentation is self-contained and doesn't
assume you've read Batect's docs.

- [Installation](docs/installation.md)
- [Getting Started](docs/getting-started.md)
- [How It Works](docs/how-it-works.md)
- [Task Lifecycle](docs/task-lifecycle.md)
- [CLI Reference](docs/cli-reference.md)
- [Configuration Reference](docs/config-reference.md)
- [Differences from Batect](docs/differences-from-batect.md)
- [Roadmap](ROADMAP.md)

## Contributing & Security

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to get involved, and
[SECURITY.md](SECURITY.md) for how to report a vulnerability privately.

## License

Ratect is licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
details, and [NOTICE](NOTICE) for third-party attributions.
