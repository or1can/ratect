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

use crate::config::{BuildSecret, Config, Container, PortMapping};
use crate::docker::ContainerRuntime;
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// Returns `root` plus every container name transitively reachable from it
/// via `dependencies` — the full set of containers that will share one
/// task's network. Used as the `no_proxy` "these are local, don't proxy
/// traffic to them" exemption list passed to
/// `proxy::proxy_environment_variables`.
///
/// Visited-set-guarded so a config cycle can't hang this pure walk — real
/// cycle detection (which actually rejects a cycle as a user-facing error)
/// still happens separately, in `TaskEngine::start_dependency`.
fn container_names_in_task(
    containers: &HashMap<String, Container>,
    root: &str,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(name) = stack.pop() {
        if !names.insert(name.clone()) {
            continue;
        }
        if let Some(dependencies) = containers.get(&name).and_then(|c| c.dependencies.as_ref()) {
            stack.extend(dependencies.iter().cloned());
        }
    }
    names
}

pub struct TaskEngine<D: ContainerRuntime + Send + Sync> {
    config: Config,
    docker: D,
    executed_tasks: Mutex<HashSet<String>>,
    pulled_images: Mutex<HashSet<String>>,
    /// Container name -> ID of the image built for it, so a container with
    /// `build_directory` is only ever built once per invocation even if
    /// referenced by multiple tasks or as both a dependency and a task's
    /// own container. Keyed by container name (not build directory) since
    /// a given name always has the same `build_directory`/`build_args`
    /// within one `Config`. Stores the image ID (not the human-readable tag)
    /// — see `resolve_image` for why.
    built_images: Mutex<HashMap<String, String>>,
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
}

