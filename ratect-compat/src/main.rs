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

use anyhow::{Context, Result};
use clap::Parser;
use ratect_core::config::{
    format_task_list, format_task_list_quiet, load_project, Config, LoadedProject,
};
use ratect_core::docker::{DockerClient, DockerConnectionOptions};
use ratect_core::engine::{TaskEngine, TaskEngineSettings};
use ratect_core::git_include::GitIncludeCache;
use ratect_core::ui::{create_event_sink, select_output_style, OutputStyle};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::fmt::writer::MakeWriterExt;
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
    #[arg(long = "config-var", value_parser = parse_config_var, help_heading = "Configuration variables")]
    config_var: Vec<(String, String)>,

    /// Path to a YAML file of config variable NAME: VALUE pairs
    #[arg(long = "config-vars-file", help_heading = "Configuration variables")]
    config_vars_file: Option<PathBuf>,

    /// Existing Docker network to use for all tasks. If not set, a new
    /// network is created (and removed) for each task.
    #[arg(long = "use-network", help_heading = "Task execution")]
    use_network: Option<String>,

    /// Disable binding of ports on the host, regardless of any `ports`
    /// configured on a container.
    #[arg(long = "disable-ports", help_heading = "Task execution")]
    disable_ports: bool,

    /// Don't propagate proxy-related environment variables such as
    /// http_proxy and no_proxy to image builds or containers.
    #[arg(long = "no-proxy-vars", help_heading = "Task execution")]
    no_proxy_vars: bool,

    /// Don't run prerequisites for the named task.
    #[arg(long = "skip-prerequisites", help_heading = "Task execution")]
    skip_prerequisites: bool,

    /// Override the image used by a container, as CONTAINER=IMAGE
    /// (repeatable). The container's own `image`/`build_directory` and
    /// `image_pull_policy` are ignored entirely — the override is always
    /// pulled under the default IfNotPresent policy.
    #[arg(long = "override-image", value_parser = parse_container_value_pair, help_heading = "Task execution")]
    override_image: Vec<(String, String)>,

    /// Tag the image built by a container, as CONTAINER=TAG (repeatable;
    /// a container may be given more than once to apply multiple tags).
    /// Only valid for a container that actually builds an image — errors
    /// if it ends up using a pulled image, or if it never runs at all.
    #[arg(long = "tag-image", value_parser = parse_container_value_pair, help_heading = "Task execution")]
    tag_image: Vec<(String, String)>,

    /// If an infrastructure error occurs before the task's own container can
    /// start, leave all containers created for that task running so the
    /// issue can be investigated. Equivalent to providing both
    /// --no-cleanup-after-failure and --no-cleanup-after-success.
    #[arg(long = "no-cleanup", help_heading = "Cleanup after a run")]
    no_cleanup: bool,

    /// If an infrastructure error occurs before the task's own container can
    /// start, leave all containers created for that task running so the
    /// issue can be investigated.
    #[arg(
        long = "no-cleanup-after-failure",
        help_heading = "Cleanup after a run"
    )]
    no_cleanup_after_failure: bool,

    /// If the task's own container runs to completion (regardless of its
    /// exit code), leave all containers created for that task running.
    #[arg(
        long = "no-cleanup-after-success",
        help_heading = "Cleanup after a run"
    )]
    no_cleanup_after_success: bool,

    /// Use BuildKit for image builds, regardless of the daemon's own
    /// advertised default or the DOCKER_BUILDKIT environment variable
    /// (which this flag takes precedence over). There's no
    /// --disable-buildkit counterpart — forcing the classic builder is
    /// only done via DOCKER_BUILDKIT=0.
    #[arg(long = "enable-buildkit", help_heading = "Task execution")]
    enable_buildkit: bool,

    /// Docker host to use, e.g. 'unix:///var/run/docker.sock' or
    /// 'tcp://1.2.3.4:5678'. Defaults to the DOCKER_HOST environment
    /// variable, then Docker's own local default. Cannot be used together
    /// with --docker-context.
    #[arg(long = "docker-host", help_heading = "Docker connection")]
    docker_host: Option<String>,

    /// Docker CLI context to use. Defaults to the DOCKER_CONTEXT
    /// environment variable, then the Docker CLI's own active context.
    /// Cannot be used together with --docker-host.
    #[arg(long = "docker-context", help_heading = "Docker connection")]
    docker_context: Option<String>,

    /// Path to the directory containing Docker CLI configuration files
    /// (context store, config.json). Defaults to the DOCKER_CONFIG
    /// environment variable, then ~/.docker.
    #[arg(long = "docker-config", help_heading = "Docker connection")]
    docker_config: Option<PathBuf>,

    /// Use TLS when connecting to the Docker host. Behaves identically to
    /// --docker-tls-verify — Ratect always fully verifies the daemon's
    /// certificate; there is no way to skip verification (unlike Batect's
    /// plain --docker-tls, which does).
    #[arg(long = "docker-tls", help_heading = "Docker connection")]
    docker_tls: bool,

    /// Use TLS when connecting to the Docker host, verifying its
    /// certificate. Defaults to the DOCKER_TLS_VERIFY environment
    /// variable.
    #[arg(long = "docker-tls-verify", help_heading = "Docker connection")]
    docker_tls_verify: bool,

    /// Path to a directory containing ca.pem/cert.pem/key.pem to
    /// authenticate to the Docker host and verify it, unless overridden
    /// individually by --docker-tls-ca-cert/-cert/-key. Defaults to the
    /// DOCKER_CERT_PATH environment variable, then ~/.docker.
    #[arg(long = "docker-cert-path", help_heading = "Docker connection")]
    docker_cert_path: Option<PathBuf>,

    /// Path to the TLS CA certificate file used to verify the Docker
    /// host's own certificate. Defaults to ca.pem in --docker-cert-path's
    /// directory.
    #[arg(long = "docker-tls-ca-cert", help_heading = "Docker connection")]
    docker_tls_ca_cert: Option<PathBuf>,

    /// Path to the TLS certificate file used to authenticate to the Docker
    /// host. Defaults to cert.pem in --docker-cert-path's directory.
    #[arg(long = "docker-tls-cert", help_heading = "Docker connection")]
    docker_tls_cert: Option<PathBuf>,

    /// Path to the TLS key file used to authenticate to the Docker host.
    /// Defaults to key.pem in --docker-cert-path's directory.
    #[arg(long = "docker-tls-key", help_heading = "Docker connection")]
    docker_tls_key: Option<PathBuf>,

    /// Maximum number of image pulls/builds to run in parallel when
    /// running a task. Unset means unbounded.
    #[arg(long = "max-parallelism", value_parser = clap::value_parser!(u32).range(1..), help_heading = "Task execution")]
    max_parallelism: Option<u32>,

    /// Storage mechanism for `cache` volume mounts: volume (a Docker named
    /// volume) or directory (a host directory under
    /// `<project_directory>/.batect/caches/<name>`).
    #[arg(
        long = "cache-type",
        value_enum,
        default_value = "volume",
        help_heading = "Task execution"
    )]
    cache_type: CacheTypeArg,

    /// Remove every one of this project's cache volumes/directories and
    /// exit, without running anything. Never needs the config file itself.
    #[arg(long = "clean", help_heading = "Cache management")]
    clean: bool,

    /// Remove the named cache volume/directory (repeatable) and exit,
    /// without running anything. Never needs the config file itself.
    #[arg(long = "clean-cache", help_heading = "Cache management")]
    clean_cache: Vec<String>,

    /// No effect. Ratect is a single native binary, not a self-updating
    /// wrapper script like Batect — recognized only so an existing Batect
    /// invocation carrying this flag doesn't fail outright.
    #[arg(long = "upgrade", hide = true)]
    upgrade: bool,

    /// No effect — see --upgrade.
    #[arg(long = "no-update-notification", hide = true)]
    no_update_notification: bool,

    /// No effect — see --upgrade.
    #[arg(long = "no-wrapper-cache-cleanup", hide = true)]
    no_wrapper_cache_cleanup: bool,

    /// Force a particular style of output (does not affect task command
    /// output): fancy (default when the console supports it — a live
    /// per-container status display), simple (plain lines, no updating
    /// text), all (interleaved output from all containers), or quiet (only
    /// error messages, and a machine-readable --list-tasks format).
    #[arg(short = 'o', long = "output", value_enum, help_heading = "Output")]
    output: Option<OutputStyleArg>,

    /// Disable colored output from Ratect. Does not affect task command
    /// output. Also makes simple (not fancy) the default output style.
    #[arg(long = "no-color", help_heading = "Output")]
    no_color: bool,

    /// Write Ratect's own internal logs to this file, in addition to
    /// stderr (still governed by RUST_LOG as usual).
    #[arg(long = "log-file", help_heading = "Output")]
    log_file: Option<PathBuf>,

    /// Name of the task to run
    task_name: Option<String>,

    /// Additional arguments to pass to the task command
    #[arg(last = true)]
    additional_args: Vec<String>,
}

