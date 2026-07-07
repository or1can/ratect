mod config;
mod docker;
mod engine;

use crate::config::Config;
use crate::docker::DockerClient;
use crate::engine::TaskEngine;
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
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
async fn main() -> Result<()> {
    init_tracing();

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
