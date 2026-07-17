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

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect"))
}

fn sample_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke.yml")
}

fn sidecar_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sidecar.yml")
}

fn task_groups_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/task-groups.yml")
}

fn exit_code_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exit-code.yml")
}

fn additional_args_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/additional-args.yml")
}

fn unsupported_key_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unsupported-key.yml")
}

fn unhealthy_dependency_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unhealthy-dependency.yml")
}

fn no_image_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-image.yml")
}

fn environment_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/environment.yml")
}

fn config_vars_file_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config-vars.yml")
}

fn working_directory_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/working-directory.yml")
}

fn entrypoint_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/entrypoint.yml")
}

fn capabilities_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capabilities.yml")
}

fn privileged_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/privileged.yml")
}

fn shm_size_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shm-size.yml")
}

fn devices_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/devices.yml")
}

fn enable_init_process_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/enable-init-process.yml")
}

fn project_directory_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project-directory.yml")
}

fn build_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build.yml")
}

fn build_with_dockerignore_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-with-dockerignore.yml")
}

fn build_customization_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-customization.yml")
}

fn build_secrets_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-secrets.yml")
}

fn build_ssh_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-ssh.yml")
}

fn build_failure_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-failure.yml")
}

fn build_failure_buildkit_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-failure-buildkit.yml")
}

fn build_buildkit_default_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-buildkit-default.yml")
}

fn project_directory_declared_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project-directory-declared.yml")
}

fn interactive_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interactive.yml")
}

fn additional_hostnames_and_hosts_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/additional-hostnames-and-hosts.yml")
}

fn ports_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ports.yml")
}

fn proxy_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proxy.yml")
}

fn include_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/include.yml")
}

/// Polls `127.0.0.1:<port>` until a TCP connection succeeds or `timeout`
/// elapses. Just proves the port is reachable — no HTTP semantics needed,
/// a bare TCP connect is already proof `ports` actually published it.
fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
fn list_tasks_lists_sample_tasks() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(sample_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Tasks in ratect-test:"));
    for task in [
        "shared-prereq",
        "prereq-task",
        "list-volume-task",
        "test-task",
    ] {
        assert!(
            stdout.contains(task),
            "expected task '{}' in output:\n{}",
            task,
            stdout
        );
    }
}

#[test]
fn list_tasks_groups_by_group_and_shows_descriptions() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(task_groups_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "Tasks in ratect-task-groups-test:\n\
         \n\
         compilation:\n\
         - build: Builds the app\n\
         \n\
         verification:\n\
         - lint\n\
         - test: Runs the test suite\n\
         \n\
         Ungrouped tasks:\n\
         - clean",
        "stdout:\n{}",
        stdout
    );
}

#[test]
fn missing_config_file_reports_error() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg("/nonexistent/batect.yml")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "stderr:\n{}", stderr);
}

#[test]
fn missing_config_file_reports_error_when_running_a_task() {
    let output = ratect_command()
        .arg("-f")
        .arg("/nonexistent/batect.yml")
        .arg("some-task")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "stderr:\n{}", stderr);
}

#[test]
fn unsupported_config_key_reports_error() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(unsupported_key_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown field") && stderr.contains("log_driver"),
        "stderr:\n{}",
        stderr
    );
}

#[test]
fn declaring_batect_project_directory_in_config_variables_reports_error() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(project_directory_declared_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("batect.project_directory") && stderr.contains("built-in"),
        "stderr:\n{}",
        stderr
    );
}

#[test]
fn overriding_batect_project_directory_via_cli_reports_error() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(sample_config_path())
        .arg("--config-var")
        .arg("batect.project_directory=/hijacked")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("batect.project_directory"),
        "stderr:\n{}",
        stderr
    );
}

#[test]
fn container_without_image_or_build_directory_reports_error() {
    let output = ratect_command()
        .arg("-f")
        .arg(no_image_config_path())
        .arg("test-task")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Container 'build-env' has neither 'image' nor 'build_directory' set"),
        "stderr:\n{}",
        stderr
    );
}

#[test]
fn no_task_name_warns() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No task name provided"),
        "stderr:\n{}",
        stderr
    );
}

#[test]
fn unknown_task_fails_with_nonzero_exit_and_logged_error() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("does-not-exist-task")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Task 'does-not-exist-task' not found"),
        "stderr:\n{}",
        stderr
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn test_task_runs_end_to_end_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("test-task")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("I should only run once").count(),
        1,
        "shared prerequisite should only execute once:\n{}",
        stdout
    );
    assert!(stdout.contains("I am a prerequisite"));
    assert!(stdout.contains("Hello from ratect!"));
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves a task with only `prerequisites` and no `run` of its own still runs
/// its prerequisites end to end, then stops cleanly (exit 0) — no container
/// of the task's own to run.
#[test]
#[ignore]
fn task_with_only_prerequisites_and_no_run_runs_its_prerequisites_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("prerequisites-only-task")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("I should only run once"));
}

/// Requires a running Docker daemon with network access to pull `redis:7-alpine`
/// and `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// This is the only way to prove real cross-container name resolution actually
/// works end to end — unit tests only prove the right bollard calls were made.
/// Covers both sibling dependencies (database, cache) and a nested one
/// (metrics, only reachable via database) sharing one network with `app`.
#[test]
#[ignore]
fn sidecars_are_reachable_by_name_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(sidecar_config_path())
        .arg("ping-sidecars")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("0% packet loss").count(),
        3,
        "expected a successful ping of all three dependency containers (two \
         siblings plus one nested) by name:\n{}",
        stdout
    );
}

/// Requires a running Docker daemon with network access to pull `redis:7-alpine`
/// and `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves a task-level `dependencies` entry (`queue`, not in `app`'s own
/// container-level `dependencies`) is actually started and reachable by
/// name — distinct from the container-level `dependencies` sidecars proven
/// reachable in `sidecars_are_reachable_by_name_via_docker`.
#[test]
#[ignore]
fn task_level_dependency_is_reachable_by_name_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(sidecar_config_path())
        .arg("ping-task-level-sidecar")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0% packet loss"),
        "expected a successful ping of the task-level dependency 'queue' by \
         name:\n{}",
        stdout
    );
}

