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

//! The forward-looking Ratect CLI — subcommands (`ratect run <task>`,
//! `ratect tasks list`) rather than `ratect-compat`'s flat, Batect-shaped
//! surface, and free to diverge from Batect entirely.
//!
//! 0.2.0 is deliberately the subcommand surface *only*: it runs on
//! `ratect-core`'s existing engine and today's YAML configuration, both
//! completely unchanged (the `ratect`-native config format is 0.3.0's own
//! scope — see ROADMAP.md). Nothing here parses configuration or talks to
//! Docker itself; it maps arguments onto `ratect_core::config::load_project`,
//! `TaskEngineSettings` and `ui::create_event_sink`, all of which
//! `ratect-compat` already proved.

use anyhow::Result;
use clap::{Args as ClapArgs, Parser, Subcommand};
use ratect_core::config::{format_task_list, format_task_list_quiet, load_project, Config};
use ratect_core::docker::{DockerClient, DockerConnectionOptions};
use ratect_core::engine::{TaskEngine, TaskEngineSettings};
use ratect_core::ui::{create_event_sink, select_output_style, OutputStyle};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

/// Options every subcommand needs — loading the configuration, and deciding
/// what Ratect's own output looks like. Deliberately *not* the Docker
/// connection options ([`DockerArgs`]): `tasks list` never connects to a
/// daemon, and a flag that's accepted but does nothing is worse than one
/// that isn't offered.
#[derive(ClapArgs, Debug)]
struct GlobalArgs {
    /// Path to the configuration file.
    #[arg(short = 'f', long, default_value = "batect.yml", global = true)]
    config_file: PathBuf,

    /// Set a config variable's value, as NAME=VALUE (repeatable). Takes
    /// precedence over --config-vars-file and the variable's own default.
    #[arg(long = "config-var", value_parser = parse_key_value, global = true)]
    config_var: Vec<(String, String)>,

    /// Path to a YAML file of config variable NAME: VALUE pairs.
    #[arg(long = "config-vars-file", global = true)]
    config_vars_file: Option<PathBuf>,

    /// Force a particular style of Ratect's own output (never affects a
    /// task command's output): fancy (a live per-container status display,
    /// the default when the console supports it), simple (plain lines),
    /// all (interleaved output from every container), or quiet (error
    /// messages only, and a machine-readable task list).
    #[arg(short = 'o', long = "output", value_enum, global = true)]
    output: Option<OutputStyleArg>,

    /// Disable colored output from Ratect. Never affects a task command's
    /// output. Also makes simple, not fancy, the default output style.
    #[arg(long = "no-color", global = true)]
    no_color: bool,
}

// `Run` carries every `run` option and `Tasks` carries a bare sub-verb, so
// the variants are wildly different sizes — irrelevant for a type built
// exactly once per process, from `Cli::parse`, and immediately destructured.
// Boxing the payload isn't an option anyway: `clap`'s `Subcommand` derive
// needs the variant's own field to implement `Args`, which `Box<RunArgs>`
// doesn't.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
enum Command {
    /// Run a task.
    Run(RunArgs),

    /// Inspect the tasks this project defines.
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
}

#[derive(Subcommand, Debug)]
enum TasksCommand {
    /// List the tasks this project defines.
    List,
}

#[derive(ClapArgs, Debug)]
struct RunArgs {
    /// The name of the task to run.
    task: String,

    #[command(flatten)]
    docker: DockerArgs,

    /// Existing Docker network to use, instead of creating (and removing)
    /// one for the task.
    #[arg(long = "use-network")]
    use_network: Option<String>,

    /// Don't bind any container ports on the host, regardless of what the
    /// configuration asks for.
    #[arg(long = "disable-ports")]
    disable_ports: bool,

    /// Don't propagate proxy-related environment variables (http_proxy,
    /// no_proxy and friends) into containers or image builds.
    #[arg(long = "no-proxy-vars")]
    no_proxy_vars: bool,

    /// Don't run the task's prerequisites.
    #[arg(long = "skip-prerequisites")]
    skip_prerequisites: bool,

    /// Override the image a container uses, as CONTAINER=IMAGE
    /// (repeatable). The container's own image/build_directory and
    /// image_pull_policy are ignored entirely.
    #[arg(long = "override-image", value_parser = parse_key_value)]
    override_image: Vec<(String, String)>,

    /// Tag the image a container builds, as CONTAINER=TAG (repeatable; name
    /// a container more than once for multiple tags). Only valid for a
    /// container that actually builds an image.
    #[arg(long = "tag-image", value_parser = parse_key_value)]
    tag_image: Vec<(String, String)>,

