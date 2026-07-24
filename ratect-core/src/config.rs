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

use anyhow::{Context, Result};
use path_clean::PathClean;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Batect's one built-in config variable, resolvable via `<batect.project_directory`/
/// `<{batect.project_directory}` without being declared in `config_variables` — always
/// the absolute path of the directory containing the config file. See
/// [`Config::resolve_expressions_with`].
const PROJECT_DIRECTORY_VAR: &str = "batect.project_directory";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project_name: String,
    pub containers: HashMap<String, Container>,
    pub tasks: HashMap<String, Task>,
    pub config_variables: Option<HashMap<String, ConfigVariable>>,
    /// Recognized but inert — Ratect collects no telemetry, so there's
    /// nothing to forbid. Accepted purely so a real Batect config using it
    /// doesn't fail to load under [`Config`]'s `deny_unknown_fields`, the
    /// same "no effect" treatment already given `--upgrade`/
    /// `--no-update-notification`/`--no-wrapper-cache-cleanup`.
    pub forbid_telemetry: Option<bool>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    /// The image to run, in Docker's own `name:tag` form. Exactly one of
    /// `image` or `build_directory` is required.
    pub image: Option<String>,
    /// Controls whether an `image` container's image is pulled fresh or
    /// only when missing locally — Docker's own pull semantics
    /// ([`TaskEngine::resolve_pulled_image`](crate::engine::TaskEngine)).
    /// On a `build_directory` container, this instead controls whether the
    /// build's own base image is force-pulled before building (`docker
    /// build --pull`) — `Always` forces it, `IfNotPresent` leaves Docker's
    /// own local-cache-if-present build behavior alone — matching Batect's
    /// second, distinct use of this same field
    /// ([`TaskEngine::resolve_image`](crate::engine::TaskEngine)). `None`
    /// defaults to [`ImagePullPolicy::IfNotPresent`], matching Batect's own
    /// default, for either use.
    pub image_pull_policy: Option<ImagePullPolicy>,
    /// The directory containing the `Dockerfile` to build an image from,
    /// resolved relative to the directory of the file declaring it.
    /// Supports expressions. Exactly one of `image` or `build_directory` is
    /// required.
    pub build_directory: Option<String>,
    /// Build arguments (Docker's own `--build-arg`) for a
    /// `build_directory` build, matched to the Dockerfile's own `ARG`
    /// instructions. Values support expressions. Ignored for an `image`
    /// container.
    pub build_args: Option<HashMap<String, String>>,
    /// The Dockerfile to build, as a path relative to `build_directory`'s
    /// own root — Batect's `dockerfile` field. Defaults to `"Dockerfile"`
    /// at `build_directory`'s root when omitted, matching Batect and
    /// Docker's own default. A plain string, not an
    /// [expression](#expressions) — matching Batect's own `String` (not
    /// `Expression`) typing for this field, unlike `build_directory`
    /// itself. Only meaningful alongside `build_directory`; like
    /// `build_args`, silently ignored for an `image` container (see
    /// `TaskEngine::resolve_image`).
    pub dockerfile: Option<String>,
    /// The build stage to stop at, for a multi-stage `FROM ... AS <name>`
    /// Dockerfile — Docker's own `--target` build option, and Batect's
    /// `build_target` field. A plain string, not an
    /// [expression](#expressions) — same reasoning as `dockerfile`. Only
    /// meaningful alongside `build_directory`; silently ignored for an
    /// `image` container, same as `dockerfile`/`build_args`.
    pub build_target: Option<String>,
    /// Exposes secrets to a `build_directory` build via BuildKit's
    /// secret-mount mechanism (a Dockerfile's `RUN
    /// --mount=type=secret,id=<key>`), without persisting them into the
    /// built image's layers — keyed by the `id` such a `RUN` instruction
    /// references. A [`BuildSecret::Path`]'s value supports
    /// [expressions](#expressions) and is resolved the same way as
    /// `build_directory`; a [`BuildSecret::Environment`]'s value is a
    /// literal host environment variable *name*, not itself an expression
    /// — matching Batect's own typing for both. Only meaningful alongside
    /// `build_directory`, same as `dockerfile`/`build_target`/`build_args`.
    pub build_secrets: Option<HashMap<String, BuildSecret>>,
    /// Forwards an SSH agent from the host into a `build_directory` build,
    /// for a Dockerfile's `RUN --mount=type=ssh` instructions — Batect's
    /// `build_ssh` field. **Ratect only supports forwarding the host's
    /// running ssh-agent (via its `SSH_AUTH_SOCK`) under the implicit
    /// `default` agent id** — at most one entry, and if given, its `id`
    /// (if set) must be `"default"` and its `paths` must be empty (checked
    /// in [`Config::resolve_expressions_with`]). Batect additionally
    /// supports multiple named agents and forwarding explicit private key
    /// files instead of a running agent; the underlying Docker client this
    /// is built on doesn't expose either — see
    /// [Differences from Batect](https://github.com/or1can/ratect/blob/main/docs/differences-from-batect.md#container-fields).
    pub build_ssh: Option<Vec<SshAgent>>,
    /// Host bind mounts (`local`) and/or named cache volumes (`cache`) — see
    /// [`VolumeMount`]. A `local` mount's host path is resolved in
    /// [`Config::resolve_expressions_with`]; a `cache` mount's Docker volume
    /// name/host directory is resolved later, once `--cache-type` and the
    /// project's own cache key are known — see [`crate::cache`].
    pub volumes: Option<Vec<VolumeMount>>,
    /// Other containers that must be started and ready before this one
    /// starts — see also a task's own `dependencies`, which apply to one
    /// task only.
    pub dependencies: Option<Vec<String>>,
    /// Environment variables to set inside the container. Values support
    /// expressions, and a non-string scalar (`1`, `true`) is coerced to its
    /// string form, matching Batect.
    #[serde(default, deserialize_with = "deserialize_scalar_string_map")]
    pub environment: Option<HashMap<String, String>>,
    /// Runs the container as the host's own user rather than the image's
    /// default, so files it writes to a mounted volume aren't root-owned.
    pub run_as_current_user: Option<RunAsCurrentUser>,
    /// Extra network aliases this container is reachable by, beyond its own
    /// name. Plain strings, no [expression](#expressions) support — matching
    /// Batect, which types this as `Set<String>`, not `Set<Expression>`.
    pub additional_hostnames: Option<Vec<String>>,
    /// Extra `/etc/hosts` entries (hostname -> IP), Docker's own
    /// `--add-host` mechanism. Plain strings, no expression support — same
    /// reasoning as `additional_hostnames`.
    pub additional_hosts: Option<HashMap<String, String>>,
    /// Publishes container ports to the host. Accepts both of Batect's
    /// forms — a `"local:container[/protocol]"` string (with port ranges,
    /// `"from-to:from-to[/protocol]"`) and the expanded object form
    /// (`{local, container, protocol}`) — see [`PortMapping`]. Validated
    /// (matching ranges, positive ports) at config-parse time, unlike
    /// `volumes`, which is never format-checked.
    pub ports: Option<Vec<PortMapping>>,
    /// Overrides the health check configuration baked into the image — see
    /// [`HealthCheckConfig`]. Applied at container creation; a dependency
    /// container with a health check (from here or from its image) must
    /// report healthy before its dependents start.
    pub health_check: Option<HealthCheckConfig>,
    /// Commands run inside the container (via `docker exec`) after it
    /// becomes healthy but before its dependents start — see
    /// [`SetupCommand`]. Plain strings, no [expression](#expressions)
    /// support — matching Batect, which doesn't type these as expressions
    /// either.
    pub setup_commands: Option<Vec<SetupCommand>>,
    /// Overrides the image's own `WORKDIR`. A plain string, not an
    /// [expression](#expressions) — matching Batect's own `String` (not
    /// `Expression`) typing for this field. Overridden by the task-level
    /// `run.working_directory`, when set — see [`TaskRun::working_directory`].
    pub working_directory: Option<String>,
    /// The command to run inside the container, in place of the image's own
    /// default `CMD`. Tokenized into literal argv the same way `entrypoint`
    /// is (`docker.rs`'s `tokenize_command_line`) — not an
    /// [expression](#expressions), and not run via a shell, matching
    /// Batect's own `Command`-typed `command` field exactly. Applies as-is
    /// to a dependency/sidecar container; for a task's own container,
    /// overridden by the task-level `run.command`, when set — see
    /// [`TaskRun::command`]. Symmetric with `entrypoint` below, and added
    /// alongside it in spirit — this field was missed when `entrypoint` and
    /// the rest of 0.13.0's container runtime options landed, since
    /// `run.command` already covered the task's own container and the gap
    /// (no way to set a dependency's own command at all) wasn't noticed
    /// until later.
    pub command: Option<String>,
    /// Overrides the image's own `ENTRYPOINT`. Tokenized into literal argv
    /// the same way `command` is (`docker.rs`'s `tokenize_command_line`) —
    /// not an [expression](#expressions), and not run via a shell, matching
    /// Batect's own `Command`-typed `entrypoint` field exactly. Overridden
    /// by the task-level `run.entrypoint`, when set — see
    /// [`TaskRun::entrypoint`].
    pub entrypoint: Option<String>,
    /// Docker labels (`key: value`) applied to the container. Container
    /// level only — no task-level `run` override, matching Batect (its
    /// `TaskRunConfiguration` has no equivalent field). Plain strings, no
    /// [expression](#expressions) support — matching Batect's own
    /// `Map<String, String>` typing.
    pub labels: Option<HashMap<String, String>>,
    /// Linux capabilities to add beyond Docker's own default set — Docker's
    /// `--cap-add`. Container level only, matching Batect (no task-level
    /// `run` override in either). No [expression](#expressions) support —
    /// matching Batect's own `Set<Capability>` typing.
    pub capabilities_to_add: Option<HashSet<Capability>>,
    /// Linux capabilities to drop from Docker's own default set — Docker's
    /// `--cap-drop`. Same typing/scope as `capabilities_to_add`.
    pub capabilities_to_drop: Option<HashSet<Capability>>,
    /// Runs the container with extended (nearly all host) privileges —
    /// Docker's `--privileged`. `None`/absent behaves like `false`,
    /// matching Batect's own default. Container level only, matching
    /// Batect (no task-level `run` override in either).
    pub privileged: Option<bool>,
    /// The size of `/dev/shm`, in bytes — Docker's `--shm-size`. Accepts
    /// Batect's own size-string format (`"128"`, `"128b"`, `"128k"`,
    /// `"128m"`, `"128g"` — a bare number means bytes; see
    /// [`parse_byte_size`]) or a plain YAML integer (also bytes), already
    /// converted to bytes here rather than deferred like `dockerfile`/
    /// `build_target`'s plain strings, since Docker's own API wants a byte
    /// count, not a string. `None` inherits Docker's own default (64 MiB).
    /// Container level only, matching Batect (no task-level `run` override
    /// in either).
    #[cfg_attr(
        feature = "schema",
        schemars(schema_with = "crate::schema::byte_size_schema")
    )]
    #[serde(default, deserialize_with = "deserialize_shm_size")]
    pub shm_size: Option<i64>,
    /// Host devices to make available inside the container — Docker's
    /// `--device`. Plain strings/objects, no [expression](#expressions)
    /// support — matching Batect's own `String` (not `Expression`) typing
    /// for `DeviceMount.localPath`. Container level only, matching Batect
    /// (no task-level `run` override in either).
    pub devices: Option<Vec<DeviceMapping>>,
    /// Runs an init process (Docker's own tini-based one, e.g. reaping
    /// zombie processes and forwarding signals) as PID 1 inside the
    /// container, ahead of the actual command — Docker's `--init`.
    /// `None`/absent behaves like `false`, matching both Docker's and
    /// Batect's own default. Container level only, matching Batect (no
    /// task-level `run` override in either).
    pub enable_init_process: Option<bool>,
    /// Docker's logging driver (`--log-driver`), e.g. `"json-file"`,
    /// `"syslog"`, `"none"`. `None` leaves Docker's own daemon-configured
    /// default alone, rather than baking in a literal default here — unlike
    /// Batect, which defaults this to `"json-file"` in its own config model
    /// (immaterial in practice: that's also Docker's own out-of-the-box
    /// default when nothing else is configured). Container level only,
    /// matching Batect (no task-level `run` override in either).
    pub log_driver: Option<String>,
    /// Driver-specific options (Docker's `--log-opt`, repeatable) for
    /// `log_driver` — meaningless without it, same as Docker's own CLI.
    /// Container level only, matching Batect (no task-level `run` override
    /// in either).
    pub log_options: Option<HashMap<String, String>>,
}

/// One entry in a container's `devices` list — a host device path made
/// available inside the container (Docker's `--device`), optionally under a
/// different container-side path and/or with non-default cgroup
/// permissions. Accepts both of Batect's forms — a
/// `"local:container[:options]"` string and the expanded object form
/// (`{local, container, options}`) — mirroring [`PortMapping`]'s
/// string-or-object handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceMapping {
    pub local: String,
    pub container: String,
    /// Docker's cgroup permissions string (e.g. `"rwm"` — read/write/mknod).
    /// `None` lets Docker apply its own default.
    pub options: Option<String>,
}

impl DeviceMapping {
    /// Parses Batect's `"local_path:container_path[:options]"` string form
    /// — ported from Batect's own `DeviceMountConfigSerializer.deserializeFromString`.
    fn parse_string(value: &str) -> Result<Self> {
        let invalid = || {
            anyhow::anyhow!(
                "Device mount definition '{value}' is invalid. It must be in the form \
                 'local_path:container_path' or 'local_path:container_path:options'."
            )
        };
        if value.is_empty() {
            anyhow::bail!("Device mount definition cannot be empty.");
        }
        let mut parts = value.splitn(4, ':');
        let local = parts.next().ok_or_else(invalid)?;
        let container = parts.next().ok_or_else(invalid)?;
        let options = parts.next();
        if parts.next().is_some() {
            // A fourth colon-separated segment — Batect's own regex (each
            // segment is `[^:]+`, no further colons allowed) rejects this
            // too.
            return Err(invalid());
        }
        if local.is_empty() || container.is_empty() {
            return Err(invalid());
        }

        Ok(Self {
            local: local.to_string(),
            container: container.to_string(),
            options: options.map(|s| s.to_string()),
        })
    }
}

impl<'de> Deserialize<'de> for DeviceMapping {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DeviceMappingVisitor;

        impl<'de> serde::de::Visitor<'de> for DeviceMappingVisitor {
            type Value = DeviceMapping;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "a device mount string ('local_path:container_path[:options]') or an \
                     object with 'local'/'container'/'options' fields",
                )
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<DeviceMapping, E>
            where
                E: serde::de::Error,
            {
                DeviceMapping::parse_string(v).map_err(serde::de::Error::custom)
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<DeviceMapping, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut local: Option<String> = None;
                let mut container: Option<String> = None;
                let mut options: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "local" => local = Some(map.next_value()?),
                        "container" => container = Some(map.next_value()?),
                        "options" => options = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["local", "container", "options"],
                            ))
                        }
                    }
                }
                let local = local.ok_or_else(|| serde::de::Error::missing_field("local"))?;
                let container =
                    container.ok_or_else(|| serde::de::Error::missing_field("container"))?;
                Ok(DeviceMapping {
                    local,
                    container,
                    options,
                })
            }
        }

        deserializer.deserialize_any(DeviceMappingVisitor)
    }
}

impl Serialize for DeviceMapping {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.options {
            Some(options) => {
                serializer.serialize_str(&format!("{}:{}:{}", self.local, self.container, options))
            }
            None => serializer.serialize_str(&format!("{}:{}", self.local, self.container)),
        }
    }
}

/// One `volumes` entry. Either a `local` bind mount (a host path, resolved
/// against the container's own base path — see
/// [`Config::resolve_expressions_with`]), a `cache` mount (a named volume
/// that persists between separate `ratect` invocations, or a host directory
/// under `--cache-type=directory` — see [`crate::cache::resolve_cache_mount`]),
/// or a `tmpfs` mount (an in-memory filesystem, lost when the container
/// exits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeMount {
    Local(LocalVolumeMount),
    Cache(CacheVolumeMount),
    Tmpfs(TmpfsVolumeMount),
}

/// A host path bind-mounted into the container. `local` supports
/// [expressions](#expressions) and is resolved against the declaring
/// container's own base path — see [`Config::resolve_expressions_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVolumeMount {
    pub local: String,
    pub container: String,
    pub options: Option<String>,
}

/// A named cache volume — Batect's `cache` mount type. `name` (not `local`,
/// unlike [`LocalVolumeMount`]) identifies the cache, combined with a
/// per-project key into a Docker volume name (`CacheType::Volume`) or a
/// directory under `.batect/caches/` (`CacheType::Directory`) — see
/// [`crate::cache`]. Plain `String`s, not [expressions](#expressions),
/// matching Batect's own `CacheMount` typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheVolumeMount {
    pub name: String,
    pub container: String,
    pub options: Option<String>,
}

/// An in-memory filesystem mount — Batect's `tmpfs` mount type. Lost when
/// the container exits; no `local` host path or cache `name`, unlike
/// [`LocalVolumeMount`]/[`CacheVolumeMount`]. `options` is an opaque string
/// (e.g. `"size=100m,mode=1770"`) forwarded verbatim to Docker's own
/// `HostConfig.Tmpfs` map — matching Batect, neither side parses or
/// validates its contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmpfsVolumeMount {
    pub container: String,
    pub options: Option<String>,
}

impl VolumeMount {
    /// Parses Batect's `"local_path:container_path[:options]"` string form —
    /// always a `Local` mount; there's no compact string form for `cache`/
    /// `tmpfs` (matching Batect, whose string form only ever produces a
    /// `LocalMount`). Mirrors [`DeviceMapping::parse_string`] exactly.
    fn parse_string(value: &str) -> Result<Self> {
        let invalid = || {
            anyhow::anyhow!(
                "Volume mount definition '{value}' is invalid. It must be in the form \
                 'local_path:container_path' or 'local_path:container_path:options'."
            )
        };
        if value.is_empty() {
            anyhow::bail!("Volume mount definition cannot be empty.");
        }
        let mut parts = value.splitn(4, ':');
        let local = parts.next().ok_or_else(invalid)?;
        let container = parts.next().ok_or_else(invalid)?;
        let options = parts.next();
        if parts.next().is_some() {
            return Err(invalid());
        }
        if local.is_empty() || container.is_empty() {
            return Err(invalid());
        }

        Ok(Self::Local(LocalVolumeMount {
            local: local.to_string(),
            container: container.to_string(),
            options: options.map(|s| s.to_string()),
        }))
    }
}

impl<'de> Deserialize<'de> for VolumeMount {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct VolumeMountVisitor;

        impl<'de> serde::de::Visitor<'de> for VolumeMountVisitor {
            type Value = VolumeMount;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "a volume mount string ('local_path:container_path[:options]') or an object \
                     with 'local'/'container'/'options'/'name'/'type' fields",
                )
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<VolumeMount, E>
            where
                E: serde::de::Error,
            {
                VolumeMount::parse_string(v).map_err(serde::de::Error::custom)
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<VolumeMount, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut local: Option<String> = None;
                let mut container: Option<String> = None;
                let mut options: Option<String> = None;
                let mut name: Option<String> = None;
                let mut mount_type: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "local" => local = Some(map.next_value()?),
                        "container" => container = Some(map.next_value()?),
                        "options" => options = Some(map.next_value()?),
                        "name" => name = Some(map.next_value()?),
                        "type" => mount_type = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["local", "container", "options", "name", "type"],
                            ))
                        }
                    }
                }
                let container =
                    container.ok_or_else(|| serde::de::Error::missing_field("container"))?;

                match mount_type.as_deref().unwrap_or("local") {
                    "local" => {
                        if name.is_some() {
                            return Err(serde::de::Error::custom(
                                "Field 'name' is not permitted for local path mounts.",
                            ));
                        }
                        let local =
                            local.ok_or_else(|| serde::de::Error::missing_field("local"))?;
                        Ok(VolumeMount::Local(LocalVolumeMount {
                            local,
                            container,
                            options,
                        }))
                    }
                    "cache" => {
                        if local.is_some() {
                            return Err(serde::de::Error::custom(
                                "Field 'local' is not permitted for cache mounts.",
                            ));
                        }
                        let name = name.ok_or_else(|| serde::de::Error::missing_field("name"))?;
                        Ok(VolumeMount::Cache(CacheVolumeMount {
                            name,
                            container,
                            options,
                        }))
                    }
                    "tmpfs" => {
                        if local.is_some() {
                            return Err(serde::de::Error::custom(
                                "Field 'local' is not permitted for tmpfs mounts.",
                            ));
                        }
                        if name.is_some() {
                            return Err(serde::de::Error::custom(
                                "Field 'name' is not permitted for tmpfs mounts.",
                            ));
                        }
                        Ok(VolumeMount::Tmpfs(TmpfsVolumeMount { container, options }))
                    }
                    other => Err(serde::de::Error::custom(format!(
                        "Unknown volume mount type '{other}'. It must be 'local', 'cache', or \
                         'tmpfs'."
                    ))),
                }
            }
        }

        deserializer.deserialize_any(VolumeMountVisitor)
    }
}

