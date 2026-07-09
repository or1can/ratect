# Ratect Roadmap

This document outlines the planned journey for Ratect, from achieving parity with Batect to implementing Rust-specific enhancements and future innovations.

## Batect Parity

The primary goal is to support the core features of Batect to ensure a seamless transition for existing users. This work targets the [`ratect-compat` binary](#two-binaries-ratect-and-ratect-compat) specifically — the `ratect` binary is not expected to maintain 1:1 Batect parity.

- **Image Building**: Support for building Docker images from a `Dockerfile` using the `build_directory` configuration.
- **Full Docker Networking**: A minimal per-task network for dependency/sidecar containers is implemented (see [the task lifecycle](docs/task-lifecycle.md)); full Batect-equivalent networking (custom drivers, reusing an existing network via `--use-network`, disabling port bindings, etc.) is not.
- **Interactive Mode**: Support for interactive terminal sessions (TTY and STDIN) for tasks that require user input.
- **Environment Variable Interpolation**: Support for using environment variables in `batect.yml`.
- **Batect Expressions**: Support for dynamic expressions within the configuration for flexible setup.
- **Includes**: Support for splitting configuration across multiple files using the `include` directive.
- **Full Configuration Parity**: Support for all available Batect configuration options and standard YAML structures. See [Differences from Batect](docs/differences-from-batect.md#configuration-format) for the itemized current status of every field.
- **Full CLI Options Parity**: Support for all standard Batect CLI flags and options (e.g., `--config-file`, `--override-image`, cleanup control flags, etc.). See [Differences from Batect](docs/differences-from-batect.md#cli-flags) for the itemized current status of every flag.
- **User Mapping**: Handling of file permissions and user mapping between host and container.
- **Proxy Support**: Automatic detection and injection of proxy settings into containers.

## Two Binaries: `ratect` and `ratect-compat`

Rather than one binary evolving through phases and eventually deprecating Batect
compatibility, the plan is a Cargo workspace with a shared core library
(config parsing, task engine, `ContainerRuntime`/Docker integration) and two thin
binary crates built on top of it:

- **`ratect-compat`**: A strict, literal, flag-for-flag and field-for-field match for
  Batect's CLI and `batect.yml` format. This is where all of the [Batect Parity](#batect-parity)
  work lands, scoped precisely by the itemized tables in
  [Differences from Batect](docs/differences-from-batect.md). Its only job is being a
  boring, reliable drop-in replacement for the (now-unmaintained) `batect` binary — it
  is not the place for new ideas.

  Ratect deliberately does **not** ship a binary literally named `batect` (that would be
  confusing, and edges toward a trademark/naming concern). Anyone who wants their
  existing `./batect` wrapper script or `PATH` entry to keep working symlinks or renames
  `ratect-compat` to `batect` themselves.

- **`ratect`**: The forward-looking CLI, free to diverge from Batect's interface —
  subcommands (`ratect tasks list`, `ratect run <task>`), better shell completions, and
  other modern-Rust-CLI conventions — without being constrained by parity concerns. This
  is also the binary that would adopt any future alternative configuration format (see
  [Future Vision](#future-vision)); `ratect-compat` stays YAML-only, permanently, since
  that's what Batect compatibility requires.

Because both binaries share the same core, an eventual migration/upgrade path from a
`ratect-compat`-managed project (Batect-format config) to a `ratect`-managed one is a
roadmap goal in its own right, not just a side effect of the split.

## Versioning & Releases

`ratect-compat` and `ratect` are versioned **independently** — they're on different
maturity clocks, and forcing one number to serve both meanings breaks the moment they
diverge (which they will, since `ratect-compat` has a head start). What *is* shared is
the release **process**: a fix in the shared core crate gets released for both binaries
at the same time (one PR/tag/CI run), each bumping its own patch version independently
— not the same version number, just released together, so nobody is left running a
stale, unpatched core. The core crate itself isn't published or meaningfully versioned
on its own; it's an internal implementation detail, not something either binary's users
interact with directly.

Mechanically, each `Cargo.toml` (`ratect`'s and `ratect-core`'s) sits at `X.Y.Z-dev`
between releases. Cutting a release is one isolated `chore:` commit that bumps both to
the plain `X.Y.Z` being released and moves `CHANGELOG.md`'s accumulated `Unreleased`
entries under a new dated `## [X.Y.Z]` header. The next commit — starting the following
version's development, also isolated, also `chore:` — bumps both back to the next
`X.Y.Z-dev`. Neither bump is ever folded into a feature commit.

### `ratect-compat`

- **0.1.0** — not a features milestone, an *honesty* milestone: the engine (prerequisites,
  cycle detection, dedup, sidecars) is already solid, but a few known gaps in
  [Differences from Batect](docs/differences-from-batect.md) currently make the tool's
  output untrustworthy rather than just incomplete, and should be fixed before anything
  is tagged:
  - ~~Container exit codes aren't checked~~ — fixed: a task whose command exits
    non-zero now fails `ratect` itself with that exact exit code, and stops the rest
    of a prerequisite chain, matching Batect.
  - ~~A missing config file exits `0` instead of failing~~ — fixed: it now fails fast
    with a non-zero exit, for both `--list-tasks` and running a task.
  - ~~`-- ADDITIONAL_ARGS` is parsed but silently dropped~~ — fixed: forwarded as
    `sh -c`'s positional parameters (`$1`, `$2`, `$@`), scoped to only the
    explicitly-requested task, never its prerequisites.
  - ~~Unsupported config keys are silently ignored rather than rejected~~ — fixed:
    every config struct now denies unknown fields, so a config using a field Ratect
    doesn't yet support fails to load with an error naming the field, instead of
    silently loading with that field ignored.
- **0.2.0** — **Environment Variables** (the `environment` field on containers/tasks)
  together with **Batect Expressions**/config variables (`$VAR`, `${VAR:-default}`,
  config variables via `<name`). Bundled deliberately, not a grab-bag: interpolation is
  the one shared mechanism both environment variables and config variables need to be
  useful, and later fields like `build_args` (0.3.0) depend on it too.
- **0.3.0** — **Image Building** (`build_directory` currently just warns and no-ops),
  including `build_args` interpolation from 0.2.0.
- **0.4.0** — **Interactive Mode** (TTY/STDIN attachment for tasks that need user input).
- **0.5.0** — **User Mapping** (`run_as_current_user`).
- **0.6.0** — **Full Docker Networking** and **Proxy Support** together — proxy
  injection is fundamentally "set environment variables automatically," so it benefits
  from 0.2.0's environment variable support already existing.
- **Beyond 0.6.0** — not yet planned release-by-release, but not optional for 1.0.0
  either: **Includes** (splitting config across files/bundles), and the long tail of
  smaller [Full Configuration](docs/differences-from-batect.md#container-fields) /
  [Full CLI](docs/differences-from-batect.md#cli-flags) parity items (`health_check`,
  `setup_commands`, `ports`, `labels`, `--skip-prerequisites`, `--override-image`, etc.)
  that 0.2.0–0.6.0 don't touch.
- **1.0.0** — the [Batect Parity](#batect-parity) section above substantially checked
  off (all of the above, not just the six headline items through 0.6.0), and verified
  against a handful of real Batect projects, not just the itemized field/flag tables
  passing in isolation. Not tagged early for appearances — earned once `ratect-compat`
  can honestly replace `batect` on real projects.

### `ratect`

Hasn't started yet — see [Two Binaries](#two-binaries-ratect-and-ratect-compat). Its
**1.0.0** means something different from `ratect-compat`'s: interface stability (the
subcommand structure and config format won't break), not feature-completeness against
Batect.

## Rust Enhancements

Leveraging Rust's strengths to provide a superior experience compared to the original JVM-based implementation.

- **Parallel Task Execution**: Utilizing `tokio` to execute independent tasks and prerequisites in parallel, significantly reducing execution time.
- **Static Binaries**: Distribution as zero-dependency static binaries (`ratect` and `ratect-compat`) for easy installation and portability.
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

- **Alternative Configuration Format (TOML)**: **Undecided, exploratory.** TOML is a more typical configuration format for Rust projects than YAML. If pursued, this would apply only to the [`ratect` binary](#two-binaries-ratect-and-ratect-compat) — `ratect-compat` stays YAML-only for Batect compatibility — and would need a migration path for projects moving from `ratect-compat`'s YAML config.
- **Wildcard Includes**: Support for including multiple files using glob patterns (e.g., `include: containers/*.yaml`).
- **Configuration Merging/Replacement**: Ability to merge or override containers and tasks when including files.
- **Init Containers**: Support for containers that must start, run, and complete before other containers can start (e.g., for database initialization).
- **External Health Checks**: Support for external health checks (e.g., HTTP) that don't require specialized tools like `curl` to be installed within the container.
- **Image Lifecycle Management**: Tools for building and pushing images independently of task execution, and cleaning up unused images.
- **`ulimit` Support**: Support for setting `ulimit` values for containers.
- **Secrets Management**: Integrated support for securely handling sensitive information like API keys and credentials.
- **Plugin System**: A flexible architecture to allow users to extend Ratect's functionality with custom logic.
