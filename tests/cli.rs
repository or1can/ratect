use std::path::{Path, PathBuf};
use std::process::Command;

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect"))
}

fn sample_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("batect.yml")
}

fn sidecar_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sidecar.yml")
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

fn no_image_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-image.yml")
}

fn environment_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/environment.yml")
}

fn config_vars_file_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config-vars.yml")
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

fn project_directory_declared_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project-directory-declared.yml")
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
        stderr.contains("unknown field") && stderr.contains("working_directory"),
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
/// The second arg deliberately contains a space and no shell metacharacters
/// are involved — proves args arrive as literal positional parameters (via
/// `sh -c`'s `$0 $1 $2...` mechanism) rather than being concatenated into the
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
