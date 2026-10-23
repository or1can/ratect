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
use clap::{CommandFactory, Parser};
use ratect_core::cache::CacheType;
use ratect_core::config::{
    format_task_list, format_task_list_quiet, load_project, Config, Dialect,
};
use ratect_core::docker::{DockerClient, DockerConnectionOptions};
use ratect_core::engine::{TaskEngine, TaskEngineSettings};
use ratect_core::exit_code::{InvalidOptionValue, Word};
use ratect_core::git_include::GitIncludeCache;
use ratect_core::ui::{
    create_event_sink, select_output_style, ColorMode, Console, OutputStyle, ProgressStream,
    TaskEvent, TerminalFacts,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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

    /// Set a config variable's value, as NAME=VALUE (repeatable, once per
    /// NAME). Takes precedence over --config-vars-file and the variable's
    /// `default` in `config_variables`.
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
    /// (repeatable, once per CONTAINER; neither side may be empty). The
    /// container's own `image`/`build_directory` and `image_pull_policy` are
    /// ignored entirely — the override is always pulled under the default
    /// IfNotPresent policy.
    #[arg(long = "override-image", value_parser = parse_override_image, help_heading = "Task execution")]
    override_image: Vec<(String, String)>,

    /// Tag the image built by a container, as CONTAINER=TAG (repeatable;
    /// a container may be given more than once to apply multiple tags).
    /// Only valid for a container that actually builds an image — errors
    /// if it ends up using a pulled image, or if it never runs at all.
    /// A TAG Docker would refuse is rejected before anything runs.
    #[arg(long = "tag-image", value_parser = parse_tag_image, help_heading = "Task execution")]
    tag_image: Vec<(String, String)>,

    /// If an infrastructure error occurs before the task's own container can
    /// start, or its own health check or setup commands fail while its main
    /// command is still running, leave all containers created for that task
    /// running so the issue can be investigated. Equivalent to providing
    /// both --no-cleanup-after-failure and --no-cleanup-after-success.
    #[arg(long = "no-cleanup", help_heading = "Cleanup after a run")]
    no_cleanup: bool,

    /// If an infrastructure error occurs before the task's own container can
    /// start, or its own health check or setup commands fail while its main
    /// command is still running, leave all containers created for that task
    /// running so the issue can be investigated.
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
    /// environment variable, then the Docker CLI's own active context,
    /// unless a Docker host is set. Cannot be used together with
    /// --docker-host, --docker-tls, --docker-tls-verify, --docker-cert-path
    /// or --docker-tls-ca-cert/-cert/-key.
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

    /// Path to a directory containing any of ca.pem/cert.pem/key.pem to
    /// authenticate to the Docker host and verify it, unless overridden
    /// individually by --docker-tls-ca-cert/-cert/-key. Defaults to the
    /// DOCKER_CERT_PATH environment variable, then --docker-config's
    /// directory if given (or DOCKER_CONFIG's), then ~/.docker.
    #[arg(long = "docker-cert-path", help_heading = "Docker connection")]
    docker_cert_path: Option<PathBuf>,

    /// Path to the TLS CA certificate file used to verify the Docker
    /// host's own certificate. Defaults to ca.pem in the certificate
    /// directory (see --docker-cert-path).
    #[arg(long = "docker-tls-ca-cert", help_heading = "Docker connection")]
    docker_tls_ca_cert: Option<PathBuf>,

    /// Path to the TLS certificate file used to authenticate to the Docker
    /// host. Defaults to cert.pem in the certificate directory.
    #[arg(long = "docker-tls-cert", help_heading = "Docker connection")]
    docker_tls_cert: Option<PathBuf>,

    /// Path to the TLS key file used to authenticate to the Docker host.
    /// Defaults to key.pem in the certificate directory.
    #[arg(long = "docker-tls-key", help_heading = "Docker connection")]
    docker_tls_key: Option<PathBuf>,

    /// Maximum number of image pulls/builds, dependency container starts and
    /// setup commands to run in parallel when running a task, from 1 to
    /// 2147483647. Unset means unbounded.
    #[arg(long = "max-parallelism", value_parser = clap::value_parser!(u32).range(1..=i32::MAX as i64), help_heading = "Task execution")]
    max_parallelism: Option<u32>,

    /// Storage mechanism for `cache` volume mounts: volume (a Docker named
    /// volume) or directory (a host directory under
    /// `<project_directory>/.batect/caches/<name>`). Defaults to the
    /// BATECT_CACHE_TYPE environment variable, then volume.
    #[arg(long = "cache-type", value_enum, help_heading = "Task execution")]
    cache_type: Option<CacheTypeArg>,

    /// Remove every one of this project's cache volumes/directories and
    /// exit, without running anything. Never needs the config file itself.
    #[arg(long = "clean", help_heading = "Cache management")]
    clean: bool,

    /// Remove the named cache volumes/directories and exit, without running
    /// anything: a comma-separated list of names, each trimmed (repeatable).
    /// Never needs the config file itself.
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

    // `task_name` and `additional_args` are declared for `--help`'s usage
    // line only: nothing after the options reaches `clap`, and
    // `parse_checked` sets them itself.
    /// Name of the task to run
    task_name: Option<String>,

    /// Additional arguments to pass to the task command
    #[arg(last = true)]
    additional_args: Vec<String>,
}

