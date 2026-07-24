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

/// What exit status the run must produce. Batect propagates the task
/// container's own exit code, and so do we — but a few scenarios (a
/// dependency that never becomes healthy) only pin that the run *failed*,
/// not the exact code, because the failure originates in Ratect rather than
/// a task command with a code of its own.
enum ExpectedExit {
    /// Exactly this code, as Batect's own `exitCode shouldBe N`.
    Code(i32),
    /// Any non-zero exit, as Batect's own `exitCode shouldNotBe 0`.
    NonZero,
}

/// One vendored Batect project, and the behaviour running it should
/// produce — the parts that are observable regardless of Batect's exact UI
/// wording (which `ratect-compat` deliberately diverges from). This is what
/// makes a Batect journey scenario portable: assert what the container did,
/// not how Batect framed it.
///
/// Build one with [`ConformanceCase::new`] and layer on the less common
/// bits with the chainable setters, so a plain case stays a single readable
/// line.
struct ConformanceCase<'a> {
    /// Directory name under `tests/conformance/batect-journey/`. The
    /// process runs with this directory as its working directory and loads
    /// [`config_file`](Self::config_file) from it, exactly as Batect's own
    /// harness runs each project in place — so a relative `volumes:` path or
    /// the `batect.project_directory` expression resolves the same way it
    /// does for Batect.
    project: &'a str,
    /// The configuration file to load with `-f`, relative to the project
    /// directory. Almost always `batect.yml`; a couple of projects use a
    /// non-standard name to prove `-f` honours it.
    config_file: &'a str,
    /// The command-line arguments after `-f <config_file>` — the flags and
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
    /// The exit status `ratect-compat` must return.
    expected_exit: ExpectedExit,
    /// Substrings the run's combined stdout+stderr must *all* contain —
    /// matched against both because Batect's own assertions check its
    /// combined `output`, and a container writes to whichever stream it
    /// likes. Deliberately not an exact-transcript match: the
    /// milestone/framing lines around the output differ from Batect's, so
    /// only the container's own output (and, for `--list-tasks`, the task
    /// listing) is pinned.
    expected_output_contains: &'a [&'a str],
    /// Substrings *at least one* of which must appear — Batect's own
    /// `shouldContainAnyOf`, used where the observable output legitimately
    /// varies (e.g. a log driver that may or may not let Docker read the
    /// container's output back, depending on the daemon version). Empty
    /// means "no any-of constraint".
    expected_output_any_of: &'a [&'a str],
    /// Substrings that must *not* appear — Batect's own `shouldNotContain`,
    /// used to prove something did *not* happen (e.g. a task whose
    /// dependency never became healthy must never have run its command).
    /// Empty means "no absence constraint".
    expected_output_absent: &'a [&'a str],
    /// Set when `ratect-compat`'s behaviour diverges from Batect's own
    /// journey assertion *on purpose* — a documented simplification, not a
    /// bug. Recording it here makes the difference an asserted fact and
    /// keeps `differences-from-batect.md` honest. `None` means "behaves
    /// exactly as Batect's own test asserts".
    divergence: Option<&'a str>,
}

