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
use std::sync::{Mutex, MutexGuard};

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect"))
}

/// Serializes the real-Docker tests below. They share one Docker daemon, and
/// [`all_projects_never_reaches_containers_ratect_did_not_create`] runs a
/// machine-wide `resources clean --all-projects` sweep that, by design,
/// removes *every* Ratect-created container and network regardless of
/// project — so it must never overlap another test here, which would lose
/// its containers mid-assertion (a real, timing-dependent CI flake, rare on
/// a fast machine but reliable on a slow runner). A process-wide lock is
/// enough on its own: Cargo runs test binaries one at a time, so only the
/// within-binary parallelism needs taming — no need for a special
/// `--test-threads=1` invocation that a contributor running these locally
/// would have to remember. The lock is recovered from poisoning so one
/// failing test reports its own assertion rather than cascading a
/// `PoisonError` onto every test after it.
static DOCKER_SERIAL: Mutex<()> = Mutex::new(());

fn serial_docker() -> MutexGuard<'static, ()> {
    DOCKER_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tasks.yml")
}

/// The native-format twin of [`fixture_path`] — a `ratect.toml` using
/// `extends`, for proving the native config path end to end.
fn native_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.toml")
}

/// A unique, empty temp directory to stand up a small project in.
fn unique_project_dir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "ratect-project-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// A `ratect.local.toml` beside the config file is auto-discovered as
/// config-variable values — no `--config-vars-file` needed, this binary's
/// native equivalent of `ratect-compat`'s `batect.local.yml` default. Proven
/// with a required config variable (no default) that only the local file
/// supplies: `tasks list` resolves the config, so it fails without the file
/// and succeeds once it's there.
#[test]
fn ratect_local_toml_is_auto_discovered_for_config_variables() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"

[containers.app]
image = "alpine:3.18"
environment = { GREETING = "<greeting" }

