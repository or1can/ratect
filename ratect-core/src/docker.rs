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

//! Wraps `bollard` for all Docker daemon interaction
//! — pulling/building images, running a task's own container, per-task networks,
//! sidecar/dependency containers, the interactive-mode TTY attach path
//! ([docs](https://github.com/or1can/ratect/blob/main/docs/interactive-mode.md)), and the user-mapping upload
//! path ([docs](https://github.com/or1can/ratect/blob/main/docs/ratect-compat-config-reference.md#user-mapping)). Exposes a `ContainerRuntime`
//! trait so the engine can be tested against a fake instead of a live daemon. Gotchas
//! worth knowing before touching it: `run_container`'s three actual start/attach
//! paths (`run_container_interactively`/`run_container_forwarding_stdin`/
//! `start_and_stream_output`) each make their own attach and Docker `start` call,
//! all three attaching *before* starting, deliberately, so no early output is missed,
//! and reading output from that attach rather than Docker's `logs` API, which answers
//! 501 for a log driver it can't read back (ratect#517); `run_container`'s own
//! `started` parameter (0.21.0) is threaded into all three so each can signal it
//! right after its own `start` call succeeds, regardless of which path actually
//! ran, letting `engine.rs`'s concurrent readiness gate begin at the right moment
//! however the container ends up attached; **`run_container` never removes the
//! container it creates** — `engine.rs`'s cleanup stage removes everything, the
//! task's own container included, so `--no-cleanup-after-success`/`-failure` are
//! read in exactly one place (0.25.0, see `engine.rs`'s own note and
//! `run_container`'s doc comment for why the split was retired rather than
//! corrected again); the interactive path's `RawModeGuard`
//! restores the terminal on `Drop`, even on an error return; since Ratect has no `--output`
//! streaming mode, a failed build's full log transcript (not just Docker's one-line
//! summary) is folded into the returned error instead; `command`/`entrypoint`/
//! `setup_commands.command` are all tokenized into literal argv by
//! `tokenize_command_line` (a from-scratch port of Batect's own `Command.parse`)
//! rather than run via a shell — `setup_commands` used to be a `sh -c` exception
//! (closed once noticed it was never actually deliberate; see
//! `config::SetupCommand`'s doc comment); and `ContainerOptions` bundles the
//! still-growing set of per-container Docker options shared by `run_container`/
//! `start_background_container` (0.13.0's `working_directory` through
//! `enable_init_process`) — add new container-level fields there rather than as more
//! flat parameters, converting from config types to plain values in
//! `container_spec::derive_spec` (`docker.rs` deliberately never depends on
//! `config` types directly).
//! `log_driver`/`log_options` (0.19.0) followed the same pattern onto
//! bollard's `HostConfig.log_config` (`build_log_config`, pure/unit-testable,
//! same shape as `build_devices`) — `None`/absent leaves the daemon's own
//! configured default alone rather than baking in a literal `"json-file"`
//! default the way Batect's own config model does. `tmpfs` (0.21.0)
//! followed the same pattern again, onto bollard's `HostConfig.Tmpfs`
//! (`build_tmpfs_mounts`, pure/unit-testable, same shape as `build_devices`/
//! `build_log_config`) — unlike `devices`/`log_options`, its `(container_path,
//! options)` pairs come from the same `volumes` config field `resolve_volumes`
//! already handles, just pulled out separately (`container_spec.rs`'s `tmpfs_mounts`)
//! since a tmpfs mount can't be expressed as a bind string. `ulimits`
//! (ratect#95) followed the same pattern, onto bollard's
//! `HostConfig.ulimits` (`build_ulimits`, pure/unit-testable, same shape as
//! `build_devices`) — `(name, soft, hard)` triples, `None`/absent leaving
//! the daemon's own defaults alone. `dns`/`dns_search`/`dns_options` (ratect#105)
//! follow it too, as plain string lists passed straight onto bollard's
//! `HostConfig` fields of the same names — no builder needed. `network_mode`
//! (ratect#106) is the one that changes more than a `HostConfig` field: a
//! container on the task's network is created on it, with its aliases
//! (`build_networking_config`), so it is never also on the default `bridge`
//! (ratect#303); one in a Docker network mode (`build_network_mode`) gets no
//! endpoint at all, and under `container:` no `hostname` either
//! (`build_hostname`) — all three read off the one [`ContainerNetwork`]
//! value `container_spec::resolve_network` derived.
//! `stop_signal`/`stop_grace_period` (ratect#112) are the one per-container pair that does *not* live in
//! `ContainerOptions`: they take effect when a container is *stopped*, not
//! created, so they are parameters of `stop_and_remove_container`
//! (`ResourceInventory`'s, hence `resources.rs`'s trait) and are `None` for
//! a leftover found by label scan, which has no config to read. `build_image` also
//! gained a `force_pull: bool` parameter (0.19.0, both the classic and
//! BuildKit paths' `BuildImageOptionsBuilder::pull("true")`) — Batect's
//! second, distinct use of `image_pull_policy` on a `build_directory`
//! container (`container_spec::derive_build_spec` computes it from
//! `container_config.image_pull_policy == Always`, since `docker.rs` still
//! doesn't depend on `config` types directly).
//! `classify_ssh_agent_paths` (0.25.0) turns one `build_ssh` entry's already
//! resolved `paths` into a `SshAgentSource` (`HostAgent`/`Socket`/`Keys`) by
//! the same rules Go BuildKit's own `sshprovider` uses — no paths means the
//! host's `SSH_AUTH_SOCK`, a path that *is* a Unix socket forwards that agent
//! and has to be the entry's only path, and anything else is a private key
//! file. Deliberately a `stat` rather than a config-schema rule, matching
//! where BuildKit draws the line: a nonexistent path is *not* a socket, so it
//! goes down the key-file route and fails naming the file instead of failing
//! to connect to something that was never there. `Keys` is what makes
//! `build_image_via_buildkit` start a `crate::ssh_agent::Keyring` and hand
//! bollard its socket — those keyrings are held in a local `Vec` and
//! explicitly dropped *after* the build stream is drained, since dropping one
//! stops the agent that every `RUN --mount=type=ssh` in the build depends on.
//! `ensure_host_volume_directories_exist` (the `run_as_current_user` host-dir
//! pre-creation step) only `mkdir -p`s a bind's source when it's a *host*
//! source (`BindSource::Host` — a `CacheType::Volume` cache is a
//! `BindSource::Volume` name, 0.18.0, not a host path), *doesn't already
//! exist* (an existing directory Docker reuses, or a single file/socket the
//! config bind-mounts like `~/.gitconfig` or an SSH agent socket — `mkdir`
//! over a non-directory both fails and is wrong), and *isn't a special
//! Docker Desktop path* (`/run/host-services`/`/run/guest-services`, which
//! Docker injects into its own VM and don't exist on a macOS host — the SSH
//! agent socket lives at `/run/host-services/ssh-auth.sock`). The latter two
//! guards are a straight port of Batect's `!Files.exists &&
//! !isSpecialDockerDesktopPath`; 0.10.0's `52e8d59` dropped the exists-check
//! as "TOCTOU-prone" and the special-path one was never ported, which
//! together broke mounting the SSH agent under `run_as_current_user` — don't
//! re-simplify them away. Its nested-mount-point half keeps the same two
//! guards for the same reason, and gives a file or socket source an empty
//! file rather than a directory (ratect#321). `list_volumes`/`remove_volume` (0.18.0, `--clean`/
//! `--clean-cache`) are thin wrappers over bollard's own volume API — see
//! `cache.rs` for the actual removal-decision logic built on top of them.
//! `list_containers`/`list_networks` (0.2.0-dev) are the equivalent pair for
//! finding what a previous run left behind, both returning the same
//! `LabelledResource` (a container and a network want reporting identically,
//! so the reporting code isn't written twice) and both filtering *daemon-side*
//! via `label_filters` — Docker ANDs the values under one `label` filter name,
//! which is what makes "this project *and* this run" mean both rather than
//! either. `list_containers` passes `all: true` deliberately: a leftover has
//! usually exited, and Docker's default lists only running containers.

use crate::cache::VolumeStore;
use crate::resources::ResourceInventory;
use crate::ui::{ContainerIoStreaming, EventSink, NullEventSink, TaskEvent};
use anyhow::{Context, Result};
use bollard::container::AttachContainerResults;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody as Config, DeviceMapping, EndpointSettings, HealthConfig,
    NetworkCreateRequest, NetworkingConfig, PortBinding, PortMap, ResourcesUlimits,
};
use bollard::query_parameters::AttachContainerOptionsBuilder;
use bollard::query_parameters::BuildImageOptionsBuilder;
use bollard::query_parameters::CreateContainerOptionsBuilder;
use bollard::query_parameters::CreateImageOptions;
use bollard::query_parameters::EventsOptionsBuilder;
use bollard::query_parameters::InspectContainerOptions;
use bollard::query_parameters::ResizeContainerTTYOptionsBuilder;
use bollard::query_parameters::UploadToContainerOptionsBuilder;
use bollard::query_parameters::WaitContainerOptions;
use bollard::service::HostConfig;
use bollard::ClientVersion;
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

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

/// The task's own container output could not be written because stdout's
/// reader has gone — `ratect run build | head` once `head` has read enough.
///
/// A run treats it as it treats a termination signal: the run ends, the
/// engine cleans up, and the process exits [`StdoutClosed::EXIT_CODE`] —
/// what a shell reports for a writer `SIGPIPE` killed (ratect#670). Only the
/// task container's own output raises it — under `-o all` written by the
/// logger, which reports the gone reader through
/// [`crate::ui::EventSink::result_reader_gone`]. Every other write ignores a
/// failure, and a closed stderr never stops a run (ratect#669).
#[derive(Debug)]
pub struct StdoutClosed;

impl StdoutClosed {
    /// 128 + `SIGPIPE`'s number, 13 — decisions/0016's `128 + n`.
    pub const EXIT_CODE: u8 = 141;
}

impl fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stdout closed")
    }
}

impl std::error::Error for StdoutClosed {}

/// Writes one chunk of container output to `out` and flushes it — so output
/// appears as the container produces it, and a reader that has gone is found
/// at this chunk rather than whenever a buffer next fills. A failed write is
/// [`StdoutClosed`] when the reader has gone and Ratect's own failure
/// otherwise.
fn write_container_output(out: &mut impl std::io::Write, chunk: &[u8]) -> Result<()> {
    out.write_all(chunk)
        .and_then(|()| out.flush())
        .map_err(output_write_error)
}

/// [`write_container_output`]'s classification of a failed write, shared
/// with the interactive path, which writes through `tokio`'s stdout.
fn output_write_error(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::BrokenPipe {
        StdoutClosed.into()
    } else {
        anyhow::Error::new(error).context("Failed to write container output")
    }
}

/// Why [`ContainerRuntime::create_container`] failed — and, when Docker's own
/// create had already succeeded, the container it left behind.
///
/// Creation is more than Docker's `create` call: the user-mapping upload
/// and the task-network join come after it and before the container counts
/// as created (see `create_container`'s own docs), and either can fail with
/// a container already in existence. That container is the caller's to
/// remove — the engine registers every id it is handed for cleanup — so the
/// id travels with the error rather than being lost to it. `None` when
/// nothing was created: Docker refused the `create` itself, or the spec
/// couldn't be turned into one.
#[derive(Debug)]
pub struct CreateContainerError {
    pub created: Option<String>,
    pub error: anyhow::Error,
}

impl CreateContainerError {
    /// A failure with nothing to clean up after.
    pub fn before_creation(error: anyhow::Error) -> Self {
        Self {
            created: None,
            error,
        }
    }
}

impl fmt::Display for CreateContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for CreateContainerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

/// The states `tokenize_command_line`'s character-by-character scan moves
/// through — outside any quote, inside a `'...'` (literal, no escapes), or
/// inside a `"..."` (backslash escapes processed).
#[derive(PartialEq)]
enum TokenizerState {
    Normal,
    SingleQuote,
    DoubleQuote,
}

/// Splits a `command`/`entrypoint` string into literal argv — ported from
/// Batect's own `Command.parse` (`batect.os.Command`), the same
/// whitespace-splitting, quote/backslash-aware tokenizer Batect uses for
/// both fields. Deliberately *not* a shell: no `$VAR` expansion, no
/// globbing, no `&&`/`|`/`>` — those characters are just ordinary content.
/// A backslash escapes the very next character (including outside any
/// quote); single quotes take everything up to the next single quote
/// completely literally (no escapes processed inside, matching Batect); double
/// quotes process backslash escapes. Whitespace-only content between
/// argument-separating whitespace is discarded, mirroring Batect's own
/// `isNotBlank()` check — except at the very end of the string, where
/// even whitespace-only trailing content (only reachable via an escaped
/// space) is kept, matching Batect's asymmetric `isNotEmpty()` there.
/// Errors (unbalanced quote, trailing backslash) carry Batect's own message
/// text. `config.rs`'s `check_values` runs every command-line field through
/// this when the file loads, as Batect parses them while decoding (#377), and
/// names the container or task and field where Batect gives the file, line
/// and path.
pub(crate) fn tokenize_command_line(input: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = input.chars().collect();
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut state = TokenizerState::Normal;
    let mut i = 0;

    let dangling_backslash = || {
        anyhow::anyhow!(
            "Command `{input}` is invalid: it ends with a backslash (backslashes always \
             escape the following character, for a literal backslash, use '\\\\')"
        )
    };

    while i < chars.len() {
        let c = chars[i];
        match state {
            TokenizerState::Normal => {
                if c == '\\' {
                    i += 1;
                    current.push(*chars.get(i).ok_or_else(dangling_backslash)?);
                } else if c.is_whitespace() {
                    if current.chars().any(|c| !c.is_whitespace()) {
                        arguments.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                } else if c == '\'' {
                    state = TokenizerState::SingleQuote;
                } else if c == '"' {
                    state = TokenizerState::DoubleQuote;
                } else {
                    current.push(c);
                }
            }
            TokenizerState::SingleQuote => {
                if c == '\'' {
                    state = TokenizerState::Normal;
                } else {
                    current.push(c);
                }
            }
            TokenizerState::DoubleQuote => {
                if c == '"' {
                    state = TokenizerState::Normal;
                } else if c == '\\' {
                    i += 1;
                    current.push(*chars.get(i).ok_or_else(dangling_backslash)?);
                } else {
                    current.push(c);
                }
            }
        }
        i += 1;
    }

    match state {
        TokenizerState::DoubleQuote => {
            anyhow::bail!("Command `{input}` is invalid: it contains an unbalanced double quote")
        }
        TokenizerState::SingleQuote => {
            anyhow::bail!("Command `{input}` is invalid: it contains an unbalanced single quote")
        }
        TokenizerState::Normal => {
            if !current.is_empty() {
                arguments.push(current);
            }
            Ok(arguments)
        }
    }
}

/// Builds the Docker `cmd` array for a task's container, folding in any
/// `-- ADDITIONAL_ARGS` from the CLI.
///
/// `command` is tokenized via `tokenize_command_line` — the same literal,
/// no-shell-involved argv Batect itself would produce — with
/// `additional_args` appended as further literal argv entries, matching
/// Batect's own `ADDITIONAL_ARGS` handling exactly (no `sh -c`, no
/// positional-parameter trick). See `docs/differences-from-batect.md` for
/// what this means for `$VAR`/glob/shell-operator characters in `command`.
///
/// When `command` is unset, non-empty `additional_args` replace the image's
/// default `CMD`, as `docker run <image> <args>` does (the image's own
/// entrypoint receives them); Batect refuses them with an error instead.
fn build_cmd(command: Option<&str>, additional_args: &[String]) -> Result<Option<Vec<String>>> {
    match command {
        Some(c) => {
            let mut argv = tokenize_command_line(c)?;
            argv.extend(additional_args.iter().cloned());
            Ok(Some(argv))
        }
        None if additional_args.is_empty() => Ok(None),
        None => Ok(Some(additional_args.to_vec())),
    }
}

/// Whether `additional_args` stand in for the image's default `CMD` — with no
/// `command`, [`build_cmd`] makes them the whole `Cmd`.
fn additional_args_replace_image_cmd(command: Option<&str>, additional_args: &[String]) -> bool {
    command.is_none() && !additional_args.is_empty()
}

/// The image's default `CMD` that `additional_args` replace, when there is
/// one to report (#399).
fn replaced_image_cmd<'a>(
    command: Option<&str>,
    additional_args: &[String],
    image_cmd: Option<&'a [String]>,
) -> Option<&'a [String]> {
    image_cmd.filter(|cmd| {
        additional_args_replace_image_cmd(command, additional_args) && !cmd.is_empty()
    })
}

/// Builds Docker's `KEY=VALUE` environment variable list from a config
/// `environment` map. Sorted by key so callers (e.g. tests) see a
/// deterministic order despite `HashMap`'s unspecified iteration order.
fn build_env(environment: Option<&HashMap<String, String>>) -> Option<Vec<String>> {
    let environment = environment?;
    let mut pairs: Vec<String> = environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    pairs.sort();
    Some(pairs)
}

/// Builds Docker's `HostConfig.extra_hosts` list (`"name:ip"` entries, its
/// own `--add-host` mechanism) from a config `additional_hosts` map, plus
/// the proxy `host_gateway` entry when this run rewrote a proxy URL. Sorted
/// by key for the same determinism reason as `build_env`.
///
/// A config entry for the gateway's own name wins over the injected one: the
/// injection exists to supply a name nothing else provides, so where the
/// project has already said what that name means, it means that.
fn build_extra_hosts(
    additional_hosts: Option<&HashMap<String, String>>,
    host_gateway: Option<crate::proxy::HostGateway>,
) -> Option<Vec<String>> {
    if additional_hosts.is_none() && host_gateway.is_none() {
        return None;
    }
    let mut hosts: HashMap<&str, &str> = host_gateway
        .map(|gateway| (gateway.name, gateway.address))
        .into_iter()
        .collect();
    if let Some(additional_hosts) = additional_hosts {
        hosts.extend(
            additional_hosts
                .iter()
                .map(|(name, address)| (name.as_str(), address.as_str())),
        );
    }
    let mut pairs: Vec<String> = hosts
        .iter()
        .map(|(name, address)| format!("{name}:{address}"))
        .collect();
    pairs.sort();
    Some(pairs)
}

