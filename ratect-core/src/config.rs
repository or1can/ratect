use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

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
    pub volumes: Option<Vec<String>>,
    pub dependencies: Option<Vec<String>>,
    pub environment: Option<HashMap<String, String>>,
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigVariable {
    pub default: Option<String>,
}

impl Config {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("Failed to open config file {:?}", path))?;
        let mut config: Config = noyalib::from_reader(file)
            .with_context(|| format!("Failed to parse config file {:?}", path))?;

        let base_path = path.parent().unwrap_or(Path::new("."));
        config.resolve_paths(base_path)?;

        Ok(config)
    }

    /// Loads a `--config-vars-file`: a flat YAML map of config variable
    /// names to values, in the same format/parser as `batect.yml` itself.
    pub fn load_config_vars_file(path: &Path) -> Result<HashMap<String, String>> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open config vars file {:?}", path))?;
        noyalib::from_reader(file)
            .with_context(|| format!("Failed to parse config vars file {:?}", path))
    }

    /// Resolves every `environment` value (on containers and task `run`s)
    /// through Batect's expression syntax — `$VAR`/`${VAR}`/`${VAR:-default}`
    /// against the real host environment, and `<name`/`<{name}` against
    /// `config_variables`, merged with `config_var_overrides` (highest
    /// precedence — from `--config-var`/`--config-vars-file`).
    pub fn resolve_environment(
        &mut self,
        config_var_overrides: &HashMap<String, String>,
    ) -> Result<()> {
        self.resolve_environment_with(config_var_overrides, |name| std::env::var(name).ok())
    }

    /// The actual implementation behind [`resolve_environment`](Self::resolve_environment),
    /// parameterized over the host environment lookup so tests don't have to
    /// touch the real process environment.
    fn resolve_environment_with(
        &mut self,
        config_var_overrides: &HashMap<String, String>,
        host_env: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
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

        for container in self.containers.values_mut() {
            if let Some(environment) = &mut container.environment {
                for value in environment.values_mut() {
                    *value = crate::expressions::interpolate(value, &host_env, &config_vars)?;
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

    fn resolve_paths(&mut self, base_path: &Path) -> Result<()> {
        for container in self.containers.values_mut() {
            if let Some(volumes) = &mut container.volumes {
                for volume in volumes {
                    let parts: Vec<&str> = volume.split(':').collect();
                    if parts.len() == 2 {
                        let host_path = Path::new(parts[0]);
                        if host_path.is_relative() {
                            let absolute_host_path = base_path.join(host_path);
                            // We use absolute() if available, but for compatibility let's just use join and canonicalize if it exists
                            // Or better, just join and use display() if we want to avoid requiring the path to exist.
                            // Docker usually requires absolute paths.
                            let resolved = std::env::current_dir()?.join(absolute_host_path);
                            *volume = format!("{}:{}", resolved.display(), parts[1]);
                        }
                    }
                }
            }
        }
        Ok(())
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
    fn resolve_paths_makes_relative_volume_absolute() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
tasks: {}
"#,
        );

        config.resolve_paths(Path::new("/base")).unwrap();

        let volume = &config.containers["build-env"].volumes.as_ref().unwrap()[0];
        assert_eq!(volume, "/base/code:/code");
    }

    #[test]
    fn resolve_paths_leaves_absolute_volume_unchanged() {
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - /already/absolute:/code
tasks: {}
"#,
        );

        config.resolve_paths(Path::new("/base")).unwrap();

        let volume = &config.containers["build-env"].volumes.as_ref().unwrap()[0];
        assert_eq!(volume, "/already/absolute:/code");
    }

    #[test]
    fn resolve_paths_leaves_malformed_volume_spec_unchanged() {
        // Three colon-separated parts (e.g. a Windows drive-letter host path) don't
        // match the `host:container` shape this resolver understands, so it's left as-is.
        let mut config = parse(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - "C:/data:/code:ro"
tasks: {}
"#,
        );

        config.resolve_paths(Path::new("/base")).unwrap();

        let volume = &config.containers["build-env"].volumes.as_ref().unwrap()[0];
        assert_eq!(volume, "C:/data:/code:ro");
    }

    /// A fresh, unique scratch directory for tests that need to write real
    /// files to disk (e.g. to exercise `load_from_file`'s own file I/O,
    /// not just YAML parsing). Caller is responsible for cleanup via
    /// `std::fs::remove_dir_all`.
    fn unique_temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ratect-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_from_file_parses_and_resolves_paths() {
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

        let config = Config::load_from_file(&config_path).unwrap();

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
            image: Some("alpine:3.18".to_string()),
            build_directory: None,
            volumes: None,
            dependencies: None,
            environment: Some(environment),
        }
    }

    #[test]
    fn resolve_environment_expands_host_var() {
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
            .resolve_environment_with(&HashMap::new(), |name| {
                (name == "HOST_VAR").then(|| "host-value".to_string())
            })
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "host-value"
        );
    }

    #[test]
    fn resolve_environment_uses_default_when_host_var_unset() {
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
            .resolve_environment_with(&HashMap::new(), |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "fallback"
        );
    }

    #[test]
    fn resolve_environment_errors_when_host_var_unset_without_default() {
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

        let result = config.resolve_environment_with(&HashMap::new(), |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_environment_prefers_cli_override_over_default() {
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
            .resolve_environment_with(&overrides, |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "prod"
        );
    }

    #[test]
    fn resolve_environment_falls_back_to_config_variable_default() {
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
            .resolve_environment_with(&HashMap::new(), |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "dev"
        );
    }

    #[test]
    fn resolve_environment_errors_on_undeclared_config_variable_reference() {
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

        let result = config.resolve_environment_with(&HashMap::new(), |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_environment_errors_on_declared_config_variable_with_no_value() {
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

        let result = config.resolve_environment_with(&HashMap::new(), |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_environment_errors_on_unknown_cli_override() {
        let mut config = Config {
            project_name: "demo".to_string(),
            containers: HashMap::new(),
            tasks: HashMap::new(),
            config_variables: None,
        };

        let overrides = HashMap::from([("unknown".to_string(), "value".to_string())]);
        let result = config.resolve_environment_with(&overrides, |_| None);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_environment_leaves_literal_values_unchanged() {
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
            .resolve_environment_with(&HashMap::new(), |_| None)
            .unwrap();

        assert_eq!(
            config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
            "literal-value"
        );
    }
}
