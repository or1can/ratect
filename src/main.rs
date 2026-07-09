use anyhow::Result;
use clap::Parser;
use ratect_core::config::Config;
use ratect_core::docker::DockerClient;
use ratect_core::engine::TaskEngine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the configuration file
    #[arg(short = 'f', long, default_value = "batect.yml")]
    config_file: PathBuf,

    /// List available tasks and exit
    #[arg(short = 'T', long)]
    list_tasks: bool,

    /// Set a config variable's value, as NAME=VALUE (repeatable). Takes
    /// precedence over --config-vars-file and the variable's `default` in
    /// `config_variables`.
    #[arg(long = "config-var", value_parser = parse_config_var)]
    config_var: Vec<(String, String)>,

    /// Path to a YAML file of config variable NAME: VALUE pairs
    #[arg(long = "config-vars-file")]
    config_vars_file: Option<PathBuf>,

    /// Name of the task to run
    task_name: Option<String>,

    /// Additional arguments to pass to the task command
    #[arg(last = true)]
    additional_args: Vec<String>,
}

/// Parses a `--config-var` value of the form `NAME=VALUE`.
fn parse_config_var(s: &str) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, value)) => Ok((name.to_string(), value.to_string())),
        None => Err(format!("expected NAME=VALUE, got '{s}'")),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Use `{:?}` (not `{}`) so the full anyhow context chain is logged,
            // matching what the default Termination handler would have printed.
            tracing::error!("{:?}", err);

            // If the task's own command exited non-zero, propagate that exact
            // code as ratect's own exit code (matching `docker run`'s
            // convention) rather than a generic failure code, so scripts can
            // inspect what actually happened.
            match err.downcast_ref::<ratect_core::docker::ContainerExitedNonZero>() {
                Some(failure) => ExitCode::from(failure.exit_code as u8),
                None => ExitCode::FAILURE,
            }
        }
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();

    if !args.config_file.exists() {
        anyhow::bail!("Configuration file {:?} not found.", args.config_file);
    }
    let mut config = Config::load_from_file(&args.config_file)?;

    let mut config_var_overrides: HashMap<String, String> = match &args.config_vars_file {
        Some(path) => Config::load_config_vars_file(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(args.config_var.iter().cloned());
    let base_path = args.config_file.parent().unwrap_or(Path::new("."));
    config.resolve_expressions(base_path, &config_var_overrides)?;

    if args.list_tasks {
        println!("Tasks in {}:", config.project_name);
        let mut tasks: Vec<_> = config.tasks.keys().collect();
        tasks.sort();
        for task in tasks {
            println!("- {}", task);
        }
        return Ok(());
    }

    match args.task_name {
        Some(task_name) => {
            let docker = DockerClient::new()?;
            let engine = TaskEngine::new(config, docker);
            engine.run_task(&task_name, &args.additional_args).await?;
        }
        None => {
            tracing::warn!("No task name provided. Use --help for usage.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_batect_yml_with_no_task() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.config_file, PathBuf::from("batect.yml"));
        assert!(!args.list_tasks);
        assert_eq!(args.task_name, None);
        assert!(args.additional_args.is_empty());
    }

    #[test]
    fn parses_list_tasks_flag() {
        let args = Args::try_parse_from(["ratect", "--list-tasks"]).unwrap();
        assert!(args.list_tasks);

        let args = Args::try_parse_from(["ratect", "-T"]).unwrap();
        assert!(args.list_tasks);
    }

    #[test]
    fn parses_custom_config_file() {
        let args = Args::try_parse_from(["ratect", "-f", "custom.yml", "build"]).unwrap();
        assert_eq!(args.config_file, PathBuf::from("custom.yml"));
        assert_eq!(args.task_name.as_deref(), Some("build"));
    }

    #[test]
    fn parses_task_name_and_trailing_args() {
        let args = Args::try_parse_from(["ratect", "build", "--", "--flag", "value"]).unwrap();
        assert_eq!(args.task_name.as_deref(), Some("build"));
        assert_eq!(
            args.additional_args,
            vec!["--flag".to_string(), "value".to_string()]
        );
    }

    #[test]
    fn parses_repeated_config_var_flags() {
        let args = Args::try_parse_from([
            "ratect",
            "--config-var",
            "ENV=prod",
            "--config-var",
            "REGION=eu",
            "build",
        ])
        .unwrap();
        assert_eq!(
            args.config_var,
            vec![
                ("ENV".to_string(), "prod".to_string()),
                ("REGION".to_string(), "eu".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_config_var_without_equals_sign() {
        let result = Args::try_parse_from(["ratect", "--config-var", "NOEQUALS", "build"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_config_vars_file() {
        let args =
            Args::try_parse_from(["ratect", "--config-vars-file", "vars.yml", "build"]).unwrap();
        assert_eq!(args.config_vars_file, Some(PathBuf::from("vars.yml")));
    }

    #[test]
    fn defaults_config_var_flags_to_empty() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(args.config_var.is_empty());
        assert_eq!(args.config_vars_file, None);
    }
}
