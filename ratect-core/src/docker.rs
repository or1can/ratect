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

use crate::ui::{ContainerIoStreaming, EventSink, NullEventSink, TaskEvent};
use anyhow::{Context, Result};
use bollard::container::AttachContainerResults;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody as Config, DeviceMapping, EndpointSettings, HealthConfig,
    NetworkConnectRequest, NetworkCreateRequest, PortBinding, PortMap,
};
use bollard::query_parameters::AttachContainerOptionsBuilder;
use bollard::query_parameters::BuildImageOptionsBuilder;
use bollard::query_parameters::CreateImageOptions;
use bollard::query_parameters::EventsOptionsBuilder;
use bollard::query_parameters::InspectContainerOptions;
use bollard::query_parameters::LogsOptions;
use bollard::query_parameters::ResizeContainerTTYOptionsBuilder;
use bollard::query_parameters::UploadToContainerOptionsBuilder;
use bollard::query_parameters::WaitContainerOptions;
use bollard::service::HostConfig;
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;
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
/// Errors (unbalanced quote, trailing backslash) match Batect's own
/// messages.
fn tokenize_command_line(input: &str) -> Result<Vec<String>> {
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
/// `command` is tokenized via [`tokenize_command_line`] — the same literal,
/// no-shell-involved argv Batect itself would produce — with
/// `additional_args` appended as further literal argv entries, matching
/// Batect's own `ADDITIONAL_ARGS` handling exactly (no `sh -c`, no
/// positional-parameter trick). See `docs/differences-from-batect.md` for
/// what this means for `$VAR`/glob/shell-operator characters in `command`.
///
/// When `command` is unset, non-empty `additional_args` are passed directly
/// as argv, letting the image's own entrypoint receive them (matching plain
/// `docker run <image> <args>`).
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
/// own `--add-host` mechanism) from a config `additional_hosts` map. Sorted
/// by key for the same determinism reason as `build_env`.
fn build_extra_hosts(additional_hosts: Option<&HashMap<String, String>>) -> Option<Vec<String>> {
    let additional_hosts = additional_hosts?;
    let mut pairs: Vec<String> = additional_hosts
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

/// Per-container network-facing options shared by `run_container` and
/// `start_background_container` — bundled together (rather than three more
/// flat parameters) since both methods were already at
/// `#[allow(clippy::too_many_arguments)]` before this.
pub struct NetworkOptions<'a> {
    /// Extra network aliases beyond the container's own name.
    pub additional_hostnames: Option<&'a Vec<String>>,
    /// Extra `/etc/hosts` entries (hostname -> IP).
    pub additional_hosts: Option<&'a HashMap<String, String>>,
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
    /// via [`tokenize_command_line`] before reaching Docker — `None`
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

/// Configures BuildKit-only build features — `build_image` receives one
/// only when a container declares `build_secrets` and/or `build_ssh`,
/// converted from config types the same way as `HealthCheckOptions` above.
/// Independent of *which builder* runs the build (that's
/// [`select_builder_version`]'s call, from the daemon's advertised default):
/// `None` just means no session providers to serve. The one interaction:
/// `Some` requires the BuildKit builder — the classic builder has no session
/// to serve these over, so `build_image` fails clearly if the classic
/// builder is selected while providers are present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildKitOptions {
    /// Keyed by the id a Dockerfile's `RUN --mount=type=secret,id=<key>`
    /// references.
    pub secrets: HashMap<String, BuildSecretSource>,
    /// Forwards the *host* process's own `SSH_AUTH_SOCK` ssh-agent under
    /// BuildKit's implicit `default` agent id, for a Dockerfile's `RUN
    /// --mount=type=ssh`. Ratect's only supported form of the `build_ssh`
    /// config field — see its doc comment for why (`bollard`, the Docker
    /// client this is built on, only exposes this single on/off toggle,
    /// not Batect's multiple named agents / explicit key file forwarding).
    pub forward_default_ssh_agent: bool,
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
/// Docker's API expects.
fn build_health_config(health_check: Option<&HealthCheckOptions>) -> Option<HealthConfig> {
    let health_check = health_check?;
    Some(HealthConfig {
        test: health_check
            .command
            .as_ref()
            .map(|command| vec!["CMD-SHELL".to_string(), command.clone()]),
        interval: health_check.interval.map(|d| d.as_nanos() as i64),
        timeout: health_check.timeout.map(|d| d.as_nanos() as i64),
        retries: health_check.retries.map(i64::from),
        start_period: health_check.start_period.map(|d| d.as_nanos() as i64),
        start_interval: None,
    })
}

/// Builds Docker's `Config.exposed_ports` + `HostConfig.port_bindings` from
/// already-expanded `(local_port, container_port, protocol)` triples — pure,
/// unit-testable without a daemon. `None` when `ports` itself is `None`
/// (absent, or already filtered out by `--disable-ports` — see
/// `NetworkOptions::ports`) or empty.
fn build_port_config(ports: Option<&Vec<(u16, u16, String)>>) -> Option<(Vec<String>, PortMap)> {
    let ports = ports?;
    if ports.is_empty() {
        return None;
    }

    let mut exposed_ports = Vec::new();
    let mut port_bindings = PortMap::new();
    for (local_port, container_port, protocol) in ports {
        let key = format!("{container_port}/{protocol}");
        exposed_ports.push(key.clone());
        port_bindings.insert(
            key,
            Some(vec![PortBinding {
                host_ip: None,
                host_port: Some(local_port.to_string()),
            }]),
        );
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
        builder
            .append_path_with_name(&absolute_path, &relative_path)
            .with_context(|| format!("Failed to add {:?} to build context", absolute_path))?;
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
        .version(bollard::query_parameters::BuilderVersion::BuilderBuildKit)
        .session(&session_id);
    if let Some(build_args) = build_args {
        options_builder = options_builder.buildargs(build_args);
    }
    if let Some(target) = target {
        options_builder = options_builder.target(target);
    }

    let mut providers = bollard::grpc::build::ImageBuildSessionProviders::default();
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
        if buildkit.forward_default_ssh_agent {
            providers = providers.enable_ssh(true);
        }
    }
    let options = options_builder.build();

    let mut stream = docker.build_image_with_session_providers(
        options,
        None,
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

    let image_id = image_id.ok_or_else(|| {
        anyhow::anyhow!("Docker did not report an image ID after building '{}'", tag)
    })?;

    Ok(image_id)
}

/// Formats `output` (the build log accumulated so far) as a "Build output:"/// Formats `output` (the build log accumulated so far) as a "Build output:"
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

/// Whether a container run should actually get a real Docker TTY and its
/// stdin forwarded. `interactive` is eligibility — this is the top-level
/// requested task's own container, see `TaskEngine::run_task_internal` — not
/// a guarantee: it's further gated on the local process's own stdin *and*
/// stdout genuinely being connected to a terminal. Deliberately not decoupled
/// (unlike Batect, which always forwards stdin to the task container
/// regardless of whether a TTY is allocated) — piping input into a
/// non-interactive run isn't supported yet.
fn should_use_tty(interactive: bool, stdin_is_tty: bool, stdout_is_tty: bool) -> bool {
    interactive && stdin_is_tty && stdout_is_tty
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

/// Resizes `container_id`'s TTY to the local terminal's current size —
/// shared by the initial attach-time sync and every subsequent local resize
/// while the session is live (see `run_container_interactively` and
/// `spawn_resize_listener` below). Takes `&Docker` directly rather than
/// `&self` so it can also be called from a separately spawned task, holding
/// its own cloned client rather than borrowing the caller's. Best-effort: a
/// failure is logged and otherwise ignored, matching the previous one-shot
/// call this replaces.
/// Drives `container_id`'s Docker log stream to completion, line-buffering
/// its output into [`TaskEvent::ContainerOutput`] events on `event_sink` —
/// the [`ContainerIoStreaming::Interleaved`] policy's (the `all` output
/// mode's) per-container streaming, shared by `start_and_stream_logs`'s
/// interleaved branch (the task's own container, whose stream error should
/// propagate and fail the task) and `start_background_container`'s spawned
/// follower (a dependency's whole lifetime, fire-and-forget — that caller
/// logs and swallows the error instead). Thin wrapper around
/// [`drain_interleaved_log_stream`] that supplies the real Docker log
/// stream; split out so the actual line-buffering/flush logic is
/// unit-testable against a synthetic stream, with no live daemon needed.
async fn stream_logs_as_interleaved_events(
    docker: &Docker,
    event_sink: &std::sync::Arc<dyn EventSink>,
    container_name: &str,
    container_id: &str,
) -> Result<()> {
    let logs = docker.logs(
        container_id,
        Some(LogsOptions {
            stdout: true,
            stderr: true,
            follow: true,
            ..Default::default()
        }),
    );
    drain_interleaved_log_stream(logs, event_sink, container_name).await
}

/// The actual line-buffering loop — see [`stream_logs_as_interleaved_events`]
/// for why it always flushes any buffered partial line before returning,
/// whether `logs` ended cleanly or with an error.
async fn drain_interleaved_log_stream(
    logs: impl futures::Stream<Item = Result<bollard::container::LogOutput, bollard::errors::Error>>,
    event_sink: &std::sync::Arc<dyn EventSink>,
    container_name: &str,
) -> Result<()> {
    let mut logs = std::pin::pin!(logs);
    let mut line_buffer = crate::ui::interleaved::LineBuffer::new();
    let emit = |line: &str| {
        event_sink.post(TaskEvent::ContainerOutput {
            container: container_name.to_string(),
            line: line.to_string(),
        });
    };
    let result = loop {
        match logs.next().await {
            Some(Ok(output)) => line_buffer.push(output.as_ref(), emit),
            Some(Err(e)) => break Err(e).context("Failed to get container logs"),
            None => break Ok(()),
        }
    };
    line_buffer.flush(emit);
    result
}

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
/// other platforms; the caller's `#[cfg(not(unix))]` side just doesn't spawn
/// this, falling back to the previous once-at-attach-only behavior rather
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
/// config is enabled — see `TaskEngine::resolve_user_mapping`.
pub struct UserMapping {
    pub user: crate::user::CurrentUser,
    pub home_directory: String,
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

/// Builds an in-memory tar containing a single directory entry for
/// `mapping.home_directory`'s leaf name, owned by `mapping.user`'s uid/gid,
/// mode `0755` — extracted to the home directory's *parent* (see
/// `ContainerRuntime::run_container`'s `user_mapping` handling), matching
/// Batect's `uploadHomeDirectoryForConfiguration`. Pure (no Docker
/// involved), so it's unit-testable directly.
fn build_home_directory_tar(mapping: &UserMapping) -> Result<Vec<u8>> {
    let leaf_name = Path::new(&mapping.home_directory)
        .file_name()
        .with_context(|| {
            format!(
                "Invalid home directory '{}': no directory name",
                mapping.home_directory
            )
        })?
        .to_string_lossy()
        .into_owned();

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_uid(mapping.user.uid as u64);
    header.set_gid(mapping.user.gid as u64);
    header.set_size(0);
    header
        .set_path(format!("{leaf_name}/"))
        .with_context(|| format!("Invalid home directory '{}'", mapping.home_directory))?;
    header.set_cksum();

    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append(&header, std::io::empty())
        .context("Failed to add home directory to user mapping archive")?;

    builder
        .into_inner()
        .context("Failed to finalize home directory archive")
}

/// The parent directory `build_home_directory_tar`'s entry should be
/// extracted into — `/` if `home_directory` has no parent (e.g. `/home`).
fn home_directory_parent(home_directory: &str) -> String {
    Path::new(home_directory)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("/"))
        .to_string_lossy()
        .into_owned()
}

/// Creates any host-side bind-mount directories in `volumes` (already
/// resolved `"host:container"` strings) that don't exist yet — as the
/// current host user, before the container is created. Otherwise Docker's
/// daemon (running as root) would auto-create them as `root:root` on first
/// use, defeating the point of `run_as_current_user` for the common "mount
/// my code directory, get build artifacts back with sane ownership" case.
/// Pure filesystem logic, no Docker involved. Ported from Batect's
/// `createMissingMountDirectories`.
fn ensure_host_volume_directories_exist(volumes: Option<&Vec<String>>) -> Result<()> {
    let Some(volumes) = volumes else {
        return Ok(());
    };

    for volume in volumes {
        let Some((host_path, _)) = volume.split_once(':') else {
            continue;
        };
        let path = Path::new(host_path);
        // No `path.exists()` pre-check: `create_dir_all` is already a no-op
        // success if `path` is an existing directory, so the check bought
        // nothing but a TOCTOU race (something else removing/replacing
        // `path` between the check and the create). It also used to mean a
        // pre-existing *non-directory* at `path` was silently left alone
        // here, deferring to a more confusing failure later when Docker
        // tries to bind-mount it — now `create_dir_all` reports that
        // directly.
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create host directory {:?}", path))?;
    }

    Ok(())
}

/// Abstracts the container operations the task engine needs, so tests can
/// inject a fake implementation instead of talking to a real Docker daemon.
#[async_trait::async_trait]
pub trait ContainerRuntime {
    async fn pull_image(&self, image: &str) -> Result<()>;

    /// `true` if `image` already exists in the local Docker image cache —
    /// used by `image_pull_policy: IfNotPresent` (the default) to decide
    /// whether a pull is needed at all, matching Batect's own semantics.
    /// Mirrors `network_exists`'s own 404-as-false convention.
    async fn image_exists_locally(&self, image: &str) -> Result<bool>;

    /// Builds an image from `build_directory` (already resolved to an
    /// absolute path), tagging it as `tag`. `dockerfile` is the Dockerfile
    /// to build, as a path relative to `build_directory`'s own root
    /// (`"Dockerfile"` for the default case). `build_args` are passed
    /// through as Docker's own `--build-arg` mechanism; `target`, when
    /// `Some`, as `--target` (the build stage to stop at, for a multi-stage
    /// Dockerfile). `buildkit`, when `Some`, switches the build to a
    /// BuildKit gRPC session instead of Docker's classic build API — see
    /// [`BuildKitOptions`].
    ///
    /// Returns the built image's ID (e.g. `sha256:...`), not `tag` — `tag` is
    /// applied so the image is identifiable in `docker images`, but isn't
    /// guaranteed unique (see `TaskEngine::resolve_image`), so callers must
    /// use the returned ID, not `tag`, to reliably reference the image this
    /// call just built.
    async fn build_image(
        &self,
        build_directory: &Path,
        dockerfile: &str,
        build_args: Option<&HashMap<String, String>>,
        target: Option<&str>,
        buildkit: Option<&BuildKitOptions>,
        tag: &str,
    ) -> Result<String>;

    /// Tags `image_id` (the ID `build_image` returned) with each of `tags`,
    /// in addition to `build_image`'s own `tag` — used by `--tag-image`.
    /// Both bollard's classic and BuildKit build options only ever accept
    /// one `t` value each, so extra tags are applied as separate `docker
    /// tag`-equivalent calls after the build completes, rather than folded
    /// into the build request itself.
    async fn tag_image(&self, image_id: &str, tags: &[String]) -> Result<()>;

    async fn create_network(&self, name: &str) -> Result<()>;

    async fn remove_network(&self, name: &str) -> Result<()>;

    /// `true` if a network named (or IDed) `name` already exists — used to
    /// validate `--use-network` up front, with a clear error, rather than
    /// letting an unrelated Docker API failure surface later when trying to
    /// join it.
    async fn network_exists(&self, name: &str) -> Result<bool>;

    /// Starts a container in the background (does not wait for it to exit),
    /// joined to `network` with a network alias of `alias` so other
    /// containers on the same network can reach it by that name. Returns the
    /// container id, used later to stop/remove it. Used for sidecar/dependency
    /// containers. `environment` is that container's own `environment` field
    /// (a dependency has no task `run`, so nothing to layer on top of it).
    ///
    /// `command` is this container's own `command` — unlike `working_directory`/
    /// `ports`, `customise` has no override for it (matching Batect's own
    /// `TaskContainerCustomisation`, which doesn't either), so this is always
    /// the container's own value, verbatim. Tokenized via
    /// [`tokenize_command_line`] the same way `run_container`'s is — `None`
    /// runs the image's own default `CMD` instead. Unlike `run_container`,
    /// there's no `additional_args` here — a dependency never receives
    /// `-- ADDITIONAL_ARGS` (only the top-level requested task's own
    /// container can).
    ///
    /// `user_mapping` is `Some` when this container's own `run_as_current_user`
    /// is enabled (independent of whether the task's own container has it
    /// enabled — see `TaskEngine::resolve_user_mapping`) — see `run_container`'s
    /// doc comment for what applying it actually does.
    ///
    /// `network_options` carries this container's own `additional_hostnames`/
    /// `additional_hosts` — see `run_container`'s doc comment.
    ///
    /// `health_check` overrides the image's own `HEALTHCHECK` configuration
    /// at creation — see [`HealthCheckOptions`]. Applying it here is what
    /// makes [`wait_for_container_healthy`](Self::wait_for_container_healthy)
    /// meaningful for images with no health check of their own.
    #[allow(clippy::too_many_arguments)]
    async fn start_background_container(
        &self,
        alias: &str,
        image: &str,
        command: Option<&str>,
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
        health_check: Option<&HealthCheckOptions>,
        container_options: &ContainerOptions,
    ) -> Result<String>;

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
    ///   `Ok`; reported-unhealthy fails with the last health-check run's
    ///   exit code and output; exiting before a verdict fails too.
    ///
    /// No Ratect-side timeout, matching Batect — Docker's own
    /// `retries`/`interval` bound how long a verdict can take.
    async fn wait_for_container_healthy(&self, container_id: &str) -> Result<()>;

    /// Runs `command` inside the already-running `container_id` — used for
    /// `setup_commands`. Tokenized into literal argv via
    /// [`tokenize_command_line`], the same as `command`/`entrypoint` — no
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

    /// Stops and removes a container started with [`start_background_container`](Self::start_background_container).
    async fn stop_and_remove_container(&self, container_id: &str) -> Result<()>;

    /// Runs a container to completion, streaming its logs to stdout, then
    /// removes it. `name` is this container's own network alias (used when
    /// `network` is set); used for a task's own container.
    ///
    /// `additional_args` are appended as literal argv entries after
    /// `command`'s own tokenized argv (see [`build_cmd`]) — matching
    /// Batect's own `ADDITIONAL_ARGS` mechanism exactly, never re-parsed as
    /// shell syntax regardless of what characters they contain. If `command`
    /// is `None`, `additional_args` (when non-empty) are passed directly as
    /// the container's argv, letting the image's own entrypoint receive them.
    /// `environment` is the container's own `environment` merged with the
    /// task's `run.environment` (which wins on key collision). `network` is
    /// this task execution's own isolated network — every task gets one,
    /// regardless of whether it has dependencies.
    ///
    /// `interactive` is *eligibility*, not a guarantee — only ever `true` for
    /// the top-level requested task's own container (never a prerequisite's,
    /// a dependency's, or a sidecar's — see `TaskEngine::run_task_internal`).
    /// Whether a real Docker TTY actually gets allocated additionally
    /// depends on the local process's own stdin/stdout genuinely being
    /// terminals; when they're not (piped output, CI, a redirected
    /// non-terminal), this container runs exactly as if `interactive` were
    /// `false`.
    ///
    /// `user_mapping` is `Some` when this container's `run_as_current_user`
    /// is enabled. When present: any of `volumes`' host paths that don't
    /// exist yet are created first (as the current host user, so Docker's
    /// daemon doesn't auto-create them as `root:root`); the container's
    /// `User` is set to the mapped `uid:gid`; and, after creation but before
    /// starting, minimal `/etc/passwd`/`/etc/shadow`/`/etc/group` entries and
    /// the declared home directory (owned by that `uid:gid`) are uploaded
    /// into it — an arbitrary host uid/gid otherwise has no corresponding
    /// entry in the image's own passwd/group, which many programs need to
    /// function at all.
    ///
    /// `network_options` bundles this container's own `additional_hostnames`
    /// (extra network aliases, beyond `name`, other containers can reach it
    /// by) and `additional_hosts` (extra `/etc/hosts` entries) — grouped into
    /// one struct rather than two more flat parameters, since both of these
    /// methods were already at `#[allow(clippy::too_many_arguments)]` before
    /// this. The container's Docker `hostname` is always set to `name`
    /// (matching Batect), independent of `network_options`.
    ///
    /// `remove_on_exit` is `--no-cleanup`/`--no-cleanup-after-success`'s own
    /// policy (see `TaskEngine::cleanup_after_success`): `false` leaves the
    /// exited container behind (never removed) regardless of its exit code
    /// — a nonzero exit is still "success" for cleanup-gating purposes,
    /// matching Batect (only an infrastructure failure, which never reaches
    /// this far, is "failure").
    #[allow(clippy::too_many_arguments)]
    async fn run_container(
        &self,
        name: &str,
        image: &str,
        command: Option<&str>,
        additional_args: &[String],
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        interactive: bool,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
        health_check: Option<&HealthCheckOptions>,
        container_options: &ContainerOptions,
        remove_on_exit: bool,
    ) -> Result<()>;
}

pub struct DockerClient {
    docker: Docker,
    /// The builder every `build_image` call uses, resolved once per client
    /// (per `ratect` invocation, in practice) on first build — see
    /// [`select_builder_version`] for the decision itself.
    builder_version: tokio::sync::OnceCell<bollard::query_parameters::BuilderVersion>,
    /// Where streamed pull/build progress detail goes for the user to see —
    /// `docker.rs` only ever posts the fine-grained progress variants (the
    /// milestones around them are `engine.rs`'s job, which knows the
    /// container/task names). [`NullEventSink`] (silent) by default, a real
    /// output-mode logger via `with_event_sink`. See `crate::ui`.
    event_sink: std::sync::Arc<dyn EventSink>,
    /// Container ID -> the background log-follower task
    /// `start_background_container` spawned for it under the interleaved
    /// (`all` output mode) policy. `stop_and_remove_container` awaits (and
    /// removes) the matching entry before returning, so a dependency's
    /// follower can never race past the container's own removal —
    /// `TaskEvent::ContainerRemoved` (which `engine.rs` posts right after
    /// `stop_and_remove_container` returns) is then only ever posted once
    /// the follower has finished flushing everything it's going to,
    /// instead of the two racing as genuinely fire-and-forget tasks.
    log_followers: std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// `true` when `--enable-buildkit` was given: forces BuildKit on for
    /// every build this client makes, taking precedence over the
    /// `DOCKER_BUILDKIT` environment variable — matching Batect's own
    /// `TristateFlagOption`, whose default value provider *is* that same
    /// environment variable, so an explicit flag on the command line always
    /// wins over it. `false` (the default) defers to
    /// `DOCKER_BUILDKIT`/the daemon's own advertised default, unchanged. See
    /// [`select_builder_version`] — there's no `--disable-buildkit`
    /// counterpart, matching Batect exactly (forcing it *off* is only ever
    /// done via `DOCKER_BUILDKIT=0`).
    enable_buildkit: bool,
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

/// The `DOCKER_BUILDKIT` value `select_builder_version` actually sees:
/// forced to `"1"` when `--enable-buildkit` was given, taking precedence
/// over `real_env_value` regardless of what it says — matching Batect's own
/// `TristateFlagOption`, whose default-value provider *is* this same
/// environment variable, so an explicit flag on the command line always
/// wins over it. `real_env_value`, verbatim, otherwise. There's no
/// `--disable-buildkit` counterpart (matching Batect exactly) — forcing the
/// classic builder is only ever done via `DOCKER_BUILDKIT=0`/`false`.
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

/// Picks the builder for this invocation's image builds, matching Batect's
/// own selection (`DockerConnectivity.kt`): an explicit `DOCKER_BUILDKIT`
/// environment variable wins (`1`/`true` forces BuildKit, `0`/`false` forces
/// the classic builder — the same env var Batect reads as its
/// `--enable-buildkit` default, and the docker CLI's own override
/// convention); otherwise the builder the daemon itself advertises as its
/// default (the `/_ping` response's `Builder-Version` header — `"2"` is
/// BuildKit) is used, which is BuildKit on any modern daemon. A missing
/// header (a daemon old enough to predate it) falls back to the classic
/// builder. A `DOCKER_BUILDKIT` value that parses as neither is a hard error
/// naming the value, matching Batect, rather than a silent guess.
///
/// Pure (both inputs injected) so the whole decision table is
/// unit-testable; [`DockerClient`] feeds it the real environment variable
/// and ping header.
fn select_builder_version(
    docker_buildkit_env: Option<&str>,
    daemon_advertised: Option<&str>,
) -> Result<bollard::query_parameters::BuilderVersion> {
    use bollard::query_parameters::BuilderVersion;
    if let Some(value) = docker_buildkit_env {
        return match value.to_ascii_lowercase().as_str() {
            "1" | "true" => Ok(BuilderVersion::BuilderBuildKit),
            "0" | "false" => Ok(BuilderVersion::BuilderV1),
            other => Err(anyhow::anyhow!(
                "The DOCKER_BUILDKIT environment variable is set to '{other}', which is not a \
                 valid value — use '1'/'true' to force BuildKit or '0'/'false' to force the \
                 classic builder."
            )),
        };
    }
    Ok(match daemon_advertised {
        Some("2") => BuilderVersion::BuilderBuildKit,
        _ => BuilderVersion::BuilderV1,
    })
}

/// CLI-facing Docker daemon connection selection (`--docker-host`,
/// `--docker-context`, `--docker-config`, `--docker-tls`/`-verify`,
/// `--docker-cert-path`, `--docker-tls-ca-cert`/`-cert`/`-key`) — `None`/
/// `false` for anything not explicitly given on the command line, so
/// `DockerClient::new` falls back to the real
/// `DOCKER_HOST`/`DOCKER_CONTEXT`/`DOCKER_CONFIG`/`DOCKER_CERT_PATH`/
/// `DOCKER_TLS_VERIFY` environment variables and the Docker CLI's own
/// active-context resolution, exactly matching Batect's own precedence
/// (`CommandLineOptionsParser.resolveDockerContext`).
///
/// One deliberate divergence from Batect, documented in
/// [Differences from Batect](../../docs/differences-from-batect.md): there's
/// no way to skip TLS verification here. Batect's own `--docker-tls`
/// (without `-verify`) sets Go's `tls.Config.InsecureSkipVerify`, which
/// disables *all* server certificate verification — chain of trust,
/// expiry, and hostname matching, not just hostname matching — while still
/// doing the TLS handshake and any configured client-certificate auth.
/// `tls` and `tls_verify` are both accepted here (for command-line
/// compatibility) but behave identically: connecting always fully
/// verifies the daemon's certificate.
#[derive(Debug, Default, Clone)]
pub struct DockerConnectionOptions {
    pub host: Option<String>,
    pub context: Option<String>,
    pub config_directory: Option<PathBuf>,
    pub tls: bool,
    pub tls_verify: bool,
    pub cert_path: Option<PathBuf>,
    pub tls_ca_cert: Option<PathBuf>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

/// The Docker CLI's own context store identifier for a context name —
/// lowercase hex `sha256(name)`. Matches the Docker CLI's own
/// `contextdir.go` naming exactly (verified against a real
/// `~/.docker/contexts/meta/<id>/meta.json` entry on this machine).
fn docker_context_id(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(name.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The subset of a context's `meta.json` this needs — just the daemon host
/// to connect to. Field names/casing match the Docker CLI's own format
/// exactly (`Endpoints.docker.Host`; `docker` itself is lowercase, unlike
/// its sibling fields).
#[derive(serde::Deserialize)]
struct DockerContextMetadata {
    #[serde(rename = "Endpoints")]
    endpoints: DockerContextEndpoints,
}

#[derive(serde::Deserialize)]
struct DockerContextEndpoints {
    docker: DockerContextDockerEndpoint,
}

#[derive(serde::Deserialize)]
struct DockerContextDockerEndpoint {
    #[serde(rename = "Host")]
    host: String,
}

/// Reads `<config_directory>/contexts/meta/<sha256(context_name)>/meta.json`
/// for `context_name`'s daemon host. A missing file (or one that doesn't
/// parse as expected) is reported as the named context not existing —
/// matching what `--docker-context` naming an unknown context should feel
/// like to a user, rather than a raw file-not-found error.
fn docker_context_host(config_directory: &Path, context_name: &str) -> Result<String> {
    let meta_path = config_directory
        .join("contexts")
        .join("meta")
        .join(docker_context_id(context_name))
        .join("meta.json");
    let contents = fs::read_to_string(&meta_path).with_context(|| {
        format!(
            "Docker context '{context_name}' does not exist (expected to find it at {}).",
            meta_path.display()
        )
    })?;
    let metadata: DockerContextMetadata = serde_json::from_str(&contents).with_context(|| {
        format!(
            "Failed to read Docker context '{context_name}' ({})",
            meta_path.display()
        )
    })?;
    Ok(metadata.endpoints.docker.host)
}

/// The subset of the Docker CLI's own `config.json` this needs — just the
/// active context's name.
#[derive(serde::Deserialize, Default)]
struct DockerCliConfig {
    #[serde(rename = "currentContext", default)]
    current_context: Option<String>,
}

/// The Docker CLI's own "currently active" context, from
/// `<config_directory>/config.json`'s `currentContext` field — consulted
/// only when neither `--docker-context`/`--docker-host` nor `DOCKER_CONTEXT`
/// says otherwise. `None` (not an error) when the file doesn't exist or sets
/// no `currentContext` — both mean the same thing as the Docker CLI's own
/// fallback: use the `default` context.
fn active_docker_context(config_directory: &Path) -> Option<String> {
    let contents = fs::read_to_string(config_directory.join("config.json")).ok()?;
    let config: DockerCliConfig = serde_json::from_str(&contents).ok()?;
    config.current_context.filter(|name| !name.is_empty())
}

/// `--docker-config`, else `DOCKER_CONFIG`, else `~/.docker` — the
/// directory the Docker CLI's own context store and `config.json` live in.
fn docker_config_directory(options: &DockerConnectionOptions) -> Result<PathBuf> {
    if let Some(dir) = &options.config_directory {
        return Ok(dir.clone());
    }
    if let Ok(dir) = std::env::var("DOCKER_CONFIG") {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::user::home_directory()?.join(".docker"))
}

/// `--docker-cert-path`, else `DOCKER_CERT_PATH`, else `~/.docker` — the
/// directory `ca.pem`/`cert.pem`/`key.pem` are read from unless
/// `--docker-tls-ca-cert`/`-cert`/`-key` individually override one.
/// Resolved independently of `docker_config_directory` (its own separate
/// environment variable, even though both happen to share the same
/// hardcoded default) — matching Batect's own two independently-settable
/// options exactly.
fn docker_cert_directory(options: &DockerConnectionOptions) -> Result<PathBuf> {
    if let Some(dir) = &options.cert_path {
        return Ok(dir.clone());
    }
    if let Ok(dir) = std::env::var("DOCKER_CERT_PATH") {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::user::home_directory()?.join(".docker"))
}

/// Whether this invocation should connect over TLS at all: `--docker-tls`
/// and `--docker-tls-verify` both enable it (Ratect always verifies
/// regardless of which — see `DockerConnectionOptions`'s own doc comment),
/// same as the real `DOCKER_TLS_VERIFY` environment variable (the only one
/// of the two flags Batect gives an environment variable default at all).
fn tls_enabled(options: &DockerConnectionOptions, docker_tls_verify_env: Option<&str>) -> bool {
    options.tls
        || options.tls_verify
        || matches!(
            docker_tls_verify_env
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("1") | Some("true")
        )
}

/// Installs `rustls`'s `ring` cryptographic provider as the process-wide
/// default, exactly once — `bollard::Docker::connect_with_ssl` panics if
/// asked to build a TLS connection before one is installed (there's no
/// provider bundled by default; `ratect-core`'s own `bollard` dependency
/// enables just enough of `ssl_providerless` for that, matching bollard's
/// own `ssl` feature). Idempotent: a later call after the first is a no-op
/// (`install_default` only errors if something else already installed a
/// provider, which never happens here — nothing else in `ratect-core`
/// touches `rustls` directly).
fn ensure_crypto_provider_installed() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The host to connect to once no context applies (step 2's own
/// resolution, used both for the plain and TLS paths): an explicit
/// `--docker-host`, else the real `DOCKER_HOST` environment variable, else
/// `None` (only valid for a plain, non-TLS connection — see `connect`,
/// which requires an explicit host for TLS). Pure (the environment value is
/// injected) so it's unit-testable without depending on whichever real
/// environment variables happen to be set on the machine running the
/// tests.
fn resolve_host(
    options: &DockerConnectionOptions,
    docker_host_env: Option<&str>,
) -> Option<String> {
    options
        .host
        .clone()
        .or_else(|| docker_host_env.map(str::to_string))
}

/// TLS has no platform-default host to fall back to the way the plain path
/// does (`Docker::connect_with_local_defaults`) — an explicit host is
/// required. Pure (takes the already-resolved host, not the environment)
/// purely to keep this one error message unit-testable in isolation.
fn require_host_for_tls(host: Option<String>) -> Result<String> {
    host.ok_or_else(|| {
        anyhow::anyhow!(
            "--docker-tls/--docker-tls-verify requires --docker-host (or the DOCKER_HOST \
             environment variable) to be set."
        )
    })
}

/// Step 1–3 of `connect`'s own doc comment, as a pure decision: which
/// context (if any) should be looked up in the store. A `None` return means
/// "no context — connect via `options.host`/`DOCKER_HOST`/the platform
/// default instead", covering both an explicit `--docker-host` (which skips
/// context resolution entirely) and the `default` context name itself
/// (never looked up in the store — it *means* "no context").
///
/// `active_context` is step 4's fallback — reading the store's own "active
/// context" needs real file I/O, so it's computed by the caller and passed
/// in already-resolved, keeping this function itself pure (and so
/// unit-testable without a filesystem) like `select_builder_version`.
fn resolve_context_name(
    options: &DockerConnectionOptions,
    docker_context_env: Option<&str>,
    active_context: Option<String>,
) -> Option<String> {
    let context_name = if let Some(context) = &options.context {
        Some(context.clone())
    } else if options.host.is_some() {
        None
    } else if let Some(context) = docker_context_env {
        Some(context.to_string())
    } else {
        active_context
    };

    context_name.filter(|name| name != "default")
}

/// Batect's own `forbiddenOptionsWithDockerContext` set, named one at a
/// time so the error can say exactly which flag conflicts, matching
/// Batect's own message format (`"Cannot use both --docker-context and
/// --docker-host."`) rather than a generic "these are mutually exclusive"
/// dump.
fn conflicting_option_with_context(options: &DockerConnectionOptions) -> Option<&'static str> {
    if options.host.is_some() {
        Some("--docker-host")
    } else if options.tls {
        Some("--docker-tls")
    } else if options.tls_verify {
        Some("--docker-tls-verify")
    } else if options.cert_path.is_some() {
        Some("--docker-cert-path")
    } else if options.tls_ca_cert.is_some() {
        Some("--docker-tls-ca-cert")
    } else if options.tls_cert.is_some() {
        Some("--docker-tls-cert")
    } else if options.tls_key.is_some() {
        Some("--docker-tls-key")
    } else {
        None
    }
}

/// Resolves and connects to the Docker daemon, matching Batect's own
/// precedence (`CommandLineOptionsParser.resolveDockerContext`/
/// `DockerClientConfigurationFactory`) exactly:
///
/// 1. An explicit `--docker-context` is looked up by name in the context
///    store.
/// 2. Otherwise, an explicit `--docker-host` connects directly to that host
///    — bypassing the context store entirely, even if `DOCKER_CONTEXT` or
///    an active context is also set (Batect's own rule: an explicit host
///    always means "ignore whatever context would otherwise apply").
/// 3. Otherwise, `DOCKER_CONTEXT` (if set) is looked up the same way as 1.
/// 4. Otherwise, the Docker CLI's own "active" context
///    (`~/.docker/config.json`'s `currentContext`) is looked up the same
///    way, falling back to connecting via `DOCKER_HOST`/bollard's own
///    platform default (unix socket/named pipe) when that's unset or names
///    the `default` context.
///
/// TLS (`--docker-tls`/`-verify`) only applies once a context is ruled out
/// — Batect rejects combining it with `--docker-context` at all (see
/// `conflicting_option_with_context`) — and has no platform-default host to
/// fall back to the way the plain path does, so an explicit host is
/// required (see `require_host_for_tls`).
fn connect(options: &DockerConnectionOptions) -> Result<Docker> {
    if options.context.is_some() {
        if let Some(conflicting) = conflicting_option_with_context(options) {
            anyhow::bail!("Cannot use both --docker-context and {conflicting}.");
        }
    }

    let config_directory = docker_config_directory(options)?;
    let docker_context_env = std::env::var("DOCKER_CONTEXT").ok();
    let context_name = resolve_context_name(
        options,
        docker_context_env.as_deref(),
        active_docker_context(&config_directory),
    );

    if let Some(context_name) = context_name {
        let host = docker_context_host(&config_directory, &context_name)?;
        return Docker::connect_with_host(&host).with_context(|| {
            format!("Failed to connect to Docker context '{context_name}' (host '{host}')")
        });
    }

    let docker_host_env = std::env::var("DOCKER_HOST").ok();
    let host = resolve_host(options, docker_host_env.as_deref());

    let docker_tls_verify_env = std::env::var("DOCKER_TLS_VERIFY").ok();
    if !tls_enabled(options, docker_tls_verify_env.as_deref()) {
        return match host {
            Some(host) => Docker::connect_with_host(&host)
                .with_context(|| format!("Failed to connect to Docker host '{host}'")),
            None => Docker::connect_with_local_defaults().context("Failed to connect to Docker"),
        };
    }

    let host = require_host_for_tls(host)?;
    let cert_directory = docker_cert_directory(options)?;
    let ca = options
        .tls_ca_cert
        .clone()
        .unwrap_or_else(|| cert_directory.join("ca.pem"));
    let cert = options
        .tls_cert
        .clone()
        .unwrap_or_else(|| cert_directory.join("cert.pem"));
    let key = options
        .tls_key
        .clone()
        .unwrap_or_else(|| cert_directory.join("key.pem"));

    ensure_crypto_provider_installed();
    Docker::connect_with_ssl(&host, &key, &cert, &ca, 120, bollard::API_DEFAULT_VERSION)
        .with_context(|| format!("Failed to connect to Docker host '{host}' over TLS"))
}

impl DockerClient {
    pub fn new(connection: &DockerConnectionOptions) -> Result<Self> {
        let docker = connect(connection)?;
        Ok(Self {
            docker,
            builder_version: tokio::sync::OnceCell::new(),
            event_sink: std::sync::Arc::new(NullEventSink),
            log_followers: std::sync::Mutex::new(HashMap::new()),
            enable_buildkit: false,
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
    /// see [`select_builder_version`].
    async fn builder_version(&self) -> Result<bollard::query_parameters::BuilderVersion> {
        self.builder_version
            .get_or_try_init(|| async {
                let real_env_value = std::env::var("DOCKER_BUILDKIT").ok();
                let env_value =
                    docker_buildkit_env_value(self.enable_buildkit, real_env_value.as_deref());
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

    async fn join_network(
        &self,
        container_id: &str,
        network: &str,
        alias: &str,
        additional_hostnames: Option<&Vec<String>>,
    ) -> Result<()> {
        let mut aliases = vec![alias.to_string()];
        if let Some(additional_hostnames) = additional_hostnames {
            aliases.extend(additional_hostnames.iter().cloned());
        }
        self.docker
            .connect_network(
                network,
                NetworkConnectRequest {
                    container: container_id.to_string(),
                    endpoint_config: Some(EndpointSettings {
                        aliases: Some(aliases),
                        ..Default::default()
                    }),
                },
            )
            .await
            .with_context(|| format!("Failed to connect '{}' to network '{}'", alias, network))?;
        tracing::debug!(container_id, network, alias, "joined network");
        Ok(())
    }

    /// Uploads the synthetic `/etc/passwd`/`/etc/shadow`/`/etc/group` and the
    /// home directory `mapping` needs into `container_id` — must be called
    /// after the container is created but before it's started (Docker's own
    /// "upload archive to container" API works on an already-created
    /// container's filesystem; the container needs those files in place
    /// before its own process starts reading them).
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

        let home_tar = build_home_directory_tar(mapping)?;
        let home_parent = home_directory_parent(&mapping.home_directory);
        let home_options = UploadToContainerOptionsBuilder::default()
            .path(&home_parent)
            .build();
        self.docker
            .upload_to_container(
                container_id,
                Some(home_options),
                bollard::body_full(home_tar.into()),
            )
            .await
            .with_context(|| {
                format!("Failed to upload home directory to container '{container_id}'")
            })?;

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
    async fn run_container_interactively(&self, container_id: &str) -> Result<i64> {
        let attach_options = AttachContainerOptionsBuilder::default()
            .stdin(true)
            .stdout(true)
            .stderr(true)
            .stream(true)
            .build();
        let AttachContainerResults {
            output: mut attach_output,
            input: mut attach_input,
        } = self
            .docker
            .attach_container(container_id, Some(attach_options))
            .await
            .context("Failed to attach to container")?;

        let _raw_mode = RawModeGuard::enable()?;

        self.docker
            .start_container(container_id, None)
            .await
            .context("Failed to start container")?;
        tracing::debug!(container_id, "started container interactively");

        // Syncs the container's TTY to the local terminal's size once, at
        // attach time — then `resize_listener` (Unix only) keeps it in sync
        // for the rest of the session, on every subsequent local resize.
        resize_tty(&self.docker, container_id).await;
        #[cfg(unix)]
        let resize_listener = Some(spawn_resize_listener(
            self.docker.clone(),
            container_id.to_string(),
        ));
        #[cfg(not(unix))]
        let resize_listener: Option<tokio::task::JoinHandle<()>> = None;

        // Local stdin has no natural end of its own here — the attach
        // output stream ending (the container exiting) is what ends the
        // session, so this pump is aborted once that happens rather than
        // awaited to completion.
        let stdin_pump = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let _ = tokio::io::copy(&mut stdin, &mut attach_input).await;
        });

        let mut stdout = tokio::io::stdout();
        let output_result: Result<()> = async {
            while let Some(chunk) = attach_output.next().await {
                let log_output = chunk.context("Failed to read container output")?;
                stdout.write_all(log_output.as_ref()).await?;
                stdout.flush().await?;
            }
            Ok(())
        }
        .await;
        stdin_pump.abort();
        if let Some(handle) = resize_listener {
            handle.abort();
        }
        output_result?;

        self.exit_code(container_id).await
    }

    /// Starts `container_id` (already created) and streams its stdout/stderr
    /// until it exits, then returns its exit code — raw to the local stdout
    /// by default, or line-buffered into `ContainerOutput` events under the
    /// [`ContainerIoStreaming::Interleaved`] policy (the `all` output mode,
    /// where the logger owns stdout and every line needs a container
    /// prefix). Uses Docker's plain (non-TTY) `logs` follow API. Shared by
    /// the fully non-interactive path and `run_container_forwarding_stdin`
    /// below — both need identical output handling, differing only in
    /// whether stdin is piped in alongside it.
    async fn start_and_stream_logs(&self, container_name: &str, container_id: &str) -> Result<i64> {
        self.docker.start_container(container_id, None).await?;
        tracing::debug!(container_id, "started container");

        if self.event_sink.container_io_streaming() == ContainerIoStreaming::Interleaved {
            stream_logs_as_interleaved_events(
                &self.docker,
                &self.event_sink,
                container_name,
                container_id,
            )
            .await?;
        } else {
            let mut logs = self.docker.logs(
                container_id,
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
        }

        self.exit_code(container_id).await
    }

    /// Forwards the local process's stdin into `container_id` without
    /// allocating a real Docker TTY — the `interactive`-but-not-`use_tty`
    /// case (e.g. `should_use_tty`'s stdin-and-stdout-both-real-terminals
    /// gate failing because stdout was piped/redirected, even though this
    /// is still the top-level requested task). Matches Batect's own
    /// unconditional stdin forwarding for the task's own container,
    /// independent of its separate (and stricter, here) TTY gate.
    ///
    /// Attaches stdin-only *before* starting the container — same
    /// before-start ordering rationale as `run_container_interactively`, so
    /// nothing written early is lost — then reuses `start_and_stream_logs`
    /// for output, since this path's output handling is identical to the
    /// plain non-interactive case.
    async fn run_container_forwarding_stdin(
        &self,
        container_name: &str,
        container_id: &str,
    ) -> Result<i64> {
        let attach_options = AttachContainerOptionsBuilder::default()
            .stdin(true)
            .stream(true)
            .build();
        let AttachContainerResults {
            input: mut attach_input,
            ..
        } = self
            .docker
            .attach_container(container_id, Some(attach_options))
            .await
            .context("Failed to attach to container")?;

        // Same "no natural end of its own" rationale as the interactive
        // path's stdin pump — aborted once output-following ends, not
        // awaited to completion.
        let stdin_pump = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let _ = tokio::io::copy(&mut stdin, &mut attach_input).await;
        });

        let result = self
            .start_and_stream_logs(container_name, container_id)
            .await;
        stdin_pump.abort();
        result
    }

    /// Awaits (and removes) `container_id`'s background log-follower
    /// handle, if `start_background_container` started one for it (the
    /// interleaved policy — see `log_followers`' own doc comment for why
    /// `stop_and_remove_container` needs this ordering). A no-op for any
    /// container with no follower — every non-interleaved run, and the
    /// task's own container (which streams via `start_and_stream_logs`
    /// directly, never through a spawned follower). Split out from
    /// `stop_and_remove_container` so the awaiting behavior itself is
    /// unit-testable without a live Docker daemon.
    async fn await_log_follower(&self, container_id: &str) {
        let follower = self.log_followers.lock().unwrap().remove(container_id);
        if let Some(follower) = follower {
            // A `JoinError` here only means the follower task itself
            // panicked — already-posted events aren't affected, and
            // there's nothing this caller could do about it beyond not
            // hanging, so it's discarded rather than turned into a
            // container-removal failure.
            let _ = follower.await;
        }
    }
}

#[async_trait::async_trait]
impl ContainerRuntime for DockerClient {
    async fn pull_image(&self, image: &str) -> Result<()> {
        let options = CreateImageOptions {
            from_image: Some(image.to_string()),
            ..Default::default()
        };

        let mut stream = self.docker.create_image(Some(options), None, None);

        // Skipped entirely (not just discarded downstream) when the active
        // logger doesn't render it — see `EventSink::wants_progress_detail`
        // — so `simple`/`quiet`/`NullEventSink` runs don't pay a `String`
        // allocation per pull status line for nothing.
        let wants_progress = self.event_sink.wants_progress_detail();
        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => {
                    if wants_progress {
                        if let Some(status) = output.status {
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

        Ok(())
    }

    async fn image_exists_locally(&self, image: &str) -> Result<bool> {
        match self.docker.inspect_image(image).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(e).with_context(|| {
                format!("Failed to check whether image '{}' exists locally", image)
            }),
        }
    }

    async fn build_image(
        &self,
        build_directory: &Path,
        dockerfile: &str,
        build_args: Option<&HashMap<String, String>>,
        target: Option<&str>,
        buildkit: Option<&BuildKitOptions>,
        tag: &str,
    ) -> Result<String> {
        match self.builder_version().await? {
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
                )
                .await;
            }
            bollard::query_parameters::BuilderVersion::BuilderV1 => {
                if buildkit.is_some() {
                    // The classic builder has no session for the daemon to
                    // request secret bytes / ssh-agent proxying over —
                    // these fields are impossible without BuildKit, so fail
                    // clearly rather than building without them.
                    anyhow::bail!(
                        "Building '{}' requires BuildKit ('build_secrets'/'build_ssh' cannot \
                         be served by the classic builder), but the classic builder is \
                         selected — the Docker daemon doesn't advertise BuildKit as its \
                         default builder, or DOCKER_BUILDKIT=0 forces it off.",
                        tag
                    );
                }
            }
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
            .rm(true);
        if let Some(build_args) = build_args {
            options_builder = options_builder.buildargs(build_args);
        }
        if let Some(target) = target {
            options_builder = options_builder.target(target);
        }
        let options = options_builder.build();

        let mut stream =
            self.docker
                .build_image(options, None, Some(bollard::body_full(tar_bytes.into())));

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

    async fn tag_image(&self, image_id: &str, tags: &[String]) -> Result<()> {
        for tag in tags {
            let (repo, tag_component) = split_image_reference(tag);
            let options = bollard::query_parameters::TagImageOptionsBuilder::default()
                .repo(repo)
                .tag(tag_component.unwrap_or("latest"))
                .build();
            self.docker
                .tag_image(image_id, Some(options))
                .await
                .with_context(|| format!("Failed to tag image '{}' as '{}'", image_id, tag))?;
            tracing::debug!(image_id, tag, "tagged image");
        }
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

    async fn start_background_container(
        &self,
        alias: &str,
        image: &str,
        command: Option<&str>,
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
        health_check: Option<&HealthCheckOptions>,
        container_options: &ContainerOptions,
    ) -> Result<String> {
        if user_mapping.is_some() {
            ensure_host_volume_directories_exist(volumes)?;
        }
        let entrypoint = container_options
            .entrypoint
            .map(tokenize_command_line)
            .transpose()?;
        let cmd = build_cmd(command, &[])?;
        let port_config = build_port_config(network_options.ports);

        let host_config = HostConfig {
            binds: volumes.cloned(),
            extra_hosts: build_extra_hosts(network_options.additional_hosts),
            port_bindings: port_config.as_ref().map(|(_, bindings)| bindings.clone()),
            cap_add: container_options.capabilities_to_add.cloned(),
            cap_drop: container_options.capabilities_to_drop.cloned(),
            privileged: container_options.privileged,
            shm_size: container_options.shm_size,
            devices: build_devices(container_options.devices),
            init: container_options.enable_init_process,
            ..Default::default()
        };

        let config = Config {
            hostname: Some(alias.to_string()),
            image: Some(image.to_string()),
            cmd,
            entrypoint,
            env: build_env(environment),
            exposed_ports: port_config.as_ref().map(|(exposed, _)| exposed.clone()),
            user: user_mapping.map(|m| format!("{}:{}", m.user.uid, m.user.gid)),
            healthcheck: build_health_config(health_check),
            working_dir: container_options.working_directory.map(str::to_string),
            labels: container_options.labels.cloned(),
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(None, config)
            .await
            .with_context(|| format!("Failed to create sidecar container '{}'", alias))?;
        tracing::debug!(container_id = %container.id, alias, image, "created sidecar container");

        if let Some(mapping) = user_mapping {
            self.apply_user_mapping(&container.id, mapping).await?;
        }

        self.join_network(
            &container.id,
            network,
            alias,
            network_options.additional_hostnames,
        )
        .await?;

        self.docker
            .start_container(&container.id, None)
            .await
            .with_context(|| format!("Failed to start sidecar container '{}'", alias))?;
        tracing::debug!(container_id = %container.id, alias, "started sidecar container");

        // Under the interleaved policy (the `all` output mode — the only
        // mode that shows dependency output at all), follow this
        // container's logs in the background for its whole lifetime,
        // posting each line as an event. Not fully fire-and-forget: the
        // handle is kept in `log_followers`, and `stop_and_remove_container`
        // awaits (and removes) it before returning, so this task's own
        // exit can never race past the container's actual removal — see
        // `log_followers`' own doc comment for why that ordering matters.
        if self.event_sink.container_io_streaming() == ContainerIoStreaming::Interleaved {
            let docker = self.docker.clone();
            let event_sink = std::sync::Arc::clone(&self.event_sink);
            let container_name = alias.to_string();
            let container_id = container.id.clone();
            let handle = tokio::spawn(async move {
                if let Err(e) = stream_logs_as_interleaved_events(
                    &docker,
                    &event_sink,
                    &container_name,
                    &container_id,
                )
                .await
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
                        "dependency container log stream ended"
                    );
                }
            });
            self.log_followers
                .lock()
                .unwrap()
                .insert(container.id.clone(), handle);
        }

        Ok(container.id)
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

        // The caller (`engine.rs`) posts `TaskEvent::ContainerRemoved`
        // right after this returns — interleaved output must never arrive
        // after that event, so wait for any background log follower to
        // actually finish flushing first. See `await_log_follower`/
        // `log_followers`' own doc comments.
        self.await_log_follower(container_id).await;

        Ok(())
    }

    async fn run_container(
        &self,
        name: &str,
        image: &str,
        command: Option<&str>,
        additional_args: &[String],
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        interactive: bool,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
        health_check: Option<&HealthCheckOptions>,
        container_options: &ContainerOptions,
        remove_on_exit: bool,
    ) -> Result<()> {
        // Independently re-enforces the same policy `engine.rs` already
        // gated `interactive` on before calling here — see
        // `ContainerIoStreaming::allows_interactive`'s own docs for why
        // both sites call the one method rather than each hand-rolling a
        // comparison against a specific variant.
        let interactive = interactive
            && self
                .event_sink
                .container_io_streaming()
                .allows_interactive();
        let use_tty = should_use_tty(
            interactive,
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        );

        if user_mapping.is_some() {
            ensure_host_volume_directories_exist(volumes)?;
        }
        let cmd = build_cmd(command, additional_args)?;
        let entrypoint = container_options
            .entrypoint
            .map(tokenize_command_line)
            .transpose()?;
        let port_config = build_port_config(network_options.ports);

        let host_config = HostConfig {
            binds: volumes.cloned(),
            extra_hosts: build_extra_hosts(network_options.additional_hosts),
            port_bindings: port_config.as_ref().map(|(_, bindings)| bindings.clone()),
            cap_add: container_options.capabilities_to_add.cloned(),
            cap_drop: container_options.capabilities_to_drop.cloned(),
            privileged: container_options.privileged,
            shm_size: container_options.shm_size,
            devices: build_devices(container_options.devices),
            init: container_options.enable_init_process,
            ..Default::default()
        };

        let config = Config {
            hostname: Some(name.to_string()),
            image: Some(image.to_string()),
            cmd,
            entrypoint,
            env: build_env(environment),
            exposed_ports: port_config.as_ref().map(|(exposed, _)| exposed.clone()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: use_tty.then_some(true),
            // `open_stdin`/`attach_stdin` are gated on `interactive` alone —
            // deliberately wider than `use_tty` — so piping input into a
            // task still works even when a real TTY isn't allocated (e.g.
            // Ratect's own stdout is redirected to a file), matching
            // Batect's own unconditional stdin forwarding for the task's
            // own container. `tty`/`stdin_once` stay TTY-only: those control
            // pty allocation itself, which still requires both stdin and
            // stdout to be real terminals (`should_use_tty`, unchanged).
            open_stdin: interactive.then_some(true),
            attach_stdin: interactive.then_some(true),
            stdin_once: use_tty.then_some(true),
            user: user_mapping.map(|m| format!("{}:{}", m.user.uid, m.user.gid)),
            healthcheck: build_health_config(health_check),
            working_dir: container_options.working_directory.map(str::to_string),
            labels: container_options.labels.cloned(),
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = self.docker.create_container(None, config).await?;
        tracing::debug!(container_id = %container.id, image, "created container");

        if let Some(mapping) = user_mapping {
            self.apply_user_mapping(&container.id, mapping).await?;
        }

        self.join_network(
            &container.id,
            network,
            name,
            network_options.additional_hostnames,
        )
        .await?;

        let exit_code = if use_tty {
            self.run_container_interactively(&container.id).await?
        } else if interactive {
            self.run_container_forwarding_stdin(name, &container.id)
                .await?
        } else {
            self.start_and_stream_logs(name, &container.id).await?
        };

        if remove_on_exit {
            self.docker.remove_container(&container.id, None).await?;
            tracing::debug!(container_id = %container.id, exit_code, "removed container");
        } else {
            tracing::info!(
                container_id = %container.id,
                exit_code,
                "cleanup disabled; leaving container in place for investigation"
            );
        }

        if exit_code != 0 {
            return Err(ContainerExitedNonZero { exit_code }.into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, unique scratch directory — same pattern as
    /// `config.rs`'s `unique_temp_dir`. Caller cleans up.
    fn unique_temp_dir() -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let dir = std::env::temp_dir().join(format!(
            "ratect-docker-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            count
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Records every posted `ContainerOutput` event's line, in order — the
    /// only variant `drain_interleaved_log_stream`'s tests need.
    #[derive(Default)]
    struct RecordingEventSink {
        lines: std::sync::Mutex<Vec<String>>,
    }

    impl EventSink for RecordingEventSink {
        fn post(&self, event: TaskEvent) {
            if let TaskEvent::ContainerOutput { line, .. } = event {
                self.lines.lock().unwrap().push(line);
            }
        }
    }

    #[tokio::test]
    async fn drain_interleaved_log_stream_flushes_a_buffered_partial_line_before_a_stream_error() {
        // The bug this proves fixed: an unterminated final line (no
        // trailing newline) followed by the log stream itself erroring
        // (e.g. the daemon restarting mid-stream) used to be silently
        // dropped — the early `return Err(...)` on the error skipped the
        // trailing flush that would have emitted it.
        let chunks = vec![
            Ok(bollard::container::LogOutput::StdOut {
                message: bytes::Bytes::from_static(b"first line\n"),
            }),
            Ok(bollard::container::LogOutput::StdOut {
                message: bytes::Bytes::from_static(b"unterminated final line"),
            }),
            Err(bollard::errors::Error::NoHomePathError),
        ];
        let stream = futures::stream::iter(chunks);
        // Kept as the concrete type so its recorded lines are inspectable
        // after the call, alongside the `Arc<dyn EventSink>` the function
        // itself needs — both point at the same underlying instance.
        let sink = std::sync::Arc::new(RecordingEventSink::default());
        let dyn_sink: std::sync::Arc<dyn EventSink> = sink.clone();

        let result = drain_interleaved_log_stream(stream, &dyn_sink, "app").await;

        assert!(result.is_err(), "the stream error should still propagate");
        assert_eq!(
            *sink.lines.lock().unwrap(),
            vec!["first line", "unterminated final line"],
            "the unterminated final line must still be flushed despite the stream error"
        );
    }

    #[tokio::test]
    async fn await_log_follower_waits_for_the_spawned_task_to_finish() {
        // `DockerClient::new` only builds a lazily-connecting client (no
        // handshake), so this doesn't need a live daemon.
        let client = DockerClient::new(&Default::default())
            .expect("DockerClient::new should not require a live daemon");
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished_in_task = std::sync::Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            finished_in_task.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        client
            .log_followers
            .lock()
            .unwrap()
            .insert("container-1".to_string(), handle);

        client.await_log_follower("container-1").await;

        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "await_log_follower should not return before the spawned task finishes — this is \
             the exact race the fix closes: ContainerRemoved posting before a follower's final \
             flush"
        );
        assert!(
            client.log_followers.lock().unwrap().is_empty(),
            "the entry should be removed once awaited, so a later call for the same id is a no-op"
        );
    }

    #[tokio::test]
    async fn await_log_follower_is_a_no_op_for_a_container_with_no_follower() {
        let client = DockerClient::new(&Default::default())
            .expect("DockerClient::new should not require a live daemon");
        // Every non-interleaved run (and the task's own container, always)
        // never inserts an entry at all — must return immediately, not hang.
        client.await_log_follower("no-such-container").await;
    }

    #[test]
    fn build_health_config_is_none_without_an_override() {
        assert_eq!(build_health_config(None), None);
    }

    #[test]
    fn build_health_config_maps_all_fields() {
        let options = HealthCheckOptions {
            command: Some("pg_isready".to_string()),
            interval: Some(Duration::from_secs(2)),
            retries: Some(5),
            start_period: Some(Duration::from_secs(90)),
            timeout: Some(Duration::from_millis(500)),
        };

        assert_eq!(
            build_health_config(Some(&options)),
            Some(HealthConfig {
                test: Some(vec!["CMD-SHELL".to_string(), "pg_isready".to_string()]),
                interval: Some(2_000_000_000),
                timeout: Some(500_000_000),
                retries: Some(5),
                start_period: Some(90_000_000_000),
                start_interval: None,
            })
        );
    }

    #[test]
    fn build_health_config_leaves_omitted_fields_unset_to_inherit_from_the_image() {
        let options = HealthCheckOptions {
            command: None,
            interval: Some(Duration::from_secs(1)),
            retries: None,
            start_period: None,
            timeout: None,
        };

        assert_eq!(
            build_health_config(Some(&options)),
            Some(HealthConfig {
                test: None,
                interval: Some(1_000_000_000),
                timeout: None,
                retries: None,
                start_period: None,
                start_interval: None,
            })
        );
    }

    #[test]
    fn build_extra_hosts_formats_and_sorts_name_ip_pairs() {
        let mut hosts = HashMap::new();
        hosts.insert("zeta-service".to_string(), "10.0.0.2".to_string());
        hosts.insert("alpha-service".to_string(), "10.0.0.1".to_string());

        assert_eq!(
            build_extra_hosts(Some(&hosts)),
            Some(vec![
                "alpha-service:10.0.0.1".to_string(),
                "zeta-service:10.0.0.2".to_string(),
            ])
        );
    }

    #[test]
    fn build_extra_hosts_is_none_when_additional_hosts_is_absent() {
        assert_eq!(build_extra_hosts(None), None);
    }

    #[test]
    fn build_devices_maps_local_container_and_options() {
        let devices = vec![
            (
                "/dev/sda".to_string(),
                "/dev/xvda".to_string(),
                Some("rwm".to_string()),
            ),
            ("/dev/sdb".to_string(), "/dev/xvdb".to_string(), None),
        ];

        assert_eq!(
            build_devices(Some(&devices)),
            Some(vec![
                DeviceMapping {
                    path_on_host: Some("/dev/sda".to_string()),
                    path_in_container: Some("/dev/xvda".to_string()),
                    cgroup_permissions: Some("rwm".to_string()),
                },
                DeviceMapping {
                    path_on_host: Some("/dev/sdb".to_string()),
                    path_in_container: Some("/dev/xvdb".to_string()),
                    cgroup_permissions: Some("rwm".to_string()),
                },
            ])
        );
    }

    #[test]
    fn build_devices_defaults_missing_options_to_rwm() {
        // Docker's own API has no default for cgroup_permissions — an
        // absent value makes runc fail outright. See build_devices' doc
        // comment for why this must be filled in here.
        let devices = vec![("/dev/sda".to_string(), "/dev/xvda".to_string(), None)];

        let result = build_devices(Some(&devices)).unwrap();
        assert_eq!(result[0].cgroup_permissions, Some("rwm".to_string()));
    }

    #[test]
    fn build_devices_is_none_when_devices_is_absent() {
        assert_eq!(build_devices(None), None);
    }

    #[test]
    fn build_port_config_is_none_when_ports_is_absent() {
        assert!(build_port_config(None).is_none());
    }

    #[test]
    fn build_port_config_is_none_when_ports_is_empty() {
        assert!(build_port_config(Some(&vec![])).is_none());
    }

    #[test]
    fn build_port_config_builds_exposed_ports_and_bindings() {
        let ports = vec![
            (8080, 80, "tcp".to_string()),
            (9000, 9000, "udp".to_string()),
        ];
        let (exposed, bindings) = build_port_config(Some(&ports)).unwrap();

        assert_eq!(exposed, vec!["80/tcp".to_string(), "9000/udp".to_string()]);
        assert_eq!(
            bindings["80/tcp"],
            Some(vec![PortBinding {
                host_ip: None,
                host_port: Some("8080".to_string()),
            }])
        );
        assert_eq!(
            bindings["9000/udp"],
            Some(vec![PortBinding {
                host_ip: None,
                host_port: Some("9000".to_string()),
            }])
        );
    }

    #[test]
    fn build_cmd_with_command_and_no_additional_args_tokenizes_it() {
        let cmd = build_cmd(Some("echo hi there"), &[]).unwrap();
        assert_eq!(
            cmd,
            Some(vec![
                "echo".to_string(),
                "hi".to_string(),
                "there".to_string(),
            ])
        );
    }

    #[test]
    fn build_cmd_with_command_and_additional_args_appends_them_as_literal_argv() {
        let additional_args = vec!["arg1".to_string(), "arg2".to_string()];
        let cmd = build_cmd(Some("echo hi"), &additional_args).unwrap();
        assert_eq!(
            cmd,
            Some(vec![
                "echo".to_string(),
                "hi".to_string(),
                "arg1".to_string(),
                "arg2".to_string(),
            ])
        );
    }

    #[test]
    fn build_cmd_with_no_command_and_no_additional_args_lets_the_image_use_its_own_entrypoint() {
        // `None` (not an empty `Vec`) — bollard/Docker treats an unset `cmd`
        // as "use the image's own default CMD/entrypoint", which an empty
        // array wouldn't.
        assert_eq!(build_cmd(None, &[]).unwrap(), None);
    }

    #[test]
    fn build_cmd_with_no_command_and_additional_args_passes_them_directly_as_argv() {
        let additional_args = vec!["migrate".to_string(), "--up".to_string()];
        let cmd = build_cmd(None, &additional_args).unwrap();
        assert_eq!(cmd, Some(vec!["migrate".to_string(), "--up".to_string()]));
    }

    #[test]
    fn build_cmd_with_an_invalid_command_and_no_additional_args_fails() {
        assert!(build_cmd(Some("echo 'unbalanced"), &[]).is_err());
    }

    #[test]
    fn tokenize_command_line_splits_on_whitespace() {
        assert_eq!(
            tokenize_command_line("echo   hi   there").unwrap(),
            vec!["echo", "hi", "there"]
        );
    }

    #[test]
    fn tokenize_command_line_treats_single_quoted_content_as_one_literal_argument() {
        // The classic Batect idiom for forcing `sh -c`'s command string to
        // stay a single argv token: `entrypoint: /bin/sh -c`, `command:
        // 'make lint'` (the outer quotes are YAML's; the value is the
        // literal string `'make lint'`).
        assert_eq!(
            tokenize_command_line("'make lint'").unwrap(),
            vec!["make lint"]
        );
    }

    #[test]
    fn entrypoint_and_command_combine_correctly_for_the_classic_sh_c_idiom() {
        // `entrypoint: /bin/sh -c` alongside `command: 'make lint'` is a
        // real, working Batect idiom — Docker execs `Entrypoint ++ Cmd`, so
        // this must produce exactly `/bin/sh -c "make lint"`, with neither
        // side inserting its own extra shell layer (the bug an earlier,
        // sh-c-wrapped `build_cmd` would have had once `entrypoint` support
        // landed — see CHANGELOG.md).
        let entrypoint = tokenize_command_line("/bin/sh -c").unwrap();
        assert_eq!(entrypoint, vec!["/bin/sh", "-c"]);

        let cmd = build_cmd(Some("'make lint'"), &[]).unwrap();
        assert_eq!(cmd, Some(vec!["make lint".to_string()]));
    }

    #[test]
    fn tokenize_command_line_does_not_process_escapes_inside_single_quotes() {
        assert_eq!(
            tokenize_command_line(r"'a\b'").unwrap(),
            vec![r"a\b".to_string()]
        );
    }

    #[test]
    fn tokenize_command_line_processes_escapes_inside_double_quotes() {
        assert_eq!(
            tokenize_command_line(r#""a\"b""#).unwrap(),
            vec![r#"a"b"#.to_string()]
        );
    }

    #[test]
    fn tokenize_command_line_processes_a_backslash_escape_outside_any_quote() {
        assert_eq!(
            tokenize_command_line(r"hello\ world").unwrap(),
            vec!["hello world"]
        );
    }

    #[test]
    fn tokenize_command_line_rejects_a_trailing_backslash() {
        let err = tokenize_command_line(r"echo hi\").unwrap_err();
        assert!(err.to_string().contains("ends with a backslash"));
    }

    #[test]
    fn tokenize_command_line_rejects_an_unbalanced_single_quote() {
        let err = tokenize_command_line("echo 'hi").unwrap_err();
        assert!(err.to_string().contains("unbalanced single quote"));
    }

    #[test]
    fn tokenize_command_line_rejects_an_unbalanced_double_quote() {
        let err = tokenize_command_line(r#"echo "hi"#).unwrap_err();
        assert!(err.to_string().contains("unbalanced double quote"));
    }

    #[test]
    fn tokenize_command_line_of_an_empty_string_produces_no_arguments() {
        assert_eq!(tokenize_command_line("").unwrap(), Vec::<String>::new());
    }

    /// The `/`-joined relative paths of every entry in a tar built by
    /// `build_context_tar`, for assertions.
    fn tar_entry_paths(tar_bytes: &[u8]) -> Vec<String> {
        let mut archive = tar::Archive::new(tar_bytes);
        archive
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn build_context_tar_includes_everything_when_no_dockerignore() {
        let dir = unique_temp_dir();
        fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
        fs::write(dir.join("app.txt"), "hello").unwrap();
        fs::create_dir_all(dir.join("subdir")).unwrap();
        fs::write(dir.join("subdir/nested.txt"), "nested").unwrap();

        let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
        let mut entries = tar_entry_paths(&tar_bytes);
        entries.sort();

        assert_eq!(
            entries,
            vec![
                "Dockerfile".to_string(),
                "app.txt".to_string(),
                "subdir/nested.txt".to_string(),
            ]
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_context_tar_excludes_dockerignore_matches() {
        let dir = unique_temp_dir();
        fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
        fs::write(dir.join(".dockerignore"), "secret.txt\n").unwrap();
        fs::write(dir.join("secret.txt"), "shh").unwrap();
        fs::write(dir.join("app.txt"), "hello").unwrap();

        let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
        let entries = tar_entry_paths(&tar_bytes);

        assert!(!entries.contains(&"secret.txt".to_string()));
        assert!(entries.contains(&"app.txt".to_string()));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_context_tar_always_includes_dockerfile_and_dockerignore_under_broad_exclusion() {
        let dir = unique_temp_dir();
        fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
        fs::write(dir.join(".dockerignore"), "*\n").unwrap();
        fs::write(dir.join("app.txt"), "hello").unwrap();

        let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
        let entries = tar_entry_paths(&tar_bytes);

        assert!(entries.contains(&"Dockerfile".to_string()));
        assert!(entries.contains(&".dockerignore".to_string()));
        assert!(!entries.contains(&"app.txt".to_string()));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_context_tar_force_includes_a_custom_named_dockerfile() {
        let dir = unique_temp_dir();
        fs::create_dir_all(dir.join("docker")).unwrap();
        fs::write(dir.join("docker/Dockerfile.prod"), "FROM alpine").unwrap();
        fs::write(dir.join(".dockerignore"), "*\n").unwrap();
        fs::write(dir.join("app.txt"), "hello").unwrap();

        let tar_bytes = build_context_tar(&dir, "docker/Dockerfile.prod").unwrap();
        let entries = tar_entry_paths(&tar_bytes);

        assert!(entries.contains(&"docker/Dockerfile.prod".to_string()));
        assert!(entries.contains(&".dockerignore".to_string()));
        assert!(!entries.contains(&"app.txt".to_string()));

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Proves the root-only-for-bare-patterns behavior (see the
    /// `dockerignore` crate) holds end-to-end through the tar: a bare
    /// pattern only excludes a root-level match, not a nested one with the
    /// same name.
    #[test]
    fn build_context_tar_bare_pattern_only_excludes_at_the_root() {
        let dir = unique_temp_dir();
        fs::write(dir.join("Dockerfile"), "FROM alpine").unwrap();
        fs::write(dir.join(".dockerignore"), "build\n").unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::write(dir.join("build/output.txt"), "root build output").unwrap();
        fs::create_dir_all(dir.join("packages/foo/build")).unwrap();
        fs::write(
            dir.join("packages/foo/build/output.txt"),
            "nested build output",
        )
        .unwrap();

        let tar_bytes = build_context_tar(&dir, "Dockerfile").unwrap();
        let entries = tar_entry_paths(&tar_bytes);

        assert!(!entries.contains(&"build/output.txt".to_string()));
        assert!(entries.contains(&"packages/foo/build/output.txt".to_string()));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_output_suffix_is_empty_when_nothing_was_captured() {
        assert_eq!(build_output_suffix(""), "");
    }

    #[test]
    fn build_output_suffix_includes_the_trimmed_transcript() {
        let output = "Step 1/3 : FROM alpine\nStep 2/3 : RUN false\n";
        assert_eq!(
            build_output_suffix(output),
            "\n\nBuild output:\nStep 1/3 : FROM alpine\nStep 2/3 : RUN false"
        );
    }

    #[test]
    fn builder_selection_follows_the_daemon_advertised_default() {
        use bollard::query_parameters::BuilderVersion;
        assert_eq!(
            select_builder_version(None, Some("2")).unwrap(),
            BuilderVersion::BuilderBuildKit
        );
        assert_eq!(
            select_builder_version(None, Some("1")).unwrap(),
            BuilderVersion::BuilderV1
        );
    }

    #[test]
    fn builder_selection_falls_back_to_classic_when_the_daemon_advertises_nothing() {
        use bollard::query_parameters::BuilderVersion;
        assert_eq!(
            select_builder_version(None, None).unwrap(),
            BuilderVersion::BuilderV1
        );
    }

    #[test]
    fn docker_buildkit_env_var_overrides_the_daemon_advertised_default() {
        use bollard::query_parameters::BuilderVersion;
        // Forced off, even though the daemon advertises BuildKit…
        assert_eq!(
            select_builder_version(Some("0"), Some("2")).unwrap(),
            BuilderVersion::BuilderV1
        );
        assert_eq!(
            select_builder_version(Some("false"), Some("2")).unwrap(),
            BuilderVersion::BuilderV1
        );
        // …and forced on, even though it doesn't.
        assert_eq!(
            select_builder_version(Some("1"), Some("1")).unwrap(),
            BuilderVersion::BuilderBuildKit
        );
        assert_eq!(
            select_builder_version(Some("TRUE"), None).unwrap(),
            BuilderVersion::BuilderBuildKit
        );
    }

    #[test]
    fn an_unparseable_docker_buildkit_env_var_is_a_hard_error() {
        let err = select_builder_version(Some("banana"), Some("2")).unwrap_err();
        assert!(err.to_string().contains("'banana'"));
    }

    #[test]
    fn split_image_reference_separates_repo_and_tag() {
        assert_eq!(
            split_image_reference("myrepo/myimage:v2"),
            ("myrepo/myimage", Some("v2"))
        );
        assert_eq!(split_image_reference("myimage:v2"), ("myimage", Some("v2")));
    }

    #[test]
    fn split_image_reference_with_no_tag_has_none() {
        assert_eq!(
            split_image_reference("myrepo/myimage"),
            ("myrepo/myimage", None)
        );
    }

    #[test]
    fn split_image_reference_treats_a_registry_ports_colon_as_not_a_tag_separator() {
        assert_eq!(
            split_image_reference("localhost:5000/myimage"),
            ("localhost:5000/myimage", None)
        );
        assert_eq!(
            split_image_reference("localhost:5000/myimage:v2"),
            ("localhost:5000/myimage", Some("v2"))
        );
    }

    #[test]
    fn enable_buildkit_flag_forces_buildkit_regardless_of_the_real_env_var() {
        assert_eq!(
            docker_buildkit_env_value(true, Some("0")).as_deref(),
            Some("1"),
            "the flag must win even when the real env var explicitly forces the classic builder"
        );
        assert_eq!(docker_buildkit_env_value(true, None).as_deref(), Some("1"));
    }

    #[test]
    fn enable_buildkit_flag_off_defers_to_the_real_env_var() {
        assert_eq!(
            docker_buildkit_env_value(false, Some("0")).as_deref(),
            Some("0")
        );
        assert_eq!(docker_buildkit_env_value(false, None), None);
    }

    #[test]
    fn docker_context_id_matches_the_docker_cli_own_hashing() {
        // Verified against a real `~/.docker/contexts/meta/<id>` entry on
        // this machine: `printf 'orbstack' | shasum -a 256`.
        assert_eq!(
            docker_context_id("orbstack"),
            "2d89b732b01a00a2d1675ed3cee9fd0f965daadf90603c989dd3afd4569c6896"
        );
    }

    fn write_docker_context_meta(config_directory: &Path, context_name: &str, host: &str) {
        let id = docker_context_id(context_name);
        let meta_dir = config_directory.join("contexts").join("meta").join(&id);
        fs::create_dir_all(&meta_dir).unwrap();
        fs::write(
            meta_dir.join("meta.json"),
            format!(
                r#"{{"Name":"{context_name}","Metadata":{{}},"Endpoints":{{"docker":{{"Host":"{host}","SkipTLSVerify":false}}}}}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn docker_context_host_reads_the_endpoints_docker_host_field() {
        let config_directory = unique_temp_dir();
        write_docker_context_meta(
            &config_directory,
            "orbstack",
            "unix:///Users/kevin/.orbstack/run/docker.sock",
        );

        let host = docker_context_host(&config_directory, "orbstack").unwrap();
        assert_eq!(host, "unix:///Users/kevin/.orbstack/run/docker.sock");
    }

    #[test]
    fn docker_context_host_errors_clearly_when_the_context_does_not_exist() {
        let config_directory = unique_temp_dir();
        let err = docker_context_host(&config_directory, "no-such-context").unwrap_err();
        assert!(
            err.to_string()
                .contains("Docker context 'no-such-context' does not exist"),
            "{err}"
        );
    }

    #[test]
    fn active_docker_context_reads_current_context_from_config_json() {
        let config_directory = unique_temp_dir();
        fs::write(
            config_directory.join("config.json"),
            r#"{"currentContext":"orbstack"}"#,
        )
        .unwrap();

        assert_eq!(
            active_docker_context(&config_directory),
            Some("orbstack".to_string())
        );
    }

    #[test]
    fn active_docker_context_is_none_when_config_json_is_missing() {
        let config_directory = unique_temp_dir();
        assert_eq!(active_docker_context(&config_directory), None);
    }

    #[test]
    fn active_docker_context_is_none_when_current_context_is_unset_or_empty() {
        let config_directory = unique_temp_dir();
        fs::write(config_directory.join("config.json"), r#"{}"#).unwrap();
        assert_eq!(active_docker_context(&config_directory), None);

        fs::write(
            config_directory.join("config.json"),
            r#"{"currentContext":""}"#,
        )
        .unwrap();
        assert_eq!(active_docker_context(&config_directory), None);
    }

    #[test]
    fn resolve_context_name_prefers_an_explicit_context_over_everything_else() {
        let options = DockerConnectionOptions {
            host: None,
            context: Some("explicit".to_string()),
            config_directory: None,
            ..Default::default()
        };
        assert_eq!(
            resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
            Some("explicit".to_string())
        );
    }

    #[test]
    fn resolve_context_name_an_explicit_host_skips_context_resolution_entirely() {
        let options = DockerConnectionOptions {
            host: Some("tcp://1.2.3.4:2375".to_string()),
            context: None,
            config_directory: None,
            ..Default::default()
        };
        assert_eq!(
            resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
            None
        );
    }

    #[test]
    fn resolve_context_name_falls_back_to_the_env_var_then_the_active_context() {
        let options = DockerConnectionOptions::default();
        assert_eq!(
            resolve_context_name(&options, Some("env-context"), Some("active".to_string())),
            Some("env-context".to_string())
        );
        assert_eq!(
            resolve_context_name(&options, None, Some("active".to_string())),
            Some("active".to_string())
        );
        assert_eq!(resolve_context_name(&options, None, None), None);
    }

    #[test]
    fn resolve_context_name_treats_the_default_context_name_as_no_context() {
        let options = DockerConnectionOptions {
            host: None,
            context: Some("default".to_string()),
            config_directory: None,
            ..Default::default()
        };
        assert_eq!(resolve_context_name(&options, None, None), None);
    }

    #[test]
    fn connect_rejects_using_both_docker_context_and_docker_host() {
        let options = DockerConnectionOptions {
            host: Some("tcp://1.2.3.4:2375".to_string()),
            context: Some("some-context".to_string()),
            config_directory: None,
            ..Default::default()
        };
        let err = connect(&options).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cannot use both --docker-context and --docker-host."
        );
    }

    #[test]
    fn connect_via_an_explicit_context_uses_that_contexts_stored_host() {
        let config_directory = unique_temp_dir();
        // A `tcp://` address (unlike `unix://`) only builds a
        // lazily-connecting client (no handshake, no eager socket-existence
        // check) — see `await_log_follower_waits_for_the_spawned_task_to_finish`'s
        // own comment for the same property.
        write_docker_context_meta(&config_directory, "my-context", "tcp://1.2.3.4:2375");

        let options = DockerConnectionOptions {
            host: None,
            context: Some("my-context".to_string()),
            config_directory: Some(config_directory),
            ..Default::default()
        };
        connect(&options).expect("connecting via a valid context's stored host should succeed");
    }

    #[test]
    fn connect_via_an_explicit_context_errors_clearly_when_it_does_not_exist() {
        let config_directory = unique_temp_dir();
        let options = DockerConnectionOptions {
            host: None,
            context: Some("no-such-context".to_string()),
            config_directory: Some(config_directory),
            ..Default::default()
        };
        let err = connect(&options).unwrap_err();
        assert!(
            err.to_string()
                .contains("Docker context 'no-such-context' does not exist"),
            "{err}"
        );
    }

    #[test]
    fn conflicting_option_with_context_names_whichever_tls_option_was_given() {
        let base = DockerConnectionOptions {
            context: Some("some-context".to_string()),
            ..Default::default()
        };

        assert_eq!(
            conflicting_option_with_context(&DockerConnectionOptions {
                tls: true,
                ..base.clone()
            }),
            Some("--docker-tls")
        );
        assert_eq!(
            conflicting_option_with_context(&DockerConnectionOptions {
                tls_verify: true,
                ..base.clone()
            }),
            Some("--docker-tls-verify")
        );
        assert_eq!(
            conflicting_option_with_context(&DockerConnectionOptions {
                cert_path: Some(PathBuf::from("/tmp/certs")),
                ..base.clone()
            }),
            Some("--docker-cert-path")
        );
        assert_eq!(
            conflicting_option_with_context(&DockerConnectionOptions {
                tls_ca_cert: Some(PathBuf::from("/tmp/ca.pem")),
                ..base.clone()
            }),
            Some("--docker-tls-ca-cert")
        );
        assert_eq!(
            conflicting_option_with_context(&DockerConnectionOptions {
                tls_cert: Some(PathBuf::from("/tmp/cert.pem")),
                ..base.clone()
            }),
            Some("--docker-tls-cert")
        );
        assert_eq!(
            conflicting_option_with_context(&DockerConnectionOptions {
                tls_key: Some(PathBuf::from("/tmp/key.pem")),
                ..base.clone()
            }),
            Some("--docker-tls-key")
        );
        assert_eq!(conflicting_option_with_context(&base), None);
    }

    #[test]
    fn connect_rejects_docker_tls_flags_combined_with_docker_context() {
        for options in [
            DockerConnectionOptions {
                context: Some("some-context".to_string()),
                tls: true,
                ..Default::default()
            },
            DockerConnectionOptions {
                context: Some("some-context".to_string()),
                tls_verify: true,
                ..Default::default()
            },
        ] {
            let err = connect(&options).unwrap_err();
            assert!(
                err.to_string()
                    .starts_with("Cannot use both --docker-context and --docker-tls"),
                "{err}"
            );
        }
    }

    #[test]
    fn tls_enabled_is_true_for_either_flag_or_the_real_env_var() {
        let base = DockerConnectionOptions::default();
        assert!(!tls_enabled(&base, None));
        assert!(tls_enabled(
            &DockerConnectionOptions {
                tls: true,
                ..base.clone()
            },
            None
        ));
        assert!(tls_enabled(
            &DockerConnectionOptions {
                tls_verify: true,
                ..base.clone()
            },
            None
        ));
        assert!(tls_enabled(&base, Some("1")));
        assert!(tls_enabled(&base, Some("true")));
        assert!(tls_enabled(&base, Some("TRUE")));
        assert!(!tls_enabled(&base, Some("0")));
        assert!(!tls_enabled(&base, Some("false")));
    }

    #[test]
    fn docker_cert_directory_prefers_the_explicit_option_over_the_env_var_and_default() {
        let options = DockerConnectionOptions {
            cert_path: Some(PathBuf::from("/tmp/explicit-certs")),
            ..Default::default()
        };
        assert_eq!(
            docker_cert_directory(&options).unwrap(),
            PathBuf::from("/tmp/explicit-certs")
        );
    }

    /// A throwaway self-signed root CA + a leaf certificate/key it signed
    /// for `CN=localhost`, generated once for this test (`openssl req
    /// -x509 ...` then `openssl x509 -req ... -CA ca.pem -CAkey
    /// ca-key.pem`) — not a secret, not used anywhere real, just enough
    /// genuine X.509 structure for `rustls` to actually parse successfully,
    /// proving the connect-over-TLS wiring works with real certificate
    /// material rather than only unit-testing the file-path resolution
    /// around it.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIDEzCCAfugAwIBAgIUT8UTyaqqr/+/sYw1zmKn21bpAugwDQYJKoZIhvcNAQEL
BQAwGTEXMBUGA1UEAwwOcmF0ZWN0LXRlc3QtY2EwHhcNMjYwNzIxMDY0NjM0WhcN
MzYwNzE4MDY0NjM0WjAZMRcwFQYDVQQDDA5yYXRlY3QtdGVzdC1jYTCCASIwDQYJ
KoZIhvcNAQEBBQADggEPADCCAQoCggEBALtqfSeGbUj3c6s85ORTznbaEXFVV0Gy
CeVQOwCGBHkDSXOz3XUgg/GGwSD6mnUi88/1rgbAfIdX598ComfBSB7bKu61QlXr
DOaNiJl8Ef9KB0ORfxMr70vzjXkv5HPengDn8vaePJFKkU3Do6BNXqfPiBzCspgu
vHkWdVFhgO+sWaH4pZAUot1Lqy5s8YfmNhhbK8uqP5xtFkqbVS4vJmlvxP2tNdKj
aCqJDQfuQxxmDH2YFR0M5hoWN1VFFCMm0IvvPfAoKerm2smsNr1vQDZOS+WLfcEo
SgpPh7FeMoyOeW4KygsQVifEmilyEMao9xinIwFE5l5oiRnZNthGrG0CAwEAAaNT
MFEwHQYDVR0OBBYEFBMM7XcX6e6rxuj1rZtLGSs33Hb/MB8GA1UdIwQYMBaAFBMM
7XcX6e6rxuj1rZtLGSs33Hb/MA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQEL
BQADggEBAAe9gMLtnSWwCgwPAhxtPupyKbOxJGnyeJrhQMomqOohkgBgz/x4lT/Y
l0xq9ZytP3wwoWwWD8BBS478R1VzXN6djiPl0mpshOV0L9qBvZDJZipuxKYpDzMD
VSvFhXNzJCKI+w5XrGoyrvVB1bMMfiQIKYEK/+/+cOYMOQlx34I22f44Gbks1mSs
sebU2RAkavTyPQ2BXGIfTvXvWtDCxtMMjRRi0/v0irRM+Yb58kdKPb5aBp9Qolbb
PacA5Q4qmco2RbhNmDxR1i/n2JJZG3YUvEuDqfRx9KO3I9ceqfEKsIwNcCUxRu9d
QxOQmeKH+itZ7e+OXYE0bUN5gTJoZkg=
-----END CERTIFICATE-----
";

    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIC/TCCAeWgAwIBAgIUQaqli6TKr1bWzqbd8O1px0IjMaIwDQYJKoZIhvcNAQEL
BQAwGTEXMBUGA1UEAwwOcmF0ZWN0LXRlc3QtY2EwHhcNMjYwNzIxMDY0NjM0WhcN
MzYwNzE4MDY0NjM0WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3
DQEBAQUAA4IBDwAwggEKAoIBAQCZoT1Mj91mXjMxZMM86rPo3CW0x5RBYCvB9dtq
dFwRoAy8sD23DFML8SqzHYM7cdIAyu6kpF5EzcqWVsc4Fs5zA7ce+BO7OYNl0SWB
UZq1Ft3Fl6YSSUESh/1WgS0mk90QlsZuMO8PRJYVJu7Fr4qmF3PdyEPwl9fVoG9B
YzcyYiYOsfKN7+dI9GGUu7Cy8vynwf2dWnOs+ovEQmTdLDq71mUicm00Vf0M9NZH
lUjuZ5yrNko4J+IhSOBM0vi9GPt4QhwG0B0eOdVlYC0plPVlAUzwKzHIVKwfxNau
AUHIBUprFPWFExzSa/4FPgIx/7qnA8UqQUz2/CHaGppk22QjAgMBAAGjQjBAMB0G
A1UdDgQWBBSfp2ZFjLS/hp/EH6TP9758NTG6pTAfBgNVHSMEGDAWgBQTDO13F+nu
q8bo9a2bSxkrN9x2/zANBgkqhkiG9w0BAQsFAAOCAQEAW1usCCQL57j84BYJLeXg
QS2Zo1nw1jSa2VmcmBNlzYqirKKScadZf+ZgAngaAxjfY9b3S2RGd5o4rkYRsiRs
ZMqWOxoicGjPujcX4k02Gae571Rgjx6BphcfhgW+xLes1llTBIkIkIeqRdaijlal
e25YrmEV+Eahc9eE7G6qBy+GvO4HlP6gUtnv/3I41hE0h7l/ojdSCLPb2LXWWukO
GZTjaGdnRUiODDkHzXcdJmID1vXf07JoQ6pkBP/zmECln03WqPJ/onXnJGLjVho9
oWxosQDqBSCQRIRbZ34PGjY+mPMoyLdWnzdwj1cPXmkMtU8HmhY1LawlW0/ye7GH
UQ==
-----END CERTIFICATE-----
";

    const TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCZoT1Mj91mXjMx
ZMM86rPo3CW0x5RBYCvB9dtqdFwRoAy8sD23DFML8SqzHYM7cdIAyu6kpF5EzcqW
Vsc4Fs5zA7ce+BO7OYNl0SWBUZq1Ft3Fl6YSSUESh/1WgS0mk90QlsZuMO8PRJYV
Ju7Fr4qmF3PdyEPwl9fVoG9BYzcyYiYOsfKN7+dI9GGUu7Cy8vynwf2dWnOs+ovE
QmTdLDq71mUicm00Vf0M9NZHlUjuZ5yrNko4J+IhSOBM0vi9GPt4QhwG0B0eOdVl
YC0plPVlAUzwKzHIVKwfxNauAUHIBUprFPWFExzSa/4FPgIx/7qnA8UqQUz2/CHa
Gppk22QjAgMBAAECggEAAX5Et0LKtx0BSGCfWS860m+ZWjl6YmxJ4JfAKze4UV+J
4CeiYe4XvIz6ikUmKmS/0swmJ6mFVQvfBTkQtKXcGdgWZpGot3Amq82tnKUraMkx
HKONtK3LmR+DQdz9kFttkaS1hwqouDBFeS0osvky0sx1jtlMd8EyEtx9WFhbh/zS
YEpmqI17rvchL8R31yRa8PSvj4K3yXcXW+3/orK7QFsXxrsKopsvkhQ/zvdXeKTy
L6+dsGr/Ou+UDduzJzaU23PTfVTHxwZixdnAlMALmUWHMDYyMRhX2saEfXJOAnOO
nzcTsyfJpSLVuVukGfGRgaUw/mI7LDwD1oJh89XGiQKBgQDNqcMb1VDQuT6Hd8tN
GrPNaEXXBxqfaLnxmL35N52567L/JT9IJ2XXiwBO1xW/YqpVBKREXiu0qTifrA3o
90g7BGmekubACzaOOfE20pV8R0uliAxsmE3Ghq8GqLMbnz4fVeIRQ0fClwkBgJxJ
SjiBAKJvDKbGLdWA2cROpb8yDQKBgQC/OzoC8m+SKU4a25Iam8j/TH7BopiGoSLu
9YVmJGeKbFUty9deQnXnG/L3enwle38OVEOBN1knd70RB8A8A8/rcMICAt+subqe
SI9XZM/pHUG5/3nV8yCjPU5fGxboXgA00c2mrObCwJ6Fe6TAtUYKJEqS+Io6PZ8q
PCEPx0HS7wKBgAwShgBxQiAub4w2LPnmsl1BXLAlm5t140xaQfSKHjkWq9gsUI2k
umavoyH9oCou2X7KGfZlbL1bHZbJ27ssINJODQEg8Giff+FTZ2RnchzsdnVOCiSp
wA8CQu3qIzFg5J2kRfPrdh/nC8FJ0mK+95gi+GX6YSPK9vhsUAip1BJVAoGAByhb
WoLihDEBmGXBiTdthYjCcdL5LIjZeuI7tQAF1BuL8KPhksigCx9zr6mo/eoqbknf
IPYGY0DLFdkZa+WkoaZdzJ946ckl4AjNPLMsSQhsTl7um4B3J0UDKvIjoFzsWw3D
ScrM9FsrU8m19/SRA44qMGgXHGj0DSuk/SczIocCgYAKhiT2cat4BQ19idFeSRi3
4wbJAkE0ZUm7xwe0rNHVvAqEV5Qm09pYA+n6OWwPiq/b+m4gYpmFg67PbuGPRANl
jyR1S/cfb1f9ezO3/qrhVPqZdpMrGKmHLiLkLt2SGoG53O06CH6yOH9tyadttrxs
pbgnkuidZ5WU2LfAJCLZOQ==
-----END PRIVATE KEY-----
";

    fn write_tls_materials(dir: &Path) {
        fs::write(dir.join("ca.pem"), TEST_CA_PEM).unwrap();
        fs::write(dir.join("cert.pem"), TEST_CERT_PEM).unwrap();
        fs::write(dir.join("key.pem"), TEST_KEY_PEM).unwrap();
    }

    #[test]
    fn connect_over_tls_with_valid_materials_builds_a_client_lazily() {
        let cert_directory = unique_temp_dir();
        write_tls_materials(&cert_directory);

        let options = DockerConnectionOptions {
            host: Some("tcp://127.0.0.1:2376".to_string()),
            tls_verify: true,
            cert_path: Some(cert_directory),
            ..Default::default()
        };

        // Like `connect_with_host`, `connect_with_ssl` only parses the
        // certificates and builds the client — no handshake happens until
        // an actual request is made, so this doesn't need a live TLS
        // listener.
        connect(&options)
            .expect("connecting over TLS with valid certificate material should succeed");
    }

    #[test]
    fn connect_over_tls_errors_clearly_when_a_certificate_file_is_missing() {
        let cert_directory = unique_temp_dir();
        // No certificate files written.

        let options = DockerConnectionOptions {
            host: Some("tcp://127.0.0.1:2376".to_string()),
            tls: true,
            cert_path: Some(cert_directory),
            ..Default::default()
        };

        let err = connect(&options).unwrap_err();
        assert!(err.to_string().contains("over TLS"), "{err}");
    }

    #[test]
    fn require_host_for_tls_errors_clearly_when_no_host_resolved() {
        let err = require_host_for_tls(None).unwrap_err();
        assert!(
            err.to_string()
                .contains("--docker-tls/--docker-tls-verify requires --docker-host"),
            "{err}"
        );
    }

    #[test]
    fn require_host_for_tls_passes_through_a_resolved_host() {
        assert_eq!(
            require_host_for_tls(Some("tcp://1.2.3.4:2376".to_string())).unwrap(),
            "tcp://1.2.3.4:2376"
        );
    }

    #[test]
    fn resolve_host_prefers_the_explicit_option_then_the_injected_env_value() {
        let options = DockerConnectionOptions {
            host: Some("tcp://explicit:2375".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_host(&options, Some("tcp://from-env:2375")),
            Some("tcp://explicit:2375".to_string())
        );

        let options = DockerConnectionOptions::default();
        assert_eq!(
            resolve_host(&options, Some("tcp://from-env:2375")),
            Some("tcp://from-env:2375".to_string())
        );
        assert_eq!(resolve_host(&options, None), None);
    }

    #[test]
    fn should_use_tty_requires_both_stdin_and_stdout_to_be_real_terminals() {
        assert!(should_use_tty(true, true, true));
    }

    #[test]
    fn should_use_tty_is_false_when_not_interactive_eligible() {
        assert!(!should_use_tty(false, true, true));
    }

    #[test]
    fn should_use_tty_is_false_when_stdin_is_not_a_terminal() {
        assert!(!should_use_tty(true, false, true));
    }

    #[test]
    fn should_use_tty_is_false_when_stdout_is_not_a_terminal() {
        assert!(!should_use_tty(true, true, false));
    }

    fn user_mapping_fixture() -> UserMapping {
        UserMapping {
            user: crate::user::CurrentUser {
                uid: 1000,
                gid: 1000,
                username: "ratect".to_string(),
                groupname: "ratect".to_string(),
            },
            home_directory: "/home/ratect".to_string(),
        }
    }

    fn tar_entry_contents(tar_bytes: &[u8], path: &str) -> String {
        let mut archive = tar::Archive::new(tar_bytes);
        let mut entry = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap())
            .find(|e| e.path().unwrap().to_string_lossy() == path)
            .unwrap_or_else(|| panic!("no {path:?} entry found"));
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
        contents
    }

    #[test]
    fn build_user_mapping_tar_includes_passwd_shadow_and_group() {
        let mapping = user_mapping_fixture();
        let tar_bytes = build_user_mapping_tar(&mapping).unwrap();
        let entries = tar_entry_paths(&tar_bytes);

        assert_eq!(entries, vec!["passwd", "shadow", "group"]);
        assert_eq!(
            tar_entry_contents(&tar_bytes, "passwd"),
            crate::user::generate_passwd_file(&mapping.user, &mapping.home_directory)
        );
        assert_eq!(
            tar_entry_contents(&tar_bytes, "shadow"),
            crate::user::generate_shadow_file(&mapping.user)
        );
        assert_eq!(
            tar_entry_contents(&tar_bytes, "group"),
            crate::user::generate_group_file(&mapping.user)
        );
    }

    #[test]
    fn build_user_mapping_tar_entries_are_root_owned_with_correct_modes() {
        let tar_bytes = build_user_mapping_tar(&user_mapping_fixture()).unwrap();
        let mut archive = tar::Archive::new(tar_bytes.as_slice());
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.gid().unwrap(), 0);
            let expected_mode = match entry.path().unwrap().to_string_lossy().as_ref() {
                "shadow" => 0o640,
                _ => 0o644,
            };
            assert_eq!(header.mode().unwrap(), expected_mode);
        }
    }

    #[test]
    fn build_home_directory_tar_creates_a_directory_entry_owned_by_the_mapped_user() {
        let tar_bytes = build_home_directory_tar(&user_mapping_fixture()).unwrap();
        let mut archive = tar::Archive::new(tar_bytes.as_slice());
        let mut entries = archive.entries().unwrap().map(|e| e.unwrap());
        let entry = entries.next().unwrap();

        assert_eq!(entry.path().unwrap().to_string_lossy(), "ratect/");
        assert_eq!(entry.header().entry_type(), tar::EntryType::Directory);
        assert_eq!(entry.header().uid().unwrap(), 1000);
        assert_eq!(entry.header().gid().unwrap(), 1000);
        assert_eq!(entry.header().mode().unwrap(), 0o755);
        assert!(entries.next().is_none());
    }

    #[test]
    fn home_directory_parent_is_the_directory_above_the_leaf() {
        assert_eq!(home_directory_parent("/home/ratect"), "/home");
    }

    #[test]
    fn home_directory_parent_is_root_for_a_top_level_home_directory() {
        assert_eq!(home_directory_parent("/ratect"), "/");
    }

    #[test]
    fn ensure_host_volume_directories_exist_creates_a_missing_directory() {
        let dir = unique_temp_dir();
        let host_path = dir.join("missing");
        let volumes = vec![format!("{}:/code", host_path.display())];

        assert!(!host_path.exists());
        ensure_host_volume_directories_exist(Some(&volumes)).unwrap();
        assert!(host_path.is_dir());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ensure_host_volume_directories_exist_leaves_an_existing_directory_alone() {
        let dir = unique_temp_dir();
        let volumes = vec![format!("{}:/code", dir.display())];

        ensure_host_volume_directories_exist(Some(&volumes)).unwrap();
        assert!(dir.is_dir());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ensure_host_volume_directories_exist_does_nothing_when_there_are_no_volumes() {
        ensure_host_volume_directories_exist(None).unwrap();
    }

    #[test]
    fn ensure_host_volume_directories_exist_errors_clearly_when_a_file_blocks_the_path() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let host_path = dir.join("blocked");
        fs::write(&host_path, "not a directory").unwrap();
        let volumes = vec![format!("{}:/code", host_path.display())];

        let result = ensure_host_volume_directories_exist(Some(&volumes));

        assert!(result.is_err());
        assert!(host_path.is_file(), "the file must be left untouched");

        fs::remove_dir_all(&dir).unwrap();
    }
}
