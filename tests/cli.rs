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

fn build_failure_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build-failure.yml")
}

fn project_directory_declared_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project-directory-declared.yml")
}

fn interactive_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interactive.yml")
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

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Proves a real failing `docker build`'s full transcript (not just
/// Docker's one-line failure summary) reaches `ratect`'s own error output —
/// `build_output_suffix`'s unit tests already cover the string formatting in
/// isolation, but only a real Docker daemon actually exercises the
/// streaming/`error_detail` wiring in `DockerClient::build_image` that feeds
/// it.
#[test]
#[ignore]
fn failing_build_output_reaches_the_error() {
    let output = ratect_command()
        .arg("-f")
        .arg(build_failure_config_path())
        .arg("build")
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
      command: id -u && id -g && touch /output/marker
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
