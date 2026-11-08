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

use super::*;
use crate::config::LoadedProject;
use crate::daemon_class::DaemonClass;
use crate::docker::LabelledResource;
use crate::resources::ResourceInventory;
use std::collections::HashMap;

/// A project's `Config` as written, for the checks that read only that.
async fn config_with(yaml: &str) -> Config {
    project_from("batect.yml", yaml).await.config
}

/// [`config_findings`] for a project, evaluated against `host_env` the way
/// `config validate` evaluates it against the real one.
fn findings_for(
    project: &LoadedProject,
    host_env: impl Fn(&str) -> Option<String>,
) -> Vec<Finding> {
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), host_env)
        .expect("no override is given");
    config_findings(&project.config, &evaluation)
}

fn no_host_env(_: &str) -> Option<String> {
    None
}

/// Loads a project the way a real invocation does — through
/// `load_project_native` on an actual file named `file_name` — rather than
/// by parsing it here, which would duplicate knowledge that belongs to
/// `config.rs`. A config needing a `ratect`-native field has to be a
/// `ratect.toml`: those may only be written in a native (TOML) file,
/// whichever project loads it (ratect#214).
async fn project_from(file_name: &str, text: &str) -> LoadedProject {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "ratect-doctor-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(file_name);
    std::fs::write(&path, text).unwrap();

    let project = crate::config::load_project_native(
        &path,
        running_0_12(),
        &crate::ui::LoadOutput::default(),
    )
    .await
    .expect("fixture config should load");
    std::fs::remove_dir_all(&directory).unwrap();
    project
}

/// Docker treats a missing tag as `latest`, so both are the same
/// reproducibility hazard — and a registry port is a colon that isn't
/// a tag, which is the case that makes this worth a function.
#[test]
fn a_floating_image_tag_is_latest_or_no_tag_at_all() {
    assert!(floating_image_tag("alpine"));
    assert!(floating_image_tag("alpine:latest"));
    assert!(floating_image_tag("registry.example.com/team/app"));
    assert!(floating_image_tag("registry.example.com:5000/team/app"));

    assert!(!floating_image_tag("alpine:3.18.2"));
    assert!(!floating_image_tag(
        "registry.example.com:5000/team/app:1.2.3"
    ));
    assert!(!floating_image_tag(
        "alpine@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    ));
}

#[tokio::test]
async fn doctor_warns_about_floating_tags_and_unguarded_dependencies() {
    let project = project_from(
        "batect.yml",
        r#"
project_name: demo
containers:
  database:
    image: postgres
  cache:
    image: redis:7-alpine
  app:
    image: alpine:3.18.2
    dependencies:
      - database
      - cache
tasks:
  test:
    run:
      container: app
      command: echo hi
"#,
    )
    .await;

    let findings = findings_for(&project, no_host_env);
    let messages: Vec<String> = findings
        .iter()
        .map(|finding| finding.render().trim().to_string())
        .collect();

    assert!(
        messages
            .iter()
            .any(|m| m.contains("'database'") && m.contains("floating image tag")),
        "an untagged image is a floating tag: {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("'cache'") && m.contains("floating")),
        "a pinned tag shouldn't be warned about: {messages:?}"
    );
    // Both dependencies lack a health check; the task's own container
    // isn't a dependency and so isn't gating anything.
    assert!(messages
        .iter()
        .any(|m| m.contains("'cache'") && m.contains("health_check")));
    assert!(messages
        .iter()
        .any(|m| m.contains("'database'") && m.contains("health_check")));
    assert!(
        !messages
            .iter()
            .any(|m| m.contains("'app'") && m.contains("health_check")),
        "the task's own container gates nothing: {messages:?}"
    );
    assert!(
        findings.iter().all(|f| !matches!(f, Finding::Problem(_))),
        "none of this stops a run: {messages:?}"
    );
}

/// The warning above is about a *health check* deciding readiness, so it is
/// false of the two dependencies whose readiness is decided some other way —
/// and worse than noise, since neither is allowed a `health_check` at all:
/// telling either to add one is telling it to write a config that will not
/// load.
#[tokio::test]
async fn doctor_does_not_ask_for_a_health_check_a_dependency_cannot_have() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "demo"

[containers.migrate]
image = "alpine:3.18.2"
run_to_completion = true

[containers.api]
image = "alpine:3.18.2"
external_health_check = { type = "tcp", port = 8080 }

[containers.app]
image = "alpine:3.18.2"
dependencies = ["migrate", "api"]

[tasks.test]
run = { container = "app", command = "echo hi" }
"#,
    )
    .await;

    let messages: Vec<String> = findings_for(&project, no_host_env)
        .iter()
        .map(|finding| finding.render().trim().to_string())
        .collect();

    for name in ["migrate", "api"] {
        assert!(
            !messages
                .iter()
                .any(|m| m.contains(&format!("'{name}'")) && m.contains("health_check")),
            "'{name}' decides readiness without a health check: {messages:?}"
        );
    }
    // The companion generated for `api`'s external check is a
    // `run_to_completion` dependency too, so it is covered by the same rule
    // — and it is the one the user could do nothing about even if told.
    assert!(
        !messages.iter().any(|m| m.contains("health_check")),
        "nothing in this config should be asked for a health check: {messages:?}"
    );
}

/// The marker Batect's authors put near the top of both wrapper forms
/// — the thing that tells a still-runs-Batect script from one already
/// repointed at Ratect.
#[test]
fn a_batect_wrapper_is_recognized_by_its_own_notice_line() {
    // The real Unix and Windows headers, trimmed to the marker line.
    assert!(is_batect_wrapper(
        "#!/usr/bin/env bash\n# This file is part of Batect.\n# Do not modify...\n"
    ));
    assert!(is_batect_wrapper(
        "@echo off\nrem This file is part of Batect.\nrem Do not modify...\n"
    ));

    // Anything that no longer runs Batect must not be flagged, however
    // it got that way: a hand-written shim that execs ratect-compat, or
    // a symlink to the ratect-compat binary (read as binary bytes).
    // Flagging one would mean flagging a finished migration.
    assert!(!is_batect_wrapper("#!/bin/sh\nexec ratect-compat \"$@\"\n"));
    assert!(!is_batect_wrapper("\u{7f}ELF\u{2}\u{1}\u{1}\u{0}"));
    assert!(!is_batect_wrapper(""));
}

