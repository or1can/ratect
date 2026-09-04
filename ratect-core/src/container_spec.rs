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

//! What a container's runtime spec is — shared vocabulary between
//! `engine.rs` (which knows the configuration) and `docker.rs` (which knows
//! bollard), owned by neither.
//!
//! [`ContainerSpec`] is the one owned value [`derive_spec`] assembles, from
//! either a task's own `run` overlay or a dependency's `customise` overlay —
//! replacing what used to be two 14-and-10-parameter
//! `ContainerRuntime::run_container`/`start_background_container` calls,
//! independently assembled at engine.rs's two call sites (the task's own
//! container, and `ensure_container_ready`'s recursive dependency start) with
//! no shared code between them to keep the two in step. [`Overlay`] is
//! matched exactly once, inside `derive_spec`, and each arm destructures its
//! config struct exhaustively by name (no `..`) — a field added to `TaskRun`
//! or `TaskContainerCustomisation` becomes a compiler error here rather than
//! a silently-ignored new field.
//!
//! [`ContainerSpec::shared`] is deliberately split from the rest: it is
//! everything a task-derived and a dependency-derived spec must agree on
//! *when both overlays are empty* — the property `container_spec_tests.rs`'s
//! equivalence test checks. `role`/`labels`/`interactive`/`additional_args`
//! sit outside it for exactly that reason: a dependency is never interactive
//! and never receives `additional_args`, and `role` is baked into `labels`'
//! own values (see [`crate::labels::RunLabels::for_container`]), so nesting
//! any of the four inside `shared` would make the equivalence property false
//! by construction, for every real pair of specs, regardless of whether the
//! two call sites actually agree on everything else.
//!
//! [`NetworkOptions`]/[`ContainerOptions`]/[`HealthCheckOptions`] are
//! re-exported from `docker.rs` (`pub use`), so every existing
//! `ratect_core::docker::NetworkOptions`-shaped path is unaffected by this
//! module's existence.

use std::collections::HashMap;
use std::time::Duration;

use crate::config::{Container, TaskContainerCustomisation, TaskRun};
use crate::docker::UserMapping;
use crate::labels::{ContainerRole, RunLabels};
use crate::proxy::ProxyEnvironment;

/// Per-container network-facing options shared by `run_container` and
/// `start_background_container` — bundled together (rather than three more
/// flat parameters) since both methods were already at
/// `#[allow(clippy::too_many_arguments)]` before this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkOptions {
    /// Extra network aliases beyond the container's own name.
    pub additional_hostnames: Option<Vec<String>>,
    /// Extra `/etc/hosts` entries (hostname -> IP).
    pub additional_hosts: Option<HashMap<String, String>>,
    /// The `/etc/hosts` entry that makes a rewritten proxy URL resolve, when
    /// this run rewrote one — see `proxy::ProxyEnvironment::host_gateway`.
    /// Merged into `additional_hosts` by `build_extra_hosts`, where an
    /// entry the config declares for the same name wins.
    pub proxy_host_gateway: Option<crate::proxy::HostGateway>,
    /// Already-expanded `(local_port, container_port, protocol)` triples —
    /// a `config::PortMapping` range expands to more than one entry (see
    /// `PortMapping::expand`). Parsing/validation already happened at
    /// config-load time, so nothing here can fail. Already filtered to
    /// `None` by the caller when `--disable-ports` is set, regardless of
    /// what `ports` config exists — this struct doesn't know about that
    /// flag itself.
    pub ports: Option<Vec<(u16, u16, String)>>,
}

