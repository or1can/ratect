# Domain glossary

What each term in Ratect's configuration model denotes. Definitions only — no
implementation detail, no behaviour, no rationale. Why a thing works as it does
belongs in [`decisions/`](decisions/README.md); what it does for a user belongs
in [`docs/`](docs/).

## Configuration files

**Project** — one root configuration file and everything it includes. Named by
`project_name`, which only the root file may set.

**File include** — an entry importing one further configuration file from the
declaring file's own tree.

**Git include** — an entry importing configuration from a *bundle*.

**Bundle** — a Git repository holding a configuration file, for importing with
a Git include. A repository is a bundle by what it contains, whether or not any
project includes it.

**Owned file** — a configuration file reached from the root without passing
through a Git include.

**Bundle file** — a configuration file reached by passing through a Git
include, including any file it then imports with a file include. Determined by
the route to a file, not by the kind of the last step that reached it.

**Bundle entry file** — the bundle file a Git include names directly. Stands
to the bundle files reached from it as a root configuration file stands to its
project, except that nothing runs it on its own.

## Trust

**Grant** — a permission widening what one bundle may do, written on a Git
include entry in an owned file (`allow_host_paths`,
`allow_nested_git_includes`).

**Boundary** — what a bundle file is loaded under, in two parts: the
containment it must stay within (which files its includes may reach, where its
containers' host paths may resolve), together with the grants it carries.

**Effective boundary** — the boundary a file is loaded with: the one carried by
the first Git include entry to reach it. Distinct from what a later entry
reaching the same file asks for.

An owned file has *no* effective boundary, which is not the same as one that
grants nothing: the first is unrestricted, the second is contained.

## Formats

**Dialect** — which binary's rules govern a project: Batect-compatible
(`ratect-compat`) or native (`ratect`). A property of a project. The word is
this file's and the code's; user docs name the binary instead.

**File syntax** — whether a file is parsed as TOML or YAML. A property of a
file, independent of its project's dialect.

**Written-for version** — the Ratect version a native project's configuration,
or a bundle entry file and what it reaches, was written for. A property of a
project or of a bundle entry, not of a single file.

**Reviewed-for version** — the Ratect version a Git include entry's author has
checked a bundle against, standing in for the bundle's own written-for version
when it is the later of the two. A property of a Git include entry.

## Batect parity

**Divergence** — a deliberate difference between `ratect-compat` and Batect,
recorded in `docs/differences-from-batect.md`.

**Parity gap** — an unintended difference between `ratect-compat` and Batect.

## Readiness

**Readiness gate** — the check a container passes before it counts as ready:
a health-check step (Docker's own health check), then its setup commands. An
externally checked container's gate is its external health check alone; a
run-to-completion container's is exiting 0. A dependency's gate gates the
start of what depends on it, and nothing else
([0019](decisions/0019-readiness-gates-starting-only.md)); a task's own
container's gate gates nothing
([0012](decisions/0012-native-task-container-has-no-readiness-gate.md)).

## Identity

**Host identity** — the uid and gid of the user running Ratect on the host.

**Image identity** — the users and groups an image ships in its own identity
files.

**Identity** — a container's declared adoption of the host identity, together
with a stated home directory. What adopting means is resolved against the
daemon, not fixed by the declaration.

**Passthrough** — a container field handed to the runtime verbatim; Ratect
neither resolves nor synthesizes anything from it.

## Cancelling a run

**Termination signal** — any signal Ratect traps so a run cleans up after
itself: `SIGINT`, `SIGTERM` or `SIGHUP`. The engine treats all three alike.

**Interrupt** — `SIGINT` specifically, the one a terminal raises from Ctrl+C:
*one* termination signal, not the category. The code's `Interrupt` tracker
counts every termination signal; `TerminationSignal::Interrupt` is `SIGINT`
alone.

**Cleanup ladder** — what a further termination signal means once cleanup has
started: the first, of any kind, *escalates* (whatever is left is
force-removed); an interrupt after that *abandons* cleanup. What a run then
exits with is [0016](decisions/0016-exit-codes.md)'s.

**Cleanup failure** — Ratect failing to remove something it set out to
remove: a removal the daemon refused, or the cleanup ladder's abandon rung
reached with something still to remove. What a `--no-cleanup*` flag leaves
in place on purpose is not one. An error, with
[0016](decisions/0016-exit-codes.md)'s exit code.

**Abandoned build** — a built image's build that a run stopped waiting for
before it finished, whether from a termination signal or a sibling failing.
Cleanup cancels it, first and whether or not a `--no-cleanup*` flag is given;
a cancel that fails is a warning, not a cleanup failure.

## Output

**Result** — what an invocation was asked to produce: for a run, the task
container's own output; for any other verb, what that verb produces (a
listing, a report, a conversion). Ratect's own progress and a dependency's
output are not part of it.

**Progress console** — the stream Ratect's own progress is written to;
[0017](decisions/0017-output-streams-and-styles.md) says which, per binary.

**Milestone** — a lifecycle event rendered to the progress console: a line
under `simple` and `all`, a status-line state under `fancy`.

**Summary line** — the progress line that ends a task's run, whatever its
outcome: that the task finished and how, or that it failed. It says that the
run ended, never why it failed — the *error* says that — and `quiet`, which
shows no progress, has none.

**Notice** — a stderr line about the run that marks no problem and asks for
no action. A cleanup that left something behind is a *cleanup failure*, and
so an error, not a notice.

**Warning** — a stderr line, marked `Warning:`, about something the user
should change though the run proceeds.

**Error** — a stderr line, marked `Error:`, reporting that Ratect failed to do
what was asked: for a run, to run the task or to clean up after it.

The markers and the notices are
[0017](decisions/0017-output-streams-and-styles.md)'s.

## Caches

**Project cache** — cache storage private to one project.

**Shared cache** — cache storage identified by name alone, so every project on
the machine naming it addresses the same storage.

## Built images

**Built image** — the image a container's `build_directory` produces, as
distinct from an image a container names to be pulled.

**Automatic tag** — the name every built image is given,
`<project_name>-<container name>`; the only image name Ratect writes without
being asked.

**Extra tag** — a further name given to one container's built image for one
run (`--tag-image`).

**Image override** — a pulled image substituted for a container's own for one
run (`--override-image`); a container with an override builds nothing.

**Image-name component** — the rule `project_name` and every container name
satisfy: lowercase letters and digits, with `.`, `_`, `__` or a run of `-`
between them, and nothing else.

The naming is [0018](decisions/0018-built-images-named-for-people-found-by-labels.md)'s.
