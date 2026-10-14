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
grants nothing: the first is unrestricted, the second is contained. They sit at
opposite ends of the same scale, so a model with one value for both orders them
backwards.

## Formats

**Dialect** — which binary's rules govern a project: Batect-compatible
(`ratect-compat`) or native (`ratect`). A property of a project. The word is
this file's and the code's; user docs name the binary instead.

**File syntax** — whether a file is parsed as TOML or YAML. A property of a
file, independent of its project's dialect.

**Written-for version** — the Ratect version a native project's configuration
was written for. A property of a project, not of a file.

## Batect parity

**Divergence** — a deliberate difference between `ratect-compat` and Batect,
recorded in `docs/differences-from-batect.md`.

**Parity gap** — an unintended difference between `ratect-compat` and Batect.

## Readiness

**Readiness gate** — the check a container passes before it counts as ready.
One gate per container, whatever it consists of: ordinarily two steps — a
health-check step (Docker's own health check), then its setup commands. An
externally checked container's gate is its external health check alone, and
a run-to-completion container's is exiting 0. For a
dependency, nothing that depends on it starts before it passes. A task's own
container's gate gates nothing: under the Batect-compatible dialect it runs
alongside the main command; under the native dialect it has none.

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

**Interrupt** — `SIGINT` specifically, the one a terminal raises from Ctrl+C.
It is *one* termination signal, not the category.

The distinction is worth writing down because the code says both words with
one vocabulary: `Interrupt` (the tracker), `interrupted()` and `count()` mean
any termination signal, while `TerminationSignal::Interrupt` and
`Interrupt::record()` mean `SIGINT` alone. Reading either as the other gives
the wrong answer about what a run exits with, and about which rung of the
cleanup ladder a further one during cleanup reaches.

**Cleanup ladder** — what a termination signal means once cleanup has
started, counted from how many had arrived when the run ended: one more, of
any kind, *escalates* (whatever is left is force-removed, with no grace
period); an interrupt after that *abandons* cleanup. Only the abandon rung
looks at the kind, because a keyboard is the one thing that sends `SIGINT`
twice, while a supervisor's timed follow-up is a `SIGTERM` and never means
"leave it". A signal during the cleanup of a finished run doesn't change
what the run exits with — the task's container reported its own exit code,
and that stands — unless the abandon rung leaves something behind, which is
a cleanup failure ([0016](decisions/0016-exit-codes.md); until #540 lands,
a passing run signalled during cleanup still reports the signal).

**Cleanup failure** — Ratect failing to remove something it set out to
remove: a removal the daemon refused, or the cleanup ladder's abandon rung
reached with something still to remove. What a `--no-cleanup*` flag leaves
in place on purpose is not one. Reported on stderr always; under
[0016](decisions/0016-exit-codes.md) (landing with #540) it changes the exit
code, to `125`, only for a run whose container exited `0`, since a task's
own non-zero code is never overridden.

## Output

**Result** — what an invocation was asked to produce: for a run, the task
container's own output; for any other verb, what that verb produces (a
listing, a report, a conversion). Ratect's own progress and a dependency's
output are not part of it.

**Progress console** — the stream Ratect's own progress is written to: stderr
for `ratect`, stdout for `ratect-compat` (under
[0017](decisions/0017-output-streams-and-styles.md), landing with #535;
until then both write it to stdout).

**Milestone** — a lifecycle event rendered to the progress console: a line
under `simple` and `all`, a status-line state under `fancy`.

**Notice** — a stderr line about the run that marks no problem and asks for
no action: the interrupt notice, a completed cleanup after an interrupt,
what a `--no-cleanup*` flag left in place. A cleanup that left something
behind is a *cleanup failure*, above, and so an error, not a notice.

**Warning** — a stderr line, marked `Warning:`, about something the user
should change though the run proceeds.

**Error** — a stderr line, marked `Error:`, reporting that Ratect failed to do
what was asked: for a run, to run the task or to clean up after it.

The markers and the printed notices are
[0017](decisions/0017-output-streams-and-styles.md)'s, landing with #535.

## Caches

**Project cache** — cache storage private to one project.

**Shared cache** — cache storage identified by name alone, so every project on
the machine naming it addresses the same storage.
