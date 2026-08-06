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
//! (see [`docs/task-lifecycle.md`](https://github.com/or1can/ratect/blob/main/docs/task-lifecycle.md)), and once-per-session
//! dedup of image pulls/builds/task runs. `TaskEngine` is generic over
//! `ContainerRuntime`. Worth knowing: opt-in settings (`existing_network`,
//! `publish_ports`, etc.) are builder methods rather than `TaskEngine::new`
//! parameters, so each new one lands without a mass-edit of the ~30 existing call
//! sites — with `TaskEngineSettings`/`with_settings` (0.2.0-dev) as the
//! plain-data form of that same set, which is what the *binaries* use (both
//! expose the same ~10 knobs behind differently-named flags, so neither
//! duplicates the builder chain; a new setting needs adding in both places or a
//! binary can't reach it, and this module's own tests keep using the builders,
//! where naming one setting reads better than a mostly-default struct); and only
//! the task actually named on the command line (never a
//! prerequisite) is ever eligible for interactive-TTY mode. `run_task_internal`
//! runs `prerequisites` first, then returns early (no error) if the task itself has
//! no `run` (0.14.0) — everything after can assume `run` is present. `customise`
//! threads through `start_dependency`'s own recursion unconditionally, so it
//! reaches its target regardless of depth in the dependency graph. The task's own
//! container goes through the same readiness gate a dependency always has too
//! (0.21.0, `run_task_container_readiness`) — health-check wait, then
//! `setup_commands`, in order — but run *concurrently* with
//! `ContainerRuntime::run_container`'s own attach-and-wait-for-exit via
//! `tokio::join!` (the engine's first concurrent-exec path), rather than gating
//! anything on it, since nothing else in the graph depends on the task container's
//! own readiness. `run_container` takes two `oneshot::Sender`s for this:
//! `created` (the container's id, sent the moment `create_container` returns —
//! *before* it's started, matching Batect's own `containersCreated` set, which is
//! what its `CleanupStagePlanner` plans removals from) and `started` (a bare `()`,
//! right after Docker's own `start` call, which is when the readiness gate may
//! begin — both its health inspect and its `docker exec` need a *running*
//! container). `created` firing that early is what lets `run_container` `?` freely
//! on every subsequent line: from that instant the engine can remove the
//! container, so no failure can strand it. This replaced (0.25.0) a scheme where
//! `run_container` removed its own container and took a third `readiness` channel
//! purely to order that removal after the gate; the cleanup flags then had to be
//! interpreted identically in two modules, and keeping them in step by hand
//! produced a distinct bug in each of three consecutive review rounds. Don't
//! reintroduce a removal here. See [task
//! lifecycle](https://github.com/or1can/ratect/blob/main/docs/task-lifecycle.md#known-simplifications-relative-to-batect) for
//! the one race this still shares with Batect (a near-instant main command with no
//! `health_check` can still race past a `setup_commands` entry's own `docker exec`)
//! and the one deliberate divergence (the main command is never cancelled early
//! just because the readiness gate fails first, unlike Batect's own coroutine
//! cancellation).
//! `resolve_volumes` (0.18.0) turns a container's `VolumeMount`s into the
//! literal bind strings `docker.rs` expects — a `Local` mount's already fully
//! resolved by `config.rs`, nothing left to do but reassemble the string; a
//! `Cache` mount goes through `cache::resolve_cache_mount`, memoizing the
//! project's own cache key in a `tokio::sync::OnceCell` field (computed at
//! most once per invocation, and only if a `cache` mount is actually
//! resolved — never eagerly). `with_cache_options` (`--cache-type` + the
//! project directory) is `main.rs`'s own builder call, always made in
//! practice despite being optional here, same convention as the other opt-in
//! settings above. `Tmpfs` mounts are deliberately *not* resolved by
//! `resolve_volumes` at all (0.21.0) — a tmpfs mount can't be expressed as a
//! bind string, and needs no async cache-key lookup either, so a separate,
//! synchronous `tmpfs_mounts` helper (alongside `capability_names`/
//! `device_triples`) pulls them out into a new `ContainerOptions.tmpfs` field
//! instead, mapped onto Docker's own `HostConfig.Tmpfs` map by `docker.rs`'s
//! `build_tmpfs_mounts`.

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

