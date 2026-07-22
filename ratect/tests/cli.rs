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
