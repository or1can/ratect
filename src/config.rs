use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub project_name: String,
    pub containers: HashMap<String, Container>,
    pub tasks: HashMap<String, Task>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Container {
    pub image: Option<String>,
    pub build_directory: Option<String>,
    pub volumes: Option<Vec<String>>,
    pub dependencies: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Task {
    pub run: TaskRun,
    pub prerequisites: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
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