impl Serialize for VolumeMount {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // Re-emits the compact string form — round-trips through the
            // same shape `parse_string` accepts.
            VolumeMount::Local(mount) => match &mount.options {
                Some(options) => serializer
                    .serialize_str(&format!("{}:{}:{}", mount.local, mount.container, options)),
                None => serializer.serialize_str(&format!("{}:{}", mount.local, mount.container)),
            },
            // No compact string form exists for `cache` — always the
            // expanded object.
            VolumeMount::Cache(mount) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("type", "cache")?;
                map.serialize_entry("name", &mount.name)?;
                map.serialize_entry("container", &mount.container)?;
                if let Some(options) = &mount.options {
                    map.serialize_entry("options", options)?;
                }
                map.end()
            }
            // No compact string form exists for `tmpfs` either — always the
            // expanded object.
            VolumeMount::Tmpfs(mount) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("type", "tmpfs")?;
                map.serialize_entry("container", &mount.container)?;
                if let Some(options) = &mount.options {
                    map.serialize_entry("options", options)?;
                }
                map.end()
            }
        }
    }
}

/// Parses Batect's own size-string format (its `BinarySize` regex,
/// `^(\d+)\s*([mkg]?)b?$`, case-insensitive): a non-negative integer,
/// optionally followed by a unit (`k`/`m`/`g`, 1024-based) and/or a
/// trailing literal `b` (bytes when there's no unit, e.g. `"128b"`) —
/// `"128"`, `"128b"`, `"128k"`, `"128m"`, and `"128g"` are all valid.
fn parse_byte_size(value: &str) -> std::result::Result<i64, String> {
    let invalid = || {
        format!(
            "Invalid size '{value}'. It must be in the format '123', '123b', '123k', '123m' or \
             '123g'."
        )
    };

    let lower = value.trim().to_ascii_lowercase();
    let without_b = lower.strip_suffix('b').unwrap_or(&lower);
    let (digits, multiplier) = if let Some(rest) = without_b.strip_suffix('k') {
        (rest, 1024_i64)
    } else if let Some(rest) = without_b.strip_suffix('m') {
        (rest, 1024_i64 * 1024)
    } else if let Some(rest) = without_b.strip_suffix('g') {
        (rest, 1024_i64 * 1024 * 1024)
    } else {
        (without_b, 1)
    };
    let digits = digits.trim_end();

    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let count: i64 = digits.parse().map_err(|_| invalid())?;
    count.checked_mul(multiplier).ok_or_else(invalid)
}

/// `serde` `deserialize_with` for [`Container::shm_size`] — accepts either
/// a Batect-style size string ([`parse_byte_size`]) or a plain integer
/// (bytes). Only invoked when the field is actually present; `#[serde(default)]`
/// handles the absent case.
fn deserialize_shm_size<'de, D>(deserializer: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct ShmSizeVisitor;

    impl<'de> serde::de::Visitor<'de> for ShmSizeVisitor {
        type Value = Option<i64>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a size like '128', '128b', '128k', '128m', or '128g'")
        }

        fn visit_str<E>(self, v: &str) -> std::result::Result<Option<i64>, E>
        where
            E: serde::de::Error,
        {
            parse_byte_size(v).map(Some).map_err(E::custom)
        }

        fn visit_u64<E>(self, v: u64) -> std::result::Result<Option<i64>, E>
        where
            E: serde::de::Error,
        {
            Ok(Some(v as i64))
        }

        fn visit_i64<E>(self, v: i64) -> std::result::Result<Option<i64>, E>
        where
            E: serde::de::Error,
        {
            Ok(Some(v))
        }
    }

    deserializer.deserialize_any(ShmSizeVisitor)
}

/// `serde` `deserialize_with` for the `environment` maps — accepts a YAML
/// scalar of any type as a value and coerces it to its string form, the way
/// Batect does, so `MY_VAR: 1` or `DEBUG: true` is read as `"1"`/`"true"`
/// rather than rejected with a type-mismatch error. Only the *values* are
/// coerced (keys are already strings), and only when the field is present;
/// `#[serde(default)]` handles the absent case.
fn deserialize_scalar_string_map<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<HashMap<String, String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// A single YAML scalar read as its string form, whatever its type.
    struct ScalarString(String);

    impl<'de> serde::Deserialize<'de> for ScalarString {
        fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            struct ScalarStringVisitor;

            impl<'de> serde::de::Visitor<'de> for ScalarStringVisitor {
                type Value = ScalarString;

                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a string, number, or boolean")
                }

                fn visit_str<E>(self, v: &str) -> std::result::Result<ScalarString, E>
                where
                    E: serde::de::Error,
                {
                    Ok(ScalarString(v.to_owned()))
                }

                fn visit_i64<E>(self, v: i64) -> std::result::Result<ScalarString, E>
                where
                    E: serde::de::Error,
                {
                    Ok(ScalarString(v.to_string()))
                }

                fn visit_u64<E>(self, v: u64) -> std::result::Result<ScalarString, E>
                where
                    E: serde::de::Error,
                {
                    Ok(ScalarString(v.to_string()))
                }

                fn visit_f64<E>(self, v: f64) -> std::result::Result<ScalarString, E>
                where
                    E: serde::de::Error,
                {
                    Ok(ScalarString(v.to_string()))
                }

                fn visit_bool<E>(self, v: bool) -> std::result::Result<ScalarString, E>
                where
                    E: serde::de::Error,
                {
                    Ok(ScalarString(v.to_string()))
                }
            }

            deserializer.deserialize_any(ScalarStringVisitor)
        }
    }

    let map: Option<HashMap<String, ScalarString>> = Option::deserialize(deserializer)?;
    Ok(map.map(|entries| entries.into_iter().map(|(key, value)| (key, value.0)).collect()))
}

/// Controls whether `TaskEngine::resolve_image` pulls an `image` container's
/// image fresh or reuses whatever's already present locally — matching
/// Batect's own `ImagePullPolicy` exactly, including its wire values
/// (`serde`'s default enum serialization already matches Rust's own PascalCase
/// variant names, so no `rename_all` is needed here, unlike [`Capability`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImagePullPolicy {
    /// Pull only if the image doesn't already exist locally — Batect's own
    /// default.
    #[default]
    IfNotPresent,
    /// Always pull, even if the image already exists locally — Ratect's
    /// entire pre-0.13.0 behavior for every `image` container.
    Always,
}

/// A Linux capability name, validated at config-parse time — an unknown name
/// is rejected with a clear error rather than silently reaching Docker's API
/// to fail there (or, worse, being silently ignored). `serde`'s
/// `SCREAMING_SNAKE_CASE` rename matches every variant to its Docker
/// capability name unchanged (e.g. `DacOverride` -> `"DAC_OVERRIDE"`);
/// [`Capability::as_str`] provides the same string back out for building
/// Docker's own `--cap-add`/`--cap-drop` values.
///
/// Based on Batect's own `batect.config.Capability` (in turn based on
/// `capabilities(7)`), but **not** a strict 1:1 port: Batect's last release
/// predates `BPF`/`CHECKPOINT_RESTORE`/`PERFMON` (added to Docker in 20.10,
/// briefly reverted, permanently supported since — see
/// [moby#41563](https://github.com/moby/moby/pull/41563)), so this list adds
/// all three rather than inheriting that gap. A superset, not a divergence —
/// every config Batect accepts here still parses identically.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Capability {
    AuditControl,
    AuditRead,
    AuditWrite,
    BlockSuspend,
    Bpf,
    CheckpointRestore,
    Chown,
    DacOverride,
    DacReadSearch,
    Fowner,
    Fsetid,
    IpcLock,
    IpcOwner,
    Kill,
    Lease,
    LinuxImmutable,
    MacAdmin,
    MacOverride,
    Mknod,
    NetAdmin,
    NetBindService,
    NetBroadcast,
    NetRaw,
    Perfmon,
    Setgid,
    Setfcap,
    Setpcap,
    Setuid,
    SysAdmin,
    SysBoot,
    SysChroot,
    SysModule,
    SysNice,
    SysPacct,
    SysPtrace,
    SysRawio,
    SysResource,
    SysTime,
    SysTtyConfig,
    Syslog,
    WakeAlarm,
    All,
}

impl Capability {
    /// The exact Docker/Batect capability name (e.g. `"DAC_OVERRIDE"`) —
    /// what `docker.rs` sends as a `--cap-add`/`--cap-drop` entry.
    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::AuditControl => "AUDIT_CONTROL",
            Capability::AuditRead => "AUDIT_READ",
            Capability::AuditWrite => "AUDIT_WRITE",
            Capability::BlockSuspend => "BLOCK_SUSPEND",
            Capability::Bpf => "BPF",
            Capability::CheckpointRestore => "CHECKPOINT_RESTORE",
            Capability::Chown => "CHOWN",
            Capability::DacOverride => "DAC_OVERRIDE",
            Capability::DacReadSearch => "DAC_READ_SEARCH",
            Capability::Fowner => "FOWNER",
            Capability::Fsetid => "FSETID",
            Capability::IpcLock => "IPC_LOCK",
            Capability::IpcOwner => "IPC_OWNER",
            Capability::Kill => "KILL",
            Capability::Lease => "LEASE",
            Capability::LinuxImmutable => "LINUX_IMMUTABLE",
            Capability::MacAdmin => "MAC_ADMIN",
            Capability::MacOverride => "MAC_OVERRIDE",
            Capability::Mknod => "MKNOD",
            Capability::NetAdmin => "NET_ADMIN",
            Capability::NetBindService => "NET_BIND_SERVICE",
            Capability::NetBroadcast => "NET_BROADCAST",
            Capability::NetRaw => "NET_RAW",
            Capability::Perfmon => "PERFMON",
            Capability::Setgid => "SETGID",
            Capability::Setfcap => "SETFCAP",
            Capability::Setpcap => "SETPCAP",
            Capability::Setuid => "SETUID",
            Capability::SysAdmin => "SYS_ADMIN",
            Capability::SysBoot => "SYS_BOOT",
            Capability::SysChroot => "SYS_CHROOT",
            Capability::SysModule => "SYS_MODULE",
            Capability::SysNice => "SYS_NICE",
            Capability::SysPacct => "SYS_PACCT",
            Capability::SysPtrace => "SYS_PTRACE",
            Capability::SysRawio => "SYS_RAWIO",
            Capability::SysResource => "SYS_RESOURCE",
            Capability::SysTime => "SYS_TIME",
            Capability::SysTtyConfig => "SYS_TTY_CONFIG",
            Capability::Syslog => "SYSLOG",
            Capability::WakeAlarm => "WAKE_ALARM",
            Capability::All => "ALL",
        }
    }
}

/// One entry in a container's `build_secrets` map — either an `environment`
/// variable (read from the *host* process's own environment at build time)
/// or a `path` to a file on the host, mirroring Batect's own
/// `EnvironmentSecret`/`FileSecret` split. Exactly one of the two is
/// required; a hand-written [`Deserialize`] impl (mirroring
/// [`PortMapping`]'s) enforces this with the same error wording Batect
/// itself uses for the equivalent mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSecret {
    /// The literal name of a host environment variable to read the
    /// secret's value from. Not an [expression](#expressions) — matching
    /// Batect's own `String` (not `Expression`) typing for this field.
    Environment(String),
    /// A path to a file on the host containing the secret's value.
    /// Supports [expressions](#expressions) and is resolved the same way
    /// as `build_directory` — see [`Config::resolve_expressions_with`].
    Path(String),
}

impl<'de> Deserialize<'de> for BuildSecret {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct BuildSecretVisitor;

        impl<'de> serde::de::Visitor<'de> for BuildSecretVisitor {
            type Value = BuildSecret;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object with either an 'environment' or a 'path' field")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<BuildSecret, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut environment: Option<String> = None;
                let mut path: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "environment" => environment = Some(map.next_value()?),
                        "path" => path = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["environment", "path"],
                            ))
                        }
                    }
                }

                match (environment, path) {
                    (Some(_), Some(_)) => Err(serde::de::Error::custom(
                        "A secret can have either 'environment' or 'path', but both have been \
                         provided.",
                    )),
                    (Some(environment), None) => Ok(BuildSecret::Environment(environment)),
                    (None, Some(path)) => Ok(BuildSecret::Path(path)),
                    (None, None) => Err(serde::de::Error::custom(
                        "A secret must have either 'environment' or 'path', but neither has \
                         been provided.",
                    )),
                }
            }
        }

        deserializer.deserialize_map(BuildSecretVisitor)
    }
}

impl Serialize for BuildSecret {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            BuildSecret::Environment(value) => map.serialize_entry("environment", value)?,
            BuildSecret::Path(value) => map.serialize_entry("path", value)?,
        }
        map.end()
    }
}

/// One entry in a container's `build_ssh` list — see [`Container::build_ssh`]
/// for why Ratect only supports a single `default`-id, agent-forwarding
/// (no explicit `paths`) entry, checked in
/// [`Config::resolve_expressions_with`] rather than here (so the error can
/// name the offending container).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshAgent {
    /// The agent id a Dockerfile's `RUN --mount=type=ssh,id=<id>` refers
    /// to. Ratect only supports the implicit `default` agent, so this must
    /// be `default` if given at all.
    pub id: Option<String>,
    /// Private key files to forward instead of a running agent. Not
    /// supported by Ratect — must be empty.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Overrides the [health check configuration](https://docs.docker.com/engine/reference/builder/#healthcheck)
/// specified in the container's image. Every field is optional — an omitted
/// field inherits the image's own value, matching Batect (and Docker's `0` =
/// inherit convention). Durations use Batect's Go-style string format:
/// `"2s"`, `"1m30s"`, `"500ms"`, `"0"`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCheckConfig {
    /// Run via the system's default shell inside the container (Docker's
    /// `CMD-SHELL` form, same as a Dockerfile `HEALTHCHECK CMD` string) —
    /// exit code 0 means healthy. Not a Batect expression (no
    /// interpolation), matching Batect's own `String` typing.
    pub command: Option<String>,
    /// The interval between runs of the health check.
    #[cfg_attr(
        feature = "schema",
        schemars(schema_with = "crate::schema::duration_schema")
    )]
    #[serde(default, with = "duration_string")]
    pub interval: Option<std::time::Duration>,
    /// The number of times to perform the health check before considering
    /// the container unhealthy.
    pub retries: Option<u32>,
    /// The time to wait before failing health checks count against the
    /// retry count.
    #[cfg_attr(
        feature = "schema",
        schemars(schema_with = "crate::schema::duration_schema")
    )]
    #[serde(default, with = "duration_string")]
    pub start_period: Option<std::time::Duration>,
    /// The time to wait before timing out a single health check invocation.
    #[cfg_attr(
        feature = "schema",
        schemars(schema_with = "crate::schema::duration_schema")
    )]
    #[serde(default, with = "duration_string")]
    pub timeout: Option<std::time::Duration>,
}

/// One entry in a container's `setup_commands` list: a command run inside
/// the started container after it becomes healthy but before its dependents
/// start. Runs with the container's own environment and user/group.
/// Tokenized into literal argv the same way `command`/`entrypoint` are (see
/// `tokenize_command_line` in `docker.rs`) — no shell involved, matching
/// Batect's own `SetupCommand.command` exactly (typed `Command`, the same
/// type as `Container.command`/`entrypoint`, and passed to Docker's exec API
/// as already-parsed argv — confirmed by reading
/// `RunContainerSetupCommandsStepRunner.runSetupCommand`, not assumed from
/// Batect's docs). A command relying on shell operators (`&&`, `$VAR`
/// expansion, etc.) needs an explicit `sh -c '...'` wrapper, same as
/// `command`/`entrypoint`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCommand {
    /// The command to run, tokenized into arguments rather than run through
    /// a shell — wrap it in `sh -c '...'` to use shell operators.
    pub command: String,
    /// Falls back to the container's own `working_directory`
    /// ([`Container::working_directory`]) when omitted, and then to the
    /// image's own default when neither is set — matching Batect.
    pub working_directory: Option<String>,
}

/// Parses Batect's duration string format (itself Go-style): one or more
/// `<number><unit>` components (`ns`, `us`/`µs`/`μs`, `ms`, `s`, `m`, `h`),
/// numbers optionally fractional, or a bare `0` — e.g. `"2s"`, `"1m30s"`,
/// `"1.5h"`, `"500ms"`, `"0"`. Ported from Batect's `DurationSerializer`,
/// except that its (accidental) acceptance of negative durations is
/// rejected here — Docker's API can't take one anyway, and rejecting it at
/// config-load time gives a far clearer error.
pub fn parse_duration(text: &str) -> Result<std::time::Duration> {
    let invalid = || anyhow::anyhow!("The value '{text}' is not a valid duration.");

    let unsigned = match text.strip_prefix(['+', '-']) {
        Some(rest) if text.starts_with('-') && rest != "0" => {
            anyhow::bail!("The duration '{text}' is negative. Durations must be positive.")
        }
        Some(rest) => rest,
        None => text,
    };

    if unsigned == "0" {
        return Ok(std::time::Duration::ZERO);
    }

    let mut remaining = unsigned;
    let mut total_nanos = 0.0f64;

    if remaining.is_empty() {
        return Err(invalid());
    }

    while !remaining.is_empty() {
        let number_len = remaining
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or_else(invalid)?;
        let number_str = &remaining[..number_len];
        // Batect's grammar: digits with at most one dot and at least one
        // digit somewhere (`2`, `2.`, `2.5`, `.5` — but never `.` alone).
        if !number_str.chars().any(|c| c.is_ascii_digit()) || number_str.matches('.').count() > 1 {
            return Err(invalid());
        }
        let number: f64 = number_str.parse().map_err(|_| invalid())?;

        // Two-character units listed before their one-character prefixes,
        // so `ms` isn't misread as `m`.
        const UNITS: &[(&str, f64)] = &[
            ("ns", 1.0),
            ("us", 1e3),
            ("µs", 1e3),
            ("μs", 1e3),
            ("ms", 1e6),
            ("s", 1e9),
            ("m", 60e9),
            ("h", 3600e9),
        ];
        let unit_str = &remaining[number_len..];
        let (unit, multiplier) = UNITS
            .iter()
            .find(|(unit, _)| unit_str.starts_with(unit))
            .ok_or_else(invalid)?;

        total_nanos += number * multiplier;
        remaining = &unit_str[unit.len()..];
    }

    Ok(std::time::Duration::from_nanos(total_nanos.round() as u64))
}

/// Serde adapter for `Option<Duration>` fields holding Batect duration
/// strings — see [`parse_duration`]. Serializes back as whole nanoseconds
/// (`"...ns"`), which the same format round-trips exactly.
mod duration_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<std::time::Duration>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match value {
            Some(duration) => serializer.serialize_str(&format!("{}ns", duration.as_nanos())),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<std::time::Duration>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            Some(text) => super::parse_duration(&text)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

/// Runs this container as the host's own user/group instead of whatever the
/// image defaults to (see [`Config::resolve_expressions_with`]'s validation
/// and `TaskEngine::resolve_user_mapping`). `home_directory` is required
/// when `enabled` is `true` (and rejected otherwise) — Ratect never guesses
/// one, matching Batect.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAsCurrentUser {
    /// Whether to run as the host's own user. Required — there's no
    /// default.
    pub enabled: bool,
    /// The home directory to create inside the container for that user.
    /// Must be an absolute path; it's a path inside the container, so it's
    /// never resolved against anything on the host.
    pub home_directory: Option<String>,
}

