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
type CapturedBuildKitOptions = Arc<Mutex<HashMap<String, Option<crate::docker::BuildKitOptions>>>>;
/// The `force_pull` a prior `build_image` call for a given tag was
/// given (see `force_pull_for`).
type CapturedForcePull = Arc<Mutex<HashMap<String, bool>>>;
type CapturedHostGateways = Arc<Mutex<HashMap<String, Option<crate::proxy::HostGateway>>>>;
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
type CapturedUserMapping = Arc<Mutex<HashMap<String, Option<(u32, u32, String, Vec<String>)>>>>;
/// `(additional_hostnames, additional_hosts, ports)`.
type NetworkOptionsValue = (
    Option<Vec<String>>,
    Option<HashMap<String, String>>,
    Option<Vec<(u16, u16, String)>>,
);
/// Keyed by container name.
type CapturedNetworkOptions = Arc<Mutex<HashMap<String, NetworkOptionsValue>>>;
/// Keyed by container name.
type CapturedHealthChecks = Arc<Mutex<HashMap<String, Option<crate::docker::HealthCheckOptions>>>>;
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
    log_driver: Option<String>,
    log_options: Option<HashMap<String, String>>,
    tmpfs: Option<Vec<(String, String)>>,
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
    // The `force_pull` a prior `build_image` call for a given tag was
    // given (see `force_pull_for`).
    force_pull: CapturedForcePull,
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
    // The labels a prior `create_network` call was given (see
    // `network_labels`).
    network_labels: Arc<Mutex<Option<HashMap<String, String>>>>,
    // The `network_options` a prior `run_container`/`start_background_container`
    // call for a given container name was given (see `network_options_for`).
    network_options: CapturedNetworkOptions,
    // The `proxy_host_gateway` a prior `run_container`/
    // `start_background_container` call for a given container name was
    // given (see `host_gateway_for`) — kept out of `network_options` above
    // so adding it doesn't rewrite every existing tuple assertion.
    host_gateways: CapturedHostGateways,
    // The `proxy_host_gateway` a prior `build_image` call for a given tag
    // was given (see `build_host_gateway_for`).
    build_host_gateways: CapturedHostGateways,
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
    // Makes `run_container` fail before it would have created anything
    // (see `failing_container_creation`).
    fail_container_creation: Arc<Mutex<bool>>,
    // Makes `build_image` fail, standing in for any Docker-layer build
    // failure (see `failing_image_build`).
    fail_image_build: Arc<Mutex<bool>>,
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
    // Same idea again, for `run_container` itself (keyed by container
    // name) — simulates the task's own container's main command still
    // running for a while after it starts, so a test can prove its
    // readiness gate (health-check wait, then `setup_commands`) actually
    // overlaps with the still-in-flight run rather than happening
    // strictly before or after it (see `with_run_delay`).
    run_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    /// When set, every `stop_and_remove_container` records an interrupt
    /// *before* doing its own work — the only way to land one in the
    /// middle of cleanup deterministically, since cleanup against this
    /// fake is otherwise instant. See
    /// `an_interrupt_during_cleanup_abandons_it_even_when_the_run_was_not_interrupted`.
    interrupt_on_stop: Arc<Mutex<Option<Arc<crate::interrupt::Interrupt>>>>,
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
            force_pull: Default::default(),
            images: Default::default(),
            commands: Default::default(),
            interactive: Default::default(),
            user_mapping: Default::default(),
            network_exists_result: Arc::new(Mutex::new(true)),
            network_labels: Default::default(),
            network_options: Default::default(),
            host_gateways: Default::default(),
            build_host_gateways: Default::default(),
            health_checks: Default::default(),
            container_options: Default::default(),
            execs: Default::default(),
            unhealthy_container: Default::default(),
            failing_setup_command: Default::default(),
            fail_container_creation: Default::default(),
            fail_image_build: Default::default(),
            locally_present_images: Default::default(),
            start_delays: Default::default(),
            pull_delays: Default::default(),
            exec_delays: Default::default(),
            health_check_delays: Default::default(),
            run_delays: Default::default(),
            interrupt_on_stop: Default::default(),
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

    /// Makes `run_container` fail *before* creating a container, so
    /// neither the `created` nor the `started` channel is ever sent —
    /// the one shape every other failure mode here can't produce, since
    /// they all report after handing the id over. Exercises the
    /// engine's concurrent readiness task giving up on a dropped
    /// sender; if it ever waited on one instead, the run would hang
    /// rather than fail.
    fn failing_container_creation(self) -> Self {
        *self.fail_container_creation.lock().unwrap() = true;
        self
    }

    /// Makes `build_image` fail with an error of the Docker layer's own
    /// wording — one that says nothing about which container asked for
    /// the build.
    fn failing_image_build(self) -> Self {
        *self.fail_image_build.lock().unwrap() = true;
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

    /// Makes `run_container` for the task's own container `name`
    /// artificially `tokio::time::sleep` for `delay` before reporting
    /// whether the (simulated) main command succeeded — see
    /// `run_delays`' own doc comment for why.
    fn with_run_delay(self, name: &str, delay: std::time::Duration) -> Self {
        self.run_delays
            .lock()
            .unwrap()
            .insert(name.to_string(), delay);
        self
    }

    /// See the `interrupt_on_stop` field.
    fn interrupting_on_stop(self, interrupt: &Arc<crate::interrupt::Interrupt>) -> Self {
        *self.interrupt_on_stop.lock().unwrap() = Some(Arc::clone(interrupt));
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

    /// The `force_pull` a prior `build_image` call for `tag` was given.
    fn force_pull_for(&self, tag: &str) -> Option<bool> {
        self.force_pull.lock().unwrap().get(tag).copied()
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

    /// The `(uid, gid, home_directory, cache_directories)` a prior
    /// `run_container`/`start_background_container` call for `name` was
    /// given (flattened, same convention as `environment_for`).
    fn user_mapping_for(&self, name: &str) -> Option<(u32, u32, String, Vec<String>)> {
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

    /// The `proxy_host_gateway` a prior `run_container`/
    /// `start_background_container` call for `name` was given (flattened,
    /// same convention as `environment_for`).
    fn host_gateway_for(&self, name: &str) -> Option<crate::proxy::HostGateway> {
        self.host_gateways
            .lock()
            .unwrap()
            .get(name)
            .copied()
            .flatten()
    }

    /// The `proxy_host_gateway` a prior `build_image` call for `tag` was
    /// given (flattened, same convention as `environment_for`).
    fn build_host_gateway_for(&self, tag: &str) -> Option<crate::proxy::HostGateway> {
        self.build_host_gateways
            .lock()
            .unwrap()
            .get(tag)
            .copied()
            .flatten()
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

    /// The labels a prior `create_network` call was given.
    fn network_labels(&self) -> Option<HashMap<String, String>> {
        self.network_labels.lock().unwrap().clone()
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

    /// The `container_options.log_driver` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn log_driver_for(&self, name: &str) -> Option<String> {
        self.container_options
            .lock()
            .unwrap()
            .get(name)
            .and_then(|options| options.log_driver.clone())
    }

    /// The `container_options.log_options` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn log_options_for(&self, name: &str) -> Option<HashMap<String, String>> {
        self.container_options
            .lock()
            .unwrap()
            .get(name)
            .and_then(|options| options.log_options.clone())
    }

    /// The `container_options.tmpfs` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn tmpfs_for(&self, name: &str) -> Option<Vec<(String, String)>> {
        self.container_options
            .lock()
            .unwrap()
            .get(name)
            .and_then(|options| options.tmpfs.clone())
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
        force_pull: bool,
        proxy_host_gateway: Option<crate::proxy::HostGateway>,
    ) -> Result<String> {
        self.build_host_gateways
            .lock()
            .unwrap()
            .insert(tag.to_string(), proxy_host_gateway);
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
        self.force_pull
            .lock()
            .unwrap()
            .insert(tag.to_string(), force_pull);
        self.push(format!("build:{tag}:{}", build_directory.display()));
        if *self.fail_image_build.lock().unwrap() {
            anyhow::bail!("the daemon said no");
        }
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

    async fn create_network(&self, name: &str, labels: &HashMap<String, String>) -> Result<()> {
        self.push(format!("network-create:{name}"));
        *self.network_labels.lock().unwrap() = Some(labels.clone());
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
            user_mapping.map(|m| {
                (
                    m.user.uid,
                    m.user.gid,
                    m.home_directory.clone(),
                    m.cache_directories.clone(),
                )
            }),
        );
        self.network_options.lock().unwrap().insert(
            alias.to_string(),
            (
                network_options.additional_hostnames.cloned(),
                network_options.additional_hosts.cloned(),
                network_options.ports.cloned(),
            ),
        );
        self.host_gateways
            .lock()
            .unwrap()
            .insert(alias.to_string(), network_options.proxy_host_gateway);
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
                log_driver: container_options.log_driver.map(str::to_string),
                log_options: container_options.log_options.cloned(),
                tmpfs: container_options.tmpfs.cloned(),
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
        let interrupt = self.interrupt_on_stop.lock().unwrap().clone();
        if let Some(interrupt) = interrupt {
            interrupt.record();
            // Yields so this removal is genuinely *in flight* when the
            // engine's race next polls — a real `stop_and_remove` waits
            // on the daemon (up to Docker's whole kill timeout for a
            // container ignoring `SIGTERM`), which is the case the race
            // exists for. Returning `Ready` immediately would instead
            // test the one situation that can't happen.
            tokio::task::yield_now().await;
        }
        self.push(format!("sidecar-stop:{container_id}"));
        Ok(())
    }

    // Neither is ever reached through `TaskEngine` (only `--clean`/
    // `--clean-cache` in `main.rs` call these, directly against a real
    // `DockerClient`) — trivial stubs only to satisfy the trait.
    // `crate::cache`'s own tests cover the actual cleanup logic against
    // plain `Vec<String>` fixtures instead, not this fake.
    async fn list_volumes(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    // The engine never lists containers or networks — that's the
    // `resources` verb's business, driven from a binary — so these stay
    // empty here rather than growing capture state no engine test uses.
    async fn list_containers(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<crate::docker::LabelledResource>> {
        Ok(Vec::new())
    }

    async fn list_networks(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<crate::docker::LabelledResource>> {
        Ok(Vec::new())
    }

    async fn remove_volume(&self, _name: &str) -> Result<()> {
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
        created: Option<tokio::sync::oneshot::Sender<String>>,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        // Before anything is recorded, and before either channel is
        // sent — both senders drop here (see
        // `failing_container_creation`).
        if *self.fail_container_creation.lock().unwrap() {
            self.push(format!("run-create-failed:{name}"));
            anyhow::bail!("Failed to create container '{name}'");
        }
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
            user_mapping.map(|m| {
                (
                    m.user.uid,
                    m.user.gid,
                    m.home_directory.clone(),
                    m.cache_directories.clone(),
                )
            }),
        );
        self.network_options.lock().unwrap().insert(
            name.to_string(),
            (
                network_options.additional_hostnames.cloned(),
                network_options.additional_hosts.cloned(),
                network_options.ports.cloned(),
            ),
        );
        self.host_gateways
            .lock()
            .unwrap()
            .insert(name.to_string(), network_options.proxy_host_gateway);
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
                log_driver: container_options.log_driver.map(str::to_string),
                log_options: container_options.log_options.cloned(),
                tmpfs: container_options.tmpfs.cloned(),
            },
        );
        self.push(format!(
            "run:{name}:{}:args=[{}]:{}",
            command.unwrap_or_default(),
            additional_args.join(","),
            network
        ));
        // Same id convention `start_background_container` uses, so
        // `with_unhealthy_container`/exec-based assertions work
        // identically whether `name` is a dependency or the task's own
        // container — and so the engine's cleanup of it shows up as the
        // same `sidecar-stop:sidecar-id-{name}` event, which is what the
        // cleanup-flag tests assert on. Both fired before `run_delays`'
        // own sleep (if any), so a concurrent readiness task genuinely
        // overlaps with this call still being in flight, the same way it
        // would against a real, still-running container.
        if let Some(created) = created {
            let _ = created.send(format!("sidecar-id-{name}"));
        }
        if let Some(started) = started {
            let _ = started.send(());
        }
        let delay = self.run_delays.lock().unwrap().get(name).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if *self.fail_run.lock().unwrap() {
            return Err(crate::docker::ContainerExitedNonZero { exit_code: 1 }.into());
        }
        Ok(())
    }
}

fn container(image: &str, dependencies: Option<Vec<String>>) -> Container {
    Container {
        extends: None,
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
        log_driver: None,
        log_options: None,
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
            extends: None,
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
            log_driver: None,
            log_options: None,
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
        forbid_telemetry: None,
    }
}

fn empty_config() -> Config {
    Config {
        project_name: "demo".to_string(),
        containers: HashMap::new(),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// Mirrors the diamond-shaped dependency graph in the sample `batect.yml`:
/// two tasks share a common prerequisite, and a final task depends on both.
fn config_with_shared_prerequisite() -> Config {
    let mut containers = HashMap::new();
    containers.insert(
        "build-env".to_string(),
        Container {
            extends: None,
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
            log_driver: None,
            log_options: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
    let engine =
        TaskEngine::new(config_with_shared_prerequisite(), docker.clone()).without_prerequisites();

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
    let engine =
        TaskEngine::new(config_with_shared_prerequisite(), docker.clone()).without_prerequisites();

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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        extends: None,
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
        log_driver: None,
        log_options: None,
        health_check: None,
        setup_commands: None,
    }
}

/// A fresh Docker volume is created root-owned, so a container running as
/// the host user cannot write to a `cache` mount unless its ownership is
/// changed too — the mount succeeds and the first write fails, which is a
/// confusing place to find out. Batect uploads a directory entry per cache
/// mount for exactly this reason; this asserts the paths reach the Docker
/// layer that does it.
///
/// Covered end to end by the `run-as-current-user-with-cache` conformance
/// case, which is `#[ignore]`d — so without this the default suite would
/// pass with the behaviour removed, which is how the gap was there to be
/// found in the first place.
#[tokio::test]
async fn cache_mounts_are_owned_by_the_mapped_user() {
    let mut container =
        container_with_run_as_current_user("alpine:3.18", None, "/home/container-user");
    container.volumes = Some(vec![
        crate::config::VolumeMount::Cache(crate::config::CacheVolumeMount {
            name: "shared".to_string(),
            container: "/cache".to_string(),
            options: None,
            scope: Default::default(),
        }),
        crate::config::VolumeMount::Cache(crate::config::CacheVolumeMount {
            name: "nested".to_string(),
            container: "/home/container-user/cache".to_string(),
            options: None,
            scope: Default::default(),
        }),
    ]);
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container);
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    // Resolving a `cache` mount needs somewhere to put it; the engine
    // requires this to have been configured rather than guessing.
    // A real directory: resolving a cache also writes the project's own
    // cache key under `.batect/`.
    let project = std::env::temp_dir().join(format!("ratect-cache-owner-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&project).unwrap();
    let engine = TaskEngine::new(config, docker.clone())
        .with_cache_options(crate::cache::CacheType::Volume, project.clone());
    engine.run_task("run", &[]).await.unwrap();

    let (_, _, _, cache_directories) = docker
        .user_mapping_for("app")
        .expect("run_as_current_user should have produced a mapping");
    std::fs::remove_dir_all(&project).ok();

    assert_eq!(
        cache_directories,
        vec![
            "/cache".to_string(),
            "/home/container-user/cache".to_string()
        ]
    );
}

/// A read-only cache cannot be chowned — the put-archive hits the
/// read-only bind and aborts the run before the task starts, turning a
/// configuration that worked into a hard failure. Nothing needs to write to
/// it either, which is what `ro` means.
#[tokio::test]
async fn a_read_only_cache_mount_is_not_owned_by_the_mapped_user() {
    let mut container =
        container_with_run_as_current_user("alpine:3.18", None, "/home/container-user");
    container.volumes = Some(vec![
        crate::config::VolumeMount::Cache(crate::config::CacheVolumeMount {
            name: "writable".to_string(),
            container: "/cache".to_string(),
            options: None,
            scope: Default::default(),
        }),
        crate::config::VolumeMount::Cache(crate::config::CacheVolumeMount {
            name: "readonly".to_string(),
            container: "/ro-cache".to_string(),
            options: Some("ro".to_string()),
            scope: Default::default(),
        }),
        crate::config::VolumeMount::Cache(crate::config::CacheVolumeMount {
            name: "readonly-with-friends".to_string(),
            container: "/ro-z-cache".to_string(),
            options: Some("z,ro".to_string()),
            scope: Default::default(),
        }),
    ]);
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container);
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let project = std::env::temp_dir().join(format!("ratect-ro-cache-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&project).unwrap();
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone())
        .with_cache_options(crate::cache::CacheType::Volume, project.clone());
    engine.run_task("run", &[]).await.unwrap();
    std::fs::remove_dir_all(&project).ok();

    let (_, _, _, cache_directories) = docker
        .user_mapping_for("app")
        .expect("run_as_current_user should have produced a mapping");
    assert_eq!(cache_directories, vec!["/cache".to_string()]);
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
        forbid_telemetry: None,
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
            "/home/container-user".to_string(),
            Vec::new()
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
        forbid_telemetry: None,
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
            "/home/container-user".to_string(),
            Vec::new()
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
            extends: None,
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
            log_driver: None,
            log_options: None,
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
        forbid_telemetry: None,
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
        extends: None,
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
        log_driver: None,
        log_options: None,
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
        forbid_telemetry: None,
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
async fn build_directory_container_does_not_force_pull_by_default() {
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    assert_eq!(docker.force_pull_for("demo-build-env"), Some(false));
}

#[tokio::test]
async fn build_directory_container_with_always_policy_force_pulls_the_base_image() {
    let mut containers = HashMap::new();
    containers.insert(
        "build-env".to_string(),
        Container {
            image_pull_policy: Some(crate::config::ImagePullPolicy::Always),
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    assert_eq!(docker.force_pull_for("demo-build-env"), Some(true));
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
                id: "default".to_string(),
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    let buildkit = docker
        .buildkit_options_for("demo-build-env")
        .expect("build_secrets/build_ssh should have produced BuildKitOptions");
    assert_eq!(
        buildkit.ssh_agents.get("default"),
        Some(&crate::docker::SshAgentSource::HostAgent)
    );
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

/// The other two `build_ssh` shapes reaching Docker: several agents
/// under distinct ids, one of them serving explicit key files. Without
/// this the conversion is only proven by the `#[ignore]`d real-daemon
/// test, which doesn't run in the default suite — so a regression in
/// how config becomes a `SshAgentSource` would go unnoticed there.
#[tokio::test]
async fn build_ssh_key_paths_reach_docker_as_a_named_key_source() {
    let mut containers = HashMap::new();
    containers.insert(
        "build-env".to_string(),
        Container {
            build_ssh: Some(vec![
                crate::config::SshAgent {
                    id: "default".to_string(),
                    paths: Vec::new(),
                },
                crate::config::SshAgent {
                    id: "deploy".to_string(),
                    paths: vec![
                        "/base/keys/id_ed25519".to_string(),
                        "/base/keys/id_rsa".to_string(),
                    ],
                },
            ]),
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    let buildkit = docker
        .buildkit_options_for("demo-build-env")
        .expect("build_ssh should have produced BuildKitOptions");
    assert_eq!(
        buildkit.ssh_agents.get("default"),
        Some(&crate::docker::SshAgentSource::HostAgent)
    );
    assert_eq!(
        buildkit.ssh_agents.get("deploy"),
        Some(&crate::docker::SshAgentSource::Keys(vec![
            PathBuf::from("/base/keys/id_ed25519"),
            PathBuf::from("/base/keys/id_rsa"),
        ]))
    );
}

/// `classify_ssh_agent_paths` speaks the Docker layer's vocabulary — an
/// agent id — so its errors say nothing about which container is
/// misconfigured. In a project with many containers that leaves the user
/// searching. Every other config error in Ratect names its container.
#[tokio::test]
async fn an_invalid_build_ssh_entry_names_the_container_it_came_from() {
    let mut containers = HashMap::new();
    let mut container = container_with_build_directory("./docker", None);
    // Two sockets under one id: rejected, and only reachable from here
    // because whether a path is a socket is a filesystem question that
    // config loading can't answer.
    let directory = std::env::temp_dir().join(format!("ratect-engine-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let sockets: Vec<String> = ["a", "b"]
        .iter()
        .map(|name| {
            let path = directory.join(name);
            std::os::unix::net::UnixListener::bind(&path).unwrap();
            path.display().to_string()
        })
        .collect();
    container.build_ssh = Some(vec![crate::config::SshAgent {
        id: "deploy".to_string(),
        paths: sockets,
    }]);
    containers.insert("build-env".to_string(), container);
    let mut tasks = HashMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let engine = TaskEngine::new(config, FakeContainerRuntime::default());
    let err = engine.run_task("build", &[]).await.unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains("container 'build-env'"),
        "the error should name the container: {message}"
    );
    assert!(
        message.contains("deploy"),
        "and still name the agent: {message}"
    );

    std::fs::remove_dir_all(&directory).unwrap();
}

/// The attribution belongs to the *build*, not to any one error site:
/// a Docker-layer failure knows only an image tag, and the keyring's
/// knows only a key file path. Pinning it on a failure from the Docker
/// layer — the one furthest from the config — is what proves the
/// wrapper covers everything rather than just the case it was added
/// for.
#[tokio::test]
async fn a_failed_build_names_the_container_whatever_layer_failed() {
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
        forbid_telemetry: None,
    };

    let engine = TaskEngine::new(
        config,
        FakeContainerRuntime::default().failing_image_build(),
    );
    let err = engine.run_task("build", &[]).await.unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains("container 'build-env'"),
        "the error should name the container: {message}"
    );
    assert!(
        message.contains("the daemon said no"),
        "and keep the underlying cause: {message}"
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
            extends: None,
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
            log_driver: None,
            log_options: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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

/// An already-recorded interrupt wins because `run_task_internal`'s
/// `select!` polls the run first (`biased`) and the run can't finish
/// synchronously — the run delay is what holds it at an await point long
/// enough for the interrupt branch to be reached. Nothing needs virtual
/// time to advance, so this stays deterministic.
fn interrupted_engine(
    interrupts: usize,
) -> (
    FakeContainerRuntime,
    TaskEngine<FakeContainerRuntime>,
    Arc<crate::interrupt::Interrupt>,
) {
    let config = config_with_database_dependency(|_| {});
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(60));
    let interrupt = crate::interrupt::Interrupt::new();
    for _ in 0..interrupts {
        interrupt.record();
    }
    let engine = TaskEngine::new(config, docker.clone()).with_interrupt(Arc::clone(&interrupt));
    (docker, engine, interrupt)
}

#[tokio::test]
async fn an_interrupt_abandons_the_run_and_still_cleans_up() {
    let (docker, engine, _interrupt) = interrupted_engine(1);

    let error = engine.run_task("start", &[]).await.unwrap_err();

    assert!(
        error.is::<crate::interrupt::TaskInterrupted>(),
        "an interrupted run should fail with TaskInterrupted, not a generic error: {error:#}"
    );

    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
        "a dependency started before the interrupt must still be removed: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.starts_with("network-remove:")),
        "the task's own network must still be removed: {events:?}"
    );
}

/// A `SIGTERM` takes the very same path as Ctrl+C — the run is abandoned
/// and cleaned up — but the failure has to carry *which* signal ended it,
/// since that is what both binaries turn into an exit code.
#[tokio::test]
async fn a_termination_signal_cleans_up_and_is_reported_as_itself() {
    let (docker, engine, interrupt) = interrupted_engine(0);
    interrupt.record_signal(crate::interrupt::TerminationSignal::Terminate);

    let error = engine.run_task("start", &[]).await.unwrap_err();

    let interrupted = error
        .downcast_ref::<crate::interrupt::TaskInterrupted>()
        .expect("a signalled run should fail with TaskInterrupted: {error:#}");
    assert_eq!(
        interrupted.signal,
        crate::interrupt::TerminationSignal::Terminate
    );

    let events = docker.events();
    assert!(
        events.iter().any(|e| e.starts_with("network-remove:")),
        "a terminated run must still remove its network: {events:?}"
    );
}

/// The whole point of routing an interrupt through the ordinary failure
/// path: `--no-cleanup-after-failure` governs it, exactly as it governs a
/// build or health-check failure. Batect behaves identically — its
/// `UserInterruptedExecutionEvent` is a `TaskFailedEvent`, so
/// `TaskStateMachine` selects `behaviourAfterFailure` for it.
#[tokio::test]
async fn an_interrupt_leaves_everything_alone_with_cleanup_after_failure_disabled() {
    let (docker, engine, _interrupt) = interrupted_engine(1);
    let engine = engine.without_cleanup_after_failure();

    let error = engine.run_task("start", &[]).await.unwrap_err();
    assert!(error.is::<crate::interrupt::TaskInterrupted>());

    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("sidecar-stop:")),
        "--no-cleanup-after-failure must leave an interrupted run's containers alone: \
             {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("network-remove:")),
        "--no-cleanup-after-failure must leave the network alone too: {events:?}"
    );
}

/// The task's own container is removed by `run_container` itself on every
/// path *except* an interrupt, where that future is dropped before it can
/// — so the engine removes it, from an id recorded as it started. Without
/// this, the whole `task_container_id` path could break with the
/// non-Docker suite still green.
#[tokio::test]
async fn an_interrupt_removes_the_tasks_own_container_too() {
    let (docker, engine, _interrupt) = interrupted_engine(1);

    let error = engine.run_task("start", &[]).await.unwrap_err();
    assert!(error.is::<crate::interrupt::TaskInterrupted>());

    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container must be removed on an interrupt, since \
             run_container never got to: {events:?}"
    );
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
        "and the dependency too: {events:?}"
    );
}

/// Arming the handler replaces the process's default behaviour for every
/// trapped signal for the whole run, so a signal Ratect doesn't act on is
/// one it has silently swallowed. The abandonment rule is therefore relative to the
/// interrupts already seen when cleanup started — otherwise the first
/// Ctrl+C during the cleanup of a run that finished normally would do
/// nothing at all, which is the common case rather than an exotic one.
#[tokio::test]
async fn an_interrupt_during_cleanup_abandons_it_even_when_the_run_was_not_interrupted() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    // Fires as cleanup removes its first container, so the run itself
    // completes entirely uninterrupted.
    let docker = FakeContainerRuntime::default().interrupting_on_stop(&interrupt);
    let engine = TaskEngine::new(config, docker.clone()).with_interrupt(Arc::clone(&interrupt));

    engine
        .run_task("start", &[])
        .await
        .expect("the run itself was never interrupted, so it should succeed");

    assert_eq!(
        interrupt.count(),
        1,
        "exactly one interrupt, landing during cleanup"
    );
    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("network-remove:")),
        "a single Ctrl+C during an uninterrupted run's cleanup should stop it, \
             leaving the network in place: {events:?}"
    );
}

/// A second Ctrl+C means "stop now", including stopping the cleanup the
/// first one started — cleanup talks to the daemon and can take tens of
/// seconds when a container ignores `SIGTERM`.
///
/// The second interrupt has to land *during* cleanup, not merely be the
/// second one overall: two presses that both arrive while the run is
/// still going are one decision ("stop"), and Batect draws the line in
/// the same place — only an interrupt reaching its cleanup stage switches
/// it to `PostTaskManualCleanup.Required`.
#[tokio::test]
async fn a_second_interrupt_during_cleanup_abandons_it() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(60))
        .interrupting_on_stop(&interrupt);
    let engine = TaskEngine::new(config, docker.clone()).with_interrupt(Arc::clone(&interrupt));

    let error = engine.run_task("start", &[]).await.unwrap_err();
    assert!(error.is::<crate::interrupt::TaskInterrupted>());

    let events = docker.events();
    // The task's own container is removed first, and it's that removal
    // which lands the second interrupt — mid-flight, so it's the one that
    // gets dropped, and nothing after it starts.
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the removal in flight when the interrupt landed should be dropped, \
             not run to completion: {events:?}"
    );
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
        "cleanup should stop rather than pressing on to the dependency: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("network-remove:")),
        "and should stop before removing the network: {events:?}"
    );
}

