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
use ratect_core::docker::{ContainerRuntime, DockerClient, DockerConnectionOptions};
use ratect_core::engine::{TaskEngine, TaskEngineSettings};
use ratect_core::ui::{create_event_sink, select_output_style, OutputStyle};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
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

/// Options every subcommand genuinely uses: which file identifies the
/// project (`caches` needs it for the project *directory* even though it
/// never reads its contents), and what Ratect's own output looks like.
///
/// Everything narrower is attached to the subcommands that actually use it
/// — [`DockerArgs`] to the ones that reach a daemon, [`ConfigVarArgs`] to
/// the ones that read configuration. A flag accepted but ignored is worse
/// than one that isn't offered: it reads as a promise.
#[derive(ClapArgs, Debug)]
struct GlobalArgs {
    /// Path to the configuration file.
    #[arg(short = 'f', long, default_value = "batect.yml", global = true)]
    config_file: PathBuf,

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

/// Values for the configuration's own `config_variables` — for the
/// subcommands that read configuration at all.
///
/// `Default` is "none supplied", which is what `resources` uses: it reads
/// the configuration only for the project's name, and a project name that
/// depended on a config variable would be a strange thing to have.
#[derive(ClapArgs, Debug, Default)]
struct ConfigVarArgs {
    /// Set a config variable's value, as NAME=VALUE (repeatable). Takes
    /// precedence over --config-vars-file and the variable's own default.
    #[arg(long = "config-var", value_parser = parse_key_value)]
    config_var: Vec<(String, String)>,

    /// Path to a YAML file of config variable NAME: VALUE pairs.
    #[arg(long = "config-vars-file")]
    config_vars_file: Option<PathBuf>,
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

    /// Inspect and remove this project's caches.
    Caches {
        #[command(subcommand)]
        command: CachesCommand,
    },

    /// Inspect and remove containers and networks left over from previous
    /// runs.
    Resources {
        #[command(subcommand)]
        command: ResourcesCommand,
    },

    /// Check this project and this machine for problems, without running
    /// anything.
    Doctor(DoctorArgs),
}

#[derive(ClapArgs, Debug)]
struct DoctorArgs {
    #[command(flatten)]
    config_vars: ConfigVarArgs,

    #[command(flatten)]
    docker: DockerArgs,
}

#[derive(Subcommand, Debug)]
enum ResourcesCommand {
    /// List containers and networks left over from previous runs.
    List(ResourcesArgs),

    /// Remove containers and networks left over from previous runs.
    ///
    /// `resources list` with the same options is the dry run: it selects
    /// exactly what this removes.
    Clean(ResourcesArgs),
}

/// Which leftovers to act on.
///
/// Like `caches`, never reads the configuration file — a leftover belongs
/// to whatever created it, not to whatever the config says now, and the
/// times you most want this are when a run went wrong.
#[derive(ClapArgs, Debug)]
struct ResourcesArgs {
    /// Include every project's leftovers, not just this one's. The
    /// machine-wide sweep, for when a project directory isn't where you're
    /// looking from.
    #[arg(long = "all-projects")]
    all_projects: bool,

    /// Only leftovers older than this, as a duration ("30m", "2h", "7d").
    /// A task running right now looks exactly like a leftover — it *is*
    /// one, until it finishes — so this is how a sweep avoids tearing down
    /// a colleague's (or your own) in-flight run.
    #[arg(long = "older-than", value_parser = parse_age)]
    older_than: Option<std::time::Duration>,

    #[command(flatten)]
    docker: DockerArgs,
}

#[derive(Subcommand, Debug)]
enum TasksCommand {
    /// List the tasks this project defines.
    List(TasksListArgs),
}

#[derive(ClapArgs, Debug)]
struct TasksListArgs {
    // A task's own description can interpolate a config variable, so
    // listing them is a configuration read like any other.
    #[command(flatten)]
    config_vars: ConfigVarArgs,
}

#[derive(Subcommand, Debug)]
enum CachesCommand {
    /// List this project's existing caches.
    List(CachesArgs),

    /// Remove this project's caches, or just the named ones.
    Clean(CleanCachesArgs),
}

/// Which caches to act on: the storage they live in, and how to reach the
/// daemon holding them. Never reads the configuration file — a cache
/// belongs to the *project directory*, so these work on a project whose
/// config doesn't parse, or isn't there at all, which is exactly when
/// clearing a cache is most likely to be what's needed.
#[derive(ClapArgs, Debug)]
struct CachesArgs {
    /// Storage to look in: volume (Docker named volumes) or directory (host
    /// directories under <project>/.batect/caches/<name>).
    #[arg(long = "cache-type", value_enum, default_value = "volume")]
    cache_type: CacheTypeArg,

    #[command(flatten)]
    docker: DockerArgs,
}

#[derive(ClapArgs, Debug)]
struct CleanCachesArgs {
    /// The caches to remove, by name. Removes every one of this project's
    /// caches when none are named.
    names: Vec<String>,

    #[command(flatten)]
    caches: CachesArgs,
}

#[derive(ClapArgs, Debug)]
struct RunArgs {
    /// The name of the task to run.
    task: String,

    #[command(flatten)]
    config_vars: ConfigVarArgs,

    #[command(flatten)]
    docker: DockerArgs,

