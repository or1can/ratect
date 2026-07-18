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

use anyhow::Result;
use clap::Parser;
use ratect_core::config::{format_task_list, format_task_list_quiet, Config};
use ratect_core::docker::DockerClient;
use ratect_core::engine::TaskEngine;
use ratect_core::ui::simple::SimpleEventLogger;
use ratect_core::ui::{select_output_style, EventSink, NullEventSink, OutputStyle};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

    /// Existing Docker network to use for all tasks. If not set, a new
    /// network is created (and removed) for each task.
    #[arg(long = "use-network")]
    use_network: Option<String>,

    /// Disable binding of ports on the host, regardless of any `ports`
    /// configured on a container.
    #[arg(long = "disable-ports")]
    disable_ports: bool,

    /// Don't propagate proxy-related environment variables such as
    /// http_proxy and no_proxy to image builds or containers.
    #[arg(long = "no-proxy-vars")]
    no_proxy_vars: bool,

    /// Force a particular style of output (does not affect task command
    /// output): simple (plain lines, no updating text), quiet (only error
    /// messages, and a machine-readable --list-tasks format), fancy or all
    /// (not implemented yet). Defaults to fancy when the console supports
    /// it, simple otherwise.
    #[arg(short = 'o', long = "output", value_enum)]
    output: Option<OutputStyleArg>,

    /// Disable colored output from Ratect. Does not affect task command
    /// output. Also makes simple (not fancy) the default output style.
    #[arg(long = "no-color")]
    no_color: bool,

    /// Name of the task to run
    task_name: Option<String>,

    /// Additional arguments to pass to the task command
    #[arg(last = true)]
    additional_args: Vec<String>,
}

/// The CLI-side `--output` value set — clap's `ValueEnum` derive gives the
/// lowercase names (`fancy`/`simple`/`quiet`/`all`) and the standard
/// invalid-value error listing them, matching Batect's own enum-converted
/// option. Mirrors [`ratect_core::ui::OutputStyle`] rather than deriving on
/// it directly, keeping `clap` a `ratect`-only dependency (see AGENTS.md's
/// CLI-vs-core dependency split).
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum OutputStyleArg {
    Fancy,
    Simple,
    Quiet,
    All,
}

impl From<OutputStyleArg> for OutputStyle {
    fn from(arg: OutputStyleArg) -> Self {
        match arg {
            OutputStyleArg::Fancy => OutputStyle::Fancy,
            OutputStyleArg::Simple => OutputStyle::Simple,
            OutputStyleArg::Quiet => OutputStyle::Quiet,
            OutputStyleArg::All => OutputStyle::All,
        }
    }
}

/// Parses a `--config-var` value of the form `NAME=VALUE`.
fn parse_config_var(s: &str) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, value)) => Ok((name.to_string(), value.to_string())),
        None => Err(format!("expected NAME=VALUE, got '{s}'")),
    }
}