/// Requires a running Docker daemon with network access to pull
/// `redis:7-alpine` and `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves a task's `customise` entry for a dependency container
/// (`configurable`) actually reaches the real container: `configurable`'s
/// `setup_commands` entry only exits `0` once its environment/
/// `working_directory` are the *customised* values, not its own base ones —
/// so the task succeeding is only possible if the customisation was applied
/// before that setup command ran.
#[test]
#[ignore]
fn customise_overrides_a_dependencys_config_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(sidecar_config_path())
        .arg("customise-sidecar")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("customise-sidecar-ok"));
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves the whole dependency readiness gate with real Docker health
/// checks and execs, via a chain where each step can only succeed if the
/// previous one really completed first:
/// - the dependency's command creates `/tmp/now-healthy` two seconds after
///   it starts (`tests/fixtures/readiness/Dockerfile`), and its configured
///   health check probes for that file — so if Ratect didn't actually wait
///   for the healthy verdict, the setup command (which `test`s for the same
///   file) would run immediately and fail;
/// - the setup command drops a marker onto a volume shared with the task's
///   own container — so if setup commands didn't complete before dependents
///   start, the task's `test` for that marker would fail.
///
/// Writes its own temporary config (same pattern as the user-mapping test
/// below) since the shared volume needs a scratch host directory.
#[test]
#[ignore]
fn dependency_health_check_and_setup_commands_gate_the_task_via_docker() {
    let test_id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let scratch_dir = std::env::temp_dir().join(format!("ratect-readiness-test-{test_id}"));
    std::fs::create_dir_all(&scratch_dir).unwrap();
    let config_path = std::env::temp_dir().join(format!("ratect-readiness-test-{test_id}.yml"));

    let config = format!(
        r#"
project_name: ratect-readiness-test
containers:
  database:
    build_directory: {build_directory}
    health_check:
      command: test -f /tmp/now-healthy
      interval: 500ms
      retries: 30
    setup_commands:
      - command: test -f /tmp/now-healthy && touch /scratch/setup-ran
    volumes:
      - {scratch}:/scratch
  app:
    image: alpine:3.18.2
    volumes:
      - {scratch}:/scratch
    dependencies:
      - database
tasks:
  check:
    run:
      container: app
      command: sh -c "test -f /scratch/setup-ran && echo SETUP-RAN-BEFORE-TASK"
"#,
        build_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/readiness")
            .display(),
        scratch = scratch_dir.display()
    );
    std::fs::write(&config_path, &config).expect("failed to write temp config");

    let cleanup = || {
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&config_path);
    };

    let output = ratect_command()
        .arg("-f")
        .arg(&config_path)
        .arg("check")
        .output()
        .expect("failed to run ratect");

    if !output.status.success() {
        cleanup();
        panic!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("SETUP-RAN-BEFORE-TASK"),
        "the task should only run once the dependency was healthy and its \
         setup command had completed:\n{stdout}"
    );

    cleanup();
}