    /// Use BuildKit for image builds, regardless of the daemon's own
    /// advertised default or DOCKER_BUILDKIT (which this takes precedence
    /// over). Forcing the classic builder is DOCKER_BUILDKIT=0's job.
    #[arg(long = "enable-buildkit")]
    enable_buildkit: bool,

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

/// How to reach the Docker daemon — connection only, deliberately nothing
/// about what to *do* once connected (`--enable-buildkit` is `run`'s own,
/// since it's about building images, not reaching a daemon). Its own struct,
/// flattened into every subcommand that connects, so each picks up the
/// identical surface rather than growing a second, subtly different copy.
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
/// `--older-than`, as a plain `<number><unit>` (`90s`, `30m`, `2h`, `7d`).
///
/// Deliberately not [`ratect_core::config::parse_duration`], Batect's
/// Go-style format: that one exists to match Batect's `health_check`
/// durations exactly, and has no day unit — which is the one anybody
/// actually reaches for when clearing up after last week.
fn parse_age(value: &str) -> std::result::Result<std::time::Duration, String> {
    let invalid = || format!("expected a duration like 30m, 2h or 7d, got '{value}'");
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(invalid)?;
    let (number, unit) = value.split_at(split);
    let number: u64 = number.parse().map_err(|_| invalid())?;
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(invalid()),
    };
    Ok(std::time::Duration::from_secs(number * seconds))
}

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

    // Gathered once and shared between the output-format decisions and
    // (inside `create_event_sink`) the logger itself, rather than each
    // querying stdout/TERM/console dimensions separately.
    let terminal = TerminalFacts::gather();
    let requested_style = global.output.map(OutputStyle::from);
    let style = select_output_style(
        requested_style,
        global.no_color,
        terminal.stdout_is_terminal,
        terminal.term.as_deref(),
        terminal.console_dimensions_available,
    );

    match command {
        Command::Tasks {
            command: TasksCommand::List(args),
        } => {
            let project = load(&global, &args.config_vars).await?;
            let listing = match style {
                OutputStyle::Quiet => format_task_list_quiet(&project.config.tasks),
                _ => format_task_list(&project.config.project_name, &project.config.tasks),
            };
            println!("{listing}");
            Ok(())
        }
        Command::Run(args) => {
            let project = load(&global, &args.config_vars).await?;
            run_task(project, args, global.no_color, requested_style, terminal).await
        }
        // Deliberately no `load` call: see `CachesArgs`.
        Command::Caches { command } => manage_caches(command, &global, style).await,
        Command::Resources { command } => manage_resources(command, &global, style).await,
        Command::Doctor(args) => diagnose(args, &global, style).await,
    }
}