/// The directory `config_file`'s relative expressions/paths (`build_directory`,
/// volume host paths, `batect.project_directory`) are resolved against.
///
/// `Path::parent()` returns `Some("")` for a bare filename with no directory
/// prefix (e.g. the default `batect.yml`) rather than `None` — that's not a
/// "no parent" case in the `unwrap_or` sense, so the common bare `-f
/// batect.yml` invocation resolves to `""`, not `"."`. Both are handled the
/// same way downstream (`Config::resolve_expressions` joins onto the current
/// directory and lexically cleans the result), but it's worth being explicit
/// here since it's easy to assume `parent()` returning `None` is the only
/// case that needs a fallback.
fn base_path_for(config_file: &Path) -> &Path {
    config_file.parent().unwrap_or(Path::new("."))
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() {
    init_tracing();

    let exit_code = match run().await {
        Ok(()) => 0,
        Err(err) => {
            // Use `{:?}` (not `{}`) so the full anyhow context chain is logged,
            // matching what the default Termination handler would have printed.
            tracing::error!("{:?}", err);

            // If the task's own command exited non-zero, propagate that exact
            // code as ratect's own exit code (matching `docker run`'s
            // convention) rather than a generic failure code, so scripts can
            // inspect what actually happened.
            match err.downcast_ref::<ratect_core::docker::ContainerExitedNonZero>() {
                Some(failure) => failure.exit_code as u8,
                None => 1,
            }
        }
    };

    // `std::process::exit` (not returning `ExitCode` from `main`) is
    // deliberate: an interactive run leaves a `tokio::io::stdin()`-backed
    // blocking read task abandoned once its session ends (the stdin pump in
    // `DockerClient::run_container_interactively` is `.abort()`ed, but that
    // only stops polling it — the underlying OS thread stays blocked in a
    // real `read()` syscall until stdin next produces data or EOF, which a
    // real terminal's stdin never does on its own). Returning `ExitCode`
    // normally would drop the `tokio::main`-managed runtime first, which
    // waits for exactly that lingering task — hanging the whole process
    // indefinitely after every interactive session. `process::exit` skips
    // that wait entirely; everything that needed to run on a clean exit
    // (the raw-mode guard restoring the terminal, container/network cleanup)
    // has already completed via ordinary `Drop`/`?`-propagation well before
    // `run().await` returns here.
    std::process::exit(exit_code.into());
}

async fn run() -> Result<()> {
    let args = Args::parse();

    if !args.config_file.exists() {
        anyhow::bail!("Configuration file {:?} not found.", args.config_file);
    }
    let mut loaded = Config::load_from_file(&args.config_file).await?;

    let mut config_var_overrides: HashMap<String, String> = match &args.config_vars_file {
        Some(path) => Config::load_config_vars_file(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(args.config_var.iter().cloned());
    let base_path = base_path_for(&args.config_file);
    loaded.resolve_expressions(base_path, &config_var_overrides)?;
    let config = loaded.config;

    let term = std::env::var("TERM").ok();
    let output_style = select_output_style(
        args.output.map(OutputStyle::from),
        args.no_color,
        std::io::stdout().is_terminal(),
        term.as_deref(),
        ratect_core::ui::console_dimensions_available(),
    );

    if args.list_tasks {
        let listing = match output_style {
            OutputStyle::Quiet => format_task_list_quiet(&config.tasks),
            _ => format_task_list(&config.project_name, &config.tasks),
        };
        println!("{listing}");
        return Ok(());
    }

    match args.task_name {
        Some(task_name) => {
            // The output-mode logger — one instance shared by the Docker
            // client (fine-grained pull/build progress) and the engine
            // (lifecycle milestones), so it sees the whole event stream in
            // order.
            let event_sink: Arc<dyn EventSink> = match output_style {
                OutputStyle::Simple => Arc::new(SimpleEventLogger::stdout(args.no_color)),
                // Batect's quiet logger renders only task-failure events;
                // Ratect reports failures via the error chain on stderr
                // regardless of output style, so quiet's remaining job —
                // suppressing every milestone — is exactly the null sink.
                OutputStyle::Quiet => Arc::new(NullEventSink),
                // Auto-selected fancy (interactive console, no explicit
                // --output): fall back to simple until the fancy logger
                // lands (rest of the 0.16.0 output-modes work).
                OutputStyle::Fancy if args.output.is_none() => {
                    Arc::new(SimpleEventLogger::stdout(args.no_color))
                }
                OutputStyle::Fancy | OutputStyle::All => {
                    let name = match output_style {
                        OutputStyle::Fancy => "fancy",
                        _ => "all",
                    };
                    anyhow::bail!(
                        "--output {name} is not implemented yet (it arrives later in \
                         0.16.0) — 'simple' and 'quiet' are available today."
                    );
                }
            };
            let docker = DockerClient::new()?.with_event_sink(Arc::clone(&event_sink));
            let mut engine = TaskEngine::new(config, docker).with_event_sink(event_sink);
            if let Some(network) = args.use_network {
                engine = engine.with_existing_network(network);
            }
            if args.disable_ports {
                engine = engine.without_port_publishing();
            }
            if args.no_proxy_vars {
                engine = engine.without_proxy_environment_variables();
            }
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

    #[test]
    fn parses_use_network_flag() {
        let args =
            Args::try_parse_from(["ratect", "--use-network", "my-network", "build"]).unwrap();
        assert_eq!(args.use_network, Some("my-network".to_string()));
    }

    #[test]
    fn defaults_use_network_to_none() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.use_network, None);
    }

    #[test]
    fn parses_disable_ports_flag() {
        let args = Args::try_parse_from(["ratect", "--disable-ports", "build"]).unwrap();
        assert!(args.disable_ports);
    }

    #[test]
    fn defaults_disable_ports_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.disable_ports);
    }

    #[test]
    fn parses_no_proxy_vars_flag() {
        let args = Args::try_parse_from(["ratect", "--no-proxy-vars", "build"]).unwrap();
        assert!(args.no_proxy_vars);
    }

    #[test]
    fn defaults_no_proxy_vars_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.no_proxy_vars);
    }

    #[test]
    fn parses_output_style_long_and_short_forms() {
        let args = Args::try_parse_from(["ratect", "--output", "quiet", "build"]).unwrap();
        assert_eq!(args.output, Some(OutputStyleArg::Quiet));
        let args = Args::try_parse_from(["ratect", "-o", "simple", "build"]).unwrap();
        assert_eq!(args.output, Some(OutputStyleArg::Simple));
        let args = Args::try_parse_from(["ratect", "-o", "fancy", "build"]).unwrap();
        assert_eq!(args.output, Some(OutputStyleArg::Fancy));
        let args = Args::try_parse_from(["ratect", "-o", "all", "build"]).unwrap();
        assert_eq!(args.output, Some(OutputStyleArg::All));
    }

    #[test]
    fn defaults_output_style_to_unset_meaning_auto_select() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.output, None);
    }

    #[test]
    fn rejects_an_unknown_output_style_naming_the_valid_ones() {
        let error = Args::try_parse_from(["ratect", "-o", "verbose", "build"])
            .unwrap_err()
            .to_string();
        for name in ["fancy", "simple", "quiet", "all"] {
            assert!(error.contains(name), "error should list '{name}': {error}");
        }
    }

    #[test]
    fn parses_no_color_flag_and_defaults_it_off() {
        let args = Args::try_parse_from(["ratect", "--no-color", "build"]).unwrap();
        assert!(args.no_color);
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.no_color);
    }

    #[test]
    fn fancy_with_no_color_parses_cleanly() {
        // Deliberately *not* a parse error, unlike Batect (whose console
        // couples color and cursor movement — Ratect's doesn't, so
        // colorless fancy is supportable). See docs/differences-from-batect.md.
        let args = Args::try_parse_from(["ratect", "-o", "fancy", "--no-color", "build"]).unwrap();
        assert_eq!(args.output, Some(OutputStyleArg::Fancy));
        assert!(args.no_color);
    }
}
