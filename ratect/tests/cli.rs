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
use std::time::{Duration, Instant};

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

/// A task that exits `42`, and one whose image can't be pulled — what this
/// binary's own `Error:` line does and doesn't report.
fn exit_code_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exit-code.toml")
}

/// `run_to_completion` (ratect#97) — native-only, so its own fixture (unlike
/// most engine/Docker behaviour) lives here rather than under
/// `ratect-compat/tests/fixtures/`.
fn run_to_completion_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-to-completion.toml")
}

/// `setup_commands`' `run_in` (ratect#111) — native-only, so its own fixture
/// lives here too, for the same reason as `run_to_completion` above.
fn setup_command_run_in_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/setup-command-run-in.toml")
}

/// `external_health_check` (ratect#98) — native-only, so its own fixture
/// lives here too, for the same reason as `run_to_completion` above.
fn external_health_check_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-health-check.toml")
}

/// `stop_signal`/`stop_grace_period` (ratect#112) — native-only, so its own
/// fixture lives here too, for the same reason as `run_to_completion` above.
fn stop_signal_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stop-signal.toml")
}

/// `ulimits` (ratect#95) — native-only, so its own fixture lives here too,
/// for the same reason as `run_to_completion` above.
fn ulimits_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ulimits.toml")
}

/// `dns`/`dns_search`/`dns_options` (ratect#105) — native-only, so its own
/// fixture lives here too, for the same reason as `run_to_completion` above.
fn dns_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dns.toml")
}

/// `network_mode` (ratect#106) — native-only, so its own fixture lives here
/// too, for the same reason as `run_to_completion` above.
fn network_mode_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/network-mode.toml")
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
/// supplies: `config validate` evaluates the config, so it warns that the
/// value can't be evaluated without the file, and not once it's there.
#[test]
fn ratect_local_toml_is_auto_discovered_for_config_variables() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
ratect_version = "0.12.0"
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
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    let unevaluated = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.contains("warning") && line.contains("<greeting"))
    };
    assert!(
        unevaluated(&without),
        "a required config variable with no local file can't be evaluated:\n{}",
        String::from_utf8_lossy(&without.stdout)
    );

    std::fs::write(dir.join("ratect.local.toml"), "greeting = \"hello\"\n").unwrap();
    let with = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    assert!(
        with.status.success() && !unevaluated(&with),
        "ratect.local.toml should be discovered automatically:\n{}",
        String::from_utf8_lossy(&with.stdout)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A value only some machines have is not a mistake in the file, so a
/// container no task uses that reads one doesn't stop `tasks list`
/// (decisions/0014) — nor does a config variable with no value yet.
#[test]
fn tasks_list_succeeds_when_an_unused_container_cannot_be_evaluated() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
ratect_version = "0.12.0"
project_name = "demo"

[config_variables.region]

[containers.app]
image = "alpine:3.18"

[containers.deploy]
image = "alpine:3.18"
volumes = ["$RATECT_TEST_NEVER_SET:/root/.aws"]
environment = { REGION = "<region", OTHER = "<never_declared" }

[tasks.build]
run = { container = "app" }
"#,
    )
    .unwrap();

    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["tasks", "list", "-o", "quiet"])
        .env_remove("RATECT_TEST_NEVER_SET")
        .output()
        .expect("failed to run ratect");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build\n");

    std::fs::remove_dir_all(&dir).ok();
}

/// #382: `\` escapes in an expression, as in a `batect.yml`. A trailing one
/// escapes nothing, so `tasks list` refuses the file; an escaped `$` is
/// literal, so the same file with its variable unset loads.
#[test]
fn tasks_list_reads_a_backslash_in_an_expression_as_an_escape() {
    let dir = unique_project_dir();
    let tasks_list = |value: &str| {
        std::fs::write(
            dir.join("ratect.toml"),
            format!(
                "ratect_version = \"0.12.0\"\nproject_name = \"demo\"\n\n[containers.app]\nimage = \"alpine:3.18\"\n\
                 environment = {{ X = '{value}' }}\n\n[tasks.build]\nrun = {{ container = \"app\" }}\n"
            ),
        )
        .unwrap();
        ratect_command()
            .arg("-f")
            .arg(dir.join("ratect.toml"))
            .args(["tasks", "list", "-o", "quiet"])
            .env_remove("RATECT_UNSET_FOR_TEST")
            .output()
            .expect("failed to run ratect")
    };

    let trailing = tasks_list(r"a\");
    let escaped = tasks_list(r"\$RATECT_UNSET_FOR_TEST");
    std::fs::remove_dir_all(&dir).ok();

    let stderr = String::from_utf8_lossy(&trailing.stderr);
    assert!(!trailing.status.success(), "{stderr}");
    assert!(stderr.contains("line 6"), "{stderr}");
    assert!(stderr.contains(r"invalid expression 'a\'"), "{stderr}");
    assert!(
        stderr.contains("must be immediately followed by a character to escape"),
        "{stderr}"
    );
    assert!(
        escaped.status.success(),
        "{}",
        String::from_utf8_lossy(&escaped.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&escaped.stdout), "build\n");
}

/// #385: a malformed expression fails `tasks list`, located in the file by
/// its line and column.
#[test]
fn tasks_list_refuses_a_malformed_expression_located_in_the_file() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        "ratect_version = \"0.12.0\"\nproject_name = \"demo\"\n\n[containers.app]\n\
         image = \"alpine:3.18\"\nenvironment = { X = '${X:default}' }\n\n\
         [tasks.build]\nrun = { container = \"app\" }\n",
    )
    .unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["tasks", "list", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("line 6, column 21"), "{stderr}");
    assert!(
        stderr.contains(
            "invalid expression '${X:default}': invalid environment variable reference: ':' \
             at column 4 must be immediately followed by '-'"
        ),
        "{stderr}"
    );
}

/// `tasks list` evaluates nothing, so its config-variable options are
/// deprecated: still accepted, with a warning saying they do nothing — and
/// the file one names, or an auto-discovered `ratect.local.toml`, is not
/// even read, so a malformed one can't stop the listing.
#[test]
fn tasks_list_warns_that_its_config_variable_options_are_deprecated() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        "ratect_version = \"0.12.0\"\nproject_name = \"demo\"\n\n[containers.app]\nimage = \"alpine:3.18\"\n\n\
         [tasks.build]\nrun = { container = \"app\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("ratect.local.toml"), "not toml at all = = =\n").unwrap();

    // `RUST_LOG=off`: the warning is printed, not logged (decisions/0017).
    let list = |args: &[&str]| {
        ratect_command()
            .arg("-f")
            .arg(dir.join("ratect.toml"))
            .args(["tasks", "list", "-o", "quiet"])
            .args(args)
            .env("RUST_LOG", "off")
            .output()
            .expect("failed to run ratect")
    };

    let plain = list(&[]);
    assert!(
        plain.status.success(),
        "a malformed ratect.local.toml must not stop the listing:\n{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&plain.stderr).contains("deprecated"),
        "no option given, nothing to warn about"
    );

    for args in [
        &["--config-var", "x=1"][..],
        &["--config-vars-file", "/nonexistent/vars.toml"][..],
    ] {
        let output = list(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}:\n{stderr}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "build\n");
        assert!(
            stderr.starts_with("Warning: --config-var and --config-vars-file are deprecated")
                && stderr.contains("no effect"),
            "{args:?}:\n{stderr}"
        );
    }

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
ratect_version = "0.12.0"
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
ratect_version = "0.12.0"
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

/// A reference to an undeclared container is a problem `config validate`
/// reports, naming both sides — not something only a run would find.
#[test]
fn config_validate_reports_a_reference_to_an_undeclared_container() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
ratect_version = "0.12.0"
project_name = "demo"