/// Builds Docker's `HostConfig.devices` from already-expanded
/// `(local_path, container_path, cgroup_permissions)` triples — pure,
/// unit-testable without a daemon. `None` when `devices` itself is `None`.
fn build_devices(
    devices: Option<&Vec<(String, String, Option<String>)>>,
) -> Option<Vec<DeviceMapping>> {
    let devices = devices?;
    Some(
        devices
            .iter()
            .map(|(local, container, options)| DeviceMapping {
                path_on_host: Some(local.clone()),
                path_in_container: Some(container.clone()),
                // Docker's own API has no default for this field — leaving
                // it unset makes runc fail outright ("device access at 16
                // field cannot be empty"). The `docker` CLI papers over
                // this with its own client-side default of "rwm"
                // (read/write/mknod, matching a device's default cgroup
                // permissions) when `--device`'s third field is omitted;
                // bollard talks to the API directly, so Ratect has to
                // apply that same default itself.
                cgroup_permissions: Some(options.clone().unwrap_or_else(|| "rwm".to_string())),
            })
            .collect(),
    )
}

/// Builds Docker's `HostConfig.network_mode` — pure, unit-testable without
/// a daemon. Docker's own `--network` spelling for the mode, the task's
/// network included: a container on it is created there, with its aliases
/// (`build_networking_config`), so it is never also on the daemon's default
/// `bridge` (ratect#303), as docker-client creates it.
fn build_network_mode(network: &ContainerNetwork) -> String {
    match network {
        ContainerNetwork::Task(name) => name.clone(),
        ContainerNetwork::Host => "host".to_string(),
        ContainerNetwork::None => "none".to_string(),
        ContainerNetwork::Container(id) => format!("container:{id}"),
    }
}

/// Builds Docker's `Config.networking_config` — pure, unit-testable without
/// a daemon. For a container on the task's network, that network's one
/// endpoint, aliased by `name` then each `additional_hostnames` entry, which
/// is how other containers reach it; `None` in any other mode, where there
/// is no endpoint to configure and docker-client sends none.
fn build_networking_config(
    network: &ContainerNetwork,
    name: &str,
    additional_hostnames: Option<&Vec<String>>,
) -> Option<NetworkingConfig> {
    let ContainerNetwork::Task(network) = network else {
        return None;
    };
    let aliases = std::iter::once(name.to_string())
        .chain(additional_hostnames.into_iter().flatten().cloned())
        .collect();
    Some(NetworkingConfig {
        endpoints_config: Some(HashMap::from([(
            network.clone(),
            EndpointSettings {
                aliases: Some(aliases),
                ..Default::default()
            },
        )])),
    })
}

/// The container's `Config.hostname`: its own name, except when it shares
/// another container's network namespace, where Docker refuses one — the
/// namespace owner's hostname is the one in use.
fn build_hostname(name: &str, network: &ContainerNetwork) -> Option<String> {
    (!network.shares_namespace()).then(|| name.to_string())
}

/// Builds Docker's `HostConfig.ulimits` from already-expanded
/// `(name, soft, hard)` triples — pure, unit-testable without a daemon.
/// `None` when `ulimits` itself is `None`, so a container that declares
/// none is created exactly as it was before ratect#95. The resource name is
/// forwarded verbatim; the daemon is what rejects one it doesn't know.
fn build_ulimits(ulimits: Option<&Vec<(String, i64, i64)>>) -> Option<Vec<ResourcesUlimits>> {
    let ulimits = ulimits?;
    Some(
        ulimits
            .iter()
            .map(|(name, soft, hard)| ResourcesUlimits {
                name: Some(name.clone()),
                soft: Some(*soft),
                hard: Some(*hard),
            })
            .collect(),
    )
}

/// Builds Docker's `HostConfig.tmpfs` from already-expanded
/// `(container_path, options)` pairs — pure, unit-testable without a daemon.
/// `None` when `tmpfs` itself is `None`. A repeated `container_path` last-one-
/// Docker's own label filter form, for
/// [`ResourceInventory::list_containers`]/[`list_networks`](ResourceInventory::list_networks):
/// `key=value` for an exact match, or a bare `key` for "has this label at
/// all". Docker ANDs the values under one filter name, so several entries
/// mean all of them must match.
///
/// An empty slice means no filter — every resource on the daemon. See the
/// trait methods' own warnings about that.
fn label_filters(labels: &[(&str, Option<&str>)]) -> HashMap<String, Vec<String>> {
    let mut filters = HashMap::new();
    if !labels.is_empty() {
        filters.insert(
            "label".to_string(),
            labels
                .iter()
                .map(|(key, value)| match value {
                    Some(value) => format!("{key}={value}"),
                    None => key.to_string(),
                })
                .collect(),
        );
    }
    filters
}

/// How Ratect removes every container it created — `v: true` above all,
/// matching Batect's own `removeContainer(..., force = true,
/// removeVolumes = true)`.
///
/// Without `v`, Docker's default, every *anonymous* volume a container
/// created is left behind: any image with a `VOLUME` instruction (which
/// is most database and cache images — `redis`, `postgres`, `mysql` all
/// declare one) produces one per container, per run, forever. They're
/// the worst kind of leftover, too: Docker names them with a random
/// hash and they carry no labels of ours — Docker creates them
/// implicitly, so there's no point at which Ratect could mark them —
/// which makes them the one resource the planned `resources` verb could
/// never identify. Not creating them is the only fix available.
///
/// `force` matches Batect for the same reason it does: the container is
/// stopped first, but a container that has already exited on its own
/// (or is still stopping) shouldn't turn cleanup into an error.
fn remove_container_options() -> bollard::query_parameters::RemoveContainerOptions {
    bollard::query_parameters::RemoveContainerOptionsBuilder::new()
        .v(true)
        .force(true)
        .build()
}

/// wins, same as building any other `HashMap` from a list — Docker itself
/// would reject a container with a genuinely duplicate mount point anyway.
fn build_tmpfs_mounts(tmpfs: Option<&Vec<(String, String)>>) -> Option<HashMap<String, String>> {
    let tmpfs = tmpfs?;
    Some(
        tmpfs
            .iter()
            .map(|(container, options)| (container.clone(), options.clone()))
            .collect(),
    )
}

/// Builds Docker's `HostConfig.log_config` from `log_driver`/`log_options` —
/// pure, unit-testable without a daemon. `None` when `log_driver` itself is
/// `None` (Docker's own API has no meaningful `log_options`-without-a-driver
/// shape — a `None` driver here still leaves the daemon's own configured
/// default alone, matching this module's `HostConfig { ..Default::default()
/// }` convention for every other unset field).
fn build_log_config(
    log_driver: Option<&str>,
    log_options: Option<&HashMap<String, String>>,
) -> Option<bollard::models::HostConfigLogConfig> {
    let log_driver = log_driver?;
    Some(bollard::models::HostConfigLogConfig {
        typ: Some(log_driver.to_string()),
        config: log_options.cloned(),
    })
}

/// One Docker resource Ratect created, as found again by its own labels —
/// see [`crate::labels`]. Deliberately the same shape for a container and a
/// network: what the caller wants to say about either is the same ("this,
/// from that task, that long ago"), and keeping one type means the
/// reporting code isn't written twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelledResource {
    pub id: String,
    /// Docker's own name — `<project>-<container>-<suffix>` for a container
    /// and `<project>-network-<suffix>` for a network Ratect created (see
    /// [`crate::resource_names`]). Never
    /// parsed: which container this is is the `container` label's to say.
    pub name: String,
    pub labels: HashMap<String, String>,
    /// When Docker created it, in seconds since the Unix epoch. Docker
    /// records this itself for both kinds, so nothing needs a label for it.
    /// `None` only if the daemon didn't report it.
    pub created: Option<i64>,
    /// A container's state (`running`, `exited`, ...) as Docker reports it;
    /// always `None` for a network, which doesn't have one.
    pub state: Option<String>,
}

pub use crate::container_spec::{
    BindMount, BindSource, ContainerNetwork, ContainerOptions, ContainerSpec, HealthCheckOptions,
    NetworkOptions,
};

/// Configures BuildKit-only build features — `build_image` receives one
/// only when a container declares `build_secrets` and/or `build_ssh`,
/// converted from config types the same way as `HealthCheckOptions` above.
/// Independent of *which builder* runs the build (that's
/// `select_builder_version`'s call, from the daemon's advertised default):
/// `None` just means no session providers to serve. The one interaction:
/// `Some` requires the BuildKit builder — the classic builder has no session
/// to serve these over, so `build_image` fails clearly if the classic
/// builder is selected while providers are present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildKitOptions {
    /// Keyed by the id a Dockerfile's `RUN --mount=type=secret,id=<key>`
    /// references.
    pub secrets: HashMap<String, BuildSecretSource>,
    /// SSH agents to serve to the build, keyed by the id a Dockerfile's
    /// `RUN --mount=type=ssh,id=<key>` selects (BuildKit's implicit
    /// `default` for a bare `RUN --mount=type=ssh`).
    pub ssh_agents: HashMap<String, SshAgentSource>,
}

/// Where one `build_ssh` agent's keys come from, mirroring
/// `config::SshAgent` after its `paths` have been classified by
/// [`classify_ssh_agent_paths`] — `docker.rs` deliberately doesn't depend
/// on config types (same conversion boundary as [`BuildSecretSource`]
/// above).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshAgentSource {
    /// Forward the *host* process's own `SSH_AUTH_SOCK` ssh-agent — an
    /// entry with no `paths`.
    HostAgent,
    /// Forward the ssh-agent listening on the given Unix socket.
    Socket(PathBuf),
    /// Serve the given private key files from an in-process agent, with no
    /// running agent involved at all — see [`crate::ssh_agent`].
    Keys(Vec<PathBuf>),
}

/// Classifies one `build_ssh` agent's resolved `paths` the way BuildKit's
/// own `sshprovider` does: no paths forwards the host's agent, a path that
/// *is* a Unix socket is an agent to forward to, and anything else is a
/// private key file to serve.
///
/// The rules about combining them are BuildKit's too, and are errors there
/// rather than config-schema violations — hence checking them here, at
/// build time, rather than at config load: a socket can't be combined with
/// anything, since forwarding to two agents at once under one id has no
/// meaning.
///
/// A path listed more than once counts once, keeping first-seen order:
/// Batect holds an agent's paths as a set, so a socket listed twice is one
/// socket to forward, not two (ratect#390).
pub fn classify_ssh_agent_paths(id: &str, paths: &[PathBuf]) -> Result<SshAgentSource> {
    if paths.is_empty() {
        return Ok(SshAgentSource::HostAgent);
    }

    let mut unique: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if !unique.contains(path) {
            unique.push(path.clone());
        }
    }
    let paths = unique.as_slice();

    let sockets: Vec<&PathBuf> = paths.iter().filter(|path| is_socket(path)).collect();
    match (sockets.len(), paths.len()) {
        (0, _) => Ok(SshAgentSource::Keys(paths.to_vec())),
        (1, 1) => Ok(SshAgentSource::Socket(sockets[0].clone())),
        (1, _) => Err(anyhow::anyhow!(
            "The 'build_ssh' agent '{}' lists both the agent socket '{}' and other files — \
             an agent socket has to be the entry's only path",
            id,
            sockets[0].display()
        )),
        _ => Err(anyhow::anyhow!(
            "The 'build_ssh' agent '{}' lists {} agent sockets, but only one agent can be \
             forwarded under a single id",
            id,
            sockets.len()
        )),
    }
}

/// Whether `path` is a Unix socket. A path that doesn't exist (or can't be
/// read) is *not* reported as a socket, so it goes down the private-key
/// route and fails naming the file, rather than as an opaque failure to
/// connect.
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

/// One `build_secrets` entry's source, mirroring `config::BuildSecret` —
/// `docker.rs` deliberately doesn't depend on config types (same
/// conversion boundary as `HealthCheckOptions` above).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSecretSource {
    /// Read from the given environment variable in *this* (the `ratect`
    /// process's own) environment at build time.
    Environment(String),
    /// Read from the given file path (already resolved to absolute) on the
    /// host at build time.
    File(PathBuf),
}

/// The outcome of one `exec_in_container` run: the exec'd command's exit
/// code plus its combined stdout/stderr (interleaved — the exec runs with a
/// TTY, which merges the two streams), so a failed setup command's error can
/// include what it printed.
#[derive(Debug)]
pub struct ExecResult {
    pub exit_code: i64,
    pub output: String,
}

/// Builds Docker's container-creation healthcheck override from a
/// container's `health_check` config — `None` when the container declares no
/// override at all, leaving the image's own `HEALTHCHECK` untouched. Pure,
/// unit-testable without a daemon. Durations become the nanosecond counts
/// Docker's API expects, clamped to the `i64` it takes them in — as Batect's
/// own Docker client does — so a duration over about 292 years is sent as
/// the longest one Docker can hold rather than wrapping to a negative.
fn build_health_config(health_check: Option<&HealthCheckOptions>) -> Option<HealthConfig> {
    let health_check = health_check?;
    let nanos = |d: std::time::Duration| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX);
    Some(HealthConfig {
        test: health_check
            .command
            .as_ref()
            .map(|command| vec!["CMD-SHELL".to_string(), command.clone()]),
        interval: health_check.interval.map(nanos),
        timeout: health_check.timeout.map(nanos),
        retries: health_check.retries.map(i64::from),
        start_period: health_check.start_period.map(nanos),
        start_interval: None,
    })
}

/// Builds Docker's `Config.exposed_ports` + `HostConfig.port_bindings` from
/// already-expanded `(local_port, container_port, protocol)` triples — pure,
/// unit-testable without a daemon. `None` when `ports` itself is `None`
/// (absent, or already filtered out by `--disable-ports` — see
/// `NetworkOptions::ports`) or empty.
///
/// One container port published on several host ports gets a binding for
/// each, in input order, under its one exposed key — as docker-client does,
/// rather than the last replacing the rest (ratect#291). An identical triple
/// binds once.
fn build_port_config(ports: Option<&Vec<(u16, u16, String)>>) -> Option<(Vec<String>, PortMap)> {
    let ports = ports?;
    if ports.is_empty() {
        return None;
    }

    let mut exposed_ports = Vec::new();
    let mut port_bindings = PortMap::new();
    for (local_port, container_port, protocol) in ports {
        let key = format!("{container_port}/{protocol}");
        if !exposed_ports.contains(&key) {
            exposed_ports.push(key.clone());
        }
        let binding = PortBinding {
            host_ip: None,
            host_port: Some(local_port.to_string()),
        };
        let bindings = port_bindings
            .entry(key)
            .or_insert(None)
            .get_or_insert_with(Vec::new);
        if !bindings.contains(&binding) {
            bindings.push(binding);
        }
    }

    Some((exposed_ports, port_bindings))
}

/// Builds an in-memory tar of `build_directory`'s contents to use as a
/// Docker build context — pure filesystem-in, bytes-out, no Docker
/// involved, so it's unit-testable without a daemon.
///
/// Reads a `.dockerignore` at `build_directory`'s own root, if present (a
/// missing one is equivalent to an empty pattern list — every file is
/// included, unchanged from before `.dockerignore` support existed), and
/// excludes anything it matches via [`dockerignore::PatternMatcher`] — see
/// that crate's docs for why this isn't the same as `.gitignore`'s
/// matching rules. `dockerfile` (relative to `build_directory`'s own root,
/// `"Dockerfile"` for the default case) and `.dockerignore` themselves are
/// always included regardless of exclusion patterns, mirroring Docker's own
/// special-casing (otherwise a broad `*` pattern would exclude the file the
/// build needs).
///
/// Every entry is owned by root (uid and gid 0), as docker-client makes
/// Batect's, keeping each file's own mode: BuildKit carries a context file's
/// ownership into a `COPY` without `--chown`, so the host user's uid would
/// otherwise end up on files in the image.
///
/// Known simplifications: symlinks are silently skipped (rare in build
/// contexts; proper support needs tar symlink entries, not just file
/// copies), and empty directories aren't preserved as their own tar
/// entries (only added implicitly as the parent of a file within them).
fn build_context_tar(build_directory: &Path, dockerfile: &str) -> Result<Vec<u8>> {
    let dockerignore_path = build_directory.join(".dockerignore");
    let patterns = if dockerignore_path.is_file() {
        let file = fs::File::open(&dockerignore_path)
            .with_context(|| format!("Failed to open {:?}", dockerignore_path))?;
        dockerignore::read_ignore_file(file)
            .with_context(|| format!("Failed to read {:?}", dockerignore_path))?
    } else {
        Vec::new()
    };
    let matcher = dockerignore::PatternMatcher::new(&patterns)
        .with_context(|| format!("Invalid pattern in {:?}", dockerignore_path))?;

    let mut entries = Vec::new();
    collect_build_context_entries(build_directory, build_directory, &mut entries)?;

    let mut builder = tar::Builder::new(Vec::new());
    for (absolute_path, relative_path) in entries {
        let force_include = relative_path == dockerfile || relative_path == ".dockerignore";
        if !force_include && matcher.matches_or_parent_matches(&relative_path) {
            continue;
        }
        let mut add = || -> std::io::Result<()> {
            let file = fs::File::open(&absolute_path)?;
            let mut header = tar::Header::new_gnu();
            header.set_metadata_in_mode(&file.metadata()?, tar::HeaderMode::Complete);
            header.set_uid(0);
            header.set_gid(0);
            builder.append_data(&mut header, &relative_path, file)
        };
        add().with_context(|| format!("Failed to add {:?} to build context", absolute_path))?;
    }

    builder
        .into_inner()
        .context("Failed to finalize build context archive")
}

