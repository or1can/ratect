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

use crate::config::{
    container_names_in_task, BuildSecret, Config, Container, PortMapping, Task,
    TaskContainerCustomisation,
};
use crate::docker::ContainerRuntime;
use crate::ui::{EventSink, NullEventSink, TaskEvent};
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// The host environment lookup `TaskEngine` reads proxy variables from —
/// boxed so the real `std::env::var`-backed closure and a fixed test
/// closure share one field type.
type HostEnv = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Merges the host's `TERM` (see `TaskEngine::term_environment_variable`),
/// proxy-derived environment variables (see
/// `TaskEngine::proxy_environment_variables`), a container's `environment`,
/// and a task's `run.environment`, each overriding the last on key
/// collision — `TERM` and proxy vars are the lowest-precedence base,
/// matching Batect (`terminalEnvironmentVariablesFor + proxyEnvironmentVariables +
/// substituteEnvironmentVariables`, later entries winning); the
/// container's `environment` overrides both, and `run.environment`
/// overrides all three. `None` only when none of the four are set.
fn merged_environment(
    term_var: Option<&HashMap<String, String>>,
    proxy_vars: Option<&HashMap<String, String>>,
    container_env: Option<&HashMap<String, String>>,
    run_env: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    if term_var.is_none() && proxy_vars.is_none() && container_env.is_none() && run_env.is_none() {
        return None;
    }
    let mut merged = term_var.cloned().unwrap_or_default();
    if let Some(proxy_vars) = proxy_vars {
        merged.extend(proxy_vars.clone());
    }
    if let Some(container_env) = container_env {
        merged.extend(container_env.clone());
    }
    if let Some(run_env) = run_env {
        merged.extend(run_env.clone());
    }
    Some(merged)
}

/// The `TERM=dumb` every container gets under the interleaved I/O policy
/// (the `all` output mode) — a full-screen program shouldn't try terminal
/// control sequences when its output is being line-buffered and prefixed,
/// matching Batect's `InterleavedContainerIOStreamingOptions`.
fn dumb_term_environment() -> HashMap<String, String> {
    HashMap::from([("TERM".to_string(), "dumb".to_string())])
}

/// Expands and concatenates a container's own `ports` with a task run's
/// *additional* `ports` — a union, not an override (matching Batect, which
/// combines these as a `Set`, so there's no concept of one entry replacing
/// another by container port; `run_ports` is `None` for a dependency, which
/// has no task `run` to add anything from). Each `PortMapping` is expanded
/// (a range becomes more than one triple — see `PortMapping::expand`)
/// before docker.rs ever sees it, so `NetworkOptions::ports` only ever
/// carries already-resolved `(local_port, container_port, protocol)`
/// triples, never a `PortMapping` needing further interpretation.
fn merged_ports(
    container_ports: Option<&Vec<PortMapping>>,
    run_ports: Option<&Vec<PortMapping>>,
) -> Vec<(u16, u16, String)> {
    container_ports
        .into_iter()
        .flatten()
        .chain(run_ports.into_iter().flatten())
        .flat_map(PortMapping::expand)
        .collect()
}

/// Converts a container's parsed `health_check` config into the docker-side
/// [`crate::docker::HealthCheckOptions`] — `docker.rs` deliberately doesn't
/// depend on config types (same conversion boundary as `merged_ports`'
/// expanded tuples above).
fn health_check_options(container: &Container) -> Option<crate::docker::HealthCheckOptions> {
    container
        .health_check
        .as_ref()
        .map(|health_check| crate::docker::HealthCheckOptions {
            command: health_check.command.clone(),
            interval: health_check.interval,
            retries: health_check.retries,
            start_period: health_check.start_period,
            timeout: health_check.timeout,
        })
}

/// Converts a `capabilities_to_add`/`capabilities_to_drop` set of
/// `config::Capability` into the plain Docker capability name strings
/// `docker.rs`'s `ContainerOptions` expects — `docker.rs` deliberately
/// doesn't depend on config types (same conversion boundary as
/// `health_check_options` above). `None` when the set itself is `None`.
fn capability_names(
    capabilities: Option<&HashSet<crate::config::Capability>>,
) -> Option<Vec<String>> {
    Some(
        capabilities?
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect(),
    )
}

/// Converts a `devices` list of `config::DeviceMapping` into the plain
/// `(local, container, options)` triples `docker.rs`'s `ContainerOptions`
/// expects — `docker.rs` deliberately doesn't depend on config types (same
/// conversion boundary as `capability_names` above).
fn device_triples(
    devices: Option<&Vec<crate::config::DeviceMapping>>,
) -> Option<Vec<(String, String, Option<String>)>> {
    Some(
        devices?
            .iter()
            .map(|device| {
                (
                    device.local.clone(),
                    device.container.clone(),
                    device.options.clone(),
                )
            })
            .collect(),
    )
}

