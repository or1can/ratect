use crate::config::Config;
use crate::docker::ContainerRuntime;
use anyhow::{Context, Result};
use async_recursion::async_recursion;
use std::collections::HashSet;
use std::sync::Mutex;

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
                    image,
                    task.run.command.as_deref(),
                    container_config.volumes.as_ref(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Container, Task, TaskRun};
    use crate::docker::DockerClient;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Records every `pull_image`/`run_container` call instead of talking to Docker,
    /// so tests can assert on dedup and ordering behavior quickly and deterministically.
    #[derive(Default, Clone)]
    struct FakeContainerRuntime {
        pulled: Arc<Mutex<Vec<String>>>,
        ran: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ContainerRuntime for FakeContainerRuntime {
        async fn pull_image(&self, image: &str) -> Result<()> {
            self.pulled.lock().unwrap().push(image.to_string());
            Ok(())
        }

        async fn run_container(
            &self,
            _image: &str,
            command: Option<&str>,
            _volumes: Option<&Vec<String>>,
        ) -> Result<()> {
            self.ran
                .lock()
                .unwrap()
                .push(command.unwrap_or_default().to_string());
            Ok(())
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

        // The image backing every task is the same, so it should only be pulled once
        // even though four tasks reference it.
        assert_eq!(
            *docker.pulled.lock().unwrap(),
            vec!["alpine:3.18".to_string()]
        );

        // "shared-prereq" is a prerequisite of both "prereq-task" and
        // "list-volume-task", but must only run once, before either of them,
        // and "test-task" must run last.
        let ran = docker.ran.lock().unwrap().clone();
        assert_eq!(ran.len(), 4);
        assert_eq!(ran[0], "shared-prereq");
        assert_eq!(ran[3], "test-task");
        assert!(ran[1..3].contains(&"prereq-task".to_string()));
        assert!(ran[1..3].contains(&"list-volume-task".to_string()));
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

        assert!(docker.pulled.lock().unwrap().is_empty());
        assert!(docker.ran.lock().unwrap().is_empty());
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
