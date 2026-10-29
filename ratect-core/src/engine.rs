// Copyright 2026 Orican Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The core execution logic — task lifecycle,
//! prerequisites, dependency-cycle detection, sidecar/dependency container resolution
//! (see [`docs/task-lifecycle.md`](https://github.com/or1can/ratect/blob/main/docs/task-lifecycle.md),
//! and [`docs/dependency-readiness.md`](https://github.com/or1can/ratect/blob/main/docs/dependency-readiness.md)
//! for when a dependency counts as ready), and once-per-session
//! dedup of image pulls/builds/task runs. `TaskEngine` is generic over
//! `ContainerRuntime`. Worth knowing: opt-in settings (`existing_network`,
//! `publish_ports`, etc.) are builder methods rather than `TaskEngine::new`
//! parameters, so each new one lands without a mass-edit of the ~30 existing call
//! sites — with `TaskEngineSettings`/`with_settings` (0.2.0-dev) as the
//! plain-data form of that same set, which is what the *binaries* use (both
//! expose the same ~10 knobs behind differently-named flags, so neither
//! duplicates the builder chain; a new setting needs adding in both places or a
//! binary can't reach it, and this module's own tests keep using the builders,
//! where naming one setting reads better than a mostly-default struct); and
//! every task's own container, the named task's and each prerequisite's, is
//! eligible for interactive-TTY mode — never a dependency's. `run_task_internal`
//! runs `prerequisites` first, then returns early (no error) if the task itself has
//! no `run` (0.14.0) — everything after can assume `run` is present. `customise`
//! threads through `start_dependency`'s own recursion unconditionally, so it
//! reaches its target regardless of depth in the dependency graph. Under the
//! Batect-compatible dialect the task's own container goes through the same
//! readiness gate a dependency has (`run_task_container_readiness`), run
//! *concurrently* with `ContainerRuntime::run_container`'s own
//! attach-and-wait-for-exit (the engine's first concurrent-exec path) rather
//! than gating anything on it. A gate failure while the main command is still
//! running cancels it — the run is dropped exactly as an interrupt drops it, and
//! the ordinary cleanup stops and removes the container — and the run's end ends
//! the gate: whatever it was still doing is dropped, and after an exit 0 a
//! failure it has already reported is forgiven. Under the native dialect it has
//! no gate at all, which `task_container_readiness_gate`, derived once from the
//! project's dialect, is the only switch for; its creation is still recorded and
//! announced either way. Why each of those, and where the outcome differs from
//! Batect's, is decisions/0012. `run_container` takes a `started`
//! `oneshot::Sender` for this — a bare `()`, sent right after Docker's own
//! `start` call, which is when the readiness gate may begin (both its health
//! inspect and its `docker exec` need a *running* container).
//!
//! Every container — a dependency's and the task's own — goes through **two
//! memoised stages** per task execution (decisions/0019): *created*
//! (`ensure_container_created`: its image resolved and the task network
//! ready, plus the creation of any `container:` network-mode target → an id
//! and the spec it was created from) and *ready* (`ensure_container_ready`:
//! every container it depends on ready → started → through its readiness
//! gate). A dependency's readiness gates only the *start* of what depends on
//! it; creation waits for nothing but the image and the network, so a
//! create-time failure (a bad mount, a log driver the daemon doesn't have)
//! ends the run as soon as it lands — not once every dependency is ready, by
//! which time their setup commands have had their side effects — and
//! `run_task_internal` fans creation out over the whole graph the moment the
//! network exists. `ContainerRuntime::create_container` returns the id
//! synchronously (or carries it in its error, when Docker created the
//! container but preparing it failed), and the engine registers it in
//! `created_containers` before anything else can happen — matching Batect's
//! own `containersCreated` set, which is what its `CleanupStagePlanner` plans
//! removals from. From that instant the engine can remove the container, so
//! no later failure can strand it, which is what lets `run_container` `?`
//! freely. This replaced (0.25.0) a scheme where `run_container` removed its
//! own container and took a `readiness` channel purely to order that removal
//! after the gate; the cleanup flags then had to be interpreted identically in
//! two modules, and keeping them in step by hand produced a distinct bug in
//! each of three consecutive review rounds. Don't reintroduce a removal here.
//! `resolve_volumes` (0.18.0) turns a container's `VolumeMount`s into the
//! literal bind strings `docker.rs` expects — a `Local` mount's already fully
//! resolved by `LoadedProject::evaluate` when its task is reached, nothing
//! left to do but reassemble the string; a `Cache` mount goes through
//! `cache::resolve_cache_mount`, memoizing the project's own cache key in a
//! `Mutex` field (computed at most once per invocation, and only if a
//! project-scoped `cache` mount is resolved under `CacheType::Volume` —
//! never eagerly, and never in directory mode). `--cache-type` + the project
//! directory reach `TaskEngineSettings::cache` via `main.rs`'s own `with_settings` call,
//! always made in practice despite being optional here. `Tmpfs` mounts are
//! deliberately *not* resolved by `resolve_volumes` at all (0.21.0) — a
//! tmpfs mount can't be expressed as a
//! bind string, and needs no cache-key lookup either, so a separate,
//! synchronous `container_spec::tmpfs_mounts` helper (alongside
//! `capability_names`/`device_triples` there) pulls them out into a
//! `ContainerOptions.tmpfs` field instead, mapped onto Docker's own
//! `HostConfig.Tmpfs` map by `docker.rs`'s `build_tmpfs_mounts`.

use crate::config::{
    container_names_in_task, Container, Evaluated, LoadedProject, Task, TaskContainerCustomisation,
};
use crate::container_spec::{derive_build_spec, BindMount, BindSource};
use crate::docker::ContainerRuntime;
use crate::ui::{CleanupCause, EventSink, TaskEvent};
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// How long a task container's failed readiness gate waits for the main
/// command's own result when Docker still reports the container running.
/// A setup command killed by the main command's exit can report before
/// Docker has recorded that exit (ratect#263's fixture, on CI), so one
/// "running" answer doesn't prove the command is still going. Seconds, not
/// milliseconds, because that window is a daemon round-trip under load; short
/// because a main command that really is still running is cancelled only
/// once it expires.
const TASK_CONTAINER_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// The host environment lookup `TaskEngine` reads proxy variables from —
/// boxed so the real `std::env::var`-backed closure and a fixed test
/// closure share one field type.
type HostEnv = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Where `unreachable_proxy_ports` gets the host's `/proc/net/tcp` tables —
/// the real files in the real constructor, fixed text in tests, for the same
/// reason `HostEnv` above is a field and not a direct `std::env::var` call.
/// Without it the loopback warning would be unreachable on any machine
/// without a `/proc`, which is every machine this suite runs on but the CI
/// Linux job.
type ProcNetTcp = Box<dyn Fn() -> Vec<String> + Send + Sync>;

/// The `TERM=dumb` every container gets under the interleaved I/O policy
/// (the `all` output mode) — a full-screen program shouldn't try terminal
/// control sequences when its output is being line-buffered and prefixed,
/// matching Batect's `InterleavedContainerIOStreamingOptions`.
fn dumb_term_environment() -> HashMap<String, String> {
    HashMap::from([("TERM".to_string(), "dumb".to_string())])
}

/// Whether a mount's Docker options mark it read-only — the `ro` flag in the
/// comma-separated list Docker itself parses. `rw` is the default and needs
/// no special casing.
fn is_read_only(options: &Option<String>) -> bool {
    options
        .as_deref()
        .is_some_and(|o| o.split(',').any(|flag| flag.trim() == "ro"))
}

/// What one container's setup-command gate needs that isn't in the commands
/// themselves — the declaring container, and what it takes to `exec` into it.
/// Bundled rather than passed as six parameters, matching
/// `container_spec::ContainerSpecInputs`'s own convention.
struct SetupCommandContext<'a> {
    /// The container declaring these setup commands — the one whose
    /// readiness gate this is, and the one every event/error names, whether
    /// or not a command actually runs somewhere else.
    name: &'a str,
    /// The declaring container's own id: where a command without `run_in`
    /// runs.
    container_id: &'a str,
    /// The declaring container's effective working directory, the fallback
    /// for a command that sets none of its own: its derived spec's, so a
    /// task's `run.working_directory` or a dependency's
    /// `customise.working_directory` applies, as in Batect's code (ratect#271).
    /// Arrives resolved rather than being recomputed in
    /// [`TaskEngine::run_setup_commands`].
    working_directory: Option<&'a str>,
    /// The declaring container's environment and user, passed to `exec` for
    /// a command running in that same container. Both are deliberately
    /// *not* forwarded to a `run_in` target — see [`ExecTarget`].
    environment: Option<&'a HashMap<String, String>>,
    user_mapping: Option<&'a crate::docker::UserMapping>,
    /// Container name -> id, for the dependencies this container was gated
    /// on, all of them already ready. The lookup a `run_in` target resolves
    /// through (ratect#111), and the reason the config loader restricts a
    /// target to exactly this set.
    dependency_ids: &'a HashMap<String, String>,
}

/// Where one setup command actually runs, once `run_in` has been resolved:
/// either the declaring container (with its own resolved environment/user
/// passed along) or a dependency of it (with neither, since Docker's `exec`
/// inherits both from whichever container it runs in — and the declaring
/// container's would be the wrong ones to send).
struct ExecTarget<'a> {
    container_id: &'a str,
    /// The quoted container name for an error message — with, when the
    /// command ran somewhere other than where it was declared, both names.
    description: String,
    working_directory: Option<&'a str>,
    environment: Option<&'a HashMap<String, String>>,
    user_mapping: Option<&'a crate::docker::UserMapping>,
}

/// The outcome of a memoized async operation (an image pull/build, or a
/// dependency container reaching "ready") shared across every concurrent
/// caller that reaches the same cache key. `anyhow::Error` isn't `Clone`, so
/// a failure is wrapped in `Arc` — every waiter that shares a [`ReadyCell`]
/// sees the same outcome without re-attempting the underlying Docker call.
type SharedResult<T> = Result<T, Arc<anyhow::Error>>;

/// A lazily-created, memoized cell holding the eventual outcome for one cache
/// key — see `get_or_create_cell`. `tokio::sync::OnceCell` (rather than
/// `get_or_try_init`, which does *not* cache a failure) so a failed
/// pull/build/create/start is cached just like a successful one: a later
/// caller sharing the same key sees the same `Err` instead of retrying the
/// real Docker call.
type Cell<T> = Arc<OnceCell<SharedResult<T>>>;

/// The cell for an image name or ID (`ensure_image_resolved`) or a *ready*
/// container's ID (`ensure_container_ready`), keyed by container name.
type ReadyCell = Cell<String>;

/// The cell for a *created* container (`ensure_container_created`), keyed by
/// container name.
type CreatedCell = Cell<CreatedContainer>;

/// What `ensure_container_created` yields: the container Docker created and
/// the spec it was created from, which the ready stage needs again — the
/// start call takes it, and the readiness gate's setup commands read their
/// working directory, environment and user from it. Carried rather than
/// re-derived so the two stages can't disagree about what the container is.
#[derive(Clone)]
struct CreatedContainer {
    id: String,
    spec: crate::container_spec::ContainerSpec,
}

/// Container name -> id, for every container Docker has created in this one
/// task execution, the task's own included — registered the moment
/// `create_container` hands the id back (whether as its result or inside its
/// error), before anything else can fail — by the create itself, which
/// `PendingCreates` keeps running to that answer even when the run that
/// asked for it is dropped. Read only at cleanup, once nothing is pending,
/// which removes exactly this set: a container whose start
/// was then abandoned (a sibling failed and `try_join_all` dropped it), or
/// whose start failed, is still here (ratect#224). `Mutex`-guarded since
/// independent branches of the dependency graph proceed concurrently and
/// each registers itself from its own task.
type CreatedContainers = Mutex<HashMap<String, String>>;

/// How long an abandoned build's cancel is given to be accepted — see
/// `TaskEngine::start_cancelling_abandoned_builds`. Short: the cancel is
/// one request the daemon answers at once, and only its warning waits on
/// it.
const BUILD_CANCEL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// One abandoned build's cancel, sent and not yet answered: the daemon's
/// acceptance or refusal, or the timeout.
type BuildCancel = tokio::task::JoinHandle<Result<Result<()>, tokio::time::error::Elapsed>>;

/// Every container create this task execution has sent, held so that one
/// whose caller is dropped mid-request — a sibling's failure ending
/// `try_join_all`, or a signal ending the run — still runs to the daemon's
/// answer and registers what it created (ratect#318). The future does the
/// registering itself, so whoever drives it to completion, the caller or
/// cleanup, records the container in `CreatedContainers`; cleanup drives
/// whatever is still unfinished before it reads that set. Without this, a
/// create the daemon had already done was dropped with its future, never
/// recorded, and left behind.
///
/// Each create is held with the container's config name, which is all a
/// report can say about one cut short before it answered.
type PendingCreates<'a> = Mutex<
    Vec<(
        String,
        futures::future::Shared<futures::future::BoxFuture<'a, ()>>,
    )>,
>;

/// Gets (or lazily creates) the shared cell for `key` in `cells`, under a
/// short synchronous lock — the lock is dropped before the returned cell is
/// ever `.await`ed on (by the caller, via `.get_or_init`), so it's held only
/// for a `HashMap` lookup/insert, never across `.await` — same
/// double-checked-lock convention this file already used for
/// `pulled_images`/`built_images` pre-0.15.0.
fn get_or_create_cell<K: Eq + std::hash::Hash, T>(
    cells: &Mutex<HashMap<K, Cell<T>>>,
    key: K,
) -> Cell<T> {
    let mut cells = cells.lock().unwrap();
    cells
        .entry(key)
        .or_insert_with(|| Arc::new(OnceCell::new()))
        .clone()
}

/// Flattens a cached [`SharedResult`] back into a plain, owned `Result` at
/// the point it's actually returned to a caller — the shared `Arc<Error>` is
/// reformatted via its `Debug` (which anyhow's `Error` renders as the full
/// context chain) rather than just its `Display` (the top message only),
/// since this may be the only place a waiter *other than* the one that hit
/// the real failure ever sees it.
///
/// Flattening drops the error's types, so a [`NotReady`] is rebuilt as one
/// around the same text: whether its report gets the re-run hint is decided
/// from its type, at the top of the run, long after this.
///
/// A [`Failed`] tag is re-attached the same way, so the step a dependency
/// failed at still names it for every waiter.
fn unshare<T: Clone>(result: &SharedResult<T>) -> Result<T> {
    result.clone().map_err(|e| {
        let flattened = if is_not_ready(&e) {
            NotReady(format!("{e:?}")).into()
        } else {
            anyhow::anyhow!("{:?}", e)
        };
        match failed_step(&e) {
            Some(cause) => Failed::tag(flattened, cause),
            None => flattened,
        }
    })
}

/// An error tagged with the step it failed at and the container it failed
/// for — what the opening cleanup line names (decisions/0017). Transparent:
/// it displays as the error it wraps and continues that error's own cause
/// chain, so the `Error:` report reads exactly as it would untagged.
/// Attached where each step fails; read back with [`failed_step`].
struct Failed {
    cause: CleanupCause,
    error: anyhow::Error,
}

impl Failed {
    /// Never a signal, a closed stdout or the task's own exit: those are
    /// found by downcasting the run's error, which this would hide.
    fn tag(error: anyhow::Error, cause: CleanupCause) -> anyhow::Error {
        debug_assert!(
            !error.is::<crate::interrupt::TaskInterrupted>()
                && !error.is::<crate::docker::StdoutClosed>()
                && !error.is::<crate::docker::ContainerExitedNonZero>(),
            "tagged a run outcome as a failed step: {error:?}"
        );
        anyhow::Error::new(Failed { cause, error })
    }

    /// `container`'s own step `cause`, unless it is an external health
    /// check's companion: that is never named (ratect#202), and whatever
    /// stopped it, the container it checks didn't become healthy.
    fn of_container(
        container: &str,
        reports_readiness_for: Option<&str>,
        cause: impl FnOnce(String) -> CleanupCause,
    ) -> CleanupCause {
        match reports_readiness_for {
            Some(checked) => CleanupCause::NotHealthy {
                container: checked.to_string(),
            },
            None => cause(container.to_string()),
        }
    }
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::fmt::Debug for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.error, f)
    }
}

impl std::error::Error for Failed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

/// The step `error` failed at, if one was tagged — found through any
/// context added since, and through the [`FailureReported`] a reported
/// failure is wrapped in before cleanup.
fn failed_step(error: &anyhow::Error) -> Option<CleanupCause> {
    match error.downcast_ref::<Failed>() {
        Some(failed) => Some(failed.cause.clone()),
        None => error
            .downcast_ref::<FailureReported>()
            .and_then(|reported| failed_step(&reported.0)),
    }
}

/// Whether `error` is a [`NotReady`] failure, looking through a [`Failed`]
/// tag, which `anyhow`'s own downcast can't see past.
fn is_not_ready(error: &anyhow::Error) -> bool {
    error.is::<NotReady>()
        || error
            .downcast_ref::<Failed>()
            .is_some_and(|failed| is_not_ready(&failed.error))
}

/// Everything one task execution's container stages share — the graph, the
/// network, the memo cells, the registries, and what the task's own
/// container is created with — handed to `ensure_container_created` and
/// `ensure_container_ready` as one borrow instead of a dozen parameters each
/// (the previous shape, which `#[allow(clippy::too_many_arguments)]` was
/// excusing). Lives on `run_task_internal`'s stack for the run.
struct TaskRunContext<'a, 'p> {
    /// Container name -> its direct dependencies, proven acyclic by
    /// `build_dependency_graph`; the root's entry is the union of its own
    /// `dependencies` and the task's.
    graph: &'a HashMap<String, Vec<String>>,
    /// Every container in the task, evaluated.
    containers: &'a HashMap<String, Container>,
    /// The task's network — created, or `--use-network`'s.
    network: &'a str,
    created_cells: &'a Mutex<HashMap<String, CreatedCell>>,
    ready_cells: &'a Mutex<HashMap<String, ReadyCell>>,
    image_cells: &'a Mutex<HashMap<String, ReadyCell>>,
    /// `'p`, not `'a`: a create in `pending_creates` registers into this,
    /// and may outlive the run's own borrows.
    created_containers: &'p CreatedContainers,
    pending_creates: &'a PendingCreates<'p>,
    no_proxy_entries: &'a std::collections::BTreeSet<String>,
    customisations: Option<&'a IndexMap<String, TaskContainerCustomisation>>,
    run_labels: &'a crate::labels::RunLabels,
    resource_names: &'a crate::resource_names::ResourceNames,
    dependency_watchers: &'a Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Every container this run started — what cleanup says `Container
    /// stopped.` for; one created but never started is only removed.
    started_containers: &'a Mutex<std::collections::HashSet<String>>,
    /// The task's own container: its name, its `run` (the overlay it is
    /// created with, where a dependency gets `customise`), the
    /// `-- ADDITIONAL_ARGS` only it receives, and whether it is
    /// interactive-eligible (`run_task_internal` decides; every task's is,
    /// prerequisites included, outside `-o all`).
    root: &'a str,
    run: &'a crate::config::TaskRun,
    additional_args: &'a [String],
    interactive: bool,
}

/// The error for a container name with no container behind it. The config
/// loader's `reject_references_to_undeclared_containers` refuses every such
/// reference before any task runs, naming what referred to it, so a miss
/// here is a bug in Ratect, not in the user's configuration — and the
/// message says so rather than repeating the loader's attribution.
fn undeclared_container_bug(name: &str) -> String {
    format!(
        "Container '{name}' not found — the configuration loader should have refused this \
         reference, so this is a bug in Ratect"
    )
}

/// Builds the deduplicated container dependency graph for one task
/// execution: `root` (the task's own container) plus any task-level
/// `dependencies` (unioned into `root`'s own adjacency list — the same union
/// `run_task_internal` computes for its dependency startup, and the same one
/// `container_names_in_task` uses for the `no_proxy` exemption list); every
/// other node's adjacency list is just its own `container.dependencies`.
///
/// Detects a circular container dependency eagerly, via an explicit DFS
/// ancestor path: a name already on the current path is a real cycle; a name
/// already fully built into the returned graph is a *diamond* (shared, not
/// circular) and is skipped without re-visiting. That detection mirrors
/// Batect's own `ContainerDependencyGraph` — a container listing itself is
/// refused too — but the message doesn't: Batect's separate self-dependency
/// and path-describing cycle wordings aren't reproduced, a divergence
/// docs/differences-from-batect.md records ("Failure messages are Ratect's
/// own"). Run once, synchronously, before any concurrent execution begins.
/// This static split is why `ensure_container_ready` no longer needs its own
/// runtime cycle guard (pre-0.15.0's `resolving` set) — a graph returned from
/// here is already proven acyclic.
fn build_dependency_graph(
    containers: &HashMap<String, Container>,
    root: &str,
    task_dependencies: Option<&[String]>,
) -> Result<HashMap<String, Vec<String>>> {
    fn visit(
        containers: &HashMap<String, Container>,
        name: &str,
        extra_root_dependencies: Option<&[String]>,
        path: &mut Vec<String>,
        graph: &mut HashMap<String, Vec<String>>,
    ) -> Result<()> {
        if graph.contains_key(name) {
            return Ok(());
        }
        if path.iter().any(|ancestor| ancestor == name) {
            anyhow::bail!(
                "Circular container dependency detected involving '{}'",
                name
            );
        }
        path.push(name.to_string());

        let container = containers
            .get(name)
            .with_context(|| undeclared_container_bug(name))?;
        let mut dependencies = container.dependencies.clone().unwrap_or_default();
        if let Some(extra) = extra_root_dependencies {
            dependencies.extend(extra.iter().cloned());
        }
        dependencies.sort();
        dependencies.dedup();

        for dependency in &dependencies {
            visit(containers, dependency, None, path, graph)?;
        }

        path.pop();
        graph.insert(name.to_string(), dependencies);
        Ok(())
    }

    let mut graph = HashMap::new();
    let mut path = Vec::new();
    visit(containers, root, task_dependencies, &mut path, &mut graph)?;
    Ok(graph)
}