/// Loads the configuration — merging `--config-vars-file` with any
/// `--config-var`s, which override it.
async fn load(
    global: &GlobalArgs,
    config_vars: &ConfigVarArgs,
) -> Result<ratect_core::config::LoadedProject> {
    let mut config_var_overrides: HashMap<String, String> = match &config_vars.config_vars_file {
        Some(path) => Config::load_config_vars_file(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(config_vars.config_var.iter().cloned());
    load_project(&global.config_file, &config_var_overrides).await
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

    // Built before the connection options are consumed below.
    let settings = args.engine_settings(project.project_directory);
    let docker = DockerClient::new(&args.docker.into())?
        .with_event_sink(Arc::clone(&event_sink))
        .with_enable_buildkit(args.enable_buildkit);

    let engine = TaskEngine::new(project.config, docker)
        .with_event_sink(event_sink)
        .with_settings(settings)?;
    engine.run_task(&args.task, &args.args).await
}

impl RunArgs {
    /// Maps `run`'s own flags onto the engine's settings.
    ///
    /// Split out from [`run_task`] so it can be tested without a Docker
    /// daemon. A *missing* field is a compile error — this literal is
    /// exhaustive, with no `..Default::default()` — so what the tests are
    /// actually for is the mistakes the compiler can't see: a field wired
    /// to the wrong flag, a dropped or inverted negation (`publish_ports:
    /// self.disable_ports` type checks perfectly and reverses the flag),
    /// and a flag that's declared but never read here at all. Keep the
    /// literal exhaustive for that reason: adding `..Default::default()`
    /// would trade the compiler's check for a silent default.
    /// `ratect-compat` has the same function for the same reasons.
    fn engine_settings(&self, project_directory: PathBuf) -> TaskEngineSettings {
        let mut image_tags: HashMap<String, HashSet<String>> = HashMap::new();
        for (container, tag) in &self.tag_image {
            image_tags
                .entry(container.clone())
                .or_default()
                .insert(tag.clone());
        }
        TaskEngineSettings {
            existing_network: self.use_network.clone(),
            publish_ports: !self.disable_ports,
            propagate_proxy_environment_variables: !self.no_proxy_vars,
            run_prerequisites: !self.skip_prerequisites,
            image_overrides: self.override_image.iter().cloned().collect(),
            image_tags,
            cleanup_after_success: !(self.no_cleanup || self.no_cleanup_after_success),
            cleanup_after_failure: !(self.no_cleanup || self.no_cleanup_after_failure),
            max_parallelism: self.max_parallelism.map(|max| max as usize),
            cache: Some((self.cache_type.into(), project_directory)),
            // Stamped onto every resource this run creates, so it can be
            // identified later — see `ratect_core::labels`.
            ratect_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }
}

/// `ratect caches list` / `ratect caches clean [NAME...]` — this project's
/// own caches, in whichever storage `--cache-type` names.
///
/// Two deliberate differences from `ratect-compat`'s `--clean`/
/// `--clean-cache`, which this replaces:
///
/// - `list` exists at all. Neither Batect nor `ratect-compat` can tell you
///   what's there, which makes removing one *by name* a guessing game
///   against the config file.
/// - `clean` with names and `clean` with none are the same verb, separated
///   by whether anything was named — rather than `--clean` meaning
///   "everything" and `--clean-cache <name>` silently overriding it when
///   both are given, which is the shape Batect's flags forced.
async fn manage_caches(
    command: CachesCommand,
    global: &GlobalArgs,
    style: OutputStyle,
) -> Result<()> {
    let (args, names) = match command {
        CachesCommand::List(args) => (args, None),
        CachesCommand::Clean(clean) => (clean.caches, Some(clean.names)),
    };
    let base_path = ratect_core::config::base_path_for(&global.config_file);
    let project_directory = ratect_core::config::project_directory_path(base_path)?;
    let cache_type: ratect_core::cache::CacheType = args.cache_type.into();
    let quiet = style == OutputStyle::Quiet;

    let Some(names) = names else {
        let existing = match cache_type {
            ratect_core::cache::CacheType::Volume => {
                let docker = DockerClient::new(&args.docker.into())?;
                let key = ratect_core::cache::project_cache_key(&project_directory)?;
                ratect_core::cache::list_volume_caches(&docker, &key).await?
            }
            ratect_core::cache::CacheType::Directory => {
                ratect_core::cache::list_directory_caches(&project_directory)?
            }
        };
        // Quiet is the machine-readable form, same contract as `tasks list`:
        // bare names, one per line, nothing else on stdout.
        if quiet {
            for name in existing {
                println!("{name}");
            }
        } else if existing.is_empty() {
            println!("This project has no caches.");
        } else {
            println!("Caches for this project:");
            for name in existing {
                println!("- {name}");
            }
        }
        return Ok(());
    };

    let only: HashSet<String> = names.into_iter().collect();
    // Reported by *cache* name whichever storage was used — a volume's own
    // Docker name carries the `batect-cache-<key>-` prefix, which is an
    // implementation detail of where it's kept, not what the user called it.
    let removed: Vec<String> = match cache_type {
        ratect_core::cache::CacheType::Volume => {
            let docker = DockerClient::new(&args.docker.into())?;
            let key = ratect_core::cache::project_cache_key(&project_directory)?;
            let prefix = ratect_core::cache::cache_volume_name(&key, "");
            ratect_core::cache::clean_volume_caches(&docker, &key, &only)
                .await?
                .into_iter()
                .map(|volume| {
                    volume
                        .strip_prefix(&prefix)
                        .unwrap_or(volume.as_str())
                        .to_string()
                })
                .collect()
        }
        ratect_core::cache::CacheType::Directory => {
            ratect_core::cache::clean_directory_caches(&project_directory, &only)?
        }
    };

    if !quiet {
        for name in &removed {
            println!("Removed cache '{name}'.");
        }
        println!("Removed {} cache(s).", removed.len());
    }

    // A name that matched nothing is worth saying out loud: the likeliest
    // cause is a typo, and silence there reads exactly like success.
    for name in only.iter().filter(|name| !removed.contains(name)) {
        tracing::warn!("No cache named '{name}' exists for this project.");
    }

    Ok(())
}

/// `ratect resources list` / `ratect resources clean` — the containers and
/// networks previous runs left behind, found by the labels Ratect stamps on
/// everything it creates (see [`ratect_core::labels`]).
///
/// Leftovers happen after a crash, a `docker kill`, a run that used
/// `--no-cleanup`, or a cleanup that itself failed. Before the labels
/// existed, answering "what should I remove?" meant reading `docker ps -a`
/// and guessing, because nothing Ratect created was identifiable
/// afterwards.
///
/// The one thing labels can't settle: a task running *right now* carries
/// exactly the same labels as a leftover, because until it finishes it is
/// one. `list` reports ages so that's visible, and `--older-than` is how a
/// sweep avoids tearing down an in-flight run. Claiming to detect liveness
/// would be a lie — the daemon can't say whether some other `ratect`
/// process still cares about a container.
///
/// There's deliberately no `--dry-run`: `list` and `clean` take the same
/// [`ResourcesArgs`] and select through this same function, so `list` with
/// the same options *is* the dry run. A flag would be a second spelling of
/// an existing command, and a second thing to keep in step with it.
async fn manage_resources(
    command: ResourcesCommand,
    global: &GlobalArgs,
    style: OutputStyle,
) -> Result<()> {
    let (args, removing) = match command {
        ResourcesCommand::List(args) => (args, false),
        ResourcesCommand::Clean(args) => (args, true),
    };
    let quiet = style == OutputStyle::Quiet;

    // Scoped to this project unless asked otherwise — the project name
    // comes from the configuration, which is the one thing `resources`
    // needs it for, so `--all-projects` also covers the case where the
    // config can't be read at all.
    //
    // `--all-projects` still filters on *having* the project label, never
    // on nothing: an unfiltered listing is every container on the machine,
    // which for `clean` would mean stopping and removing other tools' work.
    // "Every project" means every project Ratect created.
    let project = if args.all_projects {
        None
    } else {
        Some(
            load(global, &ConfigVarArgs::default())
                .await?
                .config
                .project_name,
        )
    };
    let filters = [(ratect_core::labels::PROJECT, project.as_deref())];

    let docker = DockerClient::new(&args.docker.into())?;
    let mut found = docker.list_containers(&filters).await?;
    found.extend(docker.list_networks(&filters).await?);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default();
    let leftovers: Vec<Leftover> = found
        .into_iter()
        // Belt and braces over the daemon-side filter above. Everything
        // here is a removal candidate, and the cost of a wrong one is
        // someone else's container: nothing without Ratect's own project
        // label is ever a leftover of ours, however the listing was
        // filtered.
        .filter(|resource| resource.labels.contains_key(ratect_core::labels::PROJECT))
        .map(|resource| Leftover::new(resource, now))
        .filter(|leftover| match args.older_than {
            Some(older_than) => leftover.age_seconds >= older_than.as_secs() as i64,
            None => true,
        })
        .collect();

    if leftovers.is_empty() {
        if !quiet {
            println!(
                "{}",
                match args.older_than {
                    Some(_) => "Nothing left over that old.",
                    None => "Nothing left over.",
                }
            );
        }
        return Ok(());
    }

    if removing {
        remove_leftovers(&docker, &leftovers, quiet).await
    } else {
        report_leftovers(&leftovers, quiet);
        Ok(())
    }
}

/// One leftover, with the labels already pulled out of the map — the
/// reporting below reads them several times each, and a resource missing
/// one (not Ratect's, or from a version that didn't set it) should read as
/// unknown rather than panic.
struct Leftover {
    resource: ratect_core::docker::LabelledResource,
    task: String,
    run: String,
    age_seconds: i64,
    is_network: bool,
}

impl Leftover {
    fn new(resource: ratect_core::docker::LabelledResource, now: i64) -> Self {
        let label = |key: &str| {
            resource
                .labels
                .get(key)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        };
        Self {
            task: label(ratect_core::labels::TASK),
            run: label(ratect_core::labels::RUN),
            age_seconds: resource.created.map(|created| now - created).unwrap_or(0),
            // Only a container has a state; see `LabelledResource`.
            is_network: resource.state.is_none(),
            resource,
        }
    }

    /// What this is, in the terms the configuration uses — a container's
    /// own Docker name is random words, which is no use for recognizing it.
    fn describe(&self) -> String {
        if self.is_network {
            return format!("network {}", self.resource.name);
        }
        let container = self
            .resource
            .labels
            .get(ratect_core::labels::CONTAINER)
            .cloned()
            .unwrap_or_else(|| self.resource.name.clone());
        match self.resource.state.as_deref() {
            Some(state) => format!("container {container} ({state})"),
            None => format!("container {container}"),
        }
    }
}

/// Rounded to one unit — "3 days" is what makes a leftover recognizable as
/// old, and no decision here is improved by knowing it was 3 days and 4
/// hours.
fn format_age(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let (count, unit) = match seconds {
        s if s >= DAY => (s / DAY, "day"),
        s if s >= HOUR => (s / HOUR, "hour"),
        s if s >= MINUTE => (s / MINUTE, "minute"),
        s => (s.max(0), "second"),
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// Grouped by run, because that's the unit a leftover actually belongs to:
/// one interrupted task leaves a network and every container it started,
/// and they're only meaningful together.
fn report_leftovers(leftovers: &[Leftover], quiet: bool) {
    if quiet {
        // Machine-readable, same contract as `tasks list`/`caches list`:
        // one id per line and nothing else, ready to pipe into `docker rm`.
        for leftover in leftovers {
            println!("{}", leftover.resource.id);
        }
        return;
    }

    let mut runs: Vec<&str> = leftovers.iter().map(|l| l.run.as_str()).collect();
    runs.sort_unstable();
    runs.dedup();

    println!(
        "{} left over from {} previous run{}:",
        leftovers.len(),
        runs.len(),
        if runs.len() == 1 { "" } else { "s" }
    );
    for run in runs {
        let group: Vec<&Leftover> = leftovers.iter().filter(|l| l.run == run).collect();
        let task = &group[0].task;
        let age = format_age(group.iter().map(|l| l.age_seconds).max().unwrap_or(0));
        println!("\n  {task} ({age} ago, run {run}):");
        for leftover in group {
            println!("    - {}", leftover.describe());
        }
    }
    println!("\nRemove them with: ratect resources clean");
}

async fn remove_leftovers(
    docker: &DockerClient,
    leftovers: &[Leftover],
    quiet: bool,
) -> Result<()> {
    // Containers first: a network still holding an endpoint can't be
    // removed, so the reverse order fails on every task that had one.
    let (networks, containers): (Vec<&Leftover>, Vec<&Leftover>) =
        leftovers.iter().partition(|leftover| leftover.is_network);

    let mut removed = 0;
    for leftover in containers.iter().chain(networks.iter()) {
        let result = if leftover.is_network {
            docker.remove_network(&leftover.resource.id).await
        } else {
            docker
                .stop_and_remove_container(&leftover.resource.id)
                .await
        };
        match result {
            Ok(()) => {
                removed += 1;
                if !quiet {
                    println!("Removed {}.", leftover.describe());
                }
            }
            // One failure doesn't abandon the rest: a resource someone else
            // removed in the meantime, or one still in use, shouldn't leave
            // the remaining leftovers behind too.
            Err(error) => tracing::warn!("Failed to remove {}: {error:#}", leftover.describe()),
        }
    }

    if !quiet {
        println!("Removed {removed} of {}.", leftovers.len());
    }
    Ok(())
}

/// One thing `doctor` looked at.
#[derive(Debug, PartialEq, Eq)]
enum Finding {
    /// Checked, nothing wrong.
    Fine(String),
    /// Works, but is likely to bite — a reproducibility hazard, or a
    /// readiness gate that isn't really gating anything.
    Warning(String),
    /// Will fail a run, or already has.
    Problem(String),
}

impl Finding {
    fn render(&self) -> String {
        match self {
            Finding::Fine(message) => format!("  ok      {message}"),
            Finding::Warning(message) => format!("  warning {message}"),
            Finding::Problem(message) => format!("  problem {message}"),
        }
    }
}

/// `ratect doctor` — what's wrong with this project, or this machine,
/// without running a task to find out.
///
/// Exits non-zero if anything is a [`Finding::Problem`], so it's usable as
/// a CI step; warnings never affect the exit code, since a warning is a
/// judgement about likely trouble rather than a fact about breakage.
///
/// Deliberately does the environment checks even when the configuration
/// itself won't load: "your config is broken *and* your Docker daemon
/// isn't running" is more useful than being told one and having to fix it
/// to discover the other.
async fn diagnose(args: DoctorArgs, global: &GlobalArgs, style: OutputStyle) -> Result<()> {
    let mut findings = Vec::new();

    // Docker first: nothing else about a task can work without it, so it's
    // the most likely single answer to "why did that fail?".
    let docker = DockerClient::new(&args.docker.into());
    let docker = match docker {
        Ok(docker) => match docker.server_version().await {
            Ok(version) => {
                findings.push(Finding::Fine(format!(
                    "Docker daemon reachable ({version})"
                )));
                Some(docker)
            }
            Err(error) => {
                findings.push(Finding::Problem(format!(
                    "Docker daemon not reachable: {error:#}"
                )));
                None
            }
        },
        Err(error) => {
            findings.push(Finding::Problem(format!(
                "Docker connection options are unusable: {error:#}"
            )));
            None
        }
    };

    match load(global, &args.config_vars).await {
        Ok(project) => {
            findings.push(Finding::Fine(format!(
                "{} loads ({} container(s), {} task(s))",
                global.config_file.display(),
                project.config.containers.len(),
                project.config.tasks.len()
            )));
            findings.extend(config_findings(&project.config));

            // Leftovers are worth reporting unasked — the whole reason
            // `resources` exists is that nobody thinks to look.
            if let Some(docker) = &docker {
                let filters = [(
                    ratect_core::labels::PROJECT,
                    Some(project.config.project_name.as_str()),
                )];
                let mut left = docker.list_containers(&filters).await.unwrap_or_default();
                left.extend(docker.list_networks(&filters).await.unwrap_or_default());
                if left.is_empty() {
                    findings.push(Finding::Fine("no leftovers from previous runs".to_string()));
                } else {
                    findings.push(Finding::Warning(format!(
                        "{} resource(s) left over from previous runs — see `ratect resources list`",
                        left.len()
                    )));
                }
            }
        }
        Err(error) => findings.push(Finding::Problem(format!(
            "{} does not load: {error:#}",
            global.config_file.display()
        ))),
    }

    let problems = findings
        .iter()
        .filter(|finding| matches!(finding, Finding::Problem(_)))
        .count();
    let warnings = findings
        .iter()
        .filter(|finding| matches!(finding, Finding::Warning(_)))
        .count();

    if style == OutputStyle::Quiet {
        // Quiet is "only what needs acting on", the same contract it has
        // everywhere else.
        for finding in findings
            .iter()
            .filter(|finding| !matches!(finding, Finding::Fine(_)))
        {
            println!("{}", finding.render().trim_start());
        }
    } else {
        println!("Checking {}...", global.config_file.display());
        for finding in &findings {
            println!("{}", finding.render());
        }
        println!();
        println!(
            "{} check(s): {problems} problem(s), {warnings} warning(s).",
            findings.len()
        );
    }

    if problems > 0 {
        anyhow::bail!("{problems} problem(s) found.");
    }
    Ok(())
}

/// The checks that need only the configuration — pure, so they're testable
/// without a daemon or a project on disk.
fn config_findings(config: &ratect_core::config::Config) -> Vec<Finding> {
    let mut findings = Vec::new();

    // A floating tag defeats the entire point of pinning a task's
    // environment: the same config gives a different image next week.
    let mut floating: Vec<&str> = config
        .containers
        .iter()
        .filter(|(_, container)| container.image.as_deref().is_some_and(floating_image_tag))
        .map(|(name, _)| name.as_str())
        .collect();
    floating.sort_unstable();
    for name in floating {
        findings.push(Finding::Warning(format!(
            "container '{name}' uses a floating image tag — pin it, or the same \
             configuration will run a different image later"
        )));
    }

    // A dependency with no health check counts as ready the moment it
    // starts, which is where "connection refused" on the first run comes
    // from. Ratect can't see whether the *image* defines one, so this is
    // phrased as something to check rather than something wrong.
    let mut unguarded: Vec<&str> = dependency_names(config)
        .into_iter()
        .filter(|name| {
            config
                .containers
                .get(*name)
                .is_some_and(|container| container.health_check.is_none())
        })
        .collect();
    unguarded.sort_unstable();
    for name in unguarded {
        findings.push(Finding::Warning(format!(
            "dependency '{name}' has no health_check — unless its image defines one, \
             it counts as ready the moment it starts"
        )));
    }

    // Already resolved to an absolute path by `load_project`, so this is
    // the path Ratect will actually hand to Docker.
    let mut missing: Vec<String> = Vec::new();
    for (name, container) in &config.containers {
        let Some(directory) = &container.build_directory else {
            continue;
        };
        let directory = Path::new(directory);
        if !directory.is_dir() {
            missing.push(format!(
                "container '{name}' has build_directory '{}', which doesn't exist",
                directory.display()
            ));
            continue;
        }
        let dockerfile = directory.join(container.dockerfile.as_deref().unwrap_or("Dockerfile"));
        if !dockerfile.is_file() {
            missing.push(format!(
                "container '{name}' has no '{}' in its build_directory",
                dockerfile.display()
            ));
        }
    }
    missing.sort();
    findings.extend(missing.into_iter().map(Finding::Problem));

    findings
}

/// `image` with no tag at all, or an explicitly floating one. Docker treats
/// a missing tag as `latest`, so both are the same hazard.
fn floating_image_tag(image: &str) -> bool {
    // A colon before the last slash is a registry port, not a tag —
    // `registry:5000/app` is untagged.
    let tag = match image.rsplit_once('/') {
        Some((_, last)) => last.rsplit_once(':').map(|(_, tag)| tag),
        None => image.rsplit_once(':').map(|(_, tag)| tag),
    };
    match tag {
        None => true,
        Some(tag) => tag == "latest",
    }
}

/// Every container named as a dependency, by another container or by a
/// task — the ones whose readiness actually gates something.
fn dependency_names(config: &ratect_core::config::Config) -> Vec<&str> {
    let mut names: Vec<&str> = config
        .containers
        .values()
        .filter_map(|container| container.dependencies.as_ref())
        .chain(
            config
                .tasks
                .values()
                .filter_map(|task| task.dependencies.as_ref()),
        )
        .flatten()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
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
    use std::time::Duration;

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
                command: TasksCommand::List(_)
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
        let config_var = match cli.command {
            Command::Run(args) => args.config_vars.config_var,
            other => panic!("expected a run command, got {other:?}"),
        };
        assert_eq!(
            config_var,
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

    #[test]
    fn caches_clean_removes_everything_when_no_names_are_given() {
        let cli = Cli::try_parse_from(["ratect", "caches", "clean"]).unwrap();
        match cli.command {
            Command::Caches {
                command: CachesCommand::Clean(args),
            } => assert!(args.names.is_empty()),
            other => panic!("expected a caches clean command, got {other:?}"),
        }
    }

    #[test]
    fn caches_clean_takes_the_names_to_remove_as_positional_arguments() {
        let cli = Cli::try_parse_from(["ratect", "caches", "clean", "npm-cache", "gradle-cache"])
            .unwrap();
        match cli.command {
            Command::Caches {
                command: CachesCommand::Clean(args),
            } => assert_eq!(args.names, vec!["npm-cache", "gradle-cache"]),
            other => panic!("expected a caches clean command, got {other:?}"),
        }
    }

    /// Which storage to act on has to be askable of both sub-verbs, or
    /// `list` and `clean` would disagree about what a cache even is.
    #[test]
    fn cache_type_applies_to_both_caches_subcommands() {
        for arguments in [
            vec!["ratect", "caches", "list", "--cache-type", "directory"],
            vec!["ratect", "caches", "clean", "--cache-type", "directory"],
        ] {
            let cli = Cli::try_parse_from(&arguments).unwrap();
            let cache_type = match cli.command {
                Command::Caches {
                    command: CachesCommand::List(args),
                } => args.cache_type,
                Command::Caches {
                    command: CachesCommand::Clean(args),
                } => args.caches.cache_type,
                other => panic!("expected a caches command, got {other:?}"),
            };
            assert_eq!(cache_type, CacheTypeArg::Directory);
        }
    }

    fn run_args(arguments: &[&str]) -> RunArgs {
        match Cli::try_parse_from(arguments)
            .expect("should parse")
            .command
        {
            Command::Run(args) => args,
            other => panic!("expected a run command, got {other:?}"),
        }
    }

    fn settings_from(flags: &[&str]) -> TaskEngineSettings {
        let mut arguments = vec!["ratect", "run", "build"];
        arguments.extend_from_slice(flags);
        run_args(&arguments).engine_settings(PathBuf::from("/p"))
    }

    /// One flag (with any value it needs) against the single setting it is
    /// supposed to move. `--no-cleanup` is deliberately absent: it moves
    /// two, and has its own test.
    const FLAG_TO_SETTING: &[(&[&str], &str)] = &[
        (&["--use-network", "existing-network"], "existing_network"),
        (&["--disable-ports"], "publish_ports"),
        (
            &["--no-proxy-vars"],
            "propagate_proxy_environment_variables",
        ),
        (&["--skip-prerequisites"], "run_prerequisites"),
        (&["--override-image", "db=postgres:16"], "image_overrides"),
        (&["--tag-image", "app=extra"], "image_tags"),
        (&["--no-cleanup-after-success"], "cleanup_after_success"),
        (&["--no-cleanup-after-failure"], "cleanup_after_failure"),
        (&["--max-parallelism", "3"], "max_parallelism"),
    ];

    /// Which settings differ from the engine's own defaults — the basis of
    /// the per-flag test below. `cache`/`ratect_version` are excluded: both
    /// are always supplied, so they always differ.
    fn changed_from_default(settings: &TaskEngineSettings) -> Vec<&'static str> {
        let defaults = TaskEngineSettings::default();
        let mut changed = Vec::new();
        if settings.existing_network != defaults.existing_network {
            changed.push("existing_network");
        }
        if settings.publish_ports != defaults.publish_ports {
            changed.push("publish_ports");
        }
        if settings.propagate_proxy_environment_variables
            != defaults.propagate_proxy_environment_variables
        {
            changed.push("propagate_proxy_environment_variables");
        }
        if settings.run_prerequisites != defaults.run_prerequisites {
            changed.push("run_prerequisites");
        }
        if settings.image_overrides != defaults.image_overrides {
            changed.push("image_overrides");
        }
        if settings.image_tags != defaults.image_tags {
            changed.push("image_tags");
        }
        if settings.cleanup_after_success != defaults.cleanup_after_success {
            changed.push("cleanup_after_success");
        }
        if settings.cleanup_after_failure != defaults.cleanup_after_failure {
            changed.push("cleanup_after_failure");
        }
        if settings.max_parallelism != defaults.max_parallelism {
            changed.push("max_parallelism");
        }
        changed
    }

    /// Each flag on its own must move its own setting and nothing else.
    ///
    /// This is the test that catches *cross-wiring*: with several flags set
    /// at once, a field reading the wrong one of two same-shaped flags
    /// looks identical to one reading the right flag. Setting a single flag
    /// and asserting the exact set of changed fields is what tells them
    /// apart — an all-at-once test can't.
    #[test]
    fn each_flag_changes_only_its_own_setting() {
        for (flag, expected) in FLAG_TO_SETTING {
            assert_eq!(
                changed_from_default(&settings_from(flag)),
                vec![*expected],
                "{flag:?} should change exactly `{expected}`"
            );
        }
    }

    /// With nothing asked for, the engine must be left exactly as it would
    /// be with no settings applied at all — an inverted boolean here would
    /// silently change the default behavior of every run.
    #[test]
    fn no_flags_maps_to_the_engines_own_defaults() {
        let settings = settings_from(&[]);
        assert!(
            changed_from_default(&settings).is_empty(),
            "no flag should mean no setting moved: {:?}",
            changed_from_default(&settings)
        );
        // The two this binary always supplies, unlike the rest.
        assert_eq!(
            settings.cache,
            Some((ratect_core::cache::CacheType::Volume, PathBuf::from("/p")))
        );
        assert_eq!(
            settings.ratect_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// Values, not just which field moved — the per-flag test above proves
    /// a flag reaches the right setting, this proves what it puts there.
    #[test]
    fn a_flags_value_reaches_its_setting_intact() {
        let settings = settings_from(&[
            "--use-network",
            "existing-network",
            "--override-image",
            "db=postgres:16",
            "--tag-image",
            "app=extra",
            "--tag-image",
            "app=second",
            "--max-parallelism",
            "3",
            "--cache-type",
            "directory",
        ]);

        assert_eq!(
            settings.existing_network.as_deref(),
            Some("existing-network")
        );
        assert_eq!(
            settings.image_overrides,
            HashMap::from([("db".to_string(), "postgres:16".to_string())])
        );
        assert_eq!(
            settings.image_tags,
            HashMap::from([(
                "app".to_string(),
                HashSet::from(["extra".to_string(), "second".to_string()])
            )]),
            "a container named more than once collects every tag"
        );
        assert_eq!(settings.max_parallelism, Some(3));
        assert_eq!(
            settings.cache,
            Some((
                ratect_core::cache::CacheType::Directory,
                PathBuf::from("/p")
            ))
        );
    }

    /// `--no-cleanup` is the pair of them together; each half also stands
    /// alone, and confusing the two would leave containers behind (or not)
    /// in exactly the case the user asked about.
    #[test]
    fn no_cleanup_is_both_halves_and_each_half_stands_alone() {
        assert_eq!(
            changed_from_default(&settings_from(&["--no-cleanup"])),
            vec!["cleanup_after_success", "cleanup_after_failure"]
        );
    }

    #[test]
    fn resources_has_a_list_and_a_clean_verb() {
        assert!(matches!(
            Cli::try_parse_from(["ratect", "resources", "list"])
                .unwrap()
                .command,
            Command::Resources {
                command: ResourcesCommand::List(_)
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["ratect", "resources", "clean"])
                .unwrap()
                .command,
            Command::Resources {
                command: ResourcesCommand::Clean(_)
            }
        ));
    }

    /// Both scoping options apply to both verbs — listing everything and
    /// then only being able to clean this project's would be a trap.
    #[test]
    fn scope_options_apply_to_both_resources_verbs() {
        for verb in ["list", "clean"] {
            let cli = Cli::try_parse_from([
                "ratect",
                "resources",
                verb,
                "--all-projects",
                "--older-than",
                "2h",
            ])
            .unwrap();
            let args = match cli.command {
                Command::Resources {
                    command: ResourcesCommand::List(args) | ResourcesCommand::Clean(args),
                } => args,
                other => panic!("expected a resources command, got {other:?}"),
            };
            assert!(args.all_projects);
            assert_eq!(args.older_than, Some(Duration::from_secs(2 * 60 * 60)));
        }
    }

    #[test]
    fn an_age_accepts_seconds_minutes_hours_and_days() {
        assert_eq!(parse_age("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_age("30m"), Ok(Duration::from_secs(1_800)));
        assert_eq!(parse_age("2h"), Ok(Duration::from_secs(7_200)));
        // The unit anyone reaches for when clearing up after last week,
        // and the reason this isn't Batect's own duration format.
        assert_eq!(parse_age("7d"), Ok(Duration::from_secs(604_800)));
    }

    #[test]
    fn an_age_without_a_valid_unit_is_rejected() {
        for value in ["30", "30x", "d", "", "-1h", "1.5h"] {
            assert!(parse_age(value).is_err(), "{value} should be rejected");
        }
    }

    /// Rounded to one unit: "3 days" is what makes a leftover recognizable
    /// as old, and singular/plural is the kind of thing that reads as
    /// sloppy in the one place someone is already annoyed.
    #[test]
    fn an_age_reads_as_a_single_rounded_unit() {
        assert_eq!(format_age(1), "1 second");
        assert_eq!(format_age(59), "59 seconds");
        assert_eq!(format_age(60), "1 minute");
        assert_eq!(format_age(60 * 90), "1 hour");
        assert_eq!(format_age(60 * 60 * 25), "1 day");
        assert_eq!(format_age(60 * 60 * 24 * 3), "3 days");
        // A clock skew between the daemon and here shouldn't print
        // something absurd.
        assert_eq!(format_age(-5), "0 seconds");
    }

    fn resource(
        id: &str,
        name: &str,
        labels: &[(&str, &str)],
        state: Option<&str>,
    ) -> ratect_core::docker::LabelledResource {
        ratect_core::docker::LabelledResource {
            id: id.to_string(),
            name: name.to_string(),
            labels: labels
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            created: Some(1_000),
            state: state.map(str::to_string),
        }
    }

    /// A container is described by its *configured* name, not Docker's
    /// randomly generated one, which is the whole reason the label exists.
    #[test]
    fn a_leftover_is_described_in_the_terms_the_config_uses() {
        let container = Leftover::new(
            resource(
                "abc",
                "nostalgic_hopper",
                &[
                    (ratect_core::labels::CONTAINER, "database"),
                    (ratect_core::labels::TASK, "check"),
                ],
                Some("exited"),
            ),
            2_000,
        );
        assert_eq!(container.describe(), "container database (exited)");
        assert_eq!(container.task, "check");
        assert_eq!(container.age_seconds, 1_000);
        assert!(!container.is_network);

        let network = Leftover::new(resource("def", "ratect-xyz", &[], None), 2_000);
        assert_eq!(network.describe(), "network ratect-xyz");
        assert!(network.is_network);
    }

    /// A resource from a Ratect old enough not to have set every label
    /// should still be listable — reporting is exactly when you don't want
    /// a panic.
    #[test]
    fn a_leftover_missing_labels_reads_as_unknown_rather_than_failing() {
        let leftover = Leftover::new(resource("abc", "some_name", &[], Some("running")), 2_000);
        assert_eq!(leftover.task, "unknown");
        assert_eq!(leftover.run, "unknown");
        // Falls back to Docker's own name when there's no container label.
        assert_eq!(leftover.describe(), "container some_name (running)");
    }

    #[test]
    fn doctor_is_its_own_verb_and_reaches_a_daemon() {
        assert!(matches!(
            Cli::try_parse_from(["ratect", "doctor"]).unwrap().command,
            Command::Doctor(_)
        ));
        // It checks the daemon, so it takes the options for reaching one.
        assert!(
            Cli::try_parse_from(["ratect", "doctor", "--docker-host", "tcp://example:2376"])
                .is_ok()
        );
    }

    /// Docker treats a missing tag as `latest`, so both are the same
    /// reproducibility hazard — and a registry port is a colon that isn't
    /// a tag, which is the case that makes this worth a function.
    #[test]
    fn a_floating_image_tag_is_latest_or_no_tag_at_all() {
        assert!(floating_image_tag("alpine"));
        assert!(floating_image_tag("alpine:latest"));
        assert!(floating_image_tag("registry.example.com/team/app"));
        assert!(floating_image_tag("registry.example.com:5000/team/app"));

        assert!(!floating_image_tag("alpine:3.18.2"));
        assert!(!floating_image_tag(
            "registry.example.com:5000/team/app:1.2.3"
        ));
        assert!(!floating_image_tag(
            "alpine@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    /// Builds a `Config` the way a real invocation does — through
    /// `load_project` on an actual file — rather than by parsing YAML
    /// here, which would need `noyalib` as a dependency of this binary and
    /// duplicate knowledge that belongs to `ratect-core`. It also means
    /// `build_directory` paths are resolved exactly as they will be at run
    /// time, which one of these checks depends on.
    async fn config_with(yaml: &str) -> ratect_core::config::Config {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "ratect-doctor-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            count
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("batect.yml");
        std::fs::write(&path, yaml).unwrap();

        let project = load_project(&path, &HashMap::new())
            .await
            .expect("fixture config should load");
        std::fs::remove_dir_all(&directory).unwrap();
        project.config
    }

    #[tokio::test]
    async fn doctor_warns_about_floating_tags_and_unguarded_dependencies() {
        let config = config_with(
            r#"
project_name: demo
containers:
  database:
    image: postgres
  cache:
    image: redis:7-alpine
  app:
    image: alpine:3.18.2
    dependencies:
      - database
      - cache
tasks:
  test:
    run:
      container: app
      command: echo hi
"#,
        )
        .await;

        let findings = config_findings(&config);
        let messages: Vec<String> = findings
            .iter()
            .map(|finding| finding.render().trim().to_string())
            .collect();

        assert!(
            messages
                .iter()
                .any(|m| m.contains("'database'") && m.contains("floating image tag")),
            "an untagged image is a floating tag: {messages:?}"
        );
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("'cache'") && m.contains("floating")),
            "a pinned tag shouldn't be warned about: {messages:?}"
        );
        // Both dependencies lack a health check; the task's own container
        // isn't a dependency and so isn't gating anything.
        assert!(messages
            .iter()
            .any(|m| m.contains("'cache'") && m.contains("health_check")));
        assert!(messages
            .iter()
            .any(|m| m.contains("'database'") && m.contains("health_check")));
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("'app'") && m.contains("health_check")),
            "the task's own container gates nothing: {messages:?}"
        );
        assert!(
            findings.iter().all(|f| !matches!(f, Finding::Problem(_))),
            "none of this stops a run: {messages:?}"
        );
    }

    /// A build directory that isn't there fails the run, so it's a problem
    /// rather than a warning — and `doctor` exits non-zero on those, which
    /// is what makes it usable as a CI step.
    #[tokio::test]
    async fn a_missing_build_directory_is_a_problem() {
        let config = config_with(
            r#"
project_name: demo
containers:
  app:
    build_directory: /nonexistent/build/context
tasks:
  test:
    run:
      container: app
      command: echo hi
"#,
        )
        .await;

        let findings = config_findings(&config);
        assert!(
            findings.iter().any(|finding| matches!(
                finding,
                Finding::Problem(message) if message.contains("build_directory") && message.contains("doesn't exist")
            )),
            "{findings:?}"
        );
    }

    /// A container named only by a *task*'s `dependencies` gates that task
    /// just as much as a container-level one.
    #[tokio::test]
    async fn a_task_level_dependency_counts_as_a_dependency() {
        let config = config_with(
            r#"
project_name: demo
containers:
  queue:
    image: redis:7-alpine
  app:
    image: alpine:3.18.2
tasks:
  test:
    run:
      container: app
      command: echo hi
    dependencies:
      - queue
"#,
        )
        .await;

        assert_eq!(dependency_names(&config), vec!["queue"]);
    }

    /// `caches` locates a project by directory, never by reading its
    /// configuration, so config-variable values would have nothing to act
    /// on — and offering them would imply otherwise.
    #[test]
    fn config_variable_options_belong_to_the_commands_that_read_configuration() {
        assert!(Cli::try_parse_from(["ratect", "run", "build", "--config-var", "a=1"]).is_ok());
        assert!(Cli::try_parse_from(["ratect", "tasks", "list", "--config-var", "a=1"]).is_ok());
        assert!(Cli::try_parse_from(["ratect", "caches", "list", "--config-var", "a=1"]).is_err());
    }

    /// Caches live in Docker volumes by default, so these do reach a
    /// daemon — but they never build anything, so they don't take the flag
    /// that's about building.
    #[test]
    fn caches_takes_the_connection_options_but_not_enable_buildkit() {
        assert!(Cli::try_parse_from([
            "ratect",
            "caches",
            "list",
            "--docker-host",
            "tcp://example:2376"
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["ratect", "caches", "list", "--enable-buildkit"]).is_err());
        assert!(Cli::try_parse_from(["ratect", "run", "build", "--enable-buildkit"]).is_ok());
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
