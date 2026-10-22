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
use crate::cache::VolumeStore;
use crate::config::{
    BuildSecret, Config, Container, Dialect, NetworkMode, PortMapping, Task, TaskRun,
};
use crate::resources::ResourceInventory;
use crate::ui::NullEventSink;
use std::collections::HashMap;
use std::sync::Arc;

/// Records every call as a single ordered event log instead of talking to
/// Docker, so tests can assert on dedup, cleanup, and ordering behavior
/// (including across pull/network/sidecar/run calls) quickly and
/// deterministically.
/// Every `build_image` call, keyed by the tag it was given — one
/// `BuildSpec` per call, rather than a `Captured*` map per field, the same
/// reason `CapturedSpecs` captures a whole `ContainerSpec` instead of one.
type CapturedBuildSpecs = Arc<Mutex<HashMap<String, crate::container_spec::BuildSpec>>>;
/// `(additional_hostnames, additional_hosts, ports)`.
type NetworkOptionsValue = (
    Option<Vec<String>>,
    Option<HashMap<String, String>>,
    Option<Vec<(u16, u16, String)>>,
);
/// Every `create_container` call, in the order they happened — one
/// [`crate::container_spec::ContainerSpec`] per call, rather than a
/// `Captured*` map per field, so the fake captures whatever `ContainerSpec`
/// grows without a matching new map each time. The `_for`
/// accessors below (`environment_for`/`command_for`/`network_options_for`/
/// etc.) each project one part of the *last* entry for a given container
/// name out of this, preserving every existing call site's shape —
/// `spec_for` is the one place that does the actual lookup.
type CapturedSpecs = Arc<Mutex<Vec<(String, crate::container_spec::ContainerSpec)>>>;
/// `(working_directory, environment, (uid, gid))`, keyed by the exec'd
/// command string.
type ExecValue = (
    Option<String>,
    Option<HashMap<String, String>>,
    Option<(u32, u32)>,
);
type CapturedExecs = Arc<Mutex<HashMap<String, ExecValue>>>;
/// `(stop_signal, stop_grace_period)` a prior `stop_and_remove_container`
/// call for a given container id was given — see `stop_options_for`.
type StopOptionsValue = (Option<String>, Option<std::time::Duration>);
type CapturedStopOptions = Arc<Mutex<HashMap<String, StopOptionsValue>>>;

#[derive(Clone)]
struct FakeContainerRuntime {
    events: Arc<Mutex<Vec<String>>>,
    fail_run: Arc<Mutex<bool>>,
    // Container name -> the exit code its `run_container` reports, overriding
    // `fail_run` for that container alone (see `with_run_exit`).
    run_exits: Arc<Mutex<HashMap<String, i64>>>,
    // Every `create_container` call, in order — see `CapturedSpecs`'s own
    // doc comment.
    specs: CapturedSpecs,
    // Every `build_image` call, keyed by tag — see `CapturedBuildSpecs`'s
    // own doc comment.
    build_specs: CapturedBuildSpecs,
    // What `network_exists` reports — defaults to `true` so tests that
    // don't care about `--use-network` aren't affected.
    network_exists_result: Arc<Mutex<bool>>,
    // The labels a prior `create_network` call was given (see
    // `network_labels`).
    network_labels: Arc<Mutex<Option<HashMap<String, String>>>>,
    // The options a prior `exec_in_container` call for a given command
    // was given (see `exec_for`).
    execs: CapturedExecs,
    // The `stop_signal`/`stop_grace_period` a prior `stop_and_remove_container`
    // call for a given container id was given (see `stop_options_for`,
    // ratect#112).
    stop_options: CapturedStopOptions,
    // Container id whose `wait_for_container_healthy` reports unhealthy
    // (see `with_unhealthy_container`).
    unhealthy_container: Arc<Mutex<Option<String>>>,
    // Command whose `exec_in_container` reports a non-zero exit (see
    // `with_failing_setup_command`).
    failing_setup_command: Arc<Mutex<Option<String>>>,
    // Container names whose `create_container` fails before Docker would
    // have created anything — a spec the daemon refuses (see
    // `failing_creation`).
    failing_creations: Arc<Mutex<HashSet<String>>>,
    // Container names whose `create_container` fails *after* Docker has
    // created the container — the user-mapping upload or network join
    // failing — so the error carries the id (see
    // `failing_creation_after_docker_created`).
    failing_creations_after_docker_created: Arc<Mutex<HashSet<String>>>,
    // Makes `build_image` fail, standing in for any Docker-layer build
    // failure (see `failing_image_build`).
    fail_image_build: Arc<Mutex<bool>>,
    rejected_tags: Arc<Mutex<HashSet<String>>>,
    // Images `image_exists_locally` reports as already present (see
    // `with_local_image`) — defaults to empty, so tests that don't care
    // about `image_pull_policy` see the "always needs a pull" behavior
    // that matches an `IfNotPresent` container whose image is missing.
    locally_present_images: Arc<Mutex<HashSet<String>>>,
    // Artificial `tokio::time::sleep` durations `create_container`/
    // `start_background_container`/`pull_image` wait out before doing
    // anything else (see `with_create_delay`/`with_start_delay`/
    // `with_pull_delay`) — lets a `#[tokio::test(start_paused = true)]`
    // test prove two independent operations actually overlap in (virtual)
    // time, rather than just asserting on event order/counts. Creation and
    // start are delayed separately so a test can hold a container in
    // either state — created but not started is the window a sibling's
    // failure lands in (ratect#224), and the state decisions/0019 adds.
    create_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    start_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    // Container names whose `start_background_container` fails (see
    // `failing_start`).
    failing_starts: Arc<Mutex<HashSet<String>>>,
    pull_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    // Images whose `pull_image` fails, after any `pull_delays` entry for
    // them has elapsed (see `with_failing_pull`).
    failing_pulls: Arc<Mutex<HashSet<String>>>,
    // Same idea as `pull_delays`, for `build_image` (keyed by the build's
    // tag — see `with_build_delay`).
    build_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    // Makes `create_network` fail (see `failing_network_creation`).
    fail_network_creation: Arc<Mutex<bool>>,
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
    // How long `run_container` for a container name keeps going *after* its
    // main command has exited — a real one is still draining output and
    // waiting on Docker's own exit report — so a readiness gate the exit
    // itself failed can resolve first (see `with_run_draining_after_exit`).
    run_drain_delays: Arc<Mutex<HashMap<String, std::time::Duration>>>,
    // Container ids whose main command has exited, as `container_is_running`
    // reports them.
    exited_containers: Arc<Mutex<HashSet<String>>>,
    // What `container_is_running` answers instead of the truth, when set —
    // `Ok(true)` for a daemon that hasn't yet recorded an exit, `Err` for an
    // inspect that fails (see `with_running_probe`).
    running_probe: Arc<Mutex<Option<std::result::Result<bool, String>>>>,
    // When set, `wait_for_container_healthy` and `exec_in_container` fail
    // as Docker does on a container `exited_containers` holds by the time
    // they resolve — its own `die` event and its 409 "is not running" —
    // rather than reaching a verdict. Off by default: a
    // run with no delays exits before its gate is first polled, and every
    // test not about that ordering expects the gate's configured verdict
    // (see `with_gate_observing_exits`).
    gate_observes_exits: Arc<Mutex<bool>>,
    // Container id (keyed the same way as `health_check_delays`) ->
    // `(delay, exit_code)` `wait_for_container_exit` resolves with — see
    // `with_dependency_exit`. Absent means "never exits on its own" (the
    // default for every container): `wait_for_container_exit` then never
    // resolves, matching a dependency that just keeps running until
    // something else stops it.
    dependency_exits: Arc<Mutex<HashMap<String, (std::time::Duration, i64)>>>,
    /// What `container_output` returns per container id — an external
    /// health check's companion diagnoses its own failure there.
    container_outputs: Arc<Mutex<HashMap<String, String>>>,
    /// When set, each removal (`stop_and_remove_container` or
    /// `remove_container_forcibly`, in the order the engine makes them)
    /// records the next queued signal on the tracker *before* doing its own
    /// work — the only way to land one in the middle of cleanup
    /// deterministically, since cleanup against this fake is otherwise
    /// instant. A queue rather than one signal so a test can choose exactly
    /// which removal each rung of the cleanup ladder lands on. See
    /// `a_signal_during_cleanup_escalates_it_even_when_the_run_was_not_interrupted`.
    #[allow(clippy::type_complexity)]
    signals_on_removal: Arc<
        Mutex<
            Option<(
                Arc<crate::interrupt::Interrupt>,
                std::collections::VecDeque<crate::interrupt::TerminationSignal>,
            )>,
        >,
    >,
    // Container ids whose removal — graceful or forced — the daemon refuses
    // (see `refusing_container_removal`).
    refused_container_removals: Arc<Mutex<HashSet<String>>>,
    // Makes `remove_network` fail, as a daemon refusing to remove a network
    // with an endpoint still attached does (see `refusing_network_removal`).
    refuse_network_removal: Arc<Mutex<bool>>,
}

impl Default for FakeContainerRuntime {
    fn default() -> Self {
        Self {
            events: Default::default(),
            fail_run: Default::default(),
            run_exits: Default::default(),
            specs: Default::default(),
            build_specs: Default::default(),
            network_exists_result: Arc::new(Mutex::new(true)),
            network_labels: Default::default(),
            execs: Default::default(),
            stop_options: Default::default(),
            unhealthy_container: Default::default(),
            failing_setup_command: Default::default(),
            failing_creations: Default::default(),
            failing_creations_after_docker_created: Default::default(),
            fail_image_build: Default::default(),
            rejected_tags: Default::default(),
            locally_present_images: Default::default(),
            create_delays: Default::default(),
            start_delays: Default::default(),
            failing_starts: Default::default(),
            pull_delays: Default::default(),
            failing_pulls: Default::default(),
            build_delays: Default::default(),
            fail_network_creation: Default::default(),
            exec_delays: Default::default(),
            health_check_delays: Default::default(),
            run_delays: Default::default(),
            run_drain_delays: Default::default(),
            exited_containers: Default::default(),
            running_probe: Default::default(),
            gate_observes_exits: Default::default(),
            dependency_exits: Default::default(),
            container_outputs: Default::default(),
            signals_on_removal: Default::default(),
            refused_container_removals: Default::default(),
            refuse_network_removal: Default::default(),
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

    /// Makes `run_container` for container `name` report its command
    /// exiting with `exit_code`, whatever `failing_run` says — so one task
    /// in a chain can succeed while another fails with a code of its own.
    fn with_run_exit(self, name: &str, exit_code: i64) -> Self {
        self.run_exits
            .lock()
            .unwrap()
            .insert(name.to_string(), exit_code);
        self
    }

    /// Makes every removal of `container_id` fail, as the daemon does for a
    /// container something else already removed (`404: No such container`).
    /// The removal records no event, since nothing was removed.
    fn refusing_container_removal(self, container_id: &str) -> Self {
        self.refused_container_removals
            .lock()
            .unwrap()
            .insert(container_id.to_string());
        self
    }

    /// Makes `remove_network` fail, as the daemon does while a container
    /// outside the run is still attached (`403: … has active endpoints`).
    fn refusing_network_removal(self) -> Self {
        *self.refuse_network_removal.lock().unwrap() = true;
        self
    }

    fn refuses_removal_of(&self, container_id: &str) -> Result<()> {
        if self
            .refused_container_removals
            .lock()
            .unwrap()
            .contains(container_id)
        {
            anyhow::bail!("No such container: {container_id}");
        }
        Ok(())
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

    /// Makes `create_container` for container `name` fail before Docker
    /// creates anything — a spec the daemon refuses (an invalid mount, a
    /// log driver it doesn't have), so there is nothing to clean up.
    fn failing_creation(self, name: &str) -> Self {
        self.failing_creations
            .lock()
            .unwrap()
            .insert(name.to_string());
        self
    }

    /// Makes `create_container` for container `name` fail *after* Docker
    /// has created the container — the user-mapping upload or the network
    /// join failing — so the error carries the id the engine has to remove.
    fn failing_creation_after_docker_created(self, name: &str) -> Self {
        self.failing_creations_after_docker_created
            .lock()
            .unwrap()
            .insert(name.to_string());
        self
    }

    /// Makes `build_image` fail with an error of the Docker layer's own
    /// wording — one that says nothing about which container asked for
    /// the build.
    fn failing_image_build(self) -> Self {
        *self.fail_image_build.lock().unwrap() = true;
        self
    }

    /// Makes `create_network` fail, as a daemon with an exhausted address
    /// pool would.
    fn failing_network_creation(self) -> Self {
        *self.fail_network_creation.lock().unwrap() = true;
        self
    }

    /// Makes `tag_image` fail for `tag`, as a daemon refusing it would.
    fn rejecting_tag(self, tag: &str) -> Self {
        self.rejected_tags.lock().unwrap().insert(tag.to_string());
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

    /// Makes `create_container` for container name `name` artificially
    /// `tokio::time::sleep` for `delay` before creating it — used with
    /// `#[tokio::test(start_paused = true)]` to hold a container
    /// uncreated while something else happens, or to prove two creates
    /// overlap (or don't, under `--max-parallelism`).
    fn with_create_delay(self, name: &str, delay: std::time::Duration) -> Self {
        self.create_delays
            .lock()
            .unwrap()
            .insert(name.to_string(), delay);
        self
    }

    /// Makes `start_background_container` for container name `name`
    /// artificially `tokio::time::sleep` for `delay` before starting it —
    /// used with `#[tokio::test(start_paused = true)]` to prove two
    /// independent dependencies actually start concurrently (overlapping
    /// in virtual time), not sequentially, or to hold a container in its
    /// created-but-not-started state so a sibling dependency's failure
    /// can cut the start short once Docker already has a container to
    /// clean up.
    fn with_start_delay(self, name: &str, delay: std::time::Duration) -> Self {
        self.start_delays
            .lock()
            .unwrap()
            .insert(name.to_string(), delay);
        self
    }

    /// Makes `start_background_container` for container name `name` fail —
    /// the start itself failing, a port already bound say — after the
    /// container was created and its id handed back.
    fn failing_start(self, name: &str) -> Self {
        self.failing_starts.lock().unwrap().insert(name.to_string());
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

    /// See the `run_drain_delays` field.
    fn with_run_draining_after_exit(self, name: &str, delay: std::time::Duration) -> Self {
        self.run_drain_delays
            .lock()
            .unwrap()
            .insert(name.to_string(), delay);
        self
    }

    /// See the `gate_observes_exits` field.
    fn with_gate_observing_exits(self) -> Self {
        *self.gate_observes_exits.lock().unwrap() = true;
        self
    }

    /// Whether the gate should find `container_id` stopped — see the
    /// `gate_observes_exits` field.
    fn gate_finds_stopped(&self, container_id: &str) -> bool {
        *self.gate_observes_exits.lock().unwrap()
            && self
                .exited_containers
                .lock()
                .unwrap()
                .contains(container_id)
    }

    /// See the `running_probe` field.
    fn with_running_probe(self, answer: std::result::Result<bool, &str>) -> Self {
        *self.running_probe.lock().unwrap() = Some(answer.map_err(str::to_string));
        self
    }

    /// See the `signals_on_removal` field.
    fn signalling_on_removal(
        self,
        interrupt: &Arc<crate::interrupt::Interrupt>,
        signals: &[crate::interrupt::TerminationSignal],
    ) -> Self {
        *self.signals_on_removal.lock().unwrap() =
            Some((Arc::clone(interrupt), signals.iter().copied().collect()));
        self
    }

    /// Records the next queued signal, if any, and yields so the removal
    /// that called this is genuinely *in flight* when the engine's race
    /// next polls — a real removal waits on the daemon (up to Docker's whole
    /// kill timeout for a container ignoring `SIGTERM`), which is the case
    /// the race exists for. Returning `Ready` immediately would instead test
    /// the one situation that can't happen.
    async fn signal_from_removal(&self) {
        let recorded = {
            let mut queued = self.signals_on_removal.lock().unwrap();
            match queued.as_mut() {
                Some((interrupt, signals)) => signals.pop_front().map(|signal| {
                    interrupt.record_signal(signal);
                }),
                None => None,
            }
        };
        if recorded.is_some() {
            tokio::task::yield_now().await;
        }
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

    /// Makes `pull_image` fail for `image` — after its `with_pull_delay`,
    /// if it has one — as a registry that doesn't have it would.
    fn with_failing_pull(self, image: &str) -> Self {
        self.failing_pulls.lock().unwrap().insert(image.to_string());
        self
    }

    /// Makes `build_image` for the build tagged `tag` artificially
    /// `tokio::time::sleep` for `delay` before doing anything else — used
    /// with `#[tokio::test(start_paused = true)]` to hold a build open
    /// while a dependency's startup runs alongside it.
    fn with_build_delay(self, tag: &str, delay: std::time::Duration) -> Self {
        self.build_delays
            .lock()
            .unwrap()
            .insert(tag.to_string(), delay);
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

    /// Makes `wait_for_container_exit` for dependency `name` resolve after
    /// `delay` with `exit_code` — simulates the dependency dying on its
    /// own after becoming ready, instead of the default (never resolving,
    /// matching a dependency that just keeps running until cleanup stops
    /// it). Used with `#[tokio::test(start_paused = true)]` to prove this
    /// is detected while something else (the task's own command, another
    /// dependency's own health/setup wait) is still going, not just after
    /// everything else already finished.
    fn with_dependency_exit(self, name: &str, delay: std::time::Duration, exit_code: i64) -> Self {
        self.dependency_exits
            .lock()
            .unwrap()
            .insert(format!("sidecar-id-{name}"), (delay, exit_code));
        self
    }

    /// What that container's `container_output` returns — keyed the same
    /// way `with_dependency_exit` keys its own map.
    fn with_container_output(self, name: &str, output: &str) -> Self {
        self.container_outputs
            .lock()
            .unwrap()
            .insert(format!("sidecar-id-{name}"), output.to_string());
        self
    }

    /// The [`crate::container_spec::ContainerSpec`] a prior `run_container`/
    /// `start_background_container` call for `name` was given — the last
    /// one, if called more than once. Every `_for` accessor below projects
    /// one part of this out; see `CapturedSpecs`'s own doc comment.
    fn spec_for(&self, name: &str) -> Option<crate::container_spec::ContainerSpec> {
        self.specs
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, spec)| spec.clone())
    }

    /// The `environment` a prior `run_container`/`start_background_container`
    /// call for `name` was given (flattened: `None` covers both "never
    /// called" and "called with no environment").
    fn environment_for(&self, name: &str) -> Option<HashMap<String, String>> {
        self.spec_for(name).and_then(|spec| spec.shared.environment)
    }

    /// The [`crate::container_spec::BuildSpec`] a prior `build_image` call
    /// for `tag` was given. Every `_for` accessor below projects one part
    /// of this out, the same way `spec_for` does for `run_container`/
    /// `start_background_container` — see `CapturedBuildSpecs`'s own doc
    /// comment.
    fn build_spec_for(&self, tag: &str) -> Option<crate::container_spec::BuildSpec> {
        self.build_specs.lock().unwrap().get(tag).cloned()
    }

    /// The `build_args` a prior `build_image` call for `tag` was given
    /// (flattened, same convention as `environment_for`).
    fn build_args_for(&self, tag: &str) -> Option<HashMap<String, String>> {
        self.build_spec_for(tag).and_then(|spec| spec.build_args)
    }

    /// The `(dockerfile, target)` a prior `build_image` call for `tag`
    /// was given.
    fn build_options_for(&self, tag: &str) -> Option<(String, Option<String>)> {
        self.build_spec_for(tag)
            .map(|spec| (spec.dockerfile, spec.target))
    }

    /// The `buildkit` a prior `build_image` call for `tag` was given
    /// (flattened, same convention as `environment_for`).
    fn buildkit_options_for(&self, tag: &str) -> Option<crate::docker::BuildKitOptions> {
        self.build_spec_for(tag).and_then(|spec| spec.buildkit)
    }

    /// The `force_pull` a prior `build_image` call for `tag` was given.
    fn force_pull_for(&self, tag: &str) -> Option<bool> {
        self.build_spec_for(tag).map(|spec| spec.force_pull)
    }

    /// The `image` a prior `run_container`/`start_background_container`
    /// call for `name` was given.
    fn image_for(&self, name: &str) -> Option<String> {
        self.spec_for(name).map(|spec| spec.shared.image)
    }

    /// The `command` a prior `start_background_container` call for
    /// `name` was given (flattened, same convention as
    /// `environment_for`).
    fn command_for(&self, name: &str) -> Option<String> {
        self.spec_for(name).and_then(|spec| spec.shared.command)
    }

    /// The `interactive` a prior `run_container` call for `name` was
    /// given.
    fn interactive_for(&self, name: &str) -> Option<bool> {
        self.spec_for(name).map(|spec| spec.interactive)
    }

    /// The `(uid, gid, home_directory, cache_directories)` a prior
    /// `run_container`/`start_background_container` call for `name` was
    /// given (flattened, same convention as `environment_for`).
    fn user_mapping_for(&self, name: &str) -> Option<(u32, u32, String, Vec<String>)> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.user_mapping)
            .map(|m| {
                (
                    m.user.uid,
                    m.user.gid,
                    m.home_directory,
                    m.cache_directories,
                )
            })
    }

    /// The `(additional_hostnames, additional_hosts)` a prior
    /// `run_container`/`start_background_container` call for `name` was
    /// given.
    fn network_options_for(&self, name: &str) -> Option<NetworkOptionsValue> {
        self.spec_for(name).map(|spec| {
            (
                spec.shared.network_options.additional_hostnames,
                spec.shared.network_options.additional_hosts,
                spec.shared.network_options.ports,
            )
        })
    }

    /// The `proxy_host_gateway` a prior `run_container`/
    /// `start_background_container` call for `name` was given (flattened,
    /// same convention as `environment_for`).
    fn host_gateway_for(&self, name: &str) -> Option<crate::proxy::HostGateway> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.network_options.proxy_host_gateway)
    }

    /// The `proxy_host_gateway` a prior `build_image` call for `tag` was
    /// given (flattened, same convention as `environment_for`).
    fn build_host_gateway_for(&self, tag: &str) -> Option<crate::proxy::HostGateway> {
        self.build_spec_for(tag)
            .and_then(|spec| spec.proxy_host_gateway)
    }

    /// The `health_check` a prior `run_container`/
    /// `start_background_container` call for `name` was given
    /// (flattened, same convention as `environment_for`).
    fn health_check_for(&self, name: &str) -> Option<crate::docker::HealthCheckOptions> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.health_check)
    }

    /// The `(working_directory, environment, (uid, gid))` a prior
    /// `exec_in_container` call for `command` was given.
    fn exec_for(&self, command: &str) -> Option<ExecValue> {
        self.execs.lock().unwrap().get(command).cloned()
    }

    /// The `stop_signal`/`stop_grace_period` a prior `stop_and_remove_container`
    /// call for `container_id` was given (ratect#112).
    fn stop_options_for(&self, container_id: &str) -> Option<StopOptionsValue> {
        self.stop_options.lock().unwrap().get(container_id).cloned()
    }

    /// The `options.working_directory` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn working_directory_for(&self, name: &str) -> Option<String> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.working_directory)
    }

    /// The `options.entrypoint` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn entrypoint_for(&self, name: &str) -> Option<String> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.entrypoint)
    }

    /// The labels a prior `create_network` call was given.
    fn network_labels(&self) -> Option<HashMap<String, String>> {
        self.network_labels.lock().unwrap().clone()
    }

    /// The `labels` a prior `run_container`/`start_background_container`
    /// call for `name` was given.
    fn labels_for(&self, name: &str) -> Option<HashMap<String, String>> {
        self.spec_for(name).map(|spec| spec.labels)
    }

    /// The `options.capabilities_to_add` a prior
    /// `run_container`/`start_background_container` call for `name` was
    /// given.
    fn capabilities_to_add_for(&self, name: &str) -> Option<Vec<String>> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.capabilities_to_add)
    }

    /// The `options.capabilities_to_drop` a prior
    /// `run_container`/`start_background_container` call for `name` was
    /// given.
    fn capabilities_to_drop_for(&self, name: &str) -> Option<Vec<String>> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.capabilities_to_drop)
    }

    /// The `options.privileged` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn privileged_for(&self, name: &str) -> Option<bool> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.privileged)
    }

    /// The `options.shm_size` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn shm_size_for(&self, name: &str) -> Option<i64> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.shm_size)
    }

    /// The `options.devices` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn devices_for(&self, name: &str) -> Option<Vec<(String, String, Option<String>)>> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.devices)
    }

    /// The `options.enable_init_process` a prior
    /// `run_container`/`start_background_container` call for `name`
    /// was given.
    fn enable_init_process_for(&self, name: &str) -> Option<bool> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.enable_init_process)
    }

    /// The `options.log_driver` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn log_driver_for(&self, name: &str) -> Option<String> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.log_driver)
    }

    /// The `options.log_options` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn log_options_for(&self, name: &str) -> Option<HashMap<String, String>> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.log_options)
    }

    /// The `options.tmpfs` a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn tmpfs_for(&self, name: &str) -> Option<Vec<(String, String)>> {
        self.spec_for(name)
            .and_then(|spec| spec.shared.options.tmpfs)
    }

    /// The bind strings a prior `run_container`/
    /// `start_background_container` call for `name` was given.
    fn volumes_for(&self, name: &str) -> Option<Vec<String>> {
        self.spec_for(name).and_then(|spec| spec.shared.volumes)
    }
}