[config_variables.greeting]

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();

    let without = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["tasks", "list"])
        .output()
        .expect("failed to run ratect");
    assert!(
        !without.status.success(),
        "a required config variable with no local file should fail to resolve"
    );

    std::fs::write(dir.join("ratect.local.toml"), "greeting = \"hello\"\n").unwrap();
    let with = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["tasks", "list"])
        .output()
        .expect("failed to run ratect");
    assert!(
        with.status.success(),
        "ratect.local.toml should be discovered automatically:\n{}",
        String::from_utf8_lossy(&with.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `completions <shell>` prints a dynamic registration script and reaches
/// nothing to do it — no config, no daemon. (The script *installs* the callback
/// that reads the config later, at `<TAB>` time.)
#[test]
fn completions_generates_a_registration_script() {
    let output = ratect_command()
        .args(["completions", "bash"])
        .output()
        .expect("failed to run ratect");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("_clap_complete_ratect") && stdout.contains("COMPLETE="),
        "expected a dynamic bash completion registration script:\n{stdout}"
    );
}

/// The precedence `--config-var` is documented to have over the config-vars
/// file (ADR-0003: defaults < file < `--config-var`). Auto-discovery was
/// pinned, but not the override — and the load-bearing line is a single
/// `.extend()` whose argument order decides it.
///
/// Also covers an explicitly-named `.yml` vars file end to end, so the
/// extension-driven parser choice is exercised through the real binary rather
/// than only as a unit.
#[test]
fn a_command_line_config_var_overrides_the_config_vars_file() {
    let dir = unique_project_dir();
    // `build_directory` is expression-bearing, and `config validate` reports the
    // *resolved* path in its "doesn't exist" problem — which is what makes
    // which value won observable without running anything.
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"

[containers.app]
build_directory = "<{which}"

[config_variables.which]

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();
    std::fs::write(dir.join("ratect.local.toml"), "which = \"from-file\"\n").unwrap();

    let resolved_directory = |args: &[&str]| {
        let output = ratect_command()
            .arg("-f")
            .arg(dir.join("ratect.toml"))
            .args(["config", "validate"])
            .args(args)
            .output()
            .expect("failed to run ratect");
        String::from_utf8_lossy(&output.stdout).to_string()
    };

    // The auto-discovered file supplies the value...
    assert!(
        resolved_directory(&[]).contains("from-file"),
        "the auto-discovered ratect.local.toml should supply the value"
    );
    // ...and an explicit --config-var beats it.
    let from_cli = resolved_directory(&["--config-var", "which=from-cli"]);
    assert!(
        from_cli.contains("from-cli") && !from_cli.contains("from-file"),
        "--config-var must win over the config-vars file:\n{from_cli}"
    );

    // An explicitly-named YAML vars file is parsed as YAML (by extension) and
    // replaces the auto-discovered TOML one.
    std::fs::write(dir.join("overrides.yml"), "which: from-yaml\n").unwrap();
    let from_yaml = resolved_directory(&[
        "--config-vars-file",
        dir.join("overrides.yml").to_str().unwrap(),
    ]);
    assert!(
        from_yaml.contains("from-yaml") && !from_yaml.contains("from-file"),
        "an explicit .yml vars file should replace the auto-discovered one:\n{from_yaml}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `config validate` is `doctor`'s config-only half: it passes a good config
/// (exit 0, no daemon touched) and fails one with a problem (exit non-zero),
/// which is what makes it usable as a CI gate.
#[test]
fn config_validate_passes_a_good_config_and_fails_a_broken_one() {
    let good = ratect_command()
        .arg("-f")
        .arg(native_fixture_path())
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    assert!(
        good.status.success(),
        "the native fixture should validate:\n{}",
        String::from_utf8_lossy(&good.stderr)
    );

    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"

[containers.app]
build_directory = "does-not-exist"

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();
    let bad = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    assert!(
        !bad.status.success(),
        "a missing build_directory is a problem and should fail validation"
    );
    assert!(
        String::from_utf8_lossy(&bad.stdout).contains("does-not-exist"),
        "the problem should name the missing directory:\n{}",
        String::from_utf8_lossy(&bad.stdout)
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `config convert` turns a `batect.yml` (anchors and all) into a
/// `ratect.toml` that actually loads and runs — proven by listing tasks from
/// the *converted* file. Also covers the no-clobber guard and `--force`.
#[test]
fn config_convert_produces_a_loadable_ratect_toml() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
containers:
  base: &base
    image: alpine:3.18
  app:
    <<: *base
tasks:
  build:
    description: Build it
    run:
      container: app
      command: echo hi
"#,
    )
    .unwrap();

    let convert = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(
        convert.status.success(),
        "convert failed:\n{}",
        String::from_utf8_lossy(&convert.stderr)
    );
    assert!(dir.join("ratect.toml").is_file());

    // The generated file loads and lists the task (image reached `app` via the
    // inlined anchor, or the task couldn't have resolved).
    let list = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["tasks", "list", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");
    assert!(
        list.status.success(),
        "the converted file should load:\n{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&list.stdout), "build\tBuild it\n");

    // No-clobber: converting again refuses to overwrite unless forced.
    let again = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(
        !again.status.success(),
        "should refuse to overwrite ratect.toml"
    );
    let forced = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "convert", "--force"])
        .output()
        .expect("failed to run ratect");
    assert!(forced.status.success(), "--force should overwrite");

    std::fs::remove_dir_all(&dir).ok();
}

/// `--stdout` prints the converted config and writes *no* file — the reviewable
/// form. Only the `--stdout`/`--force` argument conflict was pinned before, not
/// the behaviour, so a regression that wrote the file anyway (or tripped the
/// no-clobber check) would have gone unnoticed.
#[test]
fn config_convert_to_stdout_writes_no_file() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\ntasks:\n  t:\n    run:\n      container: app\n",
    )
    .unwrap();

    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "convert", "--stdout"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Generated by") && stdout.contains("project_name = \"demo\""),
        "expected the converted document on stdout:\n{stdout}"
    );
    assert!(
        !dir.join("ratect.toml").exists(),
        "--stdout must not write a file"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A container used only as an `extends` base needs neither `image` nor
/// `build_directory` — ADR-0003's "no `abstract` marker needed", which holds
/// because that requirement is enforced lazily, only for containers a task
/// actually runs. Nothing pinned it, so an eager check added later would have
/// silently broken the format's headline reuse pattern.
#[test]
fn a_base_only_container_needs_no_image_and_validates() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"

# No image and no build_directory: this exists only to be extended.
[containers.base]
environment = { TZ = "UTC" }

[containers.app]
extends = "base"
image = "alpine:3.18"

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();

    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    assert!(
        output.status.success(),
        "a base-only container should be valid:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// With no `-f`, `config convert` reads `batect.yml` (the source), not the
/// `ratect.toml` that `-f` otherwise defaults to (the output) — otherwise the
/// command would try to convert its own output format.
#[test]
fn config_convert_without_an_explicit_file_reads_batect_yml() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\ntasks:\n  t:\n    run:\n      container: app\n",
    )
    .unwrap();

    let convert = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(
        convert.status.success(),
        "convert with no -f should find batect.yml:\n{}",
        String::from_utf8_lossy(&convert.stderr)
    );
    assert!(dir.join("ratect.toml").is_file());

    std::fs::remove_dir_all(&dir).ok();
}