/// Per-container runtime options shared by `run_container` and
/// `start_background_container` — bundled together (following the same
/// reasoning as `NetworkOptions` above), rather than a growing list of flat
/// parameters, since Batect has several more of these container-level
/// fields still to land (see `ROADMAP.md`'s 0.13.0 entry). Labels live on
/// [`ContainerSpec`] itself, not here — see this module's own doc comment
/// for why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerOptions {
    /// Overrides the image's own `WORKDIR`. `None` inherits it.
    pub working_directory: Option<String>,
    /// Overrides the image's own `ENTRYPOINT`. Tokenized into literal argv
    /// via `tokenize_command_line` before reaching Docker — `None`
    /// inherits the image's own.
    pub entrypoint: Option<String>,
    /// Linux capability names to add beyond Docker's own default set
    /// (`--cap-add`) — already converted from `config::Capability` to plain
    /// strings by the caller (`docker.rs` deliberately doesn't depend on
    /// config types), each Docker's own capability name (e.g.
    /// `"DAC_OVERRIDE"`, `"ALL"`).
    pub capabilities_to_add: Option<Vec<String>>,
    /// Linux capability names to drop from Docker's own default set
    /// (`--cap-drop`). Same conversion/typing as `capabilities_to_add`.
    pub capabilities_to_drop: Option<Vec<String>>,
    /// Runs the container with extended (nearly all host) privileges —
    /// Docker's `--privileged`. `None`/`Some(false)` both behave like
    /// Docker's own unset default.
    pub privileged: Option<bool>,
    /// The size of `/dev/shm`, in bytes — Docker's `--shm-size`. `None`
    /// inherits Docker's own default (64 MiB).
    pub shm_size: Option<i64>,
    /// Host devices to make available inside the container — Docker's
    /// `--device`. `(local_path, container_path, cgroup_permissions)`
    /// triples — `docker.rs` deliberately doesn't depend on config types
    /// (same conversion boundary as `NetworkOptions::ports`'
    /// already-expanded tuples).
    pub devices: Option<Vec<(String, String, Option<String>)>>,
    /// Runs Docker's own tini-based init process as PID 1 ahead of the
    /// actual command — Docker's `--init`. `None`/`Some(false)` both
    /// behave like Docker's own unset default.
    pub enable_init_process: Option<bool>,
    /// Docker's logging driver (`--log-driver`), e.g. `"json-file"`,
    /// `"syslog"`, `"none"`. `None` leaves the daemon's own configured
    /// default alone.
    pub log_driver: Option<String>,
    /// Driver-specific options (`--log-opt`, repeatable) for `log_driver` —
    /// meaningless without it, same as Docker's own CLI.
    pub log_options: Option<HashMap<String, String>>,
    /// In-memory `tmpfs` mounts — Docker's `--tmpfs`. `(container_path,
    /// options)` pairs, `options` an opaque string (e.g.
    /// `"size=100m,mode=1770"`) forwarded verbatim to Docker's own
    /// `HostConfig.Tmpfs` map, matching Batect's own `VolumeMountResolver`
    /// (which does no parsing/validation of it either) — `docker.rs`
    /// deliberately doesn't depend on config types (same conversion boundary
    /// as `devices` above).
    pub tmpfs: Option<Vec<(String, String)>>,
}

/// A container's `health_check` override, applied at container creation on
/// top of whatever `HEALTHCHECK` its image declares. Mirrors
/// `config::HealthCheckConfig` as plain values, keeping this module free of
/// config types (same reasoning as `NetworkOptions::ports`'
/// already-expanded tuples). Every field is optional — an omitted field
/// inherits the image's own value (Docker treats an absent/zero field as
/// "inherit").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthCheckOptions {
    /// Run via the system's default shell (Docker's `CMD-SHELL` form, same
    /// as a Dockerfile `HEALTHCHECK CMD <string>`); exit code 0 = healthy.
    pub command: Option<String>,
    pub interval: Option<Duration>,
    pub retries: Option<u32>,
    pub start_period: Option<Duration>,
    pub timeout: Option<Duration>,
}

/// Everything a task-derived and a dependency-derived [`ContainerSpec`] must
/// agree on when the overlay contributes nothing — see this module's own doc
/// comment for why `role`/`labels`/`interactive`/`additional_args` are *not*
/// in here.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedContainerSpec {
    /// This container's own network alias — Docker's `hostname`, and the
    /// name other containers on the same network reach it by.
    pub name: String,
    /// The already-resolved image name/ID this container runs — never a
    /// `build_directory`; `docker.rs` deliberately doesn't resolve or build
    /// images itself (see `TaskEngine::resolve_image`).
    pub image: String,
    /// This container's command, already resolved from the overlay (a
    /// task's own `run.command`) and the container's own `command` — a
    /// dependency's `customise` has no equivalent override (matching
    /// Batect's own `TaskContainerCustomisation`), so a dependency's is
    /// always its container's own value, verbatim. `None` runs the image's
    /// own default `CMD` instead (plus `additional_args`, when the task's
    /// own container has any).
    pub command: Option<String>,
    /// Already-resolved bind-mount strings (`resolve_volumes`) — `tmpfs`
    /// mounts are carried separately, on `options.tmpfs`.
    pub volumes: Option<Vec<String>>,
    /// This container's own `environment`, merged with `TERM`/proxy
    /// variables and the overlay's own `environment` — see
    /// `merged_environment`.
    pub environment: Option<HashMap<String, String>>,
    /// This task execution's own isolated network — every task gets one,
    /// regardless of whether it has dependencies (or `--use-network`'s own
    /// existing network, when given).
    pub network: String,
    /// `Some` when this container's own `run_as_current_user` is enabled.
    /// When present: any of `volumes`' host paths that don't exist yet are
    /// created first (as the current host user, so Docker's daemon doesn't
    /// auto-create them as `root:root`); the container's `User` is set to
    /// the mapped `uid:gid`; and, after creation but before starting,
    /// minimal `/etc/passwd`/`/etc/shadow`/`/etc/group` entries and the
    /// declared home directory (owned by that `uid:gid`) are uploaded into
    /// it — an arbitrary host uid/gid otherwise has no corresponding entry
    /// in the image's own passwd/group, which many programs need to
    /// function at all.
    pub user_mapping: Option<UserMapping>,
    pub network_options: NetworkOptions,
    /// Overrides the image's own `HEALTHCHECK` at creation — applying it is
    /// what makes `ContainerRuntime::wait_for_container_healthy` meaningful
    /// for images with no health check of their own.
    pub health_check: Option<HealthCheckOptions>,
    pub options: ContainerOptions,
}