#[async_trait::async_trait]
impl ContainerRuntime for FakeContainerRuntime {
    async fn pull_image(&self, image: &str) -> Result<()> {
        let delay = self.pull_delays.lock().unwrap().get(image).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if self.failing_pulls.lock().unwrap().contains(image) {
            anyhow::bail!("manifest for {image} not found");
        }
        self.push(format!("pull:{image}"));
        Ok(())
    }

    async fn image_exists_locally(&self, image: &str) -> Result<bool> {
        Ok(self.locally_present_images.lock().unwrap().contains(image))
    }

    async fn build_image(&self, spec: &crate::container_spec::BuildSpec) -> Result<String> {
        let delay = self.build_delays.lock().unwrap().get(&spec.tag).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.build_specs
            .lock()
            .unwrap()
            .insert(spec.tag.clone(), spec.clone());
        self.push(format!(
            "build:{}:{}",
            spec.tag,
            spec.build_directory.display()
        ));
        if *self.fail_image_build.lock().unwrap() {
            anyhow::bail!("the daemon said no");
        }
        // Real Docker returns an image ID distinct from the tag; the fake
        // has no such concept, so it just echoes the tag back — tests
        // that assert `image_for(name) == tag` still hold either way.
        Ok(spec.tag.clone())
    }

    async fn tag_image(&self, image_id: &str, tag: &str) -> Result<()> {
        self.push(format!("tag:{image_id}:{tag}"));
        if self.rejected_tags.lock().unwrap().contains(tag) {
            anyhow::bail!("the daemon refused '{tag}'");
        }
        Ok(())
    }

    async fn create_network(&self, name: &str, labels: &HashMap<String, String>) -> Result<()> {
        if *self.fail_network_creation.lock().unwrap() {
            anyhow::bail!("could not find an available, non-overlapping IPv4 address pool");
        }
        self.push(format!("network-create:{name}"));
        *self.network_labels.lock().unwrap() = Some(labels.clone());
        Ok(())
    }

    async fn network_exists(&self, name: &str) -> Result<bool> {
        self.push(format!("network-exists:{name}"));
        Ok(*self.network_exists_result.lock().unwrap())
    }

    async fn create_container(
        &self,
        spec: &crate::container_spec::ContainerSpec,
    ) -> std::result::Result<String, crate::docker::CreateContainerError> {
        let name = &spec.shared.name;
        let delay = self.create_delays.lock().unwrap().get(name).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        // Before anything is recorded: nothing was created (see
        // `failing_creation`).
        if self.failing_creations.lock().unwrap().contains(name) {
            self.push(format!("create-failed:{name}"));
            return Err(crate::docker::CreateContainerError::before_creation(
                anyhow::anyhow!("Failed to create container '{name}'"),
            ));
        }
        self.specs
            .lock()
            .unwrap()
            .push((name.clone(), spec.clone()));
        self.push(format!(
            "create:{name}:{}",
            network_label(&spec.shared.network)
        ));
        // One id convention for a dependency and the task's own container,
        // so `with_unhealthy_container`/exec-based assertions work
        // identically for either — and so the engine's cleanup of either
        // shows up as the same `sidecar-stop:sidecar-id-{name}` event,
        // which is what the cleanup-flag tests assert on.
        let container_id = format!("sidecar-id-{name}");
        if self
            .failing_creations_after_docker_created
            .lock()
            .unwrap()
            .contains(name)
        {
            return Err(crate::docker::CreateContainerError {
                created: Some(container_id),
                error: anyhow::anyhow!("Failed to connect '{name}' to network"),
            });
        }
        Ok(container_id)
    }

    async fn start_background_container(
        &self,
        container_id: &str,
        spec: &crate::container_spec::ContainerSpec,
    ) -> Result<()> {
        let alias = &spec.shared.name;
        assert_eq!(
            container_id,
            format!("sidecar-id-{alias}"),
            "a container is started by the id its own creation returned"
        );
        let delay = self.start_delays.lock().unwrap().get(alias).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if self.failing_starts.lock().unwrap().contains(alias) {
            anyhow::bail!("Failed to start container '{alias}'");
        }
        self.push(format!(
            "sidecar-start:{alias}:{}",
            network_label(&spec.shared.network)
        ));
        Ok(())
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
        if self.gate_finds_stopped(container_id) {
            anyhow::bail!("The container exited before becoming healthy.");
        }
        if self.unhealthy_container.lock().unwrap().as_deref() == Some(container_id) {
            anyhow::bail!(
                "The configured health check did not indicate that the container was \
                     healthy within the timeout period."
            );
        }
        Ok(())
    }

    async fn container_output(&self, container_id: &str) -> Result<String> {
        Ok(self
            .container_outputs
            .lock()
            .unwrap()
            .get(container_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn container_is_running(&self, container_id: &str) -> Result<bool> {
        if let Some(answer) = self.running_probe.lock().unwrap().clone() {
            return answer.map_err(|message| anyhow::anyhow!(message));
        }
        Ok(!self
            .exited_containers
            .lock()
            .unwrap()
            .contains(container_id))
    }

    async fn wait_for_container_exit(&self, container_id: &str) -> Result<i64> {
        let configured = self
            .dependency_exits
            .lock()
            .unwrap()
            .get(container_id)
            .copied();
        match configured {
            Some((delay, exit_code)) => {
                tokio::time::sleep(delay).await;
                Ok(exit_code)
            }
            // Never exits on its own — matching a real container that just
            // keeps running until something else stops it. Aborted by the
            // engine rather than ever resolved, for every dependency this
            // fake wasn't told to make exit.
            None => std::future::pending().await,
        }
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
        if self.gate_finds_stopped(container_id) {
            anyhow::bail!(
                "Docker responded with status code 409: container {container_id} is not running"
            );
        }
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

    async fn run_container(
        &self,
        container_id: &str,
        spec: &crate::container_spec::ContainerSpec,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        let name = &spec.shared.name;
        assert_eq!(
            container_id,
            format!("sidecar-id-{name}"),
            "the task container is run by the id its own creation returned"
        );
        self.push(format!(
            "run:{name}:{}:args=[{}]:{}",
            spec.shared.command.as_deref().unwrap_or_default(),
            spec.additional_args.join(","),
            network_label(&spec.shared.network)
        ));
        // Fired before `run_delays`' own sleep (if any), so a concurrent
        // readiness task genuinely overlaps with this call still being in
        // flight, the same way it would against a real, still-running
        // container.
        if let Some(started) = started {
            let _ = started.send(());
        }
        let delay = self.run_delays.lock().unwrap().get(name).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.exited_containers
            .lock()
            .unwrap()
            .insert(format!("sidecar-id-{name}"));
        let drain = self.run_drain_delays.lock().unwrap().get(name).copied();
        if let Some(drain) = drain {
            tokio::time::sleep(drain).await;
        }
        let exit_code = match self.run_exits.lock().unwrap().get(name) {
            Some(&exit_code) => exit_code,
            None if *self.fail_run.lock().unwrap() => 1,
            None => 0,
        };
        if exit_code != 0 {
            return Err(crate::docker::ContainerExitedNonZero { exit_code }.into());
        }
        Ok(())
    }

    async fn remove_container_forcibly(&self, container_id: &str) -> Result<()> {
        self.signal_from_removal().await;
        // Always in flight for at least one poll, signal or not: the engine
        // issues forced removals together, and a real one is a daemon
        // round-trip, so a signal landing on one of them finds the others
        // pending too — and dropped with it — rather than already done.
        tokio::task::yield_now().await;
        self.refuses_removal_of(container_id)?;
        self.push(format!("force-remove:{container_id}"));
        Ok(())
    }
}

#[async_trait::async_trait]
impl ResourceInventory for FakeContainerRuntime {
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

    async fn remove_network(&self, name: &str) -> Result<()> {
        if *self.refuse_network_removal.lock().unwrap() {
            anyhow::bail!("error while removing network: network {name} has active endpoints");
        }
        self.push(format!("network-remove:{name}"));
        Ok(())
    }

    async fn stop_and_remove_container(
        &self,
        container_id: &str,
        stop_signal: Option<&str>,
        stop_grace_period: Option<std::time::Duration>,
    ) -> Result<()> {
        self.stop_options.lock().unwrap().insert(
            container_id.to_string(),
            (stop_signal.map(str::to_string), stop_grace_period),
        );
        self.signal_from_removal().await;
        self.refuses_removal_of(container_id)?;
        self.push(format!("sidecar-stop:{container_id}"));
        Ok(())
    }
}

#[async_trait::async_trait]
impl VolumeStore for FakeContainerRuntime {
    // Neither is ever reached through `TaskEngine` (only `--clean`/
    // `--clean-cache` in `main.rs` call these, directly against a real
    // `DockerClient`) — trivial stubs only to satisfy the trait.
    // `crate::cache`'s own tests cover the actual cleanup logic against
    // plain `Vec<String>` fixtures instead, not this fake.
    async fn list_volumes(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn remove_volume(&self, _name: &str) -> Result<()> {
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
        run_to_completion: None,
        external_health_check: None,
        reports_readiness_for: None,
        stop_signal: None,
        stop_grace_period: None,
        ulimits: None,
        dns: None,
        dns_search: None,
        dns_options: None,
        network_mode: None,
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
    let mut containers = IndexMap::new();
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
            run_to_completion: None,
            external_health_check: None,
            reports_readiness_for: None,
            stop_signal: None,
            stop_grace_period: None,
            ulimits: None,
            dns: None,
            dns_search: None,
            dns_options: None,
            network_mode: None,
        },
    );

    let mut tasks = IndexMap::new();
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
        containers: IndexMap::new(),
        tasks: IndexMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// Mirrors the diamond-shaped dependency graph in the sample `batect.yml`:
/// two tasks share a common prerequisite, and a final task depends on both.
fn config_with_shared_prerequisite() -> Config {
    let mut containers = IndexMap::new();
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
            run_to_completion: None,
            external_health_check: None,
            reports_readiness_for: None,
            stop_signal: None,
            stop_grace_period: None,
            ulimits: None,
            dns: None,
            dns_search: None,
            dns_options: None,
            network_mode: None,
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

    let mut tasks = IndexMap::new();
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

/// How the fake's event strings spell a container's network: a task
/// network by its bare name, so every pre-ratect#106 assertion still reads
/// as it did, and a Docker network mode as Docker's own `--network` would.
fn network_label(network: &crate::container_spec::ContainerNetwork) -> String {
    use crate::container_spec::ContainerNetwork;
    match network {
        ContainerNetwork::Task(name) => name.clone(),
        ContainerNetwork::Host => "host".to_string(),
        ContainerNetwork::None => "none".to_string(),
        ContainerNetwork::Container(id) => format!("container:{id}"),
    }
}

/// [`TaskEngine::new`] with the two defaults almost every test here wants —
/// a silent event sink, and an interrupt tracker nothing will ever record
/// on (behaviourally identical to "no tracker", see `TaskEngine::interrupt`'s
/// own doc comment). A test that cares about either constructs a
/// `TaskEngine::new` directly instead, same as it would have called
/// `with_event_sink`/`with_interrupt` before those became constructor
/// arguments.
/// `config` as a project declared entirely in `/project`, as an engine
/// takes it — every value unevaluated, as loading leaves it.
fn project(config: Config) -> crate::config::LoadedProject {
    crate::config::LoadedProject::in_memory(config, PathBuf::from("/project"))
}

fn engine<D: ContainerRuntime + Send + Sync>(config: Config, docker: D) -> TaskEngine<D> {
    TaskEngine::new(
        project(config),
        docker,
        Arc::new(NullEventSink),
        crate::interrupt::Interrupt::new(),
    )
}

#[tokio::test]
async fn shared_prerequisite_runs_once_and_image_pulled_once() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(config_with_shared_prerequisite(), docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));

    let mut tasks = IndexMap::new();
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
    let engine = engine(config_with_wildcard_prerequisite_tasks(), docker.clone());

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

/// Batect's CL1-16: literal prerequisites run in the order they're listed —
/// only a wildcard's own matches are sorted. Listed out of alphabetical
/// order here so sorting would be caught.
#[tokio::test]
async fn literal_prerequisites_run_in_declared_order() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("alpha".to_string(), task("build-env", "alpha"));
    tasks.insert("zeta".to_string(), task("build-env", "zeta"));
    let mut ci_task = task("build-env", "ci");
    ci_task.prerequisites = Some(vec!["zeta".to_string(), "alpha".to_string()]);
    tasks.insert("ci".to_string(), ci_task);
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("ci", &[]).await.unwrap();

    let events = docker.events();
    let commands: Vec<&str> = events
        .iter()
        .filter_map(|e| e.strip_prefix("run:build-env:"))
        .filter_map(|rest| rest.split(':').next())
        .collect();
    assert_eq!(commands, vec!["zeta", "alpha", "ci"], "{events:?}");
}

#[tokio::test]
async fn wildcard_prerequisite_matching_no_tasks_is_not_an_error() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

    let err = engine.run_task("ci", &[]).await.unwrap_err();
    assert!(err.to_string().contains("Task 'does-not-exist' not found"));
}

#[tokio::test]
async fn wildcard_pattern_with_multiple_asterisks_matches() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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

    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let engine = engine(config_with_shared_prerequisite(), docker.clone());

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
    let engine = engine(config_with_shared_prerequisite(), docker.clone())
        .with_settings(TaskEngineSettings {
            run_prerequisites: false,
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config_with_shared_prerequisite(), docker.clone())
        .with_settings(TaskEngineSettings {
            run_prerequisites: false,
            ..Default::default()
        })
        .unwrap();

    engine.run_task("prereq-task", &[]).await.unwrap();

    let events = docker.events();
    let runs: Vec<_> = events.iter().filter(|e| e.starts_with("run:")).collect();
    assert_eq!(runs.len(), 1, "events: {events:?}");
    assert!(runs[0].starts_with("run:build-env:prereq-task:args=[]:"));
}

#[tokio::test]
async fn only_the_top_level_tasks_own_container_run_is_interactive_eligible() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("run", &[]).await.unwrap();

    assert_eq!(
        docker.interactive_for("app"),
        Some(true),
        "the task actually named on the command line is interactive-eligible"
    );
}

#[tokio::test]
async fn prerequisite_tasks_own_container_is_never_interactive() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    containers.insert("setup".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
        run_to_completion: None,
        external_health_check: None,
        reports_readiness_for: None,
        stop_signal: None,
        stop_grace_period: None,
        ulimits: None,
        dns: None,
        dns_search: None,
        dns_options: None,
        network_mode: None,
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container);
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cache: Some((crate::cache::CacheType::Volume, project.clone())),
            ..Default::default()
        })
        .unwrap();
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container);
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cache: Some((crate::cache::CacheType::Volume, project.clone())),
            ..Default::default()
        })
        .unwrap();
    engine.run_task("run", &[]).await.unwrap();
    std::fs::remove_dir_all(&project).ok();

    let (_, _, _, cache_directories) = docker
        .user_mapping_for("app")
        .expect("run_as_current_user should have produced a mapping");
    assert_eq!(cache_directories, vec!["/cache".to_string()]);
}

#[tokio::test]
async fn run_as_current_user_reaches_the_container() {
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container_with_run_as_current_user("alpine:3.18", None, "/home/container-user"),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert(
        "database".to_string(),
        container_with_run_as_current_user("alpine:3.18", None, "/home/container-user"),
    );
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container_with_ports("alpine:3.18", vec![single_port(8080, 80, "tcp")]),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("run", &[]).await.unwrap();

    let (_, _, ports) = docker.network_options_for("app").unwrap();
    assert_eq!(ports, Some(vec![(8080, 80, "tcp".to_string())]));
}

#[tokio::test]
async fn task_run_ports_are_added_to_the_containers_own_ports() {
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container_with_ports("alpine:3.18", vec![single_port(8080, 80, "tcp")]),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

    engine.run_task("run", &[]).await.unwrap();

    let (_, _, ports) = docker.network_options_for("app").unwrap();
    let ports = ports.unwrap();
    assert!(ports.contains(&(8080, 80, "tcp".to_string())));
    assert!(ports.contains(&(9090, 90, "tcp".to_string())));
}

#[tokio::test]
async fn disable_port_publishing_suppresses_configured_ports() {
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container_with_ports("alpine:3.18", vec![single_port(8080, 80, "tcp")]),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            publish_ports: false,
            ..Default::default()
        })
        .unwrap();

    engine.run_task("run", &[]).await.unwrap();

    let (_, _, ports) = docker.network_options_for("app").unwrap();
    assert_eq!(
        ports, None,
        "ports were configured but --disable-ports should suppress them"
    );
}

#[tokio::test]
async fn run_as_current_user_explicitly_disabled_reaches_the_container_with_no_mapping() {
    let mut containers = IndexMap::new();
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
            run_to_completion: None,
            external_health_check: None,
            reports_readiness_for: None,
            stop_signal: None,
            stop_grace_period: None,
            ulimits: None,
            dns: None,
            dns_search: None,
            dns_options: None,
            network_mode: None,
        },
    );
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
        run_to_completion: None,
        external_health_check: None,
        reports_readiness_for: None,
        stop_signal: None,
        stop_grace_period: None,
        ulimits: None,
        dns: None,
        dns_search: None,
        dns_options: None,
        network_mode: None,
    }
}

#[tokio::test]
async fn build_directory_container_builds_then_runs_the_built_image() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    let events = docker.events();
    let build_event = events
        .iter()
        .find(|e| e.starts_with("build:"))
        .expect("image should have been built");
    assert!(
        build_event.ends_with(":/project/docker"),
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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    assert_eq!(docker.force_pull_for("demo-build-env"), Some(false));
}

#[tokio::test]
async fn build_directory_container_with_always_policy_force_pulls_the_base_image() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        Container {
            image_pull_policy: Some(crate::config::ImagePullPolicy::Always),
            ..container_with_build_directory("./docker", None)
        },
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    assert_eq!(docker.force_pull_for("demo-build-env"), Some(true));
}

#[tokio::test]
async fn build_directory_container_passes_dockerfile_and_target_through() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        Container {
            dockerfile: Some("docker/Dockerfile.prod".to_string()),
            build_target: Some("builder".to_string()),
            ..container_with_build_directory("./docker", None)
        },
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    let (dockerfile, target) = docker
        .build_options_for("demo-build-env")
        .expect("build_image should have been called for the built container's tag");
    assert_eq!(dockerfile, "Dockerfile");
    assert_eq!(target, None);
}

#[tokio::test]
async fn build_directory_container_without_secrets_or_ssh_skips_buildkit() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("build", &[]).await.unwrap();

    assert_eq!(docker.buildkit_options_for("demo-build-env"), None);
}

#[tokio::test]
async fn build_directory_container_passes_secrets_and_ssh_through_as_buildkit_options() {
    // A file secret has to exist by the time the build runs (#315).
    let cert = std::env::temp_dir().join(format!("ratect-cert-{}", uuid::Uuid::new_v4()));
    std::fs::write(&cert, "cert").unwrap();
    let mut containers = IndexMap::new();
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
                    BuildSecret::Path(cert.display().to_string()),
                ),
            ])),
            build_ssh: Some(vec![crate::config::SshAgent {
                id: "default".to_string(),
                paths: Vec::new(),
            }]),
            ..container_with_build_directory("./docker", None)
        },
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    let result = engine.run_task("build", &[]).await;
    std::fs::remove_file(&cert).unwrap();
    result.unwrap();

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
        Some(&crate::docker::BuildSecretSource::File(cert))
    );
}

/// #315: a file secret that doesn't exist fails the build before Docker is
/// asked to build anything — whether or not the Dockerfile mounts it, which
/// the fake can't see and Batect doesn't consult — attributed to the
/// container whose entry it is.
#[tokio::test]
async fn a_missing_build_secret_file_fails_before_the_image_is_built() {
    let missing = std::env::temp_dir().join(format!("ratect-315-{}", uuid::Uuid::new_v4()));
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        Container {
            build_secrets: Some(HashMap::from([(
                "cert".to_string(),
                BuildSecret::Path(missing.display().to_string()),
            )])),
            ..container_with_build_directory("./docker", None)
        },
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    let err = engine.run_task("build", &[]).await.unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("Failed to build the image for container 'build-env'"),
        "{message}"
    );
    assert!(
        message.contains(&format!(
            "The 'build_secrets' entry 'cert' reads '{}', which does not exist",
            missing.display()
        )),
        "{message}"
    );
    assert_eq!(docker.build_spec_for("demo-build-env"), None);
}

/// The other two `build_ssh` shapes reaching Docker: several agents
/// under distinct ids, one of them serving explicit key files. Without
/// this the conversion is only proven by the `#[ignore]`d real-daemon
/// test, which doesn't run in the default suite — so a regression in
/// how config becomes a `SshAgentSource` would go unnoticed there.
#[tokio::test]
async fn build_ssh_key_paths_reach_docker_as_a_named_key_source() {
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let engine = engine(config, FakeContainerRuntime::default());
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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let engine = engine(
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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", None),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", Some(build_args)),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert(
        "database".to_string(),
        container_with_build_directory("./db", None),
    );
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    let build_event = events
        .iter()
        .find(|e| e.starts_with("build:") && e.ends_with(":/project/db"))
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
    let mut containers = IndexMap::new();
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
            run_to_completion: None,
            external_health_check: None,
            reports_readiness_for: None,
            stop_signal: None,
            stop_grace_period: None,
            ulimits: None,
            dns: None,
            dns_search: None,
            dns_options: None,
            network_mode: None,
        },
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            existing_network: Some("my-network".to_string()),
            ..Default::default()
        })
        .unwrap();

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