/// Without an interrupt tracker at all — every unit test, and both
/// binaries before 0.25.0 — the run is awaited directly and nothing
/// about its behaviour changes.
#[tokio::test]
async fn a_run_with_no_interrupt_tracker_is_unaffected() {
    let config = config_with_database_dependency(|_| {});
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine
        .run_task("start", &[])
        .await
        .expect("an uninterrupted run should still succeed");

    let events = docker.events();
    assert!(
        events.iter().any(|e| e.starts_with("run:")),
        "the task should have run: {events:?}"
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
        message
            .contains("Setup command './seed-data.sh' in container 'database' exited with code 1"),
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
async fn task_containers_own_health_check_reaches_docker_and_is_waited_on() {
    // 0.21.0 closed this gap: the task's own container now goes through
    // the same readiness gate a dependency always has (see
    // `run_task_container_readiness`), run concurrently with its main
    // command rather than gating anything.
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
        forbid_telemetry: None,
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
    // ...and its verdict is now actually waited on.
    let events = docker.events();
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("wait-healthy:sidecar-id-app")),
        "the task's own container should now be gated on health: {events:?}"
    );
}

#[tokio::test]
async fn unhealthy_task_container_fails_the_task() {
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().with_unhealthy_container("app");
    let engine = TaskEngine::new(config, docker.clone());

    let result = engine.run_task("start", &[]).await;

    assert!(
        result.is_err(),
        "an unhealthy task container should fail the task even though its own command \
             would have succeeded"
    );
}

