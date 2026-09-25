# Includes

A project is one root configuration file and everything it includes. An
`include` entry pulls in either another file from your own tree or a *bundle*
— a Git repository holding a configuration file — and the result is one flat
set of containers, tasks and config variables, as if it had all been written
in the root file. This page is the one place for how included files combine,
where their relative paths end up, and what a file that arrived through a Git
include is and isn't allowed to do. The entries' own fields — `repo`, `ref`,
`path`, the clone cache and how to refresh it — are in the [`batect.yml`
reference](ratect-compat-config-reference.md#includes), and mean the same in
a `ratect.toml`; what the native format adds is in the [`ratect.toml`
reference](ratect-config-reference.md#includes). Why you'd share a bundle
across projects at all is [Reusable Pipeline Building
Blocks](reusable-building-blocks.md).

Every rule here holds in both formats: the two parse into one model, and an
include is resolved the same way whichever file declared it. The one exception
— what a bundle's *own* Git includes are allowed to do — is
[marked](#nested-git-includes) where it comes up. The examples are `ratect.toml`,
with the `batect.yml` spelling shown where the syntax differs.

## Two kinds of include

A **file include** names another configuration file in your own tree, by a path
relative to the directory of the file that declares it — not the root project
directory — so a file in a subdirectory can include more files relative to its
own location. A **Git include** names a repository and a ref, and Ratect clones
it into `~/.ratect/incl` and includes one file from the clone — the bundle's
`batect-bundle.yml` unless the entry's `path` says otherwise, with a
`ratect.toml` project looking for `ratect-bundle.toml` first (the [`ratect.toml`
reference](ratect-config-reference.md#includes) has the lookup). In a
`ratect.toml` project both kinds may mix, and each included file is parsed by
its own extension, so a native project can pull in an existing `batect.yml`
fragment or bundle unchanged:

```toml
include = [
    { path = "ci/tasks.toml" },                                   # a file include
    { path = "shared/legacy.yml" },                               # still YAML
    { type = "git", repo = "https://example.com/bundle.git", ref = "v2" },
]
```

A `batect.yml` spells the same three entries as a bare path, an object with
`type: file`, or an object with `type: git`:

```yaml
include:
  - ci/tasks.yml
  - path: shared/other.yml
    type: file
  - type: git
    repo: https://example.com/bundle.git
    ref: v2
```

## How included files combine

Every loaded file's `containers`, `tasks` and `config_variables` are merged
into one flat set. A name defined in more than one file is a hard error naming
the conflicting files — it is never treated as one file overriding another,
so a project can't redefine a container that came from a bundle (see [what a
consuming project can and can't
change](reusable-building-blocks.md#what-a-consuming-project-can-and-cant-change)
for the two narrower ways to adapt one). An included file may `include`
further files of its own.

Includes are walked breadth-first: every entry in the root file is reached
before any included file's own, and each file is loaded exactly once, by its
resolved absolute path, however many entries name it. So two files may both
include a common third without it being read twice — and, for a Git include,
the *first* entry to reach a file is the one whose settings that file is
loaded with, which is what [a grant goes on the entry that reaches the file
first](#a-grant-goes-on-the-entry-that-reaches-the-file-first) rests on.

## Which fields a file may use

A file's own **format** decides which fields and which rules apply to the
containers it declares — not the project that includes it. A `.yml` is
Batect's format, so a container written in one may use no `ratect`-native
field (`extends`, `ulimits`, `stop_signal`, `dns`/`dns_search`/`dns_options`,
`run_to_completion`,
`external_health_check`, a cache's `scope`, a setup command's `run_in`) and
gets Batect's own semantics: no expressions resolved in `image`, and exactly
one of `image`/`build_directory`. That holds even when a `ratect.toml`
project is what pulled the file in, and it holds for a YAML *root* file too
— `ratect -f batect.yml` reads a Batect file, and reads it as one.

To use a native field on a container, move that container into a `.toml`
file. Incremental migration exists so a project needn't be converted in one
go (see [Migrating a Batect
project](migrating-batect-project.md)), not so a YAML file can grow native
features — and the unit you convert is a container, which is smaller than a
file.

The flow between them is one-way, which is what makes this workable: a
native container may [`extends`](ratect-config-reference.md#extends-inheritance-instead-of-yaml-anchors)
a container declared in a YAML file, because the container namespace is flat
once includes are merged. The reverse — a YAML-declared container extending
anything — is the rejected case above.

For a bundle author, the same rule is why a Git bundle can ship
`ratect-bundle.toml` and `batect-bundle.yml` side by side (see [Two kinds of
include](#two-kinds-of-include)): the YAML one stays consumable by Batect
and `ratect-compat`, and the TOML one is free to use native fields.

## Where relative paths resolve

A relative path *within a container* — a volume's host path, `build_directory`,
a `build_secrets` entry's `path`, a `build_ssh` entry's `paths` — resolves
against the directory of the file that declares the container, not the root
project directory. For a file reached through a Git include that is the file's
own directory inside the clone — the same rule, rooted in the clone rather than
in your project. The built-in [`batect.project_directory`](ratect-compat-config-reference.md#built-in-config-variable-batectproject_directory)
config variable always resolves to the root file's directory, whichever file it
is written in, and is how an included file names the project directory
explicitly.

A project laid out as:

```
myproject/
├── ratect.toml
├── scripts/
└── containers/
    ├── extra.toml
    └── data/
```

with a root `ratect.toml` of:

```toml
project_name = "myproject"
include = [{ path = "containers/extra.toml" }]

[tasks.my-task]
run = { container = "my-other-container" }
```

and a `containers/extra.toml` <!-- example --> of:

```toml
[containers.my-other-container]
image = "alpine:1.2.3"
volumes = [
    # containers/extra.toml's own directory is containers/, so this
    # resolves to myproject/containers/data — not myproject/data.
    { local = "./data", container = "/data" },
    # Always the root project directory, regardless of which file this
    # is written in — so this is myproject/scripts, not
    # myproject/containers/scripts.
    { local = "<{batect.project_directory}/scripts", container = "/scripts" },
]
```

runs `my-other-container` with those two volumes mounted exactly as resolved
in the comments. `my-task` lives in the root file, but the container it runs
could equally have been declared there instead; only the volume paths'
resolution depends on which file declares the container. In a `batect.yml`
project the included file is `containers/extra.yml` <!-- example --> and its
two entries are the string forms, `./data:/data` and
`<{batect.project_directory}/scripts:/scripts`. (This project is
illustrative; the nearest real one is
[`ratect-compat/tests/fixtures/include.yml`](https://github.com/or1can/ratect/blob/main/ratect-compat/tests/fixtures/include.yml),
whose included file mounts its own directory for exactly this reason.)

## Owned files and bundle files

What a file may do depends on how it was reached, not on what kind of include
named it last, and not on its extension. A file reached from the root without
passing through a Git include is **owned**: yours, unrestricted, the same
trust as the root file itself. A file reached by passing through a Git include
is a **bundle file** — including any file the bundle then includes with a
plain file include, since that is still the bundle choosing what to load. A
local `.yml` your `ratect.toml` includes is owned; a `.toml` a bundle includes
from its own clone is not. ([`CONTEXT.md`](../CONTEXT.md) is the glossary for
these terms.)

An owned file is subject to none of what follows. A bundle file is loaded
under a *boundary*: the containment it must stay within, together with
whatever grants the include entry that reached it carried.

## What a bundle may do

A bundle defines both a container *and* the command that runs inside it, which
is the combination that matters: without a check, a bundle could declare a
container mounting `~/.ssh` or `~/.aws` and a command that reads it, and a
project including that bundle would have no way to know from its own
configuration alone. [decisions/0004](https://github.com/or1can/ratect/blob/main/decisions/0004-git-include-host-path-trust.md)
has the full reasoning; the rules it settled on are these.

### Containment

Two things are contained, and both are a deliberate divergence from Batect,
which has no equivalent check.

**Which files a bundle can make part of your configuration.** A Git include's
`path`, and every `include` entry declared (transitively) by the file it names,
must resolve to somewhere *inside* that repository's own clone — an absolute
path, a `../..` traversal, or a symlink pointing back out are all rejected with
a clear error rather than silently reading a file elsewhere on the machine
running Ratect. A file include in an owned file has no such restriction, since
it always names something already in your own checkout. A bundle *can* declare
a further `type: git` include of its own — a fresh repository with its own
boundary, not an escape from this one — in a `ratect-compat` project; a
`ratect` project refuses that unless you opt in, see [Nested Git
includes](#nested-git-includes) below.

**Where a bundle's containers can reach on your machine.** A `volumes` host
path, `build_directory`, `build_secrets` `path` or `build_ssh` `paths` declared
by a container in a bundle file must resolve to somewhere inside that repository's own clone, **or**
inside your project directory; an absolute path, a `../..` traversal, or a
symlink pointing back out of both is rejected the same way. A path Ratect
cannot resolve at all is also rejected rather than assumed harmless: if a
directory along it can't be searched, or a symlink loops, where the path really
leads can't be established — and the Docker daemon that would dereference it
runs as root, under no such restriction. A path that simply doesn't exist yet
is fine; Ratect or Docker creates it. The project directory is allowed as a
second root, rather than requiring pure containment within the clone, because
referencing it explicitly via `batect.project_directory` (say,
`<{batect.project_directory}/output`) is a legitimate, common thing for a
shared bundle to do — the project directory is your own fully-trusted tree,
distinct from the repository the container definition itself came from.

### Vouching for a bundle

Some bundles legitimately need a path outside both roots — most often a shared
tool cache under your home directory, like `~/.cache/trivy`, so a security
scanner's own database is reused across every project rather than re-fetched
per project. Since neither `customise` nor a local redefinition can add a
volume to someone else's container, there would otherwise be no way to use
such a bundle at all. Setting `allow_host_paths` on your own include entry
lifts the restriction for that bundle:

```toml
[[include]]
type = "git"
repo = "https://github.com/my-org/infra-bundle.git"
ref = "1.2.3"
allow_host_paths = true
```

```yaml
include:
  - type: git
    repo: https://github.com/my-org/infra-bundle.git
    ref: 1.2.3
    allow_host_paths: true
```

It applies **only to the bundle named there** — never to bundles *it* includes
in turn — and is honoured **only in your own configuration**: the same flag
written inside a bundle file is ignored, so a bundle can neither grant itself
the permission nor pass along one you gave it. It doesn't relax the file
containment above either; that governs which *files* become part of your
configuration, a separate question from where a container may mount. What a
future allowlist form would have to preserve is in decisions/0004.

For a native bundle whose whole purpose is a shared cache, a [shared cache
volume](ratect-config-reference.md#shared-caches) says the same thing without
asking any consumer for host filesystem access — reach for that before
`allow_host_paths`.

### Nested Git includes

> **`batect.yml` differs here.** In a `ratect-compat` project a bundle's own
> `type: git` includes are always allowed, matching Batect, and the field below
> is rejected rather than ignored. Everything in this section is the `ratect`
> binary's behaviour — decided by which binary loads the project, not by the
> extension of the file an entry sits in.

A bundle can declare `include` entries of its own, and under `ratect-compat`
those may be further Git includes naming any remote, with the same trust your
own includes get. Under `ratect` that is **refused by default**:

```
The bundle 'https://github.com/my-org/infra-bundle.git' at '1.2.3' declares a
Git include of its own ('https://elsewhere.example/other.git'), which would
fetch and run configuration from a remote you have not named. Set
'allow_nested_git_includes' to true on that bundle's own include entry to
accept this.
```

You chose the bundle; you did not choose whatever it decides to pull in next,
and that choice can change under you the next time its ref moves. Opt in per
bundle by setting `allow_nested_git_includes` on the entry:

```toml
[[include]]
type = "git"
repo = "https://github.com/my-org/infra-bundle.git"
ref = "1.2.3"
allow_nested_git_includes = true
```

The entry doesn't have to be in the `ratect.toml` itself — an entry in a local
`.yml` the native project includes is just as much your own configuration,
spelled `allow_nested_git_includes: true`, because that file is
[owned](#owned-files-and-bundle-files).

**The grant is one level deep.** It admits that bundle's own Git includes; it
does not let *those* bundles declare further ones. Like `allow_host_paths`, it
counts only in configuration you control — written inside a bundle file it is
ignored, so a bundle can neither grant itself the permission nor pass on the
one you gave it. If a bundle genuinely needs a chain deeper than that, include
the second repository yourself, where you can see it.

The refusal happens before anything is cloned, so a bundle can't use a nested
include to probe for a remote it names. When a nested include *is* admitted
and its clone fails, the error names both repositories but keeps `git`'s own
transport detail behind `RUST_LOG=debug` — see the [`ratect.toml`
reference](ratect-config-reference.md#nested-git-includes) for why.

### A grant goes on the entry that reaches the file first

A repository is cloned once and each file in it read once, however many
entries reach it, so whichever entry gets there first decides what that file
is allowed — and a grant on the loser would quietly do nothing. Entries in the
root file are always reached before any bundle's own, so declaring the include
yourself beats a bundle to it; between two entries in the same file, the
earlier one wins. Where two entries reach the same file and the losing one
carries a grant, Ratect refuses to load, names the repository and gives the
ordering rule above, rather than leaving you to wonder why the flag had no
effect. It can't name the winning entry for you, and that entry is often inside
a bundle you can't edit — in which case the move is to declare the include
yourself, in your root file, where it gets there first. Both grants follow this
rule.

It is the *file* that races, not the repository: two entries naming the same
repository with different `path`s pull in two different files, and each keeps
the grant written on its own entry.

That is a different case from a grant written inside a bundle, which two words
could easily blur. A grant written *inside* a bundle file is **ignored** —
accepted by the parser and worth nothing, because honouring it would let a
bundle grant itself. A grant in your own configuration that loses the race
above is **refused** — the load stops, because you wrote something that cannot
take effect and nothing else would tell you.

## What this does and doesn't buy you

None of this makes a bundle *safe* to include blindly — it still runs code you
didn't write, the same as adding any dependency does. What it buys you is that
the one attack this project has specifically hardened against (a bundle
quietly reaching for your credentials via a host mount) needs an explicit,
visible grant in your own file rather than working by default. Anything that
lets a bundle read or write outside its clone or the project directory without
that grant is a security bug — see [`SECURITY.md`](../SECURITY.md) for how to
report one.