/// `container`'s direct dependencies in the order they were declared, each
/// once at its first position — for the task's own container, `task`'s
/// `dependencies` first, then its own. What a display orders by, as Batect's
/// insertion-ordered graph does; `build_dependency_graph`'s sorted lists are
/// what startup walks, and are unaffected.
fn declared_dependencies(container: &Container, task: Option<&[String]>) -> Vec<String> {
    let mut declared: Vec<String> = Vec::new();
    for name in task
        .into_iter()
        .flatten()
        .chain(container.dependencies.iter().flatten())
    {
        if !declared.contains(name) {
            declared.push(name.clone());
        }
    }
    declared
}

/// Whether `name` gets lines of its own — progress, a removal, a hint — in
/// what a run prints: every container but one whose readiness is reported
/// as another's, an `external_health_check`'s companion, which is an
/// implementation detail of the container it checks (ratect#202). A report
/// of what was *left* names a companion anyway, since it is there to remove
/// and nothing else says so. One answer for every site, so the next one
/// added can't forget it.
fn is_narrated(containers: &HashMap<String, Container>, name: &str) -> bool {
    containers
        .get(name)
        .is_none_or(|container| container.reports_readiness_for.is_none())
}

/// For each container in `created`, the others in `created` that depend on
/// it — directly, or through a container that was never created — and so
/// must have their stop attempted before it: Batect's `StopContainerStepRule`
/// waits on each container's dependents, which at any depth is the same
/// order. A dependent only has to have been *created* to count: one that
/// never started is stopped at once (Docker answers `304`), so it holds
/// nothing back, which is what Batect gets by counting only started
/// dependents. A container `graph` doesn't know waits on nothing.
fn dependents_to_stop_first(
    graph: &HashMap<String, Vec<String>>,
    created: &[&str],
) -> HashMap<String, HashSet<String>> {
    fn reach<'g>(graph: &'g HashMap<String, Vec<String>>, name: &str, seen: &mut HashSet<&'g str>) {
        for dependency in graph.get(name).into_iter().flatten() {
            if seen.insert(dependency.as_str()) {
                reach(graph, dependency, seen);
            }
        }
    }

    let mut dependents: HashMap<String, HashSet<String>> = created
        .iter()
        .map(|name| (name.to_string(), HashSet::new()))
        .collect();
    for dependent in created {
        let mut dependencies = HashSet::new();
        reach(graph, dependent, &mut dependencies);
        for dependency in dependencies {
            if let Some(waiting) = dependents.get_mut(dependency) {
                waiting.insert(dependent.to_string());
            }
        }
    }
    dependents
}

/// Builds the anchored, case-sensitive regex a `*`-wildcard prerequisite
/// pattern expands to — a direct port of Batect's own
/// `TaskExecutionOrderResolver.toWildcardRegex`: each literal segment
/// between `*`s is regex-escaped (so a task name containing regex
/// metacharacters like `.`/`+`/`(` is matched literally, not interpreted),
/// and `*` itself becomes `.*` (zero or more characters) — equivalent to
/// escaping every `*`-delimited segment and joining them with `.*`.
fn wildcard_to_regex(pattern: &str) -> Result<regex::Regex> {
    let escaped_segments: Vec<String> = pattern.split('*').map(regex::escape).collect();
    let pattern = format!("^{}$", escaped_segments.join(".*"));
    regex::Regex::new(&pattern)
        .with_context(|| format!("Invalid wildcard prerequisite pattern '{}'", pattern))
}

/// Expands any `*`-wildcard entry in a task's `prerequisites` against the
/// full set of task names — a direct port of Batect's own
/// `TaskExecutionOrderResolver.resolveWildcards`. An entry with no `*` passes
/// through unchanged, so a nonexistent literal prerequisite name still
/// surfaces its usual "Task not found" error (from `find_task`), rather than
/// being silently dropped here. A wildcard matching zero tasks
/// contributes nothing — not an error, matching Batect ("if a wildcard does
/// not match any tasks, no error is raised"). Multiple matches for one
/// wildcard are sorted alphabetically, matching Batect too.
///
/// A name appearing more than once in the returned list (an explicit name
/// also matched by a wildcard, or matched by two overlapping wildcards) is
/// left as-is, deliberately not deduplicated here: Ratect's existing
/// per-invocation `executed_tasks` tracking (see `run_task_scoped`) already
/// collapses repeated runs of the same task down to a single actual run,
/// using whichever occurrence comes first — matching Batect's own "if a task
/// is listed explicitly and also matches a wildcard, the first occurrence is
/// used" rule as a natural side effect, with no extra list-level dedup
/// needed here.
fn expand_prerequisite_wildcards(
    tasks: &IndexMap<String, Task>,
    patterns: &[String],
) -> Result<Vec<String>> {
    let mut expanded = Vec::new();
    for pattern in patterns {
        if !pattern.contains('*') {
            expanded.push(pattern.clone());
            continue;
        }
        let regex = wildcard_to_regex(pattern)?;
        let mut matches: Vec<String> = tasks
            .keys()
            .filter(|name| regex.is_match(name))
            .cloned()
            .collect();
        matches.sort();
        expanded.extend(matches);
    }
    Ok(expanded)
}

/// Levenshtein edit distance between `a` and `b` — a textbook Wagner-Fischer
/// implementation, ported from Batect's own `EditDistanceCalculator`.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous_row: Vec<usize> = (0..=b.len()).collect();
    let mut current_row = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        current_row[0] = i;
        for j in 1..=b.len() {
            current_row[j] = if a[i - 1] == b[j - 1] {
                previous_row[j - 1]
            } else {
                1 + previous_row[j - 1]
                    .min(previous_row[j])
                    .min(current_row[j - 1])
            };
        }
        std::mem::swap(&mut previous_row, &mut current_row);
    }

    previous_row[b.len()]
}

/// Suggests likely-intended task names for a mistyped `name` — ported from
/// Batect's own `TaskSuggester`: every task name within edit distance 3,
/// closest first. Deliberately not a literal port of Batect's own tie
/// handling: Batect's `suggestCorrections` sorts via a `Comparator` that
/// only compares by distance, and — because that same comparator also
/// decides the backing `TreeMap`'s key uniqueness — two task names that tie
/// on distance are treated as "equal" and silently collapse to just one
/// suggestion, dropping the other. This breaks ties alphabetically instead,
/// so a tie shows every equally-close match rather than an arbitrary one of
/// them.
fn suggest_task_names(tasks: &IndexMap<String, Task>, name: &str) -> Vec<String> {
    let mut suggestions: Vec<(usize, &String)> = tasks
        .keys()
        .map(|task_name| (edit_distance(name, task_name), task_name))
        .filter(|(distance, _)| *distance <= 3)
        .collect();
    suggestions.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    suggestions
        .into_iter()
        .map(|(_, task_name)| task_name.clone())
        .collect()
}

/// Joins `items` into a human-readable list — `["a"]` → `"a"`, `["a", "b"]`
/// → `"a or b"`, `["a", "b", "c"]` → `"a, b or c"` (no Oxford comma) — ported
/// from Batect's own `Collection<String>.asHumanReadableList`.
pub(crate) fn human_readable_list(items: &[String], conjunction: &str) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        _ => {
            let (last, rest) = items.split_last().expect("non-empty, checked above");
            format!("{} {} {}", rest.join(", "), conjunction, last)
        }
    }
}

/// Builds the `" Did you mean 'x' or 'y'?"` suffix Batect appends to a
/// "task does not exist" error — an empty string when nothing is close
/// enough to suggest. See `suggest_task_names`.
fn format_task_suggestions(tasks: &IndexMap<String, Task>, name: &str) -> String {
    let suggestions = suggest_task_names(tasks, name);
    if suggestions.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = suggestions.iter().map(|s| format!("'{}'", s)).collect();
    format!(" Did you mean {}?", human_readable_list(&quoted, "or"))
}

/// The task a run was asked for doesn't exist — `Task 'X' not found.`,
/// with any `Did you mean` suffix. A distinct type so each binary can append
/// its own hint on listing the tasks, which `ratect-core` can't word: Batect
/// ends this error with one. A missing *prerequisite* is never one of these,
/// as Batect gives that no hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTask {
    task: String,
    suggestions: String,
}

impl std::fmt::Display for UnknownTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Task '{}' not found.{}", self.task, self.suggestions)
    }
}

impl std::error::Error for UnknownTask {}

/// `name`'s task, or the error a run fails with when there is none — one
/// lookup for `check_prerequisites` and `run_task_internal`, so their
/// messages can't drift. `requested` says `name` is the task the run was
/// asked for, whose absence is an [`UnknownTask`]; a prerequisite's is the
/// same message, untyped.
fn find_task<'t>(
    tasks: &'t IndexMap<String, Task>,
    name: &str,
    requested: bool,
) -> Result<&'t Task> {
    tasks.get(name).ok_or_else(|| {
        let unknown = UnknownTask {
            task: name.to_string(),
            suggestions: format_task_suggestions(tasks, name),
        };
        if requested {
            unknown.into()
        } else {
            anyhow::anyhow!("{unknown}")
        }
    })
}

/// The error for a task reached again while it is still in progress.
fn task_cycle(name: &str) -> anyhow::Error {
    anyhow::anyhow!("Dependency cycle detected involving task '{name}'")
}

/// Checks `task_name`'s whole prerequisite order before any of it runs, and
/// fails with the missing task or cycle Batect's `TaskExecutionOrderResolver`
/// would report — it resolves the order before `SessionRunner` starts the
/// first task. Without this, a problem in a later prerequisite surfaced only
/// after the earlier ones had run, side effects and all.
///
/// Batect's order: every entry of a task's list (wildcards expanded) is
/// looked up and checked against the current path before any of them is
/// walked into, so a problem in a shallower list is the one reported. Each
/// task is walked at most once. The run itself still walks prerequisites
/// one at a time, depth first (`run_task_internal`), with the same lookup
/// and expansion. `skip_prerequisites` skips `task_name`'s own list, as
/// `--skip-prerequisites` does, so nothing in it is looked up.
fn check_prerequisites(
    tasks: &IndexMap<String, Task>,
    task_name: &str,
    skip_prerequisites: bool,
) -> Result<()> {
    fn visit(
        tasks: &IndexMap<String, Task>,
        task: &Task,
        path: &mut Vec<String>,
        checked: &mut HashSet<String>,
    ) -> Result<()> {
        let Some(prerequisites) = &task.prerequisites else {
            return Ok(());
        };
        let mut resolved = Vec::new();
        for name in expand_prerequisite_wildcards(tasks, prerequisites)? {
            let prerequisite = find_task(tasks, &name, false)?;
            if path.contains(&name) {
                return Err(task_cycle(&name));
            }
            resolved.push((name, prerequisite));
        }
        for (name, prerequisite) in resolved {
            if checked.contains(&name) {
                continue;
            }
            path.push(name.clone());
            visit(tasks, prerequisite, path, checked)?;
            path.pop();
            checked.insert(name);
        }
        Ok(())
    }

    let task = find_task(tasks, task_name, true)?;
    if skip_prerequisites {
        return Ok(());
    }
    visit(
        tasks,
        task,
        &mut vec![task_name.to_string()],
        &mut HashSet::new(),
    )
}

/// The error a task fails with when its command exited `0` but cleanup then
/// failed to remove something it set out to remove — the daemon refused a
/// removal, or the cleanup ladder's abandon rung was reached with something
/// still to remove. Like any other failure it ends the chain: a prerequisite
/// that fails this way stops the run before the task that depends on it.
///
/// A distinct type, like [`crate::docker::ContainerExitedNonZero`] and
/// [`crate::interrupt::TaskInterrupted`], so [`crate::exit_code::for_error`]
/// can give it [`crate::exit_code::RATECT_FAILED`] without inspecting a
/// message. Only ever returned in place of `Ok`: a run that failed keeps its
/// own error, cleanup notwithstanding (decisions/0016). What `--no-cleanup*`
/// leaves in place on purpose is not a cleanup failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupFailed {
    /// The task whose cleanup failed — a prerequisite's, as often as not.
    pub task: String,
    /// That task's run id, the value of [`crate::labels::RUN`] on whatever
    /// it left behind.
    pub run: String,
}

impl std::fmt::Display for CleanupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "task '{}' passed, but cleaning up after it failed; anything left behind \
             carries the label {}={}",
            self.task,
            crate::labels::RUN,
            self.run
        )
    }
}

impl std::error::Error for CleanupFailed {}

/// A container that didn't become ready: it never became healthy, or one of
/// its setup commands couldn't be run or exited non-zero — a dependency's,
/// or the task's own under its gate. The three failures Batect's
/// `FailureErrorMessageFormatter` follows with a hint to re-run with
/// `--no-cleanup-after-failure`, because the containers it would keep are
/// where the cause is (ratect#368). A type of its own, used as the context
/// or the error at exactly those sites, so the hint never depends on what a
/// message happens to say; it displays as its message alone.
#[derive(Debug)]
struct NotReady(String);

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotReady {}

/// What a [`NotReady`] failure's report ends with while cleanup after
/// failure is on.
const RERUN_WITHOUT_CLEANUP_HINT: &str = "You can re-run the task with \
     --no-cleanup-after-failure to leave its containers in place and diagnose the failure.";

/// A run's infrastructure failure — any but the task's own non-zero exit, a
/// signal, or stdout's reader going — that the engine has already reported
/// as a [`crate::ui::TaskEvent::Error`] at the moment it happened, before
/// cleanup (ratect#334). [`crate::exit_code::already_reported`] knows it, so
/// `main` prints no second `Error:` line for it.
///
/// Transparent: it displays, and chains its causes, exactly as the error it
/// wraps, so `{:?}` and `{:#}` render the same text as before. What it does
/// hide is the wrapped error's type from `anyhow`'s downcasts, so
/// [`crate::exit_code::for_error`] looks through it to find the exit code.
pub struct FailureReported(pub(crate) anyhow::Error);

impl std::fmt::Display for FailureReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Debug for FailureReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::error::Error for FailureReported {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// What a cleanup left in place — a removal the daemon refused, one the
/// abandon rung never reached, or everything a `--no-cleanup*` flag kept —
/// and the one command that removes it, for the line that reports it
/// (decisions/0017: `left behind: <what>. Remove with: <command>`, the same
/// tail on the cleanup-failure error and the `--no-cleanup*` notice).
///
/// The command is `docker`'s, not `ratect resources clean`: this is shared
/// core, and `resources` is a `ratect` verb `ratect-compat` doesn't have,
/// while `docker` works from either. It names containers by id — the handle
/// the engine registers each one under at creation, shortened to the twelve
/// characters `docker ps` shows, which Docker resolves — and removes them
/// with `-f -v`, since that is what Ratect's own removal does (force, and
/// the anonymous volumes with them); a plain `docker rm -f` would leave
/// those volumes. The list names each by its config name too, since that is
/// the one the user knows.
struct LeftBehind {
    /// `(config name, container id)`.
    containers: Vec<(String, String)>,
    network: Option<String>,
}

impl LeftBehind {
    fn new<'a>(
        containers: impl IntoIterator<Item = (&'a str, &'a str)>,
        network: Option<&str>,
    ) -> Self {
        Self {
            containers: containers
                .into_iter()
                .map(|(name, id)| (name.to_string(), id.to_string()))
                .collect(),
            network: network.map(str::to_string),
        }
    }

    fn is_empty(&self) -> bool {
        self.containers.is_empty() && self.network.is_none()
    }

    /// The cleanup-failure error: what went wrong — abandonment, a refusal,
    /// or both, since a refusal before the abandonment still says why its
    /// container was left — then what is left and how to remove it.
    /// `refusals` are the daemon's own messages, in the order they came.
    fn failure_message(&self, abandoned: bool, refusals: &[String]) -> String {
        let what = match (abandoned, refusals.is_empty()) {
            (true, true) => "Cleanup abandoned".to_string(),
            (true, false) => format!("Cleanup abandoned after failing: {}", refusals.join("; ")),
            (false, _) => format!("Cleanup failed: {}", refusals.join("; ")),
        };
        if self.is_empty() {
            format!("{what}. Nothing is known to be left behind.")
        } else {
            format!(
                "{what}; left behind: {}. Remove with: {}",
                self.list(),
                self.command()
            )
        }
    }

    /// The id as `docker ps` shows it — its first twelve characters, which
    /// Docker resolves like the full id.
    fn short_id(id: &str) -> &str {
        id.get(..12).unwrap_or(id)
    }