impl<D: ContainerRuntime + Send + Sync> TaskEngine<D> {
    pub fn new(config: Config, docker: D) -> Self {
        Self {
            config,
            docker,
            executed_tasks: Mutex::new(HashSet::new()),
            pulled_images: Mutex::new(HashSet::new()),
            built_images: Mutex::new(HashMap::new()),
            in_progress_tasks: Mutex::new(HashSet::new()),
            existing_network: None,
            publish_ports: true,
            propagate_proxy_environment_variables: true,
            host_env: Box::new(|name| std::env::var(name).ok()),
        }
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

    /// The host's `TERM` to inject into the invoked task's own container's
    /// environment, or `None` when this isn't that container (`interactive`
    /// is `false` — never a prerequisite's, a dependency's, or a sidecar's,
    /// nor an image build) or the host has no `TERM` set. Gated on
    /// `interactive` alone — deliberately *not* on whether a real TTY ends
    /// up being allocated (that's decided later, inside
    /// `ContainerRuntime::run_container`, from information not yet known
    /// here) — matching Batect's own `ConsoleInfo.terminalType`/
    /// `TaskContainerOnlyIOStreamingOptions.terminalTypeForContainer`, both
    /// unconditional on any TTY check. So a full-screen terminal program
    /// inside the container knows the terminal type even when piping output
    /// elsewhere still lets it detect it isn't attached to a real TTY.
    fn term_environment_variable(&self, interactive: bool) -> Option<HashMap<String, String>> {
        if !interactive {
            return None;
        }
        let term = (self.host_env)("TERM")?;
        Some(HashMap::from([("TERM".to_string(), term)]))
    }

    /// Resolves `container_config`'s `image` (pulling it, deduped by image
    /// name) or `build_directory` (building it, deduped by `container_name`)
    /// into the image reference to actually run. Shared by a task's own
    /// container and its dependency containers — both need exactly this and
    /// nothing else, which is also why dependency containers now support
    /// `build_directory` (they didn't before this was unified).
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
        if let Some(image) = &container_config.image {
            let needs_pull = {
                let pulled = self.pulled_images.lock().unwrap();
                !pulled.contains(image)
            };

            if needs_pull {
                self.docker.pull_image(image).await?;
                let mut pulled = self.pulled_images.lock().unwrap();
                pulled.insert(image.to_string());
            }

            Ok(image.clone())
        } else if let Some(build_directory) = &container_config.build_directory {
            let existing_image_id = {
                let built = self.built_images.lock().unwrap();
                built.get(container_name).cloned()
            };
            if let Some(image_id) = existing_image_id {
                return Ok(image_id);
            }

            let tag = format!("{}-{}", self.config.project_name, container_name);
            // No `extra_no_proxy_entries` at build time — matches Batect,
            // which never adds container names to `no_proxy` for a build
            // (nothing's running yet to be exempted from proxying).
            let proxy_vars = self.proxy_environment_variables(&std::collections::BTreeSet::new());
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

            let mut built = self.built_images.lock().unwrap();
            built.insert(container_name.to_string(), image_id.clone());

            Ok(image_id)
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
    pub async fn run_task(&self, task_name: &str, additional_args: &[String]) -> Result<()> {
        self.run_task_scoped(task_name, additional_args, true).await
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
        let task = self
            .config
            .tasks
            .get(task_name)
            .with_context(|| format!("Task '{}' not found", task_name))?;

        // Run prerequisites (never with additional args, and never eligible
        // for interactive TTY attachment — both scoped to only the
        // originally-requested task).
        if let Some(prerequisites) = &task.prerequisites {
            for prerequisite in prerequisites {
                self.run_task_scoped(prerequisite, &[], false).await?;
            }
        }

        // Run the task itself
        let container_config = self
            .config
            .containers
            .get(&task.run.container)
            .with_context(|| format!("Container '{}' not found", task.run.container))?;

        tracing::info!("Running task '{}'", task_name);

        // A task-scoped network + its dependency containers: created fresh for
        // this one task execution and torn down before this function returns,
        // regardless of outcome. Not shared across tasks — see docs/task-lifecycle.md.
        // Always created, even with no dependencies, so the task's own
        // container is never left on Docker's shared default bridge network.
        //
        // Unless `--use-network` was given (`self.existing_network`), in which
        // case that network is validated to exist and reused instead —
        // checked fresh on every task execution, never cached — and, since
        // Ratect didn't create it, it's never removed during cleanup either
        // (matching Batect: cleanup only ever tears down networks it created
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

        let mut running_sidecars: HashMap<String, String> = HashMap::new();
        let mut resolving: HashSet<String> = HashSet::new();
        // Fixed for the whole task, computed once up front — every
        // container started for this task (the task's own and each
        // dependency) gets the same `no_proxy` exemption list, matching
        // Batect's `allContainersInNetwork` being fixed for the whole graph
        // rather than recomputed per container.
        let no_proxy_entries =
            container_names_in_task(&self.config.containers, &task.run.container);

        let result: Result<()> = async {
            if let Some(deps) = &container_config.dependencies {
                for dep_name in deps {
                    self.start_dependency(
                        dep_name,
                        &network_name,
                        &mut running_sidecars,
                        &mut resolving,
                        &no_proxy_entries,
                    )
                    .await?;
                }
            }

            let image = self
                .resolve_image(&task.run.container, container_config)
                .await?;
            // Eligibility only — `ContainerRuntime::run_container` further
            // gates this on the local process's own stdin/stdout genuinely
            // being terminals before actually attaching a TTY, and stdin
            // forwarding on `interactive` alone (see `run_container`'s own
            // docs). Computed here, ahead of the environment merge below,
            // since `term_environment_variable` needs it.
            let interactive = top_level;
            let proxy_vars = self.proxy_environment_variables(&no_proxy_entries);
            let term_var = self.term_environment_variable(interactive);
            let environment = merged_environment(
                term_var.as_ref(),
                proxy_vars.as_ref(),
                container_config.environment.as_ref(),
                task.run.environment.as_ref(),
            );
            let user_mapping = self.resolve_user_mapping(container_config).await?;
            let expanded_ports =
                merged_ports(container_config.ports.as_ref(), task.run.ports.as_ref());
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
            let working_directory = task
                .run
                .working_directory
                .as_deref()
                .or(container_config.working_directory.as_deref());
            let entrypoint = task
                .run
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
            self.docker
                .run_container(
                    &task.run.container,
                    &image,
                    task.run.command.as_deref(),
                    additional_args,
                    container_config.volumes.as_ref(),
                    environment.as_ref(),
                    &network_name,
                    interactive,
                    user_mapping.as_ref(),
                    &network_options,
                    health_check.as_ref(),
                    &container_options,
                )
                .await?;

            Ok(())
        }
        .await;

        for (name, container_id) in &running_sidecars {
            if let Err(e) = self.docker.stop_and_remove_container(container_id).await {
                tracing::warn!(
                    dependency = name.as_str(),
                    error = ?e,
                    "Failed to clean up dependency container"
                );
            }
        }
        if self.existing_network.is_none() {
            if let Err(e) = self.docker.remove_network(&network_name).await {
                tracing::warn!(network = network_name.as_str(), error = ?e, "Failed to remove network");
            }
        }

        result
    }

    /// Recursively starts `name` (and, first, any containers it depends on)
    /// as a background container on `network`, scoped to a single task's
    /// execution. `running` dedupes within that scope; `resolving` detects
    /// circular container dependencies.
    #[async_recursion]
    #[allow(clippy::too_many_arguments)]
    async fn start_dependency(
        &self,
        name: &str,
        network: &str,
        running: &mut HashMap<String, String>,
        resolving: &mut HashSet<String>,
        no_proxy_entries: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        if running.contains_key(name) {
            return Ok(());
        }
        if resolving.contains(name) {
            return Err(anyhow::anyhow!(
                "Circular container dependency detected involving '{}'",
                name
            ));
        }
        resolving.insert(name.to_string());

        let dependency_config = self
            .config
            .containers
            .get(name)
            .with_context(|| format!("Container '{}' not found", name))?;

        if let Some(nested) = &dependency_config.dependencies {
            for nested_name in nested {
                self.start_dependency(nested_name, network, running, resolving, no_proxy_entries)
                    .await?;
            }
        }

        let image = self.resolve_image(name, dependency_config).await?;
        let user_mapping = self.resolve_user_mapping(dependency_config).await?;
        let proxy_vars = self.proxy_environment_variables(no_proxy_entries);
        let environment = merged_environment(
            None,
            proxy_vars.as_ref(),
            dependency_config.environment.as_ref(),
            None,
        );
        let expanded_ports = merged_ports(dependency_config.ports.as_ref(), None);
        let network_options = crate::docker::NetworkOptions {
            additional_hostnames: dependency_config.additional_hostnames.as_ref(),
            additional_hosts: dependency_config.additional_hosts.as_ref(),
            ports: (self.publish_ports && !expanded_ports.is_empty()).then_some(&expanded_ports),
        };

        let health_check = health_check_options(dependency_config);
        let capabilities_to_add = capability_names(dependency_config.capabilities_to_add.as_ref());
        let capabilities_to_drop =
            capability_names(dependency_config.capabilities_to_drop.as_ref());
        let devices = device_triples(dependency_config.devices.as_ref());
        let container_options = crate::docker::ContainerOptions {
            working_directory: dependency_config.working_directory.as_deref(),
            entrypoint: dependency_config.entrypoint.as_deref(),
            labels: dependency_config.labels.as_ref(),
            capabilities_to_add: capabilities_to_add.as_ref(),
            capabilities_to_drop: capabilities_to_drop.as_ref(),
            privileged: dependency_config.privileged,
            shm_size: dependency_config.shm_size,
            devices: devices.as_ref(),
            enable_init_process: dependency_config.enable_init_process,
        };

        let container_id = self
            .docker
            .start_background_container(
                name,
                &image,
                dependency_config.volumes.as_ref(),
                environment.as_ref(),
                network,
                user_mapping.as_ref(),
                &network_options,
                health_check.as_ref(),
                &container_options,
            )
            .await?;

        resolving.remove(name);
        // Registered for cleanup *before* the readiness gate below — a
        // dependency that starts but never becomes healthy (or whose setup
        // command fails) still gets stopped and removed.
        running.insert(name.to_string(), container_id.clone());

        // Batect's readiness gate (see docs/task-lifecycle.md): started
        // isn't ready. The dependency must report healthy (immediate for a
        // container with no health check at all), then every one of its
        // setup commands must succeed, before anything that depends on it
        // starts.
        self.docker
            .wait_for_container_healthy(&container_id)
            .await
            .with_context(|| format!("Container '{}' did not become healthy", name))?;

        for setup_command in dependency_config.setup_commands.iter().flatten() {
            tracing::info!(
                container = name,
                command = setup_command.command.as_str(),
                "Running setup command"
            );
            let result = self
                .docker
                .exec_in_container(
                    &container_id,
                    &setup_command.command,
                    setup_command
                        .working_directory
                        .as_deref()
                        .or(dependency_config.working_directory.as_deref()),
                    environment.as_ref(),
                    user_mapping.as_ref(),
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to run setup command '{}' in container '{}'",
                        setup_command.command, name
                    )
                })?;
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

        Ok(())
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
                interactive: Default::default(),
                user_mapping: Default::default(),
                network_exists_result: Arc::new(Mutex::new(true)),
                network_options: Default::default(),
                health_checks: Default::default(),
                container_options: Default::default(),
                execs: Default::default(),
                unhealthy_container: Default::default(),
                failing_setup_command: Default::default(),
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
            self.push(format!("pull:{image}"));
            Ok(())
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
            _volumes: Option<&Vec<String>>,
            environment: Option<&HashMap<String, String>>,
            network: &str,
            user_mapping: Option<&crate::docker::UserMapping>,
            network_options: &crate::docker::NetworkOptions,
            health_check: Option<&crate::docker::HealthCheckOptions>,
            container_options: &crate::docker::ContainerOptions,
        ) -> Result<String> {
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
            run: TaskRun {
                container: container.to_string(),
                command: Some(command.to_string()),
                environment: None,
                ports: None,
                working_directory: None,
                entrypoint: None,
            },
            prerequisites: None,
        }
    }

