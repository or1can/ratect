# Differences from Batect

Ratect is a from-scratch Rust implementation inspired by
[Batect](https://github.com/batect/batect) (which is itself no longer maintained — the
upstream repository was archived in October 2023), not a wrapper or fork. It does not
read Batect's documentation or source at runtime. To move an existing project
over, follow [Migrating a Batect Project to Ratect](migrating-batect-project.md);
this page is what changes once you have.

**Every Batect configuration field and CLI flag is supported, field-for-field and
flag-for-flag, unless listed below** — see [config reference](ratect-compat-config-reference.md)/
[CLI reference](ratect-compat-cli.md) for the full accepted schema and flags. This page
lists the exceptions: a real behavioral divergence, an extension beyond what Batect
does, or a restriction narrower than it. It doesn't restate how a field or flag
works — [config reference](ratect-compat-config-reference.md)/[CLI reference](ratect-compat-cli.md)/
[task lifecycle](task-lifecycle.md) are the authoritative source for that; this page
only says what's *different* and points there for the rest.

> **Unrecognized fields fail closed**: Ratect's YAML parsing rejects unknown keys —
> a typo'd field name, or (unlikely, since Batect's own `include` `type`s beyond
> `file`/`git` are the only thing left genuinely unsupported here) a real gap —
> fails config loading with an error naming it, rather than silently ignoring it.
> There's no partial/best-effort mode.

## Configuration format

### Top-level fields

Every other top-level field is supported field-for-field — see [config
reference](ratect-compat-config-reference.md) for the full list. The exceptions:

| Field | Notes |
|---|---|
| `config_variables` | `description:` is recognized but inert — Ratect has no help/usage output to show one in. |
| `include` | Ratect enforces that a Git include's `path` (and anything it transitively includes) stays within that repository's own clone; Batect has no equivalent containment check. In `ratect.toml` specifically, a Git-included bundle also can't declare further Git includes of its own unless `allow_nested_git_includes` is set — `ratect-compat` stays unrestricted, matching Batect. See [What a bundle may do](includes.md#what-a-bundle-may-do). |
| `forbid_telemetry` | Recognized, no effect — Ratect doesn't collect telemetry, so there's nothing to forbid. |

### Expressions

