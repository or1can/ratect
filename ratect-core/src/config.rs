use anyhow::{Context, Result};
use path_clean::PathClean;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub image: Option<String>,
    pub build_directory: Option<String>,
    pub build_args: Option<HashMap<String, String>>,
    pub volumes: Option<Vec<String>>,
    pub dependencies: Option<Vec<String>>,
    pub environment: Option<HashMap<String, String>>,
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
}

/// Runs this container as the host's own user/group instead of whatever the
/// image defaults to (see [`Config::resolve_expressions_with`]'s validation
/// and `TaskEngine::resolve_user_mapping`). `home_directory` is required
/// when `enabled` is `true` (and rejected otherwise) — Ratect never guesses
/// one, matching Batect.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAsCurrentUser {
    pub enabled: bool,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub run: TaskRun,
    pub prerequisites: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRun {
    pub container: String,
    pub command: Option<String>,
    pub environment: Option<HashMap<String, String>>,
    /// Additional port mappings for this task's run specifically —
    /// *added* to the container's own `ports` (a union, not an override:
    /// matching Batect, which combines these as a `Set`, so there's no
    /// concept of one replacing an entry from the other by container
    /// port). See [`Container::ports`].
    pub ports: Option<Vec<PortMapping>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigVariable {
    pub default: Option<String>,
}

impl Config {
    /// Parses the config file only — no path resolution or expression
    /// interpolation. Those need `config_var_overrides` from the CLI
    /// (`--config-var`/`--config-vars-file`), which aren't known yet at this
    /// point, so callers must follow up with
    /// [`resolve_expressions`](Self::resolve_expressions).
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("Failed to open config file {:?}", path))?;
        noyalib::from_reader(file)
            .with_context(|| format!("Failed to parse config file {:?}", path))
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
        self.resolve_expressions_with(base_path, config_var_overrides, |name| {
            std::env::var(name).ok()
        })
    }

    /// The actual implementation behind [`resolve_expressions`](Self::resolve_expressions),
    /// parameterized over the host environment lookup so tests don't have to
    /// touch the real process environment.
    fn resolve_expressions_with(
        &mut self,
        base_path: &Path,
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
        // overridden. `.clean()`d for the same reason as `resolve_path`
        // below — `base_path` is frequently "" or "." (e.g. from `-f
        // batect.yml` or `-f ./batect.yml`), which without cleaning would
        // leave a trailing slash or `/.` in every value derived from this.
        let project_directory = std::env::current_dir()?
            .join(base_path)
            .clean()
            .display()
            .to_string();
        config_vars.insert(PROJECT_DIRECTORY_VAR.to_string(), Some(project_directory));

        for (container_name, container) in self.containers.iter_mut() {
            if let Some(environment) = &mut container.environment {
                for value in environment.values_mut() {
                    *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
                }
            }
            if let Some(volumes) = &mut container.volumes {
                for volume in volumes {
                    *volume = resolve_volume(volume, base_path, &host_env, &config_vars)?;
                }
            }
            if let Some(build_directory) = &mut container.build_directory {
                *build_directory =
                    resolve_path(build_directory, base_path, &host_env, &config_vars)?;
            }
            if let Some(build_args) = &mut container.build_args {
                for value in build_args.values_mut() {
                    *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
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
                } else if run_as_current_user.home_directory.is_some() {
                    anyhow::bail!(
                        "Container '{}' has 'run_as_current_user.home_directory' set, but \
                         'run_as_current_user.enabled' is not true",
                        container_name
                    );
                }
            }
        }

        for task in self.tasks.values_mut() {
            if let Some(environment) = &mut task.run.environment {
                for value in environment.values_mut() {
                    *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
                }
            }
        }

        Ok(())
    }
}