/// The filesystem half: a leftover wrapper in the project directory is
/// a warning (it still works, and that's the trap), never a problem.
#[test]
fn a_leftover_wrapper_in_the_project_directory_is_warned_about() {
    let directory = std::env::temp_dir().join(format!(
        "ratect-wrapper-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("batect"),
        "#!/usr/bin/env bash\n# This file is part of Batect.\n",
    )
    .unwrap();
    // A same-named file that isn't the wrapper mustn't be flagged.
    std::fs::write(directory.join("batect.cmd"), "echo not really batect\n").unwrap();

    let findings = wrapper_script_findings(&directory);
    std::fs::remove_dir_all(&directory).unwrap();

    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(matches!(
        &findings[0],
        Finding::Warning(message) if message.contains("'batect'") && message.contains("still runs Batect")
    ));
}

#[test]
fn a_project_with_no_wrapper_scripts_is_not_warned() {
    let directory = std::env::temp_dir().join(format!(
        "ratect-nowrapper-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    assert!(wrapper_script_findings(&directory).is_empty());
    std::fs::remove_dir_all(&directory).unwrap();
}

/// A build directory that isn't there fails the run, so it's a problem
/// rather than a warning — and `doctor` exits non-zero on those, which
/// is what makes it usable as a CI step.
#[tokio::test]
async fn a_missing_build_directory_is_a_problem() {
    let project = project_from(
        "batect.yml",
        r#"
project_name: demo
containers:
  app:
    build_directory: /nonexistent/build/context
tasks:
  test:
    run:
      container: app
      command: echo hi
"#,
    )
    .await;

    let findings = findings_for(&project, no_host_env);
    assert!(
            findings.iter().any(|finding| matches!(
                finding,
                Finding::Problem(message) if message.contains("build_directory") && message.contains("doesn't exist")
            )),
            "{findings:?}"
        );
}

/// A native container may set both `image` and `build_directory`, and
/// `image` wins (`TaskEngine::resolve_image`): an inherited
/// `build_directory` is how a child ends up with both. Nothing is built, so
/// a build directory or Dockerfile that isn't there is no problem.
#[tokio::test]
async fn a_build_directory_beside_an_image_is_not_judged() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "demo"

[containers.base]
build_directory = "/nonexistent/build/context"

[containers.app]
extends = "base"
image = "alpine:3.18.2"
"#,
    )
    .await;

    let findings = findings_for(&project, no_host_env);
    assert!(
        !findings.iter().any(|finding| matches!(
            finding,
            Finding::Problem(message) if message.contains("container 'app'")
        )),
        "{findings:?}"
    );
    // The base, which has no image, is still judged.
    assert!(
        findings.iter().any(|finding| matches!(
            finding,
            Finding::Problem(message) if message.contains("container 'base'") && message.contains("doesn't exist")
        )),
        "{findings:?}"
    );
}

/// decisions/0014: a value this machine can't evaluate is a warning naming
/// the container, field and expression — never a problem — and the checks
/// that read evaluated values skip the container it's in rather than judge
/// it as written.
#[tokio::test]
async fn an_unevaluable_value_is_a_warning_and_its_container_is_not_judged() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "demo"

[containers.deploy]
build_directory = "/nonexistent/build/context"
environment = { PROFILE = "$AWS_PROFILE" }

[containers.tagged]
image = "alpine"
environment = { PROFILE = "$AWS_PROFILE" }

[containers.app]
image = "alpine:3.18.2"
"#,
    )
    .await;

    let findings = findings_for(&project, no_host_env);
    let warnings: Vec<&String> = findings
        .iter()
        .filter_map(|finding| match finding {
            Finding::Warning(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 2, "{findings:?}");
    for container in ["deploy", "tagged"] {
        assert!(
            warnings.iter().any(
                |warning| warning.contains(&format!("Container '{container}'"))
                    && warning.contains("'environment.PROFILE'")
                    && warning.contains("$AWS_PROFILE")
            ),
            "{findings:?}"
        );
    }
    assert!(
        findings.iter().all(|f| !matches!(f, Finding::Problem(_))),
        "an unevaluated container's build_directory isn't judged: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|f| matches!(f, Finding::Fine(m) if m.contains("every value evaluates"))),
        "{findings:?}"
    );

    // Set, it evaluates — and is judged like any other container.
    let findings = findings_for(&project, |_| Some("ci".to_string()));
    assert!(
        findings
            .iter()
            .any(|f| matches!(f, Finding::Fine(m) if m.contains("every value evaluates"))),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(
            |f| matches!(f, Finding::Warning(m) if m.contains("'tagged'") && m.contains("floating"))
        ),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(
            |f| matches!(f, Finding::Problem(m) if m.contains("'deploy'") && m.contains("doesn't exist"))
        ),
        "{findings:?}"
    );
}

/// A container named only by a *task*'s `dependencies` gates that task
/// just as much as a container-level one.
#[tokio::test]
async fn a_task_level_dependency_counts_as_a_dependency() {
    let config = config_with(
        r#"
project_name: demo
containers:
  queue:
    image: redis:7-alpine
  app:
    image: alpine:3.18.2
tasks:
  test:
    run:
      container: app
      command: echo hi
    dependencies:
      - queue
"#,
    )
    .await;

    assert_eq!(dependency_names(&config), vec!["queue"]);
}

/// ratect#393: Batect refuses a list naming one entry twice; Ratect loads it
/// and starts or runs the entry once, so the slip is reported here instead.
/// One warning per list and entry, naming the container or task, the field
/// and the entry — containers before tasks, each sorted by name.
#[tokio::test]
async fn an_entry_given_twice_in_dependencies_or_prerequisites_is_warned_about() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "demo"

[containers.db]
image = "postgres:16"
health_check = { command = "pg_isready" }

[containers.app]
image = "alpine:3.18.2"
dependencies = ["db", "db"]

[tasks.lint]
run = { container = "app", command = "echo lint" }

[tasks.test]
run = { container = "app", command = "echo test" }
dependencies = ["db", "db"]
prerequisites = ["lint", "lint"]
"#,
    )
    .await;

    assert_eq!(
        repeated_entries(&project.config),
        vec![
            "container 'app' gives 'db' more than once in its 'dependencies' — it starts only once"
                .to_string(),
            "task 'test' gives 'db' more than once in its 'dependencies' — it starts only once"
                .to_string(),
            "task 'test' gives 'lint' more than once in its 'prerequisites' — it runs only once"
                .to_string(),
        ]
    );
    let findings = findings_for(&project, no_host_env);
    for message in repeated_entries(&project.config) {
        assert!(
            findings.contains(&Finding::Warning(message.clone())),
            "'{message}' should be a doctor warning: {findings:?}"
        );
    }
    assert!(
        findings.iter().all(|f| !matches!(f, Finding::Problem(_))),
        "a repeated entry doesn't stop a run: {findings:?}"
    );
}

/// Batect compares the strings as written, before any wildcard is expanded:
/// the same pattern twice is a repeat, but a pattern and a name it matches
/// are not — that overlap is legal, and runs the match once.
#[tokio::test]
async fn a_repeated_prerequisite_is_compared_before_wildcard_expansion() {
    let tasks = r#"
project_name = "demo"

[containers.app]
image = "alpine:3.18.2"

[tasks.lint-a]
run = { container = "app", command = "echo a" }

[tasks.lint-b]
run = { container = "app", command = "echo b" }
"#;

    let repeated = project_from(
        "ratect.toml",
        &format!("{tasks}\n[tasks.all]\nprerequisites = [\"lint-*\", \"lint-*\"]\n"),
    )
    .await;
    assert_eq!(
        repeated_entries(&repeated.config),
        vec![
            "task 'all' gives 'lint-*' more than once in its 'prerequisites' — it runs only once"
                .to_string()
        ]
    );

    let overlapping = project_from(
        "ratect.toml",
        &format!("{tasks}\n[tasks.all]\nprerequisites = [\"lint-*\", \"lint-a\"]\n"),
    )
    .await;
    assert_eq!(repeated_entries(&overlapping.config), Vec::<String>::new());
}