/// Requires a running Docker daemon with network access to pull
/// `redis:7-alpine` and `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// The failure half of the readiness gate: a dependency whose health check
/// can never pass must fail the task itself — with Docker's real
/// "unhealthy" verdict surfaced, naming the container — and the task's own
/// command must never run.
#[test]
#[ignore]
fn unhealthy_dependency_fails_the_task_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(unhealthy_dependency_config_path())
        .arg("check")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "the task must fail when a dependency never becomes healthy"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'database' did not become healthy"),
        "the error should name the unhealthy container:\n{stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("the task should never run"),
        "the task's own command must never have run:\n{stdout}"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn successful_container_command_exits_zero() {
    let output = ratect_command()
        .arg("-f")
        .arg(exit_code_config_path())
        .arg("succeeds")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves the exact container exit code becomes ratect's own process exit
/// code, not just "some" non-zero code.
#[test]
#[ignore]
fn failing_container_command_propagates_exact_exit_code() {
    let output = ratect_command()
        .arg("-f")
        .arg(exit_code_config_path())
        .arg("fails")
        .output()
        .expect("failed to run ratect");

    assert_eq!(
        output.status.code(),
        Some(42),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Matches Batect's documented behavior: "If a prerequisite task finishes
/// with a non-zero exit code, then neither this task nor any other
/// prerequisites will be run."
#[test]
#[ignore]
fn failing_prerequisite_stops_the_chain() {
    let output = ratect_command()
        .arg("-f")
        .arg(exit_code_config_path())
        .arg("stops-prerequisite-chain")
        .output()
        .expect("failed to run ratect");

    assert_eq!(output.status.code(), Some(42));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("should never print this"),
        "the task depending on the failed prerequisite must not have run:\n{}",
        stdout
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// The second arg deliberately contains a space — proves args arrive as
/// literal argv appended after the tokenized `command`, matching Batect's
/// own `ADDITIONAL_ARGS` mechanism, rather than being concatenated into the
/// command string and re-parsed as shell syntax.
#[test]
#[ignore]
fn additional_args_are_forwarded_to_the_task_command() {
    let output = ratect_command()
        .arg("-f")
        .arg(additional_args_config_path())
        .arg("echo-args")
        .arg("--")
        .arg("foo")
        .arg("bar baz")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "args: foo bar baz");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves environment values actually reach the real container (not just
/// that the right bollard calls were made): a container-level
/// `${HOST_VAR:-fallback}` expression (its default, since `HOST_VAR` is
/// never set) and a task `run.environment` entry referencing a config
/// variable. Passes both `--config-vars-file` (`env_name: from-file`) and
/// `--config-var env_name=from-cli` together to prove the real precedence
/// too, not just the isolated merge logic a unit test would cover.
#[test]
#[ignore]
fn environment_and_config_variables_reach_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(environment_config_path())
        .arg("--config-vars-file")
        .arg(config_vars_file_path())
        .arg("--config-var")
        .arg("env_name=from-cli")
        .arg("print-env")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "GREETING=hello-fallback ENV_NAME=from-cli",
        "--config-var should take precedence over --config-vars-file"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Same fixture as above, but with only `--config-vars-file` (no
/// `--config-var`), proving a config variable's value can come from the file
/// alone, not just as an override on top of a CLI-supplied one.
#[test]
#[ignore]
fn config_vars_file_alone_provides_a_declared_variables_value() {
    let output = ratect_command()
        .arg("-f")
        .arg(environment_config_path())
        .arg("--config-vars-file")
        .arg(config_vars_file_path())
        .arg("print-env")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "GREETING=hello-fallback ENV_NAME=from-file");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves a container's `working_directory` reaches the real container (via
/// `pwd`, not just the right bollard call), overriding the image's own
/// default `WORKDIR`.
#[test]
#[ignore]
fn container_working_directory_reaches_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(working_directory_config_path())
        .arg("print-pwd")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "/tmp");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Same fixture as above, but proves `run.working_directory` overrides the
/// container's own `working_directory` in the real container.
#[test]
#[ignore]
fn task_run_working_directory_overrides_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(working_directory_config_path())
        .arg("print-pwd-overridden")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "/var");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves the classic Batect `entrypoint: /bin/sh -c` + `command: 'some
/// command'` idiom actually works against a real container — Docker execs
/// `Entrypoint ++ Cmd`, so this must produce exactly `/bin/sh -c "echo
/// hello-from-sh-c"`, with neither Ratect's `command` tokenizer nor its
/// `entrypoint` tokenizer inserting an extra shell layer.
#[test]
#[ignore]
fn container_entrypoint_combines_correctly_with_command_on_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(entrypoint_config_path())
        .arg("classic-idiom")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "hello-from-sh-c");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `run.entrypoint` overrides the container's own `entrypoint` on the
/// real container: if the override didn't take effect, the container's own
/// `/bin/sh -c` entrypoint would try (and fail) to run a shell command
/// literally named `override-worked`, instead of `/bin/echo` printing it.
#[test]
#[ignore]
fn task_run_entrypoint_overrides_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(entrypoint_config_path())
        .arg("run-entrypoint-override")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "override-worked");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Contrast case for `capabilities_to_drop` below: without dropping
/// anything, `chown` succeeds (Docker grants CHOWN by default).
#[test]
#[ignore]
fn chown_succeeds_without_a_dropped_capability() {
    let output = ratect_command()
        .arg("-f")
        .arg(capabilities_config_path())
        .arg("chown-succeeds")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "chown-worked");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `capabilities_to_drop` reaches the real container: dropping CHOWN
/// makes `chown` fail even as root, unlike the contrast case above.
#[test]
#[ignore]
fn capabilities_to_drop_removes_chown_on_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(capabilities_config_path())
        .arg("chown-fails-without-capability")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "chown should fail once CHOWN is dropped:\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `privileged` reaches the real container: a `privileged: true`
/// container's own effective-capabilities bitmask (`/proc/self/status`'s
/// `CapEff` — read-only, no actual privileged operation exercised) must be
/// numerically larger than a normal container's, since privileged mode
/// grants Docker's full capability set.
#[test]
#[ignore]
fn privileged_grants_a_larger_capability_set_on_the_real_container() {
    let cap_eff = |task: &str| {
        let output = ratect_command()
            .arg("-f")
            .arg(privileged_config_path())
            .arg(task)
            .output()
            .unwrap_or_else(|e| panic!("failed to run ratect: {e}"));
        assert!(
            output.status.success(),
            "stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let hex = stdout
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("unexpected CapEff line: {stdout}"));
        u64::from_str_radix(hex, 16)
            .unwrap_or_else(|e| panic!("failed to parse CapEff '{hex}': {e}"))
    };

    let normal = cap_eff("show-caps-normal");
    let privileged = cap_eff("show-caps-privileged");

    assert!(
        privileged > normal,
        "privileged CapEff ({privileged:#x}) should exceed normal CapEff ({normal:#x})"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `shm_size` reaches the real container: `/dev/shm` is a tmpfs
/// mount, so `128m` must make its actual `df` size exactly 131072 1K-blocks
/// (128 * 1024).
#[test]
#[ignore]
fn shm_size_reaches_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(shm_size_config_path())
        .arg("print-shm-size")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let blocks: u64 = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("unexpected df output: {stdout}"))
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse df output '{stdout}': {e}"));

    assert_eq!(blocks, 128 * 1024, "df output:\n{stdout}");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `devices` reaches the real container: remapping the host's
/// `/dev/null` to `/dev/xnull` must make `/dev/xnull` exist as a character
/// device inside the container — no image ships with it by default.
#[test]
#[ignore]
fn devices_reaches_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(devices_config_path())
        .arg("check-device")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "device-mapped");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `enable_init_process` reaches the real container: without it, the
/// container's own command runs directly as PID 1 (`/proc/1/comm` reports
/// the actual command image — alpine's `sh -c` execs its single final
/// command in place rather than forking, so this is `cat`, not `sh`); with
/// it, Docker's own init process wraps it as PID 1 instead, so
/// `/proc/1/comm` must report something else entirely.
#[test]
#[ignore]
fn enable_init_process_wraps_pid_1_on_the_real_container() {
    let pid1_comm = |task: &str| {
        let output = ratect_command()
            .arg("-f")
            .arg(enable_init_process_config_path())
            .arg(task)
            .output()
            .unwrap_or_else(|e| panic!("failed to run ratect: {e}"));
        assert!(
            output.status.success(),
            "stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };

    let normal = pid1_comm("show-pid1-normal");
    let with_init = pid1_comm("show-pid1-with-init");

    assert_eq!(
        normal, "cat",
        "without enable_init_process, cat should be PID 1"
    );
    assert_ne!(
        with_init, "cat",
        "with enable_init_process, Docker's own init should be PID 1 instead"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `batect.project_directory` resolves to the real, absolute
/// directory containing the config file - in both a bare-form `environment`
/// reference and a braced-form volume host path - without being declared
/// under `config_variables`.
#[test]
#[ignore]
fn batect_project_directory_resolves_to_the_configs_own_directory() {
    let output = ratect_command()
        .arg("-f")
        .arg(project_directory_config_path())
        .arg("print-project-dir")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    assert!(
        stdout.contains(&format!("PROJECT_DIR={}", expected_dir.display())),
        "stdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("project-directory.yml"),
        "expected the volume mount (at /mnt) to list this fixture's own \
         directory contents, proving it mounted the right path:\n{}",
        stdout
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Covers local file `include` end to end (see
/// `tests/fixtures/include.yml`/`tests/fixtures/include/extra.yml`): the
/// task run here (`print-include-dir`, declared in the root file) references
/// `build-env`, a container declared only in the included file - proving
/// containers/tasks actually merge across files, not just parse
/// independently - and that container's relative volume path resolves
/// against *its own* file's directory (`tests/fixtures/include`), not the
/// root config's directory (`tests/fixtures`).
#[test]
#[ignore]
fn include_merges_containers_and_tasks_across_files() {
    let output = ratect_command()
        .arg("-f")
        .arg(include_config_path())
        .arg("print-include-dir")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("extra.yml"),
        "expected the volume mount (at /mnt) to list tests/fixtures/include's own \
         directory contents, proving its relative volume path resolved against \
         that included file's own directory, not the root config's:\n{}",
        stdout
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2` and build `tests/fixtures/build/Dockerfile`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `build_directory` and `build_args` both reach a real `docker
/// build`, not just that the right calls were made: the Dockerfile
/// promotes the `MESSAGE` build arg to a runtime environment variable,
/// which the task then echoes.
#[test]
#[ignore]
fn build_directory_and_build_args_reach_a_real_docker_build() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_config_path())
        .arg("print-message")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "hello-from-build-arg");
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2` and build
/// `tests/fixtures/build-customization/docker/Dockerfile.multistage`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `dockerfile` and `build_target` both reach a real `docker build`:
/// the build directory has no file literally named `Dockerfile`, so a
/// successful build proves the custom `dockerfile` path was used, and the
/// task's output differs between the multi-stage Dockerfile's two stages,
/// so seeing the first stage's output (not the second's) proves
/// `build_target` reached the build too.
#[test]
#[ignore]
fn dockerfile_and_build_target_reach_a_real_docker_build() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_customization_config_path())
        .arg("print-message")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "from-builder-stage");
}

/// Requires a running Docker daemon that supports BuildKit sessions (any
/// reasonably current Docker Engine) and network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves `build_secrets` reaches a real BuildKit build (its per-build
/// session is what serves the secret — see `build_image_via_buildkit` in
/// `ratect-core/src/docker.rs`): the Dockerfile's
/// `RUN --mount=type=secret,id=token` only sees the secret inside that one
/// instruction's mount, so the task catting the file that `RUN` copied it
/// into — and seeing this test process's own env var value, not a baked-in
/// one — proves the secret's value made the full round trip from host env
/// var through the build's session to the build.
///
/// See `build_ssh_forwards_a_real_ssh_agent_into_the_build` below for
/// `build_ssh`'s equivalent.
#[test]
#[ignore]
fn build_secrets_reach_a_real_buildkit_session_build() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_secrets_config_path())
        .arg("print-secret")
        .env("RATECT_BUILD_SECRETS_TEST_TOKEN", "hello-from-build-secret")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "hello-from-build-secret");
}

/// A dedicated throwaway `ssh-agent` process spawned for one test, plus a
/// scratch keypair loaded into it — so the `build_ssh` test below never
/// depends on (or leaks into) the developer's or CI runner's own agent.
/// Same spirit as the `portable-pty` trick the interactive-mode tests use:
/// create the real infrastructure in-process rather than assuming the host
/// provides it. The agent is killed on `Drop`, even if the test panics.
struct ScratchSshAgent {
    socket_path: String,
    pid: String,
    key_comment: String,
    key_dir: PathBuf,
}

impl ScratchSshAgent {
    fn spawn() -> Self {
        let output = Command::new("ssh-agent")
            .arg("-s")
            .output()
            .expect("failed to spawn ssh-agent");
        assert!(output.status.success(), "ssh-agent -s failed");
        let stdout = String::from_utf8_lossy(&output.stdout);

        // `ssh-agent -s` prints sh-syntax assignments:
        //   SSH_AUTH_SOCK=/path/to/agent.sock; export SSH_AUTH_SOCK;
        //   SSH_AGENT_PID=12345; export SSH_AGENT_PID;
        let extract = |name: &str| -> String {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}=")))
                .and_then(|rest| rest.split(';').next())
                .unwrap_or_else(|| panic!("no {name} in ssh-agent output: {stdout}"))
                .to_string()
        };
        let socket_path = extract("SSH_AUTH_SOCK");
        let pid = extract("SSH_AGENT_PID");

        let key_dir = std::env::temp_dir().join(format!(
            "ratect-build-ssh-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&key_dir).unwrap();
        let key_comment = "ratect-build-ssh-test-key".to_string();
        let key_path = key_dir.join("id_ed25519");
        let keygen = Command::new("ssh-keygen")
            .arg("-t")
            .arg("ed25519")
            .arg("-N")
            .arg("")
            .arg("-C")
            .arg(&key_comment)
            .arg("-f")
            .arg(&key_path)
            .output()
            .expect("failed to run ssh-keygen");
        assert!(
            keygen.status.success(),
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&keygen.stderr)
        );

        let add = Command::new("ssh-add")
            .arg(&key_path)
            .env("SSH_AUTH_SOCK", &socket_path)
            .output()
            .expect("failed to run ssh-add");
        assert!(
            add.status.success(),
            "ssh-add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );

        Self {
            socket_path,
            pid,
            key_comment,
            key_dir,
        }
    }
}

impl Drop for ScratchSshAgent {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(&self.pid).output();
        let _ = std::fs::remove_dir_all(&self.key_dir);
    }
}

/// Requires a running Docker daemon that supports BuildKit sessions,
/// network access to pull `alpine:3.18.2` (and `apk add openssh-client`
/// inside the build), and `ssh-agent`/`ssh-keygen`/`ssh-add` binaries on
/// the host (standard OpenSSH client tools — present on any Unix dev
/// machine or CI runner). Run explicitly with `cargo test -- --ignored`.
///
/// Proves `build_ssh` forwards a real ssh-agent into a real BuildKit
/// build: the test spawns its own throwaway agent with one scratch
/// key ([`ScratchSshAgent`]), points the child `ratect`'s `SSH_AUTH_SOCK`
/// at it, and asserts the Dockerfile's `RUN --mount=type=ssh ssh-add -l`
/// saw exactly that key — the full host-agent → build-session → build
/// sandbox round trip, not just that the right options were passed (that
/// part is covered by `ratect-core/src/engine.rs`'s unit tests).
///
/// The fixture's `CACHE_BUST` build arg (see `build-ssh.yml`) is what
/// makes this sound across repeated runs — without it, BuildKit's normal
/// layer caching would serve a previous run's `ssh-add -l` output, since
/// the instruction text alone never changes and `build_ssh` (unlike
/// `build_secrets`) doesn't disable the cache.
#[test]
#[ignore]
fn build_ssh_forwards_a_real_ssh_agent_into_the_build() {
    let agent = ScratchSshAgent::spawn();

    let cache_bust = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let output = ratect_command()
        .arg("-f")
        .arg(build_ssh_config_path())
        .arg("print-keys")
        .env("SSH_AUTH_SOCK", &agent.socket_path)
        .env("RATECT_BUILD_SSH_TEST_CACHE_BUST", cache_bust)
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&agent.key_comment),
        "the build's `ssh-add -l` should have listed the scratch key \
         '{}', but printed:\n{stdout}",
        agent.key_comment
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2` and build `tests/fixtures/build-with-dockerignore/Dockerfile`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `.dockerignore` semantics hold against a real `docker build`, not
/// just that `build_context_tar` constructs the right tar bytes (already
/// covered by `ratect-core/src/docker.rs`'s unit tests) — a bug in how the
/// tar is actually sent to Docker (path encoding, `bollard::body_full`
/// wiring, etc.) wouldn't be caught by an in-memory-only test. The
/// Dockerfile's own `RUN test` assertions fail the build if the context
/// doesn't match what's expected, so a successful build is the proof.
#[test]
#[ignore]
fn dockerignore_semantics_hold_against_a_real_docker_build() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_with_dockerignore_config_path())
        .arg("check")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves a real failing `docker build`'s full transcript (not just
/// Docker's one-line failure summary) reaches `ratect`'s own error output —
/// `build_output_suffix`'s unit tests already cover the string formatting in
/// isolation, but only a real Docker daemon actually exercises the
/// streaming/`error_detail` wiring in `DockerClient::build_image` that feeds
/// it. Pinned to the *classic* builder via `DOCKER_BUILDKIT=0` — with
/// BuildKit now the default, this is what keeps the classic path's failure
/// wiring covered at all; `failing_buildkit_build_output_reaches_the_error`
/// below is the BuildKit path's equivalent.
#[test]
#[ignore]
fn failing_build_output_reaches_the_error() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_failure_config_path())
        .arg("build")
        .env("DOCKER_BUILDKIT", "0")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "the build should have failed: stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("this line should reach the user"),
        "the Dockerfile's RUN output should be in the error: {stderr}"
    );
}

/// Requires a running Docker daemon that supports BuildKit sessions and
/// network access to pull `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves a failing BuildKit build with session providers in play (the
/// fixture's `build_secrets` entry) carries the failing step's own printed
/// output in `ratect`'s error — the transcript is assembled from BuildKit's
/// structured status stream rather than the classic path's plain `stream`
/// lines. This is the test 0.11.0 couldn't have: its gRPC-driver BuildKit
/// path exposed no log stream to capture.
///
/// Also asserts the transcript's step *ordering*: BuildKit's first status
/// message announces the entire build graph upfront, before anything runs,
/// in graph (reverse-topological) order — so a transcript that records
/// steps on first sight, rather than when each one starts, reads backwards
/// (`[3/3]` down to `[1/3]`). The fixture's Dockerfile has multiple steps
/// specifically so that regression is observable here.
#[test]
#[ignore]
fn failing_buildkit_build_output_reaches_the_error() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_failure_buildkit_config_path())
        .arg("build")
        .env("RATECT_BUILD_FAILURE_TEST_TOKEN", "irrelevant")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "the build should have failed: stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("this buildkit line should reach the user"),
        "the Dockerfile's RUN output should be in the error: {stderr}"
    );

    // Step names are `[stage-0 N/3]`-style; matching on the `N/3]` suffix
    // keeps this robust to the stage-name prefix. Only the two `RUN` steps
    // are asserted on — deliberately not the `FROM` step: BuildKit
    // content-addresses vertexes and shares them across *concurrent* builds,
    // so a shared step like `FROM alpine:3.18.2` can carry another build's
    // graph name entirely (e.g. `[1/7] FROM ...` borrowed from a 7-step
    // fixture building in a parallel test). The `RUN` commands are unique to
    // this Dockerfile, so their names — and relative order — are stable.
    // Both must appear in execution order: the whole-graph-upfront
    // announcement would put them in reverse order if recorded on first
    // sight instead of on start.
    let position = |needle: &str| {
        stderr
            .find(needle)
            .unwrap_or_else(|| panic!("'{needle}' should be in the error transcript: {stderr}"))
    };
    assert!(
        position("2/3]") < position("3/3]"),
        "build steps should appear in execution order in the transcript: {stderr}"
    );
    assert!(
        position("an earlier step that should appear first")
            < position("this buildkit line should reach the user"),
        "step output should appear in execution order in the transcript: {stderr}"
    );
}

/// Requires a running Docker daemon that advertises BuildKit as its default
/// builder (any modern daemon — its `/_ping` response's `Builder-Version`
/// header) and network access to pull `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves the *default* builder is the daemon-advertised one (BuildKit),
/// matching Batect: the fixture's Dockerfile uses heredoc `RUN <<EOF`
/// syntax the classic builder cannot parse, and declares no
/// `build_secrets`/`build_ssh` — so this succeeding means a plain,
/// no-BuildKit-features build genuinely ran under BuildKit by default.
/// `DOCKER_BUILDKIT` is explicitly cleared so an ambient value on the host
/// can't mask the default-selection logic under test.
#[test]
#[ignore]
fn default_builder_is_the_daemon_advertised_buildkit() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_buildkit_default_config_path())
        .arg("print-proof")
        .env_remove("DOCKER_BUILDKIT")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "built with buildkit heredoc support");
}

