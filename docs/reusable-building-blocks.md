# Reusable Pipeline Building Blocks

This is about *why* you'd reach for a Git include, not the mechanics of one —
those are on [Includes](includes.md), with the entries' own fields in the
[`batect.yml`](ratect-compat-config-reference.md#git-includes) and
[`ratect.toml`](ratect-config-reference.md#includes) references. This page
motivates the use case first, then the trust model it depends on, since a
bundle is code you didn't write, running in your own environment.

## The use case

Several projects that share a stack tend to end up wanting the same handful
of things: the same linter run the same way, the same base image with the
same hardening applied, the same "scan this for vulnerabilities" task. Copying
a container/task definition into every project's own config works, right up
until the definition needs to change — then it's a find-and-replace across
however many repositories remembered to keep it in sync, and however many
didn't.

A **bundle** — a Git repository holding a configuration file, meant to be
imported rather than run on its own — is Ratect's answer: define the
containers and tasks once, version the repository like any other code (tags,
releases, a changelog), and let every consuming project pull in a pinned
version with one `include` entry. Bump the pin when you're ready to take a
change; every consumer bumps on its own schedule, not in lockstep.

```yaml
# batect.yml
include:
  - type: git
    repo: https://github.com/my-org/lint-bundle.git
    ref: v2.1.0

tasks:
  lint:
    prerequisites:
      - shared-lint
```

Here `shared-lint` isn't defined anywhere in this project's own file — it
came from the bundle, merged in exactly as if it had been written locally
(see [how included files combine](includes.md#how-included-files-combine)). The
project's own `lint` task just sequences it in as a
[prerequisite](faq.md#whats-the-difference-between-a-dependency-and-a-prerequisite).

## A bundle is untrusted code by default

A bundle doesn't just define a container — it also controls the command that
runs inside it, so Ratect loads it under a boundary: its containers may mount
only inside the bundle's own clone or your project directory unless you
vouch for it with `allow_host_paths`, a grant counts only when written in your
own configuration, and (in `ratect.toml`) its own Git includes are refused
until you opt in per bundle. The rules, the two grants and what they do and
don't buy you are on [Includes](includes.md#what-a-bundle-may-do);
[decisions/0004](https://github.com/or1can/ratect/blob/main/decisions/0004-git-include-host-path-trust.md)
covers the full reasoning.

## What a consuming project can and can't change

A project can't redefine a container that came from an include — declaring a
container with the same name in your own file is a hard error, not an
override. Two narrower ways to adapt a bundle's container to your own project
without forking it:

- **`customise`** (both formats) — a task can override a non-main container's
  `environment`, `ports`, and `working_directory`. Deliberately not
  `volumes`: letting a consumer silently add a mount to someone else's
  container would undercut the containment above at the one place it
  actually matters. See
  [TaskContainerCustomisation](ratect-compat-config-reference.md#taskcontainercustomisation).
- **`extends`** (`ratect.toml` only) — a container in your own file can
  inherit from one defined in an included bundle and layer its own fields on
  top, shallow-merged per field. Useful when a bundle ships a base image and
  expects projects to build on it rather than use it as-is. See
  [`extends`](ratect-config-reference.md#extends-inheritance-instead-of-yaml-anchors).

## A cache shared across projects, without a host mount

`ratect.toml`'s [shared caches](ratect-config-reference.md#shared-caches)
(`scope = "shared"` on a `cache` volume) are worth calling out specifically
for a bundle that wants one Cargo registry or npm cache reused across every
project that includes it. The alternative is a host path —
`local: ~/.cache/cargo` — which means asking every consumer for
`allow_host_paths`. A shared cache says the same thing directly, grants no
host filesystem access, and stays under Ratect's own control
(`ratect-shared-cache-<name>`, alongside `~/.ratect/incl` where Git includes
themselves are cloned). If you're authoring a native bundle whose whole
purpose is a shared tool cache, reach for this before reaching for
`allow_host_paths`.

## Pinning and updating a bundle

A bundle's `(repo, ref)` is cloned once and cached forever at
`~/.ratect/incl` — never re-fetched, even if `ref` later moves. This is why
`ref` **must** be an immutable tag or a pinned commit SHA, never a branch: a
branch ref would look like it updates and never actually would, which is
worse than a pin that's honest about being frozen. To take a bundle change,
bump `ref` to a new tag (the versioning story that makes bundles worth
having in the first place). If `ref` itself moved (a branch, or a re-pushed
tag) rather than your own config changing, `ratect` has a purpose-built
command for that: `ratect includes refresh` discards every cached clone and
re-fetches it — see [the CLI reference](ratect-cli.md#includes-options).
`ratect-compat` has no equivalent subcommand, but both binaries share
`~/.ratect/incl`, so `ratect includes refresh` refreshes what a
`ratect-compat` run cached too; failing that, delete the corresponding
directory under `~/.ratect/incl` by hand.

## Authoring a bundle that supports both formats

A bundle repository is just a config file at a path, so there's nothing
format-specific about *being* a bundle. `ratect.toml`'s own Git-include
resolution looks for `ratect-bundle.toml` first, then falls back to
`batect-bundle.yml` when no `path` is given — so a bundle author can ship
both files, and a project on either format includes the same repository and
gets the file meant for it. See [Includes](ratect-config-reference.md#includes)
for the exact lookup order.
