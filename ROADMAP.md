# Ratect Roadmap

This document outlines the planned journey for Ratect, from achieving parity with Batect to implementing Rust-specific enhancements and future innovations.

## Batect Parity

The primary goal is to support the core features of Batect to ensure a seamless transition for existing users.

- **Image Building**: Support for building Docker images from a `Dockerfile` using the `build_directory` configuration.
- **Sidecar Containers**: Ability to start and manage dependency containers (sidecars) for tasks.
- **Docker Networking**: Automatic management of Docker networks for inter-container communication.
- **Interactive Mode**: Support for interactive terminal sessions (TTY and STDIN) for tasks that require user input.
- **Environment Variable Interpolation**: Support for using environment variables in `batect.yml`.
- **Batect Expressions**: Support for dynamic expressions within the configuration for flexible setup.
- **Includes**: Support for splitting configuration across multiple files using the `include` directive.
- **Full Configuration Parity**: Support for all available Batect configuration options and standard YAML structures. See [Differences from Batect](docs/differences-from-batect.md#configuration-format) for the itemized current status of every field.
- **Full CLI Options Parity**: Support for all standard Batect CLI flags and options (e.g., `--config-file`, `--override-image`, cleanup control flags, etc.). See [Differences from Batect](docs/differences-from-batect.md#cli-flags) for the itemized current status of every flag.
- **User Mapping**: Handling of file permissions and user mapping between host and container.
- **Proxy Support**: Automatic detection and injection of proxy settings into containers.

## CLI Evolution

To provide both familiarity for existing users and a modern experience for new ones, the CLI will evolve in two phases:

- **Batect-Compatible CLI (Phase 1)**: Initial focus on providing 1:1 parity with Batect's flag-based interface (e.g., `ratect --list-tasks`, `ratect <task>`). This ensures that existing Batect users can migrate with zero friction.
- **Rust-native CLI (Phase 2)**: Introduction of a modern, subcommand-centric interface (e.g., `ratect tasks list`, `ratect run <task>`) that follows modern Rust CLI conventions. This will include better environment variable integration, improved shell completions, and a more intuitive structure. Once the Rust-native interface is stable, the Batect-compatible interface will be deprecated.

## Rust Enhancements

Leveraging Rust's strengths to provide a superior experience compared to the original JVM-based implementation.

- **Parallel Task Execution**: Utilizing `tokio` to execute independent tasks and prerequisites in parallel, significantly reducing execution time.
- **Static Binaries**: Distribution as a single, zero-dependency static binary for easy installation and portability.
- **First-class Cross-platform Support**: Providing a high-performance, native experience across macOS, Linux, and Windows without the overhead or startup latency of a JVM.
- **Precise Error Reporting**: Utilizing Rust's type system and error handling to provide clear, actionable feedback on configuration errors and execution failures.

## UX & Tooling

Improving the developer experience through better tools and feedback.

- **`ratect doctor`**: A built-in linter and diagnostic tool to validate configuration and environment setup. This will include checks for `latest` image tags, missing health checks on dependencies, and host-container permission issues.
- **Automatic Output Mode Detection**: Automatically enabling or disabling color and fancy output based on terminal capabilities and TTY detection.
- **Improved Progress UI**: A more descriptive and visually appealing progress interface for task execution and image management, including build context upload progress.
- **Watch Mode**: Automatically re-running tasks when source files change.

## Future Vision

Exploring innovative features that go beyond the original Batect, as well as planned improvements from the Batect roadmap.

- **Wildcard Includes**: Support for including multiple files using glob patterns (e.g., `include: containers/*.yaml`).
- **Configuration Merging/Replacement**: Ability to merge or override containers and tasks when including files.
- **Init Containers**: Support for containers that must start, run, and complete before other containers can start (e.g., for database initialization).
- **External Health Checks**: Support for external health checks (e.g., HTTP) that don't require specialized tools like `curl` to be installed within the container.
- **Image Lifecycle Management**: Tools for building and pushing images independently of task execution, and cleaning up unused images.
- **`ulimit` Support**: Support for setting `ulimit` values for containers.
- **Secrets Management**: Integrated support for securely handling sensitive information like API keys and credentials.
- **Plugin System**: A flexible architecture to allow users to extend Ratect's functionality with custom logic.
