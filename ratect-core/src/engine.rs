use crate::config::{Config, Container};
use crate::docker::ContainerRuntime;
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// Merges a container's `environment` with a task's `run.environment`
/// (which wins on key collision), matching Batect: the container's is the
/// baseline, the run's is a per-task override on top of it. `None` only
/// when neither is set.
fn merged_environment(
    container_env: Option<&HashMap<String, String>>,
    run_env: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    if container_env.is_none() && run_env.is_none() {
        return None;
    }
    let mut merged = container_env.cloned().unwrap_or_default();
    if let Some(run_env) = run_env {
        merged.extend(run_env.clone());
    }
    Some(merged)
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
        }
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
            let image_id = self
                .docker
                .build_image(
                    Path::new(build_directory),
                    container_config.build_args.as_ref(),
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
        let network_name = format!("ratect-{}", Uuid::new_v4());
        self.docker.create_network(&network_name).await?;

        let mut running_sidecars: HashMap<String, String> = HashMap::new();
        let mut resolving: HashSet<String> = HashSet::new();

        let result: Result<()> = async {
            if let Some(deps) = &container_config.dependencies {
                for dep_name in deps {
                    self.start_dependency(
                        dep_name,
                        &network_name,
                        &mut running_sidecars,
                        &mut resolving,
                    )
                    .await?;
                }
            }

            let image = self
                .resolve_image(&task.run.container, container_config)
                .await?;
            let environment = merged_environment(
                container_config.environment.as_ref(),
                task.run.environment.as_ref(),
            );
            let user_mapping = self.resolve_user_mapping(container_config).await?;
            // Eligibility only — `ContainerRuntime::run_container` further
            // gates this on the local process's own stdin/stdout genuinely
            // being terminals before actually attaching a TTY.
            let interactive = top_level;
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
        if let Err(e) = self.docker.remove_network(&network_name).await {
            tracing::warn!(network = network_name.as_str(), error = ?e, "Failed to remove network");
        }

        result
    }

    /// Recursively starts `name` (and, first, any containers it depends on)
    /// as a background container on `network`, scoped to a single task's
    /// execution. `running` dedupes within that scope; `resolving` detects
    /// circular container dependencies.
    #[async_recursion]
    async fn start_dependency(
        &self,
        name: &str,
        network: &str,
        running: &mut HashMap<String, String>,
        resolving: &mut HashSet<String>,
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
                self.start_dependency(nested_name, network, running, resolving)
                    .await?;
            }
        }

        let image = self.resolve_image(name, dependency_config).await?;
        let user_mapping = self.resolve_user_mapping(dependency_config).await?;

        let container_id = self
            .docker
            .start_background_container(
                name,
                &image,
                dependency_config.volumes.as_ref(),
                dependency_config.environment.as_ref(),
                network,
                user_mapping.as_ref(),
            )
            .await?;

        resolving.remove(name);
        running.insert(name.to_string(), container_id);

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
    type CapturedImages = Arc<Mutex<HashMap<String, String>>>;
    type CapturedInteractive = Arc<Mutex<HashMap<String, bool>>>;
    /// `(uid, gid, home_directory)`, keyed by container name.
    type CapturedUserMapping = Arc<Mutex<HashMap<String, Option<(u32, u32, String)>>>>;

    #[derive(Default, Clone)]
    struct FakeContainerRuntime {
        events: Arc<Mutex<Vec<String>>>,
        fail_run: Arc<Mutex<bool>>,
        // Captured separately from `events` (rather than folded into its
        // strings) so the many existing exact-string event assertions don't
        // have to change shape just because environment support was added.
        environments: CapturedEnvironments,
        // Keyed by the tag `build_image` was called with.
        build_args: CapturedBuildArgs,
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
            build_args: Option<&HashMap<String, String>>,
            tag: &str,
        ) -> Result<String> {
            self.build_args
                .lock()
                .unwrap()
                .insert(tag.to_string(), build_args.cloned());
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

        async fn start_background_container(
            &self,
            alias: &str,
            image: &str,
            _volumes: Option<&Vec<String>>,
            environment: Option<&HashMap<String, String>>,
            network: &str,
            user_mapping: Option<&crate::docker::UserMapping>,
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
            self.push(format!("sidecar-start:{alias}:{network}"));
            Ok(format!("sidecar-id-{alias}"))
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
            volumes: None,
            dependencies,
            environment: None,
            run_as_current_user: None,
        }
    }

    fn task(container: &str, command: &str) -> Task {
        Task {
            run: TaskRun {
                container: container.to_string(),
                command: Some(command.to_string()),
                environment: None,
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
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
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
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
            },
        );

        let task = |command: &str, prerequisites: Option<Vec<String>>| Task {
            run: TaskRun {
                container: "build-env".to_string(),
                command: Some(command.to_string()),
                environment: None,
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
            volumes: None,
            dependencies,
            environment: None,
            run_as_current_user: Some(crate::config::RunAsCurrentUser {
                enabled: true,
                home_directory: Some(home_directory.to_string()),
            }),
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

    fn container_with_build_directory(
        build_directory: &str,
        build_args: Option<HashMap<String, String>>,
    ) -> Container {
        Container {
            image: None,
            build_directory: Some(build_directory.to_string()),
            build_args,
            volumes: None,
            dependencies: None,
            environment: None,
            run_as_current_user: None,
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
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
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
                volumes: None,
                dependencies: None,
                environment: None,
                run_as_current_user: None,
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
}
