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

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "stderr:\n{}", stderr);
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