#[tokio::test]
async fn task_containers_own_setup_commands_run() {
    let mut containers = HashMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = HashMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    assert!(
        events
            .iter()
            .any(|e| e == "exec:sidecar-id-app:./migrate.sh"),
        "the task's own container's setup command should have run: {events:?}"
    );
}

#[tokio::test]
async fn failing_setup_command_on_the_tasks_own_container_fails_the_task() {
    let mut containers = HashMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = HashMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh");
    let engine = TaskEngine::new(config, docker.clone());

    let result = engine.run_task("start", &[]).await;

    assert!(
        result.is_err(),
        "a failing setup command on the task's own container should fail the task even \
             though its own command would have succeeded"
    );
}

/// A readiness-gate failure on the task's own container is an
/// *infrastructure* failure, not the container's own verdict — so
/// `--no-cleanup-after-failure` keeps it, exactly as it keeps that run's
/// sidecars and network. This is only assertable now that the engine
/// owns the removal; while `run_container` removed its own container it
/// classified the same error as a completed run and force-removed the
/// one container the flag existed to preserve.
#[tokio::test]
async fn a_readiness_failure_on_the_tasks_own_container_honours_no_cleanup_after_failure() {
    let config = config_with_failing_task_container_setup_command();
    let docker = FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh");
    let engine = TaskEngine::new(config, docker.clone()).without_cleanup_after_failure();

    engine.run_task("start", &[]).await.unwrap_err();

    let events = docker.events();
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container must be left for investigation: {events:?}"
    );
}