/// Builds `build_directory` via Docker's BuildKit builder — the same classic
/// `/build` endpoint the non-BuildKit path uses, but with
/// `BuilderVersion::BuilderBuildKit` and a per-build session: the channel the
/// daemon calls back over to have `build_secrets`/`build_ssh` requests served
/// mid-build (the pre-BuildKit builder has no such channel at all). The
/// session upgrades the *existing* Docker daemon's own `/session`+`/grpc`
/// endpoints — no separate persistent builder container needed. Requires the
/// session-providers support carried by this workspace's `[patch.crates-io]`
/// bollard fork (see the root `Cargo.toml`) until it lands upstream.
///
/// Unlike the classic path's plain `stream` lines, BuildKit reports progress
/// as structured `StatusResponse` messages — vertexes (build steps) plus
/// their raw log chunks. Both are accumulated into the same kind of
/// transcript the classic path keeps: logged at `debug` live, and folded
/// into a failure's error via `build_output_suffix`. The built image ID
/// arrives in the same stream (a final `Default` aux message), same as the
/// classic path — no post-build lookup needed.
#[allow(clippy::too_many_arguments)]
async fn build_image_via_buildkit(
    docker: &Docker,
    event_sink: &dyn EventSink,
    build_directory: &Path,
    dockerfile: &str,
    build_args: Option<&HashMap<String, String>>,
    target: Option<&str>,
    buildkit: Option<&BuildKitOptions>,
    tag: &str,
    force_pull: bool,
    proxy_host_gateway: Option<crate::proxy::HostGateway>,
    credentials: Option<HashMap<String, bollard::auth::DockerCredentials>>,
    build_id: &str,
) -> Result<String> {
    let build_directory = build_directory.to_path_buf();
    let dockerfile_owned = dockerfile.to_string();
    let tar_bytes =
        tokio::task::spawn_blocking(move || build_context_tar(&build_directory, &dockerfile_owned))
            .await
            .context("Failed to build the Docker build context")??;

    let session_id = uuid::Uuid::new_v4().to_string();
    let mut options_builder = BuildImageOptionsBuilder::default()
        .dockerfile(dockerfile)
        .t(tag)
        .rm(true)
        .forcerm(true)
        .version(bollard::query_parameters::BuilderVersion::BuilderBuildKit)
        .session(&session_id)
        .buildid(build_id);
    if let Some(build_args) = build_args {
        options_builder = options_builder.buildargs(build_args);
    }
    if let Some(target) = target {
        options_builder = options_builder.target(target);
    }
    if force_pull {
        options_builder = options_builder.pull("true");
    }
    if let Some(gateway) = proxy_host_gateway {
        options_builder = options_builder.extrahosts(&gateway.extra_host());
    }

    let mut providers = bollard::grpc::build::ImageBuildSessionProviders::default();
    // One in-process ssh-agent per `build_ssh` entry that names key files,
    // each alive for as long as this vector is — see the `Keys` arm below.
    let mut keyrings = Vec::new();
    if let Some(buildkit) = buildkit {
        for (id, secret) in &buildkit.secrets {
            let source = match secret {
                BuildSecretSource::Environment(name) => {
                    bollard::grpc::build::SecretSource::Env(name.clone())
                }
                BuildSecretSource::File(path) => {
                    bollard::grpc::build::SecretSource::File(path.clone())
                }
            };
            providers = providers.set_secret(id, &source);
        }
        if !buildkit.secrets.is_empty() {
            // BuildKit deliberately excludes a secret mount's *value* from its
            // layer cache key (so a secret's content can't leak into a cache
            // key/log) — meaning a `RUN --mount=type=secret` layer is a cache
            // hit even when the secret's value changed and nothing else in the
            // Dockerfile did, silently serving stale secret content baked into
            // a previous build. Disabling the cache whenever secrets are in
            // play avoids that trap; `build_ssh`-only builds are unaffected
            // (ordinary caching semantics — no equivalent value-vs-cache-key
            // mismatch, since forwarding *the same* agent isn't expected to
            // vary the way a secret's value is).
            options_builder = options_builder.nocache(true);
        }
        for (id, source) in &buildkit.ssh_agents {
            let source = match source {
                SshAgentSource::HostAgent => bollard::grpc::SshAgentSource::DefaultAgentSocket,
                SshAgentSource::Socket(path) => bollard::grpc::SshAgentSource::Socket(path.clone()),
                SshAgentSource::Keys(key_files) => {
                    // Held in `keyrings` until the build's stream is fully
                    // drained below: dropping one stops its agent and
                    // removes its socket, so an early drop would break
                    // exactly the `RUN --mount=type=ssh` this exists for.
                    let keyring = crate::ssh_agent::Keyring::start(key_files)
                        .await
                        .with_context(|| {
                            format!("Failed to serve the keys for SSH agent '{id}'")
                        })?;
                    let socket = keyring.socket_path().to_path_buf();
                    keyrings.push(keyring);
                    bollard::grpc::SshAgentSource::Socket(socket)
                }
            };
            providers = providers.set_ssh_agent(Some(id), &source);
        }
    }
    let options = options_builder.build();

    let mut stream = docker.build_image_with_session_providers(
        options,
        credentials,
        Some(bollard::body_full(tar_bytes.into())),
        providers,
    );

    let mut image_id = None;
    // The full build transcript — same purpose as the classic path's (see
    // `build_image` below), just assembled from BuildKit's structured status
    // messages: each vertex (build step) name once, its raw log output, and
    // any per-vertex error.
    let mut output = String::new();
    let mut seen_vertexes = std::collections::HashSet::new();
    let mut seen_vertex_errors = std::collections::HashSet::new();
    // Skipped entirely (not just discarded downstream) when the active
    // logger doesn't render it — see `EventSink::wants_progress_detail`.
    let wants_progress = event_sink.wants_progress_detail();
    while let Some(result) = stream.next().await {
        match result {
            Ok(info) => match info.aux {
                Some(bollard::models::BuildInfoAux::BuildKit(status)) => {
                    for vertex in &status.vertexes {
                        // Vertexes are re-sent on every state change — and
                        // the very first status message announces the *whole*
                        // build graph upfront, before anything runs, in graph
                        // (not execution) order. So a step's name is recorded
                        // when it first reports *started* — execution order,
                        // matching the docker CLI — not on first sight, which
                        // would dump the entire plan at once (and reversed).
                        // Errors are recorded once, when they appear.
                        if vertex.started.is_some()
                            && seen_vertexes.insert(vertex.digest.clone())
                            && !vertex.name.is_empty()
                        {
                            let cached_suffix = if vertex.cached { " CACHED" } else { "" };
                            tracing::debug!(image = tag, "{}{}", vertex.name, cached_suffix);
                            output.push_str(&vertex.name);
                            output.push_str(cached_suffix);
                            output.push('\n');
                            if wants_progress {
                                event_sink.post(TaskEvent::ImageBuildProgress {
                                    tag: tag.to_string(),
                                    message: format!("{}{}", vertex.name, cached_suffix),
                                });
                            }
                        }
                        if !vertex.error.is_empty()
                            && seen_vertex_errors.insert(vertex.digest.clone())
                        {
                            tracing::debug!(image = tag, "{}", vertex.error);
                            output.push_str(&vertex.error);
                            output.push('\n');
                        }
                    }
                    for log in &status.logs {
                        // Raw output chunks from a step — not necessarily
                        // whole lines, so appended verbatim rather than
                        // line-trimmed like the classic path's stream lines.
                        let msg = String::from_utf8_lossy(&log.msg);
                        output.push_str(&msg);
                        let trimmed = msg.trim_end();
                        if !trimmed.is_empty() {
                            tracing::debug!(image = tag, "{trimmed}");
                            if wants_progress {
                                event_sink.post(TaskEvent::ImageBuildProgress {
                                    tag: tag.to_string(),
                                    message: trimmed.to_string(),
                                });
                            }
                        }
                    }
                }
                Some(bollard::models::BuildInfoAux::Default(aux_image_id)) => {
                    if let Some(id) = aux_image_id.id {
                        image_id = Some(id);
                    }
                }
                None => {}
            },
            Err(e) => {
                return Err(e).context(format!(
                    "Failed to build image '{}'{}",
                    tag,
                    build_output_suffix(&output)
                ));
            }
        }
    }

    // Explicit rather than incidental: every `RUN --mount=type=ssh` in the
    // build is served over these sockets, so they have to outlive the
    // stream above. Dropping them here says so, where dropping them at the
    // end of the function would leave it to whoever moves this code next.
    drop(keyrings);

    let image_id = image_id.ok_or_else(|| {
        anyhow::anyhow!("Docker did not report an image ID after building '{}'", tag)
    })?;

    Ok(image_id)
}

/// Formats `output` (the build log accumulated so far) as a "Build output:"
/// section to append to a build failure message, or an empty string if
/// nothing was captured yet (e.g. the build failed before Docker streamed
/// anything). Kept separate from `build_image` so it's unit-testable without
/// a Docker daemon.
fn build_output_suffix(output: &str) -> String {
    let trimmed = output.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n\nBuild output:\n{trimmed}")
    }
}

/// Whether a container run should actually get a real Docker TTY.
/// `interactive` is eligibility — this is a task's own container, the named
/// task's or a prerequisite's, see `TaskEngine::run_task_internal` — not a
/// guarantee: it's further gated on the local process's own stdin *and*
/// stdout genuinely being connected to a terminal. Stdin forwarding is not gated on this —
/// see [`StdinFlags`].
fn should_use_tty(interactive: bool, stdin_is_tty: bool, stdout_is_tty: bool) -> bool {
    interactive && stdin_is_tty && stdout_is_tty
}

/// A spawned task aborted when this is dropped, not only when a session
/// reaches its own end: a session's future can be dropped mid-way — a
/// cancelled run drops the task container's run — and a detached stdin pump
/// would otherwise keep [`stdin::StdinReader`] to itself, taking what is
/// typed during cleanup.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts a session's stdin pump, stopped when the returned guard is
/// dropped — however the session ends.
fn spawn_stdin_pump(
    reader: &'static stdin::StdinReader,
    attach_input: std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
) -> AbortOnDrop {
    AbortOnDrop(tokio::spawn(reader.forward(attach_input)))
}

/// The stdin half of a task container's creation config. `open_stdin`/
/// `attach_stdin` follow `interactive` alone — wider than `use_tty` — so
/// piping input into a task works even when no TTY is allocated (Ratect's
/// own stdout redirected to a file, say), matching Batect's unconditional
/// stdin forwarding for each task's own container, prerequisites included.
/// `stdin_once` follows them, not the TTY: it is what makes Docker close the
/// container's stdin when the attach connection's input ends, and without it piped input reaching its
/// end never reaches the container, so a process that runs until its input
/// ends never exits (ratect#220). `docker run -i` and Batect's docker-client
/// both set it whenever stdin is attached. Only `tty` itself — pty
/// allocation — stays `use_tty`-gated.
#[derive(Debug, PartialEq, Eq)]
struct StdinFlags {
    tty: Option<bool>,
    open_stdin: Option<bool>,
    attach_stdin: Option<bool>,
    stdin_once: Option<bool>,
}

fn stdin_flags(interactive: bool, use_tty: bool) -> StdinFlags {
    StdinFlags {
        tty: use_tty.then_some(true),
        open_stdin: interactive.then_some(true),
        attach_stdin: interactive.then_some(true),
        stdin_once: interactive.then_some(true),
    }
}

/// Puts the local terminal into raw mode for the duration of an interactive
/// container session — no local line buffering/echo, so every keystroke
/// passes straight through to the container's own TTY instead of being
/// handled locally first. Restores the terminal on `Drop`, so it's never
/// left in raw mode even if the session ends via an error return.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("Failed to enable raw terminal mode")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Err(e) = crossterm::terminal::disable_raw_mode() {
            tracing::warn!(error = ?e, "Failed to restore terminal mode");
        }
    }
}

/// Drives a container's attached output stream to completion,
/// line-buffering it into [`TaskEvent::ContainerOutput`] events on
/// `event_sink` — the [`ContainerIoStreaming::Interleaved`] policy's (the
/// `all` output mode's) per-container streaming, shared by
/// `start_and_stream_output`'s interleaved branch (the task's own container,
/// whose stream error should propagate and fail the task) and
/// `start_background_container`'s spawned follower (a dependency's whole
/// lifetime, fire-and-forget — that caller logs and swallows the error
/// instead). Takes the stream rather than attaching itself, so each caller
/// can attach before its own `start` and the line-buffering is unit-testable
/// against a synthetic stream, with no live daemon needed. Always flushes
/// any buffered partial line before returning, whether `output` ended cleanly
/// or with an error.
async fn drain_interleaved_output(
    output: impl futures::Stream<Item = Result<bollard::container::LogOutput, bollard::errors::Error>>,
    event_sink: &std::sync::Arc<dyn EventSink>,
    container_name: &str,
) -> Result<()> {
    let mut output = std::pin::pin!(output);
    let mut line_buffer = crate::ui::interleaved::LineBuffer::new();
    let emit = |line: &str| {
        event_sink.post(TaskEvent::ContainerOutput {
            container: container_name.to_string(),
            line: line.to_string(),
        });
    };
    let result = loop {
        match output.next().await {
            Some(Ok(output)) => line_buffer.push(output.as_ref(), emit),
            Some(Err(e)) => break Err(e).context("Failed to read container output"),
            None => break Ok(()),
        }
        // The logger's `Console` ignores a failed write, so its reader having
        // gone is asked for rather than seen here — see `StdoutClosed`. Only
        // the task container's lines can make it true, but a dependency's
        // follower stops on it too: the run is ending by then.
        if event_sink.result_reader_gone() {
            break Err(StdoutClosed.into());
        }
    };
    line_buffer.flush(emit);
    result
}

/// Resizes `container_id`'s TTY to the local terminal's current size —
/// shared by the initial attach-time sync and every subsequent local resize
/// while the session is live (see `run_container_interactively` and
/// `spawn_resize_listener` below). Takes `&Docker` directly rather than
/// `&self` so it can also be called from a separately spawned task, holding
/// its own cloned client rather than borrowing the caller's. Best-effort: a
/// failure is logged and otherwise ignored, matching the previous one-shot
/// call this replaces.
async fn resize_tty(docker: &Docker, container_id: &str) {
    let Ok((cols, rows)) = crossterm::terminal::size() else {
        return;
    };
    let resize_options = ResizeContainerTTYOptionsBuilder::default()
        .w(cols as i32)
        .h(rows as i32)
        .build();
    if let Err(e) = docker
        .resize_container_tty(container_id, resize_options)
        .await
    {
        match &e {
            // A short-lived interactive task (e.g. a plain `echo`) can exit
            // before the attach-time size sync lands (409, "is not
            // running"), or even be cleaned up entirely by the time a
            // terminal-resize signal arrives (404) — benign races on an
            // otherwise clean run, nothing a user could act on, so not
            // worth a warning.
            bollard::errors::Error::DockerResponseServerError { status_code, .. }
                if *status_code == 409 || *status_code == 404 =>
            {
                tracing::debug!(
                    container_id,
                    error = ?e,
                    "Skipping TTY resize — the container has already exited"
                );
            }
            _ => tracing::warn!(container_id, error = ?e, "Failed to resize container TTY"),
        }
    }
}

/// Listens for `SIGWINCH` (the local terminal being resized) for the
/// lifetime of one interactive session, re-running `resize_tty` on every
/// occurrence — closes the "not tracked live" gap `resize_tty`'s own
/// one-shot call used to leave (see `docs/differences-from-batect.md`).
/// Deliberately built on `tokio::signal::unix`, not crossterm's
/// `event`/`EventStream` API — see the `crossterm` entry in CLAUDE.md for
/// why that API is off-limits here (it would consume/interpret stdin bytes
/// instead of passing them through raw); a plain OS signal doesn't have
/// that problem. Unix-only — `SignalKind::window_change()` doesn't exist on
/// other platforms, where the caller just doesn't spawn this, falling back
/// to the previous once-at-attach-only behavior rather
/// than erroring (interactive mode itself stays cross-platform — this is a
/// narrower, non-fatal gap on non-Unix, unlike `user.rs`'s hard Unix-only
/// functions).
#[cfg(unix)]
fn spawn_resize_listener(docker: Docker, container_id: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut sig =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()) {
                Ok(sig) => sig,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        "Failed to install SIGWINCH handler; live terminal-resize forwarding \
                         disabled for this session"
                    );
                    return;
                }
            };
        loop {
            sig.recv().await;
            resize_tty(&docker, &container_id).await;
        }
    })
}

/// Recursively collects every regular file under `dir` as `(absolute_path,
/// path_relative_to_root)` pairs, the latter always `/`-joined regardless
/// of platform, matching the path style `.dockerignore` patterns use.
fn collect_build_context_entries(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    let read_dir =
        fs::read_dir(dir).with_context(|| format!("Failed to read directory {:?}", dir))?;
    for entry in read_dir {
        let entry = entry.with_context(|| format!("Failed to read entry in {:?}", dir))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to determine file type of {:?}", path))?;

        if file_type.is_dir() {
            collect_build_context_entries(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walked path is always under root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((path, relative));
        }
    }
    Ok(())
}

/// The host user a container should run as, when its `run_as_current_user`
/// config is enabled — see `TaskEngine::resolve_user_mapping`. Derives
/// `PartialEq`/`Eq` so it can nest inside `ContainerSpec`, which does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMapping {
    pub user: crate::user::CurrentUser,
    pub home_directory: String,
    /// Absolute container paths of this container's writable `cache`
    /// mounts, which need the same ownership treatment the home directory
    /// gets.
    ///
    /// A fresh Docker volume is created root-owned, so a container running
    /// as the host user cannot write to one — the mount succeeds and the
    /// first write fails, which is a confusing place to discover it. Batect
    /// uploads a directory entry per cache mount for exactly this reason
    /// (`uploadCacheDirectories`), but for every one: this list leaves out
    /// a read-only mount, whose upload would fail (see
    /// `TaskEngine::resolve_user_mapping`).
    pub cache_directories: Vec<String>,
}

