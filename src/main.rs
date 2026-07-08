use anyhow::Result;
use clap::Parser;
use ratect_core::config::Config;
use ratect_core::docker::DockerClient;
use ratect_core::engine::TaskEngine;
use std::path::PathBuf;
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

    /// Name of the task to run
    task_name: Option<String>,

    /// Additional arguments to pass to the task command
    #[arg(last = true)]
    additional_args: Vec<String>,
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

    let config = if args.config_file.exists() {
        Some(Config::load_from_file(&args.config_file)?)
    } else {
        None
    };

    if args.list_tasks {
        if let Some(config) = config {
            println!("Tasks in {}:", config.project_name);
            let mut tasks: Vec<_> = config.tasks.keys().collect();
            tasks.sort();
            for task in tasks {
                println!("- {}", task);
            }
        } else {
            tracing::error!("Configuration file {:?} not found.", args.config_file);
        }
        return Ok(());
    }

    match args.task_name {
        Some(task_name) => {
            if let Some(config) = config {
                let docker = DockerClient::new()?;
                let engine = TaskEngine::new(config, docker);
                engine.run_task(&task_name).await?;
            } else {
                tracing::error!("Configuration file {:?} not found.", args.config_file);
            }
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
}