/// Converts a `volumes` list's `tmpfs` entries into the plain
/// `(container, options)` pairs `docker.rs`'s `ContainerOptions` expects —
/// same conversion boundary as `capability_names`/`device_triples` above.
/// `Local`/`Cache` entries are skipped here — they're resolved separately, by
/// `TaskEngine::resolve_volumes`, into Docker bind-mount strings instead. A
/// missing `options` is normalized to `""`, matching Batect's own
/// `VolumeMountResolver` (`TmpfsMount(it.containerPath, it.options ?: "")`).
fn tmpfs_mounts(
    volumes: Option<&Vec<crate::config::VolumeMount>>,
) -> Option<Vec<(String, String)>> {
    let mounts: Vec<(String, String)> = volumes?
        .iter()
        .filter_map(|volume| match volume {
            crate::config::VolumeMount::Tmpfs(tmpfs) => Some((
                tmpfs.container.clone(),
                tmpfs.options.clone().unwrap_or_default(),
            )),
            crate::config::VolumeMount::Local(_) | crate::config::VolumeMount::Cache(_) => None,
        })
        .collect();
    if mounts.is_empty() {
        None
    } else {
        Some(mounts)
    }
}

/// Converts a container's parsed `build_secrets`/`build_ssh` config into the
/// docker-side [`crate::docker::BuildKitOptions`] — `None` when neither is
/// set (no session providers to serve; which *builder* runs the build is
/// decided separately, by the `DockerClient` itself, from the daemon's
/// advertised default).
///
/// Each `build_ssh` entry's already-resolved `paths` are classified into a
/// source here — see [`crate::docker::classify_ssh_agent_paths`], which is
/// what makes this fallible. Ids are unique by the time this runs, checked
/// by [`crate::config::Config::resolve_expressions_with`].
///
/// Failures here name an agent id rather than a container, deliberately:
/// [`TaskEngine::resolve_image`] attributes every way one container's build
/// can fail, in a single place, so nothing along the path carries the name
/// itself.
/// Whether a mount's Docker options mark it read-only — the `ro` flag in the
/// comma-separated list Docker itself parses. `rw` is the default and needs
/// no special casing.
fn is_read_only(options: &Option<String>) -> bool {
    options
        .as_deref()
        .is_some_and(|o| o.split(',').any(|flag| flag.trim() == "ro"))
}