[containers.app]
image = "alpine:3.18"

[tasks.t]
run = { container = "app" }
dependencies = ["ghost"]
"#,
    )
    .unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "stdout:\n{stdout}");
    assert!(
        stdout.contains("problem")
            && stdout.contains("The container 'ghost' referenced by task 't' does not exist."),
        "stdout:\n{stdout}"
    );
}

/// decisions/0014: a value this machine can't evaluate, in a container no
/// task here needs to run, is a warning `config validate` reports — each one,
/// naming the container, the field and the expression — not a failure.
#[test]
fn config_validate_warns_about_each_value_this_machine_cannot_evaluate() {
    const FIRST: &str = "RATECT_TEST_UNSET_DEPLOY_PROFILE";
    const SECOND: &str = "RATECT_TEST_UNSET_PUBLISH_TOKEN";
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        format!(
            r#"
ratect_version = "0.12.0"
project_name = "demo"

[containers.build-env]
image = "alpine:3.18"

[containers.deploy]
image = "alpine:3.18"
environment = {{ PROFILE = "${FIRST}" }}

[containers.publish]
image = "alpine:3.18"
environment = {{ TOKEN = "${SECOND}" }}

[tasks.build]
run = {{ container = "build-env" }}
"#
        ),
    )
    .unwrap();
    let validate = |set: &[&str]| {
        let mut command = ratect_command();
        command
            .arg("-f")
            .arg(dir.join("ratect.toml"))
            .args(["config", "validate"])
            .env_remove(FIRST)
            .env_remove(SECOND);
        for name in set {
            command.env(name, "set");
        }
        let output = command.output().expect("failed to run ratect");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "a warning isn't a failure:\n{stdout}"
        );
        stdout
    };
    let warns_about = |stdout: &str, container: &str, field: &str, variable: &str| {
        stdout.lines().any(|line| {
            line.trim_start().starts_with("warning ")
                && line.contains(&format!("Container '{container}'"))
                && line.contains(&format!("'{field}'"))
                && line.contains(&format!("${variable}"))
        })
    };

    let neither = validate(&[]);
    assert!(
        warns_about(&neither, "deploy", "environment.PROFILE", FIRST),
        "{neither}"
    );
    assert!(
        warns_about(&neither, "publish", "environment.TOKEN", SECOND),
        "{neither}"
    );

    let one = validate(&[FIRST]);
    assert!(
        !warns_about(&one, "deploy", "environment.PROFILE", FIRST),
        "setting the variable removes its warning:\n{one}"
    );
    assert!(
        warns_about(&one, "publish", "environment.TOKEN", SECOND),
        "{one}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// This binary's version as a project declares it: no pre-release suffix.
fn written_for_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
        .split_once('-')
        .map_or(env!("CARGO_PKG_VERSION"), |(release, _)| release)
}

/// decisions/0015: a project written for a newer Ratect is refused for that
/// reason — before the parser gets to the field this binary doesn't know,
/// which would otherwise be all the user is told.
#[test]
fn a_project_written_for_a_newer_ratect_is_refused_before_it_is_parsed() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        "ratect_version = \"99.0.0\"\nproject_name = \"demo\"\nfuture_field = true\n",
    )
    .unwrap();
    for args in [&["tasks", "list"][..], &["config", "validate"][..]] {
        let output = ratect_command()
            .arg("-f")
            .arg(dir.join("ratect.toml"))
            .args(args)
            .output()
            .expect("failed to run ratect");
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{args:?}:\n{all}");
        assert!(
            all.contains("needs ratect 99.0 or later"),
            "{args:?}:\n{all}"
        );
        assert!(!all.contains("future_field"), "{args:?}:\n{all}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A project written before ratect 0.12.0 — or that doesn't say, and so
/// counts as written for 0.3.0 — is refused until someone has reviewed the
/// breaking change since and declared `ratect_version` (decisions/0015). One
/// that has loads, with nothing said about it.
#[test]
fn config_validate_refuses_a_project_written_before_a_breaking_change() {
    let dir = unique_project_dir();
    let validate = |body: &str| {
        std::fs::write(dir.join("ratect.toml"), body).unwrap();
        ratect_command()
            .arg("-f")
            .arg(dir.join("ratect.toml"))
            .args(["config", "validate"])
            .output()
            .expect("failed to run ratect")
    };

    for (body, written) in [
        ("project_name = \"demo\"\n", "doesn't set 'ratect_version'"),
        (
            "ratect_version = \"0.11.0\"\nproject_name = \"demo\"\n",
            "was written for ratect 0.11.0",
        ),
    ] {
        let output = validate(body);
        // Reported as one of `config validate`'s problems, on stdout.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{stdout}");
        assert!(stdout.contains(written), "{stdout}");
        assert!(stdout.contains("ratect 0.12.0: a '\\'"), "{stdout}");
    }

    let marked = validate(&format!(
        "ratect_version = \"{}\"\nproject_name = \"demo\"\n",
        written_for_version()
    ));
    let stdout = String::from_utf8_lossy(&marked.stdout);
    assert!(
        marked.status.success(),
        "{}",
        String::from_utf8_lossy(&marked.stderr)
    );
    assert!(!stdout.contains("ratect_version"), "{stdout}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A YAML root follows Batect's rules, so it loads whatever the breaking
/// changes since — unless it includes a TOML file, whose meaning may have
/// changed and which nothing in a YAML project can say was reviewed.
#[test]
fn a_yaml_root_is_refused_only_when_it_includes_a_toml_file() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("extra.toml"),
        "[containers.app]\nimage = \"alpine:3.18\"\n",
    )
    .unwrap();
    let tasks_list = |root: &str| {
        std::fs::write(dir.join("batect.yml"), root).unwrap();
        ratect_command()
            .arg("-f")
            .arg(dir.join("batect.yml"))
            .args(["tasks", "list", "-o", "quiet"])
            .output()
            .expect("failed to run ratect")
    };

    let yaml_alone = tasks_list("project_name: demo\n");
    let with_toml = tasks_list("project_name: demo\ninclude:\n  - extra.toml\n");
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        yaml_alone.status.success(),
        "{}",
        String::from_utf8_lossy(&yaml_alone.stderr)
    );
    let stderr = String::from_utf8_lossy(&with_toml.stderr);
    assert!(!with_toml.status.success(), "{stderr}");
    assert!(stderr.contains("extra.toml"), "{stderr}");
    assert!(stderr.contains("ratect config convert"), "{stderr}");
}

/// A YAML root that loads has nothing a written-for version would protect —
/// its files mean what they mean to Batect — so `config validate` says
/// nothing about one.
#[test]
fn config_validate_says_nothing_about_a_yaml_roots_version() {
    let dir = unique_project_dir();
    std::fs::write(dir.join("batect.yml"), "project_name: demo\n").unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(!stdout.contains("ratect_version"), "{stdout}");
}

/// `config convert` declares the converting binary's version — the rules the
/// result was converted under.
#[test]
fn config_convert_declares_the_version_it_was_converted_with() {
    let dir = unique_project_dir();
    std::fs::write(dir.join("batect.yml"), "project_name: demo\n").unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "convert", "--stdout"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("ratect_version = \"{}\"", written_for_version())),
        "{stdout}"
    );
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

/// One `batect.yml` converted both ways — written to a file and `--stdout`.
struct Conversion {
    /// The converted file's path, as the heading names it.
    output: PathBuf,
    written_stdout: String,
    written_stderr: String,
    /// The document, which the test checks is the same as the written file.
    printed_stdout: String,
    printed_stderr: String,
}

