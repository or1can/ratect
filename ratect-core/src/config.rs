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

//! Data models for the configuration — **two text
//! formats, one model**. Every entry point comes in a pair: `load_project`/
//! `load_from_file` (Batect-compatible, YAML via `noyalib`) and their `_native`
//! siblings (`ratect`'s `ratect.toml`, TOML via `toml`, with `.yml`/`.yaml`
//! includes still parsed as YAML *by extension*). A binary picks a format by
//! *which function it calls*, so the private `ConfigFormat` policy enum never
//! leaks into the public API; `parse_config_file` is the single dispatch point,
//! and both parsers feed the same `ConfigFile`, so nothing downstream knows which
//! format a file came from. Several things ride on that policy — the native-only
//! `extends` pass, the nested-git-include gate and its error redaction, expressions
//! in `image`, the `ratect-bundle.toml`-before-`batect-bundle.yml` probe for a
//! pathless git include, and the object-only *documented* schema (the parser
//! itself stays string-tolerant, which is what lets one set of hand-written
//! `Deserialize` impls serve both formats). **`extends`** (native only; a
//! `batect.yml` using it is *rejected*, not ignored) is a final pass *after*
//! expression/path resolution — mechanically `child.or(parent)` over the
//! already-`Option` fields, so a set field replaces and an unset one inherits,
//! single-parent, transitive, cycle-checked. `inherit_container_fields`
//! destructures the parent exhaustively on purpose: a new `Container` field that
//! forgets to inherit is a compile error, not a silent gap. Ordering is
//! load-bearing — resolve *then* extend, so an inherited relative path stays
//! anchored to the *parent's* own file rather than re-anchoring to the child's.
//! See [decisions/0003](https://github.com/or1can/ratect/blob/main/decisions/0003-ratect-native-config-format.md).
//! Two Batect behaviours worth knowing, both found by running real-world bundles
//! and both applying to *either* format's YAML: a top-level key starting with `.`
//! is an **extension** (it exists only to hold a YAML anchor and is stripped
//! before the schema sees it — which is why YAML is deserialized in two steps,
//! text → `noyalib::Value` → `ConfigFile`, since anchors must resolve *before*
//! the key is dropped), and a **leading `~`** in a host path expands to the home
//! directory (component-wise, matching Batect's `PathResolver.resolveHomeDir`, so
//! `~user/…` stays literal). `task_names_for_completion` is a deliberate
//! *non*-load for shell completion: names only, follows local and
//! already-cached-git includes, never clones or errors.
//! `Config::load_from_file` parses the root file and resolves
//! `include` (local files and Git bundles — see
//! [config reference](https://github.com/or1can/ratect/blob/main/docs/config-reference.md#includes)), merging every loaded
//! file's `containers`/`tasks`/`config_variables` into one `Config`, returned inside a
//! `LoadedConfig` alongside a `container_base_paths` map (each container name → its
//! own origin file's directory). A separate `LoadedConfig::resolve_expressions` call
//! (needs CLI-supplied `--config-var`/`--config-vars-file` overrides, so it can't
//! happen inside `load_from_file`) interpolates and resolves paths — per-container,
//! against `container_base_paths` rather than a single shared directory, so an
//! included file's relative paths resolve against *its own* directory while
//! `batect.project_directory` still always resolves to the root's (`Config`'s own
//! `resolve_expressions` stays available too, unchanged, for a `Config` built without
//! going through `load_from_file`). `load_project` (0.2.0-dev) wraps that whole
//! sequence — existence check, `load_from_file`, `base_path_for`,
//! `project_directory_path`, `resolve_expressions` — into the one call a binary
//! actually wants, returning a `LoadedProject`; it exists so `ratect` and
//! `ratect-compat` can't get the ordering (includes before expressions) or the
//! missing-file error wording out of step with each other. Merging
//! `--config-vars-file` with individually-supplied variables stays the caller's
//! job — only the caller knows what its own flags are called.
//! `run_as_current_user.home_directory` is
//! interpolated but *not* resolved against a base path — it's a container-side path,
//! validated to start with `/` instead. `PortRange`/`PortMapping`,
//! `DeviceMapping` (`devices`), and `VolumeMount` (`volumes` — `Local`/`Cache`
//! variants, 0.18.0, plus `Tmpfs`, 0.21.0) all have hand-written `Deserialize`
//! impls so an entry can be either Batect's string form (`"local:container[/protocol]"` /
//! `"local:container[:options]"` — `VolumeMount`'s string form is always
//! `Local`; there's no compact string form for `Cache`/`Tmpfs`) or the expanded
//! object form. A `VolumeMount::Local`'s host path is resolved here (against
//! `container_base_paths`, same as `build_directory`); a `Cache`'s `name`/
//! `container` are plain strings, not `Expression`s, matching Batect — nothing
//! to resolve here at all, since `--cache-type` and the project's own cache
//! key (needed to actually resolve one) aren't known until `engine.rs`/
//! `cache.rs`. A `Tmpfs`'s `container`/`options` are likewise plain strings —
//! nothing to resolve here either, matching Batect's own `TmpfsMount` typing.
//! `Capability`
//! (`capabilities_to_add`/`capabilities_to_drop`) and `ImagePullPolicy` are fixed
//! enums validated at parse time — `Capability`'s list is a deliberate *superset* of
//! Batect's own (unmaintained) one, not a strict port, see its doc comment.
//! `Task.run` is `Option<TaskRun>` (0.14.0, see docs/task-lifecycle.md) — still
//! requires at least one of `run`/`prerequisites`. `dependencies` (task-level
//! sidecars, distinct from `Container.dependencies`) requires `run` and is
//! rejected without it; `customise` requires `run` too but is merely inert
//! without it, matching Batect. `container_names_in_task` lives here (moved from
//! `engine.rs`) since both the `no_proxy` exemption list and `customise`'s
//! graph-membership check need the same transitive-dependency walk.
//! `format_task_list` is the single source of `--list-tasks` formatting.
//! `Container.command` (a container's own default `CMD` override, symmetric with
//! `Container.entrypoint`) was missed when 0.13.0's container runtime options
//! landed — `run.command` covered the task's own container, but a dependency had
//! no way to set a command of its own at all, silently defaulting to the image's
//! own `CMD` regardless. Closed once noticed, threading through
//! `ContainerRuntime::start_background_container` (a new `command` parameter,
//! reusing `docker.rs`'s existing `build_cmd`/`tokenize_command_line`) the same
//! way `run_container`'s already did. `forbid_telemetry`
//! (`Config`/`ConfigFile`) and `config_variables.<name>.description`
//! (`ConfigVariable`) are recognized but inert (0.19.0), the same "no
//! effect" treatment already given `--upgrade`/`--no-update-notification`/
//! `--no-wrapper-cache-cleanup` (0.17.0, `main.rs`) — parsed and, for
//! `forbid_telemetry`, carried onto the merged `Config` (root file only,
//! same precedent as `project_name`), but never read anywhere else.

use anyhow::{Context, Result};
use path_clean::PathClean;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::include_trust::{self, Bundle, BundleId, EffectiveGrants, Grants};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    /// Inherit every field from another container by name, then override only
    /// the fields set here — `ratect`'s native replacement for YAML anchors.
    ///
    /// Shallow and per-field: a field set here replaces the inherited one
    /// outright (nested maps are not merged into), and a field left unset is
    /// taken from the named container. Single-parent, and may chain (`a`
    /// extends `b` extends `c`). Resolved after path/expression resolution, so
    /// an inherited relative path stays anchored to the *parent's* own file.
    /// `ratect`-native only — not accepted in a `batect.yml`, and skipped from
    /// the committed schema, which is `batect.yml`'s. See
    /// [decisions/0003](../../decisions/0003-ratect-native-config-format.md).
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub extends: Option<String>,
    /// The image to run, in Docker's own `name:tag` form. Exactly one of
    /// `image` or `build_directory` is required. No expression support here:
    /// Batect resolves none, so one is rejected when the file loads rather
    /// than resolved.
    ///
    /// The paragraph above is the *compat* schema's `description` (only a doc
    /// comment's first paragraph becomes one, per `schema.rs`'s summarizer), which is
    /// why the rejection is stated there rather than here. `ratect.toml` does
    /// resolve expressions in this field, and `make_native` overrides the
    /// text to say so; the rejection itself is
    /// [`reject_image_expressions_in_compat`].
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
    /// Makes SSH keys available to a `build_directory` build, for a
    /// Dockerfile's `RUN --mount=type=ssh` instructions. Each entry is one
    /// agent, named by an `id` a `RUN` instruction can select
    /// (`--mount=type=ssh,id=<id>`), and ids must be unique across the list
    /// (checked in [`Config::resolve_expressions_with`]). An entry with no
    /// `paths` forwards the host's own running ssh-agent via
    /// `SSH_AUTH_SOCK`; an entry with `paths` serves those private key
    /// files instead, which is what works in CI where no agent is running.
    /// See [`SshAgent`], and [Image building](https://github.com/or1can/ratect/blob/main/docs/config-reference.md#image-building).
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
    /// Whether this cache belongs to one project or is shared across every
    /// project on the machine — see [`CacheScope`]. `ratect.toml` only;
    /// Batect has no equivalent, so a `batect.yml` using it is rejected.
    ///
    /// `None` means the field was absent, which is not the same as
    /// `Some(Project)`: `ratect-compat` has to reject the *field*, since
    /// real `batect` rejects any unknown property — writing `scope: project`
    /// in a `batect.yml` is still a config that only Ratect will load. Use
    /// [`CacheVolumeMount::scope`] for the effective value.
    pub scope: Option<CacheScope>,
}