/// The other side of the same coin — with the flag left alone, that
/// container is cleaned up like anything else.
#[tokio::test]
async fn a_readiness_failure_on_the_tasks_own_container_still_removes_it_by_default() {
    let config = config_with_failing_task_container_setup_command();
    let docker = FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh");
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap_err();

    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container should be removed: {events:?}"
    );
}

/// `--no-cleanup-after-success` must *not* apply to a readiness failure:
/// it is the flag for keeping a container whose command ran, and this
/// container's didn't get that far. Pins the two flags apart from each
/// other on the same scenario as the two tests above.
#[tokio::test]
async fn a_readiness_failure_is_unaffected_by_no_cleanup_after_success() {
    let config = config_with_failing_task_container_setup_command();
    let docker = FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh");
    let engine = TaskEngine::new(config, docker.clone()).without_cleanup_after_success();

    engine.run_task("start", &[]).await.unwrap_err();

    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container should still be removed: {events:?}"
    );
}

/// `run_container` can fail before it ever creates a container, in which
/// case it drops both channels without sending. The concurrent readiness
/// task has to treat that as "nothing to do" — if it awaited a sender
/// that will never send, the whole run would hang instead of reporting
/// the failure. Every other failure the fake can express sends `created`
/// first, so this shape is unreachable without its own switch.
///
/// Wrapped in a timeout rather than left to hang: a deadlock here would
/// otherwise stall the suite with no indication of which test did it.
#[tokio::test]
async fn a_run_container_failure_before_creation_reports_rather_than_hanging() {
    let config = config_with_database_dependency(|_| {});
    let docker = FakeContainerRuntime::default().failing_container_creation();
    let engine = TaskEngine::new(config, docker.clone());

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.run_task("start", &[]),
    )
    .await
    .expect("the run must not hang waiting on a sender that will never send");

    assert!(result.is_err(), "the creation failure must be reported");

    let events = docker.events();
    assert!(
        events.contains(&"run-create-failed:app".to_string()),
        "the run should have got as far as attempting the task container: {events:?}"
    );
    // Nothing was created, so there is nothing of the task container's to
    // remove — but the rest of the run's cleanup still has to happen.
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "no task container was created, so none should be removed: {events:?}"
    );
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
        "the dependency should still be cleaned up: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.starts_with("network-remove:")),
        "the network should still be removed: {events:?}"
    );
}