/// One container's complete, owned runtime specification — the argument
/// `ContainerRuntime::run_container`/`start_background_container` take
/// instead of their old flat parameter lists. Owned throughout (one clone
/// per container start) rather than borrowed, so a fake `ContainerRuntime`
/// can capture it wholesale instead of copying each field into its own
/// captured-parameter maps.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSpec {
    pub shared: SharedContainerSpec,
    /// Whether this is the task's own container or a dependency — baked
    /// into `labels`' own values (see
    /// [`crate::labels::RunLabels::for_container`]), which is why a fake
    /// `ContainerRuntime` can recover it from `labels` alone if needed.
    pub role: ContainerRole,
    /// This container's Ratect ownership labels, merged over its own
    /// configured `labels` — see `RunLabels::for_container`. Always
    /// present (possibly containing nothing beyond Ratect's own keys),
    /// unlike `shared.options`' other fields, which are `None` when unset.
    pub labels: HashMap<String, String>,
    /// Eligibility, not a guarantee — only ever `true` for the top-level
    /// requested task's own container (never a prerequisite's, a
    /// dependency's, or a sidecar's — see `TaskEngine::run_task_internal`).
    /// Whether a real Docker TTY actually gets allocated additionally
    /// depends on the local process's own stdin/stdout genuinely being
    /// terminals; when they're not, this container runs exactly as if this
    /// were `false`. Always `false` for a dependency.
    pub interactive: bool,
    /// Appended as literal argv entries after `shared.command`'s own
    /// tokenized argv — matching Batect's own `ADDITIONAL_ARGS` mechanism
    /// exactly, never re-parsed as shell syntax regardless of what
    /// characters they contain. If `shared.command` is `None`, a non-empty
    /// `additional_args` is passed directly as the container's argv,
    /// letting the image's own entrypoint receive them. Always empty for a
    /// dependency — only the top-level requested task's own container can
    /// receive `-- ADDITIONAL_ARGS`.
    pub additional_args: Vec<String>,
}

/// The task-`run`/dependency-`customise` overlay a container's own config is
/// laid under — matched exactly once, inside [`derive_spec`], each arm
/// destructured exhaustively by name so a field added to either config
/// struct is a compile error here until this match handles it.
pub enum Overlay<'a> {
    Run(&'a TaskRun),
    /// `None` when this dependency has no `customise` entry of its own —
    /// distinct from `Some` of an all-`None`-fields entry only in name, not
    /// in effect (both override nothing), so callers don't need to
    /// synthesize a placeholder value just to call [`derive_spec`].
    Customise(Option<&'a TaskContainerCustomisation>),
}

/// Everything the caller must resolve or compute — asynchronously, or from
/// engine state `derive_spec` has no access to — before a [`ContainerSpec`]
/// can be assembled. Bundled into one struct for the same reason
/// `NetworkOptions`/`ContainerOptions` are: `derive_spec` would otherwise
/// take more positional parameters than the two call sites it replaces did.
pub struct ContainerSpecInputs<'a> {
    /// This container's own network alias — see
    /// [`SharedContainerSpec::name`].
    pub name: &'a str,
    pub container_config: &'a Container,
    pub overlay: Overlay<'a>,
    /// The already-resolved image name/ID (`TaskEngine::resolve_image`).
    pub image: &'a str,
    pub network: &'a str,
    /// `false` for a dependency — see [`ContainerSpec::interactive`].
    pub interactive: bool,
    /// Empty for a dependency — see [`ContainerSpec::additional_args`].
    pub additional_args: &'a [String],
    pub user_mapping: Option<&'a UserMapping>,
    pub volumes: Option<&'a Vec<String>>,
    /// The host's own `TERM`, when the interleaved output policy applies —
    /// see `TaskEngine::term_environment_variable`.
    pub term_var: Option<&'a HashMap<String, String>>,
    /// This task execution's proxy environment, when
    /// `propagate_proxy_environment_variables` applies — see
    /// `TaskEngine::proxy_environment`.
    pub proxy: Option<&'a ProxyEnvironment>,
    /// `--publish-ports` — whether `network_options.ports` carries anything
    /// at all, regardless of what `ports` config exists.
    pub publish_ports: bool,
    pub role: ContainerRole,
    pub run_labels: &'a RunLabels,
}

