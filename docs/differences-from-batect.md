# Differences from Batect

Ratect is a from-scratch Rust implementation inspired by
[Batect](https://github.com/batect/batect), not a wrapper or fork. It does not read
Batect's documentation or source at runtime, and it does not (yet) support everything
Batect does. This page exists so you don't have to guess which Batect behavior applies —
if it's not written down here or in the [config](config-reference.md)/
[CLI](cli-reference.md) reference, assume Ratect doesn't do it.

For the forward-looking plan, see [`ROADMAP.md`](../ROADMAP.md) — this page describes
*current* gaps, the roadmap describes where they're headed.

## Configuration format gaps

Batect features not currently supported by `batect.yml` parsing in Ratect:

- **Image building** (`build_directory`): the field is parsed, but building an image
  from a `Dockerfile` isn't implemented — see [config reference](config-reference.md#container).
- **Sidecar/dependency containers** (`dependencies`): the field is parsed, but has no
  effect — containers don't start any dependencies alongside them.
- **Environment variable interpolation**: `batect.yml` can't reference environment
  variables (e.g. Batect's `${VAR}` syntax).
- **Batect expressions**: no support for dynamic/computed configuration values.
- **`include`**: no support for splitting configuration across multiple files.
- Any other Batect configuration keys not listed in the
  [configuration reference](config-reference.md) — Ratect's schema is a small subset of
  Batect's.

## Runtime behavior gaps

Batect features not implemented in task execution:

- **Docker networking**: no automatic network management for inter-container
  communication.
- **Interactive mode**: no TTY/STDIN attachment for tasks that need user input.
- **User mapping**: no handling of host/container file permission mapping.
- **Proxy support**: no automatic proxy environment detection/injection.
- **Parallel execution**: prerequisites run sequentially, not in parallel.

## CLI gaps

- Only a small subset of Batect's flags exist — see the [CLI reference](cli-reference.md)
  for the full current list. Flags like `--project-name` or Batect's cleanup-control
  flags aren't implemented.
- Trailing arguments after `--` are parsed but **not forwarded** to the task's command
  — see the [CLI reference](cli-reference.md#positional-arguments).

## Known correctness gaps (not Batect-parity issues — just bugs)

These aren't "missing Batect features," they're places where Ratect's current behavior
is likely surprising regardless of what Batect does:

- **Container exit codes aren't checked.** A task whose command fails inside the
  container (e.g. `exit 1`) is currently still reported as successful — see the
  [CLI reference](cli-reference.md#exit-codes-and-error-reporting).
- **Missing config file doesn't fail the process.** Running with `--list-tasks` or a
  task name against a nonexistent config file logs an error but exits `0`.
- **A container with neither `image` nor `build_directory`** silently does nothing
  instead of raising a configuration error.

## What Ratect *does* support today

For the positive list — what's actually implemented and working — see:

- [Getting started](getting-started.md) for a walkthrough
- [Configuration reference](config-reference.md) for the supported schema
- [CLI reference](cli-reference.md) for the supported flags
- [How it works](how-it-works.md) for the execution model (prerequisites, dependency
  cycle detection, once-per-run dedup of tasks and image pulls)