/// `network_mode` (ratect#106) takes only the container that sets it off
/// the task's network — a `container:` target resolved to that dependency's
/// running id — while one that sets none still joins the task's network,
/// whether that is the run's own or `--use-network`'s. Readiness is gated
/// the same way in every mode: a setup command still runs, via `exec`.
#[tokio::test]
async fn network_mode_applies_to_its_own_container_alone() {
    for existing_network in [None, Some("my-network".to_string())] {
        let mut sharer = container("alpine:3.18", Some(vec!["peer".to_string()]));
        sharer.network_mode = Some(NetworkMode::Container("peer".to_string()));
        let mut isolated = container("alpine:3.18", None);
        isolated.network_mode = Some(NetworkMode::None);
        isolated.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./seed.sh".to_string(),
            working_directory: None,
            run_in: None,
        }]);
        let mut app = container(
            "alpine:3.18",
            Some(vec!["sharer".to_string(), "isolated".to_string()]),
        );
        app.network_mode = Some(NetworkMode::Host);
        let containers = IndexMap::from([
            ("peer".to_string(), container("alpine:3.18", None)),
            ("sharer".to_string(), sharer),
            ("isolated".to_string(), isolated),
            ("app".to_string(), app),
        ]);
        let tasks = IndexMap::from([("build".to_string(), task("app", "echo hi"))]);
        let config = Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
            forbid_telemetry: None,
        };

        let docker = FakeContainerRuntime::default();
        let engine = engine(config, docker.clone())
            .with_settings(TaskEngineSettings {
                existing_network: existing_network.clone(),
                ..Default::default()
            })
            .unwrap();

        engine.run_task("build", &[]).await.unwrap();

        let events = docker.events();
        let task_network = existing_network.unwrap_or_else(|| {
            events
                .iter()
                .find_map(|e| e.strip_prefix("network-create:"))
                .expect("the run's own network is created")
                .to_string()
        });
        for expected in [
            format!("sidecar-start:peer:{task_network}"),
            "sidecar-start:sharer:container:sidecar-id-peer".to_string(),
            "sidecar-start:isolated:none".to_string(),
            "exec:sidecar-id-isolated:./seed.sh".to_string(),
            "run:app:echo hi:args=[]:host".to_string(),
        ] {
            assert!(events.contains(&expected), "{expected}: {events:?}");
        }
    }
}

#[tokio::test]
async fn use_network_errors_clearly_when_the_network_does_not_exist() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().without_existing_network();
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            existing_network: Some("missing".to_string()),
            ..Default::default()
        })
        .unwrap();

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    )
    .with_settings(TaskEngineSettings {
        existing_network: Some("missing".to_string()),
        ..Default::default()
    })
    .unwrap();

    assert!(engine.run_task("build", &[]).await.is_err());

    assert_eq!(
        sink.events(),
        vec![
            TaskEvent::TaskStarting {
                task: "build".into()
            },
            TaskEvent::RunEnded {
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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    let network = events
        .iter()
        .find_map(|e| e.strip_prefix("network-create:"))
        .expect("a network should have been created")
        .to_string();

    let network_index = events
        .iter()
        .position(|e| *e == format!("network-create:{network}"))
        .expect("the network is created");
    let database_created_index = events
        .iter()
        .position(|e| *e == format!("create:database:{network}"))
        .expect("dependency should have been created, joined to the network");
    let app_created_index = events
        .iter()
        .position(|e| *e == format!("create:app:{network}"))
        .expect("main container should have been created, joined to the network");
    let sidecar_index = events
        .iter()
        .position(|e| *e == format!("sidecar-start:database:{network}"))
        .expect("dependency should have started");
    let run_index = events
        .iter()
        .position(|e| *e == format!("run:app:echo hi:args=[]:{network}"))
        .expect("main container should have run, joined to the dependency's network");
    // The total order a run has: network, then every create (each after
    // the network, before its own start), then the dependency's start
    // before the main container's. Where the main container's create
    // falls against the dependency's start is the daemon's business
    // (decisions/0019), so nothing here pins it.
    assert!(
        network_index < database_created_index && network_index < app_created_index,
        "the network must exist before anything is created on it: {events:?}"
    );
    assert!(
        database_created_index < sidecar_index,
        "a container is created before it is started: {events:?}"
    );
    assert!(
        app_created_index < run_index,
        "the main container is created before it is run: {events:?}"
    );
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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// Cleanup must forward *each* container's own `stop_signal`/
/// `stop_grace_period` to Docker — `app`'s own value, not `database`'s, and
/// vice versa — at both cleanup call sites in `run_task_internal`: the
/// task's own container and a dependency. Proves this at the unit level
/// rather than relying solely on the real-Docker `_via_docker` tests in
/// `ratect/tests/cli.rs`, which don't run in the default `cargo test`
/// suite (ratect#112).
#[tokio::test]
async fn cleanup_passes_each_containers_own_stop_signal_and_grace_period_to_docker() {
    let mut config = config_with_database_dependency(|database| {
        database.stop_grace_period = Some(std::time::Duration::from_secs(30));
    });
    config.containers.get_mut("app").unwrap().stop_signal = Some("SIGINT".to_string());

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert_eq!(
        docker.stop_options_for("sidecar-id-app"),
        Some((Some("SIGINT".to_string()), None)),
        "the task's own container should stop with its own configured stop_signal, \
         and no configured stop_grace_period"
    );
    assert_eq!(
        docker.stop_options_for("sidecar-id-database"),
        Some((None, Some(std::time::Duration::from_secs(30)))),
        "the dependency should stop with its own configured stop_grace_period, \
         and no configured stop_signal"
    );
}

/// The class of bug this proves isn't happening: a fake that can only
/// express a dependency exiting at an *expected* moment (e.g. in response
/// to cleanup) would make this untestable, since there would be no way to
/// distinguish "detected while something else was still going" from
/// "detected because the run had already finished anyway" (see AGENTS.md's
/// note on coverage shaped by the test harness). `with_dependency_exit`
/// exists so this fake can actually express the case: the dependency exits
/// mid-run, at a moment the engine did not ask for and does not expect.
#[tokio::test(start_paused = true)]
async fn dependency_exiting_after_ready_warns_while_the_task_container_still_runs() {
    let config = config_with_database_dependency(|_| {});
    let docker = FakeContainerRuntime::default()
        // Long enough that the exit below is unambiguously mid-run, not a
        // race against the task container's own (near-instant, on this
        // fake) completion.
        .with_run_delay("app", std::time::Duration::from_secs(10))
        .with_dependency_exit("database", std::time::Duration::from_secs(1), 137);
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    engine.run_task("start", &[]).await.unwrap();

    let events = sink.events();
    let warning_index = events
        .iter()
        .position(|e| {
            matches!(
                e,
                TaskEvent::DependencyExitedUnexpectedly { container, exit_code }
                    if container == "database" && *exit_code == 137
            )
        })
        .expect("the dependency's unexpected exit should have been reported");
    let finished_index = events
        .iter()
        .position(|e| matches!(e, TaskEvent::TaskFinished { .. }))
        .expect("the task should still finish");

    assert!(
        warning_index < finished_index,
        "the warning should arrive while the task container is still running, not after: {events:?}"
    );
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
                run_in: None,
            },
            crate::config::SetupCommand {
                command: "./seed-data.sh".to_string(),
                working_directory: None,
                run_in: None,
            },
        ]);
    });

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
            run_in: None,
        }]);
    });

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
            run_in: None,
        }]);
    });

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let (working_directory, _, _) = docker.exec_for("./apply-migrations.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-container"));
}

#[tokio::test]
async fn setup_command_falls_back_to_a_customised_working_directory() {
    let mut config = config_with_database_dependency(|database| {
        database.working_directory = Some("/from-container".to_string());
        database.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./apply-migrations.sh".to_string(),
            working_directory: None,
            run_in: None,
        }]);
    });
    config.tasks.get_mut("start").unwrap().customise = Some(IndexMap::from([(
        "database".to_string(),
        TaskContainerCustomisation {
            environment: None,
            ports: None,
            working_directory: Some("/from-customise".to_string()),
        },
    )]));

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let (working_directory, _, _) = docker.exec_for("./apply-migrations.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-customise"));
}

#[tokio::test]
async fn setup_commands_own_working_directory_overrides_the_containers() {
    let config = config_with_database_dependency(|database| {
        database.working_directory = Some("/from-container".to_string());
        database.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./apply-migrations.sh".to_string(),
            working_directory: Some("/from-setup-command".to_string()),
            run_in: None,
        }]);
    });

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let (working_directory, _, _) = docker.exec_for("./apply-migrations.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-setup-command"));
}

/// `run_in` (ratect#111): the declaring container's readiness gate runs the
/// command, but Docker's `exec` targets the named dependency's container —
/// the whole point being that the tooling lives in a different image.
#[tokio::test]
async fn a_setup_command_with_run_in_execs_into_the_named_dependency() {
    let mut config = config_with_database_dependency(|database| {
        database.dependencies = Some(vec!["seed-client".to_string()]);
        database.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./seed.sh".to_string(),
            working_directory: None,
            run_in: Some("seed-client".to_string()),
        }]);
    });
    config.containers.insert(
        "seed-client".to_string(),
        container("my-org/db-tools", None),
    );

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    assert!(
        events
            .iter()
            .any(|e| e == "exec:sidecar-id-seed-client:./seed.sh"),
        "the setup command should have run in 'seed-client', not 'database': {events:?}"
    );

    // Still ordered by the *declaring* container's gate: its own health
    // check comes first, and the task container only starts afterwards.
    let ordered_positions: Vec<usize> = [
        "sidecar-start:seed-client:",
        "wait-healthy:sidecar-id-database",
        "exec:sidecar-id-seed-client:./seed.sh",
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
}

/// The declaring container's environment, user and working directory are
/// deliberately *not* forwarded to a `run_in` target — Docker's `exec`
/// inherits all three from whichever container it runs in, and the
/// declaring container's would be the wrong ones to send. The setup
/// command's own `working_directory` still applies.
#[tokio::test]
async fn a_run_in_setup_command_does_not_carry_the_declaring_containers_environment() {
    let mut config = config_with_database_dependency(|database| {
        let mut environment = HashMap::new();
        environment.insert("POSTGRES_PASSWORD".to_string(), "secret".to_string());
        database.environment = Some(environment);
        database.working_directory = Some("/from-database".to_string());
        database.dependencies = Some(vec!["seed-client".to_string()]);
        database.setup_commands = Some(vec![
            crate::config::SetupCommand {
                command: "./seed.sh".to_string(),
                working_directory: None,
                run_in: Some("seed-client".to_string()),
            },
            crate::config::SetupCommand {
                command: "./seed-elsewhere.sh".to_string(),
                working_directory: Some("/from-setup-command".to_string()),
                run_in: Some("seed-client".to_string()),
            },
        ]);
    });
    config.containers.insert(
        "seed-client".to_string(),
        container("my-org/db-tools", None),
    );

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let (working_directory, environment, _) = docker.exec_for("./seed.sh").unwrap();
    assert_eq!(working_directory, None);
    assert_eq!(environment, None);
    let (working_directory, _, _) = docker.exec_for("./seed-elsewhere.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-setup-command"));
}

/// `run_in` naming the declaring container itself is what omitting it means,
/// so both spellings must behave identically — including carrying that
/// container's own environment and working directory, which a command
/// running elsewhere does not.
#[tokio::test]
async fn run_in_naming_the_declaring_container_behaves_exactly_like_omitting_it() {
    let config = config_with_database_dependency(|database| {
        let mut environment = HashMap::new();
        environment.insert("POSTGRES_PASSWORD".to_string(), "secret".to_string());
        database.environment = Some(environment);
        database.working_directory = Some("/from-database".to_string());
        database.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./apply-migrations.sh".to_string(),
            working_directory: None,
            run_in: Some("database".to_string()),
        }]);
    });

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert!(
        docker
            .events()
            .iter()
            .any(|e| e == "exec:sidecar-id-database:./apply-migrations.sh"),
        "expected the exec to target the declaring container: {:?}",
        docker.events()
    );
    let (working_directory, environment, _) = docker.exec_for("./apply-migrations.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-database"));
    assert_eq!(
        environment
            .unwrap()
            .get("POSTGRES_PASSWORD")
            .map(String::as_str),
        Some("secret")
    );
}

/// A failing `run_in` setup command names both containers: the one it ran
/// in, and the one whose readiness gate declared it. Naming only the
/// declaring container would point at the wrong place to go looking.
#[tokio::test]
async fn a_failing_run_in_setup_command_names_both_containers() {
    let mut config = config_with_database_dependency(|database| {
        database.dependencies = Some(vec!["seed-client".to_string()]);
        database.setup_commands = Some(vec![crate::config::SetupCommand {
            command: "./seed.sh".to_string(),
            working_directory: None,
            run_in: Some("seed-client".to_string()),
        }]);
    });
    config.containers.insert(
        "seed-client".to_string(),
        container("my-org/db-tools", None),
    );

    let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed.sh");
    let engine = engine(config, docker.clone());

    let err = engine.run_task("start", &[]).await.unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("in container 'seed-client' (a setup command of container 'database')"),
        "got: {message}"
    );
}

/// The task's own container can use `run_in` too — its setup commands run
/// through the same gate, and its dependencies are equally guaranteed ready
/// by the time they do.
#[tokio::test(start_paused = true)]
async fn the_task_containers_own_run_in_setup_command_execs_into_its_dependency() {
    let mut config = config_with_database_dependency(|_| {});
    config.containers.get_mut("app").unwrap().setup_commands =
        Some(vec![crate::config::SetupCommand {
            command: "./warm-cache.sh".to_string(),
            working_directory: None,
            run_in: Some("database".to_string()),
        }]);

    // The gate runs alongside the main command and is dropped the moment
    // the command exits (decisions/0012), so the command has to outlive it
    // for the gate to do anything — as a real one does.
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert!(
        docker
            .events()
            .iter()
            .any(|e| e == "exec:sidecar-id-database:./warm-cache.sh"),
        "expected the exec to target 'database': {:?}",
        docker.events()
    );
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
    let engine = engine(config, docker.clone());

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

/// Two sibling dependencies of `app`, `peer` and `fails`, for the
/// cut-short-start tests below (ratect#224).
fn config_with_sibling_dependencies() -> Config {
    let mut containers = IndexMap::new();
    containers.insert("peer".to_string(), container("alpine:3.18", None));
    containers.insert("fails".to_string(), container("alpine:3.18", None));
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["peer".to_string(), "fails".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// Asserts `container`'s dependency container was stopped and removed, and
/// before the network — a container still attached when the network goes is
/// what makes its removal fail with "has active endpoints".
fn assert_removed_before_network(events: &[String], container: &str) {
    let stop = events
        .iter()
        .position(|e| e == &format!("sidecar-stop:sidecar-id-{container}"))
        .unwrap_or_else(|| panic!("'{container}' must be cleaned up: {events:?}"));
    let network = events
        .iter()
        .position(|e| e.starts_with("network-remove:"))
        .unwrap_or_else(|| panic!("the network must be removed: {events:?}"));
    assert!(
        stop < network,
        "'{container}' must be removed before the network: {events:?}"
    );
}

/// A sibling whose start is abandoned because another dependency failed is
/// still removed, as long as Docker had created it: `peer` is created at
/// once, `fails` is reported unhealthy a second later, and `peer`'s start
/// would only have returned after a minute.
#[tokio::test(start_paused = true)]
async fn a_dependency_created_before_a_sibling_fails_is_still_cleaned_up() {
    let docker = FakeContainerRuntime::default()
        .with_start_delay("peer", std::time::Duration::from_secs(60))
        .with_health_check_delay("fails", std::time::Duration::from_secs(1))
        .with_unhealthy_container("fails");
    let engine = engine(config_with_sibling_dependencies(), docker.clone());

    let result = engine.run_task("start", &[]).await;

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("'fails' did not become healthy"),
        "the run must report the failing sibling: {message}"
    );
    let events = docker.events();
    assert_removed_before_network(&events, "peer");
    assert_removed_before_network(&events, "fails");
}

/// A dependency whose own start fails after Docker created it is removed,
/// and the start failure is what the run reports.
#[tokio::test]
async fn a_dependency_whose_start_fails_after_creation_is_still_cleaned_up() {
    let docker = FakeContainerRuntime::default().failing_start("fails");
    let engine = engine(config_with_sibling_dependencies(), docker.clone());

    let result = engine.run_task("start", &[]).await;

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("Failed to start container 'fails'"),
        "the run must report the start failure: {message}"
    );
    assert_removed_before_network(&docker.events(), "fails");
}

/// Creation is more than Docker's `create`: the user-mapping upload and the
/// network join come after it, and a failure there leaves a container
/// behind. `CreateContainerError` carries its id out, and the engine
/// registers it like any other, so it is removed — and the failure itself
/// is what the run reports.
#[tokio::test]
async fn a_dependency_whose_creation_fails_after_docker_created_it_is_still_cleaned_up() {
    let docker = FakeContainerRuntime::default().failing_creation_after_docker_created("fails");
    let engine = engine(config_with_sibling_dependencies(), docker.clone());

    let result = engine.run_task("start", &[]).await;

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("Failed to connect 'fails' to network"),
        "the run must report the creation failure: {message}"
    );
    assert_removed_before_network(&docker.events(), "fails");
}

/// Creation doesn't wait for a dependency's readiness; only the start does
/// (decisions/0019, rule 2). `a` takes a second to become healthy; `b`
/// (which depends on it) and the task container are both *created* before
/// that verdict and *started* only after it — one assertion per container
/// per side, on the fake's own call order.
#[tokio::test(start_paused = true)]
async fn containers_are_created_before_their_dependencies_are_ready_and_started_after() {
    let docker = FakeContainerRuntime::default()
        .with_health_check_delay("a", std::time::Duration::from_secs(1));
    let engine = engine(
        config_with_a_dependency_chain_of_three_pulls(),
        docker.clone(),
    );

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    let position = |prefix: &str| {
        events
            .iter()
            .position(|e| e.starts_with(prefix))
            .unwrap_or_else(|| panic!("no '{prefix}' event: {events:?}"))
    };
    let a_healthy = position("wait-healthy:sidecar-id-a");
    assert!(
        position("create:b:") < a_healthy,
        "'b' is created before 'a' is healthy: {events:?}"
    );
    assert!(
        a_healthy < position("sidecar-start:b:"),
        "'b' is started only once 'a' is healthy: {events:?}"
    );
    assert!(
        position("create:app:") < a_healthy,
        "the task container is created before 'a' is healthy: {events:?}"
    );
    assert!(
        a_healthy < position("run:app:"),
        "the task container is run only once 'a' is healthy: {events:?}"
    );
}

/// A dependency whose create fails — a spec the daemon refuses — ends the
/// run with that error the moment it lands: no dependency's setup command
/// has run, and every container created so far is removed before the
/// network. `peer` and the task container are created at once; `fails`'s
/// create takes a second to be refused, while `peer` is still waiting to
/// become healthy, so its setup command never runs. Nothing was created for
/// `fails`, so nothing of its is removed.
#[tokio::test(start_paused = true)]
async fn a_dependency_whose_create_fails_stops_the_run_before_any_setup_command() {
    let mut config = config_with_sibling_dependencies();
    config.containers.get_mut("peer").unwrap().setup_commands =
        Some(vec![crate::config::SetupCommand {
            command: "./seed.sh".to_string(),
            working_directory: None,
            run_in: None,
        }]);
    let docker = FakeContainerRuntime::default()
        .with_create_delay("fails", std::time::Duration::from_secs(1))
        .failing_creation("fails")
        .with_health_check_delay("peer", std::time::Duration::from_secs(2));
    let engine = engine(config, docker.clone());

    let result = engine.run_task("start", &[]).await;

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("Failed to create container 'fails'"),
        "the run must report the create failure: {message}"
    );
    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("exec:")),
        "no setup command may have run: {events:?}"
    );
    assert_removed_before_network(&events, "peer");
    assert_removed_before_network(&events, "app");
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-fails".to_string()),
        "nothing was created for 'fails', so nothing of its is removed: {events:?}"
    );
}

/// The task's own container is created before its dependencies are ready,
/// and is removed like any other if a dependency then fails its readiness
/// gate — it was never started, and never run.
#[tokio::test(start_paused = true)]
async fn the_task_container_is_created_before_its_dependencies_are_ready_and_removed_if_one_fails()
{
    let docker = FakeContainerRuntime::default()
        .with_health_check_delay("database", std::time::Duration::from_secs(1))
        .with_unhealthy_container("database");
    let engine = engine(config_with_database_dependency(|_| {}), docker.clone());

    let result = engine.run_task("start", &[]).await;

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("'database' did not become healthy"),
        "the run must report the dependency's gate failure: {message}"
    );
    let events = docker.events();
    let app_created = events
        .iter()
        .position(|e| e.starts_with("create:app:"))
        .unwrap_or_else(|| panic!("the task container is created: {events:?}"));
    let database_verdict = events
        .iter()
        .position(|e| e == "wait-healthy:sidecar-id-database")
        .unwrap_or_else(|| panic!("'database' reaches a verdict: {events:?}"));
    assert!(
        app_created < database_verdict,
        "the task container is created before 'database' reaches its verdict: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("run:app:")),
        "the task container is never run: {events:?}"
    );
    assert_removed_before_network(&events, "app");
    assert_removed_before_network(&events, "database");
}

/// A `network_mode = "container:<target>"` container needs the target's id
/// to be created, and nothing more: it is created once the target is
/// *created* — with that id in its spec — not once the target is ready, and
/// started only once the target is ready, like any other dependency.
#[tokio::test(start_paused = true)]
async fn a_container_sharing_anothers_network_is_created_after_its_target_and_started_after_its_readiness(
) {
    let mut sharer = container("alpine:3.18", Some(vec!["db".to_string()]));
    sharer.network_mode = Some(NetworkMode::Container("db".to_string()));
    let containers = IndexMap::from([
        ("db".to_string(), container("postgres:16", None)),
        ("sharer".to_string(), sharer),
        (
            "app".to_string(),
            container("alpine:3.18", Some(vec!["sharer".to_string()])),
        ),
    ]);
    let tasks = IndexMap::from([("start".to_string(), task("app", "echo hi"))]);
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };
    let docker = FakeContainerRuntime::default()
        .with_health_check_delay("db", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    let position = |event: &str| {
        events
            .iter()
            .position(|e| e == event || e.starts_with(event))
            .unwrap_or_else(|| panic!("no '{event}' event: {events:?}"))
    };
    let db_created = position("create:db:");
    let sharer_created = position("create:sharer:container:sidecar-id-db");
    let db_healthy = position("wait-healthy:sidecar-id-db");
    let sharer_started = position("sidecar-start:sharer:");
    assert!(
        db_created < sharer_created,
        "the sharer is created with its target's id, so after the target: {events:?}"
    );
    assert!(
        sharer_created < db_healthy,
        "the sharer's creation does not wait for its target to be ready: {events:?}"
    );
    assert!(
        db_healthy < sharer_started,
        "the sharer's start does wait for its target to be ready: {events:?}"
    );
}

/// Under `--max-parallelism 1` a chain of three completes — a create and a
/// start are separate permits, and none is held while waiting for a
/// dependency, so the chain can't deadlock on the cap — and no two creates
/// or starts overlap: three creates and two dependency starts, each
/// holding the one permit for `delay`, take at least five of them. The
/// task container's own run is never capped.
#[tokio::test(start_paused = true)]
async fn max_parallelism_of_one_serializes_creates_and_starts_along_a_chain_without_deadlock() {
    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_create_delay("a", delay)
        .with_create_delay("b", delay)
        .with_create_delay("app", delay)
        .with_start_delay("a", delay)
        .with_start_delay("b", delay);
    let engine = engine(config_with_a_dependency_chain_of_three_pulls(), docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(1),
            ..Default::default()
        })
        .unwrap();

    let start = tokio::time::Instant::now();
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        engine.run_task("start", &[]),
    )
    .await
    .expect("a chain longer than the cap must not deadlock on it")
    .unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed >= delay * 5,
        "with --max-parallelism 1, three creates and two starts each hold the one permit \
         in turn (elapsed: {elapsed:?})"
    );
}

