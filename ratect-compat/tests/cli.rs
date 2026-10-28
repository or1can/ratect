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
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect-compat"))
}

/// Extracts the task's own container output from ratect's framed stdout.
///
/// Since the 0.16.0 output-modes work, stdout also carries milestone lines
/// (see `ratect-core/src/ui/`): the container's own output sits between the
/// task container's "Running `<command>` in `<container>`..." line and the
/// blank line before "Cleaning up...". Tests asserting on exactly what the
/// container printed use this instead of raw stdout — deliberately still
/// exact-match on the extracted chunk, not weakened to `contains`, so they
/// keep proving the container printed that and nothing else.
///
/// That exact-match still holds after 0.21.0, where the task's own
/// container's readiness gate (health-check wait, then `setup_commands`)
/// started running concurrently with its command: simple mode drops that
/// container's own readiness milestones entirely rather than printing them
/// into the middle of its output (see `SimpleEventLogger::is_task_container`),
/// so nothing nondeterministic lands inside this window. Dependency
/// containers' milestones are all printed before the window opens.
///
/// `container` (TODO.md item 4, ratect#72) anchors the frame line to the
/// task's own container by name, matching `SimpleEventLogger`'s two actual
/// output shapes exactly (`ratect-core/src/ui/simple.rs`'s
/// `RunningTaskContainer` handler) rather than the old generic
/// `starts_with("Running ") && contains(" in ") && ends_with("...")` shape,
/// which had two bugs: it could match a line the container itself printed
/// (any `"Running ... in ..."`-shaped line, not just ratect's own), and it
/// never matched the command-less `"Running <container>..."` phrasing a
/// task with no `command` override (image default `CMD`) actually produces —
/// silently including the whole stdout, including every milestone, rather
/// than truncating. Naming the real container narrows the collision risk to
/// a container printing that *exact* container name in the same position
/// (a much smaller target), and the explicit bare-line branch fixes the
/// command-less case outright — see
/// `task_with_no_command_uses_the_image_default_cmd_via_docker` below.
fn task_output(stdout: &str, container: &str) -> String {
    let bare = format!("Running {container}...");
    let suffix = format!(" in {container}...");
    let lines: Vec<&str> = stdout.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| {
            *line == bare || (line.starts_with("Running ") && line.ends_with(&suffix))
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let end = lines[start..]
        .iter()
        .position(|line| *line == "Cleaning up...")
        .map(|index| start + index)
        .unwrap_or(lines.len());
    lines[start..end].join("\n").trim().to_string()
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

fn failing_setup_command_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/failing-setup-command.yml")
}

fn task_container_setup_commands_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/task-container-setup-commands.yml")
}

fn task_container_gate_failure_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/task-container-gate-failure.yml")
}

fn task_container_stopped_before_gate_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/task-container-stopped-before-gate.yml")
}

fn no_image_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-image.yml")
}

fn image_with_build_fields_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/image-with-build-fields.yml")
}

/// #324: Batect reads a config-variables file's values as strings, so an
/// unquoted number in one — named by `--config-vars-file` or the default
/// `batect.local.yml` — loads rather than failing the run. No daemon: the
/// run is pointed at a socket that doesn't exist, so it fails *after* the
/// file loads, on connecting, which is what proves the load succeeded.
#[test]
fn run_loads_unquoted_numbers_from_a_config_variables_file() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-unquoted-vars-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\nconfig_variables:\n  declared: {}\n\
         containers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  build:\n    run:\n      container: app\n",
    )
    .unwrap();
    std::fs::write(dir.join("batect.local.yml"), "declared: 1\n").unwrap();
    std::fs::write(dir.join("named.yml"), "declared: 3.10\n").unwrap();
    let missing_socket = format!("unix://{}", dir.join("no-daemon.sock").display());

    for args in [
        &["build"][..],
        &["--config-vars-file", "named.yml", "build"],
    ] {
        let output = ratect_command()
            .current_dir(&dir)
            .args(["--docker-host", &missing_socket, "-o", "quiet"])
            .args(args)
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}: {stderr}");
        assert!(
            !stderr.contains("Failed to parse") && stderr.contains("no-daemon.sock"),
            "{args:?}: {stderr}"
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// #468: a daemon nothing answers for is reported as unreachable, naming the
/// host tried, where it came from and the likely causes — not as a failed
/// API version negotiation, which never got as far as starting. No daemon:
/// nothing listens on port 1.
#[test]
fn run_reports_an_unreachable_daemon_naming_the_host_tried() {
    let output = ratect_command()
        .args(["-f", sample_config_path().to_str().unwrap()])
        .args(["--docker-host", "tcp://127.0.0.1:1", "test-task"])
        .output()
        .expect("failed to run ratect");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    let headline = stderr.lines().next().unwrap_or_default();
    assert!(
        headline.contains("Couldn't reach the Docker daemon")
            && headline.contains("'tcp://127.0.0.1:1'")
            && headline.contains("--docker-host")
            && headline.contains("Docker is running")
            && headline.contains("host or context"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Failed to negotiate the Docker API version"),
        "{stderr}"
    );
    assert!(stderr.contains("Connection refused"), "{stderr}");
}

/// #376: Batect reads a task's `description` and `group` as the text written,
/// so unquoted numbers in them load and are listed exactly as written —
/// `12.6000`, not `12.6`.
#[test]
fn list_tasks_shows_an_unquoted_number_as_written() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-unquoted-description-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\n\
         containers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  build:\n    description: 12.6000\n    group: 1.0\n\
         \x20   run:\n      container: app\n",
    )
    .unwrap();

    let output = ratect_command()
        .current_dir(&dir)
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout.trim_end(),
        "Tasks in demo:\n\n1.0:\n- build: 12.6000",
        "stdout:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Runs `ratect-compat` with `args` and returns its stdout, failing the test
/// on a non-zero exit.
fn help_stdout(command: &mut Command, args: &[&str]) -> String {
    let output = command.args(args).output().expect("failed to run ratect");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

/// The entry `--help` prints for `option`, if any: its name line, with or
/// without a short form, and any description lines under it, up to the next
/// entry, blank line or heading.
fn help_entry(help: &str, option: &str) -> Option<String> {
    let mut lines = help.lines().skip_while(|line| {
        !(line.trim_start().starts_with('-')
            && line.split_whitespace().take(2).any(|word| word == option))
    });
    let name = lines.next()?;
    Some(
        std::iter::once(name)
            .chain(
                lines.take_while(|line| {
                    line.starts_with("  ") && !line.trim_start().starts_with('-')
                }),
            )
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// #464, #469: an option declared hidden is left out of `--help` and `-h`,
/// as Batect leaves out one with `showInHelp = false`. Which options are
/// hidden differs — these three are listed by Batect — and that is the
/// "Recognized, no effect" row on the differences page, not this test's.
#[test]
fn help_leaves_out_the_hidden_no_effect_flags() {
    for help in ["--help", "-h"] {
        let stdout = help_stdout(&mut ratect_command(), &[help]);
        for flag in [
            "--upgrade",
            "--no-update-notification",
            "--no-wrapper-cache-cleanup",
        ] {
            // By entry, not substring: unhidden, the other two's
            // descriptions would say "see --upgrade" and blame the wrong
            // flag.
            assert_eq!(
                help_entry(&stdout, flag),
                None,
                "{help} lists {flag}:\n{stdout}"
            );
        }
    }
}

/// #464: with stdout piped, Batect's help has no width to wrap to, so a
/// description stays on one line however long it is. `ratect-compat` never
/// wraps, so a small `COLUMNS` changes nothing either.
#[test]
fn help_never_wraps_a_description_when_piped() {
    const COLUMNS: usize = 40;
    let stdout = help_stdout(
        ratect_command().env("COLUMNS", COLUMNS.to_string()),
        &["--help"],
    );
    let entry = help_entry(&stdout, "--override-image")
        .unwrap_or_else(|| panic!("no --override-image entry:\n{stdout}"));
    assert!(
        entry.lines().any(|line| line.len() > COLUMNS
            && line.contains("Override the image used by a container")
            && line.contains("IfNotPresent policy")),
        "--override-image's description is wrapped:\n{entry}"
    );
}

/// #469: `--upgrade` is recognized but has nothing to upgrade, so it says so
/// on stderr and exits `0`, as the CLI reference says.
#[test]
fn upgrade_prints_a_notice_and_exits_0() {
    let output = ratect_command()
        .arg("--upgrade")
        .output()
        .expect("failed to run ratect");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("--upgrade has no effect:")),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// #469: `--no-update-notification` and `--no-wrapper-cache-cleanup` are
/// recognized and change nothing, silently.
#[test]
fn no_effect_wrapper_flags_leave_list_tasks_unchanged() {
    let smoke = sample_config_path();
    let list_tasks = |flags: &[&str]| {
        let output = ratect_command()
            .args(["-f", smoke.to_str().unwrap()])
            .args(flags)
            .arg("--list-tasks")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(output.status.code(), Some(0), "{flags:?}: {stderr}");
        (output.stdout, stderr)
    };

    let (plain, _) = list_tasks(&[]);
    let (flagged, stderr) = list_tasks(&["--no-update-notification", "--no-wrapper-cache-cleanup"]);
    assert_eq!(
        String::from_utf8_lossy(&flagged),
        String::from_utf8_lossy(&plain)
    );
    for mention in [
        "--upgrade has no effect",
        "no-update-notification",
        "no-wrapper-cache-cleanup",
    ] {
        assert!(!stderr.contains(mention), "{mention}: {stderr}");
    }
}

/// #470: `--help` is static, where Batect's says what each `DOCKER_*`
/// variable is currently set to — a divergence the differences page records.
#[test]
fn help_does_not_depend_on_the_docker_environment() {
    let variables = [
        ("DOCKER_HOST", "tcp://help-test-host:1"),
        ("DOCKER_CONTEXT", "help-test-context"),
        ("DOCKER_TLS_VERIFY", "help-test-tls-verify"),
        ("DOCKER_BUILDKIT", "help-test-buildkit"),
        ("DOCKER_CONFIG", "/help-test-config"),
        ("DOCKER_CERT_PATH", "/help-test-cert-path"),
    ];
    let mut unset = ratect_command();
    for (name, _) in variables {
        unset.env_remove(name);
    }
    let unset = help_stdout(&mut unset, &["--help"]);
    let set = help_stdout(ratect_command().envs(variables), &["--help"]);

    assert_eq!(set, unset);
    for (name, value) in variables {
        assert!(!set.contains(value), "{name}'s value is shown:\n{set}");
    }
}

/// #470: `--help` is the same whether or not a `batect.local.yml` exists,
/// where Batect's says whether it currently does.
#[test]
fn help_does_not_depend_on_a_batect_local_yml() {
    let dir = scratch_directory("help-local-yml");
    let with = dir.join("with");
    let without = dir.join("without");
    std::fs::create_dir_all(&with).unwrap();
    std::fs::create_dir_all(&without).unwrap();
    std::fs::write(with.join("batect.local.yml"), "").unwrap();

    let with = help_stdout(ratect_command().current_dir(&with), &["--help"]);
    let without = help_stdout(ratect_command().current_dir(&without), &["--help"]);
    std::fs::remove_dir_all(&dir).unwrap();

    assert_eq!(with, without);
}

/// #470: value placeholders are `clap`'s, derived from the field name, and
/// a fixed default is `clap`'s `[default: …]` — not Batect's
/// `--override-image=<container>=<image>` or `Defaults to 'batect.yml' if
/// not set.`
#[test]
fn help_uses_clap_placeholders_and_default_format() {
    let stdout = help_stdout(&mut ratect_command(), &["--help"]);
    for expected in [
        "--config-file <CONFIG_FILE>",
        "[default: batect.yml]",
        "--override-image <OVERRIDE_IMAGE>",
    ] {
        assert!(stdout.contains(expected), "no {expected}:\n{stdout}");
    }
}

/// #470: an option with no default shows only its own description, as in
/// Batect, whose default providers describe nothing for one.
#[test]
fn help_shows_no_default_for_an_option_without_one() {
    let stdout = help_stdout(&mut ratect_command(), &["--help"]);
    for option in ["--use-network", "--disable-ports"] {
        let entry =
            help_entry(&stdout, option).unwrap_or_else(|| panic!("no {option} entry:\n{stdout}"));
        assert!(
            entry.lines().count() > 1,
            "{option} has no description: {entry}"
        );
        assert!(!entry.contains("[default:"), "{entry}");
    }
}

fn environment_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/environment.yml")
}

fn default_command_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/default-command.yml")
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

fn tmpfs_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tmpfs.yml")
}

fn repeated_volumes_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/repeated-volumes.yml")
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

fn build_context_ownership_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-context-ownership.yml")
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

fn build_ssh_paths_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-ssh-paths.yml")
}

fn build_failure_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-failure.yml")
}

/// Serializes the tests that build `tests/fixtures/build-failure.yml` with
/// the classic builder: one counts the failed step's containers that weren't
/// there before its own run, and the other's run would add to them.
static CLASSIC_BUILD_FAILURE_PROJECT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn build_failure_buildkit_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-failure-buildkit.yml")
}

fn build_buildkit_default_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-buildkit-default.yml")
}

fn project_directory_declared_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project-directory-declared.yml")
}

fn undeclared_container_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/undeclared-container.yml")
}

fn interactive_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interactive.yml")
}

fn prerequisite_task_container_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prerequisite-task-container.yml")
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

fn include_forbids_telemetry_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/include-forbids-telemetry.yml")
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

/// A config using Batect *extensions* — top-level `.`-prefixed keys holding
/// YAML anchors — loads through the real binary rather than being rejected as
/// unknown fields. Needs no Docker: getting as far as listing tasks means the
/// whole file parsed.
#[test]
fn list_tasks_accepts_a_config_using_yaml_extensions() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yaml-extensions.yml"))
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "a config using extensions should load:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("show-environment"),
        "expected the fixture's task in output:\n{stdout}"
    );
}

/// #542: a null `environment` value is refused when the file loads, so
/// `--list-tasks` fails, and the error names the variable, the container
/// and that the value is null. No daemon is needed: nothing gets that far.
#[test]
fn list_tasks_names_a_null_environment_value_and_its_container() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-null-environment-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\n\
         containers:\n  app:\n    image: alpine:3.18.2\n    environment:\n      FOO: ~\n\
         tasks: {}\n",
    )
    .unwrap();

    let output = ratect_command()
        .current_dir(&dir)
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("containers.app.environment.FOO: invalid type: null"),
        "{stderr}"
    );
}

/// #372: a task given twice in one file is refused when the file loads, so
/// `--list-tasks` fails naming the key and lists nothing.
#[test]
fn list_tasks_refuses_a_task_given_twice() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-duplicate-task-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\n\
         containers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  t:\n    run:\n      container: app\n  t:\n    run:\n      container: app\n",
    )
    .unwrap();

    let output = ratect_command()
        .current_dir(&dir)
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stdout.contains("Tasks in"), "{stdout}");
    assert!(stderr.contains("duplicate key \"t\""), "{stderr}");
}

/// #377: a command line that cannot be split into argv is refused when the
/// file loads, so `--list-tasks` fails naming the task and the field, rather
/// than the task failing once its dependencies have started.
#[test]
fn list_tasks_refuses_an_unparseable_run_command() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-unparseable-command-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\n\
         containers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  the-task:\n    run:\n      container: app\n      command: \"'\"\n",
    )
    .unwrap();

    let output = ratect_command()
        .current_dir(&dir)
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stdout.contains("Tasks in"), "{stdout}");
    assert!(
        stderr.contains("Task 'the-task' has an invalid 'run.command'")
            && stderr.contains("Command `'` is invalid: it contains an unbalanced single quote"),
        "{stderr}"
    );
}

/// #382: `\` escapes in an expression, as in Batect. A trailing one escapes
/// nothing, so `--list-tasks` refuses the file; an escaped `$` is literal, so
/// the same file with its variable unset loads.
#[test]
fn list_tasks_reads_a_backslash_in_an_expression_as_an_escape() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-backslash-escape-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let list_tasks = |value: &str| {
        std::fs::write(
            dir.join("batect.yml"),
            format!(
                "project_name: demo\n\
                 containers:\n  app:\n    image: alpine:3.18.2\n    environment:\n      \
                 X: '{value}'\n\
                 tasks: {{}}\n"
            ),
        )
        .unwrap();
        ratect_command()
            .current_dir(&dir)
            .arg("--list-tasks")
            .env_remove("RATECT_UNSET_FOR_TEST")
            .output()
            .expect("failed to run ratect")
    };

    let trailing = list_tasks(r"a\");
    let escaped = list_tasks(r"\$RATECT_UNSET_FOR_TEST");
    std::fs::remove_dir_all(&dir).ok();

    let stderr = String::from_utf8_lossy(&trailing.stderr);
    assert_eq!(trailing.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("line 6, column 10: containers.app.environment.X"),
        "{stderr}"
    );
    assert!(
        stderr.contains("must be immediately followed by a character to escape"),
        "{stderr}"
    );
    assert!(
        escaped.status.success(),
        "{}",
        String::from_utf8_lossy(&escaped.stderr)
    );
}

/// #385: a malformed expression fails `--list-tasks`, located in the file;
/// a well-formed reference to an unset variable still loads, since only a
/// task that uses it evaluates it. #384: an unbraced `<foo.bar` is `foo`
/// then `.bar`, so it loads with only `foo` declared.
#[test]
fn list_tasks_refuses_a_malformed_expression_located_in_the_file() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-malformed-expression-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let list_tasks = |value: &str| {
        std::fs::write(
            dir.join("batect.yml"),
            format!(
                "project_name: demo
\
                 config_variables:\n  foo:\n    default: hello\n\
                 containers:\n  app:\n    image: alpine:3.18.2\n    environment:\n      \
                 X: '{value}'\n\
                 tasks: {{}}\n"
            ),
        )
        .unwrap();
        ratect_command()
            .current_dir(&dir)
            .arg("--list-tasks")
            .env_remove("RATECT_UNSET_FOR_TEST")
            .output()
            .expect("failed to run ratect")
    };

    let malformed = list_tasks("${X:default}");
    let unset = list_tasks("$RATECT_UNSET_FOR_TEST");
    let dotted = list_tasks("<foo.bar");
    std::fs::remove_dir_all(&dir).ok();

    let stderr = String::from_utf8_lossy(&malformed.stderr);
    assert_eq!(malformed.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "line 9, column 10: containers.app.environment.X: invalid expression \
             '${X:default}': invalid environment variable reference: ':' at column 4 must be \
             immediately followed by '-'"
        ),
        "{stderr}"
    );
    for output in [unset, dotted] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
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
fn list_tasks_with_quiet_output_is_machine_readable() {
    let output = ratect_command()
        .args(["--list-tasks", "-o", "quiet", "-f"])
        .arg(task_groups_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Sorted by name, `name<TAB>description` (tab only when a description
    // exists), no header, no grouping — Batect's own machine-readable
    // format.
    assert_eq!(
        stdout.trim_end(),
        "build\tBuilds the app\n\
         clean\n\
         lint\n\
         test\tRuns the test suite",
        "stdout:\n{}",
        stdout
    );
}

/// #471: Batect's machine-readable listing of a project with no tasks is
/// nothing at all — not a lone newline a line-counting consumer would read
/// as one unnamed task.
#[test]
fn list_tasks_with_quiet_output_prints_nothing_when_there_are_no_tasks() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-no-tasks-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("batect.yml"), "project_name: demo\n").unwrap();

    let output = ratect_command()
        .current_dir(&dir)
        .args(["--list-tasks", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

/// #444: a reader that has already gone (`--list-tasks | head -c 0`) is not
/// a crash. Batect's `PrintStream` swallows the write error and exits 0; so
/// does this, rather than panicking with exit 101.
#[test]
fn list_tasks_exits_cleanly_when_stdout_is_closed() {
    for style in ["simple", "quiet"] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);

        let output = ratect_command()
            .args(["--list-tasks", "-o", style, "-f"])
            .arg(sample_config_path())
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .expect("failed to run ratect");

        assert!(
            output.status.success(),
            "-o {style} exited {:?}:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stderr), "", "-o {style}");
    }
}

#[test]
fn requesting_fancy_output_without_an_interactive_console_fails_clearly() {
    // This test's own spawned process has piped stdout — never a terminal —
    // so an explicit `-o fancy` must be rejected up front (a deliberate
    // divergence from Batect, which accepts it and crashes mid-repaint).
    let output = ratect_command()
        .args(["-o", "fancy", "-f"])
        .arg(sample_config_path())
        .arg("test-task")
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires an interactive console"),
        "stderr:\n{stderr}"
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
fn an_include_forbidding_telemetry_reports_error() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(include_forbids_telemetry_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("include-forbids-telemetry/extra.yml")
            && stderr.contains("only the root configuration file can forbid telemetry"),
        "{stderr}"
    );
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
        stderr.contains("line 8, column 23")
            && stderr
                .contains("containers.build-env.not_a_real_field: unknown field: not_a_real_field"),
        "stderr:\n{}",
        stderr
    );
}

/// A run that fails before any task starts — here, a config that doesn't
/// load — reached no event sink, so `main` prints its `Error:` line, and
/// exactly once (ratect#334).
#[test]
fn a_run_failing_before_its_task_starts_prints_its_error_once() {
    let output = ratect_command()
        .arg("-f")
        .arg(unsupported_key_config_path())
        .args(["-o", "quiet", "build"])
        .env("RUST_LOG", "off")
        .output()
        .expect("failed to run ratect");

    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Error: ") && stderr.matches("Error:").count() == 1,
        "{stderr}"
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
        stderr.contains("line 7, column 5")
            && stderr.contains("Invalid config variable name 'batect.project_directory'."),
        "stderr:\n{}",
        stderr
    );
}

/// A value only some machines have is not a mistake in the file, so a
/// container no task uses that reads one doesn't stop `--list-tasks`, as it
/// doesn't stop Batect (decisions/0014).
#[test]
fn list_tasks_succeeds_when_an_unused_container_cannot_be_evaluated() {
    let output = ratect_command()
        .args(["--list-tasks", "-o", "quiet", "-f"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/unevaluable-unused-container.yml"),
        )
        .env_remove("RATECT_TEST_NEVER_SET")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build\n");
}

/// #355: Batect reads a config-variables file only when a task runs, so
/// nothing in one — malformed, or naming a variable the config doesn't
/// declare — stops `--list-tasks`, whether it's an auto-discovered
/// `batect.local.yml` or named by `--config-vars-file`. Batect's option
/// parser does check that a named file is a file, for every command, so a
/// missing one, or a directory, still fails. A run reads the file, so a
/// malformed one still fails it.
#[test]
fn list_tasks_reads_no_config_variables_file() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-list-tasks-vars-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  build:\n    run:\n      container: app\n",
    )
    .unwrap();
    std::fs::write(dir.join("batect.local.yml"), "- not\n- a map\n").unwrap();
    std::fs::write(dir.join("undeclared.yml"), "nowhere: x\n").unwrap();
    let ratect = |args: &[&str]| {
        ratect_command()
            .current_dir(&dir)
            .args(args)
            .output()
            .expect("failed to run ratect")
    };

    for args in [
        &["--list-tasks", "-o", "quiet"][..],
        &[
            "--list-tasks",
            "-o",
            "quiet",
            "--config-vars-file",
            "undeclared.yml",
        ],
    ] {
        let output = ratect(args);
        assert!(
            output.status.success(),
            "{args:?}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "build\n",
            "{args:?}"
        );
    }

    std::fs::create_dir(dir.join("a-directory")).unwrap();
    for (flag, path) in [
        ("--list-tasks", "missing.yml"),
        ("--list-tasks", "a-directory"),
        ("--clean", "missing.yml"),
    ] {
        let output = ratect(&[flag, "--config-vars-file", path]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{flag} {path}: {stderr}");
        assert!(
            stderr.contains("--config-vars-file") && stderr.contains(path),
            "{flag} {path}: {stderr}"
        );
    }

    let run = ratect(&["build"]);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success(), "{stderr}");
    assert!(
        stderr.contains("Failed to parse config vars file"),
        "{stderr}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The run's counterpart: a task is evaluated only when it is reached, and
/// only the containers its own graph uses, so `deploy`'s values are never
/// looked at (decisions/0014).
#[test]
#[ignore]
fn a_task_runs_when_an_unused_container_cannot_be_evaluated_via_docker() {
    let output = ratect_command()
        .args(["-o", "simple", "-f"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/unevaluable-unused-container.yml"),
        )
        .arg("build")
        .env_remove("RATECT_TEST_NEVER_SET")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "built"),
        "stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Refused when the file loads, not only when a task reaching it runs as in
/// Batect — so even `--list-tasks` fails.
#[test]
fn a_reference_to_an_undeclared_container_fails_the_load() {
    let output = ratect_command()
        .args(["--list-tasks", "-f"])
        .arg(undeclared_container_config_path())
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .contains("The container 'ghost' referenced by container 'build-env' does not exist."),
        "stderr:\n{}",
        stderr
    );
}

/// Refused where overrides are read — when the task's values are
/// evaluated, before anything reaches Docker.
#[test]
fn overriding_batect_project_directory_via_cli_reports_error() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("--config-var")
        .arg("batect.project_directory=/hijacked")
        .arg("test-task")
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

/// `image` with a build-only field is rejected when the configuration
/// loads, not when something runs — `--list-tasks` starts no container.
///
/// Batect rejects all seven `image`-versus-`build_*` combinations; Ratect
/// accepted them and resolved silently, so a configured `build_ssh` here
/// was read by nothing. The engine's own lazy check can't stand in for this
/// one: it only fires when *neither* field is set.
#[test]
fn image_combined_with_a_build_field_is_rejected_when_the_config_loads() {
    let output = ratect_command()
        .arg("-f")
        .arg(image_with_build_fields_config_path())
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Container 'build-env' has 'build_ssh', which cannot be used with 'image'"),
        "stderr:\n{stderr}"
    );
}