/// Assembles one container's [`ContainerSpec`] from its own config plus
/// `inputs.overlay` — the one place `TaskRun`/`TaskContainerCustomisation`
/// are read when starting a container, replacing what used to be two
/// independently-assembled parameter lists at engine.rs's two call sites.
/// See this module's own doc comment for the `shared`/non-`shared` split and
/// the exhaustive-destructure rationale.
pub fn derive_spec(inputs: ContainerSpecInputs<'_>) -> ContainerSpec {
    let ContainerSpecInputs {
        name,
        container_config,
        overlay,
        image,
        network,
        interactive,
        additional_args,
        user_mapping,
        volumes,
        term_var,
        proxy,
        publish_ports,
        role,
        run_labels,
    } = inputs;

    // Exhaustive by name (no `..`): a field added to either struct must be
    // handled here before this compiles.
    let (
        command_override,
        entrypoint_override,
        working_directory_override,
        environment_override,
        ports_override,
    ) = match overlay {
        Overlay::Run(run) => {
            let TaskRun {
                container: _,
                command,
                environment,
                ports,
                working_directory,
                entrypoint,
            } = run;
            (
                command.as_deref(),
                entrypoint.as_deref(),
                working_directory.as_deref(),
                environment.as_ref(),
                ports.as_ref(),
            )
        }
        Overlay::Customise(None) => (None, None, None, None, None),
        Overlay::Customise(Some(customisation)) => {
            let TaskContainerCustomisation {
                environment,
                ports,
                working_directory,
            } = customisation;
            (
                None,
                None,
                working_directory.as_deref(),
                environment.as_ref(),
                ports.as_ref(),
            )
        }
    };

    let command = command_override.or(container_config.command.as_deref());
    let working_directory =
        working_directory_override.or(container_config.working_directory.as_deref());
    let entrypoint = entrypoint_override.or(container_config.entrypoint.as_deref());
    let proxy_vars = proxy.map(|proxy| &proxy.variables);
    let environment = merged_environment(
        term_var,
        proxy_vars,
        container_config.environment.as_ref(),
        environment_override,
    );
    let expanded_ports = merged_ports(container_config.ports.as_ref(), ports_override);
    let network_options = NetworkOptions {
        additional_hostnames: container_config.additional_hostnames.clone(),
        additional_hosts: container_config.additional_hosts.clone(),
        proxy_host_gateway: proxy.and_then(|proxy| proxy.host_gateway()),
        ports: (publish_ports && !expanded_ports.is_empty()).then_some(expanded_ports),
    };
    let container_options = ContainerOptions {
        working_directory: working_directory.map(str::to_string),
        entrypoint: entrypoint.map(str::to_string),
        capabilities_to_add: capability_names(container_config.capabilities_to_add.as_ref()),
        capabilities_to_drop: capability_names(container_config.capabilities_to_drop.as_ref()),
        privileged: container_config.privileged,
        shm_size: container_config.shm_size,
        devices: device_triples(container_config.devices.as_ref()),
        enable_init_process: container_config.enable_init_process,
        log_driver: container_config.log_driver.clone(),
        log_options: container_config.log_options.clone(),
        tmpfs: tmpfs_mounts(container_config.volumes.as_ref()),
    };
    let labels = run_labels.for_container(name, role, container_config.labels.as_ref());

    ContainerSpec {
        shared: SharedContainerSpec {
            name: name.to_string(),
            image: image.to_string(),
            command: command.map(str::to_string),
            volumes: volumes.cloned(),
            environment,
            network: network.to_string(),
            user_mapping: user_mapping.cloned(),
            network_options,
            health_check: health_check_options(container_config),
            options: container_options,
        },
        role,
        labels,
        interactive,
        additional_args: additional_args.to_vec(),
    }
}