/// Three of one entry is still one slip in one list: one warning.
#[tokio::test]
async fn an_entry_given_three_times_is_warned_about_once() {
    let project = project_from(
        "batect.yml",
        r#"
project_name: demo
containers:
  db:
    image: postgres:16
  app:
    image: alpine:3.18.2
    dependencies: [db, db, db]
"#,
    )
    .await;

    assert_eq!(repeated_entries(&project.config).len(), 1);
}

/// No list repeats anything — including the ones load extends itself: each
/// dependent of a container with an `external_health_check` gains its
/// companion, which must never read as something the user repeated.
#[tokio::test]
async fn lists_without_a_repeat_are_not_warned_about() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "demo"

[containers.api]
image = "alpine:3.18.2"
external_health_check = { type = "tcp", port = 8080 }

[containers.app]
image = "alpine:3.18.2"
dependencies = ["api"]

[tasks.lint]
run = { container = "app", command = "echo lint" }

[tasks.test]
run = { container = "app", command = "echo test" }
dependencies = ["api"]
prerequisites = ["lint"]
"#,
    )
    .await;

    assert!(
        project.config.containers["app"]
            .dependencies
            .as_ref()
            .is_some_and(|dependencies| dependencies.len() == 2),
        "load should have added the companion: {:?}",
        project.config.containers["app"].dependencies
    );
    assert_eq!(repeated_entries(&project.config), Vec::<String>::new());
}

/// Implements only what `leftover_finding` reaches through
/// `resources::find` — a fake scoped to this module, not shared with
/// `resources_tests.rs`'s: each test module's fake answers only what that
/// module calls, and both implement the same compiler-checked
/// `ResourceInventory` trait, so neither can silently drift from its real
/// shape. `stop_and_remove_container`/`remove_network` stay stubbed —
/// `leftover_finding` only ever lists, never removes — but the trait's
/// other two methods are what this fake exists to answer for real.
#[derive(Default)]
struct FakeRuntime {
    containers: Vec<LabelledResource>,
    networks: Vec<LabelledResource>,
}

#[async_trait::async_trait]
impl ResourceInventory for FakeRuntime {
    async fn list_containers(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> anyhow::Result<Vec<LabelledResource>> {
        Ok(self.containers.clone())
    }

    async fn list_networks(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> anyhow::Result<Vec<LabelledResource>> {
        Ok(self.networks.clone())
    }

    async fn stop_and_remove_container(
        &self,
        _container_id: &str,
        _stop_signal: Option<&str>,
        _stop_grace_period: Option<std::time::Duration>,
        _stopped: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> anyhow::Result<()> {
        unimplemented!("doctor never removes a container")
    }

    async fn remove_network(&self, _name: &str) -> anyhow::Result<()> {
        unimplemented!("doctor never removes a network")
    }
}

fn resource(id: &str, project: &str) -> LabelledResource {
    LabelledResource {
        id: id.to_string(),
        name: id.to_string(),
        labels: HashMap::from([(crate::labels::PROJECT.to_string(), project.to_string())]),
        created: Some(0),
        state: Some("running".to_string()),
    }
}

/// No connection at all — the connection finding already said so, so this
/// contributes nothing rather than repeating it.
#[tokio::test]
async fn no_docker_connection_means_no_leftover_finding() {
    let docker: Option<&FakeRuntime> = None;
    assert_eq!(leftover_finding(docker, "demo", 1_000).await, None);
}

/// `doctor` reports the leftover count through the exact same selection
/// `resources list` uses — the two must never be able to disagree about
/// what counts as a leftover. Cross-project exclusion itself is
/// `resources_tests.rs`'s to prove; this only proves the two callers agree
/// on whatever `resources::find` returns.
#[tokio::test]
async fn a_leftover_count_matches_what_resources_find_returns() {
    let docker = FakeRuntime {
        containers: vec![resource("c1", "demo"), resource("c2", "demo")],
        networks: vec![resource("n1", "demo")],
    };

    let expected = crate::resources::find(&docker, Some("demo"), None, 1_000)
        .await
        .unwrap();
    assert_eq!(expected.len(), 3);

    let finding = leftover_finding(Some(&docker), "demo", 1_000)
        .await
        .unwrap();
    assert!(matches!(
        &finding,
        Finding::Warning(message) if message.contains("3 resource(s)")
    ));
}

#[tokio::test]
async fn no_leftovers_is_a_fine_finding() {
    let docker = FakeRuntime::default();
    let finding = leftover_finding(Some(&docker), "demo", 1_000)
        .await
        .unwrap();
    assert_eq!(
        finding,
        Finding::Fine("no leftovers from previous runs".to_string())
    );
}

const OWN_CONTAINER_LINK: &str =
    "https://github.com/or1can/ratect/blob/main/docs/dependency-readiness.md#the-tasks-own-container";

/// One task, one field: the warning names the task, the container and only
/// the field it has — "isn't waited on", since Docker still runs a health
/// check; it is Ratect that stops watching it (ratect#267).
#[tokio::test]
async fn an_ungated_task_containers_health_check_is_warned_about() {
    let config = config_with(
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    health_check:\n      command: \"true\"\ntasks:\n  t:\n    run:\n      container: app\n",
    )
    .await;

    assert_eq!(
        ungated_task_container_checks(&config),
        vec![ConversionCheck {
            container: Some("app".to_string()),
            kind: ConversionCheckKind::UngatedTaskContainer,
            message: format!(
                "Task 't' runs container 'app' as its own, so under ratect the container's \
                 'health_check' isn't waited on — a task's own container has no readiness \
                 gate (see {OWN_CONTAINER_LINK})."
            ),
        }]
    );
}

/// Both fields, several tasks: tasks sorted and listed once per container.
#[tokio::test]
async fn an_ungated_task_container_shared_by_several_tasks_is_warned_about_once() {
    let config = config_with(
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    health_check:\n      command: \"true\"\n    setup_commands:\n      - command: ./migrate.sh\ntasks:\n  v:\n    run:\n      container: app\n  t:\n    run:\n      container: app\n  u:\n    run:\n      container: app\n",
    )
    .await;

    let messages: Vec<String> = ungated_task_container_checks(&config)
        .into_iter()
        .map(|check| check.message)
        .collect();
    assert_eq!(
        messages,
        vec![format!(
            "Tasks 't', 'u' and 'v' run container 'app' as their own, so under ratect the \
             container's 'health_check' isn't waited on and its 'setup_commands' don't run — \
             a task's own container has no readiness gate (see {OWN_CONTAINER_LINK})."
        )]
    );
}

/// Nothing to warn about: the fields on a dependency (still gated), an
/// empty `setup_commands`, and a task with no `run` of its own.
#[tokio::test]
async fn gated_or_empty_readiness_fields_are_not_warned_about() {
    let config = config_with(
        "project_name: demo\ncontainers:\n  db:\n    image: postgres:16\n    health_check:\n      command: pg_isready\n    setup_commands:\n      - command: ./seed.sh\n  app:\n    image: alpine:3.18\n    dependencies: [db]\n    setup_commands: []\ntasks:\n  t:\n    run:\n      container: app\n  all:\n    prerequisites: [t]\n",
    )
    .await;

    assert!(ungated_task_container_checks(&config).is_empty());
}

/// #462: a Batect-format `home_directory` is literal, but `ratect.toml`
/// evaluates the same text, so a `$`/`<` that `config convert` carries across
/// unchanged changes meaning. One warning per such container, naming the
/// value.
#[tokio::test]
async fn a_home_directory_that_would_become_an_expression_is_warned_about() {
    let config = config_with(
        "project_name: demo\ncontainers:\n  b:\n    image: alpine:3.18\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/$USER\n  a:\n    image: alpine:3.18\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/<who\n  plain:\n    image: alpine:3.18\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/me\n",
    )
    .await;
    let warnings = home_directory_expression_checks(&config);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].container.as_deref() == Some("a")
            && warnings[0].message.contains("'/home/<who'"),
        "{warnings:?}"
    );
    assert!(
        warnings[1].container.as_deref() == Some("b")
            && warnings[1].message.contains("'/home/$USER'"),
        "{warnings:?}"
    );
}

/// #379: a Batect-format compact `volumes` entry's host path is literal, but
/// `ratect.toml` evaluates the same text, so a `$`/`<` in one that `config
/// convert` carries across unchanged changes meaning. One warning per such entry, naming its
/// container and value — and none for the object form, whose `local` is an
/// expression in both formats.
#[tokio::test]
async fn a_compact_volume_that_would_become_an_expression_is_warned_about() {
    let config = config_with(
        "project_name: demo\ncontainers:\n  b:\n    image: alpine:3.18\n    volumes:\n      - $HOME/x:/x\n      - ./plain:/plain\n      - local: $HOME/y\n        container: /y\n  a:\n    image: alpine:3.18\n    volumes:\n      - <dir:/d:ro\n",
    )
    .await;
    let warnings = compact_volume_expression_checks(&config);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].container.as_deref() == Some("a") && warnings[0].message.contains("'<dir'"),
        "{warnings:?}"
    );
    assert!(
        warnings[1].container.as_deref() == Some("b") && warnings[1].message.contains("'$HOME/x'"),
        "{warnings:?}"
    );
}

