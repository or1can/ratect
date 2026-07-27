# 0002 — Runtime-ownership labels (`eu.orican.ratect.*`)

**Status:** Accepted — shipped (ratect-compat 0.21.1 · ratect 0.2.0). Implemented
in [`ratect-core/src/labels.rs`](../ratect-core/src/labels.rs).

## Context

Cleaning up what a previous run left behind — after a crash, a `docker kill`, a
`--no-cleanup` run, or a failed teardown — was unanswerable. Nothing was marked
on the way in: containers were created via `create_container(None, config)` with
no name of Ratect's own, and `labels` carried only what the *user* configured. A
leftover was identifiable at best by inference (it's attached to a `ratect-<uuid>`
network), and under `--use-network` not even that. Networks were greppable only
by their `ratect-` name prefix, and couldn't be attributed to a project or task.

Batect is no better here — `DockerContainerCreationSpecFactory` applies
`container.labels` and nothing of its own, and Batect has no cleanup command at
all — so this had simply never been answerable, which is exactly the complaint
the [`resources` verb](../ROADMAP.md#uxtooling) exists to fix.

## Decision

**Stamp Ratect's own labels on every container and network it creates**, modelled
on Docker Compose's `com.docker.compose.*` labels — runtime *ownership*, distinct
from OCI image annotations:

| Label | On | Value |
| --- | --- | --- |
| `eu.orican.ratect.project` | containers, networks | `project_name` |
| `eu.orican.ratect.task` | containers, networks | the task being run |
| `eu.orican.ratect.run` | containers, networks | the per-run id (the `Uuid` that already names the per-task network, reused) |
| `eu.orican.ratect.container` | containers | the *config* container name (Docker's own name is random) |
| `eu.orican.ratect.role` | containers | `task` or `dependency` |
| `eu.orican.ratect.version` | containers, networks | the Ratect version that created it |

Sub-decisions baked in:

- **Namespace `eu.orican.ratect.*`** — reverse-DNS of a domain the project
  already owns, not a new `ratect.dev`-style one. Reverse-DNS here is purely a
  collision-avoidance convention (nothing resolves it), so a new domain would buy
  nothing functional while adding a renewal obligation that every `docker inspect`
  output would then depend on.
- **Ratect's keys win on an exact collision** with a user-configured `labels`
  entry. They're load-bearing for cleanup — a config that set
  `eu.orican.ratect.run` (accidentally or otherwise) would otherwise make its own
  resources unfindable.
- **The `version` value comes from the *binary*** (`TaskEngineSettings::ratect_version`),
  not `ratect-core` — whose version isn't what `--version` reports, and since the
  two binaries are on independent version lines
  ([ADR-0001](0001-two-binaries.md)) it also records *which* binary created the
  resource.
- **The run id is generated per task execution** in `run_task_internal` and
  threaded down through `ensure_container_ready`, rather than read from the
  network's name — because `--use-network` creates no network to read it from,
  yet the containers still have to agree on one id. Creation *time* needs no
  label; Docker records its own for both object kinds.

## Alternatives considered

- **OCI image annotations.** Rejected: those describe an *image*'s provenance,
  a different thing from a *running resource*'s ownership. We label what we
  create at runtime, which Compose's model — not the OCI image spec — is the
  right precedent for.
- **A new `ratect.dev` domain for the namespace.** Rejected: see above — no
  functional gain, a standing renewal obligation. The one thing that might have
  justified one, a durable public URL for the committed
  [JSON schema](../schema/batect-config.schema.json) (as Batect used
  `ide-integration.batect.dev`), doesn't need it either: ~35–40% of SchemaStore's
  own catalog entries are `raw.githubusercontent.com` URLs, and a future docs
  site is planned for `ratect.orican.eu` — whose reverse-DNS *is* this namespace.
- **Inferring ownership from the `ratect-<uuid>` network name (no labels).** The
  status quo. Rejected: breaks entirely under `--use-network`, can't attribute a
  resource to a project or task, and says nothing about containers directly.

## Consequences

- The [`resources list`/`clean`](../ROADMAP.md#uxtooling) verb becomes possible
  at all, plus `ContainerRuntime::list_containers`/`list_networks` with
  daemon-side label filtering.
- It's a **parity divergence** — Batect writes no labels of its own — but a
  strictly *additive* one that changes no task behaviour and can't break a task
  adopting `ratect-compat`, in the same family as the `Capability` superset and
  the UUID cache key. Documented in
  [Differences from Batect](../docs/differences-from-batect.md#runtime-behavior-gaps).
- The namespace is **sticky, not irreversible**: the only reader that matters is
  Ratect itself, so a later version can match a legacy namespace alongside a new
  one for a release or two and still find older orphans.
- One thing labels *can't* resolve: a concurrently-running task's containers are
  labelled identically to an orphan (they are the same thing until the run ends).
  Reporting age and taking `--older-than` is the honest mitigation; claiming to
  detect liveness would be a lie, since the daemon can't say whether some other
  `ratect` process still cares about a container.