/// Runs `config convert` on `batect_yml` both ways, with `RUST_LOG=off`, so
/// what it asks the reader to check can't be riding on the log (#578). Asserts
/// both runs succeed and that the two documents are the same, so nothing it
/// reports can leak into the converted output.
fn convert_both_ways(batect_yml: &str) -> Conversion {
    let dir = unique_project_dir();
    std::fs::write(dir.join("batect.yml"), batect_yml).unwrap();
    let convert = |extra: &[&str]| {
        let output = ratect_command()
            .env("RUST_LOG", "off")
            .arg("-f")
            .arg(dir.join("batect.yml"))
            .args(["config", "convert"])
            .args(extra)
            .output()
            .expect("failed to run ratect");
        assert!(
            output.status.success(),
            "convert {extra:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };

    let written = convert(&[]);
    let printed = convert(&["--stdout"]);
    let printed_stdout = String::from_utf8_lossy(&printed.stdout).into_owned();
    assert_eq!(
        printed_stdout,
        std::fs::read_to_string(dir.join("ratect.toml")).unwrap(),
        "--stdout and the written file should be the same document"
    );
    std::fs::remove_dir_all(&dir).ok();
    Conversion {
        output: dir.join("ratect.toml"),
        written_stdout: String::from_utf8_lossy(&written.stdout).into_owned(),
        written_stderr: String::from_utf8_lossy(&written.stderr).into_owned(),
        printed_stdout,
        printed_stderr: String::from_utf8_lossy(&printed.stderr).into_owned(),
    }
}

/// The items `conversion` asked the reader to check, after asserting it
/// reported the same ones, in the same order, in all three places (#578):
/// after the success message on stdout when writing the file; on stderr with
/// `--stdout`, so the document is untouched; and in the document's own header.
/// Writing the file puts nothing on stderr.
fn reported_checks(conversion: &Conversion) -> Vec<String> {
    let heading = format!("Check before using {}:", conversion.output.display());
    let listed = |output: &str| -> Vec<String> {
        let mut lines = output.lines().skip_while(|line| *line != heading);
        if lines.next().is_none() {
            return Vec::new();
        }
        lines
            .map(|line| {
                line.strip_prefix("  warning ")
                    .unwrap_or_else(|| panic!("not rendered as a warning: {line:?}\n{output}"))
                    .to_string()
            })
            .collect()
    };

    let written = listed(&conversion.written_stdout);
    assert!(
        conversion.written_stdout.starts_with("Converted "),
        "the list comes after the success message:\n{}",
        conversion.written_stdout
    );
    assert_eq!(conversion.written_stderr, "", "nothing on stderr");
    assert_eq!(
        listed(&conversion.printed_stderr),
        written,
        "--stdout's list"
    );
    assert!(
        !conversion.printed_stdout.contains("Check before using"),
        "--stdout's document holds no terminal list:\n{}",
        conversion.printed_stdout
    );

    let comments: Vec<&str> = conversion
        .printed_stdout
        .lines()
        .take_while(|line| line.starts_with('#'))
        .collect();
    let in_header: Vec<String> = match comments
        .iter()
        .position(|line| *line == "# Check before use:")
    {
        Some(at) => {
            assert_eq!(
                comments[at - 1],
                "#",
                "a blank comment line before the block"
            );
            comments[at + 1..]
                .iter()
                .map(|line| line.strip_prefix("# ").unwrap().to_string())
                .collect()
        }
        None => Vec::new(),
    };
    assert_eq!(in_header, written, "the header's block");
    written
}

/// Converted config is identical, but `ratect` runs a task's own container
/// with no readiness gate (decisions/0012), so a task container's
/// `setup_commands` would silently stop running (ratect#267). Reported
/// either way the output goes, naming the container and the field.
#[test]
fn config_convert_reports_a_task_containers_readiness_fields() {
    let checks = reported_checks(&convert_both_ways(
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    setup_commands:\n      - command: ./migrate.sh\ntasks:\n  t:\n    run:\n      container: app\n",
    ));
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert!(
        checks[0].contains("'app'")
            && checks[0].contains("'setup_commands'")
            && checks[0].contains("dependency-readiness.md#the-tasks-own-container"),
        "expected an item naming 'app' and 'setup_commands': {checks:?}"
    );
    assert!(
        !checks[0].contains("'health_check'"),
        "only the fields the container has should be named: {checks:?}"
    );
}

/// #462: `ratect.toml` evaluates a `home_directory` that `batect.yml` uses as
/// written, so a converted `$`/`<` changes meaning — reported either way the
/// conversion is written.
#[test]
fn config_convert_reports_a_home_directory_that_becomes_an_expression() {
    let checks = reported_checks(&convert_both_ways(
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/$USER\n",
    ));
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert!(
        checks[0].contains("'app'") && checks[0].contains("'/home/$USER'"),
        "expected an item naming 'app' and its home_directory: {checks:?}"
    );
}

/// #379: `ratect.toml` evaluates a compact `volumes` host path that
/// `batect.yml` uses as written, so a converted `$`/`<` changes meaning —
/// reported either way the conversion is written.
#[test]
fn config_convert_reports_a_compact_volume_that_becomes_an_expression() {
    let checks = reported_checks(&convert_both_ways(
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    volumes:\n      - $HOME/cache:/cache\n",
    ));
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert!(
        checks[0].contains("'app'") && checks[0].contains("'$HOME/cache'"),
        "expected an item naming 'app' and its volume: {checks:?}"
    );
}

/// #578: several items, one per line in the same order everywhere — by
/// container name, then kind.
#[test]
fn config_convert_reports_its_items_in_container_order() {
    let checks = reported_checks(&convert_both_ways(
        "project_name: demo\ncontainers:\n  b:\n    image: alpine:3.18\n    volumes:\n      - $HOME/b:/b\n  a:\n    image: alpine:3.18\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/$USER\n    volumes:\n      - $HOME/a:/a\n",
    ));
    assert_eq!(checks.len(), 3, "{checks:?}");
    assert!(checks[0].contains("'/home/$USER'"), "{checks:?}");
    assert!(checks[1].contains("'$HOME/a'"), "{checks:?}");
    assert!(checks[2].contains("'$HOME/b'"), "{checks:?}");
}

/// The same fields on a *dependency* still run under `ratect`, so there is
/// nothing to report — no heading, no list, and a header that is exactly the
/// generic one.
#[test]
fn config_convert_does_not_report_a_dependencys_readiness_fields() {
    let conversion = convert_both_ways(
        "project_name: demo\ncontainers:\n  db:\n    image: postgres:16\n    health_check:\n      command: pg_isready\n    setup_commands:\n      - command: ./seed.sh\n  app:\n    image: alpine:3.18\n    dependencies: [db]\ntasks:\n  t:\n    run:\n      container: app\n",
    );

    assert_eq!(reported_checks(&conversion), Vec::<String>::new());
    assert_eq!(conversion.printed_stderr, "");
    // The source as named from the converted file's own directory — the bare
    // name, whatever `-f` was given.
    let header = "# Generated by `ratect config convert` from batect.yml.\n\
         # Review before use: comments were not carried over.\n\nratect_version = ";
    assert!(
        conversion.printed_stdout.starts_with(header),
        "{}",
        conversion.printed_stdout
    );
}

/// An item quotes a value from the source, which can hold a line break; in
/// the header that would end the comment and put the rest of the value into
/// the file as TOML. Escaped there, so the item stays one comment line and
/// the converted file still loads as the configuration it was converted from.
#[test]
fn config_convert_keeps_a_line_break_in_an_item_out_of_the_converted_file() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    run_as_current_user:\n      enabled: true\n      home_directory: \"/home/$USER\\n[tasks.injected]\"\n",
    )
    .unwrap();
    let convert = ratect_command()
        .arg("-f")
        .arg(dir.join("batect.yml"))
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(convert.status.success(), "{convert:?}");
    let document = std::fs::read_to_string(dir.join("ratect.toml")).unwrap();
    assert!(
        document.contains("'/home/$USER\\n[tasks.injected]'"),
        "{document}"
    );

    let list = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["tasks", "list", "-o", "quiet"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();
    assert!(list.status.success(), "{list:?}\n{document}");
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("injected"),
        "{document}"
    );
}

