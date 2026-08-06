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
        &ConformanceCase::new(
            "proxy-variables",
            &["the-task"],
            0,
            &[
                "http_proxy: some-http-proxy",
                "https_proxy: some-https-proxy",
                "ftp_proxy: some-ftp-proxy",
                // Batect appends the container name to no_proxy at runtime.
                "no_proxy: bypass-proxy,build-env",
            ],
        )
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
        &ConformanceCase::new(
            "non-standard-name",
            &["--list-tasks"],
            0,
            &["- task-1", "- task-2", "- task-3"],
        )
        .config_file("another-name.yml"),
    );
}

/// `non-standard-name` (run): the same non-default file name, running one
/// of its tasks. Proves task execution honours `-f another-name.yml`.
#[test]
#[ignore]
fn non_standard_name_run() {
    run_case(
        &ConformanceCase::new(
            "non-standard-name",
            &["task-1"],
            123,
            &["This is some output from task 1"],
        )
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
        &ConformanceCase::new(
            "task-with-unhealthy-dependency",
            &["--no-color", "the-task"],
            0,
            &["This is some normal output", "This is some error output"],
        )
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

/// The combined stdout+stderr of a run, for the two bespoke cases below that
/// don't fit [`run_case`]'s single-run table shape.
fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Serialises the two tests that share the `cache-mount` project directory.
///
/// They use different cache *types*, so they never contend for the cache
/// itself — but both read and write `.batect/caches/key`, the file naming
/// this project's cache volumes. One test resetting that while the other is
/// mid-run silently reassigns the second run's volume, which shows up as
/// "the cache did not persist" rather than as a race. `cargo test` runs a
/// binary's tests on several threads, so nothing else prevents it.
static CACHE_MOUNT_PROJECT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Removes every Docker volume for `cache-mount`'s named cache, whatever the
/// project key that produced it (the full name is
/// `batect-cache-<key>-<config name>`). Key-independent on purpose: it must
/// clear a volume a crashed earlier run left behind just as well as the one
/// this test creates, so the first run below reliably starts empty.
fn remove_cache_mount_volumes() {
    // The name this project configures, which itself begins "batect-cache-"
    // — an oddity of Batect's own fixture, and the reason the argument here
    // is the *configured* name rather than a bare suffix.
    remove_cache_volumes_named("batect-cache-mount-journey-test-cache");
}

/// Removes every cache volume for one configured cache name, whatever
/// project key produced it — see [`remove_cache_mount_volumes`] for why the
/// key is deliberately not matched on.
///
/// Matches on the configured name alone. A volume is
/// `batect-cache-<key>-<configured name>`, so prefixing the match with
/// `batect-cache-` would put the key on the wrong side of it and match
/// nothing at all — which is exactly what a previous version did, silently,
/// because the only symptom is a leaked volume on the *second* run.
fn remove_cache_volumes_named(cache_name: &str) {
    let suffix = cache_name;
    let listed = Command::new("docker")
        .args(["volume", "ls", "-q"])
        .output()
        .expect("failed to list docker volumes");
    for name in String::from_utf8_lossy(&listed.stdout).lines() {
        if name.ends_with(suffix) {
            let _ = Command::new("docker")
                .args(["volume", "rm", "-f", name])
                .output();
        }
    }
}

/// The container user/group names `run_as_current_user` should map onto —
/// the host's own, which is the entire point of the feature. Batect reads
/// these from the JVM and `id -gn`; the same two shell-outs here, so the
/// expectation is derived from the host rather than hard-coded.
fn host_user_and_group() -> (String, String) {
    let run = |args: &[&str]| {
        let out = Command::new("id")
            .args(args)
            .output()
            .expect("failed to run id");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    (run(&["-un"]), run(&["-gn"]))
}

/// The project-local directory standing in for Batect's own build tree —
/// see the note in these projects' `batect.yml`.
///
/// Removed, not emptied: **`ratect-compat` creating it is part of what is
/// under test.** `run_as_current_user` pre-creates a bind mount's host
/// directory (`ensure_host_volume_directories_exist`) precisely so Docker
/// does not create it root-owned, and leaving one behind here would let
/// that regress unnoticed. Removing it also means a previous run's
/// `created-file` can't satisfy the ownership assertion.
///
/// Left in place afterwards so a failure can be inspected; `.gitignore`
/// covers it and the next run clears it.
///
/// A removal failure is fatal rather than ignored — the assertions below
/// are only meaningful against a directory this run produced, so carrying
/// on with a stale one would turn a real failure into a false pass.
fn reset_output_dir(project: &str) -> PathBuf {
    let dir = project_dir(project).join("output");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("failed to clear {}: {e}", dir.display()),
    }
    dir
}

/// Asserts the shared shape of the two `run_as_current_user` projects that
/// write into `/output`: the container ran as the host's own user, saw its
/// `home_directory` owned by that user, and the file it created on the host
/// belongs to the invoking user rather than root.
///
/// The ownership half is the one that matters and the one a
/// pure-output assertion would miss — a container running as root still
/// prints plausible-looking lines, but leaves a root-owned file the
/// developer then cannot delete. That is the bug `run_as_current_user`
/// exists to prevent, so it is checked on the host, not in the container.
fn run_as_current_user_case(project: &str) {
    let (user, group) = host_user_and_group();
    let output_dir = reset_output_dir(project);

    let output = ratect_command()
        .current_dir(project_dir(project))
        .args(["-f", "batect.yml", "the-task"])
        .output()
        .expect("failed to run ratect-compat");
    let combined = combined_output(&output);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the task should succeed — output:\n{combined}"
    );
    for expected in [
        format!("User: {user}"),
        format!("Group: {group}"),
        "Home directory: /home/special-place".to_string(),
        "Home directory exists".to_string(),
        format!("Home directory owned by user: {user}"),
        format!("Home directory owned by group: {group}"),
        "/etc/hosts exists".to_string(),
    ] {
        assert!(
            combined.contains(&expected),
            "expected {expected:?} in output:\n{combined}"
        );
    }

    let created = output_dir.join("created-file");
    let metadata = std::fs::metadata(&created)
        .unwrap_or_else(|e| panic!("{} should exist: {e}", created.display()));
    assert_eq!(
        std::os::unix::fs::MetadataExt::uid(&metadata),
        nix_getuid(),
        "the file the container created should belong to the invoking user, \
         not root — output:\n{combined}"
    );
}

/// The invoking user's real uid. Deliberately not `nix`, which
/// `ratect-compat` doesn't depend on — `id -u` is already being shelled out
/// to above.
fn nix_getuid() -> u32 {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .expect("failed to run id -u");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("id -u should print a number")
}

/// `run-as-current-user`: the task container runs as the host user, with a
/// `home_directory` Ratect creates and chowns inside the container.
#[test]
#[ignore]
fn run_as_current_user() {
    run_as_current_user_case("run-as-current-user");
}

/// `run-as-current-user-with-mount`: the same, but the project directory is
/// bind-mounted *inside* the container's home directory, so Ratect's home
/// setup has to coexist with a mount rather than owning the whole tree.
#[test]
#[ignore]
fn run_as_current_user_with_mount() {
    run_as_current_user_case("run-as-current-user-with-mount");
}

/// `run-as-current-user-with-cache`: three `type: cache` mounts under
/// `run_as_current_user` — one outside the home directory, one directly
/// inside it, and one nested a level deeper. All three must be writable by
/// the mapped user, which is the case a cache created root-owned would fail.
#[test]
#[ignore]
fn run_as_current_user_with_cache() {
    const CACHES: [&str; 3] = [
        "run-as-current-user-with-cache-test-normal-cache",
        "run-as-current-user-with-cache-test-nested-cache",
        "run-as-current-user-with-cache-test-deeply-nested-cache",
    ];
    // Before *and* after: the point of the test is what happens to a
    // freshly created volume, so a leftover from a previous run would let
    // it pass against an already-chowned one. Cleaning up afterwards is
    // what keeps that true on the next run.
    let reset = || {
        CACHES
            .iter()
            .for_each(|name| remove_cache_volumes_named(name))
    };
    reset();

    let output = ratect_command()
        .current_dir(project_dir("run-as-current-user-with-cache"))
        .args(["-f", "batect.yml", "the-task"])
        .output()
        .expect("failed to run ratect-compat");
    let combined = combined_output(&output);

    // Reset before asserting, so a failure doesn't strand the volumes.
    reset();

    assert_eq!(
        output.status.code(),
        Some(0),
        "the task should succeed — output:\n{combined}"
    );
    // One `\n`-joined block, not six `contains` calls: "/cache exists" is a
    // substring of "/home/special-place/cache exists", so asserting the
    // shorter lines individually passes even if `/cache` was never mounted
    // — the script's `else` branch only echoes, so nothing fails the run.
    // Batect joins them for the same reason.
    let expected = [
        "/cache exists",
        "/cache/created-file created",
        "/home/special-place/cache exists",
        "/home/special-place/cache/created-file created",
        "/home/special-place/subdir/cache exists",
        "/home/special-place/subdir/cache/created-file created",
    ]
    .join("\n");
    assert!(
        combined.contains(&expected),
        "expected this block in output:\n{expected}\n\nactual output:\n{combined}"
    );
}

/// `config-with-include`: a root file that pulls its containers and tasks
/// from a local `include`, and whose included file mounts a script using
/// `<{batect.project_directory}`. Proves the built-in resolves to the *root*
/// project directory rather than the included file's own — the one thing an
/// include can silently get wrong while still loading cleanly.
#[test]
#[ignore]
fn config_with_include() {
    run_case(&ConformanceCase::new(
        "config-with-include",
        &["the-task"],
        123,
        &["This is some output from the task"],
    ));
}

/// `git-include`: the root file's only content is a `type: git` include of
/// Batect's own `hello-world-bundle` at a pinned tag, so the task being run
/// exists solely inside the clone.
///
/// **Needs network access on first run**, unlike every other case here: it
/// clones from GitHub. Afterwards it is served from `~/.ratect/incl`, which
/// this deliberately does not clear — reusing the cache is the behaviour a
/// second run should exercise, and clearing it would make every run pay for
/// a clone.
#[test]
#[ignore]
fn git_include() {
    run_case(&ConformanceCase::new(
        "git-include",
        &["say-hello"],
        0,
        &["Hello world!"],
    ));
}

/// `cache-mount`: a `type: cache` volume mounted at `/cache`, whose contents
/// persist across runs. Batect's own journey test runs the task twice and
/// asserts the cache is empty the first time and populated the second. This
/// is the volume-cache variant (`--cache-type=volume`, the default); see
/// [`cache_mount_persists_across_runs_as_a_directory`] for the other.
///
/// A bespoke test rather than a [`ConformanceCase`]: it runs the task twice
/// with different expected output per run, and resets the shared cache
/// volume around itself.
#[test]
#[ignore]
fn cache_mount_persists_across_runs() {
    let _guard = CACHE_MOUNT_PROJECT
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    remove_cache_mount_volumes();

    let run = || {
        ratect_command()
            .current_dir(project_dir("cache-mount"))
            .args(["-f", "batect.yml", "--cache-type=volume", "the-task"])
            .output()
            .expect("failed to run ratect-compat")
    };

    let first = run();
    let first_output = combined_output(&first);
    let second = run();
    let second_output = combined_output(&second);

    // Reset before asserting, so a failure doesn't strand the volume.
    remove_cache_mount_volumes();

    assert_eq!(
        first.status.code(),
        Some(0),
        "first run should succeed — output:\n{first_output}"
    );
    assert!(
        first_output.contains("File created in task does not exist, creating it"),
        "first run should see an empty cache — output:\n{first_output}"
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "second run should succeed — output:\n{second_output}"
    );
    assert!(
        second_output.contains("File created in task exists"),
        "second run should reuse the cache the first populated — output:\n{second_output}"
    );
}

/// The same project under `--cache-type=directory`: the cache is a host
/// directory under `.batect/caches/` rather than a Docker volume, so
/// persistence has to hold through an entirely different mechanism.
///
/// Worth having as well as the volume variant rather than instead of it —
/// the two differ in who creates the directory and who owns it, which is
/// exactly where `run_as_current_user` interacts badly with caches (see
/// [`run_as_current_user_with_cache`]).
///
/// Removes the host directory rather than a volume, so it shares no cleanup
/// with the volume case.
#[test]
#[ignore]
fn cache_mount_persists_across_runs_as_a_directory() {
    let _guard = CACHE_MOUNT_PROJECT
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // Only this cache's own directory. `.batect/caches/` also holds `key`,
    // which names every *volume* this project ever creates — removing that
    // would hand the volume test a new name mid-run and orphan the old one.
    let cache_directory = project_dir("cache-mount")
        .join(".batect/caches")
        .join("batect-cache-mount-journey-test-cache");
    let reset = || {
        let _ = std::fs::remove_dir_all(&cache_directory);
    };
    reset();

    let run = || {
        ratect_command()
            .current_dir(project_dir("cache-mount"))
            .args(["-f", "batect.yml", "--cache-type=directory", "the-task"])
            .output()
            .expect("failed to run ratect-compat")
    };

    let first = run();
    let first_output = combined_output(&first);
    let second = run();
    let second_output = combined_output(&second);

    reset();

    assert_eq!(
        first.status.code(),
        Some(0),
        "first run should succeed — output:\n{first_output}"
    );
    assert!(
        first_output.contains("File created in task does not exist, creating it"),
        "first run should see an empty cache — output:\n{first_output}"
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "second run should succeed — output:\n{second_output}"
    );
    assert!(
        second_output.contains("File created in task exists"),
        "second run should reuse the directory the first populated — \
         output:\n{second_output}"
    );
}

/// `simple-task-using-dockerfile` under `--tag-image`: Batect's
/// `TagImageJourneyTest` builds the image and additionally tags it under a
/// name of the caller's choosing, then checks that tag exists. Proves
/// `--tag-image` applies the extra tag to the built image, on top of the
/// task still running with its own exit code and output.
///
/// A bespoke test rather than a [`ConformanceCase`]: it inspects Docker for
/// the applied tag and cleans it up afterwards.
#[test]
#[ignore]
fn simple_task_using_dockerfile_tag_image() {
    let tag = "ratect-conformance-tag-image-test:latest";
    let remove_tag = || {
        let _ = Command::new("docker")
            .args(["image", "rm", "-f", tag])
            .output();
    };
    remove_tag();

    let output = ratect_command()
        .current_dir(project_dir("simple-task-using-dockerfile"))
        .args([
            "-f",
            "batect.yml",
            "--tag-image",
            &format!("build-env={tag}"),
            "the-task",
        ])
        .output()
        .expect("failed to run ratect-compat");
    let combined = combined_output(&output);

    let tagged = Command::new("docker")
        .args(["image", "inspect", tag])
        .output()
        .expect("failed to run docker image inspect");

    // Cleaned up before asserting, so a failure doesn't strand the tag.
    remove_tag();

    assert_eq!(
        output.status.code(),
        Some(123),
        "the task's own exit code should still propagate — output:\n{combined}"
    );
    assert!(
        combined.contains("This is some output from the task"),
        "the task should still run — output:\n{combined}"
    );
    assert!(
        tagged.status.success(),
        "the built image should have been tagged {tag}"
    );
}
