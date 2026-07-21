# CLI Reference

```
ratect [OPTIONS] [TASK_NAME] [-- ADDITIONAL_ARGS...]
```

This reflects the flags Ratect actually implements today (`src/main.rs`), not the full
Batect CLI — see [differences from Batect](differences-from-batect.md) for what's
missing.

## Options

| Flag | Short | Default | Description |
|---|---|---|---|
| `--config-file <PATH>` | `-f` | `batect.yml` | Path to the configuration file to load. |
| `--list-tasks` | `-T` | — | List all tasks defined in the config file, then exit. Doesn't run anything. |
| `--config-var <NAME=VALUE>` | — | — | Sets a [config variable](config-reference.md#configvariable)'s value; repeatable. Takes precedence over `--config-vars-file` and the variable's `default`. |
| `--config-vars-file <PATH>` | — | — | A flat YAML file of config variable `name: value` pairs, in the same format as `batect.yml` itself. Lower precedence than `--config-var`. |
| `--use-network <NAME>` | — | — | Reuses an existing Docker network for every task in this invocation instead of creating (and removing) a fresh one per task. Errors clearly if the named network doesn't exist. See [task lifecycle](task-lifecycle.md). |
| `--disable-ports` | — | — | Disables publishing of any container's `ports` to the host, regardless of what's configured. |
| `--no-proxy-vars` | — | — | Don't propagate proxy-related environment variables (`http_proxy`, `https_proxy`, `ftp_proxy`, `no_proxy`) to image builds or containers. See [Proxy environment variables](config-reference.md#proxy-environment-variables). |
| `--skip-prerequisites` | — | — | Don't run the named task's own `prerequisites`. Only ever affects the task actually named on the command line — if that task is itself reached as someone else's prerequisite in a later invocation, this flag has no bearing on that. |
| `--override-image <CONTAINER=IMAGE>` | — | — | Overrides the image used by `CONTAINER`; repeatable. Replaces the container's `image`/`build_directory` and `image_pull_policy` entirely — the override is always pulled under the default `IfNotPresent` policy, regardless of what the container itself configures. Errors immediately if `CONTAINER` isn't defined in the config. |
| `--tag-image <CONTAINER=TAG>` | — | — | Tags the image built by `CONTAINER` with `TAG`, in addition to the default `<project_name>-<container_name>` tag; repeatable, and `CONTAINER` may be given more than once to apply multiple tags. Only valid for a container that actually builds an image — errors immediately if `CONTAINER` ends up using a pulled image (whether configured that way or via `--override-image`), and errors once the whole task (and its prerequisites) finishes if `CONTAINER` never actually ran. |
| `--no-cleanup-after-failure` | — | — | If an infrastructure error occurs (a build/pull/health-check/setup-command failure, or anything else before the task's own container gets to run), leave every container and network created for that task in place instead of removing them, so the issue can be investigated. A task's own container exiting non-zero is *not* "failure" for this purpose — see `--no-cleanup-after-success`. |
| `--no-cleanup-after-success` | — | — | If the task's own container runs to completion — regardless of its exit code — leave every container and network created for that task in place instead of removing them. |
| `--no-cleanup` | — | — | Equivalent to providing both `--no-cleanup-after-failure` and `--no-cleanup-after-success`. |
| `--enable-buildkit` | — | — | Use BuildKit for image builds, taking precedence over the `DOCKER_BUILDKIT` environment variable — see [config reference](config-reference.md#image-building). No `--disable-buildkit` counterpart; force the classic builder via `DOCKER_BUILDKIT=0`/`false` instead. |
| `--docker-host <HOST>` | — | — | Docker host to connect to, e.g. `unix:///var/run/docker.sock` or `tcp://1.2.3.4:5678`. Defaults to the `DOCKER_HOST` environment variable, then Docker's own platform default (a Unix socket or Windows named pipe). Cannot be combined with `--docker-context`. |
| `--docker-context <NAME>` | — | — | Docker CLI context to connect through — read from the Docker CLI's own context store (`~/.docker/contexts/`, or `--docker-config`'s directory). Defaults to the `DOCKER_CONTEXT` environment variable, then the Docker CLI's own active context (`~/.docker/config.json`'s `currentContext`). Cannot be combined with `--docker-host`. Errors clearly if the named context doesn't exist in the store. |
| `--docker-config <PATH>` | — | — | Directory containing the Docker CLI's own configuration files (context store, `config.json`). Defaults to the `DOCKER_CONFIG` environment variable, then `~/.docker`. |
| `--docker-tls` | — | — | Use TLS when connecting to the Docker host. Behaves identically to `--docker-tls-verify` — the daemon's certificate is always fully verified; there is no way to skip verification. Cannot be combined with `--docker-context`. |
| `--docker-tls-verify` | — | — | Use TLS when connecting to the Docker host, verifying its certificate. Defaults to the `DOCKER_TLS_VERIFY` environment variable. Cannot be combined with `--docker-context`. |
| `--docker-cert-path <PATH>` | — | — | Directory containing `ca.pem`/`cert.pem`/`key.pem` to authenticate to the Docker host and verify it, unless overridden individually by `--docker-tls-ca-cert`/`-cert`/`-key`. Defaults to the `DOCKER_CERT_PATH` environment variable, then `~/.docker`. Cannot be combined with `--docker-context`. |
| `--docker-tls-ca-cert <PATH>` | — | — | Path to the TLS CA certificate file used to verify the Docker host's own certificate. Defaults to `ca.pem` in `--docker-cert-path`'s directory. Cannot be combined with `--docker-context`. |
| `--docker-tls-cert <PATH>` | — | — | Path to the TLS certificate file used to authenticate to the Docker host. Defaults to `cert.pem` in `--docker-cert-path`'s directory. Cannot be combined with `--docker-context`. |
| `--docker-tls-key <PATH>` | — | — | Path to the TLS key file used to authenticate to the Docker host. Defaults to `key.pem` in `--docker-cert-path`'s directory. Cannot be combined with `--docker-context`. |
| `--output <STYLE>` | `-o` | auto | Forces a particular output style for Ratect's own progress reporting: `fancy` (a live-updating status block, one line per container), `simple` (plain, append-only milestone lines), `quiet` (error messages only, and a machine-readable `--list-tasks` format), or `all` (line-by-line output from *every* container, prefixed with its name — the only style that changes what the task command's own output looks like; the others never touch it) — see [Output styles](#output-styles). Unset means auto-select: `fancy` on an interactive console, `simple` otherwise. |
| `--no-color` | — | — | Disables colored output from Ratect itself (task command output is never affected). Colors are already skipped automatically when stdout isn't a terminal, so this only matters on an interactive console. Also makes `simple` the auto-selected output style. |
| `--help` | `-h` | — | Print help (auto-generated by `clap`). |
| `--version` | `-V` | — | Print the Ratect version. |

## Output styles

`--output`/`-o` controls how Ratect reports its own progress on stdout — never what
the task's command itself prints, which always streams through unmodified. The
styles are Batect's own four, all implemented:

- **`fancy`** — a live status block, one line per container in the task's
  dependency graph (`<name>: <what it's doing right now>` — pulling/building with
  live progress detail, waiting for dependencies, starting, waiting to become
  healthy, running setup commands, ready), repainted in place as events arrive.
  There is no spinner — the animation is purely rewriting changed lines, exactly
  like Batect. The moment the task's own container starts, the block freezes
  behind a blank line and the container's raw output streams below it untouched;
  after it exits, a single live `Cleaning up: ...` countdown line tracks
  teardown, then makes way for the final summary line. Lines are clipped to the
  terminal's current width. Requires an interactive console — an explicit
  `-o fancy` without one fails up front with a clear error (Batect instead
  accepts it and crashes on the first repaint). Works with
  [`--no-color`](#options) (the repaint stays; bold/color go — a combination
  Batect rejects).
- **`simple`** — plain, append-only milestone lines: `Running <task>...`,
  `Pulling <image>...`/`Pulled <image>.`, `Building <container>...`/`Built
  <container>.`, dependency start/health/setup-command milestones, a blank line +
  `Cleaning up...`, and a final `<task> finished with exit code <n> in
  <duration>.` summary (the exit code green/red on a color-capable console). No
  live-updating progress detail at all — safe for CI logs and redirected output.
- **`quiet`** — no milestone lines at all: stdout is exactly the containers' own
  output, so it's safe to pipe (error reporting stays on stderr, unchanged). Also
  switches `--list-tasks` to a machine-readable format: one task per line, sorted
  by name, as `name` alone or `name<TAB>description` — no header, no
  [grouping](config-reference.md#list-tasks-output).
- **`all`** — every line of output prefixed with the container it belongs to
  (`name    | `, padded to a common column, each container's prefix in its own
  color), interleaved as it happens. The only style that shows *dependency*
  containers' stdout/stderr, setup-command output (`Setup command N | ...`), and
  full image-build output (`Image build | ...`) — everything the other styles
  discard. In exchange, no container is interactive in this mode: the task
  container gets no TTY and no stdin, and every container gets `TERM=dumb`
  (matching Batect — a full-screen program can't render into line-prefixed
  output). Task-level lines (the `Running <task>...` preamble, `Cleaning up...`,
  the summary) carry the task's own name as their prefix.

When `--output` isn't given, Ratect auto-selects: `fancy` on an interactive
console (stdout a real terminal, `TERM` set and not `dumb`, terminal size
queryable, no `--no-color`); `simple` otherwise. `quiet` and `all` are never
auto-selected.

## Positional arguments

| Argument | Description |
|---|---|
| `TASK_NAME` | The name of the task to run, as defined under `tasks:` in the config file. Optional — if omitted (and `--list-tasks` isn't given), Ratect logs a warning and exits without doing anything. |
| `-- ADDITIONAL_ARGS...` | Anything after a literal `--` is appended as literal argv entries after the task's own tokenized `command` — see below. Only applies to the task named on the command line, never to its prerequisites. |

## Examples

```bash
# List tasks defined in ./batect.yml
ratect --list-tasks

# Run a task from ./batect.yml
ratect test

# Use a config file in a different location
ratect -f ./ci/batect.yml build

# Pass extra arguments through to the task's command
ratect test -- --verbose some/specific/file.rs

# Set a config variable referenced via `<name`/`<{name}` in `environment`
ratect --config-var environment_name=staging test

# Load config variable values from a file instead
ratect --config-vars-file ./ci/config-vars.yml test
```

### Using ADDITIONAL_ARGS in a task command

`run.command` is tokenized into literal argv (quote/backslash-aware whitespace
splitting, no shell involved — matching Batect's own tokenizer exactly), and anything
after `--` is appended as further literal argv entries — no special syntax needed in
`command` itself to receive them:

```yaml
tasks:
  test:
    run:
      container: build-env
      command: cargo test
```

Running `ratect test -- --nocapture` here runs `cargo test --nocapture` inside the
container. Args are appended as literal argv entries (never concatenated into the
command string and re-parsed), so they're safe even if they contain characters that
would be shell metacharacters elsewhere, like `;`, `&&`, or backticks — Ratect never
passes `command`/`ADDITIONAL_ARGS` through a shell at all.

If the task's container has no `command` at all, `ADDITIONAL_ARGS` (when given) are
passed directly as the container's entrypoint arguments instead, matching plain
`docker run <image> <args>`.

## Exit codes and error reporting

Ratect uses a plain `0` (success) / non-zero (failure) convention, but note the current
actual behavior — it doesn't yet distinguish "nothing to do" from "success":

- Running with no task name at all (and not `--list-tasks`) currently **exits `0`** —
  Ratect logs a warning but doesn't fail the process. This is a rough edge, not
  intentional design; don't rely on it in scripts.
- A missing or malformed config file (fails to parse), a task/container referenced by
  name that doesn't exist, or a dependency cycle all cause a non-zero (`1`) exit. The
  error is printed to stderr as `Error: <message>` — deliberately *not* through
  `tracing::error!`/`RUST_LOG` (which every other diagnostic goes through — see
  [how it works](how-it-works.md#5-logging-vs-output)): a fatal error is the reason the
  process is about to exit non-zero, not an optional diagnostic, so it stays visible
  even under `RUST_LOG=off` or a filter that excludes Ratect's own target — including
  under [`-o quiet`](#output-styles), whose whole contract is "only error messages".
- A misspelled task name (whether given directly on the command line, or as a
  [`prerequisites`](config-reference.md#task) entry) gets a `Did you mean 'x'?`
  suggestion appended to the error, for every existing task name within a Levenshtein
  edit distance of 3 — ported from Batect's own `TaskSuggester`/`EditDistanceCalculator`
  (confirmed by reading Batect's source). Multiple equally-close matches are all
  suggested, e.g. `Did you mean 'build' or 'bulid'?` — Batect's own implementation
  can silently drop one of two equally-close suggestions (its sorting comparator
  doubles as its de-duplication key), which Ratect's deliberately doesn't replicate.
- **A failing command *inside* the container fails the `ratect` process too, with the
  same exit code.** Ratect waits for the container to exit and inspects its status —
  a task whose command is `exit 42` makes `ratect` itself exit `42`, matching
  `docker run`'s convention rather than collapsing every failure to a generic `1`. A
  task that runs as a [prerequisite](config-reference.md#task) and fails this way
  stops the rest of the chain immediately — no other prerequisites, and not the task
  that depended on it, will run — matching
  [Batect's documented behavior](https://github.com/batect/batect.dev/blob/main/docs/reference/config/tasks.md#prerequisites).

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | Controls log verbosity on stderr (`error`, `warn`, `info` [default], `debug`, `trace`). See [how it works](how-it-works.md#5-logging-vs-output). |

Ratect supports interpolating host environment variables and config variables into
`environment` values, volume host paths, `build_directory`, and `build_args` in
`batect.yml` (`$VAR`, `${VAR:-default}`, `<name` — see
[config reference](config-reference.md#expressions)), but not yet within fields that
don't exist yet (`build_secrets.path`, `build_ssh.paths`) — see
[differences from Batect](differences-from-batect.md).
