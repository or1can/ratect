use crate::config::{Config};
use crate::docker::DockerClient;
use anyhow::{Result, Context};
use async_recursion::async_recursion;
use std::collections::HashSet;
use std::sync::Mutex;

pub struct TaskEngine {
    config: Config,
    docker: DockerClient,
    executed_tasks: Mutex<HashSet<String>>,
    pulled_images: Mutex<HashSet<String>>,
    in_progress_tasks: Mutex<HashSet<String>>,
}

impl TaskEngine {
    pub fn new(config: Config, docker: DockerClient) -> Self {
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
                return Err(anyhow::anyhow!("Dependency cycle detected involving task '{}'", task_name));
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
        let task = self.config.tasks.get(task_name)
            .with_context(|| format!("Task '{}' not found", task_name))?;

        // Run prerequisites
        if let Some(prerequisites) = &task.prerequisites {
            for prerequisite in prerequisites {
                self.run_task(prerequisite).await?;
            }
        }

        // Run the task itself
        let container_config = self.config.containers.get(&task.run.container)
            .with_context(|| format!("Container '{}' not found", task.run.container))?;

        println!("Running task '{}'...", task_name);

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
            self.docker.run_container(image, task.run.command.as_deref(), container_config.volumes.as_ref()).await?;
        } else if let Some(build_dir) = &container_config.build_directory {
            println!("Building from directory {} not implemented yet", build_dir);
        }

        Ok(())
    }
}