/// Requires a running Docker daemon and network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves `DOCKER_BUILDKIT=0` genuinely selects the classic builder (not
/// just a different code path that still lands on BuildKit): the same
/// BuildKit-only heredoc Dockerfile that succeeds by default must *fail*
/// under the override, since the classic builder can't parse it.
#[test]
#[ignore]
fn docker_buildkit_env_zero_genuinely_selects_the_classic_builder() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_buildkit_default_config_path())
        .arg("print-proof")
        .env("DOCKER_BUILDKIT", "0")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "a heredoc Dockerfile should not build on the classic builder: stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Requires a running Docker daemon and network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves the classic build path still works when forced via
/// `DOCKER_BUILDKIT=0` — with BuildKit now the default on any modern
/// daemon, this override is what keeps the classic path exercised in CI
/// at all, rather than only ever running against a legacy daemon nobody
/// tests on.
#[test]
#[ignore]
fn classic_builder_still_works_when_forced_via_docker_buildkit_env() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_config_path())
        .arg("print-message")
        .env("DOCKER_BUILDKIT", "0")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "hello-from-build-arg");
}

/// Requires a running Docker daemon. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves `build_secrets`/`build_ssh` fail with a clear error — rather
/// than silently building without the secret — when the classic builder is
/// forced, since only BuildKit has a session to serve them over.
#[test]
#[ignore]
fn build_secrets_error_clearly_when_the_classic_builder_is_forced() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_secrets_config_path())
        .arg("print-secret")
        .env("RATECT_BUILD_SECRETS_TEST_TOKEN", "irrelevant")
        .env("DOCKER_BUILDKIT", "0")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "the build should have been refused: stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires BuildKit"),
        "the error should name the BuildKit requirement: {stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`, and the ability to allocate a local pseudo-terminal —
