use crate::config::Config;
use crate::docker::ContainerRuntime;
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use uuid::Uuid;

pub struct TaskEngine<D: ContainerRuntime + Send + Sync> {
    config: Config,
    docker: D,
    executed_tasks: Mutex<HashSet<String>>,
    pulled_images: Mutex<HashSet<String>>,
    in_progress_tasks: Mutex<HashSet<String>>,
}

impl<D: ContainerRuntime + Send + Sync> TaskEngine<D> {
    pub fn new(config: Config, docker: D) -> Self {
        Self {
            config,
            docker,
            executed_tasks: Mutex::new(HashSet::new()),
            pulled_images: Mutex::new(HashSet::new()),
            in_progress_tasks: Mutex::new(HashSet::new()),
        }
    }

    #[async_recursion]
    pub async fn run_task(&self, task_name: &str) -> Result<()> {
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

        let result = self.run_task_internal(task_name).await;

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

    async fn run_task_internal(&self, task_name: &str) -> Result<()> {
        let task = self
            .config
            .tasks
            .get(task_name)
            .with_context(|| format!("Task '{}' not found", task_name))?;

        // Run prerequisites
        if let Some(prerequisites) = &task.prerequisites {
            for prerequisite in prerequisites {
                self.run_task(prerequisite).await?;
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
        let has_dependencies = container_config
            .dependencies
            .as_ref()
            .is_some_and(|deps| !deps.is_empty());

        let network_name = if has_dependencies {
            let name = format!("ratect-{}", Uuid::new_v4());
            self.docker.create_network(&name).await?;
            Some(name)
        } else {
            None
        };

        let mut running_sidecars: HashMap<String, String> = HashMap::new();
        let mut resolving: HashSet<String> = HashSet::new();

        let result: Result<()> = async {
            if let (Some(network), Some(deps)) = (&network_name, &container_config.dependencies) {
                for dep_name in deps {
                    self.start_dependency(dep_name, network, &mut running_sidecars, &mut resolving)
                        .await?;
                }
            }

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
                self.docker
                    .run_container(
                        &task.run.container,
                        image,
                        task.run.command.as_deref(),
                        container_config.volumes.as_ref(),
                        network_name.as_deref(),
                    )
                    .await?;
            } else if let Some(build_dir) = &container_config.build_directory {
                tracing::warn!(
                    "Building from directory '{}' is not implemented yet",
                    build_dir
                );
            }

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
        if let Some(network) = &network_name {
            if let Err(e) = self.docker.remove_network(network).await {
                tracing::warn!(network = network.as_str(), error = ?e, "Failed to remove network");
            }
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

        let image = dependency_config.image.as_ref().with_context(|| {
            format!(
                "Container '{}' has no image and cannot be started as a dependency \
                 (build_directory is not yet supported for dependency containers)",
                name
            )
        })?;

        let needs_pull = {
            let pulled = self.pulled_images.lock().unwrap();
            !pulled.contains(image)
        };
        if needs_pull {
            self.docker.pull_image(image).await?;
            let mut pulled = self.pulled_images.lock().unwrap();
            pulled.insert(image.to_string());
        }

        let container_id = self
            .docker
            .start_background_container(name, image, dependency_config.volumes.as_ref(), network)
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
    #[derive(Default, Clone)]
    struct FakeContainerRuntime {
        events: Arc<Mutex<Vec<String>>>,
        fail_run: Arc<Mutex<bool>>,
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
    }

    #[async_trait::async_trait]
    impl ContainerRuntime for FakeContainerRuntime {
        async fn pull_image(&self, image: &str) -> Result<()> {
            self.push(format!("pull:{image}"));
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

        async fn start_background_container(
            &self,
            alias: &str,
            _image: &str,
            _volumes: Option<&Vec<String>>,
            network: &str,
        ) -> Result<String> {
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
            _image: &str,
            command: Option<&str>,
            _volumes: Option<&Vec<String>>,
            network: Option<&str>,
        ) -> Result<()> {
            self.push(format!(
                "run:{name}:{}:{}",
                command.unwrap_or_default(),
                network.unwrap_or("none")
            ));
            if *self.fail_run.lock().unwrap() {
                return Err(crate::docker::ContainerExitedNonZero { exit_code: 1 }.into());
            }
            Ok(())
        }
    }

    fn container(image: &str, dependencies: Option<Vec<String>>) -> Container {
        Container {
            image: Some(image.to_string()),
            build_directory: None,
            volumes: None,
            dependencies,
        }
    }

    fn task(container: &str, command: &str) -> Task {
        Task {
            run: TaskRun {
                container: container.to_string(),
                command: Some(command.to_string()),
            },
            prerequisites: None,
        }
    }

    fn config_with_cycle() -> Config {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                image: Some("alpine:3.18".to_string()),
                build_directory: None,
                volumes: None,
                dependencies: None,
            },
        );

        let mut tasks = HashMap::new();
        tasks.insert(
            "a".to_string(),
            Task {
                run: TaskRun {
                    container: "build-env".to_string(),
                    command: None,
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
                },
                prerequisites: Some(vec!["a".to_string()]),
            },
        );

        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
        }
    }

    fn empty_config() -> Config {
        Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
        }
    }

    /// Mirrors the diamond-shaped dependency graph in the sample `batect.yml`:
    /// two tasks share a common prerequisite, and a final task depends on both.
    fn config_with_shared_prerequisite() -> Config {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                image: Some("alpine:3.18".to_string()),
                build_directory: None,
                volumes: None,
                dependencies: None,
            },
        );

        let task = |command: &str, prerequisites: Option<Vec<String>>| Task {
            run: TaskRun {
                container: "build-env".to_string(),
                command: Some(command.to_string()),
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
        }
    }

    #[tokio::test]
    async fn shared_prerequisite_runs_once_and_image_pulled_once() {
        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config_with_shared_prerequisite(), docker.clone());

        engine.run_task("test-task").await.unwrap();

        let events = docker.events();

        // The image backing every task is the same, so it should only be pulled once
        // even though four tasks reference it.
        let pulls: Vec<_> = events.iter().filter(|e| e.starts_with("pull:")).collect();
        assert_eq!(pulls, vec!["pull:alpine:3.18"]);

        // "shared-prereq" is a prerequisite of both "prereq-task" and
        // "list-volume-task", but must only run once, before either of them,
        // and "test-task" must run last. None of these tasks declare
        // dependencies, so no network is involved ("none").
        let runs: Vec<_> = events
            .iter()
            .filter(|e| e.starts_with("run:"))
            .cloned()
            .collect();
        assert_eq!(runs.len(), 4);
        assert_eq!(runs[0], "run:build-env:shared-prereq:none");
        assert_eq!(runs[3], "run:build-env:test-task:none");
        assert!(runs[1..3].contains(&"run:build-env:prereq-task:none".to_string()));
        assert!(runs[1..3].contains(&"run:build-env:list-volume-task:none".to_string()));
    }

