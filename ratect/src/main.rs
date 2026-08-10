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
//! From 0.3.0 this binary reads its own native config format (`ratect.toml`
//! by default) via `ratect_core::config::load_project_native` — TOML, with
//! `extends` and TOML/YAML includes chosen by extension (see
//! `decisions/0003`). A `batect.yml` is still readable by naming it with
//! `-f`, for migration. Nothing here parses configuration or talks to Docker
//! itself; it maps arguments onto that loader, `TaskEngineSettings` and
//! `ui::create_event_sink`, the engine and UI unchanged from what
//! `ratect-compat` already proved.

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, CommandFactory, Parser, Subcommand};
use ratect_core::config::{format_task_list, format_task_list_quiet, load_project_native, Config};
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
    /// Path to the configuration file. Defaults to `ratect.toml`, this
    /// binary's own native format; point it at a `batect.yml` to keep reading
    /// the Batect-format config while migrating (see `ratect config convert`).
    #[arg(
        short = 'f',
        long,
        default_value = DEFAULT_CONFIG_FILE,
        global = true,
        value_hint = clap::ValueHint::FilePath,
    )]
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

    /// Path to a file of config variable values (a flat NAME = VALUE map),
    /// parsed as TOML or YAML by its extension. Defaults to an auto-discovered
    /// `ratect.local.toml` beside the config file, when present.
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
// Variants are ordered as they appear in `--help` and the docs: the
// task-running verbs first, then the resource-management nouns, then
// configuration and diagnostics, and finally the shell-integration utility
// (`completions`) — grouped by purpose rather than alphabetically, so the
// flagship `run` stays first. `clap` can't render group *headings* for
// subcommands (only for args), so the docs ([`docs/ratect-cli.md`]) carry the
// labels; here the order alone conveys it.
enum Command {
    /// Run a task.
    Run(RunArgs),

    /// Inspect the tasks this project defines.
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },

    /// Inspect and remove caches — this project's, and the machine's shared ones.
    Caches {
        #[command(subcommand)]
        command: CachesCommand,
    },

    /// Inspect and manage the cache of Git includes shared by every project
    /// on this machine.
    Includes {
        #[command(subcommand)]
        command: IncludesCommand,
    },

    /// Inspect and remove containers and networks left over from previous
    /// runs.
    Resources {
        #[command(subcommand)]
        command: ResourcesCommand,
    },

    /// Work with this project's configuration file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Check this project and this machine for problems, without running
    /// anything.
    Doctor(DoctorArgs),

    /// Print a shell completion script to standard output. Reaches no daemon
    /// and reads no configuration.
    Completions(CompletionsArgs),
}

#[derive(ClapArgs, Debug)]
struct CompletionsArgs {
    /// The shell to generate a completion script for.
    #[arg(value_enum)]
    shell: clap_complete::Shell,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    /// Check the configuration loads and is free of problems, without a
    /// daemon — `doctor`'s config-only half, for use as a CI step.
    Validate(ConfigValidateArgs),

    /// Convert a Batect-format `batect.yml` into this binary's native
    /// `ratect.toml`. Point `-f` at the `batect.yml` to convert.
    ///
    /// The output is a reviewable starting point, not a blind drop-in:
    /// comments are lost, and any `include`d files are flattened into the one
    /// result (Git bundles included). It preserves behaviour, not formatting —
    /// the conversion is checked to round-trip losslessly before it's written.
    Convert(ConfigConvertArgs),
}

#[derive(ClapArgs, Debug)]
struct ConfigValidateArgs {
    #[command(flatten)]
    config_vars: ConfigVarArgs,
}

#[derive(ClapArgs, Debug)]
struct ConfigConvertArgs {
    /// Overwrite `ratect.toml` if it already exists.
    #[arg(long = "force")]
    force: bool,

    /// Write the converted configuration to standard output instead of a
    /// file, so it can be reviewed or piped before being saved.
    #[arg(long = "stdout", conflicts_with = "force")]
    stdout: bool,
}

#[derive(Subcommand, Debug)]
enum IncludesCommand {
    /// List what's in the Git include cache.
    List,

    /// Remove cached Git includes.
    Clean(CleanIncludesArgs),