/// `portable-pty` emulates one in-process, so this works on regular
/// Linux/macOS CI runners and locally, same as every other `--ignored` test
/// here; no real terminal is required. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves the actual interactive attach path end-to-end — `attach_container`,
/// the raw-mode guard, and both I/O pumps together — not just that
/// `should_use_tty`/the eligibility policy compute the right bool (already
/// covered by unit tests, which can't exercise any of this without a real
/// terminal). `ratect` is spawned with its stdin/stdout/stderr wired to a pty
/// pair's slave side, so `IsTerminal` genuinely returns true and it takes the
/// real interactive branch; a scripted `echo <marker>` is then written to the
/// pty's master side, and the resulting output is checked for the marker
/// having round-tripped through stdin -> container -> stdout.
#[test]
#[ignore]
fn interactive_session_forwards_stdin_and_stdout() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect"));
    cmd.arg("-f");
    cmd.arg(interactive_config_path());
    cmd.arg("shell");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("failed to spawn ratect in the pty");
    // Drop our side of the slave now that the child has its own — otherwise
    // the master's reader never sees EOF, since the pty only closes once
    // every writer to it (including ours) is gone.
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("failed to clone pty reader");
    let mut writer = pair
        .master
        .take_writer()
        .expect("failed to take pty writer");

    // Reads in a background thread since `Read::read` blocks; the main
    // thread polls the accumulated output with a bounded timeout instead of
    // blocking indefinitely if something hangs.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Safe to write immediately, before the shell has necessarily started
    // reading: the pty's own kernel-level buffer holds unread input until
    // something reads it, so this doesn't race the container's startup.
    let marker = "ratect-interactive-test-marker";
    writeln!(writer, "echo {marker}").expect("failed to write to pty");
    writeln!(writer, "exit").expect("failed to write to pty");

    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !String::from_utf8_lossy(&output).contains(marker) && Instant::now() < deadline {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            output.extend_from_slice(&chunk);
        }
    }

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains(marker),
        "expected the echoed marker to round-trip through stdin -> container -> stdout: {output_str:?}"
    );

    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });
    let status = wait_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("ratect did not exit after the interactive session ended")
        .expect("failed to wait for ratect");

    assert!(
        status.success(),
        "ratect should exit successfully once the shell session ends: {status:?}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`, and the ability to allocate a local pseudo-terminal —
/// see `interactive_session_forwards_stdin_and_stdout` above for why
/// `portable-pty` makes this work in headless CI too. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves an interactive task that exits the instant it starts doesn't warn
/// about failing to resize the container's TTY: the attach-time size sync
/// races such a container's own exit, and losing that race (the daemon
/// answers 409 "is not running", or 404 once cleanup has already removed
/// the container) is benign — `resize_tty` must classify it as such rather
/// than surfacing a warning on an otherwise clean run. The race doesn't
/// trigger on every run, so a pass here doesn't *alone* prove the
/// classification — but the fixed code can never emit the warning for
/// those two status codes, while the pre-fix code warned whenever the race
/// was lost.
#[test]
#[ignore]
fn instantly_exiting_interactive_task_does_not_warn_about_tty_resize() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect"));
    cmd.arg("-f");
    cmd.arg(interactive_config_path());
    cmd.arg("instant");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("failed to spawn ratect in the pty");
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("failed to clone pty reader");

    // Drain everything the child writes (stdout and stderr both arrive via
    // the pty) until EOF, in a background thread — the child exits on its
    // own, no input needed.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });
    let status = wait_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("ratect did not exit")
        .expect("failed to wait for ratect");
    assert!(status.success(), "the task should succeed: {status:?}");

    let mut output = Vec::new();
    while let Ok(chunk) = rx.recv_timeout(Duration::from_millis(500)) {
        output.extend_from_slice(&chunk);
    }
    let output_str = String::from_utf8_lossy(&output);
    assert!(
        !output_str.contains("Failed to resize container TTY"),
        "an instantly-exiting task should not warn about TTY resizing: {output_str:?}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`, and the ability to allocate a local pseudo-terminal —
/// see `interactive_session_forwards_stdin_and_stdout` above for why
/// `portable-pty` makes this work in headless CI too. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves the container's TTY is kept in sync with the local terminal for
/// the *whole* session, not just once at attach time: resizes the pty's
/// master side mid-session (`MasterPty::resize`, which delivers a real
/// `SIGWINCH` to `ratect`, the pty's slave-side foreground process), and
/// checks the container's own shell actually sees the new geometry via
/// `stty size` — a precise, end-to-end assertion of the full round trip
/// (local resize -> `SIGWINCH` -> `resize_tty` -> Docker's
/// `resize_container_tty` -> the container's own pty).
#[test]
#[ignore]
fn interactive_session_forwards_live_terminal_resizes() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect"));
    cmd.arg("-f");
    cmd.arg(interactive_config_path());
    cmd.arg("shell");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("failed to spawn ratect in the pty");
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("failed to clone pty reader");
    let mut writer = pair
        .master
        .take_writer()
        .expect("failed to take pty writer");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut output = Vec::new();

    // Confirm the shell starts out reporting the pty's initial size before
    // touching resize at all — otherwise a later "40 120" match wouldn't
    // prove anything actually changed.
    writeln!(writer, "stty size").expect("failed to write to pty");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !String::from_utf8_lossy(&output).contains("24 80") && Instant::now() < deadline {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            output.extend_from_slice(&chunk);
        }
    }
    let initial = String::from_utf8_lossy(&output).to_string();
    assert!(
        initial.contains("24 80"),
        "expected the shell to initially report the pty's opened size (24 rows, 80 cols): {initial:?}"
    );

    pair.master
        .resize(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to resize pty");

    // Retries `stty size` on a short interval rather than writing it once —
    // the resize -> SIGWINCH -> Docker API round trip isn't instantaneous,
    // so this polls (re-issuing the command each time) until the
    // container-side shell actually reports the new geometry, or the
    // bounded timeout gives up.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !String::from_utf8_lossy(&output).contains("40 120") && Instant::now() < deadline {
        writeln!(writer, "stty size").expect("failed to write to pty");
        let poll_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < poll_deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                output.extend_from_slice(&chunk);
            }
        }
    }
    let resized = String::from_utf8_lossy(&output).to_string();
    assert!(
        resized.contains("40 120"),
        "expected the container's shell to report the resized pty's new size (40 rows, \
         120 cols) after a live SIGWINCH-triggered resize: {resized:?}"
    );

    writeln!(writer, "exit").expect("failed to write to pty");

    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });
    let status = wait_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("ratect did not exit after the interactive session ended")
        .expect("failed to wait for ratect");

    assert!(
        status.success(),
        "ratect should exit successfully once the shell session ends: {status:?}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves stdin forwarding is decoupled from TTY allocation: `ratect` is
/// spawned with both stdin *and* stdout wired to plain OS pipes — neither
/// end is a terminal at all, unlike the `portable-pty`-based test above, so
/// `should_use_tty` is false and no real Docker TTY gets allocated — yet the
/// invoked task is still the top-level one, so `interactive` (eligibility)
/// is still `true` and stdin should still reach the container per
/// `run_container_forwarding_stdin`. A scripted `echo <marker>` is written
/// to the piped stdin, and the piped stdout is checked for the marker
/// having round-tripped through stdin -> container -> stdout, exactly like
/// the TTY-based test, just without a TTY anywhere in the chain.
#[test]
#[ignore]
fn piped_stdin_reaches_a_non_tty_task_container() {
    use std::process::Stdio;

    let mut child = ratect_command()
        .arg("-f")
        .arg(interactive_config_path())
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");

    let mut stdin = child.stdin.take().expect("child should have piped stdin");
    let mut stdout = child.stdout.take().expect("child should have piped stdout");

    // Reads in a background thread since `Read::read` blocks; the main
    // thread polls the accumulated output with a bounded timeout instead of
    // blocking indefinitely if something hangs — same pattern as the
    // pty-based test above.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let marker = "ratect-piped-stdin-test-marker";
    writeln!(stdin, "echo {marker}").expect("failed to write to piped stdin");
    writeln!(stdin, "exit").expect("failed to write to piped stdin");
    drop(stdin); // let the shell see EOF once it's done reading commands

    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !String::from_utf8_lossy(&output).contains(marker) && Instant::now() < deadline {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            output.extend_from_slice(&chunk);
        }
    }

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains(marker),
        "expected the echoed marker to round-trip through piped stdin -> container -> \
         piped stdout, even without a TTY: {output_str:?}"
    );

    let status = child.wait().expect("failed to wait for ratect");
    assert!(
        status.success(),
        "ratect should exit successfully once the shell session ends: {status:?}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`, and runs against the real host user (this doesn't need a
/// TTY, unlike the interactive test above — `run_as_current_user` and
/// interactive mode are independent features). Run explicitly with
/// `cargo test -- --ignored`.
///
/// Writes its own temporary config (rather than a static checked-in
/// fixture) pointing a volume at a temp scratch host directory that doesn't
/// exist yet, so this also exercises the host-directory-pre-creation half
/// of the feature, not just the container-side uid/gid mapping. Proves the
/// container actually runs as the host's real uid/gid (compared against
/// this test process's own, via `id -u`/`id -g` — no need for a new test
/// dependency), and that a file the container writes to the mounted volume
/// comes back owned by the current host user on disk, not root — the actual
/// practical point of the feature, not just that the right calls were made.
#[test]
#[ignore]
fn run_as_current_user_maps_the_container_onto_the_host_user() {
    let test_id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let scratch_dir = std::env::temp_dir().join(format!("ratect-user-mapping-test-{test_id}"));
    if scratch_dir.exists() {
        std::fs::remove_dir_all(&scratch_dir).unwrap();
    }
    let config_path = std::env::temp_dir().join(format!("ratect-user-mapping-test-{test_id}.yml"));

    let config = format!(
        r#"
project_name: ratect-user-mapping-test
containers:
  app:
    image: alpine:3.18.2
    volumes:
      - {volume}:/output
    run_as_current_user:
      enabled: true
      home_directory: /home/container-user
tasks:
  check:
    run:
      container: app
      command: sh -c "id -u && id -g && touch /output/marker"
"#,
        volume = scratch_dir.display()
    );
    std::fs::write(&config_path, &config).expect("failed to write temp config");

    let cleanup = || {
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&config_path);
    };

    let output = ratect_command()
        .arg("-f")
        .arg(&config_path)
        .arg("check")
        .output()
        .expect("failed to run ratect");

    if !output.status.success() {
        cleanup();
        panic!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let host_uid = String::from_utf8(
        Command::new("id")
            .arg("-u")
            .output()
            .expect("failed to run id -u")
            .stdout,
    )
    .unwrap();
    let host_gid = String::from_utf8(
        Command::new("id")
            .arg("-g")
            .output()
            .expect("failed to run id -g")
            .stdout,
    )
    .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let container_uid = lines.next().unwrap_or_default();
    let container_gid = lines.next().unwrap_or_default();

    assert_eq!(
        container_uid.trim(),
        host_uid.trim(),
        "the container should run as the host's own uid: stdout:\n{stdout}"
    );
    assert_eq!(
        container_gid.trim(),
        host_gid.trim(),
        "the container should run as the host's own gid: stdout:\n{stdout}"
    );

    let marker = scratch_dir.join("marker");
    assert!(
        marker.exists(),
        "the container should have written a marker file into the mounted volume"
    );

    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(&marker).expect("failed to stat marker file");
    assert_eq!(
        metadata.uid().to_string(),
        host_uid.trim(),
        "a file the container wrote to the mounted volume should be host-user-owned, not root"
    );

    cleanup();
}

/// Requires a running Docker daemon. Run explicitly with `cargo test -- --ignored`.
///
/// Pre-creates a real Docker network via the `docker` CLI, runs a task with
/// `--use-network` pointed at it, and proves both halves of the behavior:
/// the run succeeds (so the container really did join it), and the network
/// still exists afterward — Ratect didn't create it, so it must not remove
/// it either, unlike the per-task networks it creates by default.
#[test]
#[ignore]
fn use_network_reuses_an_existing_docker_network() {
    let network_name = format!(
        "ratect-use-network-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let create = Command::new("docker")
        .args(["network", "create", &network_name])
        .output()
        .expect("failed to run docker network create");
    assert!(
        create.status.success(),
        "failed to create test network: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let cleanup = || {
        let _ = Command::new("docker")
            .args(["network", "rm", &network_name])
            .output();
    };

    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("--use-network")
        .arg(&network_name)
        .arg("shared-prereq")
        .output()
        .expect("failed to run ratect");

    if !output.status.success() {
        cleanup();
        panic!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let inspect = Command::new("docker")
        .args(["network", "inspect", &network_name])
        .output()
        .expect("failed to run docker network inspect");
    assert!(
        inspect.status.success(),
        "the existing network should still exist after the run, not be removed by ratect"
    );

    cleanup();
}

/// Requires a running Docker daemon (to distinguish "network doesn't exist"
/// from a connection failure). Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn use_network_errors_clearly_for_a_nonexistent_network() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("--use-network")
        .arg("ratect-network-that-does-not-exist")
        .arg("shared-prereq")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "ratect should fail when --use-network points at a network that doesn't exist"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ratect-network-that-does-not-exist"),
        "the error should name the missing network: {stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `redis:7-alpine`/`alpine:3.18.2`. Run explicitly with `cargo test --
/// --ignored`.
///
/// Covers three things at once (all closely related — see
/// `tests/fixtures/additional-hostnames-and-hosts.yml`): a dependency's
/// `additional_hostnames` makes it reachable under an extra alias beyond its
/// container name, a container's own `additional_hosts` adds a real
/// `/etc/hosts` entry, and every container's Docker hostname is set to its
/// own container name (not Docker's default random short container ID).
#[test]
#[ignore]
fn additional_hostnames_and_hosts_are_applied() {
    let output = ratect_command()
        .arg("-f")
        .arg(additional_hostnames_and_hosts_config_path())
        .arg("check-network-options")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("0% packet loss"),
        "the dependency should be reachable by its additional_hostnames alias: {stdout}"
    );
    assert!(
        stdout.contains("10.0.0.9"),
        "additional_hosts should have added the extra /etc/hosts entry: {stdout}"
    );
    assert!(
        stdout.contains("app"),
        "the container's own hostname should be its container name, not a \
         random container ID: {stdout}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `nginx:alpine`/`alpine:3.18.2`, and free host ports 18080/18081. Run
/// explicitly with `cargo test -- --ignored`.
///
/// Spawns ratect (rather than waiting for it via `.output()`, which would
/// block until the task's own `sleep 5` command exits) so the host-side
/// test has a window to reach the published port while the dependency that
/// published it is still running.
#[test]
#[ignore]
fn ports_publishes_a_container_port_to_the_host() {
    let mut child = ratect_command()
        .arg("-f")
        .arg(ports_config_path())
        .arg("serve")
        .spawn()
        .expect("failed to spawn ratect");

    let reachable = wait_for_port(18080, Duration::from_secs(15));

    let status = child.wait().expect("failed to wait for ratect");

    assert!(
        reachable,
        "the published port should have been reachable while the task ran"
    );
    assert!(status.success(), "ratect should exit successfully");
}

/// Same real-Docker requirements as `ports_publishes_a_container_port_to_the_host`.
/// Uses a separate container/port (18081) from that test so the two can run
/// concurrently without colliding.
#[test]
#[ignore]
fn disable_ports_flag_suppresses_port_publishing() {
    let mut child = ratect_command()
        .arg("-f")
        .arg(ports_config_path())
        .arg("--disable-ports")
        .arg("serve-for-disabled-check")
        .spawn()
        .expect("failed to spawn ratect");

    // Short timeout: this is asserting the port is *never* reachable, not
    // waiting out a real one — the container has plenty of time to start
    // within this window if the port were (incorrectly) published.
    let reachable = wait_for_port(18081, Duration::from_secs(5));

    let status = child.wait().expect("failed to wait for ratect");

    assert!(
        !reachable,
        "the port should not be reachable when --disable-ports is set"
    );
    assert!(status.success(), "ratect should exit successfully");
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Sets `http_proxy`/`no_proxy` on ratect's own process env (not the
/// container's — that's the whole point: ratect running on the host reads
/// its own env and injects the derived values into the container).
/// `tests/fixtures/proxy.yml`'s `app` container has a dependency
/// (`database`) sharing its network, so this also proves the automatic
/// `no_proxy` container-name exemption reaches the real container, not
/// just the isolated unit-tested merge logic.
#[test]
#[ignore]
fn proxy_environment_variables_are_propagated_into_the_container() {
    let output = ratect_command()
        .env("http_proxy", "http://proxy.example.com:8080")
        .env("no_proxy", "existing.example.com")
        .arg("-f")
        .arg(proxy_config_path())
        .arg("print-proxy-vars")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("HTTP_PROXY=http://proxy.example.com:8080"),
        "the host's http_proxy should reach the container: {stdout}"
    );
    assert!(
        stdout.contains("existing.example.com"),
        "the host's own no_proxy value should be preserved: {stdout}"
    );
    assert!(
        stdout.contains("app") && stdout.contains("database"),
        "both containers sharing this task's network should be auto-exempted \
         from proxying: {stdout}"
    );
}

/// Same real-Docker requirements as
/// `proxy_environment_variables_are_propagated_into_the_container`.
#[test]
#[ignore]
fn no_proxy_vars_flag_disables_propagation() {
    let output = ratect_command()
        .env("http_proxy", "http://proxy.example.com:8080")
        .arg("-f")
        .arg(proxy_config_path())
        .arg("--no-proxy-vars")
        .arg("print-proxy-vars")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("HTTP_PROXY=") && !stdout.contains("proxy.example.com"),
        "--no-proxy-vars should suppress propagation entirely: {stdout}"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `image_pull_policy` reaches the real container's pull decision, by
/// tagging a locally-present image under a name that exists on no registry
/// anywhere: `IfNotPresent` (the default) must succeed, since the image is
/// already local and no pull is attempted; `Always` must genuinely fail,
/// since it forces a real pull attempt against that nonexistent remote repo.
#[test]
#[ignore]
fn image_pull_policy_controls_whether_a_real_pull_is_attempted() {
    let tag = format!(
        "ratect-image-pull-policy-test-{}-{}:local",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let pull = Command::new("docker")
        .args(["pull", "alpine:3.18.2"])
        .output()
        .expect("failed to run docker pull");
    assert!(
        pull.status.success(),
        "failed to pre-pull alpine:3.18.2: {}",
        String::from_utf8_lossy(&pull.stderr)
    );

    let docker_tag = Command::new("docker")
        .args(["tag", "alpine:3.18.2", &tag])
        .output()
        .expect("failed to run docker tag");
    assert!(
        docker_tag.status.success(),
        "failed to tag test image: {}",
        String::from_utf8_lossy(&docker_tag.stderr)
    );

    let cleanup = || {
        let _ = Command::new("docker").args(["rmi", &tag]).output();
    };

    let config = format!(
        r#"
project_name: ratect-image-pull-policy-test
containers:
  if-not-present:
    image: {tag}
  always:
    image: {tag}
    image_pull_policy: Always
tasks:
  run-if-not-present:
    run:
      container: if-not-present
      command: echo ran
  run-always:
    run:
      container: always
      command: echo ran
"#,
    );
    let config_path = std::env::temp_dir().join(format!(
        "ratect-image-pull-policy-test-{}.yml",
        std::process::id()
    ));
    std::fs::write(&config_path, &config).expect("failed to write temp config");

    let if_not_present_output = ratect_command()
        .arg("-f")
        .arg(&config_path)
        .arg("run-if-not-present")
        .output()
        .expect("failed to run ratect");

    let always_output = ratect_command()
        .arg("-f")
        .arg(&config_path)
        .arg("run-always")
        .output()
        .expect("failed to run ratect");

    let _ = std::fs::remove_file(&config_path);
    cleanup();

    assert!(
        if_not_present_output.status.success(),
        "IfNotPresent should succeed without attempting a pull:\nstderr:\n{}",
        String::from_utf8_lossy(&if_not_present_output.stderr)
    );
    assert!(
        !always_output.status.success(),
        "Always should fail attempting a real pull of a tag that exists on no registry"
    );
}