    /// Leave every container this task created running, whatever happens,
    /// so the state can be investigated. Equivalent to both
    /// --no-cleanup-after-success and --no-cleanup-after-failure.
    #[arg(long = "no-cleanup")]
    no_cleanup: bool,

    /// Leave containers running if the task's own container runs to
    /// completion, whatever its exit code.
    #[arg(long = "no-cleanup-after-success")]
    no_cleanup_after_success: bool,

    /// Leave containers running if something fails before the task's own
    /// container can start.
    #[arg(long = "no-cleanup-after-failure")]
    no_cleanup_after_failure: bool,

    /// Maximum number of image pulls/builds to run in parallel. Unset means
    /// unbounded.
    #[arg(long = "max-parallelism", value_parser = clap::value_parser!(u32).range(1..))]
    max_parallelism: Option<u32>,

    /// Storage for `cache` volume mounts: volume (a Docker named volume) or
    /// directory (a host directory under <project>/.batect/caches/<name>).
    #[arg(long = "cache-type", value_enum, default_value = "volume")]
    cache_type: CacheTypeArg,

    /// Arguments to pass to the task's own command, after `--`.
    #[arg(last = true)]
    args: Vec<String>,
}

/// How to reach the Docker daemon. Its own struct, flattened into the
/// subcommands that actually connect to one, so a later verb that needs a
/// daemon (`ratect doctor`, cache management) picks the identical surface up
/// rather than growing a second, subtly different copy.
#[derive(ClapArgs, Debug)]
struct DockerArgs {
    /// Docker host to use, e.g. 'unix:///var/run/docker.sock' or
    /// 'tcp://1.2.3.4:5678'. Defaults to DOCKER_HOST, then Docker's own
    /// local default. Cannot be combined with --docker-context.
    #[arg(long = "docker-host")]
    host: Option<String>,

    /// Docker CLI context to use. Defaults to DOCKER_CONTEXT, then the
    /// Docker CLI's own active context. Cannot be combined with
    /// --docker-host.
    #[arg(long = "docker-context")]
    context: Option<String>,

    /// Directory containing the Docker CLI's configuration (context store,
    /// config.json). Defaults to DOCKER_CONFIG, then ~/.docker.
    #[arg(long = "docker-config")]
    config_directory: Option<PathBuf>,

    /// Use TLS when connecting to the Docker host. Identical to
    /// --docker-tls-verify: Ratect always verifies the daemon's
    /// certificate, and offers no way to skip that.
    #[arg(long = "docker-tls")]
    tls: bool,

    /// Use TLS when connecting to the Docker host, verifying its
    /// certificate. Defaults to DOCKER_TLS_VERIFY.
    #[arg(long = "docker-tls-verify")]
    tls_verify: bool,

    /// Directory containing ca.pem/cert.pem/key.pem, unless overridden
    /// individually below. Defaults to DOCKER_CERT_PATH, then ~/.docker.
    #[arg(long = "docker-cert-path")]
    cert_path: Option<PathBuf>,

    /// TLS CA certificate verifying the Docker host's own certificate.
    /// Defaults to ca.pem in --docker-cert-path.
    #[arg(long = "docker-tls-ca-cert")]
    tls_ca_cert: Option<PathBuf>,

    /// TLS certificate authenticating to the Docker host. Defaults to
    /// cert.pem in --docker-cert-path.
    #[arg(long = "docker-tls-cert")]
    tls_cert: Option<PathBuf>,

    /// TLS key authenticating to the Docker host. Defaults to key.pem in
    /// --docker-cert-path.
    #[arg(long = "docker-tls-key")]
    tls_key: Option<PathBuf>,

    /// Use BuildKit for image builds, regardless of the daemon's own
    /// advertised default or DOCKER_BUILDKIT (which this takes precedence
    /// over). Forcing the classic builder is DOCKER_BUILDKIT=0's job.
    #[arg(long = "enable-buildkit")]
    enable_buildkit: bool,
}

impl From<DockerArgs> for DockerConnectionOptions {
    fn from(args: DockerArgs) -> Self {
        Self {
            host: args.host,
            context: args.context,
            config_directory: args.config_directory,
            tls: args.tls,
            tls_verify: args.tls_verify,
            cert_path: args.cert_path,
            tls_ca_cert: args.tls_ca_cert,
            tls_cert: args.tls_cert,
            tls_key: args.tls_key,
        }
    }
}

