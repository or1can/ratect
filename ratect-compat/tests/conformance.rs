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

//! Runs `ratect-compat` against test projects vendored verbatim from
//! Batect's own journey-test suite, asserting the same observable
//! behaviour Batect's acceptance tests do — the strongest evidence that
//! `ratect-compat` is a drop-in replacement, since the scenarios are
//! Batect's rather than ours. See `tests/conformance/README.md` for
//! provenance, licensing, and what is (and isn't) asserted.
//!
//! These need a real Docker daemon and are `#[ignore]`d, like the rest of
//! the end-to-end suite:
//!
//! ```text
//! cargo test -p ratect-compat --test conformance -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect-compat"))
}

/// One vendored Batect project, and the behaviour running it should
/// produce — the parts that are observable regardless of Batect's exact UI
/// wording (which `ratect-compat` deliberately diverges from). This is what
/// makes a Batect journey scenario portable: assert what the container did,
/// not how Batect framed it.
///
/// Build one with [`ConformanceCase::new`] and layer on the less common
/// bits (`env`/`unset_env`) with the chainable setters, so a plain case
/// stays a single readable line.
struct ConformanceCase<'a> {
    /// Directory name under `tests/conformance/batect-journey/`. The
    /// process runs with this directory as its working directory and loads
    /// `batect.yml` from it, exactly as Batect's own harness runs each
    /// project in place — so a relative `volumes:` path or the
    /// `batect.project_directory` expression resolves the same way it does
    /// for Batect.
    project: &'a str,
    /// The command-line arguments after `-f batect.yml` — the flags and
    /// task name Batect's own journey test passes (e.g. `["the-task"]`,
    /// `["--list-tasks"]`, or `["--config-var", "X=Y", "the-task"]`).
    args: &'a [&'a str],
    /// Host environment variables to set for the run — the second `map`
    /// argument Batect's harness passes (e.g. a `MESSAGE` a task reads).
    /// Empty for most projects.
    env: &'a [(&'a str, &'a str)],
    /// Host environment variables to *remove* before the run, so a value
    /// left in the developer's or CI shell can't mask a project that
    /// deliberately relies on a variable being unset (e.g. an `${X:-default}`
    /// fallback).
    unset_env: &'a [&'a str],
    /// The exit code `ratect-compat` must return. Batect propagates the
    /// task container's own exit code, and so do we (`docker run`'s
    /// convention).
    expected_exit_code: i32,
    /// Substrings the run's combined stdout+stderr must contain — matched
    /// against both because Batect's own assertions check its combined
    /// `output`, and a container writes to whichever stream it likes.
    /// Deliberately not an exact-transcript match: the milestone/framing
    /// lines around the output differ from Batect's, so only the
    /// container's own output (and, for `--list-tasks`, the task listing)
    /// is pinned.
    expected_output_contains: &'a [&'a str],
    /// Set when `ratect-compat`'s behaviour diverges from Batect's own
    /// journey assertion *on purpose* — a documented simplification, not a
    /// bug. Recording it here makes the difference an asserted fact and
    /// keeps `differences-from-batect.md` honest. `None` means "behaves
    /// exactly as Batect's own test asserts".
    divergence: Option<&'a str>,
}

impl<'a> ConformanceCase<'a> {
    /// A case with no host-environment fiddling and no divergence — the
    /// shape most journey projects take.
    fn new(
        project: &'a str,
        args: &'a [&'a str],
        expected_exit_code: i32,
        expected_output_contains: &'a [&'a str],
    ) -> Self {
        Self {
            project,
            args,
            env: &[],
            unset_env: &[],
            expected_exit_code,
            expected_output_contains,
            divergence: None,
        }
    }

    fn env(mut self, env: &'a [(&'a str, &'a str)]) -> Self {
        self.env = env;
        self
    }

    fn unset_env(mut self, unset_env: &'a [&'a str]) -> Self {
        self.unset_env = unset_env;
        self
    }
}

fn project_dir(project: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/conformance/batect-journey")
        .join(project)
}

/// Runs one case and asserts its observable behaviour.
fn run_case(case: &ConformanceCase) {
    let mut command = ratect_command();
    command
        .current_dir(project_dir(case.project))
        .arg("-f")
        .arg("batect.yml")
        .args(case.args);
    for (name, value) in case.env {
        command.env(name, value);
    }
    for name in case.unset_env {
        command.env_remove(name);
    }

    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", case.project));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Batect asserts against its combined `output`; a container writes to
    // whichever stream it chooses, so match substrings against both.
    let combined = format!("{stdout}{stderr}");
    let context = || {
        format!(
            "project {}{}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            case.project,
            case.divergence
                .map(|note| format!(" (expected divergence: {note})"))
                .unwrap_or_default(),
        )
    };

    assert_eq!(
        output.status.code(),
        Some(case.expected_exit_code),
        "exit code should match Batect's — {}",
        context()
    );
    for expected in case.expected_output_contains {
        assert!(
            combined.contains(expected),
            "output should contain {expected:?} — {}",
            context()
        );
    }
}