/// No task name, and no flag that replaces one, is a usage error found
/// while the arguments are parsed, as in Batect (ratect#434): before the
/// config-variables file or the config is read, so neither a broken one nor
/// `RUST_LOG=off` changes what is reported, and with the same code as an
/// option `clap` doesn't know on a run.
#[test]
fn no_task_name_is_a_usage_error_before_any_config_is_read() {
    let directory = invalid_values_directory();
    let run = |args: &[&str], env: &[(&str, &str)]| {
        ratect_command()
            .current_dir(&directory)
            .args(args)
            .envs(env.iter().copied())
            .output()
            .expect("failed to run ratect")
    };
    let bogus = run(&["--bogus", "build"], &[]).status.code();
    assert!(bogus.is_some_and(|code| code != 0));

    let sample = sample_config_path();
    let sample = sample.to_str().unwrap();
    for (args, env) in [
        (&["-f", sample][..], &[][..]),
        (&["-f", sample], &[("RUST_LOG", "off")]),
        (&["-f", sample, "--", "foo"], &[]),
        (&["-f", "broken.yml"], &[]),
        (&["-f", "nope.yml"], &[]),
        (&["-f", sample, "--config-vars-file", "nope.yml"], &[]),
        (&[], &[]),
    ] {
        let output = run(args, env);
        assert_eq!(output.status.code(), bogus, "{args:?} {env:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("no task name")
                && stderr.contains("--list-tasks")
                && stderr.contains("--help"),
            "{args:?} {env:?}: {stderr}"
        );
        assert!(output.stdout.is_empty(), "{args:?} printed to stdout");
    }

    // The flags that replace a task still need none.
    let listed = run(&["-f", sample, "--list-tasks"], &[]);
    assert_eq!(listed.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&listed.stdout).contains("Tasks in ratect-test:"));
    let upgraded = run(&["-f", sample, "--upgrade"], &[]);
    assert_eq!(upgraded.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&upgraded.stderr).contains("--upgrade has no effect"));
    for clean in [&["--clean"][..], &["--clean-cache", "x"]] {
        let output = run(
            // A file `--clean` never reads, so it may be broken; it must
            // only exist, as a given `-f` must (ratect#437).
            &[&["-f", "broken.yml", "--cache-type", "directory"], clean].concat(),
            &[],
        );
        assert_eq!(output.status.code(), Some(0), "{clean:?}");
    }
    for mode in ["--help", "--version"] {
        assert_eq!(run(&[mode], &[]).status.code(), Some(0), "{mode}");
    }

    std::fs::remove_dir_all(&directory).unwrap();
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

/// What an unknown task's error ends with, as Batect's does — naming this
/// binary's own commands, never `./batect`'s.
const UNKNOWN_TASK_HINT: &str = "(Run 'ratect-compat --list-tasks' for a list of all tasks in \
                                 this project, or 'ratect-compat --help' for help.)";

/// Runs `args` against the smoke fixture, expecting failure, and returns
/// stderr.
fn failing_stderr(args: &[&str]) -> String {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .args(args)
        .output()
        .expect("failed to run ratect");
    assert_eq!(output.status.code(), Some(125));
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_unknown_task_error_ends_with_how_to_list_the_tasks() {
    for args in [
        &["does-not-exist-task"][..],
        &["-o", "quiet", "does-not-exist-task"],
    ] {
        let stderr = failing_stderr(args);
        assert!(
            stderr.trim_end().ends_with(&format!(
                "Task 'does-not-exist-task' not found. {UNKNOWN_TASK_HINT}"
            )),
            "{args:?} stderr:\n{stderr}"
        );
    }
}

#[test]
fn a_misspelled_task_error_gives_the_hint_after_its_suggestion() {
    let stderr = failing_stderr(&["tst-task"]);
    assert!(
        stderr.trim_end().ends_with(&format!(
            "Task 'tst-task' not found. Did you mean 'test-task'? {UNKNOWN_TASK_HINT}"
        )),
        "stderr:\n{stderr}"
    );
}

/// Batect gives a missing prerequisite no hint: the task asked for exists.
#[test]
fn a_missing_prerequisite_error_has_no_hint() {
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-missing-prerequisite-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  build:\n    prerequisites: [does-not-exist]\n    run:\n      container: app\n",
    )
    .unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .arg("build")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Task 'does-not-exist' not found.") && !stderr.contains("--list-tasks"),
        "stderr:\n{stderr}"
    );
}

fn exit_code_of(args: &[&str]) -> Option<i32> {
    ratect_command()
        .args(args)
        .output()
        .expect("failed to run ratect")
        .status
        .code()
}

/// `--help` and `--version` exit `0` once `clap` reaches them. An argument
/// `clap` rejects takes its path's code (decisions/0016): `125` on a line
/// meant to run a task — named or not, and even with `--help` after the
/// rejection — and `2` on one that replaces the task with a flag. Both
/// divergences from Batect (`1` for `--help`, `255` for a usage error).
#[test]
fn help_exits_zero_and_a_usage_error_takes_its_paths_code() {
    assert_eq!(exit_code_of(&["--help"]), Some(0));
    assert_eq!(exit_code_of(&["--version"]), Some(0));

    assert_eq!(exit_code_of(&["--bogus", "build"]), Some(125));
    assert_eq!(exit_code_of(&["--bogus"]), Some(125));
    assert_eq!(exit_code_of(&["--bogus", "--help"]), Some(125));
    assert_eq!(
        exit_code_of(&["build", "--bogus", "--", "--clean"]),
        Some(125)
    );

    assert_eq!(exit_code_of(&["--bogus", "--clean"]), Some(2));
    assert_eq!(exit_code_of(&["--bogus", "--list-tasks"]), Some(2));
}

/// An unknown option's error doesn't tip writing it after `--`, which would
/// pass it to the task (ratect#660), and keeps its path's exit code.
#[test]
fn an_unknown_options_error_has_no_double_dash_tip() {
    for (line, code) in [(&["--bogus", "build"][..], 125), (&["--bogus", "-T"], 2)] {
        let output = ratect_command()
            .args(line)
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{line:?}: {stderr}");
        assert!(stderr.contains("'--bogus'"), "{line:?}: {stderr}");
        assert!(!stderr.contains("use '-- "), "{line:?}: {stderr}");
    }
}

/// Options end at the task name (ratect#435): an argument after it but
/// `-- ARGS…` is a usage error on a run, `125`, and lists nothing — while a
/// flag that replaces the task ignores whatever follows the options.
#[test]
fn an_option_after_the_task_name_is_a_usage_error_not_applied() {
    let config = sample_config_path();
    let config = config.to_str().unwrap();
    let run = |args: &[&str]| {
        ratect_command()
            .args(["-f", config])
            .args(args)
            .output()
            .expect("failed to run ratect")
    };

    for line in [
        &["test-task", "--list-tasks"][..],
        &["test-task", "extra"],
        &["build", "--disable-ports"],
    ] {
        let output = run(line);
        assert_eq!(output.status.code(), Some(125), "{line:?}");
        assert!(output.stdout.is_empty(), "{line:?} printed to stdout");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("'{}'", line[0]))
                && stderr.contains(&format!("'{}'", line[1])),
            "{line:?}: {stderr}"
        );
    }

    for line in [
        &["--list-tasks", "a", "b"][..],
        &["--list-tasks", "a", "--", "x"],
    ] {
        let output = run(line);
        assert_eq!(output.status.code(), Some(0), "{line:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Tasks in ratect-test:"),
            "{line:?}"
        );
    }

    let output = run(&["--use-network", "-x", "test-task"]);
    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--use-network=-x") && !stderr.contains("'-- -x'"),
        "{stderr}"
    );
}

/// `--list-tasks` wins over `--clean` and `--upgrade`, as in Batect
/// (ratect#442): each line prints what plain `--list-tasks` prints, so
/// `--clean` neither cleans nor connects (no daemon listens on the host
/// given), and `--upgrade` prints no notice.
#[test]
fn list_tasks_wins_over_clean_and_upgrade() {
    let config = sample_config_path();
    let config = config.to_str().unwrap();
    let run = |args: &[&str]| {
        ratect_command()
            .env_remove("BATECT_CACHE_TYPE")
            .args(["-f", config, "--docker-host", "tcp://127.0.0.1:1"])
            .args(args)
            .output()
            .expect("failed to run ratect")
    };
    for style in [&[][..], &["-o", "quiet"]] {
        let plain = run(&[style, &["-T"]].concat());
        assert_eq!(plain.status.code(), Some(0));
        for flags in [&["-T", "--clean"][..], &["--upgrade", "-T"]] {
            let output = run(&[style, flags].concat());
            assert_eq!(output.status.code(), Some(0), "{style:?} {flags:?}");
            assert_eq!(output.stdout, plain.stdout, "{style:?} {flags:?}");
            assert!(output.stderr.is_empty(), "{style:?} {flags:?}");
        }
    }
}

/// `--max-parallelism` takes Batect's range, 1 to 2147483647, its
/// `Integer.parseInt`'s: a larger value is an argument error naming the
/// option and the value, in either form (ratect#451).
#[test]
fn max_parallelism_takes_batects_range() {
    let config = sample_config_path();
    let config = config.to_str().unwrap();
    let run = |value: &[&str]| {
        ratect_command()
            .args(["-f", config])
            .args(value)
            .arg("--list-tasks")
            .output()
            .expect("failed to run ratect")
    };
    for accepted in [
        "--max-parallelism=2147483647",
        "--max-parallelism=1",
        "--max-parallelism=+1",
    ] {
        let output = run(&[accepted]);
        assert_eq!(output.status.code(), Some(0), "{accepted}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Tasks in ratect-test:"));
    }
    for (rejected, value) in [
        (&["--max-parallelism=2147483648"][..], "2147483648"),
        (&["--max-parallelism=4294967295"], "4294967295"),
        (&["--max-parallelism", "2147483648"], "2147483648"),
        (&["--max-parallelism=0"], "0"),
    ] {
        let output = run(rejected);
        assert_eq!(output.status.code(), Some(2), "{rejected:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--max-parallelism") && stderr.contains(value),
            "{rejected:?}: {stderr}"
        );
        assert!(output.stdout.is_empty(), "{rejected:?}");
    }
}

/// `--use-network=`, `--docker-host=` and `--docker-context=` are argument
/// errors, as `--output=` is, as in Batect (ratect#449): nothing is listed,
/// and the empty host is reported rather than taken as set alongside a
/// context.
#[test]
fn an_empty_string_option_after_equals_is_an_argument_error() {
    let config = sample_config_path();
    let config = config.to_str().unwrap();
    let run = |args: &[&str]| {
        ratect_command()
            .args(["-f", config])
            .args(args)
            .output()
            .expect("failed to run ratect")
    };
    let expected = run(&["--list-tasks", "--output="]).status.code();
    assert!(expected.is_some_and(|code| code != 0));
    for option in ["--use-network=", "--docker-host=", "--docker-context="] {
        let output = run(&["--list-tasks", option]);
        assert_eq!(output.status.code(), expected, "{option}");
        assert!(output.stdout.is_empty(), "{option} listed the tasks");
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("'{option}'")));
    }

    let output = run(&["--docker-context=foo", "--docker-host=", "test-task"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'--docker-host='") && !stderr.contains("Cannot use both"),
        "{stderr}"
    );
}

/// `--clean-cache` takes a comma-separated list, as in Batect, and
/// repeats; `--clean-cache=` is refused and removes nothing (ratect#448).
/// Asserted on the directories left behind.
#[test]
fn clean_cache_takes_a_comma_separated_list() {
    let project = one_task_project("clean-cache-list");
    let caches = project.join(".batect/caches");
    let remaining = |args: &[&str]| {
        for name in ["foo", "bar", "baz"] {
            std::fs::create_dir_all(caches.join(name)).unwrap();
        }
        let output = ratect_command()
            .current_dir(&project)
            .args(["--cache-type", "directory"])
            .args(args)
            .output()
            .expect("failed to run ratect");
        let mut left: Vec<String> = std::fs::read_dir(&caches)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        (output, left)
    };

    for (args, left) in [
        (&["--clean-cache=foo,bar"][..], &["baz"][..]),
        (&["--clean-cache=foo"], &["bar", "baz"]),
        (&["--clean-cache", "foo", "--clean-cache", "bar"], &["baz"]),
    ] {
        let (output, remaining) = remaining(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(remaining, left, "{args:?}");
    }

    let (output, remaining) = remaining(&["--clean-cache="]);
    assert_ne!(output.status.code(), Some(0));
    assert_eq!(remaining, ["bar", "baz", "foo"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("'--clean-cache='"));

    std::fs::remove_dir_all(&project).unwrap();
}

/// A unique, empty temp directory holding a directory `dir` and a
/// config file `broken.yml` that exists but doesn't parse.
fn invalid_values_directory() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "ratect-compat-invalid-values-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(directory.join("dir")).unwrap();
    std::fs::write(directory.join("broken.yml"), "project_name: [").unwrap();
    directory
}

/// An invalid option value — a path that doesn't exist or is the wrong kind,
/// or a log file whose directory doesn't exist — exits `2` under
/// `--list-tasks` and `125` on a run, while a config file that exists but
/// doesn't parse is `--list-tasks`'s own failure, `1` (decisions/0016).
/// Needs no daemon: each fails before anything connects.
#[test]
fn an_invalid_option_value_exits_2_or_125_by_path() {
    let directory = invalid_values_directory();
    let exit_code = |args: &[&str]| {
        ratect_command()
            .current_dir(&directory)
            .args(args)
            .output()
            .expect("failed to run ratect")
            .status
            .code()
    };

    for invalid in [
        &["--config-vars-file", "dir"][..],
        &["--config-vars-file", "nope.yml"],
        &["-f", "nope.yml"],
        &["-f", "dir"],
        &["--log-file", "nodir/ratect.log"],
    ] {
        let mut list = invalid.to_vec();
        list.push("--list-tasks");
        assert_eq!(exit_code(&list), Some(2), "{list:?}");
        let mut run = invalid.to_vec();
        run.push("build");
        assert_eq!(exit_code(&run), Some(125), "{run:?}");
    }
    assert_eq!(exit_code(&["-f", "broken.yml", "--list-tasks"]), Some(1));
    assert_eq!(exit_code(&["-f", "broken.yml", "build"]), Some(125));

    std::fs::remove_dir_all(&directory).unwrap();
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
    // Exact line match, not a substring count — the milestone line "Running
    // echo "I should only run once" in build-env..." quotes the command, so
    // the substring appears once per run even when the task only ran once.
    assert_eq!(
        stdout
            .lines()
            .filter(|line| *line == "I should only run once")
            .count(),
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
/// Proves the output format itself (0.16.0's output-modes work, simple
/// style — the non-TTY default): the milestone lines frame the task's own
/// output in the documented order, on stdout. Every other e2e test merely
/// *tolerates* these lines (via `task_output`); this one is what actually
/// pins them, end to end through `main.rs`'s real logger wiring — a unit
/// test of `SimpleEventLogger` can't prove the binary wired it up at all.
#[test]
#[ignore]
fn simple_output_format_frames_task_output_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(sample_config_path())
        .arg("shared-prereq")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    // The image may or may not already be local ("Pulling"/"Pulled" lines
    // are correctly absent when it is), so match the invariant frame, not
    // an exact transcript.
    let running_task = lines
        .iter()
        .position(|line| *line == "Running shared-prereq...")
        .unwrap_or_else(|| panic!("missing task milestone line:\n{stdout}"));
    let running_container = lines
        .iter()
        .position(|line| *line == r#"Running echo "I should only run once" in build-env..."#)
        .unwrap_or_else(|| panic!("missing container run milestone line:\n{stdout}"));
    let task_output_line = lines
        .iter()
        .position(|line| *line == "I should only run once")
        .unwrap_or_else(|| panic!("missing task output:\n{stdout}"));
    let cleaning_up = lines
        .iter()
        .position(|line| *line == "Cleaning up...")
        .unwrap_or_else(|| panic!("missing cleanup milestone line:\n{stdout}"));
    let finished = lines
        .iter()
        .position(|line| {
            line.starts_with("shared-prereq finished with exit code 0 in ") && line.ends_with('.')
        })
        .unwrap_or_else(|| panic!("missing task summary line:\n{stdout}"));

    let order = [
        running_task,
        running_container,
        task_output_line,
        cleaning_up,
        finished,
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "milestone lines out of order ({order:?}):\n{stdout}"
    );
    assert_eq!(
        lines[cleaning_up - 1],
        "",
        "a blank line should separate task output from cleanup:\n{stdout}"
    );
}

/// Requires a running Docker daemon and network access to build from
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Regression test for TODO.md item 4 (ratect#72): a task with no `command`
/// override at either the container or `run` level produces the command-less
/// `"Running <container>..."` milestone (see `RunningTaskContainer`'s `None`
/// arm in `ratect-core/src/ui/simple.rs`) instead of `"Running <command> in
/// <container>..."`. `task_output`'s old heuristic required `" in "` and
/// never matched this shape, so this case would previously have failed
/// loudly with the whole framed stdout instead of just the container's own
/// output.
#[test]
#[ignore]
fn task_with_no_command_uses_the_image_default_cmd_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(default_command_config_path())
        .arg("run-default-command")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(task_output(&stdout, "build-env"), "default-cmd-output");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `--output quiet` end to end: stdout is *exactly* the task's own
/// output — every milestone line suppressed, across a whole prerequisite
/// chain — so quiet-mode output is safe to pipe.
#[test]
#[ignore]
fn quiet_output_is_exactly_the_tasks_own_output_via_docker() {
    let output = ratect_command()
        .args(["-o", "quiet", "-f"])
        .arg(sample_config_path())
        .arg("prereq-task")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim_end(),
        "I should only run once\n\
         I am a prerequisite",
        "quiet stdout should carry only the containers' own output:\n{stdout}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`, and the ability to allocate a local pseudo-terminal —
/// see `interactive_session_forwards_stdin_and_stdout` below for why
/// `portable-pty` makes this work in headless CI too. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves the fancy output mode end to end: on a real (emulated) terminal
/// it's the auto-selected default — no `-o` flag here, deliberately — and
/// its live display actually renders: the per-container status block
/// (cursor-movement repaints included), the frozen block above the task's
/// own streamed output, and the final summary line.
#[test]
#[ignore]
fn fancy_output_renders_a_live_status_block_on_a_terminal_via_docker() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect-compat"));
    cmd.arg("-f");
    cmd.arg(sample_config_path());
    cmd.arg("shared-prereq");
    cmd.env("TERM", "xterm-256color");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("failed to spawn ratect in the pty");
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("failed to clone pty reader");
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
    let deadline = Instant::now() + Duration::from_secs(60);
    while !String::from_utf8_lossy(&output).contains("finished with exit code")
        && Instant::now() < deadline
    {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            output.extend_from_slice(&chunk);
        }
    }
    let output = String::from_utf8_lossy(&output);

    // The task preamble (bold task name — auto-selected fancy implies
    // color on a terminal).
    assert!(
        output.contains("Running \x1b[1mshared-prereq\x1b[0m..."),
        "missing bold task preamble: {output:?}"
    );
    // The status block painted at least once: clear-line control + the
    // container's bold name and a stage description.
    assert!(
        output.contains("\x1b[2K\x1b[1mbuild-env\x1b[0m: "),
        "missing per-container status line: {output:?}"
    );
    // The task's own output streamed below the frozen block.
    assert!(
        output.contains("I should only run once"),
        "missing task output: {output:?}"
    );
    // The summary line, with the colored exit code.
    assert!(
        output.contains("finished with exit code \x1b[32m0\x1b[0m"),
        "missing summary line: {output:?}"
    );

    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });
    let status = wait_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("ratect did not exit after the task finished")
        .expect("failed to wait for ratect");
    assert!(status.success(), "ratect should exit cleanly: {status:?}");
}

/// Requires a running Docker daemon with network access to pull
/// `redis:7-alpine` and `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves `--output all` end to end: every line on stdout carries a padded
/// container (or task) prefix, and — uniquely to this mode — dependency
/// containers' own stdout is visible at all (redis's startup log reaching
/// stdout under the `database` prefix proves the background log-follower
/// actually attached).
#[test]
#[ignore]
fn all_output_prefixes_every_line_and_shows_dependency_output_via_docker() {
    let output = ratect_command()
        .args(["-o", "all", "-f"])
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
    // Width = len("ping-sidecars") = 13, the longest name in play.
    assert!(
        stdout.contains("ping-sidecars | Running ping-sidecars..."),
        "missing task preamble:\n{stdout}"
    );
    // Container milestones, prefixed and padded to the same column.
    assert!(
        stdout.contains("database      | Container became healthy."),
        "missing prefixed dependency milestone:\n{stdout}"
    );
    // The task container's own output, prefixed.
    assert!(
        stdout.contains("app           | ") && stdout.contains("packets transmitted"),
        "missing prefixed task output:\n{stdout}"
    );
    // Dependency stdout — the thing only this mode shows: redis logs its
    // startup banner well before its health check passes.
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("database      | ") && line.contains("Ready to accept")),
        "missing dependency container output:\n{stdout}"
    );
    // Cleanup + summary lines are task-prefixed.
    assert!(
        stdout.contains("ping-sidecars | Cleaning up..."),
        "missing cleanup line:\n{stdout}"
    );
    assert!(
        stdout.contains("ping-sidecars | ping-sidecars finished with exit code 0 in "),
        "missing summary line:\n{stdout}"
    );
    // Every non-empty stdout line is prefixed — nothing bypasses the
    // interleaved output.
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        assert!(
            line.contains(" | "),
            "unprefixed line in all-mode output: {line:?}\nfull:\n{stdout}"
        );
    }
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// A task's own non-zero exit is reported by the exit code — and, outside
/// `quiet`, by the summary line — and never by an `Error:` line on stderr
/// (ratect#454; decisions/0017's report-once rule). Under `quiet` that leaves
/// both streams empty: stdout untouched (the failure half of quiet's
/// pipe-safety), stderr with nothing to add.
#[test]
#[ignore]
fn a_tasks_own_exit_code_prints_no_error_line_via_docker() {
    let quiet = ratect_command()
        .args(["-o", "quiet", "-f"])
        .arg(exit_code_config_path())
        .arg("fails")
        .output()
        .expect("failed to run ratect");

    assert_eq!(quiet.status.code(), Some(42));
    assert_eq!(
        String::from_utf8_lossy(&quiet.stdout),
        "",
        "quiet stdout must stay empty on failure"
    );
    assert_eq!(
        String::from_utf8_lossy(&quiet.stderr),
        "",
        "a task's own exit is no error of Ratect's, so stderr stays empty"
    );

    let simple = ratect_command()
        .args(["-o", "simple", "-f"])
        .arg(exit_code_config_path())
        .arg("fails")
        .output()
        .expect("failed to run ratect");

    assert_eq!(simple.status.code(), Some(42));
    let stdout = String::from_utf8_lossy(&simple.stdout);
    assert!(
        stdout.contains("fails finished with exit code 42"),
        "the summary line reports the exit:\n{stdout}"
    );
    let stderr = String::from_utf8_lossy(&simple.stderr);
    assert!(
        !stderr.contains("Error:") && !stderr.contains("exited with code 42"),
        "no second report on stderr:\n{stderr}"
    );
}