/// A project with includes, for the per-file conversion tests (ratect#574):
/// the root includes `a/x.yml`, which includes `a/b/y.yml`, which the root
/// also includes directly. `y` declares every kind of relative host path,
/// and `a/b/ctx/Dockerfile` exists so that `config validate` on the
/// converted project passes only if `build_directory` still resolves under
/// `a/b/` — as `ratect-compat` resolves the original.
fn project_with_nested_includes() -> PathBuf {
    let dir = unique_project_dir();
    std::fs::create_dir_all(dir.join("a/b/ctx")).unwrap();
    std::fs::write(dir.join("a/b/ctx/Dockerfile"), "FROM alpine:3.18\n").unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ninclude:\n  - a/x.yml\n  - a/b/y.yml\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("a/x.yml"),
        "include:\n  - b/y.yml\ntasks:\n  t:\n    run:\n      container: y\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("a/b/y.yml"),
        "containers:\n  y:\n    build_directory: ctx\n    build_secrets:\n      s:\n        path: secret.txt\n    build_ssh:\n      - id: k\n        paths:\n          - key\n    volumes:\n      - local: .\n        container: /c\n",
    )
    .unwrap();
    dir
}

/// ratect#574: each included file converts to its own `.toml` beside it, at
/// every depth, with each `include` entry pointing at the converted file —
/// so the converted project resolves `a/b/y.yml`'s paths under `a/b/`, which
/// `config validate` proves for `build_directory`. The success message names
/// every converted source.
#[test]
fn config_convert_writes_one_toml_per_included_file() {
    let dir = project_with_nested_includes();
    let convert = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(
        convert.status.success(),
        "convert failed:\n{}",
        String::from_utf8_lossy(&convert.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&convert.stdout),
        "Converted batect.yml to ratect.toml.\n\
         Converted a/x.yml to a/x.toml.\n\
         Converted a/b/y.yml to a/b/y.toml.\n\
         Review them, then remove batect.yml, a/x.yml and a/b/y.yml.\n"
    );
    let root = std::fs::read_to_string(dir.join("ratect.toml")).unwrap();
    assert!(
        root.contains(
            "include = [\n    { path = \"a/x.toml\" },\n    { path = \"a/b/y.toml\" },\n]\n"
        ),
        "{root}"
    );
    let x = std::fs::read_to_string(dir.join("a/x.toml")).unwrap();
    assert!(
        x.starts_with("# Generated by `ratect config convert` from x.yml.\n")
            && x.contains("include = [\n    { path = \"b/y.toml\" },\n]\n"),
        "{x}"
    );
    assert!(dir.join("a/b/y.toml").is_file());

    let validate = ratect_command()
        .current_dir(&dir)
        .args(["config", "validate"])
        .output()
        .expect("failed to run ratect");
    assert!(
        validate.status.success(),
        "the converted project should validate, with y's build_directory under a/b/:\n{}\n{}",
        String::from_utf8_lossy(&validate.stdout),
        String::from_utf8_lossy(&validate.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--stdout` prints every document in order, root first, each under a
/// banner naming its target — and writes nothing. (A single-file project's
/// `--stdout` is the bare document: [`convert_both_ways`] checks it is the
/// written file byte for byte.)
#[test]
fn config_convert_to_stdout_prints_each_document_under_a_banner() {
    let dir = project_with_nested_includes();
    let output = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert", "--stdout"])
        .output()
        .expect("failed to run ratect");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let banners: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("# === "))
        .collect();
    assert_eq!(
        banners,
        vec![
            "# === ratect.toml ===",
            "# === a/x.toml ===",
            "# === a/b/y.toml ===",
        ]
    );
    assert!(
        stdout.contains(
            "\n\n# === a/x.toml ===\n# Generated by `ratect config convert` from x.yml.\n"
        ),
        "a blank line, then the banner, then the document:\n{stdout}"
    );
    assert!(!dir.join("ratect.toml").exists() && !dir.join("a/x.toml").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// Targets are written all or nothing: with any of them already present,
/// the error lists every one and nothing is written; `--force` writes them
/// all.
#[test]
fn config_convert_refuses_every_existing_target_and_writes_nothing() {
    let dir = project_with_nested_includes();
    std::fs::write(dir.join("a/x.toml"), "stale\n").unwrap();
    std::fs::write(dir.join("a/b/y.toml"), "stale\n").unwrap();
    let refused = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("a/x.toml and a/b/y.toml already exist — pass --force to overwrite them"),
        "{stderr}"
    );
    assert!(!dir.join("ratect.toml").exists(), "nothing is written");
    assert_eq!(
        std::fs::read_to_string(dir.join("a/x.toml")).unwrap(),
        "stale\n"
    );

    let forced = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert", "--force"])
        .output()
        .expect("failed to run ratect");
    assert!(forced.status.success(), "{forced:?}");
    assert!(dir.join("ratect.toml").is_file());
    assert!(std::fs::read_to_string(dir.join("a/x.toml"))
        .unwrap()
        .contains("include = ["));
    assert!(std::fs::read_to_string(dir.join("a/b/y.toml"))
        .unwrap()
        .contains("[containers.y]"));
    std::fs::remove_dir_all(&dir).ok();
}

/// Two sources that would convert to one target: an error naming both, and
/// nothing written — even with `--force`.
#[test]
fn config_convert_refuses_two_sources_with_one_target() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ninclude:\n  - c.yml\n  - c.yaml\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("c.yml"),
        "containers:\n  a:\n    image: alpine:3.18\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("c.yaml"),
        "containers:\n  b:\n    image: alpine:3.18\n",
    )
    .unwrap();
    let output = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert", "--force"])
        .output()
        .expect("failed to run ratect");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("c.yml") && stderr.contains("c.yaml") && stderr.contains("c.toml"),
        "{stderr}"
    );
    assert!(!dir.join("ratect.toml").exists() && !dir.join("c.toml").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// An include outside the root's own directory is named by its absolute
/// path in the success message — there's no spelling of it under the root to
/// use — while its converted file's header, read from beside it, names the
/// bare file.
#[test]
fn config_convert_names_an_include_outside_the_root_directory_absolutely() {
    let base = unique_project_dir();
    std::fs::create_dir_all(base.join("proj")).unwrap();
    std::fs::create_dir_all(base.join("shared")).unwrap();
    std::fs::write(
        base.join("proj/batect.yml"),
        "project_name: demo\ninclude:\n  - ../shared/common.yml\n",
    )
    .unwrap();
    std::fs::write(
        base.join("shared/common.yml"),
        "containers:\n  app:\n    image: alpine:3.18\n",
    )
    .unwrap();
    let convert = ratect_command()
        .current_dir(base.join("proj"))
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    assert!(convert.status.success(), "{convert:?}");
    // As the binary sees its working directory: symlinks resolved (macOS's
    // temp directory is one).
    let base = base.canonicalize().unwrap();
    let shared_toml = base.join("shared/common.toml");
    assert_eq!(
        String::from_utf8_lossy(&convert.stdout),
        format!(
            "Converted batect.yml to ratect.toml.\n\
             Converted {} to {}.\n\
             Review them, then remove batect.yml and {}.\n",
            base.join("shared/common.yml").display(),
            shared_toml.display(),
            base.join("shared/common.yml").display()
        )
    );
    let common = std::fs::read_to_string(&shared_toml).unwrap();
    assert!(
        common.starts_with("# Generated by `ratect config convert` from common.yml.\n"),
        "its header names the source from its own directory:\n{common}"
    );
    assert!(
        std::fs::read_to_string(base.join("proj/ratect.toml"))
            .unwrap()
            .contains("{ path = \"../shared/common.toml\" }"),
        "the entry keeps its own spelling"
    );
    std::fs::remove_dir_all(&base).ok();
}

/// A write that fails partway names the files already written — the only
/// guidance there is about what now exists. Provoked by making an include's
/// directory read-only; skipped where that doesn't stop a write (running as
/// root), which the test probes rather than assumes.
#[cfg(unix)]
#[test]
fn config_convert_names_what_it_wrote_when_a_later_write_fails() {
    use std::os::unix::fs::PermissionsExt;

    let dir = project_with_nested_includes();
    let read_only = dir.join("a/b");
    std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o555)).unwrap();
    let unwritable = std::fs::write(read_only.join("probe"), "").is_err();
    let convert = ratect_command()
        .current_dir(&dir)
        .args(["config", "convert"])
        .output()
        .expect("failed to run ratect");
    std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !unwritable {
        std::fs::remove_dir_all(&dir).ok();
        return;
    }
    assert!(!convert.status.success(), "{convert:?}");
    let stderr = String::from_utf8_lossy(&convert.stderr);
    assert!(
        stderr.contains("Already written: ratect.toml and a/x.toml")
            && stderr.contains("Failed to write a/b/y.toml"),
        "{stderr}"
    );
    assert!(dir.join("ratect.toml").is_file() && dir.join("a/x.toml").is_file());
    assert!(!dir.join("a/b/y.toml").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// The `convert/` fixture, so `docs/ratect-cli.md`'s captured `config convert
/// --stdout` comes from a checked-in project rather than an invented one:
/// its two items, in every place convert reports them.
#[test]
fn config_convert_reports_the_convert_fixtures_checks() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/convert/batect.yml");
    let checks = reported_checks(&convert_both_ways(
        &std::fs::read_to_string(fixture).unwrap(),
    ));
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert!(
        checks[0].contains("Task 'test' runs container 'app'")
            && checks[0].contains("'setup_commands' don't run"),
        "{checks:?}"
    );
    assert!(
        checks[1].contains("Container 'app' has a volume host path '$HOME/.cache'"),
        "{checks:?}"
    );
}

/// The other half of why `batect.yml`'s image-source validation is
/// deliberately not applied to the native format: `extends` is
/// `child.or(parent)` with no way to *unset* an inherited field, so setting
/// `image` on a child is the only way to override a parent's
/// `build_directory` — which necessarily leaves both set. `ratect-compat`
/// rejects that combination (Batect does, and has no `extends` to need it);
/// rejecting it here would forbid the reuse pattern the format exists for.
#[test]
fn a_native_container_may_override_an_inherited_build_directory_with_an_image() {
    let dir = unique_project_dir();
    // `config validate` also checks that a build directory exists, and the
    // inherited one is still present on the child — this test is about the
    // config loading, so give it something real to point at.
    std::fs::create_dir(dir.join("ctx")).unwrap();
    std::fs::write(dir.join("ctx/Dockerfile"), "FROM alpine:3.18\n").unwrap();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
ratect_version = "0.12.0"
project_name = "demo"

[containers.base]
build_directory = "ctx"

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
        "overriding an inherited build with an image should be valid:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A container used only as an `extends` base needs neither `image` nor
/// `build_directory` — ADR-0003's "no `abstract` marker needed", which holds
/// because that requirement is enforced lazily, only for containers a task
/// actually runs. Nothing pinned it, so an eager check added later would have
/// silently broken the format's headline reuse pattern.
///
/// Which is exactly what it then caught: 0.25.0's port of Batect's
/// `resolveImageSource` validation started out applying to both formats, and
/// this test is what forced it to `ratect-compat` only. Don't delete it as
/// hypothetical.
#[test]
fn a_base_only_container_needs_no_image_and_validates() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
ratect_version = "0.12.0"
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

/// `--help` exits `0` and a usage error exits `2` — `clap`'s conventions,
/// and both divergences from Batect (`1` and `255`) that
/// `decisions/0016-exit-codes.md` keeps. Pinned so a `clap` upgrade can't
/// move either unnoticed.
#[test]
fn help_exits_zero_and_a_usage_error_exits_two() {
    let help = ratect_command()
        .arg("--help")
        .output()
        .expect("failed to run ratect");
    assert_eq!(help.status.code(), Some(0));

    let usage_error = ratect_command()
        .arg("--bogus")
        .output()
        .expect("failed to run ratect");
    assert_eq!(usage_error.status.code(), Some(2));
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

/// The default-suite cache tests all pass `--scope project`.
///
/// A shared cache lives under the *real* `~/.ratect/caches` — `home_directory()`
/// reads the passwd entry, not `$HOME`, so a test cannot redirect it. Without
/// the flag these tests would read, and one would remove, a developer's own
/// shared caches: a machine with `~/.ratect/caches/npm-cache` saw this suite
/// delete it. Scoping them says what they are actually about, which is a
/// project's own storage.
fn caches_command(project: &Path) -> Command {
    let mut command = ratect_command();
    command.arg("-f").arg(project.join("batect.yml"));
    command
}

/// Requires a real Docker daemon. Run with `cargo test -- --ignored`.
///
/// The shared half of `caches`, which only volumes can exercise: a shared
/// cache's directory form lives under the real `~/.ratect/caches`, and
/// `home_directory()` reads the passwd entry rather than `$HOME`, so a test
/// cannot redirect it without writing into the developer's own home.
///
/// Covers the scope column, the `--scope` filter, and the case the column
/// exists for — one name in both scopes, which `clean` must refuse rather
/// than guess at, since removing the shared one discards storage other
/// projects are still using.
#[test]
#[ignore]
fn caches_reports_scope_and_refuses_an_ambiguous_name() {
    let _guard = serial_docker();

    let project = project_with_directory_caches(&[]);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("batect.yml"),
        "project_name: scope-demo\ncontainers: {}\ntasks: {}\n",
    )
    .unwrap();

    // Written rather than read back: the key file is created lazily, the
    // first time something resolves a cache, and this test creates its
    // volumes directly. Fixing the key is also what makes the project
    // volume's name predictable enough to assert on.
    let project_key = "scope-demo-key";
    std::fs::create_dir_all(project.join(".batect/caches")).unwrap();
    std::fs::write(project.join(".batect/caches/key"), "scope-demo-key\n").unwrap();

    let volumes = [
        format!("batect-cache-{project_key}-clash"),
        "ratect-shared-cache-clash".to_string(),
        "ratect-shared-cache-only-shared".to_string(),
    ];
    for volume in &volumes {
        std::process::Command::new("docker")
            .args(["volume", "create", volume])
            .output()
            .expect("failed to create a docker volume");
    }
    let cleanup = || {
        for volume in &volumes {
            let _ = std::process::Command::new("docker")
                .args(["volume", "rm", "-f", volume])
                .output();
        }
    };

    let run = |args: &[&str]| {
        caches_command(&project)
            .args(args)
            .output()
            .expect("failed to run ratect")
    };

    let listed = run(&["caches", "list"]);
    let listed_out = String::from_utf8_lossy(&listed.stdout).to_string();

    let filtered = run(&["caches", "list", "--scope", "shared"]);
    let filtered_out = String::from_utf8_lossy(&filtered.stdout).to_string();

    let ambiguous = run(&["caches", "clean", "clash"]);
    let ambiguous_err = String::from_utf8_lossy(&ambiguous.stderr).to_string();

    let disambiguated = run(&["caches", "clean", "clash", "--scope", "shared"]);
    let survived = std::process::Command::new("docker")
        .args([
            "volume",
            "inspect",
            &format!("batect-cache-{project_key}-clash"),
        ])
        .output()
        .expect("failed to inspect");
    // Asserted separately from the project volume surviving: checking only
    // that would stay green if `clean --scope shared` regressed to removing
    // nothing at all.
    let shared_gone = std::process::Command::new("docker")
        .args(["volume", "inspect", "ratect-shared-cache-clash"])
        .output()
        .expect("failed to inspect");

    cleanup();
    std::fs::remove_dir_all(&project).ok();

    // Grouped under headings that don't claim the shared ones belong to
    // this project — most of them belong to other projects on the machine.
    let (project_section, shared_section) = listed_out
        .split_once("Shared caches on this machine:")
        .unwrap_or_else(|| panic!("both groups should appear:\n{listed_out}"));
    assert!(
        project_section.contains("Caches for this project:") && project_section.contains("clash"),
        "the project's own cache belongs in the first group:\n{listed_out}"
    );
    assert!(
        shared_section.contains("clash") && shared_section.contains("only-shared"),
        "shared caches belong in the second:\n{listed_out}"
    );
    assert!(
        filtered_out.contains("only-shared") && !filtered_out.contains("Caches for this project:"),
        "--scope shared should list only shared caches:\n{filtered_out}"
    );
    assert!(
        !ambiguous.status.success() && ambiguous_err.contains("--scope"),
        "an ambiguous name should be refused, pointing at --scope:\n{ambiguous_err}"
    );
    assert!(
        disambiguated.status.success(),
        "--scope should resolve it:\n{}",
        String::from_utf8_lossy(&disambiguated.stderr)
    );
    assert!(
        survived.status.success(),
        "removing the shared cache must leave the project one alone"
    );
    assert!(
        !shared_gone.status.success(),
        "the shared cache named should actually have been removed"
    );
}

#[test]
fn caches_list_reports_this_projects_caches_by_name() {
    let project = project_with_directory_caches(&["npm-cache", "gradle-cache"]);

    let output = caches_command(&project)
        .args([
            "caches",
            "list",
            "--cache-type",
            "directory",
            "--scope",
            "project",
        ])
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
        .args([
            "caches",
            "clean",
            "npm-cache",
            "--cache-type",
            "directory",
            "--scope",
            "project",
        ])
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
        .args([
            "caches",
            "clean",
            "--cache-type",
            "directory",
            "--scope",
            "project",
        ])
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
        .args([
            "caches",
            "clean",
            "--cache-type",
            "directory",
            "--scope",
            "project",
        ])
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

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// This binary's stdout is the task container's own output and nothing else
/// (decisions/0017): progress — milestones and the summary line — is on
/// stderr, so `ratect run x > out.txt` captures the result alone.
/// `ratect-compat` keeps Batect's stdout for progress; its own suite pins
/// that.
#[test]
#[ignore]
fn run_keeps_stdout_for_the_tasks_own_output_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .args(["run", "build", "-o", "simple"])
        .output()
        .expect("failed to run ratect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "built\n",
        "stdout is exactly the task's own output"
    );
    assert!(
        stderr.contains("Running build...") && stderr.contains("build finished with exit code"),
        "progress is on stderr:\n{stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// Under `all`, the task container's own lines still go to stdout — prefixed
/// like every other line — while Ratect's own lines (and, with dependencies,
/// theirs) follow the progress console to stderr, so `2>/dev/null` leaves
/// the task's lines alone.
#[test]
#[ignore]
fn run_all_keeps_stdout_for_the_task_containers_lines_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(fixture_path())
        .args(["run", "build", "-o", "all"])
        .output()
        .expect("failed to run ratect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr:\n{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "build-env | built\n",
        "stdout is the task container's prefixed lines and nothing else"
    );
    assert!(
        stderr.contains("build     | Running build...")
            && stderr.contains("build     | build finished with exit code"),
        "Ratect's own prefixed lines are on stderr:\n{stderr}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A task's own non-zero exit is reported by the exit code — and, outside
/// `quiet`, by the summary line — never by an `Error:` line (ratect#454;
/// decisions/0017's report-once rule). A failure of Ratect's own still is,
/// in every style and under `RUST_LOG=off`.
#[test]
#[ignore]
fn a_tasks_own_exit_code_prints_no_error_line_but_ratects_own_failure_does_via_docker() {
    let _guard = serial_docker();
    let run = |task: &str, style: &str| {
        ratect_command()
            .arg("-f")
            .arg(exit_code_fixture_path())
            .args(["run", task, "-o", style])
            .env("RUST_LOG", "off")
            .output()
            .expect("failed to run ratect")
    };

    let quiet = run("fails", "quiet");
    assert_eq!(quiet.status.code(), Some(42));
    assert_eq!(String::from_utf8_lossy(&quiet.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&quiet.stderr),
        "",
        "a task's own exit is no error of Ratect's"
    );

    let simple = run("fails", "simple");
    assert_eq!(simple.status.code(), Some(42));
    let stderr = String::from_utf8_lossy(&simple.stderr);
    assert!(
        stderr.contains("fails finished with exit code 42") && !stderr.contains("Error:"),
        "the summary line reports the exit, nothing else does:\n{stderr}"
    );

    let broken = run("unpullable", "quiet");
    assert_eq!(broken.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&broken.stderr);
    assert!(
        stderr.starts_with("Error: ") && stderr.contains("Failed to pull image"),
        "Ratect's own failure is printed whatever RUST_LOG says:\n{stderr}"
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

/// `run_to_completion` (ratect#97), end to end: `app`'s `init` dependency
/// must run to completion (`command = "true"`, exit 0) before `app` itself
/// starts. Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn run_to_completion_dependency_runs_before_the_task_container_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(run_to_completion_fixture_path())
        .args(["run", "start"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-started"),
        "the task should only run once its run-to-completion dependency exited 0:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `setup_commands`' `run_in` (ratect#111), end to end: `db`'s setup command
/// execs into `db-seed-client` — one of `db`'s own dependencies — rather than
/// into `db`. It asserts on an environment variable the two containers
/// disagree about, so it exits non-zero (failing the run) if it lands in the
/// wrong container or is handed the declaring container's environment.
/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn a_setup_command_with_run_in_execs_into_its_dependency_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(setup_command_run_in_fixture_path())
        .args(["run", "start"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-started"),
        "the task should only run once the setup command succeeded inside \
         'db-seed-client':\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `external_health_check` (ratect#98), end to end, both kinds at once:
/// `app` depends on `api` (HTTP) and `socket` (TCP), neither of which could
/// possibly check itself — `traefik/whoami` is a `scratch` image with no
/// shell. Proves the acceptance criterion directly: the dependents start
/// only once the checks pass, and nothing ran inside the checked containers
/// to make that happen. Requires a running Docker daemon with network access
/// to pull `traefik/whoami:v1.10.2`, `alpine:3.18.2` and the companion's own
/// `curlimages/curl`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn external_health_checks_gate_a_dependent_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(external_health_check_fixture_path())
        .args(["run", "start"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-started"),
        "the task should only run once both external health checks passed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// The external check alone decides readiness (ratect#269):
/// `api-with-failing-image-healthcheck`'s image declares a `HEALTHCHECK`
/// that can never pass, and the task still runs once the external check
/// does, rather than failing on Docker's "unhealthy". Requires the same
/// images as the test above, and builds one on top of `traefik/whoami`. Run
/// explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn an_external_health_check_ignores_the_images_own_healthcheck_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(external_health_check_fixture_path())
        .args(["run", "start-despite-failing-image-healthcheck"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-started"),
        "the task should run once the external check passed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// The failure half of the fixture above: `unreachable`'s TCP check targets
/// a port nothing listens on, so it exhausts its retries and fails the run
/// before the dependent container ever starts. The error names the companion
/// container, whose name carries the checked container's own — the only
/// thread back to the configuration a user wrote, so it is asserted rather
/// than assumed.
#[test]
#[ignore]
fn a_failing_external_health_check_fails_the_run_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(external_health_check_fixture_path())
        .args(["run", "start-with-unreachable-dependency"])
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "an external health check that never passes should fail the run"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("should-not-run"),
        "the task must never run when its external health check fails:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unreachable"),
        "the error should name the checked container:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The failure half of the fixture above: `init-failing` (`command =
/// "false"`) exits non-zero, which must fail the run the same way an
/// unhealthy dependency does — before `app-with-failing-init` ever starts.
/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn a_failing_run_to_completion_dependency_fails_the_run_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(run_to_completion_fixture_path())
        .args(["run", "start-with-failing-init"])
        .output()
        .expect("failed to run ratect");

    assert!(
        !output.status.success(),
        "a non-zero exit from a run-to-completion dependency should fail the run"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("should-not-run"),
        "the task must never run when its run-to-completion dependency fails:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("init-failing"),
        "the error should name the failing dependency:\n{}",
        String::from_utf8_lossy(&output.stderr)
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

/// Project-scoped `resources` needs only `project_name` (ratect#563): a
/// project refused by the written-for check, or holding a value this machine
/// can't evaluate, is still one whose leftovers you can list. Proven without
/// a daemon — pointed at an unreachable one, the command gets past the
/// configuration and fails on Docker instead.
#[test]
fn project_scoped_resources_reads_nothing_but_the_project_name() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        "ratect_version = \"99.0.0\"\nproject_name = \"demo\"\n\n\
         [containers.deploy]\nimage = \"alpine:3.18\"\n\
         environment = { PROFILE = \"$UNSET_RATECT_563_VARIABLE\" }\n",
    )
    .unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["resources", "list", "--docker-host", "tcp://127.0.0.1:1"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "no daemon is listening:\n{stderr}"
    );
    assert!(!stderr.contains("ratect_version"), "{stderr}");
    assert!(!stderr.contains("UNSET_RATECT_563_VARIABLE"), "{stderr}");
}

/// Without a `project_name` to scope by, the error says where it looked and
/// what to use instead.
#[test]
fn project_scoped_resources_without_a_project_name_points_at_all_projects() {
    let dir = unique_project_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        "[containers.app]\nimage = \"alpine:3.18\"\n",
    )
    .unwrap();
    let output = ratect_command()
        .arg("-f")
        .arg(dir.join("ratect.toml"))
        .args(["resources", "list"])
        .output()
        .expect("failed to run ratect");
    std::fs::remove_dir_all(&dir).ok();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("'project_name'"), "{stderr}");
    assert!(stderr.contains("--all-projects"), "{stderr}");
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

/// Each warning `doctor` can raise from a configuration, plus a problem,
/// from one fixture (`doctor/`), so `docs/ratect-cli.md`'s captured `doctor` output
/// comes from a checked-in project rather than an invented one. Each finding
/// is asserted on its own line — one assertion per kind, not one for "doctor
/// found things" — and the Docker/leftover findings are left alone, since
/// whether a daemon is reachable is the machine's business, not the fixture's.
#[test]
fn doctor_reports_each_kind_of_config_finding_from_the_doctor_fixture() {
    let output = ratect_command()
        .arg("-f")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/doctor/ratect.toml"))
        .arg("doctor")
        .env_remove("DEPLOY_TOKEN")
        .output()
        .expect("failed to run ratect");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // One line carrying every fragment — two half-matching lines would
    // otherwise pass for one finding.
    let has_line = |kind: &str, fragments: &[&str]| {
        stdout.lines().any(|line| {
            line.trim_start().starts_with(kind)
                && fragments.iter().all(|fragment| line.contains(fragment))
        })
    };

    assert!(
        has_line("warning ", &["'batect' is a Batect wrapper script"]),
        "the wrapper stub beside the fixture should be flagged:\n{stdout}"
    );
    assert!(
        has_line(
            "warning ",
            &["container 'database' uses a floating image tag"]
        ),
        "the untagged image should be flagged:\n{stdout}"
    );
    assert!(
        has_line("warning ", &["dependency 'cache' has no health_check"]),
        "the unguarded dependency should be flagged:\n{stdout}"
    );
    assert!(
        has_line(
            "warning ",
            &["container 'app' gives 'cache' more than once in its 'dependencies'"]
        ),
        "the repeated dependency should be flagged:\n{stdout}"
    );
    assert!(
        has_line(
            "warning ",
            &[
                "Container 'deploy'",
                "'environment.TOKEN'",
                "('$DEPLOY_TOKEN')"
            ]
        ),
        "the value this machine can't evaluate should be flagged:\n{stdout}"
    );
    assert!(
        has_line(
            "problem ",
            &[
                "container 'app' has build_directory",
                "missing-dir', which doesn't exist"
            ]
        ),
        "the missing build_directory should be a problem:\n{stdout}"
    );
    assert!(
        !output.status.success(),
        "a problem must fail the command:\n{stdout}"
    );
}

/// Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
///
/// A shared cache made the way a user makes one — by running a task whose
/// `cache` mount says `scope = "shared"` — rather than by creating the
/// volume directly as `caches_reports_scope_and_refuses_an_ambiguous_name`
/// does. Proves the run creates `ratect-shared-cache-<name>`, that an
/// unqualified `clean` refuses it by name, and that `--scope shared` removes
/// it. Asserts on `docker volume inspect`, not the exit code, so a `clean`
/// that removed nothing can't pass. Its own fixture (`shared-cache.toml`)
/// because the shared volume is machine-wide: nothing else may share the
/// name, or this test's `clean` would take that project's storage with it.
#[test]
#[ignore]
fn a_run_creates_a_shared_cache_that_only_a_scoped_clean_removes() {
    let _guard = serial_docker();
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shared-cache.toml");
    let ratect = |arguments: &[&str]| {
        let mut command = ratect_command();
        command.arg("-f").arg(&config);
        command.args(arguments);
        command.output().expect("failed to run ratect")
    };
    let volume_exists = || {
        std::process::Command::new("docker")
            .args(["volume", "inspect", "ratect-shared-cache-shared-tools"])
            .output()
            .expect("failed to inspect")
            .status
            .success()
    };

    // Every observation is taken before any assertion, and the volume is
    // removed before the first one runs: a failure partway through would
    // otherwise leave the machine-wide volume behind, and the next run's
    // "the run created it" would pass without the run having done anything.
    let run = ratect(&["run", "warm-cache"]);
    let created = volume_exists();
    let listed = ratect(&["caches", "list", "--scope", "shared"]);
    let refused = ratect(&["caches", "clean", "shared-tools"]);
    let survived_refusal = volume_exists();
    let removed = ratect(&["caches", "clean", "shared-tools", "--scope", "shared"]);
    let gone = !volume_exists();
    let _ = std::process::Command::new("docker")
        .args(["volume", "rm", "-f", "ratect-shared-cache-shared-tools"])
        .output();

    assert!(
        run.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(created, "the run should have created the shared volume");
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listing.contains("- shared-tools"),
        "the shared cache should be listed under its own name:\n{listing}"
    );
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && refusal.contains("--scope shared"),
        "an unqualified clean should refuse and name the flag:\n{refusal}"
    );
    assert!(
        survived_refusal,
        "a refused clean must not have removed the volume"
    );
    assert!(
        removed.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(gone, "the scoped clean should have removed the volume");
}

/// `stop_signal` (ratect#112), end to end: `traps-usr1` traps only
/// `SIGUSR1` and ignores everything else (as PID 1, with no explicit
/// `SIGTERM` handler installed, the kernel gives it an ignore-disposition
/// for `SIGTERM` — the classic reason a plain shell script as PID 1 needs
/// `--init` or a trap to shut down promptly). Cleanup stops it as soon as
/// `app-with-custom-signal`'s own `true` command exits. If `stop_signal`
/// reached the container, the trap fires and it exits almost immediately;
/// if the default signal were sent instead (the bug this proves the fix
/// for), the container would ignore it and only stop once Docker's own
/// ~10s default timeout escalates to a forceful kill. Asserting well under
/// that default, rather than a tight bound, is what keeps this from being
/// a race against real daemon scheduling. Requires a running Docker daemon
/// with network access to pull `alpine:3.18.2`. Run explicitly with `cargo
/// test -- --ignored`.
#[test]
#[ignore]
fn stop_signal_reaches_the_configured_signal_via_docker() {
    let _guard = serial_docker();
    let start = Instant::now();
    let output = ratect_command()
        .arg("-f")
        .arg(stop_signal_fixture_path())
        .args(["run", "stop-signal"])
        .output()
        .expect("failed to run ratect");
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "cleanup should have stopped 'traps-usr1' promptly via its trapped stop_signal, \
         not waited out Docker's own ~10s default timeout for a signal it never traps: \
         took {elapsed:?}"
    );
}

/// `stop_grace_period` (ratect#112), end to end: `ignores-term` traps no
/// signal at all — same PID 1 kernel ignore-disposition as the test above
/// — so it can only be stopped by Docker's own forceful kill once the
/// grace period elapses. Its `stop_grace_period = "1s"` is far shorter
/// than Docker's own ~10s default, so a total run well under that default
/// proves the configured grace period was actually used rather than
/// ignored. Requires a running Docker daemon with network access to pull
/// `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn stop_grace_period_shortens_the_wait_before_a_forceful_kill_via_docker() {
    let _guard = serial_docker();
    let start = Instant::now();
    let output = ratect_command()
        .arg("-f")
        .arg(stop_signal_fixture_path())
        .args(["run", "stop-grace-period"])
        .output()
        .expect("failed to run ratect");
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "cleanup should have force-killed 'ignores-term' after its own 1s stop_grace_period, \
         not Docker's own ~10s default: took {elapsed:?}"
    );
}

/// `ulimits` (ratect#95), end to end: `object-form` asks for a `nofile`
/// soft/hard limit of 1024/2048 and checks both from inside itself, so the
/// run fails if the limits never reached Docker. Requires a running Docker
/// daemon with network access to pull `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
#[test]
#[ignore]
fn ulimits_in_the_object_form_reach_the_container_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(ulimits_fixture_path())
        .args(["run", "object-form"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("object-form-ok"),
        "the container should have seen its own configured nofile limits:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// A container's `ulimits` apply to that container alone: the
/// `limited-dependency` runs to completion and fails the run unless *it*
/// got its own 3000 `nofile` soft limit, while `unlimited-app`, which
/// declares none, fails unless its own limit is something else. Requires a
/// running Docker daemon with network access to pull `alpine:3.18.2`. Run
/// explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn each_container_gets_only_its_own_ulimits_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(ulimits_fixture_path())
        .args(["run", "per-container"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-unaffected"),
        "a dependency's ulimits must not reach the task's own container:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `dns`/`dns_search`/`dns_options` (ratect#105), end to end: the
/// `configured-dependency` runs to completion and fails the run unless its
/// own `/etc/resolv.conf` carries the nameserver, search domain and option
/// it asked for and it can still resolve its `peer` by name, while
/// `unconfigured-app`, which declares none, fails unless its own carries
/// none of them. Requires a running Docker daemon with network access to
/// pull `alpine:3.18.2`. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore]
fn each_container_gets_only_its_own_dns_settings_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(dns_fixture_path())
        .args(["run", "per-container"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-unaffected"),
        "a dependency's DNS settings must not reach the task's own container:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `network_mode` (ratect#106), end to end: `shares-peer`, `isolated` and
/// `on-host` each run to completion and fail the run unless their own
/// network is the `container:peer`, `none` or `host` one they asked for,
/// while `app`, which sets none, fails unless it still reaches `peer` by
/// name on the task's own network. Requires a running Docker daemon with
/// network access to pull `alpine:3.18.2`. Run explicitly with
/// `cargo test -- --ignored`.
#[test]
#[ignore]
fn each_container_gets_only_its_own_network_mode_via_docker() {
    let _guard = serial_docker();
    let output = ratect_command()
        .arg("-f")
        .arg(network_mode_fixture_path())
        .args(["run", "each-mode"])
        .output()
        .expect("failed to run ratect");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("app-unaffected"),
        "a dependency's network mode must not reach the task's own container:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// A stand-in daemon on a plain TCP port: answers `/version` with just
/// enough JSON for the API-version negotiation, and anything else with an
/// empty list. Serves until the returned handle's sender is dropped.
/// Returns the port.
fn spawn_fake_plain_daemon() -> (u16, std::sync::mpsc::Sender<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || loop {
        if let Err(std::sync::mpsc::TryRecvError::Disconnected) = stopped.try_recv() {
            return;
        }
        let Ok((mut stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        stream.set_nonblocking(false).unwrap();
        let mut request = [0u8; 2048];
        let read = stream.read(&mut request).unwrap_or(0);
        let body = if String::from_utf8_lossy(&request[..read]).contains("/version") {
            r#"{"ApiVersion":"1.44","Version":"0.0.0-fake"}"#
        } else {
            "[]"
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (port, stop)
}

/// `DOCKER_TLS_VERIFY=0` reaches the Docker client library, which used to
/// treat the variable being *set at all* as "use TLS" for a `tcp://` host
/// and then fail for want of a `ca.pem` (ratect#257). An in-process test
/// can't set the variable safely (every other test reads the environment
/// concurrently), so this drives the binary with it. `--all-projects` so no
/// config file is needed.
#[test]
fn a_tcp_host_stays_plain_when_docker_tls_verify_is_off() {
    let (port, _stop) = spawn_fake_plain_daemon();

    for off in ["0", "false", ""] {
        let output = ratect_command()
            .args(["resources", "list", "--all-projects"])
            .arg("--docker-host")
            .arg(format!("tcp://127.0.0.1:{port}"))
            .env("DOCKER_TLS_VERIFY", off)
            .env_remove("DOCKER_CERT_PATH")
            .output()
            .expect("failed to run ratect");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "DOCKER_TLS_VERIFY={off:?} should connect without TLS:\n{stderr}"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Nothing left over."),
            "{stderr}"
        );
    }
}

/// Only a run's failure exits `125`; every other verb's exits `1` — the same
/// unloadable config, two verbs (decisions/0016). Needs no daemon: the config
/// fails before anything connects.
#[test]
fn a_run_that_fails_exits_125_and_any_other_verb_1() {
    let directory = std::env::temp_dir().join(format!("ratect-exit-codes-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let config = directory.join("ratect.toml");
    std::fs::write(&config, "nonsense = [").unwrap();
    let exit_code = |args: &[&str]| {
        ratect_command()
            .args(args)
            .arg("-f")
            .arg(&config)
            .output()
            .expect("failed to run ratect")
            .status
            .code()
    };

    let run = exit_code(&["run", "build"]);
    let list = exit_code(&["tasks", "list"]);
    let validate = exit_code(&["config", "validate"]);
    std::fs::remove_dir_all(&directory).unwrap();

    assert_eq!(run, Some(125));
    assert_eq!(list, Some(1));
    assert_eq!(validate, Some(1));
}
