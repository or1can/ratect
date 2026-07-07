use anyhow::{Context, Result};
use bollard::models::ContainerCreateBody as Config;
use bollard::query_parameters::CreateImageOptions;
use bollard::query_parameters::LogsOptions;
use bollard::service::HostConfig;
use bollard::Docker;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct DockerClient {
    docker: Docker,
}

impl DockerClient {
    pub fn new() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().context("Failed to connect to Docker")?;
        Ok(Self { docker })
    }

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message(format!("Pulling image {}...", image));
        pb.enable_steady_tick(Duration::from_millis(100));

        let options = CreateImageOptions {
            from_image: Some(image.to_string()),
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);

        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => {
                    if let Some(status) = output.status {
                        pb.set_message(format!("{}: {}", image, status));
                    }
                }
                Err(e) => {
                    pb.finish_with_message(format!("Failed to pull image {}", image));
                    return Err(e).context(format!("Failed to pull image {}", image));
                }
            }
        }

        pb.finish_with_message(format!("Image {} pulled successfully", image));
        Ok(())
    }

    pub async fn run_container(
        &self,
        image: &str,
        command: Option<&str>,
        volumes: Option<&Vec<String>>,
    ) -> Result<()> {
        let host_config = HostConfig {
            binds: volumes.cloned(),
            ..Default::default()
        };

        let config = Config {
            image: Some(image.to_string()),
            cmd: command.map(|c| vec!["sh".to_string(), "-c".to_string(), c.to_string()]),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = self.docker.create_container(None, config).await?;

        self.docker.start_container(&container.id, None).await?;

        let mut logs = self.docker.logs(
            &container.id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                follow: true,
                ..Default::default()
            }),
        );

        while let Some(log) = logs.next().await {
            match log {
                Ok(output) => print!("{}", output),
                Err(e) => return Err(e).context("Failed to get container logs"),
            }
        }

        self.docker.remove_container(&container.id, None).await?;

        Ok(())
    }
}