impl Args {
    /// Maps the engine-affecting flags onto the engine's settings.
    ///
    /// Split out from [`run`] so it can be tested without a Docker daemon.
    /// A *missing* field is a compile error — this literal is exhaustive,
    /// with no `..Default::default()` — so the tests exist for what the
    /// compiler can't see: a field wired to the wrong flag, a dropped or
    /// inverted negation (`publish_ports: args.disable_ports` type checks
    /// perfectly and reverses the flag), and a flag declared but never read
    /// here. Keep the literal exhaustive for that reason. `ratect` has the
    /// same function for the same reasons.
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
            // Created here rather than in `ratect-core`, because a library
            // shouldn't take over a process's signal handling on its own
            // initiative. It isn't listening yet, though: `listen` spawns,
            // so it panics outside a runtime, and this function is
            // deliberately synchronous so the flag-mapping tests can call it
            // directly. The async path that actually runs a task arms it,
            // right after calling this.
            interrupt: Some(ratect_core::interrupt::Interrupt::new()),
        }
    }
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

/// The CLI-side `--cache-type` value set. Mirrors [`ratect_core::cache::CacheType`]
/// rather than deriving on it directly, same reasoning as `OutputStyleArg`.
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

/// Parses a `--config-var` value of the form `NAME=VALUE`.
fn parse_config_var(s: &str) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, value)) => Ok((name.to_string(), value.to_string())),
        None => Err(format!("expected NAME=VALUE, got '{s}'")),
    }
}