    /// `db (a1b2c3d4e5f6), app (0f1e2d3c4b5a), network demo-network-…`.
    fn list(&self) -> String {
        self.containers
            .iter()
            .map(|(name, id)| format!("{name} ({})", Self::short_id(id)))
            .chain(
                self.network
                    .iter()
                    .map(|network| format!("network {network}")),
            )
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `Investigate db with: docker logs a1b2c3d4e5f6, or docker exec -it
    /// a1b2c3d4e5f6 <command>` — Batect's two hints for a kept container,
    /// the `exec` preceded by a `docker start` when it isn't `running`, so
    /// the line works when pasted either way.
    fn investigation(name: &str, id: &str, running: bool) -> String {
        let id = Self::short_id(id);
        let start = if running {
            String::new()
        } else {
            format!("docker start {id}; ")
        };
        format!(
            "Investigate {name} with: docker logs {id}, or {start}docker exec -it {id} <command>"
        )
    }

    /// `docker rm -f -v a1b2c3d4e5f6 0f1e2d3c4b5a; docker network rm
    /// demo-network-…` — the network last, since it can't go while a container
    /// is still attached. Joined with `;`, not `&&`: an abandoned removal
    /// the daemon finished anyway is listed too, and `docker rm` failing on
    /// that one id must not stop the network's removal after it.
    fn command(&self) -> String {
        let mut steps = Vec::new();
        if !self.containers.is_empty() {
            let ids: Vec<&str> = self
                .containers
                .iter()
                .map(|(_, id)| Self::short_id(id))
                .collect();
            steps.push(format!("docker rm -f -v {}", ids.join(" ")));
        }
        if let Some(network) = &self.network {
            steps.push(format!("docker network rm {network}"));
        }
        steps.join("; ")
    }
}

pub struct TaskEngine<D: ContainerRuntime + Send + Sync + 'static> {
    /// The project as loaded — its values unevaluated. Each task's
    /// containers and values are [evaluated](LoadedProject::evaluate) when
    /// the task is reached (see `run_task_internal`), and everything that
    /// task's run does reads that evaluated view; this is read only for what
    /// comes before it — finding the task, its prerequisites, and which
    /// containers to evaluate.
    project: LoadedProject,
    /// `--config-var`/`--config-vars-file`, for evaluating each task.
    config_var_overrides: HashMap<String, String>,
    /// `Arc`, not owned by value: `ensure_container_ready` needs to hand a
    /// clone into a detached `tokio::spawn`ed dependency-exit watcher (see
    /// `TaskEvent::DependencyExitedUnexpectedly`) that outlives its own
    /// call frame. Every other call site keeps calling `self.docker.foo()`
    /// unchanged — method resolution derefs through the `Arc` for free.
    docker: Arc<D>,
    executed_tasks: Mutex<HashSet<String>>,
    /// Image name and pull policy -> the shared, memoized pull outcome for
    /// that pair (the image ID to run — see `resolve_pulled_image`), so an
    /// image referenced by multiple containers (across tasks, or by
    /// concurrent branches of one task's own dependency graph — 0.15.0) is
    /// only ever decided/pulled once per invocation for each policy. The
    /// policy is part of the key, as it is of Batect's `PullImage` source,
    /// so an `IfNotPresent` container finding the image locally never
    /// stands in for an `Always` one's pull, nor the reverse (ratect#313).
    /// Batect re-decides for each task of a prerequisite chain; this
    /// doesn't — see docs/differences-from-batect.md. A `ReadyCell` rather
    /// than a plain `HashSet`+check-then-act specifically so two containers
    /// racing to resolve the same image concurrently share one in-flight
    /// pull instead of double-pulling.
    pulled_images: Mutex<HashMap<(String, crate::config::ImagePullPolicy), ReadyCell>>,
    /// Container name -> the shared, memoized build outcome (the built image
    /// ID) for that container, so a container with `build_directory` is only
    /// ever built once per invocation even if referenced by multiple tasks,
    /// as both a dependency and a task's own container, or reached
    /// concurrently by two branches of one task's dependency graph (0.15.0).
    /// Keyed by container name (not build directory) since a given name
    /// always has the same `build_directory`/`build_args` within one
    /// invocation: each task re-evaluates its containers, but against the
    /// same host environment and config-variable overrides, so to the same
    /// values. Stores the image ID (not the human-readable tag) — see
    /// `resolve_image` for why.
    built_images: Mutex<HashMap<String, ReadyCell>>,
    /// Build ID -> the container whose image it builds, for every
    /// `build_image` call that hasn't answered yet: registered before the
    /// call is made, removed once it returns, Ok or Err. One still here at
    /// cleanup is an abandoned build — its future was dropped, by a signal
    /// or by a sibling's failure ending `try_join_all` — and dropping it
    /// only closed the connection, which leaves a BuildKit build running in
    /// the daemon. Cleanup cancels each by its ID (ratect#537).
    in_flight_builds: Mutex<HashMap<String, String>>,
    in_progress_tasks: Mutex<HashSet<String>>,
    /// Set via `--use-network`: an existing Docker network to reuse for
    /// every task in this invocation instead of creating a fresh one per
    /// task. `None` (the default) preserves today's behavior.
    existing_network: Option<String>,
    /// `false` when `--disable-ports` was given: suppresses every
    /// container's `ports` regardless of config, matching Batect's
    /// `disablePortMappings`. `true` (the default) publishes them.
    publish_ports: bool,
    /// `false` when `--no-proxy-vars` was given: suppresses proxy
    /// environment variable propagation entirely, matching Batect's
    /// `dontPropagateProxyEnvironmentVariables`. `true` (the default)
    /// propagates them.
    propagate_proxy_environment_variables: bool,
    /// The host environment lookup each task's values are evaluated
    /// against, and `proxy::proxy_environment_variables` reads from — real
    /// `std::env::var` in the real constructor, a fixed closure in tests
    /// (see `with_host_env`), same reason `LoadedProject::evaluate`
    /// parameterizes over this.
    host_env: HostEnv,
    /// The `/proc/net/tcp`/`tcp6` source `unreachable_proxy_ports` reads —
    /// see `ProcNetTcp` and `with_proc_net_tcp`.
    proc_net_tcp: ProcNetTcp,
    /// `true` when `--skip-prerequisites` was given: the top-level task's
    /// own `prerequisites` are never run. Matches Batect's flag of the same
    /// name — scoped to the named task only (never a prerequisite itself,
    /// which is the only other thing that could otherwise trigger this
    /// check; see `run_task_internal`'s `top_level` parameter).
    skip_prerequisites: bool,
    /// Set via `--override-image <container>=<image>` (repeatable):
    /// container name -> the image to pull instead of whatever that
    /// container actually configures. Checked against `config.containers`
    /// each time a task with a `run` is reached (`check_image_overrides`),
    /// as in Batect, rather than when the engine is configured. See
    /// `resolve_image`.
    image_overrides: HashMap<String, String>,
    /// Set via `--tag-image <container>=<tag>` (repeatable, multiple tags
    /// per container): container name -> extra tags applied to that
    /// container's *built* image, in addition to the default
    /// `<project_name>-<container_name>` tag `resolve_image` already
    /// applies. Never looked up in `config.containers`, unlike
    /// `image_overrides`: a tag on a container that uses a pulled image
    /// fails when that container's image is resolved (see `resolve_image`),
    /// and one on a container that's never reached at all once the whole
    /// invocation finishes (see `run_task`).
    /// Batect makes the second check at the same point, but the first only
    /// once the task using the container has finished.
    image_tags: HashMap<String, std::collections::HashSet<String>>,
    /// Every container name `resolve_image` has been asked to resolve so
    /// far this invocation (task and prerequisites alike) — regardless of
    /// whether the underlying pull/build was deduped. Used only to answer
    /// `--tag-image`'s "did this container actually run" check once the
    /// whole invocation finishes (see `run_task`).
    containers_used: Mutex<HashSet<String>>,
    /// A constructor argument (alongside `docker`), not a setting: this is a
    /// collaborator the engine talks to, not something it was told, and
    /// every caller has one to give it (a binary always constructs one,
    /// even if [`crate::interrupt::Interrupt::listen`] is only called on it
    /// afterward; a test that doesn't care passes a fresh, never-recorded
    /// one). No `Option` — an `Interrupt` nothing has recorded on behaves
    /// exactly like "no tracker" did: `wait_for`/`interrupted` never
    /// resolve, so the `biased` `tokio::select!`s below always take the
    /// non-interrupt branch, same as the `None` arm they replace. That was
    /// every unit test before this, and every run before 0.25.0.
    interrupt: Arc<crate::interrupt::Interrupt>,
    /// Whether the task's own container runs its readiness gate
    /// (`run_task_container_readiness`) alongside its main command. Derived
    /// once, in `with_settings`, from the project's
    /// [`crate::config::Dialect`]: `true` for Batect-compatible (the
    /// default), `false` for native, where running the container *is* the
    /// task — see decisions/0012.
    task_container_readiness_gate: bool,
    /// `false` when `--no-cleanup`/`--no-cleanup-after-success` was given:
    /// the task's own container (regardless of exit code — see
    /// `docker::ContainerRuntime::run_container`'s own doc comment for why
    /// a nonzero exit is still "success" here), its dependency containers,
    /// and the task's own network are all left in place instead of removed.
    /// Applies to the top-level task — the one named — alone: a
    /// prerequisite is always cleaned up after success, as Batect's
    /// `RunOptions` does for every task but the main one. `true` (the
    /// default) always cleans up. See `run_task_internal`.
    cleanup_after_success: bool,
    /// `false` when `--no-cleanup`/`--no-cleanup-after-failure` was given:
    /// same as `cleanup_after_success`, but for a genuine infrastructure
    /// failure (a build/pull/health-check/setup-command failure, or
    /// anything else that never reaches the task's own container exiting),
    /// and for every task the run reaches, prerequisites included — Batect's
    /// split between the two buckets (`TaskEvent::TaskFinished` vs
    /// `TaskEvent::TaskFailed` already encode it), and its scope, where only
    /// the success flag is limited to the main task. `true` (the default)
    /// always cleans up.
    cleanup_after_failure: bool,
    /// Set via `--max-parallelism <N>`: caps how many resource-intensive
    /// operations run concurrently across the whole invocation — image
    /// pulls/builds (`resolve_pulled_image`/`resolve_image`'s build branch),
    /// every container's creation (`ensure_container_created`'s
    /// `create_container` call, the task's own included), a dependency's
    /// start (`ensure_container_ready`'s `start_background_container`
    /// call), and setup-command execution (`run_setup_commands`'
    /// `exec_in_container` call, one permit per command — a single
    /// container's own setup commands already run sequentially, so this
    /// only ever limits how many *different* containers' setup commands
    /// overlap). A create and a start are separate permits, each released
    /// when its step returns, and none is held while waiting for a
    /// dependency to be ready — so a chain longer than the cap can't
    /// deadlock on it (decisions/0019). `None` (the default) is unbounded,
    /// matching both Ratect's own pre-existing behavior and Batect's own
    /// default when the flag isn't passed.
    ///
    /// Deliberately *not* applied to `wait_for_container_healthy` (a health
    /// check is a polling wait, not CPU/disk work — gating it would only
    /// slow down convergence for no resource-saving benefit) or to
    /// `stop_and_remove_container` (cleanup teardown isn't resource-
    /// intensive in practice). Also never applied to the task's own
    /// container's `run_container` call, which starts it and waits for its
    /// main command: that command is often long-running by design (an
    /// interactive shell, a dev server), so it must never hold or wait on a
    /// slot. That is Ratect's own reason. Its *creation* counts.
    ///
    /// Batect draws the line elsewhere. Its one step with
    /// `countsAgainstParallelismCap = false` is `RunContainerStep` — every
    /// container's start and run, dependencies included — because that step
    /// lasts the container's whole lifetime; every container's
    /// `CreateContainerStep` counts. So Ratect caps a dependency's start
    /// where Batect doesn't, and otherwise covers less: Batect's flag
    /// schedules every other setup/cleanup step (the ones excluded here
    /// among them) through a step-scheduling model
    /// (`ParallelExecutionManager`) Ratect doesn't have — see [Differences
    /// from Batect](../../docs/differences-from-batect.md#cli-flags). A single
    /// shared semaphore (rather than one per image/container) is what makes
    /// this an invocation-wide cap rather than a per-resource one.
    max_parallelism: Option<Arc<tokio::sync::Semaphore>>,
    /// Where task-execution milestones go for the user to see — a
    /// constructor argument alongside `docker`/`interrupt`, since this too
    /// is a collaborator the engine talks to rather than a setting it was
    /// told. [`crate::ui::NullEventSink`] (silent) is what tests that don't
    /// care pass.
    /// See `crate::ui`.
    event_sink: Arc<dyn EventSink>,
    /// Set via `with_settings` (always called by `main.rs`, unset only
    /// in tests that don't exercise `cache` volumes): `--cache-type` and the
    /// project's own root directory, needed to resolve a `cache` volume
    /// mount into an actual Docker bind string — see
    /// `resolve_volumes`/`crate::cache`.
    cache_options: Option<crate::cache::CacheOptions>,
    /// The creating binary's own version, stamped onto every resource this
    /// engine creates (`crate::labels::VERSION`). `None` in tests, which
    /// have no binary version to report — the label is then omitted rather
    /// than invented. Set via `TaskEngineSettings::ratect_version`, since
    /// `ratect-core`'s own version isn't what a user sees from
    /// `--version` (see `ROADMAP.md`'s versioning section).
    ratect_version: Option<String>,
    /// Memoizes `crate::cache::project_cache_key` for the life of this
    /// `TaskEngine` — computed at most once per invocation, and only when a
    /// project-scoped `cache` mount is resolved under `CacheType::Volume`
    /// (never eagerly, and never for a directory or shared cache), matching
    /// where Batect reads its own `by lazy` `CacheManager.projectCacheKey`.
    /// A `Mutex` rather than a `OnceCell` because `resolve_cache_mount`
    /// asks for the key synchronously; held across the read so two
    /// containers resolving at once can't each generate a different key.
    cache_key: std::sync::Mutex<Option<String>>,
}

/// Refuses a container named by both `--tag-image` and `--override-image`,
/// as Batect's `CommandLineOptionsParser.validate()` does once the options
/// have parsed: an overridden container is pulled, so there is no built
/// image to tag. Both binaries call this before they read any config, so
/// the conflict is reported even for a container the config doesn't
/// declare. Of several, the first in `image_tags`' order is named, as in
/// Batect. An [`InvalidOptionValue`](crate::exit_code::InvalidOptionValue),
/// so it exits as an argument error does (decisions/0016).
pub fn check_tag_and_override(
    image_tags: &[(String, String)],
    image_overrides: &[(String, String)],
) -> Result<()> {
    let overridden: HashSet<&str> = image_overrides
        .iter()
        .map(|(container, _)| container.as_str())
        .collect();
    match image_tags
        .iter()
        .find(|(container, _)| overridden.contains(container.as_str()))
    {
        Some((container, _)) => Err(crate::exit_code::InvalidOptionValue::new(format!(
            "Cannot both tag the built image for container '{container}' and also override \
             the image for that container."
        ))
        .into()),
        None => Ok(()),
    }
}

/// Every opt-in [`TaskEngine`] setting as plain data, for
/// [`TaskEngine::with_settings`] — the shape a binary's own CLI flags map
/// onto, so `ratect` and `ratect-compat` can offer the same knobs under
/// whatever names each one's interface calls for without either duplicating
/// the builder chain.
///
/// [`Default`] is Ratect's own default behavior with no flags given at all
/// (publish ports, propagate proxy variables, run prerequisites, clean up
/// either way, unbounded parallelism) — so a binary only has to set what
/// its user actually asked to change.
#[derive(Debug, Clone)]
pub struct TaskEngineSettings {
    /// `--use-network`: reuse this existing network for every task instead
    /// of creating one per task.
    pub existing_network: Option<String>,
    /// `false` for `--disable-ports`.
    pub publish_ports: bool,
    /// `false` for `--no-proxy-vars`.
    pub propagate_proxy_environment_variables: bool,
    /// `false` for `--skip-prerequisites`.
    pub run_prerequisites: bool,
    /// `--override-image <container>=<image>`. Validated against the config
    /// by [`TaskEngine::with_settings`], which is why that returns a
    /// `Result`.
    pub image_overrides: HashMap<String, String>,
    /// `--tag-image <container>=<tag>`, collected per container.
    pub image_tags: HashMap<String, HashSet<String>>,
    /// `false` for `--no-cleanup-after-success` (or `--no-cleanup`).
    pub cleanup_after_success: bool,
    /// `false` for `--no-cleanup-after-failure` (or `--no-cleanup`).
    pub cleanup_after_failure: bool,
    /// `--max-parallelism <N>`; `None` is unbounded.
    pub max_parallelism: Option<usize>,
    /// `--cache-type` plus the project's own root directory
    /// ([`crate::config::LoadedProject::project_directory`]). `None` only
    /// makes sense for a caller whose config has no `cache` volume mounts —
    /// in practice every binary supplies it.
    pub cache: Option<(crate::cache::CacheType, PathBuf)>,
    /// The calling binary's own version (`env!("CARGO_PKG_VERSION")`),
    /// recorded on every resource created — see [`crate::labels::VERSION`].
    /// The binary passes its own rather than `ratect-core` reading its
    /// own, since the core's version isn't what a user sees from
    /// `--version`.
    pub ratect_version: Option<String>,
    /// The project's dialect — [`crate::config::LoadedProject::dialect`].
    pub dialect: crate::config::Dialect,
    /// `--config-var`s merged over a `--config-vars-file`'s values, which
    /// each task's values are evaluated with. Checked against the project's
    /// declared variables by [`TaskEngine::with_settings`].
    pub config_var_overrides: HashMap<String, String>,
}

impl Default for TaskEngineSettings {
    fn default() -> Self {
        Self {
            existing_network: None,
            publish_ports: true,
            propagate_proxy_environment_variables: true,
            run_prerequisites: true,
            image_overrides: HashMap::new(),
            image_tags: HashMap::new(),
            cleanup_after_success: true,
            cleanup_after_failure: true,
            max_parallelism: None,
            cache: None,
            ratect_version: None,
            dialect: crate::config::Dialect::default(),
            config_var_overrides: HashMap::new(),
        }
    }
}

/// Whether a project of this dialect runs its task container's readiness
/// gate — see `TaskEngine::task_container_readiness_gate`.
fn task_container_readiness_gate(dialect: crate::config::Dialect) -> bool {
    match dialect {
        crate::config::Dialect::BatectCompatible => true,
        crate::config::Dialect::Native => false,
    }
}