    /// Re-clone every cached Git include, picking up any `ref` that has
    /// moved since it was first fetched.
    Refresh,
}

#[derive(ClapArgs, Debug)]
struct CleanIncludesArgs {
    /// Remove every cached include, not just the ones nothing has used
    /// recently. Nothing is lost that a re-clone can't restore.
    #[arg(long = "all", conflicts_with = "older_than")]
    all: bool,

    /// Remove includes unused for longer than this ("30m", "2h", "7d").
    /// Defaults to the same 30 days the automatic sweep uses.
    #[arg(long = "older-than", value_parser = parse_age)]
    older_than: Option<std::time::Duration>,
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
    // Listing tasks means loading and *resolving* the configuration, which
    // fails outright if a declared config variable has no value — so the
    // options that supply one belong here, even though nothing in the listing
    // itself interpolates. (A task's `description` is a plain string, not an
    // expression, matching Batect's own typing.)
    #[command(flatten)]
    config_vars: ConfigVarArgs,
}

#[derive(Subcommand, Debug)]
enum CachesCommand {
    /// List the caches this project can see: its own, and the machine's
    /// shared ones.
    List(CachesArgs),

    /// Remove this project's caches, or just the named ones — which may
    /// be shared, and so used by other projects.
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
    /// directories under `<project>/.batect/caches/<name>`).
    #[arg(long = "cache-type", value_enum, default_value = "volume")]
    cache_type: CacheTypeArg,

    /// Restrict to one scope. The readable listing shows both by default,
    /// `-o quiet` shows this project's only, and on `clean` this is how an
    /// ambiguous name is disambiguated.
    #[arg(long = "scope", value_enum)]
    scope: Option<CacheScopeArg>,

    #[command(flatten)]
    docker: DockerArgs,
}

/// `--scope`, this binary's own mirror of [`ratect_core::config::CacheScope`]
/// — the same duplication `CacheTypeArg`/`OutputStyleArg` exist for, keeping
/// `clap` out of `ratect-core` and the accepted spellings part of this
/// binary's interface rather than the library's.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum CacheScopeArg {
    /// This project's own caches.
    Project,
    /// Caches shared with every other project on this machine.
    Shared,
}

impl From<CacheScopeArg> for ratect_core::config::CacheScope {
    fn from(value: CacheScopeArg) -> Self {
        match value {
            CacheScopeArg::Project => Self::Project,
            CacheScopeArg::Shared => Self::Shared,
        }
    }
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
    #[arg(add = clap_complete::engine::ArgValueCompleter::new(complete_task_names))]
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
    /// directory (a host directory under `<project>/.batect/caches/<name>`).
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
    // Dynamic shell completion (prototype, `unstable-dynamic`). At `<TAB>` the
    // shell — driven by the script `ratect completions <shell>` installs — sets
    // `COMPLETE=<shell>` and re-invokes `ratect`; this handles that request and
    // exits *before* any normal work, so completion never parses a real
    // command, reaches a daemon, or does anything with a side effect. On a
    // normal invocation it's a cheap no-op and returns.
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

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
                // 128 + SIGINT — see `ratect-compat`'s own handler for why
                // an interrupt gets its own code rather than a generic 1.
                None if error.is::<ratect_core::interrupt::TaskInterrupted>() => 130,
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

    // Arms follow the `Command` enum's own order (see there).
    match command {
        Command::Run(args) => {
            let project = load(&global, &args.config_vars).await?;
            run_task(project, args, global.no_color, requested_style, terminal).await
        }
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
        // Deliberately no `load` call: see `CachesArgs`.
        Command::Caches { command } => manage_caches(command, &global, style).await,
        Command::Includes { command } => manage_includes(command, style).await,
        Command::Resources { command } => manage_resources(command, &global, style).await,
        Command::Config { command } => match command {
            ConfigCommand::Validate(args) => validate_config(args, &global, style).await,
            ConfigCommand::Convert(args) => convert_config(args, &global, style).await,
        },
        Command::Doctor(args) => diagnose(args, &global, style).await,
        Command::Completions(args) => generate_completions(args),
    }
}