/// Batect's own `simple-task-using-image` journey scenario: a task that
/// prints a line and exits 123. Its Batect assertions are `output
/// shouldContain "This is some output from the task"` and `exitCode
/// shouldBe 123` — both purely behavioural, so they port unchanged. Proves
/// exact exit-code propagation end to end, against Batect's own project.
#[test]
#[ignore]
fn simple_task_using_image() {
    run_case(&ConformanceCase::new(
        "simple-task-using-image",
        &["the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `simple-task-using-dockerfile`: the same task, but its container is
/// *built* from a one-line `build-env/Dockerfile` rather than pulled.
/// Proves the image-build path reaches the same exit-code/output behaviour.
#[test]
#[ignore]
fn simple_task_using_dockerfile() {
    run_case(&ConformanceCase::new(
        "simple-task-using-dockerfile",
        &["the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `container-with-custom-dockerfile`: the container is built from a
/// non-default Dockerfile name (`dockerfile: my-special-dockerfile`).
/// Proves the `dockerfile:` override is honoured end to end.
#[test]
#[ignore]
fn container_with_custom_dockerfile() {
    run_case(&ConformanceCase::new(
        "container-with-custom-dockerfile",
        &["the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `container-with-mount`: a local `./task.sh` bind-mounted into the
/// container and run. Proves relative volume-path resolution (against the
/// project directory) and that the mounted script actually executes.
#[test]
#[ignore]
fn container_with_mount() {
    run_case(&ConformanceCase::new(
        "container-with-mount",
        &["the-task"],
        123,
        &["This is some output from the script"],
    ));
}

/// `task-with-prerequisite`: `do-stuff` declares `prerequisites: [build]`,
/// so `build` runs first and then the main task. Proves both run and both
/// their outputs appear, with the main task's own exit code propagated.
#[test]
#[ignore]
fn task_with_prerequisite() {
    run_case(&ConformanceCase::new(
        "task-with-prerequisite",
        &["do-stuff"],
        123,
        &[
            "This is some output from the build task",
            "This is some output from the main task",
        ],
    ));
}

/// `task-with-only-prerequisite`: `do-stuff` declares *only*
/// `prerequisites: [build]` and no `run` of its own. Proves the
/// prerequisite runs and the task then exits 0 (Batect's "nothing more to
/// do") rather than erroring on the absent `run`.
#[test]
#[ignore]
fn task_with_only_prerequisite() {
    run_case(&ConformanceCase::new(
        "task-with-only-prerequisite",
        &["do-stuff"],
        0,
        &["This is some output from the build task"],
    ));
}

/// `config-vars`: a config variable set three ways — from the auto-loaded
/// `batect.local.yml`, from `--config-var` on the command line, and from a
/// declared `default`. Proves all three sources resolve, in particular the
/// `batect.local.yml` auto-discovery that matches Batect's default
/// `--config-vars-file`.
#[test]
#[ignore]
fn config_vars() {
    run_case(&ConformanceCase::new(
        "config-vars",
        &[
            "--config-var",
            "FROM_COMMAND_LINE=Hello from the command line",
            "the-task",
        ],
        123,
        &[
            "Hello from the file",
            "Hello from the command line",
            "Hello from the default value",
        ],
    ));
}

/// `task-with-environment-from-host`: a task environment sourced from a
/// host variable (`MESSAGE`) plus an `${OTHER_MESSAGE:-default}` fallback.
/// Proves host-environment passthrough and default expansion; `OTHER_MESSAGE`
/// is unset so a value in the developer's shell can't mask the default.
#[test]
#[ignore]
fn task_with_environment_from_host() {
    run_case(
        &ConformanceCase::new(
            "task-with-environment-from-host",
            &["the-task"],
            123,
            &[
                "This is some output from the environment variable",
                "This is the default message",
            ],
        )
        .env(&[(
            "MESSAGE",
            "This is some output from the environment variable",
        )])
        .unset_env(&["OTHER_MESSAGE"]),
    );
}

/// `dependency-container-with-setup-command`: the task depends on a `server`
/// container whose `setup_commands` write a file the task then reads over
/// HTTP. Proves setup commands run on a dependency before the task starts.
#[test]
#[ignore]
fn dependency_container_with_setup_command() {
    run_case(&ConformanceCase::new(
        "dependency-container-with-setup-command",
        &["the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `task-container-with-setup-command`: the task's *own* container runs a
/// `setup_command` that writes a file the task then waits for and reads.
/// Proves setup commands run on the task container, whose readiness gate
/// now runs concurrently with its main command.
#[test]
#[ignore]
fn task_container_with_setup_command() {
    run_case(&ConformanceCase::new(
        "task-container-with-setup-command",
        &["the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `container-with-dependency`: the task depends on an HTTP server with a
/// `HEALTHCHECK`, then curls it. Proves the dependency is started, waited
/// on until healthy, and reachable by container name before the task runs.
#[test]
#[ignore]
fn container_with_dependency() {
    run_case(&ConformanceCase::new(
        "container-with-dependency",
        &["the-task"],
        0,
        &["Status code for request: 200"],
    ));
}

/// `many-tasks`: three tasks with descriptions, listed via `--list-tasks`.
/// Proves the listing format (`- <name>: <description>`) and that listing
/// runs no task and exits 0.
#[test]
#[ignore]
fn many_tasks_list() {
    run_case(&ConformanceCase::new(
        "many-tasks",
        &["--list-tasks"],
        0,
        &[
            "- task-1: do the first thing",
            "- task-2: do the second thing",
            "- task-3: do the third thing",
        ],
    ));
}
