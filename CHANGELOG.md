# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Fancy mode's cleanup line could erase a task's final output**: `repaint_cleanup` now always starts on a fresh line (a blank separator, matching `simple` mode's own "Cleaning up..." framing) instead of painting directly at the current cursor position — previously, a task command whose last line of output had no trailing newline got "Cleaning up..." appended onto that same terminal row, which the very next repaint's cursor-up-and-clear would then erase, permanently destroying the task's real output. Found during pre-release review of 0.16.0's output-modes work.
- **A `--use-network` validation (or network creation) failure silently ended the event stream right after `TaskStarting`**, with no `TaskFailed`/`TaskFinished` ever posted — violating the documented contract that every task execution ends in one or the other. Network resolution now happens *inside* `run_task_internal`'s own result-tracking block instead of before it, so a failure there posts `TaskFailed` like any other infrastructure failure; cleanup only ever attempts to remove a network it can see was actually created. Also fixes a related gap this exposed: `all` mode's deferred "Running `<task>`..." preamble (normally flushed once the container graph resolves) would never print at all for a failure this early — `InterleavedEventLogger` now flushes it on `TaskFailed` too. Found during pre-release review of 0.16.0's output-modes work.

### Added

- **`all` output mode** (`--output all`), completing the four-style `--output` surface: every line on stdout — Ratect's own milestones and *all* containers' stdout/stderr alike — carries a padded per-container prefix (`name    | `, each container's prefix colored round-robin, excluding white/red which mark task-level/error lines, matching Batect's `InterleavedOutput`), line-buffered and interleaved as it happens. The only mode that shows dependency containers' output, setup-command output (`Setup command N | ...`), and full image-build output (`Image build | ...`) — everything the other modes discard. Its I/O policy follows Batect's `InterleavedContainerIOStreamingOptions` exactly: no container gets a TTY or stdin, and every container gets `TERM=dumb` — the policy is *declared by the logger itself* (a new `EventSink::container_io_streaming`, mirroring Batect's `EventLogger.ioStreamingOptions` design) and consulted by the engine and Docker client, so it can never disagree with the selected style. Container output reaches the logger as new `ContainerOutput`/`SetupCommandOutput` events: the Docker client line-buffers the task container's log stream into events instead of printing (a port of Batect's `InterleavedContainerOutputSink` line splitting, trailing `\r` stripped) and follows each dependency container's logs in a background task for its whole lifetime — the first time dependency output is captured at all. One deliberate simplification against Batect, documented in [Differences from Batect](docs/differences-from-batect.md#cli-flags): its inner `Batect | ` prefix on status lines is dropped (the outer prefix already says whose line it is). See [CLI reference](docs/cli-reference.md#output-styles).

- **Fancy output mode** (`--output fancy`, and now the auto-selected default on an interactive console): a live status block with one line per container in the task's dependency graph (`<bold name>: <current stage>` — ready to pull/build, pulling/building with live streamed progress detail, waiting for named dependencies, starting, waiting to become healthy, running setup commands (n of m), ready), repainted in place via cursor movement as events arrive — no spinner, the animation is purely rewriting lines, exactly like Batect's own `FancyEventLogger`/`StartupProgressDisplay`. The block freezes behind a blank line the moment the task's own container starts (so its raw output streams below, never fighting a repaint — Batect's `keepUpdatingStartupProgress` mechanism), and after it exits a single live `Cleaning up: N containers (...) left to remove...` countdown line tracks teardown before making way for the summary line. Lines are clipped to the terminal's current width (re-measured every repaint, so resizes render correctly; a pseudo-terminal reporting width 0 is treated as unknown rather than clipping everything to `...`). Two deliberate divergences from Batect, both documented in [Differences from Batect](docs/differences-from-batect.md#cli-flags): an explicit `-o fancy` on a non-interactive console fails up front with a clear error (Batect accepts it and crashes mid-repaint), and `-o fancy --no-color` works — colorless fancy, repaint kept, bold/color dropped (Batect rejects the combination outright). Internally this also grew the event model: `TaskGraphResolved` (the resolved container graph, so the display draws every line from the start), `ContainerRemoved`/`RemovingNetwork` (the cleanup countdown), and `TaskFailed` (freezes the display cleanly before the error reaches stderr). Batect's per-changed-line diff repaint is simplified to a whole-block rewrite per frame (emitted as one atomic write, so there's no flicker). Proven end-to-end on a real pseudo-terminal via `portable-pty` (the auto-selection to fancy included), plus unit tests over the repaint escape sequences. See [CLI reference](docs/cli-reference.md#output-styles).

- **`--output`/`-o` and `--no-color`**: Batect's output-style selection lands on the CLI, with two of its four styles implemented so far — `simple` (the plain milestone lines introduced below) and `quiet` (no milestone lines at all: stdout is exactly the containers' own output, safe to pipe, with error reporting staying on stderr; also switches `--list-tasks` to Batect's machine-readable `name<TAB>description` format, sorted by name, no header or grouping — an exact port of its `ListTasksCommand.printMachineReadableFormat`). `fancy` and `all` are accepted flag values but error clearly as not-implemented-yet rather than silently falling back — they arrive later in 0.16.0. When `--output` isn't given, the style is auto-selected by Batect's own rule (`EventLoggerProvider`/`ConsoleInfo.supportsInteractivity`, confirmed by reading its source): fancy on a console that can support it — stdout a real terminal, `TERM` set and not `dumb`, terminal dimensions queryable, no `--no-color` — and simple otherwise (until `fancy` exists, the interactive default is simple too; Batect's mintty and legacy `TRAVIS` special cases are deliberately skipped — Windows is untested here, and modern CI doesn't allocate a TTY). `--no-color` disables Ratect's own colored output (never the task command's), and — one deliberate divergence, a superset: Batect rejects `--output=fancy --no-color` outright because its console couples color and cursor movement; Ratect keeps them independent, so that combination will render colorless fancy once `fancy` lands. See [CLI reference](docs/cli-reference.md#output-styles).

### Changed

- **Task progress is now reported as plain milestone lines on stdout instead of an ephemeral spinner**, as the first step of 0.16.0's output-modes work (see `ROADMAP.md`): task execution now prints Batect-`simple`-style lines — "Running `<task>`...", "Pulling `<image>`..."/"Pulled `<image>`.", "Building/Built `<container>`.", "Starting/Started `<dependency>`.", health/setup-command milestones, "Cleaning up...", and a final "`<task>` finished with exit code `<n>` in `<duration>`." summary (exit code green/red when stdout is a terminal) — replacing the previous `indicatif` spinner during image pulls/builds (the `indicatif` dependency is removed entirely; none of Batect's output modes use a spinner — its `fancy` mode is a cursor-movement line repaint, arriving later in 0.16.0). Internally this lands Batect's `EventLogger` architecture (`ratect-core/src/ui/`, ported from its `TaskEventSink`/`EventLogger` design): `engine.rs` posts typed milestone events and `docker.rs` posts streamed pull/build progress events to an injected `EventSink`, and the selected logger decides what each event renders as — the seam the `--output` mode selection (`fancy`/`simple`/`quiet`/`all`) plugs into next. Also fixes concurrent pulls/builds (possible since 0.15.0's within-task parallelism) racing their uncoordinated spinners onto the same terminal line. The engine's "Running task"/setup-command `tracing::info` diagnostics dropped to `debug` so `RUST_LOG=info` doesn't duplicate the new stdout lines on stderr. See [how it works](docs/how-it-works.md#5-logging-vs-output).

## [0.15.0] - 2026-07-17

### Added

- **Task name suggestions**: a misspelled task name (given directly on the command line, or as a `prerequisites` entry) now gets a `Did you mean 'x'?` suggestion appended to the "task not found" error, for every existing task name within a Levenshtein edit distance of 3 — ported from Batect's own `TaskSuggester`/`EditDistanceCalculator` (confirmed by reading Batect's source). Deliberately not a literal port of Batect's own tie-breaking: its sort comparator only compares by distance, and since that same comparator also decides its backing `TreeMap`'s key uniqueness, two equally-close task names silently collapse to just one suggestion there — Ratect's breaks ties alphabetically instead, so every equally-close match is suggested. See [CLI reference](docs/cli-reference.md#exit-codes-and-error-reporting).
- **Wildcard (`*`) prerequisites**: a `prerequisites` entry containing `*` is now expanded against every task name at run time instead of being looked up literally — e.g. `prerequisites: ["lint:*"]` runs every task whose name matches, in alphabetical order. Ported directly from Batect's own `TaskExecutionOrderResolver` (`resolveWildcards`/`toWildcardRegex`, confirmed by reading Batect's source): `*` matches zero or more characters, case-sensitive, anchored to the whole task name; a wildcard matching zero tasks is not an error; a task named both explicitly and by an overlapping wildcard still only runs once (Ratect's existing per-invocation dedup already gives this for free). This was meant to land alongside 0.14.0's Task Model Completeness but was missed until noticed later. See [config reference](docs/config-reference.md#wildcard-prerequisites).
- **`command` on a container**: overrides the image's own default `CMD`, symmetric with the existing `entrypoint` field — tokenized into literal argv the same way, no expression support. Previously a dependency/sidecar container had no way to set its own command at all (only a task's own container could, via `run.command`), silently running the image's default regardless of what a real-world config might expect — this was meant to land alongside `entrypoint` and the rest of 0.13.0's container runtime options but was missed until noticed later. `run.command` continues to override a task's own container's `command`, matching Batect's `task.runConfiguration.command ?: container.command` precedence exactly (confirmed by reading Batect's source). See [config reference](docs/config-reference.md#container).

### Changed

- **`setup_commands.command` is now tokenized into literal argv instead of running via `sh -c`**, matching Batect's own tokenizer exactly (`SetupCommand.command` is typed `Command` there — the same type as `Container.command`/`entrypoint` — and passed to Docker's exec API as already-parsed argv, confirmed by reading `RunContainerSetupCommandsStepRunner.runSetupCommand`, not assumed). This closes a divergence left open since 0.13.0's `command`/`entrypoint`/`ADDITIONAL_ARGS` tokenizer work, which explicitly called out `setup_commands` as "a narrower, still-open divergence" rather than deliberately preserved. **Breaking change, accepted this early** (same reasoning as 0.13.0's): a `setup_commands` entry relying on shell operators (`&&`, `$VAR` expansion, glob characters) without an explicit `sh -c '...'` wrapper now runs those characters literally instead of interpreting them — write `command: sh -c "..."` explicitly to keep shell behavior, same as `command`/`entrypoint` already require.
- **Within-task container startup is now concurrent**: independent branches of one task's own container dependency graph (image pulls/builds, container starts, health-check waits, setup commands) now run at the same time instead of one after another, gated only by each container's own `dependencies` actually being ready — matching Batect's own `ParallelExecutionManager`/`ContainerDependencyGraph` behavior (confirmed by reading Batect's source before implementing this, not assumed). `ratect-core/src/engine.rs` gained a static, up-front `build_dependency_graph` pass (cycle detection via DFS ancestor path, mirroring Batect's `ContainerDependencyGraph`) ahead of a rewritten `ensure_container_ready` (formerly `start_dependency`), memoized per task execution via `Arc<tokio::sync::OnceCell<...>>` so two concurrent branches sharing a dependency (a diamond) converge on one in-flight start instead of double-starting it. Also fixes a latent race this exposed in image pull/build dedup (`pulled_images`/`built_images`), which used a check-then-act pattern only safe under the old fully-sequential execution — both now use the same memoization, so two containers concurrently resolving the same image share one in-flight pull/build. `prerequisites` deliberately stay strictly sequential — this matches Batect exactly (it doesn't parallelize independent prerequisite tasks either), not a shortfall. See [task lifecycle](docs/task-lifecycle.md#dependency-resolution) and `ROADMAP.md`'s 0.15.0 entry.

## [0.14.0] - 2026-07-17

### Added

- **`description`/`group` on a task**: `description` is shown next to the task's name in `--list-tasks` output; `group` heads tasks sharing the same value under their own listing, with an ungrouped task falling into a trailing "Ungrouped tasks:" bucket — only once *some* task in the project declares a `group` at all, otherwise `--list-tasks` stays the flat list it's always been. See [config reference](docs/config-reference.md#list-tasks-output). Part of 0.14.0's Task Model Completeness (see `ROADMAP.md`).
- **Task-level `dependencies`**: a task can now declare sidecar containers scoped to that task specifically, distinct from a container's own `dependencies` (shipped in 0.6.0), which every task using that container picks up. Unioned with the task's own container's `dependencies` when resolving what to start alongside it, and folded into the same `no_proxy` exemption list. Requires `run`, and can't name `run.container` itself — see [config reference](docs/config-reference.md#task). Part of 0.14.0's Task Model Completeness (see `ROADMAP.md`).
- **`customise` on a task**: per-task `environment`/`ports`/`working_directory` overrides for a *non-main* container used anywhere in the task's own container graph (a task-level or container-level dependency, at any depth), merged the same way the task's own `run` overrides its main container. Can't target the main task container itself (set the equivalent property on `run` instead) or a container outside the task's graph — both rejected at config-load time, matching Batect's own validation. See [config reference](docs/config-reference.md#taskcontainercustomisation). Completes 0.14.0's Task Model Completeness (see `ROADMAP.md`).

### Changed

- **`run` is no longer required on a task**: a task with only `prerequisites` and no `run` is now valid, matching Batect — its prerequisites still execute, then Ratect stops there (there's no container of the task's own left to run). A task must still have at least one of `run`/`prerequisites` — see [config reference](docs/config-reference.md#task). Part of 0.14.0's Task Model Completeness (see `ROADMAP.md`).

## [0.13.0] - 2026-07-17

### Added

- **`working_directory`**: a container (and, for a task's own container, the task-level `run.working_directory` override) can now override the image's own `WORKDIR` — see [config reference](docs/config-reference.md#container). A `setup_commands` entry with no `working_directory` of its own now falls back to the container's, then the image's own default, closing a known gap left by 0.9.0's dependency readiness work (see `ROADMAP.md`). Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`entrypoint`**: a container (and, for a task's own container, the task-level `run.entrypoint` override) can now override the image's own `ENTRYPOINT` — see [config reference](docs/config-reference.md#container). Tokenized into literal argv the same way `command` is, via the tokenizer landed just ahead of this in `Unreleased` — so the classic Batect idiom of `entrypoint: /bin/sh -c` combined with a single-quoted `command: 'some command'` (forcing it to stay one argv token) produces exactly `/bin/sh -c "some command"`, with neither field inserting its own extra shell layer. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`labels`**: a container can now set Docker labels — see [config reference](docs/config-reference.md#container). Container level only, matching Batect (no task-level `run` override in either). Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`capabilities_to_add`/`capabilities_to_drop`**: a container can now add/drop Linux capabilities beyond Docker's own default set (`--cap-add`/`--cap-drop`) — see [config reference](docs/config-reference.md#container). Validated at config-load time against a fixed list based on Batect's own `Capability` enum — extended with `BPF`/`CHECKPOINT_RESTORE`/`PERFMON`, which postdate Batect's last release ([moby#41563](https://github.com/moby/moby/pull/41563)) — rejecting an unknown name with a clear error rather than letting it reach Docker's API. Container level only, matching Batect. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`privileged`**: a container can now run with extended (nearly all host) privileges — Docker's `--privileged` — see [config reference](docs/config-reference.md#container). Defaults to `false`, matching Batect. Container level only. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`shm_size`**: a container's `/dev/shm` size can now be set — Docker's `--shm-size` — see [config reference](docs/config-reference.md#container). Accepts Batect's own size-string format (`"128m"`, etc., ported as `parse_byte_size` in `config.rs`) or a plain YAML integer. Container level only, matching Batect. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`devices`**: a container can now have host devices made available inside it — Docker's `--device` — see [config reference](docs/config-reference.md#container). Both of Batect's forms (`"local:container[:options]"` string and `{local, container, options}` object). `options` (cgroup permissions) defaults to `"rwm"` when omitted, matching the `docker` CLI's own client-side default — Docker's raw API applies none, and an omitted value makes `runc` fail outright (`"device access at 16 field cannot be empty"`), caught by this release's own real-daemon integration test. Container level only, matching Batect. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`enable_init_process`**: a container can now run Docker's own init process as PID 1 ahead of the actual command — Docker's `--init` — see [config reference](docs/config-reference.md#container). Defaults to `false`, matching Batect. Container level only. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`).
- **`image_pull_policy`**: an `image` container can now control whether its image is pulled fresh or reused from the local Docker image cache — see [config reference](docs/config-reference.md#container). **Changes an existing default**: `IfNotPresent` (the new default, matching Batect) now skips the pull entirely when the image already exists locally — new `ContainerRuntime::image_exists_locally` (`docker.rs`), checked via `docker inspect`. Every `image` container that doesn't set this field explicitly now gets `IfNotPresent` instead of Ratect's previous always-pull behavior; set `image_pull_policy: Always` to keep the old behavior. Part of 0.13.0's Container Runtime Options (see `ROADMAP.md`) — completes the section (see `ROADMAP.md`'s Batect Parity list).

### Changed

- **`command` (and `-- ADDITIONAL_ARGS`) is now tokenized into literal argv instead of running via `sh -c`**, matching Batect's own `Command.parse` tokenizer exactly (`docker.rs`'s new `tokenize_command_line`, a straight Rust port): whitespace-splitting, quote-aware (`'...'` fully literal, `"..."` processes backslash escapes), with a backslash escaping the next character outside quotes too. `ADDITIONAL_ARGS` are now appended as further literal argv entries — Batect's real mechanism — rather than becoming `sh -c`'s positional parameters (`$1`/`$2`/`$@`). This closes two real divergences (a literal `$VAR`/glob/shell-operator character in `command` no longer gets silently shell-interpreted; `command` no longer needs a shell present in the image at all — e.g. distroless/`FROM scratch` images), but it's a breaking change for any `command` that relied on the old implicit shell: write `command: sh -c "..."` explicitly to keep shell operators. `setup_commands` is unaffected — it still runs via `sh -c` (a separate, narrower, still-open divergence — see `docs/differences-from-batect.md`). Lands ahead of 0.13.0's `entrypoint` field (see `ROADMAP.md`), which reuses the same tokenizer and would otherwise conflict with the old implicit `sh -c` wrap.

## [0.12.0] - 2026-07-16

### Changed

- **Images now build with the builder the Docker daemon advertises as its default** — BuildKit on any modern daemon — exactly matching Batect's own selection rule (its `DockerConnectivity.kt`, which reads the same `/_ping` `Builder-Version` header). Previously every build used Docker's classic builder unless `build_secrets`/`build_ssh` forced BuildKit — a quiet divergence that could break real Batect projects outright, since the classic builder rejects modern Dockerfile syntax (heredocs, `COPY --link`, `RUN --mount=type=cache`) that projects building under Batect (so, under BuildKit) for years may already use. The `DOCKER_BUILDKIT` environment variable overrides the daemon's default either way (`1`/`true`/`0`/`false`; any other value is a hard error naming it) — the same variable the docker CLI honors and that Batect reads as its `--enable-buildkit` flag's default (the flag itself is later CLI-parity work); a daemon too old to advertise a default builder falls back to the classic builder. Selection happens once per invocation (`select_builder_version`/`ping_info`, cached) and `build_secrets`/`build_ssh` under a forced or daemon-imposed classic builder fail with a clear "requires BuildKit" error rather than silently building without the secret/agent. New `#[ignore]`d integration tests prove each: a heredoc-only Dockerfile builds by default (and *fails* under `DOCKER_BUILDKIT=0`, proving the override genuinely selects the classic builder), the classic path still works when forced (which is also what keeps it exercised in CI now), and the providers-need-BuildKit error. Completes 0.12.0's BuildKit by Default (see `ROADMAP.md`).

- **BuildKit builds now capture build output**, closing 0.11.0's known gap: `build_secrets`/`build_ssh` builds moved off bollard's gRPC-driver path (which exposed no log stream at all) onto the same classic `/build` endpoint every other build uses, with `BuilderVersion::BuilderBuildKit` plus a per-build session serving the secrets/ssh providers — `build_image_via_buildkit` (`ratect-core/src/docker.rs`), rewritten. BuildKit's structured status stream (build steps and their raw output) is accumulated into the same transcript the classic path keeps: each step recorded when it *starts* (execution order, matching the docker CLI — BuildKit announces the whole build graph upfront in graph order, which reads reversed if recorded on first sight), cache hits marked `CACHED`, logged live at `debug` level, and folded into a build failure's error in full — the failing step's own printed output included — instead of 0.11.0's bare failing-instruction-and-exit-code summary. The built image ID is now read from the same response stream too, replacing 0.11.0's post-build `inspect_image(tag)` lookup (and its narrow tag-reuse race window). This requires session-providers support missing from bollard 0.21.0, carried by a fork consumed via `[patch.crates-io]` (`or1can/bollard`, branch `ratect/session-providers-0.21`, pinned by commit in `Cargo.lock`) while the same change is PR'd upstream — see `ROADMAP.md`'s 0.12.0 entry. Also deletes the old path's `spawn_blocking`/`Handle::block_on` workaround (the new path's future is `Send`, so `#[async_trait]` accepts it directly) and the `flate2`/`bytes` dependencies (only the gRPC-driver upload provider needed a gzip-compressed context; the endpoint path posts the same uncompressed tar as classic builds). New `#[ignore]`d integration test `failing_buildkit_build_output_reaches_the_error` (`tests/cli.rs`) proves the transcript-in-error against a real daemon — the test 0.11.0 explicitly couldn't have. Part of 0.12.0's BuildKit by Default (see `ROADMAP.md`).

### Fixed

- **Short-lived interactive tasks no longer warn about TTY resizing**: a task quick enough to exit before the attach-time terminal-size sync landed (e.g. a plain `echo`, run from a real terminal) logged `Failed to resize container TTY` on every otherwise-clean run — in practice deterministically, since the resize API round trip always loses that race. The daemon's 409 ("container is not running") and 404 (already cleaned up — the same race against a mid-session `SIGWINCH` resize) answers are now classified as the benign races they are and logged at `debug`; genuinely unexpected resize failures still warn. Regression-tested with a `portable-pty` integration test (`instantly_exiting_interactive_task_does_not_warn_about_tty_resize`, verified to fail 5-of-5 against the pre-fix code). Present since live resize forwarding shipped in 0.10.0.

## [0.11.0] - 2026-07-16

### Added

- **`dockerfile` and `build_target`**: a container's `build_directory` build can now point at a custom-named or -located Dockerfile (`dockerfile`, a path relative to `build_directory`'s own root, defaulting to `Dockerfile` there) and stop at a given stage of a multi-stage build (`build_target`, Docker's own `--target` mechanism) — see [config reference](docs/config-reference.md#image-building). Both are plain strings with no [expression](docs/config-reference.md#expressions) support, matching Batect's own `String` (not `Expression`) typing for these two fields. `build_context_tar`'s (`ratect-core/src/docker.rs`) force-include logic, which previously hardcoded the literal name `"Dockerfile"` so a broad `.dockerignore` pattern couldn't accidentally exclude it, now force-includes whatever `dockerfile` resolves to instead. Part of 0.11.0's Build Customization (see `ROADMAP.md`).
- **`build_secrets` and `build_ssh`**: a `build_directory` build can now receive secrets via BuildKit's secret-mount mechanism (`build_secrets`, either `{environment: NAME}` — read from `ratect`'s own environment at build time — or `{path: ...}`, which supports [expressions](docs/config-reference.md#expressions) and is resolved/containment-checked the same way as `build_directory`) and forward an SSH agent (`build_ssh`) — see [config reference](docs/config-reference.md#image-building). Both switch that specific build from Docker's classic build API to a BuildKit gRPC session (`bollard`'s `Moby` driver, upgrading the existing daemon's own `/session`+`/grpc` endpoints — no separate persistent builder container needed); every build that uses neither field is completely unaffected. `build_secrets` additionally disables the build cache for that build — BuildKit deliberately excludes a secret's value from its cache key, which would otherwise silently serve a stale secret from a cached layer after only the secret's value changed. **Known divergence from Batect**: `build_ssh` only supports forwarding the host's running `ssh-agent` (via `SSH_AUTH_SOCK`) under BuildKit's implicit `default` agent id — at most one entry, and an entry naming a non-`default` id or explicit key `paths` is rejected with a clear error. Batect supports multiple named agents and forwarding explicit private key files instead of a running agent (confirmed by reading Batect's own `BuildImageStepRunner`/`docker-client` source, not assumed from its docs); the underlying Docker client this is built on (`bollard`) doesn't expose either — see [Differences from Batect](docs/differences-from-batect.md#container-fields). Two further BuildKit-path limitations, both documented in [Differences from Batect](docs/differences-from-batect.md#runtime-behavior-gaps): a BuildKit-session build's output isn't captured (no `RUST_LOG=debug` transcript, and a failure's error names the failing instruction and exit code — verified empirically — but not what that step printed, unlike the classic path's full-transcript errors; `bollard`'s session API exposes no log stream to capture); and Ratect's classic-builder-by-default is itself a quiet divergence from Batect, which defaults to the daemon's ping-advertised builder — BuildKit on any modern daemon (`--enable-buildkit` is a force-override there, not the primary switch). `build_ssh` is proven end-to-end by an `#[ignore]`d integration test that spawns its own throwaway `ssh-agent` with a scratch key (`ScratchSshAgent`, `tests/cli.rs`) rather than assuming the host has one — the fixture's `CACHE_BUST` build arg keeps repeated runs sound, since a `build_ssh`-only build keeps BuildKit's normal layer caching and would otherwise be served a previous run's `ssh-add -l` output. Part of 0.11.0's Build Customization.

## [0.10.0] - 2026-07-15

### Added

- **`TERM` propagation**: the host's `TERM` environment variable is now propagated into the invoked task's own container's environment (lowest-precedence tier, alongside proxy variables in `merged_environment`), matching Batect's own `ConsoleInfo.terminalType`/`terminalTypeForContainer` — both read the host's `TERM` unconditionally, with no TTY check anywhere in them. Gated on the task being interactive-eligible (the top-level task named on the command line) — deliberately *not* on a real Docker TTY actually being allocated, since that's decided later and from information (`std::io::IsTerminal`) not yet available at the point the environment is built. Never applied to dependency/sidecar containers or image builds; a container's own explicit `environment`/`run.environment` still overrides it on collision. Part of 0.10.0's Interactive Mode Completeness (see `ROADMAP.md`).
- **Decoupled stdin forwarding**: piping input into a task no longer requires a real Docker TTY — `open_stdin`/`attach_stdin` on the container's Docker config are now gated on the task being interactive-eligible alone (`interactive`), independent of `should_use_tty`'s stricter both-stdin-and-stdout-real-terminals gate (unchanged, still controls `tty`/`stdin_once`), matching Batect's own unconditional stdin forwarding for the task's own container. `DockerClient::run_container` (`ratect-core/src/docker.rs`) gained a new `run_container_forwarding_stdin` path (attach stdin-only, forward it, stream output via the plain non-TTY `logs` follow API, now shared with the fully-non-interactive case via a new `start_and_stream_logs` helper) for exactly this "interactive but no real TTY" case. New `#[ignore]`d integration test `piped_stdin_reaches_a_non_tty_task_container` (`tests/cli.rs`) proves this against a real Docker daemon, using plain OS pipes on both stdin and stdout (neither end a TTY at all). Part of 0.10.0's Interactive Mode Completeness.
- **Live terminal-resize forwarding**: an interactive session's container TTY now stays in sync with the local terminal for the whole session, not just once at attach time — `run_container_interactively` (`ratect-core/src/docker.rs`) spawns a `SIGWINCH` listener (`tokio::signal::unix`, Unix-only; a plain OS signal, deliberately not crossterm's `event`/`EventStream` API — see the `crossterm` entry in `CLAUDE.md`) that re-runs the same resize call (now extracted into a shared `resize_tty` helper) on every subsequent local terminal resize. Non-Unix hosts keep the previous once-at-attach-only behavior rather than erroring. New `#[ignore]`d integration test `interactive_session_forwards_live_terminal_resizes` (`tests/cli.rs`) proves the full round trip against a real Docker daemon — resizing a `portable-pty` master side and confirming the container's own shell reports the new size via `stty size`. Part of 0.10.0's Interactive Mode Completeness.

### Fixed

- **Security**: the 0.8.0 Git-include containment fix (`GitBoundary::check_contains`, commit `6fcd0b8`) only constrained an `include`'s own `path` field — it didn't stop a *container* declared inside a Git-included bundle from escaping via its `volumes` host paths or `build_directory`. A pinned `type: git` include could declare a container with, e.g., `volumes: ["/:/hostroot"]` or `build_directory: /home/you/.ssh`, mounting or reading an arbitrary host path the moment any task using that container ran — undermining the containment guarantee `docs/config-reference.md` already documented for includes generally. `resolve_path`/`resolve_volume` (`ratect-core/src/config.rs`) now reject an absolute path or `../..` traversal that escapes both the Git repository's own clone directory *and* the project directory, for any container whose origin file was reached through a Git include — the project directory is allowed as a second root (not just the clone) since referencing it via `<{batect.project_directory}` is a legitimate, common thing for a shared bundle to do, not an escape. `container_git_boundaries`, a new `HashMap<String, GitBoundary>` alongside the existing `container_base_paths` on `LoadedConfig`, tracks which containers need the check. New regression tests mirror the existing include-escape tests, for both the rejection and the `batect.project_directory` carve-out.
- **Security**: `run_as_current_user.home_directory` was only validated to be an absolute path — a value containing `:` or a newline reached `user::generate_passwd_file`/`generate_shadow_file` raw, where it's interpolated into a colon-delimited `/etc/passwd`/`/etc/shadow` line uploaded into the container. A `:` shifted that line's fields; a newline injected an entirely separate, attacker-chosen entry (e.g. a second `uid=0` account). `resolve_expressions_with` (`ratect-core/src/config.rs`) now also rejects any `home_directory` containing a `:` or a control character, alongside the existing absolute-path check.
- **Security**: `git_include::cache_key` joined `remote`/`git_ref` with a bare `" @"` separator before hashing (`format!("git {remote} @{git_ref}")`), so two differently-split pairs sharing that separator — e.g. `(remote="repo.git @evil-ref", ref="main")` and `(remote="repo.git", ref="evil-ref @main")` — hashed identically. Since `~/.ratect/incl` is a single cache shared across every project on the machine and is never re-fetched once populated, a collision could make one project's `type: git` include silently reuse an unrelated project's cached clone. Each field is now length-prefixed before hashing, making the two unambiguously separable regardless of content. This changes every existing cache key, so upgrading causes a one-time re-clone of each cached `(repo, ref)` on next use — expected, not a bug.
- **Security**: a Git-included bundle's own `.gitmodules` could point a submodule at an arbitrary local path via a `file://` URL, and `SystemGitClient`'s `git checkout --recurse-submodules` allowed the `file` transport — letting a malicious bundle silently pull a sibling local repository on the host running `ratect` into its own clone (chainable with volume-mount escapes to exfiltrate it). `file` is now excluded from `GIT_ALLOW_PROTOCOL` for the checkout/submodule-fetch step specifically, since a submodule URL is always third-party content from the fetched ref, unlike the top-level `repo` field (which keeps `file` allowed, since a local-path `repo` is itself a documented, supported feature — the caller's own config value, not third-party content). Git doesn't fail the overall checkout when a submodule's transport is disallowed; it silently leaves that submodule's directory uninitialized instead, which the new regression test asserts directly.
- **Security**: `ensure_host_volume_directories_exist` (`ratect-core/src/docker.rs`) checked `path.exists()` before calling `fs::create_dir_all` — a redundant TOCTOU-prone check, since `create_dir_all` is already a no-op success when the directory already exists. It also meant a pre-existing *non-directory* at a bind-mount host path was silently left alone here, deferring to a more confusing failure later when Docker tries to bind-mount it. The check is now gone; `create_dir_all` runs unconditionally and reports that case directly.

## [0.9.0] - 2026-07-15

### Added

- **Dependency readiness** (`health_check` and `setup_commands` on containers):
  a started dependency is no longer treated as ready immediately — matching Batect,
  it must first report healthy and then complete its setup commands before anything
  that depends on it (another dependency, or the task's own container) starts.
  - `health_check` (`command`, `interval`, `retries`, `start_period`, `timeout`)
    overrides the health check configuration baked into the container's image, at
    container creation. `command` runs via the container's default shell (Docker's
    `CMD-SHELL` form); durations are Batect's Go-style strings (`2s`, `500ms`,
    `1m30s`); an omitted field inherits the image's own value.
  - After a dependency starts, Ratect waits for Docker's own health verdict (via the
    event stream, replayed from the beginning of time so an early verdict can't be
    missed). A container with no health check at all — from neither its image nor
    config — is immediately considered healthy, so configs without health checks
    behave exactly as before. A dependency reported unhealthy fails the task, with
    the last health-check run's exit code and output in the error; so does a
    dependency that exits before any verdict. There is no Ratect-side timeout
    (matching Batect) — Docker's own `interval`/`retries` bound the wait.
  - `setup_commands` (each `command`, with optional `working_directory`) then run
    inside the running dependency via Docker's `exec` mechanism, one at a time in
    declared order, with the container's own `environment` and (under
    `run_as_current_user`) the container's own `uid:gid`, each via `sh -c`. A
    command exiting non-zero fails the task, with its output in the error.
  - A dependency that starts but never becomes ready is still cleaned up (stopped
    and removed, its task network deleted) like any other failure.
  - The task's *own* container's `health_check` is applied (Docker records and runs
    it) but never gates the task's outcome, and its `setup_commands` don't run —
    see the new [Dependency readiness](docs/config-reference.md#dependency-readiness)
    section and [Differences from Batect](docs/differences-from-batect.md#container-fields)
    for these two deliberate divergences.

## [0.8.0] - 2026-07-14

### Added

- **Git includes**: a config file's top-level `include` list can now use a `type: git`
  entry (`repo`, `ref`, and an optional `path`, defaulting to `batect-bundle.yml`) to
  import shared tasks/containers from a separate Git repository — a "bundle" — matching
  Batect's own Git include semantics.
  - Shells out to the system `git` binary (`clone --quiet --no-checkout` then
    `checkout --recurse-submodules <ref>`, then an atomic rename into place) rather than
    embedding a Git library.
  - A `(repo, ref)` pair is cloned once and cached forever at `~/.ratect/incl/<hash>` —
    never re-fetched, so `ref` must be pinned to something immutable (a tag or commit
    SHA, not a branch). A per-cache-entry lock file (create-exclusive, polled, with a
    5-minute timeout) makes concurrent `ratect` invocations targeting the same repo/ref
    safe. Each cached clone gets a `<hash>.toml` sidecar (`type`, `repo.remote`,
    `repo.ref`, `cloned_with_version`, `last_used`), written via
    write-to-temp-then-atomic-rename.
  - A Git-included file's own relative paths (volume host paths, `build_directory`, and
    any further `include` entries) resolve against the cloned repository's root, the
    same `container_base_paths` mechanism 0.7.0's local file includes already use.
  - `repo`/`ref` are config-file-supplied (possibly transitively, from a git-included
    bundle) and treated as untrusted: a leading `-` is rejected outright (argv flag
    smuggling), and `GIT_ALLOW_PROTOCOL` is restricted to `file:git:http:https:ssh` for
    both the clone and the checkout (which can itself trigger submodule clones) —
    without this, a `repo`/submodule URL of the form `ext::sh -c ...` can execute
    arbitrary shell commands, since git otherwise resolves it at "user" trust level for
    a directly-invoked `clone`.
  - **Containment**: a Git include's `path`, and every `include` entry declared
    (transitively) by the file it names, must resolve within that repository's own
    clone directory — an absolute path, a `../..` traversal, or a symlink pointing back
    out are all rejected, rather than silently reading an arbitrary file elsewhere on
    the host (verified end-to-end: a crafted bundle could otherwise merge in another
    project's tasks/`environment` values, e.g. secrets, from outside its own repo). A
    nested `type: git` include still works — it establishes its own fresh boundary
    rather than inheriting (or being rejected by) its parent's.
  - Known gaps, deferred as follow-on work rather than blocking this release: no 30-day
    cache eviction sweep and no manual cache-clear CLI subcommand — `~/.ratect/incl`
    grows unbounded until removed by hand. See [config
    reference](docs/config-reference.md#git-includes) and [Differences from
    Batect](docs/differences-from-batect.md#top-level-fields).

## [0.7.0] - 2026-07-14

### Added

- **Local file `include`s**: a config file's top-level `include` list splits one
  project's configuration across multiple files, matching Batect's local file include
  semantics (Git bundle includes remain unsupported — see
  [ROADMAP.md](ROADMAP.md#ratect-compat)).
  - Each entry is a bare string path, or the expanded `{path, type: file}` object form;
    any other `type` (e.g. `git`) is rejected with a clear "not supported yet" error.
  - An included file's path resolves relative to the directory of the file that
    declares it, traversed recursively (an included file can itself `include` more
    files) and de-duplicated by resolved path, so an include cycle or a file included
    from two places is harmless rather than an error or an infinite loop.
  - Every loaded file's `containers`, `tasks`, and `config_variables` merge into one
    flat set; a name defined in more than one file is a hard error naming both files.
    Only the root file may declare `project_name`.
  - A container's relative paths (`volumes` host paths, `build_directory`) resolve
    against *its own* origin file's directory, not the root project directory —
    `<batect.project_directory` still always resolves to the root's directory
    regardless of which file a container came from. See
    [config reference](docs/config-reference.md#includes).

## [0.6.0] - 2026-07-14

### Added

- **`--use-network`**: reuses an existing Docker network for every task in an
  invocation instead of creating (and removing) a fresh one per task, matching
  Batect's flag of the same name.
  - New `ContainerRuntime::network_exists` (`ratect-core/src/docker.rs`), backed by
    `bollard`'s `inspect_network`, validates the named network up front with a clear
    error (`"The network '{name}' does not exist."`) rather than failing later with an
    unrelated Docker API error when trying to join it.
  - `TaskEngine::with_existing_network` (`ratect-core/src/engine.rs`) opts a task
    engine into reusing a network; when set, `run_task_internal` skips both
    `create_network` and `remove_network` for that network — Ratect didn't create it,
    so cleanup never removes it either, matching Batect (which only ever tears down
    networks it created itself).
- **`additional_hostnames` and `additional_hosts`**: two new per-container fields —
  `additional_hostnames` adds extra network aliases beyond a container's own name;
  `additional_hosts` adds extra `/etc/hosts` entries (Docker's own `--add-host`
  mechanism). Neither takes [expressions](docs/config-reference.md#expressions),
  matching Batect (which types both as plain strings, not `Expression`, itself).
  - New `NetworkOptions` (`ratect-core/src/docker.rs`) bundles both, passed as one
    trailing parameter to `ContainerRuntime::run_container`/
    `start_background_container` rather than two more flat ones — both methods were
    already at `#[allow(clippy::too_many_arguments)]`.
  - Also fixes a related gap found while implementing this: every container's Docker
    `hostname` is now always set to its own container name (matching Batect), not
    left as Docker's default random short container ID — previously a container was
    reachable *by* its name on the network, but `hostname`/`$HOSTNAME` *inside* it
    resolved to something unrelated.
- **`ports`, `run.ports`, and `--disable-ports`**: publishes container ports to the
  host, Docker's own `-p`/`--publish` mechanism.
  - New `ports: Option<Vec<PortMapping>>` on `Container`, accepting both of Batect's
    forms: a `"local:container[/protocol]"` string (protocol defaults to `tcp`,
    including port ranges, `"from-to:from-to[/protocol]"`) or the expanded
    `{local, container, protocol}` object form. New `config.rs::PortRange`/
    `PortMapping` types with hand-written `Deserialize` impls (accepting either form)
    validate `local`/`container` cover the same number of ports at config-load time —
    unlike `volumes`, which is never format-checked.
  - New `TaskRun.ports`: *additional* port mappings for a specific task's run, added
    to the container's own `ports` as a union (not an override — matching Batect,
    which combines these as a `Set`), via the new `engine.rs::merged_ports`.
  - `--disable-ports` suppresses publishing of every container's `ports` (both
    `Container.ports` and any `TaskRun.ports`) regardless of config, matching Batect's
    flag of the same name; `NetworkOptions` (added for `additional_hostnames`/
    `additional_hosts` above) gained a `ports` field — already-expanded
    `(local_port, container_port, protocol)` triples, via `PortMapping::expand` — so
    this stays one bundled parameter rather than a fourth flat one, and `docker.rs`
    itself never needs to parse or validate a `ports` entry.
- **Proxy environment variable propagation** (`--no-proxy-vars` to disable): detects
  `http_proxy`/`https_proxy`/`ftp_proxy`/`no_proxy` (either case) from the host
  environment and injects them into every container's environment and every image
  build's `build_args`, matching Batect's automatic behavior.
  - New `ratect-core/src/proxy.rs` module ports `ProxyEnvironmentVariablesProvider`/
    `ProxyEnvironmentVariablePreprocessor` in spirit: case-insensitive host lookup,
    `localhost`/`127.0.0.1`/`::1` URLs rewritten to `host.docker.internal` (macOS/
    Windows only — no automatic equivalent on Linux, and no Docker-version-gated
    hostname fallback chain the way Batect has, both accepted gaps), and every other
    container name sharing a task's network auto-appended to `no_proxy`/`NO_PROXY`.
  - Injected as the lowest-precedence layer — a container's own `environment`/
    `run.environment`, or explicit `build_args`, always override a proxy-derived value
    on a key collision.
  - New `url` dependency (`ratect-core`) for the `localhost`-rewriting URL parsing —
    already resolved transitively via `bollard`'s own dependency tree.

## [0.5.0] - 2026-07-13

### Added

- **User mapping** (`run_as_current_user`): a container can now run as the host's own
  user/group instead of the image's default (often root), so files a task writes to a
  bind-mounted volume come back owned by you, not root.
  - New `run_as_current_user: { enabled: bool, home_directory: string }` field on
    `Container` (`ratect-core/src/config.rs`), mirroring Batect's own shape exactly.
    `home_directory` is required whenever `enabled` is `true` (and rejected if given
    without it) — Ratect never guesses one. Interpolated through the existing
    expression machinery, but — unlike `build_directory` or volume host paths — *not*
    resolved against `base_path`: it's a path inside the container, validated to start
    with `/` instead.
  - This isn't just `--user uid:gid`: an arbitrary host uid/gid has no entry in the
    image's own `/etc/passwd`/`/etc/group`, which many programs need to function at
    all (no `$HOME`, no username resolution). New `ratect-core/src/user.rs` looks up
    the real host user (`nix`'s `Uid`/`Gid`/`User`/`Group`, new dependency, Unix-only)
    and generates minimal synthetic `/etc/passwd`/`/etc/shadow`/`/etc/group` content —
    ported from Batect's own `RunAsCurrentUserConfigurationProvider`, including its
    `uid == 0`/`gid == 0` special-casing so running as the current user doesn't
    produce a duplicate, conflicting `root` entry. New `docker.rs` functions
    (`build_user_mapping_tar`, `build_home_directory_tar`, both pure and
    unit-tested) build the tars uploaded into the container — via `bollard`'s
    `upload_to_container` — after it's created but before it starts.
  - Host-side bind-mount directories that don't exist yet are created *before* the
    container is even created (`ensure_host_volume_directories_exist`), as the
    current host user — otherwise Docker's daemon (running as root) would
    auto-create them as `root:root` on first use, defeating the point for the common
    "mount my code directory, get build artifacts back with sane ownership" case.
  - `ContainerRuntime::run_container`/`start_background_container` both gained a
    `user_mapping: Option<&UserMapping>` parameter; applies per-container (not
    per-task) — a task's own container and each of its dependencies can set
    `run_as_current_user` independently, matching Batect.
    `TaskEngine::resolve_user_mapping` (`ratect-core/src/engine.rs`) is the shared
    entry point, called from both `run_task_internal` and `start_dependency`.
  - New `#[ignore]`d Docker-backed test
    (`run_as_current_user_maps_the_container_onto_the_host_user`, `tests/cli.rs`) —
    writes its own temporary config at test time (rather than a static fixture,
    since it needs a *missing* host directory to exist beforehand to exercise
    pre-creation) and proves the container actually runs as the host's real uid/gid
    (compared against the test process's own `id -u`/`id -g`), and that a file it
    writes to the mounted volume comes back host-user-owned on disk, not root — the
    actual practical point of the feature, not just that the right calls were made.

## [0.4.0] - 2026-07-11

### Added

- **Interactive mode**: a task's own container now gets a real Docker TTY and its
  stdin forwarded when it's actually being run interactively (e.g. `command: sh` drops
  you into a working shell), instead of always running non-interactively with no
  stdin.
  - Fully automatic, matching Batect: no new config field, no new CLI flag. Applies
    whenever the invoked task's own container is running and Ratect's own stdin *and*
    stdout are both real terminals — falls back to today's `docker logs --follow`
    streaming otherwise (piped output, CI, redirected non-terminals). Never applies to
    a prerequisite's container, a dependency's, or a sidecar's — even though
    prerequisites are themselves full recursive task runs here, only the task actually
    named on the command line is eligible, via a new `top_level: bool` threaded
    through `TaskEngine::run_task`/the new private `run_task_scoped`
    (`ratect-core/src/engine.rs`).
  - `ContainerRuntime::run_container` (`ratect-core/src/docker.rs`) gained an
    `interactive: bool` parameter (eligibility, decided by the engine) and a new
    `should_use_tty` helper (its own unit tests) that further gates it on real
    `IsTerminal` checks. When true: the container is created with
    `tty`/`open_stdin`/`attach_stdin`/`stdin_once` set, attached to via `bollard`'s
    `attach_container` (before starting it, so no early output is lost) instead of
    `docker logs`, the local terminal is put into raw mode for the session (restored
    via a `Drop` guard, even on an error return), and stdin/stdout are pumped
    concurrently between the local terminal and the container until the session ends.
    The container's TTY size is synced to the local terminal's once, at attach time —
    not tracked live if the terminal is resized mid-session (known gap).
  - New `crossterm` dependency (`ratect-core`) for raw-mode enable/disable and
    terminal size; `std::io::IsTerminal` (stable stdlib) covers the "is this actually
    a terminal" checks, no crate needed for that part.
  - **Fixed a real hang found along the way**: `main` previously returned `ExitCode`
    from `#[tokio::main]`, which drops (and blocking-shuts-down) the Tokio runtime
    before the process actually exits — including waiting for the interactive
    session's abandoned `tokio::io::stdin()`-backed blocking read task, which never
    completes on its own (a real terminal's stdin has no natural EOF). Every
    interactive session would have hung the whole process afterward. `main` now calls
    `std::process::exit` explicitly once its own cleanup (raw-mode restoration,
    container/network teardown) has already run via ordinary `Drop`/`?`-propagation,
    bypassing that wait entirely.
  - New `portable-pty` dev-dependency and `#[ignore]`d `tests/cli.rs` test
    (`interactive_session_forwards_stdin_and_stdout`, `tests/fixtures/interactive.yml`)
    spawning `ratect` attached to a real (emulated) pseudo-terminal, scripting input,
    and asserting it round-trips through stdin → container → stdout and the process
    exits cleanly — proves the actual attach/raw-mode/pump path end-to-end (this is
    what caught the hang above), not just that the eligibility policy computes the
    right bool. Works in headless CI; no real terminal required.

## [0.3.0] - 2026-07-10

### Added

- Image building: a container with `build_directory` set now actually builds an image from a `Dockerfile` (always that name, at `build_directory`'s own root) via `bollard`'s classic (non-BuildKit) build API, instead of logging a warning and no-op'ing. New `ContainerRuntime::build_image` and free function `build_context_tar` (`ratect-core/src/docker.rs`) build an in-memory tar of the build directory, respecting a `.dockerignore` if present. Dependency containers now support `build_directory` too (previously only a task's own container could use it) — `TaskEngine::run_task_internal` and `start_dependency` (`ratect-core/src/engine.rs`) both now go through a single shared `TaskEngine::resolve_image`, which pulls or builds as needed and dedupes both (a container is only ever pulled/built once per `ratect` invocation, keyed by image name or container name respectively, via new `built_images: Mutex<HashMap<String, String>>`). Built images are tagged `<project_name>-<container_name>`, matching Batect's own convention, so they're identifiable in `docker images` instead of showing up as an opaque generated name. That tag isn't unique, though (retagged on every run) — `ContainerRuntime::build_image` now returns the image *ID* Docker's build reports back, and `resolve_image` runs/caches that ID rather than the tag, so two overlapping `ratect` invocations retagging the same name can't race each other into running the wrong image.
- `build_args` field on `Container` (`ratect-core/src/config.rs`), passed to the build as Docker's own `--build-arg` mechanism. Values support the same expression syntax as `environment` (interpolated in `resolve_expressions_with`, alongside a new `build_directory` resolution that reuses the same interpolate-then-resolve-to-absolute logic as volume host paths, now factored into a shared `resolve_path` helper).
- `.dockerignore` support: a new workspace crate, `dockerignore/`, is a from-scratch Rust port of Docker's own `.dockerignore` matching (`github.com/moby/patternmatcher`, which Docker's documentation cites as the reference implementation) — deliberately not a `.gitignore`-compatible matcher, since Docker's actual rules differ in ways confirmed against upstream's own source and test suite: most notably, a bare pattern with no wildcard (e.g. `node_modules`) only excludes it at the build context root, not at every depth, unlike `.gitignore`. No existing Rust crate implements this faithfully (two candidates checked: one's matcher is an unfinished, uncompiled stub; the other is an unmaintained 0.0.1 "primitive" from an unfamiliar publisher). Kept as its own crate (zero dependency on any ratect-specific type) rather than a `ratect-core` module, so it could be extracted and published independently later without that being decided now. Ported and verified against upstream's own ~70-case test table (`patternmatcher_test.go`'s `TestMatches`) plus its `ignorefile.ReadAll` parsing tests, both carried over as this crate's own tests. `Dockerfile` and `.dockerignore` themselves are always included in the build context regardless of exclusion patterns, matching Docker's own special-casing. `moby/patternmatcher` is Apache-2.0 licensed (same as Ratect) — new root `NOTICE` file and attribution doc comments in `dockerignore/src/lib.rs`/`pattern.rs` carry forward its own copyright/attribution notice.
- New unit tests across `dockerignore/src/pattern.rs` (the ported upstream test table, negation, root-only-for-bare-patterns behavior), `ratect-core/src/config.rs` (`build_directory`/`build_args` resolution and interpolation), `ratect-core/src/docker.rs` (`build_context_tar` — its first unit tests, since everything else there was previously only covered indirectly), and `ratect-core/src/engine.rs` (build-then-run, build dedup across tasks, `build_args` reaching the build, dependency containers with `build_directory`, the `<project_name>-<container_name>` tag format). New Docker-backed end-to-end test (`tests/fixtures/build.yml`, `tests/fixtures/build/Dockerfile`) proves `build_directory` and `build_args` reach a real `docker build`, not just that the right calls were made.
- Image build output is no longer silently lost: previously each streamed build log line only updated an ephemeral `indicatif` spinner message (never rendered on a non-TTY, e.g. CI) and a failure surfaced only Docker's own one-line `error_detail.message` — not the `RUN` step output that actually explains the failure. `DockerClient::build_image` (`ratect-core/src/docker.rs`) now logs every build log line at `debug` level as it streams (`RUST_LOG=info,ratect_core=debug` for a live transcript without unrelated `bollard` noise — see [filtering `RUST_LOG`](docs/how-it-works.md#filtering-rust_log)), and on failure folds the *entire* accumulated transcript into the returned error via a new `build_output_suffix` helper (its own unit tests), so a failing build is diagnosable without any extra flags. Ratect has no `--output` mode to stream build progress to instead, so this is deliberately the "for now" answer via the logging/error-reporting Ratect already has, not a new UI concept. New `#[ignore]`d Docker-backed test (`tests/fixtures/build-failure.yml`, `tests/fixtures/build-failure/Dockerfile`) proves a real failing build's transcript reaches Ratect's own error output, not just that `build_output_suffix` formats a string correctly in isolation.

### Fixed

- A task whose container has no `dependencies` was left running on Docker's shared default bridge network instead of an isolated one, since `TaskEngine::run_task_internal` (`ratect-core/src/engine.rs`) only created a per-task network when `dependencies` was non-empty — meaning such a task's container was reachable from, and could reach, anything else on that bridge (other unrelated containers on the host, other concurrent `ratect` runs' non-dependency containers), contrary to the isolation `docs/task-lifecycle.md` otherwise describes and to Batect's own behavior of always scoping a network per task. Every task execution now creates (and tears down) its own network unconditionally; dependency containers still only start if `dependencies` is set. `ContainerRuntime::run_container`'s `network` parameter changed from `Option<&str>` to `&str`, since a network is now always present by the time it's called.
- `resolve_path` (`ratect-core/src/config.rs`, used for `build_directory` and volume host paths) and the built-in `batect.project_directory` config variable left stray `.`/trailing-slash artifacts in resolved paths, since joining paths with `PathBuf::join` doesn't lexically normalize the result. Most visibly, running `ratect` with a bare `-f batect.yml` (no directory prefix — the common case) made `batect.project_directory` resolve to the project directory *with a trailing slash* (`/project/` instead of `/project`), since `base_path` becomes `""` in that case and `cwd.join("")` preserves it. Both now run the joined path through `path-clean`'s `.clean()` before returning it. Purely cosmetic (the paths still resolved correctly on disk either way), but user-visible in interpolated `environment`/`build_args` values and error messages. The `base_path` computation itself (`src/main.rs`) is now a small named `base_path_for` function with its own unit tests, covering the bare-filename (`""`), `./`-relative, subdirectory, and absolute cases — previously untested inline logic.

## [0.2.0] - 2026-07-09

### Added

- `environment` field on both containers and task `run`s (`ratect-core/src/config.rs`), merged when a task's own container runs (the container's values apply first, `run.environment` overrides them on a key collision) and passed through to Docker as real container environment variables. A dependency/sidecar container only ever gets its own container-level `environment`, since it has no task `run` of its own. `ContainerRuntime::run_container`/`start_background_container` gained an `environment` parameter, mapped to bollard's `ContainerCreateBody.env` via a new `build_env` helper in `ratect-core/src/docker.rs`.
- Batect expression syntax (`$VAR`, `${VAR}`, `${VAR:-default}` for host environment variables; `<name`, `<{name}` for config variables) — new `ratect-core/src/expressions.rs` module, with host-env and config-variable lookups injected as parameters rather than reading the real process environment, so resolution is deterministic and testable. An unset host variable with no `:-default` fallback, an undeclared config variable, or a declared config variable with no value from any source, are all hard errors naming the variable. Resolved within `environment` values (both containers and task `run`s) and, separately, within a volume's `host_path` — see next entry.
- Volume `host_path` interpolation: a container's `volumes` entries now run through the same expression syntax, before the existing relative-to-absolute path resolution rather than after — an expression resolving to an absolute path (e.g. a `<project_root` config variable) is used as-is rather than wrongly treated as a literal relative fragment of the config file's directory. This required moving path resolution out of `Config::load_from_file` (which runs before CLI-supplied config variable overrides are known) into the new combined `Config::resolve_expressions`/`resolve_expressions_with`, called explicitly from `main.rs` after `load_from_file`; the old separate `resolve_environment`/`resolve_paths` methods are gone, folded into this one pass (plus a new standalone `resolve_volume` helper in `ratect-core/src/config.rs`).
- `config_variables` top-level field (`ratect-core/src/config.rs`), declaring which names are resolvable via `<name`/`<{name}` and their optional `default:`. `Config::resolve_expressions` merges CLI-supplied overrides over each declared variable's `default` and runs every expression-bearing value (as above) through the expressions module.
- `--config-var NAME=VALUE` (repeatable) and `--config-vars-file PATH` CLI flags to supply config variable values, highest-precedence first: `--config-var` over `--config-vars-file` over a variable's own `default`. New `Config::load_config_vars_file` (a flat YAML map, parsed via the existing `noyalib` dependency) lives in `ratect-core`, not `main.rs`, keeping the CLI crate a thin parsing/orchestration layer per its documented architecture split.
- New `docs/config-reference.md#expressions`/`#configvariable` sections (plus an updated Volume path resolution section) and `docs/cli-reference.md` entries for the above; `docs/differences-from-batect.md` and `ROADMAP.md` updated to reflect `environment`, volume host paths, `config_variables`, and the two CLI flags as supported — `build_directory`/`build_args`/etc. remain literal-only, moot until image building itself exists.
- `batect.project_directory`, Batect's one built-in config variable (the absolute path of the directory containing the config file), resolvable via `<batect.project_directory`/`<{batect.project_directory}` without being declared under `config_variables` — in fact declaring it there, or supplying it via `--config-var`/`--config-vars-file`, is now a hard error, since it isn't meant to be overridable. Required allowing `.` in config-variable identifiers (but not host environment variable ones, which never contain dots) in `ratect-core/src/expressions.rs`'s identifier parsing, now parameterized per-sigil.
- New unit tests across `ratect-core/src/config.rs` (parsing, `resolve_expressions` merge/precedence/error cases including volume paths and the built-in variable's guard rails, `resolve_volume`'s relative-vs-absolute-after-interpolation behavior, `load_config_vars_file`), `ratect-core/src/expressions.rs` (token parsing, defaults, literal passthrough, error messages, dotted identifiers), `ratect-core/src/engine.rs` (environment reaching a task's own container vs. a dependency container, run-level override), and `src/main.rs` (the two new CLI flags). New Docker-backed end-to-end tests (`tests/fixtures/environment.yml`, `tests/fixtures/config-vars.yml`, `tests/fixtures/project-directory.yml`) prove `environment`/volume-path values — including both CLI flags' precedence and `batect.project_directory` in both bare and braced form — reach a real container's real environment, not just that the right calls were made; two new fast (non-`#[ignore]`d) `tests/cli.rs` tests cover `batect.project_directory`'s two guard-rail errors, which fail during config resolution before any Docker interaction.

### Fixed

- `unique_temp_dir()` (a test helper in `ratect-core/src/config.rs`) named scratch directories from just the process ID and a nanosecond timestamp, which could collide between tests running in parallel on platforms with coarser clock resolution, occasionally causing one test's scratch file to race with another's. Added a monotonic counter alongside them; confirmed clean across 20 repeated full-suite runs after the fix, versus an intermittent failure before it.

### Changed

- Both workspace crates (`ratect`, `ratect-core`) now sit at `0.2.0-dev`, the first commit of the 0.2.0 development cycle now that 0.1.0 is tagged. The `X.Y.Z-dev` ↔ `X.Y.Z` version bump convention itself is now documented in `ROADMAP.md`'s Versioning & Releases section and `AGENTS.md`, rather than only existing as an inferable pattern in the 0.1.0 release commit.

## [0.1.0] - 2026-07-09

### Added

- `ROADMAP.md` file outlining the path to Batect parity and future enhancements.
- Guideline in `AGENTS.md` for maintaining the changelog.
- `AGENTS.md` file providing context and instructions for AI agents working on the project.
- Initial Rust implementation of Batect core functionality.
- Support for `batect.yml` configuration parsing.
- Task execution engine with support for prerequisites and dependency cycle detection.
- Docker integration using the `bollard` library.
- Container execution with real-time log streaming.
- Automated image pulling with progress indicators.
- Support for volume mounting, including relative path resolution.
- Command-line interface with task listing (`--list-tasks`) and execution.
- Project documentation and Apache 2.0 license.
- GitHub Actions CI workflow running `cargo fmt --check`, `cargo clippy`, `cargo build`/`cargo test`, and `cargo audit` on every push and pull request.
- Unit tests for config parsing and volume path resolution (`src/config.rs`), task engine dependency-cycle detection, prerequisite dedup, and error handling via a fake `ContainerRuntime` (`src/engine.rs`), and CLI argument parsing (`src/main.rs`).
- `tests/cli.rs` integration tests covering `--list-tasks`, missing-config and no-task-name behavior, plus a Docker-backed end-to-end test (`#[ignore]`d by default, runnable via `cargo test -- --ignored`) that exercises the full sample `batect.yml` against a real daemon; wired into CI as its own `docker-integration` job.
- `ContainerRuntime` trait in `src/docker.rs` (via `async-trait`), implemented by `DockerClient`, so `TaskEngine` can be tested against a fake instead of a live Docker daemon.
- Coverage tooling via `cargo-llvm-cov`; CI generates an HTML report and uploads it as a `coverage-report` artifact for spotting untested code, without gating on a percentage.
- `docs/` directory with self-contained user documentation: installation, getting started, how it works (architecture), CLI reference, configuration reference, and a differences-from-Batect page — linked from `README.md`. Documents current gaps found in the process: `-- ADDITIONAL_ARGS` are parsed but not forwarded to the running command, `build_directory`/container `dependencies` are parsed but unimplemented, a container with neither `image` nor `build_directory` is a silent no-op, a missing config file doesn't fail the process, and container exit codes aren't currently checked.
- Itemized field-by-field and flag-by-flag comparison tables in `docs/differences-from-batect.md`, verified directly against Batect's own reference documentation (its config `overview`/`containers`/`tasks` and `cli` pages) rather than assumption — this is the detail behind the roadmap's "Full Configuration Parity" and "Full CLI Options Parity" items, and it also surfaced that Ratect silently ignores unsupported config keys instead of rejecting them.
- Sidecar/dependency container support (`Container.dependencies`, previously parsed but unused): dependencies are started (recursively, for nested dependencies) before a task's own container, reachable by name over a Docker network created and torn down for that single task execution. Deduped within one task's dependency resolution; not shared across tasks — each task execution gets its own instance and network, matching Batect's documented behavior. `ContainerRuntime` gained `create_network`, `remove_network`, `start_background_container`, and `stop_and_remove_container`; `run_container` now takes a `name`/`network` pair so a task's own container can join its dependencies' network. New `uuid` dependency for collision-resistant network naming (process ID was considered and rejected — it's frequently `1` when `ratect` runs inside a container, e.g. CI). No `health_check`/`setup_commands` support, so a dependency counts as ready as soon as it starts; see `docs/task-lifecycle.md` (new) for the full model with diagrams, and `docs/differences-from-batect.md` for what's simplified relative to Batect. New `tests/fixtures/sidecar.yml` fixture (two sibling dependencies plus one nested behind them) and ignored Docker integration test prove real cross-container DNS resolution for both siblings and nesting together, not just that the right calls were made. Unit tests cover nesting to four levels deep, within-task dedup of a dependency shared by multiple siblings (asserting each sibling itself started, not just the shared one), cross-task isolation, and circular-dependency detection.

### Fixed

- `ROADMAP.md` incorrectly listed `--project-name` as an example Batect CLI flag; it's actually a `batect.yml` config field, not a CLI option. Corrected and cross-linked to the itemized flag table in `docs/differences-from-batect.md`.
- Fatal errors (malformed config, missing task/container, dependency cycle) previously bypassed `tracing` entirely, propagating to `main`'s default `Result` handler and printing via `anyhow`'s raw `Debug` formatting — inconsistent with every other diagnostic message, and unaffected by `RUST_LOG`. `main` now returns `ExitCode` and routes the final error through `tracing::error!` like everything else.
- Container exit codes weren't checked: a task whose command exited non-zero was still reported as successful, and dependent tasks still ran. `run_container` now waits for the container and checks its exit status (via `wait_container`, falling back to bollard's `DockerContainerWaitError` for non-zero codes); a non-zero exit raises a new `ContainerExitedNonZero` error, and `main` propagates the *exact* exit code as `ratect`'s own process exit code (matching `docker run`'s convention), rather than a generic failure code. This also means a failing prerequisite now correctly stops the rest of the chain, matching Batect's documented behavior. New ignored Docker integration tests (`tests/fixtures/exit-code.yml`) prove both the exact-code propagation and the prerequisite-chain-stops behavior against a real daemon; a new unit test proves dependency/network cleanup still happens even when the main container fails.
- A missing config file exited `0` instead of failing, for both `--list-tasks` and running a task. `run()` now checks for the config file up front and fails fast with a non-zero exit before branching into either mode, which also removed the duplicated `Option<Config>` handling that existed to work around the old behavior. (Running with no task name at all, unrelated to this fix, still intentionally exits `0` with a warning — see `docs/cli-reference.md`.)
- `-- ADDITIONAL_ARGS` was parsed but silently dropped, never reaching the task's command. `TaskEngine::run_task` and `ContainerRuntime::run_container` now thread the args through, scoped to only the explicitly-requested task (never its prerequisites, matching Batect). Since `command` always runs via `sh -c`, args are appended as `sh -c`'s own positional parameters (`$1`, `$2`, `$@` — `sh -c '<command>' sh arg1 arg2 ...`) rather than concatenated into the command string, so they're never re-parsed as shell syntax regardless of what characters they contain (verified with an arg containing `;`, `&&`, and backticks against a real daemon). If a container has no `command` at all, non-empty additional args are passed directly as its argv instead, matching plain `docker run <image> <args>`. New `tests/fixtures/additional-args.yml` and an ignored Docker integration test prove real forwarding end to end; a new unit test proves prerequisites never receive the args.
- Unsupported `batect.yml` keys were silently ignored instead of raising an error — a typo'd or not-yet-implemented field (e.g. `environment` on a container) would load without complaint and just silently do nothing. `Config`, `Container`, `Task`, and `TaskRun` now derive `#[serde(deny_unknown_fields)]`, so any unrecognized key fails config loading with an error naming the field. This closes the last of the four `ratect-compat` 0.1.0 correctness gaps in `ROADMAP.md`. Deliberately implemented via `#[serde(deny_unknown_fields)]` on plain `noyalib::from_reader`, not `noyalib::from_reader_strict`/`from_str_strict`: `noyalib` 0.0.13's strict-mode path deserializes through its `Value` type, whose `Deserializer` impl forwards `deserialize_option` straight to `deserialize_any` — which breaks (`invalid type: string "...", expected option`) on every *populated* `Option` field, i.e. almost every field in this schema. `deny_unknown_fields` on the regular streaming deserializer doesn't hit that path and works correctly. New unit test `load_from_file_unsupported_key_errors` proves it, plus a `tests/fixtures/unsupported-key.yml` fixture and `unsupported_config_key_reports_error` CLI test proving the end-to-end process behavior (non-zero exit, field name on stderr).
- A task's own container with neither `image` nor `build_directory` set silently did nothing and still exited `0`, as if the task had succeeded — unlike dependency/sidecar containers, which already errored in this situation via `start_dependency`. `run_task_internal` now raises the same class of error for the main task container, naming the container. New unit test `container_without_image_or_build_directory_errors`, plus `tests/fixtures/no-image.yml` and `container_without_image_or_build_directory_reports_error` CLI test.

### Changed

- Added a "Versioning & Releases" section to `ROADMAP.md`: `ratect-compat` and `ratect` are versioned independently (different maturity clocks), but shared-core bug fixes/security patches get a coordinated release for both regardless. Defines `ratect-compat`'s 0.1.0 as an honesty milestone (fix the known correctness gaps in `docs/differences-from-batect.md` — exit codes not checked, missing config exits `0`, dropped `-- ADDITIONAL_ARGS`, silently-ignored unsupported keys) rather than a features milestone, plans 0.2.0 through 0.6.0 (environment variables/expressions, image building, interactive mode, user mapping, then networking/proxy support — sequenced so later items can reuse earlier ones, e.g. proxy support building on 0.2.0's environment variable support), notes what's left beyond that (includes, the long tail of smaller config/CLI fields) isn't optional for 1.0.0 even though it's not release-planned yet, and ties 1.0.0 for each binary to its own definition of "done" (Batect parity vs. interface stability).
- Converted the project into a Cargo workspace: extracted `config.rs`/`docker.rs`/`engine.rs` (and their tests) into a new `ratect-core` library crate, leaving the `ratect` binary crate as thin CLI glue (`src/main.rs` only) over `ratect-core`'s public API. Pure refactor, no behavior change — sets up the [two-binary plan](ROADMAP.md#two-binaries-ratect-and-ratect-compat) (a future `ratect-compat` and `ratect` sharing this same core) without committing to the rename or building the second binary yet. CI now runs `--workspace` variants of build/test/clippy/coverage.
- Restructured `ROADMAP.md`'s CLI plan from a two-phase single-binary evolution (with eventual deprecation of Batect-compatible flags) into two permanent binaries sharing one core: `ratect-compat` (strict Batect CLI/YAML parity, the target for all "Batect Parity" roadmap items) and `ratect` (a free-to-diverge modern CLI, not required to maintain Batect parity). Ratect will not ship a binary literally named `batect`, to avoid confusion/trademark concerns; a drop-in `./batect` replacement is achieved by the user symlinking or renaming `ratect-compat` themselves. Also added an undecided/exploratory TOML-as-alternative-config-format item to "Future Vision", scoped to the `ratect` binary only.
- Updated project version to `0.1.0-dev` to reflect pre-release status.
- Migrated YAML parsing from `serde_yaml` to `noyalib` for improved safety and maintenance.
- Upgraded core dependencies to their latest stable versions.
- `Cargo.lock` is now committed to the repository (previously gitignored), following the convention for binary crates to ensure reproducible builds and accurate dependency audits.
- Applied `cargo fmt` formatting across `src/`.
- Wired up `tracing`/`tracing-subscriber`: task lifecycle, unimplemented-feature, and config-error diagnostics now go through leveled, `RUST_LOG`-filterable log events on stderr, while command output (task listing, container log streaming) remains on stdout via `println!`/`print!`.
