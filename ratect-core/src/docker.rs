use anyhow::{Context, Result};
use bollard::models::{
    ContainerCreateBody as Config, EndpointSettings, NetworkConnectRequest, NetworkCreateRequest,
};
use bollard::query_parameters::CreateImageOptions;
use bollard::query_parameters::LogsOptions;
use bollard::query_parameters::WaitContainerOptions;
use bollard::service::HostConfig;
use bollard::Docker;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::fmt;
use std::time::Duration;

/// The task's own container ran to completion, but its command exited with a
/// non-zero status. Distinct from other errors (Docker API failures, missing
/// images, etc.) so callers can distinguish "the task failed" from "ratect
/// itself failed to run the task", and so `main` can propagate the exact exit
/// code as ratect's own.
#[derive(Debug)]
pub struct ContainerExitedNonZero {
    pub exit_code: i64,
}

impl fmt::Display for ContainerExitedNonZero {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "container command exited with code {}", self.exit_code)
    }
}

impl std::error::Error for ContainerExitedNonZero {}

/// Abstracts the container operations the task engine needs, so tests can
/// inject a fake implementation instead of talking to a real Docker daemon.
#[async_trait::async_trait]
pub trait ContainerRuntime {
    async fn pull_image(&self, image: &str) -> Result<()>;

    async fn create_network(&self, name: &str) -> Result<()>;

    async fn remove_network(&self, name: &str) -> Result<()>;

    /// Starts a container in the background (does not wait for it to exit),
    /// joined to `network` with a network alias of `alias` so other
    /// containers on the same network can reach it by that name. Returns the
    /// container id, used later to stop/remove it. Used for sidecar/dependency
    /// containers.
    async fn start_background_container(
        &self,
        alias: &str,
        image: &str,
        volumes: Option<&Vec<String>>,
        network: &str,
    ) -> Result<String>;

    /// Stops and removes a container started with [`start_background_container`](Self::start_background_container).
    async fn stop_and_remove_container(&self, container_id: &str) -> Result<()>;

    /// Runs a container to completion, streaming its logs to stdout, then
    /// removes it. `name` is this container's own network alias (used when
    /// `network` is set); used for a task's own container.
    async fn run_container(
        &self,
        name: &str,
        image: &str,
        command: Option<&str>,
        volumes: Option<&Vec<String>>,
        network: Option<&str>,
    ) -> Result<()>;
}

pub struct DockerClient {
    docker: Docker,
}

impl DockerClient {
    pub fn new() -> Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().context("Failed to connect to Docker")?;
        Ok(Self { docker })
    }

    async fn join_network(&self, container_id: &str, network: &str, alias: &str) -> Result<()> {
        self.docker
            .connect_network(
                network,
                NetworkConnectRequest {
                    container: container_id.to_string(),
                    endpoint_config: Some(EndpointSettings {
                        aliases: Some(vec![alias.to_string()]),
                        ..Default::default()
                    }),
                },
            )
            .await
            .with_context(|| format!("Failed to connect '{}' to network '{}'", alias, network))?;
        tracing::debug!(container_id, network, alias, "joined network");
        Ok(())
    }

    /// Must only be called once the container has already stopped (e.g. after
    /// its log stream, followed with `follow: true`, has ended) — at that
    /// point Docker still has the exit status available, so this resolves
    /// immediately rather than actually waiting.
    async fn exit_code(&self, container_id: &str) -> Result<i64> {
        let mut wait_stream = self
            .docker
            .wait_container(container_id, None::<WaitContainerOptions>);

        match wait_stream.next().await {
            Some(Ok(response)) => Ok(response.status_code),
            Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => Ok(code),
            Some(Err(e)) => {
                Err(e).with_context(|| format!("Failed to wait for container '{}'", container_id))
            }
            None => Err(anyhow::anyhow!(
                "Docker did not report an exit status for container '{}'",
                container_id
            )),
        }
    }
}

#[async_trait::async_trait]
impl ContainerRuntime for DockerClient {
    async fn pull_image(&self, image: &str) -> Result<()> {
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

    async fn create_network(&self, name: &str) -> Result<()> {
        self.docker
            .create_network(NetworkCreateRequest {
                name: name.to_string(),
                ..Default::default()
            })
            .await
            .with_context(|| format!("Failed to create network '{}'", name))?;
        tracing::debug!(network = name, "created network");
        Ok(())
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.docker
            .remove_network(name)
            .await
            .with_context(|| format!("Failed to remove network '{}'", name))?;
        tracing::debug!(network = name, "removed network");
        Ok(())
    }

    async fn start_background_container(
        &self,
        alias: &str,
        image: &str,
        volumes: Option<&Vec<String>>,
        network: &str,
    ) -> Result<String> {
        let host_config = HostConfig {
            binds: volumes.cloned(),
            ..Default::default()
        };

        let config = Config {
            image: Some(image.to_string()),
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(None, config)
            .await
            .with_context(|| format!("Failed to create sidecar container '{}'", alias))?;
        tracing::debug!(container_id = %container.id, alias, image, "created sidecar container");

        self.join_network(&container.id, network, alias).await?;

        self.docker
            .start_container(&container.id, None)
            .await
            .with_context(|| format!("Failed to start sidecar container '{}'", alias))?;
        tracing::debug!(container_id = %container.id, alias, "started sidecar container");

        Ok(container.id)
    }

    async fn stop_and_remove_container(&self, container_id: &str) -> Result<()> {
        self.docker
            .stop_container(container_id, None)
            .await
            .with_context(|| format!("Failed to stop container '{}'", container_id))?;
        self.docker
            .remove_container(container_id, None)
            .await
            .with_context(|| format!("Failed to remove container '{}'", container_id))?;
        tracing::debug!(container_id, "stopped and removed container");
        Ok(())
    }

    async fn run_container(
        &self,
        name: &str,
        image: &str,
        command: Option<&str>,
        volumes: Option<&Vec<String>>,
        network: Option<&str>,
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
        tracing::debug!(container_id = %container.id, image, "created container");

        if let Some(network) = network {
            self.join_network(&container.id, network, name).await?;
        }

        self.docker.start_container(&container.id, None).await?;
        tracing::debug!(container_id = %container.id, "started container");

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

        let exit_code = self.exit_code(&container.id).await?;

        self.docker.remove_container(&container.id, None).await?;
        tracing::debug!(container_id = %container.id, exit_code, "removed container");

        if exit_code != 0 {
            return Err(ContainerExitedNonZero { exit_code }.into());
        }

        Ok(())
    }
}