/// Appends a plain file entry (`name`, `contents`, `mode`) to `builder`,
/// owned by root (`0:0` — these files must be root-owned regardless of which
/// uid/gid the container itself runs as, matching real `/etc/passwd`-style
/// files on any system).
fn append_tar_file(
    builder: &mut tar::Builder<Vec<u8>>,
    name: &str,
    contents: &str,
    mode: u32,
) -> Result<()> {
    let data = contents.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    builder
        .append_data(&mut header, name, data)
        .with_context(|| format!("Failed to add {name} to user mapping archive"))
}

/// Builds an in-memory tar containing minimal `/etc/passwd`, `/etc/shadow`,
/// and `/etc/group` entries for `mapping`'s user, extracted to `/etc` (see
/// `ContainerRuntime::run_container`'s `user_mapping` handling) before the
/// container starts. Necessary because a container running as an arbitrary
/// host uid/gid has no corresponding entry in the image's own passwd/group —
/// many programs misbehave or refuse to run at all without one. Pure (no
/// Docker involved), so it's unit-testable directly. Ported from Batect's
/// `RunAsCurrentUserConfigurationProvider.uploadFilesForConfiguration`.
fn build_user_mapping_tar(mapping: &UserMapping) -> Result<Vec<u8>> {
    let passwd = crate::user::generate_passwd_file(&mapping.user, &mapping.home_directory);
    let shadow = crate::user::generate_shadow_file(&mapping.user);
    let group = crate::user::generate_group_file(&mapping.user);

    let mut builder = tar::Builder::new(Vec::new());
    append_tar_file(&mut builder, "passwd", &passwd, 0o644)?;
    append_tar_file(&mut builder, "shadow", &shadow, 0o640)?;
    append_tar_file(&mut builder, "group", &group, 0o644)?;

    builder
        .into_inner()
        .context("Failed to finalize user mapping archive")
}

/// Builds an in-memory tar containing a single directory entry for `path`'s
/// leaf name, owned by `uid`/`gid`, mode `0755` — extracted into `path`'s
/// *parent*, which is how Docker's archive API is made to set ownership on
/// a directory that already exists (a mount point, typically).
///
/// Used for both the home directory and each cache mount, matching Batect's
/// `uploadHomeDirectoryForConfiguration`/`uploadCacheDirectories`, which
/// share one `uploadDirectory` for the same reason. Pure (no Docker
/// involved), so it's unit-testable directly.
fn build_owned_directory_tar(path: &str, uid: u32, gid: u32) -> Result<Vec<u8>> {
    if !path.starts_with('/') {
        anyhow::bail!("'{path}' is not an absolute path");
    }
    let leaf_name = Path::new(path)
        .file_name()
        .with_context(|| format!("Invalid directory '{path}': no directory name"))?
        .to_string_lossy()
        .into_owned();

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_uid(uid as u64);
    header.set_gid(gid as u64);
    header.set_size(0);
    header
        .set_path(format!("{leaf_name}/"))
        .with_context(|| format!("Invalid directory '{path}'"))?;
    header.set_cksum();

    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append(&header, std::io::empty())
        .context("Failed to add directory to user mapping archive")?;

    builder
        .into_inner()
        .context("Failed to finalize directory archive")
}

/// The parent directory `build_owned_directory_tar`'s entry should be
/// extracted into — `/` if `path` has no parent (e.g. `/home`).
fn owned_directory_parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("/"))
        .to_string_lossy()
        .into_owned()
}

/// Creates what a container running as the host user needs on the host and
/// doesn't have yet — as that user, before the container is created.
/// Otherwise Docker's daemon (running as root) would auto-create it as
/// `root:root` on first use, defeating the point of `run_as_current_user`
/// for the common "mount my code directory, get build artifacts back with
/// sane ownership" case. Ported from Batect's
/// `createMissingMountDirectories`, whose two loops are
/// [`create_missing_host_sources`] and [`create_nested_mount_points`] — with
/// two divergences: a nested mount point is created with the type its source
/// needs, where Batect always creates a directory, and a symlink on the way
/// to one is followed only while it stays inside the enclosing mount.
///
/// A nested mount point is created only where ratect is sure of the host
/// path and may write there; anything else is left for the daemon, which
/// creates it as root, as it did before that loop existed. Creating in the
/// wrong place is the worse of the two ways to get that wrong. (A missing
/// host source still fails the run when it can't be created, as in Batect.)
///
/// Pure filesystem logic, no Docker involved.
fn ensure_host_volume_directories_exist(
    binds: &[BindMount],
    tmpfs: &[(String, String)],
) -> Result<()> {
    create_missing_host_sources(binds)?;
    create_nested_mount_points(binds, tmpfs)
}

/// Each host mount's own source, as a directory, when it's missing.
fn create_missing_host_sources(binds: &[BindMount]) -> Result<()> {
    for path in binds.iter().filter_map(BindMount::host_path) {
        if is_special_docker_desktop_path(path) {
            continue;
        }
        // Only a *missing* source is pre-created. The whole reason to do so
        // is that Docker otherwise creates a missing bind source as a
        // root-owned directory the mapped user can't write to; an existing
        // source has no such problem and must be left exactly as it is —
        // whether it's a directory Docker will reuse, or a single file or
        // socket the config means to bind-mount (`~/.gitconfig`, an SSH
        // agent socket). `create_dir_all` over an existing non-directory
        // fails, which is exactly how a valid file/socket mount used to
        // break. Matches Batect's own `!Files.exists(path)` guard. The
        // theoretical TOCTOU race this reintroduces (something replacing
        // `path` between check and create) is the lesser problem by far.
        if path.exists() {
            continue;
        }
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create host directory {:?}", path))?;
    }

    Ok(())
}

/// One mount's container path, for [`create_nested_mount_points`].
struct MountPoint<'a> {
    container: &'a str,
    segments: Vec<String>,
    /// `Some` for a host mount; `None` for a volume or a `tmpfs`.
    host_source: Option<&'a Path>,
}

/// Each mount point that falls inside a host mount's container path — of a
/// host mount, a volume or a `tmpfs` — created inside that host mount's
/// source, with every directory above it, when it's missing. Only the
/// *closest* enclosing mount (Batect's rule: the longest other container
/// path that is a strict segment prefix, both cleaned as Docker cleans
/// them) counts, and only when it is a host mount of a directory: one inside
/// a volume is the volume's to hold, and a file, a socket or a Docker
/// Desktop special path has nowhere to put one.
///
/// The mount point is an empty regular file when its source is an existing
/// non-directory (a file, a socket, a device), since Linux binds a
/// non-directory onto a file; otherwise a directory. Batect always creates a
/// directory, which fails a nested file or socket mount at container start.
/// A host mount of a Docker Desktop special path gets nothing (its type
/// can't be known on the host), and an existing mount point is never
/// touched, whatever its type — a dangling symlink included. Nor is a
/// symlink on the way to one followed ([`missing_mount_point`]).
fn create_nested_mount_points(binds: &[BindMount], tmpfs: &[(String, String)]) -> Result<()> {
    let mounts: Vec<MountPoint> = binds
        .iter()
        .map(|bind| (bind.container.as_str(), bind.host_path()))
        .chain(
            tmpfs
                .iter()
                .map(|(container, _)| (container.as_str(), None)),
        )
        .map(|(container, host_source)| MountPoint {
            container,
            segments: container_path_segments(container),
            host_source,
        })
        .collect();

    for mount in &mounts {
        let closest_parent = mounts
            .iter()
            .filter(|parent| {
                parent.segments.len() < mount.segments.len()
                    && mount.segments.starts_with(&parent.segments)
            })
            .max_by_key(|parent| parent.segments.len());
        let Some(parent) = closest_parent else {
            continue;
        };
        let Some(parent_source) = parent.host_source.filter(|source| source.is_dir()) else {
            continue;
        };
        if mount
            .host_source
            .is_some_and(is_special_docker_desktop_path)
        {
            continue;
        }
        let Some(path) =
            missing_mount_point(parent_source, &mount.segments[parent.segments.len()..])
        else {
            continue;
        };
        let source_is_a_file = mount
            .host_source
            .is_some_and(|source| source.exists() && !source.is_dir());
        let created = if source_is_a_file {
            path.parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map(drop)
                })
        } else {
            fs::create_dir_all(&path)
        };
        match created {
            // Somewhere the host user may not write — a directory a root
            // daemon created on an earlier run, say — is the daemon's to
            // create in, as root, as it was before this step existed.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            created => created.with_context(|| {
                format!(
                    "Failed to create the mount point for {} at {:?}",
                    mount.container, path
                )
            })?,
        }
    }

    Ok(())
}

/// Where a mount point `relative` to `parent_source` should be created on
/// the host — `None` when ratect should create nothing and leave it to the
/// daemon. Each component that already exists is followed on the host, but
/// only while it resolves to a directory still inside `parent_source`
/// (itself resolved, so that the mount's own source counts as written): a
/// symlink leading out of it would otherwise send the creation wherever it
/// points on the host, outside the mount the daemon resolves the same path
/// in (ratect#703). `None`, too, when the mount point already exists, or
/// when something on the way can't be resolved — a dangling symlink, a loop,
/// a directory the host user can't search — since the daemon, running as
/// root, may see what this process can't.
///
/// That agrees with the daemon for a relative link that stays below
/// `parent_source`. The daemon resolves an absolute link, or one climbing
/// above the mount, against the container's own filesystem instead, so for
/// one that still lands inside `parent_source` on the host, the entry created
/// here is one nothing mounts over: a stray, but inside the mount, never
/// outside it. Matching the daemon exactly would mean modelling its
/// in-container resolution.
///
/// The path returned is fully resolved, so only components that were missing
/// when it was checked are left to create.
fn missing_mount_point(parent_source: &Path, relative: &[String]) -> Option<PathBuf> {
    let root = parent_source.canonicalize().ok()?;
    let mut resolved = root.clone();
    for (index, segment) in relative.iter().enumerate() {
        let next = resolved.join(segment);
        match fs::symlink_metadata(&next) {
            Ok(_) if index + 1 == relative.len() => return None,
            Ok(_) => {
                let real = next.canonicalize().ok()?;
                if !real.starts_with(&root) || !real.is_dir() {
                    return None;
                }
                resolved = real;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Some(
                    relative[index..]
                        .iter()
                        .fold(resolved, |path, segment| path.join(segment)),
                );
            }
            Err(_) => return None,
        }
    }
    None
}