Matches Batect exactly, field-for-field — see
[Expressions](ratect-compat-config-reference.md#expressions) for the full syntax and which
fields support it. The one exception, `image`, is in [Container
fields](#container-fields) below.

### Container fields

Every other container field is supported field-for-field — see [config
reference](ratect-compat-config-reference.md#container) for the full list. The exceptions:

| Field | Notes |
|---|---|
| `image` | An [expression](#expressions)-looking value (`$VAR`) is rejected when the file loads rather than resolved or used as a literal — Batect resolves nothing here either, but silently treats it as a literal image name that then fails at pull time instead. `ratect.toml` does resolve them; see [config reference](ratect-compat-config-reference.md#container). |
| `volumes` | A `cache` mount's `name` must use Docker's own volume-name character set; Batect doesn't validate it at all, so an unvalidated name could bind-mount an arbitrary host directory under `--cache-type=directory`. Breaking change for `--cache-type=directory` only — `--cache-type=volume` already enforced this via Docker itself. See [Cache volumes](ratect-compat-config-reference.md#cache-volumes). |
| `capabilities_to_add` / `capabilities_to_drop` | Also accepts `BPF`/`CHECKPOINT_RESTORE`/`PERFMON` — Docker capabilities added after Batect's last release, so its own `Capability` enum predates them. A superset: every config Batect itself accepts here still parses identically. |
| `health_check` / `setup_commands` | The task's own container's readiness gate can race a very fast main command — see [task lifecycle](task-lifecycle.md#known-limitations). |
| `log_driver` / `log_options` | An absent value leaves the daemon's own default alone; Batect's config model bakes in a literal `"json-file"` default explicitly. Immaterial in practice — that's Docker's own out-of-the-box default too. |
| `run_as_current_user` | Host-side uid/gid lookup only works on Unix — see [User mapping](ratect-compat-config-reference.md#user-mapping). |

### Task fields

Every task field is supported field-for-field, with no divergence from Batect —
see [config reference](ratect-compat-config-reference.md#task) for the full list.

### `run` fields

Every `run` field is supported field-for-field, with no divergence from Batect —
see [TaskRun](ratect-compat-config-reference.md#taskrun) for the full list.

## CLI flags

Every other flag from Batect's own [CLI
reference](https://github.com/batect/batect.dev/blob/main/docs/reference/cli.mdx)
is supported flag-for-flag — see [CLI reference](ratect-compat-cli.md) for the full
list. The exceptions:

| Flag | Notes |
|---|---|
| `--version` | Also gets a `-V` short form Batect doesn't have (a `clap` default). |
| `--output` / `-o` | An explicit `-o fancy` on a non-interactive console fails up front with a clear error; Batect accepts it and crashes with an unhandled exception on the first repaint. `all`'s status lines also drop Batect's inner `Batect \| ` prefix — the outer prefix already says whose line it is. See [Output Styles](output-styles.md). |
| `--no-color` | A superset, not a gap: Batect rejects `-o fancy --no-color` at parse time (its console couples color and cursor movement under one flag); Ratect's keeps them independent, so that combination renders colorless fancy instead. |
| `--no-cleanup`, `--no-cleanup-after-failure`, `--no-cleanup-after-success` | Batect's own `DontCleanup` still stops a started container, just skips removing it; Ratect leaves it genuinely running (not just present-but-stopped) for investigation. |
| `--docker-cert-path`, `--docker-tls`, `--docker-tls-verify`, `--docker-tls-ca-cert`, `--docker-tls-cert`, `--docker-tls-key` | Batect's bare `--docker-tls` (without `-verify`) disables *all* server certificate verification, not just hostname matching. Ratect doesn't support that mode at all — `--docker-tls` and `--docker-tls-verify` behave identically here, the daemon's certificate always fully verified. See [TLS with a private certificate authority](connecting-to-docker.md#tls-with-a-private-certificate-authority) for the supported alternative (your own CA). |
| `--cache-type` | Unlike Batect, not forced to `directory` for Windows containers — Ratect has no Windows support to special-case. |
| `--max-parallelism` | Batect's flag caps *every* setup/cleanup step via a step-scheduling model Ratect doesn't have. Ratect's caps a narrower set — image pulls/builds, a dependency's create+start, and setup commands — the resource-intensive operations; health-check waits and cleanup teardown are deliberately excluded, and the task's own container's run is never gated, matching Batect's own exemption for it. |
| `--log-file` | Batect's own default (no `--log-file`) is a silent `NullLogSink`, nothing anywhere; Ratect always logs to stderr regardless, so `--log-file` here tees into a file *in addition to* stderr, not instead of it. |
| `--no-update-notification`, `--upgrade`, `--no-wrapper-cache-cleanup` | Recognized, no effect — permanently inapplicable, since Ratect is a single native binary with no self-updating wrapper script to disable notifications for, clean caches for, or upgrade. Recognized rather than rejected so an existing Batect invocation carrying one doesn't hard-fail outright. See [CLI reference](ratect-compat-cli.md#recognized-no-effect). |

## Runtime behavior gaps

Batect behavior not implemented in task execution, beyond what's covered by the field
tables above:

- **Cleanup on a termination signal (Ctrl+C, `SIGTERM`, `SIGHUP`)**: three deliberate
  differences from Batect, which traps `SIGINT` only:

  - **Ratect also traps `SIGTERM` and `SIGHUP`**, down the same cleanup path — a task
    runner is stopped by more than a keystroke (an editor closing its subprocess,
    `docker stop`, `systemd`, most CI cancel buttons), and under Batect each
    leaks a container and network.
  - **The exit code names the signal**: 128 + the signal's own number (`130`/`143`/`129`
    for Ctrl+C/`SIGTERM`/`SIGHUP`). Batect returns `-1`/255 for every failure alike.
  - **A second signal during cleanup stops the cleanup itself**, immediately — Batect
    instead switches to printing manual cleanup commands. Whatever's left still carries
    Ratect's ownership labels, so [`ratect resources
    list`/`clean`](ratect-cli.md#managing-resources) finds it (`ratect`-only;
    `ratect-compat` has no equivalent verb, so from it the sweep is `docker` itself,
    filtering the same labels).

  `SIGKILL` remains untrappable by either tool, same underlying OS limitation — the
  same labels are what a post-hoc `ratect resources clean` needs to find what it left.
- **Ownership labels**: every container and network Ratect creates carries
  `eu.orican.ratect.*` labels; Batect labels nothing of its own. An additive
  divergence — changes no behavior. See
  [decisions/0002](../decisions/0002-runtime-ownership-labels.md).
- **Interactive mode**: two known divergences. Batect's real-TTY gate checks only
  whether its output is a real terminal; Ratect's requires *both* stdin and stdout
  to be real terminals. Live terminal-resize tracking is also Unix-only — synced
  once at session start elsewhere, not tracked further. See [Interactive
  Mode](interactive-mode.md).
- **Proxy support**: two deliberate differences — see [Proxies](proxies.md)
  for the full mechanics.

  - **A `localhost` proxy is rewritten on Linux too.** Batect rewrites on
    macOS/Windows only; on Linux it propagates the URL verbatim, where `localhost`
    means the container itself. This is [Batect's oldest open
    issue](https://github.com/batect/batect/issues/10), eight years old.
  - **A proxy bound to loopback only is diagnosed, not left to fail** — Batect's own
    roadmap notes the same warning is needed and never shipped one.

  What stays an accepted gap is Batect's Docker-version-gated hostname fallback
  chain, which reaches back to Docker 17.06 — not worth chasing for any
  actively-maintained daemon.
- **Private registry credentials**: Batect's own Go client swallows every
  credential-helper error unconditionally; Ratect warns instead, naming the
  registry that failed to resolve. See [Private registry
  credentials](ratect-compat-config-reference.md#private-registry-credentials).
- **A dependency exiting unexpectedly is reported, not silent.** Batect has no
  equivalent: a dependency that dies after becoming ready — while the task's own
  command, or a later dependency's own health/setup wait, is still going — is
  otherwise invisible until whatever depended on it fails for a confusing,
  unrelated-looking reason (a connection refused, a timeout). Ratect prints a
  warning naming the container and its exit code instead, in every output mode.
  See [Dependency Readiness](dependency-readiness.md#once-ready).
- **`all` mode splits on a lone carriage return too, not just `\n`.** Batect's
  `InterleavedContainerOutputSink` splits on `\n` only, so a container using
  `\r` to redraw progress in place (pip/curl/apt-style) produces no output at
  all until the stream ends, then dumps everything as one giant concatenated
  line. A deliberate divergence: Ratect flushes on a lone `\r` (one not
  immediately followed by `\n` — a CRLF pair still folds to a single line
  break) the same way it already does on `\n`, so a real progress bar
  prints one interleaved line per redraw tick instead of staying silent —
  spammier, but never silent-then-dumped.

## Next steps

The behaviour both binaries share is documented in its own right rather
than against Batect: [Task Lifecycle](task-lifecycle.md)
for what a run does step by step, [Dependency
Readiness](dependency-readiness.md) for when a dependency counts as ready,
[Includes](includes.md) for how included files combine and what a Git bundle
may do, and the [FAQ](faq.md).