/// One task container whose own `setup_commands` entry fails — the
/// shared fixture for the three cleanup-policy tests above, which differ
/// only in which flag they set.
fn config_with_failing_task_container_setup_command() -> Config {
    let mut containers = HashMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = HashMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

#[tokio::test]
async fn task_containers_own_setup_commands_run_concurrently_with_its_main_command() {
    // Proves the concurrency itself, not just that setup commands run
    // at all — same idiom as
    // `independent_dependencies_start_concurrently_not_sequentially`:
    // an equal delay on both the main run and the setup command's own
    // exec means a *sequential* readiness-then-run (or run-then-
    // readiness) model would take roughly their sum, while running them
    // concurrently (matching Batect — see docs/task-lifecycle.md's
    // "Known simplifications") takes roughly just the one delay.
    let mut containers = HashMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = HashMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", delay)
        .with_exec_delay("./migrate.sh", delay);
    let engine = TaskEngine::new(config, docker.clone());

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < delay * 2,
        "the task's own container's main command and its setup command should overlap, not \
             run sequentially (elapsed: {elapsed:?})"
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
        forbid_telemetry: None,
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
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container must not be removed either: {events:?}"
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
async fn without_cleanup_after_failure_leaves_everything_in_place_on_an_infrastructure_failure() {
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
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container should still be removed too: {events:?}"
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
    }
}

#[tokio::test(start_paused = true)]
async fn max_parallelism_of_one_serializes_independent_image_pulls() {
    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_pull_delay("image-a:1", delay)
        .with_pull_delay("image-b:1", delay);
    let engine =
        TaskEngine::new(config_with_two_independent_image_pulls(), docker).with_max_parallelism(1);

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
    let engine =
        TaskEngine::new(config_with_two_independent_image_pulls(), docker).with_max_parallelism(2);

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
        forbid_telemetry: None,
    }
}