/// A `run_to_completion` dependency (ratect#97) is started like any other,
/// but its readiness gate is running to exit 0 — not a health check or
/// `setup_commands`, neither of which apply to it.
#[tokio::test]
async fn run_to_completion_dependency_runs_to_completion_before_the_task_starts() {
    let config = config_with_database_dependency(|database| {
        database.run_to_completion = Some(true);
    });
    let docker = FakeContainerRuntime::default().with_dependency_exit(
        "database",
        std::time::Duration::ZERO,
        0,
    );
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    let start_index = events
        .iter()
        .position(|e| e.starts_with("sidecar-start:database:"))
        .expect("the dependency should have started");
    let run_index = events
        .iter()
        .position(|e| e.starts_with("run:app:"))
        .expect("the task container should have run");
    assert!(
        start_index < run_index,
        "the dependency must run to completion before the task starts: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("wait-healthy:sidecar-id-database")),
        "a run-to-completion dependency has no health check to wait for: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("exec:sidecar-id-database:")),
        "a run-to-completion dependency has no setup commands to run: {events:?}"
    );
}

/// The `DependencyCompleted` event (`ContainerBecameHealthy`'s counterpart
/// for this dialect) is what tells a display the dependency is ready.
#[tokio::test]
async fn run_to_completion_dependency_posts_dependency_completed() {
    let config = config_with_database_dependency(|database| {
        database.run_to_completion = Some(true);
    });
    let docker = FakeContainerRuntime::default().with_dependency_exit(
        "database",
        std::time::Duration::ZERO,
        0,
    );
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    engine.run_task("start", &[]).await.unwrap();

    let events = sink.events();
    assert!(
        events.iter().any(
            |e| matches!(e, TaskEvent::DependencyCompleted { container } if container == "database")
        ),
        "expected a DependencyCompleted event for 'database': {events:?}"
    );
    assert!(
        !events.iter().any(
            |e| matches!(e, TaskEvent::ContainerBecameHealthy { container } if container == "database")
        ),
        "a run-to-completion dependency has no health check, so no ContainerBecameHealthy \
         event should post for it: {events:?}"
    );
}

/// A config shaped the way `expand_external_health_checks` produces one:
/// `database` carries an `external_health_check`, and its companion is
/// marked as reporting `database`'s readiness rather than its own. Built by
/// hand rather than through the loader — this is the engine's half of
/// ratect#202, and the config half has its own tests.
fn config_with_an_external_health_check() -> Config {
    let mut database = container("postgres:16", None);
    database.external_health_check = Some(crate::config::ExternalHealthCheck {
        kind: crate::config::ExternalHealthCheckKind::Tcp { port: 5432 },
        interval: None,
        retries: None,
        timeout: None,
    });
    let mut companion = container("curlimages/curl", Some(vec!["database".to_string()]));
    companion.run_to_completion = Some(true);
    companion.reports_readiness_for = Some("database".to_string());

    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(COMPANION.to_string(), companion);
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["database".to_string(), COMPANION.to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

const COMPANION: &str = "ratect-health-check-database";

/// The whole of ratect#202: an external health check is reported as a health
/// check on the container it checks. The companion narrates nothing of its
/// own — not its start, not its completion, not even its removal at cleanup
/// — and the checked container's `ContainerBecameHealthy` is the companion's
/// success, not the empty Docker gate it skipped on the way past.
#[tokio::test]
async fn an_external_health_checks_companion_reports_the_checked_containers_readiness() {
    let docker = FakeContainerRuntime::default().with_dependency_exit(
        COMPANION,
        std::time::Duration::ZERO,
        0,
    );
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_an_external_health_check()),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    engine.run_task("start", &[]).await.unwrap();

    let events = sink.events();
    let healthy: Vec<&TaskEvent> = events
        .iter()
        .filter(|e| matches!(e, TaskEvent::ContainerBecameHealthy { container } if container == "database"))
        .collect();
    assert_eq!(
        healthy.len(),
        1,
        "'database' should be reported healthy exactly once — when its check \
         passed, not when its empty Docker gate was skipped: {events:?}"
    );
    for event in &events {
        let named = match event {
            TaskEvent::ContainerCreating { container }
            | TaskEvent::ContainerCreated { container }
            | TaskEvent::DependencyStarting { container }
            | TaskEvent::DependencyStarted { container }
            | TaskEvent::DependencyCompleted { container }
            | TaskEvent::ContainerRemoved { container }
            | TaskEvent::ImageResolved { container }
            | TaskEvent::ContainerBecameHealthy { container } => container.as_str(),
            _ => continue,
        };
        assert_ne!(
            named, COMPANION,
            "the companion narrates nothing of its own: {events:?}"
        );
    }
}

/// A failed external check is a failure of the container it checks, in that
/// container's name — carrying what the check actually saw, which only the
/// companion's own output knows, and naming the companion so it can still be
/// found afterwards.
#[tokio::test]
async fn a_failing_external_health_check_fails_in_the_checked_containers_name() {
    let docker = FakeContainerRuntime::default()
        .with_dependency_exit(COMPANION, std::time::Duration::ZERO, 1)
        .with_container_output(
            COMPANION,
            "no TCP connection to database:5432 after 30 attempt(s)",
        );
    let engine = TaskEngine::new(
        project(config_with_an_external_health_check()),
        docker,
        Arc::new(RecordingEventSink::default()),
        crate::interrupt::Interrupt::new(),
    );

    let error = format!("{:#}", engine.run_task("start", &[]).await.unwrap_err());

    assert!(
        error.contains("Container 'database' did not become healthy"),
        "the failure belongs to the checked container: {error}"
    );
    assert!(
        error.contains("no TCP connection to database:5432 after 30 attempt(s)"),
        "the check's own diagnosis has to survive the container it ran in: {error}"
    );
    assert!(
        error.contains(COMPANION),
        "the companion is named so it can be inspected afterwards: {error}"
    );
}

/// The external check alone decides readiness (ratect#269): the checked
/// container's image may carry a `HEALTHCHECK` of its own, which Docker
/// still runs, but Ratect doesn't wait on it — whether it would fail or
/// never report at all. The companion passing is what lets `app` start.
#[tokio::test(start_paused = true)]
async fn an_external_health_check_does_not_also_wait_on_the_images_own_healthcheck() {
    for label in ["unhealthy", "never reports"] {
        let docker = FakeContainerRuntime::default().with_dependency_exit(
            COMPANION,
            std::time::Duration::ZERO,
            0,
        );
        let docker = match label {
            "unhealthy" => docker.with_unhealthy_container("database"),
            _ => docker.with_health_check_delay("database", std::time::Duration::from_secs(3600)),
        };
        let engine = engine(config_with_an_external_health_check(), docker.clone());

        tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
            .await
            .unwrap_or_else(|_| panic!("{label}: the task waited on the image's HEALTHCHECK"))
            .unwrap_or_else(|error| panic!("{label}: the task should succeed: {error:#}"));

        let events = docker.events();
        assert!(
            !events
                .iter()
                .any(|e| e.starts_with("wait-healthy:sidecar-id-database")),
            "{label}: Ratect must not wait on the checked container's own health: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.starts_with("run:app:")),
            "{label}: the dependent should have run: {events:?}"
        );
    }
}

/// A non-zero exit fails the task run the same way a health-check or
/// `setup_commands` failure does today, with normal cleanup still running
/// afterward.
#[tokio::test]
async fn failing_run_to_completion_dependency_fails_the_task_and_still_cleans_up() {
    let config = config_with_database_dependency(|database| {
        database.run_to_completion = Some(true);
    });
    let docker = FakeContainerRuntime::default().with_dependency_exit(
        "database",
        std::time::Duration::ZERO,
        1,
    );
    let engine = engine(config, docker.clone());

    let result = engine.run_task("start", &[]).await;

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("'database'") && message.contains("exited with code 1"),
        "error should name the failing dependency and its exit code: {message}"
    );

    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("run:")),
        "the task must not run when a run-to-completion dependency fails: {events:?}"
    );
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-database".to_string()),
        "the failed dependency's already-exited container must still be cleaned up: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.starts_with("network-remove:")),
        "the network must still be removed: {events:?}"
    );
}

/// A container can mix a regular (long-running) dependency and a
/// run-to-completion one at once, in any combination.
#[tokio::test]
async fn run_to_completion_and_regular_dependencies_can_be_mixed() {
    let mut containers = IndexMap::new();
    containers.insert("migrate".to_string(), {
        let mut c = container("alpine:3.18", None);
        c.run_to_completion = Some(true);
        c
    });
    containers.insert("cache".to_string(), container("redis:7", None));
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["migrate".to_string(), "cache".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().with_dependency_exit(
        "migrate",
        std::time::Duration::ZERO,
        0,
    );
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    assert!(events
        .iter()
        .any(|e| e.starts_with("sidecar-start:migrate:")));
    assert!(events.iter().any(|e| e.starts_with("sidecar-start:cache:")));
    assert!(events
        .iter()
        .any(|e| e.starts_with("wait-healthy:sidecar-id-cache")));
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("wait-healthy:sidecar-id-migrate")),
        "the run-to-completion dependency must not go through the health-check gate: {events:?}"
    );
    assert!(events.iter().any(|e| e.starts_with("run:app:")));
}

/// Two run-to-completion dependencies for the same container that don't
/// depend on each other run concurrently, gated purely by the graph —
/// mirrors `independent_dependencies_start_concurrently_not_sequentially`.
#[tokio::test(start_paused = true)]
async fn independent_run_to_completion_dependencies_run_concurrently() {
    let mut containers = IndexMap::new();
    containers.insert("migrate-a".to_string(), {
        let mut c = container("alpine:3.18", None);
        c.run_to_completion = Some(true);
        c
    });
    containers.insert("migrate-b".to_string(), {
        let mut c = container("alpine:3.18", None);
        c.run_to_completion = Some(true);
        c
    });
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["migrate-a".to_string(), "migrate-b".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
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
        .with_dependency_exit("migrate-a", delay, 0)
        .with_dependency_exit("migrate-b", delay, 0);
    let engine = engine(config, docker);

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < delay * 2,
        "two independent run-to-completion dependencies with a {delay:?} delay each should \
         overlap, not run sequentially (elapsed: {elapsed:?})"
    );
}

/// A run-to-completion dependency can be declared directly under a task's
/// own `dependencies`, not only nested under another dependency — same
/// readiness gate either way.
#[tokio::test]
async fn run_to_completion_dependency_declared_directly_on_the_task() {
    let mut containers = IndexMap::new();
    containers.insert("migrate".to_string(), {
        let mut c = container("alpine:3.18", None);
        c.run_to_completion = Some(true);
        c
    });
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    let mut task = task("app", "echo hi");
    task.dependencies = Some(vec!["migrate".to_string()]);
    tasks.insert("start".to_string(), task);
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().with_dependency_exit(
        "migrate",
        std::time::Duration::ZERO,
        0,
    );
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    let start_index = events
        .iter()
        .position(|e| e.starts_with("sidecar-start:migrate:"))
        .expect("the task-level dependency should have started");
    let run_index = events
        .iter()
        .position(|e| e.starts_with("run:app:"))
        .expect("the task container should have run");
    assert!(start_index < run_index, "events: {events:?}");
}

/// `run_to_completion` is inert on a task's own container (ratect#268):
/// task `a` runs `c` as an ordinary task container, while task `b`, which
/// depends on that same `c`, still waits on its exit as a dependency.
#[tokio::test]
async fn run_to_completion_is_inert_on_a_tasks_own_container() {
    let config = || {
        let mut containers = IndexMap::new();
        containers.insert("c".to_string(), {
            let mut c = container("alpine:3.18", None);
            c.run_to_completion = Some(true);
            c
        });
        containers.insert("app".to_string(), container("alpine:3.18", None));
        let mut tasks = IndexMap::new();
        tasks.insert("a".to_string(), task("c", "echo hi"));
        let mut b = task("app", "echo hi");
        b.dependencies = Some(vec!["c".to_string()]);
        tasks.insert("b".to_string(), b);
        Config {
            project_name: "demo".to_string(),
            containers,
            tasks,
            config_variables: None,
            forbid_telemetry: None,
        }
    };

    let docker = FakeContainerRuntime::default();
    engine(config(), docker.clone())
        .run_task("a", &[])
        .await
        .unwrap();
    let events = docker.events();
    assert!(
        events.iter().any(|e| e.starts_with("run:c:")),
        "task 'a' must run 'c' as its own container: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("sidecar-start:c:")),
        "task 'a' must not start 'c' as a dependency too: {events:?}"
    );

    let docker =
        FakeContainerRuntime::default().with_dependency_exit("c", std::time::Duration::ZERO, 0);
    engine(config(), docker.clone())
        .run_task("b", &[])
        .await
        .unwrap();
    let events = docker.events();
    let start_index = events
        .iter()
        .position(|e| e.starts_with("sidecar-start:c:"))
        .expect("task 'b' must start 'c' as a dependency");
    let run_index = events
        .iter()
        .position(|e| e.starts_with("run:app:"))
        .expect("task 'b''s own container should have run");
    assert!(
        start_index < run_index,
        "'c' must run to completion before task 'b' starts: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("wait-healthy:sidecar-id-c")),
        "'c' is a run-to-completion dependency of 'b', not a health-checked one: {events:?}"
    );

    // Its exit is what `b` waits on: a non-zero one fails `b` before
    // `b`'s own container runs.
    let docker =
        FakeContainerRuntime::default().with_dependency_exit("c", std::time::Duration::ZERO, 1);
    let result = engine(config(), docker.clone()).run_task("b", &[]).await;
    assert!(result.is_err(), "a failing 'c' must fail task 'b'");
    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("run:app:")),
        "task 'b''s own container must not run after 'c' failed: {events:?}"
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
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );
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
    let engine = engine
        .with_settings(TaskEngineSettings {
            cleanup_after_failure: false,
            ..Default::default()
        })
        .unwrap();

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

/// The task's own container is removed by the engine's cleanup stage like
/// any other, from the id registered at its creation — an interrupt drops
/// the run's future before `run_container` returns, so that registration
/// is the only thing cleanup has to go on. Without this, the whole
/// `created_containers` path could break with the non-Docker suite still
/// green.
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
/// one it has silently swallowed. The cleanup ladder is therefore relative
/// to the signals already seen when cleanup started — otherwise the first
/// Ctrl+C during the cleanup of a run that finished normally would do
/// nothing at all, which is the common case rather than an exotic one.
///
/// What that first signal does is *escalate*: the graceful stop in flight
/// is dropped and everything left is force-removed instead, so nothing is
/// left behind (ratect#249). The run still passes: the signal arrived after
/// the command had exited `0`, so it ended nothing, and a forced removal that
/// completes is a faster cleanup rather than a failed one (decisions/0016).
#[tokio::test]
async fn a_signal_during_cleanup_escalates_it_even_when_the_run_was_not_interrupted() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    // Fires as cleanup removes its first container, so the run itself
    // completes entirely uninterrupted.
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[crate::interrupt::TerminationSignal::Terminate],
    );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    engine
        .run_task("start", &[])
        .await
        .expect("a signal during a passing run's completed cleanup leaves it passing");

    assert_eq!(
        interrupt.count(),
        1,
        "exactly one signal, landing during cleanup"
    );
    let events = docker.events();
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the graceful stop in flight when the signal landed is dropped: {events:?}"
    );
    assert!(
        events.contains(&"force-remove:sidecar-id-app".to_string()),
        "and replaced by a forced removal of the same container: {events:?}"
    );
    assert!(
        events.contains(&"force-remove:sidecar-id-database".to_string()),
        "everything after it is force-removed too: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.starts_with("network-remove:")),
        "and the network still goes: {events:?}"
    );
}

/// A second signal during cleanup means "faster", not "leave it": a
/// supervisor escalating `SIGINT` → `SIGTERM` on a fixed timer (Claude
/// Code's MCP shutdown sends them 100 ms apart, then `SIGKILL` 400 ms
/// later) never meant to abandon anything, and a person's second Ctrl+C
/// wants the process gone, which force-removal delivers in a few daemon
/// round-trips rather than a container's full kill timeout (ratect#249).
///
/// The signal has to land *during* cleanup, not merely be the second one
/// overall: two that both arrive while the run is still going are one
/// decision ("stop").
#[tokio::test]
async fn a_second_signal_during_cleanup_force_removes_what_is_left() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(60))
        .signalling_on_removal(
            &interrupt,
            &[crate::interrupt::TerminationSignal::Terminate],
        );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("start", &[]).await.unwrap_err();
    let interrupted = error
        .downcast_ref::<crate::interrupt::TaskInterrupted>()
        .expect("an interrupted run fails as TaskInterrupted");
    assert_eq!(
        interrupted.signal,
        crate::interrupt::TerminationSignal::Interrupt,
        "the exit code stays with the signal that ended the run, not the one that \
         hurried its cleanup"
    );

    let events = docker.events();
    // The task's own container is removed first, and it's that removal
    // which lands the second signal — mid-flight, so it's the one that
    // gets dropped and force-removed instead.
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the removal in flight when the signal landed should be dropped, \
             not run to completion: {events:?}"
    );
    assert!(
        events.contains(&"force-remove:sidecar-id-app".to_string()),
        "and force-removed instead: {events:?}"
    );
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-database".to_string())
            && events.contains(&"force-remove:sidecar-id-database".to_string()),
        "the dependency skips the graceful stop entirely: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.starts_with("network-remove:")),
        "and the network is still removed: {events:?}"
    );
}

/// The same for a person's second Ctrl+C — the kind of signal doesn't
/// decide the rung, the count does.
#[tokio::test]
async fn a_second_interrupt_during_cleanup_force_removes_what_is_left() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(60))
        .signalling_on_removal(
            &interrupt,
            &[crate::interrupt::TerminationSignal::Interrupt],
        );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("start", &[]).await.unwrap_err();
    assert!(error.is::<crate::interrupt::TaskInterrupted>());

    let events = docker.events();
    assert!(
        events.contains(&"force-remove:sidecar-id-app".to_string())
            && events.contains(&"force-remove:sidecar-id-database".to_string())
            && events.iter().any(|e| e.starts_with("network-remove:")),
        "a double Ctrl+C leaves nothing behind: {events:?}"
    );
}

/// A `SIGINT` after escalation is the escape hatch: forced removal still
/// talks to the daemon, and a daemon that has stopped answering would
/// otherwise hold the process until something untrappable ends it. So the
/// rung after "faster" is "stop", exactly as the second used to be — for
/// the one signal only a person sends twice.
#[tokio::test]
async fn a_third_interrupt_during_cleanup_abandons_it() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(60))
        .signalling_on_removal(
            &interrupt,
            &[
                crate::interrupt::TerminationSignal::Interrupt,
                crate::interrupt::TerminationSignal::Interrupt,
            ],
        );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("start", &[]).await.unwrap_err();
    assert!(error.is::<crate::interrupt::TaskInterrupted>());

    let events = docker.events();
    // The graceful stop lands the second signal and is dropped; the forced
    // removal that replaces it lands the third and is dropped too, and
    // nothing after it starts.
    let removals: Vec<&String> = events
        .iter()
        .filter(|e| e.starts_with("sidecar-stop:") || e.starts_with("force-remove:"))
        .collect();
    assert!(
        removals.is_empty(),
        "neither removal of the task's own container runs to completion, and cleanup \
         stops rather than pressing on to the dependency: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("network-remove:")),
        "and stops before removing the network: {events:?}"
    );
}

/// The abandon rung is `SIGINT`-only. A supervisor stopping a run whose
/// task had already finished — so its cleanup was already under way — sends
/// `SIGINT` then `SIGTERM` 100 ms apart; the first escalates, and the
/// second must not abandon the forced removals it just started, or the
/// leak this ladder exists to close is back for exactly that case.
#[tokio::test]
async fn a_supervisors_follow_up_signal_after_escalation_does_not_abandon() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[
            crate::interrupt::TerminationSignal::Interrupt,
            crate::interrupt::TerminationSignal::Terminate,
        ],
    );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    engine
        .run_task("start", &[])
        .await
        .expect("the run had passed, and its cleanup completed");

    let events = docker.events();
    assert!(
        events.contains(&"force-remove:sidecar-id-app".to_string())
            && events.contains(&"force-remove:sidecar-id-database".to_string())
            && events.iter().any(|e| e.starts_with("network-remove:")),
        "the SIGTERM hurries nothing further and abandons nothing: {events:?}"
    );
}

/// Two signals during the cleanup of a run that finished normally: the
/// first escalates, a `SIGINT` after it abandons — and an abandoned cleanup
/// is a failed one, so a run that would have passed fails with
/// `CleanupFailed` rather than reporting either signal, neither of which
/// ended the command (decisions/0016).
#[tokio::test]
async fn an_interrupt_after_escalation_of_an_uninterrupted_runs_cleanup_abandons_it() {
    let config = config_with_database_dependency(|_| {});
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[
            crate::interrupt::TerminationSignal::Hangup,
            crate::interrupt::TerminationSignal::Interrupt,
        ],
    );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("start", &[]).await.unwrap_err();
    assert!(
        error.is::<CleanupFailed>(),
        "an abandoned cleanup fails a passing run: {error:#}"
    );

    let events = docker.events();
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("force-remove:sidecar-id-database"))
            && !events.iter().any(|e| e.starts_with("network-remove:")),
        "the second signal abandons what the first had hurried: {events:?}"
    );
}

/// A failing run signalled during its cleanup keeps its own code whether
/// that cleanup completes or is abandoned: the failure is what the user
/// needs to see, and the signal ended nothing (decisions/0016, rule 1).
#[tokio::test]
async fn a_failing_run_signalled_during_cleanup_keeps_its_own_code() {
    for signals in [
        &[crate::interrupt::TerminationSignal::Interrupt][..],
        &[
            crate::interrupt::TerminationSignal::Interrupt,
            crate::interrupt::TerminationSignal::Interrupt,
        ][..],
    ] {
        let config = config_with_database_dependency(|_| {});
        let interrupt = crate::interrupt::Interrupt::new();
        let docker = FakeContainerRuntime::default()
            .with_run_exit("app", 3)
            .signalling_on_removal(&interrupt, signals);
        let engine = TaskEngine::new(
            project(config),
            docker.clone(),
            Arc::new(NullEventSink),
            Arc::clone(&interrupt),
        );

        let error = engine.run_task("start", &[]).await.unwrap_err();
        let failure = error
            .downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .unwrap_or_else(|| panic!("{} signal(s): {error:#}", signals.len()));
        assert_eq!(failure.exit_code, 3);
    }
}

/// A passing run whose cleanup the daemon refuses fails with
/// `CleanupFailed`, which both binaries exit `125` with — once each for the
/// network and for a dependency, the two #316 reproduced. Everything else is
/// still removed: one refusal doesn't stop the rest of cleanup.
#[tokio::test]
async fn a_passing_run_whose_cleanup_fails_fails_with_cleanup_failed() {
    let network = FakeContainerRuntime::default().refusing_network_removal();
    let dependency =
        FakeContainerRuntime::default().refusing_container_removal("sidecar-id-database");
    for docker in [network, dependency] {
        let engine = engine(config_with_database_dependency(|_| {}), docker.clone());

        let error = engine.run_task("start", &[]).await.unwrap_err();

        assert!(error.is::<CleanupFailed>(), "{error:#}");
        let events = docker.events();
        assert!(
            events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
            "the task's own container is still removed: {events:?}"
        );
    }
}

