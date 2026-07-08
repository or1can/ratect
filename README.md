# Ratect

Ratect is a Rust implementation of [Batect](https://github.com/batect/batect).

It aims to be a fast, lightweight, and robust CLI application for defining and running development tasks in Docker containers.

## Status

**Experimental / Work in Progress**

Ratect is currently in early development. It supports a subset of Batect's features.

## Features

- **YAML Configuration**: Uses `batect.yml` to define containers and tasks.
- **Docker Integration**: Powered by [bollard](https://github.com/fujiapple86/bollard) for direct Docker API communication.
- **Task Execution**:
  - Prerequisite task handling.
  - Image pulling with progress bars.
  - Volume mounting.
  - Log streaming from containers.
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