/// Mirrors [`ratect_core::ui::OutputStyle`] rather than deriving `ValueEnum`
/// on it directly, keeping `clap` out of `ratect-core` — see AGENTS.md's
/// CLI-vs-core dependency split. `ratect-compat` has its own copy for the
/// same reason; they're independent on purpose, since each binary's value
/// names are part of its own interface.
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

/// Mirrors [`ratect_core::cache::CacheType`], same reasoning as
/// [`OutputStyleArg`].
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum CacheTypeArg {
    Volume,
    Directory,
}

impl From<CacheTypeArg> for ratect_core::cache::CacheType {
    fn from(arg: CacheTypeArg) -> Self {
        match arg {
            CacheTypeArg::Volume => ratect_core::cache::CacheType::Volume,
            CacheTypeArg::Directory => ratect_core::cache::CacheType::Directory,
        }
    }
}

/// Parses a `NAME=VALUE` pair — `--config-var`, `--override-image` and
/// `--tag-image` all take one.
fn parse_key_value(value: &str) -> std::result::Result<(String, String), String> {
    match value.split_once('=') {
        Some((name, value)) => Ok((name.to_string(), value.to_string())),
        None => Err(format!("expected NAME=VALUE, got '{value}'")),
    }
}

/// Diagnostics go to stderr, filtered by `RUST_LOG` (default `info`) — the
/// same arrangement `ratect-compat` has, minus its Batect-compatible
/// `--log-file`, which nothing has asked this binary for yet.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing();

    let exit_code = match run(cli).await {
        Ok(()) => 0,
        Err(error) => {
            // Straight to stderr, never through `tracing::error!`, which
            // `RUST_LOG=off` (or any filter excluding this target) would
            // suppress entirely — leaving a non-zero exit with no visible
            // reason anywhere, in every output style including quiet. Same
            // reasoning, and the same `{:?}` full-context-chain formatting,
            // as `ratect-compat`'s own top-level handler.
            eprintln!("Error: {error:?}");
            match error.downcast_ref::<ratect_core::docker::ContainerExitedNonZero>() {
                Some(failure) => failure.exit_code as u8,
                None => 1,
            }
        }
    };

    // `std::process::exit`, not returning `ExitCode`: an interactive run
    // leaves a blocking stdin read abandoned, and dropping the runtime
    // normally would wait for it forever. See `ratect-compat`'s own `main`
    // for the full explanation — everything needing to run on a clean exit
    // already has by the time `run` returns.
    std::process::exit(exit_code.into());
}

