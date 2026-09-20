# How Ratect Compares to Other Tools

Batect's own version of this page named Cage and Toast — both largely dormant
today. The comparison worth writing is against what people actually reach
for now, which is a different set: [Docker
Compose](https://github.com/docker/compose),
[Make](https://www.gnu.org/software/make/),
[Task](https://github.com/go-task/task),
[Earthly](https://github.com/earthly/earthly),
[Dagger](https://github.com/dagger/dagger), and
[`just`](https://github.com/casey/just).

None of these six exist to lose to Ratect. Each was built to solve a real
problem, several of them solve it better than Ratect does for the case they
targeted, and picking the right one is usually about which problem you
actually have — not which tool wins a feature count. What follows is honest
about both directions.

Reach for Ratect when the problem is **running development tasks
identically on every machine, in Docker, with real dependency
orchestration** — a database or another service that needs to actually be
ready, not just started, before your task touches it. That's a narrower
claim than "runs my project"; the sections below say plainly where it isn't
the best fit.

## Docker Compose

Compose defines and runs multi-container applications — the closest match
to Ratect in shape (containers, networks, dependencies between them), and
the one most likely to already be sitting in a project you're looking at.

**Where Compose wins:** it's the default, not an alternative you have to
choose — ships with Docker Desktop and Engine, and most developers already
know it. Bringing up a stack and leaving it running (the actual point of a
`docker-compose.yml`) needs less ceremony in Compose than expressing the
same thing as a Ratect *task*, because that's what Compose is for and
Ratect's task model isn't. `docker compose watch` gives you a live dev
loop with no equivalent in Ratect at all.

**Where Ratect wins:** Compose has no task layer — no prerequisites, no
distinct `build`/`test`/`lint` verbs, no per-invocation ephemeral
containers. `depends_on: condition: service_healthy` gates *start order* on
a health check, which is real and useful, but Compose has nothing like
[`setup_commands`](config-reference.md#dependency-readiness) — a scripted
step that runs *inside* a dependency once it's healthy but before anything
depends on it starts. Seeding a database before code touches it means an
init container or an entrypoint script you write and maintain yourself;
in Ratect it's a config field.

## Make

The original. Targets, prerequisites, recipes, and a dependency graph driven
by file timestamps — genuinely incremental, in a way nothing else on this
page (Ratect included) fully replicates.

**Where Make wins:** it's already installed, on nearly every Unix-like
system, with zero extra daemon and zero extra dependency — spinning up
Docker for a one-line shell command is real overhead Make never pays.
Make's own incremental rebuild — skip a target outright if its
prerequisites' files haven't changed — is a genuine capability Ratect
doesn't have; a Ratect task always runs, and whether *the tool inside the
container* decides to skip work is between that tool and its own cache, not
something Ratect tracks itself.

**Where Ratect wins:** Make runs on whatever's already on your host — the
exact "works on my machine" problem Ratect exists to remove. It also has no
native model for orchestrating dependent *services* (bring up a database,
wait for it, run something against it); that's shell script bolted onto a
recipe, not something the tool understands.

## Task

[Taskfiles](https://taskfile.dev) — YAML, explicitly positioned as "inspired
by Make, designed for modern workflows." Closest in *spirit* to Ratect and
Batect's own "define tasks in a file" model of any tool here — but Task
runs on the host by default; Docker is something you can reach for per-task,
not the execution model itself.

**Where Task wins:** far lighter for tasks that don't need containerizing
at all — no daemon, near-instant startup, and it works the same on Windows
without Docker Desktop in the loop. Its `sources`/`generates` fields give
file-based skip-if-unchanged tracking similar to Make's, which Ratect has
no equivalent of.

**Where Ratect wins:** the same reproducibility gap as Make — Task's default
is your host toolchain, so "works on my machine" is back on the table unless
you deliberately containerize each task yourself, and even then there's no
built-in service-dependency/readiness model to reach for.

## Earthly

Worth naming plainly: **Earthly is no longer actively maintained** — its own
README says so directly, pointing at a post titled "shutting down Earthly
Cloud." Its last tagged release was mid-2025. Comparing against it now is
closer to Ratect's own relationship with Batect than a live rivalry.

While it was under active development, Earthly's actual design point was a
containerized *build* framework — Dockerfile-and-Makefile-flavored syntax,
BuildKit-based, with a real target-level build-cache graph more
sophisticated than Ratect's `cache` volume mounts for the specific job of
producing cacheable build artifacts identically locally and in CI.

**Where Ratect wins today, practically:** it's maintained. Setting that
aside, the two tools' centers of gravity differ anyway — Earthly is oriented
around building artifacts; Ratect's `health_check`/`setup_commands`/
`dependencies` model is oriented around running and orchestrating live
services, which was never Earthly's target.

## Dagger

A programmable automation engine, not a config format: pipelines are code,
in a real language (Go, Python, TypeScript, and others), compiling down to
containers as Dagger's own primitive. The pitch is portability — the same
pipeline runs on your laptop, in CI, or in Dagger's own cloud — backed by
built-in OpenTelemetry tracing.

**Where Dagger wins:** a real programming language for pipeline logic —
loops, conditionals, functions, tests for the pipeline itself — which
`ratect.toml`/`batect.yml`'s declarative shape deliberately doesn't offer,
and isn't trying to. For a genuinely complex pipeline that outgrows what a
static config file can express cleanly, that's a real advantage, not a
rounding error.

**Where Ratect wins:** simplicity for the common case. Most projects need
"run these containers, in this order, with these dependencies" — a small
TOML/YAML file to read and edit, not SDK code in a general-purpose language
to write and maintain. Ratect's Batect-compatibility story (an existing
`batect.yml` keeps working, field-for-field) also has no Dagger analogue —
Dagger is a from-scratch mental model, not a migration path from anything.

## `just`

Its own README calls it "a command runner, not a build system" — a
deliberate, stated distinction from Make, not an oversight. `justfile`
recipes run on the host, with none of Make's dependency-graph or
incremental-rebuild machinery.

**Where `just` wins:** about as light as this category gets — a single
small binary, no daemon, recipes that are just shell commands with better
ergonomics (parameters, cross-platform support, no `.PHONY` workarounds).
For "give me short, memorable names for the commands I run all the time,"
reaching for Ratect and its Docker daemon is real overhead for no benefit.

**Where Ratect wins:** `just` doesn't attempt reproducibility or isolation
at all — recipes run in your own shell, on your own machine — and it has no
service-orchestration model whatsoever. Neither is a flaw in `just`; it's
explicitly not trying to be either of those things.

## If you're choosing

| Reach for... | when the problem is |
| --- | --- |
| **Ratect** | development tasks that need to run identically everywhere, and/or real services (a database, another API) they depend on actually being ready first |
| **Docker Compose** | bringing up and living inside a multi-container stack, more than running discrete tasks against it |
| **Make** | you already have `make` everywhere you need it, and don't need containers or cross-machine reproducibility |
| **Task** | Make-like ergonomics without Make's own syntax, for tasks that don't need Docker |
| **Dagger** | a pipeline complex enough to genuinely need a real programming language, not a config file |
| **`just`** | short, memorable shortcuts for commands you already know how to run |

Earthly isn't in that table on purpose — recommending an unmaintained tool
for new work doesn't make sense regardless of what it used to be good at;
see its own section above for what that was.

Nothing here is exclusive — a real project often uses more than one of
these at once (a `justfile` that shells out to `ratect run`, or a Compose
stack a Ratect task talks to). This is a guide to which problem each tool
was built for, not a bracket with one winner.