/// The native format parses and its tasks list — no Docker needed. That the
/// list is complete also proves `extends` resolved (`build-env` inherits its
/// image from `base`), since a container that failed to resolve wouldn't stop
/// the tasks listing, but a parse failure would stop everything.
#[test]
fn tasks_list_reads_a_native_toml_config() {
    let output = ratect_command()
        .arg("-f")
        .arg(native_fixture_path())
        .args(["tasks", "list", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "build\tBuild the thing\ncheck\tCheck the thing\n"
    );
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

/// `doctor` is meant to be usable as a CI step, which means its exit code
/// has to mean something: non-zero when it found something that will fail a
/// run, zero when it only found things worth knowing.
#[test]
fn doctor_exits_non_zero_only_for_problems() {
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .arg("doctor")
        .output()
        .expect("failed to run ratect");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Matched on a finding *line*, not the word: the summary line always
    // says "N problem(s)", so a substring search takes the wrong branch
    // even when N is zero.
    let found_a_problem = stdout
        .lines()
        .any(|line| line.trim_start().starts_with("problem "));

    // This fixture pins its image and has no dependencies, so the only
    // thing that can fail here is the Docker check — which may legitimately
    // fail on a machine with no daemon, and that's a problem by design.
    if found_a_problem {
        assert!(
            !output.status.success(),
            "a reported problem must fail the command:\n{stdout}"
        );
    } else {
        assert!(
            output.status.success(),
            "no problems means success:\n{stdout}"
        );
    }
}

#[test]
fn doctor_reports_a_config_that_does_not_load_as_a_problem() {
    let output = ratect_command()
        .args(["-f", "/nonexistent/batect.yml", "doctor"])
        .output()
        .expect("failed to run ratect");

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with("problem ")
                && line.contains("does not load")),
        "the missing config should be reported as a problem:\n{stdout}"
    );
    // The daemon check still ran: being told both at once is the point.
    assert!(
        stdout.contains("Docker daemon"),
        "the environment checks shouldn't be skipped because the config is broken:\n{stdout}"
    );
}