/// #578: every kind together, ordered by container name, then by kind — a
/// container's own items stay together, whichever producer found them — and
/// a compact volume's items in list order. Each renders as `doctor` renders
/// a warning.
#[tokio::test]
async fn conversion_checks_are_ordered_by_container_then_kind() {
    let config = config_with(
        "project_name: demo\ncontainers:\n  b:\n    image: alpine:3.18\n    volumes:\n      - $HOME/2:/2\n      - $HOME/1:/1\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/$USER\n    setup_commands:\n      - command: ./migrate.sh\n  a:\n    image: alpine:3.18\n    volumes:\n      - $HOME/a:/a\ntasks:\n  t:\n    run:\n      container: b\n",
    )
    .await;

    let checks = conversion_checks(&config);
    let placed: Vec<(&str, ConversionCheckKind)> = checks
        .iter()
        .map(|check| (check.container.as_deref().unwrap(), check.kind))
        .collect();
    assert_eq!(
        placed,
        vec![
            ("a", ConversionCheckKind::CompactVolumeExpression),
            ("b", ConversionCheckKind::UngatedTaskContainer),
            ("b", ConversionCheckKind::RunAsCurrentUserBecomesIdentity),
            ("b", ConversionCheckKind::HomeDirectoryExpression),
            ("b", ConversionCheckKind::CompactVolumeExpression),
            ("b", ConversionCheckKind::CompactVolumeExpression),
        ]
    );
    assert!(checks[4].message.contains("'$HOME/2'"), "{checks:?}");
    assert!(checks[5].message.contains("'$HOME/1'"), "{checks:?}");
    assert_eq!(
        checks[0].finding(),
        Finding::Warning(checks[0].message.clone())
    );
}

/// #461: an invalid `DOCKER_TLS_VERIFY` or `DOCKER_BUILDKIT` is a problem
/// naming the variable and its value as written; a valid, empty or unset
/// one is nothing.
#[test]
fn docker_environment_findings_reports_each_invalid_boolean_variable() {
    let findings = docker_environment_findings(Some("Bogus"), Some("nope"));
    let [Finding::Problem(tls), Finding::Problem(buildkit)] = findings.as_slice() else {
        panic!("expected two problems: {findings:?}");
    };
    assert!(
        tls.contains("DOCKER_TLS_VERIFY") && tls.contains("'Bogus'"),
        "{tls}"
    );
    assert!(
        buildkit.contains("DOCKER_BUILDKIT") && buildkit.contains("'nope'"),
        "{buildkit}"
    );

    for value in [None, Some(""), Some("1"), Some("No")] {
        assert_eq!(
            docker_environment_findings(value, value),
            vec![],
            "{value:?}"
        );
    }
}

/// The `ratect` version a native load in these tests runs as.
fn running_0_12() -> crate::written_for::Version {
    "0.12.0".parse().unwrap()
}