/// `path`'s segments once cleaned, as Docker cleans a mount's destination —
/// so `/code/` and `/code` are the same mount point (as in Batect's
/// `splitToPathSegments`), and `/code/../scratch` is `/scratch`, not a path
/// inside `/code` (where Batect would say it is).
fn container_path_segments(path: &str) -> Vec<String> {
    use path_clean::PathClean;
    Path::new(path)
        .clean()
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// Docker Desktop injects a few paths into its own Linux VM that don't
/// exist on the host at all — most notably the SSH agent socket at
/// `/run/host-services/ssh-auth.sock`. `mkdir -p` on one fails (there's no
/// `/run` on a macOS host to create under), so they're skipped and left for
/// Docker to provide. Component-wise prefix match, so
/// `/run/host-services-something` isn't caught. Ported from Batect's own
/// `isSpecialDockerDesktopPath`.
fn is_special_docker_desktop_path(path: &Path) -> bool {
    path.starts_with("/run/host-services") || path.starts_with("/run/guest-services")
}

/// Abstracts the container operations the task engine needs, so tests can
/// inject a fake implementation instead of talking to a real Docker daemon.
/// Requires [`ResourceInventory`] and [`VolumeStore`] as supertraits rather
/// than declaring their methods again here — those are the narrower
/// contracts `resources.rs`/`cache.rs` actually depend on, so a caller that
/// only needs one of them bounds on it directly instead of the whole
/// surface; `engine.rs`, which genuinely needs everything, keeps bounding on
/// `ContainerRuntime` and gets both for free.
#[async_trait::async_trait]
pub trait ContainerRuntime: ResourceInventory + VolumeStore {
    /// Pulls `image` and returns the ID of exactly the image pulled, as
    /// Batect's docker-client does — not `image`, a mutable tag another
    /// pull or `docker tag` could move before the container is created
    /// (ratect#301). Callers run the returned ID, as a built image's is run.
    async fn pull_image(&self, image: &str) -> Result<String>;

    /// The ID of `image` in the local Docker image cache, or `None` if it
    /// isn't there — used by `image_pull_policy: IfNotPresent` (the default)
    /// to decide whether a pull is needed at all, and then as the image to
    /// run, matching Batect's own semantics. Mirrors `network_exists`'s own
    /// 404-as-absent convention.
    async fn local_image_id(&self, image: &str) -> Result<Option<String>>;

    /// Builds an image per `spec` — see [`crate::container_spec::BuildSpec`]'s
    /// own field docs for what each one means and where it comes from.
    ///
    /// Returns the built image's ID (e.g. `sha256:...`), not `spec.tag` —
    /// the tag is applied so the image is identifiable in `docker images`,
    /// but isn't guaranteed unique (see `TaskEngine::resolve_image`), so
    /// callers must use the returned ID, not the tag, to reliably reference
    /// the image this call just built.
    ///
    /// `build_id` identifies this one attempt, so that [`cancel_build`]
    /// can stop it in the daemon if the caller stops waiting for it. It
    /// isn't part of `spec`: it says nothing about what is built. Only a
    /// BuildKit build uses it; the classic builder has no cancel by ID.
    ///
    /// [`cancel_build`]: ContainerRuntime::cancel_build
    async fn build_image(
        &self,
        spec: &crate::container_spec::BuildSpec,
        build_id: &str,
    ) -> Result<String>;

    /// Asks the daemon to stop the BuildKit build started with `build_id`
    /// — `POST /build/cancel`, as Batect sends for a build it abandons.
    /// Dropping `build_image`'s future only closes the connection, which
    /// leaves a BuildKit build running. Returns once the daemon accepts the
    /// request, not once the build has stopped; an unknown or finished ID
    /// is accepted too. When the classic builder was chosen there is nothing
    /// to cancel by ID, and nothing is asked.
    async fn cancel_build(&self, build_id: &str) -> Result<()>;

    /// Tags `image_id` (the ID `build_image` returned) with `tag`, in
    /// addition to `build_image`'s own `tag` — used by `--tag-image`. Both
    /// bollard's classic and BuildKit build options only ever accept one `t`
    /// value each, so extra tags are applied as separate `docker
    /// tag`-equivalent calls after the build completes, rather than folded
    /// into the build request itself. One tag per call, so the engine can
    /// attempt every tag and report each one the daemon refuses.
    async fn tag_image(&self, image_id: &str, tag: &str) -> Result<()>;

    /// Creates a task-scoped network. `labels` are Ratect's own ownership
    /// labels (see [`crate::labels`]) — a network has no user-configurable
    /// labels of its own, so this is the whole set.
    async fn create_network(&self, name: &str, labels: &HashMap<String, String>) -> Result<()>;

    /// `true` if a network named (or IDed) `name` already exists — used to
    /// validate `--use-network` up front, with a clear error, rather than
    /// letting an unrelated Docker API failure surface later when trying to
    /// join it.
    async fn network_exists(&self, name: &str) -> Result<bool>;

    /// Creates a container per `spec`, ready to be started, and returns its
    /// id — for a dependency and for the task's own container alike (see
    /// [`ContainerSpec`]'s own field docs for what each part means, and
    /// `spec.role` for which this is). Creation is its own step because it
    /// doesn't wait for what starting waits for: the engine creates a
    /// container as soon as its image and the task network are ready, and
    /// starts it only once every container it depends on is
    /// (decisions/0019, Batect's `CreateContainerStepRule`).
    ///
    /// "Created" means everything that makes the container startable:
    /// Docker's own `create`, under `spec.shared.docker_name` and on
    /// `spec.shared.network` — the task's network with a network alias of
    /// `spec.shared.name` so other containers on it can reach it by that
    /// name, or a Docker network mode ([`ContainerNetwork`]) — then the
    /// user-mapping upload (`spec.shared.user_mapping`).
    /// [`start_background_container`](Self::start_background_container) and
    /// [`run_container`](Self::run_container) then only start it.
    ///
    /// From the moment Docker's `create` returns, the container is the
    /// caller's to remove: a success returns its id, and a failure after
    /// that point carries it in [`CreateContainerError::created`], so the
    /// caller can register it for cleanup either way and nothing this call
    /// creates is ever stranded (ratect#224). Dropping this call's future
    /// mid-request doesn't stop the daemon creating the container, only the
    /// caller hearing about it, so a caller must drive it to its answer —
    /// the engine holds every one it starts until it has (ratect#318). The
    /// container's Docker `hostname` is set to `spec.shared.name` (matching
    /// Batect), independent of `spec.shared.network_options` — except under
    /// a `container:` network mode, where Docker refuses one
    /// (`build_hostname`).
    async fn create_container(
        &self,
        spec: &ContainerSpec,
    ) -> std::result::Result<String, CreateContainerError>;

    /// Starts `container_id` — created by
    /// [`create_container`](Self::create_container) from `spec` — in the
    /// background: does not wait for it to exit. Used for sidecar/dependency
    /// containers; `spec.role` is always
    /// [`crate::labels::ContainerRole::Dependency`] here, `spec.interactive`
    /// always `false`, and `spec.additional_args` always empty (only the
    /// top-level requested task's own container can receive
    /// `-- ADDITIONAL_ARGS`, via [`run_container`](Self::run_container)
    /// instead). Under the interleaved I/O policy (the `all` output mode)
    /// this is also where the container's log follower is spawned — see
    /// `LogFollowers`.
    async fn start_background_container(
        &self,
        container_id: &str,
        spec: &ContainerSpec,
    ) -> Result<()>;

    /// Blocks until `container_id` — already started — reports healthy.
    /// Ported from Batect's `WaitForContainerToBecomeHealthyStepRunner`:
    ///
    /// - A container with no health check at all (neither from its image nor
    ///   from `health_check` config) is immediately considered healthy —
    ///   "started = ready", exactly Ratect's pre-0.9.0 behavior for every
    ///   container.
    /// - Otherwise, waits on Docker's own event stream (`health_status`/
    ///   `die`, replayed from the beginning of time so a verdict that
    ///   arrived before this call still counts): reported-healthy returns
    ///   `Ok`; reported-unhealthy fails, including the last health-check
    ///   run's exit code and output when the container and daemon are still
    ///   there to report them; exiting before a verdict fails too.
    ///
    /// No Ratect-side timeout, matching Batect — Docker's own
    /// `retries`/`interval` bound how long a verdict can take.
    async fn wait_for_container_healthy(&self, container_id: &str) -> Result<()>;

    /// Blocks until `container_id` exits, returning its exit code — a
    /// genuine, possibly-long wait, unlike [`exit_code`](DockerClient::exit_code)'s
    /// own callers, which only ever call it once a container is already
    /// known to have stopped. Two call sites in engine.rs, with different
    /// stakes:
    ///
    /// - The background watcher for a dependency that has already become
    ///   ready (see `TaskEvent::DependencyExitedUnexpectedly`), watching for
    ///   it exiting on its own while something else is still going — engine.rs
    ///   aborts this call the moment the task's own run finishes, strictly
    ///   before cleanup stops anything, so it never needs to distinguish a
    ///   genuine crash from a deliberate stop itself.
    /// - A `run_to_completion` dependency's own readiness gate (ratect#97):
    ///   called synchronously, in the foreground, *before* the dependency is
    ///   considered ready at all — not a background watcher, and not aborted
    ///   by anything; its exit code is what readiness is decided on.
    async fn wait_for_container_exit(&self, container_id: &str) -> Result<i64>;

    /// Whether `container_id` is still running, answered at once. The engine
    /// asks when the task container's readiness gate fails, to tell a main
    /// command still running from one that has already exited and whose run
    /// is only still reporting it (awaited, so its exit code survives).
    /// Docker's own answer, so it can briefly still be `true` for a process
    /// that has already exited, until the daemon has recorded the exit —
    /// which is why a `true` (or an `Err`) makes the engine wait a little
    /// for the run's own result before cancelling it, rather than cancel at
    /// once (`engine::TASK_CONTAINER_EXIT_GRACE`, decisions/0012).
    async fn container_is_running(&self, container_id: &str) -> Result<bool>;

    /// Everything `container_id` has written to stdout and stderr, for a
    /// container that has already exited.
    ///
    /// The counterpart of [`unhealthy_details`](DockerClient::unhealthy_details)
    /// for a check Ratect runs itself rather than Docker: an
    /// `external_health_check`'s companion diagnoses its own failure on
    /// stderr (which attempt, what it last saw), and without this that
    /// diagnosis dies with the container. Not a streaming call — the
    /// container is finished by the time anything asks.
    async fn container_output(&self, container_id: &str) -> Result<String>;

    /// Runs `command` inside the already-running `container_id` — used for
    /// `setup_commands`. Tokenized into literal argv via
    /// `tokenize_command_line`, the same as `command`/`entrypoint` — no
    /// shell involved, matching Batect's own `SetupCommand.command` (typed
    /// `Command`, the same type as `Container.command`/`entrypoint`, and
    /// passed to Docker's exec API as already-parsed argv — confirmed by
    /// reading `RunContainerSetupCommandsStepRunner.runSetupCommand`, not
    /// assumed from Batect's docs). Docker's `exec`
    /// mechanism. Runs with the container's own environment and (when
    /// `user_mapping` is set) the same `uid:gid` the container itself runs
    /// as, matching Batect. Failure to *run* the command is an `Err`; the
    /// command running and exiting non-zero is an `Ok` whose
    /// [`ExecResult::exit_code`] says so — the caller decides what a
    /// non-zero setup command means.
    async fn exec_in_container(
        &self,
        container_id: &str,
        command: &str,
        working_directory: Option<&str>,
        environment: Option<&HashMap<String, String>>,
        user_mapping: Option<&UserMapping>,
    ) -> Result<ExecResult>;

    /// Starts `container_id` — created by
    /// [`create_container`](Self::create_container) from `spec` — and runs
    /// it to completion, streaming its output to stdout: a task's own
    /// container. Unlike
    /// [`start_background_container`](Self::start_background_container),
    /// `spec.role` is always [`crate::labels::ContainerRole::Task`], and
    /// `spec.interactive`/`spec.additional_args` may be set. Three
    /// attach-and-start paths sit behind it — fully non-interactive,
    /// stdin-forwarding, and a real TTY — chosen from `spec.interactive`
    /// and whether this process's own stdin and stdout are terminals
    /// (`task_container_io`, the same decision creation baked into the
    /// container's config); all three attach *before* Docker's `start`, so
    /// nothing typed or printed early is lost, and read the container's
    /// output from that attach.
    ///
    /// **This never removes the container.** Everything Ratect
    /// creates is removed by `engine.rs`'s own cleanup stage, under one
    /// reading of `--no-cleanup-after-success`/`--no-cleanup-after-failure`
    /// — the task's own container included, exactly like its sidecars and
    /// network. It used to remove its own, which meant the same two flags
    /// were interpreted in two places against two different notions of what
    /// "failed" means; keeping those in step by hand produced a bug in each
    /// of three consecutive review rounds, so the split is gone rather than
    /// corrected again. Batect draws the line in the same place: its
    /// `CleanupStagePlanner` generates a `RemoveContainerStep` for every
    /// entry in `containersCreated`, with no special case for the task's.
    ///
    /// The caller has owned the container since `create_container` handed
    /// its id back, which is what makes the paragraph above safe: a failure
    /// here can propagate immediately instead of having to be carried past
    /// a removal.
    ///
    /// `started`, given, is signaled right after Docker's own `start` call
    /// succeeds — never sent at all if the container never gets that far.
    /// Lets `engine.rs` run the task container's readiness gate (health-check
    /// wait, then `setup_commands`, via `wait_for_container_healthy`/
    /// `exec_in_container`; Batect-compatible dialect only — a native
    /// project's task container has none, see decisions/0012) concurrently
    /// with this call's own attach-and-wait-for-exit. Carries no id: the
    /// caller has had one since creation.
    async fn run_container(
        &self,
        container_id: &str,
        spec: &ContainerSpec,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()>;

    /// Kills and removes `container_id` in one daemon call, waiting on no
    /// grace period — Docker's own `rm -f`, the same request
    /// [`stop_and_remove_container`](ResourceInventory::stop_and_remove_container)
    /// ends with once its graceful stop has returned. This is the engine's
    /// *escalated* cleanup (ratect#249): a further termination signal during
    /// cleanup means "faster", not "leave it", and a supervisor escalating
    /// `SIGINT` → `SIGTERM` → `SIGKILL` on a fixed timer leaves well under a
    /// second for the whole of cleanup, which a container ignoring its stop
    /// signal never fits in gracefully. Safe on a container whose graceful
    /// stop was cancelled mid-flight — force removal doesn't care what state
    /// the container is in, or whether the daemon finished that stop anyway.
    async fn remove_container_forcibly(&self, container_id: &str) -> Result<()>;
}

/// Container ID -> the background log-follower task
/// `start_background_container` spawned for it under the interleaved (`all`
/// output mode) policy. `stop_and_remove_container` awaits (and removes) the
/// matching entry before returning, via [`LogFollowers::await_and_remove`],
/// so a dependency's follower can never race past the container's own
/// removal — `TaskEvent::ContainerRemoved` (which `engine.rs` posts right
/// after `stop_and_remove_container` returns) is then only ever posted once
/// the follower has finished flushing everything it's going to, instead of
/// the two racing as genuinely fire-and-forget tasks. Holds only the map and
/// the remove-and-await behavior — no connection of its own — so that
/// behavior stays unit-testable without a live Docker daemon regardless of
/// how `DockerClient` itself connects.
#[derive(Default)]
struct LogFollowers {
    handles: std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
}

impl LogFollowers {
    fn insert(&self, container_id: String, handle: tokio::task::JoinHandle<()>) {
        self.handles.lock().unwrap().insert(container_id, handle);
    }

    /// Awaits (and removes) `container_id`'s background log-follower handle,
    /// if one was inserted for it. A no-op for any container with no
    /// follower — every non-interleaved run, and the task's own container
    /// (which streams via `start_and_stream_output` directly, never through a
    /// spawned follower).
    ///
    /// Cancelled mid-wait (a graceful removal dropped on the engine's
    /// escalation rung), the handle goes back into the map rather than
    /// being dropped: the follower is still running, and the forced removal
    /// that follows has to be able to wait on it too, or its
    /// `ContainerRemoved` could be posted with output still flushing.
    async fn await_and_remove(&self, container_id: &str) {
        let follower = self.handles.lock().unwrap().remove(container_id);
        if let Some(follower) = follower {
            struct Reinsert<'a> {
                followers: &'a LogFollowers,
                container_id: &'a str,
                handle: Option<tokio::task::JoinHandle<()>>,
            }
            impl Drop for Reinsert<'_> {
                fn drop(&mut self) {
                    if let Some(handle) = self.handle.take() {
                        self.followers.insert(self.container_id.to_string(), handle);
                    }
                }
            }
            let mut guard = Reinsert {
                followers: self,
                container_id,
                handle: Some(follower),
            };
            // A `JoinError` here only means the follower task itself
            // panicked — already-posted events aren't affected, and
            // there's nothing this caller could do about it beyond not
            // hanging, so it's discarded rather than turned into a
            // container-removal failure.
            let _ = guard.handle.as_mut().expect("set above").await;
            guard.handle = None;
        }
    }
}

pub struct DockerClient {
    docker: Docker,
    /// The builder every `build_image` call uses, resolved once per client
    /// (per `ratect` invocation, in practice) on first build — see
    /// `select_builder_version` for the decision itself.
    builder_version: tokio::sync::OnceCell<bollard::query_parameters::BuilderVersion>,
    /// Where streamed pull/build progress detail goes for the user to see —
    /// `docker.rs` only ever posts the fine-grained progress variants (the
    /// milestones around them are `engine.rs`'s job, which knows the
    /// container/task names). [`NullEventSink`] (silent) by default, a real
    /// output-mode logger via `with_event_sink`. See `crate::ui`.
    event_sink: std::sync::Arc<dyn EventSink>,
    log_followers: LogFollowers,
    /// `true` when `--enable-buildkit` was given: forces BuildKit on for
    /// every build this client makes, taking precedence over the
    /// `DOCKER_BUILDKIT` environment variable — matching Batect's own
    /// `TristateFlagOption`, whose default value provider *is* that same
    /// environment variable, so an explicit flag on the command line always
    /// wins over it. `false` (the default) defers to
    /// `DOCKER_BUILDKIT`/the daemon's own advertised default, unchanged. See
    /// `select_builder_version` — there's no `--disable-buildkit`
    /// counterpart, matching Batect exactly (forcing it *off* is only ever
    /// done via `DOCKER_BUILDKIT=0`).
    enable_buildkit: bool,
    /// Resolves a registry's configured credential for `pull_image` — see
    /// `resolve_pull_credential`, which owns the actual decision (what to
    /// pass, whether to warn) so it stays unit-testable via a fake resolver
    /// without a live daemon. The real
    /// `registry_auth::DockerCredentialHelperResolver` by default.
    credential_resolver: std::sync::Arc<dyn crate::registry_auth::RegistryCredentialResolver>,
    /// `<docker config directory>/config.json` — read fresh on every
    /// `build_image` call (not cached), for two things: `build_registries`,
    /// to enumerate every registry it declares under `auths`/`credHelpers`
    /// and every one its `credsStore` holds, for build's own credential
    /// resolution; and `build_args_with_config_file_proxies`, for its
    /// `proxies`.
    /// Kept as a plain path rather than through `credential_resolver`
    /// itself, since a fake resolver used in tests has no file of its own —
    /// enumeration and per-registry resolution are deliberately separate
    /// concerns.
    docker_config_path: std::path::PathBuf,
    /// The daemon host this client connected to, as the Docker CLI spells
    /// it — what `config.json`'s `proxies` map is keyed by.
    daemon_host: String,
}

/// Refuses a build whose `buildkit` options (`build_secrets`/`build_ssh`)
/// the selected `builder_version` can't serve: the classic builder has no
/// session for the daemon to request secret bytes or ssh-agent proxying
/// over, so these fields are impossible without BuildKit, and the build
/// fails clearly rather than running without them. Called before any
/// credential is resolved or the build context is made, and pure — a daemon
/// advertising no builder, which selects the classic one, is otherwise
/// untestable.
fn refuse_buildkit_options_on_the_classic_builder(
    builder_version: &bollard::query_parameters::BuilderVersion,
    buildkit: Option<&BuildKitOptions>,
    tag: &str,
) -> Result<()> {
    if *builder_version == bollard::query_parameters::BuilderVersion::BuilderV1
        && buildkit.is_some()
    {
        anyhow::bail!(
            "Building '{}' requires BuildKit ('build_secrets'/'build_ssh' cannot \
             be served by the classic builder), but the classic builder is \
             selected — the Docker daemon doesn't advertise BuildKit as its \
             default builder, or DOCKER_BUILDKIT=0 forces it off.",
            tag
        );
    }
    Ok(())
}

/// Splits a full image reference (as given to `--tag-image`, e.g.
/// `myrepo/myimage:v2` or `myrepo/myimage`) into the repository and tag
/// components Docker's tag API takes separately. A colon before the last
/// `/` is a registry host's port (e.g. `localhost:5000/myimage` has no tag
/// component), not a tag separator — mirrors Docker's own image-reference
/// parsing rule. `None` for the tag means "no tag given"; callers apply
/// Docker's own implicit `latest` default.
fn split_image_reference(reference: &str) -> (&str, Option<&str>) {
    let repo_start = reference.rfind('/').map_or(0, |i| i + 1);
    match reference[repo_start..].rfind(':') {
        Some(colon) => (
            &reference[..repo_start + colon],
            Some(&reference[repo_start + colon + 1..]),
        ),
        None => (reference, None),
    }
}

/// The `from_image`/`tag` pair `pull_image` asks the daemon for, given a
/// configured image reference. A reference with neither tag nor digest gets
/// `latest`, as the docker CLI and Batect's docker-client both default it:
/// the image-create API itself pulls *every* tag of the repository when
/// given neither (ratect#536). A digest reference (`repo@sha256:…`, with or
/// without a tag) goes unchanged — checked before [`split_image_reference`],
/// which knows nothing of digests and would read the digest's own `sha256:`
/// colon as a tag separator.
fn pull_request_reference(reference: &str) -> (&str, Option<&str>) {
    if reference.contains('@') {
        return (reference, None);
    }
    let (repository, tag) = split_image_reference(reference);
    (repository, Some(tag.unwrap_or("latest")))
}

/// `reference`'s repository at `digest` — what `pull_image` looks the
/// pulled image up by, as Batect's docker-client does with the pull
/// stream's `Digest:` line (`images_pull.go`), so the ID it returns is the
/// image this pull fetched even if the tag has moved since (ratect#301).
/// Any tag or digest already in `reference` is dropped: the digest names
/// the image on its own.
fn digest_pinned_reference(reference: &str, digest: &str) -> String {
    let name = reference
        .split_once('@')
        .map_or(reference, |(name, _)| name);
    let (repository, _) = split_image_reference(name);
    format!("{repository}@{digest}")
}

/// The `DOCKER_BUILDKIT` value `select_builder_version` actually sees:
/// forced to `"1"` when `--enable-buildkit` was given, taking precedence
/// over `real_env_value` regardless of what it says — matching Batect's own
/// `TristateFlagOption`, whose default-value provider *is* this same
/// environment variable, so an explicit flag on the command line always
/// wins over it. `real_env_value`, verbatim, otherwise. There's no
/// `--disable-buildkit` counterpart (matching Batect exactly) — forcing the
/// classic builder is only ever done via `DOCKER_BUILDKIT=0`/`no`/`false`.
///
/// Pure, so `--enable-buildkit`'s precedence is unit-testable without a live
/// daemon; [`DockerClient::builder_version`] feeds it the real environment
/// variable.
fn docker_buildkit_env_value(
    enable_buildkit_flag: bool,
    real_env_value: Option<&str>,
) -> Option<String> {
    if enable_buildkit_flag {
        Some("1".to_string())
    } else {
        real_env_value.map(str::to_string)
    }
}