    #[tokio::test]
    async fn build_directory_container_warns_and_skips_run() {
        let mut containers = HashMap::new();
        containers.insert(
            "build-env".to_string(),
            Container {
                image: None,
                build_directory: Some("./docker".to_string()),
                volumes: None,
                dependencies: None,
            },
        );
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            Task {
                run: TaskRun {
                    container: "build-env".to_string(),
                    command: None,
                },
                prerequisites: None,
            },
        );
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("build").await.unwrap();

        assert!(docker.events().is_empty());
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
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start").await.unwrap();

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
            .position(|e| *e == format!("run:app:echo hi:{network}"))
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
        };

        let docker = FakeContainerRuntime::default().failing_run();
        let engine = TaskEngine::new(config, docker.clone());

        let err = engine.run_task("start").await.unwrap_err();
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
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start").await.unwrap();

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
            .position(|e| *e == format!("run:app:echo hi:{network}"))
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
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start").await.unwrap();

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
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("start").await.unwrap();

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
            .position(|e| *e == format!("run:app:echo hi:{network}"))
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
                },
                prerequisites: Some(vec!["migrate".to_string()]),
            },
        );
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker.clone());

        engine.run_task("test").await.unwrap();

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
    async fn dependency_without_image_errors() {
        let mut containers = HashMap::new();
        containers.insert(
            "database".to_string(),
            Container {
                image: None,
                build_directory: None,
                volumes: None,
                dependencies: None,
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
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker);

        let err = engine.run_task("start").await.unwrap_err();
        assert!(err
            .to_string()
            .contains("Container 'database' has no image"));
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
        };

        let docker = FakeContainerRuntime::default();
        let engine = TaskEngine::new(config, docker);

        let err = engine.run_task("start").await.unwrap_err();
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

        let err = engine.run_task("a").await.unwrap_err();
        assert!(err.to_string().contains("Dependency cycle detected"));
    }

    #[tokio::test]
    async fn missing_task_returns_error() {
        let docker = DockerClient::new().expect("constructing a Docker client is infallible here");
        let engine = TaskEngine::new(empty_config(), docker);

        let err = engine.run_task("does-not-exist").await.unwrap_err();
        assert!(err.to_string().contains("Task 'does-not-exist' not found"));
    }
}
