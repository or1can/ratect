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
(`ratect-compat`) or native (`ratect`). A property of a project.

**File syntax** — whether a file is parsed as TOML or YAML. A property of a
file, independent of its project's dialect.

## Cancelling a run

**Termination signal** — any signal Ratect traps so a run cleans up after
itself: `SIGINT`, `SIGTERM` or `SIGHUP`. The engine treats all three alike.

**Interrupt** — `SIGINT` specifically, the one a terminal raises from Ctrl+C.
It is *one* termination signal, not the category.

The distinction is worth writing down because the code says both words with
one vocabulary: `Interrupt` (the tracker), `interrupted()` and `count()` mean
any termination signal, while `TerminationSignal::Interrupt` and
`Interrupt::record()` mean `SIGINT` alone. Reading either as the other gives
the wrong answer about what a run exits with, and about what a second one
during cleanup abandons.

## Caches

**Project cache** — cache storage private to one project.

**Shared cache** — cache storage identified by name alone, so every project on
the machine naming it addresses the same storage.
