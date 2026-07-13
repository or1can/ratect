use anyhow::{Context, Result};
use bollard::container::AttachContainerResults;
use bollard::models::{
    ContainerCreateBody as Config, EndpointSettings, NetworkConnectRequest, NetworkCreateRequest,
    PortBinding, PortMap,
};
use bollard::query_parameters::AttachContainerOptionsBuilder;
use bollard::query_parameters::BuildImageOptionsBuilder;
use bollard::query_parameters::CreateImageOptions;
use bollard::query_parameters::LogsOptions;
use bollard::query_parameters::ResizeContainerTTYOptionsBuilder;
use bollard::query_parameters::UploadToContainerOptionsBuilder;
use bollard::query_parameters::WaitContainerOptions;
use bollard::service::HostConfig;
use bollard::Docker;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
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

/// Builds the Docker `cmd` array for a task's container, folding in any
/// `-- ADDITIONAL_ARGS` from the CLI.
///
/// When `command` is set, `sh -c '<command>' sh arg1 arg2 ...` is used
/// instead of string concatenation, so the args become the shell's
/// positional parameters (`$1`, `$2`, `$@`) inside `command` rather than
/// being re-parsed as shell syntax — safe regardless of what characters
/// they contain. The `sh` immediately after the command string fills the
/// `$0` slot; it's conventional and not otherwise meaningful.
///
/// When `command` is unset, non-empty `additional_args` are passed directly
/// as argv, letting the image's own entrypoint receive them (matching plain
/// `docker run <image> <args>`).
fn build_cmd(command: Option<&str>, additional_args: &[String]) -> Option<Vec<String>> {
    match (command, additional_args.is_empty()) {
        (Some(c), true) => Some(vec!["sh".to_string(), "-c".to_string(), c.to_string()]),
        (Some(c), false) => {
            let mut cmd = vec![
                "sh".to_string(),
                "-c".to_string(),
                c.to_string(),
                "sh".to_string(),
            ];
            cmd.extend(additional_args.iter().cloned());
            Some(cmd)
        }
        (None, true) => None,
        (None, false) => Some(additional_args.to_vec()),
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

/// Per-container network-facing options shared by `run_container` and
/// `start_background_container` — bundled together (rather than three more
/// flat parameters) since both methods were already at
/// `#[allow(clippy::too_many_arguments)]` before this.
pub struct NetworkOptions<'a> {
    /// Extra network aliases beyond the container's own name.
    pub additional_hostnames: Option<&'a Vec<String>>,
    /// Extra `/etc/hosts` entries (hostname -> IP).
    pub additional_hosts: Option<&'a HashMap<String, String>>,
    /// Container ports to publish to the host — see `parse_port_mapping`
    /// for the supported format. Already filtered to `None` by the caller
    /// when `--disable-ports` is set, regardless of what `ports` config
    /// exists — this struct doesn't know about that flag itself.
    pub ports: Option<&'a Vec<String>>,
}

/// Parses one `ports` entry (`"local:container[/protocol]"`, protocol
/// defaulting to `tcp`) into `(local_port, container_port, protocol)`. Only
/// single ports are supported — no ranges (`"8000-8002:9000-9002"`) and no
/// object form, unlike Batect's full `PortMapping`/`PortRange`; same
/// simplification precedent as `volumes`' string-only form. Not called
/// until a `ports` entry is actually applied (matching `volumes`, which
/// isn't format-checked at config-parse time either).
fn parse_port_mapping(spec: &str) -> Result<(u16, u16, String)> {
    let (local, rest) = spec.split_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Port mapping '{spec}' is invalid. It must be in the form 'local:container' or \
             'local:container/protocol'."
        )
    })?;
    let (container, protocol) = match rest.split_once('/') {
        Some((container, protocol)) => (container, protocol),
        None => (rest, "tcp"),
    };

    let invalid = || {
        anyhow::anyhow!(
            "Port mapping '{spec}' is invalid. Both the local and container ports must be \
             positive integers."
        )
    };
    let local_port: u16 = local.parse().map_err(|_| invalid())?;
    let container_port: u16 = container.parse().map_err(|_| invalid())?;
    if local_port == 0 || container_port == 0 {
        return Err(invalid());
    }

    Ok((local_port, container_port, protocol.to_string()))
}