impl Args {
    /// Whether this invocation is a *run* — none of the flags that [`run`]
    /// handles instead of a task, whether or not a task is named — what
    /// `ratect_core::exit_code::for_failure` is told (decisions/0016). A line
    /// naming no task is one: it was meant to run something, and a CI step
    /// that runs nothing should not look like an ordinary verb's failure.
    /// [`is_a_run`] answers the same question from the raw arguments, for
    /// when they don't parse.
    fn runs_a_task(&self) -> bool {
        !self.upgrade && !self.clean && self.clean_cache.is_empty() && !self.list_tasks
    }

    /// The cache type: `--cache-type`, else `BATECT_CACHE_TYPE`
    /// (`batect_cache_type_env`, injected so this stays testable), else
    /// volume. The variable must name a cache type exactly, as in Batect —
    /// `Directory` and an empty value are refused — but only when the flag
    /// doesn't make it moot. [`run`] resolves it once, for every command,
    /// as Batect checks its options' defaults before running any.
    fn cache_type(&self, batect_cache_type_env: Option<&str>) -> Result<CacheType> {
        if let Some(cache_type) = self.cache_type {
            return Ok(cache_type.into());
        }
        match batect_cache_type_env {
            None | Some("volume") => Ok(CacheType::Volume),
            Some("directory") => Ok(CacheType::Directory),
            Some(other) => Err(InvalidOptionValue::new(format!(
                "BATECT_CACHE_TYPE is '{other}', which is not a cache type: use volume or \
                 directory, or give --cache-type."
            ))
            .into()),
        }
    }

    /// The Docker connection flags, as `ratect-core` takes them.
    fn docker_connection(&self) -> DockerConnectionOptions {
        DockerConnectionOptions {
            host: self.docker_host.clone(),
            context: self.docker_context.clone(),
            config_directory: self.docker_config.clone(),
            tls: self.docker_tls,
            tls_verify: self.docker_tls_verify,
            cert_path: self.docker_cert_path.clone(),
            tls_ca_cert: self.docker_tls_ca_cert.clone(),
            tls_cert: self.docker_tls_cert.clone(),
            tls_key: self.docker_tls_key.clone(),
        }
    }