/// A single port or a range of consecutive ports (`from..=to`; `from == to`
/// for a single port). Ported from Batect's own `PortRange`: `from` must be
/// positive, and `from <= to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub from: u16,
    pub to: u16,
}

impl PortRange {
    /// Parses `"port"` or `"from-to"`. Ported from Batect's
    /// `PortRange.parse`.
    pub fn parse(value: &str) -> Result<Self> {
        let invalid = || {
            anyhow::anyhow!(
                "Port range '{value}' is invalid. It must be in the form 'port' or 'from-to' \
                 and each port must be a positive integer."
            )
        };
        let (from_str, to_str) = value.split_once('-').unwrap_or((value, value));
        let from: u16 = from_str.parse().map_err(|_| invalid())?;
        let to: u16 = to_str.parse().map_err(|_| invalid())?;
        if from == 0 {
            anyhow::bail!("Port range '{value}' is invalid. Ports must be positive integers.");
        }
        if from > to {
            anyhow::bail!(
                "Port range '{value}' is invalid. Port range limits must be given in ascending \
                 order."
            );
        }
        Ok(Self { from, to })
    }

    /// How many ports this range covers — `1` for a single port.
    pub fn size(&self) -> u32 {
        (self.to as u32 - self.from as u32) + 1
    }
}

impl std::fmt::Display for PortRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.from == self.to {
            write!(f, "{}", self.from)
        } else {
            write!(f, "{}-{}", self.from, self.to)
        }
    }
}

impl<'de> Deserialize<'de> for PortRange {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PortRangeVisitor;

        impl serde::de::Visitor<'_> for PortRangeVisitor {
            type Value = PortRange;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a port number or a port range in the form 'from-to'")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<PortRange, E>
            where
                E: serde::de::Error,
            {
                PortRange::parse(v).map_err(serde::de::Error::custom)
            }

            fn visit_u64<E>(self, v: u64) -> std::result::Result<PortRange, E>
            where
                E: serde::de::Error,
            {
                PortRange::parse(&v.to_string()).map_err(serde::de::Error::custom)
            }

            fn visit_i64<E>(self, v: i64) -> std::result::Result<PortRange, E>
            where
                E: serde::de::Error,
            {
                PortRange::parse(&v.to_string()).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_any(PortRangeVisitor)
    }
}

// No `Serialize` impl for `PortRange` on its own: it only ever appears
// inside a `PortMapping`, whose hand-written `Serialize` below formats the
// whole `"local:container/protocol"` string itself (via `Display`), so a
// bare-`PortRange` serializer would be dead code.

/// A `ports` entry: publishes `local` (a container's `container` port, or
/// range) to the host. Accepts either Batect form — a
/// `"local:container[/protocol]"` string (`parse_string`) or an expanded
/// object (`{local, container, protocol}`, via [`Deserialize`]) — and
/// validates `local`/`container` cover the same number of ports at
/// construction time either way, matching Batect's own
/// `PortMappingConfigSerializer.validateDeserializedObject`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    pub local: PortRange,
    pub container: PortRange,
    pub protocol: String,
}

impl PortMapping {
    fn new(local: PortRange, container: PortRange, protocol: String) -> Result<Self> {
        if local.size() != container.size() {
            anyhow::bail!(
                "Port mapping definition is invalid. The local port range has {} port(s) and \
                 the container port range has {} port(s), but the ranges must be the same size.",
                local.size(),
                container.size()
            );
        }
        Ok(Self {
            local,
            container,
            protocol,
        })
    }

    /// Parses `"local:container"`, `"local:container/protocol"`,
    /// `"from-to:from-to"`, or `"from-to:from-to/protocol"` (protocol
    /// defaults to `tcp`). Ported from Batect's
    /// `PortMappingConfigSerializer.deserializeFromString`.
    fn parse_string(value: &str) -> Result<Self> {
        let invalid = || {
            anyhow::anyhow!(
                "Port mapping definition '{value}' is invalid. It must be in the form \
                 'local:container', 'local:container/protocol', 'from-to:from-to' or \
                 'from-to:from-to/protocol' and each port must be a positive integer."
            )
        };
        if value.is_empty() {
            anyhow::bail!("Port mapping definition cannot be empty.");
        }
        let (local, rest) = value.split_once(':').ok_or_else(invalid)?;
        let (container, protocol) = match rest.split_once('/') {
            Some((container, protocol)) => (container, protocol),
            None => (rest, "tcp"),
        };
        if local.is_empty() || container.is_empty() || protocol.is_empty() {
            return Err(invalid());
        }

        let local = PortRange::parse(local)?;
        let container = PortRange::parse(container)?;
        Self::new(local, container, protocol.to_string())
    }

    /// Expands this mapping into concrete `(local_port, container_port,
    /// protocol)` triples — more than one when `local`/`container` are
    /// ranges, zipped by position (e.g. `8000-8002:9000-9002` becomes
    /// `8000->9000`, `8001->9001`, `8002->9002`). `local.size() ==
    /// container.size()` is already guaranteed by construction (`new`),
    /// never checked again here.
    pub fn expand(&self) -> Vec<(u16, u16, String)> {
        (0..self.local.size())
            .map(|i| {
                (
                    self.local.from + i as u16,
                    self.container.from + i as u16,
                    self.protocol.clone(),
                )
            })
            .collect()
    }
}

impl<'de> Deserialize<'de> for PortMapping {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PortMappingVisitor;

        impl<'de> serde::de::Visitor<'de> for PortMappingVisitor {
            type Value = PortMapping;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "a port mapping string ('local:container[/protocol]') or an object with \
                     'local'/'container'/'protocol' fields",
                )
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<PortMapping, E>
            where
                E: serde::de::Error,
            {
                PortMapping::parse_string(v).map_err(serde::de::Error::custom)
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<PortMapping, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut local: Option<PortRange> = None;
                let mut container: Option<PortRange> = None;
                let mut protocol: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "local" => local = Some(map.next_value()?),
                        "container" => container = Some(map.next_value()?),
                        "protocol" => protocol = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["local", "container", "protocol"],
                            ))
                        }
                    }
                }
                let local = local.ok_or_else(|| serde::de::Error::missing_field("local"))?;
                let container =
                    container.ok_or_else(|| serde::de::Error::missing_field("container"))?;
                let protocol = protocol.unwrap_or_else(|| "tcp".to_string());
                PortMapping::new(local, container, protocol).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_any(PortMappingVisitor)
    }
}

impl Serialize for PortMapping {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!(
            "{}:{}/{}",
            self.local, self.container, self.protocol
        ))
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Absent for a task that only exists to chain `prerequisites` together
    /// — validated in [`Config::resolve_expressions_with_boundaries`] to
    /// require at least one of `run`/`prerequisites`, matching Batect. A
    /// `run`-less task's prerequisites still execute; there's just no
    /// container of the task's own to run afterwards — see
    /// `TaskEngine::run_task_internal`.
    pub run: Option<TaskRun>,
    /// Other tasks to run to completion, in order, before this one. At
    /// least one of `run` or `prerequisites` is required.
    pub prerequisites: Option<Vec<String>>,
    /// Sidecar containers scoped to this task specifically — distinct from
    /// [`Container::dependencies`], which every task using that container
    /// picks up. Unioned with the task's own container's `dependencies` when
    /// resolving what to start alongside it — see
    /// `TaskEngine::run_task_internal`. Requires `run` (validated in
    /// [`Config::resolve_expressions_with_boundaries`], matching Batect) and
    /// can't name `run.container` itself.
    pub dependencies: Option<Vec<String>>,
    /// Free-text shown next to the task's name in `--list-tasks` output —
    /// see [`format_task_list`].
    pub description: Option<String>,
    /// Groups this task under a heading in `--list-tasks` output, together
    /// with every other task sharing the same `group` — see
    /// [`format_task_list`]. Purely a display grouping; has no effect on
    /// execution order or prerequisites.
    pub group: Option<String>,
    /// Per-task overrides for a *non-main* container used somewhere in this
    /// task's own container graph (a task-level or container-level
    /// dependency, at any depth) — keyed by container name. Can't target
    /// `run.container` itself (set the equivalent property on `run`
    /// instead) or a container outside this task's graph — both validated
    /// in [`Config::resolve_expressions_with_boundaries`], matching
    /// Batect's own `Task`/`ContainerDependencyGraph` checks. Applied in
    /// `TaskEngine::start_dependency`.
    pub customise: Option<HashMap<String, TaskContainerCustomisation>>,
}

/// One entry in a task's `customise` map — see [`Task::customise`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContainerCustomisation {
    /// Merged with the container's own `environment` (see
    /// [`Container::environment`]): the container's values apply first, and
    /// this overrides them on a key collision — same precedence as
    /// [`TaskRun::environment`] over the main container's.
    #[serde(default, deserialize_with = "deserialize_scalar_string_map")]
    pub environment: Option<HashMap<String, String>>,
    /// *Added* to the container's own `ports`, not an override — same
    /// union semantics as [`TaskRun::ports`].
    pub ports: Option<Vec<PortMapping>>,
    /// Overrides the container's own `working_directory` — same semantics
    /// as [`TaskRun::working_directory`].
    pub working_directory: Option<String>,
}

/// Returns `root` plus every container name transitively reachable from it
/// via `dependencies` — the full set of containers that will share one
/// task's network. Used both as the `no_proxy` "these are local, don't
/// proxy traffic to them" exemption list passed to
/// `proxy::proxy_environment_variables`, and to validate a `customise`
/// entry actually names a container that's part of the task (see
/// [`Config::resolve_expressions_with_boundaries`]).
///
/// `task_dependencies` (a task's own task-level `dependencies` — sidecars
/// scoped to this one task, distinct from `root`'s own container-level
/// `dependencies`) are unioned in at the root only, matching Batect's
/// `taskDependencies = task.dependsOnContainers + taskContainer.dependencies`
/// — each one's *own* container-level `dependencies` still resolve
/// transitively from there, same as any other dependency.
///
/// Visited-set-guarded so a config cycle can't hang this pure walk — real
/// cycle detection (which actually rejects a cycle as a user-facing error)
/// still happens separately, in `TaskEngine::start_dependency`.
pub fn container_names_in_task(
    containers: &HashMap<String, Container>,
    root: &str,
    task_dependencies: Option<&[String]>,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut stack = vec![root.to_string()];
    stack.extend(task_dependencies.into_iter().flatten().cloned());
    while let Some(name) = stack.pop() {
        if !names.insert(name.clone()) {
            continue;
        }
        if let Some(dependencies) = containers.get(&name).and_then(|c| c.dependencies.as_ref()) {
            stack.extend(dependencies.iter().cloned());
        }
    }
    names
}

/// Formats `--list-tasks` output for `--output quiet`: one task per line,
/// sorted by name, as `name` alone or `name<TAB>description` (the tab only
/// present when the task has a non-blank description) — no header, no
/// grouping, nothing else, so the output is machine-parsable. An exact port
/// of Batect's own `ListTasksCommand.printMachineReadableFormat`.
pub fn format_task_list_quiet(tasks: &HashMap<String, Task>) -> String {
    let mut names: Vec<_> = tasks.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| match tasks[name].description.as_deref() {
            Some(description) if !description.trim().is_empty() => {
                format!("{name}\t{description}")
            }
            _ => name.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Formats `--list-tasks` output: every task's name (and `description`, if
/// set) under a `Tasks in {project_name}:` header. Groups tasks under a
/// `{group}:` heading — with a task that declares no `group` falling into a
/// trailing `Ungrouped tasks:` bucket — but only once *some* task in the
/// project actually declares one; a project with no `group` usage at all
/// (the common case, and Ratect's pre-0.14.0 behavior) stays a single flat
/// list with no extra headings. Matches Batect's own `ListTasksCommand`
/// human-readable format: groups sorted alphabetically with the ungrouped
/// bucket last, tasks sorted alphabetically within a group.
pub fn format_task_list(project_name: &str, tasks: &HashMap<String, Task>) -> String {
    let mut lines = vec![format!("Tasks in {}:", project_name)];

    if tasks.values().all(|task| task.group.is_none()) {
        let mut names: Vec<_> = tasks.keys().collect();
        names.sort();
        for name in names {
            lines.push(format_task_line(name, tasks[name].description.as_deref()));
        }
        return lines.join("\n");
    }

    let mut groups: HashMap<Option<&str>, Vec<&String>> = HashMap::new();
    for (name, task) in tasks {
        groups.entry(task.group.as_deref()).or_default().push(name);
    }
    for names in groups.values_mut() {
        names.sort();
    }

    let mut group_keys: Vec<_> = groups.keys().copied().collect();
    group_keys.sort_by(|a, b| match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => a.cmp(b),
    });

    lines.push(String::new());
    for (i, key) in group_keys.iter().enumerate() {
        lines.push(match key {
            Some(name) => format!("{}:", name),
            None => "Ungrouped tasks:".to_string(),
        });
        for name in &groups[key] {
            lines.push(format_task_line(name, tasks[*name].description.as_deref()));
        }
        if i + 1 < group_keys.len() {
            lines.push(String::new());
        }
    }

    lines.join("\n")
}

fn format_task_line(name: &str, description: Option<&str>) -> String {
    match description {
        Some(description) => format!("- {}: {}", name, description),
        None => format!("- {}", name),
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRun {
    /// The container to run the task in, by name.
    pub container: String,
    /// Overrides the container's own `command` for this task's run
    /// specifically — see [`Container::command`]. If neither this nor the
    /// container's own `command` is set, the image's own default `CMD`
    /// runs instead.
    pub command: Option<String>,
    /// Environment variables for this task's run specifically, merged over
    /// the container's own `environment` — see `Container::environment`.
    #[serde(default, deserialize_with = "deserialize_scalar_string_map")]
    pub environment: Option<HashMap<String, String>>,
    /// Additional port mappings for this task's run specifically —
    /// *added* to the container's own `ports` (a union, not an override:
    /// matching Batect, which combines these as a `Set`, so there's no
    /// concept of one replacing an entry from the other by container
    /// port). See [`Container::ports`].
    pub ports: Option<Vec<PortMapping>>,
    /// Overrides the container's own `working_directory` for this task's
    /// run specifically — see [`Container::working_directory`].
    pub working_directory: Option<String>,
    /// Overrides the container's own `entrypoint` for this task's run
    /// specifically — see [`Container::entrypoint`].
    pub entrypoint: Option<String>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigVariable {
    /// The value to use when `--config-var` doesn't supply one. Without a
    /// default, a task referring to this variable fails unless it's set.
    pub default: Option<String>,
    /// Recognized but inert — Batect surfaces this in its own generated
    /// docs/help output; Ratect has no such output to show one in, so it's
    /// accepted purely so a real Batect config using it doesn't fail to
    /// load under `deny_unknown_fields`.
    pub description: Option<String>,
}

/// The `path` a `type: git` include defaults to when omitted, matching
/// Batect's own default.
const DEFAULT_GIT_INCLUDE_PATH: &str = "batect-bundle.yml";

/// One entry in a config file's top-level `include` list — either a local
/// file (a bare string path, or the expanded `{type: file, path: ...}`
/// object form, mirroring [`PortMapping`]'s string-or-object handling
/// above), or a Git bundle (`{type: git, repo, ref, path}` — `path` defaults
/// to `batect-bundle.yml`). Any other `type` is rejected with a clear "not
/// supported yet" error rather than a generic parse failure.
#[derive(Debug, Clone)]
pub(crate) enum IncludeEntry {
    File {
        path: String,
    },
    Git {
        repo: String,
        git_ref: String,
        path: String,
    },
}

impl<'de> Deserialize<'de> for IncludeEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct IncludeEntryVisitor;

        impl<'de> serde::de::Visitor<'de> for IncludeEntryVisitor {
            type Value = IncludeEntry;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "an include path, or an object with 'path'/'type' fields (plus 'repo'/'ref' \
                     for 'type: git')",
                )
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<IncludeEntry, E>
            where
                E: serde::de::Error,
            {
                Ok(IncludeEntry::File {
                    path: v.to_string(),
                })
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<IncludeEntry, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut path: Option<String> = None;
                let mut include_type: Option<String> = None;
                let mut repo: Option<String> = None;
                let mut git_ref: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "path" => path = Some(map.next_value()?),
                        "type" => include_type = Some(map.next_value()?),
                        "repo" => repo = Some(map.next_value()?),
                        "ref" => git_ref = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["path", "type", "repo", "ref"],
                            ))
                        }
                    }
                }

                match include_type.as_deref() {
                    Some("git") => {
                        let repo = repo.ok_or_else(|| serde::de::Error::missing_field("repo"))?;
                        let git_ref =
                            git_ref.ok_or_else(|| serde::de::Error::missing_field("ref"))?;
                        let path = path.unwrap_or_else(|| DEFAULT_GIT_INCLUDE_PATH.to_string());
                        Ok(IncludeEntry::Git {
                            repo,
                            git_ref,
                            path,
                        })
                    }
                    Some(other) if other != "file" => Err(serde::de::Error::custom(format!(
                        "Include type '{other}' is not supported yet — only 'file' and 'git' \
                         includes are implemented."
                    ))),
                    _ => {
                        if repo.is_some() || git_ref.is_some() {
                            return Err(serde::de::Error::custom(
                                "'repo' and 'ref' are only valid for 'type: git' includes",
                            ));
                        }
                        let path = path.ok_or_else(|| serde::de::Error::missing_field("path"))?;
                        Ok(IncludeEntry::File { path })
                    }
                }
            }
        }

        deserializer.deserialize_any(IncludeEntryVisitor)
    }
}

/// The Git-clone boundary a file's own further `include` entries must stay
/// within, once traversal has crossed from the caller's own local project
/// tree into a Git-included bundle's content — see the security note on
/// [`Config::load_from_file_with_git_cache`]. Propagated through
/// [`Config::load_from_file_with_git_cache`]'s traversal queue: a local file
/// include inherits its declaring file's own boundary unchanged; a `type:
/// git` include always establishes a fresh one, rooted at its own newly (or
/// previously) cloned repository, regardless of the declaring file's own
/// boundary.
#[derive(Debug, Clone)]
struct GitBoundary {
    repo_dir: PathBuf,
    remote: String,
    git_ref: String,
}

impl GitBoundary {
    /// Purely lexical containment check — deliberately runs before
    /// `resolved` is confirmed to exist, so a `path` engineered to escape
    /// (an absolute path, or a `../..` traversal) is rejected without ever
    /// touching the filesystem at the escaped location.
    fn check_contains(&self, resolved: &Path) -> Result<()> {
        if resolved.starts_with(&self.repo_dir) {
            return Ok(());
        }
        anyhow::bail!(
            "Included file '{}' escapes the Git repository '{}' at '{}' it was included from \
             — includes reached through a Git include must resolve within that repository.",
            resolved.display(),
            self.remote,
            self.git_ref
        );
    }

    /// A second check against the *canonicalized* (symlink-resolved) form
    /// of both paths, once `resolved` is confirmed to exist — closes the
    /// gap `check_contains` alone can't: a malicious repository planting a
    /// symlink inside its own clone that itself points back outside it
    /// would still lexically "start with" `repo_dir`.
    fn check_contains_canonical(&self, resolved: &Path) -> Result<()> {
        let canonical_resolved = resolved
            .canonicalize()
            .with_context(|| format!("Failed to resolve {resolved:?}"))?;
        let canonical_root = self
            .repo_dir
            .canonicalize()
            .with_context(|| format!("Failed to resolve {:?}", self.repo_dir))?;
        if canonical_resolved.starts_with(&canonical_root) {
            return Ok(());
        }
        anyhow::bail!(
            "Included file '{}' escapes the Git repository '{}' at '{}' it was included from \
             (via a symlink) — includes reached through a Git include must resolve within that \
             repository.",
            resolved.display(),
            self.remote,
            self.git_ref
        );
    }