/// A task's own non-zero code is never overridden by a cleanup failure.
#[tokio::test]
async fn a_failing_run_whose_cleanup_fails_keeps_its_own_code() {
    let docker = FakeContainerRuntime::default()
        .with_run_exit("app", 3)
        .refusing_network_removal();
    let engine = engine(config_with_database_dependency(|_| {}), docker);

    let error = engine.run_task("start", &[]).await.unwrap_err();

    let failure = error
        .downcast_ref::<crate::docker::ContainerExitedNonZero>()
        .unwrap_or_else(|| panic!("{error:#}"));
    assert_eq!(failure.exit_code, 3);
}

/// A run a signal ended keeps the signal's code, even when its cleanup then
/// fails (decisions/0016, rule 2).
#[tokio::test]
async fn a_signalled_run_whose_cleanup_fails_keeps_the_signal() {
    let config = config_with_database_dependency(|_| {});
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(60))
        .refusing_network_removal();
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record_signal(crate::interrupt::TerminationSignal::Terminate);
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("start", &[]).await.unwrap_err();

    let interrupted = error
        .downcast_ref::<crate::interrupt::TaskInterrupted>()
        .unwrap_or_else(|| panic!("{error:#}"));
    assert_eq!(
        interrupted.signal,
        crate::interrupt::TerminationSignal::Terminate
    );
}

/// A prerequisite whose cleanup fails ends the chain like any other failure
/// of Ratect's own: the task that depends on it never runs, so its code —
/// whatever it would have been — never decides the run's (decisions/0016,
/// rule 3).
#[tokio::test]
async fn a_prerequisites_cleanup_failure_stops_the_chain() {
    let mut config = config_with_database_dependency(|_| {});
    config
        .containers
        .insert("tool".to_string(), container("alpine:3.18", None));
    config
        .tasks
        .insert("prepare".to_string(), task("app", "true"));
    let mut main = task("tool", "true");
    main.prerequisites = Some(vec!["prepare".to_string()]);
    config.tasks.insert("main".to_string(), main);
    let docker = FakeContainerRuntime::default()
        .refusing_container_removal("sidecar-id-database")
        .with_run_exit("tool", 3);
    let engine = engine(config, docker.clone());

    let error = engine.run_task("main", &[]).await.unwrap_err();

    assert!(error.is::<CleanupFailed>(), "{error:#}");
    let ran: Vec<String> = docker
        .specs
        .lock()
        .unwrap()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    assert!(
        !ran.contains(&"tool".to_string()),
        "the main task's container never runs: {ran:?}"
    );
}

/// A signal during a prerequisite's cleanup leaves that prerequisite passed,
/// but ends the run: the task that depends on it never starts, and the run
/// reports the signal (decisions/0016, rule 2).
#[tokio::test]
async fn a_signal_during_a_prerequisites_cleanup_ends_the_run_before_the_next_task() {
    let mut config = config_with_database_dependency(|_| {});
    config
        .containers
        .insert("tool".to_string(), container("alpine:3.18", None));
    config
        .tasks
        .insert("prepare".to_string(), task("app", "true"));
    let mut main = task("tool", "true");
    main.prerequisites = Some(vec!["prepare".to_string()]);
    config.tasks.insert("main".to_string(), main);
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[crate::interrupt::TerminationSignal::Terminate],
    );
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("main", &[]).await.unwrap_err();

    let interrupted = error
        .downcast_ref::<crate::interrupt::TaskInterrupted>()
        .unwrap_or_else(|| panic!("{error:#}"));
    assert_eq!(
        interrupted.signal,
        crate::interrupt::TerminationSignal::Terminate
    );
    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("run:tool:")),
        "the main task never starts: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.starts_with("network-create:"))
            .count(),
        events
            .iter()
            .filter(|e| e.starts_with("network-remove:"))
            .count(),
        "every network created is removed: {events:?}"
    );
}

/// With an interrupt tracker nothing has recorded on — every unit test
/// that doesn't deliberately record one, and both binaries before 0.25.0's
/// tracker existed at all — the run behaves exactly as if there were no
/// tracker: see `TaskEngine::interrupt`'s own doc comment for why.
#[tokio::test]
async fn an_interrupt_tracker_nothing_has_recorded_on_leaves_a_run_unaffected() {
    let config = config_with_database_dependency(|_| {});
    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
                run_in: None,
            },
            crate::config::SetupCommand {
                command: "./seed-data.sh".to_string(),
                working_directory: None,
                run_in: None,
            },
        ]);
    });

    let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed-data.sh");
    let engine = engine(config, docker.clone());

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

#[tokio::test(start_paused = true)]
async fn task_containers_own_health_check_reaches_docker_and_is_waited_on() {
    // 0.21.0 closed this gap: the task's own container now goes through
    // the same readiness gate a dependency always has (see
    // `run_task_container_readiness`), run concurrently with its main
    // command rather than gating anything.
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.health_check = Some(crate::config::HealthCheckConfig {
        command: Some("wget -q localhost".to_string()),
        interval: None,
        retries: None,
        start_period: None,
        timeout: None,
    });
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    // The gate runs alongside the main command and is dropped the moment
    // the command exits (decisions/0012), so the command has to outlive it
    // for the gate to do anything — as a real one does.
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

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

#[tokio::test(start_paused = true)]
async fn unhealthy_task_container_fails_the_task() {
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.health_check = Some(crate::config::HealthCheckConfig {
        command: Some("wget -q localhost".to_string()),
        interval: None,
        retries: None,
        start_period: None,
        timeout: None,
    });
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_unhealthy_container("app");
    let engine = engine(config, docker.clone());

    let result = engine.run_task("start", &[]).await;

    assert!(
        result.is_err(),
        "an unhealthy task container should fail the task while its own command is \
             still running"
    );
}

#[tokio::test(start_paused = true)]
async fn task_containers_own_setup_commands_run() {
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    // The gate runs alongside the main command and is dropped the moment
    // the command exits (decisions/0012), so the command has to outlive it
    // for the gate to do anything — as a real one does.
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    assert!(
        events
            .iter()
            .any(|e| e == "exec:sidecar-id-app:./migrate.sh"),
        "the task's own container's setup command should have run: {events:?}"
    );
}

/// A task whose `run.working_directory` overrides its container's, with a
/// setup command of the given `working_directory` on that container.
fn config_with_task_working_directory_and_setup_command(
    setup_command_working_directory: Option<&str>,
) -> Config {
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.working_directory = Some("/from-container".to_string());
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: setup_command_working_directory.map(str::to_string),
        run_in: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut start = task("app", "echo hi");
    start.run.as_mut().unwrap().working_directory = Some("/from-task".to_string());
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), start);
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// Batect's code, not its docs (which say the override is ignored): the
/// task-specialised container carries `run.working_directory`, and that is
/// what a setup command with none of its own falls back to (ratect#271).
#[tokio::test(start_paused = true)]
async fn task_containers_setup_command_falls_back_to_the_tasks_run_working_directory() {
    let config = config_with_task_working_directory_and_setup_command(None);
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let (working_directory, _, _) = docker.exec_for("./migrate.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-task"));
}

#[tokio::test(start_paused = true)]
async fn task_containers_setup_commands_own_working_directory_overrides_the_tasks() {
    let config = config_with_task_working_directory_and_setup_command(Some("/from-setup-command"));
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let (working_directory, _, _) = docker.exec_for("./migrate.sh").unwrap();
    assert_eq!(working_directory.as_deref(), Some("/from-setup-command"));
}

#[tokio::test(start_paused = true)]
async fn failing_setup_command_on_the_tasks_own_container_fails_the_task() {
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(config, docker.clone());

    let result = engine.run_task("start", &[]).await;

    assert!(
        result.is_err(),
        "a failing setup command on the task's own container should fail the task \
             while its own command is still running"
    );
}

/// A readiness-gate failure on the task's own container is an
/// *infrastructure* failure, not the container's own verdict — so
/// `--no-cleanup-after-failure` keeps it, exactly as it keeps that run's
/// sidecars and network. This is only assertable now that the engine
/// owns the removal; while `run_container` removed its own container it
/// classified the same error as a completed run and force-removed the
/// one container the flag existed to preserve.
#[tokio::test(start_paused = true)]
async fn a_readiness_failure_on_the_tasks_own_container_honours_no_cleanup_after_failure() {
    let config = config_with_failing_task_container_setup_command();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cleanup_after_failure: false,
            ..Default::default()
        })
        .unwrap();

    engine.run_task("start", &[]).await.unwrap_err();

    let events = docker.events();
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container must be left for investigation: {events:?}"
    );
}

/// The other side of the same coin — with the flag left alone, that
/// container is cleaned up like anything else.
#[tokio::test(start_paused = true)]
async fn a_readiness_failure_on_the_tasks_own_container_still_removes_it_by_default() {
    let config = config_with_failing_task_container_setup_command();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(config, docker.clone());

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
#[tokio::test(start_paused = true)]
async fn a_readiness_failure_is_unaffected_by_no_cleanup_after_success() {
    let config = config_with_failing_task_container_setup_command();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cleanup_after_success: false,
            ..Default::default()
        })
        .unwrap();

    engine.run_task("start", &[]).await.unwrap_err();

    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container should still be removed: {events:?}"
    );
}

/// The task's own container's create can fail with nothing created — a spec
/// the daemon refuses. The run fails with that error, there is nothing of
/// the task container's to remove, and the rest of the run's cleanup still
/// happens: `database`, created a second before `app`'s create is refused,
/// is removed. Wrapped in a timeout rather than left to hang: a deadlock
/// here would otherwise stall the suite with no indication of which test
/// did it.
#[tokio::test(start_paused = true)]
async fn a_task_container_whose_create_fails_fails_the_run_and_still_cleans_up() {
    let config = config_with_database_dependency(|_| {});
    let docker = FakeContainerRuntime::default()
        .with_create_delay("app", std::time::Duration::from_secs(1))
        .failing_creation("app");
    let engine = engine(config, docker.clone());

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        engine.run_task("start", &[]),
    )
    .await
    .expect("the run must not hang on a failed create");

    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("Failed to create container 'app'"),
        "the creation failure must be reported: {message}"
    );

    let events = docker.events();
    assert!(
        events.contains(&"create-failed:app".to_string()),
        "the run should have got as far as attempting the task container: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("run:app:")),
        "a container that was never created is never run: {events:?}"
    );
    // Nothing was created, so there is nothing of the task container's to
    // remove — but the rest of the run's cleanup still has to happen.
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "no task container was created, so none should be removed: {events:?}"
    );
    assert_removed_before_network(&events, "database");
}

/// One task container whose own `setup_commands` entry fails — the
/// shared fixture for the three cleanup-policy tests above, which differ
/// only in which flag they set.
fn config_with_failing_task_container_setup_command() -> Config {
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
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
    // "Known limitations") takes roughly just the one delay.
    let mut containers = IndexMap::new();
    let mut app = container("alpine:3.18", None);
    app.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./migrate.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < delay * 2,
        "the task's own container's main command and its setup command should overlap, not \
             run sequentially (elapsed: {elapsed:?})"
    );
}

/// Long enough that a task still waiting on its main command is
/// unmistakable against `GATE_FAILURE_DEADLINE` — the stand-in for a main
/// command that waits forever on what its own readiness gate would have
/// done (ratect#260).
const MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE: std::time::Duration =
    std::time::Duration::from_secs(3600);
const GATE_FAILURE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// A readiness-gate failure on the task's own container abandons the main
/// command still running in it, as Batect's cancellation does, rather than
/// waiting for a command that may be waiting on the gate itself
/// (ratect#260). The container is then stopped and removed by the same
/// cleanup an interrupt falls through to.
#[tokio::test(start_paused = true)]
async fn a_task_containers_failing_setup_command_cancels_its_still_running_main_command() {
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(
        config_with_failing_task_container_setup_command(),
        docker.clone(),
    );

    let err = tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
        .await
        .expect("the gate's failure must end the task without waiting for the main command")
        .unwrap_err();

    assert!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .is_none(),
        "the gate's failure, not an exit code, should be the task's error: {err:#}"
    );
    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the cancelled task container should be stopped and removed: {events:?}"
    );
}

/// The same, when the gate fails on its health check instead.
#[tokio::test(start_paused = true)]
async fn a_task_containers_unhealthy_status_cancels_its_still_running_main_command() {
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_unhealthy_container("app");
    let engine = engine(config_with_a_gated_task_container(), docker.clone());

    tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
        .await
        .expect("the gate's failure must end the task without waiting for the main command")
        .unwrap_err();

    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the cancelled task container should be stopped and removed: {events:?}"
    );
}

/// Cancelling is still a failure, so `--no-cleanup-after-failure` keeps the
/// container it cancelled.
#[tokio::test(start_paused = true)]
async fn a_task_container_cancelled_by_its_gate_honours_no_cleanup_after_failure() {
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(
        config_with_failing_task_container_setup_command(),
        docker.clone(),
    )
    .with_settings(TaskEngineSettings {
        cleanup_after_failure: false,
        ..Default::default()
    })
    .unwrap();

    tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
        .await
        .expect("the gate's failure must end the task without waiting for the main command")
        .unwrap_err();

    let events = docker.events();
    assert!(
        !events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container must be left for investigation: {events:?}"
    );
}

/// A main command that exits non-zero before its gate fails keeps its own
/// exit code: the gate only cancels a command that is still running.
#[tokio::test(start_paused = true)]
async fn a_main_command_exiting_nonzero_before_its_gate_fails_keeps_its_exit_code() {
    let docker = FakeContainerRuntime::default()
        .failing_run()
        .with_exec_delay("./migrate.sh", std::time::Duration::from_secs(1))
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(config_with_failing_task_container_setup_command(), docker);

    let err = engine.run_task("start", &[]).await.unwrap_err();

    assert_eq!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the main command's own exit code should be the task's: {err:#}"
    );
}

/// The tie: the main command exits non-zero in the same instant its gate
/// fails. The exit code still wins — through `biased` alone, since the probe
/// here claims the container is still running and would cancel it. Without
/// `biased`, `select!` picks a branch at random, so losing it shows up as
/// this test failing intermittently rather than every time.
#[tokio::test(start_paused = true)]
async fn a_main_command_exiting_nonzero_as_its_gate_fails_keeps_its_exit_code() {
    let at_once = std::time::Duration::from_secs(1);
    let docker = FakeContainerRuntime::default()
        .with_running_probe(Ok(true))
        .failing_run()
        .with_run_delay("app", at_once)
        .with_exec_delay("./migrate.sh", at_once)
        .with_failing_setup_command("./migrate.sh");
    let engine = engine(config_with_failing_task_container_setup_command(), docker);

    let err = engine.run_task("start", &[]).await.unwrap_err();

    assert_eq!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the main command's own exit code should be the task's: {err:#}"
    );
}

/// A gate the main command's own exit failed — Docker reports the container
/// dying before it reports healthy — can resolve while the run is still
/// draining output and waiting on the exit report. That is not a command to
/// cancel: it has finished, and its exit code still wins.
#[tokio::test(start_paused = true)]
async fn a_gate_failed_by_the_main_commands_own_exit_keeps_its_exit_code() {
    let docker = FakeContainerRuntime::default()
        .failing_run()
        .with_run_delay("app", std::time::Duration::from_secs(1))
        .with_run_draining_after_exit("app", std::time::Duration::from_secs(10))
        .with_health_check_delay("app", std::time::Duration::from_secs(2))
        .with_unhealthy_container("app");
    let engine = engine(config_with_a_gated_task_container(), docker);

    let err = engine.run_task("start", &[]).await.unwrap_err();

    assert_eq!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the main command's own exit code should be the task's: {err:#}"
    );
}

/// When the probe can't confirm the main command exited — the daemon hasn't
/// recorded the exit yet, or the inspect fails — and the run doesn't report
/// within `TASK_CONTAINER_EXIT_GRACE` either, the run is cancelled, and the
/// gate's failure is the task's error: waiting on a command that might
/// still be running is the hang ratect#260 is about. Cancelled when the
/// grace expires, not later.
#[tokio::test(start_paused = true)]
async fn a_gate_failure_the_probe_cannot_attribute_to_an_exit_cancels_the_run() {
    for probe in [Ok(true), Err("inspect failed")] {
        let gate_fails_after = std::time::Duration::from_secs(2);
        let docker = FakeContainerRuntime::default()
            .with_running_probe(probe)
            .failing_run()
            .with_run_delay("app", std::time::Duration::from_secs(1))
            .with_run_draining_after_exit("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE)
            .with_health_check_delay("app", gate_fails_after)
            .with_unhealthy_container("app");
        let engine = engine(config_with_a_gated_task_container(), docker.clone());

        let start = tokio::time::Instant::now();
        let err = tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
            .await
            .unwrap_or_else(|_| panic!("{probe:?}: the run should have been cancelled"))
            .unwrap_err();
        assert_eq!(
            start.elapsed(),
            gate_fails_after + super::TASK_CONTAINER_EXIT_GRACE,
            "{probe:?}: the run should be cancelled exactly when the grace expires"
        );

        assert!(
            err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
                .is_none(),
            "{probe:?}: the gate's failure should be the task's error: {err:#}"
        );
        let events = docker.events();
        assert!(
            events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
            "{probe:?}: the cancelled task container should be removed: {events:?}"
        );
    }
}

/// The race ratect#263's fixture hit on CI: a setup command killed by the
/// main command's exit reports its 137 before Docker has marked the
/// container stopped, so the probe still says "running". The main command
/// has exited, though, and its result arrives within
/// `TASK_CONTAINER_EXIT_GRACE` — so it decides the task, exactly as it
/// would had the probe already seen the exit: 0 succeeds, non-zero keeps
/// its exit code.
#[tokio::test(start_paused = true)]
async fn a_gate_failure_reported_before_docker_records_the_exit_waits_for_the_run() {
    for fails in [false, true] {
        let mut docker = FakeContainerRuntime::default()
            .with_running_probe(Ok(true))
            .with_run_delay("app", std::time::Duration::from_secs(1))
            .with_run_draining_after_exit("app", super::TASK_CONTAINER_EXIT_GRACE / 2)
            .with_exec_delay("./migrate.sh", std::time::Duration::from_secs(1))
            .with_failing_setup_command("./migrate.sh");
        if fails {
            docker = docker.failing_run();
        }
        let engine = engine(config_with_failing_task_container_setup_command(), docker);

        let result = engine.run_task("start", &[]).await;

        if fails {
            let err = result.unwrap_err();
            assert_eq!(
                err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
                    .map(|exited| exited.exit_code),
                Some(1),
                "the main command's own exit code should be the task's: {err:#}"
            );
        } else {
            assert!(result.is_ok(), "the task should succeed: {result:?}");
        }
    }
}

/// A gate failure that says only that the task container stopped — the
/// health wait saw it die, or a setup command found it no longer running —
/// says nothing about its readiness. After a main command that exited 0,
/// that is the task succeeding, not failing (ratect#248). Covered with the
/// run still draining when the gate fails, and without.
#[tokio::test(start_paused = true)]
async fn a_gate_failed_only_by_a_successful_main_commands_exit_does_not_fail_the_task() {
    for drain in [None, Some(std::time::Duration::from_secs(10))] {
        for (label, config, docker) in [
            (
                "health wait",
                config_with_a_gated_task_container(),
                FakeContainerRuntime::default()
                    .with_health_check_delay("app", std::time::Duration::from_secs(2)),
            ),
            (
                "setup command",
                config_with_failing_task_container_setup_command(),
                FakeContainerRuntime::default()
                    .with_exec_delay("./migrate.sh", std::time::Duration::from_secs(2)),
            ),
        ] {
            let mut docker = docker
                .with_gate_observing_exits()
                .with_run_delay("app", std::time::Duration::from_secs(1));
            if let Some(drain) = drain {
                docker = docker.with_run_draining_after_exit("app", drain);
            }
            let sink = RecordingEventSink::default();
            let engine = TaskEngine::new(
                project(config),
                docker.clone(),
                Arc::new(sink.clone()),
                crate::interrupt::Interrupt::new(),
            );

            let result = engine.run_task("start", &[]).await;

            assert!(
                result.is_ok(),
                "{label}, drain {drain:?}: the task should succeed: {result:?}"
            );
            let posted = sink.events();
            assert!(
                !posted
                    .iter()
                    .any(|e| matches!(e, TaskEvent::TaskFailed { .. })),
                "{label}, drain {drain:?}: no failure should be reported: {posted:?}"
            );
            let events = docker.events();
            assert!(
                events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
                "{label}, drain {drain:?}: the task container should be removed: {events:?}"
            );
        }
    }
}

/// The same stopped container after a main command that exited non-zero:
/// the exit code is still the task's.
#[tokio::test(start_paused = true)]
async fn a_gate_failed_by_a_failing_main_commands_exit_keeps_its_exit_code() {
    let docker = FakeContainerRuntime::default()
        .with_gate_observing_exits()
        .failing_run()
        .with_run_delay("app", std::time::Duration::from_secs(1))
        .with_health_check_delay("app", std::time::Duration::from_secs(2));
    let engine = engine(config_with_a_gated_task_container(), docker);

    let err = engine.run_task("start", &[]).await.unwrap_err();

    assert_eq!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the main command's own exit code should be the task's: {err:#}"
    );
}

/// A verdict the gate reaches while the main command is still running — an
/// unhealthy status, a setup command exiting non-zero — fails the task,
/// cancelling that command, whether or not the gate also observes exits.
#[tokio::test(start_paused = true)]
async fn a_gate_verdict_reached_while_the_main_command_runs_fails_the_task() {
    for label in ["unhealthy", "setup command"] {
        let (config, docker) = match label {
            "unhealthy" => (
                config_with_a_gated_task_container(),
                FakeContainerRuntime::default().with_unhealthy_container("app"),
            ),
            _ => (
                config_with_failing_task_container_setup_command(),
                FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh"),
            ),
        };
        let running = docker
            .with_gate_observing_exits()
            .with_run_delay("app", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE);
        let err = tokio::time::timeout(
            GATE_FAILURE_DEADLINE,
            engine(config, running).run_task("start", &[]),
        )
        .await
        .unwrap_or_else(|_| panic!("{label}: the gate's failure should cancel the run"))
        .unwrap_err();
        let verdict = match label {
            "unhealthy" => "did not indicate that the container was healthy",
            _ => "exited with code 1",
        };
        assert!(
            format!("{err:#}").contains(verdict),
            "{label}: the gate's own verdict should be the task's error: {err:#}"
        );
    }
}

/// Once the main command has exited 0 the gate's outcome no longer counts,
/// whatever it was doing: a setup command the container's exit killed
/// reports a non-zero code (137 on a real daemon), which says nothing about
/// readiness (ratect#263, a listed divergence: Batect fails it). So does a
/// health wait still in flight. Covered with the gate still running when
/// the run ends (dropped), and with it failing while the run is still
/// draining output after the exit (forgiven once the run reports 0) —
/// the order a real daemon produced for the issue's reproduction. The cost
/// is a genuine failure landing in the same instant, which is lost too.
#[tokio::test(start_paused = true)]
async fn a_gate_still_in_flight_when_the_main_command_exits_zero_does_not_fail_the_task() {
    for drain in [None, Some(std::time::Duration::from_secs(10))] {
        for label in ["unhealthy", "setup command"] {
            let (config, docker) = match label {
                "unhealthy" => (
                    config_with_a_gated_task_container(),
                    FakeContainerRuntime::default()
                        .with_unhealthy_container("app")
                        .with_health_check_delay("app", std::time::Duration::from_secs(2)),
                ),
                _ => (
                    config_with_failing_task_container_setup_command(),
                    FakeContainerRuntime::default()
                        .with_failing_setup_command("./migrate.sh")
                        .with_exec_delay("./migrate.sh", std::time::Duration::from_secs(2)),
                ),
            };
            let mut docker = docker.with_run_delay("app", std::time::Duration::from_secs(1));
            if let Some(drain) = drain {
                docker = docker.with_run_draining_after_exit("app", drain);
            }
            let sink = RecordingEventSink::default();
            let engine = TaskEngine::new(
                project(config),
                docker.clone(),
                Arc::new(sink.clone()),
                crate::interrupt::Interrupt::new(),
            );

            let result = engine.run_task("start", &[]).await;

            assert!(
                result.is_ok(),
                "{label}, drain {drain:?}: the task should succeed: {result:?}"
            );
            let posted = sink.events();
            assert!(
                !posted
                    .iter()
                    .any(|e| matches!(e, TaskEvent::TaskFailed { .. })),
                "{label}, drain {drain:?}: no failure should be reported: {posted:?}"
            );
            let events = docker.events();
            assert!(
                events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
                "{label}, drain {drain:?}: the task container should be removed: {events:?}"
            );
        }
    }
}

/// Dropped, not waited on: a setup command that would never finish doesn't
/// hold up a task whose main command has exited 0 (ratect#263).
#[tokio::test(start_paused = true)]
async fn a_setup_command_still_running_does_not_hold_up_a_successful_task() {
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(1))
        .with_exec_delay("./migrate.sh", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE);
    let engine = engine(config_with_failing_task_container_setup_command(), docker);

    tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
        .await
        .expect("the task should end when its main command does, not when its gate does")
        .unwrap();
}