/// Picks the builder for this invocation's image builds, as Batect's own
/// selection does (`DockerConnectivity.kt`): an explicit `DOCKER_BUILDKIT`
/// environment variable wins — read by [`boolean_variable::parse`], so `1`,
/// `yes` or `true` forces BuildKit and `0`, `no` or `false` the classic
/// builder, in any case, and an empty one is unset (Batect's vocabulary,
/// with two divergences that module lists; the docker CLI accepts no
/// `yes`/`no`) — the same env var Batect reads as its `--enable-buildkit`
/// default; otherwise the builder the daemon itself advertises as its
/// default (the `/_ping` response's `Builder-Version` header — `"2"` is
/// BuildKit) is used, which is BuildKit on any modern daemon. A missing or
/// empty header (a daemon old enough to predate it) or `"1"` means the
/// classic builder. A `DOCKER_BUILDKIT` value that parses as neither, or a
/// header value that is none of those — when no `DOCKER_BUILDKIT` overrides
/// it — is a hard error naming the value rather than a silent guess, as
/// docker-client's `BuilderVersion.fromAPI` makes it. Both are raised when a
/// build first needs the builder, not on every command as in Batect (and,
/// for `DOCKER_BUILDKIT`, the docker CLI; see
/// `docs/differences-from-batect.md`'s `--enable-buildkit` row). Batect
/// also refuses BuildKit on a daemon older than Docker 18.09 without
/// experimental features, or older than 17.07 at all; neither can be met
/// above [`MINIMUM_API_VERSION`], so neither is here.
///
/// Pure (both inputs injected) so the whole decision table is
/// unit-testable; [`DockerClient`] feeds it the real environment variable
/// and ping header.
fn select_builder_version(
    docker_buildkit_env: Option<&str>,
    daemon_advertised: Option<&str>,
) -> Result<bollard::query_parameters::BuilderVersion> {
    use bollard::query_parameters::BuilderVersion;
    match boolean_variable::parse("DOCKER_BUILDKIT", docker_buildkit_env)? {
        Some(true) => return Ok(BuilderVersion::BuilderBuildKit),
        Some(false) => return Ok(BuilderVersion::BuilderV1),
        None => {}
    }
    match daemon_advertised {
        None | Some("") | Some("1") => Ok(BuilderVersion::BuilderV1),
        Some("2") => Ok(BuilderVersion::BuilderBuildKit),
        Some(other) => Err(anyhow::anyhow!(
            "The Docker daemon's Builder-Version ping header is '{other}', which is not a builder \
             Ratect knows — set the DOCKER_BUILDKIT environment variable to '1' to use BuildKit \
             or '0' to use the classic builder."
        )),
    }
}

/// The oldest Docker Engine API version Ratect's own features actually
/// need — API 1.41 (Docker 20.10, December 2020), the release that added the
/// `host-gateway` special address Ratect's proxy support depends on. Not
/// `bollard`'s own default peg (`DockerClient::new` negotiates down from
/// that): a floor Ratect chose, kept here as the one place it's spelled out.
const MINIMUM_API_VERSION: ClientVersion = ClientVersion {
    major_version: 1,
    minor_version: 41,
};

/// Checks a negotiated API version against [`MINIMUM_API_VERSION`], naming
/// both the API version the daemon supports and the floor Ratect requires on
/// failure — a clear, upfront reason rather than some later, arbitrary
/// request failing with a message that doesn't explain why. Pure (the version is injected)
/// so every scenario — well above the floor, exactly at it, below it — is
/// unit-testable without a real daemon of that exact version;
/// `DockerClient::new` feeds it the real negotiated version.
fn check_api_version_floor(negotiated: ClientVersion) -> Result<()> {
    if negotiated < MINIMUM_API_VERSION {
        return Err(anyhow::anyhow!(
            "Ratect requires Docker 20.10 or newer, but the daemon only supports API version \
             {negotiated} (Ratect requires at least {MINIMUM_API_VERSION}) — upgrade Docker to \
             use Ratect."
        ));
    }
    Ok(())
}

/// Refuses a daemon running Windows containers, given the operating system
/// its `/version` reports (`windows`, in any case): Ratect only supports
/// Linux containers. A deliberate divergence — Batect accepts both, and
/// refuses only a daemon reporting neither, which no real Docker daemon
/// does, so that check isn't copied: an empty or missing operating system
/// passes. Pure (the value is injected) so it's unit-testable without such
/// a daemon; `DockerClient::new` feeds it the real one.
fn check_daemon_os(os: Option<&str>) -> Result<()> {
    match os {
        Some(os) if os.eq_ignore_ascii_case("windows") => Err(anyhow::anyhow!(
            "The Docker daemon is running Windows containers (it reports its operating system \
             as '{os}'), but Ratect needs one running Linux containers — switch Docker to Linux \
             containers to use Ratect."
        )),
        _ => Ok(()),
    }
}

/// Resolves the credential to pull `image` with, via `resolver` — the
/// image's own registry (via [`crate::registry_auth::registry_hostname`]) is
/// the only one pull ever needs. Pulled out of `pull_image` itself so this
/// decision — what credential to pass, whether to warn — is unit-testable
/// via a fake resolver, without `DockerClient` or a live daemon.
///
/// A resolution failure is never fatal here — see
/// [`crate::registry_auth::RegistryCredentialResolver`]'s own doc comment —
/// so the second return value is the one warning to log naming the failed
/// registry, for `pull_image` to emit; it's `None` whenever resolution
/// itself succeeded, whether or not a credential was actually configured.
async fn resolve_pull_credential(
    resolver: &dyn crate::registry_auth::RegistryCredentialResolver,
    image: &str,
) -> (Option<bollard::auth::DockerCredentials>, Option<String>) {
    let registry = crate::registry_auth::registry_hostname(image);
    match resolver.resolve(&registry).await {
        Ok(credential) => (credential, None),
        Err(error) => (None, Some(credential_resolution_warning(&registry, &error))),
    }
}

/// The one warning message both `resolve_pull_credential` and
/// `resolve_build_credentials` log for a registry that failed to resolve —
/// factored out so the wording only needs to stay consistent in one place.
fn credential_resolution_warning(registry: &str, error: &anyhow::Error) -> String {
    format!(
        "Could not resolve credentials for registry '{registry}' ({error:#}); proceeding \
         without them."
    )
}

/// Every registry a build sends credentials for, from `config_json` (raw
/// `config.json` contents): those it names under `auths`/`credHelpers`
/// ([`crate::registry_auth::configured_registries`]), and, when it names a
/// default `credsStore`, every one that store holds, as `resolver` lists
/// them — the Docker CLI's `GetAllCredentials`, which Batect's builds send.
/// Each once, sorted. A store that can't be listed costs only what it would
/// have added, and is the one warning returned. Pulled out of `build_image`
/// so the union and the warning are testable with a fake resolver.
async fn build_registries(
    resolver: &dyn crate::registry_auth::RegistryCredentialResolver,
    config_json: &str,
) -> (Vec<String>, Option<String>) {
    let mut registries = crate::registry_auth::configured_registries(config_json);
    let Some(store) = crate::registry_auth::configured_creds_store(config_json) else {
        return (registries, None);
    };
    match resolver.list(&store).await {
        Ok(listed) => {
            registries.extend(listed);
            registries.sort();
            registries.dedup();
            (registries, None)
        }
        Err(error) => (
            registries,
            Some(format!(
                "Could not list the registries credential store '{store}' holds ({error:#}); \
                 building with only those config.json names."
            )),
        ),
    }
}

/// `spec`'s build args with the gaps filled from the `proxies` of the
/// Docker CLI `config.json` whose contents are `config_json` (see
/// `proxy::fill_config_file_proxies`) — unless `spec.config_file_proxies`
/// is off (`--no-proxy-vars`), or there is no file, when they are `spec`'s
/// own unchanged. `None` only when there is nothing to send at all.
fn build_args_with_config_file_proxies(
    spec: &crate::container_spec::BuildSpec,
    config_json: Option<&str>,
    daemon_host: &str,
) -> Option<HashMap<String, String>> {
    let Some(config_json) = config_json.filter(|_| spec.config_file_proxies) else {
        return spec.build_args.clone();
    };
    let mut build_args = spec.build_args.clone().unwrap_or_default();
    crate::proxy::fill_config_file_proxies(config_json, daemon_host, &mut build_args);
    (spec.build_args.is_some() || !build_args.is_empty()).then_some(build_args)
}

/// Resolves credentials for every registry in `registries`, via `resolver` —
/// build covers every registry Ratect's own Docker config knows about,
/// since neither Batect nor Ratect parses Dockerfile `FROM` lines to scope
/// this more precisely. Pulled out of `build_image` itself so this decision
/// is unit-testable via a fake resolver, without `DockerClient` or a live
/// daemon — mirrors [`resolve_pull_credential`]'s own separation.
///
/// A registry that fails to resolve is omitted from the returned map —
/// never aborting the rest — and contributes one warning naming it, for
/// `build_image` to emit; see
/// [`crate::registry_auth::RegistryCredentialResolver`]'s own doc comment
/// for why a resolution failure is never fatal.
async fn resolve_build_credentials(
    resolver: &dyn crate::registry_auth::RegistryCredentialResolver,
    registries: &[String],
) -> (
    HashMap<String, bollard::auth::DockerCredentials>,
    Vec<String>,
) {
    let mut credentials = HashMap::new();
    let mut warnings = Vec::new();
    for registry in registries {
        match resolver.resolve(registry).await {
            Ok(Some(credential)) => {
                credentials.insert(registry.clone(), credential);
            }
            Ok(None) => {}
            Err(error) => warnings.push(credential_resolution_warning(registry, &error)),
        }
    }
    (credentials, warnings)
}

pub(crate) mod boolean_variable;
mod connection;
mod stdin;
pub use connection::DockerConnectionOptions;

impl DockerClient {
    /// The daemon's own version string, and — the actual point — proof
    /// that it can be reached at all. `DockerClient::new` already asked
    /// `/version` twice (to negotiate the API version and to check the
    /// daemon's operating system) before this method exists to be called,
    /// so this is a further, deliberate round trip — this is how `ratect doctor` distinguishes "unreachable"
    /// from "reachable, and here's what it is" without running a task.
    pub async fn server_version(&self) -> Result<String> {
        let version = self
            .docker
            .version()
            .await
            .context("Failed to reach the Docker daemon")?;
        Ok(version
            .version
            .unwrap_or_else(|| "unknown version".to_string()))
    }

    /// Connects as `connection` says. `warn` is given each warning choosing
    /// the daemon raised — a Docker CLI `config.json` that can't be used,
    /// say — before the daemon is first contacted, so it is said even when
    /// that fails. Required rather than collected afterwards because only
    /// the caller knows where a warning goes: a run's event sink, a verb's
    /// stderr, a `doctor` finding.
    ///
    /// Then checks the daemon, before anything else asks it for anything:
    /// that it answers at all — an error naming the daemon tried when it
    /// doesn't (see `Connection::explain_failure`) — that its API is at
    /// least `MINIMUM_API_VERSION`, and that it isn't running Windows
    /// containers (see `check_daemon_os`). Each is a request: one
    /// `/version` to negotiate the API version, and another, at that
    /// version, for the daemon's operating system.
    pub async fn new(connection: &DockerConnectionOptions, warn: impl Fn(String)) -> Result<Self> {
        let connected = connection::connect(connection)?;
        connected.warnings.iter().cloned().for_each(warn);
        let docker = connected
            .docker
            .clone()
            .negotiate_version()
            .await
            .map_err(|err| {
                connected.explain_failure(
                    err.into(),
                    "Failed to negotiate the Docker API version with the daemon",
                )
            })?;
        check_api_version_floor(docker.client_version())?;
        let version = docker.version().await.map_err(|err| {
            connected.explain_failure(
                err.into(),
                "Failed to read the Docker daemon's operating system",
            )
        })?;
        check_daemon_os(version.os.as_deref())?;
        let config_directory = connected.config_directory.clone();
        Ok(Self {
            docker,
            builder_version: tokio::sync::OnceCell::new(),
            event_sink: std::sync::Arc::new(NullEventSink),
            log_followers: LogFollowers::default(),
            enable_buildkit: false,
            credential_resolver: std::sync::Arc::new(
                crate::registry_auth::DockerCredentialHelperResolver::new(config_directory.clone()),
            ),
            docker_config_path: config_directory.join("config.json"),
            daemon_host: connected.daemon_host.clone(),
        })
    }

    /// Opts into `--enable-buildkit`: see `enable_buildkit`'s own doc
    /// comment.
    pub fn with_enable_buildkit(mut self, enable_buildkit: bool) -> Self {
        self.enable_buildkit = enable_buildkit;
        self
    }

    /// Injects the output-mode logger streamed pull/build progress renders
    /// through — the same sink `main.rs` gives the `TaskEngine`, so one
    /// logger sees the whole event stream in order.
    pub fn with_event_sink(mut self, event_sink: std::sync::Arc<dyn EventSink>) -> Self {
        self.event_sink = event_sink;
        self
    }

    /// The builder this invocation's image builds use — resolved on first
    /// call (one `/_ping` round trip) and cached for the client's lifetime;
    /// see `select_builder_version`.
    async fn builder_version(&self) -> Result<bollard::query_parameters::BuilderVersion> {
        self.builder_version_given(std::env::var("DOCKER_BUILDKIT").ok().as_deref())
            .await
    }

    /// [`Self::builder_version`] with the real `DOCKER_BUILDKIT` value
    /// injected, so a test can show the daemon's ping header deciding
    /// without depending on the environment it runs in.
    async fn builder_version_given(
        &self,
        real_env_value: Option<&str>,
    ) -> Result<bollard::query_parameters::BuilderVersion> {
        self.builder_version
            .get_or_try_init(|| async {
                let env_value = docker_buildkit_env_value(self.enable_buildkit, real_env_value);
                let ping_info = self
                    .docker
                    .ping_info()
                    .await
                    .context("Failed to query the Docker daemon's default builder")?;
                let selected = select_builder_version(
                    env_value.as_deref(),
                    ping_info.builder_version.as_deref(),
                )?;
                tracing::debug!(
                    env = ?env_value,
                    daemon_advertised = ?ping_info.builder_version,
                    selected = ?selected,
                    "selected image builder"
                );
                Ok(selected)
            })
            .await
            .copied()
    }

    /// Uploads the synthetic `/etc/passwd`/`/etc/shadow`/`/etc/group`
    /// `mapping` needs into `container_id`, then takes ownership of its home
    /// directory and each of its cache mounts for the mapped user.
    ///
    /// Must be called after the container is created but before it's started
    /// (Docker's own "upload archive to container" API works on an
    /// already-created container's filesystem; the container needs those
    /// files in place before its own process starts reading them).
    ///
    /// The cache mounts are the non-obvious half: a fresh Docker volume is
    /// created root-owned, so without this the container mounts its cache
    /// successfully and then fails on the first write. `engine.rs` filters
    /// out read-only ones, which cannot be chowned and don't need to be.
    async fn apply_user_mapping(&self, container_id: &str, mapping: &UserMapping) -> Result<()> {
        let passwd_tar = build_user_mapping_tar(mapping)?;
        let passwd_options = UploadToContainerOptionsBuilder::default()
            .path("/etc")
            .build();
        self.docker
            .upload_to_container(
                container_id,
                Some(passwd_options),
                bollard::body_full(passwd_tar.into()),
            )
            .await
            .with_context(|| {
                format!("Failed to upload user mapping files to container '{container_id}'")
            })?;

        // The home directory, then every writable cache mount — each an
        // existing mount point whose ownership has to be changed to the
        // mapped user, and all done the same way for that reason.
        for directory in
            std::iter::once(&mapping.home_directory).chain(mapping.cache_directories.iter())
        {
            let tar = build_owned_directory_tar(directory, mapping.user.uid, mapping.user.gid)
                // Not attributed to a container here: a non-absolute
                // mount path is a *config* error, rejected when the
                // config loads or is evaluated, naming the container
                // the user wrote. Reaching this is a bug in Ratect, so the
                // Docker id is the useful identifier rather than a
                // misleading one.
                .context("Failed to build the directory-ownership archive")?;
            let options = UploadToContainerOptionsBuilder::default()
                .path(&owned_directory_parent(directory))
                .build();
            self.docker
                .upload_to_container(container_id, Some(options), bollard::body_full(tar.into()))
                .await
                .with_context(|| {
                    format!(
                        "Failed to take ownership of '{directory}' in container '{container_id}'"
                    )
                })?;
        }

        tracing::debug!(
            container_id,
            uid = mapping.user.uid,
            gid = mapping.user.gid,
            "applied user mapping"
        );
        Ok(())
    }

    /// Explains why a container was reported unhealthy, from its last
    /// recorded health-check run — ported from Batect's
    /// `containerBecameUnhealthyMessage`, including its "exited 0 just
    /// after the timeout" special case. Best-effort: falls back to the
    /// verdict alone if the inspect (or its health log) is unavailable,
    /// rather than masking the real failure with an inspection error.
    async fn unhealthy_details(&self, container_id: &str) -> String {
        const VERDICT: &str = "The configured health check did not indicate that the container \
                               was healthy within the timeout period.";

        let last_result = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|inspection| inspection.state)
            .and_then(|state| state.health)
            .and_then(|health| health.log)
            .and_then(|log| log.into_iter().next_back());

        let Some(last_result) = last_result else {
            return VERDICT.to_string();
        };

        let exit_code = last_result.exit_code.unwrap_or_default();
        let output = last_result.output.unwrap_or_default();
        let details = if exit_code == 0 {
            "The most recent health check exited with code 0, which usually indicates that the \
             container became healthy just after the timeout period expired."
                .to_string()
        } else if output.is_empty() {
            format!("The last health check exited with code {exit_code} but did not produce any output.")
        } else {
            format!(
                "The last health check exited with code {exit_code} and output:\n{}",
                output.trim()
            )
        };