/// Builds Docker's `Config.exposed_ports` + `HostConfig.port_bindings` from
/// a config `ports` list — pure, unit-testable without a daemon. `None`
/// when `ports` itself is `None` (absent, or already filtered out by
/// `--disable-ports` — see `NetworkOptions::ports`).
fn build_port_config(ports: Option<&Vec<String>>) -> Result<Option<(Vec<String>, PortMap)>> {
    let Some(ports) = ports else {
        return Ok(None);
    };
    if ports.is_empty() {
        return Ok(None);
    }

    let mut exposed_ports = Vec::new();
    let mut port_bindings = PortMap::new();
    for spec in ports {
        let (local_port, container_port, protocol) = parse_port_mapping(spec)?;
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

    Ok(Some((exposed_ports, port_bindings)))
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
/// matching rules. `Dockerfile` and `.dockerignore` themselves are always
/// included regardless of exclusion patterns, mirroring Docker's own
/// special-casing (otherwise a broad `*` pattern would exclude the file the
/// build needs).
///
/// Known simplifications: symlinks are silently skipped (rare in build
/// contexts; proper support needs tar symlink entries, not just file
/// copies), and empty directories aren't preserved as their own tar
/// entries (only added implicitly as the parent of a file within them).
fn build_context_tar(build_directory: &Path) -> Result<Vec<u8>> {
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
        let force_include = relative_path == "Dockerfile" || relative_path == ".dockerignore";
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
        if !path.exists() {
            fs::create_dir_all(path)
                .with_context(|| format!("Failed to create host directory {:?}", path))?;
        }
    }

    Ok(())
}

/// Abstracts the container operations the task engine needs, so tests can
/// inject a fake implementation instead of talking to a real Docker daemon.
#[async_trait::async_trait]
pub trait ContainerRuntime {
    async fn pull_image(&self, image: &str) -> Result<()>;

    /// Builds an image from `build_directory` (already resolved to an
    /// absolute path), tagging it as `tag`. The Dockerfile is always named
    /// `Dockerfile`, at `build_directory`'s own root. `build_args` are
    /// passed through as Docker's own `--build-arg` mechanism.
    ///
    /// Returns the built image's ID (e.g. `sha256:...`), not `tag` — `tag` is
    /// applied so the image is identifiable in `docker images`, but isn't
    /// guaranteed unique (see `TaskEngine::resolve_image`), so callers must
    /// use the returned ID, not `tag`, to reliably reference the image this
    /// call just built.
    async fn build_image(
        &self,
        build_directory: &Path,
        build_args: Option<&HashMap<String, String>>,
        tag: &str,
    ) -> Result<String>;

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
    /// `user_mapping` is `Some` when this container's own `run_as_current_user`
    /// is enabled (independent of whether the task's own container has it
    /// enabled — see `TaskEngine::resolve_user_mapping`) — see `run_container`'s
    /// doc comment for what applying it actually does.
    ///
    /// `network_options` carries this container's own `additional_hostnames`/
    /// `additional_hosts` — see `run_container`'s doc comment.
    #[allow(clippy::too_many_arguments)]
    async fn start_background_container(
        &self,
        alias: &str,
        image: &str,
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
    ) -> Result<String>;

    /// Stops and removes a container started with [`start_background_container`](Self::start_background_container).
    async fn stop_and_remove_container(&self, container_id: &str) -> Result<()>;

    /// Runs a container to completion, streaming its logs to stdout, then
    /// removes it. `name` is this container's own network alias (used when
    /// `network` is set); used for a task's own container.
    ///
    /// `additional_args` become positional parameters (`$1`, `$2`, ... `$@`)
    /// within `command` (which always runs via `sh -c`) rather than being
    /// concatenated into the command string — so they're never re-parsed as
    /// shell syntax, regardless of what characters they contain. If `command`
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

        // Best-effort: syncs the container's TTY to the local terminal's
        // size once, at attach time. Not tracked live if the local terminal
        // is resized mid-session — see the 0.4.0 plan's known gaps.
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            let resize_options = ResizeContainerTTYOptionsBuilder::default()
                .w(cols as i32)
                .h(rows as i32)
                .build();
            if let Err(e) = self
                .docker
                .resize_container_tty(container_id, resize_options)
                .await
            {
                tracing::warn!(container_id, error = ?e, "Failed to resize container TTY");
            }
        }

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
        output_result?;

        self.exit_code(container_id).await
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

    async fn build_image(
        &self,
        build_directory: &Path,
        build_args: Option<&HashMap<String, String>>,
        tag: &str,
    ) -> Result<String> {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message(format!("Building image {}...", tag));
        pb.enable_steady_tick(Duration::from_millis(100));

        let build_directory = build_directory.to_path_buf();
        let tar_bytes = tokio::task::spawn_blocking(move || build_context_tar(&build_directory))
            .await
            .context("Failed to build the Docker build context")??;

        let mut options_builder = BuildImageOptionsBuilder::default()
            .dockerfile("Dockerfile")
            .t(tag)
            .rm(true);
        if let Some(build_args) = build_args {
            options_builder = options_builder.buildargs(build_args);
        }
        let options = options_builder.build();

        let mut stream =
            self.docker
                .build_image(options, None, Some(bollard::body_full(tar_bytes.into())));

        let mut image_id = None;
        // The full build transcript, so a failure's error carries everything
        // that led up to it (not just Docker's own one-line summary) — the
        // only other place this streamed output goes is the ephemeral
        // spinner message below, which is gone the instant the next line
        // arrives and never rendered at all on a non-TTY (CI, redirected
        // output).
        let mut output = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(message) = info.error_detail.and_then(|detail| detail.message) {
                        pb.finish_with_message(format!("Failed to build image {}", tag));
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
                            pb.set_message(format!("{}: {}", tag, trimmed));
                        }
                    }
                    if let Some(id) = info.aux.and_then(|aux| aux.id) {
                        image_id = Some(id);
                    }
                }
                Err(e) => {
                    pb.finish_with_message(format!("Failed to build image {}", tag));
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

        pb.finish_with_message(format!("Image {} built successfully", tag));
        Ok(image_id)
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
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
    ) -> Result<String> {
        if user_mapping.is_some() {
            ensure_host_volume_directories_exist(volumes)?;
        }
        let port_config = build_port_config(network_options.ports)?;

        let host_config = HostConfig {
            binds: volumes.cloned(),
            extra_hosts: build_extra_hosts(network_options.additional_hosts),
            port_bindings: port_config.as_ref().map(|(_, bindings)| bindings.clone()),
            ..Default::default()
        };

        let config = Config {
            hostname: Some(alias.to_string()),
            image: Some(image.to_string()),
            env: build_env(environment),
            exposed_ports: port_config.as_ref().map(|(exposed, _)| exposed.clone()),
            user: user_mapping.map(|m| format!("{}:{}", m.user.uid, m.user.gid)),
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
        additional_args: &[String],
        volumes: Option<&Vec<String>>,
        environment: Option<&HashMap<String, String>>,
        network: &str,
        interactive: bool,
        user_mapping: Option<&UserMapping>,
        network_options: &NetworkOptions,
    ) -> Result<()> {
        let use_tty = should_use_tty(
            interactive,
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        );

        if user_mapping.is_some() {
            ensure_host_volume_directories_exist(volumes)?;
        }
        let port_config = build_port_config(network_options.ports)?;

        let host_config = HostConfig {
            binds: volumes.cloned(),
            extra_hosts: build_extra_hosts(network_options.additional_hosts),
            port_bindings: port_config.as_ref().map(|(_, bindings)| bindings.clone()),
            ..Default::default()
        };

        let config = Config {
            hostname: Some(name.to_string()),
            image: Some(image.to_string()),
            cmd: build_cmd(command, additional_args),
            env: build_env(environment),
            exposed_ports: port_config.as_ref().map(|(exposed, _)| exposed.clone()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: use_tty.then_some(true),
            open_stdin: use_tty.then_some(true),
            attach_stdin: use_tty.then_some(true),
            stdin_once: use_tty.then_some(true),
            user: user_mapping.map(|m| format!("{}:{}", m.user.uid, m.user.gid)),
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
        } else {
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

            self.exit_code(&container.id).await?
        };

        self.docker.remove_container(&container.id, None).await?;
        tracing::debug!(container_id = %container.id, exit_code, "removed container");

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
    fn parse_port_mapping_without_protocol_defaults_to_tcp() {
        assert_eq!(
            parse_port_mapping("8080:80").unwrap(),
            (8080, 80, "tcp".to_string())
        );
    }

    #[test]
    fn parse_port_mapping_with_protocol_is_honored() {
        assert_eq!(
            parse_port_mapping("8080:80/udp").unwrap(),
            (8080, 80, "udp".to_string())
        );
    }

    #[test]
    fn parse_port_mapping_without_a_colon_is_an_error() {
        assert!(parse_port_mapping("8080").is_err());
    }

    #[test]
    fn parse_port_mapping_with_a_non_numeric_port_is_an_error() {
        assert!(parse_port_mapping("abc:80").is_err());
        assert!(parse_port_mapping("8080:abc").is_err());
    }

    #[test]
    fn parse_port_mapping_with_a_zero_port_is_an_error() {
        assert!(parse_port_mapping("0:80").is_err());
        assert!(parse_port_mapping("8080:0").is_err());
    }

    #[test]
    fn build_port_config_is_none_when_ports_is_absent() {
        assert!(build_port_config(None).unwrap().is_none());
    }

    #[test]
    fn build_port_config_is_none_when_ports_is_empty() {
        assert!(build_port_config(Some(&vec![])).unwrap().is_none());
    }

    #[test]
    fn build_port_config_builds_exposed_ports_and_bindings() {
        let ports = vec!["8080:80".to_string(), "9000:9000/udp".to_string()];
        let (exposed, bindings) = build_port_config(Some(&ports)).unwrap().unwrap();

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
    fn build_port_config_propagates_a_malformed_entry_as_an_error() {
        assert!(build_port_config(Some(&vec!["not-valid".to_string()]))
            .unwrap_err()
            .to_string()
            .contains("not-valid"));
    }

    #[test]
    fn build_cmd_with_command_and_no_additional_args_runs_it_via_sh_c() {
        let cmd = build_cmd(Some("echo hi"), &[]);
        assert_eq!(
            cmd,
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ])
        );
    }

    #[test]
    fn build_cmd_with_command_and_additional_args_passes_them_as_positional_parameters() {
        let additional_args = vec!["arg1".to_string(), "arg2".to_string()];
        let cmd = build_cmd(Some("echo $1 $2"), &additional_args);
        assert_eq!(
            cmd,
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo $1 $2".to_string(),
                "sh".to_string(),
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
        assert_eq!(build_cmd(None, &[]), None);
    }

    #[test]
    fn build_cmd_with_no_command_and_additional_args_passes_them_directly_as_argv() {
        let additional_args = vec!["migrate".to_string(), "--up".to_string()];
        let cmd = build_cmd(None, &additional_args);
        assert_eq!(cmd, Some(vec!["migrate".to_string(), "--up".to_string()]));
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

        let tar_bytes = build_context_tar(&dir).unwrap();
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

        let tar_bytes = build_context_tar(&dir).unwrap();
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

        let tar_bytes = build_context_tar(&dir).unwrap();
        let entries = tar_entry_paths(&tar_bytes);

        assert!(entries.contains(&"Dockerfile".to_string()));
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

        let tar_bytes = build_context_tar(&dir).unwrap();
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
}