impl<D: ContainerRuntime + Send + Sync + 'static> TaskEngine<D> {
    /// `event_sink`/`interrupt` are constructor arguments, not opt-in
    /// settings, like `docker` itself — see their own field doc comments
    /// for why. Everything else opt-in still goes through [`with_settings`]
    /// (this module's own tests use struct-update syntax on
    /// [`TaskEngineSettings`] directly, naming only the one setting under
    /// test).
    ///
    /// [`with_settings`]: Self::with_settings
    pub fn new(
        project: LoadedProject,
        docker: D,
        event_sink: Arc<dyn EventSink>,
        interrupt: Arc<crate::interrupt::Interrupt>,
    ) -> Self {
        Self {
            project,
            config_var_overrides: HashMap::new(),
            docker: Arc::new(docker),
            executed_tasks: Mutex::new(HashSet::new()),
            pulled_images: Mutex::new(HashMap::new()),
            built_images: Mutex::new(HashMap::new()),
            in_flight_builds: Mutex::new(HashMap::new()),
            in_progress_tasks: Mutex::new(HashSet::new()),
            existing_network: None,
            publish_ports: true,
            propagate_proxy_environment_variables: true,
            host_env: Box::new(|name| std::env::var(name).ok()),
            proc_net_tcp: Box::new(crate::proxy::proc_net_tcp_tables),
            event_sink,
            skip_prerequisites: false,
            image_overrides: HashMap::new(),
            image_tags: HashMap::new(),
            containers_used: Mutex::new(HashSet::new()),
            cleanup_after_success: true,
            cleanup_after_failure: true,
            max_parallelism: None,
            cache_options: None,
            ratect_version: None,
            cache_key: std::sync::Mutex::new(None),
            interrupt,
            task_container_readiness_gate: task_container_readiness_gate(
                crate::config::Dialect::default(),
            ),
        }
    }

    /// Whether the selected output mode owns container I/O line by line
    /// (the `all` mode) — see `crate::ui::ContainerIoStreaming`.
    fn interleaved_output(&self) -> bool {
        self.event_sink.container_io_streaming() == crate::ui::ContainerIoStreaming::Interleaved
    }

    /// Applies a whole [`TaskEngineSettings`] at once, driven by plain data
    /// instead of a chain of `if flag { engine.field = ... }` at a call
    /// site.
    ///
    /// This exists for the binaries, which all have the same ~10 knobs
    /// behind differently-named flags; this module's own tests construct
    /// [`TaskEngineSettings`] with struct-update syntax instead (naming only
    /// the one setting under test) rather than calling this with a
    /// mostly-default struct. Anything added to [`TaskEngineSettings`] needs
    /// assigning here too, or a binary has no way to reach it.
    pub fn with_settings(mut self, settings: TaskEngineSettings) -> Result<Self> {
        let TaskEngineSettings {
            existing_network,
            publish_ports,
            propagate_proxy_environment_variables,
            run_prerequisites,
            image_overrides,
            image_tags,
            cleanup_after_success,
            cleanup_after_failure,
            max_parallelism,
            cache,
            ratect_version,
            dialect,
            config_var_overrides,
        } = settings;
        // Up front, so an override for a variable the project doesn't
        // declare is refused before any task runs rather than when the first
        // one is evaluated. Evaluating nothing reads no host value.
        self.project
            .evaluate([], [], &config_var_overrides, |_| None)?;
        self.config_var_overrides = config_var_overrides;
        self.existing_network = existing_network;
        self.publish_ports = publish_ports;
        self.propagate_proxy_environment_variables = propagate_proxy_environment_variables;
        self.skip_prerequisites = !run_prerequisites;
        self.image_overrides = image_overrides;
        self.image_tags = image_tags;
        self.cleanup_after_success = cleanup_after_success;
        self.cleanup_after_failure = cleanup_after_failure;
        self.max_parallelism =
            max_parallelism.map(|max| Arc::new(tokio::sync::Semaphore::new(max)));
        self.cache_options =
            cache.map(
                |(cache_type, project_directory)| crate::cache::CacheOptions {
                    cache_type,
                    project_directory,
                },
            );
        self.ratect_version = ratect_version;
        self.task_container_readiness_gate = task_container_readiness_gate(dialect);
        Ok(self)
    }

    /// Resolves a container's `volumes` into the [`BindMount`]s a
    /// [`ContainerSpec`](crate::container_spec::ContainerSpec) carries to
    /// `docker.rs`. `Local` mounts
    /// are already fully resolved (host path made absolute, interpolated) by
    /// `LoadedProject::evaluate` — nothing left to do here but carry them
    /// over as host mounts. `Cache` mounts are resolved here instead, since that needs
    /// `--cache-type` (see `with_settings`) and the project's own cache
    /// key, neither available to `config.rs`. `cache_key` is only ever
    /// computed the first time a `Cache` mount needs it — a project-scoped
    /// one under `CacheType::Volume` — so a config with none, or a
    /// directory-mode run, never touches the key file at all.
    /// `Tmpfs` mounts are skipped entirely here — they can't be expressed as
    /// a bind at all, and need no async resolution, so they're pulled
    /// out separately (and synchronously) by `container_spec::tmpfs_mounts`
    /// instead.
    ///
    /// A bind identical to an earlier one is dropped, keeping first-seen
    /// order: Batect holds a container's mounts as a set, and the daemon
    /// refuses a create naming one mount point twice (ratect#390). Compared
    /// after resolution, so `./a:/a` and `a:/a` collapse too; the same paths
    /// with different options don't, and the daemon refuses those as it does
    /// under Batect.
    async fn resolve_volumes(
        &self,
        volumes: Option<&Vec<crate::config::VolumeMount>>,
    ) -> Result<Option<Vec<BindMount>>> {
        let Some(volumes) = volumes else {
            return Ok(None);
        };

        let mut resolved = Vec::with_capacity(volumes.len());
        for volume in volumes {
            let bind = match volume {
                crate::config::VolumeMount::Local(local) => BindMount {
                    source: BindSource::Host(local.local.clone()),
                    container: local.container.clone(),
                    options: local.options.clone(),
                },
                crate::config::VolumeMount::Cache(cache) => {
                    let cache_options = self.cache_options.as_ref().expect(
                        "a config with a 'cache' volume mount requires with_settings to have \
                         been called with `cache: Some(..)` first",
                    );
                    let cache_key = || {
                        let mut memo = self.cache_key.lock().unwrap();
                        if let Some(key) = memo.as_ref() {
                            return Ok(key.clone());
                        }
                        let key =
                            crate::cache::project_cache_key(&cache_options.project_directory)?;
                        *memo = Some(key.clone());
                        Ok(key)
                    };
                    crate::cache::resolve_cache_mount(cache_options, cache_key, cache)?
                }
                crate::config::VolumeMount::Tmpfs(_) => continue,
            };
            if !resolved.contains(&bind) {
                resolved.push(bind);
            }
        }

        if resolved.is_empty() {
            Ok(None)
        } else {
            Ok(Some(resolved))
        }
    }

    /// Runs one container's `setup_commands` in declared order — the
    /// readiness gate's second step (see docs/dependency-readiness.md), shared
    /// by [`Self::run_task_container_readiness`] and the dependency gate
    /// inside [`Self::ensure_container_ready`]. Those two functions stay
    /// separate on purpose (a dependency's gate also handles
    /// `customise`/cache-key/dedup concerns the one task container has no
    /// use for), but this loop is identical between them, and `run_in`
    /// (ratect#111) is the sort of thing that would otherwise have to be
    /// written — and kept right — twice.
    ///
    /// Everything that *does* differ between the two callers arrives in
    /// [`SetupCommandContext`] — the working directory and environment from
    /// each caller's own derived spec, which carries the task's `run`
    /// overrides for the task container and its `customise` for a
    /// dependency — so none of it is recomputed here.
    async fn run_setup_commands(
        &self,
        setup_commands: &[crate::config::SetupCommand],
        context: &SetupCommandContext<'_>,
    ) -> Result<()> {
        let name = context.name;
        for (setup_command_index, setup_command) in setup_commands.iter().enumerate() {
            // Where the command actually runs. `run_in` naming the declaring
            // container itself is spelled out as valid (it is what omitting
            // the field means), so both spellings land on the same branch
            // rather than the explicit one taking a different path.
            let elsewhere = setup_command
                .run_in
                .as_deref()
                .filter(|target| *target != name);
            // The config loader has already refused any target outside the
            // declaring container's own dependencies
            // (`validate_setup_command_targets`), and every one of those has
            // been resolved to ready before this runs — so a miss here is a
            // bug in this file, not a user's configuration, and says so.
            let target = match elsewhere {
                Some(target) => {
                    let container_id = context.dependency_ids.get(target).with_context(|| {
                        format!(
                            "Setup command '{}' on container '{name}' names 'run_in' \
                                 container '{target}', which was not started as one of its \
                                 dependencies — the configuration loader should have refused \
                                 this, so this is a bug in Ratect",
                            setup_command.command
                        )
                    })?;
                    // The target container's own environment, user and
                    // working directory come with it — Docker's `exec`
                    // inherits all three from the container it runs in, so
                    // sending the *declaring* container's resolved values
                    // along would be actively wrong. The setup command's own
                    // `working_directory` still overrides.
                    ExecTarget {
                        container_id,
                        description: format!("'{target}' (a setup command of container '{name}')"),
                        working_directory: setup_command.working_directory.as_deref(),
                        environment: None,
                        user_mapping: None,
                    }
                }
                None => ExecTarget {
                    container_id: context.container_id,
                    description: format!("'{name}'"),
                    working_directory: setup_command
                        .working_directory
                        .as_deref()
                        .or(context.working_directory),
                    environment: context.environment,
                    user_mapping: context.user_mapping,
                },
            };

            // The user-facing setup-command line is the event sink's job now
            // (see `crate::ui`) — `debug` so `RUST_LOG=info` doesn't
            // duplicate it on stderr.
            tracing::debug!(
                container = name,
                command = setup_command.command.as_str(),
                "Running setup command"
            );
            self.event_sink.post(TaskEvent::RunningSetupCommand {
                container: name.to_string(),
                command: setup_command.command.clone(),
                index: setup_command_index + 1,
                total: setup_commands.len(),
                run_in: elsewhere.map(str::to_string),
            });
            let result = {
                let _permit = self.acquire_parallelism_permit().await;
                self.docker
                    .exec_in_container(
                        target.container_id,
                        &setup_command.command,
                        target.working_directory,
                        target.environment,
                        target.user_mapping,
                    )
                    .await
                    .with_context(|| {
                        NotReady(format!(
                            "Failed to run setup command '{}' in container {}",
                            setup_command.command, target.description
                        ))
                    })
                    .map_err(|error| {
                        Failed::tag(
                            error,
                            CleanupCause::SetupCommandNotRun {
                                container: name.to_string(),
                                index: setup_command_index + 1,
                            },
                        )
                    })?
            };
            // The command's output, line by line — exec output arrives
            // collected rather than streamed, so this posts after completion
            // (success or failure; a failure's output additionally lands in
            // the error below). Only the `all` output mode renders these —
            // skipped entirely otherwise (see
            // `EventSink::wants_progress_detail`) rather than allocating and
            // posting one event per line only to have every other mode
            // immediately discard it. Framed by the same `LineBuffer` as a
            // container's streamed output, so both follow one rule.
            if self.event_sink.wants_progress_detail() {
                let post = |line: &str| {
                    self.event_sink.post(TaskEvent::SetupCommandOutput {
                        container: name.to_string(),
                        index: setup_command_index + 1,
                        line: line.to_string(),
                    });
                };
                let mut lines = crate::ui::interleaved::LineBuffer::new();
                lines.push(result.output.as_bytes(), post);
                lines.flush(post);
            }
            if result.exit_code != 0 {
                let output = if result.output.trim().is_empty() {
                    ", and did not produce any output".to_string()
                } else {
                    format!(", with output:\n{}", result.output.trim())
                };
                return Err(Failed::tag(
                    NotReady(format!(
                        "Setup command '{}' in container {} exited with code {}{}",
                        setup_command.command, target.description, result.exit_code, output
                    ))
                    .into(),
                    CleanupCause::SetupCommandExited {
                        container: name.to_string(),
                        index: setup_command_index + 1,
                        exit_code: result.exit_code,
                    },
                ));
            }
        }
        if !setup_commands.is_empty() {
            self.event_sink.post(TaskEvent::SetupCommandsCompleted {
                container: name.to_string(),
            });
        }
        Ok(())
    }

    /// Sends the cancel of every build still in `in_flight_builds` — each
    /// one an abandoned build — and empties it. Each cancel runs as a task
    /// of its own, bounded by `BUILD_CANCEL_TIMEOUT`, so cleanup goes on
    /// removing things while the daemon answers: a run's cleanup has to beat
    /// a supervisor's `SIGKILL` (ratect#249), and a daemon slow to answer a
    /// cancel must not spend that time. Yields once, so each request has
    /// started before the first removal — started, not necessarily on the
    /// wire, which nothing depends on. The answers are for
    /// `report_build_cancels`.
    async fn start_cancelling_abandoned_builds(&self) -> Vec<(String, BuildCancel)> {
        let mut abandoned: Vec<(String, String)> =
            std::mem::take(&mut *self.in_flight_builds.lock().unwrap())
                .into_iter()
                .collect();
        abandoned.sort_by(|(_, a), (_, b)| a.cmp(b));
        let cancels = abandoned
            .into_iter()
            .map(|(build_id, container)| {
                let docker = Arc::clone(&self.docker);
                let cancel = tokio::spawn(async move {
                    tokio::time::timeout(BUILD_CANCEL_TIMEOUT, docker.cancel_build(&build_id)).await
                });
                (container, cancel)
            })
            .collect::<Vec<_>>();
        if !cancels.is_empty() {
            tokio::task::yield_now().await;
        }
        cancels
    }

    /// Waits for the answers `start_cancelling_abandoned_builds` sent for,
    /// until `rung` — the cleanup ladder's escalation, a signal since the
    /// one that ended the run: the lines cleanup ends with come after these,
    /// and a supervisor's `SIGKILL` follows its second signal by less than
    /// `BUILD_CANCEL_TIMEOUT` (ratect#249), so that signal stops the wait
    /// rather than cost those lines. A cancel that failed, or that the
    /// wait stopped for, leaves a build that may still be running, a warning
    /// naming its container; nothing the run created is left behind by it,
    /// so it is no cleanup failure.
    async fn report_build_cancels(
        &self,
        rung: impl std::future::Future<Output = ()>,
        mut cancels: Vec<(String, BuildCancel)>,
    ) {
        if cancels.is_empty() {
            return;
        }
        let answers = futures::future::join_all(cancels.iter_mut().map(|(_, cancel)| cancel));
        let answers = self.until_interrupted(rung, answers).await;
        let answers: Vec<Option<_>> = match answers {
            Some(answers) => answers.into_iter().map(Some).collect(),
            None => cancels
                .iter_mut()
                .map(|(_, cancel)| futures::FutureExt::now_or_never(cancel))
                .collect(),
        };
        for ((container, _), answer) in cancels.iter().zip(answers) {
            let reason = match answer {
                Some(Ok(Ok(Ok(())))) => continue,
                Some(Ok(Ok(Err(error)))) => format!("{error:#}"),
                Some(Ok(Err(_))) => format!(
                    "the Docker daemon didn't answer within {} seconds",
                    BUILD_CANCEL_TIMEOUT.as_secs()
                ),
                Some(Err(error)) => format!("{error}"),
                None => "cleanup stopped waiting for the Docker daemon's answer".to_string(),
            };
            self.event_sink.post(TaskEvent::Warning {
                message: format!(
                    "Couldn't cancel the image build for '{container}', so it may still be \
                     running in the Docker daemon: {reason}"
                ),
            });
        }
    }

    /// Runs one cleanup step, giving up on it if `rung` resolves first.
    ///
    /// `None` means the step lost the race and was dropped mid-flight —
    /// whatever it was removing may still be there. `rung` is one of the
    /// cleanup ladder's waits on the interrupt tracker (see
    /// `run_task_internal`).
    ///
    /// An interrupt nothing has recorded on behaves as though there were no
    /// tracker at all — its waits never resolve, so `step` always wins —
    /// which is every unit test that doesn't deliberately record one, and
    /// both binaries before 0.25.0.
    async fn until_interrupted<T>(
        &self,
        rung: impl std::future::Future<Output = ()>,
        step: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        tokio::select! {
            biased;
            output = step => Some(output),
            () = rung => None,
        }
    }

    /// The task's own container's readiness gate, run only under the
    /// Batect-compatible dialect (see `task_container_readiness_gate`; the
    /// native dialect has none): waits for it to report healthy, then runs
    /// its `setup_commands` in order — the same gate `ensure_container_ready`
    /// applies to a dependency, ported here almost unchanged (not shared
    /// directly, since a dependency's version also handles
    /// `customise`/cache-key/dedup concerns that don't apply to the one,
    /// always-present task container). The caller runs this concurrently
    /// with `run_container`'s own attach-and-wait-for-exit, cancels the
    /// main command if this fails while it is still running, and drops or
    /// forgives this once the main command has exited — see decisions/0012
    /// for why.
    ///
    /// `container_id` must already be running: the caller waits for
    /// `run_container`'s own `started` signal before calling here, since
    /// both steps need a running container (a health status only appears
    /// once one exists, and `docker exec` refuses anything else).
    async fn run_task_container_readiness(
        &self,
        container_id: &str,
        name: &str,
        setup_commands: &[crate::config::SetupCommand],
        spec: &crate::container_spec::ContainerSpec,
        dependency_ids: &HashMap<String, String>,
    ) -> Result<()> {
        self.docker
            .wait_for_container_healthy(container_id)
            .await
            .with_context(|| NotReady(format!("Container '{}' did not become healthy", name)))
            .map_err(|error| {
                Failed::tag(
                    error,
                    CleanupCause::NotHealthy {
                        container: name.to_string(),
                    },
                )
            })?;
        self.event_sink.post(TaskEvent::ContainerBecameHealthy {
            container: name.to_string(),
        });

        self.run_setup_commands(
            setup_commands,
            &SetupCommandContext {
                name,
                container_id,
                working_directory: spec.shared.options.working_directory.as_deref(),
                environment: spec.shared.environment.as_ref(),
                user_mapping: spec.shared.user_mapping.as_ref(),
                dependency_ids,
            },
        )
        .await
    }

    #[cfg(test)]
    fn with_host_env(
        mut self,
        host_env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.host_env = Box::new(host_env);
        self
    }

    #[cfg(test)]
    fn with_proc_net_tcp(
        mut self,
        proc_net_tcp: impl Fn() -> Vec<String> + Send + Sync + 'static,
    ) -> Self {
        self.proc_net_tcp = Box::new(proc_net_tcp);
        self
    }

    /// The proxy configuration to apply to a container or build in this
    /// task, or `None` when propagation is disabled (`--no-proxy-vars`) or
    /// the host environment has none set — an empty map is normalized to
    /// `None` here so `merged_environment`'s "`None` only when nothing at
    /// all is set" behavior isn't disturbed by an empty-but-`Some` map.
    ///
    /// Carries the host-gateway decision along with the variables rather
    /// than leaving each call site to infer it, since every site that
    /// injects the variables must also add the entry that makes them work —
    /// see `proxy::ProxyEnvironment`.
    fn proxy_environment(
        &self,
        extra_no_proxy_entries: &std::collections::BTreeSet<String>,
    ) -> Option<crate::proxy::ProxyEnvironment> {
        if !self.propagate_proxy_environment_variables {
            return None;
        }
        let host_env = |name: &str| (self.host_env)(name);
        let proxy = crate::proxy::proxy_environment_variables(host_env, extra_no_proxy_entries);
        (!proxy.variables.is_empty()).then_some(proxy)
    }

    /// The ports of this run's rewritten proxy URLs that the host is
    /// listening on through loopback only — everything
    /// `warn_about_unreachable_proxies` decides, separated from the warning
    /// itself so the decision is assertable without capturing log output.
    ///
    /// Empty when propagation is off (`--no-proxy-vars`), when no URL was
    /// rewritten, and on any platform without `/proc/net/tcp` — see
    /// `proxy::proc_net_tcp_tables`.
    fn unreachable_proxy_ports(&self) -> std::collections::BTreeSet<u16> {
        let Some(proxy) = self.proxy_environment(&std::collections::BTreeSet::new()) else {
            return std::collections::BTreeSet::new();
        };
        crate::proxy::loopback_only_ports_in(&(self.proc_net_tcp)(), proxy.rewritten_ports())
    }

    /// Whether `task_name` has already run to completion this invocation —
    /// `run_task_scoped` would return at once, starting nothing.
    fn has_run(&self, task_name: &str) -> bool {
        self.executed_tasks.lock().unwrap().contains(task_name)
    }

    /// Ends the run before `next` starts if a signal has arrived since
    /// `baseline` — reported by a notice naming the task that won't start,
    /// since nothing else says so: an escalation notice from the cleanup the
    /// signal hurried says only what happened to that cleanup, and `main`
    /// prints no `Error:` for a signalled run (decisions/0017).
    fn stop_if_signalled(&self, baseline: usize, next: &str) -> Result<()> {
        if self.interrupt.count() <= baseline {
            return Ok(());
        }
        let signal = self.interrupt.last_signal();
        self.post_notice(signal.not_starting(next));
        Err(crate::interrupt::TaskInterrupted::new(signal).into())
    }

    /// Posts `message` as a notice — printed on stderr in every style, never
    /// logged (decisions/0017).
    fn post_notice(&self, message: String) {
        self.event_sink.post(TaskEvent::Notice { message });
    }

    /// Warns, once per invocation, about every rewritten proxy URL whose
    /// port the host is listening on through loopback only.
    ///
    /// Rewriting the URL is all Ratect can do; a proxy bound to `127.0.0.1`
    /// stays unreachable from a container whatever the URL says, and the
    /// failure it produces mid-build ("connection refused" from a package
    /// manager) points nowhere near the cause. This is a warning and not a
    /// failure because a run may never use the proxy, and `--no-proxy-vars`
    /// already turns propagation off for one that shouldn't.
    fn warn_about_unreachable_proxies(&self) {
        for port in self.unreachable_proxy_ports() {
            self.event_sink.post(TaskEvent::Warning {
                message: format!(
                    "The proxy on port {port} is listening on loopback addresses only, so \
                     containers in this run cannot reach it even though its URL now names the \
                     host. Bind the proxy to 0.0.0.0 to make it reachable — which also exposes \
                     it to anything else that can reach this machine, so do that only on a \
                     network you trust. Use --no-proxy-vars if this run doesn't need the proxy."
                ),
            });
        }
    }

    /// The `TERM` to inject into a container's environment — `dumb` under
    /// the interleaved I/O policy (the `all` output mode), matching Batect's
    /// `InterleavedContainerIOStreamingOptions`, and otherwise the host's own
    /// value, or `None` when the host has none. The same for every container
    /// a task run creates — the task's own, a prerequisite's, a dependency's
    /// — matching Batect's
    /// `TaskContainerOnlyIOStreamingOptions.terminalTypeForContainer`, which
    /// ignores the container it is given. Never applied to an image build,
    /// which Batect gives no `TERM` either.
    ///
    /// Not gated on interactive eligibility, nor on whether a real TTY ends
    /// up being allocated (decided later, inside
    /// `ContainerRuntime::run_container`) — Batect's `ConsoleInfo.terminalType`
    /// is unconditional on any TTY check too. So a full-screen terminal
    /// program inside the container knows the terminal type even when piping
    /// output elsewhere still lets it detect it isn't attached to a real TTY.
    fn term_environment_variable(&self) -> Option<HashMap<String, String>> {
        if self.interleaved_output() {
            return Some(dumb_term_environment());
        }
        let term = (self.host_env)("TERM")?;
        Some(HashMap::from([("TERM".to_string(), term)]))
    }

    /// Acquires a permit from `max_parallelism`'s semaphore, if configured —
    /// `None` (a no-op) when `--max-parallelism` wasn't given, unbounded as
    /// before. Every call site holds this only for the duration of the one
    /// actual Docker-facing operation it wraps (a pull, a build, a create,
    /// a start, one setup-command exec) — see `max_parallelism`'s own
    /// doc comment for exactly which operations that is, and why each
    /// acquire/release is scoped narrowly rather than held across a whole
    /// container's readiness sequence (nesting two acquisitions from the
    /// same semaphore in one call chain would deadlock under a cap of 1).
    async fn acquire_parallelism_permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        match &self.max_parallelism {
            Some(semaphore) => Some(
                semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("max_parallelism semaphore is never closed"),
            ),
            None => None,
        }
    }

    /// Pulls `image` under `policy` (deduped by image name and policy across
    /// the whole invocation via `pulled_images` — see `resolve_image`),
    /// returning the ID of the image to run: the one the pull fetched, or
    /// under `IfNotPresent` the local one the lookup found — never `image`
    /// itself, a tag that could be moved before the container is created, as
    /// Batect runs a pulled image by ID too (ratect#301). Shared by
    /// `resolve_image`'s two pull-shaped callers: a container's own
    /// configured `image`, and an `--override-image` replacement (always
    /// `IfNotPresent`, never the container's own configured policy).
    async fn resolve_pulled_image(
        &self,
        image: &str,
        policy: crate::config::ImagePullPolicy,
    ) -> Result<String> {
        let cell = get_or_create_cell(&self.pulled_images, (image.to_string(), policy));
        let result = cell
            .get_or_init(|| async {
                let outcome: Result<String> = async {
                    if policy == crate::config::ImagePullPolicy::IfNotPresent {
                        if let Some(id) = self.docker.local_image_id(image).await? {
                            return Ok(id);
                        }
                    }
                    // Milestones post only when a pull actually happens —
                    // a skip (image already local under `IfNotPresent`)
                    // stays silent. Batect posts both even then; see
                    // docs/differences-from-batect.md.
                    self.event_sink.post(TaskEvent::ImagePullStarting {
                        image: image.to_string(),
                    });
                    let _permit = self.acquire_parallelism_permit().await;
                    let id = self.docker.pull_image(image).await?;
                    self.event_sink.post(TaskEvent::ImagePullCompleted {
                        image: image.to_string(),
                    });
                    Ok(id)
                }
                .await;
                outcome.map_err(Arc::new)
            })
            .await;

        unshare(result)
    }

    /// Every `--override-image` must name a container the config declares,
    /// in Batect's words.
    fn check_image_overrides(&self) -> Result<()> {
        match self
            .image_overrides
            .keys()
            .find(|name| !self.project.config.containers.contains_key(*name))
        {
            Some(name) => anyhow::bail!(
                "Cannot override image for container '{name}' because there is no container \
                 named '{name}' defined."
            ),
            None => Ok(()),
        }
    }

    /// `--tag-image` only ever makes sense for a *built* image — errors
    /// immediately (rather than silently ignoring the tag request) the
    /// moment a tagged container name turns out to resolve via a pull
    /// instead. For a container whose own configured `image` pulls, this
    /// matches Batect's `ContainerUsesPulledImageException` message exactly,
    /// but not its timing: Batect's `ImageTaggingValidator` raises it once
    /// the task using the container has finished, where this fails before
    /// that container runs. For an `--override-image` replacement, both
    /// binaries refuse the pair while parsing arguments, as Batect does
    /// ([`check_tag_and_override`]), so only a direct `ratect-core` caller
    /// building conflicting [`TaskEngineSettings`] reaches this with one.
    fn reject_tagged_pulled_image(&self, container_name: &str) -> Result<()> {
        if self.image_tags.contains_key(container_name) {
            anyhow::bail!(
                "The image built for container '{container_name}' was requested to be tagged \
                 with --tag-image, but '{container_name}' uses a pulled image."
            );
        }
        Ok(())
    }

    /// Resolves `container_config`'s `image` (pulling it, deduped by image
    /// name and pull policy) or `build_directory` (building it, deduped by
    /// `container_name`) into the image reference to actually run. Shared by
    /// a task's own container and its dependency containers — both need
    /// exactly this and nothing else, which is also why dependency containers
    /// now support `build_directory` (they didn't before this was unified).
    ///
    /// `image`'s `image_pull_policy` (`IfNotPresent` by default, matching
    /// Batect) decides whether a pull actually reaches the registry the
    /// first time an image name is seen under that policy this session:
    /// `IfNotPresent` skips it entirely when
    /// `ContainerRuntime::local_image_id` already finds it; `Always` never
    /// checks. Either way, once decided for a given image name and policy,
    /// later containers reusing that same pair within this session reuse
    /// the decision rather than re-checking or re-pulling.
    ///
    /// Built images are tagged `<project_name>-<container_name>` — the same
    /// convention Batect uses — so `docker images` shows something a user can
    /// actually identify, rather than an opaque generated name. That tag is
    /// human-facing only, though: what this returns (and what `run_container`/
    /// `start_background_container` are actually given) is the image *ID*
    /// `ContainerRuntime::build_image` reports back from the build, not the
    /// tag string. This matters because the tag isn't unique — two
    /// *overlapping* `ratect` invocations (e.g. two checkouts of the same
    /// project, or two projects that happen to share a name) could race to
    /// retag the same name, and a Docker tag is a mutable pointer. Resolving
    /// by ID sidesteps that race entirely: whichever image this process just
    /// built is the one it runs, regardless of what the tag currently points
    /// to by the time the container actually starts. A pulled image is run
    /// by ID for the same reason (see `resolve_pulled_image`).
    async fn resolve_image(
        &self,
        container_name: &str,
        container_config: &Container,
    ) -> Result<String> {
        // Recorded unconditionally, regardless of pull/build dedup — see
        // `containers_used`'s own doc comment for why.
        self.containers_used
            .lock()
            .unwrap()
            .insert(container_name.to_string());

        // `--override-image` wholesale replaces whatever the container
        // actually configures (`image` *or* `build_directory`, and that
        // configured `image`'s own `image_pull_policy`) with a plain pull of
        // the override value under the default `IfNotPresent` policy —
        // matching Batect's `TaskSpecialisedConfigurationFactory`, which
        // replaces the container's entire `imageSource` with a fresh
        // `PullImage(value)` rather than patching the existing one. A build
        // is never attempted for an overridden container, even if
        // `build_directory` is set.
        if let Some(image) = self.image_overrides.get(container_name) {
            self.reject_tagged_pulled_image(container_name)?;
            return self
                .resolve_pulled_image(image, crate::config::ImagePullPolicy::IfNotPresent)
                .await;
        }

        if let Some(image) = &container_config.image {
            self.reject_tagged_pulled_image(container_name)?;
            let policy = container_config.image_pull_policy.unwrap_or_default();
            self.resolve_pulled_image(image, policy).await
        } else if let Some(build_directory) = &container_config.build_directory {
            let cell = get_or_create_cell(&self.built_images, container_name.to_string());
            let result = cell
                .get_or_init(|| async {
                    let build: Result<String> = async {
                        let project_name = &self.project.config.project_name;
                        let tag = format!("{project_name}-{container_name}");
                        let mut extra_tags: Vec<String> = self
                            .image_tags
                            .get(container_name)
                            .map(|tags| tags.iter().cloned().collect())
                            .unwrap_or_default();
                        extra_tags.sort();
                        // Every tag is checked before anything is built, as
                        // Batect checks its build request's, so a bad one
                        // costs no build and leaves no tag half-applied.
                        crate::image_reference::check(&tag).map_err(|reason| {
                            anyhow::anyhow!(
                                "'{tag}', the tag made from project_name '{project_name}' \
                                 and container name '{container_name}', is not a valid \
                                 image tag: {reason}"
                            )
                        })?;
                        for extra_tag in &extra_tags {
                            crate::image_reference::check_tag(extra_tag)
                                .map_err(anyhow::Error::msg)?;
                        }
                        // And so is everything the build reads on the host,
                        // so a Dockerfile that can't work is refused naming
                        // it, before the daemon is asked anything.
                        let dockerfile = crate::container_spec::check_build_files(
                            std::path::Path::new(build_directory),
                            container_config.dockerfile.as_deref(),
                        )
                        .map_err(anyhow::Error::msg)?;
                        // No `extra_no_proxy_entries` at build time — matches
                        // Batect, which never adds container names to
                        // `no_proxy` for a build (nothing's running yet to be
                        // exempted from proxying).
                        let proxy = self.proxy_environment(&std::collections::BTreeSet::new());
                        let spec = derive_build_spec(
                            container_config,
                            build_directory,
                            dockerfile,
                            &tag,
                            proxy.as_ref(),
                            self.propagate_proxy_environment_variables,
                        )?;
                        self.event_sink.post(TaskEvent::ImageBuildStarting {
                            container: container_name.to_string(),
                        });
                        let _permit = self.acquire_parallelism_permit().await;
                        let build_id = uuid::Uuid::new_v4().to_string();
                        self.in_flight_builds
                            .lock()
                            .unwrap()
                            .insert(build_id.clone(), container_name.to_string());
                        let built = self.docker.build_image(&spec, &build_id).await;
                        self.in_flight_builds.lock().unwrap().remove(&build_id);
                        let image_id = built?;
                        self.event_sink.post(TaskEvent::ImageBuildCompleted {
                            container: container_name.to_string(),
                        });
                        // `--tag-image`: applied once here (inside this
                        // cell's do-once build), never re-applied on a later
                        // cache hit for the same container this invocation.
                        // Every tag is attempted and every refusal reported
                        // (decisions/0018): each has passed the grammar check
                        // above, so a failure here is the daemon's, and one
                        // shouldn't hide the next.
                        let mut refused = Vec::new();
                        for extra_tag in &extra_tags {
                            if let Err(error) = self.docker.tag_image(&image_id, extra_tag).await {
                                refused.push(format!("{error:#}"));
                            }
                        }
                        if !refused.is_empty() {
                            anyhow::bail!(
                                "The image was built, but applying its tags failed:\n  {}",
                                refused.join("\n  ")
                            );
                        }
                        Ok(image_id)
                    }
                    .await;
                    let outcome = build
                        // One place attributes everything this build can fail
                        // on, rather than each error site remembering to. The
                        // failures come from three layers with three
                        // vocabularies — an ssh agent id (`build_ssh`
                        // classification), a key file path (the keyring), an
                        // image tag (Docker itself) — and none of them knows
                        // which container the user has to go and edit. Adding
                        // it per-site is what left two adjacent `build_ssh`
                        // errors disagreeing about whether they named one.
                        .with_context(|| {
                            format!("Failed to build the image for container '{container_name}'")
                        });
                    outcome.map_err(Arc::new)
                })
                .await;

            unshare(result)
        } else {
            Err(anyhow::anyhow!(
                "Container '{}' has neither 'image' nor 'build_directory' set",
                container_name
            ))
        }
    }

    /// `resolve_image` for `name`, memoized per task execution in `cells`
    /// (one [`ReadyCell`] per container, same shape as `ensure_container_ready`'s)
    /// and followed by its `ImageResolved` milestone — unless `name` is a
    /// readiness companion, which narrates nothing of its own (ratect#202).
    ///
    /// Memoized because it is reached twice: `run_task_internal` fans out
    /// over every container in the task's graph the moment the network is
    /// ready, so every pull and build starts at once (decisions/0019), and
    /// each container then awaits its own cell again at the point it is
    /// about to be created — a hit when the fan-out got there first, the
    /// same in-flight work otherwise. One mechanism for both, so no call
    /// site can resolve an image a second time or post the milestone twice.
    async fn ensure_image_resolved(
        &self,
        name: &str,
        containers: &HashMap<String, Container>,
        cells: &Mutex<HashMap<String, ReadyCell>>,
    ) -> Result<String> {
        let cell = get_or_create_cell(cells, name.to_string());
        let result = cell
            .get_or_init(|| async {
                let outcome: Result<String> = async {
                    let container_config = containers
                        .get(name)
                        .with_context(|| undeclared_container_bug(name))?;
                    let image =
                        self.resolve_image(name, container_config)
                            .await
                            .map_err(|error| {
                                let built = container_config.build_directory.is_some();
                                Failed::tag(
                                    error,
                                    Failed::of_container(
                                        name,
                                        container_config.reports_readiness_for.as_deref(),
                                        |container| match built {
                                            true => CleanupCause::ImageBuildFailed { container },
                                            false => CleanupCause::ImagePullFailed { container },
                                        },
                                    ),
                                )
                            })?;
                    if container_config.reports_readiness_for.is_none() {
                        self.event_sink.post(TaskEvent::ImageResolved {
                            container: name.to_string(),
                        });
                    }
                    Ok(image)
                }
                .await;
                outcome.map_err(Arc::new)
            })
            .await;

        unshare(result)
    }

    /// `None` unless `container_config.run_as_current_user` is enabled — in
    /// which case, resolves the actual host user to map the container onto.
    /// Applies per-container, matching Batect: a task's own container and
    /// each of its dependencies set this independently, so this is called
    /// from both `run_task_internal` and `start_dependency` rather than
    /// once per task. No caching — there's only ever one real host user per
    /// process, so recomputing it per call is cheap and simpler than adding
    /// a memoization layer for no real benefit.
    async fn resolve_user_mapping(
        &self,
        container_config: &Container,
    ) -> Result<Option<crate::docker::UserMapping>> {
        let Some(run_as_current_user) = &container_config.run_as_current_user else {
            return Ok(None);
        };
        if !run_as_current_user.enabled {
            return Ok(None);
        }

        let user = crate::user::current_user()?;
        let home_directory = run_as_current_user
            .home_directory
            .clone()
            .expect("validated non-None when the config loads, when enabled is true");

        // A cache mount's container path — the volume behind it is created
        // root-owned, so without this the mapped user cannot write to it.
        let cache_directories = container_config
            .volumes
            .iter()
            .flatten()
            .filter_map(|volume| match volume {
                // A read-only cache can't be chowned: the put-archive would
                // hit the read-only bind and abort the run before the task
                // starts, where it previously worked. Nothing needs to write
                // to it either, which is the whole point of `ro`. Batect
                // doesn't skip it, so its run fails there instead.
                crate::config::VolumeMount::Cache(cache) if !is_read_only(&cache.options) => {
                    Some(cache.container.clone())
                }
                _ => None,
            })
            .collect();

        Ok(Some(crate::docker::UserMapping {
            user,
            home_directory,
            cache_directories,
        }))
    }

    /// `additional_args` are only ever forwarded to the container run for
    /// exactly the task named here — not to any of its prerequisites, which
    /// always run with no additional args, matching Batect's behavior of
    /// scoping `-- ARGS` to the task named on the command line.
    ///
    /// Checks the whole prerequisite order up front (`check_prerequisites`),
    /// then runs `run_task_scoped` with `top_level` fixed to `true` — the
    /// only externally-visible entry point (called once from `main.rs`), so
    /// it's always the task actually named on the command line.
    ///
    /// The `--tag-image` "did every tagged container actually run"
    /// validation happens here, once, only after the whole task (and every
    /// prerequisite) has completed successfully — matching Batect's
    /// `SessionRunner`, which only ever reaches its own equivalent check
    /// once every task in the run has exited zero; any failure short-
    /// circuits before it's ever consulted, same as the early `?` here.
    pub async fn run_task(&self, task_name: &str, additional_args: &[String]) -> Result<()> {
        // Once per invocation, not once per container: the answer is the
        // same for every container in the run, and repeating it for each
        // would bury the run's own output under it.
        self.warn_about_unreachable_proxies();
        check_prerequisites(
            &self.project.config.tasks,
            task_name,
            self.skip_prerequisites,
        )?;
        self.run_task_scoped(task_name, additional_args, true)
            .await?;

        let containers_used = self.containers_used.lock().unwrap();
        let mut untagged: Vec<String> = self
            .image_tags
            .keys()
            .filter(|name| !containers_used.contains(*name))
            .cloned()
            .collect();
        drop(containers_used);
        if !untagged.is_empty() {
            untagged.sort();
            let quoted: Vec<String> = untagged.iter().map(|name| format!("'{name}'")).collect();
            if quoted.len() == 1 {
                anyhow::bail!(
                    "The image for container {} was requested to be tagged with --tag-image, \
                     but this container did not run as part of the task or its prerequisites.",
                    quoted[0]
                );
            } else {
                anyhow::bail!(
                    "The images for containers {} were requested to be tagged with --tag-image, \
                     but these containers did not run as part of the task or its prerequisites.",
                    human_readable_list(&quoted, "and")
                );
            }
        }

        Ok(())
    }

    /// `top_level` is `true` only for the task actually named on the command
    /// line, `false` for every prerequisite (however deeply nested). It
    /// scopes `--skip-prerequisites` to the named task and keeps a
    /// prerequisite out of `--no-cleanup-after-success` — not interactive
    /// eligibility, which every task's own container has, as in Batect,
    /// whose task container is per task, not per run (see
    /// `run_task_internal`).
    // `async_recursion` adds a bare `#[must_use]` to a function returning a
    // boxed future, which is already `#[must_use]` — Clippy 1.99's
    // `double_must_use` flags that macro output, not anything written here.
    #[allow(clippy::double_must_use)]
    #[async_recursion]
    async fn run_task_scoped(
        &self,
        task_name: &str,
        additional_args: &[String],
        top_level: bool,
    ) -> Result<()> {
        {
            let executed = self.executed_tasks.lock().unwrap();
            if executed.contains(task_name) {
                return Ok(());
            }
        }

        {
            let mut in_progress = self.in_progress_tasks.lock().unwrap();
            // A backstop: `check_prerequisites` has already refused every
            // cycle before anything ran.
            if in_progress.contains(task_name) {
                return Err(task_cycle(task_name));
            }
            in_progress.insert(task_name.to_string());
        }

        let result = self
            .run_task_internal(task_name, additional_args, top_level)
            .await;

        {
            let mut in_progress = self.in_progress_tasks.lock().unwrap();
            in_progress.remove(task_name);
        }

        if result.is_ok() {
            let mut executed = self.executed_tasks.lock().unwrap();
            executed.insert(task_name.to_string());
        }

        result
    }

    /// `task` and every container in its graph,
    /// [evaluated](LoadedProject::evaluate) with this run's config-variable
    /// overrides against the host environment — what the rest of the task's
    /// run reads its values from. A task with no `run` has no graph, and only
    /// its own values are evaluated.
    fn evaluate_task(
        &self,
        task_name: &str,
        task: &Task,
    ) -> Result<(Task, HashMap<String, Container>)> {
        let names = task
            .run
            .as_ref()
            .map(|run| {
                container_names_in_task(
                    |name| self.project.config.containers.get(name),
                    &run.container,
                    task.dependencies.as_deref(),
                )
            })
            .unwrap_or_default();
        let Evaluated {
            containers,
            mut tasks,
        } = self.project.evaluate(
            names.iter().map(String::as_str),
            [task_name],
            &self.config_var_overrides,
            |name| (self.host_env)(name),
        )?;
        let task = tasks
            .remove(task_name)
            .expect("evaluate returns every task it is asked for");
        Ok((task, containers))
    }

    async fn run_task_internal(
        &self,
        task_name: &str,
        additional_args: &[String],
        top_level: bool,
    ) -> Result<()> {
        let task = find_task(&self.project.config.tasks, task_name, top_level)?;

        // Run prerequisites (never with additional args, which are scoped to
        // only the originally-requested task). A `*`-wildcard entry is expanded
        // against the full task list first — see
        // `expand_prerequisite_wildcards` — then run through the same
        // sequential loop as any other prerequisite; its dedup (see
        // `run_task_scoped`) already collapses a name reached more than once
        // (e.g. named explicitly *and* matched by a wildcard) to a single
        // actual run. Every name here exists and no cycle is reachable:
        // `check_prerequisites` proved both before the run began.
        //
        // Skipped entirely when `--skip-prerequisites` was given and this is
        // the top-level task — never for a prerequisite task itself, which
        // always runs its own prerequisites regardless (matching Batect: the
        // flag only ever names the one task given on the command line).
        let interrupts_before_prerequisites = self.interrupt.count();
        if !(top_level && self.skip_prerequisites) {
            if let Some(prerequisites) = &task.prerequisites {
                let prerequisites =
                    expand_prerequisite_wildcards(&self.project.config.tasks, prerequisites)?;
                for prerequisite in &prerequisites {
                    // A signal that landed during an earlier prerequisite's
                    // cleanup — or just after it — left that one passed, but
                    // ends the run: this one never starts, so nothing is
                    // created only to be torn down again (decisions/0016,
                    // rule 2). Checked before each prerequisite that would
                    // actually run, not only before the task itself, or it
                    // starts, is interrupted at once and reports the same
                    // signal a second time. One that already ran this
                    // invocation starts nothing, so it is no reason to stop.
                    if !self.has_run(prerequisite) {
                        self.stop_if_signalled(interrupts_before_prerequisites, prerequisite)?;
                    }
                    self.run_task_scoped(prerequisite, &[], false).await?;
                }
            }
        }

        // This task's own values, evaluated only now that its prerequisites
        // have run, and before anything of its own touches Docker
        // (decisions/0014): a prerequisite that fails reports its own
        // failure, never this task's bad value.
        let (task, containers) = self.evaluate_task(task_name, task)?;

        // A task with only `prerequisites` and no `run` of its own — those
        // have already executed above; there's no container of the task's
        // own left to run. Stops here rather than treating this as an error,
        // and says so as Batect's `TaskRunner` does: the same sentence, on
        // the progress console, in every style but `quiet` — which prints no
        // progress, where Batect prints it anyway (ratect#364).
        let Some(run) = &task.run else {
            tracing::debug!("Task '{}' has only prerequisites", task_name);
            self.event_sink.post(TaskEvent::PrerequisitesOnly {
                task: task_name.to_string(),
            });
            return Ok(());
        };

        // Batect looks each override up when it specialises a task with a
        // `run`, before that task starts, so the first such task fails, and
        // a run that reaches none doesn't look (ratect#453).
        self.check_image_overrides()?;

        // The same check before the task's own run. A prerequisite-only task
        // never gets here: once its last prerequisite has passed, nothing is
        // left to start, so a signal during that one's cleanup ended
        // nothing — exactly as one during a passing run's own cleanup ends
        // nothing.
        self.stop_if_signalled(interrupts_before_prerequisites, task_name)?;

        // Run the task itself
        let container_config = containers
            .get(&run.container)
            .with_context(|| undeclared_container_bug(&run.container))?;

        // Up-front cycle check (see `build_dependency_graph`): before the
        // task starts and before any Docker call — the network included — as
        // Batect builds its graph before `onTaskStarting`, so a cycle prints
        // only the error and creates nothing. Proving the graph acyclic before
        // any concurrent execution starts is also why `ensure_container_ready`
        // needs no runtime cycle guard of its own. Startup walks this graph,
        // and cleanup stops containers in its reverse order.
        let dependency_graph =
            build_dependency_graph(&containers, &run.container, task.dependencies.as_deref())?;

        // The user-facing "Running <task>..." line is the event sink's job
        // now (see `crate::ui`) — this stays at `debug` so `RUST_LOG=info`
        // doesn't duplicate it on stderr.
        tracing::debug!("Running task '{}'", task_name);
        self.event_sink.post(TaskEvent::TaskStarting {
            task: task_name.to_string(),
        });
        let task_started_at = std::time::Instant::now();

        // The task-scoped network's name, resolved *inside* the `result`
        // block below (not here) so a validation/creation failure is
        // reported through the same `TaskFailed`/error path as every other
        // infrastructure failure, instead of an early `?`-return that would
        // skip it — see the `TaskEvent::TaskFailed` doc comment's contract.
        // `None` here means "not resolved" (either not attempted yet, or
        // resolution failed) — the cleanup section below only ever removes
        // a network it can see was actually created.
        let network_name_cell: Mutex<Option<String>> = Mutex::new(None);

        // Every container Docker creates for this run, registered the
        // moment `create_container` hands its id back — see
        // `CreatedContainers`. Cleanup removes exactly this set.
        let created_containers: CreatedContainers = Mutex::new(HashMap::new());
        let pending_creates: PendingCreates<'_> = Mutex::new(Vec::new());
        // Every dependency-exit watcher `ensure_container_ready` spawns
        // once a dependency reaches `Ready` (see
        // `TaskEvent::DependencyExitedUnexpectedly`), collected so they can
        // all be aborted the moment this run's own main body finishes —
        // strictly before cleanup ever stops a container, so a deliberate
        // stop is never misreported as one of these.
        let dependency_watchers: Mutex<Vec<tokio::task::JoinHandle<()>>> = Mutex::new(Vec::new());
        // See `TaskRunContext::started_containers`.
        let started_containers: Mutex<std::collections::HashSet<String>> = Mutex::default();
        // Memoizes each container's creation and readiness futures for this
        // one task execution — see `ensure_container_created`/
        // `ensure_container_ready`. Reset per task (unlike
        // `pulled_images`/`built_images`, which persist for the whole
        // invocation): a dependency is deliberately re-created fresh for
        // every task that uses it — see docs/task-lifecycle.md's "Cross-task
        // isolation".
        let created_cells: Mutex<HashMap<String, CreatedCell>> = Mutex::new(HashMap::new());
        let ready_cells: Mutex<HashMap<String, ReadyCell>> = Mutex::new(HashMap::new());
        // The same, for each container's image — see `ensure_image_resolved`.
        // Per task like `ready_cells` (the pull and build dedup underneath
        // is what persists for the invocation), so a later task's
        // `ImageResolved` milestones post again, as its startup is narrated
        // afresh.
        let image_cells: Mutex<HashMap<String, ReadyCell>> = Mutex::new(HashMap::new());
        // Fixed for the whole task, computed once up front — every
        // container started for this task (the task's own and each
        // dependency) gets the same `no_proxy` exemption list, matching
        // Batect's `allContainersInNetwork` being fixed for the whole graph
        // rather than recomputed per container.
        let no_proxy_entries = container_names_in_task(
            |name| containers.get(name),
            &run.container,
            task.dependencies.as_deref(),
        );
        // Identifies every resource this one task execution creates (see
        // `crate::labels`). The id is generated here rather than alongside
        // the network below, so it exists even under `--use-network`, where
        // no network is created at all — the containers still need to agree
        // on which run they belong to.
        let run_id = Uuid::new_v4().to_string();
        let run_labels = crate::labels::RunLabels::new(
            &self.project.config.project_name,
            task_name,
            &run_id,
            self.ratect_version.as_deref(),
        );
        // The Docker names this run creates its network and containers
        // under — one suffix for the lot, as Batect draws one per task
        // (ratect#299). Names only: ownership is `run_labels`' job.
        let resource_names =
            crate::resource_names::ResourceNames::new(&self.project.config.project_name);
        // Eligibility only — `ContainerRuntime::run_container` further
        // gates this on the local process's own stdin/stdout genuinely
        // being terminals before actually attaching a TTY, and stdin
        // forwarding on `interactive` alone (see `run_container`'s own
        // docs). Gated on `ContainerIoStreaming::allows_interactive` (not
        // the interleaved-specific `interleaved_output()`) — the same
        // method `docker.rs`'s own `task_container_io` independently
        // re-checks, so the two can't disagree about which containers a
        // policy allows to be interactive.
        //
        // Every task's own container is eligible, prerequisites included,
        // as in Batect. That relies on prerequisites running one after
        // another: `docker::stdin`'s one stdin reader is lent to one container
        // at a time, and only one can hold the terminal — revisit this if
        // prerequisites ever run concurrently (ratect#102).
        let interactive = self
            .event_sink
            .container_io_streaming()
            .allows_interactive();

        let execution = async {
            // Always created, even with no dependencies, so the task's own
            // container is never left on Docker's shared default bridge
            // network. Unless `--use-network` was given
            // (`self.existing_network`), in which case that network is
            // validated to exist and reused instead — checked fresh on
            // every task execution, never cached — and, since Ratect didn't
            // create it, it's never removed during cleanup either (matching
            // Batect: cleanup only ever tears down networks it created
            // itself).
            let network_name = match &self.existing_network {
                Some(name) => {
                    if !self.docker.network_exists(name).await? {
                        anyhow::bail!("The network '{}' does not exist.", name);
                    }
                    name.clone()
                }
                None => {
                    let name = resource_names.network();
                    self.docker
                        .create_network(&name, &run_labels.for_network())
                        .await?;
                    name
                }
            };
            // Recorded for the cleanup section below *before* any further
            // failure in this block — same "register before anything can
            // fail" principle `ensure_container_created` applies to
            // `created_containers`.
            *network_name_cell.lock().unwrap() = Some(network_name.clone());

            // The resolved graph, for per-container progress displays (see
            // `TaskEvent::TaskGraphResolved`). A node missing from config
            // can't happen for a graph `build_dependency_graph` built from
            // these same containers, but degrade to bare info rather than
            // panic if it somehow does.
            // A container whose readiness is reported as another's gets no
            // line of its own, and is not something anything is shown
            // waiting for (ratect#202) — an `external_health_check`'s
            // companion is an implementation detail of the container it
            // checks. Dropping its name from a dependent's list loses
            // nothing: `expand_external_health_checks` only ever adds a
            // companion beside the container it checks, which is therefore
            // still in that list and is what the display drains on.
            let narrated = |name: &String| is_narrated(&containers, name);
            let container_infos = dependency_graph
                .iter()
                .filter(|(name, _)| narrated(name))
                .map(|(name, dependencies)| {
                    let container_config = containers.get(name);
                    // The source `resolve_image` will actually use: an
                    // override replaces the whole of it with a pull, a
                    // `build_directory` included, as Batect's does.
                    let image_override = self.image_overrides.get(name);
                    crate::ui::TaskContainerInfo {
                        name: name.clone(),
                        image: image_override
                            .cloned()
                            .or_else(|| container_config.and_then(|c| c.image.clone())),
                        build_tag: container_config
                            .and_then(|c| c.build_directory.as_ref())
                            .filter(|_| image_override.is_none())
                            .map(|_| format!("{}-{}", self.project.config.project_name, name)),
                        dependencies: container_config
                            .map_or_else(
                                || dependencies.clone(),
                                |config| {
                                    let task_dependencies = (name == &run.container)
                                        .then_some(task.dependencies.as_deref())
                                        .flatten();
                                    declared_dependencies(config, task_dependencies)
                                },
                            )
                            .into_iter()
                            .filter(|dependency| narrated(dependency))
                            .collect(),
                        is_task_container: name == &run.container,
                        has_setup_commands: container_config.is_some_and(|config| {
                            config
                                .setup_commands
                                .as_ref()
                                .is_some_and(|commands| !commands.is_empty())
                        }),
                    }
                })
                .collect();
            self.event_sink.post(TaskEvent::TaskGraphResolved {
                containers: container_infos,
            });
            let root_dependencies = dependency_graph
                .get(&run.container)
                .cloned()
                .unwrap_or_default();
            let context = TaskRunContext {
                graph: &dependency_graph,
                containers: &containers,
                network: &network_name,
                created_cells: &created_cells,
                ready_cells: &ready_cells,
                image_cells: &image_cells,
                created_containers: &created_containers,
                pending_creates: &pending_creates,
                no_proxy_entries: &no_proxy_entries,
                customisations: task.customise.as_ref(),
                run_labels: &run_labels,
                resource_names: &resource_names,
                dependency_watchers: &dependency_watchers,
                started_containers: &started_containers,
                root: &run.container,
                run,
                additional_args,
                interactive,
            };
            // Every container in the graph — each dependency, each readiness
            // companion, the task's own — is created now, concurrently, with
            // the network ready and nothing else: each one's image is pulled
            // or built first (`ensure_container_created` awaits it), and
            // nothing waits for a dependency's readiness (decisions/0019;
            // Batect's `PullImageStepRule`/`BuildImageStepRule` are likewise
            // ready at once, and its `CreateContainerStepRule` waits only
            // for the image and the network). A doomed pull, build or create
            // therefore fails the run as soon as it fails — not once every
            // dependency is ready, by which time their setup commands have
            // had their side effects — and pulls along a dependency chain
            // overlap instead of each waiting for the previous container's
            // readiness. In container-name order so the order each
            // container's work *starts* in is fixed, rather than a
            // `HashMap`'s; the work runs concurrently, so what finishes
            // first — and when its milestones post — is the daemon's.
            let mut container_names: Vec<&String> = dependency_graph.keys().collect();
            container_names.sort();
            let creations = futures::future::try_join_all(
                container_names
                    .iter()
                    .map(|name| self.ensure_container_created(name, &context)),
            );
            // Independent branches of the dependency graph start
            // concurrently; a dependent container's own
            // `ensure_container_ready` call still waits on its dependencies
            // first (see that function) — the readiness gate gates
            // starting, and only starting (see docs/dependency-readiness.md).
            // Kept for the same reason as the fan-out inside
            // `ensure_container_ready`: the task container's own `run_in`
            // setup commands (ratect#111) resolve through it. The graph's root
            // adjacency is the union of the container's own `dependencies`
            // and the task's, so it is a superset of what the config loader
            // allows as a target.
            let dependencies = futures::future::try_join_all(
                root_dependencies
                    .iter()
                    .map(|dependency_name| self.ensure_container_ready(dependency_name, &context)),
            );
            // One `try_join`, so a failure on either side drops the other
            // at once — the startup-failure behaviour
            // docs/differences-from-batect.md already records.
            let (_, dependency_ids) = futures::future::try_join(creations, dependencies).await?;
            let dependency_ids: HashMap<String, String> = dependency_ids
                .into_iter()
                .zip(root_dependencies.iter())
                .map(|(id, dependency_name)| (dependency_name.clone(), id))
                .collect();

            // Created above, alongside everything else; this is the hit.
            let CreatedContainer {
                id: container_id,
                spec,
            } = self
                .ensure_container_created(&run.container, &context)
                .await?;
            // Under the Batect-compatible dialect, the task's own container
            // goes through the same readiness gate as any dependency, run
            // *concurrently* with its main command instead of gating
            // anything; under the native dialect it has none
            // (`task_container_readiness_gate`). See
            // `run_task_container_readiness` and decisions/0012.
            let command = run
                .command
                .as_deref()
                .or(container_config.command.as_deref());
            self.event_sink.post(TaskEvent::RunningTaskContainer {
                container: run.container.clone(),
                command: command.map(str::to_string),
            });
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            // `run_container`'s own start signal, recorded as this run's
            // start of the task container and passed on to the readiness
            // gate below; polled within `run_future`, so a start that
            // happened is recorded even when an interrupt then drops it.
            let (run_started_tx, run_started_rx) = tokio::sync::oneshot::channel();
            let record_start = async {
                if run_started_rx.await.is_ok() {
                    started_containers
                        .lock()
                        .unwrap()
                        .insert(run.container.clone());
                    let _ = started_tx.send(());
                }
            };
            // A refused start is the task container's own failed step; its
            // exit, or a failure once it runs, is not.
            let run_future = async {
                let (outcome, ()) = futures::future::join(
                    self.docker
                        .run_container(&container_id, &spec, Some(run_started_tx)),
                    record_start,
                )
                .await;
                outcome.map_err(|error| {
                    if error.is::<crate::docker::ContainerStartFailed>() {
                        Failed::tag(
                            error,
                            CleanupCause::StartFailed {
                                container: run.container.clone(),
                            },
                        )
                    } else {
                        error
                    }
                })
            };
            // Batect-compatible dialect only: gate on the container's
            // readiness once it's actually running. The `select!` below is
            // what drives this concurrently with `run_future` — without
            // something polling it, the channel would never resolve.
            let readiness_future = async {
                if !self.task_container_readiness_gate {
                    return Ok(());
                }
                // A dropped sender means `run_container` failed before
                // starting the container; its own error is the report, and
                // there is nothing of ours to wait on.
                if started_rx.await.is_err() {
                    return Ok(());
                }
                self.run_task_container_readiness(
                    &container_id,
                    &run.container,
                    container_config
                        .setup_commands
                        .as_deref()
                        .unwrap_or_default(),
                    &spec,
                    &dependency_ids,
                )
                .await
            };
            let mut run_future = std::pin::pin!(run_future);
            let mut readiness_future = std::pin::pin!(readiness_future);
            // Only a readiness *failure* short-circuits: it abandons the
            // main command still running, since that command may be waiting
            // on what the gate would have done. Dropping `run_future` is the
            // same cancel an interrupt makes, and the cleanup below stops
            // and removes the container, registered at its creation. The
            // run's end ends the gate, whatever the exit code: a run that
            // finishes first drops the gate instead (which is also why this
            // isn't `tokio::try_join!`). `biased` towards the run so a main
            // command that has exited keeps its own exit code over a gate
            // failing in the same poll.
            //
            // A gate can also fail *because* the main command exited — the
            // health wait sees the container die, a setup command is killed
            // with it — while the run is still draining output and waiting
            // on Docker's exit report. So Docker is asked whether the
            // container is still running before anything is cancelled: an
            // exited one is awaited for its exit code, and "running", or a
            // failed inspect, waits up to `TASK_CONTAINER_EXIT_GRACE` for
            // the run's own result first, since a setup command killed by
            // the exit can report before Docker has marked the container
            // stopped. The reasoning, and the race this leaves, are in
            // decisions/0012.
            let (run_result, readiness_result) = tokio::select! {
                biased;
                run_result = &mut run_future => {
                    // A main command that has exited has ended the gate's
                    // question: whatever the gate is still doing is dropped
                    // rather than awaited.
                    (run_result, Ok(()))
                }
                readiness_result = &mut readiness_future => match readiness_result {
                    Ok(()) => (run_future.await, Ok(())),
                    Err(error) => {
                        let main_command_exited = matches!(
                            self.docker.container_is_running(&container_id).await,
                            Ok(false)
                        );
                        if main_command_exited {
                            (run_future.await, Err(error))
                        } else {
                            match tokio::time::timeout(TASK_CONTAINER_EXIT_GRACE, &mut run_future)
                                .await
                            {
                                Ok(run_result) => (run_result, Err(error)),
                                Err(_) => return Err(error),
                            }
                        }
                    }
                },
            };
            // Ordered, not merged: a nonzero exit code is the task's own
            // verdict and the more useful thing to report, so it wins over a
            // readiness failure that happened alongside it.
            run_result?;
            // Reached with a gate failure only once the main command has
            // exited 0: Docker had already reported the container stopped
            // when the gate failed, or the exit arrived within
            // `TASK_CONTAINER_EXIT_GRACE` — one still running past that was
            // cancelled above. Forgiven, judged by the run's outcome rather
            // than the failure's cause — see decisions/0012.
            if let Err(error) = readiness_result {
                tracing::debug!(
                    container = run.container.as_str(),
                    error = format!("{error:#}"),
                    "readiness gate ended by the task container exiting 0; ignored"
                );
            }
            Ok(())
        };

        // An interrupt abandons the run and falls through to the cleanup
        // below, rather than killing the process where it stands. Dropping
        // `run` is what cancels the work in flight — the Rust equivalent of
        // Batect's `cancellationContext.cancel()`, and immediate where
        // Batect then waits for its steps to wind down.
        //
        // `biased` so a run that finishes in the same moment it's
        // interrupted is reported as having finished: the two are
        // indistinguishable to the user, and completing is the more useful
        // reading of a tie.
        //
        // Dropping `execution` drops a container create waiting on the
        // daemon's answer too, but not the request: that is held in
        // `pending_creates`, and cleanup waits for its answer before it reads
        // what was created, so the container is removed like any other — as
        // under Batect, whose `CreateContainerStepRunner` always posts the
        // created event.
        //
        // Captured in the same expression that ends the run, not further
        // down: every interrupt after this instant is one the *cleanup*
        // should react to, and reading the count later would fold anything
        // arriving in between into the baseline and swallow it. The window
        // is small either way, but it is the window that matters.
        let (result, interrupts_before_cleanup): (Result<()>, usize) = tokio::select! {
            biased;
            result = execution => (result, self.interrupt.count()),
            () = self.interrupt.interrupted() => (
                Err(anyhow::Error::new(crate::interrupt::TaskInterrupted::new(
                    self.interrupt.last_signal(),
                ))),
                self.interrupt.count(),
            ),
        };
        // Before any notice about how it ended: a live display needs to know
        // the run is over before it prints anything that says so.
        self.event_sink.post(TaskEvent::RunEnded {
            task: task_name.to_string(),
        });
        // `Some(signal)` is the "was it signalled" question and "which one"
        // in the same read — the cleanup message below has to name the
        // signal, and there is no second source for it once the failure is
        // the only thing left.
        let interrupted_by = result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<crate::interrupt::TaskInterrupted>())
            .map(|interrupted| interrupted.signal);
        // The other way a run ends without a failure of its own: stdout's
        // reader went (ratect#670). Like a signal, its notice below says so,
        // and no summary line follows.
        let stdout_closed = result
            .as_ref()
            .is_err_and(|error| error.is::<crate::docker::StdoutClosed>());
        // Stop watching before cleanup below ever stops a container — see
        // `dependency_watchers`' own doc comment for why the ordering
        // matters, not just that it happens. `abort()` only takes effect at
        // a watcher's next `.await` point, so one that already resolved
        // `wait_for_container_exit` and is synchronously mid-`post()` right
        // now can still complete despite this — accepted, since cleanup
        // hasn't stopped anything yet at this point either way, so the
        // report that slips through is still a truthful one, just late,
        // never a deliberate stop misreported as a crash.
        for watcher in dependency_watchers.into_inner().unwrap() {
            watcher.abort();
        }
        // `Some` only if network resolution inside the block above actually
        // succeeded — `None` both when `--use-network` was given (we never
        // own that network) and when our own creation failed before ever
        // recording it.
        let network_name = network_name_cell.into_inner().unwrap();
        let owns_network = self.existing_network.is_none() && network_name.is_some();

        // Classifies `result` for both cleanup-gating (below) and the
        // `TaskFinished`/`TaskFailed` event posted below that: `Some(n)`
        // means the task's own container actually ran to completion with
        // exit code `n` — Batect's own "success" cleanup-gating bucket
        // regardless of whether `n` is zero (see `cleanup_after_success`'s
        // doc comment) — `None` means a genuine infrastructure failure
        // (`cleanup_after_failure`'s bucket instead).
        let exit_code = match &result {
            Ok(()) => Some(0),
            Err(error) => error
                .downcast_ref::<crate::docker::ContainerExitedNonZero>()
                .map(|failure| failure.exit_code),
        };
        // The success flag keeps the named task's resources alone: a
        // prerequisite is always cleaned up after success, as Batect's
        // `RunOptions` does for every task but the main one. The failure
        // flag applies to every task, prerequisites included.
        let should_cleanup = if exit_code.is_some() {
            self.cleanup_after_success || !top_level
        } else {
            self.cleanup_after_failure
        };
        // An infrastructure failure is reported now, before cleanup says a
        // word, as Batect reports every failure the moment it's posted —
        // cleanup can take a container's whole stop timeout, and the error
        // used to arrive after all of it (ratect#334). The same chain `main`
        // would have printed, and marked so it prints nothing more. A
        // signal or stdout's reader going is reported by its own notice
        // below instead, and a task's own exit by its summary line.
        //
        // A container that didn't become ready ends with how to keep it for
        // a look, while cleanup is about to remove it (ratect#368); nothing
        // else does, as under Batect, since no other failure leaves a
        // container worth looking inside.
        let result = match result {
            Err(error) if exit_code.is_none() && !crate::exit_code::already_reported(&error) => {
                let mut message = format!("{error:?}");
                if self.cleanup_after_failure && is_not_ready(&error) {
                    message.push_str("\n\n");
                    message.push_str(RERUN_WITHOUT_CLEANUP_HINT);
                }
                self.event_sink.post(TaskEvent::Error { message });
                Err(FailureReported(error).into())
            }
            result => result,
        };

        // A further signal during cleanup climbs a ladder: the first one,
        // of any kind, *escalates* — the graceful stop in flight is dropped
        // and everything left is force-removed instead — and a `SIGINT`
        // after that *abandons* cleanup altogether. Cleanup talks to the
        // daemon and isn't instant (a container ignoring `SIGTERM` waits out
        // Docker's full kill timeout), so a signal here has to mean
        // something; what it can't mean is "leave it". A supervisor
        // escalating `SIGINT` → `SIGTERM` → `SIGKILL` on a fixed timer
        // (Claude Code stops a stdio MCP server that way, 100 ms then 400 ms
        // apart) only ever means "exit", and when a second signal used to
        // abandon, every such session end leaked the task's container and
        // network until the daemon ran out of address pools (ratect#249).
        // Force removal is one daemon call per container with no grace
        // period, so it usually beats the `SIGKILL` that a graceful stop
        // never could. An abandon rung stays: forced removal still waits on
        // the daemon, and a daemon that has stopped answering would
        // otherwise hold the process until something untrappable ended it.
        // But it is `SIGINT`-only, because the kind is the one thing that
        // tells a person from a timer: a keyboard is what sends `SIGINT`
        // twice, and a supervisor's follow-up is a `SIGTERM` — which, when
        // the run had already finished and its cleanup was what the
        // supervisor's first signal escalated, would otherwise abandon that
        // cleanup 100 ms later, the very leak this exists to close.
        //
        // Batect never abandons: a second `SIGINT` in its cleanup stage lets
        // cleanup finish and then prints manual-cleanup commands
        // (`PostTaskManualCleanup.Required`). Ratect's ladder is its own.
        //
        // Measured against the count when the run *ended* (captured above),
        // not a fixed `>= 2`. Arming the handler replaces the process's
        // default behaviour for every trapped signal for the whole run, so a
        // signal Ratect doesn't act on is one it has silently swallowed — and
        // a fixed threshold swallows the first Ctrl+C during the cleanup of a
        // run that was never interrupted (the common case: a task finished,
        // cleanup is slow, the user wants out). Relative to the baseline,
        // one press escalates cleanup after a normal run and a second does
        // after an interrupted one, which is the same rule stated once.
        //
        // One press is still absorbed rather than acted on: the one that
        // arrives in the same instant the run completes, which `biased`
        // resolves in the run's favour. That tie is deliberate — the run
        // finished, so there is nothing left to abandon except the cleanup
        // the user probably wants — but it is an absorbed press, not a
        // guarantee that none exist.
        let escalate_at = interrupts_before_cleanup + 1;
        // The abandon rung is a `SIGINT` *after* escalation, so its baseline
        // is the `SIGINT` count at that moment — `Some` once escalation has
        // been detected. Before then the rung is reached in two steps:
        // whatever escalates, then a `SIGINT`.
        let mut interrupts_at_escalation: Option<usize> = None;
        // Set by any removal the daemon refuses, and by an abandoned cleanup —
        // see `CleanupFailed`. Atomic rather than a `Cell` because the closures
        // that set it are held across awaits, and `async_recursion` requires
        // this future to be `Send`.
        let cleanup_failed = AtomicBool::new(false);
        let abandon_rung = |interrupts_at_escalation: Option<usize>| async move {
            let interrupts = match interrupts_at_escalation {
                Some(interrupts) => interrupts,
                None => {
                    self.interrupt.wait_for(escalate_at).await;
                    self.interrupt.interrupt_count()
                }
            };
            self.interrupt.wait_for_interrupts(interrupts + 1).await;
        };

        if let Some(signal) = interrupted_by {
            // Named rather than fixed at "Interrupted"/"Press Ctrl+C":
            // a run stopped with `SIGTERM` was not interrupted by
            // anyone at a terminal, and telling whoever reads the log
            // to press Ctrl+C would send them somewhere with no
            // keyboard attached to this process at all.
            //
            // A notice, printed, never `tracing` (decisions/0017): this
            // is the one line the person at the keyboard must read, and
            // `RUST_LOG=off` used to hide it (ratect#369). Posted whether
            // or not cleanup runs: with `--no-cleanup*` it is still the only
            // line saying why the run stopped.
            tracing::debug!(task = task_name, signal = signal.name(), "run interrupted");
            self.post_notice(if should_cleanup {
                format!(
                    "{}; cleaning up. {} to force-remove what's left, and Ctrl+C after \
                     that to stop cleaning up.",
                    signal.ended_run(),
                    signal.send_again(),
                )
            } else {
                format!("{}; not cleaning up.", signal.ended_run())
            });
        } else if stdout_closed {
            // A reader that has gone ends the run as a signal would
            // (ratect#670), and says so the same way: nothing else reports
            // it, and the same ladder applies — but no signal arrived, so
            // there is nothing to send "again".
            self.post_notice(
                if should_cleanup {
                    "Stdout closed; cleaning up. Press Ctrl+C to force-remove what's left, \
                     and Ctrl+C after that to stop cleaning up."
                } else {
                    "Stdout closed; not cleaning up."
                }
                .to_string(),
            );
        }
        // A build whose caller the run dropped is still running in the
        // daemon — see `in_flight_builds` — so its cancel is sent before
        // anything else, with or without `--no-cleanup*`: it is no
        // container anyone asked to keep, only daemon work nobody is
        // waiting for. Sent whatever the ladder does, since nothing here
        // waits on it, and without waiting for the build to stop, as
        // Batect doesn't.
        let build_cancels = self.start_cancelling_abandoned_builds().await;
        // A create whose caller the run dropped is still with the daemon —
        // see `PendingCreates` — so its answer is waited for before cleanup
        // reads what exists. Already finished, and so instant, for every
        // create of a run that wasn't cut short. Part of cleanup, so it is
        // the ladder's first rung that ends the wait, as it ends a graceful
        // stop: everything already known is then force-removed below, and
        // waiting longer for a create's answer can't be worth leaving all of
        // that to a supervisor's `SIGKILL` (ratect#249).
        let pending = pending_creates.into_inner().unwrap();
        let _ = self
            .until_interrupted(
                self.interrupt.wait_for(escalate_at),
                futures::future::join_all(pending.iter().map(|(_, create)| create.clone())),
            )
            .await;
        // A create cut short there may still have made its container, and
        // with no id to remove it by, nothing else can say so: a line of its
        // own, since the cleanup report that follows ends on a command to
        // paste. Said as the wait stopping, not cleanup failing: under
        // `--no-cleanup*` there is no cleanup to fail.
        let mut unanswered: Vec<String> = pending
            .into_iter()
            .filter(|(_, create)| create.peek().is_none())
            .map(|(name, _)| name)
            .collect();
        unanswered.sort();
        unanswered.dedup();
        if !unanswered.is_empty() {
            let (creates, they, them) = if unanswered.len() == 1 {
                ("create", "it", "it")
            } else {
                ("creates", "they", "them")
            };
            self.event_sink.post(TaskEvent::Warning {
                message: format!(
                    "Stopped waiting for the {creates} of {} to answer, so {they} may exist; \
                     find {them} with docker ps -a --filter label={}={run_id}",
                    unanswered.join(", "),
                    crate::labels::RUN,
                ),
            });
        }
        // Everything Docker created, however far each got — see
        // `CreatedContainers`. The task's own container is pulled out: what
        // was kept or left behind is reported with it first.
        let mut created = created_containers.into_inner().unwrap();
        let task_container_id = created.remove(&run.container);
        let created_sidecars = created;

        if should_cleanup {
            let narrates_cleanup =
                !created_sidecars.is_empty() || owns_network || task_container_id.is_some();
            if narrates_cleanup {
                // Named when cleanup follows a failure (decisions/0017); a
                // signal's or a closed stdout's notice already said why.
                let cause = match exit_code {
                    _ if interrupted_by.is_some() || stdout_closed => None,
                    Some(0) => None,
                    Some(exit_code) => Some(CleanupCause::Exited {
                        task: task_name.to_string(),
                        exit_code,
                    }),
                    None => Some(result.as_ref().err().and_then(failed_step).unwrap_or_else(
                        || CleanupCause::Failed {
                            task: task_name.to_string(),
                        },
                    )),
                };
                self.event_sink.post(TaskEvent::CleanupStarting { cause });
            }
            // Every removal below is raced against the next signal rather
            // than merely checked between removals. Checking between them
            // misses the case the ladder exists for: a container ignoring
            // `SIGTERM` sits in `stop_and_remove_container` for Docker's full
            // kill timeout, which is exactly when the next signal arrives —
            // and with one container and `--use-network`, "between
            // removals" never comes around again at all.
            //
            // Losing the race cancels that removal's request in flight. On
            // the escalation rung Docker may still finish the stop
            // server-side, which the forced removal that replaces it doesn't
            // mind; on the abandonment rung the container is reported as
            // possibly left behind, which is the honest reading.
            let mut abandoned = false;

            #[derive(Clone, Copy)]
            struct Removal<'a> {
                name: &'a str,
                container_id: &'a str,
                /// A container whose readiness is reported as another's has
                /// contributed no line all run, and appearing for the first
                /// time at cleanup would name it exactly once, with nothing
                /// to attach it to (ratect#202) — removed like any other, but
                /// not narrated.
                narrate: bool,
                /// Whether this run started it — only then does its stop
                /// say `Container stopped.`.
                started: bool,
                is_task_container: bool,
            }
            // What cleanup ends by saying is built from these: the ids of
            // the containers it confirmed removed (anything else it set out
            // to remove is left behind — refused, or never reached on the
            // abandon rung) and what the daemon said when it refused. One
            // line at the end rather than one per refusal, so the user gets
            // the whole list and one command to remove it with.
            let removed: Mutex<std::collections::HashSet<String>> = Mutex::default();
            let refusals: Mutex<Vec<String>> = Mutex::default();
            let report = |removal: &Removal<'_>, outcome: Result<()>| match outcome {
                Ok(()) => {
                    removed
                        .lock()
                        .unwrap()
                        .insert(removal.container_id.to_string());
                    if removal.narrate {
                        self.event_sink.post(TaskEvent::ContainerRemoved {
                            container: removal.name.to_string(),
                        });
                    }
                }
                Err(e) => {
                    cleanup_failed.store(true, Ordering::Relaxed);
                    tracing::debug!(
                        container = removal.name,
                        task_container = removal.is_task_container,
                        error = ?e,
                        "failed to remove container"
                    );
                    refusals.lock().unwrap().push(format!("{e:#}"));
                }
            };

            // `run_container` never removes the container it creates, so
            // this is the only place the task's own container is removed —
            // however the run ended. `Some` exactly when Docker got as far
            // as creating one, which is also the only case where there is
            // anything to remove.
            //
            // Listed with the task's own container first and the rest by
            // name, so what cleanup reports comes in the same order every
            // run. The order things are *stopped* in is the graceful loop's
            // below.
            let mut created_ids: HashMap<&str, &str> = created_sidecars
                .iter()
                .map(|(name, id)| (name.as_str(), id.as_str()))
                .collect();
            if let Some(container_id) = task_container_id.as_deref() {
                created_ids.insert(run.container.as_str(), container_id);
            }
            let mut created_names: Vec<&str> = created_ids.keys().copied().collect();
            created_names.sort_by_key(|name| (*name != run.container, *name));
            let started_containers = started_containers.into_inner().unwrap();
            let removals: Vec<Removal<'_>> = created_names
                .iter()
                .filter_map(|name| created_ids.get_key_value(name))
                .map(|(&name, &container_id)| {
                    let is_task_container = name == run.container;
                    Removal {
                        name,
                        container_id,
                        narrate: is_task_container || is_narrated(&containers, name),
                        started: started_containers.contains(name),
                        is_task_container,
                    }
                })
                .collect();
            // Everything cleanup set out to remove, for the report at the
            // end — `removals` itself is consumed by the rungs below.
            let to_remove: Vec<(&str, &str)> = removals
                .iter()
                .map(|removal| (removal.name, removal.container_id))
                .collect();

            // Graceful, until the ladder's first rung, and concurrent as
            // Batect's is: each container's stop begins the moment every
            // container depending on it has had its own attempt — whatever
            // the outcome, so a refused stop holds nothing back — and every
            // container ready at once is stopped at once, so independent
            // dependencies wait out their grace periods together rather
            // than one after another. Not gated by `--max-parallelism`
            // (see `max_parallelism`). The rung is reached either by a
            // signal cutting the stops in flight short or by one landing
            // between two batches, which the count check catches so no stop
            // is requested only to be dropped at once.
            let mut waiting = dependents_to_stop_first(&dependency_graph, &created_names);
            let mut queued = removals;
            let mut in_flight: Vec<Removal<'_>> = Vec::new();
            let mut stops = futures::stream::FuturesUnordered::new();
            loop {
                if self.interrupt.count() >= escalate_at {
                    break;
                }
                let (ready, still_waiting): (Vec<_>, Vec<_>) =
                    queued.into_iter().partition(|removal| {
                        waiting
                            .get(removal.name)
                            .is_none_or(|dependents| dependents.is_empty())
                    });
                queued = still_waiting;
                for removal in ready {
                    in_flight.push(removal);
                    let container_config = containers.get(removal.name);
                    let stop_signal = container_config.and_then(|c| c.stop_signal.as_deref());
                    let stop_grace_period = container_config.and_then(|c| c.stop_grace_period);
                    stops.push(async move {
                        // Said the moment the stop succeeds, while the
                        // removal is still under way: a slow or refused
                        // removal is when that line matters (decisions/0017).
                        let (stopped, on_stopped) = tokio::sync::oneshot::channel();
                        let (outcome, ()) = futures::future::join(
                            self.docker.stop_and_remove_container(
                                removal.container_id,
                                stop_signal,
                                stop_grace_period,
                                Some(stopped),
                            ),
                            async {
                                if on_stopped.await.is_ok() && removal.narrate && removal.started {
                                    self.event_sink.post(TaskEvent::ContainerStopped {
                                        container: removal.name.to_string(),
                                    });
                                }
                            },
                        )
                        .await;
                        (removal, outcome)
                    });
                }
                let next = futures::StreamExt::next(&mut stops);
                match self
                    .until_interrupted(self.interrupt.wait_for(escalate_at), next)
                    .await
                {
                    Some(Some((removal, outcome))) => {
                        report(&removal, outcome);
                        in_flight.retain(|other| other.container_id != removal.container_id);
                        for dependents in waiting.values_mut() {
                            dependents.remove(removal.name);
                        }
                    }
                    // Nothing in flight, so nothing is left to start either:
                    // the graph is acyclic, so whatever is still queued is
                    // waiting on something in flight.
                    Some(None) => break,
                    None => break,
                }
            }
            // A stop that finished in the same instant as the one the signal
            // landed on, but wasn't reported yet, is reported as it is —
            // handing it to the forced rung would remove it twice and never
            // say it was removed. Polled once, never awaited: the rest are
            // still in flight.
            while let Some(Some((removal, outcome))) =
                futures::FutureExt::now_or_never(futures::StreamExt::next(&mut stops))
            {
                report(&removal, outcome);
                in_flight.retain(|other| other.container_id != removal.container_id);
            }
            // Dropping the stops in flight cancels their requests; they go to
            // the forced rung with everything not yet started.
            drop(stops);
            let removals: Vec<Removal<'_>> = in_flight.into_iter().chain(queued).collect();
            // Forced, all at once: dependency order only mattered for the
            // graceful stops, and a supervisor's `SIGKILL` is on a timer, so
            // what is left goes out in one round-trip rather than one each.
            if !removals.is_empty() {
                let signal = self.interrupt.last_signal();
                interrupts_at_escalation = Some(self.interrupt.interrupt_count());
                // Always Ctrl+C: the abandon rung is `SIGINT`-only, and the
                // only sender it is advice for is a person at a terminal.
                self.post_notice(format!(
                    "{} during cleanup; force-removing what's left. Press Ctrl+C to stop \
                     cleaning up.",
                    signal.ended_run(),
                ));
                let forced = futures::future::join_all(removals.iter().map(|removal| async {
                    let outcome = self
                        .docker
                        .remove_container_forcibly(removal.container_id)
                        .await;
                    report(removal, outcome);
                }));
                if self
                    .until_interrupted(abandon_rung(interrupts_at_escalation), forced)
                    .await
                    .is_none()
                {
                    abandoned = true;
                }
            }
            let mut network_removed = false;
            if owns_network && !abandoned {
                let network_name = network_name
                    .as_deref()
                    .expect("owns_network implies network_name is Some");
                self.event_sink.post(TaskEvent::RemovingNetwork);
                // A network has no faster removal to escalate to, so it is
                // raced against the abandonment rung alone; a first signal
                // landing here lets it finish, and doesn't change what the
                // run exits with (decisions/0016).
                let removal = self.docker.remove_network(network_name);
                match self
                    .until_interrupted(abandon_rung(interrupts_at_escalation), removal)
                    .await
                {
                    Some(Ok(())) => {
                        network_removed = true;
                        self.event_sink.post(TaskEvent::NetworkRemoved);
                    }
                    Some(Err(e)) => {
                        cleanup_failed.store(true, Ordering::Relaxed);
                        tracing::debug!(network = network_name, error = ?e, "failed to remove network");
                        refusals.lock().unwrap().push(format!("{e:#}"));
                    }
                    None => abandoned = true,
                }
            }
            // Driven by work actually being cut short, not by the raw count:
            // an interrupt arriving once everything is already removed leaves
            // nothing behind, and sending someone hunting for leftovers that
            // don't exist is its own small betrayal.
            if abandoned {
                cleanup_failed.store(true, Ordering::Relaxed);
            }
            // A cleanup that left something behind is a cleanup failure, and
            // is reported as the error it is (decisions/0017) — here, where
            // what was left and what the daemon said are known, rather than
            // by `main` from the `CleanupFailed` it may (a passing task) or
            // may not (a task that exited `3`, a signalled run) get back; so
            // `main` prints nothing further for a `CleanupFailed`
            // (`exit_code::already_reported`). Abandonment names itself;
            // a refusal quotes the daemon.
            // Ahead of every line cleanup ends with, which are about a
            // cleanup that is over.
            self.report_build_cancels(self.interrupt.wait_for(escalate_at), build_cancels)
                .await;
            // Before the failure report, if any: that line is about a
            // cleanup that is over, and a live countdown painted after it
            // would say otherwise.
            if narrates_cleanup {
                self.event_sink.post(TaskEvent::CleanupFinished);
            }
            if cleanup_failed.load(Ordering::Relaxed) {
                let removed = removed.into_inner().unwrap();
                let left = LeftBehind::new(
                    to_remove
                        .iter()
                        .filter(|(_, id)| !removed.contains(*id))
                        .copied(),
                    network_name
                        .as_deref()
                        .filter(|_| owns_network && !network_removed),
                );
                self.event_sink.post(TaskEvent::Error {
                    message: left.failure_message(abandoned, &refusals.into_inner().unwrap()),
                });
            } else if interrupted_by.is_some() {
                // The interrupt sequence's last line (decisions/0017): a
                // signalled run has no summary line to end on, and its notice
                // said cleanup was under way, so this says it is over — or
                // the failure above does, in its place (ratect#369).
                self.post_notice("Cleanup complete.".to_string());
            }
        } else {
            self.report_build_cancels(self.interrupt.wait_for(escalate_at), build_cancels)
                .await;
            // The task's own container is reported here like any other:
            // nothing else deals with it now, so a run kept for
            // investigation says so once, for everything it kept — by what
            // the flag left and how to remove it (decisions/0017). Not a
            // failure, so a notice, with the same tail as the cleanup
            // failure above: one shape, learned once. Names the flag family
            // (`--no-cleanup`), as the ADR writes it, not the bucket that
            // applied: after a task that exited `3`, "after-success" reads
            // as a contradiction to the person who typed `--no-cleanup`.
            let kept_task_container = task_container_id
                .as_deref()
                .map(|id| (run.container.as_str(), id));
            let left = LeftBehind::new(
                kept_task_container.into_iter().chain(
                    created_sidecars
                        .iter()
                        .map(|(name, id)| (name.as_str(), id.as_str())),
                ),
                network_name.as_deref().filter(|_| owns_network),
            );
            if !left.is_empty() {
                self.post_notice(format!(
                    "Left in place (--no-cleanup): {}. Remove with: {}",
                    left.list(),
                    left.command()
                ));
                // Then how to look inside each, as Batect says — but not a
                // readiness companion, which nothing else names either
                // (ratect#202). Whether one is running is the daemon's to
                // say: the task's own container has usually exited, but not
                // after a signal, and a dependency may never have started.
                // Asked all at once, and raced against the next signal as
                // cleanup's own daemon calls are, so a daemon that has
                // stopped answering costs one request timeout at most, and
                // a signal since the run ended — one that cut cleanup's own
                // waits short included — ends this one rather than being
                // swallowed by it.
                // Unknown — no answer, or an error — counts as stopped,
                // since starting a running container does nothing.
                let mut investigable: Vec<(&str, &str)> = kept_task_container
                    .into_iter()
                    .chain(
                        created_sidecars
                            .iter()
                            .filter(|(name, _)| is_narrated(&containers, name))
                            .map(|(name, id)| (name.as_str(), id.as_str())),
                    )
                    .collect();
                investigable.sort();
                let answers = futures::future::join_all(
                    investigable
                        .iter()
                        .map(|(_, id)| self.docker.container_is_running(id)),
                );
                let running = self
                    .until_interrupted(self.interrupt.wait_for(escalate_at), answers)
                    .await
                    .unwrap_or_default();
                for (index, (name, id)) in investigable.into_iter().enumerate() {
                    let running = matches!(running.get(index), Some(Ok(true)));
                    self.post_notice(LeftBehind::investigation(name, id, running));
                }
            }
        }

        // A signal that arrived only during cleanup changes nothing here: the
        // command had already exited, so the signal ended nothing, and the
        // run's result stands (decisions/0016, rule 2) — a divergence from
        // Batect, where an interrupt in the cleanup stage fails the task. If
        // it led cleanup to be abandoned, that is a cleanup failure, recorded
        // above like a refused removal.
        //
        // A cleanup failure replaces only an `Ok`: a task's own non-zero code,
        // a signal that ended it, or an infrastructure failure is what the
        // user needs to see, and the leak is reported on stderr either way.
        // Returned like any other failure, so a prerequisite's ends the chain
        // before the task that depends on it runs.
        let result = match result {
            Ok(()) if cleanup_failed.load(Ordering::Relaxed) => Err(CleanupFailed {
                task: task_name.to_string(),
                run: run_id.clone(),
            }
            .into()),
            result => result,
        };

        // "Finished" means the task's own command ran to completion and
        // reported an exit code — zero (`Ok`) or not (the
        // `ContainerExitedNonZero` error, which still propagates to become
        // ratect's own exit code); an infrastructure failure posts
        // `TaskFailed` instead, and so does a passing command whose cleanup
        // then failed: "finished with exit code 0" would be true of the
        // command and false of the run (ratect#610), and the `CleanupFailed`
        // error already says the command passed. A non-zero code a cleanup
        // failure leaves alone keeps its "finished" line, still true. Both
        // are posted after cleanup, matching Batect's `onTaskFinished`
        // (called once `ParallelExecutionManager.run()` — cleanup included —
        // has returned).
        let exit_code =
            exit_code.filter(|_| !result.as_ref().is_err_and(|e| e.is::<CleanupFailed>()));
        if let Some(exit_code) = exit_code {
            self.event_sink.post(TaskEvent::TaskFinished {
                task: task_name.to_string(),
                exit_code,
                duration: task_started_at.elapsed(),
            });
        } else if interrupted_by.is_none() && !stdout_closed {
            // Infrastructure failure — the error itself was reported before
            // cleanup, above, or by it for a cleanup failure; this ends the
            // task, as `TaskFinished` would. A signal or a gone reader posts
            // nothing: its notice already said how the run ended
            // (decisions/0017's interrupt sequence).
            self.event_sink.post(TaskEvent::TaskFailed {
                task: task_name.to_string(),
                duration: task_started_at.elapsed(),
            });
        }

        result
    }

    /// Ensures `name`'s container is *created* — Docker has it on its
    /// network, user mapping applied, ready to be started — memoized per this
    /// one task execution via `context.created_cells`, and returns its id
    /// and the spec it was created from. Waits for exactly what creation
    /// needs and nothing else (decisions/0019, Batect's
    /// `CreateContainerStepRule`): the container's own image
    /// (`ensure_image_resolved`), the task network (already there — the
    /// caller made it before any of this), and, for a `container:` network
    /// mode, the *creation* of its target — Docker's create needs the
    /// target's id, and no more; the target's readiness gates this
    /// container's *start*, in `ensure_container_ready`, as for any other
    /// dependency. Never a dependency's readiness: that is the whole point
    /// of the split, so that a create-time failure (a bad mount, a log
    /// driver the daemon doesn't have) ends the run as soon as it lands
    /// rather than once every dependency is ready and its setup commands
    /// have had their side effects.
    ///
    /// Reached twice per container: `run_task_internal` fans this out over
    /// the whole graph the moment the network exists, and the ready stage
    /// awaits the same cell when it is about to start the container — a hit
    /// when the fan-out got there first, the same in-flight work otherwise.
    /// The task's own container (`context.root`) comes through here too, with
    /// its `run` overlay, `-- ADDITIONAL_ARGS` and interactive eligibility
    /// where a dependency gets its `customise`.
    ///
    /// The id is registered in `context.created_containers` the instant
    /// `create_container` hands it back — on success and inside a failure
    /// alike (`CreateContainerError::created`) — before this returns and
    /// before anything else can fail, so cleanup removes the container
    /// however the run ends. Recursive only through the `container:` target,
    /// which the config loader requires to be one of `name`'s own
    /// dependencies, so the graph's proven acyclicity bounds it.
    // `double_must_use`: see `run_task_scoped`.
    #[allow(clippy::double_must_use)]
    #[async_recursion]
    async fn ensure_container_created<'p>(
        &'p self,
        name: &str,
        context: &TaskRunContext<'_, 'p>,
    ) -> Result<CreatedContainer> {
        let cell = get_or_create_cell(context.created_cells, name.to_string());
        let result = cell
            .get_or_init(|| async {
                let outcome: Result<CreatedContainer> = async {
                    let container_config = context
                        .containers
                        .get(name)
                        .with_context(|| undeclared_container_bug(name))?;
                    let is_root = name == context.root;
                    let image = self
                        .ensure_image_resolved(name, context.containers, context.image_cells)
                        .await?;
                    // The one thing a container is created with that comes
                    // from another container: the id a `container:` network
                    // mode shares. Only the target's creation is awaited,
                    // never its readiness (see above).
                    let network_target_ids: HashMap<String, String> = match &container_config
                        .network_mode
                    {
                        Some(crate::config::NetworkMode::Container(target)) => {
                            let created = self.ensure_container_created(target, context).await?;
                            HashMap::from([(target.clone(), created.id)])
                        }
                        _ => HashMap::new(),
                    };
                    let network = crate::container_spec::resolve_network(
                        container_config.network_mode.as_ref(),
                        context.network,
                        &network_target_ids,
                    )?;
                    let user_mapping = self.resolve_user_mapping(container_config).await?;
                    let proxy = self.proxy_environment(context.no_proxy_entries);
                    // Everything that tells the task's own container from a
                    // dependency, decided once: its `run` overlay where a
                    // dependency gets its `customise` entry (applied on top
                    // of its own base config, same precedence — see
                    // `config::check_values` for the validation ensuring a
                    // customisation can never target the main task
                    // container, and
                    // `config::reject_customisations_outside_the_task_graph`
                    // — which runs after `extends` — for one outside this
                    // task's own graph), the `-- ADDITIONAL_ARGS` only it
                    // receives, its role, and its interactive eligibility —
                    // a dependency is never interactive.
                    let (overlay, additional_args, role, interactive) = if is_root {
                        (
                            crate::container_spec::Overlay::Run(context.run),
                            context.additional_args,
                            crate::labels::ContainerRole::Task,
                            context.interactive,
                        )
                    } else {
                        (
                            crate::container_spec::Overlay::Customise(
                                context.customisations.and_then(|c| c.get(name)),
                            ),
                            &[][..],
                            crate::labels::ContainerRole::Dependency,
                            false,
                        )
                    };
                    let term_var = self.term_environment_variable();
                    let volumes = self
                        .resolve_volumes(container_config.volumes.as_ref())
                        .await?;
                    let spec = crate::container_spec::derive_spec(
                        crate::container_spec::ContainerSpecInputs {
                            name,
                            docker_name: &context.resource_names.container(name),
                            container_config,
                            overlay,
                            image: &image,
                            network,
                            interactive,
                            additional_args,
                            user_mapping: user_mapping.as_ref(),
                            volumes: volumes.as_ref(),
                            term_var: term_var.as_ref(),
                            proxy: proxy.as_ref(),
                            publish_ports: self.publish_ports,
                            role,
                            run_labels: context.run_labels,
                        },
                    );

                    // Derived once, and consumed everywhere this container's
                    // own lifecycle would otherwise be narrated: a
                    // companion is an implementation detail of the
                    // container it checks, so it contributes no lines of
                    // its own (ratect#202).
                    let narrated = container_config.reports_readiness_for.is_none();
                    let created = {
                        // Held only around the create call itself — see
                        // `max_parallelism`'s own doc comment for why a
                        // create and a start are separate permits, and why
                        // none is held while waiting on a dependency. The
                        // milestone is posted once the permit is held, so
                        // it marks the create beginning rather than a wait
                        // for the cap — as the "starting" one does.
                        let _permit = self.acquire_parallelism_permit().await;
                        if narrated {
                            self.event_sink.post(TaskEvent::ContainerCreating {
                                container: name.to_string(),
                            });
                        }
                        // Held in `pending_creates` as well as awaited here,
                        // so dropping this call doesn't drop the request —
                        // see `PendingCreates`. The answer comes back through
                        // `answer` rather than the shared future's output,
                        // which would have to be `Clone`.
                        let answer: Arc<
                            Mutex<Option<Result<String, crate::docker::CreateContainerError>>>,
                        > = Arc::default();
                        let create = {
                            let answer = Arc::clone(&answer);
                            let spec = spec.clone();
                            let name = name.to_string();
                            let created_containers = context.created_containers;
                            futures::FutureExt::shared(futures::FutureExt::boxed(async move {
                                let created = self.docker.create_container(&spec).await;
                                // Registered before anything else can happen
                                // to it, on both arms: a container Docker
                                // created is removed however the rest of the
                                // run goes — see `CreatedContainers`.
                                let id = match &created {
                                    Ok(id) => Some(id),
                                    Err(error) => error.created.as_ref(),
                                };
                                if let Some(id) = id {
                                    created_containers
                                        .lock()
                                        .unwrap()
                                        .insert(name.clone(), id.clone());
                                    // Posted from the same place the id is
                                    // registered, so a display's idea of what
                                    // is outstanding can never disagree with
                                    // what cleanup will actually remove.
                                    if narrated && created.is_ok() {
                                        self.event_sink
                                            .post(TaskEvent::ContainerCreated { container: name });
                                    }
                                }
                                *answer.lock().unwrap() = Some(created);
                            }))
                        };
                        context
                            .pending_creates
                            .lock()
                            .unwrap()
                            .push((name.to_string(), create.clone()));
                        create.await;
                        let outcome = answer
                            .lock()
                            .unwrap()
                            .take()
                            .expect("a finished create has left its answer");
                        outcome
                    };
                    let id = created.map_err(|error| {
                        Failed::tag(
                            error.error,
                            Failed::of_container(
                                name,
                                container_config.reports_readiness_for.as_deref(),
                                |container| CleanupCause::CreateFailed { container },
                            ),
                        )
                    })?;
                    Ok(CreatedContainer { id, spec })
                }
                .await;
                outcome.map_err(Arc::new)
            })
            .await;

        unshare(result)
    }

    /// Ensures `name`'s container is started and *ready* (healthy, then every
    /// one of its `setup_commands` succeeded — or, with `run_to_completion`,
    /// exited 0), memoized per this one task execution via
    /// `context.ready_cells` — concurrent callers reaching the same node (a
    /// diamond in the dependency graph) share one `ReadyCell`, so the second
    /// awaits the first's in-flight work instead of starting it twice. Fans
    /// out to `name`'s own dependencies (`graph[name]`) concurrently, via
    /// `try_join_all`, before starting its own container — this and the
    /// memoization together are what let independent branches of one task's
    /// dependency graph run at the same time while a container with
    /// dependencies of its own still waits for them first (see
    /// docs/dependency-readiness.md). Only the *start* waits: the container
    /// is created by `ensure_container_created`, which `run_task_internal`
    /// fans out over the whole graph the moment the network exists and which
    /// the call here awaits — a hit when the fan-out got there first
    /// (decisions/0019). No cycle guard here any more (pre-0.15.0's
    /// `resolving`/`running` params): `graph` is already proven acyclic by
    /// `build_dependency_graph`, run once, synchronously, before this is
    /// ever called.
    // `double_must_use`: see `run_task_scoped`.
    #[allow(clippy::double_must_use)]
    #[async_recursion]
    async fn ensure_container_ready<'p>(
        &'p self,
        name: &str,
        context: &TaskRunContext<'_, 'p>,
    ) -> Result<String> {
        let cell = get_or_create_cell(context.ready_cells, name.to_string());
        let result = cell
            .get_or_init(|| async {
                let outcome: Result<String> = async {
                    let empty = Vec::new();
                    let dependencies = context.graph.get(name).unwrap_or(&empty);
                    // Kept, rather than discarded: a `run_in` setup command
                    // (ratect#111) resolves its target through exactly this
                    // set, which is why the config loader restricts it to
                    // one of `name`'s own dependencies — every id here
                    // belongs to a container already through its own full
                    // readiness gate.
                    let dependency_ids: HashMap<String, String> =
                        futures::future::try_join_all(dependencies.iter().map(|dependency_name| {
                            self.ensure_container_ready(dependency_name, context)
                        }))
                        .await?
                        .into_iter()
                        .zip(dependencies.iter())
                        .map(|(id, dependency_name)| (dependency_name.clone(), id))
                        .collect();

                    let dependency_config = context
                        .containers
                        .get(name)
                        .with_context(|| undeclared_container_bug(name))?;
                    // Created the moment its image was ready, by the fan-out
                    // in `run_task_internal`; this awaits that same cell.
                    let CreatedContainer {
                        id: container_id,
                        spec,
                    } = self.ensure_container_created(name, context).await?;

                    // Derived once, and consumed everywhere this container's
                    // own lifecycle would otherwise be narrated: a
                    // companion is an implementation detail of the
                    // container it checks, so it contributes no lines of
                    // its own (ratect#202).
                    let reports_readiness_for = dependency_config.reports_readiness_for.as_deref();
                    {
                        // Held only around the start call — matching
                        // `resolve_image`'s own placement, not the
                        // health-check wait or the readiness bookkeeping
                        // either side of it. See `max_parallelism`'s own
                        // doc comment for why starting counts against the
                        // cap but waiting for healthy doesn't. The
                        // "starting" milestone is posted once the permit is
                        // held, so it marks the start actually beginning
                        // rather than a wait for the cap (ratect#505).
                        let _permit = self.acquire_parallelism_permit().await;
                        if reports_readiness_for.is_none() {
                            self.event_sink.post(TaskEvent::DependencyStarting {
                                container: name.to_string(),
                            });
                        }
                        self.docker
                            .start_background_container(&container_id, &spec)
                            .await
                            .map_err(|error| {
                                Failed::tag(
                                    error,
                                    Failed::of_container(
                                        name,
                                        reports_readiness_for,
                                        |container| CleanupCause::StartFailed { container },
                                    ),
                                )
                            })?;
                        context
                            .started_containers
                            .lock()
                            .unwrap()
                            .insert(name.to_string());
                    }
                    if reports_readiness_for.is_none() {
                        self.event_sink.post(TaskEvent::DependencyStarted {
                            container: name.to_string(),
                        });
                    }

                    // A `run_to_completion` dependency (ratect#97) takes a
                    // different readiness gate entirely: it has no health
                    // check and no `setup_commands` of its own — mutually
                    // exclusive at config-load time (see
                    // `config::reject_run_to_completion_conflicts`, which
                    // runs after `extends` has resolved — this branch is
                    // exactly what silently dropped an *inherited*
                    // `setup_commands` while that check ran too early) — so
                    // instead it must simply run to completion and exit 0.
                    // No watcher is spawned below for it either: its exit is
                    // the readiness signal itself, not a later surprise.
                    if dependency_config.run_to_completion.unwrap_or(false) {
                        let exit_code = self
                            .docker
                            .wait_for_container_exit(&container_id)
                            .await
                            .with_context(|| {
                                format!("Container '{}' did not run to completion", name)
                            })?;
                        if exit_code != 0 {
                            // An external health check's companion diagnoses
                            // its own failure on stderr — which attempt,
                            // what it last saw. Reported in the *checked*
                            // container's name, because that is the
                            // container the user asked about; the companion
                            // is named only so `--no-cleanup-after-failure`
                            // leaves something findable.
                            if let Some(checked) = reports_readiness_for {
                                let detail = self
                                    .docker
                                    .container_output(&container_id)
                                    .await
                                    .unwrap_or_else(|error| {
                                        tracing::debug!(
                                            "Could not read the output of container '{name}', \
                                             so its diagnosis is lost: {error:#}"
                                        );
                                        String::new()
                                    });
                                let detail = detail.trim();
                                return Err(Failed::tag(
                                    anyhow::anyhow!(
                                        "Container '{checked}' did not become healthy: {}. The \
                                         external check ran in container '{name}' — \
                                         --no-cleanup-after-failure keeps it for inspection",
                                        if detail.is_empty() {
                                            "its external health check failed".to_string()
                                        } else {
                                            detail.to_string()
                                        }
                                    ),
                                    CleanupCause::NotHealthy {
                                        container: checked.to_string(),
                                    },
                                ));
                            }
                            return Err(Failed::tag(
                                anyhow::anyhow!(
                                    "Container '{}' (a run-to-completion dependency) exited with \
                                     code {}",
                                    name,
                                    exit_code
                                ),
                                CleanupCause::DependencyExited {
                                    container: name.to_string(),
                                    exit_code,
                                },
                            ));
                        }
                        // A companion's success *is* the checked container's
                        // readiness, and is reported as such — see
                        // `Container::reports_readiness_for`.
                        match reports_readiness_for {
                            Some(checked) => {
                                self.event_sink.post(TaskEvent::ContainerBecameHealthy {
                                    container: checked.to_string(),
                                })
                            }
                            None => self.event_sink.post(TaskEvent::DependencyCompleted {
                                container: name.to_string(),
                            }),
                        }
                        return Ok(container_id);
                    }

                    // Batect's readiness gate (see docs/dependency-readiness.md):
                    // started isn't ready. The dependency must report
                    // healthy (immediate for a container with no health
                    // check at all), then every one of its setup commands
                    // must succeed, before anything that depends on it
                    // starts.
                    //
                    // Except for a container whose readiness is an
                    // `external_health_check` (ratect#269): that check alone
                    // decides it, so its image's own `HEALTHCHECK` — which
                    // Docker still runs — isn't waited on, and there is no
                    // verdict here to report. Its companion posts
                    // `ContainerBecameHealthy` instead, when the check
                    // actually passes. One branch for both, so the wait and
                    // the event can't disagree about which containers skip it.
                    if dependency_config.external_health_check.is_none() {
                        self.docker
                            .wait_for_container_healthy(&container_id)
                            .await
                            .with_context(|| {
                                NotReady(format!("Container '{}' did not become healthy", name))
                            })
                            .map_err(|error| {
                                Failed::tag(
                                    error,
                                    CleanupCause::NotHealthy {
                                        container: name.to_string(),
                                    },
                                )
                            })?;
                        self.event_sink.post(TaskEvent::ContainerBecameHealthy {
                            container: name.to_string(),
                        });
                    }

                    self.run_setup_commands(
                        dependency_config
                            .setup_commands
                            .as_deref()
                            .unwrap_or_default(),
                        &SetupCommandContext {
                            name,
                            container_id: &container_id,
                            working_directory: spec.shared.options.working_directory.as_deref(),
                            environment: spec.shared.environment.as_ref(),
                            user_mapping: spec.shared.user_mapping.as_ref(),
                            dependency_ids: &dependency_ids,
                        },
                    )
                    .await?;

                    // Now, and only now: ready is the point after which an
                    // exit is news rather than expected (still starting, or
                    // still in its own health/setup wait, both already
                    // report clearly on their own). Detached, not part of
                    // this future's own result — this dependency being
                    // ready doesn't wait on it ever exiting; collected in
                    // `dependency_watchers` purely so the run can abort it
                    // later, once watching would otherwise risk catching
                    // cleanup's own deliberate stop instead of a real one.
                    let watcher = {
                        let docker = Arc::clone(&self.docker);
                        let event_sink = Arc::clone(&self.event_sink);
                        let watched_id = container_id.clone();
                        let watched_name = name.to_string();
                        tokio::spawn(async move {
                            match docker.wait_for_container_exit(&watched_id).await {
                                Ok(exit_code) => {
                                    event_sink.post(TaskEvent::DependencyExitedUnexpectedly {
                                        container: watched_name,
                                        exit_code,
                                    });
                                }
                                // Aborted before this ever resolves in the
                                // overwhelmingly common case (the run
                                // finishing normally) — a real `Err` here
                                // means the wait itself failed (a daemon
                                // hiccup, the container vanishing out from
                                // under this watcher some other way), which
                                // isn't worth failing anything over but is
                                // worth a trace rather than none at all.
                                Err(error) => tracing::debug!(
                                    container = watched_name.as_str(),
                                    error = ?error,
                                    "dependency-exit watcher ended without a verdict"
                                ),
                            }
                        })
                    };
                    context.dependency_watchers.lock().unwrap().push(watcher);

                    Ok(container_id)
                }
                .await;
                outcome.map_err(Arc::new)
            })
            .await;

        unshare(result)
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
