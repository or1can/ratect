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

//! End-to-end tests for the `ratect` binary's own subcommand surface. The
//! argument *parsing* is unit-tested in `src/main.rs`; these prove the
//! subcommands actually reach `ratect-core` and do the thing — which no
//! amount of `try_parse_from` can.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect"))
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tasks.yml")
}

#[test]
fn tasks_list_prints_every_task_with_its_description_and_group() {
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .args(["tasks", "list"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Build the thing") && stdout.contains("Check the thing"),
        "every task and its description should be listed:\n{stdout}"
    );
    assert!(
        stdout.contains("Development"),
        "a task's group should head its section:\n{stdout}"
    );
}

/// `-o quiet` is the machine-readable form — the reason a script would ever
/// call this rather than reading the config itself.
#[test]
fn tasks_list_in_quiet_output_is_one_task_per_line_sorted_by_name() {
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .args(["tasks", "list", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "build\tBuild the thing\ncheck\tCheck the thing\n"
    );
}

#[test]
fn a_missing_config_file_fails_rather_than_listing_nothing() {
    let output = ratect_command()
        .args(["-f", "/nonexistent/batect.yml", "tasks", "list"])
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success(), "a missing config should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "the error should say the file is missing:\n{stderr}"
    );
}

#[test]
fn an_unknown_subcommand_is_rejected_with_the_usage_message() {
    let output = ratect_command()
        .arg("nonsense")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage:") && stderr.contains("ratect"),
        "an unrecognized subcommand should print usage:\n{stderr}"
    );
}

/// A scratch project directory containing `.batect/caches/<name>` for each
/// name given — the on-disk shape `--cache-type directory` acts on, which
/// makes the whole `caches` verb testable without a Docker daemon.
fn project_with_directory_caches(names: &[&str]) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "ratect-caches-test-{}-{}",
        std::process::id(),
        count
    ));
    for name in names {
        std::fs::create_dir_all(directory.join(".batect/caches").join(name)).unwrap();
    }
    directory
}

fn caches_command(project: &Path) -> Command {
    let mut command = ratect_command();
    command.arg("-f").arg(project.join("batect.yml"));
    command
}

#[test]
fn caches_list_reports_this_projects_caches_by_name() {
    let project = project_with_directory_caches(&["npm-cache", "gradle-cache"]);

    let output = caches_command(&project)
        .args(["caches", "list", "--cache-type", "directory"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("gradle-cache") && stdout.contains("npm-cache"),
        "both caches should be listed:\n{stdout}"
    );

    std::fs::remove_dir_all(&project).unwrap();
}

/// The reason `list` exists at all: what it prints has to be exactly what
/// `clean` accepts back, or naming a cache is guesswork against the config.
#[test]
fn caches_list_in_quiet_output_prints_names_that_clean_accepts() {
    let project = project_with_directory_caches(&["npm-cache"]);

    let listed = caches_command(&project)
        .args(["caches", "list", "--cache-type", "directory", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "npm-cache\n");

    let cleaned = caches_command(&project)
        .args(["caches", "clean", "npm-cache", "--cache-type", "directory"])
        .output()
        .expect("failed to run ratect");
    assert!(cleaned.status.success());
    assert!(
        !project.join(".batect/caches/npm-cache").exists(),
        "the named cache should have been removed"
    );

    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn caches_clean_without_names_removes_every_cache() {
    let project = project_with_directory_caches(&["npm-cache", "gradle-cache"]);

    let output = caches_command(&project)
        .args(["caches", "clean", "--cache-type", "directory"])
        .output()
        .expect("failed to run ratect");

    assert!(output.status.success());
    assert!(!project.join(".batect/caches/npm-cache").exists());
    assert!(!project.join(".batect/caches/gradle-cache").exists());

    std::fs::remove_dir_all(&project).unwrap();
}

/// Clearing a cache is most useful exactly when the project is in a bad
/// state, so `caches` deliberately never reads the configuration file — it
/// works on a project whose config is broken, or not there at all.
#[test]
fn caches_works_without_a_configuration_file() {
    let project = project_with_directory_caches(&["npm-cache"]);
    assert!(!project.join("batect.yml").exists());

    let output = caches_command(&project)
        .args(["caches", "clean", "--cache-type", "directory"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!project.join(".batect/caches/npm-cache").exists());

    std::fs::remove_dir_all(&project).unwrap();
}

/// A typo'd name would otherwise be indistinguishable from success.
#[test]
fn cleaning_a_cache_that_does_not_exist_says_so() {
    let project = project_with_directory_caches(&["npm-cache"]);

    let output = caches_command(&project)
        .args([
            "caches",
            "clean",
            "no-such-cache",
            "--cache-type",
            "directory",
        ])
        .output()
        .expect("failed to run ratect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no-such-cache"),
        "the warning should name the cache that didn't exist:\n{stderr}"
    );
    assert!(
        project.join(".batect/caches/npm-cache").is_dir(),
        "an unrelated cache should be left alone"
    );

    std::fs::remove_dir_all(&project).unwrap();
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The whole point of the skeleton: `run` really does reach the engine and
/// execute a container, not just parse. Also proves `--` forwarding lands
/// as the command's own positional arguments.
#[test]
#[ignore]
fn run_executes_a_task_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .args(["run", "build"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("built"),
        "the task's own command output should reach stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The default `--cache-type volume` path end to end, which is the half no
/// daemon-free test can reach: run a task that writes into a `cache` mount,
/// then find that volume by its cache name and remove it. Also proves the
/// name `list` reports is the one `clean` takes back — the Docker volume
/// itself is called `batect-cache-<project key>-build-cache`, and neither
/// command should ever make anyone type that.
#[test]
#[ignore]
fn caches_list_and_clean_manage_real_docker_volumes() {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cache.yml");
    let ratect = |arguments: &[&str]| {
        let mut command = ratect_command();
        command.arg("-f").arg(&config);
        command.args(arguments);
        command.output().expect("failed to run ratect")
    };

    // Start from a known-clean slate: an earlier run of this test would
    // otherwise leave the volume behind and make the assertions vacuous.
    ratect(&["caches", "clean"]);

    let run = ratect(&["run", "warm-cache"]);
    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let listed = ratect(&["caches", "list", "-o", "quiet"]);
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "build-cache\n",
        "the cache should be listed by its config name, not its volume name"
    );

    let cleaned = ratect(&["caches", "clean", "build-cache"]);
    assert!(
        cleaned.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&cleaned.stderr)
    );

    let after = ratect(&["caches", "list", "-o", "quiet"]);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        "",
        "the cache should be gone after cleaning it"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn run_forwards_arguments_after_a_double_dash_to_the_task_command() {
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .args(["run", "check", "--", "the-argument"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("checked the-argument"),
        "the argument should reach the task's own command:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