/// The same with any exit code: a main command exiting non-zero ends the
/// gate too, rather than the run waiting on a setup command that may never
/// finish before reporting the exit code that already decides the task. The
/// container is still cleaned up.
#[tokio::test(start_paused = true)]
async fn a_setup_command_still_running_does_not_hold_up_a_failed_task() {
    let docker = FakeContainerRuntime::default()
        .failing_run()
        .with_run_delay("app", std::time::Duration::from_secs(1))
        .with_exec_delay("./migrate.sh", MAIN_COMMAND_THAT_OUTLASTS_ITS_GATE);
    let engine = engine(
        config_with_failing_task_container_setup_command(),
        docker.clone(),
    );

    let err = tokio::time::timeout(GATE_FAILURE_DEADLINE, engine.run_task("start", &[]))
        .await
        .expect("the task should end when its main command does, not when its gate does")
        .unwrap_err();

    assert_eq!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the main command's own exit code should be the task's: {err:#}"
    );
    let events = docker.events();
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task container should be removed: {events:?}"
    );
}

/// Settings for a native-dialect project — the only thing that differs
/// from the default (Batect-compatible) engine in the tests below.
fn native_settings() -> TaskEngineSettings {
    TaskEngineSettings {
        dialect: Dialect::Native,
        ..Default::default()
    }
}

/// A task whose own container declares both steps of a readiness gate: a
/// `health_check` and one `setup_commands` entry (`./migrate.sh`).
fn config_with_a_gated_task_container() -> Config {
    let mut config = config_with_failing_task_container_setup_command();
    config.containers.get_mut("app").unwrap().health_check =
        Some(crate::config::HealthCheckConfig {
            command: Some("wget -q localhost".to_string()),
            interval: None,
            retries: None,
            start_period: None,
            timeout: None,
        });
    config
}

/// Under the native dialect a task's own container has no readiness gate
/// (decisions/0012): nothing waits on its health, no health event posts
/// for it, and its `setup_commands` never run — while its creation is
/// still announced and it is still cleaned up.
#[tokio::test]
async fn native_task_containers_own_readiness_gate_does_not_run() {
    let docker = FakeContainerRuntime::default();
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_a_gated_task_container()),
        docker.clone(),
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    )
    .with_settings(native_settings())
    .unwrap();

    engine.run_task("start", &[]).await.unwrap();

    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("wait-healthy:")),
        "nothing should wait on the task container's health: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("exec:")),
        "the task container's setup commands should not run: {events:?}"
    );
    assert!(
        events.contains(&"sidecar-stop:sidecar-id-app".to_string()),
        "the task's own container should still be removed: {events:?}"
    );
    let posted = sink.events();
    assert!(
        !posted
            .iter()
            .any(|e| matches!(e, TaskEvent::ContainerBecameHealthy { .. })),
        "no health event should post for the task container: {posted:?}"
    );
    assert!(
        posted
            .iter()
            .any(|e| matches!(e, TaskEvent::ContainerCreated { container } if container == "app")),
        "the task container's creation should still be announced: {posted:?}"
    );
}

#[tokio::test]
async fn native_task_container_that_would_be_unhealthy_still_succeeds() {
    let docker = FakeContainerRuntime::default().with_unhealthy_container("app");
    let engine = engine(config_with_a_gated_task_container(), docker)
        .with_settings(native_settings())
        .unwrap();

    engine.run_task("start", &[]).await.unwrap();
}

#[tokio::test]
async fn native_task_container_whose_setup_command_would_fail_still_succeeds() {
    let docker = FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh");
    let engine = engine(config_with_a_gated_task_container(), docker)
        .with_settings(native_settings())
        .unwrap();

    engine.run_task("start", &[]).await.unwrap();
}

#[tokio::test]
async fn native_task_container_exiting_nonzero_still_fails_the_task() {
    let docker = FakeContainerRuntime::default().failing_run();
    let engine = engine(config_with_a_gated_task_container(), docker)
        .with_settings(native_settings())
        .unwrap();

    let err = engine.run_task("start", &[]).await.unwrap_err();

    assert_eq!(
        err.downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the main command's own exit code should be the task's: {err:#}"
    );
}

/// The dialect only decides the *task's own* container's gate: a
/// dependency's is what everything after it waits on, so its failure fails
/// the task in both dialects.
#[tokio::test]
async fn a_dependencys_readiness_failure_fails_the_task_in_either_dialect() {
    for dialect in [Dialect::BatectCompatible, Dialect::Native] {
        let settings = || TaskEngineSettings {
            dialect,
            ..Default::default()
        };

        let unhealthy = FakeContainerRuntime::default().with_unhealthy_container("database");
        let unhealthy_engine = engine(config_with_database_dependency(|_| {}), unhealthy)
            .with_settings(settings())
            .unwrap();
        assert!(
            unhealthy_engine.run_task("start", &[]).await.is_err(),
            "{dialect:?}: an unhealthy dependency should fail the task"
        );

        let failing_setup =
            FakeContainerRuntime::default().with_failing_setup_command("./migrate.sh");
        let config = config_with_database_dependency(|database| {
            database.setup_commands = Some(vec![crate::config::SetupCommand {
                command: "./migrate.sh".to_string(),
                working_directory: None,
                run_in: None,
            }]);
        });
        let failing_setup_engine = engine(config, failing_setup)
            .with_settings(settings())
            .unwrap();
        assert!(
            failing_setup_engine.run_task("start", &[]).await.is_err(),
            "{dialect:?}: a dependency's failing setup command should fail the task"
        );

        // A dependency that dies before becoming healthy fails with the
        // same health-wait error the task container's gate forgives after
        // a successful exit (ratect#248); a dependency's never is.
        let died = FakeContainerRuntime::default().with_gate_observing_exits();
        died.exited_containers
            .lock()
            .unwrap()
            .insert("sidecar-id-database".to_string());
        let died_engine = engine(config_with_database_dependency(|_| {}), died)
            .with_settings(settings())
            .unwrap();
        let err = died_engine.run_task("start", &[]).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("The container exited before becoming healthy."),
            "{dialect:?}: a dependency exiting before becoming healthy should fail the task: \
             {err:#}"
        );
    }
}

#[tokio::test]
async fn task_fails_when_container_exits_nonzero_but_dependencies_are_still_cleaned_up() {
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "exit 1"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().failing_run();
    let engine = engine(config, docker.clone());

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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cleanup_after_success: false,
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cleanup_after_success: false,
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cleanup_after_failure: false,
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cleanup_after_failure: false,
            ..Default::default()
        })
        .unwrap();

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
    let mut containers = IndexMap::new();
    containers.insert("cache".to_string(), container("redis:7", None));
    containers.insert(
        "database".to_string(),
        container("postgres:16", Some(vec!["cache".to_string()])),
    );
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("dep-a".to_string(), container("alpine:3.18", None));
    containers.insert("dep-b".to_string(), container("alpine:3.18", None));
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker);

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
    let mut containers = IndexMap::new();
    containers.insert("dep-a".to_string(), container("shared-image:1", None));
    containers.insert("dep-b".to_string(), container("shared-image:1", None));
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("dep-a".to_string(), container("image-a:1", None));
    containers.insert("dep-b".to_string(), container("image-b:1", None));
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config_with_two_independent_image_pulls(), docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(1),
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config_with_two_independent_image_pulls(), docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(2),
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config_with_two_independent_image_pulls(), docker);

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < delay * 2,
        "with no --max-parallelism given, independent image pulls should overlap by \
             default, matching pre-existing behavior (elapsed: {elapsed:?})"
    );
}

/// A chain — the task's container depends on `b`, which depends on `a` —
/// with every image given the same pull delay. The three pulls overlap
/// (decisions/0019: image work waits for the network, not for a
/// dependency's readiness), so the run takes about one delay, not three.
fn config_with_a_dependency_chain_of_three_pulls() -> Config {
    let mut containers = IndexMap::new();
    containers.insert("a".to_string(), container("image-a:1", None));
    containers.insert(
        "b".to_string(),
        container("image-b:1", Some(vec!["a".to_string()])),
    );
    containers.insert(
        "app".to_string(),
        container("image-app:1", Some(vec!["b".to_string()])),
    );
    let mut tasks = IndexMap::new();
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
async fn pulls_along_a_dependency_chain_overlap() {
    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_pull_delay("image-a:1", delay)
        .with_pull_delay("image-b:1", delay)
        .with_pull_delay("image-app:1", delay);
    let engine = engine(config_with_a_dependency_chain_of_three_pulls(), docker);

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < delay * 2,
        "the three pulls along a dependency chain should overlap rather than each wait for \
         the previous container's readiness (elapsed: {elapsed:?})"
    );
}

#[tokio::test(start_paused = true)]
async fn max_parallelism_of_one_still_serializes_pulls_along_a_dependency_chain() {
    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_pull_delay("image-a:1", delay)
        .with_pull_delay("image-b:1", delay)
        .with_pull_delay("image-app:1", delay);
    let engine = engine(config_with_a_dependency_chain_of_three_pulls(), docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(1),
            ..Default::default()
        })
        .unwrap();

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed >= delay * 3,
        "with --max-parallelism 1, the three pulls still each hold the one permit in turn \
         (elapsed: {elapsed:?})"
    );
}

/// A dependency with a setup command, and a task container whose image
/// can't be pulled. The pull is attempted alongside the dependency's
/// startup, and its failure ends the run the moment it lands — here,
/// while the setup command is still in flight, so it never completes —
/// asserted on the fake's exec record, which only ever holds a command
/// that actually ran.
#[tokio::test(start_paused = true)]
async fn a_failed_pull_of_the_task_image_stops_a_dependency_before_its_setup_command() {
    let mut containers = IndexMap::new();
    let mut database = container("postgres:15", None);
    database.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./init.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("nope:does-not-exist", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    // The pull takes a moment to fail, and the setup command takes longer
    // than that to run: the failure lands while the exec is in flight.
    let docker = FakeContainerRuntime::default()
        .with_pull_delay("nope:does-not-exist", std::time::Duration::from_millis(100))
        .with_failing_pull("nope:does-not-exist")
        .with_exec_delay("./init.sh", std::time::Duration::from_secs(1));
    let engine = engine(config, docker.clone());

    let error = engine.run_task("start", &[]).await.unwrap_err();

    assert!(
        format!("{error:?}").contains("manifest for nope:does-not-exist not found"),
        "the run should fail with the pull error: {error:?}"
    );
    assert!(
        docker.exec_for("./init.sh").is_none(),
        "the dependency's setup command should never have run: {:?}",
        docker.events()
    );
}

/// The task container's build runs alongside its dependency's startup:
/// `ImageBuildStarting` for the task container is posted before the
/// dependency's `SetupCommandsCompleted`, and the task container still
/// doesn't run until both have finished.
#[tokio::test(start_paused = true)]
async fn a_build_starts_before_a_dependency_is_ready() {
    let mut containers = IndexMap::new();
    let mut database = container("postgres:15", None);
    database.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./init.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("database".to_string(), database);
    let mut app = container_with_build_directory("./docker", None);
    app.dependencies = Some(vec!["database".to_string()]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let sink = RecordingEventSink::default();
    let docker = FakeContainerRuntime::default()
        .with_build_delay("demo-app", std::time::Duration::from_millis(100));
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    engine.run_task("start", &[]).await.unwrap();

    let events = sink.events();
    let position = |wanted: &TaskEvent| {
        events
            .iter()
            .position(|event| event == wanted)
            .unwrap_or_else(|| panic!("missing {wanted:?} in {events:?}"))
    };
    let build_starting = position(&TaskEvent::ImageBuildStarting {
        container: "app".into(),
    });
    let build_completed = position(&TaskEvent::ImageBuildCompleted {
        container: "app".into(),
    });
    let setup_completed = position(&TaskEvent::SetupCommandsCompleted {
        container: "database".into(),
    });
    let running = position(&TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("echo hi".into()),
    });
    assert!(
        build_starting < setup_completed,
        "the task container's build should start before its dependency is ready: {events:?}"
    );
    assert!(
        build_completed < running && setup_completed < running,
        "the task container should run only once its build and its dependency have both \
         finished: {events:?}"
    );
}

/// Two `build_directory` containers, one depending on the other, each
/// wait for their *own* build: the dependency's start follows its own
/// build's completion, not the task container's (which finishes first).
#[tokio::test(start_paused = true)]
async fn each_built_container_waits_for_its_own_build() {
    let mut containers = IndexMap::new();
    containers.insert(
        "dep".to_string(),
        container_with_build_directory("./dep", None),
    );
    let mut app = container_with_build_directory("./app", None);
    app.dependencies = Some(vec!["dep".to_string()]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let dep_build = std::time::Duration::from_millis(300);
    let docker = FakeContainerRuntime::default()
        .with_build_delay("demo-app", std::time::Duration::from_millis(100))
        .with_build_delay("demo-dep", dep_build);
    let engine = engine(config, docker.clone());

    let start = tokio::time::Instant::now();
    engine.run_task("start", &[]).await.unwrap();
    let elapsed = start.elapsed();

    let events = docker.events();
    let position = |prefix: &str| {
        events
            .iter()
            .position(|event| event.starts_with(prefix))
            .unwrap_or_else(|| panic!("missing {prefix} in {events:?}"))
    };
    assert!(
        position("build:demo-app:") < position("build:demo-dep:")
            && position("build:demo-dep:") < position("sidecar-start:dep:"),
        "dep should start after its own build, not after app's: {events:?}"
    );
    assert!(
        elapsed >= dep_build,
        "dep's start should have waited for its own build (elapsed: {elapsed:?})"
    );
}

/// The network comes first, and stays first (a recorded divergence from
/// Batect, which creates it alongside the pulls): a failure to create it
/// is the run's error, before any pull or build has been attempted.
#[tokio::test]
async fn a_failed_network_creation_attempts_no_pull_or_build() {
    let mut containers = IndexMap::new();
    containers.insert(
        "dep".to_string(),
        container_with_build_directory("./dep", None),
    );
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["dep".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().failing_network_creation();
    let engine = engine(config, docker.clone());

    let error = engine.run_task("start", &[]).await.unwrap_err();

    assert!(
        format!("{error:?}").contains("non-overlapping IPv4 address pool"),
        "the run should fail with the network error: {error:?}"
    );
    let image_work: Vec<_> = docker
        .events()
        .iter()
        .filter(|event| event.starts_with("pull:") || event.starts_with("build:"))
        .cloned()
        .collect();
    assert!(
        image_work.is_empty(),
        "no image should be pulled or built once the network has failed: {image_work:?}"
    );
}

/// Shared by the three `max_parallelism` tests below covering start/
/// setup-command/health-check concurrency: two independent dependencies
/// (no relationship to each other), same shape as
/// `config_with_two_independent_image_pulls` but sharing one image
/// (irrelevant here — nothing in these tests is keyed by image name).
fn config_with_two_independent_dependencies() -> Config {
    let mut containers = IndexMap::new();
    containers.insert("dep-a".to_string(), container("alpine:3.18", None));
    containers.insert("dep-b".to_string(), container("alpine:3.18", None));
    containers.insert(
        "app".to_string(),
        container(
            "alpine:3.18",
            Some(vec!["dep-a".to_string(), "dep-b".to_string()]),
        ),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config_with_two_independent_dependencies(), docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(1),
            ..Default::default()
        })
        .unwrap();

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
            run_in: None,
        }]);
    config.containers.get_mut("dep-b").unwrap().setup_commands =
        Some(vec![crate::config::SetupCommand {
            command: "setup-b".to_string(),
            working_directory: None,
            run_in: None,
        }]);

    let delay = std::time::Duration::from_millis(100);
    let docker = FakeContainerRuntime::default()
        .with_exec_delay("setup-a", delay)
        .with_exec_delay("setup-b", delay);
    let engine = engine(config, docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(1),
            ..Default::default()
        })
        .unwrap();

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
    let engine = engine(config_with_two_independent_dependencies(), docker)
        .with_settings(TaskEngineSettings {
            max_parallelism: Some(1),
            ..Default::default()
        })
        .unwrap();

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
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("cache".to_string(), container("redis:7", None));
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["cache".to_string()])),
    );
    containers.insert("queue".to_string(), container("redis:7", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("cache".to_string(), container("redis:7", None));
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["cache".to_string()])),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
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
            run_to_completion: None,
            external_health_check: None,
            reports_readiness_for: None,
            stop_signal: None,
            stop_grace_period: None,
            ulimits: None,
            dns: None,
            dns_search: None,
            dns_options: None,
            network_mode: None,
        },
    );
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker);

    let err = engine.run_task("start", &[]).await.unwrap_err();
    assert!(err
        .to_string()
        .contains("Container 'database' has neither 'image' nor 'build_directory' set"));
}

#[tokio::test]
async fn detects_circular_container_dependency() {
    let mut containers = IndexMap::new();
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
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker);

    let err = engine.run_task("start", &[]).await.unwrap_err();
    assert!(err
        .to_string()
        .contains("Circular container dependency detected"));
}

#[tokio::test]
async fn detects_dependency_cycle() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(config_with_cycle(), docker);

    let err = engine.run_task("a", &[]).await.unwrap_err();
    assert!(err.to_string().contains("Dependency cycle detected"));
}

#[tokio::test]
async fn missing_task_returns_error() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(empty_config(), docker);

    let err = engine.run_task("does-not-exist", &[]).await.unwrap_err();
    assert!(err.to_string().contains("Task 'does-not-exist' not found"));
}

#[tokio::test]
async fn a_slightly_misspelled_task_name_suggests_the_real_one() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(config_with_shared_prerequisite(), docker);

    let err = engine.run_task("tst-task", &[]).await.unwrap_err();
    assert!(
        err.to_string().contains("Did you mean 'test-task'?"),
        "error should suggest the close match: {err}"
    );
}

#[tokio::test]
async fn a_wildly_misspelled_task_name_suggests_nothing() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(config_with_shared_prerequisite(), docker);

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
    let mut tasks = IndexMap::new();
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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut task_config = task("build-env", "echo hi");
    task_config.run.as_mut().unwrap().environment = Some(HashMap::from([(
        "RUN_VAR".to_string(),
        "run-value".to_string(),
    )]));
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task_config);

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut task_config = task("build-env", "echo hi");
    task_config.run.as_mut().unwrap().environment = Some(HashMap::from([(
        "SHARED".to_string(),
        "from-run".to_string(),
    )]));
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task_config);

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    let environment = docker.environment_for("build-env").unwrap();
    assert_eq!(environment.get("SHARED"), Some(&"from-run".to_string()));
}

#[tokio::test]
async fn container_working_directory_reaches_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.working_directory = Some("/app".to_string());
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut task_config = task("build-env", "echo hi");
    task_config.run.as_mut().unwrap().working_directory = Some("/from-run".to_string());
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task_config);

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    let events = docker.events();
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("run:build-env:/from-container:args=[]:")),
        "the container's own command should reach the run when run.command is unset: {events:?}"
    );
}

/// #417 (Batect's TSF-10): with no `run.command`, additional arguments are
/// appended to the task container's own `command`, each kept whole — and,
/// as for a `run.command`, only the named task's own container gets them,
/// not a dependency or a prerequisite task's container.
#[tokio::test]
async fn additional_args_follow_the_containers_own_command_when_run_command_is_unset() {
    let with_command = |command: &str| {
        let mut container_config = container("alpine:3.18", None);
        container_config.command = Some(command.to_string());
        container_config
    };
    let run_without_command = |container: &str| TaskRun {
        container: container.to_string(),
        command: None,
        environment: None,
        ports: None,
        working_directory: None,
        entrypoint: None,
    };
    let mut containers = IndexMap::new();
    containers.insert("tool".to_string(), with_command("echo from-container"));
    containers.insert("db".to_string(), with_command("db-server"));
    containers.insert("prep".to_string(), with_command("prepare"));

    let mut tasks = IndexMap::new();
    tasks.insert(
        "setup".to_string(),
        Task {
            run: Some(run_without_command("prep")),
            dependencies: None,
            prerequisites: None,
            description: None,
            group: None,
            customise: None,
        },
    );
    tasks.insert(
        "main".to_string(),
        Task {
            run: Some(run_without_command("tool")),
            dependencies: Some(vec!["db".to_string()]),
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
    let engine = engine(config, docker.clone());

    let args = vec!["extra1".to_string(), "two words".to_string()];
    engine.run_task("main", &args).await.unwrap();

    let tool = docker.spec_for("tool").unwrap();
    assert_eq!(tool.shared.command.as_deref(), Some("echo from-container"));
    assert_eq!(tool.additional_args, args);
    let db = docker.spec_for("db").unwrap();
    assert_eq!(db.shared.command.as_deref(), Some("db-server"));
    assert!(db.additional_args.is_empty(), "{:?}", db.additional_args);
    let prep = docker.spec_for("prep").unwrap();
    assert_eq!(prep.shared.command.as_deref(), Some("prepare"));
    assert!(
        prep.additional_args.is_empty(),
        "{:?}",
        prep.additional_args
    );
}

#[tokio::test]
async fn container_entrypoint_reaches_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.entrypoint = Some("/bin/sh -c".to_string());
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut app = container("alpine:3.18", None);
    app.dependencies = Some(vec!["database".to_string()]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("check".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut app = container("alpine:3.18", None);
    app.dependencies = Some(vec!["database".to_string()]);
    containers.insert("app".to_string(), app);
    let mut tasks = IndexMap::new();
    tasks.insert("check".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            existing_network: Some("someone-elses-network".to_string()),
            ..Default::default()
        })
        .unwrap();
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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(docker.privileged_for("build-env"), Some(true));
}

#[tokio::test]
async fn container_shm_size_reaches_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.shm_size = Some(128 * 1024 * 1024);
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(docker.enable_init_process_for("build-env"), Some(true));
}

#[tokio::test]
async fn container_log_driver_and_log_options_reach_the_container() {
    let mut container_config = container("alpine:3.18", None);
    container_config.log_driver = Some("json-file".to_string());
    container_config.log_options =
        Some(HashMap::from([("max-size".to_string(), "10m".to_string())]));
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
            compact: false,
        }),
        crate::config::VolumeMount::Tmpfs(crate::config::TmpfsVolumeMount {
            container: "/code/tmp".to_string(),
            options: Some("size=64m".to_string()),
        }),
    ]);
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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