impl<'a> ConformanceCase<'a> {
    /// A case that pins an exact exit code and a set of required output
    /// substrings — the shape most journey projects take. Layer on the rest
    /// with the setters below.
    fn new(
        project: &'a str,
        args: &'a [&'a str],
        expected_exit_code: i32,
        expected_output_contains: &'a [&'a str],
    ) -> Self {
        Self {
            project,
            config_file: "batect.yml",
            args,
            env: &[],
            unset_env: &[],
            expected_exit: ExpectedExit::Code(expected_exit_code),
            expected_output_contains,
            expected_output_any_of: &[],
            expected_output_absent: &[],
            divergence: None,
        }
    }

    fn config_file(mut self, config_file: &'a str) -> Self {
        self.config_file = config_file;
        self
    }

    fn env(mut self, env: &'a [(&'a str, &'a str)]) -> Self {
        self.env = env;
        self
    }

    fn unset_env(mut self, unset_env: &'a [&'a str]) -> Self {
        self.unset_env = unset_env;
        self
    }

    /// Require only that the run failed, not a specific code — Batect's
    /// `exitCode shouldNotBe 0`.
    fn nonzero_exit(mut self) -> Self {
        self.expected_exit = ExpectedExit::NonZero;
        self
    }

    fn any_of(mut self, expected_output_any_of: &'a [&'a str]) -> Self {
        self.expected_output_any_of = expected_output_any_of;
        self
    }

    fn absent(mut self, expected_output_absent: &'a [&'a str]) -> Self {
        self.expected_output_absent = expected_output_absent;
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
        .arg(case.config_file)
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

    match case.expected_exit {
        ExpectedExit::Code(code) => assert_eq!(
            output.status.code(),
            Some(code),
            "exit code should match Batect's — {}",
            context()
        ),
        ExpectedExit::NonZero => assert!(
            output.status.code().is_none_or(|code| code != 0),
            "run should have failed (non-zero exit), like Batect's — {}",
            context()
        ),
    }
    for expected in case.expected_output_contains {
        assert!(
            combined.contains(expected),
            "output should contain {expected:?} — {}",
            context()
        );
    }
    if !case.expected_output_any_of.is_empty() {
        assert!(
            case.expected_output_any_of
                .iter()
                .any(|expected| combined.contains(expected)),
            "output should contain at least one of {:?} — {}",
            case.expected_output_any_of,
            context()
        );
    }
    for absent in case.expected_output_absent {
        assert!(
            !combined.contains(absent),
            "output should not contain {absent:?} — {}",
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

/// `additional-arguments`: extra arguments after `--` are appended to the
/// task container's command, so `echo "…config file…"` also prints the
/// argument. Proves the trailing-argument passthrough.
#[test]
#[ignore]
fn additional_arguments() {
    run_case(&ConformanceCase::new(
        "additional-arguments",
        &[
            "the-task",
            "--",
            "This is some output from the additional arguments.",
        ],
        0,
        &["This is the output from the config file. This is some output from the additional arguments."],
    ));
}

/// `additional-hosts`: an `additional_hosts` entry adds a name to the
/// container's `/etc/hosts`, which `getent hosts` then resolves. Proves the
/// extra host entry reaches the container.
#[test]
#[ignore]
fn additional_hosts() {
    run_case(&ConformanceCase::new(
        "additional-hosts",
        &["the-task"],
        0,
        // Batect prints the getent line `1.2.3.4  additionalhost.batect.dev
        // …`; the exact column spacing is getent's, so pin the two fields
        // rather than the whitespace between them.
        &["1.2.3.4", "additionalhost.batect.dev"],
    ));
}

/// `image-override`: the container's configured image is deliberately
/// `this-image-does-not-exist`, and `--override-image` points it at a real
/// one. Proves the override replaces the configured image end to end.
#[test]
#[ignore]
fn image_override() {
    run_case(&ConformanceCase::new(
        "image-override",
        &["--override-image", "build-env=alpine:3.18.3", "the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `container-with-multiple-dependencies`: the task depends on two HTTP
/// servers and curls both, with `--max-parallelism=1` forcing them up one
/// at a time. Proves multiple dependencies and the parallelism cap.
#[test]
#[ignore]
fn container_with_multiple_dependencies() {
    run_case(&ConformanceCase::new(
        "container-with-multiple-dependencies",
        &["--max-parallelism=1", "the-task"],
        0,
        &[
            "Status code for first request: 200",
            "Status code for second request: 200",
        ],
    ));
}

/// `task-with-customisation`: the task's `customise` block overrides a
/// dependency's `working_directory` and environment. Run with `--output=all`
/// so the dependency's own output is captured. Proves the per-task
/// customisation reaches the dependency (working directory and both a new
/// and an overridden environment variable), while a variable set only on
/// the container and not customised is left untouched.
#[test]
#[ignore]
fn task_with_customisation() {
    run_case(&ConformanceCase::new(
        "task-with-customisation",
        &["--output=all", "the-task"],
        0,
        // The container's own lines; `--output=all` prefixes them with
        // `dependency | `, which the substring match tolerates.
        &[
            "Working directory is /customised",
            "Value of CONTAINER_VAR is set on container",
            "Value of OVERRIDDEN_VAR is overridden value from task",
            "Value of NEW_VAR is new value from task",
        ],
    ));
}

/// `task-with-slow-healthy-dependency`: a dependency whose health check
/// only passes after ~11s (its check interval times out once first). Proves
/// Ratect waits through a slow-to-become-healthy dependency rather than
/// giving up, then runs the task.
#[test]
#[ignore]
fn task_with_slow_healthy_dependency() {
    run_case(&ConformanceCase::new(
        "task-with-slow-healthy-dependency",
        &["the-task"],
        0,
        &["Started!"],
    ));
}

/// `proxy-variables`: proxy environment variables set on the host are
/// propagated both to the image build and to the running container, with
/// the container name appended to `no_proxy` at runtime. Proves proxy
/// propagation on both paths.
#[test]
#[ignore]
fn proxy_variables() {
    run_case(
        &ConformanceCase::new("proxy-variables", &["the-task"], 0, &[
            "http_proxy: some-http-proxy",
            "https_proxy: some-https-proxy",
            "ftp_proxy: some-ftp-proxy",
            // Batect appends the container name to no_proxy at runtime.
            "no_proxy: bypass-proxy,build-env",
        ])
        .env(&[
            ("http_proxy", "some-http-proxy"),
            ("https_proxy", "some-https-proxy"),
            ("ftp_proxy", "some-ftp-proxy"),
            ("no_proxy", "bypass-proxy"),
        ]),
    );
}

/// `non-standard-name` (listing): the configuration lives in
/// `another-name.yml`, loaded with `-f`. Proves `--list-tasks` honours a
/// non-default file name.
#[test]
#[ignore]
fn non_standard_name_list() {
    run_case(
        &ConformanceCase::new("non-standard-name", &["--list-tasks"], 0, &[
            "- task-1", "- task-2", "- task-3",
        ])
        .config_file("another-name.yml"),
    );
}

/// `non-standard-name` (run): the same non-default file name, running one
/// of its tasks. Proves task execution honours `-f another-name.yml`.
#[test]
#[ignore]
fn non_standard_name_run() {
    run_case(
        &ConformanceCase::new("non-standard-name", &["task-1"], 123, &[
            "This is some output from task 1",
        ])
        .config_file("another-name.yml"),
    );
}

/// `task-with-unhealthy-dependency`: a dependency whose health check always
/// fails, so it never becomes healthy and the task's own command must never
/// run. Proves the run fails, surfaces the failing health check's own
/// output, and does *not* execute the task command.
#[test]
#[ignore]
fn task_with_unhealthy_dependency() {
    run_case(
        &ConformanceCase::new("task-with-unhealthy-dependency", &["--no-color", "the-task"], 0, &[
            "This is some normal output",
            "This is some error output",
        ])
        .nonzero_exit()
        .absent(&["This task should never be executed!"]),
    );
}

/// `task-using-log-driver`: the container uses the `gelf` log driver. Batect
/// pins `shouldContainAnyOf` because whether Docker can read the container's
/// output back through a non-`json-file` driver is daemon-version dependent
/// — so either the task's own line appears, or Docker's "does not support
/// reading" message does. Either way the task's exit code propagates.
#[test]
#[ignore]
fn task_using_log_driver() {
    run_case(
        &ConformanceCase::new("task-using-log-driver", &["the-task"], 123, &[]).any_of(&[
            "This is some output from the task",
            "configured logging driver does not support reading",
        ]),
    );
}