    /// The command line in Batect's grammar, which `clap`'s alone can't say
    /// (ratect#435). The options end at the first argument that isn't one —
    /// [`split_at_task`] finds it — and only they reach `clap`. What follows
    /// is the task name, then nothing or `-- ARGS…`, and a run without one is
    /// refused here, before any file is read (ratect#434); under a flag that
    /// replaces the task it is ignored instead, as Batect's `parseTaskName`
    /// ignores it.
    ///
    /// `--clean-cache`'s values are split into names here, as Batect's
    /// `SetOption` splits them. Then the one check `clap` can't make per
    /// value: a key given twice to
    /// `--config-var` or `--override-image`, which Batect's
    /// `SingleValueMapOption` refuses rather than keeping the last
    /// (ratect#438). `--tag-image` may repeat a container, as in Batect. A
    /// `--config-var` also set by a `--config-vars-file` isn't a repeat:
    /// only the command line is counted, and it still wins over the file.
    /// Every error is `clap`'s own kind, so it prints and exits like any
    /// other argument `clap` rejects.
    fn parse_checked<I, T>(arguments: I) -> std::result::Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let arguments: Vec<std::ffi::OsString> = arguments.into_iter().map(Into::into).collect();
        let (options, rest, refused) = split_at_task(&arguments);
        let mut args = match (Self::try_parse_from(options), refused) {
            // `--help` and `--version` act as soon as `clap` reaches them.
            (Err(error), _) if !error.use_stderr() => return Err(error),
            (_, Some(error)) => return Err(error),
            (parsed, None) => parsed?,
        };
        // Batect's `SetOption`: each value is a comma-separated list, each
        // name trimmed (ratect#448). An empty name stays, matching no cache,
        // so `--clean-cache ""` still selects cleanup, as in Batect.
        args.clean_cache = args
            .clean_cache
            .iter()
            .flat_map(|value| value.split(',').map(|name| name.trim().to_string()))
            .collect();
        if args.runs_a_task() {
            let rest = rest
                .iter()
                .map(|word| {
                    word.to_str().ok_or_else(|| {
                        Self::command().error(
                            clap::error::ErrorKind::InvalidUtf8,
                            format!("{word:?} is not valid UTF-8"),
                        )
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            match rest.as_slice() {
                [] | ["--", ..] => {
                    return Err(Self::command().error(
                        clap::error::ErrorKind::MissingRequiredArgument,
                        "no task name was given: name the task to run after the options, as \
                         in 'ratect-compat build'. 'ratect-compat --list-tasks' lists this \
                         project's tasks, and 'ratect-compat --help' every option",
                    ));
                }
                [task] => args.task_name = Some(task.to_string()),
                [task, "--", task_args @ ..] => {
                    args.task_name = Some(task.to_string());
                    args.additional_args = task_args.iter().map(|arg| arg.to_string()).collect();
                }
                [task, extra, ..] => {
                    return Err(Self::command().error(
                        clap::error::ErrorKind::UnknownArgument,
                        format!(
                            "'{task}' was taken as the task name, and '{extra}' follows it: \
                             options go before the task name, and arguments for the task \
                             after '--', as in '{task} -- {extra}'"
                        ),
                    ));
                }
            }
        }
        for (option, key_name, pairs) in [
            ("--config-var", "variable", &args.config_var),
            ("--override-image", "container", &args.override_image),
        ] {
            let mut seen = HashSet::new();
            if let Some((key, _)) = pairs.iter().find(|(key, _)| !seen.insert(key)) {
                return Err(Self::command().error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("the {key_name} '{key}' was given to {option} more than once"),
                ));
            }
        }
        Ok(args)
    }

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
    fn engine_settings(
        &self,
        project_directory: PathBuf,
        dialect: Dialect,
        config_var_overrides: HashMap<String, String>,
        cache_type: CacheType,
    ) -> TaskEngineSettings {
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
            cache: Some((cache_type, project_directory)),
            // Stamped onto every resource this run creates, so it can be
            // identified later — see `ratect_core::labels`.
            ratect_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            dialect,
            config_var_overrides,
        }
    }
}

/// The CLI-side `--output` value set — clap's `ValueEnum` derive gives the
/// lowercase names (`fancy`/`simple`/`quiet`/`all`) and the standard
/// invalid-value error listing them, as Batect's own enum-converted option
/// lists them — though in `clap`'s format and words, not Batect's. Mirrors
/// [`ratect_core::ui::OutputStyle`] rather than deriving on it directly,
/// keeping `clap` a `ratect`-only dependency (see AGENTS.md's CLI-vs-core
/// dependency split).
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

/// The CLI-side `--cache-type` value set. Mirrors [`CacheType`]
/// rather than deriving on it directly, same reasoning as `OutputStyleArg`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum CacheTypeArg {
    Volume,
    Directory,
}