fn local_mount(local: &str, container: &str, options: Option<&str>) -> crate::config::VolumeMount {
    crate::config::VolumeMount::Local(crate::config::LocalVolumeMount {
        local: local.to_string(),
        container: container.to_string(),
        options: options.map(str::to_string),
        compact: false,
    })
}

/// Runs a one-container task whose container has `volumes`, returning the
/// bind strings its container was created with.
async fn binds_for_volumes(volumes: Vec<crate::config::VolumeMount>) -> Option<Vec<String>> {
    let mut container_config = container("alpine:3.18", None);
    container_config.volumes = Some(volumes);
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let project = std::env::temp_dir().join(format!("ratect-binds-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&project).unwrap();
    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            cache: Some((crate::cache::CacheType::Volume, project.clone())),
            ..Default::default()
        })
        .unwrap();
    engine.run_task("test", &[]).await.unwrap();
    std::fs::remove_dir_all(&project).ok();
    docker.volumes_for("build-env")
}

/// Batect holds a container's mounts as a set, so one listed twice is
/// mounted once. Passing both on makes the daemon refuse the create with
/// `Duplicate mount point` (ratect#390).
#[tokio::test]
async fn a_local_volume_listed_twice_is_bound_once() {
    let binds = binds_for_volumes(vec![
        local_mount("/host/a", "/a", None),
        local_mount("/host/a", "/a", None),
    ])
    .await;

    assert_eq!(binds, Some(vec!["/host/a:/a".to_string()]));
}

/// Compared once resolved, so two spellings of one host path collapse.
#[tokio::test]
async fn two_spellings_of_one_host_path_are_bound_once() {
    let binds = binds_for_volumes(vec![
        local_mount("./a", "/a", None),
        local_mount("a", "/a", None),
    ])
    .await;

    assert_eq!(binds, Some(vec!["/project/a:/a".to_string()]));
}

#[tokio::test]
async fn a_cache_volume_listed_twice_is_bound_once() {
    let cache = crate::config::VolumeMount::Cache(crate::config::CacheVolumeMount {
        name: "deps".to_string(),
        container: "/deps".to_string(),
        options: None,
        scope: Default::default(),
    });

    let binds = binds_for_volumes(vec![cache.clone(), cache]).await.unwrap();

    assert_eq!(binds.len(), 1, "{binds:?}");
    assert!(binds[0].ends_with(":/deps"), "{binds:?}");
}

#[tokio::test]
async fn deduplicated_binds_keep_the_order_each_was_first_listed_in() {
    let binds = binds_for_volumes(vec![
        local_mount("/host/b", "/b", None),
        local_mount("/host/a", "/a", None),
        local_mount("/host/b", "/b", None),
        local_mount("/host/c", "/c", None),
        local_mount("/host/a", "/a", None),
    ])
    .await;

    assert_eq!(
        binds,
        Some(vec![
            "/host/b:/b".to_string(),
            "/host/a:/a".to_string(),
            "/host/c:/c".to_string(),
        ])
    );
}

/// Only an identical mount collapses: Batect compares the options too, so
/// the same paths with different options are both kept.
#[tokio::test]
async fn the_same_paths_with_different_options_are_both_bound() {
    let binds = binds_for_volumes(vec![
        local_mount("/host/a", "/a", None),
        local_mount("/host/a", "/a", Some("ro")),
    ])
    .await;

    assert_eq!(
        binds,
        Some(vec!["/host/a:/a".to_string(), "/host/a:/a:ro".to_string()])
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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(
        docker.tmpfs_for("build-env"),
        Some(vec![("/code/tmp".to_string(), String::new())])
    );
}

#[tokio::test]
async fn if_not_present_policy_pulls_when_the_image_is_missing_locally() {
    // No image_pull_policy set — IfNotPresent is the default.
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert!(docker.events().contains(&"pull:alpine:3.18".to_string()));
}

#[tokio::test]
async fn if_not_present_policy_skips_the_pull_when_the_image_already_exists_locally() {
    let mut container_config = container("alpine:3.18", None);
    container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::IfNotPresent);
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().with_local_image("alpine:3.18");
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert!(!docker.events().contains(&"pull:alpine:3.18".to_string()));
}

#[tokio::test]
async fn always_policy_pulls_even_when_the_image_already_exists_locally() {
    let mut container_config = container("alpine:3.18", None);
    container_config.image_pull_policy = Some(crate::config::ImagePullPolicy::Always);
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default().with_local_image("alpine:3.18");
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert!(docker.events().contains(&"pull:alpine:3.18".to_string()));
}

#[tokio::test]
async fn image_override_pulls_the_override_instead_of_the_configured_image() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            image_overrides: overrides,
            ..Default::default()
        })
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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            image_overrides: overrides,
            ..Default::default()
        })
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
    let mut containers = IndexMap::new();
    let mut container_config = container("unused-if-overridden", None);
    container_config.image = None;
    container_config.build_directory = Some(".".to_string());
    containers.insert("build-env".to_string(), container_config);
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            image_overrides: overrides,
            ..Default::default()
        })
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
fn with_settings_rejects_an_unknown_image_override_container_name() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let err = match engine(config, docker).with_settings(TaskEngineSettings {
        image_overrides: overrides,
        ..Default::default()
    }) {
        Ok(_) => panic!("expected with_settings to reject an unknown container name"),
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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: Some(IndexMap::from([(
            "flavour".to_string(),
            crate::config::ConfigVariable {
                description: None,
                default: None,
            },
        )])),
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
        dialect: Dialect::Native,
        config_var_overrides: HashMap::from([("flavour".to_string(), "spicy".to_string())]),
    };
    let engine = engine(config, FakeContainerRuntime::default())
        .with_settings(settings)
        .expect("settings naming a real container should apply");

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
    assert!(!engine.task_container_readiness_gate);
    assert_eq!(engine.config_var_overrides["flavour"], "spicy");
}

/// The default is "no flags given at all", so an engine built from it
/// has to behave exactly like one built with no builder calls — every
/// binary's no-flags path depends on that.
#[test]
fn default_settings_leave_an_engine_in_its_no_flags_state() {
    let config = Config {
        project_name: "demo".to_string(),
        containers: IndexMap::new(),
        tasks: IndexMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };
    let engine = engine(config, FakeContainerRuntime::default())
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
    assert!(engine.task_container_readiness_gate);
    assert!(engine.config_var_overrides.is_empty());
}

/// The one setting that can fail — `with_settings` returns a `Result`
/// solely because of it, and the error has to survive the indirection.
#[test]
fn with_settings_still_rejects_an_unknown_image_override() {
    let config = Config {
        project_name: "demo".to_string(),
        containers: IndexMap::new(),
        tasks: IndexMap::new(),
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
    let error = match engine(config, FakeContainerRuntime::default()).with_settings(settings) {
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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory(".", None),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            image_tags: tags,
            ..Default::default()
        })
        .unwrap();

    engine.run_task("test", &[]).await.unwrap();

    let events = docker.events();
    assert!(
        events.contains(&"tag:demo-build-env:my.registry/build-env:v1".to_string()),
        "{events:?}"
    );
}

fn config_building(project_name: &str, container_name: &str) -> Config {
    let mut containers = IndexMap::new();
    containers.insert(
        container_name.to_string(),
        container_with_build_directory(".", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task(container_name, "echo hi"));
    Config {
        project_name: project_name.to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

/// `--tag-image` values are checked at argument parse, but `image_tags` is a
/// library setting too; the engine checks every tag before it builds, so a
/// bad one costs no build and leaves no tag half-applied.
#[tokio::test]
async fn an_invalid_extra_tag_fails_before_the_image_is_built_or_tagged() {
    let docker = FakeContainerRuntime::default();
    let tags = HashMap::from([(
        "build-env".to_string(),
        HashSet::from(["good:1".to_string(), "_nonsense".to_string()]),
    )]);
    let engine = engine(config_building("demo", "build-env"), docker.clone())
        .with_settings(TaskEngineSettings {
            image_tags: tags,
            ..Default::default()
        })
        .unwrap();

    let error = engine.run_task("test", &[]).await.unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains("'_nonsense' is not a valid image tag: invalid reference format"),
        "{message}"
    );
    assert!(message.contains("container 'build-env'"), "{message}");
    let events = docker.events();
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("build:") || e.starts_with("tag:")),
        "{events:?}"
    );
}

/// ADR 0018: once every tag has passed the grammar check, a tag can fail only
/// on the daemon, and every one that does is reported — the rest are still
/// applied.
#[tokio::test]
async fn every_tag_the_daemon_refuses_is_reported_and_the_rest_applied() {
    let docker = FakeContainerRuntime::default()
        .rejecting_tag("a:1")
        .rejecting_tag("c:1");
    let tags = HashMap::from([(
        "build-env".to_string(),
        HashSet::from(["a:1".to_string(), "b:1".to_string(), "c:1".to_string()]),
    )]);
    let engine = engine(config_building("demo", "build-env"), docker.clone())
        .with_settings(TaskEngineSettings {
            image_tags: tags,
            ..Default::default()
        })
        .unwrap();

    let error = engine.run_task("test", &[]).await.unwrap_err();

    let message = format!("{error:#}");
    for line in [
        "The image was built, but applying its tags failed:",
        "the daemon refused 'a:1'",
        "the daemon refused 'c:1'",
    ] {
        assert!(message.lines().any(|l| l.trim() == line), "{message}");
    }
    let events = docker.events();
    for tag in ["a:1", "b:1", "c:1"] {
        assert!(
            events.contains(&format!("tag:demo-build-env:{tag}")),
            "every tag should be attempted: {events:?}"
        );
    }
}

/// Project and container names are checked at load (#290), but two valid
/// ones can still make an automatic tag Docker refuses — one too long — so
/// the tag is checked here, before the build, naming the fields it is made
/// from.
#[tokio::test]
async fn an_invalid_automatic_tag_fails_before_the_image_is_built() {
    let project_name = "a".repeat(200);
    let container_name = "b".repeat(60);
    let docker = FakeContainerRuntime::default();
    let engine = engine(
        config_building(&project_name, &container_name),
        docker.clone(),
    );

    let error = engine.run_task("test", &[]).await.unwrap_err();

    let message = format!("{error:#}");
    assert!(
        message.contains(&format!(
            "'{project_name}-{container_name}', the tag made from project_name \
             '{project_name}' and container name '{container_name}', is not a valid image \
             tag: repository name must not be more than 255 characters"
        )),
        "{message}"
    );
    let events = docker.events();
    assert!(
        !events.iter().any(|e| e.starts_with("build:")),
        "{events:?}"
    );
}

#[tokio::test]
async fn tag_image_errors_immediately_when_the_container_uses_a_pulled_image() {
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker)
        .with_settings(TaskEngineSettings {
            image_tags: tags,
            ..Default::default()
        })
        .unwrap();

    let err = engine.run_task("test", &[]).await.unwrap_err();

    assert_eq!(
        err.to_string(),
        "The image built for container 'build-env' was requested to be tagged with \
             --tag-image, but 'build-env' uses a pulled image."
    );
}

#[tokio::test]
async fn tag_image_errors_immediately_when_an_override_image_replaces_a_build_with_a_pull() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory(".", None),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker)
        .with_settings(TaskEngineSettings {
            image_overrides: overrides,
            image_tags: tags,
            ..Default::default()
        })
        .unwrap();

    let err = engine.run_task("test", &[]).await.unwrap_err();

    assert_eq!(
        err.to_string(),
        "The image built for container 'build-env' was requested to be tagged with \
             --tag-image, but 'build-env' uses a pulled image."
    );
}

#[tokio::test]
async fn tag_image_errors_once_the_task_finishes_if_the_tagged_container_never_ran() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory(".", None),
    );
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker)
        .with_settings(TaskEngineSettings {
            image_tags: tags,
            ..Default::default()
        })
        .unwrap();

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "/from-run"));

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container_config);

    let mut task_config = task("build-env", "echo hi");
    task_config.run.as_mut().unwrap().entrypoint = Some("/from-run".to_string());
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task_config);

    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("test", &[]).await.unwrap();

    assert_eq!(
        docker.entrypoint_for("build-env"),
        Some("/from-run".to_string())
    );
}

#[tokio::test]
async fn proxy_environment_variables_reach_a_tasks_own_container() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone()).with_host_env(|name| {
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container_config);
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone()).with_host_env(|name| {
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_host_env(|name| {
            (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
        })
        .with_settings(TaskEngineSettings {
            propagate_proxy_environment_variables: false,
            ..Default::default()
        })
        .unwrap();

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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(proxy_warning_config(), docker)
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
    let engine = engine(proxy_warning_config(), docker)
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
    let engine = engine(proxy_warning_config(), docker)
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
    let engine = engine(proxy_warning_config(), docker)
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
        .with_proc_net_tcp(|| vec![proc_net_tcp("0100007F", 3333)])
        .with_settings(TaskEngineSettings {
            propagate_proxy_environment_variables: false,
            ..Default::default()
        })
        .unwrap();

    assert!(engine.unreachable_proxy_ports().is_empty());
}

/// The platform default, and the reason the seam above exists: with no
/// `/proc` to read there is nothing to conclude, and the run must say
/// nothing rather than guess.
#[test]
fn a_host_with_no_proc_net_tcp_reports_nothing() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(proxy_warning_config(), docker)
        .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
        .with_proc_net_tcp(Vec::new);

    assert!(engine.unreachable_proxy_ports().is_empty());
}

/// Rewriting the URL to name the host is only useful if the name resolves,
/// and on Linux nothing supplies it — so the run that rewrites must also
/// add the entry, or it has swapped one unreachable URL for another.
#[tokio::test]
async fn a_rewritten_proxy_url_adds_the_host_gateway_to_a_tasks_own_container() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
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
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_host_env(|name| (name == "http_proxy").then(|| "http://127.0.0.1:3333".to_string()));

    engine.run_task("run", &[]).await.unwrap();

    assert_eq!(docker.host_gateway_for("database"), Some(HOST_GATEWAY));
}

/// A `RUN` step reaches the proxy through the same rewritten URL the
/// container will, so a build without the entry fails exactly where the
/// image it produces would have worked.
#[tokio::test]
async fn a_rewritten_proxy_url_adds_the_host_gateway_to_an_image_build() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container_with_build_directory(".", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone()).with_host_env(|name| {
        (name == "http_proxy").then(|| "http://proxy.example.com:8080".to_string())
    });

    engine.run_task("run", &[]).await.unwrap();

    assert!(docker.environment_for("app").is_some());
    assert_eq!(docker.host_gateway_for("app"), None);
}

#[tokio::test]
async fn a_dependencys_name_is_exempted_from_the_tasks_own_no_proxy() {
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone()).with_host_env(|name| {
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    containers.insert("queue".to_string(), container("redis:7", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone()).with_host_env(|name| {
        (name == "http_proxy").then(|| "http://proxy.example.com".to_string())
    });

    engine.run_task("run", &[]).await.unwrap();

    let app_no_proxy = docker.environment_for("app").unwrap();
    let app_no_proxy = app_no_proxy.get("no_proxy").unwrap();
    assert!(app_no_proxy.split(',').any(|entry| entry == "queue"));
}

#[tokio::test]
async fn customise_overrides_a_dependencys_working_directory_environment_and_ports() {
    let mut containers = IndexMap::new();
    let mut database = container_with_ports("postgres:16", vec![single_port(5432, 5432, "tcp")]);
    database.environment = Some(HashMap::from([("BASE".to_string(), "base".to_string())]));
    database.working_directory = Some("/from-container".to_string());
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    let mut run_task = task("app", "echo hi");
    run_task.customise = Some(IndexMap::from([(
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
    let engine = engine(config, docker.clone());

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

/// A config whose only unevaluable value sits in `deploy`, a container no
/// task here uses — what decisions/0014's per-task evaluation is for.
fn config_with_an_unevaluable_container(unevaluable: &str) -> Config {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut deploy = container("alpine:3.18", None);
    deploy.environment = Some(HashMap::from([(
        "PROFILE".to_string(),
        "$RATECT_TEST_UNSET".to_string(),
    )]));
    containers.insert(unevaluable.to_string(), deploy);
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("app", "echo build"));
    let mut test = task("app", "echo test");
    test.prerequisites = Some(vec!["build".to_string()]);
    tasks.insert("test".to_string(), test);
    tasks.insert("deploy".to_string(), task(unevaluable, "echo deploy"));
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

#[tokio::test]
async fn a_container_outside_the_task_graph_is_never_evaluated() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(
        config_with_an_unevaluable_container("deploy"),
        docker.clone(),
    )
    .with_host_env(|_| None);

    engine
        .run_task("test", &[])
        .await
        .expect("'deploy' is in neither task's graph, so its value is never needed");
}

#[tokio::test]
async fn an_unevaluable_value_fails_its_task_before_any_docker_work_for_it() {
    let docker = FakeContainerRuntime::default();
    let engine = engine(
        config_with_an_unevaluable_container("deploy"),
        docker.clone(),
    )
    .with_host_env(|_| None);

    let error = engine
        .run_task("deploy", &[])
        .await
        .expect_err("'deploy' reads a variable this machine doesn't have");

    let message = format!("{error:#}");
    for named in ["'deploy'", "'environment.PROFILE'", "'$RATECT_TEST_UNSET'"] {
        assert!(message.contains(named), "should name {named}: {message}");
    }
    assert_eq!(docker.events(), Vec::<String>::new());
}

/// Batect evaluates a task only once it is reached, so a prerequisite that
/// fails on its own is what the run reports — not a later task's bad value.
#[tokio::test]
async fn a_failing_prerequisite_is_reported_before_a_later_tasks_unevaluable_value() {
    let mut config = config_with_an_unevaluable_container("deploy");
    config.tasks.get_mut("deploy").unwrap().prerequisites = Some(vec!["build".to_string()]);
    let docker = FakeContainerRuntime::default().failing_run();
    let engine = engine(config, docker.clone()).with_host_env(|_| None);

    let error = engine
        .run_task("deploy", &[])
        .await
        .expect_err("the prerequisite fails");

    assert!(
        error.is::<crate::docker::ContainerExitedNonZero>(),
        "the prerequisite's own failure, not deploy's value: {error:#}"
    );
    assert_eq!(crate::exit_code::for_error(&error), 1);
}

/// `test`, whose container `app` runs only after its prerequisite `prepare`
/// has run in `prep` — separate containers, so the fake can give each its
/// own exit code. `builder` is built from a directory and used by neither
/// task, so tagging it with `--tag-image` can never be satisfied.
fn config_with_a_prerequisite_in_its_own_container() -> Config {
    let mut containers = IndexMap::new();
    containers.insert("prep".to_string(), container("alpine:3.18", None));
    containers.insert("app".to_string(), container("alpine:3.18", None));
    containers.insert(
        "builder".to_string(),
        container_with_build_directory(".", None),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("prepare".to_string(), task("prep", "echo prepare"));
    let mut test = task("app", "echo test");
    test.prerequisites = Some(vec!["prepare".to_string()]);
    tasks.insert("test".to_string(), test);
    Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    }
}

fn ran(docker: &FakeContainerRuntime, container: &str) -> bool {
    let prefix = format!("run:{container}:");
    docker
        .events()
        .iter()
        .any(|event| event.starts_with(&prefix))
}

/// Mirrors Batect's `SessionRunnerSpec`, "the task has a prerequisite" /
/// "the dependency finishes with an exit code of 0": "returns the exit code
/// of the main task" — the run's code is the main task's, not the
/// prerequisite's.
#[tokio::test]
async fn a_main_task_failing_after_its_prerequisite_succeeds_exits_with_its_own_code() {
    let docker = FakeContainerRuntime::default().with_run_exit("app", 42);
    let engine = engine(
        config_with_a_prerequisite_in_its_own_container(),
        docker.clone(),
    );

    let error = engine
        .run_task("test", &[])
        .await
        .expect_err("the main task's command exits 42");

    assert_eq!(
        error
            .downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(42),
        "the main task's own exit code: {error:#}"
    );
    assert!(
        ran(&docker, "prep"),
        "the prerequisite ran: {:?}",
        docker.events()
    );
    assert!(
        ran(&docker, "app"),
        "the main task ran: {:?}",
        docker.events()
    );
}

/// Mirrors Batect's `SessionRunnerSpec`, "the task has a prerequisite" /
/// "and the dependency finishes with a non-zero exit code": "returns the exit
/// code of the dependency task". The run stops there, so the check that every
/// `--tag-image` container ran is never reached, and its error never replaces
/// that exit code.
#[tokio::test]
async fn a_failing_prerequisite_skips_the_untagged_image_check() {
    let docker = FakeContainerRuntime::default().failing_run();
    let tags = HashMap::from([(
        "builder".to_string(),
        HashSet::from(["my.registry/builder:v1".to_string()]),
    )]);
    let engine = engine(
        config_with_a_prerequisite_in_its_own_container(),
        docker.clone(),
    )
    .with_settings(TaskEngineSettings {
        image_tags: tags,
        ..Default::default()
    })
    .unwrap();

    let error = engine
        .run_task("test", &[])
        .await
        .expect_err("the prerequisite fails");

    assert_eq!(
        error
            .downcast_ref::<crate::docker::ContainerExitedNonZero>()
            .map(|exited| exited.exit_code),
        Some(1),
        "the prerequisite's own exit code, not the untagged-image error: {error:#}"
    );
    assert!(
        ran(&docker, "prep"),
        "the prerequisite ran: {:?}",
        docker.events()
    );
    assert!(
        !ran(&docker, "app"),
        "the main task must not run: {:?}",
        docker.events()
    );
}

/// Mirrors Batect's `SessionRunnerSpec`, "the task has a prerequisite" /
/// "and the dependency finishes with a non-zero exit code": "does not run
/// the main task". The simple and fancy loggers print the blank line between
/// tasks only when the next `TaskStarting` arrives, so the main task's never
/// being posted is what keeps a failed prerequisite from being followed by
/// one.
#[tokio::test]
async fn a_failing_prerequisite_posts_no_task_starting_for_the_main_task() {
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_a_prerequisite_in_its_own_container()),
        FakeContainerRuntime::default().failing_run(),
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    engine
        .run_task("test", &[])
        .await
        .expect_err("the prerequisite fails");

    let starting: Vec<TaskEvent> = sink
        .events()
        .into_iter()
        .filter(|event| matches!(event, TaskEvent::TaskStarting { .. }))
        .collect();
    assert_eq!(
        starting,
        vec![TaskEvent::TaskStarting {
            task: "prepare".into()
        }]
    );
}

#[tokio::test]
async fn a_tasks_values_are_evaluated_against_the_host_and_its_config_variables() {
    let mut containers = IndexMap::new();
    let mut database = container("postgres:16", None);
    database.environment = Some(HashMap::from([(
        "FROM_CONTAINER".to_string(),
        "$RATECT_TEST_HOST".to_string(),
    )]));
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut run_task = task("app", "echo hi");
    run_task.run.as_mut().unwrap().environment = Some(HashMap::from([(
        "FROM_RUN".to_string(),
        "<flavour".to_string(),
    )]));
    run_task.customise = Some(IndexMap::from([(
        "database".to_string(),
        TaskContainerCustomisation {
            environment: Some(HashMap::from([(
                "FROM_CUSTOMISE".to_string(),
                "${RATECT_TEST_UNSET:-fallback}".to_string(),
            )])),
            ports: None,
            working_directory: None,
        },
    )]));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks: IndexMap::from([("run".to_string(), run_task)]),
        config_variables: Some(IndexMap::from([(
            "flavour".to_string(),
            crate::config::ConfigVariable {
                description: None,
                default: Some("plain".to_string()),
            },
        )])),
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_settings(TaskEngineSettings {
            config_var_overrides: HashMap::from([("flavour".to_string(), "spicy".to_string())]),
            ..TaskEngineSettings::default()
        })
        .unwrap()
        .with_host_env(|name| (name == "RATECT_TEST_HOST").then(|| "from-host".to_string()));

    engine.run_task("run", &[]).await.unwrap();

    let database_env = docker.environment_for("database").unwrap();
    assert_eq!(database_env["FROM_CONTAINER"], "from-host");
    assert_eq!(database_env["FROM_CUSTOMISE"], "fallback");
    assert_eq!(docker.environment_for("app").unwrap()["FROM_RUN"], "spicy");
}

#[test]
fn with_settings_rejects_an_override_for_an_undeclared_config_variable() {
    let error = engine(
        config_with_an_unevaluable_container("deploy"),
        FakeContainerRuntime::default(),
    )
    .with_settings(TaskEngineSettings {
        config_var_overrides: HashMap::from([("nope".to_string(), "x".to_string())]),
        ..TaskEngineSettings::default()
    })
    .err()
    .expect("'nope' isn't declared");

    assert!(format!("{error:#}").contains("'nope'"), "{error:#}");
}

#[tokio::test]
async fn term_env_var_reaches_a_tasks_own_container_when_interactive() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
        .with_host_env(|name| (name == "TERM").then(|| "xterm-256color".to_string()));

    engine.run_task("run", &[]).await.unwrap();

    let environment = docker.environment_for("app").unwrap();
    assert_eq!(environment.get("TERM"), Some(&"xterm-256color".to_string()));
}

#[tokio::test]
async fn term_env_var_is_absent_when_host_has_no_term_set() {
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone()).with_host_env(|_| None);

    engine.run_task("run", &[]).await.unwrap();

    assert_eq!(
        docker.environment_for("app"),
        None,
        "an absent host TERM shouldn't inject an empty/placeholder value"
    );
}

#[tokio::test]
async fn term_env_var_does_not_reach_a_dependency_container() {
    let mut containers = IndexMap::new();
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    containers.insert("database".to_string(), container("postgres:16", None));
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container_config);
    let mut tasks = IndexMap::new();
    tasks.insert("run".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone())
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
    let mut containers = IndexMap::new();
    containers.insert("app".to_string(), container("alpine:3.18", None));
    containers.insert("setup".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = engine(config, docker.clone())
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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container_with_build_directory("./docker", Some(build_args)),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("build".to_string(), task("build-env", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone()).with_host_env(|name| match name {
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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    let labels = docker.labels_for("database").expect("labels should be set");
    assert_eq!(labels["com.example.role"], "database");
    assert_eq!(labels[crate::labels::CONTAINER], "database");
}

#[tokio::test]
async fn dependency_container_capabilities_reach_the_sidecar() {
    let mut database = container("postgres:16", None);
    database.capabilities_to_add = Some(HashSet::from([crate::config::Capability::SysPtrace]));
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert_eq!(docker.privileged_for("database"), Some(true));
}

#[tokio::test]
async fn dependency_container_shm_size_reaches_the_sidecar() {
    let mut database = container("postgres:16", None);
    database.shm_size = Some(256 * 1024 * 1024);
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

    engine.run_task("start", &[]).await.unwrap();

    assert_eq!(docker.enable_init_process_for("database"), Some(true));
}

#[tokio::test]
async fn dependency_container_log_driver_reaches_the_sidecar() {
    let mut database = container("postgres:16", None);
    database.log_driver = Some("syslog".to_string());
    let mut containers = IndexMap::new();
    containers.insert("database".to_string(), database);
    containers.insert(
        "app".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut tasks = IndexMap::new();
    tasks.insert("start".to_string(), task("app", "echo hi"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let docker = FakeContainerRuntime::default();
    let engine = engine(config, docker.clone());

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

#[tokio::test(start_paused = true)]
async fn posts_lifecycle_events_in_order_for_task_with_dependency() {
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    let mut database = container("postgres:15", None);
    database.setup_commands = Some(vec![crate::config::SetupCommand {
        command: "./init.sh".to_string(),
        working_directory: None,
        run_in: None,
    }]);
    containers.insert("database".to_string(), database);
    let mut tasks = IndexMap::new();
    tasks.insert("test".to_string(), task("build-env", "cargo test"));
    let config = Config {
        project_name: "demo".to_string(),
        containers,
        tasks,
        config_variables: None,
        forbid_telemetry: None,
    };

    let sink = RecordingEventSink::default();
    // The task container's gate runs alongside its command and is dropped
    // when the command exits (decisions/0012); `cargo test` runs for a
    // moment, as a real one does, so its gate reports.
    let docker = FakeContainerRuntime::default()
        .with_run_delay("build-env", std::time::Duration::from_secs(1));
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

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
        // Every container's image work and creation start before any
        // dependency does (decisions/0019), in container-name order
        // (`build-env` before `database`); the fake's pulls and creates
        // complete instantly, so here each container's image milestones
        // and creation all land before the next one's start, and before
        // `database` starts. Against a real daemon they overlap, and only
        // their starting order is fixed. The task's own container is
        // created here too, long before it runs: creation waits for the
        // image and the network, not for `database` to be ready.
        TaskEvent::ImagePullStarting {
            image: "alpine:3.18".into(),
        },
        TaskEvent::ImagePullCompleted {
            image: "alpine:3.18".into(),
        },
        TaskEvent::ImageResolved {
            container: "build-env".into(),
        },
        TaskEvent::ContainerCreating {
            container: "build-env".into(),
        },
        TaskEvent::ContainerCreated {
            container: "build-env".into(),
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
        TaskEvent::ContainerCreating {
            container: "database".into(),
        },
        TaskEvent::ContainerCreated {
            container: "database".into(),
        },
        // Only now, with `database` created, does its start begin — the
        // readiness gate gates starting and nothing else.
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
            run_in: None,
        },
        TaskEvent::SetupCommandsCompleted {
            container: "database".into(),
        },
        // Posted when the task container's *start* begins — once
        // `database` is ready — not at its creation, which was announced
        // above.
        TaskEvent::RunningTaskContainer {
            container: "build-env".into(),
            command: Some("cargo test".into()),
        },
        // `build-env` has no `health_check` of its own, but the task's
        // own container now goes through the same readiness gate a
        // dependency always has (0.21.0) — a container with no health
        // check at all is immediately considered healthy, same as for
        // a dependency, so this still posts unconditionally.
        TaskEvent::ContainerBecameHealthy {
            container: "build-env".into(),
        },
        // The run is over before cleanup says anything — what lets a live
        // display stop before any line about how it ended.
        TaskEvent::RunEnded {
            task: "test".into(),
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
        TaskEvent::CleanupFinished,
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
        let mut containers = IndexMap::new();
        containers.insert("build-env".to_string(), container("alpine:3.18", None));
        let mut tasks = IndexMap::new();
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
    let engine = TaskEngine::new(
        project(config()),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );
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
    let engine = TaskEngine::new(
        project(config()),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );
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
    let mut containers = IndexMap::new();
    containers.insert("build-env".to_string(), container("alpine:3.18", None));
    let mut tasks = IndexMap::new();
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
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );
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
            run_in: None,
        }]);
    });
    let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed-data.sh");
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );
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
            run_in: None,
        }]);
    });
    let docker = FakeContainerRuntime::default().with_failing_setup_command("./seed-data.sh");
    let sink = InterleavedRecordingSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );
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
    let mut containers = IndexMap::new();
    containers.insert(
        "build-env".to_string(),
        container("alpine:3.18", Some(vec!["database".to_string()])),
    );
    containers.insert("database".to_string(), container("postgres:15", None));
    let mut tasks = IndexMap::new();
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
    let engine = TaskEngine::new(
        project(config),
        docker.clone(),
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    )
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