/// Interpolates expressions within a volume spec's host-path segment, then
/// resolves the result to an absolute path (relative to `base_path`) if
/// it's relative. Volume specs that don't split into exactly two
/// `:`-separated parts (e.g. a three-part `host:container:ro` spec, or a
/// Windows drive-letter path) are left completely untouched, including no
/// interpolation — ambiguous to parse, so left for the user to write
/// literally, matching this resolver's pre-existing behavior for that case.
fn resolve_volume(
    volume: &str,
    base_path: &Path,
    host_env: &impl Fn(&str) -> Option<String>,
    config_vars: &HashMap<String, Option<String>>,
) -> Result<String> {
    let parts: Vec<&str> = volume.split(':').collect();
    if parts.len() != 2 {
        return Ok(volume.to_string());
    }

    let resolved_host_path = resolve_path(parts[0], base_path, host_env, config_vars)?;

    Ok(format!("{}:{}", resolved_host_path, parts[1]))
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
) -> Result<String> {
    let interpolated = crate::expressions::interpolate(path, host_env, config_vars)?;
    if Path::new(&interpolated).is_relative() {
        let absolute_path = base_path.join(&interpolated);
        Ok(std::env::current_dir()?
            .join(absolute_path)
            .clean()
            .display()
            .to_string())
    } else {
        Ok(interpolated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(yaml: &str) -> Config {
        noyalib::from_reader(Cursor::new(yaml.as_bytes())).expect("valid yaml")
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
            &vec!["code:/code".to_string()]
        );

        let task = config.tasks.get("test").unwrap();
        assert_eq!(task.run.container, "build-env");
        assert_eq!(task.run.command.as_deref(), Some("echo hi"));
        assert_eq!(
            task.prerequisites.as_ref().unwrap(),
            &vec!["other".to_string()]
        );
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

    fn no_host_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn resolve_volume_makes_relative_host_path_absolute() {
        let resolved = resolve_volume(
            "code:/code",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(resolved, "/base/code:/code");
    }

    #[test]
    fn resolve_volume_leaves_absolute_host_path_unchanged() {
        let resolved = resolve_volume(
            "/already/absolute:/code",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(resolved, "/already/absolute:/code");
    }

    #[test]
    fn resolve_volume_leaves_malformed_volume_spec_unchanged() {
        // Three colon-separated parts (e.g. a Windows drive-letter host path) don't
        // match the `host:container` shape this resolver understands, so it's left as-is
        // — no interpolation either, matching that "left completely unresolved" behavior.
        let resolved = resolve_volume(
            "C:/data:/code:ro",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(resolved, "C:/data:/code:ro");
    }

    #[test]
    fn resolve_volume_interpolates_relative_host_path_expression() {
        let config_vars = HashMap::from([("subdir".to_string(), Some("code".to_string()))]);
        let resolved = resolve_volume(
            "<subdir:/code",
            Path::new("/base"),
            &no_host_env,
            &config_vars,
        )
        .unwrap();
        assert_eq!(resolved, "/base/code:/code");
    }

    #[test]
    fn resolve_volume_interpolates_absolute_host_path_expression_without_prefixing_base_path() {
        // `<project_root` resolving to an absolute path must be used as-is,
        // not treated as a literal relative fragment of `base_path` the way
        // it would be if resolution happened before interpolation.
        let config_vars =
            HashMap::from([("project_root".to_string(), Some("/abs/root".to_string()))]);
        let resolved = resolve_volume(
            "<project_root:/code",
            Path::new("/base"),
            &no_host_env,
            &config_vars,
        )
        .unwrap();
        assert_eq!(resolved, "/abs/root:/code");
    }

    #[test]
    fn resolve_path_makes_relative_path_absolute() {
        let resolved =
            resolve_path("docker", Path::new("/base"), &no_host_env, &HashMap::new()).unwrap();
        assert_eq!(resolved, "/base/docker");
    }

    #[test]
    fn resolve_path_cleans_dot_components_from_the_joined_path() {
        let resolved = resolve_path(
            "./docker",
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
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
        )
        .unwrap();
        assert_eq!(resolved, "/abs/root");
    }

    fn container_with_build(
        build_directory: &str,
        build_args: HashMap<String, String>,
    ) -> Container {
        Container {
            image: None,
            build_directory: Some(build_directory.to_string()),
            build_args: Some(build_args),
            volumes: None,
            dependencies: None,
            environment: None,
            run_as_current_user: None,
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env)
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |name| {
                (name == "HOST_VAR").then(|| "host-value".to_string())
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].build_args.as_ref().unwrap()["MESSAGE"],
            "host-value"
        );
    }

    fn container_with_run_as_current_user(
        enabled: bool,
        home_directory: Option<&str>,
    ) -> Container {
        Container {
            image: Some("alpine:3.18".to_string()),
            build_directory: None,
            build_args: None,
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
        }
    }

    fn config_with_container(container: Container) -> Config {
        Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
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
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env)
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
    fn resolve_expressions_leaves_ports_untouched() {
        let mut config = config_with_container(Container {
            ports: Some(vec![port_mapping((8080, 8080), (80, 80), "tcp")]),
            ..container_with_build("docker", HashMap::new())
        });

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env)
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

        let result =
            config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env);

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

        let result =
            config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env);

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

        let result =
            config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env);

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("is not an absolute path"));
    }

    #[test]
    fn resolve_expressions_interpolates_run_as_current_user_home_directory() {
        let mut config = config_with_container(container_with_run_as_current_user(
            true,
            Some("/home/$HOST_VAR"),
        ));

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |name| {
                (name == "HOST_VAR").then(|| "container-user".to_string())
            })
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
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), no_host_env)
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

    #[test]
    fn load_from_file_then_resolve_expressions_resolves_paths() {
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

        let mut config = Config::load_from_file(&config_path).unwrap();
        config.resolve_expressions(&dir, &HashMap::new()).unwrap();

        let volume = &config.containers["build-env"].volumes.as_ref().unwrap()[0];
        assert_eq!(*volume, format!("{}:/code", dir.join("code").display()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_from_file_missing_file_errors() {
        let result = Config::load_from_file(Path::new("/nonexistent/batect.yml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_from_file_unsupported_key_errors() {
        let dir = unique_temp_dir();
        let config_path = dir.join("batect.yml");
        std::fs::write(
            &config_path,
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    working_directory: /code
tasks: {}
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&config_path);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
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
            task.run.environment.as_ref().unwrap().get("RUN_VAR"),
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

    fn container_with_environment(environment: HashMap<String, String>) -> Container {
        Container {
            build_args: None,
            image: Some("alpine:3.18".to_string()),
            build_directory: None,
            volumes: None,
            dependencies: None,
            environment: Some(environment),
            run_as_current_user: None,
            additional_hostnames: None,
            additional_hosts: None,
            ports: None,
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |name| {
                (name == "HOST_VAR").then(|| "host-value".to_string())
            })
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None)
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
        };

        let result = config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None);
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
        };

        let overrides = HashMap::from([("env_name".to_string(), "prod".to_string())]);
        config
            .resolve_expressions_with(Path::new("/base"), &overrides, |_| None)
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None)
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
        };

        let result = config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_errors_on_declared_config_variable_with_no_value() {
        let mut environment = HashMap::new();
        environment.insert("FOO".to_string(), "<env_name".to_string());
        let mut config_variables = HashMap::new();
        config_variables.insert("env_name".to_string(), ConfigVariable { default: None });
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([(
                "build-env".to_string(),
                container_with_environment(environment),
            )]),
            tasks: HashMap::new(),
            config_variables: Some(config_variables),
        };

        let result = config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_errors_on_unknown_cli_override() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: None,
        };

        let overrides = HashMap::from([("unknown".to_string(), "value".to_string())]);
        let result = config.resolve_expressions_with(Path::new("/base"), &overrides, |_| None);
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None)
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
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "/base"
        );
    }

    #[test]
    fn resolve_expressions_resolves_built_in_project_directory_var_in_volumes() {
        let mut container = container_with_environment(HashMap::new());
        container.volumes = Some(vec![
            "<{batect.project_directory}/scripts:/scripts".to_string()
        ]);
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::from([("build-env".to_string(), container)]),
            tasks: HashMap::new(),
            config_variables: None,
        };

        config
            .resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].volumes.as_ref().unwrap()[0],
            "/base/scripts:/scripts"
        );
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
        };

        config
            .resolve_expressions_with(Path::new(""), &HashMap::new(), |_| None)
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
            },
        );
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: Some(config_variables),
        };

        let result = config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_expressions_errors_if_project_directory_is_given_as_a_cli_override() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: None,
        };

        let overrides = HashMap::from([(
            "batect.project_directory".to_string(),
            "/hijacked".to_string(),
        )]);
        let result = config.resolve_expressions_with(Path::new("/base"), &overrides, |_| None);
        assert!(result.is_err());
    }
}