async fn run(cli: Cli) -> Result<()> {
    let Cli { global, command } = cli;

    let mut config_var_overrides: HashMap<String, String> = match &global.config_vars_file {
        Some(path) => Config::load_config_vars_file(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(global.config_var.iter().cloned());
    let project = load_project(&global.config_file, &config_var_overrides).await?;

    // Gathered once and shared between the task-list format decision and
    // (inside `create_event_sink`) the logger itself, rather than each
    // querying stdout/TERM/console dimensions separately.
    let terminal = TerminalFacts::gather();
    let requested_style = global.output.map(OutputStyle::from);

    match command {
        Command::Tasks {
            command: TasksCommand::List,
        } => {
            let style = select_output_style(
                requested_style,
                global.no_color,
                terminal.stdout_is_terminal,
                terminal.term.as_deref(),
                terminal.console_dimensions_available,
            );
            let listing = match style {
                OutputStyle::Quiet => format_task_list_quiet(&project.config.tasks),
                _ => format_task_list(&project.config.project_name, &project.config.tasks),
            };
            println!("{listing}");
            Ok(())
        }
        Command::Run(args) => {
            run_task(project, args, global.no_color, requested_style, terminal).await
        }
    }
}

async fn run_task(
    project: ratect_core::config::LoadedProject,
    args: RunArgs,
    no_color: bool,
    requested_style: Option<OutputStyle>,
    terminal: TerminalFacts,
) -> Result<()> {
    // One logger, shared by the Docker client (pull/build progress) and the
    // engine (lifecycle milestones), so it sees the whole event stream in
    // order.
    let event_sink = create_event_sink(
        requested_style,
        no_color,
        terminal.stdout_is_terminal,
        terminal.term.as_deref(),
        terminal.console_dimensions_available,
    )?;

    let enable_buildkit = args.docker.enable_buildkit;
    let docker = DockerClient::new(&args.docker.into())?
        .with_event_sink(Arc::clone(&event_sink))
        .with_enable_buildkit(enable_buildkit);

    let mut image_tags: HashMap<String, HashSet<String>> = HashMap::new();
    for (container, tag) in args.tag_image {
        image_tags.entry(container).or_default().insert(tag);
    }
    let settings = TaskEngineSettings {
        existing_network: args.use_network,
        publish_ports: !args.disable_ports,
        propagate_proxy_environment_variables: !args.no_proxy_vars,
        run_prerequisites: !args.skip_prerequisites,
        image_overrides: args.override_image.into_iter().collect(),
        image_tags,
        cleanup_after_success: !(args.no_cleanup || args.no_cleanup_after_success),
        cleanup_after_failure: !(args.no_cleanup || args.no_cleanup_after_failure),
        max_parallelism: args.max_parallelism.map(|max| max as usize),
        cache: Some((args.cache_type.into(), project.project_directory)),
    };

    let engine = TaskEngine::new(project.config, docker)
        .with_event_sink(event_sink)
        .with_settings(settings)?;
    engine.run_task(&args.task, &args.args).await
}

/// The terminal facts every output decision is made from, read once per
/// invocation — `select_output_style` and `create_event_sink` both want
/// them, and querying twice risks answering differently.
struct TerminalFacts {
    term: Option<String>,
    stdout_is_terminal: bool,
    console_dimensions_available: bool,
}

impl TerminalFacts {
    fn gather() -> Self {
        Self {
            term: std::env::var("TERM").ok(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            console_dimensions_available: ratect_core::ui::console_dimensions_available(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_internally_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_takes_the_task_name_as_its_own_argument() {
        let cli = Cli::try_parse_from(["ratect", "run", "build"]).unwrap();
        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.task, "build");
                assert!(args.args.is_empty());
            }
            other => panic!("expected a run command, got {other:?}"),
        }
    }

    /// The deliberate absence of `ratect <task>` sugar: with more verbs
    /// coming, "is `doctor` a task or a subcommand?" is a question the
    /// interface should never have to answer — see ROADMAP.md.
    #[test]
    fn a_bare_task_name_is_not_accepted_as_a_shorthand_for_run() {
        assert!(Cli::try_parse_from(["ratect", "build"]).is_err());
    }

    #[test]
    fn arguments_after_a_double_dash_go_to_the_task_command() {
        let cli =
            Cli::try_parse_from(["ratect", "run", "build", "--", "--verbose", "extra"]).unwrap();
        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.task, "build");
                assert_eq!(args.args, vec!["--verbose", "extra"]);
            }
            other => panic!("expected a run command, got {other:?}"),
        }
    }

    #[test]
    fn tasks_list_is_its_own_subcommand_not_a_flag() {
        let cli = Cli::try_parse_from(["ratect", "tasks", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::List
            }
        ));
        // `--list-tasks` is `ratect-compat`'s spelling, and stays there.
        assert!(Cli::try_parse_from(["ratect", "--list-tasks"]).is_err());
    }

    /// Global options are accepted on either side of the subcommand — `-f`
    /// before `run` reads naturally, and after it is what anyone used to
    /// the flat CLI will type first.
    #[test]
    fn global_options_work_before_and_after_the_subcommand() {
        for arguments in [
            ["ratect", "-f", "custom.yml", "run", "build"],
            ["ratect", "run", "build", "-f", "custom.yml"],
        ] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            assert_eq!(cli.global.config_file, PathBuf::from("custom.yml"));
        }
    }

    #[test]
    fn a_repeatable_name_value_option_collects_every_occurrence() {
        let cli = Cli::try_parse_from([
            "ratect",
            "run",
            "build",
            "--config-var",
            "one=1",
            "--config-var",
            "two=2",
        ])
        .unwrap();
        assert_eq!(
            cli.global.config_var,
            vec![
                ("one".to_string(), "1".to_string()),
                ("two".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn a_name_value_option_without_an_equals_sign_is_rejected() {
        assert!(
            Cli::try_parse_from(["ratect", "run", "build", "--config-var", "no-equals"]).is_err()
        );
    }

    /// `tasks list` never reaches a daemon, so it doesn't take the flags
    /// for reaching one — an accepted-but-ignored flag is worse than one
    /// that isn't offered.
    #[test]
    fn docker_options_belong_to_run_not_to_tasks_list() {
        assert!(Cli::try_parse_from([
            "ratect",
            "run",
            "build",
            "--docker-host",
            "tcp://example:2376"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "ratect",
            "tasks",
            "list",
            "--docker-host",
            "tcp://example:2376"
        ])
        .is_err());
    }
}