/// ratect#296: the problems a run would refuse a build for are the ones
/// `check_build_files` gives, attributed to the container; an absolute
/// dockerfile that works is a warning naming the relative form instead.
#[tokio::test]
async fn build_files_are_judged_as_a_run_would_judge_them() {
    let directory =
        std::env::temp_dir().join(format!("ratect-296-doctor-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(directory.join("docker")).unwrap();
    std::fs::write(directory.join("docker/Dockerfile"), "FROM scratch").unwrap();
    let project = project_from(
        "ratect.toml",
        &format!(
            r#"
project_name = "demo"

[containers.absolute]
build_directory = "{dir}"
dockerfile = "{dir}/docker/Dockerfile"

[containers.missing]
build_directory = "{dir}"
dockerfile = "Missing.Dockerfile"
"#,
            dir = directory.display()
        ),
    )
    .await;

    let findings = findings_for(&project, no_host_env);
    std::fs::remove_dir_all(&directory).unwrap();

    assert!(
        findings.iter().any(|finding| matches!(
            finding,
            Finding::Warning(message) if message == "container 'absolute' has an absolute \
                dockerfile; written as 'docker/Dockerfile', relative to build_directory, it \
                would work from any checkout"
        )),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|finding| matches!(
            finding,
            Finding::Problem(message) if message == &format!(
                "container 'missing': dockerfile 'Missing.Dockerfile' doesn't exist in \
                 build_directory '{}'",
                directory.display()
            )
        )),
        "{findings:?}"
    );
}

/// decisions/0014: an `identity.home` expression that evaluates to a bad value
/// loads, and `config validate`/`doctor` warn rather than refuse.
#[tokio::test]
async fn a_bad_evaluated_identity_home_is_a_warning() {
    let project = project_from(
        "ratect.toml",
        "project_name = \"demo\"\n[config_variables.home]\ndefault = \"relative\"\n\
         [containers.app]\nimage = \"alpine:3.18\"\nidentity = { home = \"<home\" }\n",
    )
    .await;

    let findings = findings_for(&project, no_host_env);

    assert!(
        findings.iter().any(|finding| matches!(
            finding,
            Finding::Warning(message)
                if message.contains("'identity.home'")
                    && message.contains("is not an absolute path")
        )),
        "{findings:?}"
    );
}

/// `user` and `identity` containers, each on its own and never in a task.
async fn remapped_daemon_project() -> LoadedProject {
    project_from(
        "ratect.toml",
        r#"
ratect_version = "0.12.0"
project_name = "demo"

[containers.named]
image = "alpine:3.18.2"
user = "guest"

[containers.numeric]
image = "alpine:3.18.2"
user = 1000

[containers.root-by-id-text]
image = "alpine:3.18.2"
user = "0"

[containers.root-by-id]
image = "alpine:3.18.2"
user = 0

[containers.root-by-name]
image = "alpine:3.18.2"
user = "root"

[containers.home]
image = "alpine:3.18.2"
identity = { home = "/home/me" }

[containers.plain]
image = "alpine:3.18.2"
"#,
    )
    .await
}

fn remapped_warning_for(project: &LoadedProject, name: &str, class: DaemonClass) -> Option<String> {
    remapped_daemon_warning(name, &project.config.containers[name], class)
}

#[tokio::test]
async fn identity_is_a_hazard_only_on_userns_remap() {
    let project = remapped_daemon_project().await;
    let warning = remapped_warning_for(&project, "home", DaemonClass::UsernsRemap).unwrap();
    for expected in [
        "'home'",
        "'identity'",
        "userns-remap",
        "on the host",
        "home",
    ] {
        assert!(warning.contains(expected), "{expected}: {warning}");
    }
    for class in [
        DaemonClass::Rootless,
        DaemonClass::Desktop,
        DaemonClass::RootfulLinux,
    ] {
        assert_eq!(
            remapped_warning_for(&project, "home", class),
            None,
            "{class:?}"
        );
    }
}

#[tokio::test]
async fn a_non_root_user_is_a_hazard_on_both_remapped_daemons() {
    let project = remapped_daemon_project().await;
    for name in ["named", "numeric"] {
        let rootless = remapped_warning_for(&project, name, DaemonClass::Rootless).unwrap();
        for expected in [
            &format!("'{name}'"),
            "'user'",
            "rootless",
            "other users' access",
        ] {
            assert!(rootless.contains(expected), "{expected}: {rootless}");
        }
        let remap = remapped_warning_for(&project, name, DaemonClass::UsernsRemap).unwrap();
        for expected in [
            &format!("'{name}'"),
            "'user'",
            "userns-remap",
            "on the host",
        ] {
            assert!(remap.contains(expected), "{expected}: {remap}");
        }
        for class in [DaemonClass::Desktop, DaemonClass::RootfulLinux] {
            assert_eq!(
                remapped_warning_for(&project, name, class),
                None,
                "{class:?}"
            );
        }
    }
}

#[tokio::test]
async fn root_and_no_user_at_all_are_no_hazard_on_any_daemon() {
    let project = remapped_daemon_project().await;
    for name in ["root-by-id-text", "root-by-id", "root-by-name", "plain"] {
        for class in [
            DaemonClass::Rootless,
            DaemonClass::UsernsRemap,
            DaemonClass::Desktop,
            DaemonClass::RootfulLinux,
        ] {
            assert_eq!(
                remapped_warning_for(&project, name, class),
                None,
                "{name}: {class:?}"
            );
        }
    }
}

#[tokio::test]
async fn remapped_daemon_findings_are_warnings_by_container_name() {
    let project = remapped_daemon_project().await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let findings = remapped_daemon_findings(&evaluation, Some(DaemonClass::UsernsRemap));
    let [Finding::Warning(home), Finding::Warning(named), Finding::Warning(numeric)] =
        &findings[..]
    else {
        panic!("{findings:?}");
    };
    assert!(home.contains("'home'"), "{home}");
    assert!(named.contains("'named'"), "{named}");
    assert!(numeric.contains("'numeric'"), "{numeric}");
}

/// An unknown class and a failed connection each have their own finding, so
/// this adds nothing to them.
#[tokio::test]
async fn remapped_daemon_findings_need_a_known_class() {
    let project = remapped_daemon_project().await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    assert_eq!(remapped_daemon_findings(&evaluation, None), vec![]);
}

/// A value that can't be evaluated leaves its container out, as for the other
/// checks reading evaluated values.
#[tokio::test]
async fn remapped_daemon_findings_leave_out_a_container_that_did_not_evaluate() {
    let project = project_from(
        "ratect.toml",
        r#"
ratect_version = "0.12.0"
project_name = "demo"

[containers.unset]
image = "alpine:$UNSET_TAG"
user = "guest"
"#,
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    assert_eq!(
        remapped_daemon_findings(&evaluation, Some(DaemonClass::Rootless)),
        vec![]
    );
}

/// A base has no image and nothing to build, so nothing can run it, and the
/// child inheriting from it is the one reported.
#[tokio::test]
async fn remapped_daemon_findings_leave_out_a_base_nothing_can_run() {
    let project = project_from(
        "ratect.toml",
        r#"
ratect_version = "0.12.0"
project_name = "demo"

[containers.base-user]
user = "guest"

[containers.base-identity]
identity = { home = "/home/me" }

[containers.child]
extends = "base-user"
image = "alpine:3.18.2"

[containers.child-of-identity]
extends = "base-identity"
build_directory = "."
"#,
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let findings = remapped_daemon_findings(&evaluation, Some(DaemonClass::UsernsRemap));
    let [Finding::Warning(child), Finding::Warning(child_of_identity)] = &findings[..] else {
        panic!("{findings:?}");
    };
    assert!(child.contains("'child'"), "{child}");
    assert!(
        child_of_identity.contains("'child-of-identity'"),
        "{child_of_identity}"
    );
}

/// [`identity_home_mount_findings`] for a container `app` with `identity.home`
/// `home` and those `volumes`.
async fn home_mount_findings_for(home: &str, volumes: &str) -> Vec<Finding> {
    let project = project_from(
        "ratect.toml",
        &format!(
            r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
identity = {{ home = "{home}" }}
volumes = {volumes}
"#
        ),
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    identity_home_mount_findings(&evaluation)
}

#[tokio::test]
async fn a_tmpfs_above_the_identity_home_is_a_problem() {
    let findings = home_mount_findings_for(
        "/mnt/up/me",
        r#"[{ type = "tmpfs", container = "/mnt/up" }]"#,
    )
    .await;
    let [Finding::Problem(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    for expected in ["'app'", "'/mnt/up/me'", "tmpfs", "'/mnt/up'", "a run fails"] {
        assert!(message.contains(expected), "{expected}: {message}");
    }
    // A run refuses the home whether or not the image already has it.
    assert!(message.contains("whether or not it exists"), "{message}");
    assert!(!message.contains("runs with"), "{message}");
}

#[tokio::test]
async fn a_read_only_mount_above_the_identity_home_is_a_warning() {
    for (volumes, kind) in [
        (
            r#"[{ local = "up", container = "/mnt/up", options = "ro" }]"#,
            "bind mount",
        ),
        (
            r#"[{ type = "cache", name = "up", container = "/mnt/up", options = "ro" }]"#,
            "cache",
        ),
    ] {
        let findings = home_mount_findings_for("/mnt/up/me", volumes).await;
        let [Finding::Warning(message)] = &findings[..] else {
            panic!("{findings:?}");
        };
        for expected in [
            "'app'",
            "'/mnt/up/me'",
            "read-only",
            kind,
            "'/mnt/up'",
            "fails unless the home already exists there",
        ] {
            assert!(message.contains(expected), "{expected}: {message}");
        }
    }
}

#[tokio::test]
async fn a_writable_bind_mount_above_the_identity_home_is_a_warning() {
    let findings =
        home_mount_findings_for("/mnt/up/me", r#"[{ local = "up", container = "/mnt/up" }]"#).await;
    let [Finding::Warning(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    for expected in [
        "'app'",
        "'/mnt/up/me'",
        "writable bind mount",
        "'/mnt/up'",
        "created on the host",
    ] {
        assert!(message.contains(expected), "{expected}: {message}");
    }
}

#[tokio::test]
async fn a_writable_cache_above_the_identity_home_is_no_finding() {
    let findings = home_mount_findings_for(
        "/mnt/up/me",
        r#"[{ type = "cache", name = "up", container = "/mnt/up" }]"#,
    )
    .await;
    assert_eq!(findings, []);
}

#[tokio::test]
async fn a_mount_at_the_identity_home_settles_it_whatever_is_above() {
    let findings = home_mount_findings_for(
        "/mnt/up/me",
        r#"[
            { type = "tmpfs", container = "/mnt/up" },
            { local = "me", container = "/mnt/up/me/" },
        ]"#,
    )
    .await;
    assert_eq!(findings, []);
}

/// Docker resolves a `..` in a `tmpfs` key when it mounts, as a run does when judging
/// it (`identity::prepare_home`), and the finding names the key as written.
#[tokio::test]
async fn a_tmpfs_written_with_a_parent_component_is_judged_by_where_it_resolves() {
    let findings = home_mount_findings_for(
        "/mnt/me",
        r#"[{ type = "tmpfs", container = "/mnt/x/.." }]"#,
    )
    .await;
    assert!(
        matches!(&findings[..], [Finding::Problem(message)] if message.contains("tmpfs at '/mnt/x/..'")),
        "{findings:?}"
    );
    assert_eq!(
        home_mount_findings_for(
            "/mnt/me",
            r#"[{ type = "tmpfs", container = "/mnt/me/x/.." }]"#,
        )
        .await,
        []
    );
}

/// Docker refuses a bind at a `tmpfs`'s own path only as spelled the same, and mounts
/// the bind on top of one that merely resolves there, whichever is listed first.
#[tokio::test]
async fn a_mount_at_the_path_a_tmpfs_resolves_to_decides_over_it() {
    let tmpfs = r#"{ type = "tmpfs", container = "/mnt/x/.." }"#;
    let bind = r#"{ local = "up", container = "/mnt" }"#;
    for volumes in [format!("[{tmpfs}, {bind}]"), format!("[{bind}, {tmpfs}]")] {
        let findings = home_mount_findings_for("/mnt/me", &volumes).await;
        assert!(
            matches!(&findings[..], [Finding::Warning(message)] if message.contains("created on the host")),
            "{volumes}: {findings:?}"
        );
    }
}

/// A run creates the home in the deeper mount, which goes on top of the `tmpfs` at
/// start, so each finding here is what a run does (`identity::prepare_home`).
#[tokio::test]
async fn a_tmpfs_above_a_deeper_mount_above_the_identity_home_is_judged_by_that_mount() {
    assert_eq!(
        home_mount_findings_for(
            "/mnt/up/me",
            r#"[
                { type = "tmpfs", container = "/mnt" },
                { type = "cache", name = "up", container = "/mnt/up" },
            ]"#,
        )
        .await,
        []
    );
    for (volume, expected) in [
        (
            r#"{ local = "up", container = "/mnt/up" }"#,
            "created on the host",
        ),
        (
            r#"{ local = "up", container = "/mnt/up", options = "ro" }"#,
            "a run fails unless the home already exists",
        ),
        (
            r#"{ type = "cache", name = "up", container = "/mnt/up", options = "ro" }"#,
            "a run fails unless the home already exists",
        ),
    ] {
        let findings = home_mount_findings_for(
            "/mnt/up/me",
            &format!(r#"[{{ type = "tmpfs", container = "/mnt" }}, {volume}]"#),
        )
        .await;
        assert!(
            matches!(&findings[..], [Finding::Warning(message)] if message.contains(expected)),
            "{volume}: {findings:?}"
        );
    }
}

#[tokio::test]
async fn only_the_deepest_mount_above_the_identity_home_decides() {
    // Listed both ways round, so it can't be the list's order that decides.
    for volumes in [
        r#"[
            { local = "up", container = "/mnt", options = "ro" },
            { type = "cache", name = "up", container = "/mnt/up" },
        ]"#,
        r#"[
            { type = "cache", name = "up", container = "/mnt/up" },
            { local = "up", container = "/mnt", options = "ro" },
        ]"#,
    ] {
        assert_eq!(home_mount_findings_for("/mnt/up/me", volumes).await, []);
    }
    let findings = home_mount_findings_for(
        "/mnt/up/me",
        r#"[
            { type = "cache", name = "up", container = "/mnt" },
            { type = "tmpfs", container = "/mnt/up" },
        ]"#,
    )
    .await;
    assert!(
        matches!(&findings[..], [Finding::Problem(_)]),
        "{findings:?}"
    );
}

#[tokio::test]
async fn mount_paths_compare_cleaned_and_by_whole_component() {
    let cleaned = home_mount_findings_for(
        "/mnt/up/me",
        r#"[{ type = "tmpfs", container = "/mnt/up/" }]"#,
    )
    .await;
    assert!(matches!(&cleaned[..], [Finding::Problem(_)]), "{cleaned:?}");
    let not_a_parent = home_mount_findings_for(
        "/mnt/upper/me",
        r#"[{ type = "tmpfs", container = "/mnt/up" }]"#,
    )
    .await;
    assert_eq!(not_a_parent, []);
}

#[tokio::test]
async fn a_mount_below_or_beside_the_identity_home_is_no_finding() {
    let findings = home_mount_findings_for(
        "/mnt/up/me",
        r#"[
            { type = "tmpfs", container = "/mnt/up/me/.cache" },
            { type = "tmpfs", container = "/srv" },
        ]"#,
    )
    .await;
    assert_eq!(findings, []);
}

#[tokio::test]
async fn a_container_without_identity_or_that_nothing_can_run_has_no_home_finding() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "x"

[containers.plain]
image = "alpine:3.18.2"
volumes = [{ type = "tmpfs", container = "/mnt" }]

[containers.base]
identity = { home = "/mnt/me" }
volumes = [{ type = "tmpfs", container = "/mnt" }]
"#,
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    assert_eq!(identity_home_mount_findings(&evaluation), []);
}

#[tokio::test]
async fn a_home_set_by_an_expression_is_judged_on_its_value() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
identity = { home = "$WHERE/me" }
volumes = [{ type = "tmpfs", container = "/mnt/up" }]
"#,
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), |name| {
            (name == "WHERE").then(|| "/mnt/up".to_string())
        })
        .unwrap();
    let findings = identity_home_mount_findings(&evaluation);
    let [Finding::Problem(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    assert!(message.contains("'/mnt/up/me'"), "{message}");
}

/// Both verbs read it through `config_findings`.
#[tokio::test]
async fn config_findings_reports_a_mount_above_the_identity_home() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
identity = { home = "/mnt/me" }
volumes = [{ type = "tmpfs", container = "/mnt" }]
"#,
    )
    .await;
    let findings = findings_for(&project, no_host_env);
    assert!(
        findings.iter().any(
            |finding| matches!(finding, Finding::Problem(message) if message.contains("tmpfs"))
        ),
        "{findings:?}"
    );
}