        format!("{VERDICT} {details}")
    }

    /// The shared primitive behind two different contracts: this method's
    /// own two callers only ever reach it once a container has already
    /// stopped (e.g. after its attached output stream has ended), at which
    /// point Docker still has the exit status available and this resolves
    /// immediately; [`wait_for_container_exit`]
    /// reaches the exact same bollard call from the opposite position — a
    /// container that may still be running for a while yet — and genuinely
    /// waits. Nothing here depends on which is true; only what each caller
    /// can promise about the wait does.
    ///
    /// [`wait_for_container_exit`]: ContainerRuntime::wait_for_container_exit
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

    /// Whether the task's own container is interactive, and whether it gets
    /// a real Docker TTY — `(interactive, use_tty)`. Decided from `spec` and
    /// this process's own stdin/stdout, so the answer is the same at
    /// creation (where `container_config` bakes it into the container's
    /// stdin flags) and at `run_container` (where it picks the attach path):
    /// one function, called from both, so the two can't disagree. For a
    /// dependency `spec.interactive` is always `false`, so both are.
    ///
    /// `interactive` independently re-enforces the same policy `engine.rs`
    /// already gated `spec.interactive` on — see
    /// `ContainerIoStreaming::allows_interactive`'s own docs for why both
    /// sites call the one method rather than each hand-rolling a comparison
    /// against a specific variant.
    fn task_container_io(&self, spec: &ContainerSpec) -> (bool, bool) {
        let interactive = spec.interactive
            && self
                .event_sink
                .container_io_streaming()
                .allows_interactive();
        let use_tty = should_use_tty(
            interactive,
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        );
        (interactive, use_tty)
    }

    /// Logs, at debug level, the image's default `CMD` that `spec`'s
    /// additional arguments replace — see [`replaced_image_cmd`]. Not a
    /// warning: a CLI-tool image's default `CMD` (often `--help`) is usually
    /// replaced on purpose. Best effort: a failed inspect is logged too, and
    /// changes nothing about the run.
    async fn log_replaced_image_cmd(&self, spec: &ContainerSpec) {
        let shared = &spec.shared;
        match self.docker.inspect_image(&shared.image).await {
            Ok(image) => {
                let image_cmd = image.config.and_then(|config| config.cmd);
                if let Some(cmd) = replaced_image_cmd(
                    shared.command.as_deref(),
                    &spec.additional_args,
                    image_cmd.as_deref(),
                ) {
                    tracing::debug!(
                        container = shared.name.as_str(),
                        image = shared.image.as_str(),
                        replaced_cmd = ?cmd,
                        additional_args = ?spec.additional_args,
                        "additional arguments replace the image's default CMD"
                    );
                }
            }
            Err(e) => tracing::debug!(
                image = shared.image.as_str(),
                "could not inspect image for its default CMD: {e}"
            ),
        }
    }

    /// `spec` as the Docker create request `create_container` sends — one
    /// builder for a dependency and the task's own container, which differ
    /// only in what the task container's attach-and-start paths need baked
    /// in at creation: its stdout/stderr attached and its stdin flags
    /// (`stdin_flags`), neither of which a dependency sets. Fails without
    /// touching the daemon when the spec can't be turned into a request (an
    /// untokenizable `command`/`entrypoint`), or when something a container
    /// running as the host user needs on the host can't be created
    /// (`ensure_host_volume_directories_exist`).
    fn container_config(&self, spec: &ContainerSpec) -> Result<Config> {
        let shared = &spec.shared;
        let options = &shared.options;
        if shared.runs_as_host_user {
            ensure_host_volume_directories_exist(
                shared.volumes.as_deref().unwrap_or_default(),
                options.tmpfs.as_deref().unwrap_or_default(),
            )?;
        }
        let cmd = build_cmd(shared.command.as_deref(), &spec.additional_args)?;
        let entrypoint = options
            .entrypoint
            .as_deref()
            .map(tokenize_command_line)
            .transpose()?;
        let port_config = build_port_config(shared.network_options.ports.as_ref());
        let is_task_container = spec.role == crate::labels::ContainerRole::Task;
        let (interactive, use_tty) = self.task_container_io(spec);
        let stdin = stdin_flags(interactive, use_tty);

        let host_config = HostConfig {
            binds: shared
                .volumes
                .as_ref()
                .map(|binds| binds.iter().map(BindMount::to_bind_string).collect()),
            extra_hosts: build_extra_hosts(
                shared.network_options.additional_hosts.as_ref(),
                shared.network_options.proxy_host_gateway,
            ),
            port_bindings: port_config.as_ref().map(|(_, bindings)| bindings.clone()),
            cap_add: options.capabilities_to_add.clone(),
            cap_drop: options.capabilities_to_drop.clone(),
            privileged: options.privileged,
            shm_size: options.shm_size,
            devices: build_devices(options.devices.as_ref()),
            init: Some(options.enable_init_process),
            log_config: build_log_config(
                options.log_driver.as_deref(),
                options.log_options.as_ref(),
            ),
            tmpfs: build_tmpfs_mounts(options.tmpfs.as_ref()),
            ulimits: build_ulimits(options.ulimits.as_ref()),
            dns: options.dns.clone(),
            dns_search: options.dns_search.clone(),
            dns_options: options.dns_options.clone(),
            network_mode: Some(build_network_mode(&shared.network)),
            ..Default::default()
        };

        Ok(Config {
            hostname: build_hostname(&shared.name, &shared.network),
            image: Some(shared.image.clone()),
            cmd,
            entrypoint,
            env: build_env(shared.environment.as_ref()),
            exposed_ports: port_config.as_ref().map(|(exposed, _)| exposed.clone()),
            attach_stdout: is_task_container.then_some(true),
            attach_stderr: is_task_container.then_some(true),
            tty: stdin.tty,
            open_stdin: stdin.open_stdin,
            attach_stdin: stdin.attach_stdin,
            stdin_once: stdin.stdin_once,
            user: shared
                .user_mapping
                .as_ref()
                .map(|m| format!("{}:{}", m.user.uid, m.user.gid)),
            healthcheck: build_health_config(shared.health_check.as_ref()),
            working_dir: options.working_directory.clone(),
            labels: Some(spec.labels.clone()),
            host_config: Some(host_config),
            networking_config: build_networking_config(
                &shared.network,
                &shared.name,
                shared.network_options.additional_hostnames.as_ref(),
            ),
            ..Default::default()
        })
    }

    /// Attaches to `container_id`'s TTY, forwards the local terminal's
    /// stdin/stdout to it for the duration of the session, and returns its
    /// exit code once the session ends. Only called once `should_use_tty`
    /// has already confirmed both the local terminal and the container's own
    /// config (`tty`/`open_stdin`/etc., set by the caller before creating the
    /// container) are set up for it.
    ///
    /// Attaches *before* starting the container — same ordering Docker's own
    /// attach-then-start pattern uses, so no early output is missed — and
    /// puts the local terminal into raw mode for the duration, restored via
    /// `RawModeGuard`'s `Drop` even if this returns early on error.
    ///
    /// `started`, if given, is signaled with `container_id` right after
    /// Docker's own `start` call succeeds — see `run_container`'s own doc
    /// comment for why.
    async fn run_container_interactively(
        &self,
        container_name: &str,
        container_id: &str,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<i64> {
        let AttachContainerResults {
            output: mut attach_output,
            input: attach_input,
        } = self
            .attach_output(container_name, container_id, true)
            .await?;

        let raw_mode = RawModeGuard::enable()?;

        self.docker
            .start_container(container_id, None)
            .await
            .with_context(|| format!("Failed to start container '{container_name}'"))?;
        tracing::debug!(container_id, "started container interactively");
        if let Some(started) = started {
            let _ = started.send(());
        }

        // Syncs the container's TTY to the local terminal's size once, at
        // attach time — then `resize_listener` (Unix only) keeps it in sync
        // for the rest of the session, on every subsequent local resize.
        resize_tty(&self.docker, container_id).await;
        #[cfg(unix)]
        let resize_listener = AbortOnDrop(spawn_resize_listener(
            self.docker.clone(),
            container_id.to_string(),
        ));

        // The attach output stream ending (the container exiting) is what
        // ends the session, so this pump is stopped once that happens rather
        // than awaited to completion.
        let stdin_pump = spawn_stdin_pump(stdin::StdinReader::process(), attach_input);

        let mut stdout = tokio::io::stdout();
        let output_result: Result<()> = async {
            while let Some(chunk) = attach_output.next().await {
                let log_output = chunk.context("Failed to read container output")?;
                stdout
                    .write_all(log_output.as_ref())
                    .await
                    .map_err(output_write_error)?;
                stdout.flush().await.map_err(output_write_error)?;
            }
            Ok(())
        }
        .await;
        // The terminal leaves raw mode with the pump, so a key pressed while
        // the exit code is fetched — Ctrl+C above all — reaches the terminal
        // as usual rather than waiting in the reader for the next container.
        drop(stdin_pump);
        drop(raw_mode);
        #[cfg(unix)]
        drop(resize_listener);
        output_result?;

        self.exit_code(container_id).await
    }

    /// Attaches to `container_id`'s stdout and stderr, and its stdin when
    /// `stdin`, as one stream, framed by whether the container has a TTY.
    /// Called before the container starts,
    /// since an attach only sees output written after it is made. Attaching,
    /// as Batect does, rather than following Docker's `logs` API: an attach
    /// works whatever the container's log driver, where `logs` answers 501
    /// for one Docker can't read back, such as `log_driver: none`
    /// (ratect#517).
    async fn attach_output(
        &self,
        container_name: &str,
        container_id: &str,
        stdin: bool,
    ) -> Result<AttachContainerResults> {
        let attach_options = AttachContainerOptionsBuilder::default()
            .stdin(stdin)
            .stdout(true)
            .stderr(true)
            .stream(true)
            .build();
        self.docker
            .attach_container(container_id, Some(attach_options))
            .await
            .with_context(|| format!("Failed to attach to container '{container_name}'"))
    }

    /// Starts `container_id` (already created) and streams `output`, its
    /// stdout/stderr from an `attach_output` made before this is called,
    /// until it exits, then returns its exit code — raw to the local stdout
    /// by default, or line-buffered into `ContainerOutput` events under the
    /// [`ContainerIoStreaming::Interleaved`] policy (the `all` output mode,
    /// where the logger owns stdout and every line needs a container
    /// prefix). Shared by the fully non-interactive path and
    /// `run_container_forwarding_stdin` below — both need identical output
    /// handling, differing only in whether stdin is piped in alongside it.
    ///
    /// `started`, if given, is signaled with `container_id` right after
    /// Docker's own `start` call succeeds — see `run_container`'s own doc
    /// comment for why.
    async fn start_and_stream_output(
        &self,
        container_name: &str,
        container_id: &str,
        output: impl futures::Stream<
            Item = Result<bollard::container::LogOutput, bollard::errors::Error>,
        >,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<i64> {
        self.docker
            .start_container(container_id, None)
            .await
            .with_context(|| format!("Failed to start container '{container_name}'"))?;
        tracing::debug!(container_id, "started container");
        if let Some(started) = started {
            let _ = started.send(());
        }

        if self.event_sink.container_io_streaming() == ContainerIoStreaming::Interleaved {
            drain_interleaved_output(output, &self.event_sink, container_name).await?;
        } else {
            let mut output = std::pin::pin!(output);
            while let Some(chunk) = output.next().await {
                let chunk = chunk.context("Failed to read container output")?;
                write_container_output(&mut std::io::stdout().lock(), chunk.as_ref())?;
            }
        }

        self.exit_code(container_id).await
    }

    /// Forwards the local process's stdin into `container_id` without
    /// allocating a real Docker TTY — the `interactive`-but-not-`use_tty`
    /// case (e.g. `should_use_tty`'s stdin-and-stdout-both-real-terminals
    /// gate failing because stdout was piped/redirected, even though this
    /// is still a task's own container). Matches Batect's own unconditional
    /// stdin forwarding for each task's own container, prerequisites
    /// included, independent of its separate (and stricter, here) TTY gate.
    ///
    /// Attaches stdin and output together *before* starting the container —
    /// same before-start ordering rationale as `run_container_interactively`,
    /// so nothing written early is lost — then reuses
    /// `start_and_stream_output` for that output, since this path's output
    /// handling is identical to the plain non-interactive case. Input comes
    /// from `stdin_reader`: the process's own, `StdinReader::process`, except
    /// in a test.
    async fn run_container_forwarding_stdin(
        &self,
        container_name: &str,
        container_id: &str,
        stdin_reader: &'static stdin::StdinReader,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<i64> {
        let AttachContainerResults {
            output,
            input: attach_input,
        } = self
            .attach_output(container_name, container_id, true)
            .await?;

        // Same as the interactive path's stdin pump — stopped once
        // output-following ends, not awaited to completion.
        let stdin_pump = spawn_stdin_pump(stdin_reader, attach_input);

        let result = self
            .start_and_stream_output(container_name, container_id, output, started)
            .await;
        drop(stdin_pump);
        result
    }
}

#[async_trait::async_trait]
impl ContainerRuntime for DockerClient {
    async fn pull_image(&self, image: &str) -> Result<String> {
        let (credentials, warning) =
            resolve_pull_credential(self.credential_resolver.as_ref(), image).await;
        if let Some(message) = warning {
            // Printed, not logged: a credential helper that failed is
            // something the user should fix (decisions/0017).
            self.event_sink.post(TaskEvent::Warning { message });
        }

        // Only the request is normalised: events, errors and the engine's
        // dedup keep `image` as the user wrote it, as Batect reports a pull
        // under its own configured name.
        let (from_image, tag) = pull_request_reference(image);
        let options = CreateImageOptions {
            from_image: Some(from_image.to_string()),
            tag: tag.map(str::to_string),
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, credentials);

        // Skipped entirely (not just discarded downstream) when the active
        // logger doesn't render it — see `EventSink::wants_progress_detail`
        // — so `simple`/`quiet`/`NullEventSink` runs don't pay a `String`
        // allocation per pull status line for nothing.
        let wants_progress = self.event_sink.wants_progress_detail();
        let mut pulled_digest = None;
        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => {
                    if let Some(status) = output.status {
                        if let Some(digest) = status.strip_prefix("Digest: ") {
                            pulled_digest = Some(digest.to_string());
                        }
                        if wants_progress {
                            self.event_sink.post(TaskEvent::ImagePullProgress {
                                image: image.to_string(),
                                message: status,
                            });
                        }
                    }
                }
                Err(e) => {
                    return Err(e).context(format!("Failed to pull image {}", image));
                }
            }
        }

        // No `Digest:` line (a daemon that didn't report one): the reference
        // as pulled, as docker-client falls back to.
        let lookup = match &pulled_digest {
            Some(digest) => digest_pinned_reference(image, digest),
            None => image.to_string(),
        };
        let pulled = self.docker.inspect_image(&lookup).await.with_context(|| {
            format!("Failed to find image {image} after pulling it (looked up as {lookup})")
        })?;
        pulled
            .id
            .with_context(|| format!("Image {image} has no ID after pulling it"))
    }

    async fn local_image_id(&self, image: &str) -> Result<Option<String>> {
        match self.docker.inspect_image(image).await {
            Ok(found) => found
                .id
                .map(Some)
                .with_context(|| format!("Image {image} exists locally but has no ID")),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(e).with_context(|| {
                format!("Failed to check whether image '{}' exists locally", image)
            }),
        }
    }

    async fn build_image(
        &self,
        spec: &crate::container_spec::BuildSpec,
        build_id: &str,
    ) -> Result<String> {
        let build_directory = spec.build_directory.as_path();
        let dockerfile = spec.dockerfile.as_str();
        // First, so a build that can't be served asks no credential helper
        // anything before it is refused.
        let builder_version = self.builder_version().await?;
        refuse_buildkit_options_on_the_classic_builder(
            &builder_version,
            spec.buildkit.as_ref(),
            &spec.tag,
        )?;
        // Read fresh on every build, not cached: `DockerClient` is
        // long-lived across a whole invocation's builds, and nothing says
        // `config.json` can't change between them (a `docker login` mid-run,
        // however unlikely). A missing or unreadable file is nothing
        // configured, silently, as a pull's resolver reads it.
        let config_json = tokio::fs::read_to_string(&self.docker_config_path)
            .await
            .ok();
        let build_args =
            build_args_with_config_file_proxies(spec, config_json.as_deref(), &self.daemon_host);
        let build_args = build_args.as_ref();
        let target = spec.target.as_deref();
        let buildkit = spec.buildkit.as_ref();
        let tag = spec.tag.as_str();
        let force_pull = spec.force_pull;
        let proxy_host_gateway = spec.proxy_host_gateway;

        let (registries, list_warning) = match &config_json {
            Some(config_json) => {
                build_registries(self.credential_resolver.as_ref(), config_json).await
            }
            None => (Vec::new(), None),
        };
        let (credentials, warnings) =
            resolve_build_credentials(self.credential_resolver.as_ref(), &registries).await;
        for message in list_warning.into_iter().chain(warnings) {
            self.event_sink.post(TaskEvent::Warning { message });
        }
        // `None` (not `Some(HashMap::new())`) when nothing resolved, so a
        // build with no registries configured sends exactly the request it
        // always has — no `X-Registry-Config` header at all, not one
        // carrying an empty JSON object.
        let credentials = (!credentials.is_empty()).then_some(credentials);

        match builder_version {
            bollard::query_parameters::BuilderVersion::BuilderBuildKit => {
                return build_image_via_buildkit(
                    &self.docker,
                    self.event_sink.as_ref(),
                    build_directory,
                    dockerfile,
                    build_args,
                    target,
                    buildkit,
                    tag,
                    force_pull,
                    proxy_host_gateway,
                    credentials,
                    build_id,
                )
                .await;
            }
            bollard::query_parameters::BuilderVersion::BuilderV1 => {}
        }

        let build_directory = build_directory.to_path_buf();
        let dockerfile_owned = dockerfile.to_string();
        let tar_bytes = tokio::task::spawn_blocking(move || {
            build_context_tar(&build_directory, &dockerfile_owned)
        })
        .await
        .context("Failed to build the Docker build context")??;

        let mut options_builder = BuildImageOptionsBuilder::default()
            .dockerfile(dockerfile)
            .t(tag)
            // `forcerm` as well as `rm`, as Batect sets both: `rm` alone
            // removes a step's intermediate container only when the step
            // succeeds, and leaves a failed `RUN`'s behind, unlabelled.
            .rm(true)
            .forcerm(true);
        if let Some(build_args) = build_args {
            options_builder = options_builder.buildargs(build_args);
        }
        if let Some(target) = target {
            options_builder = options_builder.target(target);
        }
        if force_pull {
            options_builder = options_builder.pull("true");
        }
        if let Some(gateway) = proxy_host_gateway {
            options_builder = options_builder.extrahosts(&gateway.extra_host());
        }
        let options = options_builder.build();

        let mut stream = self.docker.build_image(
            options,
            credentials,
            Some(bollard::body_full(tar_bytes.into())),
        );

        let mut image_id = None;
        // The full build transcript, so a failure's error carries everything
        // that led up to it (not just Docker's own one-line summary) —
        // always built regardless of output mode; the progress *event*
        // below is the one part skipped when the active logger doesn't
        // render it (see `EventSink::wants_progress_detail`).
        let mut output = String::new();
        let wants_progress = self.event_sink.wants_progress_detail();
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(message) = info.error_detail.and_then(|detail| detail.message) {
                        return Err(anyhow::anyhow!(
                            "Failed to build image '{}': {}{}",
                            tag,
                            message,
                            build_output_suffix(&output)
                        ));
                    }
                    if let Some(stream_line) = info.stream {
                        let trimmed = stream_line.trim();
                        if !trimmed.is_empty() {
                            tracing::debug!(image = tag, "{trimmed}");
                            output.push_str(trimmed);
                            output.push('\n');
                            if wants_progress {
                                self.event_sink.post(TaskEvent::ImageBuildProgress {
                                    tag: tag.to_string(),
                                    message: trimmed.to_string(),
                                });
                            }
                        }
                    }
                    // Classic (non-BuildKit) builds always report `Default`
                    // aux info — `BuildKit` is the other variant, only ever
                    // sent for a build issued with
                    // `BuilderVersion::BuilderBuildKit` (this path never
                    // sets that), which is how `build_image_via_buildkit`
                    // reports its own progress instead.
                    if let Some(bollard::models::BuildInfoAux::Default(aux_image_id)) = info.aux {
                        if let Some(id) = aux_image_id.id {
                            image_id = Some(id);
                        }
                    }
                }
                Err(e) => {
                    return Err(e).context(format!(
                        "Failed to build image '{}'{}",
                        tag,
                        build_output_suffix(&output)
                    ));
                }
            }
        }

        let image_id = image_id.ok_or_else(|| {
            anyhow::anyhow!("Docker did not report an image ID after building '{}'", tag)
        })?;

        Ok(image_id)
    }

    async fn cancel_build(&self, build_id: &str) -> Result<()> {
        // Only a BuildKit build has an ID to cancel by. A classic one stops
        // when its connection closes, and a daemon with no BuildKit may have
        // no `/build/cancel` at all, so asking would only warn falsely.
        if self.builder_version.get()
            != Some(&bollard::query_parameters::BuilderVersion::BuilderBuildKit)
        {
            tracing::debug!(build_id, "no BuildKit build to cancel");
            return Ok(());
        }
        self.docker
            .cancel_build(build_id)
            .await
            .context("Failed to cancel the build")?;
        tracing::debug!(build_id, "cancelled abandoned build");
        Ok(())
    }

    async fn tag_image(&self, image_id: &str, tag: &str) -> Result<()> {
        let (repo, tag_component) = split_image_reference(tag);
        let options = bollard::query_parameters::TagImageOptionsBuilder::default()
            .repo(repo)
            .tag(tag_component.unwrap_or("latest"))
            .build();
        self.docker
            .tag_image(image_id, Some(options))
            .await
            .with_context(|| format!("Failed to tag the image as '{tag}'"))?;
        tracing::debug!(image_id, tag, "tagged image");
        Ok(())
    }

    async fn create_network(&self, name: &str, labels: &HashMap<String, String>) -> Result<()> {
        self.docker
            .create_network(NetworkCreateRequest {
                name: name.to_string(),
                labels: Some(labels.clone()),
                ..Default::default()
            })
            .await
            .with_context(|| format!("Failed to create network '{}'", name))?;
        tracing::debug!(network = name, "created network");
        Ok(())
    }

    async fn network_exists(&self, name: &str) -> Result<bool> {
        match self.docker.inspect_network(name, None).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => {
                Err(e).with_context(|| format!("Failed to check whether network '{}' exists", name))
            }
        }
    }

    async fn create_container(
        &self,
        spec: &ContainerSpec,
    ) -> std::result::Result<String, CreateContainerError> {
        let shared = &spec.shared;
        let config = self
            .container_config(spec)
            .map_err(CreateContainerError::before_creation)?;
        let options = CreateContainerOptionsBuilder::new()
            .name(&shared.docker_name)
            .build();
        // Named for the container the user configured, not `docker_name`:
        // a daemon refusing the name (a conflict, or a character Docker
        // won't take) still has to say which container that was.
        let container = self
            .docker
            .create_container(Some(options), config)
            .await
            .with_context(|| format!("Failed to create container '{}'", shared.name))
            .map_err(CreateContainerError::before_creation)?;
        tracing::debug!(container_id = %container.id, name = shared.name.as_str(), image = shared.image.as_str(), "created container");
        // The inspect only feeds the log line, so it is skipped unless the
        // line would be printed.
        if tracing::enabled!(tracing::Level::DEBUG)
            && additional_args_replace_image_cmd(shared.command.as_deref(), &spec.additional_args)
        {
            self.log_replaced_image_cmd(spec).await;
        }

        // From here the container exists, so a failure carries its id out
        // for the caller to remove — see `CreateContainerError`.
        let prepared: Result<()> = async {
            if let Some(mapping) = &shared.user_mapping {
                // `apply_user_mapping`'s own detail names the Docker id; this
                // names what the user wrote — the container and the field.
                self.apply_user_mapping(&container.id, mapping)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to apply run_as_current_user to container '{}'",
                            shared.name
                        )
                    })?;
            }
            Ok(())
        }
        .await;
        match prepared {
            Ok(()) => Ok(container.id),
            Err(error) => Err(CreateContainerError {
                created: Some(container.id),
                error,
            }),
        }
    }

    async fn start_background_container(
        &self,
        container_id: &str,
        spec: &ContainerSpec,
    ) -> Result<()> {
        let shared = &spec.shared;
        // Under the interleaved policy (the `all` output mode — the only
        // mode that shows dependency output at all), this container's output
        // is followed for its whole lifetime, through an attach made before
        // it starts so nothing it writes at once is lost. An attach that
        // fails fails this start, as it would under Batect; only an error
        // once the stream is being read is swallowed, below.
        let attached =
            if self.event_sink.container_io_streaming() == ContainerIoStreaming::Interleaved {
                Some(
                    self.attach_output(&shared.name, container_id, false)
                        .await?,
                )
            } else {
                None
            };
        self.docker
            .start_container(container_id, None)
            .await
            .with_context(|| format!("Failed to start container '{}'", shared.name))?;
        tracing::debug!(
            container_id,
            name = shared.name.as_str(),
            "started container"
        );

        // The follower posts each line as an event. Not fully
        // fire-and-forget: the handle is kept in `log_followers`, and
        // `stop_and_remove_container` awaits (and removes) it before
        // returning, so this task's own exit can never race past the
        // container's actual removal — see `LogFollowers`' own doc comment
        // for why that ordering matters.
        if let Some(attached) = attached {
            let event_sink = std::sync::Arc::clone(&self.event_sink);
            let container_name = shared.name.clone();
            let handle = tokio::spawn(async move {
                if let Err(e) =
                    drain_interleaved_output(attached.output, &event_sink, &container_name).await
                {
                    // The stream ending with an error overwhelmingly means
                    // the container went away — normal during cleanup, not
                    // worth a `warn`. `debug` still leaves a trace for the
                    // rarer case (a genuine daemon hiccup on a still-running
                    // dependency, which would otherwise silently end that
                    // container's visible output for the rest of the task).
                    tracing::debug!(
                        container = container_name.as_str(),
                        error = ?e,
                        "dependency container output stream ended"
                    );
                }
            });
            self.log_followers.insert(container_id.to_string(), handle);
        }

        Ok(())
    }

    async fn wait_for_container_healthy(&self, container_id: &str) -> Result<()> {
        let inspection = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .with_context(|| format!("Failed to inspect container '{}'", container_id))?;
        let has_health_check = inspection
            .config
            .as_ref()
            .and_then(|config| config.healthcheck.as_ref())
            .and_then(|healthcheck| healthcheck.test.as_ref())
            .is_some_and(|test| !test.is_empty());
        if !has_health_check {
            tracing::debug!(container_id, "no health check, considering healthy");
            return Ok(());
        }

        tracing::debug!(container_id, "waiting for container to become healthy");

        // Replayed from the beginning of time (matching Batect), so a
        // verdict Docker emitted between the container starting and this
        // stream opening still counts — without `since`, that verdict
        // would be missed and this wait would hang.
        let filters = HashMap::from([
            ("container", vec![container_id]),
            ("event", vec!["die", "health_status"]),
        ]);
        let options = EventsOptionsBuilder::default()
            .since("0")
            .filters(&filters)
            .build();
        let mut events = self.docker.events(Some(options));
        let event = events
            .next()
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("Docker's event stream ended without reporting a health status")
            })?
            .context(
                "Failed to stream Docker events while waiting for the container to become healthy",
            )?;

        match event.action.as_deref() {
            Some("health_status: healthy") => {
                tracing::debug!(container_id, "container became healthy");
                Ok(())
            }
            Some("health_status: unhealthy") => {
                Err(anyhow::anyhow!(self.unhealthy_details(container_id).await))
            }
            Some("die") => Err(anyhow::anyhow!(
                "The container exited before becoming healthy."
            )),
            other => Err(anyhow::anyhow!(
                "Unexpected event '{}' received while waiting for the container to become healthy",
                other.unwrap_or("<none>")
            )),
        }
    }

    async fn container_output(&self, container_id: &str) -> Result<String> {
        let options = bollard::query_parameters::LogsOptionsBuilder::default()
            .follow(false)
            .stdout(true)
            .stderr(true)
            .build();
        let mut stream = self.docker.logs(container_id, Some(options));
        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| {
                format!("Failed to read the output of container '{container_id}'")
            })?;
            output.push_str(&chunk.to_string());
        }
        Ok(output)
    }

    async fn wait_for_container_exit(&self, container_id: &str) -> Result<i64> {
        self.exit_code(container_id).await
    }

    async fn container_is_running(&self, container_id: &str) -> Result<bool> {
        let inspection = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .with_context(|| format!("Failed to inspect container '{}'", container_id))?;
        inspection
            .state
            .and_then(|state| state.running)
            .with_context(|| format!("Docker reported no state for container '{}'", container_id))
    }

    async fn exec_in_container(
        &self,
        container_id: &str,
        command: &str,
        working_directory: Option<&str>,
        environment: Option<&HashMap<String, String>>,
        user_mapping: Option<&UserMapping>,
    ) -> Result<ExecResult> {
        let exec = self
            .docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    env: build_env(environment),
                    cmd: Some(tokenize_command_line(command)?),
                    user: user_mapping.map(|m| format!("{}:{}", m.user.uid, m.user.gid)),
                    working_dir: working_directory.map(str::to_string),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("Failed to create exec in container '{}'", container_id))?;

        let mut output = String::new();
        if let StartExecResults::Attached {
            output: mut stream, ..
        } = self
            .docker
            .start_exec(&exec.id, None)
            .await
            .with_context(|| format!("Failed to start exec in container '{}'", container_id))?
        {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("Failed to stream exec output")?;
                let text = chunk.to_string();
                tracing::debug!(container_id, output = %text.trim_end(), "exec output");
                output.push_str(&text);
            }
        }

        let inspection =
            self.docker.inspect_exec(&exec.id).await.with_context(|| {
                format!("Failed to inspect exec in container '{}'", container_id)
            })?;
        let exit_code = inspection.exit_code.ok_or_else(|| {
            anyhow::anyhow!(
                "Docker did not report an exit code for the exec in container '{}'",
                container_id
            )
        })?;

        Ok(ExecResult { exit_code, output })
    }

    async fn run_container(
        &self,
        container_id: &str,
        spec: &ContainerSpec,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        let shared = &spec.shared;
        let (interactive, use_tty) = self.task_container_io(spec);

        let exit_code = if use_tty {
            self.run_container_interactively(&shared.name, container_id, started)
                .await
        } else if interactive {
            self.run_container_forwarding_stdin(
                &shared.name,
                container_id,
                stdin::StdinReader::process(),
                started,
            )
            .await
        } else {
            let attached = self
                .attach_output(&shared.name, container_id, false)
                .await?;
            self.start_and_stream_output(&shared.name, container_id, attached.output, started)
                .await
        };

        let exit_code = exit_code?;
        tracing::debug!(container_id, exit_code, "container exited");
        if exit_code != 0 {
            return Err(ContainerExitedNonZero { exit_code }.into());
        }

        Ok(())
    }

    async fn remove_container_forcibly(&self, container_id: &str) -> Result<()> {
        match self
            .docker
            .remove_container(container_id, Some(remove_container_options()))
            .await
        {
            Ok(()) => tracing::debug!(container_id, "removed container"),
            // Already gone (404), or the daemon is already removing it
            // (409): a graceful stop dropped mid-flight on the escalation
            // rung may have got that far server-side. Either is the outcome
            // asked for, not a failure to report.
            Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
                if status_code == 404 || status_code == 409 =>
            {
                tracing::debug!(container_id, status_code, "container already removed");
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("Failed to remove container '{}'", container_id));
            }
        }

        // The caller (`engine.rs`) posts `TaskEvent::ContainerRemoved`
        // right after this returns — interleaved output must never arrive
        // after that event, so wait for any background log follower to
        // actually finish flushing first. See `LogFollowers`' own doc
        // comment.
        self.log_followers.await_and_remove(container_id).await;

        Ok(())
    }
}