    fn config_with_cycle() -> Config {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                build_args: None,
                image: Some("alpine:3.18".to_string()),
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
                run: TaskRun {
                    container: "build-env".to_string(),
                    command: None,
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                },
                prerequisites: Some(vec!["b".to_string()]),
            },
        );
        tasks.insert(
            "b".to_string(),
            Task {
                run: TaskRun {
                    container: "build-env".to_string(),
                    command: None,
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                },
                prerequisites: Some(vec!["a".to_string()]),
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
            run: TaskRun {
                container: "build-env".to_string(),
                command: Some(command.to_string()),
                environment: None,
                ports: None,
                working_directory: None,
                entrypoint: None,
            },
            prerequisites,
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
                run: TaskRun {
                    container: "app".to_string(),
                    command: Some("echo hi".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                },
                prerequisites: Some(vec!["setup".to_string()]),
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
        task_config.run.ports = Some(vec![single_port(9090, 90, "tcp")]);
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
                run: TaskRun {
                    container: "build-env".to_string(),
                    command: Some("echo two".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                },
                prerequisites: Some(vec!["first".to_string()]),
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
                run: TaskRun {
                    container: "app".to_string(),
                    command: Some("test".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                },
                prerequisites: Some(vec!["migrate".to_string()]),
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
        let docker = DockerClient::new().expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(config_with_cycle(), docker);

        let err = engine.run_task("a", &[]).await.unwrap_err();
        assert!(err.to_string().contains("Dependency cycle detected"));
    }

    #[tokio::test]
    async fn missing_task_returns_error() {
        let docker = DockerClient::new().expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(empty_config(), docker);

        let err = engine.run_task("does-not-exist", &[]).await.unwrap_err();
        assert!(err.to_string().contains("Task 'does-not-exist' not found"));
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
        task_config.run.environment = Some(HashMap::from([(
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
        task_config.run.environment = Some(HashMap::from([(
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
        task_config.run.working_directory = Some("/from-run".to_string());
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
    async fn task_run_entrypoint_overrides_container_entrypoint() {
        let mut container_config = container("alpine:3.18", None);
        container_config.entrypoint = Some("/from-container".to_string());
        let mut containers = HashMap::new();
        containers.insert("build-env".to_string(), container_config);

        let mut task_config = task("build-env", "echo hi");
        task_config.run.entrypoint = Some("/from-run".to_string());
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
                run: TaskRun {
                    container: "app".to_string(),
                    command: Some("echo hi".to_string()),
                    environment: None,
                    ports: None,
                    working_directory: None,
                    entrypoint: None,
                },
                prerequisites: Some(vec!["setup".to_string()]),
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
}