const HOST: DirectoryOwner = DirectoryOwner {
    uid: 1000,
    gid: 1000,
};

/// [`root_written_mount_findings`] for a container `app`, written in `fields`, with
/// a daemon of `class`.
async fn root_written_findings_for(fields: &str, class: DaemonClass) -> Vec<Finding> {
    let project = project_from(
        "ratect.toml",
        &format!(
            r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
{fields}
"#
        ),
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    root_written_mount_findings(
        &evaluation,
        Some(class),
        Some(Transport::Network),
        Some(HOST),
    )
}

const WRITABLE_MOUNT: &str = r#"volumes = [{ local = "/srv/code", container = "/code" }]"#;

#[tokio::test]
async fn a_writable_mount_written_as_root_is_a_warning_on_rootful_linux() {
    for fields in [
        WRITABLE_MOUNT.to_string(),
        format!("user = 0\n{WRITABLE_MOUNT}"),
        format!("user = \"0\"\n{WRITABLE_MOUNT}"),
        format!("user = \"root\"\n{WRITABLE_MOUNT}"),
    ] {
        let findings = root_written_findings_for(&fields, DaemonClass::RootfulLinux).await;
        let [Finding::Warning(message)] = &findings[..] else {
            panic!("{fields}: {findings:?}");
        };
        for expected in [
            "'app'",
            "'/srv/code'",
            "'/code'",
            "may end up root-owned",
            "set identity",
            "run_as_current_user in a .yml file",
        ] {
            assert!(message.contains(expected), "{expected}: {message}");
        }
    }
}

#[tokio::test]
async fn each_writable_mount_written_as_root_gets_its_own_warning() {
    let findings = root_written_findings_for(
        r#"volumes = [
            { local = "/srv/code", container = "/code" },
            { local = "/srv/out", container = "/out", options = "rw,cached" },
        ]"#,
        DaemonClass::RootfulLinux,
    )
    .await;
    assert_eq!(findings.len(), 2, "{findings:?}");
}