// ---- Printed, not logged: the stderr classes the engine posts (decisions/0017) ----

/// The events of one class, as their messages.
fn messages(events: &[TaskEvent], class: fn(&TaskEvent) -> Option<&String>) -> Vec<String> {
    events.iter().filter_map(class).cloned().collect()
}

fn notice(event: &TaskEvent) -> Option<&String> {
    match event {
        TaskEvent::Notice { message } => Some(message),
        _ => None,
    }
}

fn warning(event: &TaskEvent) -> Option<&String> {
    match event {
        TaskEvent::Warning { message } => Some(message),
        _ => None,
    }
}

fn error_line(event: &TaskEvent) -> Option<&String> {
    match event {
        TaskEvent::Error { message } => Some(message),
        _ => None,
    }
}

/// The interrupt notice — the one line a person at the keyboard must read —
/// is a notice event the loggers print on stderr in every style, not a
/// `tracing::warn!` that `RUST_LOG=off` would hide (ratect#369). The words
/// are `TerminationSignal::ended_run`/`send_again`'s, unchanged.
#[tokio::test]
async fn an_interrupted_run_posts_the_interrupt_notice() {
    for (signal, expected) in [
        (
            crate::interrupt::TerminationSignal::Interrupt,
            "Interrupted; cleaning up. Press Ctrl+C again to force-remove what's left, and \
             Ctrl+C after that to stop cleaning up.",
        ),
        (
            crate::interrupt::TerminationSignal::Terminate,
            "Terminated by SIGTERM; cleaning up. Send SIGTERM again to force-remove what's \
             left, and Ctrl+C after that to stop cleaning up.",
        ),
    ] {
        let docker = FakeContainerRuntime::default()
            .with_run_delay("app", std::time::Duration::from_secs(60));
        let interrupt = crate::interrupt::Interrupt::new();
        interrupt.record_signal(signal);
        let sink = RecordingEventSink::default();
        let engine = TaskEngine::new(
            project(config_with_database_dependency(|_| {})),
            docker,
            Arc::new(sink.clone()),
            interrupt,
        );

        let error = engine.run_task("start", &[]).await.unwrap_err();

        assert!(error.is::<crate::interrupt::TaskInterrupted>(), "{error:#}");
        let events = sink.events();
        assert_eq!(messages(&events, notice), vec![expected.to_string()]);
        assert!(messages(&events, warning).is_empty(), "{events:?}");
        assert!(
            messages(&events, error_line).is_empty(),
            "a clean cleanup after a signal is no error: {events:?}"
        );
    }
}

/// The escalation rung's notice, likewise — and, on the abandon rung, the
/// cleanup failure is an `Error:` naming what was left (by config name and
/// Docker id) and the one `docker` command that removes it: `rm -f -v`, as
/// Ratect's own removal would have, the network last.
#[tokio::test]
async fn an_abandoned_cleanup_posts_the_escalation_notice_and_reports_what_it_left() {
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record();
    let docker = FakeContainerRuntime::default()
        .with_run_delay("app", std::time::Duration::from_secs(60))
        .signalling_on_removal(
            &interrupt,
            &[
                crate::interrupt::TerminationSignal::Interrupt,
                crate::interrupt::TerminationSignal::Interrupt,
            ],
        );
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_database_dependency(|_| {})),
        docker,
        Arc::new(sink.clone()),
        interrupt,
    );

    engine.run_task("start", &[]).await.unwrap_err();

    let events = sink.events();
    let notices = messages(&events, notice);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(
        notices[1],
        "Interrupted during cleanup; force-removing what's left. Press Ctrl+C to stop \
         cleaning up."
    );
    let errors = messages(&events, error_line);
    assert_eq!(errors.len(), 1, "{errors:?}");
    let error = &errors[0];
    assert!(
        error.starts_with(
            "Cleanup abandoned; left behind: app (sidecar-id-a), database (sidecar-id-d), \
             network ratect-"
        ),
        "{error}"
    );
    assert!(
        error.contains(
            ". Remove with: docker rm -f -v sidecar-id-a sidecar-id-d; docker network rm ratect-"
        ),
        "{error}"
    );
}

/// A removal the daemon refuses is reported once, at the end of cleanup,
/// quoting the daemon and listing only what was actually left — the rest
/// of cleanup still ran. The engine's line is the report: `main` prints no
/// second `Error:` for the `CleanupFailed` it gets back
/// (`exit_code::already_reported`).
#[tokio::test]
async fn a_refused_removal_reports_the_daemons_reason_and_what_was_left() {
    let docker = FakeContainerRuntime::default().refusing_container_removal("sidecar-id-database");
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_database_dependency(|_| {})),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    let error = engine.run_task("start", &[]).await.unwrap_err();

    assert!(error.is::<CleanupFailed>(), "{error:#}");
    let errors = messages(&sink.events(), error_line);
    assert_eq!(
        errors,
        vec![
            "Cleanup failed: No such container: sidecar-id-database; left behind: database \
             (sidecar-id-d). Remove with: docker rm -f -v sidecar-id-d"
                .to_string()
        ]
    );

    let docker = FakeContainerRuntime::default().refusing_network_removal();
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_database_dependency(|_| {})),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    );

    engine.run_task("start", &[]).await.unwrap_err();

    let errors = messages(&sink.events(), error_line);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].starts_with("Cleanup failed: error while removing network: network ratect-"),
        "{}",
        errors[0]
    );
    assert!(
        errors[0].contains("has active endpoints; left behind: network ratect-"),
        "{}",
        errors[0]
    );
    assert!(
        errors[0].contains(". Remove with: docker network rm ratect-"),
        "{}",
        errors[0]
    );
}

/// What a `--no-cleanup*` flag keeps is a notice — not a failure, so no
/// marker — with the same `left behind`/`Remove with` tail as the cleanup
/// failure, whichever of the two flags did the keeping.
#[tokio::test]
async fn disabled_cleanup_posts_a_notice_naming_what_it_kept() {
    let docker = FakeContainerRuntime::default();
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_database_dependency(|_| {})),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    )
    .with_settings(TaskEngineSettings {
        cleanup_after_success: false,
        ..Default::default()
    })
    .unwrap();

    engine.run_task("start", &[]).await.unwrap();

    let notices = messages(&sink.events(), notice);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with(
            "Left in place (--no-cleanup): app (sidecar-id-a), database (sidecar-id-d), \
             network ratect-"
        ),
        "{}",
        notices[0]
    );
    assert!(
        notices[0].contains(
            ". Remove with: docker rm -f -v sidecar-id-a sidecar-id-d; docker network rm ratect-"
        ),
        "{}",
        notices[0]
    );

    // An infrastructure failure is the other flag's bucket.
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
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    )
    .with_settings(TaskEngineSettings {
        cleanup_after_failure: false,
        ..Default::default()
    })
    .unwrap();

    engine.run_task("start", &[]).await.unwrap_err();

    let notices = messages(&sink.events(), notice);
    assert_eq!(notices.len(), 1, "{notices:?}");
    // The task's own container is created before `database`'s gate fails
    // (decisions/0019), so it was kept too — never started, but there.
    assert!(
        notices[0].starts_with(
            "Left in place (--no-cleanup): app (sidecar-id-a), database (sidecar-id-d), \
             network ratect-"
        ),
        "{}",
        notices[0]
    );
}

/// The loopback-proxy warning is a warning event — printed in every style —
/// not a `tracing::warn!`; `a_rewritten_proxy_url_on_a_loopback_bound_port_is_reported`
/// pins the decision, this pins the route.
#[test]
fn the_loopback_proxy_warning_is_posted_as_a_warning_event() {
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(proxy_warning_config()),
        FakeContainerRuntime::default(),
        Arc::new(sink.clone()),
        crate::interrupt::Interrupt::new(),
    )
    .with_host_env(|name| (name == "http_proxy").then(|| "http://localhost:3333".to_string()))
    .with_proc_net_tcp(|| vec![proc_net_tcp("0100007F", 3333)]);

    engine.warn_about_unreachable_proxies();

    let warnings = messages(&sink.events(), warning);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("The proxy on port 3333 is listening on loopback addresses only"),
        "{}",
        warnings[0]
    );
}

/// A signal that ends a run with cleanup disabled still says so: the
/// notice is the only line explaining why the run stopped, since `main`
/// prints no `Error: Interrupted` (decisions/0017).
#[tokio::test]
async fn an_interrupted_run_with_cleanup_disabled_still_posts_a_notice() {
    let docker =
        FakeContainerRuntime::default().with_run_delay("app", std::time::Duration::from_secs(60));
    let interrupt = crate::interrupt::Interrupt::new();
    interrupt.record();
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config_with_database_dependency(|_| {})),
        docker,
        Arc::new(sink.clone()),
        interrupt,
    )
    .with_settings(TaskEngineSettings {
        cleanup_after_failure: false,
        ..Default::default()
    })
    .unwrap();

    let error = engine.run_task("start", &[]).await.unwrap_err();

    assert!(error.is::<crate::interrupt::TaskInterrupted>(), "{error:#}");
    let notices = messages(&sink.events(), notice);
    assert_eq!(notices[0], "Interrupted; not cleaning up.", "{notices:?}");
    assert!(
        notices[1].starts_with("Left in place (--no-cleanup): "),
        "{notices:?}"
    );
}

/// Every path that ends a run on a signal reports it — here a signal during
/// a prerequisite's cleanup, which ends the run before the next task starts.
/// The cleanup's own escalation notice says only what happened to that
/// cleanup, so the skipped task gets a notice of its own naming it.
#[tokio::test]
async fn a_signal_during_a_prerequisites_cleanup_is_reported() {
    let mut config = config_with_database_dependency(|_| {});
    config
        .containers
        .insert("tool".to_string(), container("alpine:3.18", None));
    config
        .tasks
        .insert("prepare".to_string(), task("app", "true"));
    let mut main = task("tool", "true");
    main.prerequisites = Some(vec!["prepare".to_string()]);
    config.tasks.insert("main".to_string(), main);
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[crate::interrupt::TerminationSignal::Terminate],
    );
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        Arc::clone(&interrupt),
    );

    engine.run_task("main", &[]).await.unwrap_err();

    let notices = messages(&sink.events(), notice);
    assert_eq!(
        notices,
        [
            "Terminated by SIGTERM during cleanup; force-removing what's left. Press Ctrl+C \
             to stop cleaning up.",
            "Terminated by SIGTERM; not starting task 'main'.",
        ]
    );
}

/// A signal during one prerequisite's cleanup stops the run before the
/// *next prerequisite*, not only before the task itself. Checked only after
/// the last, the next prerequisite started, was interrupted at once, and
/// reported the same signal a second time as if it had interrupted it.
#[tokio::test]
async fn a_signal_between_prerequisites_stops_before_the_next_one() {
    let mut config = config_with_database_dependency(|_| {});
    for name in ["tool", "other"] {
        config
            .containers
            .insert(name.to_string(), container("alpine:3.18", None));
    }
    config
        .tasks
        .insert("prepare".to_string(), task("app", "true"));
    config
        .tasks
        .insert("second".to_string(), task("other", "true"));
    let mut main = task("tool", "true");
    main.prerequisites = Some(vec!["prepare".to_string(), "second".to_string()]);
    config.tasks.insert("main".to_string(), main);
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[crate::interrupt::TerminationSignal::Interrupt],
    );
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        Arc::clone(&interrupt),
    );

    let error = engine.run_task("main", &[]).await.unwrap_err();

    assert!(error.is::<crate::interrupt::TaskInterrupted>(), "{error:#}");
    let started: Vec<TaskEvent> = sink
        .events()
        .into_iter()
        .filter(|event| matches!(event, TaskEvent::TaskStarting { .. }))
        .collect();
    assert_eq!(
        started,
        [TaskEvent::TaskStarting {
            task: "prepare".into()
        }],
        "neither 'second' nor 'main' starts"
    );
    let notices = messages(&sink.events(), notice);
    assert_eq!(
        notices.last().map(String::as_str),
        Some("Interrupted; not starting task 'second'."),
        "{notices:?}"
    );
    assert!(
        !notices
            .iter()
            .any(|notice| notice.contains("cleaning up. Press")),
        "no second interrupt notice for the same signal: {notices:?}"
    );
}

/// A prerequisite-only task whose last prerequisite passed has nothing left
/// to start, so a signal during that prerequisite's cleanup ended nothing —
/// the run passes, as one during a passing run's own cleanup does
/// (decisions/0016).
#[tokio::test]
async fn a_signal_after_a_prerequisite_only_tasks_last_prerequisite_ends_nothing() {
    let mut config = config_with_database_dependency(|_| {});
    config
        .tasks
        .insert("prepare".to_string(), task("app", "true"));
    let mut all = task("app", "true");
    all.run = None;
    all.prerequisites = Some(vec!["prepare".to_string()]);
    config.tasks.insert("all".to_string(), all);
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[crate::interrupt::TerminationSignal::Terminate],
    );
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(NullEventSink),
        Arc::clone(&interrupt),
    );

    engine.run_task("all", &[]).await.unwrap();
}

/// Every shape of the cleanup-failure message: abandonment alone, a refusal
/// alone, and both — where the refusal made before the abandonment is kept,
/// since it is the one place the daemon's reason appears — plus the case
/// with nothing left, which still reads as a sentence. Pure, since a
/// refusal followed by an abandonment can't be staged reliably through the
/// fake runtime: which removals finish is decided by the abandon race.
#[test]
fn the_cleanup_failure_message_keeps_every_refusal_and_reads_as_a_sentence() {
    let left = LeftBehind::new([("db", "0123456789abcdef")], Some("ratect-run"));
    let refusal = vec!["device or resource busy".to_string()];

    assert_eq!(
        left.failure_message(true, &[]),
        "Cleanup abandoned; left behind: db (0123456789ab), network ratect-run. Remove with: \
         docker rm -f -v 0123456789ab; docker network rm ratect-run"
    );
    assert_eq!(
        left.failure_message(true, &refusal),
        "Cleanup abandoned after failing: device or resource busy; left behind: db \
         (0123456789ab), network ratect-run. Remove with: docker rm -f -v 0123456789ab; \
         docker network rm ratect-run"
    );
    assert_eq!(
        left.failure_message(false, &refusal),
        "Cleanup failed: device or resource busy; left behind: db (0123456789ab), network \
         ratect-run. Remove with: docker rm -f -v 0123456789ab; docker network rm ratect-run"
    );
    assert_eq!(
        LeftBehind::new([], None).failure_message(false, &refusal),
        "Cleanup failed: device or resource busy. Nothing is known to be left behind."
    );
}

/// A prerequisite that already ran this invocation starts nothing, so a
/// signal is no reason to stop before it — and no "not starting" notice
/// names a task that was never going to start. Here `main` has
/// [prepare, prepare]: the second is a no-op, so the run reaches `main`
/// itself, and that is the task the notice names.
#[tokio::test]
async fn a_prerequisite_that_already_ran_is_not_named_as_skipped() {
    let mut config = config_with_database_dependency(|_| {});
    config
        .containers
        .insert("tool".to_string(), container("alpine:3.18", None));
    config
        .tasks
        .insert("prepare".to_string(), task("app", "true"));
    let mut main = task("tool", "true");
    main.prerequisites = Some(vec!["prepare".to_string(), "prepare".to_string()]);
    config.tasks.insert("main".to_string(), main);
    let interrupt = crate::interrupt::Interrupt::new();
    let docker = FakeContainerRuntime::default().signalling_on_removal(
        &interrupt,
        &[crate::interrupt::TerminationSignal::Interrupt],
    );
    let sink = RecordingEventSink::default();
    let engine = TaskEngine::new(
        project(config),
        docker,
        Arc::new(sink.clone()),
        Arc::clone(&interrupt),
    );

    engine.run_task("main", &[]).await.unwrap_err();

    let notices = messages(&sink.events(), notice);
    assert_eq!(
        notices.last().map(String::as_str),
        Some("Interrupted; not starting task 'main'."),
        "{notices:?}"
    );
}