#[tokio::test(start_paused = true)]
async fn max_parallelism_of_one_serializes_independent_container_starts() {
    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_start_delay("dep-a", delay)
        .with_start_delay("dep-b", delay);
    let engine =
        TaskEngine::new(config_with_two_independent_dependencies(), docker).with_max_parallelism(1);

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
    let engine =
        TaskEngine::new(config_with_two_independent_dependencies(), docker).with_max_parallelism(1);

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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
            extends: None,
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
            log_driver: None,
            log_options: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    // Ratect's own ownership labels are added alongside, never
    // instead of, the container's configured ones — see
    // `crate::labels`.
    let labels = docker
        .labels_for("build-env")
        .expect("labels should be set");
    assert_eq!(labels["com.example.owner"], "platform-team");
    assert_eq!(labels[crate::labels::CONTAINER], "build-env");
}

/// The whole point of the labels: everything one task execution
/// created can be found again, and recognized as belonging to that run
/// rather than any other. See `crate::labels` and `ROADMAP.md`'s
/// orphaned-resource entry.
#[tokio::test]
async fn every_resource_a_run_creates_is_labelled_with_that_run() {
    let mut containers = HashMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut app = container("alpine:3.18", None);
    app.dependencies = Some(vec!["database".to_string()]);
    containers.insert("app".to_string(), app);
    let mut tasks = HashMap::new();
    tasks.insert("check".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone())
        .with_settings(TaskEngineSettings {
            ratect_version: Some("1.2.3".to_string()),
            ..TaskEngineSettings::default()
        })
        .unwrap();
    engine.run_task("check", &[]).await.unwrap();

    let task_container = docker
        .labels_for("app")
        .expect("the task container is labelled");
    let dependency = docker
        .labels_for("database")
        .expect("a dependency is labelled");
    let network = docker.network_labels().expect("the network is labelled");

    // Same project, task, run and version across all three.
    for labels in [&task_container, &dependency, &network] {
        assert_eq!(labels[crate::labels::PROJECT], "demo");
        assert_eq!(labels[crate::labels::TASK], "check");
        assert_eq!(labels[crate::labels::VERSION], "1.2.3");
    }
    assert_eq!(
        task_container[crate::labels::RUN],
        network[crate::labels::RUN],
        "the task container and the network should agree on the run"
    );
    assert_eq!(
        dependency[crate::labels::RUN],
        network[crate::labels::RUN],
        "a dependency and the network should agree on the run"
    );

    // ...and each container says which one it is, and what it was for.
    assert_eq!(task_container[crate::labels::CONTAINER], "app");
    assert_eq!(task_container[crate::labels::ROLE], "task");
    assert_eq!(dependency[crate::labels::CONTAINER], "database");
    assert_eq!(dependency[crate::labels::ROLE], "dependency");
}

/// `--use-network` creates no network, so the run id can't come from
/// one — the containers still have to agree on which run they're from.
#[tokio::test]
async fn containers_share_a_run_id_even_with_an_existing_network() {
    let mut containers = HashMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut app = container("alpine:3.18", None);
    app.dependencies = Some(vec!["database".to_string()]);
    containers.insert("app".to_string(), app);
    let mut tasks = HashMap::new();
    tasks.insert("check".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone())
        .with_existing_network("someone-elses-network".to_string());
    engine.run_task("check", &[]).await.unwrap();

    assert!(
        docker.network_labels().is_none(),
        "no network should have been created"
    );
    let task_container = docker.labels_for("app").unwrap();
    let dependency = docker.labels_for("database").unwrap();
    assert_eq!(
        task_container[crate::labels::RUN],
        dependency[crate::labels::RUN]
    );
}

#[tokio::test]
async fn container_capabilities_reach_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.capabilities_to_add =
        Some(HashSet::from([crate::config::Capability::NetAdmin]));
    container_config.capabilities_to_drop = Some(HashSet::from([crate::config::Capability::Chown]));
    let mut containers = HashMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = HashMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(docker.enable_init_process_for("build-env"), Some(true));
}

#[tokio::test]
async fn container_log_driver_and_log_options_reach_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.log_driver = Some("json-file".to_string());
    container_config.log_options =
        Some(HashMap::from([("max-size".to_string(), "10m".to_string())]));
    let mut containers = HashMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = HashMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(
        docker.log_driver_for("build-env"),
        Some("json-file".to_string())
    );
    assert_eq!(
        docker.log_options_for("build-env"),
        Some(HashMap::from([("max-size".to_string(), "10m".to_string())]))
    );
}

