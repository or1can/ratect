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
use ratect_core::docker::{DockerClient, DockerConnectionOptions};
use ratect_core::engine::TaskEngine;
use ratect_core::ui::{create_event_sink, select_output_style, OutputStyle};
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

/// Parses a `CONTAINER=VALUE` pair, shared by `--override-image` (VALUE is
/// an image) and `--tag-image` (VALUE is a tag).
fn parse_container_value_pair(s: &str) -> std::result::Result<(String, String), String> {
    match s.split_once('=') {
        Some((container, value)) => Ok((container.to_string(), value.to_string())),
        None => Err(format!("expected CONTAINER=VALUE, got '{s}'")),
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

    match args.task_name {
        Some(task_name) => {
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
            if args.skip_prerequisites {
                engine = engine.without_prerequisites();
            }
            if !args.override_image.is_empty() {
                engine = engine.with_image_overrides(args.override_image.into_iter().collect())?;
            }
            if !args.tag_image.is_empty() {
                let mut image_tags: HashMap<String, std::collections::HashSet<String>> =
                    HashMap::new();
                for (container, tag) in args.tag_image {
                    image_tags.entry(container).or_default().insert(tag);
                }
                engine = engine.with_image_tags(image_tags);
            }
            if args.no_cleanup || args.no_cleanup_after_failure {
                engine = engine.without_cleanup_after_failure();
            }
            if args.no_cleanup || args.no_cleanup_after_success {
                engine = engine.without_cleanup_after_success();
            }
            if let Some(max_parallelism) = args.max_parallelism {
                engine = engine.with_max_parallelism(max_parallelism as usize);
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