impl CacheVolumeMount {
    /// This cache's effective scope — [`CacheScope::Project`] when the field
    /// was omitted, which is Batect's only behaviour.
    pub fn scope(&self) -> CacheScope {
        self.scope.unwrap_or_default()
    }
}

/// How widely a [`CacheVolumeMount`] is shared.
///
/// Batect has only the project-scoped kind, which is why a bundle wanting a
/// cache that outlives one project has to spell it as a host path — the
/// thing [`allow_host_paths`](IncludeEntry) exists to permit and
/// [decisions/0004](https://github.com/or1can/ratect/blob/main/decisions/0004-git-include-host-path-trust.md)
/// would rather solve properly.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheScope {
    /// Private to this project — the Docker volume name carries the
    /// project's own cache key, so two projects declaring the same cache
    /// name get different storage. The default, and Batect's only
    /// behaviour.
    #[default]
    Project,
    /// Shared by every project on the machine that names it. The storage
    /// carries no project key, which is the whole point: one Cargo registry
    /// or npm cache, populated once.
    ///
    /// Deliberately never removed by a bare `ratect caches clean`, which
    /// sweeps this project's caches — discarding storage other projects are
    /// still using should take naming it.
    Shared,
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
                let mut scope: Option<CacheScope> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "local" => local = Some(map.next_value()?),
                        "container" => container = Some(map.next_value()?),
                        "options" => options = Some(map.next_value()?),
                        "name" => name = Some(map.next_value()?),
                        "type" => mount_type = Some(map.next_value()?),
                        "scope" => scope = Some(map.next_value()?),
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["local", "container", "options", "name", "type", "scope"],
                            ))
                        }
                    }
                }
                let container =
                    container.ok_or_else(|| serde::de::Error::missing_field("container"))?;

                match mount_type.as_deref().unwrap_or("local") {
                    "local" => {
                        if scope.is_some() {
                            return Err(serde::de::Error::custom(
                                "Field 'scope' is only permitted for cache mounts.",
                            ));
                        }
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
                            scope,
                        }))
                    }
                    "tmpfs" => {
                        if scope.is_some() {
                            return Err(serde::de::Error::custom(
                                "Field 'scope' is only permitted for cache mounts.",
                            ));
                        }
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
                // Emitted only when it was set: `config convert` writes a
                // `ratect.toml` from a `batect.yml`, where it never can be,
                // and a round trip must not invent it.
                if let Some(scope) = &mount.scope {
                    map.serialize_entry("scope", scope)?;
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
    Ok(map.map(|entries| {
        entries
            .into_iter()
            .map(|(key, value)| (key, value.0))
            .collect()
    }))
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

/// One agent in a container's `build_ssh` list — see
/// [`Container::build_ssh`]. Ids must be unique across the list, which is
/// checked in [`Config::resolve_expressions_with`] rather than here, so the
/// error can name the offending container.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshAgent {
    /// The agent id a Dockerfile's `RUN --mount=type=ssh,id=<id>` refers
    /// to. Required, matching Batect — use `default` for the id a bare `RUN
    /// --mount=type=ssh` uses, which is BuildKit's own implicit one.
    ///
    /// Deliberately not defaulted, even though BuildKit itself would: a
    /// `batect.yml` omitting it is invalid, so accepting it here would let
    /// a config work under `ratect-compat` and fail under `batect` — the
    /// one direction a drop-in replacement must not diverge in. Required
    /// in the native format too rather than only in `ratect-compat`, since
    /// making a field's *requiredness* format-dependent would be a new kind
    /// of difference between the two, for one line of saved typing.
    pub id: String,
    /// Private key files to serve instead of forwarding a running agent —
    /// the case that works in CI, where there is usually no agent at all.
    /// Values support [expressions](#expressions) and are resolved the same
    /// way as `build_directory`. Leave empty to forward the host's own
    /// running agent via `SSH_AUTH_SOCK`.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Overrides the [health check configuration](https://docs.docker.com/engine/reference/builder/#healthcheck)
/// specified in the container's image. Every field is optional — an omitted
/// field inherits the image's own value, matching Batect (and Docker's `0` =
/// inherit convention). Durations use Batect's Go-style string format:
/// `"2s"`, `"1m30s"`, `"500ms"`, `"0"`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// The bundle file names a pathless `type: git` include looks for, in order.
/// `Compat` is Batect's single default; `Native` prefers its own TOML bundle
/// but falls back to the Batect one — so an unmigrated bundle still works from
/// a native project, and a bundle author can ship both files and support both
/// tools at once (native takes the TOML, Batect/`ratect-compat` the YAML). See
/// [decisions/0003](../../decisions/0003-ratect-native-config-format.md).
fn git_bundle_candidates(format: ConfigFormat) -> &'static [&'static str] {
    match format {
        ConfigFormat::Compat => &["batect-bundle.yml"],
        ConfigFormat::Native => &["ratect-bundle.toml", "batect-bundle.yml"],
    }
}

/// One entry in a config file's top-level `include` list — either a local
/// file (a bare string path, or the expanded `{type: file, path: ...}`
/// object form, mirroring [`PortMapping`]'s string-or-object handling
/// above), or a Git bundle (`{type: git, repo, ref, path}`). A Git entry's
/// `path` is optional: when omitted, the bundle file is discovered by
/// [`git_bundle_candidates`] (format-dependent), so the default isn't baked in
/// here — the load doesn't yet know whether it's running in native or compat
/// mode.
#[derive(Debug, Clone)]
pub(crate) enum IncludeEntry {
    File {
        path: String,
    },
    Git {
        repo: String,
        git_ref: String,
        path: Option<String>,
        /// Vouches for this bundle, letting its containers resolve host paths
        /// outside the usual containment — see [`crate::include_trust::Trust::host_paths`] and
        /// [decisions/0004](../../decisions/0004-git-include-host-path-trust.md).
        /// Honoured only when the file declaring this entry is one the project
        /// owner controls.
        allow_host_paths: bool,
        /// Lets this bundle declare `type: git` includes of its own, which is
        /// otherwise refused in the native format — see [`crate::include_trust::Trust::nested_git`].
        /// Honoured only when the file declaring this entry is one the project
        /// owner controls.
        ///
        /// `Option` because presence, not value, is what
        /// [`include_trust::check_dialect`] rejects in a `batect.yml`: writing
        /// `allow_nested_git_includes: false` there would otherwise look
        /// accepted while describing the opposite of what that format does.
        allow_nested_git_includes: Option<bool>,
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
                let mut allow_host_paths: Option<bool> = None;
                let mut allow_nested_git_includes: Option<bool> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "path" => path = Some(map.next_value()?),
                        "type" => include_type = Some(map.next_value()?),
                        "repo" => repo = Some(map.next_value()?),
                        "ref" => git_ref = Some(map.next_value()?),
                        "allow_host_paths" => allow_host_paths = Some(map.next_value()?),
                        "allow_nested_git_includes" => {
                            allow_nested_git_includes = Some(map.next_value()?)
                        }
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &[
                                    "path",
                                    "type",
                                    "repo",
                                    "ref",
                                    "allow_host_paths",
                                    "allow_nested_git_includes",
                                ],
                            ))
                        }
                    }
                }

                match include_type.as_deref() {
                    Some("git") => {
                        let repo = repo.ok_or_else(|| serde::de::Error::missing_field("repo"))?;
                        let git_ref =
                            git_ref.ok_or_else(|| serde::de::Error::missing_field("ref"))?;
                        // Left as `None` when omitted: the default bundle name
                        // depends on the load's format, unknown here.
                        Ok(IncludeEntry::Git {
                            repo,
                            git_ref,
                            path,
                            allow_host_paths: allow_host_paths.unwrap_or(false),
                            allow_nested_git_includes,
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
                        // A local file is always the project owner's own, so
                        // there's no containment to relax and nothing to vouch
                        // for — accepting the flag here would only suggest
                        // otherwise.
                        if allow_host_paths.is_some() {
                            return Err(serde::de::Error::custom(
                                "'allow_host_paths' is only valid for 'type: git' includes",
                            ));
                        }
                        // Same reasoning: a local file can't be the thing that
                        // redirects the load to a further remote, so there is
                        // nothing here to permit.
                        if allow_nested_git_includes.is_some() {
                            return Err(serde::de::Error::custom(
                                "'allow_nested_git_includes' is only valid for 'type: git' includes",
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

/// The repository an include entry names, or `None` for a local file include.
fn include_repo(include: &IncludeEntry) -> Option<&str> {
    match include {
        IncludeEntry::Git { repo, .. } => Some(repo),
        IncludeEntry::File { .. } => None,
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
    /// Which bundle this is, and what it was granted. The grants half is
    /// [`crate::include_trust`]'s: this type is the *containment* half, and
    /// carries the bundle so the two travel together, since every containment
    /// failure has to name the bundle the path came from.
    ///
    /// A grant deliberately does *not* relax the include-`path` containment
    /// ([`check_contains`](Self::check_contains)): that stops a bundle pulling
    /// an arbitrary host *file* into the configuration, which is a separate
    /// concern from where its containers may mount.
    bundle: Bundle,
}

impl GitBoundary {
    /// Purely lexical containment check — deliberately runs before
    /// `resolved` is confirmed to exist, so a `path` engineered to escape
    /// (an absolute path, or a `../..` traversal) is rejected without ever
    /// touching the filesystem at the escaped location.
    ///
    /// Normalizes `resolved` itself rather than trusting the caller to. The
    /// comparison is [`Path::starts_with`], which matches components without
    /// interpreting any of them, so `<repo_dir>/../../elsewhere` starts with
    /// `<repo_dir>` and passes — the check is inert on exactly the input it
    /// exists to reject. Two call sites got this wrong (completion's walk, and
    /// [`resolve_path`]'s absolute branch below), which is one more than a
    /// convention survives; cleaning here makes the mistake unrepresentable.
    fn check_contains(&self, resolved: &Path) -> Result<()> {
        let resolved = &resolved.clean();
        if resolved.starts_with(&self.repo_dir) {
            return Ok(());
        }
        anyhow::bail!(
            "Included file '{}' escapes the Git repository '{}' at '{}' it was included from \
             — includes reached through a Git include must resolve within that repository.",
            resolved.display(),
            self.bundle.id.remote,
            self.bundle.id.git_ref
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
            self.bundle.id.remote,
            self.bundle.id.git_ref
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
    ///
    /// Normalizes `resolved` for the same reason
    /// [`check_contains`](Self::check_contains) does — and here it was not
    /// merely theoretical: `<{batect.project_directory}/../../../etc` is an
    /// absolute path a bundle can write without knowing anything about the
    /// machine, and it starts with the project directory component-for-
    /// component.
    fn check_path_allowed(&self, resolved: &Path, project_dir: &Path) -> Result<()> {
        let resolved = &resolved.clean();
        if self.bundle.trust.host_paths
            || resolved.starts_with(&self.repo_dir)
            || resolved.starts_with(project_dir)
        {
            return Ok(());
        }
        anyhow::bail!(
            "Path '{}' escapes both the Git repository '{}' at '{}' it was included from and \
             the project directory '{}' — a container reached through a Git include must \
             resolve its 'volumes'/'build_directory' paths within one of the two. If you trust \
             this bundle to reach that path, set 'allow_host_paths' to true on the include entry \
             for '{}' in your own configuration.",
            resolved.display(),
            self.bundle.id.remote,
            self.bundle.id.git_ref,
            project_dir.display(),
            self.bundle.id.remote
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

/// Which config file format(s) a load accepts, and how a file's extension
/// maps to a parser. Selected by the *binary*, not the file: `ratect-compat`
/// is a byte-compatible Batect replacement, so it only ever reads YAML;
/// `ratect` reads its native TOML and accepts YAML includes, choosing the
/// parser per file by extension. Not `pub` — a binary picks a policy by
/// calling the matching entry point ([`load_project`] vs.
/// [`load_project_native`]), never by naming this. See
/// [decisions/0003](../../decisions/0003-ratect-native-config-format.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFormat {
    /// YAML only, whatever the extension — exactly Batect's behavior, so a
    /// `.toml` passed to `ratect-compat` fails as invalid YAML just as it
    /// would under Batect, rather than being newly accepted.
    Compat,
    /// TOML native (`.toml`) with YAML includes (`.yml`/`.yaml`) accepted;
    /// any other extension is rejected rather than guessed at.
    Native,
}

/// The parser a file's extension selects under [`ConfigFormat::Native`].
enum FileFormat {
    Toml,
    Yaml,
}

/// Classifies a file by extension for [`ConfigFormat::Native`]. Deliberately
/// strict — an unrecognized extension errors rather than being content-sniffed,
/// since TOML and YAML are too easy to confuse for a simple document.
fn config_file_format(path: &Path) -> Result<FileFormat> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("toml") => Ok(FileFormat::Toml),
        Some(ext) if ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml") => {
            Ok(FileFormat::Yaml)
        }
        _ => anyhow::bail!(
            "Unrecognized config file extension for {:?}; expected .toml, .yml, or .yaml",
            path
        ),
    }
}

/// Parses one config file (the root, or an included one) only — no include
/// resolution, path resolution, or expression interpolation. The `format`
/// decides which parser handles it: `Compat` is always YAML; `Native` picks
/// TOML or YAML by extension. Both feed the same [`ConfigFile`], so nothing
/// downstream depends on which format a file was written in.
fn parse_config_file(path: &Path, format: ConfigFormat) -> Result<ConfigFile> {
    match format {
        ConfigFormat::Compat => parse_yaml_config_file(path),
        ConfigFormat::Native => match config_file_format(path)? {
            FileFormat::Toml => parse_toml_config_file(path),
            FileFormat::Yaml => parse_yaml_config_file(path),
        },
    }
}

/// A top-level key prefix marking a Batect *extension*: an entry that exists
/// only to hold a YAML anchor for the rest of the file to alias, and is
/// otherwise ignored. Batect enables this via kaml's
/// `extensionDefinitionPrefix = "."` (its `ConfigurationLoader`), so a config
/// like
///
/// ```yaml
/// .common-environment: &common-environment
///   TZ: UTC
///
/// containers:
///   app:
///     environment:
///       <<: *common-environment
/// ```
///
/// is valid Batect configuration. `ConfigFile` is `deny_unknown_fields`, so
/// without stripping these first they'd be rejected as unknown fields.
const EXTENSION_KEY_PREFIX: char = '.';

/// Parses a YAML config file, dropping Batect's top-level *extension* entries
/// (see [`EXTENSION_KEY_PREFIX`]) before deserializing.
///
/// Deserialized in two steps — text to [`noyalib::Value`], then `Value` to
/// [`ConfigFile`] — rather than straight into `ConfigFile`, purely so the
/// extension keys can be removed in between. The parse step is what resolves
/// anchors, aliases and merge keys, so by the time an extension entry is
/// dropped its content has already been inlined everywhere it was aliased —
/// which is exactly what makes dropping it safe. Only *top-level* keys are
/// considered, matching kaml, so a `.`-prefixed key nested anywhere else still
/// reaches the schema (and is still rejected if it isn't a real field).
fn parse_yaml_config_file(path: &Path) -> Result<ConfigFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to open config file {:?}", path))?;
    let mut document: noyalib::Value = noyalib::from_str(&text)
        .with_context(|| format!("Failed to parse config file {:?}", path))?;
    if let noyalib::Value::Mapping(mapping) = &mut document {
        mapping.retain(|key, _| !key.starts_with(EXTENSION_KEY_PREFIX));
    }
    noyalib::from_value(&document)
        .with_context(|| format!("Failed to parse config file {:?}", path))
}

fn parse_toml_config_file(path: &Path) -> Result<ConfigFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to open config file {:?}", path))?;
    toml::from_str(&text).with_context(|| format!("Failed to parse config file {:?}", path))
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

/// Resolves an include entry to its actual file. A single candidate — a local
/// file, or a Git bundle with an explicit `path` — keeps the precise
/// exists/not-a-file messages the loop has always given. Multiple candidates —
/// a pathless Git bundle probing [`git_bundle_candidates`] — are tried in
/// order, and the first that exists wins, since a bundle may ship only one of
/// them. Each candidate's *lexical* containment within a Git boundary is
/// checked before it's touched; the caller re-checks the chosen one
/// canonically.
fn resolve_include_target(
    base_dir: &Path,
    candidates: &[String],
    boundary: Option<&GitBoundary>,
) -> Result<PathBuf> {
    if let [only] = candidates {
        let resolved = absolute_path(&base_dir.join(only))?;
        if let Some(boundary) = boundary {
            boundary.check_contains(&resolved)?;
        }
        if !resolved.is_file() {
            if resolved.exists() {
                anyhow::bail!("Included file '{}' is not a file.", resolved.display());
            }
            anyhow::bail!("Included file '{}' does not exist.", resolved.display());
        }
        return Ok(resolved);
    }

    for candidate in candidates {
        let resolved = absolute_path(&base_dir.join(candidate))?;
        if let Some(boundary) = boundary {
            boundary.check_contains(&resolved)?;
        }
        if resolved.is_file() {
            return Ok(resolved);
        }
    }
    anyhow::bail!(
        "No bundle file found in the Git include (looked for {}).",
        candidates.join(", ")
    );
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
    ///
    /// Loads in Batect-compatible mode (YAML only) — `ratect-compat`'s policy.
    /// The `ratect` binary uses [`load_from_file_native`](Self::load_from_file_native).
    pub async fn load_from_file(path: &Path) -> Result<LoadedConfig> {
        let git_cache = crate::git_include::GitIncludeCache::new();
        Self::load_from_file_with_git_cache(path, &git_cache).await
    }

    /// Like [`load_from_file`](Self::load_from_file), but in `ratect`'s native
    /// mode: a TOML root file, with TOML/YAML includes chosen by extension —
    /// see [`ConfigFormat::Native`].
    pub async fn load_from_file_native(path: &Path) -> Result<LoadedConfig> {
        let git_cache = crate::git_include::GitIncludeCache::new();
        Self::load_from_file_native_with_git_cache(path, &git_cache).await
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
        Self::load_from_file_impl(path, git_cache, ConfigFormat::Compat).await
    }

    /// Native-mode counterpart of
    /// [`load_from_file_with_git_cache`](Self::load_from_file_with_git_cache).
    pub async fn load_from_file_native_with_git_cache<G: crate::git_include::GitClient>(
        path: &Path,
        git_cache: &crate::git_include::GitIncludeCache<G>,
    ) -> Result<LoadedConfig> {
        Self::load_from_file_impl(path, git_cache, ConfigFormat::Native).await
    }

    async fn load_from_file_impl<G: crate::git_include::GitClient>(
        path: &Path,
        git_cache: &crate::git_include::GitIncludeCache<G>,
        format: ConfigFormat,
    ) -> Result<LoadedConfig> {
        let root_path = absolute_path(path)?;
        let root_file = parse_config_file(path, format)?;
        let root_dir = root_path.parent().unwrap_or(Path::new("")).to_path_buf();

        let mut seen: HashSet<PathBuf> = HashSet::new();
        seen.insert(root_path.clone());

        let mut effective_grants = EffectiveGrants::default();

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
            let (base_dir, candidates, boundary) = match &include {
                IncludeEntry::File { path } => (containing_dir, vec![path.clone()], boundary),
                IncludeEntry::Git {
                    repo,
                    git_ref,
                    path,
                    allow_host_paths,
                    allow_nested_git_includes,
                } => {
                    let asked = Grants {
                        host_paths: *allow_host_paths,
                        nested_git: *allow_nested_git_includes,
                    };
                    let declaring = boundary.as_ref().map(|boundary| &boundary.bundle);
                    include_trust::check_dialect(asked, declaring, repo, format)?;
                    // The single value every native-only behaviour below keys
                    // off, derived once so the gate and the clone-detail
                    // redaction can never disagree about which includes are a
                    // bundle's own.
                    let restricted = include_trust::restricting(declaring, format);
                    include_trust::check_may_declare_git(restricted, repo)?;
                    let key = (repo.clone(), git_ref.clone());
                    let repo_dir =
                        match git_repo_paths.get(&key) {
                            Some(dir) => dir.clone(),
                            None => {
                                let dir = git_cache.ensure_cached(repo, git_ref).await.map_err(
                                    |error| {
                                        include_trust::hide_clone_detail(
                                            error, repo, git_ref, restricted,
                                        )
                                    },
                                )?;
                                git_repo_paths.insert(key, dir.clone());
                                dir
                            }
                        };
                    let boundary = GitBoundary {
                        repo_dir: repo_dir.clone(),
                        bundle: Bundle::granted(
                            declaring,
                            BundleId {
                                remote: repo.clone(),
                                git_ref: git_ref.clone(),
                            },
                            asked,
                        ),
                    };
                    // An explicit `path` is the single candidate; a pathless
                    // bundle probes the format's default names in order.
                    let candidates = match path {
                        Some(path) => vec![path.clone()],
                        None => git_bundle_candidates(format)
                            .iter()
                            .map(|name| name.to_string())
                            .collect(),
                    };
                    (repo_dir, candidates, Some(boundary))
                }
            };
            let resolved = resolve_include_target(&base_dir, &candidates, boundary.as_ref())?;

            if let Some(boundary) = &boundary {
                boundary.check_contains_canonical(&resolved)?;
            }
            // `None` where there is no boundary — an owned file, contained
            // by nothing. Kept distinct from a boundary granting nothing all
            // the way into `EffectiveGrants`, since the two are opposites.
            let effective = boundary.as_ref().map(|boundary| boundary.bundle.trust);
            if !seen.insert(resolved.clone()) {
                // Already loaded by another route, so whatever trust this
                // entry arrived carrying cannot apply. Which repository the
                // refusal names depends on where that trust came from: a
                // `type: git` entry carries its own grant, while a local
                // include carries the boundary of the bundle it sits in — and
                // that bundle's include entry is where the owner wrote the
                // flag, so naming it points at the line to edit.
                //
                // Both are `None` together, for an owned file's own local
                // include: no boundary, so no trust arrived and no repository
                // is involved to name.
                let repo = include_repo(&include).or_else(|| {
                    boundary
                        .as_ref()
                        .map(|boundary| boundary.bundle.id.remote.as_str())
                });
                if let (Some(repo), Some(wanted)) = (repo, effective) {
                    effective_grants.check(&resolved, wanted, repo)?;
                }
                continue;
            }
            effective_grants.record(resolved.clone(), effective);

            let file = parse_config_file(&resolved, format)?;
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

    /// Like [`load_config_vars_file`](Self::load_config_vars_file), but for
    /// `ratect`'s native mode: the file's format follows its extension, so
    /// `ratect.local.toml` is parsed as TOML while an explicitly-named
    /// `batect.local.yml` (or any `.yml`/`.yaml`) is still YAML. Either way a
    /// flat `name = "value"` / `name: value` map of config-variable values.
    pub fn load_config_vars_file_native(path: &Path) -> Result<HashMap<String, String>> {
        match config_file_format(path)? {
            FileFormat::Yaml => Self::load_config_vars_file(path),
            FileFormat::Toml => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to open config vars file {:?}", path))?;
                toml::from_str(&text)
                    .with_context(|| format!("Failed to parse config vars file {:?}", path))
            }
        }
    }

    /// Resolves every expression-bearing value in the config — `environment`
    /// entries (on containers and task `run`s), volume host paths, and a
    /// container's `image` (native-only in effect: an expression there is
    /// refused in a `batect.yml` before this runs, so interpolating it is a
    /// no-op for that format) — through Batect's expression syntax:
    /// `$VAR`/`${VAR}`/`${VAR:-default}`
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
    /// for callers that never need
    /// [`resolve_expressions_with_boundaries`](Self::resolve_expressions_with_boundaries)'s
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
            // One attribution point for the whole block. Everything below
            // speaks its own vocabulary — `expressions` knows a variable
            // name, `resolve_path` a path — and none of them know which
            // container asked. Naming it per call site means the next
            // field added is unattributed by default, which is how `image`
            // shipped reporting an unset variable with no container while
            // the compat rejection beside it named one.
            (|| -> Result<()> {
                let container_base_path = container_base_paths
                    .get(container_name)
                    .map(PathBuf::as_path)
                    .unwrap_or(base_path);
                let container_boundary = container_git_boundaries
                    .get(container_name)
                    .map(|boundary| (boundary, project_directory_path.as_path()));
                // Unconditional, though only `ratect.toml` can reach it with an
                // expression in it: a `batect.yml` carrying one is refused at load
                // (see [`reject_image_expressions_in_compat`]), and anything that
                // survives that has nothing here to substitute. Keeping the
                // *decision* at the rejection — where the format is already known —
                // is what saves threading `ConfigFormat` through path resolution.
                if let Some(image) = &mut container.image {
                    *image = crate::expressions::interpolate(image, &host_env, &config_vars)?;
                }
                if let Some(environment) = &mut container.environment {
                    for value in environment.values_mut() {
                        *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
                    }
                }
                if let Some(volumes) = &mut container.volumes {
                    for volume in volumes {
                        // A cache `name` becomes a host directory under
                        // `--cache-type directory`, so it is checked before
                        // anything can join it onto one.
                        if let VolumeMount::Cache(cache) = volume {
                            validate_cache_name(&cache.name)?;
                        }
                        // `Cache` mounts have nothing else to resolve here —
                        // `name`/`container` are plain strings, not expressions,
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
                if let Some(build_ssh) = &mut container.build_ssh {
                    let mut ids_seen = HashSet::new();
                    for agent in build_ssh.iter_mut() {
                        let id = agent.id.clone();
                        if !ids_seen.insert(id.clone()) {
                            // A Dockerfile selects an agent by id, so two
                            // entries claiming one id have no defined meaning —
                            // rejected here rather than silently letting one win,
                            // matching Batect's own `checkForDuplicateSSHAgents`.
                            anyhow::bail!(
                                "has more than one 'build_ssh' entry with the id \
                                 '{}', but each SSH agent must have a unique id",
                                id
                            );
                        }
                        for path in &mut agent.paths {
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
                if let Some(run_as_current_user) = &mut container.run_as_current_user {
                    if run_as_current_user.enabled {
                        let home_directory =
                            run_as_current_user.home_directory.as_mut().ok_or_else(|| {
                                anyhow::anyhow!(
                                    "has 'run_as_current_user.enabled' set to true, \
                                     but no 'home_directory' was provided",
                                )
                            })?;
                        // Not `resolve_path` — this is a path *inside the
                        // container*, never resolved against `base_path`.
                        *home_directory =
                            crate::expressions::interpolate(home_directory, &host_env, &config_vars)?;
                        if !home_directory.starts_with('/') {
                            anyhow::bail!(
                                "has an invalid 'run_as_current_user.home_directory': \
                                 '{}' is not an absolute path",
                                home_directory
                            );
                        }
                        // Each `cache` mount's container path, for the same
                        // reason and in the same place: `run_as_current_user`
                        // takes ownership of them, which means uploading an
                        // archive to that path. A non-absolute one would
                        // otherwise surface from the Docker layer, whose only
                        // identifier is a container id — reading as though the
                        // configuration had named that.
                        //
                        // Scoped to `cache` mounts under an *enabled*
                        // `run_as_current_user`, matching Batect exactly. Wider
                        // would be tempting and wrong: Batect never checks
                        // `local`/`tmpfs` destinations, so a Windows-container
                        // config mounting `C:\code` would stop loading here
                        // while still working there.
                        for volume in container.volumes.iter().flatten() {
                            if let VolumeMount::Cache(cache) = volume {
                                if !cache.container.starts_with('/') {
                                    anyhow::bail!(
                                        "has an invalid 'cache' volume mount: \
                                         '{}' is not an absolute path",
                                        cache.container
                                    );
                                }
                            }
                        }
                        // `home_directory` is interpolated raw into a
                        // colon-delimited `/etc/passwd`/`/etc/shadow` line
                        // (`user::generate_passwd_file`) — a `:` shifts that
                        // line's fields, and a newline/other control character
                        // injects an entirely new (attacker-chosen) entry.
                        if home_directory.contains(':') || home_directory.chars().any(char::is_control)
                        {
                            anyhow::bail!(
                                "has an invalid 'run_as_current_user.home_directory': \
                                 '{}' contains a ':' or a control character, which would corrupt the \
                                 generated /etc/passwd and /etc/shadow entries",
                                home_directory
                            );
                        }
                    } else if run_as_current_user.home_directory.is_some() {
                        anyhow::bail!(
                            "has 'run_as_current_user.home_directory' set, but \
                             'run_as_current_user.enabled' is not true",
                        );
                    }
                }
                Ok(())
            })()
            .with_context(|| format!("Container '{container_name}'"))?;
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

/// Expands a leading `~` to the host user's home directory, matching Batect's
/// own `PathResolver.resolveHomeDir` — so a volume like `~/.cache/trivy`
/// mounts the real cache directory rather than a literal `~` directory under
/// the project.
///
/// Only when the *first path component* is exactly `~`, which is Batect's rule
/// (its check is `path.startsWith(Path("~"))`, a component comparison, not a
/// string prefix). So `~` and `~/x` expand, while `~user/x` doesn't — bash's
/// "another user's home" form is not supported there and isn't here either,
/// and a path with `~` anywhere but the front is untouched. `None` means
/// there was nothing to expand, leaving the caller's own handling alone.
fn expand_home_directory(path: &str) -> Result<Option<PathBuf>> {
    let mut components = Path::new(path).components();
    let leads_with_tilde = matches!(
        components.next(),
        Some(std::path::Component::Normal(first)) if first == "~"
    );
    if !leads_with_tilde {
        return Ok(None);
    }
    let home = crate::user::home_directory()
        .with_context(|| format!("Cannot expand '~' in path '{path}'"))?;
    // For a bare `~` the remainder is empty, and joining that yields the home
    // directory itself.
    Ok(Some(home.join(components.as_path())))
}

/// Interpolates expressions within `path`, expands a leading `~` (see
/// [`expand_home_directory`]), then resolves the result to an
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
    let resolved = if let Some(home_relative) = expand_home_directory(&interpolated)? {
        // Already absolute (it's rooted at the home directory), so it never
        // joins onto `base_path` — same as any other absolute path below.
        home_relative.clean()
    } else if Path::new(&interpolated).is_relative() {
        let absolute_path = base_path.join(&interpolated);
        std::env::current_dir()?.join(absolute_path).clean()
    } else {
        // Cleaned like the two branches above, not left as written: an
        // absolute path is the one form a Git-included bundle can build
        // without knowing anything about the machine — `<{batect
        // .project_directory}/../../../etc` — and the containment check it
        // then faces compares path components without interpreting `..`.
        //
        // Cleaning *every* absolute path rather than only a bundle's is what
        // Batect does, so this is parity rather than a widening: its
        // `PathResolver.resolve` runs one expression for all paths,
        // `context.relativeTo.resolve(originalPath).normalize()
        //  .toAbsolutePath()`, and `Path.resolve` returns an absolute
        // argument unchanged — leaving `normalize()` to run on it. A `..`
        // whose parent is a symlink therefore resolves the same way under
        // both tools, which is the point. Flagged twice in review as a
        // behaviour change; recorded here so it isn't a third time.
        PathBuf::from(&interpolated).clean()
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

/// Serializes a merged, *unresolved* [`Config`] to native `ratect.toml` text —
/// the rendering half of `ratect config convert`. Goes through [`toml::Value`]
/// rather than serializing the struct directly, so scalar fields are emitted
/// before table fields (raw struct serialization would produce a value after a
/// `[table]` header — invalid TOML — because `Container` interleaves the two).
///
/// Proves the result **round-trips**: it parses the generated TOML straight
/// back into a `Config` and checks the two are identical, so a lossy
/// conversion — most plausibly in the hand-written `volumes`/`ports`/`devices`
/// (de)serializers — can't be written out silently. This works precisely
/// because both formats target the same `Config`: everything that survives to
/// that model is preserved, and the things that don't (comments, key order)
/// are exactly what a converted file is expected to lose.
pub fn to_native_toml(config: &Config) -> Result<String> {
    let value = toml::Value::try_from(config).context("serializing the configuration")?;
    let text = toml::to_string_pretty(&value).context("rendering TOML")?;

    let reparsed: Config = toml::from_str(&text).context("re-parsing the generated TOML")?;
    let reparsed_value =
        toml::Value::try_from(&reparsed).context("re-serializing the generated TOML")?;
    anyhow::ensure!(
        reparsed_value == value,
        "the conversion did not round-trip losslessly — this is a bug; please report it"
    );
    Ok(text)
}

/// The task names `config_file` defines, following its `include`s — for shell
/// completion, which must be instant and side-effect-free, so this is
/// deliberately *not* a real load. It parses each file (TOML or YAML by
/// extension) and collects task names, but resolves no expressions, reaches no
/// Docker, and — the load-bearing part — **never touches the network**: local
/// includes are followed, and a `type: git` include only if its repository is
/// *already* cached (see [`crate::git_include::cached_working_copy`]), never
/// cloning. Any unreadable or half-written file contributes nothing rather than
/// producing an error, which is the only sane behaviour on `<TAB>`.
///
/// It therefore mirrors the loader's decisions about **which files are read**,
/// and deliberately not its decisions about **whether to fail**. That is why
/// both the nested-Git gate and a bundle's [`GitBoundary`] containment are
/// honoured here — each stops a file being read at all, so ignoring either
/// would offer tasks the loader never sees, and in containment's case would
/// read a file outside the clone that the loader refuses outright — while
/// [`include_trust::EffectiveGrants`]'s lost-grant refusal is not: it fires
/// only on an include whose target was *already* loaded by an earlier route,
/// so it changes nothing about the set of files, only whether the load
/// aborts. Honouring it would mean completing to nothing for a config whose
/// task names are all perfectly well known — the wrong trade at a `<TAB>`,
/// where the user is most likely reaching for the very task that will print
/// the refusal and tell them how to fix it.
pub fn task_names_for_completion(config_file: &Path) -> Vec<String> {
    task_names_for_completion_in(config_file, None)
}

/// [`task_names_for_completion`], against a named Git-include cache directory
/// — `None` being the real `~/.ratect/incl`. The seam exists so this walk can
/// be tested at all: reaching its `type: git` arm needs a populated cache, and
/// pointing at the developer's own would make the test depend on what they
/// happen to have cloned.
fn task_names_for_completion_in(config_file: &Path, cache_root: Option<&Path>) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut visited = HashSet::new();
    collect_completion_task_names(config_file, &mut names, &mut visited, None, cache_root);
    names.into_iter().collect()
}

/// Recursive worker for [`task_names_for_completion`]. `visited` (cleaned
/// absolute paths) dedups shared includes and stops a cycle; every fallible
/// step is a silent early return, never an error.
fn collect_completion_task_names(
    config_file: &Path,
    names: &mut std::collections::BTreeSet<String>,
    visited: &mut HashSet<PathBuf>,
    declaring: Option<GitBoundary>,
    cache_root: Option<&Path>,
) {
    let Ok(absolute) = absolute_path(config_file) else {
        return;
    };
    if !visited.insert(absolute.clone()) {
        return;
    }
    let Ok(file) = parse_config_file(&absolute, ConfigFormat::Native) else {
        return;
    };
    names.extend(file.tasks.into_keys());

    let base_dir = absolute.parent().unwrap_or(Path::new(""));
    for include in file.include {
        let bundle = declaring.as_ref().map(|boundary| &boundary.bundle);
        let (next_file, declaring) = match include {
            // A local include is the declaring file's own tree, so it carries
            // that file's boundary unchanged — exactly as the loader propagates
            // one through `type: file`.
            IncludeEntry::File { path } => (base_dir.join(path), declaring.clone()),
            IncludeEntry::Git {
                repo,
                git_ref,
                path,
                // `allow_host_paths` matters to the loader but not here:
                // completion only ever *reads* task names, so there are no
                // container paths to contain. It is still carried through
                // [`Bundle::granted`] rather than dropped, so the one rule
                // stays one rule.
                allow_host_paths,
                allow_nested_git_includes,
            } => {
                // The loader refuses this include, so offering its tasks would
                // complete a `ratect run` that then fails. Completion is
                // native-only, which is the format that has the gate at all.
                let restricted = include_trust::restricting(bundle, ConfigFormat::Native);
                if include_trust::refusing_nested_git(restricted).is_some() {
                    continue;
                }
                // Completion never clones — only an already-cached repo counts.
                let Some(repo_dir) =
                    crate::git_include::cached_working_copy(&repo, &git_ref, cache_root)
                else {
                    continue;
                };
                let candidates: Vec<String> = match path {
                    Some(path) => vec![path],
                    None => git_bundle_candidates(ConfigFormat::Native)
                        .iter()
                        .map(|name| name.to_string())
                        .collect(),
                };
                let boundary = GitBoundary {
                    repo_dir: repo_dir.clone(),
                    bundle: Bundle::granted(
                        bundle,
                        BundleId {
                            remote: repo,
                            git_ref,
                        },
                        Grants {
                            host_paths: allow_host_paths,
                            nested_git: allow_nested_git_includes,
                        },
                    ),
                };
                // Containment before `is_file`, as in the loader, so a `path`
                // engineered to escape is dropped without the filesystem being
                // touched where it points.
                match candidates
                    .iter()
                    .filter_map(|candidate| absolute_path(&repo_dir.join(candidate)).ok())
                    .find(|candidate| {
                        boundary.check_contains(candidate).is_ok() && candidate.is_file()
                    }) {
                    Some(bundle_file) => (bundle_file, Some(boundary)),
                    None => continue,
                }
            }
        };
        // Absolute and normalized before anything looks at it — what the
        // loader's `resolve_include_target` does, and what makes `visited`
        // dedup two spellings of one file.
        let Ok(next_file) = absolute_path(&next_file) else {
            continue;
        };
        // The second half of the loader's containment check, against the
        // symlink-resolved forms. A boundary declines rather than errors here,
        // like every other fallible step on this walk.
        if let Some(boundary) = &declaring {
            if boundary.check_contains(&next_file).is_err()
                || boundary.check_contains_canonical(&next_file).is_err()
            {
                continue;
            }
        }
        collect_completion_task_names(&next_file, names, visited, declaring, cache_root);
    }
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
    load_project_impl(config_file, config_var_overrides, ConfigFormat::Compat).await
}

/// Native-mode counterpart of [`load_project`] — `ratect`'s entry point. A
/// TOML root file, TOML/YAML includes by extension (see
/// [`ConfigFormat::Native`]).
pub async fn load_project_native(
    config_file: &Path,
    config_var_overrides: &HashMap<String, String>,
) -> Result<LoadedProject> {
    load_project_impl(config_file, config_var_overrides, ConfigFormat::Native).await
}

async fn load_project_impl(
    config_file: &Path,
    config_var_overrides: &HashMap<String, String>,
    format: ConfigFormat,
) -> Result<LoadedProject> {
    if !config_file.exists() {
        anyhow::bail!("Configuration file {:?} not found.", config_file);
    }
    let mut loaded = match format {
        ConfigFormat::Compat => Config::load_from_file(config_file).await?,
        ConfigFormat::Native => Config::load_from_file_native(config_file).await?,
    };
    if format == ConfigFormat::Compat {
        reject_extends_in_compat(&loaded.config)?;
        reject_shared_caches_in_compat(&loaded.config)?;
        validate_image_sources_in_compat(&loaded.config.containers)?;
        // Before `resolve_expressions` below, so it judges what was written
        // rather than what an expression resolved to.
        reject_image_expressions_in_compat(&loaded.config)?;
    }
    let base_path = base_path_for(config_file);
    let project_directory = project_directory_path(base_path)?;
    loaded.resolve_expressions(base_path, config_var_overrides)?;
    let mut config = loaded.config;
    // Resolved *after* expression/path resolution, so an inherited relative
    // path is already absolute and stays anchored to its parent's own file —
    // see [`Container::extends`] and decisions/0003.
    if format == ConfigFormat::Native {
        resolve_extends(&mut config.containers)?;
    }
    // After `extends`, so an inherited cache mount is judged on the scope
    // the container effectively has.
    reject_conflicting_cache_scopes(&config)?;
    Ok(LoadedProject {
        config,
        project_directory,
    })
}

/// Rejects a container whose image source is ambiguous or absent, and the
/// `build_*` fields that only mean something for a build.
///
/// Ported from Batect's own `resolveImageSource`, which rejects all seven
/// combinations below. Ratect previously accepted every one of them and let
/// `resolve_image`'s precedence decide silently: `image` wins over
/// `build_directory`, so a container with both quietly never built, and
/// `build_args`/`build_ssh`/`build_secrets` alongside `image` were read by
/// nothing at all. Configuring a build secret and having it ignored without
/// a word is the failure this exists to prevent.
///
/// **Compat-only, and this one really is a format difference rather than a
/// convenience.** `ratect`'s native format has `extends`, which gives every
/// combination here a defined meaning it lacks in a `batect.yml`: a base
/// container legitimately has *neither* field (ADR-0003's "no `abstract`
/// marker needed" — pinned by `ratect`'s own
/// `a_base_only_container_needs_no_image_and_validates`), and because
/// inheritance is `child.or(parent)` with no way to unset, `image` on a
/// child is the *only* way to override a parent's `build_directory`, which
/// necessarily leaves both set. Applying these checks there would forbid the
/// format's headline reuse pattern to gain a diagnostic. Native keeps
/// today's lazy behaviour: the requirement is enforced when a task actually
/// runs a container.
///
/// Errors name the container, where Batect names a line and column instead
/// — it keeps positions on its parsed nodes and Ratect doesn't, and the
/// container name is what the rest of Ratect's config errors identify.
fn validate_image_sources_in_compat(containers: &HashMap<String, Container>) -> Result<()> {
    // Sorted, so a project with more than one offending container always
    // reports the same one rather than whichever the hash order surfaced.
    let mut names: Vec<&String> = containers.keys().collect();
    names.sort_unstable();

    for name in names {
        let container = &containers[name];
        match (&container.image, &container.build_directory) {
            (Some(_), Some(_)) => anyhow::bail!(
                "Container '{name}' has both 'image' and 'build_directory', but only one of \
                 the two can be given."
            ),
            // Deliberately the same wording as `engine.rs`'s own lazy check,
            // which still fires for the native format (and for a `Config`
            // built without going through `load_project`). One condition
            // should read the same however it was reached.
            (None, None) => {
                anyhow::bail!("Container '{name}' has neither 'image' nor 'build_directory' set")
            }
            _ => {}
        }

        if container.image.is_none() {
            continue;
        }
        let build_only = [
            ("build_args", container.build_args.is_some()),
            ("build_target", container.build_target.is_some()),
            ("dockerfile", container.dockerfile.is_some()),
            ("build_secrets", container.build_secrets.is_some()),
            ("build_ssh", container.build_ssh.is_some()),
        ];
        if let Some((field, _)) = build_only.iter().find(|(_, present)| *present) {
            anyhow::bail!(
                "Container '{name}' has '{field}', which cannot be used with 'image' — it \
                 only applies to a container built from a 'build_directory'."
            );
        }
    }
    Ok(())
}

/// `extends` is a `ratect`-native field; a `batect.yml` that uses it is
/// rejected rather than silently ignored, keeping `ratect-compat` a faithful
/// Batect replacement (Batect has no such field).
fn reject_extends_in_compat(config: &Config) -> Result<()> {
    let mut offenders: Vec<&str> = config
        .containers
        .iter()
        .filter(|(_, container)| container.extends.is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    offenders.sort_unstable();
    if let Some(name) = offenders.first() {
        anyhow::bail!(
            "The container '{name}' uses 'extends', which is a ratect-native field \
             not supported in Batect-compatible configuration."
        );
    }
    Ok(())
}

/// Rejects a cache `name` that isn't a safe single path segment.
///
/// A cache name is joined onto a directory to produce a host path
/// (`.batect/caches/<name>`, or `~/.ratect/caches/<name>` when shared), and
/// `Path::join` neither rejects `..` nor refuses an absolute path — it
/// *replaces* the base with one. So without this, `name = "/etc"` or
/// `name = "../../.ssh"` gets an arbitrary host directory created and
/// bind-mounted read-write into the container, under `--cache-type
/// directory`.
///
/// That matters most for a **Git-included bundle**, which is configuration
/// the project owner may not have written: containment
/// ([decisions/0004](https://github.com/or1can/ratect/blob/main/decisions/0004-git-include-host-path-trust.md))
/// deliberately checks `local` mounts and include paths, and just as
/// deliberately skips `cache` mounts — because a cache name was never
/// supposed to be a path.
///
/// The permitted shape is Docker's own volume-name character set. Under
/// `--cache-type volume` Docker already enforces it, so this only tightens
/// the directory form to match, and no name that works today stops working.
/// Batect performs no equivalent check; diverging in the safer direction is
/// the same call [decisions/0004](https://github.com/or1can/ratect/blob/main/decisions/0004-git-include-host-path-trust.md)
/// made for include containment.
fn validate_cache_name(name: &str) -> Result<()> {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        anyhow::bail!(
            "has a 'cache' volume mount named '{name}', which \
             is not a valid cache name. A cache name must start with a letter or digit and \
             contain only letters, digits, underscores, dots and dashes — it names storage, \
             not a path."
        );
    }
    Ok(())
}

/// Rejects an expression in a container's `image` in a `batect.yml`.
///
/// The objection is **one-way lock-in**: a `batect.yml` using it stops working
/// under real `batect`, and "you can go back" is the whole proposition of the
/// compat binary. A parameterised image tag would be used on every pipeline,
/// so that lock-in would be routine rather than incidental — which is what
/// separates this from the `Capability` superset. A `ratect.toml` cannot run
/// under Batect anyway, so it creates none of it.
///
/// **This is a breaking change, and the tempting justification for it is
/// wrong.** "`$`/`{`/`}` are invalid in a Docker reference, so such a config
/// already fails everywhere" holds only for a container that actually runs.
/// Checked over the whole file *before task selection*, this also rejects
/// three configurations that worked before and work under `batect` today.
/// In rising order of how often they occur: a container whose image
/// `--override-image` replaces; a container nothing references at all; and
/// — the common one — a container used only by some *other* task, which now
/// fails every task in the file rather than only the one that would have
/// broken. All three are rejected deliberately — the latent portability bug is worth surfacing at
/// load rather than on the day someone adds a task that uses the container —
/// but the cost is real and is recorded in CHANGELOG.md rather than papered
/// over here. Whole-file is also what [`validate_image_sources_in_compat`]
/// does, for the same reason.
fn reject_image_expressions_in_compat(config: &Config) -> Result<()> {
    let mut offenders: Vec<(&str, &str)> = config
        .containers
        .iter()
        .filter_map(|(container_name, container)| {
            let image = container.image.as_deref()?;
            crate::expressions::contains_expression(image)
                .then_some((container_name.as_str(), image))
        })
        .collect();
    // Sorted on the whole pair, so a project with several always reports the
    // same one — `containers` is a `HashMap` whose order varies between runs.
    offenders.sort_unstable();
    if let Some((container_name, image)) = offenders.first() {
        anyhow::bail!(
            // Names what to do, not a flag that would fix this file: the
            // check runs at load, so '--override-image' does not get past it
            // — the expression has to go first.
            "Container '{container_name}' uses an expression in its 'image' ('{image}'), which \
             is a ratect-native feature not supported in Batect-compatible configuration. \
             Batect resolves no expression there, so a file using one would stop working under \
             'batect' itself. Write a fixed image here; '--override-image {container_name}=...' \
             is how this binary chooses one per run."
        );
    }
    Ok(())
}

/// Rejects `scope` on a cache mount in a `batect.yml`, for the same reason
/// as [`reject_extends_in_compat`]: Batect has no such field, so accepting
/// it would let a config be written here that real `batect` refuses. The
/// default (`Project`) is Batect's only behaviour, so nothing is lost —
/// this only rejects a file that asked for the native one.
fn reject_shared_caches_in_compat(config: &Config) -> Result<()> {
    let mut offenders: Vec<(&str, &str)> = config
        .containers
        .iter()
        .flat_map(|(container_name, container)| {
            container
                .volumes
                .iter()
                .flatten()
                .filter_map(move |volume| match volume {
                    // Presence, not value: real `batect` rejects any unknown
                    // property, so `scope: project` is just as unloadable
                    // there as `scope: shared`.
                    VolumeMount::Cache(cache) if cache.scope.is_some() => {
                        Some((container_name.as_str(), cache.name.as_str()))
                    }
                    _ => None,
                })
        })
        .collect();
    offenders.sort_unstable();
    if let Some((container_name, cache_name)) = offenders.first() {
        anyhow::bail!(
            "The container '{container_name}' declares the cache '{cache_name}' with a \
             'scope', which is a ratect-native field not supported in \
             Batect-compatible configuration."
        );
    }
    Ok(())
}

/// Rejects a project that gives one cache name two different scopes.
///
/// The name maps to storage — a project-scoped `cargo` and a shared `cargo`
/// are two different volumes — so one name meaning both is incoherent, and
/// `ratect caches clean cargo` could not say which was meant. Checked across
/// the whole project rather than per container, because two *containers*
/// naming the same cache is the ordinary way to share one between them.
fn reject_conflicting_cache_scopes(config: &Config) -> Result<()> {
    let mut seen: std::collections::BTreeMap<&str, CacheScope> = std::collections::BTreeMap::new();
    // Sorted, so a project with more than one conflict always reports the
    // same one — `config.containers` is a `HashMap`, and its order varies
    // between runs. Same reason [`reject_shared_caches_in_compat`] sorts.
    //
    // The container name breaks ties, which is what makes this an order at
    // all: two containers can share a lowest cache name, and a sort that
    // leaves them equal falls back to the `HashMap` order this exists to
    // escape.
    fn lowest_cache_name(container: &Container) -> &str {
        container
            .volumes
            .iter()
            .flatten()
            .filter_map(|volume| match volume {
                VolumeMount::Cache(cache) => Some(cache.name.as_str()),
                _ => None,
            })
            .min()
            .unwrap_or("")
    }
    let mut containers: Vec<(&String, &Container)> = config.containers.iter().collect();
    containers.sort_by(|(left_name, left), (right_name, right)| {
        lowest_cache_name(left)
            .cmp(lowest_cache_name(right))
            .then_with(|| left_name.cmp(right_name))
    });
    for (_, container) in containers {
        for volume in container.volumes.iter().flatten() {
            let VolumeMount::Cache(cache) = volume else {
                continue;
            };
            // The *effective* scope: an omitted `scope` is `project`, so
            // one entry writing it out and another leaving it off is not a
            // conflict.
            match seen.get(cache.name.as_str()) {
                Some(scope) if *scope != cache.scope() => anyhow::bail!(
                    "The cache '{}' is declared with both 'project' and 'shared' scope. \
                     A cache name refers to one piece of storage, so it can only have one.",
                    cache.name
                ),
                _ => {
                    seen.insert(cache.name.as_str(), cache.scope());
                }
            }
        }
    }
    Ok(())
}

/// Resolves every container's `extends` — `ratect`'s native inheritance,
/// replacing YAML anchors. Shallow, per field (`child.or(parent)`, exactly
/// Cargo's profile `inherits`): a field the child sets wins, an unset one is
/// taken from the (already-resolved) parent, with no recursion into nested
/// maps. Single-parent, transitive, and cycle-checked. Runs on already
/// path/expression-resolved containers, so an inherited relative path points
/// where it did on the parent. See decisions/0003.
fn resolve_extends(containers: &mut HashMap<String, Container>) -> Result<()> {
    let mut resolved: HashMap<String, Container> = HashMap::new();
    let names: Vec<String> = containers.keys().cloned().collect();
    for name in names {
        resolve_container_extends(&name, containers, &mut resolved, &mut Vec::new())?;
    }
    *containers = resolved;
    Ok(())
}

/// Resolves one container's inheritance chain into `resolved`, memoized so a
/// shared ancestor is merged once. `ancestors` is the current resolution
/// path, for detecting cycles (including a container extending itself).
fn resolve_container_extends(
    name: &str,
    source: &HashMap<String, Container>,
    resolved: &mut HashMap<String, Container>,
    ancestors: &mut Vec<String>,
) -> Result<()> {
    if resolved.contains_key(name) {
        return Ok(());
    }
    if ancestors.iter().any(|ancestor| ancestor == name) {
        ancestors.push(name.to_string());
        anyhow::bail!(
            "The container '{}' has a cyclic 'extends': {}",
            name,
            ancestors.join(" -> ")
        );
    }

    let mut merged = source
        .get(name)
        .expect("resolve_container_extends called with a known container name")
        .clone();

    if let Some(parent) = merged.extends.clone() {
        if !source.contains_key(&parent) {
            anyhow::bail!("The container '{name}' extends '{parent}', which is not defined.");
        }
        ancestors.push(name.to_string());
        resolve_container_extends(&parent, source, resolved, ancestors)?;
        ancestors.pop();
        let parent = resolved
            .get(&parent)
            .expect("parent resolved by the recursive call above")
            .clone();
        inherit_container_fields(&mut merged, parent);
    }

    // Consumed: the resolved container carries no `extends` of its own.
    merged.extends = None;
    resolved.insert(name.to_string(), merged);
    Ok(())
}

/// Fills every field the `child` left unset from `parent` — the shallow,
/// per-field half of [`resolve_extends`]. `parent` is owned (a resolved clone)
/// so each field moves in without a further clone; `extends` itself is never
/// inherited (it's structural, already consumed). Exhaustive by design: a new
/// `Container` field that isn't listed here silently wouldn't inherit, so the
/// missing-field compile error is the check that keeps this in step with the
/// struct.
fn inherit_container_fields(child: &mut Container, parent: Container) {
    let Container {
        extends: _,
        image,
        image_pull_policy,
        build_directory,
        build_args,
        dockerfile,
        build_target,
        build_secrets,
        build_ssh,
        volumes,
        dependencies,
        environment,
        run_as_current_user,
        additional_hostnames,
        additional_hosts,
        ports,
        health_check,
        setup_commands,
        working_directory,
        command,
        entrypoint,
        labels,
        capabilities_to_add,
        capabilities_to_drop,
        privileged,
        shm_size,
        devices,
        enable_init_process,
        log_driver,
        log_options,
    } = parent;
    child.image = child.image.take().or(image);
    child.image_pull_policy = child.image_pull_policy.take().or(image_pull_policy);
    child.build_directory = child.build_directory.take().or(build_directory);
    child.build_args = child.build_args.take().or(build_args);
    child.dockerfile = child.dockerfile.take().or(dockerfile);
    child.build_target = child.build_target.take().or(build_target);
    child.build_secrets = child.build_secrets.take().or(build_secrets);
    child.build_ssh = child.build_ssh.take().or(build_ssh);
    child.volumes = child.volumes.take().or(volumes);
    child.dependencies = child.dependencies.take().or(dependencies);
    child.environment = child.environment.take().or(environment);
    child.run_as_current_user = child.run_as_current_user.take().or(run_as_current_user);
    child.additional_hostnames = child.additional_hostnames.take().or(additional_hostnames);
    child.additional_hosts = child.additional_hosts.take().or(additional_hosts);
    child.ports = child.ports.take().or(ports);
    child.health_check = child.health_check.take().or(health_check);
    child.setup_commands = child.setup_commands.take().or(setup_commands);
    child.working_directory = child.working_directory.take().or(working_directory);
    child.command = child.command.take().or(command);
    child.entrypoint = child.entrypoint.take().or(entrypoint);
    child.labels = child.labels.take().or(labels);
    child.capabilities_to_add = child.capabilities_to_add.take().or(capabilities_to_add);
    child.capabilities_to_drop = child.capabilities_to_drop.take().or(capabilities_to_drop);
    child.privileged = child.privileged.take().or(privileged);
    child.shm_size = child.shm_size.take().or(shm_size);
    child.devices = child.devices.take().or(devices);
    child.enable_init_process = child.enable_init_process.take().or(enable_init_process);
    child.log_driver = child.log_driver.take().or(log_driver);
    child.log_options = child.log_options.take().or(log_options);
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