#[tokio::test]
async fn container_tmpfs_mounts_reach_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.volumes = Some(vec![
        crate::config::VolumeMount::Local(crate::config::LocalVolumeMount {
            local: "/host/code".to_string(),
            container: "/code".to_string(),
            options: None,
        }),
        crate::config::VolumeMount::Tmpfs(crate::config::TmpfsVolumeMount {
            container: "/code/tmp".to_string(),
            options: Some("size=64m".to_string()),
        }),
    ]);
    let mut containers = HashMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = HashMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    // Only the `tmpfs` entry reaches `ContainerOptions.tmpfs` — the
    // `local` entry is resolved separately, into the bind-mount string
    // `run_container`'s own `volumes` parameter carries instead (not
    // captured by `ContainerOptionsValue`, so nothing to assert here).
    assert_eq!(
        docker.tmpfs_for("build-env"),
        Some(vec![("/code/tmp".to_string(), "size=64m".to_string())])
    );
}

#[tokio::test]
async fn container_tmpfs_mount_without_options_defaults_to_an_empty_string() {
    let mut container_config = container("alpine:3.18", None);
    container_config.volumes = Some(vec![crate::config::VolumeMount::Tmpfs(
        crate::config::TmpfsVolumeMount {
            container: "/code/tmp".to_string(),
            options: None,
        },
    )]);
    let mut containers = HashMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = HashMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(
        docker.tmpfs_for("build-env"),
        Some(vec![("/code/tmp".to_string(), String::new())])
    );
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let overrides = HashMap::from([("no-such-container".to_string(), "ubuntu:22.04".to_string())]);
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

/// `with_settings` is how both binaries configure an engine, so a
/// setting it silently drops would take every flag driving it with it —
/// each one is checked against the field the equivalent builder sets.
#[test]
fn with_settings_applies_every_setting() {
    let mut containers = HashMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = HashMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let settings = TaskEngineSettings {
        existing_network: Some("existing".to_string()),
        publish_ports: false,
        propagate_proxy_environment_variables: false,
        run_prerequisites: false,
        image_overrides: HashMap::from([("build-env".to_string(), "ubuntu:22.04".to_string())]),
        image_tags: HashMap::from([(
            "build-env".to_string(),
            HashSet::from(["extra".to_string()]),
        )]),
        cleanup_after_success: false,
        cleanup_after_failure: false,
        max_parallelism: Some(3),
        cache: Some((
            crate::cache::CacheType::Directory,
            PathBuf::from("/projects/demo"),
        )),
        ratect_version: Some("1.2.3".to_string()),
        interrupt: Some(crate::interrupt::Interrupt::new()),
    };
    let engine = TaskEngine::new(config, FakeContainerRuntime::default())
        .with_settings(settings)
        .expect("settings naming a real container should apply");

    assert!(
        engine.interrupt.is_some(),
        "an interrupt tracker should reach the engine, or no signal will clean up"
    );

    assert_eq!(engine.existing_network.as_deref(), Some("existing"));
    assert!(!engine.publish_ports);
    assert!(!engine.propagate_proxy_environment_variables);
    assert!(engine.skip_prerequisites);
    assert_eq!(
        engine.image_overrides.get("build-env").map(String::as_str),
        Some("ubuntu:22.04")
    );
    assert!(engine.image_tags["build-env"].contains("extra"));
    assert!(!engine.cleanup_after_success);
    assert!(!engine.cleanup_after_failure);
    assert_eq!(
        engine
            .max_parallelism
            .as_ref()
            .map(|semaphore| semaphore.available_permits()),
        Some(3)
    );
    assert_eq!(engine.ratect_version.as_deref(), Some("1.2.3"));
    let cache = engine.cache_options.expect("cache options should be set");
    assert_eq!(cache.cache_type, crate::cache::CacheType::Directory);
    assert_eq!(cache.project_directory, PathBuf::from("/projects/demo"));
}

/// The default is "no flags given at all", so an engine built from it
/// has to behave exactly like one built with no builder calls — every
/// binary's no-flags path depends on that.
#[test]
fn default_settings_leave_an_engine_in_its_no_flags_state() {
    let config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::new(),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };
    let engine = TaskEngine::new(config, FakeContainerRuntime::default())
        .with_settings(TaskEngineSettings::default())
        .expect("the default settings never fail to apply");

    assert!(engine.existing_network.is_none());
    assert!(engine.publish_ports);
    assert!(engine.propagate_proxy_environment_variables);
    assert!(!engine.skip_prerequisites);
    assert!(engine.image_overrides.is_empty());
    assert!(engine.image_tags.is_empty());
    assert!(engine.cleanup_after_success);
    assert!(engine.cleanup_after_failure);
    assert!(engine.max_parallelism.is_none());
    assert!(engine.cache_options.is_none());
    assert!(engine.ratect_version.is_none());
}

/// The one setting that can fail — `with_settings` returns a `Result`
/// solely because of it, and the error has to survive the indirection.
#[test]
fn with_settings_still_rejects_an_unknown_image_override() {
    let config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::new(),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };
    let settings = TaskEngineSettings {
        image_overrides: HashMap::from([(
            "no-such-container".to_string(),
            "ubuntu:22.04".to_string(),
        )]),
        ..TaskEngineSettings::default()
    };
    let error =
        match TaskEngine::new(config, FakeContainerRuntime::default()).with_settings(settings) {
            Ok(_) => panic!("an override naming an unknown container should be rejected"),
            Err(error) => error,
        };
    assert_eq!(
        error.to_string(),
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
    assert_eq!(
        docker.host_gateway_for("app"),
        None,
        "--no-proxy-vars should suppress the host-gateway entry with them"
    );
}

/// The entry `proxy::ProxyEnvironment::host_gateway` produces, spelt out so
/// these tests pin what reaches Docker rather than agreeing with whatever
/// the code currently builds.
const HOST_GATEWAY: crate::proxy::HostGateway = crate::proxy::HostGateway {
    name: "host.docker.internal",
    address: "host-gateway",
};

/// A `/proc/net/tcp` table listening on `port` at `address`.
///
/// A cut-down twin of `proxy_tests.rs`'s helper: one listening row, where that
/// one also carries an established connection on the same port to pin the
/// `st == 0A` filter. That filter is the parser's business and is tested
/// there; these tests are about what the *engine* concludes from a parsed
/// result, so the row that exists only to be ignored would be noise here.
///
/// Duplicated rather than shared for that reason — the two fixtures answer to
/// different tests, and tying them together would make either one's needs
/// constrain the other.
fn proc_net_tcp(address: &str, port: u16) -> String {
    format!(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
            0: {address}:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 34567 1 0000000000000000 100 0 0 10 0\n"
    )
}