    /// Containment check for a Git-included container's path-bearing fields
    /// (`volumes` host paths, `build_directory`) — see the security note on
    /// [`Config::resolve_expressions_with_boundaries`]. Unlike
    /// `check_contains`/`check_contains_canonical` above (used only for
    /// further `include` resolution, which must stay entirely within the
    /// repository), a shared bundle may reasonably want to reference the
    /// caller's own project directory (e.g.
    /// `<{batect.project_directory}/output:/output`) — so `project_dir` is
    /// accepted as a second allowed root alongside the repository's own
    /// clone directory. Purely lexical, like `check_contains`: a symlink
    /// inside the clone that itself points back outside both allowed roots
    /// isn't caught here, since unlike an `include` target (which must exist
    /// and is read as a file), a `volumes`/`build_directory` path need not
    /// exist yet at config-resolution time — Docker/`docker build` are the
    /// ones that ultimately dereference it.
    fn check_path_allowed(&self, resolved: &Path, project_dir: &Path) -> Result<()> {
        if resolved.starts_with(&self.repo_dir) || resolved.starts_with(project_dir) {
            return Ok(());
        }
        anyhow::bail!(
            "Path '{}' escapes both the Git repository '{}' at '{}' it was included from and \
             the project directory '{}' — a container reached through a Git include must \
             resolve its 'volumes'/'build_directory' paths within one of the two.",
            resolved.display(),
            self.remote,
            self.git_ref,
            project_dir.display()
        );
    }
}

/// One parsed YAML document, before include resolution/merging —
/// [`Config::load_from_file`]'s traversal over `include` produces one of
/// these per file (the root file and every included file, however deeply
/// nested) and merges them into a single [`Config`]. Kept as a distinct type
/// rather than making `Config`'s own fields `Option`/defaulted so `Config`
/// itself — consumed throughout `engine.rs` and this module's own tests via
/// plain struct literals — never has to change shape for this feature.
///
/// `pub(crate)` purely so [`crate::schema`] can generate the JSON schema
/// from it: this — not [`Config`] — is the shape an editor has open, since
/// `include` only exists per-file and every other field is pre-merge.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigFile {
    /// The project's name, used to name the images this project builds and
    /// (with `--cache-type=volume`) its cache volumes. Taken from the root
    /// config file only; ignored in an included file. Defaults to the
    /// project directory's own name.
    project_name: Option<String>,
    /// The containers tasks can run in, keyed by name.
    #[serde(default)]
    containers: HashMap<String, Container>,
    /// The tasks this project defines, keyed by the name used to run them.
    #[serde(default)]
    tasks: HashMap<String, Task>,
    /// Variables tasks and containers can refer to as `<name` or
    /// `<{name}`, overridable per-invocation with `--config-var`.
    config_variables: Option<HashMap<String, ConfigVariable>>,
    /// Other configuration files to merge into this one — local files
    /// (relative to this file's own directory) or Git bundles.
    #[serde(default)]
    include: Vec<IncludeEntry>,
    /// Recognized but has no effect: Ratect collects no telemetry, so
    /// there's nothing to forbid. Accepted so a config written for Batect
    /// still loads.
    #[serde(default)]
    forbid_telemetry: Option<bool>,
}

/// Parses one config file (the root, or an included one) only — no include
/// resolution, path resolution, or expression interpolation.
fn parse_config_file(path: &Path) -> Result<ConfigFile> {
    let file =
        File::open(path).with_context(|| format!("Failed to open config file {:?}", path))?;
    noyalib::from_reader(file).with_context(|| format!("Failed to parse config file {:?}", path))
}

/// Resolves `path` to an absolute, lexically-cleaned path, anchored at the
/// current directory if `path` is itself relative — same normalization
/// [`resolve_path`] applies to a resolved value, reused here for include
/// paths (to de-duplicate an already-loaded file regardless of how many
/// differently-spelled relative paths reach it, and for clear error
/// messages) and to compute the directory a loaded file's own relative
/// paths (`volumes`, `build_directory`) are resolved against.
fn absolute_path(path: &Path) -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join(path).clean())
}

/// The result of [`Config::load_from_file`]: the merged, but not yet
/// expression-resolved, [`Config`], plus enough information for
/// [`resolve_expressions`](Self::resolve_expressions) to resolve each
/// container's relative paths (`volumes` host paths, `build_directory`)
/// against *its own* origin file's directory rather than always the root
/// config's directory — see [Includes](../../docs/config-reference.md#includes).
#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    container_base_paths: HashMap<String, PathBuf>,
    /// The Git boundary a container's `volumes`/`build_directory` paths must
    /// stay within, for every container whose origin file was reached
    /// (directly or via a nested local include) through a `type: git`
    /// include — see [`GitBoundary::check_path_allowed`]. A container absent
    /// from this map was declared entirely within the caller's own local
    /// project tree and has no such restriction, matching the trust model
    /// local includes already had.
    container_git_boundaries: HashMap<String, GitBoundary>,
}

impl LoadedConfig {
    /// Like [`Config::resolve_expressions`], but resolves each container's
    /// relative paths against its own origin file's directory (recorded by
    /// [`Config::load_from_file`]) rather than uniformly against
    /// `base_path`, and additionally confines a Git-included container's
    /// resolved `volumes`/`build_directory` paths to that repository's own
    /// clone directory or the project directory (see
    /// [`GitBoundary::check_path_allowed`]). Identical behavior to
    /// `Config::resolve_expressions` when no `include` was used (every
    /// container's origin is then the root file's own directory anyway, and
    /// `container_git_boundaries` is empty).
    pub fn resolve_expressions(
        &mut self,
        base_path: &Path,
        config_var_overrides: &HashMap<String, String>,
    ) -> Result<()> {
        self.config.resolve_expressions_with_boundaries(
            base_path,
            &self.container_base_paths,
            &self.container_git_boundaries,
            config_var_overrides,
            |name| std::env::var(name).ok(),
        )
    }
}

impl Config {
    /// Like [`load_from_file_with_git_cache`](Self::load_from_file_with_git_cache),
    /// using the production Git include cache (`~/.ratect/incl`, the real
    /// `git` binary) — see that method for the full behavior. Split out so
    /// tests can inject a fake cache instead.
    pub async fn load_from_file(path: &Path) -> Result<LoadedConfig> {
        let git_cache = crate::git_include::GitIncludeCache::new();
        Self::load_from_file_with_git_cache(path, &git_cache).await
    }

    /// Parses the config file and resolves `include`s — but no path
    /// resolution or expression interpolation yet. Those need
    /// `config_var_overrides` from the CLI (`--config-var`/
    /// `--config-vars-file`), which aren't known yet at this point, so
    /// callers must follow up with
    /// [`LoadedConfig::resolve_expressions`].
    ///
    /// A local file `include` entry is resolved relative to the directory of
    /// the file that declares it (not necessarily the root file's
    /// directory). A `type: git` entry is resolved relative to the root of
    /// its cloned repository instead — `git_cache` clones it (or reuses an
    /// existing clone) at most once per distinct `(repo, ref)` per call,
    /// memoized locally even across multiple include entries naming the same
    /// repo/ref. Both kinds are traversed breadth-first; an already-loaded
    /// file (by cleaned absolute path) is skipped rather than reloaded,
    /// which also makes an include cycle harmless rather than infinite. Only
    /// the root file may declare `project_name`; `containers`/`tasks`/
    /// `config_variables` are merged across every loaded file, and a name
    /// defined in more than one file is a hard error naming both files —
    /// matching Batect's own `include` semantics.
    ///
    /// **Containment**: once an include is reached *through* a Git include —
    /// the entry itself, or any local file include declared (transitively)
    /// by the file it named — its resolved path must stay within that Git
    /// repository's own clone directory. `repo`/`ref`/`path` are supplied by
    /// a config file that may itself have come from a third-party Git
    /// repository the caller doesn't fully control, and `path.join` treats
    /// an absolute `path` as replacing its base entirely (not erroring), so
    /// without this check a Git-included bundle could declare an absolute
    /// path, or a `../..` traversal, and pull in an arbitrary file from the
    /// host running `ratect` (e.g. another project's config, or a file with
    /// secrets in its `environment` values) rather than something from its
    /// own repository. The check is purely lexical for paths that don't
    /// exist yet (so it still rejects before ever touching the filesystem),
    /// and additionally re-checked against the *canonicalized* (symlink-
    /// resolved) paths once the target is confirmed to exist, since a
    /// malicious repository could otherwise plant a symlink inside its own
    /// clone that itself points back outside it. Local includes declared
    /// entirely within the caller's own project tree (never having crossed
    /// a Git include) are unrestricted, as before — matching the trust model
    /// local file includes already had prior to Git includes existing.
    pub async fn load_from_file_with_git_cache<G: crate::git_include::GitClient>(
        path: &Path,
        git_cache: &crate::git_include::GitIncludeCache<G>,
    ) -> Result<LoadedConfig> {
        let root_path = absolute_path(path)?;
        let root_file = parse_config_file(path)?;
        let root_dir = root_path.parent().unwrap_or(Path::new("")).to_path_buf();

        let mut seen: HashSet<PathBuf> = HashSet::new();
        seen.insert(root_path.clone());

        let mut git_repo_paths: HashMap<(String, String), PathBuf> = HashMap::new();

        let mut queue: VecDeque<(PathBuf, Option<GitBoundary>, IncludeEntry)> = root_file
            .include
            .iter()
            .cloned()
            .map(|include| (root_dir.clone(), None, include))
            .collect();

        let mut loaded: Vec<(PathBuf, PathBuf, ConfigFile, Option<GitBoundary>)> =
            vec![(root_path, root_dir, root_file, None)];

        while let Some((containing_dir, boundary, include)) = queue.pop_front() {
            let (base_dir, include_path, boundary) = match &include {
                IncludeEntry::File { path } => (containing_dir, path.clone(), boundary),
                IncludeEntry::Git {
                    repo,
                    git_ref,
                    path,
                } => {
                    let key = (repo.clone(), git_ref.clone());
                    let repo_dir = match git_repo_paths.get(&key) {
                        Some(dir) => dir.clone(),
                        None => {
                            let dir = git_cache.ensure_cached(repo, git_ref).await.with_context(
                                || format!("Failed to resolve Git include '{repo}' at '{git_ref}'"),
                            )?;
                            git_repo_paths.insert(key, dir.clone());
                            dir
                        }
                    };
                    let boundary = GitBoundary {
                        repo_dir: repo_dir.clone(),
                        remote: repo.clone(),
                        git_ref: git_ref.clone(),
                    };
                    (repo_dir, path.clone(), Some(boundary))
                }
            };
            let resolved = absolute_path(&base_dir.join(&include_path))?;

            if let Some(boundary) = &boundary {
                boundary.check_contains(&resolved)?;
            }

            if !resolved.is_file() {
                if resolved.exists() {
                    anyhow::bail!("Included file '{}' is not a file.", resolved.display());
                }
                anyhow::bail!("Included file '{}' does not exist.", resolved.display());
            }
            if let Some(boundary) = &boundary {
                boundary.check_contains_canonical(&resolved)?;
            }
            if !seen.insert(resolved.clone()) {
                continue;
            }

            let file = parse_config_file(&resolved)?;
            if file.project_name.is_some() {
                anyhow::bail!(
                    "Included file '{}' declares 'project_name', but only the root \
                     configuration file can do so.",
                    resolved.display()
                );
            }

            let file_dir = resolved.parent().unwrap_or(Path::new("")).to_path_buf();
            queue.extend(
                file.include
                    .iter()
                    .cloned()
                    .map(|include| (file_dir.clone(), boundary.clone(), include)),
            );
            loaded.push((resolved, file_dir, file, boundary));
        }

        let project_name = loaded[0].2.project_name.clone().ok_or_else(|| {
            anyhow::anyhow!("Configuration file is missing the required 'project_name' field")
        })?;
        let forbid_telemetry = loaded[0].2.forbid_telemetry;

        let mut containers = HashMap::new();
        let mut container_base_paths = HashMap::new();
        let mut container_git_boundaries: HashMap<String, GitBoundary> = HashMap::new();
        let mut container_origins: HashMap<String, PathBuf> = HashMap::new();
        let mut tasks = HashMap::new();
        let mut task_origins: HashMap<String, PathBuf> = HashMap::new();
        let mut config_variables = HashMap::new();
        let mut config_variable_origins: HashMap<String, PathBuf> = HashMap::new();

        for (file_path, file_dir, file, boundary) in loaded {
            for (name, container) in file.containers {
                if let Some(previous) = container_origins.insert(name.clone(), file_path.clone()) {
                    anyhow::bail!(
                        "The container '{name}' is defined in multiple files: '{}' and '{}'",
                        previous.display(),
                        file_path.display()
                    );
                }
                container_base_paths.insert(name.clone(), file_dir.clone());
                if let Some(boundary) = &boundary {
                    container_git_boundaries.insert(name.clone(), boundary.clone());
                }
                containers.insert(name, container);
            }
            for (name, task) in file.tasks {
                if let Some(previous) = task_origins.insert(name.clone(), file_path.clone()) {
                    anyhow::bail!(
                        "The task '{name}' is defined in multiple files: '{}' and '{}'",
                        previous.display(),
                        file_path.display()
                    );
                }
                tasks.insert(name, task);
            }
            for (name, var) in file.config_variables.into_iter().flatten() {
                if let Some(previous) =
                    config_variable_origins.insert(name.clone(), file_path.clone())
                {
                    anyhow::bail!(
                        "The config variable '{name}' is defined in multiple files: '{}' and \
                         '{}'",
                        previous.display(),
                        file_path.display()
                    );
                }
                config_variables.insert(name, var);
            }
        }

        Ok(LoadedConfig {
            config: Config {
                project_name,
                containers,
                tasks,
                config_variables: if config_variables.is_empty() {
                    None
                } else {
                    Some(config_variables)
                },
                forbid_telemetry,
            },
            container_base_paths,
            container_git_boundaries,
        })
    }

    /// Loads a `--config-vars-file`: a flat YAML map of config variable
    /// names to values, in the same format/parser as `batect.yml` itself.
    pub fn load_config_vars_file(path: &Path) -> Result<HashMap<String, String>> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open config vars file {:?}", path))?;
        noyalib::from_reader(file)
            .with_context(|| format!("Failed to parse config vars file {:?}", path))
    }

    /// Resolves every expression-bearing value in the config — `environment`
    /// entries (on containers and task `run`s) and volume host paths —
    /// through Batect's expression syntax: `$VAR`/`${VAR}`/`${VAR:-default}`
    /// against the real host environment, and `<name`/`<{name}` against
    /// `config_variables`, merged with `config_var_overrides` (highest
    /// precedence — from `--config-var`/`--config-vars-file`).
    ///
    /// Also turns relative volume host paths into absolute ones (relative to
    /// `base_path`, the config file's directory) — done here, *after*
    /// interpolation, rather than automatically in `load_from_file`. An
    /// expression can itself resolve to an absolute path (e.g. a
    /// `<project_root` config variable), and that must not be prefixed with
    /// `base_path` as if it were still a literal relative fragment — so
    /// path resolution has to run after interpolation, which in turn has to
    /// wait for CLI-supplied config variable overrides to be known.
    pub fn resolve_expressions(
        &mut self,
        base_path: &Path,
        config_var_overrides: &HashMap<String, String>,
    ) -> Result<()> {
        self.resolve_expressions_with(base_path, &HashMap::new(), config_var_overrides, |name| {
            std::env::var(name).ok()
        })
    }

    /// The actual implementation behind [`resolve_expressions`](Self::resolve_expressions),
    /// for callers that never need [`resolve_expressions_with_boundaries`]'s
    /// Git-containment checks (i.e. every caller except
    /// [`LoadedConfig::resolve_expressions`]) — a thin wrapper so their call
    /// sites don't have to pass an always-empty boundaries map.
    fn resolve_expressions_with(
        &mut self,
        base_path: &Path,
        container_base_paths: &HashMap<String, PathBuf>,
        config_var_overrides: &HashMap<String, String>,
        host_env: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        self.resolve_expressions_with_boundaries(
            base_path,
            container_base_paths,
            &HashMap::new(),
            config_var_overrides,
            host_env,
        )
    }

    /// The actual implementation behind [`resolve_expressions`](Self::resolve_expressions)
    /// and [`LoadedConfig::resolve_expressions`], parameterized over the host
    /// environment lookup so tests don't have to touch the real process
    /// environment. `container_base_paths` (empty when called from
    /// `Config::resolve_expressions` directly) overrides `base_path` on a
    /// per-container basis — see [`LoadedConfig`]. `container_git_boundaries`
    /// (likewise empty outside `LoadedConfig::resolve_expressions`) confines
    /// a Git-included container's resolved `volumes`/`build_directory` paths
    /// to that repository's own clone directory *or* the project directory
    /// — see [`GitBoundary::check_path_allowed`] for why the project
    /// directory is a second allowed root rather than requiring pure
    /// containment within the clone.
    fn resolve_expressions_with_boundaries(
        &mut self,
        base_path: &Path,
        container_base_paths: &HashMap<String, PathBuf>,
        container_git_boundaries: &HashMap<String, GitBoundary>,
        config_var_overrides: &HashMap<String, String>,
        host_env: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        if self
            .config_variables
            .as_ref()
            .is_some_and(|vars| vars.contains_key(PROJECT_DIRECTORY_VAR))
        {
            anyhow::bail!(
                "'{PROJECT_DIRECTORY_VAR}' is a built-in config variable and can't be declared \
                 in 'config_variables'"
            );
        }

        for key in config_var_overrides.keys() {
            let declared = self
                .config_variables
                .as_ref()
                .is_some_and(|vars| vars.contains_key(key));
            if !declared {
                anyhow::bail!(
                    "Config variable '{}' was given a value via --config-var/--config-vars-file, \
                     but isn't declared in 'config_variables'",
                    key
                );
            }
        }

        let mut config_vars: HashMap<String, Option<String>> = HashMap::new();
        if let Some(declared) = &self.config_variables {
            for (name, var) in declared {
                let value = config_var_overrides
                    .get(name)
                    .cloned()
                    .or_else(|| var.default.clone());
                config_vars.insert(name.clone(), value);
            }
        }

        // Batect's one built-in config variable: the absolute path of the
        // directory containing the config file. Not user-declarable (see
        // the check above) or overridable via --config-var — the guard
        // above already stops that, since only *declared* names can be
        // overridden.
        let project_directory_path = project_directory_path(base_path)?;
        let project_directory = project_directory_path.display().to_string();
        config_vars.insert(PROJECT_DIRECTORY_VAR.to_string(), Some(project_directory));

        for (container_name, container) in self.containers.iter_mut() {
            let container_base_path = container_base_paths
                .get(container_name)
                .map(PathBuf::as_path)
                .unwrap_or(base_path);
            let container_boundary = container_git_boundaries
                .get(container_name)
                .map(|boundary| (boundary, project_directory_path.as_path()));
            if let Some(environment) = &mut container.environment {
                for value in environment.values_mut() {
                    *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
                }
            }
            if let Some(volumes) = &mut container.volumes {
                for volume in volumes {
                    // `Cache` mounts have nothing to resolve here — `name`/
                    // `container` are plain strings, not expressions,
                    // matching Batect's own `CacheMount` typing. Their
                    // Docker volume name/host directory is resolved later,
                    // once `--cache-type` and the project's cache key are
                    // known — see `crate::cache::resolve_cache_mount`. `Tmpfs`
                    // mounts likewise have nothing to resolve — `container`/
                    // `options` are plain strings too, matching Batect's own
                    // `TmpfsMount` typing.
                    if let VolumeMount::Local(local) = volume {
                        local.local = resolve_path(
                            &local.local,
                            container_base_path,
                            &host_env,
                            &config_vars,
                            container_boundary,
                        )?;
                    }
                }
            }
            if let Some(build_directory) = &mut container.build_directory {
                *build_directory = resolve_path(
                    build_directory,
                    container_base_path,
                    &host_env,
                    &config_vars,
                    container_boundary,
                )?;
            }
            if let Some(build_args) = &mut container.build_args {
                for value in build_args.values_mut() {
                    *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
                }
            }
            if let Some(build_secrets) = &mut container.build_secrets {
                for secret in build_secrets.values_mut() {
                    // `Environment` is a literal host env var *name*, not
                    // itself an expression — matches Batect's own `String`
                    // (not `Expression`) typing for that variant.
                    if let BuildSecret::Path(path) = secret {
                        *path = resolve_path(
                            path,
                            container_base_path,
                            &host_env,
                            &config_vars,
                            container_boundary,
                        )?;
                    }
                }
            }
            if let Some(build_ssh) = &container.build_ssh {
                if build_ssh.len() > 1 {
                    anyhow::bail!(
                        "Container '{}' has {} 'build_ssh' entries, but Ratect only supports \
                         forwarding a single SSH agent from the host — see \
                         docs/differences-from-batect.md#container-fields",
                        container_name,
                        build_ssh.len()
                    );
                }
                if let Some(agent) = build_ssh.first() {
                    if let Some(id) = &agent.id {
                        if id != "default" {
                            anyhow::bail!(
                                "Container '{}' has a 'build_ssh' entry with id '{}', but \
                                 Ratect only supports the implicit 'default' SSH agent id — \
                                 see docs/differences-from-batect.md#container-fields",
                                container_name,
                                id
                            );
                        }
                    }
                    if !agent.paths.is_empty() {
                        anyhow::bail!(
                            "Container '{}' has a 'build_ssh' entry with explicit key \
                             'paths', but Ratect only supports forwarding the host's \
                             running ssh-agent (via SSH_AUTH_SOCK), not explicit key files \
                             — see docs/differences-from-batect.md#container-fields",
                            container_name
                        );
                    }
                }
            }
            if let Some(run_as_current_user) = &mut container.run_as_current_user {
                if run_as_current_user.enabled {
                    let home_directory =
                        run_as_current_user.home_directory.as_mut().ok_or_else(|| {
                            anyhow::anyhow!(
                                "Container '{}' has 'run_as_current_user.enabled' set to true, \
                                 but no 'home_directory' was provided",
                                container_name
                            )
                        })?;
                    // Not `resolve_path` — this is a path *inside the
                    // container*, never resolved against `base_path`.
                    *home_directory =
                        crate::expressions::interpolate(home_directory, &host_env, &config_vars)?;
                    if !home_directory.starts_with('/') {
                        anyhow::bail!(
                            "Container '{}' has an invalid 'run_as_current_user.home_directory': \
                             '{}' is not an absolute path",
                            container_name,
                            home_directory
                        );
                    }
                    // `home_directory` is interpolated raw into a
                    // colon-delimited `/etc/passwd`/`/etc/shadow` line
                    // (`user::generate_passwd_file`) — a `:` shifts that
                    // line's fields, and a newline/other control character
                    // injects an entirely new (attacker-chosen) entry.
                    if home_directory.contains(':') || home_directory.chars().any(char::is_control)
                    {
                        anyhow::bail!(
                            "Container '{}' has an invalid 'run_as_current_user.home_directory': \
                             '{}' contains a ':' or a control character, which would corrupt the \
                             generated /etc/passwd and /etc/shadow entries",
                            container_name,
                            home_directory
                        );
                    }
                } else if run_as_current_user.home_directory.is_some() {
                    anyhow::bail!(
                        "Container '{}' has 'run_as_current_user.home_directory' set, but \
                         'run_as_current_user.enabled' is not true",
                        container_name
                    );
                }
            }
        }

        for (task_name, task) in self.tasks.iter_mut() {
            if task.run.is_none() && task.prerequisites.as_ref().is_none_or(|p| p.is_empty()) {
                anyhow::bail!(
                    "Task '{}' must have at least one of 'run' or 'prerequisites'",
                    task_name
                );
            }
            match (&task.run, &task.dependencies) {
                (None, Some(dependencies)) if !dependencies.is_empty() => {
                    anyhow::bail!(
                        "Task '{}' has 'dependencies' but no 'run' — 'run' is required if \
                         'dependencies' is provided",
                        task_name
                    );
                }
                (Some(run), Some(dependencies)) if dependencies.contains(&run.container) => {
                    anyhow::bail!(
                        "Task '{}' cannot have container '{}' as both the main task \
                         container (via 'run') and a task-level dependency",
                        task_name,
                        run.container
                    );
                }
                _ => {}
            }
            if let (Some(run), Some(customise)) = (&task.run, &task.customise) {
                if let Some(customisation_name) = customise.keys().find(|n| *n == &run.container) {
                    anyhow::bail!(
                        "Cannot apply customisations to main task container '{}' in task \
                         '{}'. Set the corresponding properties on 'run' instead",
                        customisation_name,
                        task_name
                    );
                }
                let names_in_task = container_names_in_task(
                    &self.containers,
                    &run.container,
                    task.dependencies.as_deref(),
                );
                if let Some(customisation_name) =
                    customise.keys().find(|n| !names_in_task.contains(*n))
                {
                    anyhow::bail!(
                        "Task '{}' has customisations for container '{}', but the container \
                         '{}' will not be started as part of the task",
                        task_name,
                        customisation_name,
                        customisation_name
                    );
                }
            }
            if let Some(run) = &mut task.run {
                if let Some(environment) = &mut run.environment {
                    for value in environment.values_mut() {
                        *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
                    }
                }
            }
        }

        Ok(())
    }
}

