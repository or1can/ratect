# Frequently Asked Questions

Answers to real questions that come up using Ratect — not a restatement of the
[configuration reference](ratect-compat-config-reference.md), which already covers every field
mechanically. This page is expected to grow: if something surprised you, it likely
surprises the next person too — open an issue.

## When should I mount a directory instead of copying files into the image?

Ratect gives you both, and they trade off differently:

- A [`volumes` bind mount](ratect-compat-config-reference.md#volume-path-resolution)
  (`{ local = ".", container = "/code" }`) makes your working directory show up live
  inside the container — every one of the [worked examples](worked-examples.md)
  does exactly this for its own source tree. Edit a file on the host, rerun the task, and the container
  sees the change with no rebuild. The image itself stays generic (a stock language
  toolchain image, not your code baked into it), so pulling it is normally fast and
  cached.
- [`build_directory`](ratect-compat-config-reference.md#image-building) instead builds a
  `Dockerfile` — your code is `COPY`'d in at build time, so the resulting image is
  fully self-contained (nothing left on the host is needed to run it) and portable
  (push it, run it elsewhere, get the exact same bytes). The cost is the build step
  itself: a `docker build` on every task run unless Docker's own layer cache absorbs
  it, and every source edit invalidates the layers built from it.

Rule of thumb: mount while you're iterating locally — a task-runner project, almost
by definition, is exactly that case, which is why every project under `examples/`
mounts rather than builds. Reach for `build_directory` when the *image itself* is
the artifact you actually want to produce or ship (a container you're going to
publish, or a build step whose whole point is producing that image) rather than a
disposable environment to run your code in.

## How do I run something at container start regardless of the task's command?

Use [`entrypoint`](ratect-compat-config-reference.md#container) rather than `command`. Docker runs
`Entrypoint ++ Cmd` — the entrypoint always executes first regardless of what
`command` (or a task's `run.command`) is set to, and the classic idiom is a wrapper
script that does its setup, then hands off to whatever was actually asked for:

```toml
[containers.app]
image = "my-app"
entrypoint = "/entrypoint.sh"
```

```sh
#!/bin/sh
# /entrypoint.sh
set -e
# ... setup that must happen no matter which task/command runs ...
exec "$@"
```

The `exec "$@"` at the end matters: it replaces the shell process with the real
command instead of running it as a child, so the real command becomes PID 1 (or
inherits proper signal delivery under `exec`) rather than being one process removed
from it. This is also why `entrypoint`/`command` combine cleanly for the well-known
`entrypoint = "/bin/sh -c"`, `command = "make lint"` idiom (an entrypoint tokenizes
exactly like `command` does — see the field reference) — Ratect passes both straight
through to Docker with neither side adding an extra shell layer of its own.

If what you actually need is *signal forwarding and zombie reaping* rather than a
custom setup step, see
[`enable_init_process`](ratect-compat-config-reference.md#container) instead — it runs Docker's
own init process ahead of your command for exactly that, with nothing to write.

## Why does task idempotency matter?

Because Ratect doesn't cache "already succeeded" *across* invocations — there's no
timestamp check, no "nothing changed, skipping" the way a build tool like `make`
gives you for free. Within *one* invocation, a prerequisite reached by more than one
path does run only once (see [`prerequisites`](ratect-compat-config-reference.md#task) — that's
cycle-safe dedup, not a guarantee your script is safe to run twice), but every
separate `ratect-compat`/`ratect` invocation from the shell starts completely fresh,
with no memory of any previous run. Take the `migrate`/`test` example from [Cross-task
isolation](task-lifecycle.md#cross-task-isolation) — `test` has `migrate` as a
prerequisite, and `migrate` runs `run-migrations.sh`:

Every `ratect run test` run executes `migrate` again first, from a fresh
container. If `run-migrations.sh` isn't safe to run twice — it reapplies a
migration that's already applied, or errors on a table that already exists — every
single `test` run breaks or corrupts state, not just the first one. The same applies
to any task with real side effects reused as a prerequisite: seeding data, writing
output files another task then reads. Design each task assuming it will run more
than once against the same environment; Ratect's model of "always run fresh
containers" (see [Cross-task isolation](task-lifecycle.md#cross-task-isolation))
means it always will.

## How do I raise Docker Desktop's resource limits?

This isn't a Ratect setting — every container Ratect starts still runs under
whatever CPU/memory ceiling Docker itself is configured with, so a heavier build
(the [JVM](worked-examples.md#jvm-gradle) or [Node](worked-examples.md#nodejs)
examples are the ones most likely to notice)
can look like it's hanging or gets silently OOM-killed when Docker's own default is
too tight, and it's easy to blame the task runner instead of the actual cause.

- **Docker Desktop on macOS or Windows**: Settings → Resources lets you raise the
  CPU/memory/swap ceiling directly. On Windows with the WSL2 backend specifically,
  Docker Desktop's own Resources page can instead defer to a `.wslconfig` file
  controlling the WSL2 VM's limits — Docker Desktop's own documentation covers
  which applies for your setup.
- **Linux with the Docker Engine directly** (no Docker Desktop): there's no VM in
  the picture, so this ceiling doesn't exist in the same way — containers share the
  host's resources directly, bounded only by whatever `docker run`/daemon-level
  limits you've configured yourself.

## Why doesn't `&&` or `$VAR` work in my `command`?

`command` (and `entrypoint`) are tokenized into literal argv — quote/backslash-aware
whitespace splitting, matching Batect's own tokenizer exactly — with **no shell
involved and no expression support**. `command = "echo $HOME && echo done"` doesn't
run two commands with `$HOME` expanded; it's parsed as a single program named
`echo` given the literal argv `$HOME`, `&&`, `echo`, `done` — Docker will fail to
find an executable called `$HOME`. Anything relying on shell operators (`&&`, `||`,
pipes) or runtime environment expansion needs an explicit shell wrapper:

```toml
command = "sh -c 'echo $HOME && echo done'"
```

This is easy to conflate with a second, entirely different `$`-syntax: Ratect's own
[config-time expressions](ratect-compat-config-reference.md#expressions) (`$VAR`,
`${VAR:-default}`, `<name`), which *do* exist — but only in specific fields
(`environment` values, a volume's `host_path`, `build_directory`, and a few others),
resolved once before any task runs, never inside `command` itself. So there are
three different things that can look like "the same" `$VAR`: Ratect's own
config-time expression syntax (only in the fields listed above), `command`'s literal
tokenizer (no substitution of any kind), and real shell expansion once you're
actually running inside a container via `sh -c` (using the *container's* runtime
environment, resolved by the shell, not by Ratect). Which one applies depends
entirely on which field you're looking at.

## Why isn't my host environment variable visible inside the container?

Containers don't inherit the host's environment automatically — not Ratect's own
choice, just how Docker containers work, but a common first surprise for anyone
coming from local shell scripts where every variable is already there. Any host
variable a container's command needs has to be passed through explicitly via
[`environment`](ratect-compat-config-reference.md#expressions), using Ratect's own expression
syntax:

```toml
[containers.app]
image = "my-app"
environment = { API_KEY = "$API_KEY" }
```

Nothing here is a default worth changing — declaring every environment variable a
container actually uses is deliberate, not boilerplate: it makes a task's real
inputs visible in the config itself instead of depending on whatever happened to be
exported in whoever's shell ran it.

## What's the difference between a dependency and a prerequisite?

They sound like synonyms but are two unrelated mechanisms:

- A **dependency** (a container's own [`dependencies`](ratect-compat-config-reference.md#container)
  list) is another *container*, started alongside the one that needs it, kept
  running for the whole task, and torn down together at the end. Use it for
  something your task's container talks to over the network while it runs — a
  database, a cache, a queue. See [Dependency
  Readiness](dependency-readiness.md#resolution-order) for how several of these
  combine.
- A **prerequisite** (a task's own [`prerequisites`](ratect-compat-config-reference.md#task) list)
  is another *task*, run to completion — including its own full cleanup — strictly
  before the task that names it starts. Use it to sequence work: `compile` before
  `test`, a migration before the tests that need it migrated. See [Task
  ordering](task-lifecycle.md#task-ordering).

A `dependencies` container is there *for* the duration of the run; a
`prerequisites` task runs *and finishes*, with nothing of its own left when the
next task starts.