impl From<CacheTypeArg> for CacheType {
    fn from(arg: CacheTypeArg) -> Self {
        match arg {
            CacheTypeArg::Volume => CacheType::Volume,
            CacheTypeArg::Directory => CacheType::Directory,
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
/// an image) and `--tag-image` (VALUE is a tag), which `value_name` names.
/// Split at the first `=`, a recorded divergence; Batect splits at the
/// last. Neither side may be empty, as in Batect.
fn parse_container_value_pair(
    s: &str,
    value_name: &str,
) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some(("", _)) => Err("the container name before '=' is empty".to_string()),
        Some((_, "")) => Err(format!("the {value_name} after '=' is empty")),
        Some((container, value)) => Ok((container.to_string(), value.to_string())),
        None => Err(format!("expected CONTAINER=VALUE, got '{s}'")),
    }
}

/// `--override-image`'s `CONTAINER=IMAGE`. The image itself stays
/// unchecked, as in Batect (decisions/0018).
fn parse_override_image(s: &str) -> std::result::Result<(String, String), String> {
    parse_container_value_pair(s, "image")
}

/// `--tag-image`'s `CONTAINER=TAG`, with TAG checked against Docker's
/// reference grammar while the arguments are parsed — earlier than Batect,
/// which checks it when that image's build starts (decisions/0018).
/// `--override-image` stays unchecked, as in Batect.
fn parse_tag_image(s: &str) -> std::result::Result<(String, String), String> {
    let (container, tag) = parse_container_value_pair(s, "tag")?;
    ratect_core::image_reference::check_tag(&tag)?;
    Ok((container, tag))
}

/// `log_file`, when given (`--log-file`), tees the same log output into
/// that file *in addition to* stderr — matching Batect's own `--log-file`
/// content, though not its silent-by-default behavior (Ratect always logs
/// to stderr regardless; Batect's own default with no `--log-file` is a
/// `NullLogSink`, nothing anywhere). `stderr_color` is stderr's own colour
/// decision (a terminal, and no `--no-color`/`NO_COLOR`), the same one every
/// other line on stderr follows (decisions/0017; ratect#479).
fn init_tracing(log_file: Option<&Path>, stderr_color: bool) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let writer = match log_file {
        Some(path) => {
            // The one check on the value itself (decisions/0016): a log file
            // is created, so only its directory has to exist already, and be
            // one. Any other failure to open it is this command's own.
            if let Some(directory) = path
                .parent()
                .filter(|directory| !directory.as_os_str().is_empty())
            {
                if !directory.is_dir() {
                    return Err(InvalidOptionValue::new(format!(
                        "--log-file {path:?}: {directory:?} does not exist or is not a directory."
                    ))
                    .into());
                }
            }
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
        // with stderr's own output: with `--log-file`, stderr goes plain
        // too, trading its colour for a greppable file.
        .with_ansi(log_file.is_none() && stderr_color)
        .with_writer(writer)
        .init();
    Ok(())
}

/// [`Args::runs_a_task`] for arguments that didn't parse: a line is a run
/// unless one of the flags that replace a task appears where it would be
/// read as a flag — among the options, so not after the task name or `--`,
/// and not as another option's value (decisions/0016; ratect#435). `args`
/// excludes the program name.
fn is_a_run(args: &[String]) -> bool {
    let command = Args::command();
    let words =
        ratect_core::exit_code::command_line_words(args, |word| takes_value(&command, word));
    !words
        .iter()
        .take_while(|word| !matches!(word, Word::Positional(_)))
        .any(|word| {
            matches!(
                word,
                Word::Long("list-tasks" | "clean" | "clean-cache" | "upgrade") | Word::Short('T')
            )
        })
}

/// Whether `word` is one of `command`'s own options and takes a value —
/// `false` for one it doesn't recognise, as `command_line_words` requires.
fn takes_value(command: &clap::Command, word: Word<'_>) -> bool {
    command
        .get_arguments()
        .find(|arg| match word {
            Word::Long(name) => arg.get_long() == Some(name),
            Word::Short(short) => arg.get_short() == Some(short),
            Word::Positional(_) => false,
        })
        .is_some_and(|arg| arg.get_action().takes_values())
}

/// Splits `arguments` (the program name first) where Batect's option parser
/// stops: at the first argument that isn't an option or an option's value —
/// the task name — or at a `--`. The first part, the program name included,
/// is what `clap` parses; the second is everything from that argument on.
/// It reads options as `clap` and [`ratect_core::exit_code::command_line_words`]
/// do, so the leniency recorded in `docs/differences-from-batect.md` stays:
/// `-Tf x`, `-fx` and `-oquiet`.
///
/// Two values `clap` would misread are refused instead, for every option
/// that takes one: the third part is the first of them, for
/// [`Args::parse_checked`] to report unless `clap`'s own parse of the
/// options ends in `--help` or `--version`. An empty one after `=`
/// (`--use-network=`, `-f=`) is Batect's "invalid format" (ratect#449,
/// ratect#448), where `clap` would take `""` — which `--use-network ''`
/// still gives, as in Batect. And a
/// separate value starting with `-`, which `clap` refuses too, but with a tip
/// to write `-- -x`, which here would pass `-x` to the task: this error's tip
/// is the `=` form (ratect#435).
fn split_at_task(
    arguments: &[std::ffi::OsString],
) -> (
    &[std::ffi::OsString],
    &[std::ffi::OsString],
    Option<clap::Error>,
) {
    let command = Args::command();
    let mut refused = None;
    let mut refuse = |message: String| {
        refused.get_or_insert_with(|| {
            command
                .clone()
                .error(clap::error::ErrorKind::InvalidValue, message)
        });
    };
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        let argument = argument.to_string_lossy();
        if argument == "--" {
            break;
        }
        // The option `argument` ends with that takes the next argument as
        // its value, if any.
        let value_option = if let Some(long) = argument.strip_prefix("--") {
            match long.split_once('=') {
                Some((name, value)) => {
                    if value.is_empty() && takes_value(&command, Word::Long(name)) {
                        refuse(format!(
                            "'--{name}=' has nothing after '=': give a value after the '='"
                        ));
                    }
                    None
                }
                None => takes_value(&command, Word::Long(long)).then(|| format!("--{long}")),
            }
        } else if let Some(shorts) = argument
            .strip_prefix('-')
            .filter(|shorts| !shorts.is_empty())
        {
            shorts
                .char_indices()
                .find_map(|(position, short)| {
                    if !takes_value(&command, Word::Short(short)) {
                        return None;
                    }
                    let attached = &shorts[position + short.len_utf8()..];
                    if attached == "=" {
                        refuse(format!(
                            "'-{short}=' has nothing after '=': give a value after the '='"
                        ));
                    }
                    Some(attached.is_empty().then(|| format!("-{short}")))
                })
                .flatten()
        } else {
            break;
        };
        if let Some(option) = value_option {
            if let Some(value) = arguments.get(index + 1) {
                let value = value.to_string_lossy();
                if value.starts_with('-') && value != "-" {
                    refuse(format!(
                        "{option} takes a value, and the argument after it, '{value}', starts \
                         with '-': to give a value starting with '-', write it as \
                         '{option}={value}'"
                    ));
                }
                index += 1;
            }
        }
        index += 1;
    }
    let (options, rest) = arguments.split_at(index.min(arguments.len()));
    (options, rest, refused)
}

/// Parses the command line, exiting as decisions/0016 says when `clap`
/// rejects it rather than with `clap`'s own `2` on either path: `125` on a
/// run, `2` otherwise. A `--help` or `--version` `clap` reached still prints
/// and exits `0`.
fn parse_args() -> Args {
    Args::parse_checked(std::env::args_os()).unwrap_or_else(|error| {
        if !error.use_stderr() {
            error.exit();
        }
        let _ = error.print();
        let args: Vec<String> = std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        std::process::exit(ratect_core::exit_code::for_invocation_error(is_a_run(&args)).into());
    })
}

#[tokio::main]
async fn main() {
    ratect_core::panic_hook::install(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    let mut args = parse_args();
    let runs_a_task = args.runs_a_task();
    let failed = |error: &anyhow::Error| ratect_core::exit_code::for_failure(error, runs_a_task);

    // Folded in once, here, rather than at each of `args.no_color`'s own
    // call sites below — see `resolve_no_color`'s own doc comment for what
    // NO_COLOR does and why it's not narrowed to "color only".
    args.no_color =
        ratect_core::ui::resolve_no_color(args.no_color, |name| std::env::var(name).ok());
    // `CLICOLOR_FORCE`, on top of that — see `resolve_color_mode`'s own doc
    // comment for why this takes the already-resolved `no_color` rather
    // than re-deriving it, and why it's a separate value from `no_color`
    // rather than folded into it: `--output`'s own auto-selection must
    // never see anything but `Off`/not, so `select_output_style` in `run`
    // still gets `args.no_color`, never this.
    let color_mode =
        ratect_core::ui::resolve_color_mode(args.no_color, |name| std::env::var(name).ok());
    // Resolved before `run`, because the subscriber and the fatal-error
    // line below both need stderr's colour decision and both sit outside it.
    let stderr = Console::stderr(color_mode);

    if let Err(err) = init_tracing(args.log_file.as_deref(), stderr.color_enabled()) {
        stderr.error(&format!("{err:?}"));
        std::process::exit(failed(&err).into());
    }

    let exit_code = match run(args, color_mode, &stderr, sweep_git_include_cache).await {
        Ok(()) => ratect_core::exit_code::SUCCESS,
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
            //
            // The deliberate exception is a run's failure that has already
            // reached the user by another route — the task's own non-zero
            // exit (its code, the summary line and the container's own
            // output say it), a signal (the interrupt notice did), a
            // cleanup failure the engine reported as it happened — where a
            // second line would be the same event twice (decisions/0017;
            // the list is `already_reported`'s, shared with `ratect`).
            if !ratect_core::exit_code::already_reported(&err) {
                stderr.error(&format!("{err:?}"));
            }

            failed(&err)
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

/// Resolves which config-variables file to load. An explicit
/// `--config-vars-file` always wins; otherwise Batect's default of
/// `batect.local.yml` in the current directory (`dir` here, not the `-f`
/// config file's own directory) applies when it is a file. An absent one is
/// "no overrides", not an error; one that exists but isn't a file is an
/// invalid default, as Batect's `FileDefaultValueProvider` makes it — even
/// for a command that never reads it, since this is a check on the
/// default, not on reading it.
fn resolve_config_vars_file(explicit: Option<PathBuf>, dir: &Path) -> Result<Option<PathBuf>> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    let default = dir.join("batect.local.yml");
    match std::fs::metadata(&default) {
        Ok(metadata) if metadata.is_file() => Ok(Some(default)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(_) => Err(InvalidOptionValue::new(format!(
            "{default:?}, --config-vars-file's default in the current directory, is not a file."
        ))
        .into()),
        Err(error) => Err(InvalidOptionValue::new(format!(
            "{default:?}, --config-vars-file's default in the current directory, can't be \
             read: {error}."
        ))
        .into()),
    }
}

/// This binary's progress console is stdout — Batect's placement, kept
/// because a Batect user's scripts may read it (decisions/0017; `ratect`'s
/// is stderr). That one line is the whole difference between the two.
const PROGRESS: ProgressStream = ProgressStream::Stdout;

/// The CLI's real work, once `Args` is parsed, in Batect's `CommandFactory`
/// order: `--list-tasks` loads the config, prints and returns without
/// touching Docker at all; otherwise `--upgrade` is a documented no-op (see
/// its own doc comment), `--clean`/`--clean-cache` short-circuit into
/// [`clean_caches`] before any config loads, and naming a task loads the
/// config, builds the engine, and runs it. [`Args::engine_settings`] is kept
/// synchronous and separate specifically so its flag-to-settings mapping can
/// be unit-tested without a Docker daemon — see its own doc comment.
/// `color_mode` is `main`'s already-resolved `--no-color`/`NO_COLOR`/
/// `CLICOLOR_FORCE` verdict; `args.no_color` is already folded too.
/// `stderr` is `main`'s own stderr console, for the notices and warnings
/// this function prints itself — the run's own go through the event sink
/// (decisions/0017). `sweep` makes the background Git include cache sweep:
/// always [`sweep_git_include_cache`] outside this binary's tests, which pass
/// a recording fake so they can see whether and when it starts without
/// touching the real `~/.ratect/incl`.
async fn run(
    args: Args,
    color_mode: ColorMode,
    stderr: &Console,
    sweep: impl FnOnce() -> Sweep,
) -> Result<()> {
    // Before anything else reads a file, as Batect's `validate()` runs once
    // the options have parsed (ratect#436).
    ratect_core::engine::check_tag_and_override(&args.tag_image, &args.override_image)?;

    // Batect's option parser checks only that a named config-variables file
    // is a file, for every command. Its contents, like `--config-var`'s
    // names, are read only when a task runs, so nothing in either stops
    // `--list-tasks` (#355).
    let config_vars_file =
        resolve_config_vars_file(args.config_vars_file.clone(), &std::env::current_dir()?)?;
    if let Some(path) = &config_vars_file {
        if !path.is_file() {
            return Err(InvalidOptionValue::new(format!(
                "--config-vars-file {path:?} does not exist or is not a file."
            ))
            .into());
        }
    }

    let cache_type = args.cache_type(std::env::var("BATECT_CACHE_TYPE").ok().as_deref())?;
    // Like Batect's option parser, for every command, whether or not it
    // connects: a Docker directory the user named must be one, and
    // `--docker-context` can't be given beside a flag that picks a
    // connection itself (ratect#439) — checked in that order, as Batect
    // checks a path option's value before `validate()`.
    let docker_connection = args.docker_connection();
    docker_connection.check_directories()?;
    docker_connection.check_context_conflict()?;

    // `--list-tasks` wins over both, as in Batect's `CommandFactory`
    // (ratect#442), so `-T --clean` lists rather than deletes.
    if args.upgrade && !args.list_tasks {
        stderr.notice(
            "--upgrade has no effect: Ratect is a single native binary, not a self-updating \
             wrapper script like Batect. Reinstall/rebuild to get a newer version instead.",
        );
        return Ok(());
    }

    if (args.clean || !args.clean_cache.is_empty()) && !args.list_tasks {
        return clean_caches(&args, cache_type, stderr).await;
    }

    let project = load_project(&args.config_file).await?;

    // Gathered once, here, and reused for both the `--list-tasks` quiet-
    // format decision below and (inside `create_event_sink`) the real
    // logger construction — rather than each querying stdout/TERM/console
    // dimensions again on top of the other.
    let terminal = TerminalFacts::gather();
    let requested_style = args.output.map(OutputStyle::from);
    let output_style = select_output_style(requested_style, args.no_color, &terminal, PROGRESS);

    if args.list_tasks {
        let listing = match output_style {
            OutputStyle::Quiet => format_task_list_quiet(&project.config.tasks),
            _ => format_task_list(&project.config.project_name, &project.config.tasks),
        };
        println!("{listing}");
        return Ok(());
    }

    let Some(task_name) = args.task_name.as_deref() else {
        unreachable!("Args::parse_checked refuses a run that names no task");
    };
    // Unconditional, fire-and-forget — matching Batect's own
    // `GitRepositoryCacheCleanupTask`, an unconditional daemon
    // thread started on every "run a task" invocation regardless of
    // whether this particular config uses a Git include, after the
    // config loads and before Docker is reached. Never awaited: a
    // failure (or simply not finishing before the process exits —
    // see `main`'s own comment on `std::process::exit`) is
    // inherently best-effort, same as a JVM daemon thread not
    // blocking process exit either.
    let sweep = sweep();
    tokio::spawn(async {
        if let Err(e) = sweep.await {
            tracing::warn!("Failed to sweep stale Git include cache entries: {e}");
        }
    });

    let mut config_var_overrides: HashMap<String, String> = match &config_vars_file {
        Some(path) => Config::load_config_vars_file(path)?,
        None => HashMap::new(),
    };
    config_var_overrides.extend(args.config_var.iter().cloned());

    // The output-mode logger — one instance shared by the Docker
    // client (fine-grained pull/build progress) and the engine
    // (lifecycle milestones), so it sees the whole event stream in
    // order. Selection, construction, and (for an explicit fancy)
    // validation all live in `create_event_sink` — see its own docs
    // for why, and for the fancy-on-a-non-interactive-console error
    // it can return.
    let event_sink = create_event_sink(requested_style, color_mode, &terminal, PROGRESS)?;
    let settings = args.engine_settings(
        project.project_directory.clone(),
        project.dialect,
        config_var_overrides,
        cache_type,
    );
    // Constructed here rather than inside `ratect-core` — a library
    // shouldn't take over a process's signal handling on its own
    // initiative — and armed immediately, since (unlike
    // `engine_settings`, kept synchronous so the flag-mapping tests
    // can call it directly) this whole function already is async.
    // From this point Ctrl+C, `SIGTERM` or `SIGHUP` abandons the run
    // and cleans up instead of killing the process where it stands.
    let interrupt = ratect_core::interrupt::Interrupt::new();
    interrupt.listen();
    let docker = DockerClient::new(&args.docker_connection(), |message| {
        event_sink.post(TaskEvent::Warning { message })
    })
    .await?
    .with_event_sink(Arc::clone(&event_sink))
    .with_enable_buildkit(args.enable_buildkit);
    let engine = TaskEngine::new(project, docker, event_sink, interrupt).with_settings(settings)?;
    engine.run_task(task_name, &args.additional_args).await?;

    Ok(())
}

/// The background work [`run`] starts on a task run and never awaits.
type Sweep = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// The real sweep: removes every `~/.ratect/incl` entry unused for 30 days.
fn sweep_git_include_cache() -> Sweep {
    Box::pin(async { GitIncludeCache::new().cleanup_stale().await })
}

/// `--clean`/`--clean-cache`: removes this project's own cache
/// volumes/directories and exits, without running anything. Never needs
/// `--config-file` to actually exist — matching Batect, whose own
/// `CleanupCachesCommand` doesn't read the task config either. Under
/// `--cache-type=directory` it never connects to Docker at all, so it works
/// with the daemon unreachable; Batect checks connectivity first under either
/// cache type, only so it can detect Windows containers (which force its
/// cache type to `directory`), which Ratect doesn't support. A divergence recorded
/// in `docs/differences-from-batect.md`.
///
/// `--clean-cache <NAME>` (repeatable) restricts this to the named caches;
/// plain `--clean` with no `--clean-cache` cleans every one of this
/// project's own caches — matching Batect's `CleanupCachesCommand` in how
/// the two combine: the explicit `cleanCaches` list (if non-empty) always
/// wins over `--clean`'s "everything" default, regardless of whether
/// `--clean` was also given. Which command runs at all is [`run`]'s.
async fn clean_caches(args: &Args, cache_type: CacheType, stderr: &Console) -> Result<()> {
    let base_path = ratect_core::config::base_path_for(&args.config_file);
    let project_directory = ratect_core::config::project_directory_path(base_path)?;
    let only: HashSet<String> = args.clean_cache.iter().cloned().collect();

    let docker = match cache_type {
        CacheType::Volume => {
            println!("Checking for cache volumes...");
            Some(
                DockerClient::new(&args.docker_connection(), |message| {
                    stderr.warning(&message)
                })
                .await?,
            )
        }
        CacheType::Directory => {
            println!(
                "Checking for cache directories in '{}'...",
                ratect_core::cache::cache_directory(&project_directory).display()
            );
            None
        }
    };

    // `ratect-compat` never has a `--scope` of its own — Batect has no
    // shared cache — so this always asks `CacheStore` for `Project` and
    // never resolves a shared root at all.
    let store =
        ratect_core::cache::CacheStore::new(cache_type, docker.as_ref(), project_directory, None)?;
    let found = store
        .list(Some(ratect_core::config::CacheScope::Project))
        .await?;
    let removed = store
        .remove(&found, &only)
        .await?
        .expect("a Project-scoped selection can never trigger CacheStore's shared-cache refusal");

    for cache in &removed {
        println!("{}", deleting_line(cache_type, &cache.storage));
    }
    println!("{}", done_line(cache_type, removed.len()));

    Ok(())
}

/// The line printed for each cache actually removed — Batect's own
/// `CleanupCachesCommand` wording, kept byte-for-byte since replacing it
/// without divergence is this binary's entire job. `storage` is the full
/// identifier Batect itself prints: a Docker volume name under
/// `CacheType::Volume`, a full host path under `CacheType::Directory` —
/// never the bare cache name a `cache` mount declares.
fn deleting_line(cache_type: CacheType, storage: &str) -> String {
    match cache_type {
        CacheType::Volume => format!("Deleting volume '{storage}'..."),
        CacheType::Directory => format!("Deleting '{storage}'..."),
    }
}

/// The summary line, singular/plural matched to what was actually removed —
/// the same Batect wording, pinned for the same reason as [`deleting_line`].
fn done_line(cache_type: CacheType, removed: usize) -> String {
    let (singular, plural) = match cache_type {
        CacheType::Volume => ("volume", "volumes"),
        CacheType::Directory => ("directory", "directories"),
    };
    format!(
        "Done! Deleted {removed} {}.",
        if removed == 1 { singular } else { plural }
    )
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