/// Interpolates expressions within `path`, then resolves the result to an
/// absolute path (relative to `base_path`) if it's relative — done in this
/// order because an expression can itself resolve to an absolute path (e.g.
/// a `<project_root` config variable), which mustn't be prefixed with
/// `base_path` as if it were still a literal relative fragment. Shared by
/// volume host paths (the host-path segment) and `build_directory`.
///
/// `base_path` itself may be relative (e.g. derived from a `-f ./batect.yml`
/// config path), so this always joins onto the current directory too, then
/// lexically `.clean()`s the result — otherwise a `.` component anywhere
/// along the way (from either `base_path` or `path`) would survive verbatim
/// into the returned string, e.g. `/project/./docker` instead of
/// `/project/docker`. Purely cosmetic (the path still resolves correctly on
/// disk either way), but worth avoiding since it's user-visible in errors.
fn resolve_path(
    path: &str,
    base_path: &Path,
    host_env: &impl Fn(&str) -> Option<String>,
    config_vars: &HashMap<String, Option<String>>,
    container_boundary: Option<(&GitBoundary, &Path)>,
) -> Result<String> {
    let interpolated = crate::expressions::interpolate(path, host_env, config_vars)?;
    let resolved = if Path::new(&interpolated).is_relative() {
        let absolute_path = base_path.join(&interpolated);
        std::env::current_dir()?.join(absolute_path).clean()
    } else {
        PathBuf::from(&interpolated)
    };

    if let Some((boundary, project_dir)) = container_boundary {
        boundary.check_path_allowed(&resolved, project_dir)?;
    }

    Ok(resolved.display().to_string())
}

/// The project's own root directory — the absolute, lexically-cleaned
/// directory containing the root config file (`base_path`). This is both
/// the value the built-in `batect.project_directory` config variable
/// resolves to, and the directory Ratect's `.batect/caches/` (cache
/// volumes — see [`crate::cache`]) is scoped under, so it's exposed here
/// rather than kept private to [`Config::resolve_expressions_with`].
///
/// `base_path` itself may be relative (e.g. derived from a `-f
/// ./batect.yml` config path), so this always joins onto the current
/// directory too, then lexically `.clean()`s the result — otherwise a `.`
/// component would survive verbatim (e.g. `/project/.` instead of
/// `/project`).
pub fn project_directory_path(base_path: &Path) -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join(base_path).clean())
}

/// The directory a config file's own relative paths (`volumes`,
/// `build_directory`) resolve against — its containing directory.
///
/// [`Path::parent`] returns `Some("")` for a bare filename with no directory
/// prefix (the common `-f batect.yml` case) rather than `None`, so that
/// isn't a "no parent" case in the `unwrap_or` sense and resolves to `""`,
/// not `"."`. Both are handled identically downstream
/// ([`Config::resolve_expressions`] joins onto the current directory and
/// lexically cleans the result), but it's worth being explicit, since it's
/// easy to assume `parent()` returning `None` is the only case needing a
/// fallback.
pub fn base_path_for(config_file: &Path) -> &Path {
    config_file.parent().unwrap_or(Path::new("."))
}

/// A configuration file loaded, merged and fully resolved — what a binary
/// actually needs before it can build a [`TaskEngine`](crate::engine::TaskEngine).
#[derive(Debug)]
pub struct LoadedProject {
    pub config: Config,
    /// The project's own root directory — see [`project_directory_path`].
    /// Needed separately from `config` for cache resolution
    /// ([`crate::engine::TaskEngine::with_cache_options`]).
    pub project_directory: PathBuf,
}

/// Loads `config_file`, resolves its `include`s, and resolves every
/// expression in the result — the whole config-to-usable-`Config` sequence
/// both binaries need, in one call, so neither has to know the order the
/// steps go in (includes before expressions; the config-vars file before
/// `--config-var`, which overrides it).
///
/// `config_var_overrides` is the merged result of a `--config-vars-file`
/// (load it with [`Config::load_config_vars_file`]) and any individually
/// supplied variables, the latter winning — merging them is the caller's
/// job, since only the caller knows what its own flags are called.
///
/// A missing file is an error here rather than an empty config: every
/// caller so far wants to fail fast, and doing it in one place means the
/// message is identical whichever binary is running.
pub async fn load_project(
    config_file: &Path,
    config_var_overrides: &HashMap<String, String>,
) -> Result<LoadedProject> {
    if !config_file.exists() {
        anyhow::bail!("Configuration file {:?} not found.", config_file);
    }
    let mut loaded = Config::load_from_file(config_file).await?;
    let base_path = base_path_for(config_file);
    let project_directory = project_directory_path(base_path)?;
    loaded.resolve_expressions(base_path, config_var_overrides)?;
    Ok(LoadedProject {
        config: loaded.config,
        project_directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_include::{FakeGitClient, GitIncludeCache};
    use std::io::Cursor;

    fn parse(yaml: &str) -> Config {
        noyalib::from_reader(Cursor::new(yaml.as_bytes())).expect("valid yaml")
    }

    /// Moved here from `ratect-compat`'s own `main.rs` when `base_path_for`
    /// became shared (`ratect` needs the identical rule) — the behavior is
    /// the same, only its home changed.
    #[test]
    fn base_path_for_a_bare_config_file_name_is_empty_not_dot() {
        // The default `-f batect.yml` case: `Path::parent()` on a bare
        // filename returns `Some("")`, not `None`, so the `.` fallback in
        // `base_path_for` never actually applies here — worth locking in
        // explicitly since it's easy to assume otherwise.
        assert_eq!(base_path_for(Path::new("batect.yml")), Path::new(""));
    }

    #[test]
    fn base_path_for_a_dot_relative_config_file_is_dot() {
        assert_eq!(base_path_for(Path::new("./batect.yml")), Path::new("."));
    }

    #[test]
    fn environment_values_accept_non_string_scalars() {
        // Batect coerces a YAML scalar to its string form; Ratect matches,
        // so `PORT: 8080` / `DEBUG: true` load rather than failing to parse
        // with a type mismatch. Surfaced by the task-with-unhealthy-dependency
        // conformance project (`NGINX_ENTRYPOINT_QUIET_LOGS: 1`).
        let config = parse(
            "project_name: p\n\
             containers:\n  \
               build-env:\n    \
                 image: alpine\n    \
                 environment:\n      \
                   PORT: 8080\n      \
                   RATIO: 1.5\n      \
                   DEBUG: true\n      \
                   NAME: already-a-string\n\
             tasks:\n  \
               the-task:\n    \
                 run:\n      \
                   container: build-env\n",
        );
        let env = config.containers["build-env"]
            .environment
            .as_ref()
            .expect("environment should be present");
        assert_eq!(env["PORT"], "8080");
        assert_eq!(env["RATIO"], "1.5");
        assert_eq!(env["DEBUG"], "true");
        assert_eq!(env["NAME"], "already-a-string");
    }

    #[test]
    fn base_path_for_a_config_file_in_a_subdirectory_is_that_subdirectory() {
        assert_eq!(
            base_path_for(Path::new("project/batect.yml")),
            Path::new("project")
        );
    }

    #[test]
    fn base_path_for_an_absolute_config_file_is_its_directory() {
        assert_eq!(
            base_path_for(Path::new("/abs/project/batect.yml")),
            Path::new("/abs/project")
        );
    }

    #[test]
    fn parses_containers_and_tasks() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
tasks:
  test:
    run:
      container: build-env
      command: echo hi
    prerequisites:
      - other
"#,
        );

        assert_eq!(config.project_name, "demo");

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.image.as_deref(), Some("alpine:3.18"));
        assert_eq!(
            container.volumes.as_ref().unwrap(),
            &vec![VolumeMount::Local(LocalVolumeMount {
                local: "code".to_string(),
                container: "/code".to_string(),
                options: None,
            })]
        );

        let task = config.tasks.get("test").unwrap();
        assert_eq!(task.run.as_ref().unwrap().container, "build-env");
        assert_eq!(
            task.run.as_ref().unwrap().command.as_deref(),
            Some("echo hi")
        );
        assert_eq!(
            task.prerequisites.as_ref().unwrap(),
            &vec!["other".to_string()]
        );
    }

    #[test]
    fn parses_a_task_with_only_prerequisites_and_no_run() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  other:
    run:
      container: build-env
  test:
    prerequisites:
      - other
"#,
        );

        let task = config.tasks.get("test").unwrap();
        assert!(task.run.is_none());
        assert_eq!(
            task.prerequisites.as_ref().unwrap(),
            &vec!["other".to_string()]
        );
    }

    #[test]
    fn parses_task_description_and_group() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    description: Runs the test suite
    group: verification
    run:
      container: build-env
"#,
        );

        let task = config.tasks.get("test").unwrap();
        assert_eq!(task.description.as_deref(), Some("Runs the test suite"));
        assert_eq!(task.group.as_deref(), Some("verification"));
    }

    #[test]
    fn task_description_and_group_default_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
"#,
        );

        let task = config.tasks.get("test").unwrap();
        assert_eq!(task.description, None);
        assert_eq!(task.group, None);
    }

    fn task_with_description_and_group(description: Option<&str>, group: Option<&str>) -> Task {
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
            description: description.map(str::to_string),
            group: group.map(str::to_string),
            customise: None,
        }
    }

    #[test]
    fn format_task_list_is_a_flat_sorted_list_when_no_task_declares_a_group() {
        let tasks = HashMap::from([
            (
                "build".to_string(),
                task_with_description_and_group(Some("Builds the app"), None),
            ),
            (
                "test".to_string(),
                task_with_description_and_group(None, None),
            ),
        ]);

        assert_eq!(
            format_task_list("demo", &tasks),
            "Tasks in demo:\n- build: Builds the app\n- test"
        );
    }

    #[test]
    fn format_task_list_groups_tasks_with_the_ungrouped_bucket_sorted_last() {
        let tasks = HashMap::from([
            (
                "lint".to_string(),
                task_with_description_and_group(None, Some("verification")),
            ),
            (
                "test".to_string(),
                task_with_description_and_group(Some("Runs the test suite"), Some("verification")),
            ),
            (
                "build".to_string(),
                task_with_description_and_group(None, Some("compilation")),
            ),
            (
                "clean".to_string(),
                task_with_description_and_group(None, None),
            ),
        ]);

        assert_eq!(
            format_task_list("demo", &tasks),
            "Tasks in demo:\n\
             \n\
             compilation:\n\
             - build\n\
             \n\
             verification:\n\
             - lint\n\
             - test: Runs the test suite\n\
             \n\
             Ungrouped tasks:\n\
             - clean"
        );
    }

    #[test]
    fn format_task_list_quiet_is_sorted_tab_separated_and_ignores_groups() {
        let tasks = HashMap::from([
            (
                "test".to_string(),
                task_with_description_and_group(Some("Runs the test suite"), Some("verification")),
            ),
            (
                "build".to_string(),
                task_with_description_and_group(None, Some("compilation")),
            ),
            (
                "clean".to_string(),
                // A whitespace-only description gets no tab either,
                // matching Batect's `isNotBlank` check.
                task_with_description_and_group(Some("   "), None),
            ),
        ]);

        assert_eq!(
            format_task_list_quiet(&tasks),
            "build\nclean\ntest\tRuns the test suite"
        );
    }

    #[test]
    fn resolve_expressions_errors_when_a_task_has_neither_run_nor_prerequisites() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test: {}
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Task 'test' must have at least one of 'run' or 'prerequisites'"));
    }

    #[test]
    fn resolve_expressions_errors_when_a_task_has_empty_prerequisites_and_no_run() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    prerequisites: []
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Task 'test' must have at least one of 'run' or 'prerequisites'"));
    }

    #[test]
    fn parses_task_level_dependencies() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - queue
"#,
        );

        let task = config.tasks.get("test").unwrap();
        assert_eq!(
            task.dependencies.as_ref().unwrap(),
            &vec!["queue".to_string()]
        );
    }

    #[test]
    fn resolve_expressions_errors_when_a_task_has_dependencies_but_no_run() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
  other:
    image: alpine:3.18
tasks:
  other:
    run:
      container: other
  test:
    prerequisites:
      - other
    dependencies:
      - queue
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("'run' is required if 'dependencies' is provided"));
    }

    #[test]
    fn resolve_expressions_errors_when_a_task_dependency_names_its_own_main_container() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - build-env
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        let message = result.unwrap_err().to_string();
        assert!(message.contains("Task 'test'"), "message: {message}");
        assert!(
            message
                .contains("both the main task container (via 'run') and a task-level dependency"),
            "message: {message}"
        );
    }

    #[test]
    fn parses_task_customise() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - queue
    customise:
      queue:
        environment:
          FOO: bar
        ports:
          - 6543:6543
        working_directory: /custom
"#,
        );

        let task = config.tasks.get("test").unwrap();
        let customisation = task.customise.as_ref().unwrap().get("queue").unwrap();
        assert_eq!(
            customisation.environment.as_ref().unwrap().get("FOO"),
            Some(&"bar".to_string())
        );
        assert_eq!(customisation.ports.as_ref().unwrap().len(), 1);
        assert_eq!(customisation.working_directory.as_deref(), Some("/custom"));
    }

    #[test]
    fn resolve_expressions_errors_when_customise_targets_the_main_task_container() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
    customise:
      build-env:
        working_directory: /custom
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Cannot apply customisations to main task container 'build-env'"));
    }

    #[test]
    fn resolve_expressions_errors_when_customise_targets_a_container_outside_the_tasks_graph() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  unrelated:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
    customise:
      unrelated:
        working_directory: /custom
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result.unwrap_err().to_string().contains(
            "Task 'test' has customisations for container 'unrelated', but the container \
             'unrelated' will not be started as part of the task"
        ));
    }

    #[test]
    fn resolve_expressions_allows_customise_for_a_task_level_dependency() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - queue
    customise:
      queue:
        working_directory: /custom
"#,
        );

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result.is_ok(), "{:?}", result.unwrap_err());
    }

    #[test]
    fn parses_build_directory_and_build_args() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_args:
      VERSION: "1.2.3"
tasks: {}
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.build_directory.as_deref(), Some("./docker"));
        assert_eq!(container.build_args.as_ref().unwrap()["VERSION"], "1.2.3");
    }

    #[test]
    fn parses_dockerfile_and_build_target() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    dockerfile: docker/Dockerfile.prod
    build_target: builder
tasks: {}
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.dockerfile.as_deref(),
            Some("docker/Dockerfile.prod")
        );
        assert_eq!(container.build_target.as_deref(), Some("builder"));
    }

    #[test]
    fn dockerfile_and_build_target_default_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
tasks: {}
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.dockerfile, None);
        assert_eq!(container.build_target, None);
    }

    #[test]
    fn parses_container_and_run_working_directory() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    working_directory: /app
