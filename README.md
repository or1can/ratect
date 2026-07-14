# Ratect

[![CI](https://github.com/or1can/ratect/actions/workflows/ci.yml/badge.svg)](https://github.com/or1can/ratect/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

Ratect is a Rust implementation of [Batect](https://github.com/batect/batect).

It aims to be a fast, lightweight, and robust CLI application for defining and running development tasks in Docker containers.

## Status

**Experimental / Work in Progress**

Ratect is currently in early development. It supports a subset of Batect's features.

## Features

- **YAML Configuration**: Uses `batect.yml` to define containers and tasks.
- **Includes**: Splits one project's configuration across multiple local files via a
  top-level `include` list — see
  [config reference](docs/config-reference.md#includes).
- **Docker Integration**: Powered by [bollard](https://github.com/fujiapple86/bollard) for direct Docker API communication.
- **Task Execution**:
  - Prerequisite task handling.
  - Image pulling with progress bars.
  - Volume mounting.
  - Log streaming from containers.
  - Interactive TTY/stdin attachment for a task's own container (e.g. dropping into a
    shell) — automatic whenever run from a real terminal, no config needed.
  - Sidecar/dependency containers, started on a per-task Docker network.
  - Environment variables, on both containers and individual task runs.
  - Image building from a `Dockerfile` via `build_directory`, with `build_args` and
    real `.dockerignore` semantics (not `.gitignore`-compatible — see
    [config reference](docs/config-reference.md#dockerignore-semantics)).
  - User mapping (`run_as_current_user`): runs a container as the host's own
    user/group instead of root, so files it writes to a mounted volume come back
    owned by you, not root.
  - Networking: `--use-network` reuses an existing Docker network instead of a fresh
    one per task; `additional_hostnames`/`additional_hosts` for extra network
    aliases/`/etc/hosts` entries; `ports`/`--disable-ports` for publishing container
    ports to the host.
  - Proxy support: `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` are detected from
    the host environment and propagated into containers and builds automatically
    (`--no-proxy-vars` to disable).
- **Expressions**: `$VAR`/`${VAR:-default}` (host environment) and `<name`/`<{name}`
  (config variables, including Batect's built-in `batect.project_directory`) within
  `environment` values, volume host paths, `build_directory`, and `build_args`.
- **CLI**: Robust command-line interface built with [clap](https://github.com/clap-rs/clap).

## Getting Started

### Prerequisites

- [Docker](https://www.docker.com/)
- [Rust](https://www.rust-lang.org/) (stable)

### Building

```bash
cargo build
```

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

## License

Ratect is licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
