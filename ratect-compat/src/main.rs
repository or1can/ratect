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

    /// Don't run prerequisites for the named task.
    #[arg(long = "skip-prerequisites")]
    skip_prerequisites: bool,

    /// Override the image used by a container, as CONTAINER=IMAGE
    /// (repeatable). The container's own `image`/`build_directory` and
    /// `image_pull_policy` are ignored entirely — the override is always
    /// pulled under the default IfNotPresent policy.
    #[arg(long = "override-image", value_parser = parse_container_value_pair)]
    override_image: Vec<(String, String)>,

    /// Tag the image built by a container, as CONTAINER=TAG (repeatable;
    /// a container may be given more than once to apply multiple tags).
    /// Only valid for a container that actually builds an image — errors
    /// if it ends up using a pulled image, or if it never runs at all.
    #[arg(long = "tag-image", value_parser = parse_container_value_pair)]
    tag_image: Vec<(String, String)>,

    /// If an infrastructure error occurs before the task's own container can
    /// start, leave all containers created for that task running so the
    /// issue can be investigated. Equivalent to providing both
    /// --no-cleanup-after-failure and --no-cleanup-after-success.
    #[arg(long = "no-cleanup")]
    no_cleanup: bool,

    /// If an infrastructure error occurs before the task's own container can
    /// start, leave all containers created for that task running so the
    /// issue can be investigated.
    #[arg(long = "no-cleanup-after-failure")]
    no_cleanup_after_failure: bool,

    /// If the task's own container runs to completion (regardless of its
    /// exit code), leave all containers created for that task running.
    #[arg(long = "no-cleanup-after-success")]
    no_cleanup_after_success: bool,

    /// Use BuildKit for image builds, regardless of the daemon's own
    /// advertised default or the DOCKER_BUILDKIT environment variable
    /// (which this flag takes precedence over). There's no
    /// --disable-buildkit counterpart — forcing the classic builder is
    /// only done via DOCKER_BUILDKIT=0.
    #[arg(long = "enable-buildkit")]
    enable_buildkit: bool,

    /// Docker host to use, e.g. 'unix:///var/run/docker.sock' or
    /// 'tcp://1.2.3.4:5678'. Defaults to the DOCKER_HOST environment
    /// variable, then Docker's own local default. Cannot be used together
    /// with --docker-context.
    #[arg(long = "docker-host")]
    docker_host: Option<String>,

    /// Docker CLI context to use. Defaults to the DOCKER_CONTEXT
    /// environment variable, then the Docker CLI's own active context.
    /// Cannot be used together with --docker-host.
    #[arg(long = "docker-context")]
    docker_context: Option<String>,

    /// Path to the directory containing Docker CLI configuration files
    /// (context store, config.json). Defaults to the DOCKER_CONFIG
    /// environment variable, then ~/.docker.
    #[arg(long = "docker-config")]
    docker_config: Option<PathBuf>,

    /// Use TLS when connecting to the Docker host. Behaves identically to
    /// --docker-tls-verify — Ratect always fully verifies the daemon's
    /// certificate; there is no way to skip verification (unlike Batect's
    /// plain --docker-tls, which does).
    #[arg(long = "docker-tls")]
    docker_tls: bool,

    /// Use TLS when connecting to the Docker host, verifying its
    /// certificate. Defaults to the DOCKER_TLS_VERIFY environment
    /// variable.
    #[arg(long = "docker-tls-verify")]
    docker_tls_verify: bool,

    /// Path to a directory containing ca.pem/cert.pem/key.pem to
    /// authenticate to the Docker host and verify it, unless overridden
    /// individually by --docker-tls-ca-cert/-cert/-key. Defaults to the
    /// DOCKER_CERT_PATH environment variable, then ~/.docker.
    #[arg(long = "docker-cert-path")]
    docker_cert_path: Option<PathBuf>,

    /// Path to the TLS CA certificate file used to verify the Docker
    /// host's own certificate. Defaults to ca.pem in --docker-cert-path's
    /// directory.
    #[arg(long = "docker-tls-ca-cert")]
    docker_tls_ca_cert: Option<PathBuf>,

    /// Path to the TLS certificate file used to authenticate to the Docker
    /// host. Defaults to cert.pem in --docker-cert-path's directory.
    #[arg(long = "docker-tls-cert")]
    docker_tls_cert: Option<PathBuf>,

    /// Path to the TLS key file used to authenticate to the Docker host.
    /// Defaults to key.pem in --docker-cert-path's directory.
    #[arg(long = "docker-tls-key")]
    docker_tls_key: Option<PathBuf>,

    /// Maximum number of image pulls/builds to run in parallel when
    /// running a task. Unset means unbounded.
    #[arg(long = "max-parallelism", value_parser = clap::value_parser!(u32).range(1..))]
    max_parallelism: Option<u32>,

    /// Storage mechanism for `cache` volume mounts: volume (a Docker named
    /// volume) or directory (a host directory under
    /// <project_directory>/.batect/caches/<name>).
    #[arg(long = "cache-type", value_enum, default_value = "volume")]
    cache_type: CacheTypeArg,

    /// Remove every one of this project's cache volumes/directories and
    /// exit, without running anything. Never needs the config file itself.
    #[arg(long = "clean")]
    clean: bool,

    /// Remove the named cache volume/directory (repeatable) and exit,
    /// without running anything. Never needs the config file itself.
    #[arg(long = "clean-cache")]
    clean_cache: Vec<String>,

    /// Write Ratect's own internal logs to this file, in addition to
    /// stderr (still governed by RUST_LOG as usual).
    #[arg(long = "log-file")]
    log_file: Option<PathBuf>,

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

    let mut config_var_overrides: HashMap<String, String> = match &args.config_vars_file {
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

    fn args(arguments: &[&str]) -> Args {
        Args::try_parse_from(arguments).expect("should parse")
    }

    const BINARY: &str = "ratect-compat";
    /// `ratect-compat` takes the task name as a trailing positional, after
    /// its flags.
    const TASK_ARGUMENTS: &[&str] = &["build"];

    fn settings_from(arguments: &[&str]) -> TaskEngineSettings {
        args(arguments).engine_settings(PathBuf::from("/p"))
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
    /// This is the test that catches *cross-wiring*, which the all-flags-at-
    /// once test above cannot: with `--disable-ports` and `--no-proxy-vars`
    /// both set, a field reading the wrong one of the two looks identical
    /// to a field reading the right one. Setting one flag at a time and
    /// asserting the exact set of changed fields is what tells them apart.
    #[test]
    fn each_flag_changes_only_its_own_setting() {
        for (flag, expected) in FLAG_TO_SETTING {
            let mut arguments = vec![BINARY];
            arguments.extend_from_slice(flag);
            arguments.extend_from_slice(TASK_ARGUMENTS);
            let settings = settings_from(&arguments);
            assert_eq!(
                changed_from_default(&settings),
                vec![*expected],
                "{flag:?} should change exactly `{expected}`"
            );
        }
    }

    /// With nothing asked for, the engine must behave exactly as it would
    /// with no settings applied — an inverted boolean would silently change
    /// the default behavior of every run.
    #[test]
    fn no_flags_maps_to_the_engines_own_defaults() {
        let settings = args(&["ratect-compat", "build"]).engine_settings(PathBuf::from("/p"));
        let defaults = TaskEngineSettings::default();

        assert_eq!(settings.existing_network, defaults.existing_network);
        assert_eq!(settings.publish_ports, defaults.publish_ports);
        assert_eq!(
            settings.propagate_proxy_environment_variables,
            defaults.propagate_proxy_environment_variables
        );
        assert_eq!(settings.run_prerequisites, defaults.run_prerequisites);
        assert_eq!(settings.image_overrides, defaults.image_overrides);
        assert_eq!(settings.image_tags, defaults.image_tags);
        assert_eq!(
            settings.cleanup_after_success,
            defaults.cleanup_after_success
        );
        assert_eq!(
            settings.cleanup_after_failure,
            defaults.cleanup_after_failure
        );
        assert_eq!(settings.max_parallelism, defaults.max_parallelism);
        assert_eq!(
            settings.cache,
            Some((ratect_core::cache::CacheType::Volume, PathBuf::from("/p")))
        );
        assert_eq!(
            settings.ratect_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// The regression guard for the whole flag surface: every field is set
    /// to something the default would never produce, so a field wired to
    /// the wrong flag, or a negation dropped, fails here. (A field missing
    /// from the literal is a compile error instead — see
    /// [`Args::engine_settings`].) It also catches a flag that's declared
    /// but never actually read, which nothing else would.
    #[test]
    fn every_flag_reaches_its_own_engine_setting() {
        let settings = args(&[
            "ratect-compat",
            "--use-network",
            "existing-network",
            "--disable-ports",
            "--no-proxy-vars",
            "--skip-prerequisites",
            "--override-image",
            "db=postgres:16",
            "--tag-image",
            "app=extra",
            "--tag-image",
            "app=second",
            "--no-cleanup",
            "--max-parallelism",
            "3",
            "--cache-type",
            "directory",
            "build",
        ])
        .engine_settings(PathBuf::from("/projects/demo"));

        assert_eq!(
            settings.existing_network.as_deref(),
            Some("existing-network")
        );
        assert!(!settings.publish_ports);
        assert!(!settings.propagate_proxy_environment_variables);
        assert!(!settings.run_prerequisites);
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
        assert!(!settings.cleanup_after_success);
        assert!(!settings.cleanup_after_failure);
        assert_eq!(settings.max_parallelism, Some(3));
        assert_eq!(
            settings.cache,
            Some((
                ratect_core::cache::CacheType::Directory,
                PathBuf::from("/projects/demo")
            ))
        );
        assert_eq!(
            settings.ratect_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    /// `--no-cleanup` is both halves together; each also stands alone, and
    /// confusing them would leave containers behind (or not) in exactly the
    /// case the user asked about.
    #[test]
    fn each_no_cleanup_flag_affects_only_its_own_half() {
        let success = args(&["ratect-compat", "--no-cleanup-after-success", "build"])
            .engine_settings(PathBuf::from("/p"));
        assert!(!success.cleanup_after_success);
        assert!(success.cleanup_after_failure);

        let failure = args(&["ratect-compat", "--no-cleanup-after-failure", "build"])
            .engine_settings(PathBuf::from("/p"));
        assert!(failure.cleanup_after_success);
        assert!(!failure.cleanup_after_failure);
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
    fn parses_skip_prerequisites_flag() {
        let args = Args::try_parse_from(["ratect", "--skip-prerequisites", "build"]).unwrap();
        assert!(args.skip_prerequisites);
    }

    #[test]
    fn defaults_skip_prerequisites_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.skip_prerequisites);
    }

    #[test]
    fn parses_repeated_override_image_flags() {
        let args = Args::try_parse_from([
            "ratect",
            "--override-image",
            "build-env=alpine:3.18",
            "--override-image",
            "test-env=ubuntu:22.04",
            "build",
        ])
        .unwrap();
        assert_eq!(
            args.override_image,
            vec![
                ("build-env".to_string(), "alpine:3.18".to_string()),
                ("test-env".to_string(), "ubuntu:22.04".to_string()),
            ]
        );
    }

    #[test]
    fn defaults_override_image_to_empty() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(args.override_image.is_empty());
    }

    #[test]
    fn rejects_override_image_without_equals_sign() {
        let result = Args::try_parse_from(["ratect", "--override-image", "NOEQUALS", "build"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_repeated_tag_image_flags() {
        let args = Args::try_parse_from([
            "ratect",
            "--tag-image",
            "build-env=my.registry/app:v1",
            "--tag-image",
            "build-env=my.registry/app:latest",
            "build",
        ])
        .unwrap();
        assert_eq!(
            args.tag_image,
            vec![
                ("build-env".to_string(), "my.registry/app:v1".to_string()),
                (
                    "build-env".to_string(),
                    "my.registry/app:latest".to_string()
                ),
            ]
        );
    }

    #[test]
    fn defaults_tag_image_to_empty() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(args.tag_image.is_empty());
    }

    #[test]
    fn rejects_tag_image_without_equals_sign() {
        let result = Args::try_parse_from(["ratect", "--tag-image", "NOEQUALS", "build"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_no_cleanup_flags() {
        let args = Args::try_parse_from(["ratect", "--no-cleanup", "build"]).unwrap();
        assert!(args.no_cleanup);
        assert!(!args.no_cleanup_after_failure);
        assert!(!args.no_cleanup_after_success);

        let args = Args::try_parse_from(["ratect", "--no-cleanup-after-failure", "build"]).unwrap();
        assert!(args.no_cleanup_after_failure);

        let args = Args::try_parse_from(["ratect", "--no-cleanup-after-success", "build"]).unwrap();
        assert!(args.no_cleanup_after_success);
    }

    #[test]
    fn defaults_no_cleanup_flags_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.no_cleanup);
        assert!(!args.no_cleanup_after_failure);
        assert!(!args.no_cleanup_after_success);
    }

    #[test]
    fn parses_enable_buildkit_flag() {
        let args = Args::try_parse_from(["ratect", "--enable-buildkit", "build"]).unwrap();
        assert!(args.enable_buildkit);
    }

    #[test]
    fn defaults_enable_buildkit_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.enable_buildkit);
    }

    #[test]
    fn parses_docker_connection_flags() {
        let args = Args::try_parse_from([
            "ratect",
            "--docker-host",
            "tcp://1.2.3.4:2375",
            "--docker-config",
            "/tmp/docker-config",
            "build",
        ])
        .unwrap();
        assert_eq!(args.docker_host, Some("tcp://1.2.3.4:2375".to_string()));
        assert_eq!(args.docker_context, None);
        assert_eq!(
            args.docker_config,
            Some(PathBuf::from("/tmp/docker-config"))
        );

        let args =
            Args::try_parse_from(["ratect", "--docker-context", "my-context", "build"]).unwrap();
        assert_eq!(args.docker_context, Some("my-context".to_string()));
    }

    #[test]
    fn defaults_docker_connection_flags_to_none() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.docker_host, None);
        assert_eq!(args.docker_context, None);
        assert_eq!(args.docker_config, None);
    }

    #[test]
    fn parses_docker_tls_flags() {
        let args = Args::try_parse_from([
            "ratect",
            "--docker-tls-verify",
            "--docker-cert-path",
            "/tmp/certs",
            "--docker-tls-ca-cert",
            "/tmp/ca.pem",
            "--docker-tls-cert",
            "/tmp/cert.pem",
            "--docker-tls-key",
            "/tmp/key.pem",
            "build",
        ])
        .unwrap();
        assert!(!args.docker_tls);
        assert!(args.docker_tls_verify);
        assert_eq!(args.docker_cert_path, Some(PathBuf::from("/tmp/certs")));
        assert_eq!(args.docker_tls_ca_cert, Some(PathBuf::from("/tmp/ca.pem")));
        assert_eq!(args.docker_tls_cert, Some(PathBuf::from("/tmp/cert.pem")));
        assert_eq!(args.docker_tls_key, Some(PathBuf::from("/tmp/key.pem")));

        let args = Args::try_parse_from(["ratect", "--docker-tls", "build"]).unwrap();
        assert!(args.docker_tls);
    }

    #[test]
    fn defaults_docker_tls_flags_to_false_or_none() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.docker_tls);
        assert!(!args.docker_tls_verify);
        assert_eq!(args.docker_cert_path, None);
        assert_eq!(args.docker_tls_ca_cert, None);
        assert_eq!(args.docker_tls_cert, None);
        assert_eq!(args.docker_tls_key, None);
    }

    #[test]
    fn parses_max_parallelism_flag() {
        let args = Args::try_parse_from(["ratect", "--max-parallelism", "4", "build"]).unwrap();
        assert_eq!(args.max_parallelism, Some(4));
    }

    #[test]
    fn defaults_max_parallelism_to_none() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.max_parallelism, None);
    }

    #[test]
    fn rejects_a_zero_max_parallelism() {
        let result = Args::try_parse_from(["ratect", "--max-parallelism", "0", "build"]);
        assert!(result.is_err());
    }

    #[test]
    fn defaults_cache_type_to_volume() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.cache_type, CacheTypeArg::Volume);
    }

    #[test]
    fn parses_cache_type_flag() {
        let args = Args::try_parse_from(["ratect", "--cache-type", "directory", "build"]).unwrap();
        assert_eq!(args.cache_type, CacheTypeArg::Directory);

        let args = Args::try_parse_from(["ratect", "--cache-type", "volume", "build"]).unwrap();
        assert_eq!(args.cache_type, CacheTypeArg::Volume);
    }

    #[test]
    fn rejects_an_unknown_cache_type_naming_the_valid_ones() {
        let error = Args::try_parse_from(["ratect", "--cache-type", "host", "build"])
            .unwrap_err()
            .to_string();
        for name in ["volume", "directory"] {
            assert!(error.contains(name), "error should list '{name}': {error}");
        }
    }

    #[test]
    fn parses_clean_flag() {
        let args = Args::try_parse_from(["ratect", "--clean"]).unwrap();
        assert!(args.clean);
    }

    #[test]
    fn defaults_clean_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.clean);
    }

    #[test]
    fn parses_repeated_clean_cache_flags() {
        let args = Args::try_parse_from([
            "ratect",
            "--clean-cache",
            "gradle-cache",
            "--clean-cache",
            "npm-cache",
        ])
        .unwrap();
        assert_eq!(
            args.clean_cache,
            vec!["gradle-cache".to_string(), "npm-cache".to_string()]
        );
    }

    #[test]
    fn defaults_clean_cache_to_empty() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(args.clean_cache.is_empty());
    }

    #[tokio::test]
    async fn clean_cache_type_directory_short_circuits_before_touching_the_config_file() {
        // `--cache-type directory` makes no Docker connection at all, so
        // this can run as a normal unit test — unlike a `--cache-type
        // volume` clean, which would need a real daemon. A nonexistent
        // config file would normally fail `run` immediately (see the
        // "Configuration file ... not found" check); `--clean` must return
        // `Ok` before ever reaching that check, proving it's a genuine
        // short-circuit, the same way `--upgrade` is (see
        // `upgrade_flag_short_circuits_before_touching_the_config_file`).
        let args = Args::try_parse_from([
            "ratect",
            "--clean",
            "--cache-type",
            "directory",
            "-f",
            "/no/such/batect.yml",
        ])
        .unwrap();
        run(args)
            .await
            .expect("--clean should return Ok without touching the config file");
    }

    #[test]
    fn parses_log_file_flag() {
        let args =
            Args::try_parse_from(["ratect", "--log-file", "/tmp/ratect.log", "build"]).unwrap();
        assert_eq!(args.log_file, Some(PathBuf::from("/tmp/ratect.log")));
    }

    #[test]
    fn defaults_log_file_to_none() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert_eq!(args.log_file, None);
    }

    #[test]
    fn parses_batect_wrapper_flags_without_erroring() {
        // These have no effect in Ratect (see each field's own doc comment)
        // but must still parse cleanly — a Batect invocation carrying them
        // shouldn't hard-fail just because Ratect doesn't have a
        // self-updating wrapper script to apply them to.
        let args = Args::try_parse_from([
            "ratect",
            "--upgrade",
            "--no-update-notification",
            "--no-wrapper-cache-cleanup",
            "build",
        ])
        .unwrap();
        assert!(args.upgrade);
        assert!(args.no_update_notification);
        assert!(args.no_wrapper_cache_cleanup);
    }

    #[test]
    fn defaults_batect_wrapper_flags_to_false() {
        let args = Args::try_parse_from(["ratect"]).unwrap();
        assert!(!args.upgrade);
        assert!(!args.no_update_notification);
        assert!(!args.no_wrapper_cache_cleanup);
    }

    #[tokio::test]
    async fn upgrade_flag_short_circuits_before_touching_the_config_file() {
        // A nonexistent config file would normally fail `run` immediately
        // (see the "Configuration file ... not found" check) — `--upgrade`
        // must return `Ok` before ever reaching that check, proving it's a
        // genuine short-circuit rather than a flag that happens to be
        // harmless most of the time.
        let args =
            Args::try_parse_from(["ratect", "--upgrade", "-f", "/no/such/batect.yml"]).unwrap();
        run(args)
            .await
            .expect("--upgrade should return Ok without touching the config file");
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