tasks:
  test:
    run:
      container: build-env
      command: echo hi
      working_directory: /app/subdir
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.working_directory.as_deref(), Some("/app"));
        let task = config.tasks.get("test").unwrap();
        assert_eq!(
            task.run.as_ref().unwrap().working_directory.as_deref(),
            Some("/app/subdir")
        );
    }

    #[test]
    fn working_directory_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.working_directory, None);
        let task = config.tasks.get("test").unwrap();
        assert_eq!(task.run.as_ref().unwrap().working_directory, None);
    }

    #[test]
    fn yaml_anchors_aliases_and_merge_keys_are_resolved() {
        // Not a Ratect-specific feature to implement — anchors (`&name`),
        // aliases (`*name`), and merge keys (`<<:`) are core YAML syntax, so
        // any spec-compliant parser (including `noyalib`) resolves them
        // before Ratect's own `Deserialize` impls ever see the document.
        // Locked in here as a regression test rather than left as an
        // untested assumption, since a future parser swap could plausibly
        // regress it silently.
        let config = parse(
            r#"
project_name: demo
containers:
  build-env: &base
    image: alpine:3.18
    environment:
      SHARED_VAR: shared-value
  other-env:
    <<: *base
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let base = config.containers.get("build-env").unwrap();
        let merged = config.containers.get("other-env").unwrap();
        assert_eq!(merged.image, base.image);
        assert_eq!(merged.environment, base.environment);
        assert_eq!(
            merged.environment.as_ref().unwrap().get("SHARED_VAR"),
            Some(&"shared-value".to_string())
        );
    }

    #[test]
    fn parses_container_and_run_command() {
        let config = parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:16
    command: postgres -c max_connections=200
tasks:
  test:
    run:
      container: database
      command: echo hi
"#,
        );

        let container = config.containers.get("database").unwrap();
        assert_eq!(
            container.command.as_deref(),
            Some("postgres -c max_connections=200")
        );
        let task = config.tasks.get("test").unwrap();
        assert_eq!(
            task.run.as_ref().unwrap().command.as_deref(),
            Some("echo hi")
        );
    }

    #[test]
    fn container_command_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:16
tasks:
  test:
    run:
      container: database
"#,
        );

        let container = config.containers.get("database").unwrap();
        assert_eq!(container.command, None);
    }

    #[test]
    fn parses_container_and_run_entrypoint() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    entrypoint: /bin/sh -c
tasks:
  test:
    run:
      container: build-env
      command: echo hi
      entrypoint: /bin/bash -c
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.entrypoint.as_deref(), Some("/bin/sh -c"));
        let task = config.tasks.get("test").unwrap();
        assert_eq!(
            task.run.as_ref().unwrap().entrypoint.as_deref(),
            Some("/bin/bash -c")
        );
    }

    #[test]
    fn entrypoint_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.entrypoint, None);
        let task = config.tasks.get("test").unwrap();
        assert_eq!(task.run.as_ref().unwrap().entrypoint, None);
    }

    #[test]
    fn parses_labels() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    labels:
      com.example.owner: platform-team
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.labels,
            Some(HashMap::from([(
                "com.example.owner".to_string(),
                "platform-team".to_string()
            )]))
        );
    }

    #[test]
    fn labels_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.labels, None);
    }

    #[test]
    fn parses_capabilities_to_add_and_drop() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    capabilities_to_add:
      - NET_ADMIN
      - SYS_PTRACE
    capabilities_to_drop:
      - CHOWN
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.capabilities_to_add,
            Some(HashSet::from([Capability::NetAdmin, Capability::SysPtrace]))
        );
        assert_eq!(
            container.capabilities_to_drop,
            Some(HashSet::from([Capability::Chown]))
        );
    }

    #[test]
    fn parses_capabilities_missing_from_batects_own_stale_list() {
        // BPF/CHECKPOINT_RESTORE/PERFMON, added to Docker in 20.10 — after
        // Batect's own Capability enum was last updated. See the doc
        // comment on `Capability` for why this is a deliberate superset,
        // not a strict Batect port.
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    capabilities_to_add:
      - BPF
      - CHECKPOINT_RESTORE
      - PERFMON
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.capabilities_to_add,
            Some(HashSet::from([
                Capability::Bpf,
                Capability::CheckpointRestore,
                Capability::Perfmon,
            ]))
        );
    }

    #[test]
    fn capabilities_default_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.capabilities_to_add, None);
        assert_eq!(container.capabilities_to_drop, None);
    }

    #[test]
    fn parses_privileged() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    privileged: true
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.privileged, Some(true));
    }

    #[test]
    fn privileged_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.privileged, None);
    }

    #[test]
    fn parse_byte_size_handles_batects_own_format() {
        assert_eq!(parse_byte_size("0"), Ok(0));
        assert_eq!(parse_byte_size("128"), Ok(128));
        assert_eq!(parse_byte_size("128b"), Ok(128));
        assert_eq!(parse_byte_size("128B"), Ok(128));
        assert_eq!(parse_byte_size("128k"), Ok(128 * 1024));
        assert_eq!(parse_byte_size("128K"), Ok(128 * 1024));
        assert_eq!(parse_byte_size("128m"), Ok(128 * 1024 * 1024));
        assert_eq!(parse_byte_size("1g"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_byte_size(" 128m "), Ok(128 * 1024 * 1024));
    }

    #[test]
    fn parse_byte_size_rejects_invalid_input() {
        assert!(parse_byte_size("").is_err());
        assert!(parse_byte_size("m").is_err());
        assert!(parse_byte_size("128x").is_err());
        assert!(parse_byte_size("-128m").is_err());
        assert!(parse_byte_size("128m b").is_err());
    }

    #[test]
    fn parses_shm_size_as_a_batect_style_string() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    shm_size: 128m
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.shm_size, Some(128 * 1024 * 1024));
    }

    #[test]
    fn parses_shm_size_as_a_plain_integer() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    shm_size: 268435456
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.shm_size, Some(268435456));
    }

    #[test]
    fn shm_size_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.shm_size, None);
    }

    #[test]
    fn an_invalid_shm_size_string_is_rejected() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    shm_size: not-a-size
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn device_mapping_parse_string_handles_batects_own_format() {
        assert_eq!(
            DeviceMapping::parse_string("/dev/sda:/dev/xvda").unwrap(),
            DeviceMapping {
                local: "/dev/sda".to_string(),
                container: "/dev/xvda".to_string(),
                options: None,
            }
        );
        assert_eq!(
            DeviceMapping::parse_string("/dev/sda:/dev/xvda:rwm").unwrap(),
            DeviceMapping {
                local: "/dev/sda".to_string(),
                container: "/dev/xvda".to_string(),
                options: Some("rwm".to_string()),
            }
        );
    }

    #[test]
    fn device_mapping_parse_string_rejects_invalid_input() {
        assert!(DeviceMapping::parse_string("").is_err());
        assert!(DeviceMapping::parse_string("/dev/sda").is_err());
        assert!(DeviceMapping::parse_string("/dev/sda:/dev/xvda:rwm:extra").is_err());
        assert!(DeviceMapping::parse_string(":/dev/xvda").is_err());
        assert!(DeviceMapping::parse_string("/dev/sda:").is_err());
    }

    #[test]
    fn parses_devices_as_strings_and_objects() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    devices:
      - /dev/sda:/dev/xvda
      - local: /dev/sdb
        container: /dev/xvdb
        options: rwm
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.devices,
            Some(vec![
                DeviceMapping {
                    local: "/dev/sda".to_string(),
                    container: "/dev/xvda".to_string(),
                    options: None,
                },
                DeviceMapping {
                    local: "/dev/sdb".to_string(),
                    container: "/dev/xvdb".to_string(),
                    options: Some("rwm".to_string()),
                },
            ])
        );
    }

    #[test]
    fn devices_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.devices, None);
    }

    #[test]
    fn parses_enable_init_process() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    enable_init_process: true
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.enable_init_process, Some(true));
    }

    #[test]
    fn enable_init_process_defaults_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.enable_init_process, None);
    }

    #[test]
    fn parses_log_driver_and_log_options() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    log_driver: json-file
    log_options:
      max-size: 10m
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.log_driver.as_deref(), Some("json-file"));
        assert_eq!(container.log_options.as_ref().unwrap()["max-size"], "10m");
    }

    #[test]
    fn log_driver_and_log_options_default_to_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.log_driver, None);
        assert_eq!(container.log_options, None);
    }

    #[test]
    fn parses_image_pull_policy() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    image_pull_policy: Always
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.image_pull_policy, Some(ImagePullPolicy::Always));
    }

    #[test]
    fn image_pull_policy_defaults_to_none_which_means_if_not_present() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(container.image_pull_policy, None);
        assert_eq!(
            container.image_pull_policy.unwrap_or_default(),
            ImagePullPolicy::IfNotPresent
        );
    }

    #[test]
    fn an_unknown_image_pull_policy_is_rejected() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    image_pull_policy: WheneverIFeelLikeIt
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn an_unknown_capability_name_is_rejected() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    capabilities_to_add:
      - NOT_A_REAL_CAPABILITY
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn parses_build_secrets_environment_and_path_variants() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_secrets:
      token:
        environment: TOKEN
      cert:
        path: ./cert.pem
tasks: {}
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        let secrets = container.build_secrets.as_ref().unwrap();
        assert_eq!(
            secrets["token"],
            BuildSecret::Environment("TOKEN".to_string())
        );
        assert_eq!(secrets["cert"], BuildSecret::Path("./cert.pem".to_string()));
    }

    #[test]
    fn build_secret_with_both_environment_and_path_is_rejected() {
        let err = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_secrets:
      token:
        environment: TOKEN
        path: ./cert.pem
tasks: {}
"#,
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("either 'environment' or 'path', but both"));
    }

    #[test]
    fn build_secret_with_neither_environment_nor_path_is_rejected() {
        let err = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_secrets:
      token: {}
tasks: {}
"#,
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("either 'environment' or 'path', but neither"));
    }

    #[test]
    fn parses_build_ssh_default_agent() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_ssh:
      - id: default
tasks: {}
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        let agents = container.build_ssh.as_ref().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id.as_deref(), Some("default"));
        assert!(agents[0].paths.is_empty());
    }

    fn no_host_env(_: &str) -> Option<String> {
        None
    }

    /// Unwraps a `VolumeMount` expected to be `Local` — most tests only
    /// care about the `local`/`container` fields, not the enum wrapper.
    fn expect_local(mount: &VolumeMount) -> &LocalVolumeMount {
        match mount {
            VolumeMount::Local(local) => local,
            VolumeMount::Cache(_) => panic!("expected a local volume mount, got a cache mount"),
            VolumeMount::Tmpfs(_) => panic!("expected a local volume mount, got a tmpfs mount"),
        }
    }

    #[test]
    fn volume_mount_parses_two_part_string_as_local() {
        let mount = VolumeMount::parse_string("code:/code").unwrap();
        assert_eq!(
            mount,
            VolumeMount::Local(LocalVolumeMount {
                local: "code".to_string(),
                container: "/code".to_string(),
                options: None,
            })
        );
    }

    #[test]
    fn volume_mount_parses_three_part_string_with_options_as_local() {
        // Previously left completely unresolved (no interpolation at all —
        // see git history), since the old string-splitting resolver
        // couldn't tell an options suffix apart from a Windows
        // drive-letter host path. `VolumeMount` now separates
        // local/container/options at parse time (mirroring
        // `DeviceMapping::parse_string`), so this is unambiguous — Ratect
        // has no Windows support to preserve the old ambiguity for.
        let mount = VolumeMount::parse_string("code:/code:ro").unwrap();
        assert_eq!(
            mount,
            VolumeMount::Local(LocalVolumeMount {
                local: "code".to_string(),
                container: "/code".to_string(),
                options: Some("ro".to_string()),
            })
        );
    }

    #[test]
    fn volume_mount_rejects_an_empty_string() {
        assert!(VolumeMount::parse_string("").is_err());
    }

    #[test]
    fn volume_mount_rejects_a_string_with_too_many_colon_separated_parts() {
        let result = VolumeMount::parse_string("C:/data:/code:ro");
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_rejects_a_string_missing_a_container_path() {
        assert!(VolumeMount::parse_string("code").is_err());
    }

    #[test]
    fn volume_mount_parses_cache_object_form() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: cache
        name: gradle-cache
        container: /root/.gradle
        options: rw
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.volumes.as_ref().unwrap(),
            &vec![VolumeMount::Cache(CacheVolumeMount {
                name: "gradle-cache".to_string(),
                container: "/root/.gradle".to_string(),
                options: Some("rw".to_string()),
            })]
        );
    }

    #[test]
    fn volume_mount_cache_object_form_requires_name() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: cache
        container: /root/.gradle
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_cache_object_form_forbids_local() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: cache
        name: gradle-cache
        local: /host/path
        container: /root/.gradle
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_local_object_form_forbids_name() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - local: /host/path
        name: not-allowed-here
        container: /code
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_parses_tmpfs_object_form() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        container: /code/tmp
        options: ro
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.volumes.as_ref().unwrap(),
            &vec![VolumeMount::Tmpfs(TmpfsVolumeMount {
                container: "/code/tmp".to_string(),
                options: Some("ro".to_string()),
            })]
        );
    }

    #[test]
    fn volume_mount_parses_tmpfs_object_form_without_options() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.volumes.as_ref().unwrap(),
            &vec![VolumeMount::Tmpfs(TmpfsVolumeMount {
                container: "/code/tmp".to_string(),
                options: None,
            })]
        );
    }

    #[test]
    fn volume_mount_tmpfs_object_form_requires_container() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_tmpfs_object_form_forbids_local() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        local: /host/path
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_tmpfs_object_form_forbids_name() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        name: not-allowed-here
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_rejects_unknown_type() {
        let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: bogus
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
        let result: std::result::Result<Config, _> =
            noyalib::from_reader(Cursor::new(yaml.as_bytes()));
        assert!(result.is_err());
    }

    #[test]
    fn volume_mount_tmpfs_serializes_to_object_form() {
        let mount = VolumeMount::Tmpfs(TmpfsVolumeMount {
            container: "/code/tmp".to_string(),
            options: Some("ro".to_string()),
        });
        let json = serde_json::to_value(&mount).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "tmpfs",
                "container": "/code/tmp",
                "options": "ro",
            })
        );
    }

    #[test]
    fn resolve_expressions_makes_relative_local_volume_host_path_absolute() {
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
            local: "code".to_string(),
            container: "/code".to_string(),
            options: None,
        })]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        let VolumeMount::Local(resolved) =
            &config.containers["build-env"].volumes.as_ref().unwrap()[0]
        else {
            panic!("expected a local volume mount");
        };
        assert_eq!(resolved.local, "/base/code");
    }

    #[test]
    fn resolve_expressions_leaves_absolute_local_volume_host_path_unchanged() {
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
            local: "/already/absolute".to_string(),
            container: "/code".to_string(),
            options: None,
        })]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        let VolumeMount::Local(resolved) =
            &config.containers["build-env"].volumes.as_ref().unwrap()[0]
        else {
            panic!("expected a local volume mount");
        };
        assert_eq!(resolved.local, "/already/absolute");
    }

    #[test]
    fn resolve_expressions_interpolates_relative_local_volume_host_path_expression() {
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
            local: "<subdir".to_string(),
            container: "/code".to_string(),
            options: None,
        })]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: Some(HashMap::from([(
                "subdir".to_string(),
                ConfigVariable {
                    default: Some("code".to_string()),
                    description: None,
                },
            )])),
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        let VolumeMount::Local(resolved) =
            &config.containers["build-env"].volumes.as_ref().unwrap()[0]
        else {
            panic!("expected a local volume mount");
        };
        assert_eq!(resolved.local, "/base/code");
    }

    #[test]
    fn resolve_expressions_interpolates_absolute_local_volume_host_path_expression_without_prefixing_base_path(
    ) {
        // `<project_root` resolving to an absolute path must be used as-is,
        // not treated as a literal relative fragment of `base_path` the way
        // it would be if resolution happened before interpolation.
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
            local: "<project_root".to_string(),
            container: "/code".to_string(),
            options: None,
        })]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: Some(HashMap::from([(
                "project_root".to_string(),
                ConfigVariable {
                    default: Some("/abs/root".to_string()),
                    description: None,
                },
            )])),
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        let VolumeMount::Local(resolved) =
            &config.containers["build-env"].volumes.as_ref().unwrap()[0]
        else {
            panic!("expected a local volume mount");
        };
        assert_eq!(resolved.local, "/abs/root");
    }

    #[test]
    fn resolve_expressions_does_not_touch_cache_volume_mounts() {
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![VolumeMount::Cache(CacheVolumeMount {
            name: "gradle-cache".to_string(),
            container: "/root/.gradle".to_string(),
            options: None,
        })]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].volumes.as_ref().unwrap()[0],
            VolumeMount::Cache(CacheVolumeMount {
                name: "gradle-cache".to_string(),
                container: "/root/.gradle".to_string(),
                options: None,
            })
        );
    }

    #[test]
    fn resolve_path_makes_relative_path_absolute() {
        let resolved = resolve_path(
            "docker",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(resolved, "/base/docker");
    }

    #[test]
    fn resolve_path_cleans_dot_components_from_the_joined_path() {
        let resolved = resolve_path(
            "./docker",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            resolved, "/base/docker",
            "a leading './' shouldn't survive into the resolved path"
        );
    }

    #[test]
    fn resolve_path_leaves_absolute_path_unchanged() {
        let resolved = resolve_path(
            "/already/absolute",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(resolved, "/already/absolute");
    }

    #[test]
    fn resolve_path_interpolates_expression_before_resolving() {
        let config_vars =
            HashMap::from([("project_root".to_string(), Some("/abs/root".to_string()))]);
        let resolved = resolve_path(
            "<project_root",
            Path::new("/base"),
            &no_host_env,
            &config_vars,
            None,
        )
        .unwrap();
        assert_eq!(resolved, "/abs/root");
    }

    #[test]
    fn resolve_path_rejects_a_git_included_containers_absolute_path_outside_both_allowed_roots() {
        let boundary = GitBoundary {
            repo_dir: PathBuf::from("/repo"),
            remote: "https://example.com/bundle.git".to_string(),
            git_ref: "v1.0.0".to_string(),
        };
        let result = resolve_path(
            "/etc",
            Path::new("/repo/sub"),
            &no_host_env,
            &HashMap::new(),
            Some((&boundary, Path::new("/project"))),
        );
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));
    }

    #[test]
    fn resolve_path_rejects_a_git_included_containers_dot_dot_traversal_outside_both_allowed_roots()
    {
        let boundary = GitBoundary {
            repo_dir: PathBuf::from("/repo"),
            remote: "https://example.com/bundle.git".to_string(),
            git_ref: "v1.0.0".to_string(),
        };
        let result = resolve_path(
            "../../etc",
            Path::new("/repo/sub"),
            &no_host_env,
            &HashMap::new(),
            Some((&boundary, Path::new("/project"))),
        );
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));
    }

    #[test]
    fn resolve_path_allows_a_git_included_containers_path_within_the_clone_directory() {
        let boundary = GitBoundary {
            repo_dir: PathBuf::from("/repo"),
            remote: "https://example.com/bundle.git".to_string(),
            git_ref: "v1.0.0".to_string(),
        };
        let resolved = resolve_path(
            "sub/docker",
            Path::new("/repo"),
            &no_host_env,
            &HashMap::new(),
            Some((&boundary, Path::new("/project"))),
        )
        .unwrap();
        assert_eq!(resolved, "/repo/sub/docker");
    }

    #[test]
    fn resolve_path_allows_a_git_included_containers_path_under_the_project_directory() {
        // A shared bundle referencing the caller's own project directory
        // (e.g. `<{batect.project_directory}/output`) is a legitimate use
        // case, not an escape — the project directory is the caller's own,
        // fully-trusted tree, distinct from the untrusted repository the
        // container definition itself came from.
        let boundary = GitBoundary {
            repo_dir: PathBuf::from("/repo"),
            remote: "https://example.com/bundle.git".to_string(),
            git_ref: "v1.0.0".to_string(),
        };
        let resolved = resolve_path(
            "/project/output",
            Path::new("/repo"),
            &no_host_env,
            &HashMap::new(),
            Some((&boundary, Path::new("/project"))),
        )
        .unwrap();
        assert_eq!(resolved, "/project/output");
    }

    fn container_with_build(
        build_directory: &str,
        build_args: HashMap<String, String>,
    ) -> Container {
        Container {
            image: None,
            image_pull_policy: None,
            build_directory: Some(build_directory.to_string()),
            build_args: Some(build_args),
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
            health_check: None,
            setup_commands: None,
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
        }
    }

    #[test]
    fn resolve_expressions_resolves_build_directory_relative_path() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build("docker", HashMap::new()),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"].build_directory.as_deref(),
            Some("/base/docker")
        );
    }

    #[test]
    fn resolve_expressions_interpolates_build_args() {
        let mut build_args = HashMap::new();
        build_args.insert("MESSAGE".to_string(), "$HOST_VAR".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build("./docker", build_args),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                |name| (name == "HOST_VAR").then(|| "host-value".to_string()),
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"].build_args.as_ref().unwrap()["MESSAGE"],
            "host-value"
        );
    }

    #[test]
    fn resolve_expressions_resolves_build_secret_path_relative_to_base() {
        let mut container = container_with_build("./docker", HashMap::new());
        container.build_secrets = Some(HashMap::from([(
            "cert".to_string(),
            BuildSecret::Path("./cert.pem".to_string()),
        )]));
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"]
                .build_secrets
                .as_ref()
                .unwrap()["cert"],
            BuildSecret::Path("/base/cert.pem".to_string())
        );
    }

    #[test]
    fn resolve_expressions_leaves_build_secret_environment_name_unresolved() {
        let mut container = container_with_build("./docker", HashMap::new());
        container.build_secrets = Some(HashMap::from([(
            "token".to_string(),
            BuildSecret::Environment("$HOST_VAR".to_string()),
        )]));
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                |name| (name == "HOST_VAR").then(|| "host-value".to_string()),
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"]
                .build_secrets
                .as_ref()
                .unwrap()["token"],
            BuildSecret::Environment("$HOST_VAR".to_string())
        );
    }

    fn container_with_build_ssh(agents: Vec<SshAgent>) -> Container {
        let mut container = container_with_build("./docker", HashMap::new());
        container.build_ssh = Some(agents);
        container
    }

    #[test]
    fn resolve_expressions_accepts_a_single_default_ssh_agent() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build_ssh(vec![SshAgent {
                    id: Some("default".to_string()),
                    paths: Vec::new(),
                }]),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();
    }

    #[test]
    fn resolve_expressions_accepts_a_build_ssh_agent_with_no_id() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build_ssh(vec![SshAgent {
                    id: None,
                    paths: Vec::new(),
                }]),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();
    }

    #[test]
    fn resolve_expressions_rejects_more_than_one_build_ssh_agent() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build_ssh(vec![
                    SshAgent {
                        id: Some("default".to_string()),
                        paths: Vec::new(),
                    },
                    SshAgent {
                        id: Some("other".to_string()),
                        paths: Vec::new(),
                    },
                ]),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let err = config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap_err();

        assert!(format!("{err:#}").contains("only supports forwarding a single SSH agent"));
    }

    #[test]
    fn resolve_expressions_rejects_a_non_default_build_ssh_agent_id() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build_ssh(vec![SshAgent {
                    id: Some("other".to_string()),
                    paths: Vec::new(),
                }]),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let err = config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap_err();

        assert!(format!("{err:#}").contains("implicit 'default' SSH agent id"));
    }

    #[test]
    fn resolve_expressions_rejects_build_ssh_explicit_key_paths() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_build_ssh(vec![SshAgent {
                    id: None,
                    paths: vec!["~/.ssh/id_rsa".to_string()],
                }]),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let err = config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap_err();

        assert!(format!("{err:#}").contains("not explicit key files"));
    }

    fn container_with_run_as_current_user(
        enabled: bool,
        home_directory: Option<&str>,
    ) -> Container {
        Container {
            image: Some("alpine:3.18".to_string()),
            image_pull_policy: None,
            build_directory: None,
            build_args: None,
            dockerfile: None,
            build_target: None,
            build_secrets: None,
            build_ssh: None,
            volumes: None,
            dependencies: None,
            environment: None,
            run_as_current_user: Some(RunAsCurrentUser {
                enabled,
                home_directory: home_directory.map(|s| s.to_string()),
            }),
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
            health_check: None,
            setup_commands: None,
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
        }
    }

    fn config_with_container(container: Container) -> Config {
        Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        }
    }

    #[test]
    fn parses_run_as_current_user() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    run_as_current_user:
      enabled: true
      home_directory: /home/container-user
