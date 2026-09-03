# Ratect Roadmap

This document outlines the planned journey for Ratect, from achieving parity with Batect to implementing Rust-specific enhancements and future innovations.

## Batect Parity

The primary goal is to support the core features of Batect to ensure a seamless transition for existing users. This work targets the [`ratect-compat` binary](#two-binaries-ratect-and-ratect-compat) specifically — the `ratect` binary is not expected to maintain 1:1 Batect parity.

- **Image Building**: Building a Docker image from a `build_directory`, including `build_args` and `.dockerignore` support (0.3.0), custom Dockerfile naming/location (`dockerfile`), a multi-stage build target (`build_target`), secrets (`build_secrets`), and SSH keys for a build (`build_ssh` — multiple named agents, forwarding the host's own agent or another by socket path, and serving explicit private key files with no agent running; 0.11.0, completed in 0.25.0), building with the builder the daemon advertises as its default — BuildKit on any modern daemon, matching Batect, with `DOCKER_BUILDKIT` honored as the force-on/off override (0.12.0) — see [config reference](docs/config-reference.md#image-building). Cross-invocation build caching and automatic image cleanup are not implemented.
- **Full Docker Networking**: Every task execution gets its own isolated network (see [the task lifecycle](docs/task-lifecycle.md)), `--use-network` reuses an existing one instead, `additional_hostnames`/`additional_hosts` add extra aliases/`/etc/hosts` entries, and `ports`/`--disable-ports` publish container ports to the host, including port ranges and the expanded object form, plus additional per-task `run.ports` (0.6.0) — see [config reference](docs/config-reference.md#port-mappings) and [CLI reference](docs/cli-reference.md).
- **Interactive Mode**: A task's own container gets a real Docker TTY, automatically, when both Ratect's own stdin and stdout are real terminals (0.4.0); its stdin forwarding and the host's `TERM` propagation both apply more broadly than that (whenever the task is interactive-eligible, not gated on a real TTY), and a real TTY's terminal size stays in sync for the whole session, not just once at attach (0.10.0) — see [Interactive mode](docs/config-reference.md#interactive-mode). One known, deliberate divergence from Batect remains — see [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps).
- **Full Environment Variable Interpolation & Batect Expressions**: `environment` on containers/tasks, `config_variables` (including Batect's one built-in, `batect.project_directory`), and `$VAR`/`${VAR}`/`${VAR:-default}`/`<name`/`<{name}` expressions are implemented for `environment` values, volume host paths, `build_directory`, `build_args`, a `build_secrets` entry's `path` (0.11.0), and a `build_ssh` entry's `paths` (0.25.0) — every field Batect supports one in — see [Expressions](docs/differences-from-batect.md#expressions).
- **Dependency Readiness**: A started dependency isn't treated as ready until it
  reports healthy (its image's own Docker health check, or the `health_check`
  override) and completes its `setup_commands` — only then do its dependents start
  (0.9.0) — see [config reference](docs/config-reference.md#dependency-readiness).
  The task's *own* container goes through this same gate too, concurrently with its
  main command rather than gating anything on it (0.21.0) — see [task
  lifecycle](docs/task-lifecycle.md#known-simplifications-relative-to-batect) for the
  one residual race this still shares with Batect, and [Differences from
  Batect](docs/differences-from-batect.md#container-fields).
- **Includes**: Local file includes — splitting one project's configuration across multiple files via the top-level `include` directive, resolved relative to each declaring file's own directory and merged into one flat `containers`/`tasks`/`config_variables` set (0.7.0) — and Git includes/bundles — importing shared tasks/containers from a separate repository, cloned once and cached forever at `~/.ratect/incl` (0.8.0), with a 30-day automatic cache eviction sweep matching Batect's own (0.19.0) — see [config reference](docs/config-reference.md#includes) and [Differences from Batect](docs/differences-from-batect.md#top-level-fields).
- **Full Configuration Parity**: Support for all available Batect configuration options and standard YAML structures. See [Differences from Batect](docs/differences-from-batect.md#configuration-format) for the itemized current status of every field.
- **Volume Mounts**: `volumes` supports all three of Batect's mount kinds — `local` (`local:container[:options]`), `cache` (a named volume that persists between separate `ratect` invocations — a Docker named volume by default, or a host directory under `--cache-type=directory`, plus `--clean`/`--clean-cache` to clear them out, [0.18.0](RELEASES.md#ratect-compat)) — see [Cache volumes](docs/config-reference.md#cache-volumes) — and `tmpfs` (an in-memory, ephemeral mount, lost when the container exits, [0.21.0](RELEASES.md#ratect-compat)) — see [Tmpfs mounts](docs/config-reference.md#tmpfs-mounts).
- **Config Schema**: A JSON schema describing Ratect's actual accepted `batect.yml` shape, for editor autocompletion/validation — generated from `ratect-core/src/config.rs`'s own types via `schemars` rather than hand-maintained separately, and committed at [`schema/batect-config.schema.json`](schema/batect-config.schema.json) ([0.21.0](RELEASES.md#ratect-compat)) — see [Editor autocompletion and validation](docs/config-reference.md#editor-autocompletion-and-validation). Deliberately **not** Batect's own published schema (listed in [SchemaStore's catalog](https://www.schemastore.org/api/json/catalog.json) for `batect.yml`/`batect-bundle.yml`, hosted at `ide-integration.batect.dev`) — that reflects Batect's full field set, not Ratect's subset, so it would either validate fields Ratect doesn't actually support (a false pass in the editor) or reject a future Ratect-only extension as invalid (a false failure). Not submitted to SchemaStore itself — that's a separate, later decision.
- **Full CLI Options Parity**: Support for all standard Batect CLI flags and options (e.g., `--config-file`, `--override-image`, cleanup control flags, etc.). See [Differences from Batect](docs/differences-from-batect.md#cli-flags) for the itemized current status of every flag.
- **User Mapping**: A container can run as the host's own user/group (`run_as_current_user`) instead of the image's default, so files it writes to a mounted volume aren't root-owned (0.5.0) — see [User mapping](docs/config-reference.md#user-mapping). Host-side uid/gid lookup is Unix-only — see [Differences from Batect](docs/differences-from-batect.md#container-fields).
- **Proxy Support**: `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` are detected from the host environment and propagated into containers and image builds automatically, `--no-proxy-vars` to disable (0.6.0) — see [Proxy environment variables](docs/config-reference.md#proxy-environment-variables). A proxy on the host is reached on every platform, including Linux, where the `localhost` rewrite is paired with the `host.docker.internal:host-gateway` entry that makes the name resolve and a warning for a proxy bound to loopback only ([0.26.0](RELEASES.md#ratect-compat)) — a deliberate improvement on Batect, which never closed its own oldest issue here. There's still no Docker-version-gated hostname fallback chain, an accepted gap — see [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps).
- **Docker API version negotiation** — **not implemented, and it costs
  compatibility.** `bollard`'s `API_DEFAULT_VERSION` is pinned at **1.53** and
  nothing in the workspace calls `negotiate_version`, so every request goes out
  at 1.53 and any older daemon refuses all of them with "client version is too
  new". Nothing Ratect does needs an API that recent — the newest feature it
  relies on is Docker 20.10's `host-gateway` — so the effective floor is set by
  a default nobody chose. Batect negotiates, via the Docker CLI's own Go client.
  One call at connection time closes it; the reason to think before making it is
  that negotiating down means some requests may then hit a daemon that doesn't
  support them, which wants a clear error rather than a confusing one.
- **Registry credentials** — **not implemented, and a parity gap rather than a new
  feature.** Ratect passes `None` where `bollard` takes registry credentials —
  `create_image` for a pull, both `build_image` paths for a build — so an `image`
  from a private registry fails to pull unless something else has already put it
  in the daemon's local store. It *does* read `~/.docker/config.json`, for the
  `currentContext` field only (`docker.rs`), and `--docker-config` already points
  at that directory with `DOCKER_CONFIG` then `~/.docker` as its defaults. So the
  gap is narrower than "doesn't read the Docker config": the file is read and its
  credential sections are ignored.

  Batect does support this, through its own `docker-client`: the Go wrapper calls
  `credentials.DetectDefaultStore(configFile.CredentialsStore)` and sends
  `RegistryAuth` on every `ImagePull`. Its two build paths differ, and Ratect's
  two will need to differ the same way — the classic builder reads
  `GetAllCredentials()` (`images_build_legacy.go`), while BuildKit hands the
  config file to `authprovider.NewDockerAuthProvider` and serves auth over the
  build session (`images_build_buildkit.go`).

  Easy to miss from the outside, because a developer who has run `docker login`
  sees it work and has no reason to attribute that to the task runner. It is also
  the reason a credential-helper prompt can appear during an otherwise ordinary
  build.

  Worth deciding deliberately rather than porting on sight, since closing it means
  reading a credential store and putting registry tokens into `X-Registry-Auth`
  headers: prefer delegating to Docker's own config semantics (store detection,
  per-registry `credHelpers`, `DOCKER_CONFIG`) over reimplementing helper
  invocation, and settle what happens when a helper fails — Docker's CLI ignores
  those errors, which is a choice to make on purpose rather than inherit. Must be
  closed before [1.0.0](RELEASES.md#ratect-compat), which claims parity substantially checked
  off against real Batect projects; a private registry is common enough in the
  corporate setting Batect was built for that the conformance corpus wouldn't
  necessarily catch it.

## Two Binaries: `ratect` and `ratect-compat`

A Cargo workspace with a shared core library (config parsing, task engine,
`ContainerRuntime`/Docker integration) and two thin binary crates on top:
**`ratect-compat`**, a strict flag-for-flag/field-for-field drop-in for Batect's
CLI and `batect.yml` (where all [Batect Parity](#batect-parity) work lands), and
**`ratect`**, the forward-looking CLI free to diverge (subcommands, a native
config format, modern-Rust-CLI conventions). No binary is literally named
`batect`. Because both share the core, an eventual migration path from a
`ratect-compat`-managed project to a `ratect`-managed one is a goal in its own
right, not a side effect.

Full rationale, alternatives, and consequences:
[decisions/0001](decisions/0001-two-binaries.md).

## Versioning & Releases

`ratect-compat` and `ratect` are versioned **independently** — they're on different
maturity clocks, and forcing one number to serve both meanings breaks the moment they
diverge (which they will, since `ratect-compat` has a head start). What *is* shared is
the release **process**: a fix in the shared core crate gets released for both binaries
at the same time (one PR/tag/CI run), each bumping its own patch version independently
— not the same version number, just released together, so nobody is left running a
stale, unpatched core. The core crate itself isn't published or meaningfully versioned
on its own; it's an internal implementation detail, not something either binary's users
interact with directly.

Mechanically, **every** `Cargo.toml` in the workspace sits at `X.Y.Z-dev` between
releases — both binaries and `ratect-core`, whichever binary a given cycle is
actually about, so a build from `main` never claims to be a released version.
Cutting a release is one isolated `chore:` commit that bumps the crates being
released to the plain `X.Y.Z` and moves `CHANGELOG.md`'s accumulated `Unreleased`
entries under a new dated heading naming every version in that release — e.g.
`## [ratect-compat 0.21.1 · ratect 0.2.0]`, or just the one binary when it's
released on its own. That commit is tagged and published as a GitHub Release
(`prerelease: true` until a binary's own 1.0.0 — see below — with that
`CHANGELOG.md` section as its body; a joint release uses that same section for
both, which is correct, because it *is* the same set of changes). The next
commit — starting the following version's development, also isolated, also
`chore:` — bumps them back to the next `X.Y.Z-dev`. Neither bump is ever folded
into a feature commit.

Three mechanics that only became concrete once `ratect` started its own release
cycle (0.2.0, the first one not about `ratect-compat`):

- **Tags are prefixed with the binary they release** — `ratect/v0.2.0`,
  `ratect-compat/v0.21.1` — because the two version lines will collide otherwise:
  `v0.2.0` is already taken, by `ratect-compat`'s own 0.2.0 back when it was the
  only binary. Bare `vX.Y.Z` tags (`v0.1.0` through `v0.21.0`) are that history and
  stay exactly as they are; nothing renames them. Everything from here on is
  prefixed, `ratect-compat` included, rather than leaving one binary on a legacy
  scheme.
- **One shared `CHANGELOG.md`, not one per binary.** Most substantive work is in
  `ratect-core` and so reaches both binaries — the anonymous-volume fix
  ([0.21.1](RELEASES.md#ratect-compat)) is the pattern, not the exception — so two files
  would be largely the same prose under different headings, drifting apart on
  every core change. (That's the opposite of the CLI reference docs, which *are*
  split per binary: those overlap by almost nothing, since they document
  different flags. Split where the content differs, share where it doesn't —
  which is also why `config-reference.md` and `task-lifecycle.md` are shared.)
  An entry with no binary named applies to both; one that doesn't says
  `(ratect only)`/`(ratect-compat only)`, so the annotation cost falls on the
  rarer case. Revisit only if `ratect` diverges far enough that shared-core
  changes stop being the bulk of the work — 0.3.0's own config format is a step
  that way — since cutting one file in two later is easy, and merging two back
  into one isn't.
- **A cycle bumps the crates it actually changes.** A `ratect`-only cycle still
  moves `ratect-core` (it's the same shared crate, and its number has always run
  with the release cadence rather than standing still) and still leaves
  `ratect-compat` on a `-dev` of its own — a patch bump if nothing but the shared
  core moved underneath it, a minor one if it gained anything itself. Which of the
  two it turns out to be is decided at release time; the `-dev` number in between
  is a statement of intent, not a commitment.

### `ratect-compat`

Moved to [`RELEASES.md`](RELEASES.md#ratect-compat). This heading stays so that
links written before the split still resolve.

### `ratect`

Moved to [`RELEASES.md`](RELEASES.md#ratect). This heading stays so that links
written before the split still resolve.


## Rust Enhancements

Leveraging Rust's strengths to provide a superior experience compared to the original JVM-based implementation.

- **Parallel Task Execution**: within-task container startup (image pulls/builds, health-check waits, setup commands for independent branches of one task's dependency graph) now runs concurrently via `tokio` — shipped as `ratect-compat` [0.15.0](RELEASES.md#ratect-compat), since it also closed a Batect parity gap (Batect does exactly this, just not more). Running independent *prerequisite tasks* concurrently too — which Batect itself doesn't do — remains a possible Rust-specific enhancement for later, not currently scheduled.
- **Static Binaries**: Distribution as zero-dependency static binaries (`ratect` and `ratect-compat`) for easy installation and portability. `x86_64-unknown-linux-musl` belongs in the release matrix specifically: Batect's only open *bug* ([batect#1335](https://github.com/batect/batect/issues/1335), `priority:high`, still unresolved) is that it can't start on Alpine at all — its JNI Docker-client wrapper is extracted to `/tmp` and fails to relocate against musl. Ratect can't have that failure (bollard talks to the daemon socket directly, with no native library to extract), but the issue is evidence that Alpine CI images are a real user environment rather than a niche one, and a glibc-only build would find its own way to fail there.
- **First-class Cross-platform Support**: Providing a high-performance, native experience across macOS, Linux, and Windows without the overhead or startup latency of a JVM. Two specifics worth naming, so "cross-platform" isn't taken to imply them: **Windows isolation mode** (`process` versus `hyperv`, applied to both builds and container runs) is a config/CLI surface Ratect doesn't have at all, and Batect wanted it too; and **live terminal-resize forwarding is Unix-only by construction** — it's built on `tokio::signal::unix`'s `SIGWINCH` listener, which has no Windows equivalent, so Batect's own "send updated console dimensions to the daemon if the console is resized" item is an open gap here rather than a covered one. Batect's remaining Windows items are JVM artefacts with no Ratect equivalent (a 32-bit JVM named-pipe hang, reading version details out of `kernel32.dll`).
- **Precise Error Reporting**: Utilizing Rust's type system and error handling to provide clear, actionable feedback on configuration errors and execution failures.

## UX & Tooling

Improving the developer experience through better tools and feedback.

- **`ratect doctor`**: ~~A built-in linter and diagnostic tool to validate configuration and environment setup. This will include checks for `latest` image tags, missing health checks on dependencies, and host-container permission issues. Should also report anything the orphaned-resource work below finds.~~ — shipped ([0.2.0](RELEASES.md#ratect)) with the daemon-reachability, config-loads, `build_directory`/Dockerfile, floating-tag, dependency-without-`health_check` and leftover-resource checks; exits non-zero for problems but not warnings, so it works as a CI step. A leftover `batect`/`batect.cmd` wrapper script that still runs the JVM binary is flagged too (matched by content, so a wrapper repointed at Ratect isn't), as migration assistance. Host-container permission issues (`run_as_current_user` against the actual uid/gid of a mounted path) are the obvious next check and aren't done — they need a real filesystem probe rather than a config read. Container-level checks that need the *image* (whether it defines its own `HEALTHCHECK`, whether an `entrypoint` exists) would need a pull to answer, so they'd belong behind a flag rather than in the default run. Four more checks come from Batect's own `doctor` wishlist, which it specified in its roadmap and never built: mounting a directory writable without `run_as_current_user` enabled (the root-owned-files trap `run_as_current_user` exists to prevent), mounting a directory over the `run_as_current_user` home directory, a proxy environment variable that isn't a URL or doesn't use an `http`/`https` scheme, and the daemon's own proxy settings not matching the local environment's — the last of which is readable from the Docker API rather than the config, so it belongs with the daemon-reachability check rather than the config ones. Its fifth, warning on container/task naming conventions, is deliberately skipped: Ratect has no convention to enforce and inventing one to lint against would be the tool overreaching.
- **Orphaned-resource discovery** (`ratect resources list`/`clean`, working title):
  what's still on this machine from a previous run — after a crash, a `docker
  kill`, a `--no-cleanup`/`--no-cleanup-after-failure` run, or Ratect itself
  failing to tear down. Today answering "what should I remove?" means reading
  `docker ps -a` and guessing, which is precisely the complaint.

  **The blocker is that nothing is marked on the way in**, so this is mostly
  groundwork, not a verb. Containers are created via `create_container(None,
  config)` — no name, and `labels` carries only what the *user* configured — so a
  leftover container is identifiable at best by inference (it's attached to a
  `ratect-<uuid>` network), and under `--use-network` not even that. Batect is no
  better: `DockerContainerCreationSpecFactory` applies `container.labels` and
  nothing of its own, and Batect has no cleanup command at all, which is why this
  has never been answerable. Networks are the one thing that's greppable today,
  purely by their `ratect-` name prefix — and even they can't be attributed to a
  project or a task.

  So the work is, in order:
  1. ~~**Label every resource Ratect creates**~~ — done ([0.21.1](RELEASES.md#ratect-compat)
     /[0.2.0](RELEASES.md#ratect)), in the shape Docker Compose's own
     `com.docker.compose.*` labels have — runtime *ownership*, which is a
     different thing from OCI image annotations (see below):

     | Label | On | Value |
     | --- | --- | --- |
     | `eu.orican.ratect.project` | containers, networks | `project_name` |
     | `eu.orican.ratect.task` | containers, networks | the task being run |
     | `eu.orican.ratect.run` | containers, networks | the per-run id — the `Uuid` that already names the per-task network, reused rather than minting a second |
     | `eu.orican.ratect.container` | containers | the *config* container name (`build-env`), since Docker's own name is random |
     | `eu.orican.ratect.role` | containers | `task` or `dependency` — derivable from the config, but the point is to work without it |
     | `eu.orican.ratect.version` | containers, networks | the Ratect version that created it, for when the label set itself changes |

     These are *additive* to the user's own `labels`, but Ratect's win on an
     exact key collision (they're load-bearing for cleanup). The namespace choice
     (`eu.orican.ratect.*` over a new `ratect.dev` domain; not OCI annotations),
     the `version`-from-the-binary and per-run-id decisions, and the shipped
     `ratect-core/src/labels.rs` mechanics are recorded in full at
     [decisions/0002](decisions/0002-runtime-ownership-labels.md).
  2. ~~**`ContainerRuntime` gains `list_containers`/`list_networks`**~~ — done
     ([0.2.0](RELEASES.md#ratect)), with label filtering (Docker supports `label=key=value`
     filters natively), alongside today's `list_volumes`. Both return one
     `LabelledResource`, since what's worth saying about a leftover container and
     a leftover network is the same; `list_containers` passes `all: true`,
     because a leftover has usually exited and Docker's default hides those.
  3. ~~**The verb itself**~~ — done ([0.2.0](RELEASES.md#ratect)), shaped like `caches`:
     `resources list` shows what's there — grouped by run, with task name and
     age, so "these four containers and a network are from `integration-test`,
     three days ago" is readable at a glance — and `resources clean` removes it.
     Scoped to the current project by default, with `--all-projects` for the
     machine-wide sweep, which is the case the complaint is really about. Also
     `--older-than`, which turned out to matter more than expected — see below.
     Removal takes containers before networks (a network still holding an
     endpoint can't be removed) and a single failure is reported rather than
     abandoning the rest.

  One thing labels can't resolve: a *concurrently running* task's containers are
  labelled identically to an orphan, because they are the same thing until the
  run ends. `list` reporting age, and `clean` taking `--older-than`, is the
  honest mitigation; claiming to detect liveness would not be — the daemon can't
  say whether some other `ratect` process still cares about a container. This is
  documented prominently for `clean`, since a bare sweep on a shared machine can
  take an in-flight run with it. If that turns out to bite in practice, the next
  step would be a heartbeat (a running invocation touching its own resources
  periodically) rather than any attempt to infer liveness after the fact.

  Two safety measures considered and **deliberately not built**, recorded so
  they aren't re-litigated from scratch:

  - **A `--dry-run` for `clean`.** Unnecessary: `list` and `clean` take the same
    options and select through the same code, so `list` already shows exactly
    what `clean` would remove. A flag would be a second spelling of an existing
    command and a second thing to keep in step with it. Both are a snapshot
    either way — a run can start, or a resource age into `--older-than` scope,
    between the two — and a `--dry-run` followed by the real command has the
    identical window.
  - **A confirmation prompt on `clean --all-projects`.** The one thing a dry run
    can't help with: typing the dangerous command by accident, which only a
    prompt catches, since a dry run helps only if you remembered to use it.
    Deferred rather than rejected — it would be the first interactive prompt in
    either binary (Batect has none, so there's no precedent to follow), it needs
    a `--yes` escape for CI, and the two-layer guard on what `--all-projects`
    can even reach ([0.2.0](RELEASES.md#ratect)) already removes the catastrophic version
    of the mistake. Worth revisiting on the first report of a near-miss.

  Cache volumes stay outside this: they're deliberate, not leftovers, and
  `caches` already finds them by name prefix. (They also *can't* carry labels
  today without creating them explicitly rather than letting a bind mount
  auto-create them — a separate change, only worth making if it buys something
  else.)

  **Anonymous volumes** were the one genuinely invisible leftover, and are fixed
  at source rather than by this verb: containers are now removed with Docker's
  `v` option ([0.21.1](RELEASES.md#ratect-compat)), so a `VOLUME`-declaring image no longer leaves a dangling
  volume per container per run. That had to be a fix rather than a feature —
  Docker names anonymous volumes with a random hash and they can carry no labels
  (Docker creates them implicitly, so Ratect never sees a point at which to mark
  one), which makes them the one resource `resources list` could never have
  identified. The complete inventory this verb covers, then: **containers** and
  **networks** (labelled, above); **cache volumes** and **cache directories**
  (`caches`, already shipped); **built images**, which are tagged
  `<project>-<container>` and are a deliberate cache rather than a leftover —
  worth *reporting* eventually, never worth deleting by default; **anonymous
  volumes**, no longer created; **tmpfs mounts** and **exec instances**, which
  die with their container; and the **Git include cache** under `~/.ratect/incl`,
  which is host filesystem rather than Docker and has its own sweep plus the
  management command below.

  **Not OCI annotations, deliberately.** `org.opencontainers.image.*` is a fixed
  vocabulary describing an *image's provenance* — `source`, `revision`,
  `created`, `licenses`, `title` — and none of it means "the task that started
  this container" or "the run it belonged to". There's no OCI key for runtime
  ownership because OCI doesn't model runtime objects at all; Docker networks
  aren't OCI objects in the first place, so half of what needs labelling here
  couldn't carry them regardless. Bending `image.title` to hold a task name
  would be a misuse of a spec'd key, and the collision risk that reverse-DNS
  namespacing exists to prevent is precisely what it would create. Docker
  Compose reached the same conclusion with `com.docker.compose.project`/
  `.service`, as did Podman with `io.podman.*` — vendor-namespaced ownership
  labels, alongside OCI annotations rather than instead of them.

  The complementary half is real, though, and stays a separate idea: OCI
  annotations belong on the images a `build_directory` container *builds*, as
  the project's own provenance (`source`, `revision`, `created`). Ratect
  shouldn't invent those — only the project knows its own repository and commit,
  and guessing by shelling out to `git` in the build context would be wrong as
  often as right. Today that's a Dockerfile `LABEL`, which already works and
  needs nothing from Ratect. A config field for build-time image labels (as
  distinct from `Container.labels`, which applies to the *container*) would be
  the way to make it ergonomic — `ratect`-only, since Batect has no such field,
  and worth doing only if someone actually wants it.

  **Both binaries label**, decided: the labelling lives in the shared core, and
  the difficulty this solves is `ratect-compat` users' difficulty today, since
  that's the binary anyone actually runs. It's a parity divergence — Batect
  writes no labels of its own — but a strictly additive one that changes no
  behavior and can't break a task that starts using `ratect-compat`, in the same
  family as the `Capability` superset and the UUID cache key. Needs documenting
  in [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps)
  as visible-in-`docker inspect` rather than internal, which is the one way it
  differs from those two.
- **Improved Progress UI**: Output-mode selection with terminal-capability auto-detection and a live per-container progress display shipped as `ratect-compat` [0.16.0](RELEASES.md#ratect-compat) (they were Batect parity work); what remains here is going *beyond* Batect — e.g. build context upload progress, richer pull progress (per-layer byte counts), and any `ratect`-binary-specific presentation ideas. Four more come from Batect's own unbuilt roadmap:
  - **A countdown to the next health check** while waiting for a dependency ("next check in 3 seconds, will time out after 2 more retries") — the wait is currently opaque, which makes a slow-starting dependency indistinguishable from a hung one at the exact moment that distinction matters most.
  - **Wrap text in `fancy` output** rather than letting a long line run off the edge. Note `fancy.rs` already clips to the real display width via `unicode-width`, so the machinery to measure is there — this is about what to *do* at the boundary.
  - **A log-aggregation output mode** (Batect's example was starting a Seq instance and pointing every container's logs at it). Ratect's `EventSink` design makes an extra mode cheap to add; the open question is whether a task runner should be starting a log server on your behalf, or just be easy to point at one you already run.
  - **Cheaper repaints in `fancy` mode.** Batect wanted to batch console updates rather than reprinting on every event. Ratect is already better in one direction — `fancy.rs:59` skips a repaint entirely when the content hasn't changed — and worse in another: it repaints the whole block per event, where Batect diffs and rewrites only the lines that changed (`fancy.rs:26`). Deliberately left as a future item rather than scoped: nobody has reported it and the cost hasn't been measured, so the honest first step is a measurement (a task with many dependencies emitting events rapidly) rather than an optimisation. Worth knowing that the whole-block repaint isn't accidental — it re-clips against the current terminal width for free, which is how resize is handled without tracking it.

  **Terminal capability detection: staying with the heuristic, deliberately.** Batect's roadmap wanted to replace its detection with a terminfo lookup, and Ratect ported the approach it was dissatisfied with — stdout is a terminal, `TERM` is set and isn't `dumb`, and the size is queryable (`ui/mod.rs:289`). Decided to keep it, so this isn't re-opened as an oversight:

  - Terminfo is **Unix-only**. Windows has no such database, so it would be a second detection path beside the heuristic rather than a replacement for it — and Windows is precisely where detection is hardest.
  - It answers a question that has largely stopped being asked. Terminfo distinguishes the terminal that does cursor movement but not colour (`vt100`, `xterm-mono`); those are effectively extinct in developer environments, and the same reasoning already justified not porting Batect's Docker-version fallback chain (`proxy.rs`).
  - **Batect needed it more than Ratect does.** Its `enableComplexOutput` coupled colour and cursor movement into one flag, so a wrong guess broke both at once. Ratect deliberately keeps them as independent axes (which is what makes colourless `fancy` possible at all), so a wrong guess degrades one axis, not the whole display.
  - It costs a new dependency — a terminfo parser or an ncurses binding — for that narrow benefit, against a dependency policy that has so far justified every addition individually.
  - And it doesn't cover what modern terminals actually signal: truecolor is advertised through `COLORTERM`, which terminfo handles poorly.

  **The genuinely useful gap is elsewhere**, and worth doing instead: `NO_COLOR`, `CLICOLOR_FORCE` and `COLORTERM` are honoured *nowhere* in either binary — only the explicit `--no-color` flag exists. `NO_COLOR` in particular is the convention users reach for now, it's what a CI system sets, and honouring it is a few lines against a terminfo integration's ongoing cost.
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
- **Git-include cache management** — ~~shipped ([0.2.0](RELEASES.md#ratect)) as
  `ratect includes list`/`clean`/`refresh`:~~ a manual command to list/evict entries from
  `~/.ratect/incl` on demand, beyond 0.19.0's automatic 30-day sweep — e.g. force
  a re-clone of one repo without waiting on the sweep, or free disk space
  immediately. **`ratect`-only**, same reasoning as "Restrict Nested Git
  Includes" below (see [Future Vision](#future-vision)) — Batect has no
  equivalent CLI surface at all for this (only the automatic sweep), so there's
  no parity obligation pulling it into `ratect-compat`, and ROADMAP's own [Two
  Binaries](#two-binaries-ratect-and-ratect-compat) principle is that
  `ratect-compat` isn't the place for new ideas.

  **Scope, settled before building:**

  - **`refresh` is the valuable one, not `list`.** `ensure_cached`'s
    `clone_if_missing` returns early when the working copy exists, so a
    `(remote, ref)` pair is cloned once and then frozen — permanently. If `ref`
    is a branch, a project silently keeps using whatever that branch pointed at
    the first time, and the 30-day sweep never rescues it, because the sweep
    removes entries that go *unused* and an actively-used include never goes
    stale. Today's only remedy is deleting a hashed directory by hand. Batect is
    identical here (`cloneRepoIfMissing` checks `Files.exists` and nothing else),
    so this is an enhancement rather than a parity gap — consistent with this
    whole bullet being `ratect`-only.
  - **It's a *global* cache, unlike `caches`/`resources`.** `~/.ratect/incl` is
    shared by every project on the machine, so there's no project scoping to
    offer and no `--all-projects` to add: `clean` here necessarily affects other
    projects' includes. That cuts both ways — wider reach than anything else
    Ratect removes, but everything in it is re-cloneable, so the worst case is a
    network fetch rather than lost work. No confirmation prompt for that reason,
    unlike the one deferred for `resources clean`.
  - **The lock is a requirement, not a nicety.** `ensure_cached` takes a
    per-entry lock file around cloning; `clean`/`refresh` have to take the same
    one, or they can delete a directory another `ratect` process is cloning into
    or reading. This is the fiddly part of the work, and the reason the removal
    logic belongs in `git_include.rs` beside the lock rather than in the binary.
  - **Shape**, mirroring `caches`: `includes list` (remote, ref, path, last used,
    size on disk), `includes clean [--older-than <age>]`, `includes refresh
    [<remote>...]`. Named `includes` after the `include:` config field — what a
    user actually types — rather than Batect's "bundles" or the `incl` directory
    name. Core owns listing/removal/refresh (like `cache.rs` does for caches);
    the binary owns presentation.

  **As built** (the decisions below all held; the one thing that changed on
  contact was that `refresh` needed no remote filter to be useful, so it still
  has none):

  - **`clean` with no arguments removes only *stale* entries** — the same 30-day
    threshold the automatic sweep uses — with `--all` for everything and
    `--older-than <age>` for a different threshold. Docker's own `prune` versus
    `prune -a` precedent, and the right default given "everything" here is
    machine-wide rather than this project's. `--all` is really `--older-than 0`,
    kept as its own flag because it's what someone reaches for.
  - **`refresh` does the lot**, with no remote filter to start with. Simpler, and
    the cache is small enough that re-cloning all of it is not the imposition it
    would be for, say, images.
  - **`list` always shows each entry's size**, no flag. Measured rather than
    assumed: a realistic bundle-sized clone (5.7 MB, ~1,000 files) walks in about
    10 ms, and sizing each entry concurrently keeps a whole cache at roughly the
    cost of one. That's what makes `list` an answer to "why is my disk full"
    rather than merely informative.

## Future Vision

Exploring innovative features that go beyond the original Batect, as well as planned improvements from the Batect roadmap.

- ~~**Alternative Configuration Format (TOML)**: Undecided, exploratory. TOML is a more typical configuration format for Rust projects than YAML. If pursued, this would apply only to the [`ratect` binary](#two-binaries-ratect-and-ratect-compat) — `ratect-compat` stays YAML-only for Batect compatibility — and would need a migration path for projects moving from `ratect-compat`'s YAML config.~~ — scoped into `ratect` [0.3.0](RELEASES.md#ratect): the format is **TOML** (native default `ratect.toml`), with the schema redesign (an `extends` field replacing YAML anchors, one object shape per `volumes`/`ports`/`devices`/`include` entry) and mixed TOML/YAML includes. Migration tooling is the `ratect config convert`/`validate` verb, which shipped alongside it — full design at [decisions/0003](decisions/0003-ratect-native-config-format.md).

- ~~**Restrict Nested Git Includes**: **`ratect`-only** — `ratect-compat` must keep Batect's own unrestricted behavior for parity (its `ConfigurationLoader`/`IncludeResolver` have the identical gap: any file, root or reached transitively through a Git include, can declare a further `type: git` include with no restriction on remote). Currently a nested include gets the exact same trust as one the project owner declared themselves — no allowlist, and (post-0.10.0's `container_git_boundaries` fix) a rogue nested include's own containers are at least bounded to its clone directory or the project directory, but the include mechanism itself will still fetch from whatever remote a third-party bundle names. Worth an opt-in gate for `ratect` (e.g. `allow_nested_git_includes`, defaulting `false`) requiring the project owner to consciously accept that a Git-included bundle may itself redirect the process to further remotes. Relatedly worth reconsidering alongside it: whether a nested (non-root-declared) include's clone/checkout failure should keep surfacing git's raw stderr, since the specific transport error (host unreachable vs. connection refused vs. repository-not-found vs. auth-failed) lets repeated attempts fingerprint an internal network — most relevant when `ratect` runs in CI against a bundle whose nested includes a less-trusted contributor can influence, and whose CI logs are visible back to them. Deferred rather than implemented immediately: real projects (including ones outside this one) depend on nested git includes working by default today, and `ratect-compat` has to default this open regardless — squarely a `ratect`-only divergence, not a blocking gap.~~ — shipped in `ratect` [0.4.0](RELEASES.md#ratect) as `allow_nested_git_includes`, per-include-entry and defaulting `false`, with the stderr question resolved the same way: a nested include's clone failure reports that it failed and moves git's own diagnosis behind `RUST_LOG=debug`, while an owner-declared include keeps it in full. `ratect-compat` is unchanged, as this entry required.
- **Trusting a Git include's host paths** (`allow_host_paths`): a per-include opt-in
  letting a bundle the project owner explicitly vouches for resolve host paths outside
  the containment 0.10.0 introduced — needed because a legitimate, common bundle
  pattern (a machine-wide tool cache at `~/.cache/<tool>`) is otherwise blocked with no
  in-config workaround, while working fine under Batect. Explicit per include and never
  recursive, honoured only in files the project owner controls, and boolean now but
  forward-compatible with a later allowlist. One of three complementary tracks, with a
  cross-project **shared cache** (the right answer for `ratect`-native configs, which
  can't help `ratect-compat`) and that allowlist (tightening the boolean for bundles
  that can't migrate). Full rationale, alternatives and the security properties any
  later change must preserve: [decisions/0004](decisions/0004-git-include-host-path-trust.md).
  The shared cache is scoped into `ratect` [0.4.0](RELEASES.md#ratect); the allowlist stays
  deferred there, for the evidence reason the ADR itself gives.
- **Wildcard Includes**: Support for including multiple files using glob patterns (e.g., `include: containers/*.yaml`). Batect wanted this too, and never built it.
- **Configuration Merging/Replacement**: Ability to merge or override containers and tasks when including files.
- **Init Containers**: Support for containers that must start, run, and complete before other containers can start (e.g., for database initialization).
- **External Health Checks**: Support for external health checks (e.g., HTTP) that don't require specialized tools like `curl` to be installed within the container.
- **Image Lifecycle Management**: Tools for building and pushing images independently of task execution, and cleaning up unused images.
- **`ulimit` Support**: Support for setting `ulimit` values for containers.
- **Secrets Management**: Integrated support for securely handling sensitive information like API keys and credentials.
- **Plugin System**: A flexible architecture to allow users to extend Ratect's functionality with custom logic.

The bullets below all come from Batect's own open issues and unbuilt roadmap — ideas
it wanted and never shipped, so they're enhancements rather than parity work, and
each needs its own decision about which binary it belongs to. Recorded here after a
pass over Batect's remaining 7 open issues and its `ROADMAP.md`, so the ideas aren't
lost when the archived repository eventually becomes hard to consult.

- **HTTP Includes** ([batect#1230](https://github.com/batect/batect/issues/1230)): a third `include` type fetching a config file over HTTP, so a bundle can be published and versioned alongside the images it uses, in the same artifact repository — Git includes make versioning and auth awkward for that. Needs the trust question answered first: an HTTP include is a fetch-and-execute of arbitrary configuration, so it inherits everything [decisions/0004](decisions/0004-git-include-host-path-trust.md) works through for Git includes, plus caching and integrity (a Git ref at least names a commit; a URL names nothing). `ratect`-only, most likely, for the same reason as nested-include restriction.
- **Arguments on a prerequisite reference** ([batect#1053](https://github.com/batect/batect/issues/1053)): today `-- ADDITIONAL_ARGS` reaches only the explicitly-invoked task, never its prerequisites, in both tools — so a `build` task that takes arguments can't be reused as a prerequisite with different ones. Batect's proposed spelling (`prerequisites: [run-gradle build]`) overloads the string; a native-format `ratect.toml` can give a prerequisite entry a proper object shape instead, which is a good argument for this being `ratect`-only.
- **Setup commands that run in a different container** ([batect#286](https://github.com/batect/batect/issues/286)): `setup_commands` always run inside the container that declares them; this is the "run a command in container B once container A is healthy, before A's dependents start" case (typically seeding a database from a client image that isn't the database itself).
- **Tasks that run on the host** ([batect#78](https://github.com/batect/batect/issues/78), Batect's oldest open enhancement): a task that executes on the host rather than in a container, so one tool runs *every* task in a workflow and host steps can participate in the dependency graph. The largest philosophical departure on this list — it trades away the reproducibility that is the entire point of a container-based task runner — so it needs a decision about whether Ratect wants to be that tool at all, not just an implementation.
- **Warn when a dependency exits before the task finishes**, with its exit code: today a dependency that dies mid-task is silent, and the task fails later for a confusing reason (a connection refused, a timeout) rather than the real one. Cheap, and squarely in the "precise error reporting" goal above.
- **Dependency relationships between containers and tasks**: letting a container declare that a task must run before it starts (Batect's example: the app container requires the build task), removing the need to repeat that task as a prerequisite on every task that starts the container.
- **Per-container graceful shutdown**: cleanup currently stops containers uniformly; Batect wanted the default to be fast termination with an opt-in graceful shutdown for containers where it matters (a database with data shared between invocations, which an abrupt stop can corrupt).
- **Clone Git includes in parallel**: `config.rs`'s include-resolution loop calls `ensure_cached` one entry at a time, so a project with several Git includes clones them serially on first use. The per-entry lock already exists; the open question Batect noted is what to do about a repository needing interactive authentication, which parallel cloning would interleave unreadably.
- **Tell people where to report a crash**: there is no `panic::set_hook` anywhere in the workspace, so a panic prints a raw Rust backtrace and nothing else — no version, no issue-tracker link, no note that it's a bug rather than the user's mistake. Batect wanted the same ("for fatal exceptions, add information on where to report the error"). Cheap, and only pays off if it lands *before* people hit it, which is an argument for doing it early rather than when the first report arrives.
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