/// `ratect completions <shell>` — writes a completion registration script for
/// `shell` to stdout, for the user to source into their shell (see
/// docs/ratect-cli.md for the per-shell install line).
///
/// This emits the **dynamic** registration script: at `<TAB>` the shell calls
/// back into `ratect` (handled by the `CompleteEnv` hook in `main`), which is
/// what lets it complete task names from the config, not just the static
/// command/flag surface. Generating the script itself reads nothing and reaches
/// nothing — it's a fixed bit of shell glue; the config is only read later, on
/// an actual completion request, and always side-effect-free.
fn generate_completions(args: CompletionsArgs) -> Result<()> {
    let shell = args.shell.to_string();
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
        .completer(&shell)
        .ok_or_else(|| anyhow::anyhow!("no completion support for shell '{shell}'"))?;
    completer
        .write_registration(
            "COMPLETE",
            "ratect",
            "ratect",
            "ratect",
            &mut std::io::stdout(),
        )
        .context("writing the completion registration script")
}

/// Completes a `run <task>` argument with the project's task names — the
/// dynamic completion invoked by the shell at `<TAB>` time via the engine wired
/// up in `main`. Reads the config through the side-effect-free
/// [`ratect_core::config::task_names_for_completion`] (which follows local and
/// already-cached includes but never clones, pulls, or reaches Docker), so a
/// `<TAB>` is instant and safe.
fn complete_task_names(current: &std::ffi::OsStr) -> Vec<clap_complete::CompletionCandidate> {
    let config_file = completion_config_file();
    let prefix = current.to_string_lossy();
    ratect_core::config::task_names_for_completion(&config_file)
        .into_iter()
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(clap_complete::CompletionCandidate::new)
        .collect()
}

/// The config file an in-progress completion should read: the value of an
/// explicit `-f`/`--config-file` on the line being completed, else the default
/// (`ratect.toml`, or `batect.yml` if that's what's present).
///
/// During a completion request the shell re-invokes us as `… ratect -- <the
/// words being completed>`, so those words are our own process args after `--`
/// — the only place a completer can see them, since clap hands its callback
/// just the word under the cursor, not the rest of the command line.
fn completion_config_file() -> PathBuf {
    let mut words = std::env::args_os()
        .skip_while(|arg| arg.to_string_lossy() != "--")
        .skip(1);
    while let Some(arg) = words.next() {
        let text = arg.to_string_lossy();
        if let Some(value) = text.strip_prefix("--config-file=") {
            return PathBuf::from(value);
        }
        if text == "-f" || text == "--config-file" {
            if let Some(value) = words.next() {
                return PathBuf::from(value);
            }
        }
    }
    let native = PathBuf::from(DEFAULT_CONFIG_FILE);
    if native.exists() {
        native
    } else {
        PathBuf::from(BATECT_CONFIG_FILE)
    }
}