tasks: {}
"#,
        );

        let run_as_current_user = config.containers["build-env"]
            .run_as_current_user
            .as_ref()
            .unwrap();
        assert!(run_as_current_user.enabled);
        assert_eq!(
            run_as_current_user.home_directory.as_deref(),
            Some("/home/container-user")
        );
    }

    #[test]
    fn parses_additional_hostnames_and_hosts() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    additional_hostnames:
      - db-alias
      - cache-alias
    additional_hosts:
      external-service: 10.0.0.1
tasks: {}
"#,
        );

        let container = &config.containers["build-env"];
        assert_eq!(
            container.additional_hostnames,
            Some(vec!["db-alias".to_string(), "cache-alias".to_string()])
        );
        assert_eq!(
            container.additional_hosts,
            Some(HashMap::from([(
                "external-service".to_string(),
                "10.0.0.1".to_string()
            )]))
        );
    }

    #[test]
    fn parses_absent_additional_hostnames_and_hosts_as_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks: {}
"#,
        );

        let container = &config.containers["build-env"];
        assert_eq!(container.additional_hostnames, None);
        assert_eq!(container.additional_hosts, None);
    }

    #[test]
    fn resolve_expressions_leaves_additional_hostnames_and_hosts_untouched() {
        let mut config = config_with_container(Container {
            additional_hostnames: Some(vec!["db-alias".to_string()]),
            additional_hosts: Some(HashMap::from([(
                "external-service".to_string(),
                "10.0.0.1".to_string(),
            )])),
            ..container_with_build("docker", HashMap::new())
        });

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();

        let container = &config.containers["build-env"];
        assert_eq!(
            container.additional_hostnames,
            Some(vec!["db-alias".to_string()])
        );
        assert_eq!(
            container.additional_hosts,
            Some(HashMap::from([(
                "external-service".to_string(),
                "10.0.0.1".to_string()
            )]))
        );
    }

    fn port_mapping(local: (u16, u16), container: (u16, u16), protocol: &str) -> PortMapping {
        PortMapping {
            local: PortRange {
                from: local.0,
                to: local.1,
            },
            container: PortRange {
                from: container.0,
                to: container.1,
            },
            protocol: protocol.to_string(),
        }
    }

    #[test]
    fn parses_ports_string_form() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - "8080:80"
      - "9000:9000/udp"
tasks: {}
"#,
        );

        let container = &config.containers["build-env"];
        assert_eq!(
            container.ports,
            Some(vec![
                port_mapping((8080, 8080), (80, 80), "tcp"),
                port_mapping((9000, 9000), (9000, 9000), "udp"),
            ])
        );
    }

    #[test]
    fn parses_ports_string_form_with_ranges() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - "8000-8002:9000-9002/udp"
tasks: {}
"#,
        );

        assert_eq!(
            config.containers["build-env"].ports,
            Some(vec![port_mapping((8000, 8002), (9000, 9002), "udp")])
        );
    }

    #[test]
    fn parses_ports_object_form() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: 8080
        container: 80
      - local: 8000-8002
        container: 9000-9002
        protocol: udp
tasks: {}
"#,
        );

        assert_eq!(
            config.containers["build-env"].ports,
            Some(vec![
                port_mapping((8080, 8080), (80, 80), "tcp"),
                port_mapping((8000, 8002), (9000, 9002), "udp"),
            ])
        );
    }

    fn try_parse(yaml: &str) -> Result<Config> {
        noyalib::from_reader(Cursor::new(yaml.as_bytes())).context("failed to parse")
    }

    #[test]
    fn parsing_ports_string_form_rejects_mismatched_range_sizes() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - "8000-8002:9000-9001"
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parsing_ports_object_form_rejects_mismatched_range_sizes() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: 8000-8002
        container: 9000-9001
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_absent_ports_as_none() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks: {}
"#,
        );

        assert_eq!(config.containers["build-env"].ports, None);
    }

    #[test]
    fn parses_health_check_config() {
        let config = parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      command: pg_isready -h localhost
      interval: 2s
      retries: 5
      start_period: 1m30s
      timeout: 500ms
tasks: {}
"#,
        );

        let health_check = config.containers["database"].health_check.as_ref().unwrap();
        assert_eq!(
            health_check.command.as_deref(),
            Some("pg_isready -h localhost")
        );
        assert_eq!(
            health_check.interval,
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(health_check.retries, Some(5));
        assert_eq!(
            health_check.start_period,
            Some(std::time::Duration::from_secs(90))
        );
        assert_eq!(
            health_check.timeout,
            Some(std::time::Duration::from_millis(500))
        );
    }

    #[test]
    fn parses_partial_health_check_config() {
        let config = parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      command: pg_isready
tasks: {}
"#,
        );

        let health_check = config.containers["database"].health_check.as_ref().unwrap();
        assert_eq!(health_check.command.as_deref(), Some("pg_isready"));
        assert_eq!(health_check.interval, None);
        assert_eq!(health_check.retries, None);
        assert_eq!(health_check.start_period, None);
        assert_eq!(health_check.timeout, None);
    }

    #[test]
    fn parsing_health_check_rejects_invalid_duration() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      interval: 2 seconds
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parsing_health_check_rejects_unknown_fields() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      cmd: pg_isready
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parses_setup_commands() {
        let config = parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:13
    setup_commands:
      - command: ./apply-migrations.sh
      - command: ./seed-data.sh
        working_directory: /setup
tasks: {}
"#,
        );

        let commands = config.containers["database"]
            .setup_commands
            .as_ref()
            .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].command, "./apply-migrations.sh");
        assert_eq!(commands[0].working_directory, None);
        assert_eq!(commands[1].command, "./seed-data.sh");
        assert_eq!(commands[1].working_directory.as_deref(), Some("/setup"));
    }

    #[test]
    fn parsing_setup_commands_rejects_missing_command() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  database:
    image: postgres:13
    setup_commands:
      - working_directory: /setup
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_duration_handles_batect_formats() {
        use std::time::Duration;

        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("+0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("-0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("100ns").unwrap(), Duration::from_nanos(100));
        assert_eq!(parse_duration("2us").unwrap(), Duration::from_micros(2));
        assert_eq!(parse_duration("2µs").unwrap(), Duration::from_micros(2));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("2.5s").unwrap(), Duration::from_millis(2500));
        assert_eq!(parse_duration(".5s").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2.s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5400));
        assert_eq!(
            parse_duration("1h2m3s4ms").unwrap(),
            Duration::from_millis(3_723_004)
        );
    }

    #[test]
    fn parse_duration_rejects_invalid_input() {
        for invalid in [
            "",
            "2",
            "s",
            ".s",
            "2 s",
            "2 seconds",
            "2S",
            "abc",
            "2ss",
            "2.5.3s",
            "-2s",
            "2s-1s",
        ] {
            assert!(
                parse_duration(invalid).is_err(),
                "expected '{invalid}' to be rejected"
            );
        }
    }

    #[test]
    fn resolve_expressions_leaves_ports_untouched() {
        let mut config = config_with_container(Container {
            ports: Some(vec![port_mapping((8080, 8080), (80, 80), "tcp")]),
            ..container_with_build("docker", HashMap::new())
        });

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"].ports,
            Some(vec![port_mapping((8080, 8080), (80, 80), "tcp")])
        );
    }

    #[test]
    fn port_range_parses_a_single_port() {
        assert_eq!(
            PortRange::parse("8080").unwrap(),
            PortRange {
                from: 8080,
                to: 8080
            }
        );
    }

    #[test]
    fn port_range_parses_a_range() {
        assert_eq!(
            PortRange::parse("8000-8002").unwrap(),
            PortRange {
                from: 8000,
                to: 8002
            }
        );
    }

    #[test]
    fn port_range_rejects_zero() {
        assert!(PortRange::parse("0").is_err());
    }

    #[test]
    fn port_range_rejects_descending_bounds() {
        assert!(PortRange::parse("8002-8000").is_err());
    }

    #[test]
    fn port_range_rejects_non_numeric_input() {
        assert!(PortRange::parse("abc").is_err());
    }

    #[test]
    fn port_mapping_expand_yields_one_triple_for_a_single_port() {
        let mapping = port_mapping((8080, 8080), (80, 80), "tcp");
        assert_eq!(mapping.expand(), vec![(8080, 80, "tcp".to_string())]);
    }

    #[test]
    fn port_mapping_expand_zips_a_range_by_position() {
        let mapping = port_mapping((8000, 8002), (9000, 9002), "udp");
        assert_eq!(
            mapping.expand(),
            vec![
                (8000, 9000, "udp".to_string()),
                (8001, 9001, "udp".to_string()),
                (8002, 9002, "udp".to_string()),
            ]
        );
    }

    #[test]
    fn port_mapping_parse_string_rejects_an_empty_definition() {
        assert!(PortMapping::parse_string("")
            .unwrap_err()
            .to_string()
            .contains("cannot be empty"));
    }

    #[test]
    fn port_mapping_parse_string_rejects_a_definition_without_a_colon() {
        assert!(PortMapping::parse_string("8080").is_err());
    }

    #[test]
    fn port_mapping_parse_string_rejects_an_empty_component() {
        assert!(PortMapping::parse_string("8080:80/").is_err());
        assert!(PortMapping::parse_string(":80").is_err());
        assert!(PortMapping::parse_string("8080:").is_err());
    }

    #[test]
    fn parsing_ports_object_form_rejects_an_unknown_field() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: 8080
        container: 80
        banana: 1