/// Merges the host's `TERM` (see `TaskEngine::term_environment_variable`),
/// proxy-derived environment variables (see `TaskEngine::proxy_environment`),
/// a container's `environment`, and an overlay's own `environment`, each
/// overriding the last on key collision — `TERM` and proxy vars are the
/// lowest-precedence base, matching Batect (`terminalEnvironmentVariablesFor +
/// proxyEnvironmentVariables + substituteEnvironmentVariables`, later entries
/// winning); the container's `environment` overrides both, and the overlay's
/// overrides all three. `None` only when none of the four are set.
///
/// Also reused, with `term_var`/`overlay` both `None`, to merge a
/// `build_directory` build's own `build_args` with proxy variables — the
/// same precedence (proxy first, config second) applies there too, and nothing
/// about the merge itself is container-specific.
pub(crate) fn merged_environment(
    term_var: Option<&HashMap<String, String>>,
    proxy_vars: Option<&HashMap<String, String>>,
    container_env: Option<&HashMap<String, String>>,
    overlay_env: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    if term_var.is_none()
        && proxy_vars.is_none()
        && container_env.is_none()
        && overlay_env.is_none()
    {
        return None;
    }
    let mut merged = term_var.cloned().unwrap_or_default();
    if let Some(proxy_vars) = proxy_vars {
        merged.extend(proxy_vars.clone());
    }
    if let Some(container_env) = container_env {
        merged.extend(container_env.clone());
    }
    if let Some(overlay_env) = overlay_env {
        merged.extend(overlay_env.clone());
    }
    Some(merged)
}

/// Expands and concatenates a container's own `ports` with an overlay's
/// *additional* `ports` — a union, not an override (matching Batect, which
/// combines these as a `Set`, so there's no concept of one entry replacing
/// another by container port; `overlay_ports` is `None` for a dependency
/// with no `customise` entry of its own). Each `PortMapping` is expanded (a
/// range becomes more than one triple — see `PortMapping::expand`) before
/// `docker.rs` ever sees it, so `NetworkOptions::ports` only ever carries
/// already-resolved `(local_port, container_port, protocol)` triples, never
/// a `PortMapping` needing further interpretation.
fn merged_ports(
    container_ports: Option<&Vec<crate::config::PortMapping>>,
    overlay_ports: Option<&Vec<crate::config::PortMapping>>,
) -> Vec<(u16, u16, String)> {
    container_ports
        .into_iter()
        .flatten()
        .chain(overlay_ports.into_iter().flatten())
        .flat_map(crate::config::PortMapping::expand)
        .collect()
}

/// Converts a container's parsed `health_check` config into
/// [`HealthCheckOptions`] — `docker.rs` deliberately doesn't depend on
/// config types (same conversion boundary as `merged_ports`'
/// expanded tuples above).
fn health_check_options(container: &Container) -> Option<HealthCheckOptions> {
    container
        .health_check
        .as_ref()
        .map(|health_check| HealthCheckOptions {
            command: health_check.command.clone(),
            interval: health_check.interval,
            retries: health_check.retries,
            start_period: health_check.start_period,
            timeout: health_check.timeout,
        })
}

/// Converts a `capabilities_to_add`/`capabilities_to_drop` set of
/// `config::Capability` into the plain Docker capability name strings
/// [`ContainerOptions`] expects — `docker.rs` deliberately doesn't depend on
/// config types (same conversion boundary as `health_check_options` above).
/// `None` when the set itself is `None`.
fn capability_names(
    capabilities: Option<&std::collections::HashSet<crate::config::Capability>>,
) -> Option<Vec<String>> {
    Some(
        capabilities?
            .iter()
            .map(|capability| capability.as_str().to_string())
            .collect(),
    )
}

/// Converts a `devices` list of `config::DeviceMapping` into the plain
/// `(local, container, options)` triples [`ContainerOptions`] expects —
/// `docker.rs` deliberately doesn't depend on config types (same conversion
/// boundary as `capability_names` above).
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

/// Converts a `volumes` list's `tmpfs` entries into the plain `(container,
/// options)` pairs [`ContainerOptions`] expects — same conversion boundary as
/// `capability_names`/`device_triples` above. `Local`/`Cache` entries are
/// skipped here — they're resolved separately, by
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