fn buildkit_options(container: &Container) -> Result<Option<crate::docker::BuildKitOptions>> {
    let secrets = container.build_secrets.as_ref();
    let ssh = container.build_ssh.as_ref();
    if secrets.is_none() && ssh.is_none() {
        return Ok(None);
    }

    let mut ssh_agents = HashMap::new();
    for agent in ssh.into_iter().flatten() {
        let paths: Vec<PathBuf> = agent.paths.iter().map(PathBuf::from).collect();
        ssh_agents.insert(
            agent.id.clone(),
            crate::docker::classify_ssh_agent_paths(&agent.id, &paths)?,
        );
    }

    Ok(Some(crate::docker::BuildKitOptions {
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
        ssh_agents,
    }))
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
    /// Set by [`TaskEngine::with_interrupt`]: abandons the run when the user
    /// interrupts it, so cleanup still happens. `None` (the default) means
    /// interrupts aren't watched at all and a `SIGINT` kills the process
    /// outright, which is every unit test and was every run before 0.25.0.
    interrupt: Option<Arc<crate::interrupt::Interrupt>>,
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
    /// The creating binary's own version, stamped onto every resource this
    /// engine creates (`crate::labels::VERSION`). `None` in tests, which
    /// have no binary version to report — the label is then omitted rather
    /// than invented. Set via `TaskEngineSettings::ratect_version`, since
    /// `ratect-core`'s own version isn't what a user sees from
    /// `--version` (see `ROADMAP.md`'s versioning section).
    ratect_version: Option<String>,
    /// Memoizes `crate::cache::project_cache_key` for the life of this
    /// `TaskEngine` — computed at most once per invocation, and only if a
    /// `cache` volume is actually resolved (never eagerly), matching
    /// Batect's own `CacheManager.projectCacheKey`'s `by lazy` behavior.
    cache_key: OnceCell<String>,
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
    /// Set by a binary that watches for Ctrl+C, so an interrupted run still
    /// cleans up after itself — see [`TaskEngine::with_interrupt`]. `None`
    /// leaves interrupts unwatched.
    pub interrupt: Option<Arc<crate::interrupt::Interrupt>>,
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
            interrupt: None,
        }
    }
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
            ratect_version: None,
            cache_key: OnceCell::new(),
            interrupt: None,
        }
    }

    /// Makes this engine abandon a run when the user interrupts it (Ctrl+C),
    /// cleaning up what it created rather than leaving it behind — see
    /// [`crate::interrupt`] and `run_task_internal`.
    ///
    /// Opt-in, like the other settings here, and left off by default so a
    /// unit test never picks up the process's real signals: only a binary
    /// that has actually called [`crate::interrupt::Interrupt::listen`]
    /// wants this.
    pub fn with_interrupt(mut self, interrupt: Arc<crate::interrupt::Interrupt>) -> Self {
        self.interrupt = Some(interrupt);
        self
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

    /// Applies a whole [`TaskEngineSettings`] at once — every builder method
    /// above, driven by plain data instead of a chain of `if flag { engine =
    /// engine.without_x() }` at a call site.
    ///
    /// This exists for the binaries, which all have the same ~10 knobs
    /// behind differently-named flags; the builders stay the interface for
    /// everything else (notably this module's own tests, where naming the
    /// one setting under test reads better than a mostly-default struct).
    /// Anything added here needs adding to [`TaskEngineSettings`] too, or a
    /// binary has no way to reach it.
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
            interrupt,
        } = settings;
        self.ratect_version = ratect_version;
        if let Some(interrupt) = interrupt {
            self = self.with_interrupt(interrupt);
        }
        if let Some(network) = existing_network {
            self = self.with_existing_network(network);
        }
        if !publish_ports {
            self = self.without_port_publishing();
        }
        if !propagate_proxy_environment_variables {
            self = self.without_proxy_environment_variables();
        }
        if !run_prerequisites {
            self = self.without_prerequisites();
        }
        if !image_overrides.is_empty() {
            self = self.with_image_overrides(image_overrides)?;
        }
        if !image_tags.is_empty() {
            self = self.with_image_tags(image_tags);
        }
        if !cleanup_after_success {
            self = self.without_cleanup_after_success();
        }
        if !cleanup_after_failure {
            self = self.without_cleanup_after_failure();
        }
        if let Some(max) = max_parallelism {
            self = self.with_max_parallelism(max);
        }
        if let Some((cache_type, project_directory)) = cache {
            self = self.with_cache_options(cache_type, project_directory);
        }
        Ok(self)
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
    /// `Tmpfs` mounts are skipped entirely here — they can't be expressed as
    /// a bind string at all, and need no async resolution, so they're pulled
    /// out separately (and synchronously) by `tmpfs_mounts` instead.
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
                crate::config::VolumeMount::Tmpfs(_) => {}
            }
        }

        if resolved.is_empty() {
            Ok(None)
        } else {
            Ok(Some(resolved))
        }
    }

    /// Runs one cleanup step, giving up on it if the user interrupts again.
    ///
    /// `false` means the step lost the race and was dropped mid-flight —
    /// whatever it was removing may still be there. `after` is the interrupt
    /// count that was already reached when the run ended, so only a *further*
    /// press counts (see `run_task_internal`).
    ///
    /// With no interrupt tracker the step simply runs, which is every unit
    /// test that doesn't opt in and both binaries before 0.25.0.
    async fn until_interrupted(
        &self,
        after: usize,
        step: impl std::future::Future<Output = ()>,
    ) -> bool {
        match &self.interrupt {
            Some(interrupt) => {
                tokio::select! {
                    biased;
                    () = step => true,
                    () = interrupt.wait_for(after + 1) => false,
                }
            }
            None => {
                step.await;
                true
            }
        }
    }

    /// The task's own container's readiness gate: waits for it to report
    /// healthy, then runs its `setup_commands` in order — the same two
    /// gates `ensure_container_ready` applies to a dependency, ported here
    /// almost unchanged (this doesn't share that code directly since a
    /// dependency's version also handles `customise`/cache-key/dedup
    /// concerns that don't apply to the one, always-present task
    /// container). Unlike a dependency, nothing in the graph depends on
    /// *this* container's own readiness, so the caller runs this
    /// concurrently with `run_container`'s own attach-and-wait-for-exit
    /// (via `tokio::join!`) rather than gating anything on it — matching
    /// Batect, which generates the identical health-check-wait/
    /// `setup_commands` steps for every container, task container
    /// included, and runs them concurrently with that container's own
    /// command (see docs/task-lifecycle.md's "Known simplifications").
    ///
    /// `container_id` must already be running: the caller waits for
    /// `run_container`'s own `started` signal before calling here, since
    /// both gates need a running container (a health status only appears
    /// once one exists, and `docker exec` refuses anything else).
    ///
    /// One deliberate divergence from Batect, left for simplicity: Batect
    /// cancels the still-running main command early the moment this gate
    /// fails (via coroutine cancellation); Ratect always lets the main
    /// command run to completion regardless. Either way the task is
    /// reported as failed overall — this only affects how much of the main
    /// command's own output/runtime you see before that failure is
    /// reported.
    async fn run_task_container_readiness(
        &self,
        container_id: &str,
        name: &str,
        container_config: &crate::config::Container,
        environment: Option<&HashMap<String, String>>,
        user_mapping: Option<&crate::docker::UserMapping>,
    ) -> Result<()> {
        self.docker
            .wait_for_container_healthy(container_id)
            .await
            .with_context(|| format!("Container '{}' did not become healthy", name))?;
        self.event_sink.post(TaskEvent::ContainerBecameHealthy {
            container: name.to_string(),
        });

        let setup_command_total = container_config.setup_commands.as_ref().map_or(0, Vec::len);
        for (setup_command_index, setup_command) in
            container_config.setup_commands.iter().flatten().enumerate()
        {
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
                        container_id,
                        &setup_command.command,
                        setup_command
                            .working_directory
                            .as_deref()
                            .or(container_config.working_directory.as_deref()),
                        environment,
                        user_mapping,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to run setup command '{}' in container '{}'",
                            setup_command.command, name
                        )
                    })?
            };
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

        Ok(())
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
                    let build: Result<String> = async {
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
                        let buildkit = buildkit_options(container_config)?;
                        // Batect's second use of `image_pull_policy`: on a
                        // `build_directory` container, `always` force-pulls
                        // the build's own base image before building
                        // (`docker build --pull`), distinct from its other
                        // use gating whether an `image` container's own
                        // image gets pulled (`resolve_pulled_image` above).
                        let force_pull = container_config.image_pull_policy.unwrap_or_default()
                            == crate::config::ImagePullPolicy::Always;
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
                                force_pull,
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
                // shares the gap; skipping is the safe side of it.
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
        // Identifies every resource this one task execution creates (see
        // `crate::labels`). The id is generated here rather than alongside
        // the network below, so it exists even under `--use-network`, where
        // no network is created at all — the containers still need to agree
        // on which run they belong to.
        let run_id = Uuid::new_v4().to_string();
        let run_labels = crate::labels::RunLabels::new(
            &self.config.project_name,
            task_name,
            &run_id,
            self.ratect_version.as_deref(),
        );

        // Recorded the moment Docker reports the task's own container as
        // *created* — before it is started, and regardless of how the run
        // ends. `run_container` removes nothing, so this is the only record
        // of the container, and the cleanup stage below is the only place it
        // is removed. Matches Batect's `containersCreated`, which is what its
        // `CleanupStagePlanner` plans removals from.
        let task_container_id: Mutex<Option<String>> = Mutex::new(None);

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
                    // Named after this run's own id, so the network's name
                    // and its `run` label always agree.
                    let name = format!("ratect-{run_id}");
                    self.docker
                        .create_network(&name, &run_labels.for_network())
                        .await?;
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
                    &run_labels,
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
            // The task's own container goes through the same readiness
            // gate as any dependency (health-check wait, then
            // `setup_commands`, in order) — matching Batect, which runs
            // every container through identical per-container steps. Unlike
            // a dependency, nothing else in the graph depends on the task
            // container's own readiness, so this runs *concurrently* with
            // its main command instead of gating anything — see
            // `run_task_container_readiness`'s own doc comment.
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
            let tmpfs = tmpfs_mounts(container_config.volumes.as_ref());
            let labels = run_labels.for_container(
                &run.container,
                crate::labels::ContainerRole::Task,
                container_config.labels.as_ref(),
            );
            let container_options = crate::docker::ContainerOptions {
                working_directory,
                entrypoint,
                labels: Some(&labels),
                capabilities_to_add: capabilities_to_add.as_ref(),
                capabilities_to_drop: capabilities_to_drop.as_ref(),
                privileged: container_config.privileged,
                shm_size: container_config.shm_size,
                devices: devices.as_ref(),
                enable_init_process: container_config.enable_init_process,
                log_driver: container_config.log_driver.as_deref(),
                log_options: container_config.log_options.as_ref(),
                tmpfs: tmpfs.as_ref(),
            };
            self.event_sink.post(TaskEvent::RunningTaskContainer {
                container: run.container.clone(),
                command: command.map(str::to_string),
            });
            let volumes = self
                .resolve_volumes(container_config.volumes.as_ref())
                .await?;
            let (created_tx, created_rx) = tokio::sync::oneshot::channel();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let run_future = self.docker.run_container(
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
                Some(created_tx),
                Some(started_tx),
            );
            // Two jobs, in order: take ownership of the container as soon as
            // it exists, then gate on its readiness once it's actually
            // running. `tokio::join!` below is what drives this concurrently
            // with `run_future` — without something polling it, neither
            // channel would ever resolve.
            let readiness_future = async {
                // A dropped sender means `run_container` failed before
                // getting this far; its own error is the report, and there
                // is nothing of ours to record or wait on.
                let Ok(container_id) = created_rx.await else {
                    return Ok(());
                };
                // Recorded before the container is even started, matching
                // Batect's `containersCreated` (the set its cleanup stage
                // plans removals from) rather than `containersStarted`. A
                // container that fails to start still has to be removed.
                *task_container_id.lock().unwrap() = Some(container_id.clone());
                // Posted from the same place, so a display's idea of what is
                // outstanding can never disagree with what cleanup will
                // actually remove.
                self.event_sink.post(TaskEvent::TaskContainerCreated {
                    container: run.container.clone(),
                });
                if started_rx.await.is_err() {
                    return Ok(());
                }
                self.run_task_container_readiness(
                    &container_id,
                    &run.container,
                    container_config,
                    environment.as_ref(),
                    user_mapping.as_ref(),
                )
                .await
            };
            let (run_result, readiness_result) = tokio::join!(run_future, readiness_future);
            // Ordered, not merged: a nonzero exit code is the task's own
            // verdict and the more useful thing to report, so it wins over a
            // readiness failure that happened alongside it. This is the
            // precedence `run_container` used to apply internally, kept.
            run_result?;
            readiness_result?;

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
        // The residual race is Batect's too: a container created but not yet
        // recorded (in `running_sidecars`, or `task_container_id` below) is
        // dropped before cleanup can see it, and survives. The ownership
        // labels are the answer to that — `ratect resources` finds exactly
        // this — rather than an ordering that could avoid it.
        // Captured in the same expression that ends the run, not further
        // down: every interrupt after this instant is one the *cleanup*
        // should react to, and reading the count later would fold anything
        // arriving in between into the baseline and swallow it. The window
        // is small either way, but it is the window that matters.
        let (result, interrupts_before_cleanup): (Result<()>, usize) = match &self.interrupt {
            Some(interrupt) => {
                tokio::select! {
                    biased;
                    result = execution => (result, interrupt.count()),
                    () = interrupt.interrupted() => (
                        Err(anyhow::Error::new(crate::interrupt::TaskInterrupted)),
                        interrupt.count(),
                    ),
                }
            }
            None => (execution.await, 0),
        };
        let interrupted = result
            .as_ref()
            .err()
            .is_some_and(|error| error.is::<crate::interrupt::TaskInterrupted>());
        let task_container_id = task_container_id.into_inner().unwrap();
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

        // A Ctrl+C during cleanup abandons the cleanup itself. Cleanup talks
        // to the daemon and isn't instant — a container ignoring `SIGTERM`
        // waits out Docker's full kill timeout — so "stop now" has to mean
        // something. Batect lands in the same place: an interrupt during its
        // cleanup stage switches it to `PostTaskManualCleanup.Required`.
        //
        // Measured against the count when the run *ended* (captured above),
        // not a fixed `>= 2`. Arming the handler replaces the process's
        // default `SIGINT` behaviour for the whole run, so an interrupt
        // Ratect doesn't act on is one it has silently swallowed — and a
        // fixed threshold swallows the first Ctrl+C during the cleanup of a
        // run that was never interrupted (the common case: a task finished,
        // cleanup is slow, the user wants out). Relative to the baseline,
        // one press abandons cleanup after a normal run and a second does
        // after an interrupted one, which is the same rule stated once.
        //
        // One press is still absorbed rather than acted on: the one that
        // arrives in the same instant the run completes, which `biased`
        // resolves in the run's favour. That tie is deliberate — the run
        // finished, so there is nothing left to abandon except the cleanup
        // the user probably wants — but it is an absorbed press, not a
        // guarantee that none exist.
        let abandon_after = interrupts_before_cleanup;

        if should_cleanup {
            if interrupted {
                tracing::warn!(
                    task = task_name,
                    "Interrupted; cleaning up. Press Ctrl+C again to stop cleaning up."
                );
            }
            if !running_sidecars.is_empty() || owns_network || task_container_id.is_some() {
                self.event_sink.post(TaskEvent::CleanupStarting);
            }
            // Every removal below is raced against the next interrupt rather
            // than merely checked between removals. Checking between them
            // misses the case the feature exists for: a container ignoring
            // `SIGTERM` sits in `stop_and_remove_container` for Docker's full
            // kill timeout, which is exactly when the user presses Ctrl+C
            // again — and with one container and `--use-network`, "between
            // removals" never comes around again at all.
            //
            // Losing the race cancels that removal's request in flight. Docker
            // may still finish it server-side; either way the container is
            // reported as possibly left behind, which is the honest reading.
            let mut abandoned = false;

            // First, and unconditionally: `run_container` never removes the
            // container it creates, so this is the only place the task's own
            // container is removed — however the run ended. `Some` exactly
            // when Docker got as far as creating one, which is also the only
            // case where there is anything to remove. Before the sidecars it
            // depends on, matching Batect's own dependency-ordered cleanup.
            if let Some(container_id) = task_container_id.as_ref() {
                let removal = async {
                    match self.docker.stop_and_remove_container(container_id).await {
                        Ok(()) => self.event_sink.post(TaskEvent::ContainerRemoved {
                            container: run.container.clone(),
                        }),
                        Err(e) => tracing::warn!(
                            container = run.container.as_str(),
                            error = ?e,
                            "Failed to clean up the task's own container"
                        ),
                    }
                };
                abandoned = !self.until_interrupted(abandon_after, removal).await;
            }
            for (name, container_id) in &running_sidecars {
                if abandoned {
                    break;
                }
                let removal = async {
                    match self.docker.stop_and_remove_container(container_id).await {
                        Ok(()) => self.event_sink.post(TaskEvent::ContainerRemoved {
                            container: name.clone(),
                        }),
                        Err(e) => tracing::warn!(
                            dependency = name.as_str(),
                            error = ?e,
                            "Failed to clean up dependency container"
                        ),
                    }
                };
                abandoned = !self.until_interrupted(abandon_after, removal).await;
            }
            if owns_network && !abandoned {
                let network_name = network_name.expect("owns_network implies network_name is Some");
                self.event_sink.post(TaskEvent::RemovingNetwork);
                let removal = async {
                    if let Err(e) = self.docker.remove_network(&network_name).await {
                        tracing::warn!(network = network_name.as_str(), error = ?e, "Failed to remove network");
                    }
                };
                abandoned = !self.until_interrupted(abandon_after, removal).await;
            }
            // Driven by work actually being cut short, not by the raw count:
            // an interrupt arriving once everything is already removed leaves
            // nothing behind, and sending someone hunting for leftovers that
            // don't exist is its own small betrayal.
            if abandoned {
                // Names the label rather than a `ratect` command: this is
                // shared core, and `resources` is a `ratect` verb that
                // `ratect-compat` doesn't have — so naming it would be wrong
                // advice for half the binaries that reach this line. The
                // label works for both, and is what `resources` itself
                // searches on.
                //
                // Both `ps` and `network ls`, because the abandoned step is
                // just as often the network: with no sidecars it is the only
                // thing left to abandon, so a containers-only hint would list
                // nothing at all in exactly that case.
                tracing::warn!(
                    task = task_name,
                    run = run_id.as_str(),
                    "Stopped cleaning up. Anything left behind carries the label \
                     eu.orican.ratect.run with this run's id — `docker ps -a --filter \
                     label=eu.orican.ratect.run=<run>` and `docker network ls --filter \
                     label=eu.orican.ratect.run=<run>` list it."
                );
            }
        } else {
            // The task's own container is reported here like any other:
            // nothing else deals with it now, so a run kept for
            // investigation says so once, for everything it kept.
            let kept_task_container = task_container_id.as_ref().map(|_| run.container.as_str());
            if !running_sidecars.is_empty() || owns_network || kept_task_container.is_some() {
                tracing::info!(
                    task = task_name,
                    task_container = kept_task_container,
                    dependencies = running_sidecars.len(),
                    network = network_name.as_deref(),
                    "cleanup disabled; leaving containers and the task network in place \
                     for investigation"
                );
            }
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
        run_labels: &crate::labels::RunLabels,
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
                            run_labels,
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
                    let tmpfs = tmpfs_mounts(dependency_config.volumes.as_ref());
                    let working_directory = customisation
                        .and_then(|c| c.working_directory.as_deref())
                        .or(dependency_config.working_directory.as_deref());
                    let labels = run_labels.for_container(
                        name,
                        crate::labels::ContainerRole::Dependency,
                        dependency_config.labels.as_ref(),
                    );
                    let container_options = crate::docker::ContainerOptions {
                        working_directory,
                        entrypoint: dependency_config.entrypoint.as_deref(),
                        labels: Some(&labels),
                        capabilities_to_add: capabilities_to_add.as_ref(),
                        capabilities_to_drop: capabilities_to_drop.as_ref(),
                        privileged: dependency_config.privileged,
                        shm_size: dependency_config.shm_size,
                        devices: devices.as_ref(),
                        enable_init_process: dependency_config.enable_init_process,
                        log_driver: dependency_config.log_driver.as_deref(),
                        log_options: dependency_config.log_options.as_ref(),
                        tmpfs: tmpfs.as_ref(),
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
#[path = "engine_tests.rs"]
mod tests;