/// A scratch project directory containing `.batect/caches/<name>` for each
/// name given — the on-disk shape `--cache-type directory` acts on, which
/// makes the whole `caches` verb testable without a Docker daemon.
fn project_with_directory_caches(names: &[&str]) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Timestamped as well as pid-and-counter keyed, matching
    // `ratect-core`'s own `unique_temp_dir`: a test that fails part-way
    // leaves its directory behind, and process ids do get reused, so
    // without this a later run could inherit an earlier one's caches and
    // see state it never created.
    let directory = std::env::temp_dir().join(format!(
        "ratect-caches-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
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
    let _guard = serial_docker();
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

/// The native config path, end to end against a real daemon: `ratect` runs a
/// task from a `ratect.toml` whose container gets its image via `extends`.
/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn run_executes_a_task_from_a_native_toml_config_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(native_fixture_path())
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
        "the task run via extends-resolved container should reach stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The ownership labels themselves are `ratect-core`'s, proven end to end in
/// `ratect-compat`'s suite against a graph with dependencies — there's no
/// value in running the same core behavior twice. What's genuinely per-binary
/// is the *version*: each `main.rs` passes its own `CARGO_PKG_VERSION` into
/// `TaskEngineSettings`, because `ratect-core`'s own version isn't what a
/// user sees from `--version`. Nothing else would fail if `ratect`'s line
/// were deleted, or made to read the core's version instead — hence this.
///
/// Being in `ratect`'s own crate, the test knows exactly which version to
/// expect, rather than only that *some* version is present.
#[test]
#[ignore]
fn a_run_stamps_this_binarys_own_version_onto_what_it_creates() {
    let _guard = serial_docker();
    // `tests/fixtures/labels.yml` has a project name of its own: even with
    // the tests here serialized (see `serial_docker`), a distinct project
    // keeps this one from finding a leftover carried by `tasks.yml`'s
    // project, which `run_executes_a_task_via_docker` also uses.
    let filter = "label=eu.orican.ratect.project=ratect-cli-labels-test";

    let output = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/labels.yml"))
        .args(["run", "build", "--no-cleanup-after-success"])
        .output()
        .expect("failed to run ratect");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // `-a`: the task's own container has already exited by now, and a bare
    // `ls` lists only running ones.
    let listed = Command::new("docker")
        .args(["container", "ls", "-aq", "--filter", filter])
        .output()
        .expect("failed to run docker container ls");
    let ids: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .map(str::to_string)
        .collect();

    let versions: Vec<String> = ids
        .iter()
        .map(|id| {
            let inspected = Command::new("docker")
                .args([
                    "container",
                    "inspect",
                    id,
                    "--format",
                    "{{index .Config.Labels \"eu.orican.ratect.version\"}}",
                ])
                .output()
                .expect("failed to run docker container inspect");
            String::from_utf8_lossy(&inspected.stdout)
                .trim()
                .to_string()
        })
        .collect();

    // Torn down before asserting, so a failure doesn't strand containers.
    for id in &ids {
        let _ = Command::new("docker").args(["rm", "-fv", id]).output();
    }
    let _ = Command::new("docker")
        .args(["network", "prune", "-f", "--filter", filter])
        .output();

    assert_eq!(ids.len(), 1, "expected the task's own container");
    assert_eq!(versions, vec![env!("CARGO_PKG_VERSION").to_string()]);
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// `--all-projects` means every project *Ratect* created, not every
/// container on the machine. It first meant the latter: the option cleared
/// the label filter entirely, and an unfiltered listing is everything the
/// daemon has — on the machine this was found on, 105 unrelated containers
/// and Docker's own `bridge`/`host`/`none` networks, all of which
/// `resources clean --all-projects` would have tried to stop and remove.
///
/// So this runs a container Ratect knows nothing about and proves it is
/// neither listed nor removed. The cost of getting this wrong is someone
/// else's work, which is why it's pinned rather than left to the filter
/// being obviously right.
#[test]
#[ignore]
fn all_projects_never_reaches_containers_ratect_did_not_create() {
    let _guard = serial_docker();
    let name = "ratect-bystander-test";
    let _ = Command::new("docker").args(["rm", "-f", name]).output();
    let started = Command::new("docker")
        .args(["run", "-d", "--name", name, "alpine:3.18.2", "sleep", "120"])
        .output()
        .expect("failed to start the bystander container");
    assert!(
        started.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&started.stderr)
    );

    let listed = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resources.yml"))
        .args(["resources", "list", "--all-projects", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");
    let ids = String::from_utf8_lossy(&listed.stdout).to_string();

    let cleaned = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resources.yml"))
        .args(["resources", "clean", "--all-projects"])
        .output()
        .expect("failed to run ratect");

    let survived = Command::new("docker")
        .args([
            "container",
            "ls",
            "-a",
            "--filter",
            &format!("name={name}"),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .expect("failed to check the bystander container");
    let survived = String::from_utf8_lossy(&survived.stdout).trim().to_string();
    let _ = Command::new("docker").args(["rm", "-f", name]).output();

    assert!(cleaned.status.success());
    // Docker's own networks carry no labels, so a key-existence filter
    // excludes them; an unfiltered listing would have included all three.
    for builtin in ["bridge", "host", "none"] {
        let exists = Command::new("docker")
            .args(["network", "inspect", builtin, "--format", "{{.Name}}"])
            .output()
            .expect("failed to inspect a built-in network");
        assert!(exists.status.success(), "{builtin} should still exist");
    }
    assert_eq!(
        survived, name,
        "a container Ratect never created must survive `clean --all-projects`"
    );
    assert!(
        !ids.contains(name),
        "it should not have been listed either:\n{ids}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// The whole point of the labels, end to end: strand a run's resources with
/// `--no-cleanup-after-success` — one of the real ways leftovers happen —
/// then find them again and remove them, without the configuration having
/// anything to say about which ones they are.
///
/// Uses a fixture project of its own (`resources.yml`) for a stronger
/// reason than the other tests do: this one runs `resources clean`, which
/// removes everything carrying that project label — sharing a project with
/// another test would sweep away *its* containers mid-assertion, which is
/// how this fixture came to exist.
#[test]
#[ignore]
fn resources_finds_and_removes_what_a_previous_run_left_behind() {
    let _guard = serial_docker();
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resources.yml");
    let ratect = |arguments: &[&str]| {
        let mut command = ratect_command();
        command.arg("-f").arg(&config);
        command.args(arguments);
        command.output().expect("failed to run ratect")
    };

    // A clean slate: an earlier failed run of this test would otherwise
    // make the counts below meaningless.
    ratect(&["resources", "clean"]);

    let run = ratect(&["run", "build", "--no-cleanup-after-success"]);
    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let listed = ratect(&["resources", "list"]);
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listing.contains("container build-env"),
        "the container should be named as the config names it:\n{listing}"
    );
    assert!(
        listing.contains("network ratect-"),
        "its network should be listed too:\n{listing}"
    );
    assert!(
        listing.contains("build ("),
        "the task that created them should be named:\n{listing}"
    );

    // Quiet is ids only, ready to pipe — two of them, container and network.
    let quiet = ratect(&["resources", "list", "-o", "quiet"]);
    let ids = String::from_utf8_lossy(&quiet.stdout);
    assert_eq!(ids.lines().count(), 2, "expected two ids:\n{ids}");

    // Nothing is minutes old yet, so an age filter excludes it all — this
    // is what stops a sweep taking an in-flight run with it.
    let too_new = ratect(&["resources", "list", "--older-than", "1h"]);
    assert!(
        String::from_utf8_lossy(&too_new.stdout).contains("Nothing left over that old."),
        "an hour-old filter should exclude a run from seconds ago"
    );

    let cleaned = ratect(&["resources", "clean"]);
    assert!(
        cleaned.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&cleaned.stderr)
    );

    let after = ratect(&["resources", "list"]);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("Nothing left over."),
        "everything should be gone:\n{}",
        String::from_utf8_lossy(&after.stdout)
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
    let _guard = serial_docker();
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
    let _guard = serial_docker();
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