/// A one-container project, since every test below differs only in the
/// engine it builds around it.
fn proxy_warning_config() -> Config {
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// The half the scope called load-bearing: rewriting the URL cannot reach a
/// proxy bound to `127.0.0.1`, so that case is diagnosed rather than left to
/// surface as a connection refused from a package manager mid-build.
///
/// Asserted through `unreachable_proxy_ports` rather than by capturing the
/// log line, so what's pinned is the decision — `ratect-core` has no
/// `tracing-subscriber` to capture with, and the `warn!` it feeds is one
/// statement over this result.
#[test]
fn a_rewritten_proxy_url_on_a_loopback_bound_port_is_reported() {
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(proxy_warning_config(), docker)
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
        .with_proc_net_tcp(|| vec![proc_net_tcp("0100007F", 3333)]);

    assert_eq!(
        engine.unreachable_proxy_ports(),
        std::collections::BTreeSet::from([3333])
    );
}

#[test]
fn a_proxy_reachable_beyond_loopback_is_not_reported() {
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(proxy_warning_config(), docker)
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
        .with_proc_net_tcp(|| vec![proc_net_tcp("00000000", 3333)]);

    assert!(engine.unreachable_proxy_ports().is_empty());
}

/// Only rewritten URLs are checked. A proxy already naming a routable host
/// may well have a loopback-bound service on the same port number locally,
/// and warning about that would be a false alarm about an unrelated socket.
#[test]
fn a_proxy_url_that_was_not_rewritten_is_never_reported() {
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(proxy_warning_config(), docker)
        .with_host_env(|name| {
            (name == "http_proxy").then(|| "http://proxy.example.com:3333".to_string())
        })
        .with_proc_net_tcp(|| vec![proc_net_tcp("0100007F", 3333)]);

    assert!(engine.unreachable_proxy_ports().is_empty());
}

/// `--no-proxy-vars` is the documented escape hatch, so it has to silence
/// the diagnostic about the propagation it just turned off.
#[test]
fn no_proxy_vars_silences_the_unreachable_proxy_warning() {
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(proxy_warning_config(), docker)
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
        .with_proc_net_tcp(|| vec![proc_net_tcp("0100007F", 3333)])
        .without_proxy_environment_variables();

    assert!(engine.unreachable_proxy_ports().is_empty());
}

/// The platform default, and the reason the seam above exists: with no
/// `/proc` to read there is nothing to conclude, and the run must say
/// nothing rather than guess.
#[test]
fn a_host_with_no_proc_net_tcp_reports_nothing() {
    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(proxy_warning_config(), docker)
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
        .with_proc_net_tcp(Vec::new);

    assert!(engine.unreachable_proxy_ports().is_empty());
}

/// Rewriting the URL to name the host is only useful if the name resolves,
/// and on Linux nothing supplies it — so the run that rewrites must also
/// add the entry, or it has swapped one unreachable URL for another.
#[tokio::test]
async fn a_rewritten_proxy_url_adds_the_host_gateway_to_a_tasks_own_container() {
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone())
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()));

    engine.run_task("run", &[]).await.unwrap();

    assert_eq!(
        docker.environment_for("app").unwrap().get("http_proxy"),
        Some(&"http://host.docker.internal:3333/".to_string())
    );
    assert_eq!(docker.host_gateway_for("app"), Some(HOST_GATEWAY));
}

/// A dependency's container is behind the same proxy as the task's own, and
/// nothing about it makes the name resolve on its own.
#[tokio::test]
async fn a_rewritten_proxy_url_adds_the_host_gateway_to_a_dependencys_container() {
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone())
        .with_host_env(|name| (name == "http_proxy").then(|| "http://127.0.0.1:3333".to_string()));

    engine.run_task("run", &[]).await.unwrap();

    assert_eq!(docker.host_gateway_for("database"), Some(HOST_GATEWAY));
}

/// A `RUN` step reaches the proxy through the same rewritten URL the
/// container will, so a build without the entry fails exactly where the
/// image it produces would have worked.
#[tokio::test]
async fn a_rewritten_proxy_url_adds_the_host_gateway_to_an_image_build() {
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container_with_build_directory(".", None));
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone())
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()));

    engine.run_task("run", &[]).await.unwrap();

    assert_eq!(
        docker.build_host_gateway_for("demo-app"),
        Some(HOST_GATEWAY)
    );
}

/// A proxy that already names a routable host needs nothing added — the
/// alternative, adding it whenever any proxy variable is set, would put a
/// name into every container that nothing in the run asked for.
#[tokio::test]
async fn a_proxy_url_that_was_not_rewritten_adds_no_host_gateway() {
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone()).with_host_env(|name| {
        (name == "http_proxy").then(|| "http://proxy.example.com:8080".to_string())
    });

    engine.run_task("run", &[]).await.unwrap();

    assert!(docker.environment_for("app").is_some());
    assert_eq!(docker.host_gateway_for("app"), None);
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
    let mut database = container_with_ports("postgres:16", vec![single_port(5432, 5432, "tcp")]);
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
    container_config.environment = Some(HashMap::from([("TERM".to_string(), "dumb".to_string())]));
    let mut containers = HashMap::new();
    containers.insert("app".to_string(), container_config);
    let mut tasks = HashMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let labels = docker.labels_for("database").expect("labels should be set");
    assert_eq!(labels["com.example.role"], "database");
    assert_eq!(labels[crate::labels::CONTAINER], "database");
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
async fn dependency_container_tmpfs_mounts_reach_the_sidecar() {
    let mut database = container("postgres:16", None);
    database.volumes = Some(vec![crate::config::VolumeMount::Tmpfs(
        crate::config::TmpfsVolumeMount {
            container: "/tmp/pgdata".to_string(),
            options: None,
        },
    )]);
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert_eq!(
        docker.tmpfs_for("database"),
        Some(vec![("/tmp/pgdata".to_string(), String::new())])
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert_eq!(docker.enable_init_process_for("database"), Some(true));
}

#[tokio::test]
async fn dependency_container_log_driver_reaches_the_sidecar() {
    let mut database = container("postgres:16", None);
    database.log_driver = Some("syslog".to_string());
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
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = TaskEngine::new(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert_eq!(
        docker.log_driver_for("database"),
        Some("syslog".to_string())
    );
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
        forbid_telemetry: None,
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
        // After `RunningTaskContainer`, not before: that one announces
        // the *stage*, this one reports Docker having actually created
        // the container, which is when it becomes the engine's to clean
        // up.
        TaskEvent::TaskContainerCreated {
            container: "build-env".into(),
        },
        // `build-env` has no `health_check` of its own, but the task's
        // own container now goes through the same readiness gate a
        // dependency always has (0.21.0) — a container with no health
        // check at all is immediately considered healthy, same as for
        // a dependency, so this still posts unconditionally.
        TaskEvent::ContainerBecameHealthy {
            container: "build-env".into(),
        },
        TaskEvent::CleanupStarting,
        // The task's own container is removed by the engine like any
        // other, and first — before the dependencies it was using.
        // `run_container` used to remove it silently, which is why this
        // event didn't exist here before.
        TaskEvent::ContainerRemoved {
            container: "build-env".into(),
        },
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
            forbid_telemetry: None,
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
        forbid_telemetry: None,
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
        forbid_telemetry: None,
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