/// Parses a `CONTAINER=VALUE` pair, shared by `--override-image` (VALUE is
/// an image) and `--tag-image` (VALUE is a tag).
fn parse_container_value_pair(s: &str) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some((container, value)) => Ok((container.to_string(), value.to_string())),
        None => Err(format!("expected CONTAINER=VALUE, got '{s}'")),
    }
}

/// `log_file`, when given (`--log-file`), tees the same log output into
/// that file *in addition to* stderr — matching Batect's own `--log-file`
/// content, though not its silent-by-default behavior (Ratect always logs
/// to stderr regardless; Batect's own default with no `--log-file` is a
/// `NullLogSink`, nothing anywhere).
fn init_tracing(log_file: Option<&Path>) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let writer = match log_file {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("Failed to open log file '{}'", path.display()))?;
            tracing_subscriber::fmt::writer::BoxMakeWriter::new(
                std::io::stderr.and(std::sync::Mutex::new(file)),
            )
        }
        None => tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        // ANSI color codes have no business ending up in a log *file* meant
        // for later grepping/processing, but this builder has no
        // per-writer ANSI control, so it's an all-or-nothing choice shared
        // with stderr's own output. Stderr's own pre-existing behavior is
        // already unconditionally-ANSI regardless of whether it's a real
        // terminal (unrelated to `--log-file`, not something to fix here)
        // — this line only changes anything when `--log-file` is actually
        // given, trading stderr's color for a plain-text file.
        .with_ansi(log_file.is_none())
        .with_writer(writer)
        .init();
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if let Err(err) = init_tracing(args.log_file.as_deref()) {
        eprintln!("Error: {:?}", err);
        std::process::exit(1);
    }

    let exit_code = match run(args).await {
        Ok(()) => 0,
        Err(err) => {
            // Printed directly to stderr, not through `tracing::error!` —
            // `RUST_LOG` can suppress that entirely (e.g. `RUST_LOG=off`,
            // or any filter that excludes `ratect`'s own target), which
            // would leave a failed run with a non-zero exit code and no
            // visible reason anywhere: not on stdout (by design, especially
            // under `-o quiet`) and not on stderr either. A fatal error is
            // the reason the process is about to exit non-zero, not an
            // optional diagnostic — it must always be visible, in every
            // output mode, including quiet (whose whole documented
            // contract is "only error messages"). `{:?}` (not `{}`) prints
            // the full anyhow context chain — exactly what Rust's own
            // `Termination` impl would have printed had `main` returned a
            // `Result` directly (see this function's own doc comment,
            // below, for why it doesn't).
            eprintln!("Error: {:?}", err);

            exit_code_for(&err)
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

/// The process's exit code for a failed run.
///
/// If the task's own command exited non-zero, that exact code is propagated
/// as ratect's own (matching `docker run`'s convention) rather than a generic
/// failure code, so scripts can inspect what actually happened.
///
/// A run ended by a signal exits 128 + that signal's number — 130 for Ctrl+C,
/// 143 for `SIGTERM`, 129 for `SIGHUP` — the shell's own convention for
/// "killed by this signal", so a script or CI job can tell a cancelled run
/// apart from a failed one *and* tell which cancelled it. A divergence from
/// Batect, which returns -1 (255) for every failure alike and so says nothing
/// about which it was; Ratect already diverges here by using 1 rather than
/// 255 for an ordinary failure.
fn exit_code_for(error: &anyhow::Error) -> u8 {
    match error.downcast_ref::<ratect_core::docker::ContainerExitedNonZero>() {
        Some(failure) => failure.exit_code as u8,
        None => match error.downcast_ref::<ratect_core::interrupt::TaskInterrupted>() {
            Some(interrupted) => (128 + interrupted.signal.number()) as u8,
            None => 1,
        },
    }
}

/// Resolves which config-variables file to load. An explicit
/// `--config-vars-file` always wins; otherwise Batect's default of
/// `batect.local.yml` applies, but *only when that file exists* — an absent
/// default file is simply "no overrides", not an error. This mirrors
/// Batect's `FileDefaultValueProvider("batect.local.yml")`, which resolves
/// the default against the current directory (`dir` here), not against the
/// `-f` config file's own directory.
fn resolve_config_vars_file(explicit: Option<PathBuf>, dir: &Path) -> Option<PathBuf> {
    explicit.or_else(|| {
        let default = dir.join("batect.local.yml");
        default.is_file().then_some(default)
    })
}

async fn run(args: Args) -> Result<()> {
    if args.upgrade {
        eprintln!(
            "--upgrade has no effect: Ratect is a single native binary, not a self-updating \
             wrapper script like Batect. Reinstall/rebuild to get a newer version instead."
        );
        return Ok(());
    }

    if args.clean || !args.clean_cache.is_empty() {
        return clean_caches(&args).await;
    }

    let config_vars_file =
        resolve_config_vars_file(args.config_vars_file.clone(), &std::env::current_dir()?);
    let mut config_var_overrides: HashMap<String, String> = match &config_vars_file {
        Some(path) => Config::load_config_vars_file(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(args.config_var.iter().cloned());
    let LoadedProject {
        config,
        project_directory,
    } = load_project(&args.config_file, &config_var_overrides).await?;

    // Gathered once, here, and reused for both the `--list-tasks` quiet-
    // format decision below and (inside `create_event_sink`) the real
    // logger construction — rather than each querying stdout/TERM/console
    // dimensions again on top of the other.
    let term = std::env::var("TERM").ok();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let console_dimensions_available = ratect_core::ui::console_dimensions_available();
    let requested_style = args.output.map(OutputStyle::from);
    let output_style = select_output_style(
        requested_style,
        args.no_color,
        stdout_is_terminal,
        term.as_deref(),
        console_dimensions_available,
    );

    if args.list_tasks {
        let listing = match output_style {
            OutputStyle::Quiet => format_task_list_quiet(&config.tasks),
            _ => format_task_list(&config.project_name, &config.tasks),
        };
        println!("{listing}");
        return Ok(());
    }

    match args.task_name.as_deref() {
        Some(task_name) => {
            // Unconditional, fire-and-forget — matching Batect's own
            // `GitRepositoryCacheCleanupTask`, an unconditional daemon
            // thread started on every "run a task" invocation regardless of
            // whether this particular config uses a Git include. Never
            // awaited: a failure (or simply not finishing before the
            // process exits — see `run` below's own doc comment on
            // `std::process::exit`) is inherently best-effort, same as a
            // JVM daemon thread not blocking process exit either.
            tokio::spawn(async {
                if let Err(e) = GitIncludeCache::new().cleanup_stale().await {
                    tracing::warn!("Failed to sweep stale Git include cache entries: {e}");
                }
            });

            // The output-mode logger — one instance shared by the Docker
            // client (fine-grained pull/build progress) and the engine
            // (lifecycle milestones), so it sees the whole event stream in
            // order. Selection, construction, and (for an explicit fancy)
            // validation all live in `create_event_sink` — see its own docs
            // for why, and for the fancy-on-a-non-interactive-console error
            // it can return.
            let event_sink = create_event_sink(
                requested_style,
                args.no_color,
                stdout_is_terminal,
                term.as_deref(),
                console_dimensions_available,
            )?;
            // Built before the connection options consume `args` below.
            let settings = args.engine_settings(project_directory);
            // Armed here rather than in `engine_settings`, which is
            // synchronous — see its own comment. From this point Ctrl+C,
            // `SIGTERM` or `SIGHUP` abandons the run and cleans up instead
            // of killing the process where it stands.
            if let Some(interrupt) = &settings.interrupt {
                interrupt.listen();
            }
            let docker_connection = DockerConnectionOptions {
                host: args.docker_host,
                context: args.docker_context,
                config_directory: args.docker_config,
                tls: args.docker_tls,
                tls_verify: args.docker_tls_verify,
                cert_path: args.docker_cert_path,
                tls_ca_cert: args.docker_tls_ca_cert,
                tls_cert: args.docker_tls_cert,
                tls_key: args.docker_tls_key,
            };
            let docker = DockerClient::new(&docker_connection)?
                .with_event_sink(Arc::clone(&event_sink))
                .with_enable_buildkit(args.enable_buildkit);
            let engine = TaskEngine::new(config, docker)
                .with_event_sink(event_sink)
                .with_settings(settings)?;
            engine.run_task(task_name, &args.additional_args).await?;
        }
        None => {
            tracing::warn!("No task name provided. Use --help for usage.");
        }
    }

    Ok(())
}

/// `--clean`/`--clean-cache`: removes this project's own cache
/// volumes/directories and exits, without running anything. Never needs
/// `--config-file` to actually exist — matching Batect, whose own
/// `CleanupCachesCommand` only needs the project directory and
/// `--cache-type`/Docker connection flags, not the task config itself.
///
/// `--clean-cache <NAME>` (repeatable) restricts this to the named caches;
/// plain `--clean` with no `--clean-cache` cleans every one of this
/// project's own caches — matching Batect's own `CommandFactory`/
/// `CleanupCachesCommand` exactly: the explicit `cleanCaches` list (if
/// non-empty) always wins over `--clean`'s "everything" default, regardless
/// of whether `--clean` was also given.
async fn clean_caches(args: &Args) -> Result<()> {
    let base_path = ratect_core::config::base_path_for(&args.config_file);
    let project_directory = ratect_core::config::project_directory_path(base_path)?;
    let only: HashSet<String> = args.clean_cache.iter().cloned().collect();
    let cache_type: ratect_core::cache::CacheType = args.cache_type.into();
    let (singular, plural) = match cache_type {
        ratect_core::cache::CacheType::Volume => ("volume", "volumes"),
        ratect_core::cache::CacheType::Directory => ("directory", "directories"),
    };

    let removed = match cache_type {
        ratect_core::cache::CacheType::Volume => {
            println!("Checking for cache volumes...");
            let docker_connection = DockerConnectionOptions {
                host: args.docker_host.clone(),
                context: args.docker_context.clone(),
                config_directory: args.docker_config.clone(),
                tls: args.docker_tls,
                tls_verify: args.docker_tls_verify,
                cert_path: args.docker_cert_path.clone(),
                tls_ca_cert: args.docker_tls_ca_cert.clone(),
                tls_cert: args.docker_tls_cert.clone(),
                tls_key: args.docker_tls_key.clone(),
            };
            let docker = DockerClient::new(&docker_connection)?;
            let project_cache_key = ratect_core::cache::project_cache_key(&project_directory)?;
            let removed =
                ratect_core::cache::clean_volume_caches(&docker, &project_cache_key, &only).await?;
            for name in &removed {
                println!("Deleting volume '{name}'...");
            }
            removed
        }
        ratect_core::cache::CacheType::Directory => {
            let cache_directory = ratect_core::cache::cache_directory(&project_directory);
            println!(
                "Checking for cache directories in '{}'...",
                cache_directory.display()
            );
            let removed = ratect_core::cache::clean_directory_caches(&project_directory, &only)?;
            for name in &removed {
                println!("Deleting '{}'...", cache_directory.join(name).display());
            }
            removed
        }
    };

    let noun = if removed.len() == 1 { singular } else { plural };
    println!("Done! Deleted {} {noun}.", removed.len());

    Ok(())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