/// Loads the configuration — merging `--config-vars-file` with any
/// `--config-var`s, which override it.
async fn load(
    global: &GlobalArgs,
    config_vars: &ConfigVarArgs,
) -> Result<ratect_core::config::LoadedProject> {
    // The config-vars file is either the one `--config-vars-file` names, or —
    // when it doesn't — an auto-discovered `ratect.local.toml` beside the
    // config file, loaded only if it exists (an absent one just means no file
    // overrides, not an error). This binary's native equivalent of
    // `ratect-compat`'s `batect.local.yml` default; see decisions/0003.
    let vars_file = config_vars.config_vars_file.clone().or_else(|| {
        let default =
            ratect_core::config::base_path_for(&global.config_file).join(LOCAL_CONFIG_VARS_FILE);
        default.exists().then_some(default)
    });
    let mut config_var_overrides: HashMap<String, String> = match &vars_file {
        Some(path) => Config::load_config_vars_file_native(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(config_vars.config_var.iter().cloned());
    load_project_native(&global.config_file, &config_var_overrides).await
}

/// The auto-discovered local config-variable overrides file — gitignored,
/// per-developer, config-variable values only. Native-named (`ratect-compat`
/// uses `batect.local.yml`); see decisions/0003.
const LOCAL_CONFIG_VARS_FILE: &str = "ratect.local.toml";

/// This binary's native config file, and `-f`'s default.
const DEFAULT_CONFIG_FILE: &str = "ratect.toml";

/// The Batect-format file `config convert` reads by default (its *source*),
/// since `-f`'s default is the `ratect.toml` it *writes*.
const BATECT_CONFIG_FILE: &str = "batect.yml";

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
    // Armed here rather than in `engine_settings`, which is synchronous —
    // see its own comment. From this point a Ctrl+C abandons the run and
    // cleans up instead of killing the process where it stands.
    if let Some(interrupt) = &settings.interrupt {
        interrupt.listen();
    }
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
            // Created here but *not* yet listening — see `ratect-compat`'s
            // own settings for both halves of the reasoning.
            interrupt: Some(ratect_core::interrupt::Interrupt::new()),
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

    let wanted: Option<ratect_core::config::CacheScope> = args.scope.map(Into::into);
    // One connection for the whole command: both the listing below and the
    // removal after it need the same daemon, and each `DockerClient::new`
    // opens its own.
    let docker = match cache_type {
        ratect_core::cache::CacheType::Volume => Some(DockerClient::new(&args.docker.into())?),
        ratect_core::cache::CacheType::Directory => None,
    };
    let found = find_caches(docker.as_ref(), cache_type, &project_directory, wanted).await?;

    let Some(names) = names else {
        if quiet {
            // Bare names, one per line — the documented contract, and by
            // construction exactly what a `clean` with these same flags
            // would act on, so it is always safe to pipe back.
            for name in found.actionable() {
                println!("{name}");
            }
        } else if found.is_empty() {
            // Named after what was searched, not after the project: with
            // `--scope shared` this project's caches were never looked at.
            match args.scope {
                Some(CacheScopeArg::Shared) => println!("There are no shared caches."),
                Some(CacheScopeArg::Project) => println!("This project has no caches."),
                None => println!("This project has no caches, and there are no shared ones."),
            }
        } else {
            if !found.owned.is_empty() {
                println!("Caches for this project:");
                for name in &found.owned {
                    println!("- {name}");
                }
            }
            // Never under "this project's": these are machine-wide, and most
            // will belong to other projects.
            if !found.shared.is_empty() {
                if !found.owned.is_empty() {
                    println!();
                }
                println!("Shared caches on this machine:");
                for name in &found.shared {
                    println!("- {name}");
                }
            }
        }
        return Ok(());
    };

    let only: HashSet<String> = names.into_iter().collect();

    // `--scope shared` with nothing named would silently do nothing, since
    // shared caches are only ever removed by name. Say so instead.
    if only.is_empty() && wanted == Some(ratect_core::config::CacheScope::Shared) {
        anyhow::bail!(
            "Name the shared caches to remove. A shared cache holds storage other \
             projects are still using, so it is never swept without being named — \
             'ratect caches list --scope shared' shows what is there."
        );
    }

    // A name in both scopes is refused rather than guessed at: removing the
    // shared one discards storage other projects are still using, and
    // removing the project one silently leaves the cache probably meant.
    if let Some(name) = found
        .ambiguous()
        .into_iter()
        .find(|name| only.contains(*name))
    {
        anyhow::bail!(
            "'{name}' names both a project cache and a shared one. Re-run with \
             '--scope project' or '--scope shared' to say which to remove."
        );
    }

    // Reported by *cache* name whichever storage was used — a volume's own
    // Docker name carries a prefix, which is an implementation detail of
    // where it's kept, not what the user called it.
    let mut removed: Vec<String> = Vec::new();
    if found.covers(ratect_core::config::CacheScope::Project) {
        removed.extend(match cache_type {
            ratect_core::cache::CacheType::Volume => {
                let docker = docker
                    .as_ref()
                    .expect("a volume cache needs a Docker client");
                let key = ratect_core::cache::project_cache_key(&project_directory)?;
                let prefix = ratect_core::cache::cache_volume_name(&key, "");
                ratect_core::cache::clean_volume_caches(docker, &key, &only)
                    .await?
                    .into_iter()
                    .map(|volume| {
                        volume
                            .strip_prefix(&prefix)
                            .unwrap_or(volume.as_str())
                            .to_string()
                    })
                    .collect::<Vec<_>>()
            }
            ratect_core::cache::CacheType::Directory => {
                ratect_core::cache::clean_directory_caches(&project_directory, &only)?
            }
        });
    }
    // Shared caches are only ever removed by name — a bare `caches clean`
    // sweeps this project's, and `only` is non-empty by construction here.
    if found.covers(ratect_core::config::CacheScope::Shared) {
        removed.extend(match cache_type {
            ratect_core::cache::CacheType::Volume => {
                let docker = docker
                    .as_ref()
                    .expect("a volume cache needs a Docker client");
                ratect_core::cache::clean_shared_volume_caches(docker, &only).await?
            }
            ratect_core::cache::CacheType::Directory => {
                ratect_core::cache::clean_shared_directory_caches(&only)?
            }
        });
    }

    if !quiet {
        for name in &removed {
            println!("Removed cache '{name}'.");
        }
        println!("Removed {} cache(s).", removed.len());
    }

    // A name that matched nothing is worth saying out loud: the likeliest
    // cause is a typo, and silence there reads exactly like success.
    for name in only.iter().filter(|name| !removed.contains(name)) {
        // Named after what was actually searched: under `--scope shared`
        // "for this project" points at the wrong storage entirely.
        match wanted {
            Some(ratect_core::config::CacheScope::Shared) => {
                tracing::warn!("No shared cache named '{name}' exists.")
            }
            Some(ratect_core::config::CacheScope::Project) => {
                tracing::warn!("No cache named '{name}' exists for this project.")
            }
            None => tracing::warn!(
                "No cache named '{name}' exists for this project, or as a shared cache."
            ),
        }
    }

    Ok(())
}

/// The caches one `ratect caches` invocation is working with: what this
/// project can see, split by whether the project *owns* them.
///
/// The split is the point. Before shared caches, `caches` rested on an
/// unstated invariant — everything it showed you belonged to this project,
/// so anything it showed you, you could delete. That is why the heading
/// could say "this project's", why an empty name set could mean "all of
/// them", and why `-o quiet` was safe to pipe into `clean`.
///
/// Shared caches invalidated it. Carrying scope as a bare tag alongside each
/// name meant every site re-derived what it implied — the heading, the quiet
/// filter, the ambiguity check, the removal gate — and each could be wrong
/// on its own. Every defect in this area was one of them being wrong
/// separately. This answers the question once instead.
struct CacheSelection {
    /// This project's own caches — what a bare `clean` sweeps.
    owned: Vec<String>,
    /// Caches shared with every project on the machine: visible from here,
    /// but not this project's, and removed only when named.
    shared: Vec<String>,
    /// The `--scope` this invocation was given, which narrows both.
    scope: Option<ratect_core::config::CacheScope>,
}

impl CacheSelection {
    fn is_empty(&self) -> bool {
        self.owned.is_empty() && self.shared.is_empty()
    }

    /// What `-o quiet` prints — and, by construction, exactly what a `clean`
    /// carrying the same flags would act on.
    ///
    /// Holding those two together is what makes the machine-readable listing
    /// safe to pipe straight back: everything it emits, the matching `clean`
    /// may remove. Deriving them separately is how a bare listing came to
    /// emit every shared cache on the machine into a command that deletes.
    fn actionable(&self) -> &[String] {
        match self.scope {
            Some(ratect_core::config::CacheScope::Shared) => &self.shared,
            _ => &self.owned,
        }
    }

    /// Names that exist in both scopes, which `clean <name>` must refuse
    /// rather than guess at — unless `--scope` has already said which.
    fn ambiguous(&self) -> Vec<&String> {
        if self.scope.is_some() {
            return Vec::new();
        }
        let mut names: Vec<&String> = self
            .owned
            .iter()
            .filter(|name| self.shared.contains(name))
            .collect();
        names.sort();
        names
    }

    /// Whether this invocation may remove caches of `scope` at all.
    fn covers(&self, scope: ratect_core::config::CacheScope) -> bool {
        self.scope.is_none_or(|wanted| wanted == scope)
    }
}

/// Every cache this project can see, split by ownership and narrowed by
/// `--scope`.
///
/// Both halves are read from *storage*, never from the configuration file —
/// see [`CachesArgs`] for why that matters. A project cache is found by its
/// `batect-cache-<key>-` prefix, a shared one by `ratect-shared-cache-`.
async fn find_caches(
    docker: Option<&DockerClient>,
    cache_type: ratect_core::cache::CacheType,
    project_directory: &std::path::Path,
    scope: Option<ratect_core::config::CacheScope>,
) -> anyhow::Result<CacheSelection> {
    use ratect_core::config::CacheScope;

    let (mut owned, mut shared) = (Vec::new(), Vec::new());
    match cache_type {
        ratect_core::cache::CacheType::Volume => {
            let docker = docker.expect("a volume cache needs a Docker client");
            let key = ratect_core::cache::project_cache_key(project_directory)?;
            // One listing for both — see `list_all_volume_caches`.
            for (name, found_scope) in
                ratect_core::cache::list_all_volume_caches(docker, &key).await?
            {
                match found_scope {
                    CacheScope::Project => owned.push(name),
                    CacheScope::Shared => shared.push(name),
                }
            }
        }
        ratect_core::cache::CacheType::Directory => {
            owned = ratect_core::cache::list_directory_caches(project_directory)?;
            shared = ratect_core::cache::list_shared_directory_caches()?;
        }
    }

    // `--scope` narrows what the invocation is working with, so every
    // question below is answered against the narrowed set rather than each
    // site remembering to re-apply it.
    if scope == Some(CacheScope::Shared) {
        owned.clear();
    }
    if scope == Some(CacheScope::Project) {
        shared.clear();
    }
    owned.sort();
    shared.sort();
    Ok(CacheSelection {
        owned,
        shared,
        scope,
    })
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

    // Independent of whether the config loads — a project mid-migration may
    // have a broken batect.yml and a leftover wrapper at once, and the
    // wrapper lives beside the config file regardless.
    findings.extend(wrapper_script_findings(ratect_core::config::base_path_for(
        &global.config_file,
    )));

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

    report_findings(&global.config_file, &findings, style)
}

/// Renders a set of [`Finding`]s and turns them into an exit status — shared
/// by `doctor` and `config validate`, so the two agree on formatting and on
/// "a problem fails, a warning doesn't". Quiet prints only what needs acting
/// on; otherwise a `Checking <file>...` header, every finding, and a summary.
fn report_findings(config_file: &Path, findings: &[Finding], style: OutputStyle) -> Result<()> {
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
        println!("Checking {}...", config_file.display());
        for finding in findings {
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

/// `ratect config validate` — `doctor`'s configuration half, without touching
/// Docker: does the config load, and is it free of the same problems `doctor`
/// checks (missing `build_directory`/Dockerfile, floating tags, unguarded
/// dependencies)? Exits non-zero on a problem, so it drops into CI as the
/// config-only gate that doesn't need a daemon.
async fn validate_config(
    args: ConfigValidateArgs,
    global: &GlobalArgs,
    style: OutputStyle,
) -> Result<()> {
    let mut findings = Vec::new();
    match load(global, &args.config_vars).await {
        Ok(project) => {
            findings.push(Finding::Fine(format!(
                "{} loads ({} container(s), {} task(s))",
                global.config_file.display(),
                project.config.containers.len(),
                project.config.tasks.len()
            )));
            findings.extend(config_findings(&project.config));
        }
        Err(error) => findings.push(Finding::Problem(format!(
            "{} does not load: {error:#}",
            global.config_file.display()
        ))),
    }
    report_findings(&global.config_file, &findings, style)
}

/// `ratect config convert` — translate a Batect-format `batect.yml` into a
/// native `ratect.toml`. One-directional: `ratect-compat` stays YAML, and the
/// reverse is both pointless and lossy.
///
/// Loads the source through the compat (YAML) path, so its anchors/aliases/
/// merge keys are expanded by the parser — the "inline for correctness" half —
/// and serializes the merged but *unresolved* config, so expressions (`<var`,
/// `$ENV`) and relative paths survive verbatim rather than being baked in.
/// `include`d files are flattened into the one result. Comments are lost, so
/// the output is a starting point to review, not a blind drop-in — but the
/// conversion is checked to round-trip losslessly first, so the behaviour it
/// encodes is guaranteed identical.
async fn convert_config(
    args: ConfigConvertArgs,
    global: &GlobalArgs,
    style: OutputStyle,
) -> Result<()> {
    // `config convert` reads a `batect.yml` and writes a `ratect.toml`. The
    // global `-f` defaults to `ratect.toml` — the *output* — which is the wrong
    // source here, so when `-f` wasn't given, default the source to
    // `batect.yml` (the thing being converted) instead.
    let source = if global.config_file == Path::new(DEFAULT_CONFIG_FILE) {
        Path::new(BATECT_CONFIG_FILE)
    } else {
        global.config_file.as_path()
    };
    let loaded = Config::load_from_file(source)
        .await
        .with_context(|| format!("Failed to load {} for conversion", source.display()))?;

    let toml = ratect_core::config::to_native_toml(&loaded.config)
        .with_context(|| format!("Failed to convert {}", source.display()))?;
    let document = format!(
        "# Generated by `ratect config convert` from {}.\n\
         # Review before use: comments were not carried over, and any included\n\
         # files were flattened into this one.\n\n{toml}",
        source.display()
    );

    if args.stdout {
        print!("{document}");
        return Ok(());
    }

    let output = ratect_core::config::base_path_for(source).join(DEFAULT_CONFIG_FILE);
    write_generated_config(&output, &document, args.force)?;

    if style != OutputStyle::Quiet {
        println!("Converted {} to {}.", source.display(), output.display());
        println!(
            "Review it, then remove {} (and any now-flattened includes).",
            source.display()
        );
    }
    Ok(())
}

/// Writes a converted `ratect.toml` without ever truncating an existing file
/// in place — the output is exactly the file the user is told to hand-edit
/// afterwards, so a half-written or clobbered result would lose real work.
///
/// Without `--force`, the OS itself enforces no-clobber (`create_new`), which
/// also closes the check-then-write race a separate `exists()` test would
/// leave open. With `--force`, the new content is written to a temp sibling
/// and atomically `rename`d over the target, so an interrupted write (full
/// disk, signal) leaves the previous file intact rather than truncated.
fn write_generated_config(output: &Path, document: &str, force: bool) -> Result<()> {
    use std::io::Write;

    if !force {
        return match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
        {
            Ok(mut file) => file
                .write_all(document.as_bytes())
                .with_context(|| format!("Failed to write {}", output.display())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => anyhow::bail!(
                "{} already exists — pass --force to overwrite it, or --stdout to print instead.",
                output.display()
            ),
            Err(error) => {
                Err(error).with_context(|| format!("Failed to write {}", output.display()))
            }
        };
    }

    let directory = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(DEFAULT_CONFIG_FILE);
    // `process::id` keeps concurrent converts from sharing a temp path; the
    // rename is what actually makes the replacement atomic.
    let temp = directory.join(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&temp, document)
        .with_context(|| format!("Failed to write {}", temp.display()))?;
    std::fs::rename(&temp, output)
        .with_context(|| format!("Failed to replace {}", output.display()))?;
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

/// Batect's own wrapper scripts (`batect`/`batect.cmd`) left in a project
/// that's moved to Ratect. Not inert: `./batect` still downloads and runs
/// the unmaintained JVM binary, so during a migration you can believe
/// you've switched over while `./batect` quietly still runs the old tool.
///
/// Only flags a script that *still runs Batect* — matched by content, not
/// name, so a `batect` file that no longer does (deleted and replaced, or a
/// hand-written shim that execs `ratect-compat`) is correctly left alone.
/// The recommended migration is to delete the wrapper and run Ratect from
/// the PATH, since Ratect is an ordinary installed binary rather than a
/// downloaded-on-demand wrapper the way Batect was — see docs/ratect-cli.md.
fn wrapper_script_findings(project_directory: &Path) -> Vec<Finding> {
    ["batect", "batect.cmd"]
        .iter()
        .filter_map(|name| {
            let path = project_directory.join(name);
            // Small scripts (~200 lines); the marker is on line 2, so a
            // partial read would do, but reading the whole thing is
            // simpler and the file is tiny.
            let content = std::fs::read(&path).ok()?;
            is_batect_wrapper(&String::from_utf8_lossy(&content)).then(|| {
                Finding::Warning(format!(
                    "'{name}' is a Batect wrapper script and still runs Batect, not Ratect — \
                     delete it and run ratect (or ratect-compat) from your PATH"
                ))
            })
        })
        .collect()
}

/// Whether `content` is one of Batect's own wrapper scripts, by the notice
/// line its authors put near the top of both the Unix and Windows forms —
/// a deliberate, stable marker (`# This file is part of Batect.` /
/// `rem This file is part of Batect.`). Matched as a substring so the
/// comment character doesn't matter. A script repointed at Ratect won't
/// carry it, which is the whole point.
fn is_batect_wrapper(content: &str) -> bool {
    content.contains("This file is part of Batect.")
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

/// `ratect includes list`/`clean`/`refresh` — the Git include cache under
/// `~/.ratect/incl` (see [`ratect_core::git_include`]).
///
/// Unlike `caches` and `resources`, this cache is **global**: one directory
/// shared by every project on this machine, keyed by `(remote, ref)`. So
/// there's no project scoping to offer, and `clean` here necessarily
/// reaches other projects' includes. That's a wider blast radius than
/// anything else Ratect removes, and simultaneously a much smaller one:
/// everything in here is re-cloneable, so the worst case of removing too
/// much is a network fetch, which is why there's no confirmation.
///
/// `refresh` is the one that isn't merely convenience. A cached
/// `(remote, ref)` pair is otherwise frozen for good — `ensure_cached`
/// clones only when the working copy is missing, and the 30-day sweep never
/// catches an actively-used entry — so a `ref` that moves (a branch, or a
/// re-pushed tag) is invisible to a project forever.
async fn manage_includes(command: IncludesCommand, style: OutputStyle) -> Result<()> {
    let cache = ratect_core::git_include::GitIncludeCache::new();
    let quiet = style == OutputStyle::Quiet;

    match command {
        IncludesCommand::List => {
            let entries = cache.list().await?;
            if entries.is_empty() {
                if !quiet {
                    println!("No Git includes are cached.");
                }
                return Ok(());
            }
            if quiet {
                // Machine-readable, same contract as the other `list`
                // verbs: the identifying pair, tab-separated.
                for entry in entries {
                    println!("{}\t{}", entry.remote, entry.git_ref);
                }
                return Ok(());
            }
            let total: u64 = entries.iter().map(|entry| entry.size_bytes).sum();
            println!(
                "{} cached Git include(s), {} on disk:",
                entries.len(),
                format_size(total)
            );
            for entry in entries {
                println!("\n  {} at {}", entry.remote, entry.git_ref);
                println!(
                    "    {}, last used {} ago",
                    format_size(entry.size_bytes),
                    format_age(age_of(entry.last_used))
                );
            }
        }
        IncludesCommand::Clean(args) => {
            // `--all` is really "no minimum age"; the default matches the
            // automatic sweep, so a bare `clean` does on demand exactly
            // what Ratect would eventually have done on its own.
            let minimum_age = if args.all {
                None
            } else {
                Some(args.older_than.unwrap_or(DEFAULT_INCLUDE_STALE_AFTER))
            };
            let removed = cache.clean(minimum_age).await?;
            if !quiet {
                for entry in &removed {
                    println!("Removed {} at {}.", entry.remote, entry.git_ref);
                }
                println!("Removed {} cached Git include(s).", removed.len());
            }
        }
        IncludesCommand::Refresh => {
            let refreshed = cache.refresh().await?;
            if !quiet {
                for entry in &refreshed {
                    println!("Re-cloned {} at {}.", entry.remote, entry.git_ref);
                }
                println!("Re-cloned {} cached Git include(s).", refreshed.len());
            }
        }
    }

    Ok(())
}

/// The default `includes clean` threshold — the same 30 days the automatic
/// sweep uses, so a manual clean does on demand what would have happened
/// anyway.
const DEFAULT_INCLUDE_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// Seconds between `last_used` (Unix seconds, from the entry's sidecar) and
/// now.
fn age_of(last_used: u64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    now.saturating_sub(last_used) as i64
}

/// Bytes, rounded to the largest unit that leaves a number worth reading —
/// nobody sizing up a cache wants "5883494 bytes".
fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    match bytes {
        b if b >= GIB => format!("{:.1} GiB", b as f64 / GIB as f64),
        b if b >= MIB => format!("{:.1} MiB", b as f64 / MIB as f64),
        b if b >= KIB => format!("{:.1} KiB", b as f64 / KIB as f64),
        b => format!("{b} B"),
    }
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
#[path = "main_tests.rs"]
mod tests;