/// Requires a running Docker daemon. Run explicitly with `cargo test -- --ignored`.
///
/// The failure half of quiet's pipe-safety for a failure of Ratect's own: a
/// run that fails before its command runs leaves stdout untouched — no
/// milestone lines, nothing — while the exit code is `125` and the error
/// still reaches stderr, `quiet` included (ratect#359's quiet-mode
/// infrastructure-failure case).
#[test]
#[ignore]
fn quiet_output_stays_silent_on_stdout_when_the_run_fails_via_docker() {
    let output = ratect_command()
        .args(["-o", "quiet", "-f"])
        .arg(exit_code_config_path())
        .arg("unpullable")
        .output()
        .expect("failed to run ratect");

    assert_eq!(output.status.code(), Some(125));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout, "",
        "quiet stdout must stay empty on failure:\n{stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error:") && stderr.contains("Failed to pull image"),
        "the failure should be reported on stderr:\n{stderr}"
    );
}

/// Requires a running Docker daemon. Run explicitly with `cargo test -- --ignored`.
///
/// Proves a run's fatal error is visible on stderr even with `RUST_LOG=off`
/// — the error used to reach stderr solely through `tracing::error!`, which
/// `RUST_LOG` can suppress entirely; a failure under `-o quiet` plus a
/// suppressed log level would then exit non-zero with no visible reason
/// anywhere. The error is printed directly, independent of `RUST_LOG`.
#[test]
#[ignore]
fn fatal_error_reaches_stderr_even_with_logging_disabled() {
    let output = ratect_command()
        .args(["-o", "quiet", "-f"])
        .arg(exit_code_config_path())
        .arg("unpullable")
        .env("RUST_LOG", "off")
        .output()
        .expect("failed to run ratect");

    assert_eq!(output.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Error: "),
        "the fatal error must still reach stderr under RUST_LOG=off:\n{stderr}"
    );
    assert!(
        stderr.contains("Failed to pull image"),
        "stderr should name the actual failure:\n{stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves a task with only `prerequisites` and no `run` of its own still runs
/// its prerequisites end to end, then stops cleanly (exit 0) — no container
/// of the task's own to run — and says so on stdout, as Batect does, whatever
/// `RUST_LOG` says; but not under `-o quiet`, which prints no progress
/// (ratect#364).
#[test]
#[ignore]
fn task_with_only_prerequisites_and_no_run_runs_its_prerequisites_via_docker() {
    const NOTHING_MORE: &str =
        "The task prerequisites-only-task only defines prerequisite tasks, nothing more to do.";
    let run = |style: &str| {
        ratect_command()
            .arg("-f")
            .arg(sample_config_path())
            .args(["-o", style, "prerequisites-only-task"])
            .env("RUST_LOG", "off")
            .output()
            .expect("failed to run ratect")
    };

    let output = run("simple");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("I should only run once"));
    assert!(stdout.trim_end().ends_with(NOTHING_MORE), "{stdout}");

    let output = run("quiet");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(NOTHING_MORE) && !stderr.contains(NOTHING_MORE),
        "quiet prints no progress:\n{stdout}{stderr}"
    );
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
/// Ratect's ownership labels (`ratect_core::labels`) have to reach real
/// Docker objects, which a fake runtime can't show: the unit tests prove the
/// right map was passed, this proves Docker stored it. Uses
/// `--no-cleanup-after-success` so the containers and network still exist to
/// be inspected — which is also the case the labels exist to serve, since
/// that flag is one of the ways leftovers are created deliberately.
///
/// Runs against a fixture with a project name of its own: these tests run
/// concurrently, and filtering by project label would otherwise match
/// whatever another test using the same fixture had running at that moment.
#[test]
#[ignore]
fn ownership_labels_reach_real_containers_and_networks_via_docker() {
    fn docker_ids(kind: &str, filter: &str) -> Vec<String> {
        // Containers need `-a`: the task's own has already exited by now
        // (its command ran to completion), and a bare `ls` lists only
        // running ones — which would quietly miss exactly the container
        // whose `role` label this is checking. `network ls` has no such
        // flag, and needs none.
        let mut arguments = vec![kind, "ls", "-q", "--filter", filter];
        if kind == "container" {
            arguments.insert(2, "-a");
        }
        let output = Command::new("docker")
            .args(&arguments)
            .output()
            .expect("failed to run docker ls");
        assert!(output.status.success(), "docker {kind} ls failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn label_of(kind: &str, id: &str, label: &str) -> String {
        let output = Command::new("docker")
            .args([
                kind,
                "inspect",
                id,
                "--format",
                &format!("{{{{index .Config.Labels \"{label}\"}}}}"),
            ])
            .output()
            .expect("failed to run docker inspect");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    // `tests/fixtures/labels.yml` has a project name of its own so this
    // filter can't match a concurrently-running test's containers.
    let project_filter = "label=eu.orican.ratect.project=ratect-labels-test";

    // `RUST_LOG=off`: the reminder below is printed, not logged.
    let output = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/labels.yml"))
        .arg("--no-cleanup-after-success")
        .arg("check")
        .env("RUST_LOG", "off")
        .output()
        .expect("failed to run ratect");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // What the flag kept, and how to remove it, is a notice on stderr
    // whatever `RUST_LOG` says (decisions/0017) — naming the containers by
    // config name and id and the command that works when pasted.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Left in place (--no-cleanup): app (")
            && stderr.contains(", database (")
            && stderr.contains(", network ratect-labels-test-network-")
            && stderr.contains(". Remove with: docker rm -f -v ")
            && stderr.contains("; docker network rm ratect-labels-test-network-"),
        "the reminder names what was kept and how to remove it:\n{stderr}"
    );
    assert!(
        !stderr.contains("Error:") && !stderr.contains("Warning:"),
        "a deliberate keep is a notice, not a failure:\n{stderr}"
    );

    let containers = docker_ids("container", project_filter);
    let networks = docker_ids("network", project_filter);

    // Everything gets torn down before any assertion can fail, so a
    // failure here doesn't leave the daemon full of this test's containers.
    let cleanup = || {
        for id in &containers {
            let _ = Command::new("docker").args(["rm", "-fv", id]).output();
        }
        for id in &networks {
            let _ = Command::new("docker").args(["network", "rm", id]).output();
        }
    };

    let roles: Vec<String> = containers
        .iter()
        .map(|id| label_of("container", id, "eu.orican.ratect.role"))
        .collect();
    let runs: Vec<String> = containers
        .iter()
        .map(|id| label_of("container", id, "eu.orican.ratect.run"))
        .collect();
    let task = containers
        .first()
        .map(|id| label_of("container", id, "eu.orican.ratect.task"))
        .unwrap_or_default();
    cleanup();

    // `app` plus its one dependency.
    assert_eq!(containers.len(), 2, "expected two labelled containers");
    assert_eq!(networks.len(), 1, "expected one labelled network");
    assert_eq!(task, "check");
    assert_eq!(
        roles.iter().filter(|role| *role == "task").count(),
        1,
        "exactly one container is the task's own: {roles:?}"
    );
    assert_eq!(
        roles.iter().filter(|role| *role == "dependency").count(),
        1,
        "the other is a dependency: {roles:?}"
    );
    assert!(
        runs.windows(2).all(|pair| pair[0] == pair[1]) && !runs[0].is_empty(),
        "every container should carry the same non-empty run id: {runs:?}"
    );
}

/// Requires a running Docker daemon with network access to pull `redis:7-alpine`
/// and `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A task's containers are created on the task network, with their aliases,
/// and are never also on Docker's default `bridge` (ratect#303) — and Docker
/// stores them, and the network, under Batect's names (ratect#299): the
/// network `<project>-network-<suffix>`, each container
/// `<project>-<container>-<suffix>`, one suffix shared by all three. Asserted
/// on what the daemon reports, with `--no-cleanup-after-success` keeping
/// everything to inspect; an exited container still lists its networks.
#[test]
#[ignore]
fn task_containers_are_only_on_the_batect_named_task_network_via_docker() {
    fn docker_lines(arguments: &[&str]) -> Vec<String> {
        let output = Command::new("docker")
            .args(arguments)
            .output()
            .expect("failed to run docker");
        assert!(output.status.success(), "docker {arguments:?} failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// `prefix-` then six `a-z0-9` characters, returning those six.
    fn suffix_after(name: &str, prefix: &str) -> String {
        let suffix = name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('-'))
            .unwrap_or_else(|| panic!("{name:?} should start with '{prefix}-'"));
        assert!(
            suffix.len() == 6
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{name:?} should end in six a-z0-9 characters"
        );
        suffix.to_string()
    }

    let project = "ratect-task-network-only-test";
    let project_filter = format!("label=eu.orican.ratect.project={project}");

    let output = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/task-network-only.yml"))
        .arg("--no-cleanup-after-success")
        .arg("check")
        .env("RUST_LOG", "off")
        .output()
        .expect("failed to run ratect");

    let containers = docker_lines(&["container", "ls", "-aq", "--filter", &project_filter]);
    let networks = docker_lines(&[
        "network",
        "ls",
        "--filter",
        &project_filter,
        "--format",
        "{{.Name}}",
    ]);
    // `(configured container, Docker name, network keys)`.
    let inspected: Vec<(String, String, String)> = containers
        .iter()
        .map(|id| {
            let line = docker_lines(&[
                "container",
                "inspect",
                id,
                "--format",
                "{{index .Config.Labels \"eu.orican.ratect.container\"}} {{.Name}} \
                 {{range $name, $_ := .NetworkSettings.Networks}}{{$name}},{{end}}",
            ])
            .concat();
            let mut fields = line.split(' ');
            let mut field = || fields.next().unwrap_or_default().to_string();
            (field(), field(), field())
        })
        .collect();
    // Torn down before any assertion can fail.
    for id in &containers {
        let _ = Command::new("docker").args(["rm", "-fv", id]).output();
    }
    for network in &networks {
        let _ = Command::new("docker")
            .args(["network", "rm", network])
            .output();
    }

    assert!(
        output.status.success(),
        "the task reaches its dependency by alias:\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(networks.len(), 1, "one task network: {networks:?}");
    let network = &networks[0];
    let suffix = suffix_after(network, &format!("{project}-network"));
    assert_eq!(inspected.len(), 2, "app and database: {inspected:?}");
    for (container, docker_name, container_networks) in &inspected {
        assert_eq!(
            suffix_after(
                docker_name.trim_start_matches('/'),
                &format!("{project}-{container}")
            ),
            suffix,
            "'{container}' shares the network's suffix"
        );
        assert_eq!(
            container_networks,
            &format!("{network},"),
            "'{container}' is on the task network alone, never also `bridge`"
        );
    }
}

/// Serializes the two signal tests below, which share one fixture and so one
/// project label: their `docker ls --filter` queries and their force-cleanup
/// cannot tell one run's containers from the other's, so running them
/// concurrently would surface as "the run left containers behind" rather than
/// as the contention it actually is.
#[cfg(unix)]
static INTERRUPT_PROJECT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Ctrl+C against a real daemon leaves nothing behind, and exits 130.
#[test]
#[ignore]
#[cfg(unix)]
fn interrupting_a_run_cleans_up_via_docker() {
    signalling_a_run_cleans_up_via_docker(&["SIGINT"], 130, None);
}

/// The same for `SIGTERM`, which is what actually bit: `ratect-compat` run as
/// a long-lived subprocess is terminated, not interrupted, when the editor or
/// supervisor that started it closes — and
/// until 0.26.0 that killed the process outright and leaked the container and
/// network every time. Exits 143 (128 + `SIGTERM`), not 130: the two are
/// different events and the exit code says which.
#[test]
#[ignore]
#[cfg(unix)]
fn terminating_a_run_cleans_up_via_docker() {
    signalling_a_run_cleans_up_via_docker(&["SIGTERM"], 143, None);
}

/// A supervisor's escalation — `SIGINT`, then `SIGTERM` 100 ms later, which
/// is how Claude Code stops a stdio MCP server before sending `SIGKILL` 400 ms
/// after that — used to abandon cleanup at the second signal and leave the
/// task's container and network behind on every session end (ratect#249).
/// The second signal now force-removes what's left instead, so nothing is
/// left behind, and it does so quickly: both containers here run `sleep` as
/// PID 1, which ignores `SIGTERM`, so a graceful stop of each takes Docker's
/// full ten-second kill timeout, and a run that cleaned up gracefully would
/// take twenty seconds to exit. Exits 130: the exit code belongs to the
/// signal that ended the run, not the one that hurried its cleanup.
#[test]
#[ignore]
#[cfg(unix)]
fn a_supervisors_escalation_still_cleans_up_via_docker() {
    signalling_a_run_cleans_up_via_docker(
        &["SIGINT", "SIGTERM"],
        130,
        Some(Duration::from_secs(8)),
    );
}

/// Serializes the cleanup-outcome tests below, which share
/// `tests/fixtures/cleanup-outcome.yml`'s project label for the same reason
/// `INTERRUPT_PROJECT` exists.
static CLEANUP_OUTCOME_PROJECT: std::sync::Mutex<()> = std::sync::Mutex::new(());

const CLEANUP_OUTCOME_PROJECT_FILTER: &str =
    "label=eu.orican.ratect.project=ratect-cleanup-outcome-test";

/// `docker <kind> ls -q --filter <filter>`, `-a` when `all`.
fn docker_ids(kind: &str, filter: &str, all: bool) -> Vec<String> {
    let mut arguments = vec![kind, "ls", "-q", "--filter", filter];
    if all {
        arguments.insert(2, "-a");
    }
    let output = Command::new("docker")
        .args(&arguments)
        .output()
        .expect("failed to run docker ls");
    assert!(output.status.success(), "docker {kind} ls failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A create the daemon already had when a sibling's failure dropped it is
/// removed by cleanup (ratect#318) — run twice, and judged by what Docker
/// still has, since a run that leaks nothing and one that cleans up exit the
/// same.
#[test]
#[ignore]
fn a_create_dropped_by_a_siblings_failure_leaves_nothing_behind_via_docker() {
    let filter = "label=eu.orican.ratect.project=ratect-create-leak-test";
    for run in 1..=2 {
        let output = ratect_command()
            .arg("-f")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/create-leak.yml"))
            .args(["-o", "simple", "t"])
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let containers = docker_ids("container", filter, true);
        let networks = docker_ids("network", filter, false);
        for id in &containers {
            let _ = Command::new("docker").args(["rm", "-fv", id]).output();
        }
        for id in &networks {
            let _ = Command::new("docker").args(["network", "rm", id]).output();
        }
        assert!(
            !output.status.success() && stderr.contains("bad"),
            "run {run}: `bad`'s create should fail the run: {stderr}"
        );
        assert!(
            containers.is_empty(),
            "run {run} left containers behind: {containers:?}\n{stderr}"
        );
        assert!(
            networks.is_empty(),
            "run {run} left its network behind: {networks:?}\n{stderr}"
        );
    }
}

/// The commands a `--no-cleanup` notice prints remove everything the run
/// kept when pasted as printed — run twice, the second time under `-o quiet`,
/// which prints the notice too, and judged by what Docker still has —
/// containers and the network by the project's label, and the anonymous
/// volume `-v` exists to remove by name (ratect#308).
#[test]
#[ignore]
fn the_no_cleanup_notices_commands_remove_what_it_kept_via_docker() {
    let filter = "label=eu.orican.ratect.project=ratect-no-cleanup-commands-test";
    for style in ["simple", "quiet"] {
        let output = ratect_command()
            .arg("-f")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/no-cleanup-commands.yml"),
            )
            .args(["-o", style, "--no-cleanup", "check"])
            .env("RUST_LOG", "off")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let commands = stderr
            .lines()
            .find_map(|line| line.split_once(". Remove with: ").map(|(_, tail)| tail))
            .map(|tail| tail.split("; ").map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        // Anonymous volumes carry no Ratect label, so they're found through
        // the containers that own them, before those are removed.
        let volumes: Vec<String> = docker_ids("container", filter, true)
            .iter()
            .flat_map(|id| {
                let output = Command::new("docker")
                    .args([
                        "inspect",
                        "--format",
                        "{{range .Mounts}}{{.Name}} {{end}}",
                        id,
                    ])
                    .output()
                    .expect("failed to run docker inspect");
                String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut pasted = Vec::new();
        for command in &commands {
            let mut words = command.split_whitespace();
            let program = words.next().unwrap_or_default().to_string();
            let status = Command::new(&program)
                .args(words)
                .output()
                .expect("failed to run a printed command")
                .status;
            pasted.push((command.clone(), status.success()));
        }
        let containers = docker_ids("container", filter, true);
        let networks = docker_ids("network", filter, false);
        for id in &containers {
            let _ = Command::new("docker").args(["rm", "-fv", id]).output();
        }
        for id in &networks {
            let _ = Command::new("docker").args(["network", "rm", id]).output();
        }
        let volumes_left: Vec<&String> = volumes
            .iter()
            .filter(|volume| {
                Command::new("docker")
                    .args(["volume", "inspect", volume.as_str()])
                    .output()
                    .is_ok_and(|output| output.status.success())
            })
            .collect();
        for volume in &volumes_left {
            let _ = Command::new("docker")
                .args(["volume", "rm", volume])
                .output();
        }
        assert!(output.status.success(), "-o {style}: {stderr}");
        assert!(
            !volumes.is_empty(),
            "-o {style}: `database` had an anonymous volume to remove, so this proves `-v`"
        );
        assert!(
            volumes_left.is_empty(),
            "-o {style}: the printed commands left anonymous volumes: {volumes_left:?}"
        );
        assert!(
            commands.len() == 2
                && commands[0].starts_with("docker rm -f -v ")
                && commands[1].starts_with("docker network rm "),
            "-o {style}: a container removal, then the network's: {commands:?}\n{stderr}"
        );
        assert!(
            pasted.iter().all(|(_, succeeded)| *succeeded),
            "-o {style}: every printed command succeeds: {pasted:?}"
        );
        assert!(
            stderr.contains("Investigate app with: docker logs ")
                && stderr.contains("Investigate database with: docker logs "),
            "-o {style}: a hint per kept container:\n{stderr}"
        );
        assert!(
            containers.is_empty(),
            "-o {style}: the printed commands left containers: {containers:?}"
        );
        assert!(
            networks.is_empty(),
            "-o {style}: the printed commands left the network: {networks:?}"
        );
    }
}

/// Polls `condition` every 200 ms until it holds or `timeout` passes.
fn poll_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Runs `task` from the cleanup-outcome fixture with a bystander container
/// attached to its network mid-run, so the network's removal is refused, and
/// returns the exit code and stderr. Removes the bystander and the network
/// afterwards, whatever happened.
fn run_with_network_removal_refused(task: &str) -> (Option<i32>, String) {
    let filter = CLEANUP_OUTCOME_PROJECT_FILTER;
    let bystander_label = "ratect-cleanup-outcome-bystander";
    let child = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cleanup-outcome.yml"))
        .args(["-o", "simple", task])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");

    let mut network = None;
    let found = poll_until(Duration::from_secs(120), || {
        network = docker_ids("network", filter, false).into_iter().next();
        network.is_some()
    });
    let attached = found
        && Command::new("docker")
            .args(["run", "-d", "--label", bystander_label, "--network"])
            .arg(network.as_deref().unwrap())
            .args(["alpine:3.18", "sleep", "120"])
            .output()
            .expect("failed to run docker run")
            .status
            .success();

    let output = child.wait_with_output().expect("failed to wait on ratect");
    for id in docker_ids("container", &format!("label={bystander_label}"), true) {
        let _ = Command::new("docker").args(["rm", "-fv", &id]).output();
    }
    for id in docker_ids("container", filter, true) {
        let _ = Command::new("docker").args(["rm", "-fv", &id]).output();
    }
    for id in docker_ids("network", filter, false) {
        let _ = Command::new("docker").args(["network", "rm", &id]).output();
    }
    assert!(
        attached,
        "the test never attached a bystander to the task network, so proved nothing"
    );
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A passing task whose network removal the daemon refuses exits 125 rather
/// than 0 — #316's reproduction, against a real daemon (decisions/0016).
#[test]
#[ignore]
fn a_passing_run_whose_cleanup_fails_exits_125_via_docker() {
    let _shared_project = CLEANUP_OUTCOME_PROJECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let (code, stderr) = run_with_network_removal_refused("pass");

    assert_eq!(code, Some(125), "stderr:\n{stderr}");
    // The engine's own line, quoting the daemon and naming what was left and
    // how to remove it — and only that line: `main` prints no second
    // `Error:` for the `CleanupFailed` it exits `125` with (decisions/0017).
    assert!(
        stderr.contains("Error: Cleanup failed: ")
            && stderr.contains("left behind: network ratect-cleanup-outcome-test-network-")
            && stderr
                .contains(". Remove with: docker network rm ratect-cleanup-outcome-test-network-"),
        "the failure says what went wrong and what to do:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("Error:").count(),
        1,
        "one cleanup failure, one Error: line:\n{stderr}"
    );
}

/// The same refusal after a task that exited 3 leaves the 3 alone: a task's
/// own non-zero code is never overridden.
#[test]
#[ignore]
fn a_failing_run_whose_cleanup_fails_keeps_its_own_code_via_docker() {
    let _shared_project = CLEANUP_OUTCOME_PROJECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let (code, stderr) = run_with_network_removal_refused("fail");

    assert_eq!(code, Some(3), "stderr:\n{stderr}");
}

/// A signal that arrives only during the cleanup of a passing run hurries
/// that cleanup and ends nothing, so the run still exits 0 — not 130, as it
/// did before decisions/0016 — and leaves nothing behind.
#[test]
#[ignore]
#[cfg(unix)]
fn a_signal_during_a_passing_runs_cleanup_still_exits_0_via_docker() {
    let _shared_project = CLEANUP_OUTCOME_PROJECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let filter = CLEANUP_OUTCOME_PROJECT_FILTER;
    let mut child = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cleanup-outcome.yml"))
        .args(["-o", "simple", "finish"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");
    let force_cleanup = || {
        for id in docker_ids("container", filter, true) {
            let _ = Command::new("docker").args(["rm", "-fv", &id]).output();
        }
        for id in docker_ids("network", filter, false) {
            let _ = Command::new("docker").args(["network", "rm", &id]).output();
        }
    };

    // Both running, then only the dependency left at all: the task's own
    // container has been *removed*, which only cleanup does, so the run has
    // already ended and cleanup is stopping the dependency, which takes ten
    // seconds. Merely exited isn't enough — the engine may not have seen the
    // exit yet, and a signal then would end the run instead.
    let started = poll_until(Duration::from_secs(120), || {
        docker_ids("container", filter, false).len() >= 2
    });
    let cleaning_up = started
        && poll_until(Duration::from_secs(30), || {
            docker_ids("container", filter, true).len() == 1
        });
    if !cleaning_up {
        let _ = child.kill();
        let _ = child.wait();
        force_cleanup();
        panic!("the run never reached its cleanup with the dependency still running");
    }
    let signalled = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("failed to run kill");
    assert!(signalled.success(), "could not send SIGINT to ratect");

    let mut status = None;
    let exited = poll_until(Duration::from_secs(60), || {
        status = child.try_wait().expect("failed to poll ratect");
        status.is_some()
    });
    if !exited {
        let _ = child.kill();
    }
    let output = child.wait_with_output().expect("failed to wait on ratect");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let leftovers = (
        docker_ids("container", filter, true),
        docker_ids("network", filter, false),
    );
    force_cleanup();

    assert_eq!(status.and_then(|s| s.code()), Some(0), "stderr:\n{stderr}");
    assert!(
        leftovers.0.is_empty() && leftovers.1.is_empty(),
        "the forced cleanup completes: {leftovers:?}"
    );
}

/// Signalling a real run against a real daemon leaves nothing behind.
///
/// `ratect-core`'s own unit tests prove the engine's *decisions* against a fake
/// runtime — that a signal abandons the run, that cleanup still happens,
/// that `--no-cleanup-after-failure` suppresses it. What they can't prove is
/// that a real signal, delivered to a real process mid-run, actually reaches
/// the handler and that the daemon ends up clean: the fake can't diverge from
/// reality because it *is* the reality under test. This closes that gap.
///
/// Unix-only because it sends a signal. Ratect's own testing is Unix-only
/// anyway (see `ratect-core/src/user.rs`), but this one is explicit about it
/// since the mechanism, not just the environment, is what's unavailable.
#[cfg(unix)]
///
/// `signals` are sent 100 ms apart — a supervisor's spacing, and a person's
/// second Ctrl+C is never faster. `must_exit_within`, when given, is measured
/// from the first signal and is how the escalation test tells a forced
/// cleanup from a graceful one that merely finished.
#[cfg(unix)]
fn signalling_a_run_cleans_up_via_docker(
    signals: &[&str],
    expected_exit_code: i32,
    must_exit_within: Option<Duration>,
) {
    let signal = signals.join(" then ");
    let _shared_project = INTERRUPT_PROJECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // `tests/fixtures/interrupt.yml` has a project name of its own so these
    // filters can't match a concurrently-running test's containers.
    let project_filter = "label=eu.orican.ratect.project=ratect-interrupt-test";

    fn ids(kind: &str, filter: &str, all: bool) -> Vec<String> {
        let mut arguments = vec![kind, "ls", "-q", "--filter", filter];
        if all {
            arguments.insert(2, "-a");
        }
        let output = Command::new("docker")
            .args(&arguments)
            .output()
            .expect("failed to run docker ls");
        assert!(output.status.success(), "docker {kind} ls failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Polls until `condition` holds, or gives up. Every wait here is on a
    /// real daemon doing real work (pulling, starting, stopping), so a fixed
    /// sleep would be either flaky or needlessly slow.
    fn wait_until(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("timed out waiting for {what}");
        false
    }

    // `RUST_LOG=off`: the interrupt notice asserted below is printed, not
    // logged, so the one line a person at the keyboard must read survives
    // any filter (decisions/0017; ratect#369). Stderr is read after exit:
    // the few lines a signalled run writes never fill the pipe.
    let mut child = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interrupt.yml"))
        .arg("wait")
        .env("RUST_LOG", "off")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");

    // Whatever happens below, this test must not leave the daemon full of its
    // own containers — including when an assertion fails or the run never got
    // as far as being interruptible.
    let force_cleanup = || {
        for id in ids("container", project_filter, true) {
            let _ = Command::new("docker").args(["rm", "-fv", &id]).output();
        }
        for id in ids("network", project_filter, false) {
            let _ = Command::new("docker").args(["network", "rm", &id]).output();
        }
    };

    // Both containers running is the point at which there is genuinely
    // something to clean up: the dependency *and* the task's own container.
    // Generous, because a cold daemon may be pulling alpine first.
    let started = wait_until("both containers to start", Duration::from_secs(120), || {
        ids("container", project_filter, false).len() >= 2
    });
    if !started {
        let _ = child.kill();
        let _ = child.wait();
        force_cleanup();
        panic!("the run never reached two running containers, so nothing was interrupted");
    }

    let first_signal_at = Instant::now();
    for (index, signal) in signals.iter().enumerate() {
        if index > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        // `kill` names a signal without its `SIG` prefix; every message here
        // names it the way the docs and the user do.
        let kill_flag = format!("-{}", signal.trim_start_matches("SIG"));
        let signalled = Command::new("kill")
            .args([&kill_flag, &child.id().to_string()])
            .status()
            .expect("failed to run kill");
        assert!(signalled.success(), "could not send {signal} to ratect");
    }

    // `try_wait` rather than `wait`, so a regression that hangs the process
    // fails this test instead of hanging CI until its own job timeout.
    let mut status = None;
    let exited = wait_until("ratect to exit", Duration::from_secs(120), || {
        status = child.try_wait().expect("failed to poll ratect");
        status.is_some()
    });
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        force_cleanup();
        panic!("ratect did not exit after {signal}");
    }
    let status = status.expect("exited implies a status");
    let exit_took = first_signal_at.elapsed();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr was piped")
        .read_to_string(&mut stderr)
        .expect("failed to read ratect's stderr");

    // Cleanup is synchronous before exit, so by here the daemon should already
    // be clean — but stopping two containers is real work against a real
    // daemon, so this is still polled rather than asserted outright.
    let cleaned = wait_until(
        "containers and network to go",
        Duration::from_secs(60),
        || {
            ids("container", project_filter, true).is_empty()
                && ids("network", project_filter, false).is_empty()
        },
    );

    let leftover_containers = ids("container", project_filter, true);
    let leftover_networks = ids("network", project_filter, false);
    force_cleanup();

    assert_eq!(
        status.code(),
        Some(expected_exit_code),
        "a run ended by {signal} should exit {expected_exit_code} (128 + the signal), \
         not a generic failure code"
    );
    assert!(
        cleaned,
        "a run ended by {signal} must remove its containers and network; left behind \
         containers {leftover_containers:?} and networks {leftover_networks:?}"
    );
    if let Some(limit) = must_exit_within {
        assert!(
            exit_took <= limit,
            "a run ended by {signal} should force-remove rather than wait out each \
             container's kill timeout, but took {exit_took:?} to exit (limit {limit:?})"
        );
    }
    // The notice names what ended the run and the next step, and the run
    // ends on the exit code — never on a trailing `Error: Interrupted`.
    let (ended_run, send_again) = match signals[0] {
        "SIGINT" => ("Interrupted; cleaning up.", "Press Ctrl+C again"),
        "SIGTERM" => ("Terminated by SIGTERM; cleaning up.", "Send SIGTERM again"),
        other => panic!("no expected notice for {other}"),
    };
    assert!(
        stderr.contains(ended_run) && stderr.contains(send_again),
        "the interrupt notice must be printed whatever RUST_LOG says:\n{stderr}"
    );
    if signals.len() > 1 {
        assert!(
            stderr.contains("during cleanup; force-removing what's left."),
            "the escalation notice too:\n{stderr}"
        );
    }
    assert!(
        !stderr.contains("Error:"),
        "a signalled run that cleaned up is no error of Ratect's:\n{stderr}"
    );
    // And the sequence ends by saying cleanup is over (ratect#369).
    assert!(
        stderr.ends_with("Cleanup complete.\n"),
        "the interrupt sequence's last line:\n{stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// A run whose stdout reader has gone exits 141 and leaves nothing behind
/// (ratect#670), in both the style that writes container output itself
/// (`simple`) and the one whose logger does (`all`). Stdin is a pipe held
/// open throughout, so a run that forwards it has a blocked read in flight —
/// the read that used to keep a panicked process alive.
#[test]
#[ignore]
fn a_run_whose_stdout_closed_exits_141_and_cleans_up_via_docker() {
    let project_filter = "label=eu.orican.ratect.project=ratect-stdout-closed-test";

    fn ids(kind: &str, filters: &[&str], all: bool) -> Vec<String> {
        let mut arguments = vec![kind, "ls", "-q"];
        if all {
            arguments.push("-a");
        }
        for filter in filters {
            arguments.extend(["--filter", filter]);
        }
        let output = Command::new("docker")
            .args(&arguments)
            .output()
            .expect("failed to run docker ls");
        assert!(output.status.success(), "docker {kind} ls failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }
    let force_cleanup = || {
        for id in ids("container", &[project_filter], true) {
            let _ = Command::new("docker").args(["rm", "-fv", &id]).output();
        }
        for id in ids("network", &[project_filter], false) {
            let _ = Command::new("docker").args(["network", "rm", &id]).output();
        }
    };

    for style in ["simple", "all"] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut child = ratect_command()
            .arg("-f")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdout-closed.yml"))
            .args(["-o", style, "print"])
            .stdin(Stdio::piped())
            .stdout(writer)
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn ratect");
        let _stdin = child.stdin.take();

        // `try_wait` rather than `wait`, so a hang fails this test instead of
        // hanging CI. Generous: a cold daemon may be pulling alpine, and the
        // task container ignores the graceful stop's `SIGTERM`.
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut status = None;
        while status.is_none() && Instant::now() < deadline {
            status = child.try_wait().expect("failed to poll ratect");
            std::thread::sleep(Duration::from_millis(200));
        }
        let Some(status) = status else {
            let _ = child.kill();
            let _ = child.wait();
            force_cleanup();
            panic!("-o {style}: ratect did not exit after its stdout closed");
        };
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("stderr was piped")
            .read_to_string(&mut stderr)
            .expect("failed to read ratect's stderr");

        let left = |kind: &str, filters: &[&str], all: bool| {
            let mut filters = filters.to_vec();
            filters.push(project_filter);
            ids(kind, &filters, all)
        };
        let task_container = left("container", &["label=eu.orican.ratect.container=app"], true);
        let dependency = left(
            "container",
            &["label=eu.orican.ratect.container=database"],
            true,
        );
        let networks = left("network", &[], false);
        force_cleanup();

        assert_eq!(
            status.code(),
            Some(141),
            "-o {style}: a run whose stdout closed exits 128 + SIGPIPE:\n{stderr}"
        );
        assert!(
            task_container.is_empty(),
            "-o {style}: the task container was left behind: {task_container:?}"
        );
        assert!(
            dependency.is_empty(),
            "-o {style}: the dependency was left behind: {dependency:?}"
        );
        assert!(
            networks.is_empty(),
            "-o {style}: the task network was left behind: {networks:?}"
        );
        assert!(
            stderr.contains("Stdout closed; cleaning up."),
            "-o {style}: the notice says why the run stopped:\n{stderr}"
        );
        assert!(
            !stderr.contains("panicked") && !stderr.contains("Error:"),
            "-o {style}: a closed stdout is neither a crash nor an error:\n{stderr}"
        );
    }
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
    // The counter (not just pid+nanos) is load-bearing: two `--ignored`
    // tests race to read `SystemTime::now()` under `cargo test`'s default
    // parallelism, and on a coarse enough clock two threads can read the
    // same nanosecond — colliding on this test's own scratch path.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let test_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
      - command: sh -c "test -f /tmp/now-healthy && touch /scratch/setup-ran"
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
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Image work doesn't wait for a dependency's readiness (ratect#312,
/// decisions/0019): the task container's image is pulled alongside the
/// dependency's startup, so a pull that fails ends the run before the
/// dependency's setup command has had its effect. The setup command here
/// sleeps for two minutes and then drops a marker onto a bind-mounted
/// scratch directory; the pull of an image under a `.invalid` hostname
/// fails in well under that (a name that can't resolve — a daemon whose
/// proxy or mirror accepts the connection and times out instead would
/// need longer), and cleanup then stops the container, sleep and all.
/// Under the old order the pull wasn't attempted until the marker
/// existed.
///
/// Run twice in one test, since a scratch directory that starts empty
/// passes the first run whatever the order (AGENTS.md guideline 15).
#[test]
#[ignore]
fn a_failed_task_image_pull_stops_a_dependency_before_its_setup_command_via_docker() {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let test_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let scratch_dir = std::env::temp_dir().join(format!("ratect-image-order-test-{test_id}"));
    std::fs::create_dir_all(&scratch_dir).unwrap();
    let config_path = std::env::temp_dir().join(format!("ratect-image-order-test-{test_id}.yml"));

    let config = format!(
        r#"
project_name: ratect-image-order-test
containers:
  dep:
    image: alpine:3.18.2
    command: sh -c 'trap "exit 0" TERM; while :; do sleep 1; done'
    setup_commands:
      - command: sh -c "sleep 120 && touch /scratch/setup-ran"
    volumes:
      - {scratch}:/scratch
  app:
    image: ratect-image-order-test.invalid/nope:does-not-exist
    dependencies:
      - dep
tasks:
  check:
    run:
      container: app
      command: echo unreachable
"#,
        scratch = scratch_dir.display()
    );
    std::fs::write(&config_path, &config).expect("failed to write temp config");

    let cleanup = || {
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&config_path);
    };

    for run in 1..=2 {
        let output = ratect_command()
            .arg("-f")
            .arg(&config_path)
            .arg("check")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() || !stderr.contains("Failed to pull image") {
            cleanup();
            panic!(
                "run {run} should have failed on the pull: status {}\nstdout:\n{}\nstderr:\n{stderr}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
            );
        }
        let marker = scratch_dir.join("setup-ran");
        if marker.exists() {
            cleanup();
            panic!(
                "run {run}: the dependency's setup command should not have completed before \
                 the task image's pull failed\nstdout:\n{}\nstderr:\n{stderr}",
                String::from_utf8_lossy(&output.stdout),
            );
        }
    }

    cleanup();
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Creation doesn't wait for a dependency's readiness either (ratect#603,
/// decisions/0019): the task container is created alongside the
/// dependency's startup, so a create the daemon refuses ends the run before
/// the dependency's setup command has had its effect. Same shape as the
/// pull test above — the setup command sleeps for two minutes and then
/// drops a marker onto a bind-mounted scratch directory — with a task
/// container whose `log_driver` names a plugin the daemon doesn't have,
/// which Docker refuses at `create` ("error looking up logging plugin"),
/// not at start. (A device that doesn't exist, the obvious alternative, is
/// refused at *start*: `docker create --device /dev/nope` succeeds, checked
/// against Docker 29.) Under the old order the create wasn't attempted
/// until the marker existed.
///
/// Run twice in one test, since a scratch directory that starts empty
/// passes the first run whatever the order (AGENTS.md guideline 15).
#[test]
#[ignore]
fn a_refused_task_container_create_stops_a_dependency_before_its_setup_command_via_docker() {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let test_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let scratch_dir = std::env::temp_dir().join(format!("ratect-create-order-test-{test_id}"));
    std::fs::create_dir_all(&scratch_dir).unwrap();
    let config_path = std::env::temp_dir().join(format!("ratect-create-order-test-{test_id}.yml"));

    let config = format!(
        r#"
project_name: ratect-create-order-test
containers:
  dep:
    image: alpine:3.18.2
    command: sh -c 'trap "exit 0" TERM; while :; do sleep 1; done'
    setup_commands:
      - command: sh -c "sleep 120 && touch /scratch/setup-ran"
    volumes:
      - {scratch}:/scratch
  app:
    image: alpine:3.18.2
    log_driver: ratect-create-order-test-no-such-plugin
    dependencies:
      - dep
tasks:
  check:
    run:
      container: app
      command: echo unreachable
"#,
        scratch = scratch_dir.display()
    );
    std::fs::write(&config_path, &config).expect("failed to write temp config");

    let cleanup = || {
        let _ = std::fs::remove_dir_all(&scratch_dir);
        let _ = std::fs::remove_file(&config_path);
    };

    for run in 1..=2 {
        let output = ratect_command()
            .arg("-f")
            .arg(&config_path)
            .arg("check")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() || !stderr.contains("Failed to create container 'app'") {
            cleanup();
            panic!(
                "run {run} should have failed on the create: status {}\nstdout:\n{}\nstderr:\n{stderr}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
            );
        }
        let marker = scratch_dir.join("setup-ran");
        if marker.exists() {
            cleanup();
            panic!(
                "run {run}: the dependency's setup command should not have completed before \
                 the task container's create was refused\nstdout:\n{}\nstderr:\n{stderr}",
                String::from_utf8_lossy(&output.stdout),
            );
        }
    }

    cleanup();
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The other half of the readiness gate (0.21.0): the task's *own*
/// container's `setup_commands` run too, concurrently with its main command
/// — see `tests/fixtures/task-container-setup-commands.yml` for why the
/// marker file it checks for can only exist if the setup command really
/// exec'd into the still-running container mid-command.
///
/// Doubles as the end-to-end proof that the task container's own readiness
/// milestones stay *out* of its output in the `simple` style (see
/// `SimpleEventLogger::is_task_container`) — `task_output`'s exact match
/// would pick up any stray "app has become healthy."/"Running setup command
/// ..." line landing in that window.
#[test]
#[ignore]
fn task_containers_own_setup_commands_run_via_docker() {
    let output = ratect_command()
        .arg("-f")
        .arg(task_container_setup_commands_config_path())
        .arg("check")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(task_output(&stdout, "app"), "SETUP-RAN-DURING-TASK");
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A main command that exits 0 before its own container's readiness gate
/// reaches a verdict succeeds (ratect#248, ratect#263) — see
/// `tests/fixtures/task-container-stopped-before-gate.yml`. The health-check
/// task is the deterministic one; whether the setup command runs or is
/// refused depends on how fast `true` exits, and either way the task
/// succeeds. A setup command still running when the main command exits is
/// killed with the container, and the task succeeds with nothing left
/// behind.
#[test]
#[ignore]
fn a_main_command_exiting_0_before_its_gate_reaches_a_verdict_succeeds_via_docker() {
    for task in ["health-check", "setup-command", "setup-command-killed"] {
        let output = ratect_command()
            .arg("-f")
            .arg(task_container_stopped_before_gate_config_path())
            .arg(task)
            .output()
            .expect("failed to run ratect");

        assert!(
            output.status.success(),
            "{task}: stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let project_filter =
        "label=eu.orican.ratect.project=ratect-task-container-stopped-before-gate-test";
    for arguments in [
        ["container", "ls", "-aq", "--filter", project_filter],
        ["network", "ls", "-q", "--filter", project_filter],
    ] {
        let output = Command::new("docker")
            .args(arguments)
            .output()
            .expect("failed to run docker ls");
        assert!(output.status.success(), "docker {} ls failed", arguments[0]);
        let left = String::from_utf8_lossy(&output.stdout);
        assert!(
            left.trim().is_empty(),
            "no {} should be left behind: {left}",
            arguments[0]
        );
    }
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A task container's failing setup command cancels its still-running main
/// command (ratect#260) — see `tests/fixtures/task-container-gate-failure.yml`,
/// whose main command never exits on its own. The deadline allows for a cold
/// pull and the container's stop timeout; without the cancellation the run
/// would outlast any deadline.
#[test]
#[ignore]
fn a_task_containers_failing_setup_command_cancels_its_main_command_via_docker() {
    let project_filter = "label=eu.orican.ratect.project=ratect-task-container-gate-failure-test";
    let ids = |kind: &str, all: bool| {
        let mut arguments = vec![kind, "ls", "-q", "--filter", project_filter];
        if all {
            arguments.insert(2, "-a");
        }
        let output = Command::new("docker")
            .args(&arguments)
            .output()
            .expect("failed to run docker ls");
        assert!(output.status.success(), "docker {kind} ls failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    let mut child = ratect_command()
        .arg("-f")
        .arg(task_container_gate_failure_config_path())
        .arg("check")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");
    // Read as it arrives, so a cold pull's progress can't fill the pipe and
    // stall the run into looking like a missing cancellation.
    let mut child_stderr = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut stderr = String::new();
        child_stderr.read_to_string(&mut stderr).unwrap();
        stderr
    });
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("failed to wait on ratect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        for id in ids("container", true) {
            let _ = Command::new("docker").args(["rm", "-fv", &id]).output();
        }
        for id in ids("network", false) {
            let _ = Command::new("docker").args(["network", "rm", &id]).output();
        }
        panic!("the gate's failure should have cancelled the main command");
    };
    let stderr = stderr.join().unwrap();

    assert_eq!(status.code(), Some(125), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Setup command 'false'"),
        "the setup command's failure should be the reported error:\n{stderr}"
    );
    assert_eq!(
        ids("container", true),
        Vec::<String>::new(),
        "the cancelled task container should have been removed"
    );
    assert_eq!(
        ids("network", false),
        Vec::<String>::new(),
        "the task network should have been removed"
    );
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

/// Requires a running Docker daemon with network access to pull
/// `redis:7-alpine` and `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// An infrastructure failure is reported when it happens — before cleanup
/// says anything — and exactly once, as Batect reports a failure the moment
/// it's posted (ratect#334). Stdout and stderr share one file here, so the
/// order between them is the order a terminal would show.
#[test]
#[ignore]
fn an_infrastructure_failure_is_reported_once_before_cleanup_via_docker() {
    let merged = std::env::temp_dir().join(format!(
        "ratect-compat-failure-order-{}.log",
        std::process::id()
    ));
    let file = std::fs::File::create(&merged).unwrap();
    let status = ratect_command()
        .arg("-f")
        .arg(unhealthy_dependency_config_path())
        .args(["-o", "simple", "check"])
        .env("RUST_LOG", "off")
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .status()
        .expect("failed to run ratect");
    let output = std::fs::read_to_string(&merged).unwrap();
    std::fs::remove_file(&merged).unwrap();
    assert_eq!(status.code(), Some(125), "{output}");
    assert_eq!(output.matches("Error:").count(), 1, "{output}");
    let error_at = output.find("Error:").unwrap();
    let cleanup_at = output.find("Cleaning up").expect(&output);
    assert!(
        error_at < cleanup_at,
        "the failure is reported before cleanup:\n{output}"
    );

    let quiet = ratect_command()
        .arg("-f")
        .arg(unhealthy_dependency_config_path())
        .args(["-o", "quiet", "check"])
        .env("RUST_LOG", "off")
        .output()
        .expect("failed to run ratect");
    let stderr = String::from_utf8_lossy(&quiet.stderr);
    assert_eq!(String::from_utf8_lossy(&quiet.stdout), "");
    assert!(
        stderr.starts_with("Error: ") && stderr.matches("Error:").count() == 1,
        "quiet prints the one error and nothing else:\n{stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A container the daemon can't create, set up for `run_as_current_user`, or
/// start — the task's own or a dependency — fails the task with an error
/// naming that container as configured, never by Docker id alone, with the
/// daemon's own reason still in it (ratect#345). Nothing is left behind.
#[test]
#[ignore]
fn a_container_that_cannot_be_created_or_started_is_named_via_docker() {
    let config =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container-setup-failures.yml");
    let filter = "label=eu.orican.ratect.project=ratect-container-setup-failures-test";
    for (task, container, says, cause) in [
        (
            "task-create",
            "bad-log-driver",
            "Failed to create container 'bad-log-driver'",
            "does-not-exist-345",
        ),
        (
            "dependency-create",
            "bad-log-driver",
            "Failed to create container 'bad-log-driver'",
            "does-not-exist-345",
        ),
        (
            "task-user-mapping",
            "bad-home",
            "Failed to apply run_as_current_user to container 'bad-home'",
            "/does-not-exist-345/home",
        ),
        (
            "dependency-user-mapping",
            "bad-home",
            "Failed to apply run_as_current_user to container 'bad-home'",
            "/does-not-exist-345/home",
        ),
        (
            "task-start",
            "bad-device",
            "Failed to start container 'bad-device'",
            "/dev/does-not-exist-345",
        ),
        (
            "dependency-start",
            "bad-device",
            "Failed to start container 'bad-device'",
            "/dev/does-not-exist-345",
        ),
    ] {
        let output = ratect_command()
            .arg("-f")
            .arg(&config)
            .args(["-o", "quiet", task])
            .env("RUST_LOG", "off")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(125), "{task}:\n{stderr}");
        assert!(
            stderr.starts_with(&format!("Error: {says}")),
            "{task} names {container} first:\n{stderr}"
        );
        assert_eq!(
            stderr.matches(&format!("'{container}'")).count(),
            1,
            "{task} names it once:\n{stderr}"
        );
        assert!(
            stderr.contains(cause),
            "{task} keeps the daemon's reason:\n{stderr}"
        );
        assert!(
            !stderr.contains("sidecar"),
            "{task} calls a dependency a container:\n{stderr}"
        );
        assert!(
            docker_ids("container", filter, true).is_empty()
                && docker_ids("network", filter, false).is_empty(),
            "{task} left something behind"
        );
    }
}

/// Requires a running Docker daemon with network access to pull
/// `redis:7-alpine` and `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// A container that doesn't become ready — a setup command that fails, a
/// health check that never passes — ends its error with the hint to re-run
/// with `--no-cleanup-after-failure`, as Batect's does, but only while that
/// flag would change something (ratect#368).
#[test]
#[ignore]
fn a_readiness_failure_hints_no_cleanup_after_failure_via_docker() {
    const HINT: &str = "You can re-run the task with --no-cleanup-after-failure to leave its \
                        containers in place and diagnose the failure.";
    let run = |config: PathBuf, flags: &[&str]| {
        let output = ratect_command()
            .arg("-f")
            .arg(config)
            .args(["-o", "quiet"])
            .args(flags)
            .arg("check")
            .output()
            .expect("failed to run ratect");
        assert_eq!(output.status.code(), Some(125));
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let stderr = run(failing_setup_command_config_path(), &[]);
    assert!(
        stderr.contains("seeding failed") && stderr.trim_end().ends_with(HINT),
        "the setup command's output, then the hint:\n{stderr}"
    );
    let stderr = run(unhealthy_dependency_config_path(), &[]);
    assert!(
        stderr.trim_end().ends_with(HINT),
        "an unhealthy dependency is hinted too:\n{stderr}"
    );

    let stderr = run(
        failing_setup_command_config_path(),
        &["--no-cleanup-after-failure"],
    );
    assert!(
        !stderr.contains(HINT),
        "nothing to suggest once the flag is given:\n{stderr}"
    );
    // That run kept its containers and network on purpose.
    let filter = "label=eu.orican.ratect.project=ratect-failing-setup-command-test";
    for id in docker_ids("container", filter, true) {
        Command::new("docker")
            .args(["rm", "-f", "-v", &id])
            .output()
            .expect("failed to run docker rm");
    }
    for id in docker_ids("network", filter, false) {
        Command::new("docker")
            .args(["network", "rm", &id])
            .output()
            .expect("failed to run docker network rm");
    }
    assert!(docker_ids("container", filter, true).is_empty());
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
/// Mirrors Batect's `SessionRunnerSpec`, "the task has a prerequisite" /
/// "the dependency finishes with an exit code of 0": "returns the exit code
/// of the main task".
#[test]
#[ignore]
fn main_task_exit_code_wins_over_a_successful_prerequisite() {
    let output = ratect_command()
        .arg("-f")
        .arg(exit_code_config_path())
        .arg("fails-after-prerequisite")
        .output()
        .expect("failed to run ratect");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(42),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("succeeds finished with exit code 0 in ")),
        "the prerequisite must have run:\n{stdout}"
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
    assert_eq!(task_output(&stdout, "build-env"), "args: foo bar baz");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// #399: with no `command`, `-- ADDITIONAL_ARGS` replace the image's default
/// `CMD`, and `RUST_LOG=debug` names the one replaced. Logged only then:
/// not when a `command` is set, and not at the default log level.
#[test]
#[ignore]
fn additional_args_replacing_the_image_cmd_are_logged_at_debug_level() {
    const LOGGED: &str = "additional arguments replace the image's default CMD";
    let run = |task: &str, rust_log: Option<&str>| {
        let mut command = ratect_command();
        command.env_remove("RUST_LOG");
        if let Some(rust_log) = rust_log {
            command.env("RUST_LOG", rust_log);
        }
        let output = command
            .arg("-f")
            .arg(additional_args_config_path())
            .args(["-o", "simple", task, "--", "echo", "replaced"])
            .output()
            .expect("failed to run ratect");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "stderr:\n{stderr}");
        (task_output(&stdout, "build-env"), stderr)
    };

    let (output, stderr) = run("no-command", Some("debug"));
    assert_eq!(output, "replaced");
    assert!(
        stderr.contains(LOGGED) && stderr.contains(r#"replaced_cmd=["/bin/sh"]"#),
        "{stderr}"
    );

    let (output, stderr) = run("no-command", None);
    assert_eq!(output, "replaced");
    assert!(!stderr.contains(LOGGED), "{stderr}");

    let (output, stderr) = run("echo-args", Some("debug"));
    assert_eq!(output, "args: echo replaced");
    assert!(!stderr.contains(LOGGED), "{stderr}");
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
        task_output(&stdout, "build-env"),
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
    assert_eq!(
        task_output(&stdout, "build-env"),
        "GREETING=hello-fallback ENV_NAME=from-file"
    );
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
    assert_eq!(task_output(&stdout, "build-env"), "/tmp");
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
    assert_eq!(task_output(&stdout, "build-env"), "/var");
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
    assert_eq!(task_output(&stdout, "build-env"), "hello-from-sh-c");
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
    assert_eq!(task_output(&stdout, "build-env"), "override-worked");
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
    assert_eq!(task_output(&stdout, "normal"), "chown-worked");
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
    let cap_eff = |task: &str, container: &str| {
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
        let stdout = task_output(&String::from_utf8_lossy(&output.stdout), container);
        let hex = stdout
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("unexpected CapEff line: {stdout}"));
        u64::from_str_radix(hex, 16)
            .unwrap_or_else(|e| panic!("failed to parse CapEff '{hex}': {e}"))
    };

    let normal = cap_eff("show-caps-normal", "normal");
    let privileged = cap_eff("show-caps-privileged", "root-privileged");

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

    let stdout = task_output(&String::from_utf8_lossy(&output.stdout), "custom-shm");
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
/// Proves a `tmpfs` volume mount reaches the real container: `size=64m` must
/// make the mount's actual `df` size exactly 65536 1K-blocks (64 * 1024),
/// same proof shape as `shm_size_reaches_the_real_container`.
#[test]
#[ignore]
fn tmpfs_mount_reaches_the_real_container() {
    let output = ratect_command()
        .arg("-f")
        .arg(tmpfs_config_path())
        .arg("print-tmpfs-size")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = task_output(&String::from_utf8_lossy(&output.stdout), "build-env");
    let blocks: u64 = stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("unexpected df output: {stdout}"))
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse df output '{stdout}': {e}"));

    assert_eq!(blocks, 64 * 1024, "df output:\n{stdout}");
}

fn container_defaults_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container-defaults.yml")
}

fn local_volumes_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local-volumes.yml")
}

/// Runs `task` from the config at `config`, returning whether it succeeded
/// and its output — stdout's task section, then all of stderr.
fn run_fixture_task(config: PathBuf, task: &str, container: &str) -> (bool, String, String) {
    let output = ratect_command()
        .arg("-f")
        .arg(config)
        .arg(task)
        .output()
        .expect("failed to run ratect");
    (
        output.status.success(),
        task_output(&String::from_utf8_lossy(&output.stdout), container),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Batect's `DockerClientContainerManagementSpec`, a container with no
/// working directory, user or shm size set: it runs in the image's `/`, as
/// root, with the daemon's default `/dev/shm` — Docker's 64 MiB unless the
/// daemon sets its own, so it's compared with a plain `docker run`'s, which
/// needs the `docker` CLI too.
#[test]
#[ignore]
fn a_container_setting_nothing_gets_the_image_and_daemon_defaults() {
    let (success, stdout, stderr) =
        run_fixture_task(container_defaults_config_path(), "print-defaults", "plain");

    assert!(success, "stderr:\n{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    let [working_directory, uid, gid, shm] = lines[..] else {
        panic!("unexpected output:\n{stdout}");
    };
    assert_eq!((working_directory, uid, gid), ("/", "0", "0"), "{stdout}");
    let daemon_default = Command::new("docker")
        .args([
            "run",
            "--rm",
            "alpine:3.18.2",
            "sh",
            "-c",
            "df -k /dev/shm | tail -1",
        ])
        .output()
        .expect("failed to run docker");
    assert!(
        daemon_default.status.success(),
        "docker run failed:\n{}",
        String::from_utf8_lossy(&daemon_default.stderr)
    );
    let daemon_default = String::from_utf8_lossy(&daemon_default.stdout);
    assert_eq!(
        shm.split_whitespace().nth(1),
        daemon_default.split_whitespace().nth(1),
        "ratect: {shm}\ndocker run: {daemon_default}"
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Batect's `DockerClientContainerManagementSpec`, no entrypoint override:
/// the image's own `ENTRYPOINT` runs, with the task's command as its
/// arguments.
#[test]
#[ignore]
fn a_container_setting_no_entrypoint_runs_the_images_own() {
    let (success, stdout, stderr) = run_fixture_task(
        container_defaults_config_path(),
        "run-own-entrypoint",
        "own-entrypoint",
    );

    assert!(success, "stderr:\n{stderr}");
    assert_eq!(stdout, "from-the-entrypoint from-the-command", "{stderr}");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Batect's `DockerClientContainerManagementSpec`, a host file mounted into
/// a container: the container reads the file's contents at its path.
#[test]
#[ignore]
fn a_mounted_host_file_is_readable_in_the_container() {
    let (success, stdout, stderr) =
        run_fixture_task(local_volumes_config_path(), "read-file", "mounts");

    assert!(success, "stderr:\n{stderr}");
    assert_eq!(stdout, "from the host");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Batect's `DockerClientContainerManagementSpec`, a file and a directory
/// mounted with options `ro`: writing to either fails as a read-only file
/// system, and the host file is untouched. Batect's own check for the file
/// removes it, which fails as `Resource busy` for any mounted file, `ro` or
/// not, so this appends to it instead.
#[test]
#[ignore]
fn a_read_only_mount_refuses_writes() {
    let host_file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local-volumes/file.txt");
    for task in ["write-ro-file", "write-ro-dir"] {
        let (success, stdout, stderr) =
            run_fixture_task(local_volumes_config_path(), task, "mounts");

        assert!(!success, "{task} should fail:\n{stdout}");
        assert!(
            format!("{stdout}\n{stderr}").contains("Read-only file system"),
            "{task}:\n{stdout}\n{stderr}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&host_file).unwrap(),
        "from the host\n"
    );
    assert!(!host_file.with_file_name("new-file").exists());
}

/// Requires a running Docker daemon with network access to pull
/// `docker:27.3.1-cli`, listening on `/var/run/docker.sock` — skipped when
/// there's no such socket or `DOCKER_HOST` names another address, as
/// Batect's own is when it connects over TCP. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Batect's `DockerClientContainerManagementSpec`, the Docker socket
/// mounted into a container: the Docker CLI there reaches the daemon.
#[test]
#[ignore]
fn the_docker_cli_reaches_the_daemon_through_a_mounted_socket() {
    let socket = Path::new("/var/run/docker.sock");
    let elsewhere = std::env::var("DOCKER_HOST")
        .is_ok_and(|host| !host.is_empty() && host != "unix:///var/run/docker.sock");
    if elsewhere || !socket.exists() {
        eprintln!("skipped: the daemon isn't reached through /var/run/docker.sock");
        return;
    }
    let (success, stdout, stderr) = run_fixture_task(
        local_volumes_config_path(),
        "docker-through-socket",
        "docker-cli",
    );

    assert!(success, "stderr:\n{stderr}");
    assert_eq!(stdout, "server-os=linux", "{stderr}");
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
    assert_eq!(task_output(&stdout, "build-env"), "device-mapped");
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves a mount listed more than once, under two spellings of one host
/// path, reaches the daemon as one bind: two would fail the create with
/// `Duplicate mount point` (ratect#390).
#[test]
#[ignore]
fn a_repeated_volume_is_mounted_once() {
    let output = ratect_command()
        .arg("-f")
        .arg(repeated_volumes_config_path())
        .arg("read-marker")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(task_output(&stdout, "build-env"), "mounted-once");
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
    let pid1_comm = |task: &str, container: &str| {
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
        task_output(&String::from_utf8_lossy(&output.stdout), container)
    };

    let normal = pid1_comm("show-pid1-normal", "no-init");
    let with_init = pid1_comm("show-pid1-with-init", "with-init");

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
    assert_eq!(task_output(&stdout, "build-env"), "hello-from-build-arg");
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
    assert_eq!(task_output(&stdout, "build-env"), "from-builder-stage");
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
    assert_eq!(task_output(&stdout, "build-env"), "hello-from-build-secret");
}

/// Same requirements as `build_secrets_reach_a_real_buildkit_session_build`.
///
/// A `build_secrets` entry with `path` reaches a real build (ratect#520):
/// see `tests/fixtures/build-secrets-file.yml`.
#[test]
#[ignore]
fn a_file_build_secret_reaches_a_real_buildkit_session_build() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let output = ratect_command()
        .arg("-f")
        .arg(fixtures.join("build-secrets-file.yml"))
        .arg("print-secret")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let secret = std::fs::read_to_string(fixtures.join("build-secrets-file/token.txt")).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(task_output(&stdout, "build-env"), secret);
}

/// A throwaway `registry:2` on a loopback port that requires a username and
/// password, holding one pushed image, plus a scratch directory for its
/// password file and the Docker configs the test writes. Everything it
/// made — the registry, every image naming it, the built image, the
/// directory — is removed on `Drop`, even if the test panics.
struct AuthenticatedRegistry {
    container: String,
    address: String,
    directory: PathBuf,
}

impl AuthenticatedRegistry {
    const USERNAME: &'static str = "ratect";
    const PASSWORD: &'static str = "ratect-520";
    const BUILT_IMAGE: &'static str = "ratect-build-authenticated-base-image-test-build-env";

    /// Starts the registry and pushes `alpine:3.18.2` to it as
    /// `<address>/ratect-520-base:1`, then removes that tag from the daemon,
    /// so a build must pull it — through the registry's authentication.
    fn start() -> Self {
        let directory = scratch_directory("authenticated-registry");
        let htpasswd = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--entrypoint",
                "htpasswd",
                "httpd:2.4-alpine",
            ])
            .args(["-Bbn", Self::USERNAME, Self::PASSWORD])
            .output()
            .expect("failed to run htpasswd");
        assert!(htpasswd.status.success(), "{htpasswd:?}");
        std::fs::create_dir_all(directory.join("auth")).unwrap();
        std::fs::write(directory.join("auth/htpasswd"), &htpasswd.stdout).unwrap();
        let started = Command::new("docker")
            .args(["run", "-d", "-p", "127.0.0.1:0:5000"])
            .arg("-v")
            .arg(format!("{}:/auth:ro", directory.join("auth").display()))
            .args([
                "-e",
                "REGISTRY_AUTH=htpasswd",
                "-e",
                "REGISTRY_AUTH_HTPASSWD_REALM=ratect-520",
                "-e",
                "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd",
                "registry:2",
            ])
            .output()
            .expect("failed to start the registry");
        assert!(started.status.success(), "{started:?}");
        let container = String::from_utf8_lossy(&started.stdout).trim().to_string();
        let port = Command::new("docker")
            .args(["port", &container, "5000/tcp"])
            .output()
            .expect("failed to read the registry's port");
        let port = String::from_utf8_lossy(&port.stdout);
        let port = port.trim().rsplit(':').next().unwrap().to_string();
        let registry = Self {
            container,
            address: format!("localhost:{port}"),
            directory,
        };

        let pusher = registry.config_directory("pusher", true);
        let docker = |args: &[&str]| {
            let output = Command::new("docker")
                .arg("--config")
                .arg(&pusher)
                .args(args)
                .output()
                .expect("failed to run docker");
            assert!(output.status.success(), "docker {args:?}: {output:?}");
        };
        docker(&["pull", "alpine:3.18.2"]);
        docker(&["tag", "alpine:3.18.2", &registry.base_image()]);
        let pushed = poll_until(Duration::from_secs(30), || {
            Command::new("docker")
                .arg("--config")
                .arg(&pusher)
                .args(["push", &registry.base_image()])
                .output()
                .is_ok_and(|output| output.status.success())
        });
        assert!(
            pushed,
            "could not push to the registry at {}",
            registry.address
        );
        docker(&["rmi", &registry.base_image()]);
        registry
    }

    fn base_image(&self) -> String {
        format!("{}/ratect-520-base:1", self.address)
    }

    /// A Docker config directory named `name`, holding this registry's
    /// credential inline under `auths` when `with_credential` — and no
    /// `credsStore` or `credHelpers` either way, so nothing on the host is
    /// asked. Inline `auths` also keep the Docker CLI from choosing a
    /// default store of its own for the pusher's config.
    fn config_directory(&self, name: &str, with_credential: bool) -> PathBuf {
        let directory = self.directory.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let config = if with_credential {
            let auth = base64_encode(&format!("{}:{}", Self::USERNAME, Self::PASSWORD));
            format!(
                r#"{{"auths": {{"{}": {{"auth": "{auth}"}}}}}}"#,
                self.address
            )
        } else {
            "{}".to_string()
        };
        std::fs::write(directory.join("config.json"), config).unwrap();
        directory
    }
}

impl Drop for AuthenticatedRegistry {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rmi", "-f", &self.base_image(), Self::BUILT_IMAGE])
            .output();
        let _ = Command::new("docker")
            .args(["rm", "-fv", &self.container])
            .output();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Standard base64, padded — all a Docker config's inline `auth` needs.
fn base64_encode(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(value >> shift & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

/// Requires a running Docker daemon with BuildKit, network access to pull
/// `registry:2`, `httpd:2.4-alpine` and `alpine:3.18.2`, and a daemon that
/// can reach a registry on the host's loopback. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Registry credentials in the Docker config reach a BuildKit build whose
/// base image needs them (ratect#520), with a control: the same build with
/// no credential is refused by the registry. See
/// `tests/fixtures/build-authenticated-base-image.yml`.
#[test]
#[ignore]
fn docker_config_credentials_reach_a_buildkit_build_of_an_authenticated_base_image() {
    let registry = AuthenticatedRegistry::start();
    let build = |config_directory: &Path| {
        ratect_command()
            .env("DOCKER_BUILDKIT", "1")
            .env("RATECT_AUTHENTICATED_BASE_IMAGE", registry.base_image())
            .arg("--docker-config")
            .arg(config_directory)
            .arg("-f")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/build-authenticated-base-image.yml"),
            )
            .arg("build")
            .output()
            .expect("failed to run ratect")
    };

    let refused = build(&registry.config_directory("anonymous", false));
    let refused_stderr = String::from_utf8_lossy(&refused.stderr).to_lowercase();
    let authenticated = build(&registry.config_directory("authenticated", true));

    assert!(
        !refused.status.success()
            && [
                "unauthorized",
                "authentication required",
                "no basic auth credentials"
            ]
            .iter()
            .any(|refusal| refused_stderr.contains(refusal)),
        "without a credential the registry should refuse the pull:\n{refused_stderr}"
    );
    assert!(
        authenticated.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&authenticated.stderr)
    );
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
    key: ScratchSshKey,
}

/// A throwaway ed25519 keypair in a directory of its own, generated by
/// `ssh-keygen`. Split out from [`ScratchSshAgent`] because the `paths`
/// test below needs the key *without* an agent — that's the whole point of
/// serving key files, and spawning an agent would hide whether the keys
/// were being served at all. Removed on `Drop`, even if the test panics.
struct ScratchSshKey {
    path: PathBuf,
    comment: String,
    directory: PathBuf,
}

impl ScratchSshKey {
    fn generate() -> Self {
        // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
        // for why the counter, not just pid+nanos, is what makes the
        // resulting path actually unique under concurrent #[ignore]d tests.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "ratect-build-ssh-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let comment = "ratect-build-ssh-test-key".to_string();
        let path = directory.join("id_ed25519");
        let keygen = Command::new("ssh-keygen")
            .arg("-t")
            .arg("ed25519")
            .arg("-N")
            .arg("")
            .arg("-C")
            .arg(&comment)
            .arg("-f")
            .arg(&path)
            .output()
            .expect("failed to run ssh-keygen");
        assert!(
            keygen.status.success(),
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&keygen.stderr)
        );

        Self {
            path,
            comment,
            directory,
        }
    }
}

impl Drop for ScratchSshKey {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
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

        let key = ScratchSshKey::generate();
        let add = Command::new("ssh-add")
            .arg(&key.path)
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
            key,
        }
    }
}

impl Drop for ScratchSshAgent {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(&self.pid).output();
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

    // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter, not just pid+nanos, makes this actually unique.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let cache_bust = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
        stdout.contains(&agent.key.comment),
        "the build's `ssh-add -l` should have listed the scratch key \
         '{}', but printed:\n{stdout}",
        agent.key.comment
    );
}

/// Requires a running Docker daemon that supports BuildKit sessions,
/// network access to pull `alpine:3.18.2` (and `apk add openssh-client`
/// inside the build), and `ssh-keygen` on the host. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves the two halves of `build_ssh` that the test above doesn't: a
/// *named* agent (the Dockerfile selects `id=deploy`, not the implicit
/// `default`), served from an explicit private key *file* by Ratect's own
/// in-process ssh-agent rather than by forwarding a running one.
///
/// `SSH_AUTH_SOCK` is removed from the child's environment deliberately —
/// that's what makes this conclusive rather than merely consistent. With no
/// agent on the host to forward, anything the build's `ssh-add -l` lists
/// can only have come from the key file being served. Without that removal,
/// a developer's own agent could satisfy the assertion while the keyring
/// did nothing at all.
#[test]
#[ignore]
fn build_ssh_serves_explicit_key_files_to_a_named_agent() {
    let key = ScratchSshKey::generate();

    // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter, not just pid+nanos, makes this actually unique.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let cache_bust = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let output = ratect_command()
        .arg("-f")
        .arg(build_ssh_paths_config_path())
        .arg("print-keys")
        .env_remove("SSH_AUTH_SOCK")
        .env("RATECT_BUILD_SSH_KEY_PATH", &key.path)
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
        stdout.contains(&key.comment),
        "the build's `ssh-add -l` should have listed the scratch key \
         '{}', but printed:\n{stdout}",
        key.comment
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
/// Proves a BuildKit `COPY` of the build context lands root-owned, which
/// `build_context_tar`'s unit test can only show of the tar's headers. Pinned
/// to BuildKit, where the headers' ownership reaches the image (the classic
/// builder chowns a `COPY` to root regardless). Meaningful only as a non-root
/// host user — see the fixture's header.
#[test]
#[ignore]
fn build_context_files_are_root_owned_in_a_buildkit_build() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_context_ownership_config_path())
        .arg("check")
        .env("DOCKER_BUILDKIT", "1")
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
    let _shared_project = CLASSIC_BUILD_FAILURE_PROJECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A classic build whose `RUN` step fails leaves no stopped intermediate
/// container behind, as Batect's builds don't (ratect#292): the daemon only
/// removes a failed step's container when asked to force it. That container
/// carries none of Ratect's labels, so it is found by the failing step's own
/// command text, among the containers that weren't there before the run.
#[test]
#[ignore]
fn a_failed_classic_build_leaves_no_intermediate_container() {
    let _shared_project = CLASSIC_BUILD_FAILURE_PROJECT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let failed_step_containers = || -> Vec<String> {
        let output = Command::new("docker")
            .args(["ps", "-a", "--no-trunc", "--format", "{{.ID}} {{.Command}}"])
            .output()
            .expect("failed to run docker ps");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("this line should reach the user"))
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect()
    };
    let before = failed_step_containers();

    let output = ratect_command()
        .arg("-f")
        .arg(build_failure_config_path())
        .arg("build")
        .env("DOCKER_BUILDKIT", "0")
        .output()
        .expect("failed to run ratect");

    let left: Vec<String> = failed_step_containers()
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    for id in &left {
        let _ = Command::new("docker").args(["rm", "-f", id]).output();
    }
    assert!(
        !output.status.success(),
        "the build should have failed: stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        left,
        Vec::<String>::new(),
        "the failed step's container was left behind"
    );
}

/// Runs `tests/fixtures/build-missing-base-image.yml`'s `build` task with
/// `DOCKER_BUILDKIT` set to `buildkit`, and returns its stderr after
/// checking what both builders say about a base image that exists nowhere:
/// a failed run, the build's own tag, and the daemon's reason, whose
/// wording depends on the daemon.
fn missing_base_image_build_stderr(buildkit: &str) -> String {
    let output = ratect_command()
        .arg("-f")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/build-missing-base-image.yml"),
        )
        .arg("build")
        .env("DOCKER_BUILDKIT", buildkit)
        .output()
        .expect("failed to run ratect");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "the build should have failed: {stderr}"
    );
    assert!(
        stderr.contains("Failed to build image 'ratect-build-missing-base-image-test-build-env'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("not found") || stderr.contains("pull access denied"),
        "the daemon's reason should reach the error: {stderr}"
    );
    stderr
}

/// Requires a running Docker daemon with BuildKit and network access to a
/// registry that answers for the nonexistent base image. Run explicitly
/// with `cargo test -- --ignored`.
///
/// Pins Batect's `DockerClientBuildKitImageBuildSpec` "non-existent base
/// image" rule (ratect#519): see `tests/fixtures/build-missing-base-image.yml`.
#[test]
#[ignore]
fn a_buildkit_build_on_a_missing_base_image_fails_naming_it() {
    let stderr = missing_base_image_build_stderr("1");
    assert!(stderr.contains("load metadata for"), "{stderr}");
}

/// Requires a running Docker daemon and network access to a registry that
/// answers for the nonexistent base image. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Pins Batect's `DockerClientLegacyImageBuildSpec` "base image does not
/// exist" rule (ratect#519) on the classic builder: see
/// `tests/fixtures/build-missing-base-image.yml`.
#[test]
#[ignore]
fn a_classic_build_on_a_missing_base_image_fails_naming_it() {
    let stderr = missing_base_image_build_stderr("0");
    assert!(stderr.contains("Step 1/"), "{stderr}");
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
    assert_eq!(
        task_output(&stdout, "build-env"),
        "built with buildkit heredoc support"
    );
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
    assert_eq!(task_output(&stdout, "build-env"), "hello-from-build-arg");
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
    // Ratect's own refusal, not the classic builder's: a `RUN --mount` it
    // was sent anyway fails saying it "requires BuildKit" too.
    assert!(
        stderr.contains("requires BuildKit ('build_secrets'/'build_ssh' cannot be served"),
        "the error should be the refusal, before anything is built: {stderr}"
    );
}

/// Same requirements as `build_secrets_error_clearly_when_the_classic_builder_is_forced`.
///
/// Batect's rule U72 (ratect#521): `build_ssh` is refused on a classic
/// builder `DOCKER_BUILDKIT=0` forces, too. `SSH_AUTH_SOCK` is removed, so
/// the refusal is the failure, not an agent that isn't there.
#[test]
#[ignore]
fn build_ssh_errors_clearly_when_the_classic_builder_is_forced() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_ssh_config_path())
        .arg("print-keys")
        .env_remove("SSH_AUTH_SOCK")
        .env("RATECT_BUILD_SSH_TEST_CACHE_BUST", "irrelevant")
        .env("DOCKER_BUILDKIT", "0")
        .output()
        .expect("failed to run ratect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the build should have been refused: {stderr}"
    );
    // Ratect's own refusal, not the classic builder's: a `RUN --mount` it
    // was sent anyway fails saying it "requires BuildKit" too.
    assert!(
        stderr.contains("requires BuildKit ('build_secrets'/'build_ssh' cannot be served"),
        "the error should be the refusal, before anything is built: {stderr}"
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

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect-compat"));
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

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect-compat"));
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

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect-compat"));
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
/// Proves a prerequisite task's own container gets the host's `TERM`, as
/// Batect gives it to every container (ratect#474) — not only the invoked
/// task's. `-o quiet` streams each task container's output to stdout.
#[test]
#[ignore]
fn a_prerequisite_tasks_container_gets_the_host_term() {
    let output = ratect_command()
        .arg("-f")
        .arg(prerequisite_task_container_config_path())
        .arg("-o")
        .arg("quiet")
        .arg("main")
        .env("TERM", "xterm-256color")
        .output()
        .expect("failed to run ratect");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout.contains("PRE TERM=[xterm-256color]"), "{stdout}");
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves stdin forwarding is decoupled from TTY allocation: `ratect` is
/// spawned with both stdin *and* stdout wired to plain OS pipes — neither
/// end is a terminal at all, unlike the `portable-pty`-based test above, so
/// `should_use_tty` is false and no real Docker TTY gets allocated — yet the
/// container is still the task's own, so `interactive` (eligibility)
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
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves a prerequisite task's own container gets stdin forwarded, as
/// Batect forwards it to each task's own container (ratect#486), with both
/// stdin and stdout piped so no TTY is involved. Only the prerequisite's
/// read is asserted: what the named task would read afterwards depends on
/// how much the stdin pump had already copied.
#[test]
#[ignore]
fn piped_stdin_reaches_a_prerequisite_tasks_container() {
    use std::process::Stdio;

    let mut child = ratect_command()
        .arg("-f")
        .arg(prerequisite_task_container_config_path())
        .arg("-o")
        .arg("quiet")
        .arg("main")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");

    let mut stdin = child.stdin.take().expect("child should have piped stdin");
    writeln!(stdin, "ratect-prerequisite-stdin-marker").expect("failed to write to piped stdin");
    drop(stdin);

    let output = child.wait_with_output().expect("failed to wait for ratect");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(
        stdout.contains("PRE-READ=[ratect-prerequisite-stdin-marker]"),
        "{stdout}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`, and the ability to allocate a local pseudo-terminal —
/// see `interactive_session_forwards_stdin_and_stdout`. Run explicitly with
/// `cargo test -- --ignored`.
///
/// Proves a prerequisite task's own container gets a real TTY and the
/// terminal's input when stdin and stdout are both terminals, as the named
/// task's does (ratect#486) — and that what is typed once the prerequisite
/// has exited reaches the named task, not a read the prerequisite's stdin
/// forwarding left behind.
#[test]
#[ignore]
fn a_prerequisite_tasks_container_gets_a_tty() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect-compat"));
    cmd.arg("-f");
    cmd.arg(prerequisite_task_container_config_path());
    cmd.arg("-o");
    cmd.arg("quiet");
    cmd.arg("main");

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
    let mut read_until = |marker: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !String::from_utf8_lossy(&output).contains(marker) && Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                output.extend_from_slice(&chunk);
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    };

    // The prerequisite reads one line before it exits; the pty buffers it
    // until then.
    writeln!(writer, "pre-line").expect("failed to write to pty");
    let output_str = read_until("PRE-READ=");
    assert!(output_str.contains("PRE-OUT-TTY"), "{output_str:?}");
    assert!(output_str.contains("PRE-READ=[pre-line]"), "{output_str:?}");

    // Typed only once the named task's container is running, so it can
    // only reach that container — unless something left over from the
    // prerequisite's session reads it first.
    read_until("MAIN-READY");
    writeln!(writer, "main-line").expect("failed to write to pty");
    let output_str = read_until("main-done");
    assert!(
        output_str.contains("MAIN-READ=[main-line]"),
        "{output_str:?}"
    );

    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });
    let status = wait_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("ratect did not exit")
        .expect("failed to wait for ratect");
    assert!(status.success(), "{status:?}");
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The end of piped stdin has to reach the task's own container (ratect#220),
/// or a process that runs until its input ends — a stdio server, `cat` —
/// never exits, and neither does Ratect, leaving its container and network
/// behind when whatever launched it gives up. Unlike
/// `piped_stdin_reaches_a_non_tty_task_container` above, nothing here tells
/// the container to exit: closing stdin is the only signal it gets. Asserts
/// the daemon's own state afterwards, not just the exit code, and removes
/// anything the run left behind whether or not it passed.
#[test]
#[ignore]
fn piped_stdin_eof_ends_a_non_tty_task_container() {
    use std::process::Stdio;

    let project_filter = "label=eu.orican.ratect.project=ratect-stdin-eof-test";
    fn ids(kind: &str, filter: &str) -> Vec<String> {
        let mut arguments = vec![kind, "ls", "-q", "--filter", filter];
        if kind == "container" {
            arguments.insert(2, "-a");
        }
        let output = Command::new("docker")
            .args(&arguments)
            .output()
            .expect("failed to run docker ls");
        assert!(output.status.success(), "docker {kind} ls failed");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    let mut child = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdin-eof.yml"))
        .arg("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");

    let mut stdin = child.stdin.take().expect("child should have piped stdin");
    let marker = "ratect-stdin-eof-test-marker";
    writeln!(stdin, "{marker}").expect("failed to write to piped stdin");
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("failed to poll ratect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    let containers = ids("container", project_filter);
    let networks = ids("network", project_filter);
    for id in &containers {
        let _ = Command::new("docker").args(["rm", "-fv", id]).output();
    }
    for id in &networks {
        let _ = Command::new("docker").args(["network", "rm", id]).output();
    }

    let status = status.expect("ratect did not exit after its stdin was closed");
    assert!(
        status.success(),
        "ratect should exit successfully once the container's input ends: {status:?}"
    );
    assert!(
        output.contains(marker),
        "expected the marker to round-trip through the container: {output:?}"
    );
    assert!(
        containers.is_empty(),
        "the run's container should have been removed: {containers:?}"
    );
    assert!(
        networks.is_empty(),
        "the run's network should have been removed: {networks:?}"
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
    // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter is load-bearing, not decorative.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let test_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

    let stdout = task_output(&String::from_utf8_lossy(&output.stdout), "app");
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

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Under `run_as_current_user`, the mount points nested inside a `local`
/// mount — a file, a directory and a `tmpfs` under `.` at `/code` — are
/// created in the project by Ratect, as the host user, with the type each
/// source needs: the nested file mount works (the task reads it), and every
/// mount point left in the project is the host user's, not root's
/// (ratect#321). Only a rootful Linux daemon would leave them root-owned
/// otherwise, so that is where the ownership assertion proves anything —
/// Docker Desktop and OrbStack map ownership to the host user regardless.
/// Run twice: the second run meets every mount point already there and must
/// leave it as it is. Writes its own temporary project, since the run
/// creates state inside it.
#[test]
#[ignore]
fn run_as_current_user_creates_nested_mount_points_as_the_host_user() {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let test_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let scratch_dir = std::env::temp_dir().join(format!("ratect-nested-mounts-test-{test_id}"));
    /// Removes the scratch project however the test ends.
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = RemoveOnDrop(scratch_dir.clone());
    let project = scratch_dir.join("project");
    let sources = scratch_dir.join("sources");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(sources.join("directory")).unwrap();
    std::fs::write(sources.join("file.txt"), "nested-file-ok").unwrap();
    std::fs::write(
        project.join("batect.yml"),
        r#"
project_name: ratect-nested-mounts-test
containers:
  app:
    image: alpine:3.18.2
    volumes:
      - local: .
        container: /code
      - local: ../sources/file.txt
        container: /code/config/file.txt
      - local: ../sources/directory
        container: /code/nested/directory
      - type: tmpfs
        container: /code/tmp/scratch
    run_as_current_user:
      enabled: true
      home_directory: /home/container-user
tasks:
  check:
    run:
      container: app
      command: cat /code/config/file.txt
"#,
    )
    .unwrap();

    let host_uid: u32 = String::from_utf8(
        Command::new("id")
            .arg("-u")
            .output()
            .expect("failed to run id -u")
            .stdout,
    )
    .unwrap()
    .trim()
    .parse()
    .unwrap();

    let mut first_run_inodes = std::collections::HashMap::new();
    for run in 1..=2 {
        let output = ratect_command()
            .current_dir(&project)
            .arg("check")
            .output()
            .expect("failed to run ratect");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            task_output(&stdout, "app"),
            "nested-file-ok",
            "run {run}: the nested file mount should be readable:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );

        use std::os::unix::fs::MetadataExt;
        for (mount_point, is_file) in [
            ("config", false),
            ("config/file.txt", true),
            ("nested", false),
            ("nested/directory", false),
            ("tmp", false),
            ("tmp/scratch", false),
        ] {
            let metadata = std::fs::symlink_metadata(project.join(mount_point))
                .unwrap_or_else(|e| panic!("run {run}: {mount_point}: {e}"));
            assert_eq!(
                metadata.is_file(),
                is_file,
                "run {run}: {mount_point} has the wrong type"
            );
            assert_eq!(
                metadata.uid(),
                host_uid,
                "run {run}: {mount_point} should be the host user's, not root's"
            );
            if is_file {
                assert_eq!(
                    metadata.len(),
                    0,
                    "run {run}: {mount_point} should be empty"
                );
            }
            let first_inode = *first_run_inodes
                .entry(mount_point)
                .or_insert(metadata.ino());
            assert_eq!(
                metadata.ino(),
                first_inode,
                "run {run}: {mount_point} should be the entry the first run created"
            );
        }
    }
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Mounting Docker Desktop's SSH-agent socket under `run_as_current_user`
/// — see `tests/fixtures/ssh-agent-socket.yml` for what makes the mount
/// path special and why the pre-creation step must skip it. A static
/// fixture, not an inline temp config: the config has no per-run temp path
/// and running it writes no state next to itself, so it's exactly the
/// checked-in-fixture case (unlike the user-mapping and cache tests, which
/// go inline for those reasons).
#[test]
#[ignore]
fn ssh_agent_socket_mounts_under_run_as_current_user() {
    let output = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssh-agent-socket.yml"))
        .arg("check")
        .output()
        .expect("failed to run ratect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Failed to create host directory"),
        "the special Docker Desktop path must be skipped, not created:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "the task should run with the socket mounted:\nstderr:\n{stderr}"
    );
    assert_eq!(
        task_output(&String::from_utf8_lossy(&output.stdout), "app"),
        "mounted-ok"
    );
}

/// Shared by `cache_volume_type_persists_and_clean_cache_removes_it` and
/// `cache_directory_type_persists_and_clean_cache_removes_it` — everything
/// about the two is identical except `extra_args` (empty for the default
/// `--cache-type=volume`, `["--cache-type", "directory"]` for the other)
/// and `assert_storage_gone`, which checks the underlying storage
/// (Docker volume / host directory) actually disappeared after
/// `--clean-cache`, in whichever shape that cache type uses.
///
/// Writes its own temporary config (rather than a static checked-in
/// fixture): a `cache` volume mount creates real, run-scoped state
/// (`.batect/caches/`) next to the config file, which a checked-in fixture
/// under `tests/fixtures/` would leak into the tracked repo on every test
/// run.
///
/// Proves two things a config-parsing test structurally can't: running the
/// same task twice shows the *second* run's marker file already contains
/// what the *first* run wrote (real persistence, not just accepted YAML),
/// and `--clean-cache <name>` genuinely removes that same state (verified
/// directly, via `assert_storage_gone`, not just via ratect's own reported
/// output).
///
/// `verify_fresh_restart`, when set, also runs the task a third time and
/// asserts it starts fresh again. Only safe to set for `CacheType::Volume`:
/// Docker manages a named volume directly through its own daemon API, so
/// deleting one and immediately recreating it under the same name is fully
/// synchronous and race-free. A `CacheType::Directory` host path, on the
/// other hand, goes through Docker Desktop's host-filesystem sharing layer
/// (VirtioFS/gRPC-FUSE) on macOS — deleting a directory and immediately
/// remounting a freshly recreated one at the *exact same path* can race
/// against that layer's own cache, intermittently reporting the mount
/// target as missing (confirmed by hand: the same sequence typed as
/// separate manual commands, with natural delay between them, never
/// reproduces it — only tight back-to-back automation does). Not a bug in
/// `resolve_cache_mount`/`clean_directory_caches` — `assert_storage_gone`
/// already gives a full, direct proof of removal for the directory case
/// without needing this extra, environment-fragile step.
fn cache_mount_persists_and_clean_cache_removes_it(
    cache_name: &str,
    extra_args: &[&str],
    assert_storage_gone: impl Fn(&Path, Option<&str>),
    verify_fresh_restart: bool,
) {
    // Called by both #[ignore]d tests below, which `cargo test`'s default
    // parallelism can run concurrently — see
    // `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter, not just pid+nanos, is what makes the resulting
    // path actually unique.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let test_id = format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let project_dir = std::env::temp_dir().join(format!("ratect-cache-test-{test_id}"));
    std::fs::create_dir_all(&project_dir).expect("failed to create scratch project directory");
    let config_path = project_dir.join("batect.yml");

    let config = format!(
        r#"
project_name: ratect-cache-test
containers:
  app:
    image: alpine:3.18.2
    volumes:
      - type: cache
        name: {cache_name}
        container: /cache
tasks:
  append-marker:
    run:
      container: app
      command: sh -c "echo run >> /cache/marker && cat /cache/marker"
"#
    );
    std::fs::write(&config_path, &config).expect("failed to write temp config");

    // A safety net in case `--clean-cache` itself is what's broken and
    // leaves the volume behind — reads the project's own cache key to
    // remove the exact volume this test created. A no-op for
    // `--cache-type=directory` (nothing named that way ever existed to
    // remove), and for the expected case where `--clean-cache` already
    // did its job.
    let cleanup = || {
        if let Ok(key) = std::fs::read_to_string(project_dir.join(".batect/caches/key")) {
            if let Some(key) = key
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
            {
                let _ = Command::new("docker")
                    .args(["volume", "rm", &format!("batect-cache-{key}-{cache_name}")])
                    .output();
            }
        }
        let _ = std::fs::remove_dir_all(&project_dir);
    };

    let run_task = || {
        let output = ratect_command()
            .arg("-f")
            .arg(&config_path)
            .args(extra_args)
            .arg("append-marker")
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
        task_output(&String::from_utf8_lossy(&output.stdout), "app")
    };

    let first_run = run_task();
    assert_eq!(
        first_run, "run",
        "the first run's marker should have exactly one line"
    );

    let second_run = run_task();
    assert_eq!(
        second_run, "run\nrun",
        "the second run should still see the first run's marker line, proving the cache mount \
         persisted across invocations"
    );

    // Only a volume-mode run creates the key file; `assert_storage_gone`
    // says which of the two it expects.
    let project_cache_key = match std::fs::read_to_string(project_dir.join(".batect/caches/key")) {
        Ok(contents) => Some(
            contents
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('#'))
                .expect("the project cache key file should have a non-comment line")
                .to_string(),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("failed to read the project cache key file: {error}"),
    };

    let clean_output = ratect_command()
        .arg("-f")
        .arg(&config_path)
        .args(extra_args)
        .arg("--clean-cache")
        .arg(cache_name)
        .output()
        .expect("failed to run ratect --clean-cache");
    if !clean_output.status.success() {
        cleanup();
        panic!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&clean_output.stdout),
            String::from_utf8_lossy(&clean_output.stderr)
        );
    }
    let clean_stdout = String::from_utf8_lossy(&clean_output.stdout);
    assert!(
        clean_stdout.contains("Done! Deleted 1 "),
        "--clean-cache should report exactly one deletion: {clean_stdout}"
    );

    assert_storage_gone(&project_dir, project_cache_key.as_deref());

    if verify_fresh_restart {
        let third_run = run_task();
        assert_eq!(
            third_run, "run",
            "after --clean-cache, the next run should start fresh again, proving the cache was \
             actually removed, not just reported as removed"
        );
    }

    cleanup();
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves the default `--cache-type=volume` end to end: a `cache` mount
/// resolves to a real Docker named volume that survives between separate
/// `ratect` invocations, and `--clean-cache` actually removes that volume
/// (verified directly via `docker volume inspect`, not just ratect's own
/// reported output).
#[test]
#[ignore]
fn cache_volume_type_persists_and_clean_cache_removes_it() {
    cache_mount_persists_and_clean_cache_removes_it(
        "test-cache-volume",
        &[],
        |_project_dir, project_cache_key| {
            let project_cache_key = project_cache_key
                .expect("the first run should have created a project cache key file");
            let volume_name = format!("batect-cache-{project_cache_key}-test-cache-volume");
            let inspect = Command::new("docker")
                .args(["volume", "inspect", &volume_name])
                .output()
                .expect("failed to run docker volume inspect");
            assert!(
                !inspect.status.success(),
                "the volume '{volume_name}' should no longer exist after --clean-cache"
            );
        },
        true,
    );
}

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `--cache-type=directory` end to end: a `cache` mount resolves to
/// a host directory under `.batect/caches/<name>/` that survives between
/// separate `ratect` invocations, `--clean-cache` actually removes that
/// directory, and the run never creates the project cache key file, which
/// only a volume's name needs.
#[test]
#[ignore]
fn cache_directory_type_persists_and_clean_cache_removes_it() {
    cache_mount_persists_and_clean_cache_removes_it(
        "test-cache-directory",
        &["--cache-type", "directory"],
        |project_dir, project_cache_key| {
            assert_eq!(
                project_cache_key, None,
                "a directory-mode run should never create the project cache key file"
            );
            let cache_dir = project_dir.join(".batect/caches/test-cache-directory");
            assert!(
                !cache_dir.exists(),
                "the directory {cache_dir:?} should no longer exist after --clean-cache"
            );
        },
        false,
    );
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
    // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter, not just pid+nanos, makes this actually unique.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let network_name = format!(
        "ratect-use-network-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

/// Same real-Docker requirements as `ports_publishes_a_container_port_to_the_host`,
/// on host ports 18082/18083. One container port published on two host
/// ports reaches the container through both, as in Batect — not only
/// through the last one listed (ratect#291).
#[test]
#[ignore]
fn ports_publishes_one_container_port_on_every_host_port_given() {
    let mut child = ratect_command()
        .arg("-f")
        .arg(ports_config_path())
        .arg("serve-on-two-host-ports")
        .spawn()
        .expect("failed to spawn ratect");

    let first = wait_for_port(18082, Duration::from_secs(15));
    let second = wait_for_port(18083, Duration::from_secs(15));

    let status = child.wait().expect("failed to wait for ratect");

    assert!(first, "host port 18082 should have been reachable");
    assert!(second, "host port 18083 should have been reachable");
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

/// Same real-Docker requirements as
/// `proxy_environment_variables_are_propagated_into_the_container`.
///
/// Rewriting a `localhost` proxy URL to name the host is only useful if the
/// name resolves, and on Linux nothing supplies it — so the run must add the
/// entry itself. Asserted against `/etc/hosts` rather than by resolving the
/// name, because Docker Desktop answers that name through its own DNS
/// without writing the file: resolution would pass on macOS whether or not
/// Ratect added anything, and the file contents wouldn't.
#[test]
#[ignore]
fn a_rewritten_proxy_url_adds_the_host_gateway_to_the_container() {
    let output = ratect_command()
        .env("http_proxy", "http://localhost:3333")
        .arg("-f")
        .arg(proxy_config_path())
        .arg("print-container-hosts")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("host.docker.internal"),
        "a rewritten proxy URL should add the host-gateway entry: {stdout}"
    );
}

/// Same real-Docker requirements as
/// `proxy_environment_variables_are_propagated_into_the_container`.
///
/// The other half of the rule: a proxy already naming a routable host needs
/// nothing added, and injecting the name regardless would put it into every
/// container of every run that has any proxy variable set at all. Run
/// against the same task as the test above so the two differ in exactly the
/// one thing they are about.
#[test]
#[ignore]
fn a_proxy_url_that_was_not_rewritten_adds_no_host_gateway_to_the_container() {
    let output = ratect_command()
        .env("http_proxy", "http://proxy.example.com:8080")
        .arg("-f")
        .arg(proxy_config_path())
        .arg("print-container-hosts")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("host.docker.internal"),
        "a proxy URL that was not rewritten should add no host-gateway entry: {stdout}"
    );
}

/// Same real-Docker requirements as
/// `proxy_environment_variables_are_propagated_into_the_container`, and
/// builds `tests/fixtures/proxy-build/`.
///
/// A `RUN` step behind a rewritten proxy URL has to resolve the same name
/// the container will, and the build reaches Docker through a different
/// parameter on a different endpoint — so this is the only thing that
/// exercises that wire format at all. The fixture's own `RUN` is the
/// assertion (it greps `/etc/hosts`), leaving this to check the exit code.
#[test]
#[ignore]
fn a_rewritten_proxy_url_adds_the_host_gateway_to_an_image_build() {
    let output = ratect_command()
        .env("http_proxy", "http://localhost:3333")
        .arg("-f")
        .arg(proxy_config_path())
        .arg("build-behind-the-proxy")
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "the build should see the host-gateway entry; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires a running Docker daemon with BuildKit and network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// An interrupted BuildKit build stops running in the daemon (ratect#537):
/// see `tests/fixtures/build-cancel.yml`. The build's `RUN` step ticks a
/// listener here once a second, through a `localhost` proxy URL that ratect
/// rewrites to name the host, so the listener binds every interface — a
/// loopback-only one is unreachable from a Linux build. Ticks still arriving
/// a few seconds after ratect has exited mean the build was only
/// disconnected from, not cancelled.
#[test]
#[ignore]
#[cfg(unix)]
fn an_interrupted_buildkit_build_stops_running_in_the_daemon() {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    let listener = TcpListener::bind("0.0.0.0:0").expect("failed to bind the tick listener");
    let port = listener.local_addr().unwrap().port();
    let ticks: Arc<Mutex<Vec<Instant>>> = Arc::default();
    {
        let ticks = Arc::clone(&ticks);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                ticks.lock().unwrap().push(Instant::now());
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
    }

    let cache_bust = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut child = ratect_command()
        .env("http_proxy", format!("http://localhost:{port}"))
        .env("RATECT_BUILD_CANCEL_TEST_CACHE_BUST", &cache_bust)
        .arg("--enable-buildkit")
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-cancel.yml"))
        .args(["-o", "simple", "build"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");

    let ticking = poll_until(Duration::from_secs(120), || {
        ticks.lock().unwrap().len() >= 2
    });
    if !ticking {
        let _ = child.kill();
        let output = child.wait_with_output().expect("failed to wait on ratect");
        panic!(
            "the build never reached its ticking RUN step; stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let signalled = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("failed to run kill");
    assert!(signalled.success(), "could not send SIGINT to ratect");
    let output = child.wait_with_output().expect("failed to wait on ratect");
    let exited = Instant::now();

    std::thread::sleep(Duration::from_secs(10));
    let late_ticks = ticks
        .lock()
        .unwrap()
        .iter()
        .filter(|tick| **tick > exited + Duration::from_secs(3))
        .count();

    assert_eq!(
        output.status.code(),
        Some(130),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        late_ticks,
        0,
        "the build kept running in the daemon after ratect exited; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// See `tests/fixtures/proxy-config-file.yml`: a proxy from the Docker CLI
/// config file reaches a build in both cases, and `--no-proxy-vars` keeps it
/// out.
#[test]
#[ignore]
fn config_file_proxies_reach_an_image_build_unless_no_proxy_vars() {
    let config_directory = scratch_directory("proxy-config-file");
    std::fs::write(
        config_directory.join("config.json"),
        r#"{"proxies": {"default": {"httpProxy": "http://config-proxy.example:3128"}}}"#,
    )
    .unwrap();
    let run = |extra: &[&str]| {
        let mut command = ratect_command();
        for name in [
            "http_proxy",
            "HTTP_PROXY",
            "https_proxy",
            "HTTPS_PROXY",
            "ftp_proxy",
            "FTP_PROXY",
            "no_proxy",
            "NO_PROXY",
        ] {
            command.env_remove(name);
        }
        let output = command
            .env("DOCKER_CONFIG", &config_directory)
            .env(
                "RATECT_PROXY_CONFIG_FILE_TEST_CACHE_BUST",
                format!("{}-{}", config_directory.display(), extra.len()),
            )
            .arg("-f")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proxy-config-file.yml"))
            .args(extra)
            .arg("print-build-proxies")
            .output()
            .expect("failed to run ratect");
        assert!(
            output.status.success(),
            "stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let propagated = run(&[]);
    let suppressed = run(&["--no-proxy-vars"]);
    let _ = std::fs::remove_dir_all(&config_directory);

    assert!(
        propagated.contains(
            "HTTP_PROXY=http://config-proxy.example:3128 http_proxy=http://config-proxy.example:3128"
        ),
        "{propagated}"
    );
    assert!(
        suppressed.contains("HTTP_PROXY= http_proxy="),
        "{suppressed}"
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
    // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter, not just pid+nanos, makes this actually unique —
    // load-bearing here since a colliding tag would be pushed by two
    // concurrently-running #[ignore]d tests onto the one shared daemon.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tag = format!(
        "ratect-image-pull-policy-test-{}-{}-{}:local",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

/// Requires a running Docker daemon with network access to pull `alpine:3.18.2`.
/// Run explicitly with `cargo test -- --ignored`.
///
/// Proves `image_pull_policy`'s second use — force-pulling a
/// `build_directory` container's own base (`FROM`) image before building —
/// reaches a real `docker build`, using the same nonexistent-registry-tag
/// trick as `image_pull_policy_controls_whether_a_real_pull_is_attempted`:
/// `IfNotPresent` (the default) must build successfully from the local
/// image alone; `Always` must genuinely fail, since it forces a real pull
/// attempt of the `FROM` image against that nonexistent remote repo before
/// building at all.
#[test]
#[ignore]
fn image_pull_policy_always_force_pulls_a_build_directorys_base_image() {
    // See `dependency_health_check_and_setup_commands_gate_the_task_via_docker`
    // for why the counter, not just pid+nanos, makes this actually unique.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tag = format!(
        "ratect-build-force-pull-test-{}-{}-{}:local",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

    let cleanup_tag = || {
        let _ = Command::new("docker").args(["rmi", &tag]).output();
    };

    let build_dir = std::env::temp_dir().join(format!(
        "ratect-build-force-pull-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&build_dir).expect("failed to create temp build directory");
    std::fs::write(
        build_dir.join("Dockerfile"),
        format!("FROM {tag}\nCMD [\"echo\", \"ran\"]\n"),
    )
    .expect("failed to write temp Dockerfile");

    let config = format!(
        r#"
project_name: ratect-build-force-pull-test
containers:
  if-not-present:
    build_directory: {build_dir}
  always:
    build_directory: {build_dir}
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
        build_dir = build_dir.display(),
    );
    let config_path = std::env::temp_dir().join(format!(
        "ratect-build-force-pull-test-{}.yml",
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
    let _ = std::fs::remove_dir_all(&build_dir);
    cleanup_tag();

    assert!(
        if_not_present_output.status.success(),
        "IfNotPresent should build successfully from the local base image, without attempting \
         a pull:\nstderr:\n{}",
        String::from_utf8_lossy(&if_not_present_output.stderr)
    );
    assert!(
        !always_output.status.success(),
        "Always should fail attempting a real pull of the base image, tagged under a name \
         that exists on no registry"
    );
}

/// Only a run's failure exits `125`; `--list-tasks`'s exits `1` — the same
/// unloadable config, and the same `--log-file` that exists but can't be
/// opened (a file with no permissions), either way (decisions/0016). A
/// `--log-file` whose directory is missing, or that is a directory, is an
/// invalid option value instead — see
/// `an_invalid_option_value_exits_2_or_125_by_path` and
/// `log_file_must_be_a_file_or_not_yet_exist`. Needs no daemon: both fail
/// before anything connects.
#[cfg(unix)]
#[test]
fn a_run_that_fails_exits_125_and_list_tasks_1() {
    let directory =
        std::env::temp_dir().join(format!("ratect-compat-exit-codes-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let config = directory.join("batect.yml");
    std::fs::write(&config, "nonsense: [").unwrap();
    let exit_code = |args: &[&str]| {
        ratect_command()
            .arg("-f")
            .arg(&config)
            .args(args)
            .output()
            .expect("failed to run ratect-compat")
            .status
            .code()
    };

    let run = exit_code(&["build"]);
    let list = exit_code(&["--list-tasks"]);
    let unopenable = directory.join("unopenable.log");
    std::fs::write(&unopenable, "").unwrap();
    let mut permissions = std::fs::metadata(&unopenable).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o000);
    std::fs::set_permissions(&unopenable, permissions).unwrap();
    let unopenable = unopenable.to_str().unwrap();
    let log_file_run = exit_code(&["--log-file", unopenable, "build"]);
    let log_file_list = exit_code(&["--log-file", unopenable, "--list-tasks"]);
    std::fs::remove_dir_all(&directory).unwrap();

    assert_eq!(run, Some(125));
    assert_eq!(list, Some(1));
    assert_eq!(log_file_run, Some(125));
    assert_eq!(log_file_list, Some(1));
}

/// Runs `ratect-compat` with `args` and `env` on a pseudo-terminal and
/// returns everything it wrote (stdout and stderr are the one terminal),
/// once it has exited.
fn run_on_a_terminal(args: &[&str], env: &[(&str, &str)]) -> String {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ratect-compat"));
    cmd.args(args);
    cmd.env("TERM", "xterm-256color");
    for (name, value) in env {
        cmd.env(name, value);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("failed to spawn ratect in the pty");
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("failed to clone pty reader");
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
    child.wait().expect("failed to wait for ratect");
    drop(pair.master);
    let mut output = Vec::new();
    while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(5)) {
        output.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// The `Error:` and `Warning:` markers are bold red and bold yellow, each
/// one SGR sequence, when stderr is a terminal, and plain under
/// `NO_COLOR`/`--no-color` — pinned byte for byte, since nothing type-checks
/// an escape sequence (ratect#493; decisions/0017). A missing config file
/// stops before any daemon is needed, and a Docker CLI `config.json` that
/// can't be read is warned about before the daemon is reached, so this runs
/// in the default suite.
#[test]
fn error_and_warning_markers_are_bold_on_a_terminal_and_plain_without_color() {
    let args = ["-f", "/no/such/batect.yml", "build"];

    let colored = run_on_a_terminal(&args, &[]);
    assert!(
        colored.contains("\x1b[1;31mError:\x1b[0m "),
        "the marker should be bold red on a terminal: {colored:?}"
    );
    // The warning marker likewise. Cleaning one cache that can't exist
    // reads the active context from `config.json`, and removes nothing even
    // where a daemon answers.
    let project = one_task_project("warning-marker");
    std::fs::write(project.join("config.json"), "{").unwrap();
    let warned = run_on_a_terminal(
        &[
            "-f",
            &project.join("batect.yml").to_string_lossy(),
            "--docker-config",
            &project.to_string_lossy(),
            "--clean-cache=no-such-cache-for-the-warning-marker",
        ],
        &[("DOCKER_HOST", ""), ("DOCKER_CONTEXT", "")],
    );
    std::fs::remove_dir_all(&project).unwrap();
    assert!(
        warned.contains("\x1b[1;33mWarning:\x1b[0m ") && warned.contains("config.json"),
        "the warning marker should be bold yellow on a terminal: {warned:?}"
    );

    for env in [&[("NO_COLOR", "1")][..], &[][..]] {
        let mut plain_args = args.to_vec();
        if env.is_empty() {
            plain_args.insert(0, "--no-color");
        }
        let plain = run_on_a_terminal(&plain_args, env);
        assert!(
            plain.contains("Error: ") && !plain.contains("\x1b["),
            "no SGR at all under {env:?}/{plain_args:?}: {plain:?}"
        );
    }
}

/// A project in a scratch directory whose `ctx` build directory holds a
/// Dockerfile and a `.dockerignore` of `*`, built by two containers that
/// spell the same Dockerfile differently: `absolute` by its absolute path,
/// `dotted` as `./Dockerfile`. Each has a task of its own name.
fn dockerfile_spellings_project() -> PathBuf {
    let dir = scratch_directory("dockerfile-spellings");
    let context = dir.join("ctx");
    std::fs::create_dir_all(&context).unwrap();
    std::fs::write(context.join("Dockerfile"), "FROM alpine:3.18.2\nRUN true\n").unwrap();
    std::fs::write(context.join(".dockerignore"), "*\n").unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        format!(
            "project_name: ratect-dockerfile-spellings-test\n\
             containers:\n\
             \x20 absolute:\n    build_directory: ctx\n    dockerfile: {}\n\
             \x20 dotted:\n    build_directory: ctx\n    dockerfile: ./Dockerfile\n\
             tasks:\n\
             \x20 absolute:\n    run:\n      container: absolute\n      command: \"true\"\n\
             \x20 dotted:\n    run:\n      container: dotted\n      command: \"true\"\n",
            context.join("Dockerfile").display()
        ),
    )
    .unwrap();
    dir
}

/// A config whose build container can't be built still loads: the build's
/// files are checked when it is built, not when the config loads
/// (ratect#296).
#[test]
fn a_build_container_that_cant_be_built_still_lists_its_tasks() {
    let dir = scratch_directory("unbuildable-lists");
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\n\
         containers:\n  app:\n    build_directory: missing\n    dockerfile: Nope\n\
         tasks:\n  t:\n    run:\n      container: app\n",
    )
    .unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A Dockerfile written as an absolute path inside the build directory, or
/// as `./Dockerfile`, builds on both builders, even with a `.dockerignore`
/// of `*` (ratect#296): Docker is sent the Dockerfile's path inside the
/// build context, and that is the path the context always includes.
#[test]
#[ignore]
fn a_dockerfile_however_spelled_builds_on_both_builders() {
    let project = dockerfile_spellings_project();
    let mut failures = Vec::new();
    for buildkit in ["0", "1"] {
        for task in ["absolute", "dotted"] {
            let output = ratect_command()
                .env("DOCKER_BUILDKIT", buildkit)
                .arg("-f")
                .arg(project.join("batect.yml"))
                .arg(task)
                .output()
                .expect("failed to run ratect");
            if !output.status.success() {
                failures.push(format!(
                    "DOCKER_BUILDKIT={buildkit} {task}:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
    }
    for tag in [
        "ratect-dockerfile-spellings-test-absolute",
        "ratect-dockerfile-spellings-test-dotted",
    ] {
        let _ = Command::new("docker").args(["rmi", tag]).output();
    }
    let _ = std::fs::remove_dir_all(&project);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A fresh, empty scratch directory named for `purpose`. Caller cleans up.
fn scratch_directory(purpose: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ratect-compat-{purpose}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A scratch project with one task, `t`, and nothing else in it.
fn one_task_project(purpose: &str) -> PathBuf {
    let dir = scratch_directory(purpose);
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\n\
         containers:\n  app:\n    image: alpine:3.18.2\n\
         tasks:\n  t:\n    run:\n      container: app\n",
    )
    .unwrap();
    dir
}

/// `ratect_command` in `project`, with none of the Docker or Batect
/// environment variables the developer's own shell might set.
fn isolated_docker_command(project: &Path) -> Command {
    let mut command = ratect_command();
    command.current_dir(project);
    for name in [
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_CERT_PATH",
        "DOCKER_TLS_VERIFY",
        "DOCKER_BUILDKIT",
        "BATECT_CACHE_TYPE",
    ] {
        command.env_remove(name);
    }
    command
}

/// #294: with TLS on and no certificate path, `cert.pem` is looked for in
/// the Docker config directory the user chose — from `DOCKER_CONFIG` and
/// from `--docker-config` alike — not in `~/.docker`. The host is
/// unreachable, so the run fails on the missing file before any request.
#[test]
fn tls_certificates_are_looked_for_in_the_chosen_docker_config_directory() {
    let project = one_task_project("cert-dir-from-config");
    let config_directory = project.join("cfg");
    std::fs::create_dir_all(&config_directory).unwrap();
    let files = project.join("files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("ca.pem"), "ca").unwrap();
    std::fs::write(files.join("key.pem"), "key").unwrap();

    for (named_by, from_flag) in [("DOCKER_CONFIG", false), ("--docker-config", true)] {
        let mut command = isolated_docker_command(&project);
        command
            .env("DOCKER_TLS_VERIFY", "1")
            .env("DOCKER_HOST", "tcp://127.0.0.1:1")
            .arg("--docker-tls-ca-cert")
            .arg(files.join("ca.pem"))
            .arg("--docker-tls-key")
            .arg(files.join("key.pem"));
        if from_flag {
            command.arg("--docker-config").arg(&config_directory);
        } else {
            command.env("DOCKER_CONFIG", &config_directory);
        }
        let output = command.arg("t").output().expect("failed to run ratect");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(125), "{named_by}: {stderr}");
        let expected = format!(
            "{named_by}: {} does not exist or is not a file",
            config_directory.join("cert.pem").display()
        );
        assert!(stderr.contains(&expected), "{named_by}: {stderr}");
    }
    std::fs::remove_dir_all(&project).ok();
}

/// #539: a Docker config or certificate directory that doesn't exist, or is
/// a file, fails every command before it runs — `--list-tasks` included,
/// which never connects — naming where it came from, and lists nothing.
#[test]
fn list_tasks_refuses_a_docker_directory_that_is_not_one() {
    let project = one_task_project("docker-directory");
    let file = project.join("a-file");
    std::fs::write(&file, "").unwrap();
    let missing = project.join("missing");
    for path in [&missing, &file] {
        let problem = if path == &file {
            "is a file, not a directory"
        } else {
            "does not exist"
        };
        for (source, from_flag) in [
            ("--docker-config", true),
            ("DOCKER_CONFIG", false),
            ("--docker-cert-path", true),
            ("DOCKER_CERT_PATH", false),
        ] {
            let mut command = isolated_docker_command(&project);
            if from_flag {
                command.arg(source).arg(path);
            } else {
                command.env(source, path);
            }
            let output = command
                .arg("--list-tasks")
                .output()
                .expect("failed to run ratect");

            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{source}: {stderr}");
            assert!(
                stderr.contains(&format!("{source} {path:?} {problem}.")),
                "{source}: {stderr}"
            );
            assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{source}");
        }
    }
    std::fs::remove_dir_all(&project).ok();
}

/// #539: a task run exits `125` for it, like any failure to run.
#[test]
fn a_run_refuses_a_docker_config_directory_that_does_not_exist() {
    let project = one_task_project("docker-directory-run");
    let output = isolated_docker_command(&project)
        .args(["--docker-config", "/nonexistent-ratect-dir", "t"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&project).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(125), "{stderr}");
    assert!(
        stderr.contains(r#"--docker-config "/nonexistent-ratect-dir" does not exist."#),
        "{stderr}"
    );
}

/// #539: `--help` and `--version` don't check it — a recorded divergence.
#[test]
fn help_and_version_ignore_a_docker_config_directory_that_does_not_exist() {
    for flag in ["--help", "--version"] {
        let output = ratect_command()
            .args(["--docker-config", "/nonexistent-ratect-dir", flag])
            .output()
            .expect("failed to run ratect");
        assert_eq!(output.status.code(), Some(0), "{flag}");
    }
}

/// #305: a `DOCKER_TLS_VERIFY` that is neither true nor false stops a run
/// before it connects — the listener standing in for the daemon receives
/// nothing, not a plaintext request — naming the variable and the value.
#[test]
fn a_run_refuses_an_invalid_docker_tls_verify_before_connecting() {
    let project = one_task_project("tls-verify-invalid");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();

    let output = isolated_docker_command(&project)
        .env("DOCKER_HOST", format!("tcp://127.0.0.1:{port}"))
        .env("DOCKER_TLS_VERIFY", "maybe")
        .arg("t")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&project).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(125), "{stderr}");
    assert!(stderr.contains("DOCKER_TLS_VERIFY is 'maybe'"), "{stderr}");
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "something connected to the stand-in daemon"
    );
}

/// #305: it is read only on connecting, so `--list-tasks` never sees it —
/// a recorded divergence from Batect, which refuses it on every command.
#[test]
fn list_tasks_ignores_an_invalid_docker_tls_verify() {
    let project = one_task_project("tls-verify-list");
    let output = isolated_docker_command(&project)
        .env("DOCKER_TLS_VERIFY", "maybe")
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&project).ok();

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("- t"));
}

/// #305: only the flags conflict with `--docker-context`; the variable,
/// which a context connection never reads, doesn't.
#[test]
fn docker_tls_verify_from_the_environment_does_not_conflict_with_a_context() {
    let project = one_task_project("tls-verify-context");
    let config_directory = project.join("cfg");
    std::fs::create_dir_all(&config_directory).unwrap();
    let output = isolated_docker_command(&project)
        .env("DOCKER_TLS_VERIFY", "1")
        .arg("--docker-context")
        .arg("nope")
        .arg("--docker-config")
        .arg(&config_directory)
        .arg("t")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&project).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Cannot use both"), "{stderr}");
    assert!(
        stderr.contains("Docker context 'nope' does not exist"),
        "{stderr}"
    );
}

/// A one-task project with a `cache` directory `<name>` already under
/// `.batect/caches`, as a `--cache-type directory` run would leave it.
fn project_with_a_cache_directory(purpose: &str, name: &str) -> (PathBuf, PathBuf) {
    let project = one_task_project(purpose);
    let cache = project.join(".batect/caches").join(name);
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("kept"), "").unwrap();
    (project, cache)
}

/// #441: `BATECT_CACHE_TYPE=directory` makes `--clean` remove the cache
/// directories, without contacting Docker, as `--cache-type directory` does.
#[test]
fn batect_cache_type_directory_cleans_cache_directories() {
    let (project, cache) = project_with_a_cache_directory("cache-type-env", "deps");
    let output = isolated_docker_command(&project)
        .env("BATECT_CACHE_TYPE", "directory")
        .env("DOCKER_HOST", "tcp://127.0.0.1:1")
        .arg("--clean")
        .output()
        .expect("failed to run ratect");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let gone = !cache.exists();
    std::fs::remove_dir_all(&project).ok();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Done! Deleted 1 directory."), "{stdout}");
    assert!(gone, "the cache directory is still there");
}

/// #459: a directory-mode clean skips a cache entry that is a symbolic
/// link rather than following it, and says so on stderr — once, though
/// both listing and removal read the directory — in every output style,
/// with stdout unchanged.
#[cfg(unix)]
#[test]
fn directory_clean_warns_once_about_a_skipped_symbolic_link() {
    let (project, _) = project_with_a_cache_directory("clean-symlink", "deps");
    // As the binary reports it: the temporary directory may be behind a
    // symbolic link itself (macOS's `/var`).
    let project = project.canonicalize().unwrap();
    let outside = scratch_directory("clean-symlink-target");
    let link = project.join(".batect/caches/linked");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    for style in [&[][..], &["-o", "quiet"]] {
        std::fs::create_dir_all(project.join(".batect/caches/deps")).unwrap();
        let output = isolated_docker_command(&project)
            .args(["--clean", "--cache-type", "directory"])
            .args(style)
            .output()
            .expect("failed to run ratect");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{style:?}: {stderr}");
        assert!(
            stdout.ends_with("Done! Deleted 1 directory.\n"),
            "{style:?}: {stdout}"
        );
        assert_eq!(
            stderr
                .matches(&format!("Skipped '{}'", link.display()))
                .count(),
            1,
            "{style:?}: {stderr}"
        );
        assert!(link.is_symlink(), "{style:?}: the link was removed");
    }

    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
}

/// #459: a directory-mode clean of a project with no `.batect/caches`
/// still succeeds with zero deletions, where Batect fails — and says
/// there was no cache directory, once, rather than leaving that to read
/// as an ordinary empty clean.
#[test]
fn directory_clean_warns_once_when_there_is_no_cache_directory() {
    let project = one_task_project("clean-no-caches").canonicalize().unwrap();
    let caches = project.join(".batect/caches");

    for args in [
        &["--clean"][..],
        &["--clean-cache", "deps"],
        &["--clean", "-o", "quiet"],
    ] {
        let output = isolated_docker_command(&project)
            .args(["--cache-type", "directory"])
            .args(args)
            .output()
            .expect("failed to run ratect");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(
            stdout,
            format!(
                "Checking for cache directories in '{}'...\nDone! Deleted 0 directories.\n",
                caches.display()
            ),
            "{args:?}"
        );
        assert_eq!(
            stderr
                .matches(&format!("no cache directory at '{}'", caches.display()))
                .count(),
            1,
            "{args:?}: {stderr}"
        );
    }

    std::fs::remove_dir_all(&project).unwrap();
}

/// #445: a volume-mode `--clean` says it is checking for cache volumes
/// only once the daemon has been reached, as Batect does — an unreachable
/// daemon prints the connection error and nothing on stdout. Two failure
/// points: a missing socket fails at connect, a refused port at version
/// negotiation.
#[test]
fn volume_clean_prints_nothing_on_stdout_when_docker_is_unreachable() {
    let project = one_task_project("clean-unreachable");
    let socket = format!("unix://{}", project.join("no.sock").display());
    for host in [socket.as_str(), "tcp://127.0.0.1:1"] {
        let output = isolated_docker_command(&project)
            .args(["--clean", "--docker-host", host])
            .output()
            .expect("failed to run ratect");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_ne!(output.status.code(), Some(0), "{host}: {stderr}");
        assert!(
            output.stdout.is_empty(),
            "{host}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(stderr.contains(host), "{host}: {stderr}");
    }
    std::fs::remove_dir_all(&project).unwrap();
}

/// #445's other half: with a reachable daemon, volume mode's output is
/// unchanged — the progress line first, then each removal, then the
/// summary — and a plain `--clean` exits 0. Asserted against the daemon's
/// own volume list too, so a pass doesn't depend on what was printed.
#[test]
#[ignore]
fn volume_clean_reports_progress_then_removals_then_the_summary_via_docker() {
    let project = one_task_project("clean-volume-order");
    let caches = project.join(".batect/caches");
    std::fs::create_dir_all(&caches).unwrap();
    let key = format!("order{}", std::process::id());
    std::fs::write(caches.join("key"), format!("{key}\n")).unwrap();
    let volume = format!("batect-cache-{key}-deps");
    let created = Command::new("docker")
        .args(["volume", "create", &volume])
        .output()
        .expect("failed to run docker");
    assert!(created.status.success(), "{created:?}");

    let output = ratect_command()
        .current_dir(&project)
        .arg("--clean")
        .output()
        .expect("failed to run ratect");
    let remaining = Command::new("docker")
        .args(["volume", "ls", "-q", "--filter", &format!("name={volume}")])
        .output()
        .expect("failed to run docker");
    let _ = Command::new("docker")
        .args(["volume", "rm", &volume])
        .output();
    std::fs::remove_dir_all(&project).ok();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "Checking for cache volumes...",
            &format!("Deleting volume '{volume}'..."),
            "Done! Deleted 1 volume.",
        ]
    );
    assert!(remaining.stdout.is_empty(), "{volume} is still there");
}

/// #668: a directory-mode `--clean` whose stdout reader has already gone
/// (`| head -c 0`) still removes the caches and exits 0, as Batect's
/// `PrintStream` does, rather than panicking at its first line with
/// nothing removed.
#[test]
fn directory_clean_removes_the_caches_when_stdout_is_closed() {
    let (project, cache) = project_with_a_cache_directory("clean-closed-stdout", "deps");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);

    let output = isolated_docker_command(&project)
        .args(["--clean", "--cache-type", "directory"])
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run ratect");

    let gone = !cache.exists();
    std::fs::remove_dir_all(&project).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(stderr, "");
    assert!(gone, "the cache directory is still there");
}

/// #668's volume-mode half: with stdout closed, `--clean` still removes
/// the volume and exits 0. Asserted against the daemon's own volume list.
#[test]
#[ignore]
fn volume_clean_removes_the_caches_when_stdout_is_closed_via_docker() {
    let project = one_task_project("clean-volume-closed-stdout");
    let caches = project.join(".batect/caches");
    std::fs::create_dir_all(&caches).unwrap();
    let key = format!("closed{}", std::process::id());
    std::fs::write(caches.join("key"), format!("{key}\n")).unwrap();
    let volume = format!("batect-cache-{key}-deps");
    let created = Command::new("docker")
        .args(["volume", "create", &volume])
        .output()
        .expect("failed to run docker");
    assert!(created.status.success(), "{created:?}");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);

    let output = ratect_command()
        .current_dir(&project)
        .arg("--clean")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run ratect");
    let remaining = Command::new("docker")
        .args(["volume", "ls", "-q", "--filter", &format!("name={volume}")])
        .output()
        .expect("failed to run docker");
    let _ = Command::new("docker")
        .args(["volume", "rm", &volume])
        .output();
    std::fs::remove_dir_all(&project).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(stderr, "");
    assert!(remaining.stdout.is_empty(), "{volume} is still there");
}

/// #441: an invalid `BATECT_CACHE_TYPE` fails every command before it
/// does anything — `2` off a run, `125` on one — naming the variable, the
/// value and the cache types; `--clean` removes nothing.
#[test]
fn an_invalid_batect_cache_type_fails_every_command_before_it_runs() {
    let (project, cache) = project_with_a_cache_directory("cache-type-invalid", "deps");
    for (arguments, code) in [
        (&["--list-tasks"][..], 2),
        (&["--clean"][..], 2),
        (&["-o", "quiet", "--list-tasks"][..], 2),
        (&["t"][..], 125),
    ] {
        let output = isolated_docker_command(&project)
            .env("BATECT_CACHE_TYPE", "bogus")
            .env("DOCKER_HOST", "tcp://127.0.0.1:1")
            .args(arguments)
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{arguments:?}: {stderr}");
        assert!(
            stderr.contains("BATECT_CACHE_TYPE is 'bogus'")
                && stderr.contains("volume")
                && stderr.contains("directory"),
            "{arguments:?}: {stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{arguments:?}");
    }
    assert!(cache.exists(), "--clean removed a cache despite the error");
    std::fs::remove_dir_all(&project).ok();
}

/// #441: `--cache-type` makes an invalid `BATECT_CACHE_TYPE` moot, and
/// `--help` and `--version` never check it.
#[test]
fn an_invalid_batect_cache_type_is_moot_beside_the_flag_and_for_help() {
    let (project, _) = project_with_a_cache_directory("cache-type-moot", "deps");
    for arguments in [
        &["--clean", "--cache-type", "directory"][..],
        &["--help"][..],
        &["--version"][..],
    ] {
        let output = isolated_docker_command(&project)
            .env("BATECT_CACHE_TYPE", "bogus")
            .args(arguments)
            .output()
            .expect("failed to run ratect");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(&project).ok();
}

/// #450: a `batect.local.yml` that exists but isn't a file fails every
/// command before it does anything — before `--clean` removes a cache,
/// before `-f`'s file is looked for — in every output style, naming it as
/// `--config-vars-file`'s default; `--help` and `--version` don't check it,
/// and an unknown flag is still reported first.
#[test]
fn a_batect_local_yml_that_is_not_a_file_fails_every_command_before_it_runs() {
    let (project, cache) = project_with_a_cache_directory("local-yml-dir", "deps");
    std::fs::create_dir(project.join("batect.local.yml")).unwrap();
    let message = "--config-vars-file's default in the current directory, is not a file.";
    for arguments in [
        &["--list-tasks"][..],
        &["-o", "quiet", "--list-tasks"][..],
        &["--clean", "--cache-type", "directory"][..],
        &["-f", "missing.yml", "--list-tasks"][..],
    ] {
        let output = isolated_docker_command(&project)
            .args(arguments)
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {stderr}");
        assert!(
            stderr.contains("batect.local.yml") && stderr.contains(message),
            "{arguments:?}: {stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{arguments:?}");
    }
    assert!(cache.exists(), "--clean removed a cache despite the error");

    for flag in ["--help", "--version"] {
        let output = isolated_docker_command(&project)
            .arg(flag)
            .output()
            .expect("failed to run ratect");
        assert_eq!(output.status.code(), Some(0), "{flag}");
    }
    let output = isolated_docker_command(&project)
        .args(["--bogus", "--list-tasks"])
        .output()
        .expect("failed to run ratect");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--bogus") && !stderr.contains(message),
        "{stderr}"
    );
    std::fs::remove_dir_all(&project).ok();
}

/// #450: an explicit value makes an invalid default moot.
#[test]
fn an_explicit_option_makes_an_invalid_default_moot() {
    let project = one_task_project("defaults-moot");
    std::fs::create_dir(project.join("batect.local.yml")).unwrap();
    std::fs::write(project.join("vars.yml"), "{}\n").unwrap();
    let output = isolated_docker_command(&project)
        .env("BATECT_CACHE_TYPE", "bogus")
        .args(["--config-vars-file", "vars.yml", "--cache-type", "volume"])
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&project).ok();

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ratect#438: an empty side in `--override-image`, or a container given
/// to it twice, is an argument error — reported before the config is read,
/// with the code every other argument error on that path gets (`2` beside
/// `--list-tasks`, `125` on a run) and nothing on stdout.
#[test]
fn an_empty_or_repeated_override_image_is_an_argument_error() {
    let config = sample_config_path();
    for (arguments, message, code) in [
        (
            &["--override-image", "build-env=", "--list-tasks"][..],
            "the image after '=' is empty",
            2,
        ),
        (
            &[
                "--override-image",
                "build-env=a",
                "--override-image",
                "build-env=b",
                "--list-tasks",
            ],
            "the container 'build-env' was given to --override-image more than once",
            2,
        ),
        (
            &["--override-image", "=alpine", "test-task"],
            "the container name before '=' is empty",
            125,
        ),
    ] {
        let output = ratect_command()
            .arg("-f")
            .arg(&config)
            .args(arguments)
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{arguments:?}: {stderr}");
        assert!(stderr.contains(message), "{arguments:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{arguments:?}");
    }
}

/// ratect#439: `--docker-context` beside any of the seven flags Batect's
/// `validate()` forbids with it is an invalid option value on every
/// command, not only one that connects — `--list-tasks` and a `--clean` that
/// never needs Docker (`2`), and a run whose config doesn't exist, which no
/// longer masks it (`125`) — naming the first flag in Batect's order, with
/// nothing on stdout. Real paths, so no check on a path's value fires first.
/// `DOCKER_TLS_VERIFY` set beside it is no conflict, as in Batect.
#[test]
fn docker_context_beside_a_connection_flag_is_refused_on_every_command() {
    let config = sample_config_path();
    let config = config.to_str().unwrap();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let directory = directory.to_str().unwrap();
    let refused = |arguments: &[&str], named: &str, code: i32| {
        let output = ratect_command()
            .args(["--docker-context", "x"])
            .args(arguments)
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{arguments:?}: {stderr}");
        assert!(
            stderr.contains(&format!("Cannot use both --docker-context and {named}.")),
            "{arguments:?}: {stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{arguments:?}");
    };
    for (flag, value) in [
        ("--docker-host", Some("tcp://1.2.3.4:1")),
        ("--docker-tls", None),
        ("--docker-tls-verify", None),
        ("--docker-cert-path", Some(directory)),
        ("--docker-tls-ca-cert", Some(config)),
        ("--docker-tls-cert", Some(config)),
        ("--docker-tls-key", Some(config)),
    ] {
        let flag_and_value: Vec<&str> = std::iter::once(flag).chain(value).collect();
        refused(
            &[&flag_and_value[..], &["-f", config, "--list-tasks"]].concat(),
            flag,
            2,
        );
    }
    refused(
        &[
            "--docker-tls-verify",
            "-f",
            config,
            "--clean",
            "--cache-type=directory",
        ],
        "--docker-tls-verify",
        2,
    );
    refused(
        &[
            "--docker-host",
            "tcp://1.2.3.4:1",
            "-f",
            "does-not-exist.yml",
            "test-task",
        ],
        "--docker-host",
        125,
    );
    refused(
        &[
            "--docker-tls-ca-cert",
            config,
            "--docker-cert-path",
            directory,
            "-f",
            config,
            "--list-tasks",
        ],
        "--docker-tls-ca-cert",
        2,
    );

    let output = ratect_command()
        .env("DOCKER_TLS_VERIFY", "1")
        .args(["-f", config, "--docker-context", "x", "--list-tasks"])
        .output()
        .expect("failed to run ratect");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ratect#437: `-f link/../batect.yml` reads `./batect.yml`, as Batect's
/// lexical normalisation does, not the `batect.yml` beside the symlink's
/// target, which the operating system would reach by following `link` first.
#[cfg(unix)]
#[test]
fn a_dot_dot_after_a_symlink_in_the_config_path_is_resolved_lexically() {
    let project =
        std::env::temp_dir().join(format!("ratect-symlink-dotdot-{}", std::process::id()));
    std::fs::create_dir_all(project.join("other/inner")).unwrap();
    let config = |name: &str| {
        format!(
            "project_name: {name}\ntasks:\n  t:\n    run:\n      container: c\n\
             containers:\n  c:\n    image: alpine\n"
        )
    };
    std::fs::write(project.join("batect.yml"), config("here")).unwrap();
    std::fs::write(project.join("other/batect.yml"), config("there")).unwrap();
    std::os::unix::fs::symlink("other/inner", project.join("link")).unwrap();
    let output = ratect_command()
        .current_dir(&project)
        .args(["-f", "link/../batect.yml", "--list-tasks"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&project).ok();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Tasks in here:"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ratect#437: an explicit `-f` is checked before any command runs, as
/// Batect's option parser checks it — so `--clean`, which never reads the
/// config, still refuses one naming no file. The default `batect.yml` isn't
/// checked there, as Batect doesn't check its static default.
#[test]
fn clean_refuses_an_explicit_config_file_naming_no_file() {
    let project = std::env::temp_dir().join(format!("ratect-clean-no-f-{}", std::process::id()));
    std::fs::create_dir_all(&project).unwrap();
    let clean = |arguments: &[&str]| {
        ratect_command()
            .current_dir(&project)
            .args(arguments)
            .args(["--clean", "--cache-type=directory"])
            .output()
            .expect("failed to run ratect")
    };
    let explicit = clean(&["-f", "missing.yml"]);
    let defaulted = clean(&[]);
    std::fs::remove_dir_all(&project).ok();

    let stderr = String::from_utf8_lossy(&explicit.stderr);
    assert_eq!(explicit.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("Configuration file \"missing.yml\" not found."),
        "{stderr}"
    );
    assert_eq!(
        defaulted.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&defaulted.stderr)
    );
}

/// ratect#437: `--log-file` naming a directory or something else that isn't
/// a file (`/dev/null`) is an invalid option value, as Batect's option
/// parser makes it. One that doesn't exist yet is created — through a
/// lexically removed `..`, so `sub/../out.log` needs no `sub` — and gets
/// the log (CLO-4, VC-19).
#[cfg(unix)]
#[test]
fn log_file_must_be_a_file_or_not_yet_exist() {
    let config = sample_config_path();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (value, problem) in [
        (directory.as_path(), "is a directory, not a file"),
        (Path::new("/dev/null"), "is not a file"),
    ] {
        let output = ratect_command()
            .arg("-f")
            .arg(&config)
            .arg("--log-file")
            .arg(value)
            .arg("--list-tasks")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{value:?}: {stderr}");
        assert!(
            stderr.contains(&format!("--log-file {value:?} {problem}.")),
            "{value:?}: {stderr}"
        );
    }

    // A run against a daemon that refuses the connection: it fails, but
    // only after logging, at `debug`, where it tried to connect.
    let project = std::env::temp_dir().join(format!("ratect-new-log-{}", std::process::id()));
    std::fs::create_dir_all(&project).unwrap();
    let output = ratect_command()
        .current_dir(&project)
        .env("RUST_LOG", "debug")
        .env("DOCKER_HOST", "tcp://127.0.0.1:9")
        .arg("-f")
        .arg(&config)
        .args(["--log-file", "sub/../out.log", "test-task"])
        .output()
        .expect("failed to run ratect");
    let log = std::fs::read_to_string(project.join("out.log"));
    std::fs::remove_dir_all(&project).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(125), "{stderr}");
    let log = log.expect("--log-file sub/../out.log was not created as ./out.log");
    assert!(
        log.contains("127.0.0.1:9"),
        "the log file holds no log: {log:?}"
    );
}

/// ratect#437: a file `--docker-tls-ca-cert`, `--docker-tls-cert` or
/// `--docker-tls-key` names must exist and not be a directory on every
/// command, as Batect's option parser checks it, not only when a TLS
/// connection reads it — an invalid option value, so `2` beside
/// `--list-tasks`. Something that exists but isn't a regular file passes,
/// as a pipe (`<(…)`) must.
#[cfg(unix)]
#[test]
fn a_missing_or_directory_tls_file_is_refused_on_every_command() {
    let config = sample_config_path();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let list = |option: &str, value: &Path| {
        ratect_command()
            .arg("-f")
            .arg(&config)
            .arg(option)
            .arg(value)
            .arg("--list-tasks")
            .output()
            .expect("failed to run ratect")
    };
    for (option, value, problem) in [
        (
            "--docker-tls-ca-cert",
            Path::new("no-such-ca.pem"),
            "does not exist",
        ),
        (
            "--docker-tls-cert",
            Path::new("no-such-cert.pem"),
            "does not exist",
        ),
        ("--docker-tls-key", &directory, "is a directory, not a file"),
    ] {
        let output = list(option, value);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{option}: {stderr}");
        assert!(
            stderr.contains(&format!("{option} {value:?} {problem}.")),
            "{option}: {stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{option}");
    }

    let output = list("--docker-tls-ca-cert", Path::new("/dev/null"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// ratect#437: `DOCKER_CERT_PATH` is resolved as `--docker-cert-path` is,
/// so a leading `~` is the passwd home — shown by the check refusing a
/// missing one, which names the resolved path, as Batect's does.
#[test]
fn a_tilde_in_docker_cert_path_is_the_home_directory() {
    let missing = "ratect-437-no-such-cert-directory";
    let output = ratect_command()
        .env("DOCKER_CERT_PATH", format!("~/{missing}"))
        .arg("-f")
        .arg(sample_config_path())
        .arg("--list-tasks")
        .output()
        .expect("failed to run ratect");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let home = ratect_core::user::home_directory().unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "DOCKER_CERT_PATH {:?} does not exist.",
            home.join(missing)
        )),
        "{stderr}"
    );
}

/// What both binaries print for `--tag-image build-env=…` beside
/// `--override-image build-env=…` — Batect's own words (ratect#436). Native
/// `ratect`'s own tests assert the same literal.
const TAG_AND_OVERRIDE_CONFLICT: &str = "Error: Cannot both tag the built image for container \
    'build-env' and also override the image for that container.\n";

/// ratect#436: a container both tagged and overridden is refused before the
/// config is read or anything runs — on `--list-tasks`, on a run whose task
/// has prerequisites, with a config file that doesn't exist, and for a
/// container the config doesn't declare — as an argument error on its path.
#[test]
fn tagging_and_overriding_one_container_is_refused_before_anything_runs() {
    let smoke = sample_config_path();
    let smoke = smoke.to_str().unwrap();
    let sidecar = sidecar_config_path();
    let conflict = [
        "--tag-image",
        "build-env=t",
        "--override-image",
        "build-env=alpine",
    ];
    for (config, extra, code) in [
        (smoke, &["--list-tasks"][..], 2),
        (smoke, &["-o", "simple", "test-task"], 125),
        ("nope.yml", &["--list-tasks"], 2),
        // The same pair, naming a container the config doesn't declare.
        (sidecar.to_str().unwrap(), &["--list-tasks"], 2),
    ] {
        let output = isolated_docker_command(Path::new(env!("CARGO_MANIFEST_DIR")))
            .env("DOCKER_HOST", "tcp://127.0.0.1:1")
            .args(["-f", config])
            .args(conflict)
            .args(extra)
            .output()
            .expect("failed to run ratect");
        assert_eq!(output.status.code(), Some(code), "{config} {extra:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            TAG_AND_OVERRIDE_CONFLICT,
            "{config} {extra:?}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{extra:?}");
    }

    // Tagging one container and overriding another is no conflict.
    let output = ratect_command()
        .args(["-f", smoke, "--tag-image", "build-env=t"])
        .args(["--override-image", "other=alpine", "--list-tasks"])
        .output()
        .expect("failed to run ratect");
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    // `--help` and `--version` act before the check, as before any other.
    for flag in ["--help", "--version"] {
        let output = ratect_command()
            .args(conflict)
            .arg(flag)
            .output()
            .expect("failed to run ratect");
        assert_eq!(output.status.code(), Some(0), "{flag}");
    }
}

/// ratect#436: `--clean` with the conflict removes nothing.
#[test]
fn clean_with_a_container_both_tagged_and_overridden_removes_nothing() {
    let (project, cache) = project_with_a_cache_directory("tag-override-clean", "deps");
    let output = isolated_docker_command(&project)
        .args(["--clean", "--cache-type", "directory"])
        .args([
            "--tag-image",
            "build-env=t",
            "--override-image",
            "build-env=a",
        ])
        .output()
        .expect("failed to run ratect");
    let kept = cache.exists();
    std::fs::remove_dir_all(&project).ok();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        TAG_AND_OVERRIDE_CONFLICT
    );
    assert!(kept, "--clean removed a cache despite the conflict");
}

fn log_driver_none_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-driver-none.yml")
}

/// Whether `stdout`, under `-o all`, has a line `line` prefixed with
/// `container`'s name — `<container><padding> | <line>`.
fn has_interleaved_line(stdout: &str, container: &str, line: &str) -> bool {
    stdout.lines().any(|candidate| {
        candidate
            .split_once(" | ")
            .is_some_and(|(prefix, rest)| prefix.trim_end() == container && rest == line)
    })
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A task container whose `log_driver` Docker can't read back still shows
/// its output, in both the default and the `all` style, and the task
/// succeeds (ratect#517): output is read by attaching to the container, as
/// Batect does. The marker is the command's first and only write, so this
/// also proves the attach is made before the container starts — `none`
/// keeps nothing to replay.
#[test]
#[ignore]
fn log_driver_none_task_output_is_shown_via_docker() {
    let marker = "ratect-log-driver-none-marker";
    for style in ["simple", "all"] {
        let output = ratect_command()
            .args(["-o", style, "-f"])
            .arg(log_driver_none_config_path())
            .arg("say")
            .output()
            .expect("failed to run ratect");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "-o {style}: {output:?}");
        if style == "simple" {
            assert_eq!(task_output(&stdout, "box"), marker);
        } else {
            assert!(
                has_interleaved_line(&stdout, "box", marker),
                "-o all: missing box's marker line:\n{stdout}"
            );
        }
    }
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The stdin-forwarding path (stdin piped, so no TTY) reads a `log_driver:
/// none` task container's output too (ratect#517), from the same attach that
/// carries its stdin.
#[test]
#[ignore]
fn log_driver_none_task_container_takes_piped_stdin_via_docker() {
    let marker = "ratect-log-driver-none-stdin-marker";
    let mut child = ratect_command()
        .arg("-f")
        .arg(log_driver_none_config_path())
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ratect");
    let mut stdin = child.stdin.take().expect("child should have piped stdin");
    writeln!(stdin, "echo {marker}").expect("failed to write to piped stdin");
    drop(stdin);

    let output = child.wait_with_output().expect("failed to wait for ratect");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(task_output(&stdout, "box"), marker);
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Under `-o all`, a dependency whose `log_driver` Docker can't read back
/// has its output shown as interleaved lines (ratect#517) — its follower
/// attaches before that container starts, so the marker it echoes at once
/// isn't lost — and the task succeeds.
#[test]
#[ignore]
fn log_driver_none_dependency_output_is_shown_under_all_via_docker() {
    let output = ratect_command()
        .args(["-o", "all", "-f"])
        .arg(log_driver_none_config_path())
        .arg("with-dependency")
        .output()
        .expect("failed to run ratect");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(
        has_interleaved_line(&stdout, "noisy", "ratect-log-driver-none-dependency-marker"),
        "missing noisy's marker line:\n{stdout}"
    );
}