tasks: {}
"#,
        );
        // `{:?}` renders anyhow's full context chain — the serde detail
        // naming the field sits below `try_parse`'s own outer context.
        assert!(format!("{:?}", result.unwrap_err()).contains("banana"));
    }

    #[test]
    fn parsing_ports_object_form_rejects_a_missing_local_or_container() {
        for object in ["local: 8080", "container: 80"] {
            let result = try_parse(&format!(
                r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - {object}
tasks: {{}}
"#,
            ));
            assert!(result.is_err(), "'{object}' alone should be rejected");
        }
    }

    #[test]
    fn parsing_a_port_mapping_that_is_neither_string_nor_object_is_an_error() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - true
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn parsing_a_port_range_that_is_neither_number_nor_string_is_an_error() {
        let result = try_parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: true
        container: 80
tasks: {}
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn port_mapping_serializes_to_its_string_form_and_round_trips() {
        let single = port_mapping((8080, 8080), (80, 80), "tcp");
        let ranged = port_mapping((8000, 8002), (9000, 9002), "udp");

        for mapping in [single, ranged] {
            let yaml = noyalib::to_string(&mapping).expect("should serialize");
            let reparsed: PortMapping = noyalib::from_reader(Cursor::new(yaml.as_bytes()))
                .expect("the serialized form should re-parse");
            assert_eq!(reparsed, mapping, "round-trip through: {yaml}");
        }
    }

    #[test]
    fn resolve_expressions_errors_when_run_as_current_user_enabled_without_home_directory() {
        let mut config = config_with_container(container_with_run_as_current_user(true, None));

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no 'home_directory' was provided"));
    }

    #[test]
    fn resolve_expressions_errors_when_home_directory_given_without_run_as_current_user_enabled() {
        let mut config = config_with_container(container_with_run_as_current_user(
            false,
            Some("/home/container-user"),
        ));

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("'run_as_current_user.enabled' is not true"));
    }

    #[test]
    fn resolve_expressions_errors_when_run_as_current_user_home_directory_is_not_absolute() {
        let mut config = config_with_container(container_with_run_as_current_user(
            true,
            Some("home/container-user"),
        ));

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("is not an absolute path"));
    }

    #[test]
    fn resolve_expressions_errors_when_run_as_current_user_home_directory_contains_a_colon() {
        // SEC-002: a ':' would shift the fields of the colon-delimited
        // /etc/passwd/etc/shadow line `home_directory` is interpolated into.
        let mut config = config_with_container(container_with_run_as_current_user(
            true,
            Some("/home/x:0:0:root:/root:/bin/sh"),
        ));

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("contains a ':' or a control character"));
    }

    #[test]
    fn resolve_expressions_errors_when_run_as_current_user_home_directory_contains_a_newline() {
        // SEC-002: a newline would inject an entirely new, attacker-chosen
        // /etc/passwd/etc/shadow entry rather than just extending this one.
        let mut config = config_with_container(container_with_run_as_current_user(
            true,
            Some("/home/x\nbackdoor:x:0:0::/root:/bin/sh"),
        ));

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        );

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("contains a ':' or a control character"));
    }

    #[test]
    fn resolve_expressions_interpolates_run_as_current_user_home_directory() {
        let mut config = config_with_container(container_with_run_as_current_user(
            true,
            Some("/home/$HOST_VAR"),
        ));

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                |name| (name == "HOST_VAR").then(|| "container-user".to_string()),
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"]
                .run_as_current_user
                .as_ref()
                .unwrap()
                .home_directory
                .as_deref(),
            Some("/home/container-user")
        );
    }

    #[test]
    fn resolve_expressions_leaves_disabled_run_as_current_user_unaffected() {
        let mut config = config_with_container(container_with_run_as_current_user(false, None));

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                no_host_env,
            )
            .unwrap();

        let run_as_current_user = config.containers["build-env"]
            .run_as_current_user
            .as_ref()
            .unwrap();
        assert!(!run_as_current_user.enabled);
        assert_eq!(run_as_current_user.home_directory, None);
    }

    /// A fresh, unique scratch directory for tests that need to write real
    /// files to disk (e.g. to exercise `load_from_file`'s own file I/O,
    /// not just YAML parsing). Caller is responsible for cleanup via
    /// `std::fs::remove_dir_all`.
    ///
    /// Includes a monotonic counter alongside the PID/timestamp: tests run
    /// in parallel by default, and two calls landing in the same clock tick
    /// (observed in practice — coarser than nanosecond resolution on some
    /// platforms) would otherwise collide on the same directory and produce
    /// flaky failures.
    fn unique_temp_dir() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let dir = std::env::temp_dir().join(format!(
            "ratect-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            count
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn load_from_file_then_resolve_expressions_resolves_paths() {
        let dir = unique_temp_dir();
        let config_path = dir.join("batect.yml");
        std::fs::write(
            &config_path,
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
tasks: {}
"#,
        )
        .unwrap();

        let mut loaded = Config::load_from_file(&config_path).await.unwrap();
        loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

        let volume = expect_local(
            &loaded.config.containers["build-env"]
                .volumes
                .as_ref()
                .unwrap()[0],
        );
        assert_eq!(volume.local, dir.join("code").display().to_string());
        assert_eq!(volume.container, "/code");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn load_from_file_missing_file_errors() {
        let result = Config::load_from_file(Path::new("/nonexistent/batect.yml")).await;
        assert!(result.is_err());
    }

    /// `load_project` is the whole load-resolve sequence both binaries use,
    /// so this proves the steps happen *and* happen in the right order: the
    /// volume path below is only correct if includes were merged before
    /// expressions were resolved, and the override only wins if it's
    /// applied at resolution rather than being ignored.
    #[tokio::test]
    async fn load_project_resolves_includes_expressions_and_the_project_directory() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("containers.yml"),
            r#"
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
    environment:
      GREETING: <greeting
"#,
        )
        .unwrap();
        let config_path = dir.join("batect.yml");
        std::fs::write(
            &config_path,
            r#"
project_name: demo
include:
  - containers.yml
config_variables:
  greeting:
    default: from-the-default
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
        )
        .unwrap();

        let overrides = HashMap::from([("greeting".to_string(), "from-the-override".to_string())]);
        let project = load_project(&config_path, &overrides).await.unwrap();

        assert_eq!(project.project_directory, dir.clean());
        let container = &project.config.containers["build-env"];
        assert_eq!(
            container.environment.as_ref().unwrap()["GREETING"],
            "from-the-override"
        );
        assert_eq!(
            expect_local(&container.volumes.as_ref().unwrap()[0]).local,
            dir.join("code").display().to_string()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn load_project_fails_fast_when_the_config_file_is_missing() {
        let error = load_project(Path::new("/nonexistent/batect.yml"), &HashMap::new())
            .await
            .expect_err("a missing config file should be an error, not an empty config");
        assert!(
            error.to_string().contains("not found"),
            "the error should say the file is missing: {error}"
        );
    }

    #[tokio::test]
    async fn load_from_file_unsupported_key_errors() {
        let dir = unique_temp_dir();
        let config_path = dir.join("batect.yml");
        std::fs::write(
            &config_path,
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    not_a_real_field: json-file
tasks: {}
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&config_path).await;
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn include_merges_containers_tasks_and_config_variables_from_another_file() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
include:
  - extra.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("extra.yml"),
            r#"
tasks:
  extra-task:
    run:
      container: build-env
config_variables:
  extra_var:
    default: value
"#,
        )
        .unwrap();

        let loaded = Config::load_from_file(&dir.join("batect.yml"))
            .await
            .unwrap();
        assert!(loaded.config.containers.contains_key("build-env"));
        assert!(loaded.config.tasks.contains_key("extra-task"));
        assert!(loaded
            .config
            .config_variables
            .as_ref()
            .unwrap()
            .contains_key("extra_var"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn nested_includes_are_resolved_transitively() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - a.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("a.yml"),
            r#"
containers:
  build-env:
    image: alpine:3.18
include:
  - nested/b.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("nested/b.yml"),
            r#"
tasks:
  deep-task:
    run:
      container: build-env
"#,
        )
        .unwrap();

        let loaded = Config::load_from_file(&dir.join("batect.yml"))
            .await
            .unwrap();
        assert!(loaded.config.containers.contains_key("build-env"));
        assert!(loaded.config.tasks.contains_key("deep-task"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_file_included_from_two_places_is_only_loaded_once() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - a.yml
  - b.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("a.yml"),
            r#"
include:
  - shared.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("b.yml"),
            r#"
include:
  - shared.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("shared.yml"),
            r#"
tasks:
  shared-task:
    run:
      container: build-env
"#,
        )
        .unwrap();

        // If `shared.yml` were (incorrectly) loaded twice, this would fail
        // with a "defined in multiple files" error instead.
        let loaded = Config::load_from_file(&dir.join("batect.yml"))
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("shared-task"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_task_defined_in_two_different_files_is_an_error() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
tasks:
  build:
    run:
      container: build-env
include:
  - extra.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("extra.yml"),
            r#"
tasks:
  build:
    run:
      container: build-env
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&dir.join("batect.yml")).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("build"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn project_name_in_an_included_file_is_an_error() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - extra.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("extra.yml"),
            r#"
project_name: not-allowed
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&dir.join("batect.yml")).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("project_name"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_missing_include_path_errors_clearly() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - does-not-exist.yml
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&dir.join("batect.yml")).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("does not exist"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_include_path_that_is_a_directory_errors_clearly() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(dir.join("a-directory")).unwrap();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - a-directory
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&dir.join("batect.yml")).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("is not a file"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_relative_volume_path_in_an_included_file_resolves_against_its_own_directory() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - nested/extra.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("nested/extra.yml"),
            r#"
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
"#,
        )
        .unwrap();

        let mut loaded = Config::load_from_file(&dir.join("batect.yml"))
            .await
            .unwrap();
        loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

        let volume = expect_local(
            &loaded.config.containers["build-env"]
                .volumes
                .as_ref()
                .unwrap()[0],
        );
        assert_eq!(
            volume.local,
            dir.join("nested").join("code").display().to_string()
        );
        assert_eq!(volume.container, "/code");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn project_directory_var_in_an_included_file_resolves_to_the_root_directory() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - nested/extra.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("nested/extra.yml"),
            r#"
containers:
  build-env:
    image: alpine:3.18
    environment:
      PROJECT_DIR: <batect.project_directory
"#,
        )
        .unwrap();

        let mut loaded = Config::load_from_file(&dir.join("batect.yml"))
            .await
            .unwrap();
        loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

        let value = &loaded.config.containers["build-env"]
            .environment
            .as_ref()
            .unwrap()["PROJECT_DIR"];
        assert_eq!(*value, dir.display().to_string());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn include_accepts_both_bare_string_and_object_form() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("extra.yml"),
            r#"
tasks:
  extra-task:
    run:
      container: build-env
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("string-form.yml"),
            r#"
project_name: demo
include:
  - extra.yml
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("object-form.yml"),
            r#"
project_name: demo
include:
  - type: file
    path: extra.yml
"#,
        )
        .unwrap();

        let loaded = Config::load_from_file(&dir.join("string-form.yml"))
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("extra-task"));

        let loaded = Config::load_from_file(&dir.join("object-form.yml"))
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("extra-task"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn include_with_unsupported_type_errors_clearly() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: bundle
    path: bundle.yml
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&dir.join("batect.yml")).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("not supported"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_git_include_clones_the_repo_and_merges_containers_and_tasks() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: bundle.yml
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "bundle.yml".to_string(),
            r#"
containers:
  bundled:
    image: alpine:3.18
tasks:
  bundled-task:
    run:
      container: bundled
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        assert!(loaded.config.containers.contains_key("bundled"));
        assert!(loaded.config.tasks.contains_key("bundled-task"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_include_without_an_explicit_path_defaults_to_batect_bundle_yml() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
tasks:
  bundled-task:
    run:
      container: build-env
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("bundled-task"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_relative_volume_path_in_a_git_included_file_resolves_against_the_clone_directory() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
containers:
  bundled:
    image: alpine:3.18
    volumes:
      - code:/code
tasks: {}
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

        let volume = expect_local(
            &loaded.config.containers["bundled"]
                .volumes
                .as_ref()
                .unwrap()[0],
        );
        let clone_dir = cache_root.join(crate::git_include::cache_key(
            "https://example.com/bundle.git",
            "v1.0.0",
        ));
        assert_eq!(volume.local, clone_dir.join("code").display().to_string());
        assert_eq!(volume.container, "/code");

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_included_containers_volume_with_an_absolute_host_path_outside_the_clone_and_project_directory_is_rejected(
    ) {
        // SEC-001: the 0.8.0 fix (commit 6fcd0b8) only contained an
        // `include`'s own `path` field to its Git repository's clone
        // directory — it didn't stop a container *declared inside* a
        // Git-included bundle from mounting an arbitrary host path via
        // `volumes`, which is exactly what this reproduces.
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
containers:
  bundled:
    image: alpine:3.18
    volumes:
      - /:/hostroot
tasks: {}
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        let result = loaded.resolve_expressions(&dir, &HashMap::new());
        assert!(
            format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"),
            "a container declared inside a Git include must not be able to mount an arbitrary \
             host path"
        );

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_included_containers_build_directory_escaping_via_dot_dot_traversal_is_rejected()
    {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
containers:
  bundled:
    build_directory: ../../../../../../etc
tasks: {}
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        let result = loaded.resolve_expressions(&dir, &HashMap::new());
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_included_containers_build_secret_path_escaping_via_dot_dot_traversal_is_rejected(
    ) {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
containers:
  bundled:
    build_directory: .
    build_secrets:
      token:
        path: ../../../../../../etc/passwd
tasks: {}
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        let result = loaded.resolve_expressions(&dir, &HashMap::new());
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_included_containers_volume_referencing_the_project_directory_is_allowed() {
        // Referencing the caller's own project directory (as opposed to an
        // arbitrary host path) is a legitimate, expected use of a shared
        // bundle — e.g. mounting an output directory back into the
        // project. It must stay allowed even though it's outside the Git
        // repository's own clone directory.
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
containers:
  bundled:
    image: alpine:3.18
    volumes:
      - <{batect.project_directory}/output:/output
tasks: {}
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

        let volume = expect_local(
            &loaded.config.containers["bundled"]
                .volumes
                .as_ref()
                .unwrap()[0],
        );
        assert_eq!(volume.local, dir.join("output").display().to_string());
        assert_eq!(volume.container, "/output");

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_local_include_inside_a_git_bundle_resolves_against_the_clone_directory() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            r#"
include:
  - nested.yml
"#
            .to_string(),
        );
        files.insert(
            "nested.yml".to_string(),
            r#"
tasks:
  nested-task:
    run:
      container: build-env
"#
            .to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("nested-task"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_includes_own_path_escaping_via_an_absolute_path_is_rejected() {
        let dir = unique_temp_dir();
        let outside = unique_temp_dir();
        std::fs::write(
            outside.join("secret.yml"),
            "tasks:\n  leaked-task:\n    run:\n      container: build-env\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("batect.yml"),
            format!(
                r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: {}
"#,
                outside.join("secret.yml").display()
            ),
        )
        .unwrap();

        // The bundle itself doesn't even need to contain the target file —
        // an absolute `path` bypasses the clone directory entirely via
        // `PathBuf::join`'s own documented behavior, which is exactly the
        // bug being guarded against here.
        let git = FakeGitClient::new().with_files(
            "https://example.com/bundle.git",
            "v1.0.0",
            HashMap::new(),
        );
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_includes_own_path_escaping_via_dot_dot_traversal_is_rejected() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: ../../../../../../etc/passwd
"#,
        )
        .unwrap();

        let git = FakeGitClient::new().with_files(
            "https://example.com/bundle.git",
            "v1.0.0",
            HashMap::new(),
        );
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_nested_local_include_inside_a_git_bundle_escaping_the_clone_is_rejected() {
        let dir = unique_temp_dir();
        let outside = unique_temp_dir();
        std::fs::write(
            outside.join("secret.yml"),
            "tasks:\n  leaked-task:\n    run:\n      container: build-env\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "batect-bundle.yml".to_string(),
            format!(
                "include:\n  - path: {}\n",
                outside.join("secret.yml").display()
            ),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_nested_git_include_inside_a_git_bundle_still_works() {
        // A Git-included bundle composing in *another* Git repo (a fresh
        // boundary of its own) must not be rejected by the containment
        // check meant for local-file escapes.
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/outer.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let mut outer_files = HashMap::new();
        outer_files.insert(
            "batect-bundle.yml".to_string(),
            r#"
include:
  - type: git
    repo: https://example.com/inner.git
    ref: v2.0.0
"#
            .to_string(),
        );
        let mut inner_files = HashMap::new();
        inner_files.insert(
            "batect-bundle.yml".to_string(),
            "tasks:\n  inner-task:\n    run:\n      container: build-env\n".to_string(),
        );
        let git = FakeGitClient::new()
            .with_files("https://example.com/outer.git", "v1.0.0", outer_files)
            .with_files("https://example.com/inner.git", "v2.0.0", inner_files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("inner-task"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_symlink_inside_a_git_bundle_escaping_the_clone_is_rejected() {
        let dir = unique_temp_dir();
        let outside = unique_temp_dir();
        std::fs::write(
            outside.join("secret.yml"),
            "tasks:\n  leaked-task:\n    run:\n      container: build-env\n",
        )
        .unwrap();

        // A real repo (needs real git — symlinks committed to a repo are
        // what this test is actually exercising) whose own bundle file
        // is a symlink pointing outside the clone entirely.
        let repo_dir = unique_temp_dir();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_dir)
                .args(args)
                .status()
                .expect("git must be installed to run this test");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        // The host's global git config must not leak into the scratch repo's
        // commits/tags — see the equivalent isolation in
        // `git_include.rs`'s `create_test_repo`.
        run(&["config", "commit.gpgsign", "false"]);
        run(&["config", "tag.gpgsign", "false"]);
        run(&["config", "tag.forceSignAnnotated", "false"]);
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.join("secret.yml"),
            repo_dir.join("batect-bundle.yml"),
        )
        .unwrap();
        run(&["add", "batect-bundle.yml"]);
        run(&["commit", "--quiet", "-m", "initial"]);
        run(&["tag", "v1.0.0"]);

        std::fs::write(
            dir.join("batect.yml"),
            format!(
                r#"
project_name: demo
include:
  - type: git
    repo: {}
    ref: v1.0.0
"#,
                repo_dir.display()
            ),
        )
        .unwrap();

        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(
            cache_root.clone(),
            crate::git_include::SystemGitClient,
            1000,
        );

        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(&repo_dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn two_git_includes_for_the_same_repo_and_ref_only_clone_once() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: a.yml
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: b.yml
"#,
        )
        .unwrap();

        let mut files = HashMap::new();
        files.insert(
            "a.yml".to_string(),
            "tasks:\n  a-task:\n    run:\n      container: build-env\n".to_string(),
        );
        files.insert(
            "b.yml".to_string(),
            "tasks:\n  b-task:\n    run:\n      container: build-env\n".to_string(),
        );
        let git =
            FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git.clone(), 1000);

        let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
            .await
            .unwrap();
        assert!(loaded.config.tasks.contains_key("a-task"));
        assert!(loaded.config.tasks.contains_key("b-task"));
        assert_eq!(git.clone_count(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn a_git_include_missing_repo_or_ref_is_a_clear_parse_error() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let git_cache = GitIncludeCache::for_test(unique_temp_dir(), FakeGitClient::new(), 1000);
        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn repo_and_ref_are_rejected_on_a_non_git_include() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - repo: https://example.com/bundle.git
    ref: v1.0.0
    path: extra.yml
"#,
        )
        .unwrap();

        let git_cache = GitIncludeCache::for_test(unique_temp_dir(), FakeGitClient::new(), 1000);
        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("only valid for 'type: git'"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_git_clone_failure_surfaces_a_clear_error() {
        let dir = unique_temp_dir();
        std::fs::write(
            dir.join("batect.yml"),
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
        )
        .unwrap();

        let git = FakeGitClient::new().failing("simulated network failure");
        let cache_root = unique_temp_dir();
        let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

        let result =
            Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
        assert!(format!("{:?}", result.unwrap_err()).contains("simulated network failure"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[test]
    fn load_config_vars_file_parses_a_flat_map() {
        let dir = unique_temp_dir();
        let vars_path = dir.join("vars.yml");
        std::fs::write(
            &vars_path,
            r#"
env_name: staging
region: eu
"#,
        )
        .unwrap();

        let vars = Config::load_config_vars_file(&vars_path).unwrap();
        assert_eq!(vars.get("env_name"), Some(&"staging".to_string()));
        assert_eq!(vars.get("region"), Some(&"eu".to_string()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_config_vars_file_missing_file_errors() {
        let result = Config::load_config_vars_file(Path::new("/nonexistent/vars.yml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_config_vars_file_malformed_yaml_errors() {
        let dir = unique_temp_dir();
        let vars_path = dir.join("vars.yml");
        // A YAML sequence, not the flat name/value map load_config_vars_file expects.
        std::fs::write(&vars_path, "- not\n- a map\n").unwrap();

        let result = Config::load_config_vars_file(&vars_path);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_container_and_task_run_environment() {
        let config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    environment:
      CONTAINER_VAR: container-value
tasks:
  test:
    run:
      container: build-env
      command: echo hi
      environment:
        RUN_VAR: run-value
"#,
        );

        let container = config.containers.get("build-env").unwrap();
        assert_eq!(
            container.environment.as_ref().unwrap().get("CONTAINER_VAR"),
            Some(&"container-value".to_string())
        );

        let task = config.tasks.get("test").unwrap();
        assert_eq!(
            task.run
                .as_ref()
                .unwrap()
                .environment
                .as_ref()
                .unwrap()
                .get("RUN_VAR"),
            Some(&"run-value".to_string())
        );
    }

    #[test]
    fn parses_config_variables() {
        let config = parse(
            r#"
project_name: demo
containers: {}
tasks: {}
config_variables:
  env_name:
    default: dev
  no_default: {}
"#,
        );

        let vars = config.config_variables.unwrap();
        assert_eq!(vars["env_name"].default.as_deref(), Some("dev"));
        assert_eq!(vars["no_default"].default, None);
    }

    #[test]
    fn config_variables_accept_an_inert_description_field() {
        let config = parse(
            r#"
project_name: demo
containers: {}
tasks: {}
config_variables:
  env_name:
    default: dev
    description: "which environment to target"
"#,
        );

        let vars = config.config_variables.unwrap();
        assert_eq!(
            vars["env_name"].description.as_deref(),
            Some("which environment to target")
        );
    }

    #[test]
    fn forbid_telemetry_is_accepted_but_inert() {
        let config = parse(
            r#"
project_name: demo
containers: {}
tasks: {}
forbid_telemetry: true
"#,
        );

        assert_eq!(config.forbid_telemetry, Some(true));
    }

    fn container_with_environment(environment: HashMap<String, String>) -> Container {
        Container {
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
            environment: Some(environment),
            run_as_current_user: None,
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
            health_check: None,
            setup_commands: None,
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
        }
    }

    #[test]
    fn resolve_expressions_expands_host_var() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "$HOST_VAR".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(
                Path::new("/base"),
                &HashMap::new(),
                &HashMap::new(),
                |name| (name == "HOST_VAR").then(|| "host-value".to_string()),
            )
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "host-value"
        );
    }

    #[test]
    fn resolve_expressions_uses_default_when_host_var_unset() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "${HOST_VAR:-fallback}".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "fallback"
        );
    }

    #[test]
    fn resolve_expressions_errors_when_host_var_unset_without_default() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "$HOST_VAR".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |_| None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_prefers_cli_override_over_default() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<env_name".to_string());
        let mut config_variables = HashMap::new();
        config_variables.insert(
            "env_name".to_string(),
            ConfigVariable {
                default: Some("dev".to_string()),
                description: None,
            },
        );
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: Some(config_variables),
            forbid_telemetry: None,
        };

        let overrides = HashMap::from([("env_name".to_string(), "prod".to_string())]);
        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &overrides, |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "prod"
        );
    }

    #[test]
    fn resolve_expressions_falls_back_to_config_variable_default() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<env_name".to_string());
        let mut config_variables = HashMap::new();
        config_variables.insert(
            "env_name".to_string(),
            ConfigVariable {
                default: Some("dev".to_string()),
                description: None,
            },
        );
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: Some(config_variables),
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "dev"
        );
    }

    #[test]
    fn resolve_expressions_errors_on_undeclared_config_variable_reference() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<missing".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |_| None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_errors_on_declared_config_variable_with_no_value() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<env_name".to_string());
        let mut config_variables = HashMap::new();
        config_variables.insert(
            "env_name".to_string(),
            ConfigVariable {
                default: None,
                description: None,
            },
        );
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: Some(config_variables),
            forbid_telemetry: None,
        };

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |_| None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_errors_on_unknown_cli_override() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let overrides = HashMap::from([("unknown".to_string(), "value".to_string())]);
        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &overrides,
            |_| None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_leaves_literal_values_unchanged() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "literal-value".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "literal-value"
        );
    }

    #[test]
    fn resolve_expressions_resolves_built_in_project_directory_var_in_environment() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<batect.project_directory".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "/base"
        );
    }

    #[test]
    fn resolve_expressions_resolves_built_in_project_directory_var_in_volumes() {
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
            local: "<{batect.project_directory}/scripts".to_string(),
            container: "/scripts".to_string(),
            options: None,
        })]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
                None
            })
            .unwrap();

        let volume = expect_local(&config.containers["build-env"].volumes.as_ref().unwrap()[0]);
        assert_eq!(volume.local, "/base/scripts");
        assert_eq!(volume.container, "/scripts");
    }

    #[test]
    fn resolve_expressions_cleans_project_directory_var_when_base_path_is_empty() {
        // An empty `base_path` is what `main.rs` passes for a bare `-f
        // batect.yml` (no directory prefix) — `Path::parent()` on that
        // returns `Some("")`, not `None`. Without cleaning, joining an empty
        // path leaves a trailing slash on every value derived from it.
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<batect.project_directory".to_string());
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        config
            .resolve_expressions_with(Path::new(""), &HashMap::new(), &HashMap::new(), |_| None)
            .unwrap();

        let resolved = &config.containers["build-env"].environment.as_ref().unwrap()["FOO"];
        assert!(
            !resolved.ends_with('/'),
            "batect.project_directory shouldn't have a trailing slash: {resolved}"
        );
    }

    #[test]
    fn resolve_expressions_errors_if_project_directory_is_declared_in_config_variables() {
        let mut config_variables = HashMap::new();
        config_variables.insert(
            "batect.project_directory".to_string(),
            ConfigVariable {
                default: Some("/somewhere".to_string()),
                description: None,
            },
        );
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: Some(config_variables),
            forbid_telemetry: None,
        };

        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |_| None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_errors_if_project_directory_is_given_as_a_cli_override() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: None,
            forbid_telemetry: None,
        };

        let overrides = HashMap::from([(
            "batect.project_directory".to_string(),
            "/hijacked".to_string(),
        )]);
        let result = config.resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &overrides,
            |_| None,
        );
        assert!(result.is_err());
    }
}