#[tokio::test]
async fn a_mount_not_written_as_root_is_no_finding() {
    for fields in [
        format!("identity = {{ home = \"/home/me\" }}\n{WRITABLE_MOUNT}"),
        format!("user = \"guest\"\n{WRITABLE_MOUNT}"),
        format!("user = 1000\n{WRITABLE_MOUNT}"),
    ] {
        assert_eq!(
            root_written_findings_for(&fields, DaemonClass::RootfulLinux).await,
            [],
            "{fields}"
        );
    }
}

#[tokio::test]
async fn only_a_writable_host_mount_can_be_written_as_root() {
    for volumes in [
        r#"volumes = [{ local = "/srv/code", container = "/code", options = "ro" }]"#,
        r#"volumes = [{ local = "/srv/code", container = "/code", options = "z,ro" }]"#,
        r#"volumes = [{ type = "cache", name = "c", container = "/code" }]"#,
        r#"volumes = [{ type = "tmpfs", container = "/code" }]"#,
    ] {
        assert_eq!(
            root_written_findings_for(volumes, DaemonClass::RootfulLinux).await,
            [],
            "{volumes}"
        );
    }
}

#[tokio::test]
async fn root_written_mounts_are_only_a_hazard_on_rootful_linux() {
    for class in [
        DaemonClass::Desktop,
        DaemonClass::Rootless,
        DaemonClass::UsernsRemap,
    ] {
        assert_eq!(
            root_written_findings_for(WRITABLE_MOUNT, class).await,
            [],
            "{class:?}"
        );
    }
}