/// Converts a container's parsed `build_secrets`/`build_ssh` config into the
/// docker-side [`crate::docker::BuildKitOptions`] — `None` when neither is
/// set (no session providers to serve; which *builder* runs the build is
/// decided separately, by the `DockerClient` itself, from the daemon's
/// advertised default). `build_ssh`'s shape (at most one `default`-id,
/// no-`paths` entry) is already validated by
/// [`crate::config::Config::resolve_expressions_with`] by the time this
/// runs — this only reads whether an entry is present at all.
fn buildkit_options(container: &Container) -> Option<crate::docker::BuildKitOptions> {
    let secrets = container.build_secrets.as_ref();
    let ssh = container.build_ssh.as_ref();
    if secrets.is_none() && ssh.is_none() {
        return None;
    }
    Some(crate::docker::BuildKitOptions {
        secrets: secrets
            .map(|secrets| {
                secrets
                    .iter()
                    .map(|(id, secret)| {
                        let source = match secret {
                            BuildSecret::Environment(name) => {
                                crate::docker::BuildSecretSource::Environment(name.clone())
                            }
                            BuildSecret::Path(path) => {
                                crate::docker::BuildSecretSource::File(PathBuf::from(path))
                            }
                        };
                        (id.clone(), source)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        forward_default_ssh_agent: ssh.is_some_and(|agents| !agents.is_empty()),
    })
}

/// The outcome of a memoized async operation (an image pull/build, or a
/// dependency container reaching "ready") shared across every concurrent
/// caller that reaches the same cache key. `anyhow::Error` isn't `Clone`, so
/// a failure is wrapped in `Arc` — every waiter that shares a [`ReadyCell`]
/// sees the same outcome without re-attempting the underlying Docker call.
type SharedResult<T> = Result<T, Arc<anyhow::Error>>;

/// A lazily-created, memoized cell holding the eventual outcome (image name/
/// ID, or a started container's ID) for one cache key (an image name, or a
/// container name) — see `get_or_create_cell`. `tokio::sync::OnceCell`
/// (rather than `get_or_try_init`, which does *not* cache a failure) so a
/// failed pull/build/start is cached just like a successful one: a later
/// caller sharing the same key sees the same `Err` instead of retrying the
/// real Docker call.
type ReadyCell = Arc<OnceCell<SharedResult<String>>>;

/// Gets (or lazily creates) the shared cell for `key` in `cells`, under a
/// short synchronous lock — the lock is dropped before the returned cell is
/// ever `.await`ed on (by the caller, via `.get_or_init`), so it's held only
/// for a `HashMap` lookup/insert, never across `.await` — same
/// double-checked-lock convention this file already used for
/// `pulled_images`/`built_images` pre-0.15.0.
fn get_or_create_cell(cells: &Mutex<HashMap<String, ReadyCell>>, key: &str) -> ReadyCell {
    let mut cells = cells.lock().unwrap();
    cells
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(OnceCell::new()))
        .clone()
}

/// Flattens a cached [`SharedResult`] back into a plain, owned `Result` at
/// the point it's actually returned to a caller — the shared `Arc<Error>` is
/// reformatted via its `Debug` (which anyhow's `Error` renders as the full
/// context chain) rather than just its `Display` (the top message only),
/// since this may be the only place a waiter *other than* the one that hit
/// the real failure ever sees it.
fn unshare(result: &SharedResult<String>) -> Result<String> {
    result.clone().map_err(|e| anyhow::anyhow!("{:?}", e))
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
/// circular) and is skipped without re-visiting — mirrors Batect's own
/// `ContainerDependencyGraph`, run once, synchronously, before any concurrent
/// execution begins. This static split is why `ensure_container_ready` no
/// longer needs its own runtime cycle guard (pre-0.15.0's `resolving` set) —
/// a graph returned from here is already proven acyclic.
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
            .with_context(|| format!("Container '{}' not found", name))?;
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
/// surfaces its usual "Task not found" error later (from `run_task_scoped`),
/// rather than being silently dropped here. A wildcard matching zero tasks
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
    tasks: &HashMap<String, Task>,
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
fn suggest_task_names(tasks: &HashMap<String, Task>, name: &str) -> Vec<String> {
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
fn human_readable_list(items: &[String], conjunction: &str) -> String {
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
fn format_task_suggestions(tasks: &HashMap<String, Task>, name: &str) -> String {
    let suggestions = suggest_task_names(tasks, name);
    if suggestions.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = suggestions.iter().map(|s| format!("'{}'", s)).collect();
    format!(" Did you mean {}?", human_readable_list(&quoted, "or"))
}

pub struct TaskEngine<D: ContainerRuntime + Send + Sync> {
    config: Config,
    docker: D,
    executed_tasks: Mutex<HashSet<String>>,
    /// Image name -> the shared, memoized pull outcome for that name, so an
    /// image referenced by multiple containers (across tasks, or by
    /// concurrent branches of one task's own dependency graph — 0.15.0) is
    /// only ever decided/pulled once per invocation. A `ReadyCell` rather
    /// than a plain `HashSet`+check-then-act specifically so two containers
    /// racing to resolve the same image concurrently share one in-flight
    /// pull instead of double-pulling.
    pulled_images: Mutex<HashMap<String, ReadyCell>>,
    /// Container name -> the shared, memoized build outcome (the built image
    /// ID) for that container, so a container with `build_directory` is only
    /// ever built once per invocation even if referenced by multiple tasks,
    /// as both a dependency and a task's own container, or reached
    /// concurrently by two branches of one task's dependency graph (0.15.0).
    /// Keyed by container name (not build directory) since a given name
    /// always has the same `build_directory`/`build_args` within one
    /// `Config`. Stores the image ID (not the human-readable tag) — see
    /// `resolve_image` for why.
    built_images: Mutex<HashMap<String, ReadyCell>>,
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
    /// The host environment lookup `proxy::proxy_environment_variables`
    /// reads from — real `std::env::var` in the real constructor, a fixed
    /// closure in tests (see `with_host_env`), same reason
    /// `config.rs::resolve_expressions_with` parameterizes over this.
    host_env: HostEnv,
    /// `true` when `--skip-prerequisites` was given: the top-level task's
    /// own `prerequisites` are never run. Matches Batect's flag of the same
    /// name — scoped to the named task only (never a prerequisite itself,
    /// which is the only other thing that could otherwise trigger this
    /// check; see `run_task_internal`'s `top_level` parameter).
    skip_prerequisites: bool,
    /// Set via `--override-image <container>=<image>` (repeatable):
    /// container name -> the image to pull instead of whatever that
    /// container actually configures. Validated against `config.containers`
    /// up front (see `with_image_overrides`) rather than left to fail lazily
    /// the first time an overridden container is reached. See
    /// `resolve_image`.
    image_overrides: HashMap<String, String>,
    /// Set via `--tag-image <container>=<tag>` (repeatable, multiple tags
    /// per container): container name -> extra tags applied to that
    /// container's *built* image, in addition to the default
    /// `<project_name>-<container_name>` tag `resolve_image` already
    /// applies. Never validated against `config.containers` up front (no
    /// eager check here, unlike `image_overrides`) — matching Batect, which
    /// only ever surfaces a problem when the named container is actually
    /// reached (see `resolve_image`) or, for one that's never reached at
    /// all, once the whole invocation finishes (see `run_task`).
    image_tags: HashMap<String, std::collections::HashSet<String>>,
    /// Every container name `resolve_image` has been asked to resolve so
    /// far this invocation (task and prerequisites alike) — regardless of
    /// whether the underlying pull/build was deduped. Used only to answer
    /// `--tag-image`'s "did this container actually run" check once the
    /// whole invocation finishes (see `run_task`).
    containers_used: Mutex<HashSet<String>>,
    /// `false` when `--no-cleanup`/`--no-cleanup-after-success` was given:
    /// the task's own container (regardless of exit code — see
    /// `docker::ContainerRuntime::run_container`'s own doc comment for why
    /// a nonzero exit is still "success" here), its dependency containers,
    /// and the task's own network are all left in place instead of removed.
    /// `true` (the default) always cleans up. See `run_task_internal`.
    cleanup_after_success: bool,
    /// `false` when `--no-cleanup`/`--no-cleanup-after-failure` was given:
    /// same as `cleanup_after_success`, but for a genuine infrastructure
    /// failure (a build/pull/health-check/setup-command failure, or
    /// anything else that never reaches the task's own container exiting)
    /// — matching Batect's own success/failure split for cleanup-gating
    /// purposes exactly (`TaskEvent::TaskFinished` vs `TaskEvent::TaskFailed`
    /// already encode it). `true` (the default) always cleans up.
    cleanup_after_failure: bool,
    /// Set via `--max-parallelism <N>`: caps how many resource-intensive
    /// operations run concurrently across the whole invocation — image
    /// pulls/builds (`resolve_pulled_image`/`resolve_image`'s build branch),
    /// a dependency's own create+start (`ensure_container_ready`'s
    /// `start_background_container` call), and setup-command execution
    /// (`ensure_container_ready`'s `exec_in_container` call, one permit per
    /// command — a single container's own setup commands already run
    /// sequentially, so this only ever limits how many *different*
    /// containers' setup commands overlap). `None` (the default) is
    /// unbounded, matching both Ratect's own pre-existing behavior and
    /// Batect's own default when the flag isn't passed.
    ///
    /// Deliberately *not* applied to `wait_for_container_healthy` (a health
    /// check is a polling wait, not CPU/disk work — gating it would only
    /// slow down convergence for no resource-saving benefit) or to
    /// `stop_and_remove_container` (cleanup teardown isn't resource-
    /// intensive in practice). Also never applied to the task's own
    /// container's `run_container` call — matching Batect's own
    /// `RunContainerStep` exemption (`countsAgainstParallelismCap = false`):
    /// it's the actual task work, not setup, and is often long-running by
    /// design (an interactive shell, a dev server), so it must never
    /// compete for or be blocked by this cap.
    ///
    /// Still narrower than Batect's own flag, which schedules *every*
    /// setup/cleanup step (including the ones excluded here) through a
    /// dedicated step-scheduling model (`ParallelExecutionManager`) Ratect
    /// doesn't have — see [Differences from
    /// Batect](../../docs/differences-from-batect.md#cli-flags). A single
    /// shared semaphore (rather than one per image/container) is what makes
    /// this an invocation-wide cap rather than a per-resource one.
    max_parallelism: Option<Arc<tokio::sync::Semaphore>>,
    /// Where task-execution milestones go for the user to see —
    /// [`NullEventSink`] (silent) by default, a real output-mode logger via
    /// `with_event_sink`. See `crate::ui`.
    event_sink: Arc<dyn EventSink>,
    /// Set via `with_cache_options` (always called by `main.rs`, unset only
    /// in tests that don't exercise `cache` volumes): `--cache-type` and the
    /// project's own root directory, needed to resolve a `cache` volume
    /// mount into an actual Docker bind string — see
    /// `resolve_volumes`/`crate::cache`.
    cache_options: Option<crate::cache::CacheOptions>,
    /// Memoizes `crate::cache::project_cache_key` for the life of this
    /// `TaskEngine` — computed at most once per invocation, and only if a
    /// `cache` volume is actually resolved (never eagerly), matching
    /// Batect's own `CacheManager.projectCacheKey`'s `by lazy` behavior.
    cache_key: OnceCell<String>,
}

impl<D: ContainerRuntime + Send + Sync> TaskEngine<D> {
    pub fn new(config: Config, docker: D) -> Self {
        Self {
            config,
            docker,
            executed_tasks: Mutex::new(HashSet::new()),
            pulled_images: Mutex::new(HashMap::new()),
            built_images: Mutex::new(HashMap::new()),
            in_progress_tasks: Mutex::new(HashSet::new()),
            existing_network: None,
            publish_ports: true,
            propagate_proxy_environment_variables: true,
            host_env: Box::new(|name| std::env::var(name).ok()),
            event_sink: Arc::new(NullEventSink),
            skip_prerequisites: false,
            image_overrides: HashMap::new(),
            image_tags: HashMap::new(),
            containers_used: Mutex::new(HashSet::new()),
            cleanup_after_success: true,
            cleanup_after_failure: true,
            max_parallelism: None,
            cache_options: None,
            cache_key: OnceCell::new(),
        }
    }

    /// Injects the output-mode logger task-execution milestones render
    /// through. Without this, the engine is silent (aside from `tracing`
    /// diagnostics) — the default every unit test relies on.
    pub fn with_event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = event_sink;
        self
    }

    /// Whether the selected output mode owns container I/O line by line
    /// (the `all` mode) — see `crate::ui::ContainerIoStreaming`.
    fn interleaved_output(&self) -> bool {
        self.event_sink.container_io_streaming() == crate::ui::ContainerIoStreaming::Interleaved
    }

    /// Opts into `--use-network`: `network` is validated to exist (and
    /// reused, never torn down) for every task run through this engine,
    /// instead of each task getting a fresh network created and removed
    /// around it. See `run_task_internal`.
    pub fn with_existing_network(mut self, network: String) -> Self {
        self.existing_network = Some(network);
        self
    }

    /// Opts into `--disable-ports`: no container's `ports` are ever
    /// published, regardless of config.
    pub fn without_port_publishing(mut self) -> Self {
        self.publish_ports = false;
        self
    }

    /// Opts into `--no-proxy-vars`: proxy environment variables are never
    /// propagated into a container's environment or a build's `build_args`,
    /// regardless of what's set in the host environment.
    pub fn without_proxy_environment_variables(mut self) -> Self {
        self.propagate_proxy_environment_variables = false;
        self
    }

    /// Opts into `--skip-prerequisites`: the named task's own `prerequisites`
    /// are never run. See `run_task_internal`.
    pub fn without_prerequisites(mut self) -> Self {
        self.skip_prerequisites = true;
        self
    }

    /// Opts into `--override-image <container>=<image>`: every entry's
    /// container name is validated to exist up front — matching Batect's own
    /// eager validation and error wording exactly — rather than only failing
    /// the first time (if ever) that container is actually reached during a
    /// task run. See `resolve_image`.
    pub fn with_image_overrides(mut self, overrides: HashMap<String, String>) -> Result<Self> {
        for name in overrides.keys() {
            if !self.config.containers.contains_key(name) {
                anyhow::bail!(
                    "Cannot override image for container '{name}' because there is no \
                     container named '{name}' defined."
                );
            }
        }
        self.image_overrides = overrides;
        Ok(self)
    }

    /// Opts into `--tag-image <container>=<tag>`: extra tags applied to a
    /// container's *built* image once it's actually resolved (see
    /// `resolve_image`) — never validated up front, matching Batect (a
    /// container name that's never reached, or that ends up using a pulled
    /// image, is only ever an error once that's actually known).
    pub fn with_image_tags(
        mut self,
        tags: HashMap<String, std::collections::HashSet<String>>,
    ) -> Self {
        self.image_tags = tags;
        self
    }

    /// Opts into `--no-cleanup-after-success` (also set by `--no-cleanup`):
    /// see `cleanup_after_success`'s own doc comment.
    pub fn without_cleanup_after_success(mut self) -> Self {
        self.cleanup_after_success = false;
        self
    }

    /// Opts into `--no-cleanup-after-failure` (also set by `--no-cleanup`):
    /// see `cleanup_after_failure`'s own doc comment.
    pub fn without_cleanup_after_failure(mut self) -> Self {
        self.cleanup_after_failure = false;
        self
    }

    /// Opts into `--max-parallelism <N>`: see `max_parallelism`'s own doc
    /// comment for exactly what it caps.
    pub fn with_max_parallelism(mut self, max: usize) -> Self {
        self.max_parallelism = Some(Arc::new(tokio::sync::Semaphore::new(max)));
        self
    }

    /// Supplies `--cache-type` and the project's own root directory, needed
    /// to resolve any `cache` volume mount a container declares (see
    /// `resolve_volumes`). `main.rs` always calls this — it's a builder
    /// method rather than a `TaskEngine::new` parameter only to match this
    /// struct's existing convention for opt-in settings, not because it's
    /// actually optional in practice.
    pub fn with_cache_options(
        mut self,
        cache_type: crate::cache::CacheType,
        project_directory: PathBuf,
    ) -> Self {
        self.cache_options = Some(crate::cache::CacheOptions {
            cache_type,
            project_directory,
        });
        self
    }

    /// Resolves a container's `volumes` into the literal Docker bind
    /// strings `docker.rs`'s `run_container`/`start_background_container`
    /// expect. `Local` mounts are already fully resolved (host path made
    /// absolute, interpolated) by `Config::resolve_expressions` — nothing
    /// left to do here but reassemble the `"local:container[:options]"`
    /// string. `Cache` mounts are resolved here instead, since that needs
    /// `--cache-type` (`with_cache_options`) and the project's own cache
    /// key, neither available to `config.rs`. `cache_key` is only ever
    /// computed the first time this actually encounters a `Cache` mount —
    /// a config with none never touches the filesystem for this at all.
    async fn resolve_volumes(
        &self,
        volumes: Option<&Vec<crate::config::VolumeMount>>,
    ) -> Result<Option<Vec<String>>> {
        let Some(volumes) = volumes else {
            return Ok(None);
        };

        let mut resolved = Vec::with_capacity(volumes.len());
        for volume in volumes {
            match volume {
                crate::config::VolumeMount::Local(local) => {
                    resolved.push(match &local.options {
                        Some(options) => {
                            format!("{}:{}:{}", local.local, local.container, options)
                        }
                        None => format!("{}:{}", local.local, local.container),
                    });
                }
                crate::config::VolumeMount::Cache(cache) => {
                    let cache_options = self.cache_options.as_ref().expect(
                        "a config with a 'cache' volume mount requires with_cache_options to \
                         have been called first",
                    );
                    let cache_key = self
                        .cache_key
                        .get_or_try_init(|| async {
                            crate::cache::project_cache_key(&cache_options.project_directory)
                        })
                        .await?;
                    resolved.push(crate::cache::resolve_cache_mount(
                        cache_options,
                        cache_key,
                        cache,
                    )?);
                }
            }
        }

        Ok(Some(resolved))
    }

    #[cfg(test)]
    fn with_host_env(
        mut self,
        host_env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.host_env = Box::new(host_env);
        self
    }

    /// The proxy environment variables to inject for a container in this
    /// task, or `None` when propagation is disabled (`--no-proxy-vars`) or
    /// the host environment has none set — an empty map is normalized to
    /// `None` here so `merged_environment`'s "`None` only when nothing at
    /// all is set" behavior isn't disturbed by an empty-but-`Some` map.
    fn proxy_environment_variables(
        &self,
        extra_no_proxy_entries: &std::collections::BTreeSet<String>,
    ) -> Option<HashMap<String, String>> {
        if !self.propagate_proxy_environment_variables {
            return None;
        }
        let host_env = |name: &str| (self.host_env)(name);
        let vars = crate::proxy::proxy_environment_variables(host_env, extra_no_proxy_entries);
        (!vars.is_empty()).then_some(vars)
    }

    /// The `TERM` to inject into a container's environment — the host's own
    /// value for the invoked task's own container (`interactive` is `true`),
    /// `None` for anything else (a prerequisite's, a dependency's, or a
    /// sidecar's container, or an image build), or `dumb` unconditionally
    /// under the interleaved I/O policy (the `all` output mode), overriding
    /// both of those — matching Batect's
    /// `InterleavedContainerIOStreamingOptions`, which sets `TERM=dumb` on
    /// every container regardless of whether it would otherwise have been
    /// the task's own. The single call both `run_task_internal` (the task's
    /// own container) and `ensure_container_ready` (a dependency, always
    /// `interactive: false`) make, so the interleaved override lives in
    /// exactly one place rather than being checked at each call site with
    /// its own idiom.
    ///
    /// Non-interleaved `interactive: true` is gated on `interactive` alone
    /// — deliberately *not* on whether a real TTY ends up being allocated
    /// (that's decided later, inside `ContainerRuntime::run_container`, from
    /// information not yet known here) — matching Batect's own
    /// `ConsoleInfo.terminalType`/
    /// `TaskContainerOnlyIOStreamingOptions.terminalTypeForContainer`, both
    /// unconditional on any TTY check. So a full-screen terminal program
    /// inside the container knows the terminal type even when piping output
    /// elsewhere still lets it detect it isn't attached to a real TTY.
    fn term_environment_variable(&self, interactive: bool) -> Option<HashMap<String, String>> {
        if self.interleaved_output() {
            return Some(dumb_term_environment());
        }
        if !interactive {
            return None;
        }
        let term = (self.host_env)("TERM")?;
        Some(HashMap::from([("TERM".to_string(), term)]))
    }

    /// Acquires a permit from `max_parallelism`'s semaphore, if configured —
    /// `None` (a no-op) when `--max-parallelism` wasn't given, unbounded as
    /// before. Every call site holds this only for the duration of the one
    /// actual Docker-facing operation it wraps (a pull, a build, a
    /// create+start, one setup-command exec) — see `max_parallelism`'s own
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

    /// Pulls `image` under `policy` (deduped by image name across the whole
    /// invocation via `pulled_images` — see `resolve_image`), returning the
    /// image reference to run. Shared by `resolve_image`'s two pull-shaped
    /// callers: a container's own configured `image`, and an
    /// `--override-image` replacement (always `IfNotPresent`, never the
    /// container's own configured policy).
    async fn resolve_pulled_image(
        &self,
        image: &str,
        policy: crate::config::ImagePullPolicy,
    ) -> Result<String> {
        let cell = get_or_create_cell(&self.pulled_images, image);
        let result = cell
            .get_or_init(|| async {
                let outcome: Result<String> = async {
                    let should_pull = match policy {
                        crate::config::ImagePullPolicy::Always => true,
                        crate::config::ImagePullPolicy::IfNotPresent => {
                            !self.docker.image_exists_locally(image).await?
                        }
                    };
                    if should_pull {
                        // Milestones post only when a pull actually happens —
                        // a skip (image already local under `IfNotPresent`)
                        // stays silent, matching Batect.
                        self.event_sink.post(TaskEvent::ImagePullStarting {
                            image: image.to_string(),
                        });
                        let _permit = self.acquire_parallelism_permit().await;
                        self.docker.pull_image(image).await?;
                        self.event_sink.post(TaskEvent::ImagePullCompleted {
                            image: image.to_string(),
                        });
                    }
                    Ok(image.to_string())
                }
                .await;
                outcome.map_err(Arc::new)
            })
            .await;

        unshare(result)
    }

    /// `--tag-image` only ever makes sense for a *built* image — errors
    /// immediately (rather than silently ignoring the tag request) the
    /// moment a tagged container name turns out to resolve via a pull
    /// instead, whether that's its own configured `image` or an
    /// `--override-image` replacement. Matches Batect's
    /// `ImageTaggingValidator`/`ContainerUsesPulledImageException` message
    /// exactly.
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
    /// name) or `build_directory` (building it, deduped by `container_name`)
    /// into the image reference to actually run. Shared by a task's own
    /// container and its dependency containers — both need exactly this and
    /// nothing else, which is also why dependency containers now support
    /// `build_directory` (they didn't before this was unified).
    ///
    /// `image`'s `image_pull_policy` (`IfNotPresent` by default, matching
    /// Batect) decides whether a pull actually reaches the registry the
    /// first time an image name is seen this session: `IfNotPresent` skips
    /// it entirely when `ContainerRuntime::image_exists_locally` already
    /// says yes; `Always` never checks. Either way, once decided for a
    /// given image name, later containers reusing that same name within
    /// this session reuse the decision rather than re-checking or
    /// re-pulling.
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
    /// to by the time the container actually starts.
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
            let cell = get_or_create_cell(&self.built_images, container_name);
            let result = cell
                .get_or_init(|| async {
                    let outcome: Result<String> = async {
                        let tag = format!("{}-{}", self.config.project_name, container_name);
                        // No `extra_no_proxy_entries` at build time — matches
                        // Batect, which never adds container names to
                        // `no_proxy` for a build (nothing's running yet to be
                        // exempted from proxying).
                        let proxy_vars =
                            self.proxy_environment_variables(&std::collections::BTreeSet::new());
                        let build_args = merged_environment(
                            None,
                            proxy_vars.as_ref(),
                            container_config.build_args.as_ref(),
                            None,
                        );
                        let dockerfile = container_config
                            .dockerfile
                            .as_deref()
                            .unwrap_or("Dockerfile");
                        let buildkit = buildkit_options(container_config);
                        self.event_sink.post(TaskEvent::ImageBuildStarting {
                            container: container_name.to_string(),
                        });
                        let _permit = self.acquire_parallelism_permit().await;
                        let image_id = self
                            .docker
                            .build_image(
                                Path::new(build_directory),
                                dockerfile,
                                build_args.as_ref(),
                                container_config.build_target.as_deref(),
                                buildkit.as_ref(),
                                &tag,
                            )
                            .await?;
                        self.event_sink.post(TaskEvent::ImageBuildCompleted {
                            container: container_name.to_string(),
                        });
                        // `--tag-image`: applied once here (inside this
                        // cell's do-once build), never re-applied on a later
                        // cache hit for the same container this invocation.
                        if let Some(tags) = self.image_tags.get(container_name) {
                            if !tags.is_empty() {
                                let mut tags: Vec<String> = tags.iter().cloned().collect();
                                tags.sort();
                                self.docker.tag_image(&image_id, &tags).await?;
                            }
                        }
                        Ok(image_id)
                    }
                    .await;
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
            .expect("validated non-None by Config::resolve_expressions when enabled is true");

        Ok(Some(crate::docker::UserMapping {
            user,
            home_directory,
        }))
    }

    /// `additional_args` are only ever forwarded to the container run for
    /// exactly the task named here — not to any of its prerequisites, which
    /// always run with no additional args, matching Batect's behavior of
    /// scoping `-- ARGS` to the task named on the command line.
    ///
    /// Thin wrapper over `run_task_scoped` fixing `top_level` to `true` — the
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
    /// line, `false` for every prerequisite (however deeply nested) — used to
    /// decide interactive-TTY eligibility for that task's own container (see
    /// `run_task_internal`). A prerequisite chain isn't the thing being "run"
    /// interactively, and stdin can only usefully attach to one container at
    /// a time, so only the top-level task's own container is ever eligible —
    /// same principle Batect applies (only ever its single "task container"),
    /// even though Ratect's prerequisites are structurally different (full
    /// recursive task runs, not steps within one task).
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
            if in_progress.contains(task_name) {
                return Err(anyhow::anyhow!(
                    "Dependency cycle detected involving task '{}'",
                    task_name
                ));
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

    async fn run_task_internal(
        &self,
        task_name: &str,
        additional_args: &[String],
        top_level: bool,
    ) -> Result<()> {
        let task = self.config.tasks.get(task_name).with_context(|| {
            format!(
                "Task '{}' not found.{}",
                task_name,
                format_task_suggestions(&self.config.tasks, task_name)
            )
        })?;

        // Run prerequisites (never with additional args, and never eligible
        // for interactive TTY attachment — both scoped to only the
        // originally-requested task). A `*`-wildcard entry is expanded
        // against the full task list first — see
        // `expand_prerequisite_wildcards` — then run through the same
        // sequential loop as any other prerequisite; its own dedup/cycle
        // detection (see `run_task_scoped`) already collapses a name reached
        // more than once (e.g. named explicitly *and* matched by a wildcard)
        // to a single actual run.
        //
        // Skipped entirely when `--skip-prerequisites` was given and this is
        // the top-level task — never for a prerequisite task itself, which
        // always runs its own prerequisites regardless (matching Batect: the
        // flag only ever names the one task given on the command line).
        if !(top_level && self.skip_prerequisites) {
            if let Some(prerequisites) = &task.prerequisites {
                let prerequisites =
                    expand_prerequisite_wildcards(&self.config.tasks, prerequisites)?;
                for prerequisite in &prerequisites {
                    self.run_task_scoped(prerequisite, &[], false).await?;
                }
            }
        }

        // A task with only `prerequisites` and no `run` of its own — those
        // have already executed above; there's no container of the task's
        // own left to run. Matches Batect's `TaskRunner`, which prints the
        // equivalent message and stops here rather than treating this as an
        // error.
        let Some(run) = &task.run else {
            tracing::info!(
                "Task '{}' only defines prerequisite tasks, nothing more to do",
                task_name
            );
            return Ok(());
        };

        // Run the task itself
        let container_config = self
            .config
            .containers
            .get(&run.container)
            .with_context(|| format!("Container '{}' not found", run.container))?;

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

        // Populated concurrently as each dependency starts (before its own
        // readiness gate — see `ensure_container_ready`), so cleanup below
        // still tears down every container that got as far as starting, even
        // one that never became ready. `Mutex`-guarded (rather than owned
        // `&mut`, pre-0.15.0) since independent branches of the dependency
        // graph now start concurrently and each registers itself here from
        // its own task.
        let running_sidecars: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
        // Memoizes each container's own readiness future for this one task
        // execution — see `ensure_container_ready`/`ReadyCell`. Reset per
        // task (unlike `pulled_images`/`built_images`, which persist for the
        // whole invocation): a dependency is deliberately re-started fresh
        // for every task that uses it — see docs/task-lifecycle.md's
        // "Cross-task isolation".
        let ready_cells: Mutex<HashMap<String, ReadyCell>> = Mutex::new(HashMap::new());
        // Fixed for the whole task, computed once up front — every
        // container started for this task (the task's own and each
        // dependency) gets the same `no_proxy` exemption list, matching
        // Batect's `allContainersInNetwork` being fixed for the whole graph
        // rather than recomputed per container.
        let no_proxy_entries = container_names_in_task(
            &self.config.containers,
            &run.container,
            task.dependencies.as_deref(),
        );

        let result: Result<()> = async {
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
                    let name = format!("ratect-{}", Uuid::new_v4());
                    self.docker.create_network(&name).await?;
                    name
                }
            };
            // Recorded for the cleanup section below *before* any further
            // failure in this block — same "register before the readiness
            // gate" principle `ensure_container_ready` already applies to
            // `running_sidecars`.
            *network_name_cell.lock().unwrap() = Some(network_name.clone());

            // Static, up-front cycle check (see `build_dependency_graph`) —
            // proves the whole graph acyclic before any concurrent execution
            // starts, so `ensure_container_ready` doesn't need its own
            // runtime cycle guard.
            let graph = build_dependency_graph(
                &self.config.containers,
                &run.container,
                task.dependencies.as_deref(),
            )?;
            // The resolved graph, for per-container progress displays (see
            // `TaskEvent::TaskGraphResolved`). A node missing from config
            // can't happen for a graph that just built successfully, but
            // degrade to bare info rather than panic if it somehow does.
            let container_infos = graph
                .iter()
                .map(|(name, dependencies)| {
                    let container_config = self.config.containers.get(name);
                    crate::ui::TaskContainerInfo {
                        name: name.clone(),
                        image: container_config.and_then(|c| c.image.clone()),
                        build_tag: container_config
                            .and_then(|c| c.build_directory.as_ref())
                            .map(|_| format!("{}-{}", self.config.project_name, name)),
                        dependencies: dependencies.clone(),
                        is_task_container: name == &run.container,
                    }
                })
                .collect();
            self.event_sink.post(TaskEvent::TaskGraphResolved {
                containers: container_infos,
            });
            let root_dependencies = graph.get(&run.container).cloned().unwrap_or_default();
            // Independent branches of the dependency graph start
            // concurrently; a dependent container's own
            // `ensure_container_ready` call still waits on its dependencies
            // first (see that function) — matching Batect's own within-task
            // container concurrency (see docs/task-lifecycle.md).
            futures::future::try_join_all(root_dependencies.iter().map(|dependency_name| {
                self.ensure_container_ready(
                    dependency_name,
                    &graph,
                    &network_name,
                    &ready_cells,
                    &running_sidecars,
                    &no_proxy_entries,
                    task.customise.as_ref(),
                )
            }))
            .await?;

            let image = self.resolve_image(&run.container, container_config).await?;
            self.event_sink.post(TaskEvent::ImageResolved {
                container: run.container.clone(),
            });
            // Eligibility only — `ContainerRuntime::run_container` further
            // gates this on the local process's own stdin/stdout genuinely
            // being terminals before actually attaching a TTY, and stdin
            // forwarding on `interactive` alone (see `run_container`'s own
            // docs). Computed here, ahead of the environment merge below,
            // since `term_environment_variable` needs it. Gated on
            // `ContainerIoStreaming::allows_interactive` (not the
            // interleaved-specific `interleaved_output()`) — the same
            // method `docker.rs`'s own `run_container` independently
            // re-checks before actually attaching, so the two can't
            // disagree about which containers a policy allows to be
            // interactive.
            let interactive = top_level
                && self
                    .event_sink
                    .container_io_streaming()
                    .allows_interactive();
            let proxy_vars = self.proxy_environment_variables(&no_proxy_entries);
            let term_var = self.term_environment_variable(interactive);
            let environment = merged_environment(
                term_var.as_ref(),
                proxy_vars.as_ref(),
                container_config.environment.as_ref(),
                run.environment.as_ref(),
            );
            let user_mapping = self.resolve_user_mapping(container_config).await?;
            let expanded_ports = merged_ports(container_config.ports.as_ref(), run.ports.as_ref());
            let network_options = crate::docker::NetworkOptions {
                additional_hostnames: container_config.additional_hostnames.as_ref(),
                additional_hosts: container_config.additional_hosts.as_ref(),
                ports: (self.publish_ports && !expanded_ports.is_empty())
                    .then_some(&expanded_ports),
            };
            // The task's own container gets its `health_check` override
            // applied too (Docker records and runs it), but nothing gates
            // on its verdict — the task is the container's own command, and
            // its `setup_commands` don't run either (see
            // docs/differences-from-batect.md).
            let health_check = health_check_options(container_config);
            let command = run
                .command
                .as_deref()
                .or(container_config.command.as_deref());
            let working_directory = run
                .working_directory
                .as_deref()
                .or(container_config.working_directory.as_deref());
            let entrypoint = run
                .entrypoint
                .as_deref()
                .or(container_config.entrypoint.as_deref());
            let capabilities_to_add =
                capability_names(container_config.capabilities_to_add.as_ref());
            let capabilities_to_drop =
                capability_names(container_config.capabilities_to_drop.as_ref());
            let devices = device_triples(container_config.devices.as_ref());
            let container_options = crate::docker::ContainerOptions {
                working_directory,
                entrypoint,
                labels: container_config.labels.as_ref(),
                capabilities_to_add: capabilities_to_add.as_ref(),
                capabilities_to_drop: capabilities_to_drop.as_ref(),
                privileged: container_config.privileged,
                shm_size: container_config.shm_size,
                devices: devices.as_ref(),
                enable_init_process: container_config.enable_init_process,
            };
            self.event_sink.post(TaskEvent::RunningTaskContainer {
                container: run.container.clone(),
                command: command.map(str::to_string),
            });
            let volumes = self
                .resolve_volumes(container_config.volumes.as_ref())
                .await?;
            self.docker
                .run_container(
                    &run.container,
                    &image,
                    command,
                    additional_args,
                    volumes.as_ref(),
                    environment.as_ref(),
                    &network_name,
                    interactive,
                    user_mapping.as_ref(),
                    &network_options,
                    health_check.as_ref(),
                    &container_options,
                    self.cleanup_after_success,
                )
                .await?;

            Ok(())
        }
        .await;

        let running_sidecars = running_sidecars.into_inner().unwrap();
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
        let should_cleanup = if exit_code.is_some() {
            self.cleanup_after_success
        } else {
            self.cleanup_after_failure
        };

        if should_cleanup {
            if !running_sidecars.is_empty() || owns_network {
                self.event_sink.post(TaskEvent::CleanupStarting);
            }
            for (name, container_id) in &running_sidecars {
                match self.docker.stop_and_remove_container(container_id).await {
                    Ok(()) => self.event_sink.post(TaskEvent::ContainerRemoved {
                        container: name.clone(),
                    }),
                    Err(e) => {
                        tracing::warn!(
                            dependency = name.as_str(),
                            error = ?e,
                            "Failed to clean up dependency container"
                        );
                    }
                }
            }
            if owns_network {
                let network_name = network_name.expect("owns_network implies network_name is Some");
                self.event_sink.post(TaskEvent::RemovingNetwork);
                if let Err(e) = self.docker.remove_network(&network_name).await {
                    tracing::warn!(network = network_name.as_str(), error = ?e, "Failed to remove network");
                }
            }
        } else if !running_sidecars.is_empty() || owns_network {
            tracing::info!(
                task = task_name,
                dependencies = running_sidecars.len(),
                network = network_name.as_deref(),
                "cleanup disabled; leaving dependency containers and the task network in place \
                 for investigation"
            );
        }

        // "Finished" means the task's own command ran to completion and
        // reported an exit code — zero (`Ok`) or not (the
        // `ContainerExitedNonZero` error, which still propagates to become
        // ratect's own exit code). An infrastructure failure posts nothing.
        // Posted after cleanup, matching Batect's `onTaskFinished` (called
        // once `ParallelExecutionManager.run()` — cleanup included — has
        // returned).
        if let Some(exit_code) = exit_code {
            self.event_sink.post(TaskEvent::TaskFinished {
                task: task_name.to_string(),
                exit_code,
                duration: task_started_at.elapsed(),
            });
        } else {
            // Infrastructure failure — the error itself propagates to
            // stderr via the returned `Err`; this only lets a live display
            // stop repainting cleanly first.
            self.event_sink.post(TaskEvent::TaskFailed {
                task: task_name.to_string(),
            });
        }

        result
    }

    /// Ensures `name`'s container is started and *ready* (healthy, then every
    /// one of its `setup_commands` succeeded) on `network`, memoized per this
    /// one task execution via `cells` — concurrent callers reaching the same
    /// node (a diamond in the dependency graph) share one `ReadyCell`, so the
    /// second awaits the first's in-flight work instead of starting it
    /// twice. Fans out to `name`'s own dependencies (`graph[name]`)
    /// concurrently, via `try_join_all`, before doing any of its own work —
    /// this and the memoization together are what let independent branches
    /// of one task's dependency graph run at the same time while a container
    /// with dependencies of its own still waits for them first (see
    /// docs/task-lifecycle.md). No cycle guard here any more (pre-0.15.0's
    /// `resolving`/`running` params): `graph` is already proven acyclic by
    /// `build_dependency_graph`, run once, synchronously, before this is
    /// ever called.
    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn ensure_container_ready(
        &self,
        name: &str,
        graph: &HashMap<String, Vec<String>>,
        network: &str,
        cells: &Mutex<HashMap<String, ReadyCell>>,
        running: &Mutex<HashMap<String, String>>,
        no_proxy_entries: &std::collections::BTreeSet<String>,
        customisations: Option<&HashMap<String, TaskContainerCustomisation>>,
    ) -> Result<String> {
        let cell = get_or_create_cell(cells, name);
        let result = cell
            .get_or_init(|| async {
                let outcome: Result<String> = async {
                    let empty = Vec::new();
                    let dependencies = graph.get(name).unwrap_or(&empty);
                    futures::future::try_join_all(dependencies.iter().map(|dependency_name| {
                        self.ensure_container_ready(
                            dependency_name,
                            graph,
                            network,
                            cells,
                            running,
                            no_proxy_entries,
                            customisations,
                        )
                    }))
                    .await?;

                    let dependency_config = self
                        .config
                        .containers
                        .get(name)
                        .with_context(|| format!("Container '{}' not found", name))?;

                    // A `customise` entry for this container specifically —
                    // applied on top of its own base config, same precedence
                    // as a task's `run` overriding its own main container
                    // (see `Config::resolve_expressions_with_boundaries` for
                    // the validation ensuring this can never target the main
                    // task container or a container outside this task's own
                    // graph).
                    let customisation = customisations.and_then(|c| c.get(name));

                    let image = self.resolve_image(name, dependency_config).await?;
                    self.event_sink.post(TaskEvent::ImageResolved {
                        container: name.to_string(),
                    });
                    let user_mapping = self.resolve_user_mapping(dependency_config).await?;
                    let proxy_vars = self.proxy_environment_variables(no_proxy_entries);
                    // A dependency is never interactive — see
                    // `term_environment_variable`'s own docs for the
                    // interleaved-policy override this still picks up.
                    let term_var = self.term_environment_variable(false);
                    let environment = merged_environment(
                        term_var.as_ref(),
                        proxy_vars.as_ref(),
                        dependency_config.environment.as_ref(),
                        customisation.and_then(|c| c.environment.as_ref()),
                    );
                    let expanded_ports = merged_ports(
                        dependency_config.ports.as_ref(),
                        customisation.and_then(|c| c.ports.as_ref()),
                    );
                    let network_options = crate::docker::NetworkOptions {
                        additional_hostnames: dependency_config.additional_hostnames.as_ref(),
                        additional_hosts: dependency_config.additional_hosts.as_ref(),
                        ports: (self.publish_ports && !expanded_ports.is_empty())
                            .then_some(&expanded_ports),
                    };

                    let health_check = health_check_options(dependency_config);
                    let capabilities_to_add =
                        capability_names(dependency_config.capabilities_to_add.as_ref());
                    let capabilities_to_drop =
                        capability_names(dependency_config.capabilities_to_drop.as_ref());
                    let devices = device_triples(dependency_config.devices.as_ref());
                    let working_directory = customisation
                        .and_then(|c| c.working_directory.as_deref())
                        .or(dependency_config.working_directory.as_deref());
                    let container_options = crate::docker::ContainerOptions {
                        working_directory,
                        entrypoint: dependency_config.entrypoint.as_deref(),
                        labels: dependency_config.labels.as_ref(),
                        capabilities_to_add: capabilities_to_add.as_ref(),
                        capabilities_to_drop: capabilities_to_drop.as_ref(),
                        privileged: dependency_config.privileged,
                        shm_size: dependency_config.shm_size,
                        devices: devices.as_ref(),
                        enable_init_process: dependency_config.enable_init_process,
                    };

                    self.event_sink.post(TaskEvent::DependencyStarting {
                        container: name.to_string(),
                    });
                    let container_id = {
                        // Held only around the actual create+start call —
                        // matching `resolve_image`'s own placement, not the
                        // health-check wait or the readiness bookkeeping
                        // either side of it. See `max_parallelism`'s own
                        // doc comment for why starting counts against the
                        // cap but waiting for healthy doesn't.
                        let _permit = self.acquire_parallelism_permit().await;
                        let volumes = self
                            .resolve_volumes(dependency_config.volumes.as_ref())
                            .await?;
                        self.docker
                            .start_background_container(
                                name,
                                &image,
                                dependency_config.command.as_deref(),
                                volumes.as_ref(),
                                environment.as_ref(),
                                network,
                                user_mapping.as_ref(),
                                &network_options,
                                health_check.as_ref(),
                                &container_options,
                            )
                            .await?
                    };
                    self.event_sink.post(TaskEvent::DependencyStarted {
                        container: name.to_string(),
                    });

                    // Registered for cleanup *before* the readiness gate
                    // below — a dependency that starts but never becomes
                    // healthy (or whose setup command fails) still gets
                    // stopped and removed.
                    running
                        .lock()
                        .unwrap()
                        .insert(name.to_string(), container_id.clone());

                    // Batect's readiness gate (see docs/task-lifecycle.md):
                    // started isn't ready. The dependency must report
                    // healthy (immediate for a container with no health
                    // check at all), then every one of its setup commands
                    // must succeed, before anything that depends on it
                    // starts.
                    self.docker
                        .wait_for_container_healthy(&container_id)
                        .await
                        .with_context(|| format!("Container '{}' did not become healthy", name))?;
                    self.event_sink.post(TaskEvent::ContainerBecameHealthy {
                        container: name.to_string(),
                    });

                    let setup_command_total = dependency_config
                        .setup_commands
                        .as_ref()
                        .map_or(0, Vec::len);
                    for (setup_command_index, setup_command) in dependency_config
                        .setup_commands
                        .iter()
                        .flatten()
                        .enumerate()
                    {
                        // The user-facing setup-command line is the event
                        // sink's job now (see `crate::ui`) — `debug` so
                        // `RUST_LOG=info` doesn't duplicate it on stderr.
                        tracing::debug!(
                            container = name,
                            command = setup_command.command.as_str(),
                            "Running setup command"
                        );
                        self.event_sink.post(TaskEvent::RunningSetupCommand {
                            container: name.to_string(),
                            command: setup_command.command.clone(),
                            index: setup_command_index + 1,
                            total: setup_command_total,
                        });
                        let result = {
                            let _permit = self.acquire_parallelism_permit().await;
                            self.docker
                                .exec_in_container(
                                    &container_id,
                                    &setup_command.command,
                                    setup_command
                                        .working_directory
                                        .as_deref()
                                        .or(working_directory),
                                    environment.as_ref(),
                                    user_mapping.as_ref(),
                                )
                                .await
                                .with_context(|| {
                                    format!(
                                        "Failed to run setup command '{}' in container '{}'",
                                        setup_command.command, name
                                    )
                                })?
                        };
                        // The command's output, line by line — exec output
                        // arrives collected rather than streamed, so this
                        // posts after completion (success or failure; a
                        // failure's output additionally lands in the error
                        // below). Only the `all` output mode renders these —
                        // skipped entirely otherwise (see
                        // `EventSink::wants_progress_detail`) rather than
                        // allocating and posting one event per line only to
                        // have every other mode immediately discard it.
                        if self.event_sink.wants_progress_detail() {
                            for line in result.output.lines() {
                                self.event_sink.post(TaskEvent::SetupCommandOutput {
                                    container: name.to_string(),
                                    index: setup_command_index + 1,
                                    line: line.trim_end_matches('\r').to_string(),
                                });
                            }
                        }
                        if result.exit_code != 0 {
                            let output = if result.output.trim().is_empty() {
                                ", and did not produce any output".to_string()
                            } else {
                                format!(", with output:\n{}", result.output.trim())
                            };
                            anyhow::bail!(
                                "Setup command '{}' in container '{}' exited with code {}{}",
                                setup_command.command,
                                name,
                                result.exit_code,
                                output
                            );
                        }
                    }
                    if setup_command_total > 0 {
                        self.event_sink.post(TaskEvent::SetupCommandsCompleted {
                            container: name.to_string(),
                        });
                    }

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
mod tests {
    use super::*;
    use crate::config::{Container, Task, TaskRun};
    use crate::docker::DockerClient;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Records every call as a single ordered event log instead of talking to
    /// Docker, so tests can assert on dedup, cleanup, and ordering behavior
    /// (including across pull/network/sidecar/run calls) quickly and
    /// deterministically.
    type CapturedEnvironments = Arc<Mutex<HashMap<String, Option<HashMap<String, String>>>>>;
    type CapturedBuildArgs = Arc<Mutex<HashMap<String, Option<HashMap<String, String>>>>>;
    /// `(dockerfile, target)`, keyed by the tag `build_image` was called with.
    type CapturedBuildOptions = Arc<Mutex<HashMap<String, (String, Option<String>)>>>;
    /// The `buildkit` a prior `build_image` call for a given tag was given
    /// (flattened, same convention as `environment_for`).
    type CapturedBuildKitOptions =
        Arc<Mutex<HashMap<String, Option<crate::docker::BuildKitOptions>>>>;
    type CapturedImages = Arc<Mutex<HashMap<String, String>>>;
    /// The `command` a prior `start_background_container` call for a given
    /// container name was given (flattened, same convention as
    /// `environment_for`) — `run_container`'s own `command` is instead baked
    /// into the `events()` string (see `run_container`'s own push), since
    /// existing tests already assert against that; this is a separate,
    /// smaller map specifically for dependency containers, which have no
    /// such event.
    type CapturedCommands = Arc<Mutex<HashMap<String, Option<String>>>>;
    type CapturedInteractive = Arc<Mutex<HashMap<String, bool>>>;
    /// `(uid, gid, home_directory)`, keyed by container name.
    type CapturedUserMapping = Arc<Mutex<HashMap<String, Option<(u32, u32, String)>>>>;
    /// `(additional_hostnames, additional_hosts, ports)`.
    type NetworkOptionsValue = (
        Option<Vec<String>>,
        Option<HashMap<String, String>>,
        Option<Vec<(u16, u16, String)>>,
    );
    /// Keyed by container name.
    type CapturedNetworkOptions = Arc<Mutex<HashMap<String, NetworkOptionsValue>>>;
    /// Keyed by container name.
    type CapturedHealthChecks =
        Arc<Mutex<HashMap<String, Option<crate::docker::HealthCheckOptions>>>>;
    /// The `ContainerOptions` a prior `run_container`/
    /// `start_background_container` call for a given container name was
    /// given (see `working_directory_for`/`entrypoint_for`/`labels_for`/
    /// `capabilities_to_add_for`/`capabilities_to_drop_for`). A named struct
    /// (not a positional tuple) since `ContainerOptions` keeps growing —
    /// see `ROADMAP.md`'s 0.13.0 entry.
    #[derive(Debug, Clone, Default)]
    struct ContainerOptionsValue {
        working_directory: Option<String>,
        entrypoint: Option<String>,
        labels: Option<HashMap<String, String>>,
        capabilities_to_add: Option<Vec<String>>,
        capabilities_to_drop: Option<Vec<String>>,
        privileged: Option<bool>,
        shm_size: Option<i64>,
        devices: Option<Vec<(String, String, Option<String>)>>,
        enable_init_process: Option<bool>,
    }
    type CapturedContainerOptions = Arc<Mutex<HashMap<String, ContainerOptionsValue>>>;
    /// `(working_directory, environment, (uid, gid))`, keyed by the exec'd
    /// command string.
    type ExecValue = (
        Option<String>,
        Option<HashMap<String, String>>,
        Option<(u32, u32)>,
    );
    type CapturedExecs = Arc<Mutex<HashMap<String, ExecValue>>>;

    #[derive(Clone)]
    struct FakeContainerRuntime {
        events: Arc<Mutex<Vec<String>>>,
        fail_run: Arc<Mutex<bool>>,
        // Captured separately from `events` (rather than folded into its
        // strings) so the many existing exact-string event assertions don't
        // have to change shape just because environment support was added.
        environments: CapturedEnvironments,
        // Keyed by the tag `build_image` was called with.
        build_args: CapturedBuildArgs,
        // `(dockerfile, target)` a prior `build_image` call for a given tag
        // was given (see `build_options_for`).
        build_options: CapturedBuildOptions,
        // The `buildkit` a prior `build_image` call for a given tag was
        // given (see `buildkit_options_for`).
        buildkit_options: CapturedBuildKitOptions,
        // The `image` a `run_container`/`start_background_container` call
        // for a given container name actually used — lets tests prove a
        // built tag (not just a pulled image) reached the run, without
        // changing the existing exact-string `events()` assertions.
        images: CapturedImages,
        // The `command` a prior `start_background_container` call for a
        // given container name was given (see `command_for`).
        commands: CapturedCommands,
        // The `interactive` a prior `run_container` call for a given
        // container name was given — lets tests prove interactive
        // eligibility is scoped to only the top-level requested task's own
        // container (see `interactive_for`).
        interactive: CapturedInteractive,
        // The `user_mapping` a prior `run_container`/`start_background_container`
        // call for a given container name was given (see `user_mapping_for`).
        user_mapping: CapturedUserMapping,
        // What `network_exists` reports — defaults to `true` so tests that
        // don't care about `--use-network` aren't affected.
        network_exists_result: Arc<Mutex<bool>>,
        // The `network_options` a prior `run_container`/`start_background_container`
        // call for a given container name was given (see `network_options_for`).
        network_options: CapturedNetworkOptions,
        // The `health_check` a prior `run_container`/`start_background_container`
        // call for a given container name was given (see `health_check_for`).
        health_checks: CapturedHealthChecks,
        // The `container_options` a prior `run_container`/`start_background_container`
        // call for a given container name was given (see `container_options_for`).
        container_options: CapturedContainerOptions,
        // The options a prior `exec_in_container` call for a given command
        // was given (see `exec_for`).
        execs: CapturedExecs,
        // Container id whose `wait_for_container_healthy` reports unhealthy
        // (see `with_unhealthy_container`).
        unhealthy_container: Arc<Mutex<Option<String>>>,
        // Command whose `exec_in_container` reports a non-zero exit (see
        // `with_failing_setup_command`).
        failing_setup_command: Arc<Mutex<Option<String>>>,
        // Images `image_exists_locally` reports as already present (see
        // `with_local_image`) — defaults to empty, so tests that don't care
        // about `image_pull_policy` see the "always needs a pull" behavior
        // that matches an `IfNotPresent` container whose image is missing.
        locally_present_images: Arc<Mutex<HashSet<String>>>,
        // Artificial `tokio::time::sleep` durations `start_background_container`/
        // `pull_image` wait out before doing anything else (see
        // `with_start_delay`/`with_pull_delay`) — lets a `#[tokio::test(start_paused
        // = true)]` test prove two independent operations actually overlap in
        // (virtual) time, rather than just asserting on event order/counts.
        start_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
        pull_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
        // Same idea, for `exec_in_container` (keyed by command) and
        // `wait_for_container_healthy` (keyed by container id) — used to
        // prove `--max-parallelism` serializes setup-command execution but
        // deliberately leaves the health-check wait itself unbounded (see
        // `TaskEngine::max_parallelism`'s own doc comment for why).
        exec_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
        health_check_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    }

    impl Default for FakeContainerRuntime {
        fn default() -> Self {
            Self {
                events: Default::default(),
                fail_run: Default::default(),
                environments: Default::default(),
                build_args: Default::default(),
                build_options: Default::default(),
                buildkit_options: Default::default(),
                images: Default::default(),
                commands: Default::default(),
                interactive: Default::default(),
                user_mapping: Default::default(),
                network_exists_result: Arc::new(Mutex::new(true)),
                network_options: Default::default(),
                health_checks: Default::default(),
                container_options: Default::default(),
                execs: Default::default(),
                unhealthy_container: Default::default(),
                failing_setup_command: Default::default(),
                locally_present_images: Default::default(),
                start_delays: Default::default(),
                pull_delays: Default::default(),
                exec_delays: Default::default(),
                health_check_delays: Default::default(),
            }
        }
    }

    impl FakeContainerRuntime {
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        fn push(&self, event: String) {
            self.events.lock().unwrap().push(event);
        }

        /// Makes `run_container` simulate the task's own command exiting
        /// non-zero, the same way the real `DockerClient` does.
        fn failing_run(self) -> Self {
            *self.fail_run.lock().unwrap() = true;
            self
        }

        /// Makes `network_exists` report `false` — simulates `--use-network`
        /// pointing at a network that doesn't exist.
        fn without_existing_network(self) -> Self {
            *self.network_exists_result.lock().unwrap() = false;
            self
        }

        /// Makes `wait_for_container_healthy` fail for the given container
        /// *name* — simulates a dependency that starts but is reported
        /// unhealthy (or exits) instead of becoming healthy.
        fn with_unhealthy_container(self, name: &str) -> Self {
            *self.unhealthy_container.lock().unwrap() = Some(format!("sidecar-id-{name}"));
            self
        }

        /// Makes `exec_in_container` report exit code 1 (with some output)
        /// for the given command — simulates a failing setup command.
        fn with_failing_setup_command(self, command: &str) -> Self {
            *self.failing_setup_command.lock().unwrap() = Some(command.to_string());
            self
        }

        /// Makes `image_exists_locally` report `true` for `image` — used to
        /// exercise `image_pull_policy: IfNotPresent` skipping a pull.
        fn with_local_image(self, image: &str) -> Self {
            self.locally_present_images
                .lock()
                .unwrap()
                .insert(image.to_string());
            self
        }

        /// Makes `start_background_container` for container name `name`
        /// artificially `tokio::time::sleep` for `delay` before doing
        /// anything else — used with `#[tokio::test(start_paused = true)]`
        /// to prove two independent dependencies actually start
        /// concurrently (overlapping in virtual time), not sequentially.
        fn with_start_delay(self, name: &str, delay: std::time::Duration) -> Self {
            self.start_delays
                .lock()
                .unwrap()
                .insert(name.to_string(), delay);
            self
        }

        /// Makes `pull_image` for `image` artificially `tokio::time::sleep`
        /// for `delay` before doing anything else — used with
        /// `#[tokio::test(start_paused = true)]` to prove an image shared by
        /// two concurrently-starting dependencies is still only pulled once,
        /// even when the race window between "decided to pull" and "pull
        /// finished" is held open long enough for both to actually overlap.
        fn with_pull_delay(self, image: &str, delay: std::time::Duration) -> Self {
            self.pull_delays
                .lock()
                .unwrap()
                .insert(image.to_string(), delay);
            self
        }

        /// Makes `exec_in_container` for `command` artificially
        /// `tokio::time::sleep` for `delay` before doing anything else —
        /// used to prove `--max-parallelism` serializes setup-command
        /// execution across different containers.
        fn with_exec_delay(self, command: &str, delay: std::time::Duration) -> Self {
            self.exec_delays
                .lock()
                .unwrap()
                .insert(command.to_string(), delay);
            self
        }

        /// Makes `wait_for_container_healthy` for dependency `name`
        /// artificially `tokio::time::sleep` for `delay` before doing
        /// anything else — used to prove `--max-parallelism` deliberately
        /// leaves the health-check wait itself unbounded (see
        /// `TaskEngine::max_parallelism`'s own doc comment for why).
        fn with_health_check_delay(self, name: &str, delay: std::time::Duration) -> Self {
            self.health_check_delays
                .lock()
                .unwrap()
                .insert(format!("sidecar-id-{name}"), delay);
            self
        }

        /// The `environment` a prior `run_container`/`start_background_container`
        /// call for `name` was given (flattened: `None` covers both "never
        /// called" and "called with no environment").
        fn environment_for(&self, name: &str) -> Option<HashMap<String, String>> {
            self.environments
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .flatten()
        }

        /// The `build_args` a prior `build_image` call for `tag` was given
        /// (flattened, same convention as `environment_for`).
        fn build_args_for(&self, tag: &str) -> Option<HashMap<String, String>> {
            self.build_args.lock().unwrap().get(tag).cloned().flatten()
        }

        /// The `(dockerfile, target)` a prior `build_image` call for `tag`
        /// was given.
        fn build_options_for(&self, tag: &str) -> Option<(String, Option<String>)> {
            self.build_options.lock().unwrap().get(tag).cloned()
        }

        /// The `buildkit` a prior `build_image` call for `tag` was given
        /// (flattened, same convention as `environment_for`).
        fn buildkit_options_for(&self, tag: &str) -> Option<crate::docker::BuildKitOptions> {
            self.buildkit_options
                .lock()
                .unwrap()
                .get(tag)
                .cloned()
                .flatten()
        }

        /// The `image` a prior `run_container`/`start_background_container`
        /// call for `name` was given.
        fn image_for(&self, name: &str) -> Option<String> {
            self.images.lock().unwrap().get(name).cloned()
        }

        /// The `command` a prior `start_background_container` call for
        /// `name` was given (flattened, same convention as
        /// `environment_for`).
        fn command_for(&self, name: &str) -> Option<String> {
            self.commands.lock().unwrap().get(name).cloned().flatten()
        }

        /// The `interactive` a prior `run_container` call for `name` was
        /// given.
        fn interactive_for(&self, name: &str) -> Option<bool> {
            self.interactive.lock().unwrap().get(name).copied()
        }

        /// The `(uid, gid, home_directory)` a prior `run_container`/
        /// `start_background_container` call for `name` was given
        /// (flattened, same convention as `environment_for`).
        fn user_mapping_for(&self, name: &str) -> Option<(u32, u32, String)> {
            self.user_mapping
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .flatten()
        }

        /// The `(additional_hostnames, additional_hosts)` a prior
        /// `run_container`/`start_background_container` call for `name` was
        /// given.
        fn network_options_for(&self, name: &str) -> Option<NetworkOptionsValue> {
            self.network_options.lock().unwrap().get(name).cloned()
        }

        /// The `health_check` a prior `run_container`/
        /// `start_background_container` call for `name` was given
        /// (flattened, same convention as `environment_for`).
        fn health_check_for(&self, name: &str) -> Option<crate::docker::HealthCheckOptions> {
            self.health_checks
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .flatten()
        }

        /// The `(working_directory, environment, (uid, gid))` a prior
        /// `exec_in_container` call for `command` was given.
        fn exec_for(&self, command: &str) -> Option<ExecValue> {
            self.execs.lock().unwrap().get(command).cloned()
        }

        /// The `container_options.working_directory` a prior `run_container`/
        /// `start_background_container` call for `name` was given.
        fn working_directory_for(&self, name: &str) -> Option<String> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.working_directory.clone())
        }

        /// The `container_options.entrypoint` a prior `run_container`/
        /// `start_background_container` call for `name` was given.
        fn entrypoint_for(&self, name: &str) -> Option<String> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.entrypoint.clone())
        }

        /// The `container_options.labels` a prior `run_container`/
        /// `start_background_container` call for `name` was given.
        fn labels_for(&self, name: &str) -> Option<HashMap<String, String>> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.labels.clone())
        }

        /// The `container_options.capabilities_to_add` a prior
        /// `run_container`/`start_background_container` call for `name` was
        /// given.
        fn capabilities_to_add_for(&self, name: &str) -> Option<Vec<String>> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.capabilities_to_add.clone())
        }

        /// The `container_options.capabilities_to_drop` a prior
        /// `run_container`/`start_background_container` call for `name` was
        /// given.
        fn capabilities_to_drop_for(&self, name: &str) -> Option<Vec<String>> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.capabilities_to_drop.clone())
        }

        /// The `container_options.privileged` a prior `run_container`/
        /// `start_background_container` call for `name` was given.
        fn privileged_for(&self, name: &str) -> Option<bool> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.privileged)
        }

        /// The `container_options.shm_size` a prior `run_container`/
        /// `start_background_container` call for `name` was given.
        fn shm_size_for(&self, name: &str) -> Option<i64> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.shm_size)
        }

        /// The `container_options.devices` a prior `run_container`/
        /// `start_background_container` call for `name` was given.
        fn devices_for(&self, name: &str) -> Option<Vec<(String, String, Option<String>)>> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.devices.clone())
        }

        /// The `container_options.enable_init_process` a prior
        /// `run_container`/`start_background_container` call for `name`
        /// was given.
        fn enable_init_process_for(&self, name: &str) -> Option<bool> {
            self.container_options
                .lock()
                .unwrap()
                .get(name)
                .and_then(|options| options.enable_init_process)
        }
    }

    #[async_trait::async_trait]
    impl ContainerRuntime for FakeContainerRuntime {
        async fn pull_image(&self, image: &str) -> Result<()> {
            let delay = self.pull_delays.lock().unwrap().get(image).copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            self.push(format!("pull:{image}"));
            Ok(())
        }

        async fn image_exists_locally(&self, image: &str) -> Result<bool> {
            Ok(self.locally_present_images.lock().unwrap().contains(image))
        }

        async fn build_image(
            &self,
            build_directory: &Path,
            dockerfile: &str,
            build_args: Option<&HashMap<String, String>>,
            target: Option<&str>,
            buildkit: Option<&crate::docker::BuildKitOptions>,
            tag: &str,
        ) -> Result<String> {
            self.build_args
                .lock()
                .unwrap()
                .insert(tag.to_string(), build_args.cloned());
            self.build_options.lock().unwrap().insert(
                tag.to_string(),
                (dockerfile.to_string(), target.map(|t| t.to_string())),
            );
            self.buildkit_options
                .lock()
                .unwrap()
                .insert(tag.to_string(), buildkit.cloned());
            self.push(format!("build:{tag}:{}", build_directory.display()));
            // Real Docker returns an image ID distinct from the tag; the fake
            // has no such concept, so it just echoes the tag back — tests
            // that assert `image_for(name) == tag` still hold either way.
            Ok(tag.to_string())
        }

        async fn tag_image(&self, image_id: &str, tags: &[String]) -> Result<()> {
            for tag in tags {
                self.push(format!("tag:{image_id}:{tag}"));
            }
            Ok(())
        }

        async fn create_network(&self, name: &str) -> Result<()> {
            self.push(format!("network-create:{name}"));
            Ok(())
        }

        async fn remove_network(&self, name: &str) -> Result<()> {
            self.push(format!("network-remove:{name}"));
            Ok(())
        }

        async fn network_exists(&self, name: &str) -> Result<bool> {
            self.push(format!("network-exists:{name}"));
            Ok(*self.network_exists_result.lock().unwrap())
        }

        async fn start_background_container(
            &self,
            alias: &str,
            image: &str,
            command: Option<&str>,
            _volumes: Option<&Vec<String>>,
            environment: Option<&HashMap<String, String>>,
            network: &str,
            user_mapping: Option<&crate::docker::UserMapping>,
            network_options: &crate::docker::NetworkOptions,
            health_check: Option<&crate::docker::HealthCheckOptions>,
            container_options: &crate::docker::ContainerOptions,
        ) -> Result<String> {
            let delay = self.start_delays.lock().unwrap().get(alias).copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            self.commands
                .lock()
                .unwrap()
                .insert(alias.to_string(), command.map(str::to_string));
            self.environments
                .lock()
                .unwrap()
                .insert(alias.to_string(), environment.cloned());
            self.images
                .lock()
                .unwrap()
                .insert(alias.to_string(), image.to_string());
            self.user_mapping.lock().unwrap().insert(
                alias.to_string(),
                user_mapping.map(|m| (m.user.uid, m.user.gid, m.home_directory.clone())),
            );
            self.network_options.lock().unwrap().insert(
                alias.to_string(),
                (
                    network_options.additional_hostnames.cloned(),
                    network_options.additional_hosts.cloned(),
                    network_options.ports.cloned(),
                ),
            );
            self.health_checks
                .lock()
                .unwrap()
                .insert(alias.to_string(), health_check.cloned());
            self.container_options.lock().unwrap().insert(
                alias.to_string(),
                ContainerOptionsValue {
                    working_directory: container_options.working_directory.map(str::to_string),
                    entrypoint: container_options.entrypoint.map(str::to_string),
                    labels: container_options.labels.cloned(),
                    capabilities_to_add: container_options.capabilities_to_add.cloned(),
                    capabilities_to_drop: container_options.capabilities_to_drop.cloned(),
                    privileged: container_options.privileged,
                    shm_size: container_options.shm_size,
                    devices: container_options.devices.cloned(),
                    enable_init_process: container_options.enable_init_process,
                },
            );
            self.push(format!("sidecar-start:{alias}:{network}"));
            Ok(format!("sidecar-id-{alias}"))
        }

        async fn wait_for_container_healthy(&self, container_id: &str) -> Result<()> {
            let delay = self
                .health_check_delays
                .lock()
                .unwrap()
                .get(container_id)
                .copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            self.push(format!("wait-healthy:{container_id}"));
            if self.unhealthy_container.lock().unwrap().as_deref() == Some(container_id) {
                anyhow::bail!(
                    "The configured health check did not indicate that the container was \
                     healthy within the timeout period."
                );
            }
            Ok(())
        }

        async fn exec_in_container(
            &self,
            container_id: &str,
            command: &str,
            working_directory: Option<&str>,
            environment: Option<&HashMap<String, String>>,
            user_mapping: Option<&crate::docker::UserMapping>,
        ) -> Result<crate::docker::ExecResult> {
            let delay = self.exec_delays.lock().unwrap().get(command).copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            self.execs.lock().unwrap().insert(
                command.to_string(),
                (
                    working_directory.map(str::to_string),
                    environment.cloned(),
                    user_mapping.map(|m| (m.user.uid, m.user.gid)),
                ),
            );
            self.push(format!("exec:{container_id}:{command}"));
            let failing = self.failing_setup_command.lock().unwrap().as_deref() == Some(command);
            Ok(crate::docker::ExecResult {
                exit_code: if failing { 1 } else { 0 },
                output: if failing {
                    "something went wrong\n".to_string()
                } else {
                    String::new()
                },
            })
        }

        async fn stop_and_remove_container(&self, container_id: &str) -> Result<()> {
            self.push(format!("sidecar-stop:{container_id}"));
            Ok(())
        }

        async fn run_container(
            &self,
            name: &str,
            image: &str,
            command: Option<&str>,
            additional_args: &[String],
            _volumes: Option<&Vec<String>>,
            environment: Option<&HashMap<String, String>>,
            network: &str,
            interactive: bool,
            user_mapping: Option<&crate::docker::UserMapping>,
            network_options: &crate::docker::NetworkOptions,
            health_check: Option<&crate::docker::HealthCheckOptions>,
            container_options: &crate::docker::ContainerOptions,
            remove_on_exit: bool,
        ) -> Result<()> {
            self.environments
                .lock()
                .unwrap()
                .insert(name.to_string(), environment.cloned());
            self.images
                .lock()
                .unwrap()
                .insert(name.to_string(), image.to_string());
            self.interactive
                .lock()
                .unwrap()
                .insert(name.to_string(), interactive);
            self.user_mapping.lock().unwrap().insert(
                name.to_string(),
                user_mapping.map(|m| (m.user.uid, m.user.gid, m.home_directory.clone())),
            );
            self.network_options.lock().unwrap().insert(
                name.to_string(),
                (
                    network_options.additional_hostnames.cloned(),
                    network_options.additional_hosts.cloned(),
                    network_options.ports.cloned(),
                ),
            );
            self.health_checks
                .lock()
                .unwrap()
                .insert(name.to_string(), health_check.cloned());
            self.container_options.lock().unwrap().insert(
                name.to_string(),
                ContainerOptionsValue {
                    working_directory: container_options.working_directory.map(str::to_string),
                    entrypoint: container_options.entrypoint.map(str::to_string),
                    labels: container_options.labels.cloned(),
                    capabilities_to_add: container_options.capabilities_to_add.cloned(),
                    capabilities_to_drop: container_options.capabilities_to_drop.cloned(),
                    privileged: container_options.privileged,
                    shm_size: container_options.shm_size,
                    devices: container_options.devices.cloned(),
                    enable_init_process: container_options.enable_init_process,
                },
            );
            self.push(format!(
                "run:{name}:{}:args=[{}]:{}",
                command.unwrap_or_default(),
                additional_args.join(","),
                network
            ));
            self.push(format!("remove_on_exit:{name}:{remove_on_exit}"));
            if *self.fail_run.lock().unwrap() {
                return Err(crate::docker::ContainerExitedNonZero { exit_code: 1 }.into());
            }
            Ok(())
        }
    }

    fn container(image: &str, dependencies: Option<Vec<String>>) -> Container {
        Container {
            build_args: None,
            image: Some(image.to_string()),
            image_pull_policy: None,
            build_directory: None,
            dockerfile: None,
            build_target: None,
            build_secrets: None,
            build_ssh: None,
            volumes: None,
            dependencies,
            environment: None,
            run_as_current_user: None,
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
            working_directory: None,
            command: None,
            entrypoint: None,
            labels: None,
            capabilities_to_add: None,
            capabilities_to_drop: None,
            privileged: None,
            shm_size: None,
            devices: None,
            enable_init_process: None,
            health_check: None,
            setup_commands: None,
        }
    }

    fn task(container: &str, command: &str) -> Task {
        Task {
            run: Some(TaskRun {
                container: container.to_string(),
                command: Some(command.to_string()),
                environment: None,
                ports: None,
                working_directory: None,
                entrypoint: None,
            }),
            dependencies: None,
            prerequisites: None,
            description: None,
            group: None,
            customise: None,
        }
    }

    fn config_with_cycle() -> Config {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                build_args: None,
                image: Some("alpine:3.18".to_string()),
                image_pull_policy: None,
                build_directory: None,
                dockerfile: None,
                build_target: None,
                build_secrets: None,
                build_ssh: None,
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
                additional_hostnames: None,
                additional_hosts: None,
                ports: None,
                working_directory: None,
                command: None,
                entrypoint: None,
                labels: None,
                capabilities_to_add: None,
                capabilities_to_drop: None,
                privileged: None,
                shm_size: None,
                devices: None,
                enable_init_process: None,
                health_check: None,
                setup_commands: None,
            },
        );

        let mut tasks = HashMap::new();
        tasks.insert(
            "a".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "build-env".to_string(),
                    command: None,
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: Some(vec!["b".to_string()]),
                description: None,
                group: None,
                customise: None,
            },
        );
        tasks.insert(
            "b".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "build-env".to_string(),
                    command: None,
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: Some(vec!["a".to_string()]),
                description: None,
                group: None,
                customise: None,
            },
        );

        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        }
    }

    fn empty_config() -> Config {
        Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: None,
        }
    }

    /// Mirrors the diamond-shaped dependency graph in the sample `batect.yml`:
    /// two tasks share a common prerequisite, and a final task depends on both.
    fn config_with_shared_prerequisite() -> Config {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                build_args: None,
                image: Some("alpine:3.18".to_string()),
                image_pull_policy: None,
                build_directory: None,
                dockerfile: None,
                build_target: None,
                build_secrets: None,
                build_ssh: None,
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
                additional_hostnames: None,
                additional_hosts: None,
                ports: None,
                working_directory: None,
                command: None,
                entrypoint: None,
                labels: None,
                capabilities_to_add: None,
                capabilities_to_drop: None,
                privileged: None,
                shm_size: None,
                devices: None,
                enable_init_process: None,
                health_check: None,
                setup_commands: None,
            },
        );

        let task = |command: &str, prerequisites: Option<Vec<String>>| Task {
            run: Some(TaskRun {
                container: "build-env".to_string(),
                command: Some(command.to_string()),
                environment: None,
                ports: None,
                working_directory: None,
                entrypoint: None,
            }),
            prerequisites,
            dependencies: None,
            description: None,
            group: None,
            customise: None,
        };

        let mut tasks = HashMap::new();
        tasks.insert("shared-prereq".to_string(), task("shared-prereq", None));
        tasks.insert(
            "prereq-task".to_string(),
            task("prereq-task", Some(vec!["shared-prereq".to_string()])),
        );
        tasks.insert(
            "list-volume-task".to_string(),
            task("list-volume-task", Some(vec!["shared-prereq".to_string()])),
        );
        tasks.insert(
            "test-task".to_string(),
            task(
                "test-task",
                Some(vec![
                    "prereq-task".to_string(),
                    "list-volume-task".to_string(),
                ]),
            ),
        );

        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        }
    }

    #[tokio::test]
    async fn shared_prerequisite_runs_once_and_image_pulled_once() {
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker.clone());

        engine.run_task("test-task", &[]).await.unwrap();

        let events = docker.events();

        // The image backing every task is the same, so it should only be pulled once
        // even though four tasks reference it.
        let pulls: Vec<_> = events.iter().filter(|e| e.starts_with("pull:")).collect();
        assert_eq!(pulls, vec!["pull:alpine:3.18"]);

        // Every task gets its own isolated network, even though none of
        // these declare `dependencies`.
        let networks_created: Vec<_> = events
            .iter()
            .filter_map(|e| e.strip_prefix("network-create:"))
            .collect();
        assert_eq!(
            networks_created.len(),
            4,
            "each of the 4 tasks should get its own network: {events:?}"
        );

        // "shared-prereq" is a prerequisite of both "prereq-task" and
        // "list-volume-task", but must only run once, before either of them,
        // and "test-task" must run last.
        let runs: Vec<_> = events
            .iter()
            .filter(|e| e.starts_with("run:"))
            .cloned()
            .collect();
        assert_eq!(runs.len(), 4);
        for (run, network) in runs.iter().zip(networks_created.iter()) {
            assert!(
                run.ends_with(&format!(":{network}")),
                "run event should be on its own task's network: {run}"
            );
        }
        assert!(runs[0].starts_with("run:build-env:shared-prereq:args=[]:"));
        assert!(runs[3].starts_with("run:build-env:test-task:args=[]:"));
        assert!(runs[1..3]
            .iter()
            .any(|r| r.starts_with("run:build-env:prereq-task:args=[]:")));
        assert!(runs[1..3]
            .iter()
            .any(|r| r.starts_with("run:build-env:list-volume-task:args=[]:")));
    }

    fn config_with_wildcard_prerequisite_tasks() -> Config {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));

        let mut tasks = HashMap::new();
        tasks.insert("lint:bar".to_string(), task("build-env", "lint-bar"));
        tasks.insert("lint:foo".to_string(), task("build-env", "lint-foo"));
        tasks.insert("build".to_string(), task("build-env", "build"));

        let mut ci_task = task("build-env", "ci");
        ci_task.prerequisites = Some(vec!["lint:*".to_string()]);
        tasks.insert("ci".to_string(), ci_task);

        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        }
    }

    #[tokio::test]
    async fn wildcard_prerequisite_expands_to_matching_tasks_in_alphabetical_order() {
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config_with_wildcard_prerequisite_tasks(), docker.clone());

        engine.run_task("ci", &[]).await.unwrap();

        let events = docker.events();
        let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
        assert_eq!(
            runs.len(),
            3,
            "'lint:*' should match exactly 'lint:bar' and 'lint:foo', not 'build': {events:?}"
        );
        assert!(
            runs[0].starts_with("run:build-env:lint-bar:"),
            "'lint:bar' should run before 'lint:foo' (alphabetical order): {events:?}"
        );
        assert!(runs[1].starts_with("run:build-env:lint-foo:"));
        assert!(runs[2].starts_with("run:build-env:ci:"));
    }

    #[tokio::test]
    async fn wildcard_prerequisite_matching_no_tasks_is_not_an_error() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        let mut ci_task = task("build-env", "ci");
        ci_task.prerequisites = Some(vec!["nonexistent:*".to_string()]);
        tasks.insert("ci".to_string(), ci_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("ci", &[]).await.unwrap();

        let events = docker.events();
        let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
        assert_eq!(
            runs.len(),
            1,
            "only 'ci' itself should run — a wildcard matching nothing isn't an error: {events:?}"
        );
    }

    #[tokio::test]
    async fn explicit_prerequisite_and_overlapping_wildcard_only_runs_once() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("lint:foo".to_string(), task("build-env", "lint-foo"));
        let mut ci_task = task("build-env", "ci");
        ci_task.prerequisites = Some(vec!["lint:foo".to_string(), "lint:*".to_string()]);
        tasks.insert("ci".to_string(), ci_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("ci", &[]).await.unwrap();

        let events = docker.events();
        let lint_foo_runs = events
            .iter()
            .filter(|e| e.starts_with("run:build-env:lint-foo:"))
            .count();
        assert_eq!(
            lint_foo_runs, 1,
            "named explicitly and also matched by a wildcard — should still only run once: {events:?}"
        );
    }

    #[tokio::test]
    async fn nonexistent_literal_prerequisite_still_errors() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        let mut ci_task = task("build-env", "ci");
        ci_task.prerequisites = Some(vec!["does-not-exist".to_string()]);
        tasks.insert("ci".to_string(), ci_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        let err = engine.run_task("ci", &[]).await.unwrap_err();
        assert!(err.to_string().contains("Task 'does-not-exist' not found"));
    }

    #[tokio::test]
    async fn wildcard_pattern_with_multiple_asterisks_matches() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert(
            "lint:foo:unit".to_string(),
            task("build-env", "lint-foo-unit"),
        );
        tasks.insert(
            "lint:bar:unit".to_string(),
            task("build-env", "lint-bar-unit"),
        );
        tasks.insert(
            "lint:foo:integration".to_string(),
            task("build-env", "lint-foo-integration"),
        );
        let mut ci_task = task("build-env", "ci");
        ci_task.prerequisites = Some(vec!["lint:*:unit".to_string()]);
        tasks.insert("ci".to_string(), ci_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("ci", &[]).await.unwrap();

        let events = docker.events();
        let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
        assert_eq!(runs.len(), 3, "events: {events:?}");
        assert!(
            !events
                .iter()
                .any(|e| e.starts_with("run:build-env:lint-foo-integration:")),
            "'lint:*:unit' should not match 'lint:foo:integration': {events:?}"
        );
    }

    #[test]
    fn wildcard_expansion_treats_regex_metacharacters_in_task_names_literally() {
        fn minimal_task() -> Task {
            Task {
                run: None,
                prerequisites: None,
                dependencies: None,
                description: None,
                group: None,
                customise: None,
            }
        }

        let mut tasks = HashMap::new();
        tasks.insert("build.env".to_string(), minimal_task());
        tasks.insert("buildXenv".to_string(), minimal_task());

        let expanded = expand_prerequisite_wildcards(&tasks, &["build.*".to_string()]).unwrap();

        assert_eq!(
            expanded,
            vec!["build.env".to_string()],
            "the literal '.' in the pattern should only match a literal '.', not any character \
             (so 'buildXenv' must not match)"
        );
    }

    #[tokio::test]
    async fn a_task_with_only_prerequisites_and_no_run_still_runs_its_prerequisites() {
        let docker = FakeContainerRuntime::default();
        let mut config = config_with_shared_prerequisite();
        config.tasks.get_mut("test-task").unwrap().run = None;
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test-task", &[]).await.unwrap();

        let events = docker.events();

        // "test-task" itself has no `run`, so it gets no container and no
        // network of its own — only its three (transitive) prerequisites do.
        let networks_created = events
            .iter()
            .filter(|e| e.starts_with("network-create:"))
            .count();
        assert_eq!(networks_created, 3, "events: {events:?}");

        let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
        assert_eq!(runs.len(), 3, "events: {events:?}");
        assert!(runs
            .iter()
            .any(|r| r.starts_with("run:build-env:shared-prereq:")));
        assert!(runs
            .iter()
            .any(|r| r.starts_with("run:build-env:prereq-task:")));
        assert!(runs
            .iter()
            .any(|r| r.starts_with("run:build-env:list-volume-task:")));
    }

    #[tokio::test]
    async fn additional_args_reach_only_the_requested_task_not_its_prerequisites() {
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker.clone());

        let extra_args = vec!["--verbose".to_string(), "arg with spaces".to_string()];
        engine.run_task("test-task", &extra_args).await.unwrap();

        let events = docker.events();
        let runs: Vec<_> = events
            .iter()
            .filter(|e| e.starts_with("run:"))
            .cloned()
            .collect();
        assert_eq!(runs.len(), 4);

        // Only "test-task" (the one explicitly requested) gets the args;
        // its prerequisites ("shared-prereq", "prereq-task",
        // "list-volume-task") all still run with none.
        assert!(runs[3].starts_with("run:build-env:test-task:args=[--verbose,arg with spaces]:"));
        for run in &runs[0..3] {
            assert!(
                run.contains("args=[]"),
                "prerequisite should not receive additional args: {run}"
            );
        }
    }

    #[tokio::test]
    async fn without_prerequisites_skips_the_named_tasks_own_prerequisites() {
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker.clone())
            .without_prerequisites();

        engine.run_task("test-task", &[]).await.unwrap();

        let events = docker.events();
        let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
        assert_eq!(runs.len(), 1, "events: {events:?}");
        assert!(runs[0].starts_with("run:build-env:test-task:args=[]:"));
    }

    #[tokio::test]
    async fn without_prerequisites_scopes_to_whichever_task_is_named_as_top_level() {
        // The flag scopes to whichever task is actually named on the command
        // line (whatever `run_task` is called with), not a task hardcoded
        // inside the engine — running "prereq-task" directly makes *it* the
        // top-level task this time, so *its* own prerequisite
        // ("shared-prereq") is what gets skipped.
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker.clone())
            .without_prerequisites();

        engine.run_task("prereq-task", &[]).await.unwrap();

        let events = docker.events();
        let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
        assert_eq!(runs.len(), 1, "events: {events:?}");
        assert!(runs[0].starts_with("run:build-env:prereq-task:args=[]:"));
    }

    #[tokio::test]
    async fn only_the_top_level_tasks_own_container_run_is_interactive_eligible() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.interactive_for("app"),
            Some(true),
            "the task actually named on the command line is interactive-eligible"
        );
    }

    #[tokio::test]
    async fn prerequisite_tasks_own_container_is_never_interactive() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        containers.insert("setup".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("setup".to_string(), task("setup", "echo setting up"));
        tasks.insert(
            "run".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "app".to_string(),
                    command: Some("echo hi".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: Some(vec!["setup".to_string()]),
                description: None,
                group: None,
                customise: None,
            },
        );
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.interactive_for("setup"),
            Some(false),
            "a prerequisite's own container should never be interactive-eligible"
        );
        assert_eq!(
            docker.interactive_for("app"),
            Some(true),
            "the top-level requested task's own container should still be interactive-eligible"
        );
    }

    fn container_with_run_as_current_user(
        image: &str,
        dependencies: Option<Vec<String>>,
        home_directory: &str,
    ) -> Container {
        Container {
            build_args: None,
            image: Some(image.to_string()),
            image_pull_policy: None,
            build_directory: None,
            dockerfile: None,
            build_target: None,
            build_secrets: None,
            build_ssh: None,
            volumes: None,
            dependencies,
            environment: None,
            run_as_current_user: Some(crate::config::RunAsCurrentUser {
                enabled: true,
                home_directory: Some(home_directory.to_string()),
            }),
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
            working_directory: None,
            command: None,
            entrypoint: None,
            labels: None,
            capabilities_to_add: None,
            capabilities_to_drop: None,
            privileged: None,
            shm_size: None,
            devices: None,
            enable_init_process: None,
            health_check: None,
            setup_commands: None,
        }
    }

    #[tokio::test]
    async fn run_as_current_user_reaches_the_container() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container_with_run_as_current_user("alpine:3.18", None, "/home/container-user"),
        );
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        let expected_user = crate::user::current_user().unwrap();
        assert_eq!(
            docker.user_mapping_for("app"),
            Some((
                expected_user.uid,
                expected_user.gid,
                "/home/container-user".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn a_dependencys_run_as_current_user_is_independent_of_its_own_containers() {
        let mut containers = HashMap::new();
        containers.insert(
            "database".to_string(),
            container_with_run_as_current_user("alpine:3.18", None, "/home/container-user"),
        );
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let expected_user = crate::user::current_user().unwrap();
        assert_eq!(
            docker.user_mapping_for("database"),
            Some((
                expected_user.uid,
                expected_user.gid,
                "/home/container-user".to_string()
            )),
            "the dependency's own run_as_current_user should be applied"
        );
        assert_eq!(
            docker.user_mapping_for("app"),
            None,
            "the task's own container has no run_as_current_user set, regardless of its dependency's"
        );
    }

    #[tokio::test]
    async fn container_without_run_as_current_user_reaches_the_container_with_no_mapping() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(docker.user_mapping_for("app"), None);
    }

    fn container_with_network_options(
        image: &str,
        dependencies: Option<Vec<String>>,
        additional_hostnames: Option<Vec<String>>,
        additional_hosts: Option<HashMap<String, String>>,
    ) -> Container {
        Container {
            additional_hostnames,
            additional_hosts,
            ..container(image, dependencies)
        }
    }

    #[tokio::test]
    async fn additional_hostnames_and_hosts_reach_a_tasks_own_container() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container_with_network_options(
                "alpine:3.18",
                None,
                Some(vec!["db-alias".to_string()]),
                Some(HashMap::from([(
                    "external-service".to_string(),
                    "10.0.0.1".to_string(),
                )])),
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.network_options_for("app"),
            Some((
                Some(vec!["db-alias".to_string()]),
                Some(HashMap::from([(
                    "external-service".to_string(),
                    "10.0.0.1".to_string()
                )])),
                None
            ))
        );
    }

    #[tokio::test]
    async fn additional_hostnames_and_hosts_reach_a_dependency_independently() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        containers.insert(
            "database".to_string(),
            container_with_network_options(
                "postgres:16",
                None,
                Some(vec!["db-alias".to_string()]),
                None,
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.network_options_for("database"),
            Some((Some(vec!["db-alias".to_string()]), None, None))
        );
        assert_eq!(
            docker.network_options_for("app"),
            Some((None, None, None)),
            "app itself declared no additional_hostnames/additional_hosts"
        );
    }

    fn single_port(local: u16, container: u16, protocol: &str) -> PortMapping {
        PortMapping {
            local: crate::config::PortRange {
                from: local,
                to: local,
            },
            container: crate::config::PortRange {
                from: container,
                to: container,
            },
            protocol: protocol.to_string(),
        }
    }

    fn container_with_ports(image: &str, ports: Vec<PortMapping>) -> Container {
        Container {
            ports: Some(ports),
            ..container(image, None)
        }
    }

    #[tokio::test]
    async fn ports_reach_the_container() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container_with_ports("alpine:3.18", vec![single_port(8080, 80, "tcp")]),
        );
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        let (_, _, ports) = docker.network_options_for("app").unwrap();
        assert_eq!(ports, Some(vec![(8080, 80, "tcp".to_string())]));
    }

    #[tokio::test]
    async fn task_run_ports_are_added_to_the_containers_own_ports() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container_with_ports("alpine:3.18", vec![single_port(8080, 80, "tcp")]),
        );
        let mut tasks = HashMap::new();
        let mut task_config = task("app", "echo hi");
        task_config.run.as_mut().unwrap().ports = Some(vec![single_port(9090, 90, "tcp")]);
        tasks.insert("run".to_string(), task_config);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        let (_, _, ports) = docker.network_options_for("app").unwrap();
        let ports = ports.unwrap();
        assert!(ports.contains(&(8080, 80, "tcp".to_string())));
        assert!(ports.contains(&(9090, 90, "tcp".to_string())));
    }

    #[tokio::test]
    async fn disable_port_publishing_suppresses_configured_ports() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container_with_ports("alpine:3.18", vec![single_port(8080, 80, "tcp")]),
        );
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).without_port_publishing();

        engine.run_task("run", &[]).await.unwrap();

        let (_, _, ports) = docker.network_options_for("app").unwrap();
        assert_eq!(
            ports, None,
            "ports were configured but --disable-ports should suppress them"
        );
    }

    #[tokio::test]
    async fn run_as_current_user_explicitly_disabled_reaches_the_container_with_no_mapping() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            Container {
                build_args: None,
                image: Some("alpine:3.18".to_string()),
                image_pull_policy: None,
                build_directory: None,
                dockerfile: None,
                build_target: None,
                build_secrets: None,
                build_ssh: None,
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: Some(crate::config::RunAsCurrentUser {
                    enabled: false,
                    home_directory: None,
                }),
                additional_hostnames: None,
                additional_hosts: None,
                ports: None,
                working_directory: None,
                command: None,
                entrypoint: None,
                labels: None,
                capabilities_to_add: None,
                capabilities_to_drop: None,
                privileged: None,
                shm_size: None,
                devices: None,
                enable_init_process: None,
                health_check: None,
                setup_commands: None,
            },
        );
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.user_mapping_for("app"),
            None,
            "run_as_current_user present but disabled should still resolve to no mapping"
        );
    }

    fn container_with_build_directory(
        build_directory: &str,
        build_args: Option<HashMap<String, String>>,
    ) -> Container {
        Container {
            image: None,
            image_pull_policy: None,
            build_directory: Some(build_directory.to_string()),
            build_args,
            dockerfile: None,
            build_target: None,
            build_secrets: None,
            build_ssh: None,
            volumes: None,
            dependencies: None,
            environment: None,
            run_as_current_user: None,
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
            working_directory: None,
            command: None,
            entrypoint: None,
            labels: None,
            capabilities_to_add: None,
            capabilities_to_drop: None,
            privileged: None,
            shm_size: None,
            devices: None,
            enable_init_process: None,
            health_check: None,
            setup_commands: None,
        }
    }

    #[tokio::test]
    async fn build_directory_container_builds_then_runs_the_built_image() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        let build_event = events
            .iter()
            .find(|e| e.starts_with("build:"))
            .expect("image should have been built");
        assert!(
            build_event.ends_with(":./docker"),
            "build should use the container's build_directory: {build_event}"
        );

        let tag = build_event
            .strip_prefix("build:")
            .unwrap()
            .split(':')
            .next()
            .unwrap();
        assert_eq!(
            docker.image_for("build-env").as_deref(),
            Some(tag),
            "the run should use the image that was just built, not a pulled/literal one"
        );
    }

    #[tokio::test]
    async fn build_directory_container_passes_dockerfile_and_target_through() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                dockerfile: Some("docker/Dockerfile.prod".to_string()),
                build_target: Some("builder".to_string()),
                ..container_with_build_directory("./docker", None)
            },
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let tag = "demo-build-env";
        let (dockerfile, target) = docker
            .build_options_for(tag)
            .expect("build_image should have been called for the built container's tag");
        assert_eq!(dockerfile, "docker/Dockerfile.prod");
        assert_eq!(target.as_deref(), Some("builder"));
    }

    #[tokio::test]
    async fn build_directory_container_defaults_dockerfile_when_unset() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let (dockerfile, target) = docker
            .build_options_for("demo-build-env")
            .expect("build_image should have been called for the built container's tag");
        assert_eq!(dockerfile, "Dockerfile");
        assert_eq!(target, None);
    }

    #[tokio::test]
    async fn build_directory_container_without_secrets_or_ssh_skips_buildkit() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        assert_eq!(docker.buildkit_options_for("demo-build-env"), None);
    }

    #[tokio::test]
    async fn build_directory_container_passes_secrets_and_ssh_through_as_buildkit_options() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                build_secrets: Some(HashMap::from([
                    (
                        "token".to_string(),
                        BuildSecret::Environment("TOKEN".to_string()),
                    ),
                    (
                        "cert".to_string(),
                        BuildSecret::Path("/base/cert.pem".to_string()),
                    ),
                ])),
                build_ssh: Some(vec![crate::config::SshAgent {
                    id: Some("default".to_string()),
                    paths: Vec::new(),
                }]),
                ..container_with_build_directory("./docker", None)
            },
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let buildkit = docker
            .buildkit_options_for("demo-build-env")
            .expect("build_secrets/build_ssh should have produced BuildKitOptions");
        assert!(buildkit.forward_default_ssh_agent);
        assert_eq!(
            buildkit.secrets.get("token"),
            Some(&crate::docker::BuildSecretSource::Environment(
                "TOKEN".to_string()
            ))
        );
        assert_eq!(
            buildkit.secrets.get("cert"),
            Some(&crate::docker::BuildSecretSource::File(PathBuf::from(
                "/base/cert.pem"
            )))
        );
    }

    #[tokio::test]
    async fn built_image_is_tagged_with_project_and_container_name() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("build:demo-build-env:")),
            "built image should be tagged '<project_name>-<container_name>', matching \
             Batect's convention, so it's identifiable in `docker images`: {events:?}"
        );
    }

    #[tokio::test]
    async fn build_directory_is_only_built_once_when_reused_across_tasks() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("first".to_string(), task("build-env", "echo one"));
        tasks.insert(
            "second".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "build-env".to_string(),
                    command: Some("echo two".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: Some(vec!["first".to_string()]),
                description: None,
                group: None,
                customise: None,
            },
        );
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("second", &[]).await.unwrap();

        let events = docker.events();
        let build_events: Vec<_> = events.iter().filter(|e| e.starts_with("build:")).collect();
        assert_eq!(
            build_events.len(),
            1,
            "the container should only be built once even though two tasks use it: {events:?}"
        );
    }

    #[tokio::test]
    async fn build_args_reach_the_build() {
        let mut build_args = HashMap::new();
        build_args.insert("VERSION".to_string(), "1.2.3".to_string());
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", Some(build_args)),
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        let tag = events
            .iter()
            .find_map(|e| e.strip_prefix("build:"))
            .and_then(|rest| rest.split(':').next())
            .expect("image should have been built");

        assert_eq!(docker.build_args_for(tag).unwrap()["VERSION"], "1.2.3");
    }

    #[tokio::test]
    async fn dependency_container_with_build_directory_is_built_and_started() {
        let mut containers = HashMap::new();
        containers.insert(
            "database".to_string(),
            container_with_build_directory("./db", None),
        );
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        let build_event = events
            .iter()
            .find(|e| e.starts_with("build:") && e.ends_with(":./db"))
            .expect("dependency container should have been built");
        let tag = build_event
            .strip_prefix("build:")
            .unwrap()
            .split(':')
            .next()
            .unwrap();

        assert!(
            events
                .iter()
                .any(|e| e.starts_with("sidecar-start:database:")),
            "dependency should have started: {events:?}"
        );
        assert_eq!(
            docker.image_for("database").as_deref(),
            Some(tag),
            "the dependency's sidecar should use the image that was just built"
        );
        assert!(
            !events.contains(&format!("pull:{tag}")),
            "a built image should never be pulled: {events:?}"
        );
    }

    #[tokio::test]
    async fn container_without_image_or_build_directory_errors() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                build_args: None,
                image: None,
                image_pull_policy: None,
                build_directory: None,
                dockerfile: None,
                build_target: None,
                build_secrets: None,
                build_ssh: None,
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
                additional_hostnames: None,
                additional_hosts: None,
                ports: None,
                working_directory: None,
                command: None,
                entrypoint: None,
                labels: None,
                capabilities_to_add: None,
                capabilities_to_drop: None,
                privileged: None,
                shm_size: None,
                devices: None,
                enable_init_process: None,
                health_check: None,
                setup_commands: None,
            },
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        let err = engine.run_task("build", &[]).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Container 'build-env' has neither 'image' nor 'build_directory' set"));
        let events = docker.events();
        assert!(
            events.iter().all(|e| e.starts_with("network-")),
            "no pull/run/sidecar events expected, just this task's own \
             network being created and torn down: {events:?}"
        );
    }

    #[tokio::test]
    async fn dependency_less_task_still_gets_its_own_network() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        let created: Vec<_> = events
            .iter()
            .filter(|e| e.starts_with("network-create:"))
            .collect();
        let removed: Vec<_> = events
            .iter()
            .filter(|e| e.starts_with("network-remove:"))
            .collect();
        assert_eq!(
            created.len(),
            1,
            "a task with no dependencies must still get its own isolated \
             network, not run on Docker's default bridge network: {events:?}"
        );
        assert_eq!(
            removed.len(),
            1,
            "the network must be torn down: {events:?}"
        );

        let network = created[0].strip_prefix("network-create:").unwrap();
        assert!(events.contains(&format!("run:build-env:echo hi:args=[]:{network}")));
    }

    #[tokio::test]
    async fn use_network_reuses_an_existing_network_instead_of_creating_one() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine =
            TaskEngine::new(config, docker.clone()).with_existing_network("my-network".to_string());

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events.contains(&"network-exists:my-network".to_string()),
            "the existing network must be checked: {events:?}"
        );
        assert!(
            !events.iter().any(|e| e.starts_with("network-create:")),
            "an existing network must not be created: {events:?}"
        );
        assert!(
            !events.iter().any(|e| e.starts_with("network-remove:")),
            "an existing network must not be torn down: {events:?}"
        );
        assert!(events.contains(&"run:build-env:echo hi:args=[]:my-network".to_string()));
    }

    #[tokio::test]
    async fn use_network_errors_clearly_when_the_network_does_not_exist() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default().without_existing_network();
        let engine =
            TaskEngine::new(config, docker.clone()).with_existing_network("missing".to_string());

        let result = engine.run_task("build", &[]).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
        let events = docker.events();
        assert!(
            !events.iter().any(|e| e.starts_with("run:")),
            "nothing should have run: {events:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_network_still_posts_task_failed() {
        // A `--use-network` validation failure used to `?`-return before the
        // block that posts TaskFailed/TaskFinished, silently ending the
        // event stream right after TaskStarting — this proves that's fixed:
        // the failure now reaches the same TaskFailed contract every other
        // infrastructure failure does, with no CleanupStarting/
        // RemovingNetwork posted (nothing was ever created to clean up).
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let sink = RecordingEventSink::default();
        let docker = FakeContainerRuntime::default().without_existing_network();
        let engine = TaskEngine::new(config, docker)
            .with_existing_network("missing".to_string())
            .with_event_sink(Arc::new(sink.clone()));

        assert!(engine.run_task("build", &[]).await.is_err());

        assert_eq!(
            sink.events(),
            vec![
                TaskEvent::TaskStarting {
                    task: "build".into()
                },
                TaskEvent::TaskFailed {
                    task: "build".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn dependency_starts_before_main_container_and_is_cleaned_up() {
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), container("postgres:16", None));
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        let network = events
            .iter()
            .find_map(|e| e.strip_prefix("network-create:"))
            .expect("a network should have been created")
            .to_string();

        let sidecar_index = events
            .iter()
            .position(|e| *e == format!("sidecar-start:database:{network}"))
            .expect("dependency should have started");
        let run_index = events
            .iter()
            .position(|e| *e == format!("run:app:echo hi:args=[]:{network}"))
            .expect("main container should have run, joined to the dependency's network");
        assert!(
            sidecar_index < run_index,
            "dependency must start before the main container: {events:?}"
        );

        let stop_index = events
            .iter()
            .position(|e| e.starts_with("sidecar-stop:"))
            .expect("dependency should have been cleaned up");
        let network_remove_index = events
            .iter()
            .position(|e| *e == format!("network-remove:{network}"))
            .expect("network should have been removed");
        assert!(
            stop_index > run_index,
            "cleanup happens after the run: {events:?}"
        );
        assert!(
            network_remove_index > run_index,
            "network removal happens after the run: {events:?}"
        );
    }

    /// Builds the standard app-depends-on-database config used by the
    /// readiness tests below, with `database` customized by `configure`.
    fn config_with_database_dependency(configure: impl FnOnce(&mut Container)) -> Config {
        let mut database = container("postgres:16", None);
        configure(&mut database);
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        }
    }

    #[tokio::test]
    async fn dependency_becomes_healthy_and_runs_setup_commands_before_the_task_starts() {
        let config = config_with_database_dependency(|database| {
            database.health_check = Some(crate::config::HealthCheckConfig {
                command: Some("pg_isready".to_string()),
                interval: Some(std::time::Duration::from_secs(2)),
                retries: Some(5),
                start_period: None,
                timeout: None,
            });
            database.setup_commands = Some(vec![
                crate::config::SetupCommand {
                    command: "./apply-migrations.sh".to_string(),
                    working_directory: Some("/setup".to_string()),
                },
                crate::config::SetupCommand {
                    command: "./seed-data.sh".to_string(),
                    working_directory: None,
                },
            ]);
        });

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        // The readiness gate runs in order — started, then healthy, then
        // each setup command in declared order — all before the task's own
        // container runs.
        let events = docker.events();
        let ordered_positions: Vec<usize> = [
            "sidecar-start:database:",
            "wait-healthy:sidecar-id-database",
            "exec:sidecar-id-database:./apply-migrations.sh",
            "exec:sidecar-id-database:./seed-data.sh",
            "run:app:",
        ]
        .iter()
        .map(|prefix| {
            events
                .iter()
                .position(|e| e.starts_with(prefix))
                .unwrap_or_else(|| panic!("expected an event starting '{prefix}': {events:?}"))
        })
        .collect();
        assert!(
            ordered_positions.windows(2).all(|pair| pair[0] < pair[1]),
            "readiness steps out of order: {events:?}"
        );

        // The health check override reached container creation.
        assert_eq!(
            docker.health_check_for("database"),
            Some(crate::docker::HealthCheckOptions {
                command: Some("pg_isready".to_string()),
                interval: Some(std::time::Duration::from_secs(2)),
                retries: Some(5),
                start_period: None,
                timeout: None,
            })
        );

        // A setup command's own working_directory reaches the exec; one
        // without falls back to the image's default (i.e. none is passed).
        let (working_directory, _, _) = docker.exec_for("./apply-migrations.sh").unwrap();
        assert_eq!(working_directory.as_deref(), Some("/setup"));
        let (working_directory, _, _) = docker.exec_for("./seed-data.sh").unwrap();
        assert_eq!(working_directory, None);
    }

    #[tokio::test]
    async fn setup_commands_run_with_the_containers_own_environment() {
        let config = config_with_database_dependency(|database| {
            let mut environment = HashMap::new();
            environment.insert("POSTGRES_PASSWORD".to_string(), "secret".to_string());
            database.environment = Some(environment);
            database.setup_commands = Some(vec![crate::config::SetupCommand {
                command: "./apply-migrations.sh".to_string(),
                working_directory: None,
            }]);
        });

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let (_, environment, _) = docker.exec_for("./apply-migrations.sh").unwrap();
        assert_eq!(
            environment
                .unwrap()
                .get("POSTGRES_PASSWORD")
                .map(String::as_str),
            Some("secret")
        );
    }

    #[tokio::test]
    async fn setup_command_falls_back_to_the_containers_own_working_directory() {
        let config = config_with_database_dependency(|database| {
            database.working_directory = Some("/from-container".to_string());
            database.setup_commands = Some(vec![crate::config::SetupCommand {
                command: "./apply-migrations.sh".to_string(),
                working_directory: None,
            }]);
        });

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let (working_directory, _, _) = docker.exec_for("./apply-migrations.sh").unwrap();
        assert_eq!(working_directory.as_deref(), Some("/from-container"));
    }

    #[tokio::test]
    async fn setup_commands_own_working_directory_overrides_the_containers() {
        let config = config_with_database_dependency(|database| {
            database.working_directory = Some("/from-container".to_string());
            database.setup_commands = Some(vec![crate::config::SetupCommand {
                command: "./apply-migrations.sh".to_string(),
                working_directory: Some("/from-setup-command".to_string()),
            }]);
        });

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let (working_directory, _, _) = docker.exec_for("./apply-migrations.sh").unwrap();
        assert_eq!(working_directory.as_deref(), Some("/from-setup-command"));
    }

    #[tokio::test]
    async fn unhealthy_dependency_fails_the_task_and_still_cleans_up() {
        let config = config_with_database_dependency(|database| {
            database.health_check = Some(crate::config::HealthCheckConfig {
                command: Some("pg_isready".to_string()),
                interval: None,
                retries: None,
                start_period: None,
                timeout: None,
            });
        });

        let docker = FakeContainerRuntime::default().with_unhealthy_container("database");
        let engine = TaskEngine::new(config, docker.clone());

        let result = engine.run_task("start", &[]).await;

        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains("'database' did not become healthy"),
            "error should name the unhealthy container: {message}"
        );

        let events = docker.events();
        assert!(
            !events.iter().any(|e| e.starts_with("run:")),
            "the task must not run when a dependency never becomes ready: {events:?}"
        );
        assert!(
            events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
            "the unhealthy dependency must still be cleaned up: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.starts_with("network-remove:")),
            "the network must still be removed: {events:?}"
        );
    }

    #[tokio::test]
    async fn failing_setup_command_fails_the_task_and_still_cleans_up() {
        let config = config_with_database_dependency(|database| {
            database.setup_commands = Some(vec![
                crate::config::SetupCommand {
                    command: "./apply-migrations.sh".to_string(),
                    working_directory: None,
                },
                crate::config::SetupCommand {
                    command: "./seed-data.sh".to_string(),
                    working_directory: None,
                },
            ]);
        });

        let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed-data.sh");
        let engine = TaskEngine::new(config, docker.clone());

        let result = engine.run_task("start", &[]).await;

        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains(
                "Setup command './seed-data.sh' in container 'database' exited with code 1"
            ),
            "error should name the failing command: {message}"
        );
        assert!(
            message.contains("something went wrong"),
            "error should include the command's output: {message}"
        );

        let events = docker.events();
        assert!(
            !events.iter().any(|e| e.starts_with("run:")),
            "the task must not run when a setup command fails: {events:?}"
        );
        assert!(
            events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
            "the dependency must still be cleaned up: {events:?}"
        );
    }

    #[tokio::test]
    async fn task_containers_own_health_check_is_applied_but_never_gates_the_run() {
        let mut containers = HashMap::new();
        let mut app = container("alpine:3.18", None);
        app.health_check = Some(crate::config::HealthCheckConfig {
            command: Some("wget -q localhost".to_string()),
            interval: None,
            retries: None,
            start_period: None,
            timeout: None,
        });
        containers.insert("app".to_string(), app);
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        // The override reaches Docker (it records and runs the check)...
        assert_eq!(
            docker.health_check_for("app"),
            Some(crate::docker::HealthCheckOptions {
                command: Some("wget -q localhost".to_string()),
                ..Default::default()
            })
        );
        // ...but nothing waits on its verdict — the task is the container's
        // own command.
        let events = docker.events();
        assert!(
            !events.iter().any(|e| e.starts_with("wait-healthy:")),
            "the task's own container must not be gated on health: {events:?}"
        );
    }

    #[tokio::test]
    async fn task_fails_when_container_exits_nonzero_but_dependencies_are_still_cleaned_up() {
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), container("postgres:16", None));
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "exit 1"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default().failing_run();
        let engine = TaskEngine::new(config, docker.clone());

        let err = engine.run_task("start", &[]).await.unwrap_err();
        assert!(err.to_string().contains("exited with code"));

        // A failing main container must not stop cleanup from happening —
        // the sidecar and network are still torn down.
        let events = docker.events();
        assert!(
            events.iter().any(|e| e.starts_with("sidecar-stop:")),
            "dependency should still be cleaned up after a failed run: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.starts_with("network-remove:")),
            "network should still be removed after a failed run: {events:?}"
        );
    }

    #[tokio::test]
    async fn without_cleanup_after_success_leaves_everything_in_place_on_a_nonzero_exit() {
        // A nonzero exit is still "success" for cleanup-gating purposes —
        // matching Batect, which only treats an infrastructure failure as
        // "failure" here (see `cleanup_after_success`'s own doc comment).
        let config = config_with_database_dependency(|_| {});
        let docker = FakeContainerRuntime::default().failing_run();
        let engine = TaskEngine::new(config, docker.clone()).without_cleanup_after_success();

        let err = engine.run_task("start", &[]).await.unwrap_err();
        assert!(err.to_string().contains("exited with code"));

        let events = docker.events();
        assert!(
            !events.iter().any(|e| e.starts_with("sidecar-stop:")),
            "dependency should be left running when cleanup-after-success is disabled: {events:?}"
        );
        assert!(
            !events.iter().any(|e| e.starts_with("network-remove:")),
            "network should be left in place when cleanup-after-success is disabled: {events:?}"
        );
        assert!(
            events.contains(&"remove_on_exit:app:false".to_string()),
            "the main container itself must not be removed either: {events:?}"
        );
    }

    #[tokio::test]
    async fn without_cleanup_after_success_has_no_effect_on_an_infrastructure_failure() {
        let config = config_with_database_dependency(|database| {
            database.health_check = Some(crate::config::HealthCheckConfig {
                command: Some("pg_isready".to_string()),
                interval: None,
                retries: None,
                start_period: None,
                timeout: None,
            });
        });
        let docker = FakeContainerRuntime::default().with_unhealthy_container("database");
        let engine = TaskEngine::new(config, docker.clone()).without_cleanup_after_success();

        engine.run_task("start", &[]).await.unwrap_err();

        let events = docker.events();
        assert!(
            events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
            "cleanup-after-failure is still enabled by default, so the dependency should still \
             be cleaned up: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.starts_with("network-remove:")),
            "cleanup-after-failure is still enabled by default, so the network should still be \
             removed: {events:?}"
        );
    }

    #[tokio::test]
    async fn without_cleanup_after_failure_leaves_everything_in_place_on_an_infrastructure_failure()
    {
        let config = config_with_database_dependency(|database| {
            database.health_check = Some(crate::config::HealthCheckConfig {
                command: Some("pg_isready".to_string()),
                interval: None,
                retries: None,
                start_period: None,
                timeout: None,
            });
        });
        let docker = FakeContainerRuntime::default().with_unhealthy_container("database");
        let engine = TaskEngine::new(config, docker.clone()).without_cleanup_after_failure();

        engine.run_task("start", &[]).await.unwrap_err();

        let events = docker.events();
        assert!(
            !events.iter().any(|e| e.starts_with("sidecar-stop:")),
            "dependency should be left running when cleanup-after-failure is disabled: \
             {events:?}"
        );
        assert!(
            !events.iter().any(|e| e.starts_with("network-remove:")),
            "network should be left in place when cleanup-after-failure is disabled: {events:?}"
        );
    }

    #[tokio::test]
    async fn without_cleanup_after_failure_has_no_effect_on_a_successful_run() {
        let config = config_with_database_dependency(|_| {});
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).without_cleanup_after_failure();

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
            "cleanup-after-success is still enabled by default, so the dependency should still \
             be cleaned up: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.starts_with("network-remove:")),
            "cleanup-after-success is still enabled by default, so the network should still be \
             removed: {events:?}"
        );
        assert!(
            events.contains(&"remove_on_exit:app:true".to_string()),
            "the main container should still be removed too: {events:?}"
        );
    }

    #[tokio::test]
    async fn nested_dependencies_start_in_order_on_same_network() {
        let mut containers = HashMap::new();
        containers.insert("cache".to_string(), container("redis:7", None));
        containers.insert(
            "database".to_string(),
            container("postgres:16", Some(vec!["cache".to_string()])),
        );
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        let network = events
            .iter()
            .find_map(|e| e.strip_prefix("network-create:"))
            .unwrap()
            .to_string();

        let cache_index = events
            .iter()
            .position(|e| *e == format!("sidecar-start:cache:{network}"))
            .expect("nested dependency should have started");
        let database_index = events
            .iter()
            .position(|e| *e == format!("sidecar-start:database:{network}"))
            .expect("direct dependency should have started");
        let run_index = events
            .iter()
            .position(|e| *e == format!("run:app:echo hi:args=[]:{network}"))
            .expect("main container should have run");

        assert!(
            cache_index < database_index,
            "a nested dependency must start before the container that depends on it: {events:?}"
        );
        assert!(database_index < run_index);
    }

    #[tokio::test(start_paused = true)]
    async fn independent_dependencies_start_concurrently_not_sequentially() {
        // "dep-a" and "dep-b" share no dependency relationship — 0.15.0
        // should start both at once rather than one after the other.
        let mut containers = HashMap::new();
        containers.insert("dep-a".to_string(), container("alpine:3.18", None));
        containers.insert("dep-b".to_string(), container("alpine:3.18", None));
        containers.insert(
            "app".to_string(),
            container(
                "alpine:3.18",
                Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_start_delay("dep-a", delay)
            .with_start_delay("dep-b", delay);
        let engine = TaskEngine::new(config, docker);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < delay * 2,
            "two independent dependencies with a {delay:?} delay each should overlap, not run \
             sequentially (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_dependencies_sharing_an_image_only_pull_it_once() {
        // "dep-a" and "dep-b" are independent (no dependency relationship
        // between them) but share one image — with a delay long enough that
        // both branches genuinely overlap while the first one is still
        // deciding/pulling, proving the pull is memoized rather than raced.
        let mut containers = HashMap::new();
        containers.insert("dep-a".to_string(), container("shared-image:1", None));
        containers.insert("dep-b".to_string(), container("shared-image:1", None));
        containers.insert(
            "app".to_string(),
            container(
                "alpine:3.18",
                Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default()
            .with_pull_delay("shared-image:1", std::time::Duration::from_millis(50));
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let pulls: Vec<_> = docker
            .events()
            .iter()
            .filter(|e| e.starts_with("pull:shared-image:1"))
            .cloned()
            .collect();
        assert_eq!(
            pulls,
            vec!["pull:shared-image:1".to_string()],
            "an image shared by two concurrently-starting dependencies should only be pulled once"
        );
    }

    /// Shared by the two `max_parallelism` tests below: two independent
    /// dependencies (no relationship to each other) with *different*
    /// images, so neither the shared-image pull dedup nor the dependency
    /// graph's own structure could explain serialization — only
    /// `--max-parallelism`'s own cap could.
    fn config_with_two_independent_image_pulls() -> Config {
        let mut containers = HashMap::new();
        containers.insert("dep-a".to_string(), container("image-a:1", None));
        containers.insert("dep-b".to_string(), container("image-b:1", None));
        containers.insert(
            "app".to_string(),
            container(
                "alpine:3.18",
                Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn max_parallelism_of_one_serializes_independent_image_pulls() {
        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_pull_delay("image-a:1", delay)
            .with_pull_delay("image-b:1", delay);
        let engine = TaskEngine::new(config_with_two_independent_image_pulls(), docker)
            .with_max_parallelism(1);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed >= delay * 2,
            "with --max-parallelism 1, two independent image pulls should be serialized, not \
             overlap (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn max_parallelism_of_two_still_lets_two_independent_pulls_overlap() {
        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_pull_delay("image-a:1", delay)
            .with_pull_delay("image-b:1", delay);
        let engine = TaskEngine::new(config_with_two_independent_image_pulls(), docker)
            .with_max_parallelism(2);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < delay * 2,
            "with --max-parallelism 2, both independent image pulls should still overlap \
             (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn default_unbounded_parallelism_still_lets_independent_pulls_overlap() {
        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_pull_delay("image-a:1", delay)
            .with_pull_delay("image-b:1", delay);
        let engine = TaskEngine::new(config_with_two_independent_image_pulls(), docker);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < delay * 2,
            "with no --max-parallelism given, independent image pulls should overlap by \
             default, matching pre-existing behavior (elapsed: {elapsed:?})"
        );
    }

    /// Shared by the three `max_parallelism` tests below covering start/
    /// setup-command/health-check concurrency: two independent dependencies
    /// (no relationship to each other), same shape as
    /// `config_with_two_independent_image_pulls` but sharing one image
    /// (irrelevant here — nothing in these tests is keyed by image name).
    fn config_with_two_independent_dependencies() -> Config {
        let mut containers = HashMap::new();
        containers.insert("dep-a".to_string(), container("alpine:3.18", None));
        containers.insert("dep-b".to_string(), container("alpine:3.18", None));
        containers.insert(
            "app".to_string(),
            container(
                "alpine:3.18",
                Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn max_parallelism_of_one_serializes_independent_container_starts() {
        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_start_delay("dep-a", delay)
            .with_start_delay("dep-b", delay);
        let engine = TaskEngine::new(config_with_two_independent_dependencies(), docker)
            .with_max_parallelism(1);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed >= delay * 2,
            "with --max-parallelism 1, two independent dependency starts should be serialized, \
             not overlap (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn max_parallelism_of_one_serializes_independent_setup_command_execution() {
        let mut config = config_with_two_independent_dependencies();
        config.containers.get_mut("dep-a").unwrap().setup_commands =
            Some(vec![crate::config::SetupCommand {
                command: "setup-a".to_string(),
                working_directory: None,
            }]);
        config.containers.get_mut("dep-b").unwrap().setup_commands =
            Some(vec![crate::config::SetupCommand {
                command: "setup-b".to_string(),
                working_directory: None,
            }]);

        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_exec_delay("setup-a", delay)
            .with_exec_delay("setup-b", delay);
        let engine = TaskEngine::new(config, docker).with_max_parallelism(1);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed >= delay * 2,
            "with --max-parallelism 1, two independent containers' setup commands should be \
             serialized, not overlap (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn max_parallelism_does_not_gate_health_check_waits() {
        // Every dependency's `wait_for_container_healthy` call happens
        // regardless of whether it declares a `health_check` at all (an
        // immediate no-op for one that doesn't — see
        // `ensure_container_ready`'s own doc comment), so the fake's delay
        // hook applies here without needing to configure one.
        let delay = std::time::Duration::from_millis(100);
        let docker = FakeContainerRuntime::default()
            .with_health_check_delay("dep-a", delay)
            .with_health_check_delay("dep-b", delay);
        let engine = TaskEngine::new(config_with_two_independent_dependencies(), docker)
            .with_max_parallelism(1);

        let start = tokio::time::Instant::now();
        engine.run_task("start", &[]).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < delay * 2,
            "health-check waits should still overlap even under --max-parallelism 1 — only \
             pulls/builds, starts, and setup-command execution are gated (elapsed: {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn shared_nested_dependency_started_once_per_task() {
        let mut containers = HashMap::new();
        containers.insert("cache".to_string(), container("redis:7", None));
        containers.insert(
            "database".to_string(),
            container("postgres:16", Some(vec!["cache".to_string()])),
        );
        containers.insert(
            "search".to_string(),
            container("elasticsearch:8", Some(vec!["cache".to_string()])),
        );
        containers.insert(
            "app".to_string(),
            container(
                "alpine:3.18",
                Some(vec!["database".to_string(), "search".to_string()]),
            ),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();

        let cache_starts = events
            .iter()
            .filter(|e| e.starts_with("sidecar-start:cache:"))
            .count();
        assert_eq!(
            cache_starts, 1,
            "a dependency shared by two of a task's direct dependencies should only start once for that task: {events:?}"
        );

        // Both direct siblings must actually start too — a shared-dependency dedup
        // bug could plausibly short-circuit one of them, not just the shared one.
        for sibling in ["database", "search"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.starts_with(&format!("sidecar-start:{sibling}:")))
                    .count(),
                1,
                "sibling dependency '{sibling}' should have started exactly once: {events:?}"
            );
        }
    }

    #[tokio::test]
    async fn task_level_dependency_starts_alongside_container_level_ones() {
        let mut containers = HashMap::new();
        containers.insert("cache".to_string(), container("redis:7", None));
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["cache".to_string()])),
        );
        containers.insert("queue".to_string(), container("redis:7", None));
        let mut tasks = HashMap::new();
        let mut start_task = task("app", "echo hi");
        start_task.dependencies = Some(vec!["queue".to_string()]);
        tasks.insert("start".to_string(), start_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        // "queue" only exists as a task-level dependency (not in "app"'s own
        // container-level `dependencies`) — it must still start alongside
        // "cache", the container-level one.
        for sidecar in ["cache", "queue"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.starts_with(&format!("sidecar-start:{sidecar}:")))
                    .count(),
                1,
                "'{sidecar}' should have started exactly once: {events:?}"
            );
        }
    }

    #[tokio::test]
    async fn task_level_dependency_shared_with_a_container_level_one_only_starts_once() {
        let mut containers = HashMap::new();
        containers.insert("cache".to_string(), container("redis:7", None));
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["cache".to_string()])),
        );
        let mut tasks = HashMap::new();
        // Task-level `dependencies` names the same container "app" already
        // depends on at the container level — must dedup to a single start,
        // not start "cache" twice.
        let mut start_task = task("app", "echo hi");
        start_task.dependencies = Some(vec!["cache".to_string()]);
        tasks.insert("start".to_string(), start_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.starts_with("sidecar-start:cache:"))
                .count(),
            1,
            "a container named by both task-level and container-level \
             dependencies should still only start once: {events:?}"
        );
    }

    #[tokio::test]
    async fn deeply_nested_dependencies_all_start_in_order() {
        // a -> b -> c -> d, four levels total, to prove the recursion isn't
        // accidentally limited to one or two levels.
        let mut containers = HashMap::new();
        containers.insert("d".to_string(), container("alpine:3.18", None));
        containers.insert(
            "c".to_string(),
            container("alpine:3.18", Some(vec!["d".to_string()])),
        );
        containers.insert(
            "b".to_string(),
            container("alpine:3.18", Some(vec!["c".to_string()])),
        );
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["b".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let events = docker.events();
        let network = events
            .iter()
            .find_map(|e| e.strip_prefix("network-create:"))
            .unwrap()
            .to_string();

        let index_of = |alias: &str| {
            events
                .iter()
                .position(|e| *e == format!("sidecar-start:{alias}:{network}"))
                .unwrap_or_else(|| panic!("expected '{alias}' to have started: {events:?}"))
        };
        let run_index = events
            .iter()
            .position(|e| *e == format!("run:app:echo hi:args=[]:{network}"))
            .expect("main container should have run");

        let (d_index, c_index, b_index) = (index_of("d"), index_of("c"), index_of("b"));
        assert!(
            d_index < c_index && c_index < b_index && b_index < run_index,
            "the whole chain must start in dependency order, deepest first: {events:?}"
        );
    }

    #[tokio::test]
    async fn separate_tasks_each_get_their_own_dependency_instance() {
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), container("postgres:16", None));
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("migrate".to_string(), task("app", "migrate"));
        tasks.insert(
            "test".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "app".to_string(),
                    command: Some("test".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: Some(vec!["migrate".to_string()]),
                description: None,
                group: None,
                customise: None,
            },
        );
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        let events = docker.events();

        let database_starts = events
            .iter()
            .filter(|e| e.starts_with("sidecar-start:database:"))
            .count();
        assert_eq!(
            database_starts, 2,
            "each task execution should get its own dependency instance, not a shared one: {events:?}"
        );

        let networks_created: std::collections::HashSet<_> = events
            .iter()
            .filter_map(|e| e.strip_prefix("network-create:"))
            .collect();
        assert_eq!(
            networks_created.len(),
            2,
            "each task execution should get its own network: {events:?}"
        );
    }

    #[tokio::test]
    async fn dependency_without_image_or_build_directory_errors() {
        let mut containers = HashMap::new();
        containers.insert(
            "database".to_string(),
            Container {
                build_args: None,
                image: None,
                image_pull_policy: None,
                build_directory: None,
                dockerfile: None,
                build_target: None,
                build_secrets: None,
                build_ssh: None,
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
                additional_hostnames: None,
                additional_hosts: None,
                ports: None,
                working_directory: None,
                command: None,
                entrypoint: None,
                labels: None,
                capabilities_to_add: None,
                capabilities_to_drop: None,
                privileged: None,
                shm_size: None,
                devices: None,
                enable_init_process: None,
                health_check: None,
                setup_commands: None,
            },
        );
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker);

        let err = engine.run_task("start", &[]).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Container 'database' has neither 'image' nor 'build_directory' set"));
    }

    #[tokio::test]
    async fn detects_circular_container_dependency() {
        let mut containers = HashMap::new();
        containers.insert(
            "a".to_string(),
            container("alpine:3.18", Some(vec!["b".to_string()])),
        );
        containers.insert(
            "b".to_string(),
            container("alpine:3.18", Some(vec!["a".to_string()])),
        );
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["a".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker);

        let err = engine.run_task("start", &[]).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Circular container dependency detected"));
    }

    #[tokio::test]
    async fn detects_dependency_cycle() {
        // DockerClient::new() never contacts a daemon (bollard builds the
        // client lazily), so this exercises the cycle-detection guard
        // without needing Docker to actually be running.
        let docker = DockerClient::new(&Default::default())
            .expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(config_with_cycle(), docker);

        let err = engine.run_task("a", &[]).await.unwrap_err();
        assert!(err.to_string().contains("Dependency cycle detected"));
    }

    #[tokio::test]
    async fn missing_task_returns_error() {
        let docker = DockerClient::new(&Default::default())
            .expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(empty_config(), docker);

        let err = engine.run_task("does-not-exist", &[]).await.unwrap_err();
        assert!(err.to_string().contains("Task 'does-not-exist' not found"));
    }

    #[tokio::test]
    async fn a_slightly_misspelled_task_name_suggests_the_real_one() {
        let docker = DockerClient::new(&Default::default())
            .expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker);

        let err = engine.run_task("tst-task", &[]).await.unwrap_err();
        assert!(
            err.to_string().contains("Did you mean 'test-task'?"),
            "error should suggest the close match: {err}"
        );
    }

    #[tokio::test]
    async fn a_wildly_misspelled_task_name_suggests_nothing() {
        let docker = DockerClient::new(&Default::default())
            .expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker);

        let err = engine
            .run_task("completely-unrelated-name", &[])
            .await
            .unwrap_err();
        assert!(
            !err.to_string().contains("Did you mean"),
            "nothing should be close enough to suggest: {err}"
        );
    }

    #[test]
    fn suggests_multiple_close_matches_as_a_human_readable_list() {
        let mut tasks = HashMap::new();
        for name in ["test", "text", "tent", "unrelated"] {
            tasks.insert(
                name.to_string(),
                Task {
                    run: None,
                    prerequisites: None,
                    dependencies: None,
                    description: None,
                    group: None,
                    customise: None,
                },
            );
        }

        let suggestions = suggest_task_names(&tasks, "test");

        // "test" itself is an exact match (distance 0); "text"/"tent" are
        // both distance 1; "unrelated" is far outside the distance-3 cutoff.
        assert_eq!(
            suggestions,
            vec!["test".to_string(), "tent".to_string(), "text".to_string()],
            "ties should break alphabetically, and nothing beyond the distance-3 cutoff should appear"
        );
    }

    #[test]
    fn human_readable_list_formats_one_two_and_three_items() {
        assert_eq!(human_readable_list(&["a".to_string()], "or"), "a");
        assert_eq!(
            human_readable_list(&["a".to_string(), "b".to_string()], "or"),
            "a or b"
        );
        assert_eq!(
            human_readable_list(&["a".to_string(), "b".to_string(), "c".to_string()], "or"),
            "a, b or c"
        );
    }

    #[tokio::test]
    async fn task_run_environment_reaches_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.environment = Some(HashMap::from([(
            "CONTAINER_VAR".to_string(),
            "container-value".to_string(),
        )]));
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut task_config = task("build-env", "echo hi");
        task_config.run.as_mut().unwrap().environment = Some(HashMap::from([(
            "RUN_VAR".to_string(),
            "run-value".to_string(),
        )]));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task_config);

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        let environment = docker.environment_for("build-env").unwrap();
        assert_eq!(
            environment.get("CONTAINER_VAR"),
            Some(&"container-value".to_string())
        );
        assert_eq!(environment.get("RUN_VAR"), Some(&"run-value".to_string()));
    }

    #[tokio::test]
    async fn task_run_environment_overrides_container_environment_on_key_collision() {
        let mut container_config = container("alpine:3.18", None);
        container_config.environment = Some(HashMap::from([(
            "SHARED".to_string(),
            "from-container".to_string(),
        )]));
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut task_config = task("build-env", "echo hi");
        task_config.run.as_mut().unwrap().environment = Some(HashMap::from([(
            "SHARED".to_string(),
            "from-run".to_string(),
        )]));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task_config);

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        let environment = docker.environment_for("build-env").unwrap();
        assert_eq!(environment.get("SHARED"), Some(&"from-run".to_string()));
    }

    #[tokio::test]
    async fn container_working_directory_reaches_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.working_directory = Some("/app".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.working_directory_for("build-env"),
            Some("/app".to_string())
        );
    }

    #[tokio::test]
    async fn task_run_working_directory_overrides_container_working_directory() {
        let mut container_config = container("alpine:3.18", None);
        container_config.working_directory = Some("/from-container".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut task_config = task("build-env", "echo hi");
        task_config.run.as_mut().unwrap().working_directory = Some("/from-run".to_string());
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task_config);

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.working_directory_for("build-env"),
            Some("/from-run".to_string())
        );
    }

    #[tokio::test]
    async fn container_command_reaches_the_container_when_run_command_is_unset() {
        // Before this, a container had no `command` field at all — a task's
        // own container could only get a command via `run.command`. This
        // proves the container-level default now reaches the container when
        // the task's own `run` doesn't set one.
        let mut container_config = container("alpine:3.18", None);
        container_config.command = Some("/from-container".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert(
            "test".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "build-env".to_string(),
                    command: None,
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: None,
                description: None,
                group: None,
                customise: None,
            },
        );

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("run:build-env:/from-container:args=[]:")),
            "the container's own command should reach the run when run.command is unset: {events:?}"
        );
    }

    #[tokio::test]
    async fn container_entrypoint_reaches_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.entrypoint = Some("/bin/sh -c".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.entrypoint_for("build-env"),
            Some("/bin/sh -c".to_string())
        );
    }

    #[tokio::test]
    async fn container_labels_reach_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.labels = Some(HashMap::from([(
            "com.example.owner".to_string(),
            "platform-team".to_string(),
        )]));
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.labels_for("build-env"),
            Some(HashMap::from([(
                "com.example.owner".to_string(),
                "platform-team".to_string()
            )]))
        );
    }

    #[tokio::test]
    async fn container_capabilities_reach_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.capabilities_to_add =
            Some(HashSet::from([crate::config::Capability::NetAdmin]));
        container_config.capabilities_to_drop =
            Some(HashSet::from([crate::config::Capability::Chown]));
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.capabilities_to_add_for("build-env"),
            Some(vec!["NET_ADMIN".to_string()])
        );
        assert_eq!(
            docker.capabilities_to_drop_for("build-env"),
            Some(vec!["CHOWN".to_string()])
        );
    }

    #[tokio::test]
    async fn container_privileged_reaches_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.privileged = Some(true);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(docker.privileged_for("build-env"), Some(true));
    }

    #[tokio::test]
    async fn container_shm_size_reaches_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.shm_size = Some(128 * 1024 * 1024);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(docker.shm_size_for("build-env"), Some(128 * 1024 * 1024));
    }

    #[tokio::test]
    async fn container_devices_reach_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.devices = Some(vec![crate::config::DeviceMapping {
            local: "/dev/sda".to_string(),
            container: "/dev/xvda".to_string(),
            options: Some("rwm".to_string()),
        }]);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.devices_for("build-env"),
            Some(vec![(
                "/dev/sda".to_string(),
                "/dev/xvda".to_string(),
                Some("rwm".to_string())
            )])
        );
    }

    #[tokio::test]
    async fn container_enable_init_process_reaches_the_container() {
        let mut container_config = container("alpine:3.18", None);
        container_config.enable_init_process = Some(true);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(docker.enable_init_process_for("build-env"), Some(true));
    }

    #[tokio::test]
    async fn if_not_present_policy_pulls_when_the_image_is_missing_locally() {
        // No image_pull_policy set — IfNotPresent is the default.
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert!(docker.events().contains(&"pull:alpine:3.18".to_string()));
    }

    #[tokio::test]
    async fn if_not_present_policy_skips_the_pull_when_the_image_already_exists_locally() {
        let mut container_config = container("alpine:3.18", None);
        container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::IfNotPresent);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default().with_local_image("alpine:3.18");
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert!(!docker.events().contains(&"pull:alpine:3.18".to_string()));
    }

    #[tokio::test]
    async fn always_policy_pulls_even_when_the_image_already_exists_locally() {
        let mut container_config = container("alpine:3.18", None);
        container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::Always);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default().with_local_image("alpine:3.18");
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert!(docker.events().contains(&"pull:alpine:3.18".to_string()));
    }

    #[tokio::test]
    async fn image_override_pulls_the_override_instead_of_the_configured_image() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let overrides = HashMap::from([("build-env".to_string(), "ubuntu:22.04".to_string())]);
        let engine = TaskEngine::new(config, docker.clone())
            .with_image_overrides(overrides)
            .unwrap();

        engine.run_task("test", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events.contains(&"pull:ubuntu:22.04".to_string()),
            "{events:?}"
        );
        assert!(
            !events.iter().any(|e| e.contains("alpine")),
            "the configured image should never be touched once overridden: {events:?}"
        );
    }

    #[tokio::test]
    async fn image_override_ignores_the_containers_configured_pull_policy() {
        // `Always` on the original container must not leak onto the
        // override — Batect's own override replaces the whole `imageSource`
        // with a fresh `PullImage` under its default `IfNotPresent`, not a
        // patched copy of the original.
        let mut container_config = container("alpine:3.18", None);
        container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::Always);
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default().with_local_image("ubuntu:22.04");
        let overrides = HashMap::from([("build-env".to_string(), "ubuntu:22.04".to_string())]);
        let engine = TaskEngine::new(config, docker.clone())
            .with_image_overrides(overrides)
            .unwrap();

        engine.run_task("test", &[]).await.unwrap();

        assert!(
            !docker.events().contains(&"pull:ubuntu:22.04".to_string()),
            "already-local override image should be skipped under the override's own \
             IfNotPresent policy, not re-pulled per the original container's Always: {:?}",
            docker.events()
        );
    }

    #[tokio::test]
    async fn image_override_replaces_a_build_directory_container_with_a_pull_instead() {
        let mut containers = HashMap::new();
        let mut container_config = container("unused-if-overridden", None);
        container_config.image = None;
        container_config.build_directory = Some(".".to_string());
        containers.insert("build-env".to_string(), container_config);
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let overrides = HashMap::from([("build-env".to_string(), "ubuntu:22.04".to_string())]);
        let engine = TaskEngine::new(config, docker.clone())
            .with_image_overrides(overrides)
            .unwrap();

        engine.run_task("test", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events.contains(&"pull:ubuntu:22.04".to_string()),
            "{events:?}"
        );
        assert!(
            !events.iter().any(|e| e.starts_with("build:")),
            "an overridden container must never be built, even with build_directory set: {events:?}"
        );
    }

    #[test]
    fn with_image_overrides_rejects_an_unknown_container_name() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let overrides =
            HashMap::from([("no-such-container".to_string(), "ubuntu:22.04".to_string())]);
        let err = match TaskEngine::new(config, docker).with_image_overrides(overrides) {
            Ok(_) => panic!("expected with_image_overrides to reject an unknown container name"),
            Err(err) => err,
        };

        assert_eq!(
            err.to_string(),
            "Cannot override image for container 'no-such-container' because there is no \
             container named 'no-such-container' defined."
        );
    }

    #[tokio::test]
    async fn tag_image_tags_a_built_image_in_addition_to_the_default_tag() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory(".", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let tags = HashMap::from([(
            "build-env".to_string(),
            HashSet::from(["my.registry/build-env:v1".to_string()]),
        )]);
        let engine = TaskEngine::new(config, docker.clone()).with_image_tags(tags);

        engine.run_task("test", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events.contains(&"tag:demo-build-env:my.registry/build-env:v1".to_string()),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn tag_image_errors_immediately_when_the_container_uses_a_pulled_image() {
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let tags = HashMap::from([(
            "build-env".to_string(),
            HashSet::from(["my.registry/build-env:v1".to_string()]),
        )]);
        let engine = TaskEngine::new(config, docker).with_image_tags(tags);

        let err = engine.run_task("test", &[]).await.unwrap_err();

        assert_eq!(
            err.to_string(),
            "The image built for container 'build-env' was requested to be tagged with \
             --tag-image, but 'build-env' uses a pulled image."
        );
    }

    #[tokio::test]
    async fn tag_image_errors_immediately_when_an_override_image_replaces_a_build_with_a_pull() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory(".", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let overrides = HashMap::from([("build-env".to_string(), "ubuntu:22.04".to_string())]);
        let tags = HashMap::from([(
            "build-env".to_string(),
            HashSet::from(["my.registry/build-env:v1".to_string()]),
        )]);
        let engine = TaskEngine::new(config, docker)
            .with_image_overrides(overrides)
            .unwrap()
            .with_image_tags(tags);

        let err = engine.run_task("test", &[]).await.unwrap_err();

        assert_eq!(
            err.to_string(),
            "The image built for container 'build-env' was requested to be tagged with \
             --tag-image, but 'build-env' uses a pulled image."
        );
    }

    #[tokio::test]
    async fn tag_image_errors_once_the_task_finishes_if_the_tagged_container_never_ran() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory(".", None),
        );
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let tags = HashMap::from([(
            "no-such-container".to_string(),
            HashSet::from(["my.registry/foo:v1".to_string()]),
        )]);
        let engine = TaskEngine::new(config, docker).with_image_tags(tags);

        let err = engine.run_task("test", &[]).await.unwrap_err();

        assert_eq!(
            err.to_string(),
            "The image for container 'no-such-container' was requested to be tagged with \
             --tag-image, but this container did not run as part of the task or its \
             prerequisites."
        );
    }

    #[tokio::test]
    async fn task_run_command_overrides_container_command() {
        let mut container_config = container("alpine:3.18", None);
        container_config.command = Some("/from-container".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "/from-run"));

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        let events = docker.events();
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("run:build-env:/from-run:args=[]:")),
            "run.command should override the container's own command: {events:?}"
        );
    }

    #[tokio::test]
    async fn task_run_entrypoint_overrides_container_entrypoint() {
        let mut container_config = container("alpine:3.18", None);
        container_config.entrypoint = Some("/from-container".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut task_config = task("build-env", "echo hi");
        task_config.run.as_mut().unwrap().entrypoint = Some("/from-run".to_string());
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task_config);

        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test", &[]).await.unwrap();

        assert_eq!(
            docker.entrypoint_for("build-env"),
            Some("/from-run".to_string())
        );
    }

    #[tokio::test]
    async fn proxy_environment_variables_reach_a_tasks_own_container() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).with_host_env(|name| {
            (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
        });

        engine.run_task("run", &[]).await.unwrap();

        let environment = docker.environment_for("app").unwrap();
        assert_eq!(
            environment.get("http_proxy"),
            Some(&"http://proxy.example.com".to_string())
        );
        assert_eq!(
            environment.get("HTTP_PROXY"),
            Some(&"http://proxy.example.com".to_string())
        );
    }

    #[tokio::test]
    async fn explicit_environment_overrides_a_proxy_derived_value_on_collision() {
        let mut container_config = container("alpine:3.18", None);
        container_config.environment = Some(HashMap::from([(
            "http_proxy".to_string(),
            "http://explicit.example.com".to_string(),
        )]));
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container_config);
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).with_host_env(|name| {
            (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
        });

        engine.run_task("run", &[]).await.unwrap();

        let environment = docker.environment_for("app").unwrap();
        assert_eq!(
            environment.get("http_proxy"),
            Some(&"http://explicit.example.com".to_string()),
            "the container's own explicit environment should win over the proxy-derived value"
        );
    }

    #[tokio::test]
    async fn no_proxy_vars_flag_suppresses_propagation() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone())
            .with_host_env(|name| {
                (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
            })
            .without_proxy_environment_variables();

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.environment_for("app"),
            None,
            "--no-proxy-vars should suppress propagation entirely"
        );
    }

    #[tokio::test]
    async fn a_dependencys_name_is_exempted_from_the_tasks_own_no_proxy() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        containers.insert("database".to_string(), container("postgres:16", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).with_host_env(|name| {
            (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
        });

        engine.run_task("run", &[]).await.unwrap();

        let app_no_proxy = docker.environment_for("app").unwrap();
        let app_no_proxy = app_no_proxy.get("no_proxy").unwrap();
        assert!(app_no_proxy.split(',').any(|entry| entry == "database"));
        assert!(app_no_proxy.split(',').any(|entry| entry == "app"));

        let database_no_proxy = docker.environment_for("database").unwrap();
        let database_no_proxy = database_no_proxy.get("no_proxy").unwrap();
        assert!(database_no_proxy
            .split(',')
            .any(|entry| entry == "database"));
        assert!(database_no_proxy.split(',').any(|entry| entry == "app"));
    }

    #[tokio::test]
    async fn a_task_level_dependencys_name_is_exempted_from_the_tasks_own_no_proxy() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        containers.insert("queue".to_string(), container("redis:7", None));
        let mut tasks = HashMap::new();
        let mut run_task = task("app", "echo hi");
        run_task.dependencies = Some(vec!["queue".to_string()]);
        tasks.insert("run".to_string(), run_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).with_host_env(|name| {
            (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
        });

        engine.run_task("run", &[]).await.unwrap();

        let app_no_proxy = docker.environment_for("app").unwrap();
        let app_no_proxy = app_no_proxy.get("no_proxy").unwrap();
        assert!(app_no_proxy.split(',').any(|entry| entry == "queue"));
    }

    #[tokio::test]
    async fn customise_overrides_a_dependencys_working_directory_environment_and_ports() {
        let mut containers = HashMap::new();
        let mut database =
            container_with_ports("postgres:16", vec![single_port(5432, 5432, "tcp")]);
        database.environment = Some(HashMap::from([("BASE".to_string(), "base".to_string())]));
        database.working_directory = Some("/from-container".to_string());
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        let mut run_task = task("app", "echo hi");
        run_task.customise = Some(HashMap::from([(
            "database".to_string(),
            TaskContainerCustomisation {
                environment: Some(HashMap::from([(
                    "BASE".to_string(),
                    "overridden".to_string(),
                )])),
                ports: Some(vec![single_port(6543, 6543, "tcp")]),
                working_directory: Some("/from-customise".to_string()),
            },
        )]));
        tasks.insert("run".to_string(), run_task);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("run", &[]).await.unwrap();

        let database_env = docker.environment_for("database").unwrap();
        assert_eq!(database_env.get("BASE"), Some(&"overridden".to_string()));
        assert_eq!(
            docker.working_directory_for("database").as_deref(),
            Some("/from-customise")
        );
        let (_, _, ports) = docker.network_options_for("database").unwrap();
        let ports = ports.unwrap();
        assert!(ports.contains(&(5432, 5432, "tcp".to_string())));
        assert!(ports.contains(&(6543, 6543, "tcp".to_string())));

        // The main task container ("app") must be entirely unaffected — the
        // customisation targets "database" specifically.
        assert_eq!(docker.working_directory_for("app"), None);
    }

    #[tokio::test]
    async fn term_env_var_reaches_a_tasks_own_container_when_interactive() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone())
            .with_host_env(|name| (name == "TERM").then(|| "xterm-256color".to_string()));

        engine.run_task("run", &[]).await.unwrap();

        let environment = docker.environment_for("app").unwrap();
        assert_eq!(environment.get("TERM"), Some(&"xterm-256color".to_string()));
    }

    #[tokio::test]
    async fn term_env_var_is_absent_when_host_has_no_term_set() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).with_host_env(|_| None);

        engine.run_task("run", &[]).await.unwrap();

        assert_eq!(
            docker.environment_for("app"),
            None,
            "an absent host TERM shouldn't inject an empty/placeholder value"
        );
    }

    #[tokio::test]
    async fn term_env_var_does_not_reach_a_dependency_container() {
        let mut containers = HashMap::new();
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        containers.insert("database".to_string(), container("postgres:16", None));
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone())
            .with_host_env(|name| (name == "TERM").then(|| "xterm".to_string()));

        engine.run_task("run", &[]).await.unwrap();

        let app_env = docker.environment_for("app").unwrap();
        assert_eq!(app_env.get("TERM"), Some(&"xterm".to_string()));

        let database_env = docker.environment_for("database");
        assert!(
            database_env.is_none_or(|env| !env.contains_key("TERM")),
            "a dependency should never receive TERM"
        );
    }

    #[tokio::test]
    async fn explicit_environment_overrides_term_on_collision() {
        let mut container_config = container("alpine:3.18", None);
        container_config.environment =
            Some(HashMap::from([("TERM".to_string(), "dumb".to_string())]));
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container_config);
        let mut tasks = HashMap::new();
        tasks.insert("run".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone())
            .with_host_env(|name| (name == "TERM").then(|| "xterm-256color".to_string()));

        engine.run_task("run", &[]).await.unwrap();

        let environment = docker.environment_for("app").unwrap();
        assert_eq!(
            environment.get("TERM"),
            Some(&"dumb".to_string()),
            "the container's own explicit environment should win over the host TERM"
        );
    }

    #[tokio::test]
    async fn term_env_var_is_absent_for_a_prerequisite_tasks_own_container() {
        let mut containers = HashMap::new();
        containers.insert("app".to_string(), container("alpine:3.18", None));
        containers.insert("setup".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("setup".to_string(), task("setup", "echo setting up"));
        tasks.insert(
            "run".to_string(),
            Task {
                run: Some(TaskRun {
                    container: "app".to_string(),
                    command: Some("echo hi".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                }),
                dependencies: None,
                prerequisites: Some(vec!["setup".to_string()]),
                description: None,
                group: None,
                customise: None,
            },
        );
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone())
            .with_host_env(|name| (name == "TERM").then(|| "xterm".to_string()));

        engine.run_task("run", &[]).await.unwrap();

        let app_env = docker.environment_for("app").unwrap();
        assert_eq!(
            app_env.get("TERM"),
            Some(&"xterm".to_string()),
            "the top-level task's own container is interactive-eligible"
        );

        let setup_env = docker.environment_for("setup");
        assert!(
            setup_env.is_none_or(|env| !env.contains_key("TERM")),
            "a prerequisite's own container is never interactive-eligible, so it shouldn't get TERM either"
        );
    }

    #[tokio::test]
    async fn build_args_get_proxy_vars_merged_with_explicit_build_args_winning() {
        let mut build_args = HashMap::new();
        build_args.insert(
            "http_proxy".to_string(),
            "http://explicit.example.com".to_string(),
        );
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container_with_build_directory("./docker", Some(build_args)),
        );
        let mut tasks = HashMap::new();
        tasks.insert("build".to_string(), task("build-env", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone()).with_host_env(|name| match name {
            "http_proxy" => Some("http://proxy.example.com".to_string()),
            "no_proxy" => Some("existing.example.com".to_string()),
            _ => None,
        });

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        let tag = events
            .iter()
            .find_map(|e| e.strip_prefix("build:"))
            .and_then(|rest| rest.split(':').next())
            .expect("image should have been built");
        let build_args = docker.build_args_for(tag).unwrap();

        assert_eq!(
            build_args.get("http_proxy"),
            Some(&"http://explicit.example.com".to_string()),
            "explicit build_args should win over the proxy-derived value"
        );
        assert_eq!(
            build_args.get("no_proxy"),
            Some(&"existing.example.com".to_string()),
            "a proxy var with no explicit build_arg override should still be merged in"
        );
    }

    #[tokio::test]
    async fn dependency_container_environment_reaches_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.environment = Some(HashMap::from([(
            "POSTGRES_PASSWORD".to_string(),
            "secret".to_string(),
        )]));
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        let environment = docker.environment_for("database").unwrap();
        assert_eq!(
            environment.get("POSTGRES_PASSWORD"),
            Some(&"secret".to_string())
        );
    }

    #[tokio::test]
    async fn dependency_container_working_directory_reaches_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.working_directory = Some("/var/lib/postgresql".to_string());
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(
            docker.working_directory_for("database"),
            Some("/var/lib/postgresql".to_string())
        );
    }

    #[tokio::test]
    async fn dependency_container_entrypoint_reaches_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.entrypoint = Some("/entrypoint.sh".to_string());
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(
            docker.entrypoint_for("database"),
            Some("/entrypoint.sh".to_string())
        );
    }

    #[tokio::test]
    async fn dependency_container_command_reaches_the_sidecar() {
        // Before this, a dependency/sidecar container had no way at all to
        // set its own command — only a task's own container could, via
        // `run.command`. redis's default command is what `sidecar.yml`
        // relies on staying alive instead; this proves a dependency can now
        // set an explicit one of its own.
        let mut database = container("postgres:16", None);
        database.command = Some("postgres -c max_connections=200".to_string());
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(
            docker.command_for("database"),
            Some("postgres -c max_connections=200".to_string())
        );
    }

    #[tokio::test]
    async fn dependency_container_labels_reach_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.labels = Some(HashMap::from([(
            "com.example.role".to_string(),
            "database".to_string(),
        )]));
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(
            docker.labels_for("database"),
            Some(HashMap::from([(
                "com.example.role".to_string(),
                "database".to_string()
            )]))
        );
    }

    #[tokio::test]
    async fn dependency_container_capabilities_reach_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.capabilities_to_add = Some(HashSet::from([crate::config::Capability::SysPtrace]));
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(
            docker.capabilities_to_add_for("database"),
            Some(vec!["SYS_PTRACE".to_string()])
        );
    }

    #[tokio::test]
    async fn dependency_container_privileged_reaches_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.privileged = Some(true);
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(docker.privileged_for("database"), Some(true));
    }

    #[tokio::test]
    async fn dependency_container_shm_size_reaches_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.shm_size = Some(256 * 1024 * 1024);
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(docker.shm_size_for("database"), Some(256 * 1024 * 1024));
    }

    #[tokio::test]
    async fn dependency_container_devices_reach_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.devices = Some(vec![crate::config::DeviceMapping {
            local: "/dev/sdb".to_string(),
            container: "/dev/xvdb".to_string(),
            options: None,
        }]);
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(
            docker.devices_for("database"),
            Some(vec![(
                "/dev/sdb".to_string(),
                "/dev/xvdb".to_string(),
                None
            )])
        );
    }

    #[tokio::test]
    async fn dependency_container_enable_init_process_reaches_the_sidecar() {
        let mut database = container("postgres:16", None);
        database.enable_init_process = Some(true);
        let mut containers = HashMap::new();
        containers.insert("database".to_string(), database);
        containers.insert(
            "app".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut tasks = HashMap::new();
        tasks.insert("start".to_string(), task("app", "echo hi"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start", &[]).await.unwrap();

        assert_eq!(docker.enable_init_process_for("database"), Some(true));
    }

    /// Records every posted [`TaskEvent`] in order, so tests can assert on
    /// the user-facing event stream the same way `FakeContainerRuntime`
    /// asserts on Docker calls.
    #[derive(Clone, Default)]
    struct RecordingEventSink {
        events: Arc<Mutex<Vec<TaskEvent>>>,
    }

    impl RecordingEventSink {
        fn events(&self) -> Vec<TaskEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl EventSink for RecordingEventSink {
        fn post(&self, event: TaskEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn posts_lifecycle_events_in_order_for_task_with_dependency() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        let mut database = container("postgres:15", None);
        database.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./init.sh".to_string(),
            working_directory: None,
        }]);
        containers.insert("database".to_string(), database);
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "cargo test"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let sink = RecordingEventSink::default();
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker).with_event_sink(Arc::new(sink.clone()));

        engine.run_task("test", &[]).await.unwrap();

        let events = sink.events();
        // The graph event's container order falls out of a HashMap, so
        // check it structurally here and exclude it from the exact-order
        // check below.
        let TaskEvent::TaskGraphResolved { containers } = &events[1] else {
            panic!("expected TaskGraphResolved second: {events:?}");
        };
        let mut infos = containers.clone();
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].name, "build-env");
        assert_eq!(infos[0].image.as_deref(), Some("alpine:3.18"));
        assert_eq!(infos[0].dependencies, vec!["database".to_string()]);
        assert!(infos[0].is_task_container);
        assert_eq!(infos[1].name, "database");
        assert!(!infos[1].is_task_container);
        assert!(infos[1].dependencies.is_empty());
        let events: Vec<TaskEvent> = events
            .iter()
            .filter(|event| !matches!(event, TaskEvent::TaskGraphResolved { .. }))
            .cloned()
            .collect();
        let expected_prefix = [
            TaskEvent::TaskStarting {
                task: "test".into(),
            },
            TaskEvent::ImagePullStarting {
                image: "postgres:15".into(),
            },
            TaskEvent::ImagePullCompleted {
                image: "postgres:15".into(),
            },
            TaskEvent::ImageResolved {
                container: "database".into(),
            },
            TaskEvent::DependencyStarting {
                container: "database".into(),
            },
            TaskEvent::DependencyStarted {
                container: "database".into(),
            },
            TaskEvent::ContainerBecameHealthy {
                container: "database".into(),
            },
            TaskEvent::RunningSetupCommand {
                container: "database".into(),
                command: "./init.sh".into(),
                index: 1,
                total: 1,
            },
            TaskEvent::SetupCommandsCompleted {
                container: "database".into(),
            },
            TaskEvent::ImagePullStarting {
                image: "alpine:3.18".into(),
            },
            TaskEvent::ImagePullCompleted {
                image: "alpine:3.18".into(),
            },
            TaskEvent::ImageResolved {
                container: "build-env".into(),
            },
            TaskEvent::RunningTaskContainer {
                container: "build-env".into(),
                command: Some("cargo test".into()),
            },
            TaskEvent::CleanupStarting,
            TaskEvent::ContainerRemoved {
                container: "database".into(),
            },
            TaskEvent::RemovingNetwork,
        ];
        assert_eq!(
            &events[..expected_prefix.len()],
            &expected_prefix,
            "full stream: {events:?}"
        );
        // `TaskFinished` carries a wall-clock duration, so match on the
        // variant rather than a full value.
        assert!(
            matches!(
                events.last(),
                Some(TaskEvent::TaskFinished {
                    task,
                    exit_code: 0,
                    ..
                }) if task == "test"
            ),
            "full stream: {events:?}"
        );
        assert_eq!(events.len(), expected_prefix.len() + 1);
    }

    #[tokio::test]
    async fn posts_pull_events_only_when_a_pull_actually_happens() {
        // `Config` isn't `Clone`, so build a fresh one per engine.
        let config = || {
            let mut containers = HashMap::new();
            containers.insert("build-env".to_string(), container("alpine:3.18", None));
            let mut tasks = HashMap::new();
            tasks.insert("test".to_string(), task("build-env", "cargo test"));
            Config {
                project_name: "demo".to_string(),
                containers,
                tasks,
                config_variables: None,
            }
        };

        // Image not local in the fake -> the pull happens and posts events.
        let sink = RecordingEventSink::default();
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config(), docker).with_event_sink(Arc::new(sink.clone()));
        engine.run_task("test", &[]).await.unwrap();
        let events = sink.events();
        assert!(events.contains(&TaskEvent::ImagePullStarting {
            image: "alpine:3.18".into()
        }));
        assert!(events.contains(&TaskEvent::ImagePullCompleted {
            image: "alpine:3.18".into()
        }));

        // Image already local -> `IfNotPresent` (the default) skips the
        // pull, and no pull events post.
        let sink = RecordingEventSink::default();
        let docker = FakeContainerRuntime::default().with_local_image("alpine:3.18");
        let engine = TaskEngine::new(config(), docker).with_event_sink(Arc::new(sink.clone()));
        engine.run_task("test", &[]).await.unwrap();
        let events = sink.events();
        assert!(
            !events.iter().any(|event| matches!(
                event,
                TaskEvent::ImagePullStarting { .. } | TaskEvent::ImagePullCompleted { .. }
            )),
            "no pull events expected: {events:?}"
        );
    }

    #[tokio::test]
    async fn image_resolved_posts_even_when_no_pull_or_build_happens() {
        // ImagePullStarting/Completed and ImageBuildStarting/Completed only
        // post the *first* time a given image/container is resolved this
        // whole invocation (see `resolve_image`'s cross-task dedup) — an
        // already-local image under the default `IfNotPresent` policy
        // never posts any of them at all. ImageResolved is the reliable
        // per-task "this container's image is ready" signal a display
        // needs regardless — this proves it posts even in that case.
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "cargo test"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let sink = RecordingEventSink::default();
        let docker = FakeContainerRuntime::default().with_local_image("alpine:3.18");
        let engine = TaskEngine::new(config, docker).with_event_sink(Arc::new(sink.clone()));
        engine.run_task("test", &[]).await.unwrap();

        let events = sink.events();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TaskEvent::ImagePullStarting { .. })),
            "no pull should have happened: {events:?}"
        );
        assert!(
            events.contains(&TaskEvent::ImageResolved {
                container: "build-env".into()
            }),
            "ImageResolved should still post: {events:?}"
        );
    }

    /// A sink declaring the interleaved I/O policy (the `all` output mode)
    /// — records events like [`RecordingEventSink`], but the engine must
    /// also react to the policy itself. Also declares interest in progress
    /// detail, matching the real `InterleavedEventLogger`'s own override —
    /// see `wants_progress_detail`'s own docs.
    #[derive(Clone, Default)]
    struct InterleavedRecordingSink {
        inner: RecordingEventSink,
    }

    impl EventSink for InterleavedRecordingSink {
        fn post(&self, event: TaskEvent) {
            self.inner.post(event);
        }

        fn container_io_streaming(&self) -> crate::ui::ContainerIoStreaming {
            crate::ui::ContainerIoStreaming::Interleaved
        }

        fn wants_progress_detail(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn setup_command_output_only_posts_when_the_sink_wants_progress_detail() {
        // engine.rs skips constructing/posting SetupCommandOutput entirely
        // when the active sink doesn't render it (every mode but `all`) —
        // proves both halves: a plain RecordingEventSink (matching
        // simple/quiet/fancy, none of which render these) sees none, while
        // an InterleavedRecordingSink (matching `all`) sees the command's
        // output lines.
        let config = config_with_database_dependency(|database| {
            database.setup_commands = Some(vec![crate::config::SetupCommand {
                command: "./seed-data.sh".to_string(),
                working_directory: None,
            }]);
        });
        let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed-data.sh");
        let sink = RecordingEventSink::default();
        let engine = TaskEngine::new(config, docker).with_event_sink(Arc::new(sink.clone()));
        engine.run_task("start", &[]).await.unwrap_err();
        assert!(
            !sink
                .events()
                .iter()
                .any(|event| matches!(event, TaskEvent::SetupCommandOutput { .. })),
            "a sink that doesn't want progress detail should see no SetupCommandOutput events: \
             {:?}",
            sink.events()
        );

        let config = config_with_database_dependency(|database| {
            database.setup_commands = Some(vec![crate::config::SetupCommand {
                command: "./seed-data.sh".to_string(),
                working_directory: None,
            }]);
        });
        let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed-data.sh");
        let sink = InterleavedRecordingSink::default();
        let engine = TaskEngine::new(config, docker).with_event_sink(Arc::new(sink.clone()));
        engine.run_task("start", &[]).await.unwrap_err();
        assert!(
            sink.inner
                .events()
                .contains(&TaskEvent::SetupCommandOutput {
                    container: "database".into(),
                    index: 1,
                    line: "something went wrong".into(),
                }),
            "an interleaved sink should see the command's output: {:?}",
            sink.inner.events()
        );
    }

    #[tokio::test]
    async fn interleaved_policy_disables_interactive_and_sets_dumb_term_everywhere() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            container("alpine:3.18", Some(vec!["database".to_string()])),
        );
        containers.insert("database".to_string(), container("postgres:15", None));
        let mut tasks = HashMap::new();
        tasks.insert("test".to_string(), task("build-env", "cargo test"));
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
        };

        let sink = InterleavedRecordingSink::default();
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone())
            .with_event_sink(Arc::new(sink.clone()))
            // A host TERM that must *not* reach the containers — the
            // interleaved policy forces `dumb` instead.
            .with_host_env(|name| (name == "TERM").then(|| "xterm-256color".to_string()));

        engine.run_task("test", &[]).await.unwrap();

        // The top-level task would normally be interactive-eligible; under
        // the interleaved policy it must not be (no TTY, no stdin).
        assert_eq!(docker.interactive_for("build-env"), Some(false));
        // Every container — the task's own and the dependency — gets
        // TERM=dumb, not the host's own terminal type.
        for name in ["build-env", "database"] {
            let environment = docker
                .environment_for(name)
                .unwrap_or_else(|| panic!("no environment recorded for '{name}'"));
            assert_eq!(
                environment.get("TERM").map(String::as_str),
                Some("dumb"),
                "container '{name}' should get TERM=dumb: {environment:?}"
            );
        }
    }
}
