# Ratect Roadmap

This document outlines the planned journey for Ratect, from achieving parity with Batect to implementing Rust-specific enhancements and future innovations.

## Batect Parity

The primary goal is to support the core features of Batect to ensure a seamless transition for existing users. This work targets the [`ratect-compat` binary](#two-binaries-ratect-and-ratect-compat) specifically — the `ratect` binary is not expected to maintain 1:1 Batect parity.

**Feature parity is done.** Every Batect configuration field and CLI flag is either
supported or a deliberate, documented divergence — see [Differences from
Batect](docs/differences-from-batect.md) for the exact, itemized per-field/per-flag
status (the living version of this list; not duplicated here, and not simply
crossed off once support lands — a field can still be "Supported, more restrictive").

What's left is **conformance**, not features: proving real Batect projects behave
identically under `ratect-compat`, not just that each field/flag passes its own
isolated test. The [Batect conformance
corpus](ratect-compat/tests/conformance/README.md) vendors Batect's own journey-test
projects verbatim and runs `ratect-compat` against them; it currently covers 28 of
Batect's 29 — only `windows-container` remains, out of reach until cross-platform
work starts (see [Rust Enhancements](#rust-enhancements)). Staying green there, not
the field/flag tables, is what [`ratect-compat`'s 1.0.0](RELEASES.md#ratect-compat)
actually gates on.

## Two Binaries: `ratect` and `ratect-compat`

A Cargo workspace with a shared core library and two thin binary crates:
`ratect-compat` (strict Batect compatibility — where all [Batect
Parity](#batect-parity) work lands) and `ratect` (forward-looking, free to
diverge). Full rationale, alternatives, and consequences:
[decisions/0001](decisions/0001-two-binaries.md).

## Versioning & Releases

`ratect-compat` and `ratect` are versioned independently, sharing a release
process — see [decisions/0001](decisions/0001-two-binaries.md#consequences)
for why (independent version lines, tag prefixes, the shared `CHANGELOG.md`,
per-cycle crate bumps), and [AGENTS.md](AGENTS.md)'s Version Lifecycle
guideline for the actual release-cutting process — the `-dev` cycle, tagging,
and what pushing a tag triggers.

### `ratect-compat`

Moved to [`RELEASES.md`](RELEASES.md#ratect-compat). This heading stays so that
links written before the split still resolve.

### `ratect`

Moved to [`RELEASES.md`](RELEASES.md#ratect). This heading stays so that links
written before the split still resolve.


## Rust Enhancements

Leveraging Rust's strengths to provide a superior experience compared to the original JVM-based implementation.

- **Parallel Task Execution**: within-task startup shipped as [`ratect-compat`
  0.15.0](RELEASES.md#ratect-compat). Running independent *prerequisite tasks*
  concurrently too — which Batect itself doesn't do — remains a possible
  enhancement for later, not currently scheduled.
- **First-class Cross-platform Support**: **Windows doesn't exist here at all
  yet** — no target in the release matrix, no CI coverage, no Windows-specific
  code path. macOS and Linux already have prebuilt binaries (see
  [decisions/0010](decisions/0010-release-binary-distribution.md)). Two
  specifics worth naming for whenever Windows work starts, so
  "cross-platform" isn't taken to already imply them: **Windows isolation
  mode** (`process` versus `hyperv`, applied to both builds and container
  runs) is a config/CLI surface Ratect doesn't have at all, and Batect
  wanted it too; and **live terminal-resize forwarding is Unix-only by
  construction** — it's built on `tokio::signal::unix`'s `SIGWINCH`
  listener, which has no Windows equivalent. Batect's remaining Windows
  items are JVM artefacts with no Ratect equivalent (a 32-bit JVM
  named-pipe hang, reading version details out of `kernel32.dll`).

## UX & Tooling

Improving the developer experience through better tools and feedback.

- **`ratect doctor`**: shipped — see [CLI reference](docs/ratect-cli.md#doctor) for
  what it checks today. Still open: host-container permission issues
  (`run_as_current_user` against the actual uid/gid of a mounted path) need a real
  filesystem probe rather than a config read; container-level checks that need the
  *image* (its own `HEALTHCHECK`, whether an `entrypoint` exists) need a pull, so
  they'd belong behind a flag rather than the default run; and four checks from
  Batect's own unbuilt `doctor` wishlist remain: mounting a directory writable
  without `run_as_current_user` enabled (the root-owned-files trap that field
  exists to prevent), mounting a directory over the `run_as_current_user` home
  directory, a proxy environment variable that isn't a URL or doesn't use an
  `http`/`https` scheme, and the daemon's own proxy settings not matching the
  local environment's (readable from the Docker API, so it belongs with the
  daemon-reachability check rather than the config ones). Batect's fifth —
  warning on container/task naming conventions — is deliberately skipped: Ratect
  has no convention to enforce, and inventing one to lint against would be the
  tool overreaching.
- **Improved Progress UI**: output-mode selection and live per-container progress
  shipped as Batect parity ([0.16.0](RELEASES.md#ratect-compat)). What remains is
  going *beyond* Batect — build context upload progress, richer pull progress
  (per-layer byte counts), any `ratect`-binary-specific presentation ideas — plus
  four items from Batect's own unbuilt roadmap:
  - **A countdown to the next health check** while waiting for a dependency ("next check in 3 seconds, will time out after 2 more retries") — the wait is currently opaque, which makes a slow-starting dependency indistinguishable from a hung one at the exact moment that distinction matters most.
  - **Wrap text in `fancy` output** rather than letting a long line run off the edge. Note `fancy.rs` already clips to the real display width via `unicode-width`, so the machinery to measure is there — this is about what to *do* at the boundary.
  - **A log-aggregation output mode** (Batect's example was starting a Seq instance and pointing every container's logs at it). Ratect's `EventSink` design makes an extra mode cheap to add; the open question is whether a task runner should be starting a log server on your behalf, or just be easy to point at one you already run.
  - **Cheaper repaints in `fancy` mode.** Batect wanted to batch console updates rather than reprinting on every event. Ratect is already better in one direction — `fancy.rs:59` skips a repaint entirely when the content hasn't changed — and worse in another: it repaints the whole block per event, where Batect diffs and rewrites only the lines that changed (`fancy.rs:26`). Deliberately left as a future item rather than scoped: nobody has reported it and the cost hasn't been measured, so the honest first step is a measurement (a task with many dependencies emitting events rapidly) rather than an optimisation.

  Also open: `NO_COLOR`, `CLICOLOR_FORCE` and `COLORTERM` are honoured *nowhere* in
  either binary — only the explicit `--no-color` flag exists. `NO_COLOR` in
  particular is the convention users reach for now, and it's what a CI system
  sets. (Terminal-capability auto-detection itself is settled, deliberately
  staying heuristic rather than moving to terminfo — see
  [`supports_interactivity`](ratect-core/src/ui.rs)'s own doc comment for why.)
- **Watch Mode**: Automatically re-running tasks when source files change.
- **Documentation beyond reference material** — tracked here as roadmap work, not as
  an afterthought, because for a task runner the documentation *is* a large part of
  the user experience: the tool's whole value is being easy to adopt on an existing
  project, and nobody adopts what they can't get started with. Ratect's `docs/` is
  strong on reference (two CLI references, two config references,
  [how-it-works](docs/how-it-works.md), [task-lifecycle](docs/task-lifecycle.md),
  [differences-from-batect](docs/differences-from-batect.md),
  [installation](docs/installation.md), [getting-started](docs/getting-started.md))
  and has nothing in the shapes below — which is also, near enough, Batect's own
  unbuilt documentation list, so the gap is inherited rather than newly created:
  - **Worked examples per language/ecosystem** — a real `ratect.toml` for a Rust,
    Go, Node, Python and JVM project. The single most-requested shape of
    documentation for a tool like this, and the fastest path from "interesting" to
    "running".
  - **How to introduce Ratect to an existing project** — incremental adoption,
    starting from one task rather than converting everything. Ratect has an
    unusually strong story here that's currently undocumented: `ratect config
    convert` for a `batect.yml`, mixed TOML/YAML includes so a project migrates a
    file at a time, and `ratect-compat` as a drop-in first step.
  - **An FAQ** — when to mount a directory versus copying files into the image; how
    to run something at container start regardless of the task's command
    (`ENTRYPOINT` plus `exec`); why task idempotency matters; raising Docker
    Desktop's CPU/memory limits on macOS.
  - **How Ratect compares to other tools** — Docker Compose, Make, Task, Earthly,
    Dagger, `just`. Batect's own list named Cage and Toast, both largely dormant
    now; the comparison worth writing is against what people actually reach for
    today, which is a different set than when Batect wrote that entry.
  - **Using Ratect as reusable pipeline building blocks** — what Git includes and
    bundles are actually *for*, which the [config
    reference](docs/config-reference.md#includes) documents mechanically without
    ever making the case for.
- **A rendered docs site** — right now `docs/` is only readable as raw Markdown on
  GitHub: no navigation, no search, no landing page for someone who hasn't heard of
  Ratect yet. Batect has exactly this in `batect.dev`; Ratect has no discoverability
  story at all beyond the repository itself (`gh repo view` shows no homepage URL
  set). Undecided: the rendering tool (mdBook, Docusaurus, Zola, ...), hosting
  (GitHub Pages is the obvious default, no infra to run), and domain
  (`ratect.dev`, matching Batect's own naming, would need registering). Content
  is what `docs/` already has plus whatever lands from the worked-examples/FAQ/
  comparison items above — this is about presentation and discoverability, not
  new writing.

## Future Vision

Exploring innovative features that go beyond the original Batect, as well as planned improvements from the Batect roadmap.

- **A repo-keyed allowlist for `allow_host_paths`**: today's grant
  ([decisions/0004](decisions/0004-git-include-host-path-trust.md), shipped
  ratect-compat 0.24.0 · ratect 0.3.0) is per-include and non-recursive — a
  nested bundle (one your own bundle includes, that you don't control) can't
  be granted it at all, since the project owner can't annotate an include
  entry they didn't write. Deliberately left open, on evidence: a survey of
  nine real bundles found none using a host path through a nested include.
  If a real case appears, the shape to reach for is a repo-keyed grant in the
  owner's own config, not a recursive boolean — see that ADR's Consequences
  for the security properties any change here must preserve.
- **Wildcard Includes**: Support for including multiple files using glob patterns (e.g., `include: containers/*.yaml`). Batect wanted this too, and never built it.
- **Configuration Merging/Replacement**: Ability to merge or override containers and tasks when including files.
- **Init Containers**: Support for containers that must start, run, and complete before other containers can start (e.g., for database initialization).
- **External Health Checks**: Support for external health checks (e.g., HTTP) that don't require specialized tools like `curl` to be installed within the container.
- **Image Lifecycle Management**: Tools for building and pushing images independently of task execution, and cleaning up unused images.
- **OCI annotations on built images**: a config field for build-time image labels (`source`, `revision`, `created` — distinct from `Container.labels`, which applies to the *container*, not the image it builds), as the project's own provenance on an image `build_directory` produces. Today that's a Dockerfile `LABEL`, which already works and needs nothing from Ratect. Ratect shouldn't guess these itself (shelling out to `git` in the build context would be wrong as often as right) — only worth building if someone actually wants the ergonomics. `ratect`-only, since Batect has no such field. Not to be confused with [decisions/0002](decisions/0002-runtime-ownership-labels.md)'s runtime-ownership labels, a different thing on a different object (a running container/network, not the image).
- **`ulimit` Support**: Support for setting `ulimit` values for containers.
- **Secrets Management**: Integrated support for securely handling sensitive information like API keys and credentials.
- **Plugin System**: A flexible architecture to allow users to extend Ratect's functionality with custom logic.

The bullets below all come from Batect's own open issues and unbuilt roadmap — ideas
it wanted and never shipped, so they're enhancements rather than parity work, and
each needs its own decision about which binary it belongs to. Recorded here after a
pass over Batect's remaining 7 open issues and its `ROADMAP.md`, so the ideas aren't
lost when the archived repository eventually becomes hard to consult.

- **HTTP Includes** ([batect#1230](https://github.com/batect/batect/issues/1230)): a third `include` type fetching a config file over HTTP, so a bundle can be published and versioned alongside the images it uses, in the same artifact repository — Git includes make versioning and auth awkward for that. Needs the trust question answered first: an HTTP include is a fetch-and-execute of arbitrary configuration, so it inherits everything [decisions/0004](decisions/0004-git-include-host-path-trust.md) works through for Git includes, plus caching and integrity (a Git ref at least names a commit; a URL names nothing). `ratect`-only, most likely — `ratect-compat` has to keep Batect's own behavior
for parity, same reasoning as `allow_nested_git_includes` (shipped `ratect`
[0.4.0](RELEASES.md#ratect)).
- **Arguments on a prerequisite reference** ([batect#1053](https://github.com/batect/batect/issues/1053)): today `-- ADDITIONAL_ARGS` reaches only the explicitly-invoked task, never its prerequisites, in both tools — so a `build` task that takes arguments can't be reused as a prerequisite with different ones. Batect's proposed spelling (`prerequisites: [run-gradle build]`) overloads the string; a native-format `ratect.toml` can give a prerequisite entry a proper object shape instead, which is a good argument for this being `ratect`-only.
- **Setup commands that run in a different container** ([batect#286](https://github.com/batect/batect/issues/286)): `setup_commands` always run inside the container that declares them; this is the "run a command in container B once container A is healthy, before A's dependents start" case (typically seeding a database from a client image that isn't the database itself).
- **Tasks that run on the host** ([batect#78](https://github.com/batect/batect/issues/78), Batect's oldest open enhancement): a task that executes on the host rather than in a container, so one tool runs *every* task in a workflow and host steps can participate in the dependency graph. The largest philosophical departure on this list — it trades away the reproducibility that is the entire point of a container-based task runner — so it needs a decision about whether Ratect wants to be that tool at all, not just an implementation.
- **Dependency relationships between containers and tasks**: letting a container declare that a task must run before it starts (Batect's example: the app container requires the build task), removing the need to repeat that task as a prerequisite on every task that starts the container.
- **Per-container graceful shutdown**: cleanup currently stops containers uniformly; Batect wanted the default to be fast termination with an opt-in graceful shutdown for containers where it matters (a database with data shared between invocations, which an abrupt stop can corrupt).
- **Clone Git includes in parallel**: `config.rs`'s include-resolution loop calls `ensure_cached` one entry at a time, so a project with several Git includes clones them serially on first use. The per-entry lock already exists; the open question Batect noted is what to do about a repository needing interactive authentication, which parallel cloning would interleave unreadably.
- **GitHub Actions integration** via [workflow commands](https://docs.github.com/en/actions/reference/workflow-commands-for-github-actions): surfacing configuration errors and task failures as real annotations against the offending file and line, rather than as text buried in a log. The highest-value integration on Batect's list — CI is where a task runner spends most of its life — and Ratect is better placed to do it well, since its config errors already carry precise position information. Detection is the standard `GITHUB_ACTIONS` environment variable, so it needs no flag.
- **Visualise a run on a timeline**: where the time actually went, and what was waiting on what — image pulls and builds, container creation, health-check waits, setup commands, the task's own command, cleanup, each on its own lane. Batect listed this among its contributor tooling rather than its features, and the shape is legible from its code: `--log-file` writes structured JSON (`LogMessage` — timestamp, severity, message, arbitrary `additionalData`), so the tool it wanted was a viewer over one run's log, with the parsed configuration as a second tab (the item below).

  **More useful for Ratect than it would have been for Batect**, because Ratect actually runs things concurrently — within-task startup since 0.15.0, and the task container's readiness gate alongside its own command since 0.21.0 — and `--max-parallelism` funnels pulls, builds, dependency starts and setup commands through a single invocation-wide semaphore. "What was this step waiting behind?" therefore has a real, non-obvious answer that no amount of reading the config will give you.

  **Don't build a viewer.** The modern equivalent is emitting a standard trace — the Chrome Trace Event format (`chrome://tracing`, Perfetto) or OTLP — and letting existing tooling render it, which is far better than anything worth hand-building here and costs almost nothing on top of `tracing`, already a dependency.

  **The actual work is instrumentation, not output.** Ratect currently has ~41 `tracing` events across `ratect-core` and *no spans at all* — no `#[instrument]`, no `span!` — so there is no duration data to plot today. Adding spans around the operations above is the item; the trace export is a small step after it. Worth doing on its own merits regardless: spans would improve `RUST_LOG` debugging immediately, well before anything renders a timeline.
- **Show the configuration as parsed**: what Ratect actually resolved — after includes are merged, expressions interpolated, paths resolved and `extends` applied — which is exactly the state that's hardest to reason about from the source files and the first thing anyone wants when a task doesn't do what the config appears to say. Pairs naturally with `ratect config validate`/`convert`, which already do all the loading and resolution work and would need only a serialization step. Batect wanted this too, as the second tab of the timeline tool above.
- **Run configurations for multiple containers**: Batect's "stereotypical `run` configuration" — start a service together with its dependencies and leave them running — with explicit options for when the group exits (when any container stops, or when all do) and whose exit code becomes the task's (any non-zero, a nominated container, or the first to exit). Today a task has exactly one container whose exit ends it, which doesn't express "bring this stack up".
- **Reference another Dockerfile as a base image**, so one container's built image can be another's `FROM` without pushing it to a registry first.
- **A language server for the config formats**: substantially answered already by the two committed JSON schemas (`schema/batect-config.schema.json`, `schema/ratect-config.schema.json`), which give autocompletion, hover documentation and invalid-field warnings in any editor with YAML or TOML language support — recorded here so that's understood as the deliberate answer rather than an accident. A real language server would add what a schema structurally cannot: resolving `include`s to validate cross-file references, go-to-definition on a container or prerequisite name, and flagging a dependency cycle. Worth it only on evidence that the schema's ceiling is being hit.
- **Built-in security scanning of the images Ratect builds and runs**: report known vulnerabilities in a task's images — as its own verb, and optionally as a gate that fails a task on findings above a threshold. Distinct from CI dependency scanning (Ratect's own `cargo audit`) in that it covers what a *user's* tasks pull and build, which is usually the larger and less-examined surface. The design question is whether Ratect should embed this at all: today it's already achievable by running a scanner as a container, which is what Ratect is for — indeed the real-world bundle that motivated [decisions/0004](decisions/0004-git-include-host-path-trust.md) was doing exactly that, with a Trivy cache under the home directory. So the honest framing is that scanning already *works* via a bundle, and this item is about whether making it first-class (image discovery from the config, a consistent report across output modes, a threshold to fail on) earns its keep over a well-written bundle that any project can already use.
