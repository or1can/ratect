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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Container {
    pub image: Option<String>,
    pub build_directory: Option<String>,
    pub volumes: Option<Vec<String>>,
    pub dependencies: Option<Vec<String>>,
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

    #[test]
    fn load_from_file_parses_and_resolves_paths() {
        let dir = std::env::temp_dir().join(format!(
            "ratect-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
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
        let dir = std::env::temp_dir().join(format!(
            "ratect-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("batect.yml");
        std::fs::write(
            &config_path,
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    environment:
      FOO: bar
tasks: {}
"#,
        )
        .unwrap();

        let result = Config::load_from_file(&config_path);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
