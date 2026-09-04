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
//! bollard), owned by neither. Re-exported from `docker.rs` (`pub use`), so
//! every existing `ratect_core::docker::NetworkOptions`-shaped path is
//! unaffected by this module's existence.

use std::collections::HashMap;
use std::time::Duration;

/// Per-container network-facing options shared by `run_container` and
/// `start_background_container` — bundled together (rather than three more
/// flat parameters) since both methods were already at
/// `#[allow(clippy::too_many_arguments)]` before this.
pub struct NetworkOptions<'a> {
    /// Extra network aliases beyond the container's own name.
    pub additional_hostnames: Option<&'a Vec<String>>,
    /// Extra `/etc/hosts` entries (hostname -> IP).
    pub additional_hosts: Option<&'a HashMap<String, String>>,
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
    pub ports: Option<&'a Vec<(u16, u16, String)>>,
}

/// Per-container runtime options shared by `run_container` and
/// `start_background_container` — bundled together (following the same
/// reasoning as `NetworkOptions` above), rather than a growing list of flat
/// parameters, since Batect has several more of these container-level
/// fields still to land (see `ROADMAP.md`'s 0.13.0 entry).
#[derive(Debug, Clone, Default)]
pub struct ContainerOptions<'a> {
    /// Overrides the image's own `WORKDIR`. `None` inherits it.
    pub working_directory: Option<&'a str>,
    /// Overrides the image's own `ENTRYPOINT`. Tokenized into literal argv
    /// via `tokenize_command_line` before reaching Docker — `None`
    /// inherits the image's own.
    pub entrypoint: Option<&'a str>,
    /// Docker labels (`key: value`) applied to the container. `None`/empty
    /// applies none beyond whatever the image's own build already baked in.
    pub labels: Option<&'a HashMap<String, String>>,
    /// Linux capability names to add beyond Docker's own default set
    /// (`--cap-add`) — already converted from `config::Capability` to plain
    /// strings by the caller (`docker.rs` deliberately doesn't depend on
    /// config types), each Docker's own capability name (e.g.
    /// `"DAC_OVERRIDE"`, `"ALL"`).
    pub capabilities_to_add: Option<&'a Vec<String>>,
    /// Linux capability names to drop from Docker's own default set
    /// (`--cap-drop`). Same conversion/typing as `capabilities_to_add`.
    pub capabilities_to_drop: Option<&'a Vec<String>>,
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
    pub devices: Option<&'a Vec<(String, String, Option<String>)>>,
    /// Runs Docker's own tini-based init process as PID 1 ahead of the
    /// actual command — Docker's `--init`. `None`/`Some(false)` both
    /// behave like Docker's own unset default.
    pub enable_init_process: Option<bool>,
    /// Docker's logging driver (`--log-driver`), e.g. `"json-file"`,
    /// `"syslog"`, `"none"`. `None` leaves the daemon's own configured
    /// default alone.
    pub log_driver: Option<&'a str>,
    /// Driver-specific options (`--log-opt`, repeatable) for `log_driver` —
    /// meaningless without it, same as Docker's own CLI.
    pub log_options: Option<&'a HashMap<String, String>>,
    /// In-memory `tmpfs` mounts — Docker's `--tmpfs`. `(container_path,
    /// options)` pairs, `options` an opaque string (e.g.
    /// `"size=100m,mode=1770"`) forwarded verbatim to Docker's own
    /// `HostConfig.Tmpfs` map, matching Batect's own `VolumeMountResolver`
    /// (which does no parsing/validation of it either) — `docker.rs`
    /// deliberately doesn't depend on config types (same conversion boundary
    /// as `devices` above).
    pub tmpfs: Option<&'a Vec<(String, String)>>,
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
