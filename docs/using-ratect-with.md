# Using Ratect With Language Ecosystems

Every toolchain below assumes its own persistence: a package manager expects
its downloaded dependencies to still be there next time, a compiler expects
its incremental build cache to survive between invocations. Ratect's own
container model doesn't give you that for free — [every task starts a fresh
container](task-lifecycle.md#cross-task-isolation), and anything written
outside a mounted volume disappears with it. Without a
[`cache` volume](ratect-compat-config-reference.md#cache-volumes) pointed at the right
directory, a toolchain that assumes persistence just re-downloads or
recompiles everything, every single run, as if it were on a brand-new
machine each time.

This page has one section per ecosystem, naming exactly which directories
need to be a `cache` volume and why, plus the couple of correctness gotchas
that aren't a caching question at all. Every config referenced here is the
real, CI-proven config under
[`examples/`](https://github.com/or1can/ratect/tree/main/examples) — see
[Worked Examples](worked-examples.md) for the full files.

## Rust

```toml
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "cargo-registry", container = "/usr/local/cargo/registry" },
    { type = "cache", name = "cargo-target", container = "/code/target" },
]
```

Two separate caches, for two separate things Cargo persists:

- **`cargo-registry`** — the crate index and every downloaded/extracted
  dependency source. Without this, `cargo build` re-fetches your entire
  dependency tree from crates.io on every run.
- **`cargo-target`** — compiled build artifacts (`target/`). Without this,
  every run is a cold, from-scratch compile — the difference between an
  incremental rebuild and a full one, which for a large dependency tree is
  minutes rather than seconds.

## Go

```toml
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "go-mod", container = "/go/pkg/mod" },
    { type = "cache", name = "go-build", container = "/root/.cache/go-build" },
]
```

Same split as Rust, Go's own equivalents:

- **`go-mod`** (`GOPATH`'s module cache) — downloaded module sources.
  Without it, `go build`/`go test` re-fetches every dependency module fresh.
- **`go-build`** (`GOCACHE`) — the compiler's own build cache (compiled
  packages, keyed by input hash). Without it, every build recompiles the
  entire dependency graph rather than reusing what hasn't changed.

A linter with its own cache (`golangci-lint`'s `~/.cache/golangci-lint/`, for
example) would need the same treatment if a project adopts one — `examples/go`
doesn't, so there's nothing to demonstrate it against here, but the pattern
is identical: mount whatever directory the tool itself says it caches into.

## Node.js

```toml
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "npm-cache", container = "/root/.npm" },
]
```

**`npm-cache`** — npm's own download cache, not `node_modules` itself.
`node_modules` stays a plain, uncached part of the working directory (rebuilt
by `npm ci` on each run, from what's in the cache); what needs to persist is
npm's *cache* of already-downloaded package tarballs, so a repeat `npm ci`
doesn't re-download the same versions from the registry every time.

**A long-lived Node process needs [`enable_init_process`](ratect-compat-config-reference.md#container)
too** — this isn't a caching question, but Batect's own `nodejs.md` covers it
for the same reason it belongs here: Node doesn't handle running as PID 1
correctly, so a container with no init process won't forward `SIGINT`/Ctrl+C
to it correctly. `examples/node`'s own `run` task doesn't need this — it
prints a message and exits, nothing to interrupt — but
[`examples/full-stack`](https://github.com/or1can/ratect/tree/main/examples/full-stack)'s
`app` container is exactly the long-lived-server case, and sets
`enable_init_process = true` for it.

## Python

```toml
volumes = [
    { local = ".", container = "/code" },
    { type = "cache", name = "pip-cache", container = "/root/.cache/pip" },
]
```

**`pip-cache`** — pip's own download/wheel-build cache. Without it, every
`pip install` re-downloads and, for a package with no prebuilt wheel for the
image's platform, rebuilds it from source from scratch, every run.

## JVM (Gradle)

```yaml
environment:
  GRADLE_OPTS: -Dorg.gradle.daemon=false
volumes:
  - local: .
    container: /code
  - type: cache
    name: gradle-cache
    container: /root/.gradle
```

**`gradle-cache`** (`~/.gradle`) — Gradle's dependency cache and its own
build cache, in one directory. Without it, every run re-resolves and
re-downloads the full dependency graph.

**Disable the Gradle daemon.** This one's a correctness/performance gotcha
rather than a caching question, and it points the opposite direction from
everything above: Gradle's daemon exists to stay *warm* across invocations,
so it can skip reloading and recompiling the build script on the next run.
That benefit depends on the daemon surviving between runs — which never
happens in a Ratect container, disposable per task. Without
`-Dorg.gradle.daemon=false`, every task pays the daemon's own startup cost
and then throws it away with the container, strictly worse than never
starting one. `examples/jvm` sets this on `build-env` via `GRADLE_OPTS`.

If a Gradle build under Docker Desktop looks hung or dies without a message,
the ceiling is usually Docker's own, not Ratect's — see the
[FAQ](faq.md#how-do-i-raise-docker-desktops-resource-limits).

## Not covered

Batect's own [per-ecosystem
pages](https://batect.dev/docs/using-batect-with/tools/) also cover .NET Core
and Ruby — both caching-only concerns there, no correctness gotcha like
Gradle's or Node's.
Ratect has no example project for either ecosystem, so there's nothing
real to ground a section in here without inventing untested config; a real
worked example for either is its own, larger undertaking than this page.

## Next steps

From here, [Task Lifecycle](task-lifecycle.md) is the step-by-step account of
what `ratect run` actually does — task ordering, per-task setup and cleanup —
and [Dependency Readiness](dependency-readiness.md) is when a dependency counts
as ready and how several dependencies' waits combine; the [FAQ](faq.md)
collects the questions that come up first. When a second project wants the
same containers and tasks, [Includes](includes.md) covers pulling in a shared
file or a Git bundle, and [Reusable Pipeline Building
Blocks](reusable-building-blocks.md) why you'd want to.