#[tokio::test]
async fn root_written_mounts_need_a_known_class_and_a_container_that_can_run() {
    let project = project_from(
        "ratect.toml",
        &format!(
            r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
{WRITABLE_MOUNT}

[containers.base]
{WRITABLE_MOUNT}
"#
        ),
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    assert_eq!(
        root_written_mount_findings(&evaluation, None, Some(Transport::Network), Some(HOST)),
        []
    );
    let findings = root_written_mount_findings(
        &evaluation,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::Network),
        Some(HOST),
    );
    let [Finding::Warning(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    assert!(message.contains("'app'"), "{message}");
}

/// Root-owned files are a root host user's own, and `identity` would adopt root.
#[tokio::test]
async fn a_root_host_user_has_no_root_written_mounts() {
    let project = project_from(
        "ratect.toml",
        &format!(
            r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
{WRITABLE_MOUNT}
"#
        ),
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let class = Some(DaemonClass::RootfulLinux);
    let root = DirectoryOwner { uid: 0, gid: 0 };
    assert_eq!(
        root_written_mount_findings(&evaluation, class, Some(Transport::Network), Some(root)),
        []
    );
    assert_eq!(
        root_written_mount_findings(&evaluation, class, Some(Transport::Network), None).len(),
        1,
        "a host with no uids still has the trap to report"
    );
}

/// A write can't change a socket's or a single file's owner, but only this
/// filesystem can say which a path is, and a missing path becomes a directory.
#[tokio::test]
async fn a_non_directory_is_left_out_only_over_a_local_socket() {
    let project = project_from(
        "ratect.toml",
        &format!(
            r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
{WRITABLE_MOUNT}
"#
        ),
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let class = Some(DaemonClass::RootfulLinux);
    let findings = |transport, is_non_directory: bool| {
        let read = std::cell::Cell::new(false);
        let findings =
            root_written_mount_findings_with(&evaluation, class, transport, Some(HOST), |path| {
                assert_eq!(path, Path::new("/srv/code"));
                read.set(true);
                is_non_directory
            });
        (findings.len(), read.get())
    };
    assert_eq!(findings(Some(Transport::LocalSocket), true), (0, true));
    assert_eq!(findings(Some(Transport::LocalSocket), false), (1, true));
    assert_eq!(findings(Some(Transport::Network), true), (1, false));
}

/// A `.yml` container can't set `identity`, and an enabled `run_as_current_user`
/// already runs it as the host user there.
#[tokio::test]
async fn run_as_current_user_is_not_written_as_root() {
    let project = project_from(
        "batect.yml",
        r#"
project_name: x
containers:
  mapped:
    image: alpine:3.18.2
    run_as_current_user:
      enabled: true
      home_directory: /home/me
    volumes:
      - /srv/code:/code
  disabled:
    image: alpine:3.18.2
    run_as_current_user:
      enabled: false
    volumes:
      - /srv/code:/code
"#,
    )
    .await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let findings = root_written_mount_findings(
        &evaluation,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::Network),
        Some(HOST),
    );
    let [Finding::Warning(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    assert!(message.contains("'disabled'"), "{message}");
}

/// [`mount_owner_findings_with`] for `project`, whose every path is owned by
/// `owner`, as host user [`HOST`], with the paths it probed.
fn owner_findings_for(
    project: &LoadedProject,
    class: Option<DaemonClass>,
    transport: Option<Transport>,
    owner: DirectoryOwner,
) -> (Vec<Finding>, Vec<String>) {
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let probed = std::cell::RefCell::new(Vec::new());
    let findings = mount_owner_findings_with(&evaluation, class, transport, Some(HOST), |path| {
        probed.borrow_mut().push(path.display().to_string());
        Some(owner)
    });
    (findings, probed.into_inner())
}

const OTHER: DirectoryOwner = DirectoryOwner { uid: 0, gid: 0 };

async fn identity_project() -> LoadedProject {
    project_from(
        "ratect.toml",
        r#"
project_name = "x"

[containers.app]
image = "alpine:3.18.2"
identity = { home = "/home/me" }
volumes = [{ local = "/srv/code", container = "/code" }]
"#,
    )
    .await
}

#[tokio::test]
async fn a_mount_owned_by_someone_else_is_a_warning_under_identity() {
    let project = identity_project().await;
    let (findings, _) = owner_findings_for(
        &project,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        OTHER,
    );
    let [Finding::Warning(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    for expected in [
        "'app'",
        "'/srv/code'",
        "'/code'",
        "uid 1000, gid 1000",
        "uid 0, gid 0",
        "may be refused",
    ] {
        assert!(message.contains(expected), "{expected}: {message}");
    }
}

/// Owner permission bits apply to a matching uid, whatever the group.
#[tokio::test]
async fn only_a_differing_uid_is_a_mismatch() {
    let project = identity_project().await;
    let findings_for_owner = |owner| {
        owner_findings_for(
            &project,
            Some(DaemonClass::RootfulLinux),
            Some(Transport::LocalSocket),
            owner,
        )
        .0
    };
    assert_eq!(
        findings_for_owner(DirectoryOwner { uid: 1000, gid: 50 }),
        []
    );
    assert_eq!(
        findings_for_owner(DirectoryOwner { uid: 50, gid: 1000 }).len(),
        1
    );
}

/// Root can write to what anyone owns, so nothing is probed.
#[tokio::test]
async fn a_root_host_user_has_no_mount_owner_findings() {
    let project = identity_project().await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let findings = mount_owner_findings_with(
        &evaluation,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        Some(DirectoryOwner { uid: 0, gid: 0 }),
        |_| panic!("root's mounts shouldn't be probed"),
    );
    assert_eq!(findings, []);
}

#[tokio::test]
async fn a_mount_the_host_user_owns_is_no_finding() {
    let project = identity_project().await;
    let (findings, probed) = owner_findings_for(
        &project,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        HOST,
    );
    assert_eq!(findings, []);
    assert_eq!(probed, ["/srv/code"]);
}

#[tokio::test]
async fn an_enabled_run_as_current_user_is_probed_too() {
    let project = project_from(
        "batect.yml",
        r#"
project_name: x
containers:
  mapped:
    image: alpine:3.18.2
    run_as_current_user:
      enabled: true
      home_directory: /home/me
    volumes:
      - /srv/code:/code
  disabled:
    image: alpine:3.18.2
    run_as_current_user:
      enabled: false
    volumes:
      - /srv/other:/other
"#,
    )
    .await;
    let (findings, probed) = owner_findings_for(
        &project,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        OTHER,
    );
    let [Finding::Warning(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    assert!(message.contains("'mapped'"), "{message}");
    assert_eq!(probed, ["/srv/code"]);
}

/// Nothing to compare, and the filesystem isn't touched.
#[tokio::test]
async fn nothing_is_probed_except_on_a_rootful_linux_daemon_over_a_local_socket() {
    let project = identity_project().await;
    for (class, transport) in [
        (Some(DaemonClass::Desktop), Some(Transport::LocalSocket)),
        (Some(DaemonClass::Rootless), Some(Transport::LocalSocket)),
        (Some(DaemonClass::UsernsRemap), Some(Transport::LocalSocket)),
        (Some(DaemonClass::RootfulLinux), Some(Transport::Network)),
        (None, None),
    ] {
        let (findings, probed) = owner_findings_for(&project, class, transport, OTHER);
        assert_eq!(findings, [], "{class:?} {transport:?}");
        assert!(probed.is_empty(), "{class:?} {transport:?}: {probed:?}");
    }
}

#[tokio::test]
async fn only_a_writable_host_mount_of_a_container_that_can_run_is_probed() {
    let project = project_from(
        "ratect.toml",
        r#"
project_name = "x"

[containers.plain]
image = "alpine:3.18.2"
volumes = [{ local = "/srv/plain", container = "/plain" }]

[containers.base]
identity = { home = "/home/me" }
volumes = [{ local = "/srv/base", container = "/base" }]

[containers.app]
image = "alpine:3.18.2"
identity = { home = "/home/me" }
volumes = [
    { local = "/srv/read-only", container = "/read-only", options = "ro" },
    { type = "cache", name = "c", container = "/cache" },
    { type = "tmpfs", container = "/tmpfs" },
]
"#,
    )
    .await;
    let (findings, probed) = owner_findings_for(
        &project,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        OTHER,
    );
    assert_eq!(findings, []);
    assert!(probed.is_empty(), "{probed:?}");
}

/// A run tries to create what is missing, as the host user, and one this process
/// can't read says nothing about who owns it.
#[tokio::test]
async fn a_path_that_cannot_be_probed_is_no_finding() {
    let project = identity_project().await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let findings = mount_owner_findings_with(
        &evaluation,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        Some(HOST),
        |_| None,
    );
    assert_eq!(findings, []);
}

#[tokio::test]
async fn with_no_host_user_to_compare_against_there_is_no_finding() {
    let project = identity_project().await;
    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    let findings = mount_owner_findings_with(
        &evaluation,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        None,
        |_| Some(OTHER),
    );
    assert_eq!(findings, []);
}

/// The case both host-user checks exist for: a `.yml` a native project includes,
/// whose container can't set `identity`.
#[tokio::test]
async fn an_included_run_as_current_user_runs_as_the_host_user() {
    let directory =
        std::env::temp_dir().join(format!("ratect-doctor-include-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("mapped.yml"),
        r#"
containers:
  mapped:
    image: alpine:3.18.2
    run_as_current_user:
      enabled: true
      home_directory: /home/me
    volumes:
      - /srv/code:/code
"#,
    )
    .unwrap();
    let root = directory.join("ratect.toml");
    std::fs::write(&root, "project_name = \"x\"\ninclude = [\"mapped.yml\"]\n").unwrap();
    let project = crate::config::load_project_native(
        &root,
        running_0_12(),
        &crate::ui::LoadOutput::default(),
    )
    .await
    .expect("fixture config should load");
    std::fs::remove_dir_all(&directory).unwrap();

    let (findings, probed) = owner_findings_for(
        &project,
        Some(DaemonClass::RootfulLinux),
        Some(Transport::LocalSocket),
        OTHER,
    );
    let [Finding::Warning(message)] = &findings[..] else {
        panic!("{findings:?}");
    };
    assert!(message.contains("'mapped'"), "{message}");
    assert_eq!(probed, ["/srv/code"]);

    let evaluation = project
        .evaluate_whole_project(&HashMap::new(), no_host_env)
        .unwrap();
    assert_eq!(
        root_written_mount_findings(
            &evaluation,
            Some(DaemonClass::RootfulLinux),
            Some(Transport::Network),
            Some(HOST)
        ),
        []
    );
}
