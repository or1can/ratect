# Arguments on a Prerequisite Reference

Ratect does not support giving a `prerequisites` list entry its own arguments
(e.g. a `ratect.toml` shape like `{ name = "gradle", args = ["installDist"] }`,
or any `batect.yml` equivalent). A prerequisite always runs with no additional
arguments — see `engine.rs`'s `run_task_scoped(prerequisite, &[], false)` call,
which is deliberate, not an oversight.

## Why this is out of scope

The motivating case (from upstream
[batect#1053](https://github.com/batect/batect/issues/1053)) is reusing a
generic, argument-forwarding task (e.g. a `gradle` task invoked as
`ratect run gradle -- installDist`) as a *prerequisite* with a fixed set of
arguments baked in at the reference site — not forwarding the outer
invocation's own `-- ADDITIONAL_ARGS` down to prerequisites, which is a
separate, still-deliberately-unsupported thing.

Batect itself never shipped this — the issue went stale with no resolution.
The reporter's own workaround already fully solves it today, in both
`batect.yml` and `ratect.toml`, with zero new config syntax: define a small
intermediate task that bakes the arguments into its own `command`, and use
*that* as the prerequisite instead of the generic one directly.

```toml
[tasks.build-cli]
description = "Build the CLI with the arguments the requirements task needs"
run = { container = "build", command = "./gradlew --stacktrace installDist" }

[tasks.requirements]
prerequisites = ["build-cli"]
run = { container = "run", command = "./cli/build/install/ort/bin/ort requirements" }
```

A dedicated object shape for prerequisite arguments would also be a poor fit
for `ratect-compat` specifically: unlike fields such as `PortMapping` (which
accept both a bare string and an object because *batect.yml itself* already
has both shapes), this would be new syntax `ratect-compat` invents rather than
matches — squarely the "new idea, not `ratect-compat`'s job" case
([decisions/0001](../decisions/0001-two-binaries.md)).

Given a working, no-new-syntax workaround exists and only one request has
surfaced so far, the extra config surface (and the parser/schema/docs work
that would come with it) isn't justified yet. **Revisit if independent reports
of this pattern recur** — a single wrapper task per reusable-with-args
prerequisite is a reasonable cost until then.

## Prior requests

- #94 — "Arguments on a prerequisite reference"