#[async_trait::async_trait]
impl ResourceInventory for DockerClient {
    async fn list_containers(
        &self,
        labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        let options = bollard::query_parameters::ListContainersOptions {
            // Leftovers have usually exited; without this they'd be
            // invisible, which is most of the point.
            all: true,
            filters: Some(label_filters(labels)),
            ..Default::default()
        };
        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .context("Failed to list Docker containers")?;
        Ok(containers
            .into_iter()
            .map(|container| LabelledResource {
                id: container.id.unwrap_or_default(),
                name: container
                    .names
                    .and_then(|names| names.into_iter().next())
                    // Docker reports container names with a leading slash.
                    .map(|name| name.trim_start_matches('/').to_string())
                    .unwrap_or_default(),
                labels: container.labels.unwrap_or_default(),
                created: container.created,
                state: container.state.map(|state| state.to_string()),
            })
            .collect())
    }

    async fn list_networks(
        &self,
        labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        let options = bollard::query_parameters::ListNetworksOptions {
            filters: Some(label_filters(labels)),
        };
        let networks = self
            .docker
            .list_networks(Some(options))
            .await
            .context("Failed to list Docker networks")?;
        Ok(networks
            .into_iter()
            .map(|network| LabelledResource {
                id: network.id.unwrap_or_default(),
                name: network.name.unwrap_or_default(),
                labels: network.labels.unwrap_or_default(),
                created: network.created.map(|created| created.timestamp()),
                state: None,
            })
            .collect())
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.docker
            .remove_network(name)
            .await
            .with_context(|| format!("Failed to remove network '{}'", name))?;
        tracing::debug!(network = name, "removed network");
        Ok(())
    }

    async fn stop_and_remove_container(
        &self,
        container_id: &str,
        stop_signal: Option<&str>,
        stop_grace_period: Option<std::time::Duration>,
    ) -> Result<()> {
        // `None` when neither is set, so this call is byte-for-byte what it
        // was before ratect#112 — no default-flip for a container that
        // configures neither field.
        let stop_options = (stop_signal.is_some() || stop_grace_period.is_some()).then(|| {
            let mut builder = bollard::query_parameters::StopContainerOptionsBuilder::new();
            if let Some(signal) = stop_signal {
                builder = builder.signal(signal);
            }
            if let Some(grace_period) = stop_grace_period {
                // Docker's `t` is whole seconds; round up rather than
                // truncate, so a sub-second grace period (e.g. "500ms",
                // one of `stop_grace_period`'s own documented examples)
                // never becomes a *shorter* wait than configured.
                let seconds = grace_period.as_secs() + u64::from(grace_period.subsec_nanos() > 0);
                builder = builder.t(seconds.min(i32::MAX as u64) as i32);
            }
            builder.build()
        });
        self.docker
            .stop_container(container_id, stop_options)
            .await
            .with_context(|| format!("Failed to stop container '{}'", container_id))?;
        // The removal itself is the forced one: the container has just been
        // stopped, and `force` only means an exit that raced the stop is not
        // an error (see `remove_container_options`).
        self.remove_container_forcibly(container_id).await
    }
}

#[async_trait::async_trait]
impl VolumeStore for DockerClient {
    async fn list_volumes(&self) -> Result<Vec<String>> {
        let response = self
            .docker
            .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
            .await
            .context("Failed to list Docker volumes")?;
        Ok(response
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|volume| volume.name)
            .collect())
    }

    async fn remove_volume(&self, name: &str) -> Result<()> {
        self.docker
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await
            .with_context(|| format!("Failed to remove volume '{}'", name))?;

        Ok(())
    }
}

#[cfg(test)]
#[path = "docker_tests.rs"]
mod tests;
