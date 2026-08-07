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
use crate::git_include::{FakeGitClient, GitIncludeCache, SystemGitClient};
use std::io::Cursor;

fn parse(yaml: &str) -> Config {
    noyalib::from_reader(Cursor::new(yaml.as_bytes())).expect("valid yaml")
}

/// Moved here from `ratect-compat`'s own `main.rs` when `base_path_for`
/// became shared (`ratect` needs the identical rule) — the behavior is
/// the same, only its home changed.
#[test]
fn base_path_for_a_bare_config_file_name_is_empty_not_dot() {
    // The default `-f batect.yml` case: `Path::parent()` on a bare
    // filename returns `Some("")`, not `None`, so the `.` fallback in
    // `base_path_for` never actually applies here — worth locking in
    // explicitly since it's easy to assume otherwise.
    assert_eq!(base_path_for(Path::new("batect.yml")), Path::new(""));
}

#[test]
fn base_path_for_a_dot_relative_config_file_is_dot() {
    assert_eq!(base_path_for(Path::new("./batect.yml")), Path::new("."));
}

#[test]
fn environment_values_accept_non_string_scalars() {
    // Batect coerces a YAML scalar to its string form; Ratect matches,
    // so `PORT: 8080` / `DEBUG: true` load rather than failing to parse
    // with a type mismatch. Surfaced by the task-with-unhealthy-dependency
    // conformance project (`NGINX_ENTRYPOINT_QUIET_LOGS: 1`).
    let config = parse(
        "project_name: p\n\
             containers:\n  \
               build-env:\n    \
                 image: alpine\n    \
                 environment:\n      \
                   PORT: 8080\n      \
                   RATIO: 1.5\n      \
                   DEBUG: true\n      \
                   NAME: already-a-string\n\
             tasks:\n  \
               the-task:\n    \
                 run:\n      \
                   container: build-env\n",
    );
    let env = config.containers["build-env"]
        .environment
        .as_ref()
        .expect("environment should be present");
    assert_eq!(env["PORT"], "8080");
    assert_eq!(env["RATIO"], "1.5");
    assert_eq!(env["DEBUG"], "true");
    assert_eq!(env["NAME"], "already-a-string");
}

#[test]
fn base_path_for_a_config_file_in_a_subdirectory_is_that_subdirectory() {
    assert_eq!(
        base_path_for(Path::new("project/batect.yml")),
        Path::new("project")
    );
}

#[test]
fn base_path_for_an_absolute_config_file_is_its_directory() {
    assert_eq!(
        base_path_for(Path::new("/abs/project/batect.yml")),
        Path::new("/abs/project")
    );
}

#[test]
fn parses_containers_and_tasks() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
tasks:
  test:
    run:
      container: build-env
      command: echo hi
    prerequisites:
      - other
"#,
    );

    assert_eq!(config.project_name, "demo");

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.image.as_deref(), Some("alpine:3.18"));
    assert_eq!(
        container.volumes.as_ref().unwrap(),
        &vec![VolumeMount::Local(LocalVolumeMount {
            local: "code".to_string(),
            container: "/code".to_string(),
            options: None,
        })]
    );

    let task = config.tasks.get("test").unwrap();
    assert_eq!(task.run.as_ref().unwrap().container, "build-env");
    assert_eq!(
        task.run.as_ref().unwrap().command.as_deref(),
        Some("echo hi")
    );
    assert_eq!(
        task.prerequisites.as_ref().unwrap(),
        &vec!["other".to_string()]
    );
}

#[test]
fn parses_a_task_with_only_prerequisites_and_no_run() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  other:
    run:
      container: build-env
  test:
    prerequisites:
      - other
"#,
    );

    let task = config.tasks.get("test").unwrap();
    assert!(task.run.is_none());
    assert_eq!(
        task.prerequisites.as_ref().unwrap(),
        &vec!["other".to_string()]
    );
}

#[test]
fn parses_task_description_and_group() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    description: Runs the test suite
    group: verification
    run:
      container: build-env
"#,
    );

    let task = config.tasks.get("test").unwrap();
    assert_eq!(task.description.as_deref(), Some("Runs the test suite"));
    assert_eq!(task.group.as_deref(), Some("verification"));
}

#[test]
fn task_description_and_group_default_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
"#,
    );

    let task = config.tasks.get("test").unwrap();
    assert_eq!(task.description, None);
    assert_eq!(task.group, None);
}

fn task_with_description_and_group(description: Option<&str>, group: Option<&str>) -> Task {
    Task {
        run: Some(TaskRun {
            container: "build-env".to_string(),
            command: None,
            environment: None,
            ports: None,
            working_directory: None,
            entrypoint: None,
        }),
        dependencies: None,
        prerequisites: None,
        description: description.map(str::to_string),
        group: group.map(str::to_string),
        customise: None,
    }
}

#[test]
fn format_task_list_is_a_flat_sorted_list_when_no_task_declares_a_group() {
    let tasks = HashMap::from([
        (
            "build".to_string(),
            task_with_description_and_group(Some("Builds the app"), None),
        ),
        (
            "test".to_string(),
            task_with_description_and_group(None, None),
        ),
    ]);

    assert_eq!(
        format_task_list("demo", &tasks),
        "Tasks in demo:\n- build: Builds the app\n- test"
    );
}

#[test]
fn format_task_list_groups_tasks_with_the_ungrouped_bucket_sorted_last() {
    let tasks = HashMap::from([
        (
            "lint".to_string(),
            task_with_description_and_group(None, Some("verification")),
        ),
        (
            "test".to_string(),
            task_with_description_and_group(Some("Runs the test suite"), Some("verification")),
        ),
        (
            "build".to_string(),
            task_with_description_and_group(None, Some("compilation")),
        ),
        (
            "clean".to_string(),
            task_with_description_and_group(None, None),
        ),
    ]);

    assert_eq!(
        format_task_list("demo", &tasks),
        "Tasks in demo:\n\
             \n\
             compilation:\n\
             - build\n\
             \n\
             verification:\n\
             - lint\n\
             - test: Runs the test suite\n\
             \n\
             Ungrouped tasks:\n\
             - clean"
    );
}

#[test]
fn format_task_list_quiet_is_sorted_tab_separated_and_ignores_groups() {
    let tasks = HashMap::from([
        (
            "test".to_string(),
            task_with_description_and_group(Some("Runs the test suite"), Some("verification")),
        ),
        (
            "build".to_string(),
            task_with_description_and_group(None, Some("compilation")),
        ),
        (
            "clean".to_string(),
            // A whitespace-only description gets no tab either,
            // matching Batect's `isNotBlank` check.
            task_with_description_and_group(Some("   "), None),
        ),
    ]);

    assert_eq!(
        format_task_list_quiet(&tasks),
        "build\nclean\ntest\tRuns the test suite"
    );
}

#[test]
fn resolve_expressions_errors_when_a_task_has_neither_run_nor_prerequisites() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test: {}
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Task 'test' must have at least one of 'run' or 'prerequisites'"));
}

#[test]
fn resolve_expressions_errors_when_a_task_has_empty_prerequisites_and_no_run() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    prerequisites: []
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Task 'test' must have at least one of 'run' or 'prerequisites'"));
}

#[test]
fn parses_task_level_dependencies() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - queue
"#,
    );

    let task = config.tasks.get("test").unwrap();
    assert_eq!(
        task.dependencies.as_ref().unwrap(),
        &vec!["queue".to_string()]
    );
}

#[test]
fn resolve_expressions_errors_when_a_task_has_dependencies_but_no_run() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
  other:
    image: alpine:3.18
tasks:
  other:
    run:
      container: other
  test:
    prerequisites:
      - other
    dependencies:
      - queue
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("'run' is required if 'dependencies' is provided"));
}

#[test]
fn resolve_expressions_errors_when_a_task_dependency_names_its_own_main_container() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - build-env
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    let message = result.unwrap_err().to_string();
    assert!(message.contains("Task 'test'"), "message: {message}");
    assert!(
        message.contains("both the main task container (via 'run') and a task-level dependency"),
        "message: {message}"
    );
}

#[test]
fn parses_task_customise() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - queue
    customise:
      queue:
        environment:
          FOO: bar
        ports:
          - 6543:6543
        working_directory: /custom
"#,
    );

    let task = config.tasks.get("test").unwrap();
    let customisation = task.customise.as_ref().unwrap().get("queue").unwrap();
    assert_eq!(
        customisation.environment.as_ref().unwrap().get("FOO"),
        Some(&"bar".to_string())
    );
    assert_eq!(customisation.ports.as_ref().unwrap().len(), 1);
    assert_eq!(customisation.working_directory.as_deref(), Some("/custom"));
}

#[test]
fn resolve_expressions_errors_when_customise_targets_the_main_task_container() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
    customise:
      build-env:
        working_directory: /custom
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Cannot apply customisations to main task container 'build-env'"));
}

#[test]
fn resolve_expressions_errors_when_customise_targets_a_container_outside_the_tasks_graph() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  unrelated:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
    customise:
      unrelated:
        working_directory: /custom
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result.unwrap_err().to_string().contains(
        "Task 'test' has customisations for container 'unrelated', but the container \
             'unrelated' will not be started as part of the task"
    ));
}

#[test]
fn resolve_expressions_allows_customise_for_a_task_level_dependency() {
    let mut config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
  queue:
    image: redis:7-alpine
tasks:
  test:
    run:
      container: build-env
    dependencies:
      - queue
    customise:
      queue:
        working_directory: /custom
"#,
    );

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result.is_ok(), "{:?}", result.unwrap_err());
}

#[test]
fn parses_build_directory_and_build_args() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_args:
      VERSION: "1.2.3"
tasks: {}
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.build_directory.as_deref(), Some("./docker"));
    assert_eq!(container.build_args.as_ref().unwrap()["VERSION"], "1.2.3");
}

#[test]
fn parses_dockerfile_and_build_target() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    dockerfile: docker/Dockerfile.prod
    build_target: builder
tasks: {}
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.dockerfile.as_deref(),
        Some("docker/Dockerfile.prod")
    );
    assert_eq!(container.build_target.as_deref(), Some("builder"));
}

#[test]
fn dockerfile_and_build_target_default_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
tasks: {}
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.dockerfile, None);
    assert_eq!(container.build_target, None);
}

#[test]
fn parses_container_and_run_working_directory() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    working_directory: /app
tasks:
  test:
    run:
      container: build-env
      command: echo hi
      working_directory: /app/subdir
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.working_directory.as_deref(), Some("/app"));
    let task = config.tasks.get("test").unwrap();
    assert_eq!(
        task.run.as_ref().unwrap().working_directory.as_deref(),
        Some("/app/subdir")
    );
}

#[test]
fn working_directory_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.working_directory, None);
    let task = config.tasks.get("test").unwrap();
    assert_eq!(task.run.as_ref().unwrap().working_directory, None);
}

#[test]
fn yaml_anchors_aliases_and_merge_keys_are_resolved() {
    // Not a Ratect-specific feature to implement — anchors (`&name`),
    // aliases (`*name`), and merge keys (`<<:`) are core YAML syntax, so
    // any spec-compliant parser (including `noyalib`) resolves them
    // before Ratect's own `Deserialize` impls ever see the document.
    // Locked in here as a regression test rather than left as an
    // untested assumption, since a future parser swap could plausibly
    // regress it silently.
    let config = parse(
        r#"
project_name: demo
containers:
  build-env: &base
    image: alpine:3.18
    environment:
      SHARED_VAR: shared-value
  other-env:
    <<: *base
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let base = config.containers.get("build-env").unwrap();
    let merged = config.containers.get("other-env").unwrap();
    assert_eq!(merged.image, base.image);
    assert_eq!(merged.environment, base.environment);
    assert_eq!(
        merged.environment.as_ref().unwrap().get("SHARED_VAR"),
        Some(&"shared-value".to_string())
    );
}

/// Batect's *extensions*: a top-level key starting with `.` exists only to
/// hold an anchor and is ignored, so a config that factors shared values
/// out that way loads rather than being rejected as an unknown field. Goes
/// through `parse_yaml_config_file` (not the `parse` helper) because
/// stripping happens there. Real-world shape, from a Batect bundle: the
/// extension is aliased into a *second* extension, which is then merged
/// into a container — so it also proves the anchors are resolved before the
/// extension keys are dropped.
#[test]
fn top_level_extension_keys_are_ignored() {
    let dir = unique_temp_dir();
    let path = dir.join("batect-bundle.yml");
    std::fs::write(
        &path,
        r#"
.aws-environment: &aws-environment
  AWS_REGION: eu-west-2

.terraform-environment: &terraform-environment
  <<: *aws-environment
  COMPONENT: infra

project_name: demo
containers:
  terraform:
    image: alpine:3.18
    environment:
      <<: *terraform-environment
      EXTRA: yes
tasks: {}
"#,
    )
    .unwrap();

    let file = parse_yaml_config_file(&path).expect("extensions should be ignored, not rejected");
    let environment = file.containers["terraform"].environment.as_ref().unwrap();
    assert_eq!(environment.get("AWS_REGION").unwrap(), "eu-west-2");
    assert_eq!(environment.get("COMPONENT").unwrap(), "infra");
    assert_eq!(environment.get("EXTRA").unwrap(), "yes");

    std::fs::remove_dir_all(&dir).ok();
}

/// Only *top-level* keys are extensions, matching kaml's own rule — a
/// `.`-prefixed key anywhere else is still an unknown field, so a typo
/// nested inside a container isn't silently swallowed.
#[test]
fn a_dot_prefixed_key_below_the_top_level_is_still_rejected() {
    let dir = unique_temp_dir();
    let path = dir.join("batect.yml");
    std::fs::write(
        &path,
        "project_name: demo\ncontainers:\n  app:\n    image: alpine\n    .oops: x\ntasks: {}\n",
    )
    .unwrap();
    assert!(parse_yaml_config_file(&path).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn parses_container_and_run_command() {
    let config = parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:16
    command: postgres -c max_connections=200
tasks:
  test:
    run:
      container: database
      command: echo hi
"#,
    );

    let container = config.containers.get("database").unwrap();
    assert_eq!(
        container.command.as_deref(),
        Some("postgres -c max_connections=200")
    );
    let task = config.tasks.get("test").unwrap();
    assert_eq!(
        task.run.as_ref().unwrap().command.as_deref(),
        Some("echo hi")
    );
}

#[test]
fn container_command_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:16
tasks:
  test:
    run:
      container: database
"#,
    );

    let container = config.containers.get("database").unwrap();
    assert_eq!(container.command, None);
}

#[test]
fn parses_container_and_run_entrypoint() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    entrypoint: /bin/sh -c
tasks:
  test:
    run:
      container: build-env
      command: echo hi
      entrypoint: /bin/bash -c
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.entrypoint.as_deref(), Some("/bin/sh -c"));
    let task = config.tasks.get("test").unwrap();
    assert_eq!(
        task.run.as_ref().unwrap().entrypoint.as_deref(),
        Some("/bin/bash -c")
    );
}

#[test]
fn entrypoint_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.entrypoint, None);
    let task = config.tasks.get("test").unwrap();
    assert_eq!(task.run.as_ref().unwrap().entrypoint, None);
}

#[test]
fn parses_labels() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    labels:
      com.example.owner: platform-team
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.labels,
        Some(HashMap::from([(
            "com.example.owner".to_string(),
            "platform-team".to_string()
        )]))
    );
}

#[test]
fn labels_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.labels, None);
}

#[test]
fn parses_capabilities_to_add_and_drop() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    capabilities_to_add:
      - NET_ADMIN
      - SYS_PTRACE
    capabilities_to_drop:
      - CHOWN
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.capabilities_to_add,
        Some(HashSet::from([Capability::NetAdmin, Capability::SysPtrace]))
    );
    assert_eq!(
        container.capabilities_to_drop,
        Some(HashSet::from([Capability::Chown]))
    );
}

#[test]
fn parses_capabilities_missing_from_batects_own_stale_list() {
    // BPF/CHECKPOINT_RESTORE/PERFMON, added to Docker in 20.10 — after
    // Batect's own Capability enum was last updated. See the doc
    // comment on `Capability` for why this is a deliberate superset,
    // not a strict Batect port.
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    capabilities_to_add:
      - BPF
      - CHECKPOINT_RESTORE
      - PERFMON
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.capabilities_to_add,
        Some(HashSet::from([
            Capability::Bpf,
            Capability::CheckpointRestore,
            Capability::Perfmon,
        ]))
    );
}

#[test]
fn capabilities_default_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.capabilities_to_add, None);
    assert_eq!(container.capabilities_to_drop, None);
}

#[test]
fn parses_privileged() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    privileged: true
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.privileged, Some(true));
}

#[test]
fn privileged_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.privileged, None);
}

#[test]
fn parse_byte_size_handles_batects_own_format() {
    assert_eq!(parse_byte_size("0"), Ok(0));
    assert_eq!(parse_byte_size("128"), Ok(128));
    assert_eq!(parse_byte_size("128b"), Ok(128));
    assert_eq!(parse_byte_size("128B"), Ok(128));
    assert_eq!(parse_byte_size("128k"), Ok(128 * 1024));
    assert_eq!(parse_byte_size("128K"), Ok(128 * 1024));
    assert_eq!(parse_byte_size("128m"), Ok(128 * 1024 * 1024));
    assert_eq!(parse_byte_size("1g"), Ok(1024 * 1024 * 1024));
    assert_eq!(parse_byte_size(" 128m "), Ok(128 * 1024 * 1024));
}

#[test]
fn parse_byte_size_rejects_invalid_input() {
    assert!(parse_byte_size("").is_err());
    assert!(parse_byte_size("m").is_err());
    assert!(parse_byte_size("128x").is_err());
    assert!(parse_byte_size("-128m").is_err());
    assert!(parse_byte_size("128m b").is_err());
}

#[test]
fn parses_shm_size_as_a_batect_style_string() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    shm_size: 128m
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.shm_size, Some(128 * 1024 * 1024));
}

#[test]
fn parses_shm_size_as_a_plain_integer() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    shm_size: 268435456
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.shm_size, Some(268435456));
}

#[test]
fn shm_size_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.shm_size, None);
}

#[test]
fn an_invalid_shm_size_string_is_rejected() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    shm_size: not-a-size
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn device_mapping_parse_string_handles_batects_own_format() {
    assert_eq!(
        DeviceMapping::parse_string("/dev/sda:/dev/xvda").unwrap(),
        DeviceMapping {
            local: "/dev/sda".to_string(),
            container: "/dev/xvda".to_string(),
            options: None,
        }
    );
    assert_eq!(
        DeviceMapping::parse_string("/dev/sda:/dev/xvda:rwm").unwrap(),
        DeviceMapping {
            local: "/dev/sda".to_string(),
            container: "/dev/xvda".to_string(),
            options: Some("rwm".to_string()),
        }
    );
}

#[test]
fn device_mapping_parse_string_rejects_invalid_input() {
    assert!(DeviceMapping::parse_string("").is_err());
    assert!(DeviceMapping::parse_string("/dev/sda").is_err());
    assert!(DeviceMapping::parse_string("/dev/sda:/dev/xvda:rwm:extra").is_err());
    assert!(DeviceMapping::parse_string(":/dev/xvda").is_err());
    assert!(DeviceMapping::parse_string("/dev/sda:").is_err());
}

#[test]
fn parses_devices_as_strings_and_objects() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    devices:
      - /dev/sda:/dev/xvda
      - local: /dev/sdb
        container: /dev/xvdb
        options: rwm
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.devices,
        Some(vec![
            DeviceMapping {
                local: "/dev/sda".to_string(),
                container: "/dev/xvda".to_string(),
                options: None,
            },
            DeviceMapping {
                local: "/dev/sdb".to_string(),
                container: "/dev/xvdb".to_string(),
                options: Some("rwm".to_string()),
            },
        ])
    );
}

#[test]
fn devices_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.devices, None);
}

#[test]
fn parses_enable_init_process() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    enable_init_process: true
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.enable_init_process, Some(true));
}

#[test]
fn enable_init_process_defaults_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.enable_init_process, None);
}

#[test]
fn parses_log_driver_and_log_options() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    log_driver: json-file
    log_options:
      max-size: 10m
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.log_driver.as_deref(), Some("json-file"));
    assert_eq!(container.log_options.as_ref().unwrap()["max-size"], "10m");
}

#[test]
fn log_driver_and_log_options_default_to_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.log_driver, None);
    assert_eq!(container.log_options, None);
}

#[test]
fn parses_image_pull_policy() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    image_pull_policy: Always
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.image_pull_policy, Some(ImagePullPolicy::Always));
}

#[test]
fn image_pull_policy_defaults_to_none_which_means_if_not_present() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(container.image_pull_policy, None);
    assert_eq!(
        container.image_pull_policy.unwrap_or_default(),
        ImagePullPolicy::IfNotPresent
    );
}

#[test]
fn an_unknown_image_pull_policy_is_rejected() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    image_pull_policy: WheneverIFeelLikeIt
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn an_unknown_capability_name_is_rejected() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    capabilities_to_add:
      - NOT_A_REAL_CAPABILITY
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn parses_build_secrets_environment_and_path_variants() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_secrets:
      token:
        environment: TOKEN
      cert:
        path: ./cert.pem
tasks: {}
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    let secrets = container.build_secrets.as_ref().unwrap();
    assert_eq!(
        secrets["token"],
        BuildSecret::Environment("TOKEN".to_string())
    );
    assert_eq!(secrets["cert"], BuildSecret::Path("./cert.pem".to_string()));
}

#[test]
fn build_secret_with_both_environment_and_path_is_rejected() {
    let err = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_secrets:
      token:
        environment: TOKEN
        path: ./cert.pem
tasks: {}
"#,
    )
    .unwrap_err();

    assert!(format!("{err:#}").contains("either 'environment' or 'path', but both"));
}

#[test]
fn build_secret_with_neither_environment_nor_path_is_rejected() {
    let err = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_secrets:
      token: {}
tasks: {}
"#,
    )
    .unwrap_err();

    assert!(format!("{err:#}").contains("either 'environment' or 'path', but neither"));
}

#[test]
fn parses_build_ssh_default_agent() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_ssh:
      - id: default
tasks: {}
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    let agents = container.build_ssh.as_ref().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, "default");
    assert!(agents[0].paths.is_empty());
}

fn no_host_env(_: &str) -> Option<String> {
    None
}

/// Unwraps a `VolumeMount` expected to be `Local` — most tests only
/// care about the `local`/`container` fields, not the enum wrapper.
fn expect_local(mount: &VolumeMount) -> &LocalVolumeMount {
    match mount {
        VolumeMount::Local(local) => local,
        VolumeMount::Cache(_) => panic!("expected a local volume mount, got a cache mount"),
        VolumeMount::Tmpfs(_) => panic!("expected a local volume mount, got a tmpfs mount"),
    }
}

#[test]
fn volume_mount_parses_two_part_string_as_local() {
    let mount = VolumeMount::parse_string("code:/code").unwrap();
    assert_eq!(
        mount,
        VolumeMount::Local(LocalVolumeMount {
            local: "code".to_string(),
            container: "/code".to_string(),
            options: None,
        })
    );
}

#[test]
fn volume_mount_parses_three_part_string_with_options_as_local() {
    // Previously left completely unresolved (no interpolation at all —
    // see git history), since the old string-splitting resolver
    // couldn't tell an options suffix apart from a Windows
    // drive-letter host path. `VolumeMount` now separates
    // local/container/options at parse time (mirroring
    // `DeviceMapping::parse_string`), so this is unambiguous — Ratect
    // has no Windows support to preserve the old ambiguity for.
    let mount = VolumeMount::parse_string("code:/code:ro").unwrap();
    assert_eq!(
        mount,
        VolumeMount::Local(LocalVolumeMount {
            local: "code".to_string(),
            container: "/code".to_string(),
            options: Some("ro".to_string()),
        })
    );
}

#[test]
fn volume_mount_rejects_an_empty_string() {
    assert!(VolumeMount::parse_string("").is_err());
}

#[test]
fn volume_mount_rejects_a_string_with_too_many_colon_separated_parts() {
    let result = VolumeMount::parse_string("C:/data:/code:ro");
    assert!(result.is_err());
}

#[test]
fn volume_mount_rejects_a_string_missing_a_container_path() {
    assert!(VolumeMount::parse_string("code").is_err());
}

#[test]
fn volume_mount_parses_cache_object_form() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: cache
        name: gradle-cache
        container: /root/.gradle
        options: rw
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.volumes.as_ref().unwrap(),
        &vec![VolumeMount::Cache(CacheVolumeMount {
            name: "gradle-cache".to_string(),
            container: "/root/.gradle".to_string(),
            options: Some("rw".to_string()),
            scope: Default::default(),
        })]
    );
}

#[test]
fn volume_mount_cache_object_form_requires_name() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: cache
        container: /root/.gradle
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_cache_object_form_forbids_local() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: cache
        name: gradle-cache
        local: /host/path
        container: /root/.gradle
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_local_object_form_forbids_name() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - local: /host/path
        name: not-allowed-here
        container: /code
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_parses_tmpfs_object_form() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        container: /code/tmp
        options: ro
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.volumes.as_ref().unwrap(),
        &vec![VolumeMount::Tmpfs(TmpfsVolumeMount {
            container: "/code/tmp".to_string(),
            options: Some("ro".to_string()),
        })]
    );
}

#[test]
fn volume_mount_parses_tmpfs_object_form_without_options() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.volumes.as_ref().unwrap(),
        &vec![VolumeMount::Tmpfs(TmpfsVolumeMount {
            container: "/code/tmp".to_string(),
            options: None,
        })]
    );
}

#[test]
fn volume_mount_tmpfs_object_form_requires_container() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_tmpfs_object_form_forbids_local() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        local: /host/path
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_tmpfs_object_form_forbids_name() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: tmpfs
        name: not-allowed-here
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_rejects_unknown_type() {
    let yaml = r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - type: bogus
        container: /code/tmp
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#;
    let result: std::result::Result<Config, _> = noyalib::from_reader(Cursor::new(yaml.as_bytes()));
    assert!(result.is_err());
}

#[test]
fn volume_mount_tmpfs_serializes_to_object_form() {
    let mount = VolumeMount::Tmpfs(TmpfsVolumeMount {
        container: "/code/tmp".to_string(),
        options: Some("ro".to_string()),
    });
    let json = serde_json::to_value(&mount).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "type": "tmpfs",
            "container": "/code/tmp",
            "options": "ro",
        })
    );
}

#[test]
fn resolve_expressions_makes_relative_local_volume_host_path_absolute() {
    let mut container = container_with_environment(HashMap::new());
    container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
        local: "code".to_string(),
        container: "/code".to_string(),
        options: None,
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    let VolumeMount::Local(resolved) = &config.containers["build-env"].volumes.as_ref().unwrap()[0]
    else {
        panic!("expected a local volume mount");
    };
    assert_eq!(resolved.local, "/base/code");
}

#[test]
fn resolve_expressions_leaves_absolute_local_volume_host_path_unchanged() {
    let mut container = container_with_environment(HashMap::new());
    container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
        local: "/already/absolute".to_string(),
        container: "/code".to_string(),
        options: None,
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    let VolumeMount::Local(resolved) = &config.containers["build-env"].volumes.as_ref().unwrap()[0]
    else {
        panic!("expected a local volume mount");
    };
    assert_eq!(resolved.local, "/already/absolute");
}

#[test]
fn resolve_expressions_interpolates_relative_local_volume_host_path_expression() {
    let mut container = container_with_environment(HashMap::new());
    container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
        local: "<subdir".to_string(),
        container: "/code".to_string(),
        options: None,
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: Some(HashMap::from([(
            "subdir".to_string(),
            ConfigVariable {
                default: Some("code".to_string()),
                description: None,
            },
        )])),
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    let VolumeMount::Local(resolved) = &config.containers["build-env"].volumes.as_ref().unwrap()[0]
    else {
        panic!("expected a local volume mount");
    };
    assert_eq!(resolved.local, "/base/code");
}

#[test]
fn resolve_expressions_interpolates_absolute_local_volume_host_path_expression_without_prefixing_base_path(
) {
    // `<project_root` resolving to an absolute path must be used as-is,
    // not treated as a literal relative fragment of `base_path` the way
    // it would be if resolution happened before interpolation.
    let mut container = container_with_environment(HashMap::new());
    container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
        local: "<project_root".to_string(),
        container: "/code".to_string(),
        options: None,
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: Some(HashMap::from([(
            "project_root".to_string(),
            ConfigVariable {
                default: Some("/abs/root".to_string()),
                description: None,
            },
        )])),
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    let VolumeMount::Local(resolved) = &config.containers["build-env"].volumes.as_ref().unwrap()[0]
    else {
        panic!("expected a local volume mount");
    };
    assert_eq!(resolved.local, "/abs/root");
}

#[test]
fn resolve_expressions_does_not_touch_cache_volume_mounts() {
    let mut container = container_with_environment(HashMap::new());
    container.volumes = Some(vec![VolumeMount::Cache(CacheVolumeMount {
        name: "gradle-cache".to_string(),
        container: "/root/.gradle".to_string(),
        options: None,
        scope: Default::default(),
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    assert_eq!(
        config.containers["build-env"].volumes.as_ref().unwrap()[0],
        VolumeMount::Cache(CacheVolumeMount {
            name: "gradle-cache".to_string(),
            container: "/root/.gradle".to_string(),
            options: None,
            scope: Default::default(),
        })
    );
}

#[test]
fn resolve_path_makes_relative_path_absolute() {
    let resolved = resolve_path(
        "docker",
        Path::new("/base"),
        &no_host_env,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(resolved, "/base/docker");
}

/// A leading `~` resolves to the host user's home directory rather than
/// being joined onto the config file's directory as a literal — matching
/// Batect's own `PathResolver.resolveHomeDir`, and what a config like
/// `local: ~/.cache/trivy` obviously intends.
#[test]
fn resolve_path_expands_a_leading_tilde_to_the_home_directory() {
    let home = crate::user::home_directory().unwrap();
    for (path, expected) in [
        ("~/.cache/trivy", home.join(".cache/trivy")),
        ("~", home.clone()),
    ] {
        let resolved = resolve_path(
            path,
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(resolved, expected.display().to_string(), "for '{path}'");
    }
}

/// Only a whole leading `~` *component* expands, matching Batect's own
/// component-wise check: `~user` (bash's "another user's home", which
/// Batect doesn't support either) and a `~` anywhere but the front stay
/// literal.
#[test]
fn resolve_path_leaves_a_tilde_that_is_not_a_leading_component_alone() {
    for (path, expected) in [
        ("~notauser/x", "/base/~notauser/x"),
        ("sub/~/x", "/base/sub/~/x"),
    ] {
        let resolved = resolve_path(
            path,
            Path::new("/base"),
            &no_host_env,
            &HashMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(resolved, expected, "for '{path}'");
    }
}

#[test]
fn resolve_path_cleans_dot_components_from_the_joined_path() {
    let resolved = resolve_path(
        "./docker",
        Path::new("/base"),
        &no_host_env,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(
        resolved, "/base/docker",
        "a leading './' shouldn't survive into the resolved path"
    );
}

#[test]
fn resolve_path_leaves_absolute_path_unchanged() {
    let resolved = resolve_path(
        "/already/absolute",
        Path::new("/base"),
        &no_host_env,
        &HashMap::new(),
        None,
    )
    .unwrap();
    assert_eq!(resolved, "/already/absolute");
}

#[test]
fn resolve_path_interpolates_expression_before_resolving() {
    let config_vars = HashMap::from([("project_root".to_string(), Some("/abs/root".to_string()))]);
    let resolved = resolve_path(
        "<project_root",
        Path::new("/base"),
        &no_host_env,
        &config_vars,
        None,
    )
    .unwrap();
    assert_eq!(resolved, "/abs/root");
}

#[test]
fn resolve_path_rejects_a_git_included_containers_absolute_path_outside_both_allowed_roots() {
    let boundary = GitBoundary {
        allow_host_paths: false,
        repo_dir: PathBuf::from("/repo"),
        remote: "https://example.com/bundle.git".to_string(),
        git_ref: "v1.0.0".to_string(),
    };
    let result = resolve_path(
        "/etc",
        Path::new("/repo/sub"),
        &no_host_env,
        &HashMap::new(),
        Some((&boundary, Path::new("/project"))),
    );
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));
}

/// `~` expansion widens what a path can reach, so it must not become a way
/// around the Git-include containment check: a third-party bundle asking
/// for `~/.ssh` resolves to the real home directory and is then rejected
/// for escaping both allowed roots, exactly as a literal `/home/...` would
/// be. (Before expansion existed this was inert — it resolved to a
/// harmless `<repo>/~/.ssh` — so it's newly load-bearing.)
#[test]
fn resolve_path_rejects_a_git_included_containers_home_directory_path() {
    let boundary = GitBoundary {
        allow_host_paths: false,
        repo_dir: PathBuf::from("/repo"),
        remote: "https://example.com/bundle.git".to_string(),
        git_ref: "v1.0.0".to_string(),
    };
    let result = resolve_path(
        "~/.ssh",
        Path::new("/repo/sub"),
        &no_host_env,
        &HashMap::new(),
        Some((&boundary, Path::new("/project"))),
    );
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));
}

#[test]
fn resolve_path_rejects_a_git_included_containers_dot_dot_traversal_outside_both_allowed_roots() {
    let boundary = GitBoundary {
        allow_host_paths: false,
        repo_dir: PathBuf::from("/repo"),
        remote: "https://example.com/bundle.git".to_string(),
        git_ref: "v1.0.0".to_string(),
    };
    let result = resolve_path(
        "../../etc",
        Path::new("/repo/sub"),
        &no_host_env,
        &HashMap::new(),
        Some((&boundary, Path::new("/project"))),
    );
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));
}

#[test]
fn resolve_path_allows_a_git_included_containers_path_within_the_clone_directory() {
    let boundary = GitBoundary {
        allow_host_paths: false,
        repo_dir: PathBuf::from("/repo"),
        remote: "https://example.com/bundle.git".to_string(),
        git_ref: "v1.0.0".to_string(),
    };
    let resolved = resolve_path(
        "sub/docker",
        Path::new("/repo"),
        &no_host_env,
        &HashMap::new(),
        Some((&boundary, Path::new("/project"))),
    )
    .unwrap();
    assert_eq!(resolved, "/repo/sub/docker");
}

#[test]
fn resolve_path_allows_a_git_included_containers_path_under_the_project_directory() {
    // A shared bundle referencing the caller's own project directory
    // (e.g. `<{batect.project_directory}/output`) is a legitimate use
    // case, not an escape — the project directory is the caller's own,
    // fully-trusted tree, distinct from the untrusted repository the
    // container definition itself came from.
    let boundary = GitBoundary {
        allow_host_paths: false,
        repo_dir: PathBuf::from("/repo"),
        remote: "https://example.com/bundle.git".to_string(),
        git_ref: "v1.0.0".to_string(),
    };
    let resolved = resolve_path(
        "/project/output",
        Path::new("/repo"),
        &no_host_env,
        &HashMap::new(),
        Some((&boundary, Path::new("/project"))),
    )
    .unwrap();
    assert_eq!(resolved, "/project/output");
}

fn container_with_build(build_directory: &str, build_args: HashMap<String, String>) -> Container {
    Container {
        extends: None,
        image: None,
        image_pull_policy: None,
        build_directory: Some(build_directory.to_string()),
        build_args: Some(build_args),
        dockerfile: None,
        build_target: None,
        build_secrets: None,
        build_ssh: None,
        volumes: None,
        dependencies: None,
        environment: None,
        run_as_current_user: None,
        additional_hostnames: None,
        additional_hosts: None,
        ports: None,
        health_check: None,
        setup_commands: None,
        working_directory: None,
        command: None,
        entrypoint: None,
        labels: None,
        capabilities_to_add: None,
        capabilities_to_drop: None,
        privileged: None,
        shm_size: None,
        devices: None,
        enable_init_process: None,
        log_driver: None,
        log_options: None,
    }
}

#[test]
fn resolve_expressions_resolves_build_directory_relative_path() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_build("docker", HashMap::new()),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"].build_directory.as_deref(),
        Some("/base/docker")
    );
}

#[test]
fn resolve_expressions_interpolates_build_args() {
    let mut build_args = HashMap::new();
    build_args.insert("MESSAGE".to_string(), "$HOST_VAR".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_build("./docker", build_args),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |name| (name == "HOST_VAR").then(|| "host-value".to_string()),
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"].build_args.as_ref().unwrap()["MESSAGE"],
        "host-value"
    );
}

#[test]
fn resolve_expressions_resolves_build_secret_path_relative_to_base() {
    let mut container = container_with_build("./docker", HashMap::new());
    container.build_secrets = Some(HashMap::from([(
        "cert".to_string(),
        BuildSecret::Path("./cert.pem".to_string()),
    )]));
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"]
            .build_secrets
            .as_ref()
            .unwrap()["cert"],
        BuildSecret::Path("/base/cert.pem".to_string())
    );
}

#[test]
fn resolve_expressions_leaves_build_secret_environment_name_unresolved() {
    let mut container = container_with_build("./docker", HashMap::new());
    container.build_secrets = Some(HashMap::from([(
        "token".to_string(),
        BuildSecret::Environment("$HOST_VAR".to_string()),
    )]));
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |name| (name == "HOST_VAR").then(|| "host-value".to_string()),
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"]
            .build_secrets
            .as_ref()
            .unwrap()["token"],
        BuildSecret::Environment("$HOST_VAR".to_string())
    );
}

/// Batect rejects each `build_*` field alongside `image`, because none
/// of them mean anything for a pulled image. Ratect read none of them in
/// that case and said nothing — the silent half of this gap, and the
/// worse half: a configured `build_secrets` that is quietly ignored
/// looks exactly like one that worked.
#[test]
fn compat_rejects_build_only_fields_alongside_an_image() {
    for (field, mutate) in [
        (
            "build_args",
            Box::new(|c: &mut Container| {
                c.build_args = Some(HashMap::from([("A".to_string(), "b".to_string())]))
            }) as Box<dyn Fn(&mut Container)>,
        ),
        (
            "build_target",
            Box::new(|c: &mut Container| c.build_target = Some("stage".to_string())),
        ),
        (
            "dockerfile",
            Box::new(|c: &mut Container| c.dockerfile = Some("Dockerfile".to_string())),
        ),
        (
            "build_secrets",
            Box::new(|c: &mut Container| c.build_secrets = Some(HashMap::new())),
        ),
        (
            "build_ssh",
            Box::new(|c: &mut Container| c.build_ssh = Some(Vec::new())),
        ),
    ] {
        let mut container = image_container();
        mutate(&mut container);
        let containers = HashMap::from([("app".to_string(), container)]);

        let err = validate_image_sources_in_compat(&containers)
            .expect_err("{field} alongside 'image' should be rejected");

        assert!(
            format!("{err:#}").contains(&format!("'{field}', which cannot be used with 'image'")),
            "unexpected error for {field}: {err:#}"
        );
    }
}

/// `resolve_image` prefers `image`, so a container with both silently
/// never built — the configuration said to build and Ratect pulled.
#[test]
fn compat_rejects_a_container_with_both_image_and_build_directory() {
    let mut container = image_container();
    container.build_directory = Some("./docker".to_string());
    let containers = HashMap::from([("app".to_string(), container)]);

    let err = validate_image_sources_in_compat(&containers)
        .expect_err("both an image and a build directory should be rejected");

    assert!(
        format!("{err:#}").contains("has both 'image' and 'build_directory'"),
        "unexpected error: {err:#}"
    );
}

/// The eager check exists to move this diagnostic from "when the task
/// ran" to "when the file loaded", so it has to use the wording
/// `engine.rs` already uses — the native format and any `Config` built
/// without `load_project` still reach the lazy one.
#[test]
fn compat_rejects_a_container_with_neither_image_nor_build_directory() {
    let mut container = image_container();
    container.image = None;
    let containers = HashMap::from([("app".to_string(), container)]);

    let err = validate_image_sources_in_compat(&containers)
        .expect_err("a container with no image source should be rejected");

    assert!(
        format!("{err:#}")
            .contains("Container 'app' has neither 'image' nor 'build_directory' set"),
        "unexpected error: {err:#}"
    );
}

/// A container with a plain pulled image and no build fields at all —
/// the valid baseline each rejection test above perturbs by one field.
fn image_container() -> Container {
    let mut container = container_with_build("./docker", HashMap::new());
    container.build_directory = None;
    // `container_with_build` sets `build_args`, which is itself one of
    // the fields under test — clear every build-only field so each case
    // below perturbs exactly one.
    container.build_args = None;
    container.build_target = None;
    container.dockerfile = None;
    container.build_secrets = None;
    container.build_ssh = None;
    container.image = Some("alpine:3.18".to_string());
    container
}

/// `run_as_current_user` takes ownership of each cache mount, which means
/// uploading an archive to that path — so a non-absolute one has to be
/// caught, and caught here, where every other config error names the
/// container the user wrote. Previously it surfaced from the Docker layer
/// against a 64-hex container id.
#[test]
fn resolve_expressions_rejects_a_relative_cache_path_under_run_as_current_user() {
    let mut container = container_with_run_as_current_user(true, Some("/home/x"));
    container.volumes = Some(vec![VolumeMount::Cache(CacheVolumeMount {
        name: "c".to_string(),
        container: "relative/path".to_string(),
        options: None,
        scope: Default::default(),
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let err = config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains("Container 'build-env'") && message.contains("not an absolute path"),
        "unexpected error: {message}"
    );
}

/// Deliberately scoped to what Batect checks. It never validates a `local`
/// or `tmpfs` destination, so rejecting one here would stop a
/// Windows-container config (`container: C:\code`) from even loading,
/// while it still runs under `batect` — a `--list-tasks` that fails is a
/// worse divergence than a mount Docker rejects later.
#[test]
fn resolve_expressions_accepts_a_non_slash_local_mount_path() {
    let mut container = container_with_build("./docker", HashMap::new());
    container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
        local: ".".to_string(),
        container: "C:\\code".to_string(),
        options: None,
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .expect("a Windows-style container path should still load");
}

/// `scope` is `ratect`-native, so a `batect.yml` using it is rejected rather
/// than silently given the default — the same treatment `extends` gets, and
/// for the same reason: a config accepted here that real `batect` refuses is
/// one-way lock-in, which is the whole thing the compat binary exists to
/// avoid.
#[tokio::test]
async fn a_shared_cache_is_rejected_in_a_batect_yml() {
    let dir = std::env::temp_dir().join(format!("ratect-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers:\n  app:\n    image: alpine:3.18\n    volumes:\n      \
         - type: cache\n        name: registry\n        container: /registry\n        \
         scope: shared\ntasks:\n  t:\n    run:\n      container: app\n",
    )
    .unwrap();

    let err = load_project(&dir.join("batect.yml"), &HashMap::new())
        .await
        .expect_err("scope is native-only");
    std::fs::remove_dir_all(&dir).ok();

    let message = format!("{err:#}");
    assert!(
        message.contains("not supported in Batect-compatible configuration"),
        "unexpected error: {message}"
    );
}

/// One cache name means one piece of storage, so a project cannot give the
/// same name two scopes — `caches clean <name>` would have no way to resolve
/// it. Two *containers* naming one cache is the ordinary way to share it
/// between them, so the check is per project rather than per container.
#[test]
fn resolve_expressions_rejects_one_cache_name_with_two_scopes() {
    let cache = |scope| {
        VolumeMount::Cache(CacheVolumeMount {
            name: "registry".to_string(),
            container: "/registry".to_string(),
            options: None,
            scope,
        })
    };
    let mut first = container_with_build("./docker", HashMap::new());
    first.volumes = Some(vec![cache(CacheScope::Project)]);
    let mut second = container_with_build("./docker", HashMap::new());
    second.volumes = Some(vec![cache(CacheScope::Shared)]);

    let config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("a".to_string(), first), ("b".to_string(), second)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let err = reject_conflicting_cache_scopes(&config)
        .expect_err("one name cannot mean two pieces of storage");
    assert!(
        format!("{err:#}").contains("both 'project' and 'shared' scope"),
        "unexpected error: {err:#}"
    );
}

fn container_with_build_ssh(agents: Vec<SshAgent>) -> Container {
    let mut container = container_with_build("./docker", HashMap::new());
    container.build_ssh = Some(agents);
    container
}

#[test]
fn resolve_expressions_accepts_a_single_default_ssh_agent() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_build_ssh(vec![SshAgent {
                id: "default".to_string(),
                paths: Vec::new(),
            }]),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();
}

/// Batect's own `SSHAgent.id` has no default, so a `build_ssh` entry
/// omitting it is invalid there. Accepting it here would let a config
/// work under `ratect-compat` and fail under `batect`, which is the one
/// direction a drop-in replacement must not diverge in — BuildKit's
/// implicit `default` id has to be written out.
#[test]
fn parsing_rejects_a_build_ssh_agent_with_no_id() {
    let err = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    build_directory: ./docker
    build_ssh:
      - paths:
          - keys/id_ed25519
tasks: {}
"#,
    )
    .unwrap_err();

    assert!(
        format!("{err:#}").contains("id"),
        "the error should name the missing field: {err:#}"
    );
}

/// Several agents under distinct ids is Batect's own behaviour, and
/// what a Dockerfile selecting `--mount=type=ssh,id=deploy` needs.
#[test]
fn resolve_expressions_accepts_several_distinctly_named_build_ssh_agents() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_build_ssh(vec![
                SshAgent {
                    id: "default".to_string(),
                    paths: Vec::new(),
                },
                SshAgent {
                    id: "deploy".to_string(),
                    paths: Vec::new(),
                },
            ]),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();
}

/// A `paths` entry is a host path, resolved against the config file's
/// own directory exactly like `build_directory` and `build_secrets`
/// — so `id_ed25519` means one next to the config file, not one in
/// whatever directory `ratect` happened to be run from.
#[test]
fn resolve_expressions_resolves_build_ssh_paths_against_the_config_directory() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_build_ssh(vec![SshAgent {
                id: "default".to_string(),
                paths: vec![
                    "keys/id_ed25519".to_string(),
                    "/etc/keys/id_rsa".to_string(),
                ],
            }]),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();

    let agents = config.containers["build-env"].build_ssh.as_ref().unwrap();
    assert_eq!(
        agents[0].paths,
        vec![
            "/base/keys/id_ed25519".to_string(),
            "/etc/keys/id_rsa".to_string(),
        ]
    );
}

/// A Dockerfile picks an agent by id, so two entries claiming one id
/// have no defined meaning — matching Batect, which rejects them too.
#[test]
fn resolve_expressions_rejects_duplicate_build_ssh_agent_ids() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_build_ssh(vec![
                SshAgent {
                    id: "deploy".to_string(),
                    paths: Vec::new(),
                },
                SshAgent {
                    id: "deploy".to_string(),
                    paths: Vec::new(),
                },
            ]),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let err = config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("more than one 'build_ssh' entry with the id 'deploy'"),
        "unexpected error: {err:#}"
    );
}

fn container_with_run_as_current_user(enabled: bool, home_directory: Option<&str>) -> Container {
    Container {
        extends: None,
        image: Some("alpine:3.18".to_string()),
        image_pull_policy: None,
        build_directory: None,
        build_args: None,
        dockerfile: None,
        build_target: None,
        build_secrets: None,
        build_ssh: None,
        volumes: None,
        dependencies: None,
        environment: None,
        run_as_current_user: Some(RunAsCurrentUser {
            enabled,
            home_directory: home_directory.map(|s| s.to_string()),
        }),
        additional_hostnames: None,
        additional_hosts: None,
        ports: None,
        health_check: None,
        setup_commands: None,
        working_directory: None,
        command: None,
        entrypoint: None,
        labels: None,
        capabilities_to_add: None,
        capabilities_to_drop: None,
        privileged: None,
        shm_size: None,
        devices: None,
        enable_init_process: None,
        log_driver: None,
        log_options: None,
    }
}

fn config_with_container(container: Container) -> Config {
    Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    }
}

#[test]
fn parses_run_as_current_user() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    run_as_current_user:
      enabled: true
      home_directory: /home/container-user
tasks: {}
"#,
    );

    let run_as_current_user = config.containers["build-env"]
        .run_as_current_user
        .as_ref()
        .unwrap();
    assert!(run_as_current_user.enabled);
    assert_eq!(
        run_as_current_user.home_directory.as_deref(),
        Some("/home/container-user")
    );
}

#[test]
fn parses_additional_hostnames_and_hosts() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    additional_hostnames:
      - db-alias
      - cache-alias
    additional_hosts:
      external-service: 10.0.0.1
tasks: {}
"#,
    );

    let container = &config.containers["build-env"];
    assert_eq!(
        container.additional_hostnames,
        Some(vec!["db-alias".to_string(), "cache-alias".to_string()])
    );
    assert_eq!(
        container.additional_hosts,
        Some(HashMap::from([(
            "external-service".to_string(),
            "10.0.0.1".to_string()
        )]))
    );
}

#[test]
fn parses_absent_additional_hostnames_and_hosts_as_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks: {}
"#,
    );

    let container = &config.containers["build-env"];
    assert_eq!(container.additional_hostnames, None);
    assert_eq!(container.additional_hosts, None);
}

#[test]
fn resolve_expressions_leaves_additional_hostnames_and_hosts_untouched() {
    let mut config = config_with_container(Container {
        additional_hostnames: Some(vec!["db-alias".to_string()]),
        additional_hosts: Some(HashMap::from([(
            "external-service".to_string(),
            "10.0.0.1".to_string(),
        )])),
        ..container_with_build("docker", HashMap::new())
    });

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();

    let container = &config.containers["build-env"];
    assert_eq!(
        container.additional_hostnames,
        Some(vec!["db-alias".to_string()])
    );
    assert_eq!(
        container.additional_hosts,
        Some(HashMap::from([(
            "external-service".to_string(),
            "10.0.0.1".to_string()
        )]))
    );
}

fn port_mapping(local: (u16, u16), container: (u16, u16), protocol: &str) -> PortMapping {
    PortMapping {
        local: PortRange {
            from: local.0,
            to: local.1,
        },
        container: PortRange {
            from: container.0,
            to: container.1,
        },
        protocol: protocol.to_string(),
    }
}

#[test]
fn parses_ports_string_form() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - "8080:80"
      - "9000:9000/udp"
tasks: {}
"#,
    );

    let container = &config.containers["build-env"];
    assert_eq!(
        container.ports,
        Some(vec![
            port_mapping((8080, 8080), (80, 80), "tcp"),
            port_mapping((9000, 9000), (9000, 9000), "udp"),
        ])
    );
}

#[test]
fn parses_ports_string_form_with_ranges() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - "8000-8002:9000-9002/udp"
tasks: {}
"#,
    );

    assert_eq!(
        config.containers["build-env"].ports,
        Some(vec![port_mapping((8000, 8002), (9000, 9002), "udp")])
    );
}

#[test]
fn parses_ports_object_form() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: 8080
        container: 80
      - local: 8000-8002
        container: 9000-9002
        protocol: udp
tasks: {}
"#,
    );

    assert_eq!(
        config.containers["build-env"].ports,
        Some(vec![
            port_mapping((8080, 8080), (80, 80), "tcp"),
            port_mapping((8000, 8002), (9000, 9002), "udp"),
        ])
    );
}

fn try_parse(yaml: &str) -> Result<Config> {
    noyalib::from_reader(Cursor::new(yaml.as_bytes())).context("failed to parse")
}

#[test]
fn parsing_ports_string_form_rejects_mismatched_range_sizes() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - "8000-8002:9000-9001"
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parsing_ports_object_form_rejects_mismatched_range_sizes() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: 8000-8002
        container: 9000-9001
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parses_absent_ports_as_none() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
tasks: {}
"#,
    );

    assert_eq!(config.containers["build-env"].ports, None);
}

#[test]
fn parses_health_check_config() {
    let config = parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      command: pg_isready -h localhost
      interval: 2s
      retries: 5
      start_period: 1m30s
      timeout: 500ms
tasks: {}
"#,
    );

    let health_check = config.containers["database"].health_check.as_ref().unwrap();
    assert_eq!(
        health_check.command.as_deref(),
        Some("pg_isready -h localhost")
    );
    assert_eq!(
        health_check.interval,
        Some(std::time::Duration::from_secs(2))
    );
    assert_eq!(health_check.retries, Some(5));
    assert_eq!(
        health_check.start_period,
        Some(std::time::Duration::from_secs(90))
    );
    assert_eq!(
        health_check.timeout,
        Some(std::time::Duration::from_millis(500))
    );
}

#[test]
fn parses_partial_health_check_config() {
    let config = parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      command: pg_isready
tasks: {}
"#,
    );

    let health_check = config.containers["database"].health_check.as_ref().unwrap();
    assert_eq!(health_check.command.as_deref(), Some("pg_isready"));
    assert_eq!(health_check.interval, None);
    assert_eq!(health_check.retries, None);
    assert_eq!(health_check.start_period, None);
    assert_eq!(health_check.timeout, None);
}

#[test]
fn parsing_health_check_rejects_invalid_duration() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      interval: 2 seconds
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parsing_health_check_rejects_unknown_fields() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:13
    health_check:
      cmd: pg_isready
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parses_setup_commands() {
    let config = parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:13
    setup_commands:
      - command: ./apply-migrations.sh
      - command: ./seed-data.sh
        working_directory: /setup
tasks: {}
"#,
    );

    let commands = config.containers["database"]
        .setup_commands
        .as_ref()
        .unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].command, "./apply-migrations.sh");
    assert_eq!(commands[0].working_directory, None);
    assert_eq!(commands[1].command, "./seed-data.sh");
    assert_eq!(commands[1].working_directory.as_deref(), Some("/setup"));
}

#[test]
fn parsing_setup_commands_rejects_missing_command() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  database:
    image: postgres:13
    setup_commands:
      - working_directory: /setup
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parse_duration_handles_batect_formats() {
    use std::time::Duration;

    assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
    assert_eq!(parse_duration("+0").unwrap(), Duration::ZERO);
    assert_eq!(parse_duration("-0").unwrap(), Duration::ZERO);
    assert_eq!(parse_duration("100ns").unwrap(), Duration::from_nanos(100));
    assert_eq!(parse_duration("2us").unwrap(), Duration::from_micros(2));
    assert_eq!(parse_duration("2µs").unwrap(), Duration::from_micros(2));
    assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
    assert_eq!(parse_duration("2.5s").unwrap(), Duration::from_millis(2500));
    assert_eq!(parse_duration(".5s").unwrap(), Duration::from_millis(500));
    assert_eq!(parse_duration("2.s").unwrap(), Duration::from_secs(2));
    assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
    assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
    assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5400));
    assert_eq!(
        parse_duration("1h2m3s4ms").unwrap(),
        Duration::from_millis(3_723_004)
    );
}

#[test]
fn parse_duration_rejects_invalid_input() {
    for invalid in [
        "",
        "2",
        "s",
        ".s",
        "2 s",
        "2 seconds",
        "2S",
        "abc",
        "2ss",
        "2.5.3s",
        "-2s",
        "2s-1s",
    ] {
        assert!(
            parse_duration(invalid).is_err(),
            "expected '{invalid}' to be rejected"
        );
    }
}

#[test]
fn resolve_expressions_leaves_ports_untouched() {
    let mut config = config_with_container(Container {
        ports: Some(vec![port_mapping((8080, 8080), (80, 80), "tcp")]),
        ..container_with_build("docker", HashMap::new())
    });

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"].ports,
        Some(vec![port_mapping((8080, 8080), (80, 80), "tcp")])
    );
}

#[test]
fn port_range_parses_a_single_port() {
    assert_eq!(
        PortRange::parse("8080").unwrap(),
        PortRange {
            from: 8080,
            to: 8080
        }
    );
}

#[test]
fn port_range_parses_a_range() {
    assert_eq!(
        PortRange::parse("8000-8002").unwrap(),
        PortRange {
            from: 8000,
            to: 8002
        }
    );
}

#[test]
fn port_range_rejects_zero() {
    assert!(PortRange::parse("0").is_err());
}

#[test]
fn port_range_rejects_descending_bounds() {
    assert!(PortRange::parse("8002-8000").is_err());
}

#[test]
fn port_range_rejects_non_numeric_input() {
    assert!(PortRange::parse("abc").is_err());
}

#[test]
fn port_mapping_expand_yields_one_triple_for_a_single_port() {
    let mapping = port_mapping((8080, 8080), (80, 80), "tcp");
    assert_eq!(mapping.expand(), vec![(8080, 80, "tcp".to_string())]);
}

#[test]
fn port_mapping_expand_zips_a_range_by_position() {
    let mapping = port_mapping((8000, 8002), (9000, 9002), "udp");
    assert_eq!(
        mapping.expand(),
        vec![
            (8000, 9000, "udp".to_string()),
            (8001, 9001, "udp".to_string()),
            (8002, 9002, "udp".to_string()),
        ]
    );
}

#[test]
fn port_mapping_parse_string_rejects_an_empty_definition() {
    assert!(PortMapping::parse_string("")
        .unwrap_err()
        .to_string()
        .contains("cannot be empty"));
}

#[test]
fn port_mapping_parse_string_rejects_a_definition_without_a_colon() {
    assert!(PortMapping::parse_string("8080").is_err());
}

#[test]
fn port_mapping_parse_string_rejects_an_empty_component() {
    assert!(PortMapping::parse_string("8080:80/").is_err());
    assert!(PortMapping::parse_string(":80").is_err());
    assert!(PortMapping::parse_string("8080:").is_err());
}

#[test]
fn parsing_ports_object_form_rejects_an_unknown_field() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: 8080
        container: 80
        banana: 1
tasks: {}
"#,
    );
    // `{:?}` renders anyhow's full context chain — the serde detail
    // naming the field sits below `try_parse`'s own outer context.
    assert!(format!("{:?}", result.unwrap_err()).contains("banana"));
}

#[test]
fn parsing_ports_object_form_rejects_a_missing_local_or_container() {
    for object in ["local: 8080", "container: 80"] {
        let result = try_parse(&format!(
            r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - {object}
tasks: {{}}
"#,
        ));
        assert!(result.is_err(), "'{object}' alone should be rejected");
    }
}

#[test]
fn parsing_a_port_mapping_that_is_neither_string_nor_object_is_an_error() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - true
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn parsing_a_port_range_that_is_neither_number_nor_string_is_an_error() {
    let result = try_parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    ports:
      - local: true
        container: 80
tasks: {}
"#,
    );
    assert!(result.is_err());
}

#[test]
fn port_mapping_serializes_to_its_string_form_and_round_trips() {
    let single = port_mapping((8080, 8080), (80, 80), "tcp");
    let ranged = port_mapping((8000, 8002), (9000, 9002), "udp");

    for mapping in [single, ranged] {
        let yaml = noyalib::to_string(&mapping).expect("should serialize");
        let reparsed: PortMapping = noyalib::from_reader(Cursor::new(yaml.as_bytes()))
            .expect("the serialized form should re-parse");
        assert_eq!(reparsed, mapping, "round-trip through: {yaml}");
    }
}

#[test]
fn resolve_expressions_errors_when_run_as_current_user_enabled_without_home_directory() {
    let mut config = config_with_container(container_with_run_as_current_user(true, None));

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("no 'home_directory' was provided"));
}

#[test]
fn resolve_expressions_errors_when_home_directory_given_without_run_as_current_user_enabled() {
    let mut config = config_with_container(container_with_run_as_current_user(
        false,
        Some("/home/container-user"),
    ));

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("'run_as_current_user.enabled' is not true"));
}

#[test]
fn resolve_expressions_errors_when_run_as_current_user_home_directory_is_not_absolute() {
    let mut config = config_with_container(container_with_run_as_current_user(
        true,
        Some("home/container-user"),
    ));

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("is not an absolute path"));
}

#[test]
fn resolve_expressions_errors_when_run_as_current_user_home_directory_contains_a_colon() {
    // SEC-002: a ':' would shift the fields of the colon-delimited
    // /etc/passwd/etc/shadow line `home_directory` is interpolated into.
    let mut config = config_with_container(container_with_run_as_current_user(
        true,
        Some("/home/x:0:0:root:/root:/bin/sh"),
    ));

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("contains a ':' or a control character"));
}

#[test]
fn resolve_expressions_errors_when_run_as_current_user_home_directory_contains_a_newline() {
    // SEC-002: a newline would inject an entirely new, attacker-chosen
    // /etc/passwd/etc/shadow entry rather than just extending this one.
    let mut config = config_with_container(container_with_run_as_current_user(
        true,
        Some("/home/x\nbackdoor:x:0:0::/root:/bin/sh"),
    ));

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        no_host_env,
    );

    assert!(result
        .unwrap_err()
        .to_string()
        .contains("contains a ':' or a control character"));
}

#[test]
fn resolve_expressions_interpolates_run_as_current_user_home_directory() {
    let mut config = config_with_container(container_with_run_as_current_user(
        true,
        Some("/home/$HOST_VAR"),
    ));

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |name| (name == "HOST_VAR").then(|| "container-user".to_string()),
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"]
            .run_as_current_user
            .as_ref()
            .unwrap()
            .home_directory
            .as_deref(),
        Some("/home/container-user")
    );
}

#[test]
fn resolve_expressions_leaves_disabled_run_as_current_user_unaffected() {
    let mut config = config_with_container(container_with_run_as_current_user(false, None));

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            no_host_env,
        )
        .unwrap();

    let run_as_current_user = config.containers["build-env"]
        .run_as_current_user
        .as_ref()
        .unwrap();
    assert!(!run_as_current_user.enabled);
    assert_eq!(run_as_current_user.home_directory, None);
}

/// A fresh, unique scratch directory for tests that need to write real
/// files to disk (e.g. to exercise `load_from_file`'s own file I/O,
/// not just YAML parsing). Caller is responsible for cleanup via
/// `std::fs::remove_dir_all`.
///
/// Includes a monotonic counter alongside the PID/timestamp: tests run
/// in parallel by default, and two calls landing in the same clock tick
/// (observed in practice — coarser than nanosecond resolution on some
/// platforms) would otherwise collide on the same directory and produce
/// flaky failures.
fn unique_temp_dir() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let dir = std::env::temp_dir().join(format!(
        "ratect-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn load_from_file_then_resolve_expressions_resolves_paths() {
    let dir = unique_temp_dir();
    let config_path = dir.join("batect.yml");
    std::fs::write(
        &config_path,
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
tasks: {}
"#,
    )
    .unwrap();

    let mut loaded = Config::load_from_file(&config_path).await.unwrap();
    loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

    let volume = expect_local(
        &loaded.config.containers["build-env"]
            .volumes
            .as_ref()
            .unwrap()[0],
    );
    assert_eq!(volume.local, dir.join("code").display().to_string());
    assert_eq!(volume.container, "/code");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The native TOML front-end must produce the *same* resolved `Config` as
/// the YAML it replaces — the whole promise of the format being a
/// re-spelling, not a redesign, of what `ratect-core` consumes. The same
/// project is written in both formats (inline tables for the object-shape
/// fields), loaded through each path, and compared after resolution.
#[tokio::test]
async fn native_toml_loads_identically_to_the_yaml_twin() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
tasks:
  test:
    run:
      container: build-env
      command: cargo test
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"

[containers.build-env]
image = "alpine:3.18"
volumes = [{ local = "code", container = "/code" }]

[tasks.test]
run = { container = "build-env", command = "cargo test" }
"#,
    )
    .unwrap();

    let mut yaml = Config::load_from_file(&dir.join("batect.yml"))
        .await
        .unwrap();
    yaml.resolve_expressions(&dir, &HashMap::new()).unwrap();
    let mut toml = Config::load_from_file_native(&dir.join("ratect.toml"))
        .await
        .unwrap();
    toml.resolve_expressions(&dir, &HashMap::new()).unwrap();

    assert_eq!(yaml.config.project_name, toml.config.project_name);
    assert_eq!(
        yaml.config.containers["build-env"].image,
        toml.config.containers["build-env"].image
    );
    let yaml_volume = expect_local(
        &yaml.config.containers["build-env"]
            .volumes
            .as_ref()
            .unwrap()[0],
    );
    let toml_volume = expect_local(
        &toml.config.containers["build-env"]
            .volumes
            .as_ref()
            .unwrap()[0],
    );
    assert_eq!(yaml_volume.local, toml_volume.local);
    assert_eq!(toml_volume.local, dir.join("code").display().to_string());
    assert_eq!(toml_volume.container, "/code");
    assert!(toml.config.tasks.contains_key("test"));

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The repository ships its own dev-task config in *both* formats —
/// `batect.yml` (run via `ratect-compat`) and `ratect.toml` (run via
/// `ratect`) — so the project dogfoods both binaries. The two must describe
/// the same project; comparing their resolved `Config`s (as JSON, so field
/// order doesn't matter) fails loudly if an edit to one isn't mirrored in
/// the other.
#[tokio::test]
async fn the_two_root_dev_configs_agree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let batect = root.join("batect.yml");
    let ratect = root.join("ratect.toml");

    let mut from_yaml = Config::load_from_file(&batect).await.unwrap();
    from_yaml
        .resolve_expressions(base_path_for(&batect), &HashMap::new())
        .unwrap();

    let mut from_toml = Config::load_from_file_native(&ratect).await.unwrap();
    from_toml
        .resolve_expressions(base_path_for(&ratect), &HashMap::new())
        .unwrap();

    assert_eq!(
        serde_json::to_value(&from_yaml.config).unwrap(),
        serde_json::to_value(&from_toml.config).unwrap(),
        "batect.yml and ratect.toml describe different projects — keep them in sync"
    );
}

/// Under native mode the parser follows the *extension*, so a YAML root
/// (or include) still loads — that's what lets a project migrate its root
/// to TOML while a `.yml` include stays as-is.
#[tokio::test]
async fn native_mode_still_parses_a_yaml_file_by_extension() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ncontainers: {}\ntasks: {}\n",
    )
    .unwrap();
    let loaded = Config::load_from_file_native(&dir.join("batect.yml"))
        .await
        .unwrap();
    assert_eq!(loaded.config.project_name, "demo");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An unrecognized extension is rejected rather than guessed at — TOML and
/// YAML are too easy to confuse for a simple document to sniff safely.
#[tokio::test]
async fn native_mode_rejects_an_unrecognized_config_extension() {
    let dir = unique_temp_dir();
    std::fs::write(dir.join("config.txt"), "project_name = \"demo\"\n").unwrap();
    let result = Config::load_from_file_native(&dir.join("config.txt")).await;
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("Unrecognized config file extension"),
        "got: {message}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `ratect-compat` must not newly accept TOML: a `.toml` handed to the
/// compat path is parsed as YAML (exactly Batect's behavior) and fails,
/// rather than being silently understood as TOML.
#[tokio::test]
async fn compat_mode_does_not_accept_toml() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.toml"),
        "project_name = \"demo\"\n[containers.build-env]\nimage = \"alpine\"\n",
    )
    .unwrap();
    let result = Config::load_from_file(&dir.join("batect.toml")).await;
    assert!(result.is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn load_from_file_missing_file_errors() {
    let result = Config::load_from_file(Path::new("/nonexistent/batect.yml")).await;
    assert!(result.is_err());
}

/// The offline names primitive shell completion uses: sorted task names
/// from one file (TOML or YAML by extension), and — crucially — an empty
/// list rather than an error on a missing or broken file, so a `<TAB>` on a
/// half-written config stays silent.
#[test]
fn task_names_for_completion_reads_names_offline() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("ratect.toml"),
        "project_name = \"demo\"\n[tasks.check]\nrun = { container = \"a\" }\n\
             [tasks.build]\nrun = { container = \"a\" }\n",
    )
    .unwrap();
    assert_eq!(
        task_names_for_completion(&dir.join("ratect.toml")),
        vec!["build".to_string(), "check".to_string()]
    );

    // A YAML file works too — parsed by extension.
    std::fs::write(
        dir.join("batect.yml"),
        "project_name: demo\ntasks:\n  ci:\n    prerequisites: [build]\n",
    )
    .unwrap();
    assert_eq!(
        task_names_for_completion(&dir.join("batect.yml")),
        vec!["ci".to_string()]
    );

    // Missing and broken files yield no completions, never an error.
    assert!(task_names_for_completion(&dir.join("nope.toml")).is_empty());
    std::fs::write(dir.join("broken.toml"), "this = is [not valid").unwrap();
    assert!(task_names_for_completion(&dir.join("broken.toml")).is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

/// Completion follows local includes, and an include cycle (here the root
/// includes itself, and a fragment includes the root back) terminates
/// instead of looping — the `visited` set does both jobs.
#[test]
fn task_names_for_completion_follows_local_includes_without_looping() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("ci")).unwrap();
    std::fs::write(
        dir.join("ratect.toml"),
        "project_name = \"demo\"\n\
             include = [{ path = \"ci/more.toml\" }, { path = \"ratect.toml\" }]\n\
             [tasks.build]\nrun = { container = \"a\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ci/more.toml"),
        "include = [{ path = \"../ratect.toml\" }]\n\
             [tasks.deploy]\nrun = { container = \"a\" }\n",
    )
    .unwrap();

    assert_eq!(
        task_names_for_completion(&dir.join("ratect.toml")),
        vec!["build".to_string(), "deploy".to_string()]
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `to_native_toml` renders a config (here one with the interleaved scalar
/// and table fields that trip naive TOML serialization, plus the custom
/// `volumes`/`ports` (de)serializers) and its own round-trip check passes,
/// so the result parses straight back to an equivalent `Config`.
#[test]
fn to_native_toml_renders_and_round_trips() {
    let config = parse(
        r#"
project_name: demo
containers:
  app:
    image: alpine:3.18
    environment:
      TZ: UTC
    volumes:
      - .:/code
    ports:
      - "8080:80"
tasks:
  build:
    run:
      container: app
      command: echo hi
"#,
    );
    let text = to_native_toml(&config).unwrap();
    let reparsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(reparsed.project_name, "demo");
    assert!(reparsed.containers.contains_key("app"));
    assert!(reparsed.tasks.contains_key("build"));
    assert_eq!(
        reparsed.containers["app"].image.as_deref(),
        Some("alpine:3.18")
    );

    // Pins `config convert`'s documented v1 limitation: `ports`/`volumes`
    // come out in the compact *string* form (the existing Batect-compatible
    // `Serialize` impls), not the object form the native format documents
    // as canonical. Both are accepted and both round-trip, so nothing is
    // broken — but a change here is a change to documented behaviour, and
    // should be noticed rather than discovered.
    assert!(
        text.contains("\"8080:80/tcp\""),
        "ports should still serialize to the compact string form: {text}"
    );
    assert!(
        text.contains("\".:/code\""),
        "volumes should still serialize to the compact string form: {text}"
    );
}

/// A helper: run the native load-and-resolve path on a single TOML string
/// written to a temp `ratect.toml`, returning the resolved project.
async fn load_native_toml(body: &str) -> Result<LoadedProject> {
    let dir = unique_temp_dir();
    let path = dir.join("ratect.toml");
    std::fs::write(&path, body).unwrap();
    let result = load_project_native(&path, &HashMap::new()).await;
    std::fs::remove_dir_all(&dir).ok();
    result
}

/// The heart of `extends`: a child inherits every field it doesn't set,
/// and a field it *does* set replaces the parent's outright (shallow —
/// here `environment` is fully the child's, not merged).
#[tokio::test]
async fn extends_inherits_unset_fields_and_child_overrides_win() {
    let project = load_native_toml(
        r#"
project_name = "demo"

[containers.base]
image = "alpine:3.18"
working_directory = "/base"
environment = { TZ = "UTC", CI = "true" }

[containers.app]
extends = "base"
working_directory = "/app"
environment = { TZ = "Europe/London" }

[tasks.t]
run = { container = "app" }
"#,
    )
    .await
    .unwrap();

    let app = &project.config.containers["app"];
    // Inherited (child left unset).
    assert_eq!(app.image.as_deref(), Some("alpine:3.18"));
    // Overridden (child set its own).
    assert_eq!(app.working_directory.as_deref(), Some("/app"));
    // Shallow: the child's environment replaces the parent's entirely, so
    // the parent's CI entry is gone, not merged in.
    let env = app.environment.as_ref().unwrap();
    assert_eq!(env.get("TZ").map(String::as_str), Some("Europe/London"));
    assert!(!env.contains_key("CI"));
    // The `extends` field is consumed — the resolved container has none.
    assert!(app.extends.is_none());
}

/// `extends` chains transitively: `a` -> `b` -> `c`, each level filling
/// what the one below left unset.
#[tokio::test]
async fn extends_chains_transitively() {
    let project = load_native_toml(
        r#"
project_name = "demo"

[containers.c]
image = "alpine:3.18"
working_directory = "/c"

[containers.b]
extends = "c"
working_directory = "/b"
command = "b-cmd"

[containers.a]
extends = "b"
command = "a-cmd"

[tasks.t]
run = { container = "a" }
"#,
    )
    .await
    .unwrap();

    let a = &project.config.containers["a"];
    assert_eq!(a.image.as_deref(), Some("alpine:3.18")); // from c
    assert_eq!(a.working_directory.as_deref(), Some("/b")); // from b
    assert_eq!(a.command.as_deref(), Some("a-cmd")); // a's own
}

/// Resolve-then-extend: an inherited relative path was resolved against
/// the *parent's* file before inheritance, so the child gets the parent's
/// absolute path, not one re-anchored to the child's own location. Here
/// both containers share the root file, so the point is simply that the
/// inherited `build_directory` is absolute and correct.
#[tokio::test]
async fn extends_inherits_an_already_resolved_path() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("ctx")).unwrap();
    let path = dir.join("ratect.toml");
    std::fs::write(
        &path,
        r#"
project_name = "demo"

[containers.base]
build_directory = "ctx"

[containers.app]
extends = "base"

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();
    let project = load_project_native(&path, &HashMap::new()).await.unwrap();
    assert_eq!(
        project.config.containers["app"].build_directory,
        Some(dir.join("ctx").display().to_string())
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The reason `extends` resolves *after* path resolution: a `base` in an
/// included file in a *different directory* has its relative
/// `build_directory` anchored to *its own* file's directory, and a child
/// in the root that inherits it must keep that anchoring — not re-resolve
/// it against the child's own directory. This is the cross-boundary case
/// the ordering exists to protect (`extends_inherits_an_already_resolved_path`
/// only covers the same-directory case); a regression to extend-then-resolve
/// would silently re-anchor the path here.
#[tokio::test]
async fn extends_across_an_include_boundary_keeps_the_parents_path_anchoring() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(
        dir.join("sub/base.toml"),
        "[containers.base]\nbuild_directory = \"ctx\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"
include = [{ path = "sub/base.toml" }]

[containers.app]
extends = "base"

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();

    let project = load_project_native(&dir.join("ratect.toml"), &HashMap::new())
        .await
        .unwrap();
    let build_directory = project.config.containers["app"]
        .build_directory
        .clone()
        .unwrap();
    // Anchored to the parent's own directory (`sub/`), not the child's root.
    assert_eq!(
        build_directory,
        dir.join("sub").join("ctx").display().to_string()
    );
    assert_ne!(build_directory, dir.join("ctx").display().to_string());

    std::fs::remove_dir_all(&dir).ok();
}

/// `extends` reaches across the *format* boundary: a native container
/// inherits from one defined in an included YAML file, because the
/// container namespace is flat once includes are merged (ADR-0003's
/// "flows one way"). Also covers per-extension parser selection for local
/// includes from a native root — a `.yml` and a `.toml` fragment in one
/// project, each parsed as its own format.
#[tokio::test]
async fn extends_can_inherit_from_a_container_defined_in_a_yaml_include() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("base.yml"),
        "containers:\n  base:\n    image: alpine:3.18\n    working_directory: /base\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("more.toml"),
        "[containers.sidecar]\nimage = \"redis:7\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ratect.toml"),
        r#"
project_name = "demo"
include = [{ path = "base.yml" }, { path = "more.toml" }]

[containers.app]
extends = "base"

[tasks.t]
run = { container = "app" }
"#,
    )
    .unwrap();

    let project = load_project_native(&dir.join("ratect.toml"), &HashMap::new())
        .await
        .unwrap();
    let app = &project.config.containers["app"];
    assert_eq!(app.image.as_deref(), Some("alpine:3.18"));
    assert_eq!(app.working_directory.as_deref(), Some("/base"));
    // The TOML fragment came through its own parser.
    assert_eq!(
        project.config.containers["sidecar"].image.as_deref(),
        Some("redis:7")
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A longer cycle than the two-node and self-cases already covered, so the
/// ancestor-path walk is pinned for a chain rather than just an immediate
/// repeat.
#[tokio::test]
async fn a_three_container_extends_cycle_errors() {
    let err = load_native_toml(
        r#"
project_name = "demo"
[containers.a]
extends = "b"
[containers.b]
extends = "c"
[containers.c]
extends = "a"
[tasks.t]
run = { container = "a" }
"#,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("cyclic 'extends'"),
        "got: {err:#}"
    );
}

#[tokio::test]
async fn extends_a_missing_container_errors() {
    let err = load_native_toml(
        r#"
project_name = "demo"
[containers.app]
extends = "nope"
image = "alpine"
[tasks.t]
run = { container = "app" }
"#,
    )
    .await
    .unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("extends 'nope', which is not defined"),
        "got: {message}"
    );
}

#[tokio::test]
async fn extends_a_cycle_errors() {
    let err = load_native_toml(
        r#"
project_name = "demo"
[containers.a]
extends = "b"
[containers.b]
extends = "a"
[tasks.t]
run = { container = "a" }
"#,
    )
    .await
    .unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("cyclic 'extends'"), "got: {message}");
}

#[tokio::test]
async fn a_container_extending_itself_is_a_cycle() {
    let err = load_native_toml(
        r#"
project_name = "demo"
[containers.a]
extends = "a"
[tasks.t]
run = { container = "a" }
"#,
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("cyclic 'extends'"));
}

/// `extends` is native-only: a `batect.yml` using it is rejected (not
/// silently ignored), keeping `ratect-compat` a faithful Batect match.
#[tokio::test]
async fn extends_is_rejected_in_compat_mode() {
    let dir = unique_temp_dir();
    let path = dir.join("batect.yml");
    std::fs::write(
            &path,
            "project_name: demo\ncontainers:\n  app:\n    extends: base\n    image: alpine\ntasks: {}\n",
        )
        .unwrap();
    let err = load_project(&path, &HashMap::new()).await.unwrap_err();
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        format!("{err:#}").contains("uses 'extends'"),
        "expected a compat rejection"
    );
}

/// `load_project` is the whole load-resolve sequence both binaries use,
/// so this proves the steps happen *and* happen in the right order: the
/// volume path below is only correct if includes were merged before
/// expressions were resolved, and the override only wins if it's
/// applied at resolution rather than being ignored.
#[tokio::test]
async fn load_project_resolves_includes_expressions_and_the_project_directory() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("containers.yml"),
        r#"
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
    environment:
      GREETING: <greeting
"#,
    )
    .unwrap();
    let config_path = dir.join("batect.yml");
    std::fs::write(
        &config_path,
        r#"
project_name: demo
include:
  - containers.yml
config_variables:
  greeting:
    default: from-the-default
tasks:
  test:
    run:
      container: build-env
      command: echo hi
"#,
    )
    .unwrap();

    let overrides = HashMap::from([("greeting".to_string(), "from-the-override".to_string())]);
    let project = load_project(&config_path, &overrides).await.unwrap();

    assert_eq!(project.project_directory, dir.clean());
    let container = &project.config.containers["build-env"];
    assert_eq!(
        container.environment.as_ref().unwrap()["GREETING"],
        "from-the-override"
    );
    assert_eq!(
        expect_local(&container.volumes.as_ref().unwrap()[0]).local,
        dir.join("code").display().to_string()
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn load_project_fails_fast_when_the_config_file_is_missing() {
    let error = load_project(Path::new("/nonexistent/batect.yml"), &HashMap::new())
        .await
        .expect_err("a missing config file should be an error, not an empty config");
    assert!(
        error.to_string().contains("not found"),
        "the error should say the file is missing: {error}"
    );
}

#[tokio::test]
async fn load_from_file_unsupported_key_errors() {
    let dir = unique_temp_dir();
    let config_path = dir.join("batect.yml");
    std::fs::write(
        &config_path,
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    not_a_real_field: json-file
tasks: {}
"#,
    )
    .unwrap();

    let result = Config::load_from_file(&config_path).await;
    assert!(result.is_err());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn include_merges_containers_tasks_and_config_variables_from_another_file() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
include:
  - extra.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("extra.yml"),
        r#"
tasks:
  extra-task:
    run:
      container: build-env
config_variables:
  extra_var:
    default: value
"#,
    )
    .unwrap();

    let loaded = Config::load_from_file(&dir.join("batect.yml"))
        .await
        .unwrap();
    assert!(loaded.config.containers.contains_key("build-env"));
    assert!(loaded.config.tasks.contains_key("extra-task"));
    assert!(loaded
        .config
        .config_variables
        .as_ref()
        .unwrap()
        .contains_key("extra_var"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn nested_includes_are_resolved_transitively() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - a.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("a.yml"),
        r#"
containers:
  build-env:
    image: alpine:3.18
include:
  - nested/b.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("nested/b.yml"),
        r#"
tasks:
  deep-task:
    run:
      container: build-env
"#,
    )
    .unwrap();

    let loaded = Config::load_from_file(&dir.join("batect.yml"))
        .await
        .unwrap();
    assert!(loaded.config.containers.contains_key("build-env"));
    assert!(loaded.config.tasks.contains_key("deep-task"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_file_included_from_two_places_is_only_loaded_once() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - a.yml
  - b.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("a.yml"),
        r#"
include:
  - shared.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("b.yml"),
        r#"
include:
  - shared.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("shared.yml"),
        r#"
tasks:
  shared-task:
    run:
      container: build-env
"#,
    )
    .unwrap();

    // If `shared.yml` were (incorrectly) loaded twice, this would fail
    // with a "defined in multiple files" error instead.
    let loaded = Config::load_from_file(&dir.join("batect.yml"))
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("shared-task"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_task_defined_in_two_different_files_is_an_error() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
tasks:
  build:
    run:
      container: build-env
include:
  - extra.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("extra.yml"),
        r#"
tasks:
  build:
    run:
      container: build-env
"#,
    )
    .unwrap();

    let result = Config::load_from_file(&dir.join("batect.yml")).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("build"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn project_name_in_an_included_file_is_an_error() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - extra.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("extra.yml"),
        r#"
project_name: not-allowed
"#,
    )
    .unwrap();

    let result = Config::load_from_file(&dir.join("batect.yml")).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("project_name"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_missing_include_path_errors_clearly() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - does-not-exist.yml
"#,
    )
    .unwrap();

    let result = Config::load_from_file(&dir.join("batect.yml")).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("does not exist"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_include_path_that_is_a_directory_errors_clearly() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("a-directory")).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - a-directory
"#,
    )
    .unwrap();

    let result = Config::load_from_file(&dir.join("batect.yml")).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("is not a file"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_relative_volume_path_in_an_included_file_resolves_against_its_own_directory() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - nested/extra.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("nested/extra.yml"),
        r#"
containers:
  build-env:
    image: alpine:3.18
    volumes:
      - code:/code
"#,
    )
    .unwrap();

    let mut loaded = Config::load_from_file(&dir.join("batect.yml"))
        .await
        .unwrap();
    loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

    let volume = expect_local(
        &loaded.config.containers["build-env"]
            .volumes
            .as_ref()
            .unwrap()[0],
    );
    assert_eq!(
        volume.local,
        dir.join("nested").join("code").display().to_string()
    );
    assert_eq!(volume.container, "/code");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn project_directory_var_in_an_included_file_resolves_to_the_root_directory() {
    let dir = unique_temp_dir();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - nested/extra.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("nested/extra.yml"),
        r#"
containers:
  build-env:
    image: alpine:3.18
    environment:
      PROJECT_DIR: <batect.project_directory
"#,
    )
    .unwrap();

    let mut loaded = Config::load_from_file(&dir.join("batect.yml"))
        .await
        .unwrap();
    loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

    let value = &loaded.config.containers["build-env"]
        .environment
        .as_ref()
        .unwrap()["PROJECT_DIR"];
    assert_eq!(*value, dir.display().to_string());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn include_accepts_both_bare_string_and_object_form() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("extra.yml"),
        r#"
tasks:
  extra-task:
    run:
      container: build-env
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("string-form.yml"),
        r#"
project_name: demo
include:
  - extra.yml
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("object-form.yml"),
        r#"
project_name: demo
include:
  - type: file
    path: extra.yml
"#,
    )
    .unwrap();

    let loaded = Config::load_from_file(&dir.join("string-form.yml"))
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("extra-task"));

    let loaded = Config::load_from_file(&dir.join("object-form.yml"))
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("extra-task"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn include_with_unsupported_type_errors_clearly() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: bundle
    path: bundle.yml
"#,
    )
    .unwrap();

    let result = Config::load_from_file(&dir.join("batect.yml")).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("not supported"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_git_include_clones_the_repo_and_merges_containers_and_tasks() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: bundle.yml
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "bundle.yml".to_string(),
        r#"
containers:
  bundled:
    image: alpine:3.18
tasks:
  bundled-task:
    run:
      container: bundled
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.containers.contains_key("bundled"));
    assert!(loaded.config.tasks.contains_key("bundled-task"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_include_without_an_explicit_path_defaults_to_batect_bundle_yml() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
tasks:
  bundled-task:
    run:
      container: build-env
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("bundled-task"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

/// Runs a sequence of `git` subcommands in `repo`, isolated from the
/// developer's own global/system git config (which might set a signing
/// key, hooks, or a template) and with a fixed identity, so the result is
/// deterministic wherever it runs.
fn run_git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Ratect Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Ratect Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("failed to run git — is it installed and on PATH?");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The counterpart to the `FakeGitClient` tests above, run against the
/// *real* [`SystemGitClient`]: a `type: git` include of an actual local
/// git repository, checked out at a real tag. The fake client just writes
/// pre-registered files, so it never exercises the one thing that can
/// only go wrong in production — the real `git clone`/`checkout`/ref
/// resolution and the default `batect-bundle.yml` path applied to a real
/// clone tree. Deliberately hermetic: a locally-built repo stands in for
/// a published bundle (no network — see ROADMAP's conformance section for
/// why a live remote is out) and a fixed temp cache root keeps it out of
/// `~/.ratect`.
#[tokio::test]
async fn a_git_include_clones_a_real_local_repository_with_the_system_client() {
    let bundle = unique_temp_dir();
    std::fs::write(
        bundle.join("batect-bundle.yml"),
        r#"
containers:
  bundled:
    image: alpine:3.18
tasks:
  bundled-task:
    run:
      container: bundled
"#,
    )
    .unwrap();
    run_git(&bundle, &["init", "--quiet"]);
    run_git(&bundle, &["add", "-A"]);
    run_git(&bundle, &["commit", "--quiet", "-m", "the bundle"]);
    run_git(&bundle, &["tag", "v1"]);

    let project = unique_temp_dir();
    std::fs::write(
        project.join("batect.yml"),
        // No `path:`, so the include must default to batect-bundle.yml —
        // resolved against the real clone tree, not a fake one.
        format!(
            "project_name: demo\ninclude:\n  - type: git\n    repo: {}\n    ref: v1\n",
            bundle.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let loaded = Config::load_from_file_with_git_cache(&project.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.containers.contains_key("bundled"));
    assert!(loaded.config.tasks.contains_key("bundled-task"));

    std::fs::remove_dir_all(&bundle).unwrap();
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

/// `allow_host_paths: true` on an include the *project owner* declared lets
/// that bundle's containers resolve host paths outside the containment —
/// the case that's otherwise a hard failure with no in-config workaround
/// (a shared tool cache at `~/.cache/<tool>`). See decisions/0004.
#[tokio::test]
async fn a_vouched_for_git_include_may_use_host_paths() {
    let bundle = git_bundle_repo(&[(
        "batect-bundle.yml",
        "containers:\n  trivy:\n    image: alpine:3.18\n    volumes:\n      \
             - local: ~/.cache/trivy\n        container: /cache\n",
    )]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("batect.yml"),
        format!(
            "project_name: demo\ninclude:\n  - type: git\n    repo: {}\n    ref: v1\n    \
                 allow_host_paths: true\n",
            bundle.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let mut loaded = Config::load_from_file_with_git_cache(&project.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    loaded
        .resolve_expressions(&project, &HashMap::new())
        .expect("a vouched-for bundle's host path should be allowed");

    let volume = expect_local(&loaded.config.containers["trivy"].volumes.as_ref().unwrap()[0]);
    let home = crate::user::home_directory().unwrap();
    assert_eq!(
        volume.local,
        home.join(".cache/trivy").display().to_string()
    );

    std::fs::remove_dir_all(&bundle).ok();
    std::fs::remove_dir_all(&project).ok();
    std::fs::remove_dir_all(&cache_root).ok();
}

/// Property 2 of decisions/0004, and the one that makes the flag a control
/// rather than theatre: a bundle setting `allow_host_paths` on *its own*
/// nested include grants nothing. Here the project vouches for nothing, the
/// outer bundle tries to vouch for the inner one, and the inner one's host
/// path is still rejected.
#[tokio::test]
async fn a_bundle_cannot_grant_host_paths_to_its_own_nested_include() {
    let inner = git_bundle_repo(&[(
        "batect-bundle.yml",
        "containers:\n  inner:\n    image: alpine:3.18\n    volumes:\n      \
             - local: ~/.ssh\n        container: /keys\n",
    )]);
    let outer = git_bundle_repo(&[(
        "batect-bundle.yml",
        &format!(
            "include:\n  - type: git\n    repo: {}\n    ref: v1\n    allow_host_paths: true\n",
            inner.display()
        ),
    )]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("batect.yml"),
        format!(
            "project_name: demo\ninclude:\n  - type: git\n    repo: {}\n    ref: v1\n",
            outer.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let mut loaded = Config::load_from_file_with_git_cache(&project.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    let error = loaded
        .resolve_expressions(&project, &HashMap::new())
        .expect_err("a bundle must not be able to grant host paths to what it includes");
    assert!(
        format!("{error:?}").contains("escapes both the Git repository"),
        "got: {error:?}"
    );

    std::fs::remove_dir_all(&inner).ok();
    std::fs::remove_dir_all(&outer).ok();
    std::fs::remove_dir_all(&project).ok();
    std::fs::remove_dir_all(&cache_root).ok();
}

/// Property 1: vouching for a bundle says nothing about bundles *it*
/// includes. The project trusts the outer bundle; the inner one — which the
/// owner never named — stays contained.
#[tokio::test]
async fn vouching_for_a_bundle_does_not_extend_to_its_nested_includes() {
    let inner = git_bundle_repo(&[(
        "batect-bundle.yml",
        "containers:\n  inner:\n    image: alpine:3.18\n    volumes:\n      \
             - local: ~/.ssh\n        container: /keys\n",
    )]);
    let outer = git_bundle_repo(&[(
        "batect-bundle.yml",
        &format!(
            "include:\n  - type: git\n    repo: {}\n    ref: v1\n",
            inner.display()
        ),
    )]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("batect.yml"),
        format!(
            "project_name: demo\ninclude:\n  - type: git\n    repo: {}\n    ref: v1\n    \
                 allow_host_paths: true\n",
            outer.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let mut loaded = Config::load_from_file_with_git_cache(&project.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    let error = loaded
        .resolve_expressions(&project, &HashMap::new())
        .expect_err("trust must not extend to a nested include");
    assert!(
        format!("{error:?}").contains("escapes both the Git repository"),
        "got: {error:?}"
    );

    std::fs::remove_dir_all(&inner).ok();
    std::fs::remove_dir_all(&outer).ok();
    std::fs::remove_dir_all(&project).ok();
    std::fs::remove_dir_all(&cache_root).ok();
}

/// Builds a git repo containing whichever bundle files `bundles` names
/// (path -> contents), commits them at tag `v1`, and returns its directory.
fn git_bundle_repo(bundles: &[(&str, &str)]) -> std::path::PathBuf {
    let repo = unique_temp_dir();
    for (name, contents) in bundles {
        std::fs::write(repo.join(name), contents).unwrap();
    }
    run_git(&repo, &["init", "--quiet"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "--quiet", "-m", "bundles"]);
    run_git(&repo, &["tag", "v1"]);
    repo
}

/// A native project with a pathless `type: git` include prefers the repo's
/// `ratect-bundle.toml` over its `batect-bundle.yml` when both are present —
/// so a bundle author can ship both and native takes the TOML.
#[tokio::test]
async fn a_native_pathless_git_include_prefers_ratect_bundle_toml() {
    let bundle = git_bundle_repo(&[
        (
            "ratect-bundle.toml",
            "[containers.from_toml]\nimage = \"alpine:3.18\"\n",
        ),
        (
            "batect-bundle.yml",
            "containers:\n  from_yaml:\n    image: alpine:3.18\n",
        ),
    ]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("ratect.toml"),
        format!(
            "project_name = \"demo\"\n[[include]]\ntype = \"git\"\nrepo = \"{}\"\nref = \"v1\"\n",
            bundle.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let loaded =
        Config::load_from_file_native_with_git_cache(&project.join("ratect.toml"), &git_cache)
            .await
            .unwrap();
    assert!(loaded.config.containers.contains_key("from_toml"));
    assert!(!loaded.config.containers.contains_key("from_yaml"));

    std::fs::remove_dir_all(&bundle).unwrap();
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

/// When only `batect-bundle.yml` is present, a native pathless include
/// falls back to it — an unmigrated Batect bundle stays usable from a
/// native project.
#[tokio::test]
async fn a_native_pathless_git_include_falls_back_to_batect_bundle_yml() {
    let bundle = git_bundle_repo(&[(
        "batect-bundle.yml",
        "containers:\n  from_yaml:\n    image: alpine:3.18\n",
    )]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("ratect.toml"),
        format!(
            "project_name = \"demo\"\n[[include]]\ntype = \"git\"\nrepo = \"{}\"\nref = \"v1\"\n",
            bundle.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let loaded =
        Config::load_from_file_native_with_git_cache(&project.join("ratect.toml"), &git_cache)
            .await
            .unwrap();
    assert!(loaded.config.containers.contains_key("from_yaml"));

    std::fs::remove_dir_all(&bundle).unwrap();
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

/// The one genuinely new error branch in the bundle-probe refactor: a
/// pathless `type: git` include whose repository contains *neither*
/// candidate reports both names it looked for, rather than the
/// single-candidate "does not exist" message.
#[tokio::test]
async fn a_pathless_git_include_with_no_bundle_file_names_both_candidates() {
    let bundle = git_bundle_repo(&[("something-else.yml", "containers: {}\n")]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("ratect.toml"),
        format!(
            "project_name = \"demo\"\n[[include]]\ntype = \"git\"\nrepo = \"{}\"\nref = \"v1\"\n",
            bundle.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let error =
        Config::load_from_file_native_with_git_cache(&project.join("ratect.toml"), &git_cache)
            .await
            .expect_err("a bundle with no recognised file should fail");
    let message = format!("{error:#}");
    assert!(
        message.contains("ratect-bundle.toml") && message.contains("batect-bundle.yml"),
        "the error should name both candidates: {message}"
    );

    std::fs::remove_dir_all(&bundle).ok();
    std::fs::remove_dir_all(&project).ok();
    std::fs::remove_dir_all(&cache_root).ok();
}

/// The compat path never looks for `ratect-bundle.toml` — Batect knows
/// nothing of it — so with both present it still takes `batect-bundle.yml`.
#[tokio::test]
async fn a_compat_pathless_git_include_ignores_a_ratect_bundle_toml() {
    let bundle = git_bundle_repo(&[
        (
            "ratect-bundle.toml",
            "[containers.from_toml]\nimage = \"alpine:3.18\"\n",
        ),
        (
            "batect-bundle.yml",
            "containers:\n  from_yaml:\n    image: alpine:3.18\n",
        ),
    ]);
    let project = unique_temp_dir();
    std::fs::write(
        project.join("batect.yml"),
        format!(
            "project_name: demo\ninclude:\n  - type: git\n    repo: {}\n    ref: v1\n",
            bundle.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);
    let loaded = Config::load_from_file_with_git_cache(&project.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.containers.contains_key("from_yaml"));
    assert!(!loaded.config.containers.contains_key("from_toml"));

    std::fs::remove_dir_all(&bundle).unwrap();
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_relative_volume_path_in_a_git_included_file_resolves_against_the_clone_directory() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
containers:
  bundled:
    image: alpine:3.18
    volumes:
      - code:/code
tasks: {}
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

    let volume = expect_local(
        &loaded.config.containers["bundled"]
            .volumes
            .as_ref()
            .unwrap()[0],
    );
    let clone_dir = cache_root.join(crate::git_include::cache_key(
        "https://example.com/bundle.git",
        "v1.0.0",
    ));
    assert_eq!(volume.local, clone_dir.join("code").display().to_string());
    assert_eq!(volume.container, "/code");

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_included_containers_volume_with_an_absolute_host_path_outside_the_clone_and_project_directory_is_rejected(
) {
    // SEC-001: the 0.8.0 fix (commit 6fcd0b8) only contained an
    // `include`'s own `path` field to its Git repository's clone
    // directory — it didn't stop a container *declared inside* a
    // Git-included bundle from mounting an arbitrary host path via
    // `volumes`, which is exactly what this reproduces.
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
containers:
  bundled:
    image: alpine:3.18
    volumes:
      - /:/hostroot
tasks: {}
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    let result = loaded.resolve_expressions(&dir, &HashMap::new());
    assert!(
        format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"),
        "a container declared inside a Git include must not be able to mount an arbitrary \
             host path"
    );

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_included_containers_build_directory_escaping_via_dot_dot_traversal_is_rejected() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
containers:
  bundled:
    build_directory: ../../../../../../etc
tasks: {}
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    let result = loaded.resolve_expressions(&dir, &HashMap::new());
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_included_containers_build_secret_path_escaping_via_dot_dot_traversal_is_rejected() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
containers:
  bundled:
    build_directory: .
    build_secrets:
      token:
        path: ../../../../../../etc/passwd
tasks: {}
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    let result = loaded.resolve_expressions(&dir, &HashMap::new());
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes both the Git repository"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_included_containers_volume_referencing_the_project_directory_is_allowed() {
    // Referencing the caller's own project directory (as opposed to an
    // arbitrary host path) is a legitimate, expected use of a shared
    // bundle — e.g. mounting an output directory back into the
    // project. It must stay allowed even though it's outside the Git
    // repository's own clone directory.
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
containers:
  bundled:
    image: alpine:3.18
    volumes:
      - <{batect.project_directory}/output:/output
tasks: {}
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let mut loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    loaded.resolve_expressions(&dir, &HashMap::new()).unwrap();

    let volume = expect_local(
        &loaded.config.containers["bundled"]
            .volumes
            .as_ref()
            .unwrap()[0],
    );
    assert_eq!(volume.local, dir.join("output").display().to_string());
    assert_eq!(volume.container, "/output");

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_local_include_inside_a_git_bundle_resolves_against_the_clone_directory() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        r#"
include:
  - nested.yml
"#
        .to_string(),
    );
    files.insert(
        "nested.yml".to_string(),
        r#"
tasks:
  nested-task:
    run:
      container: build-env
"#
        .to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("nested-task"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_includes_own_path_escaping_via_an_absolute_path_is_rejected() {
    let dir = unique_temp_dir();
    let outside = unique_temp_dir();
    std::fs::write(
        outside.join("secret.yml"),
        "tasks:\n  leaked-task:\n    run:\n      container: build-env\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        format!(
            r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: {}
"#,
            outside.join("secret.yml").display()
        ),
    )
    .unwrap();

    // The bundle itself doesn't even need to contain the target file —
    // an absolute `path` bypasses the clone directory entirely via
    // `PathBuf::join`'s own documented behavior, which is exactly the
    // bug being guarded against here.
    let git =
        FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", HashMap::new());
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_includes_own_path_escaping_via_dot_dot_traversal_is_rejected() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: ../../../../../../etc/passwd
"#,
    )
    .unwrap();

    let git =
        FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", HashMap::new());
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_nested_local_include_inside_a_git_bundle_escaping_the_clone_is_rejected() {
    let dir = unique_temp_dir();
    let outside = unique_temp_dir();
    std::fs::write(
        outside.join("secret.yml"),
        "tasks:\n  leaked-task:\n    run:\n      container: build-env\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "batect-bundle.yml".to_string(),
        format!(
            "include:\n  - path: {}\n",
            outside.join("secret.yml").display()
        ),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_nested_git_include_inside_a_git_bundle_still_works() {
    // A Git-included bundle composing in *another* Git repo (a fresh
    // boundary of its own) must not be rejected by the containment
    // check meant for local-file escapes.
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/outer.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let mut outer_files = HashMap::new();
    outer_files.insert(
        "batect-bundle.yml".to_string(),
        r#"
include:
  - type: git
    repo: https://example.com/inner.git
    ref: v2.0.0
"#
        .to_string(),
    );
    let mut inner_files = HashMap::new();
    inner_files.insert(
        "batect-bundle.yml".to_string(),
        "tasks:\n  inner-task:\n    run:\n      container: build-env\n".to_string(),
    );
    let git = FakeGitClient::new()
        .with_files("https://example.com/outer.git", "v1.0.0", outer_files)
        .with_files("https://example.com/inner.git", "v2.0.0", inner_files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("inner-task"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_symlink_inside_a_git_bundle_escaping_the_clone_is_rejected() {
    let dir = unique_temp_dir();
    let outside = unique_temp_dir();
    std::fs::write(
        outside.join("secret.yml"),
        "tasks:\n  leaked-task:\n    run:\n      container: build-env\n",
    )
    .unwrap();

    // A real repo (needs real git — symlinks committed to a repo are
    // what this test is actually exercising) whose own bundle file
    // is a symlink pointing outside the clone entirely.
    let repo_dir = unique_temp_dir();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_dir)
            .args(args)
            .status()
            .expect("git must be installed to run this test");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    // The host's global git config must not leak into the scratch repo's
    // commits/tags — see the equivalent isolation in
    // `git_include.rs`'s `create_test_repo`.
    run(&["config", "commit.gpgsign", "false"]);
    run(&["config", "tag.gpgsign", "false"]);
    run(&["config", "tag.forceSignAnnotated", "false"]);
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.join("secret.yml"),
        repo_dir.join("batect-bundle.yml"),
    )
    .unwrap();
    run(&["add", "batect-bundle.yml"]);
    run(&["commit", "--quiet", "-m", "initial"]);
    run(&["tag", "v1.0.0"]);

    std::fs::write(
        dir.join("batect.yml"),
        format!(
            r#"
project_name: demo
include:
  - type: git
    repo: {}
    ref: v1.0.0
"#,
            repo_dir.display()
        ),
    )
    .unwrap();

    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(
        cache_root.clone(),
        crate::git_include::SystemGitClient,
        1000,
    );

    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("escapes the Git repository"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(&repo_dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn two_git_includes_for_the_same_repo_and_ref_only_clone_once() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: a.yml
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
    path: b.yml
"#,
    )
    .unwrap();

    let mut files = HashMap::new();
    files.insert(
        "a.yml".to_string(),
        "tasks:\n  a-task:\n    run:\n      container: build-env\n".to_string(),
    );
    files.insert(
        "b.yml".to_string(),
        "tasks:\n  b-task:\n    run:\n      container: build-env\n".to_string(),
    );
    let git = FakeGitClient::new().with_files("https://example.com/bundle.git", "v1.0.0", files);
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git.clone(), 1000);

    let loaded = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache)
        .await
        .unwrap();
    assert!(loaded.config.tasks.contains_key("a-task"));
    assert!(loaded.config.tasks.contains_key("b-task"));
    assert_eq!(git.clone_count(), 1);

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[tokio::test]
async fn a_git_include_missing_repo_or_ref_is_a_clear_parse_error() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let git_cache = GitIncludeCache::for_test(unique_temp_dir(), FakeGitClient::new(), 1000);
    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(result.is_err());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn repo_and_ref_are_rejected_on_a_non_git_include() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - repo: https://example.com/bundle.git
    ref: v1.0.0
    path: extra.yml
"#,
    )
    .unwrap();

    let git_cache = GitIncludeCache::for_test(unique_temp_dir(), FakeGitClient::new(), 1000);
    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("only valid for 'type: git'"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_git_clone_failure_surfaces_a_clear_error() {
    let dir = unique_temp_dir();
    std::fs::write(
        dir.join("batect.yml"),
        r#"
project_name: demo
include:
  - type: git
    repo: https://example.com/bundle.git
    ref: v1.0.0
"#,
    )
    .unwrap();

    let git = FakeGitClient::new().failing("simulated network failure");
    let cache_root = unique_temp_dir();
    let git_cache = GitIncludeCache::for_test(cache_root.clone(), git, 1000);

    let result = Config::load_from_file_with_git_cache(&dir.join("batect.yml"), &git_cache).await;
    assert!(format!("{:?}", result.unwrap_err()).contains("simulated network failure"));

    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&cache_root).unwrap();
}

#[test]
fn load_config_vars_file_parses_a_flat_map() {
    let dir = unique_temp_dir();
    let vars_path = dir.join("vars.yml");
    std::fs::write(
        &vars_path,
        r#"
env_name: staging
region: eu
"#,
    )
    .unwrap();

    let vars = Config::load_config_vars_file(&vars_path).unwrap();
    assert_eq!(vars.get("env_name"), Some(&"staging".to_string()));
    assert_eq!(vars.get("region"), Some(&"eu".to_string()));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_config_vars_file_missing_file_errors() {
    let result = Config::load_config_vars_file(Path::new("/nonexistent/vars.yml"));
    assert!(result.is_err());
}

/// The native config-vars loader picks its parser by extension, so a
/// `.toml` file (the `ratect.local.toml` default) and a `.yml` file (an
/// explicitly-named override, or a migrating `batect.local.yml`) both
/// produce the same flat map.
#[test]
fn load_config_vars_file_native_parses_toml_and_yaml_by_extension() {
    let dir = unique_temp_dir();

    let toml_path = dir.join("ratect.local.toml");
    std::fs::write(&toml_path, "env_name = \"staging\"\nregion = \"eu\"\n").unwrap();
    let from_toml = Config::load_config_vars_file_native(&toml_path).unwrap();
    assert_eq!(from_toml.get("env_name"), Some(&"staging".to_string()));
    assert_eq!(from_toml.get("region"), Some(&"eu".to_string()));

    let yaml_path = dir.join("overrides.yml");
    std::fs::write(&yaml_path, "env_name: staging\nregion: eu\n").unwrap();
    let from_yaml = Config::load_config_vars_file_native(&yaml_path).unwrap();
    assert_eq!(from_toml, from_yaml);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_config_vars_file_malformed_yaml_errors() {
    let dir = unique_temp_dir();
    let vars_path = dir.join("vars.yml");
    // A YAML sequence, not the flat name/value map load_config_vars_file expects.
    std::fs::write(&vars_path, "- not\n- a map\n").unwrap();

    let result = Config::load_config_vars_file(&vars_path);
    assert!(result.is_err());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn parses_container_and_task_run_environment() {
    let config = parse(
        r#"
project_name: demo
containers:
  build-env:
    image: alpine:3.18
    environment:
      CONTAINER_VAR: container-value
tasks:
  test:
    run:
      container: build-env
      command: echo hi
      environment:
        RUN_VAR: run-value
"#,
    );

    let container = config.containers.get("build-env").unwrap();
    assert_eq!(
        container.environment.as_ref().unwrap().get("CONTAINER_VAR"),
        Some(&"container-value".to_string())
    );

    let task = config.tasks.get("test").unwrap();
    assert_eq!(
        task.run
            .as_ref()
            .unwrap()
            .environment
            .as_ref()
            .unwrap()
            .get("RUN_VAR"),
        Some(&"run-value".to_string())
    );
}

#[test]
fn parses_config_variables() {
    let config = parse(
        r#"
project_name: demo
containers: {}
tasks: {}
config_variables:
  env_name:
    default: dev
  no_default: {}
"#,
    );

    let vars = config.config_variables.unwrap();
    assert_eq!(vars["env_name"].default.as_deref(), Some("dev"));
    assert_eq!(vars["no_default"].default, None);
}

#[test]
fn config_variables_accept_an_inert_description_field() {
    let config = parse(
        r#"
project_name: demo
containers: {}
tasks: {}
config_variables:
  env_name:
    default: dev
    description: "which environment to target"
"#,
    );

    let vars = config.config_variables.unwrap();
    assert_eq!(
        vars["env_name"].description.as_deref(),
        Some("which environment to target")
    );
}

#[test]
fn forbid_telemetry_is_accepted_but_inert() {
    let config = parse(
        r#"
project_name: demo
containers: {}
tasks: {}
forbid_telemetry: true
"#,
    );

    assert_eq!(config.forbid_telemetry, Some(true));
}

fn container_with_environment(environment: HashMap<String, String>) -> Container {
    Container {
        extends: None,
        build_args: None,
        image: Some("alpine:3.18".to_string()),
        image_pull_policy: None,
        build_directory: None,
        dockerfile: None,
        build_target: None,
        build_secrets: None,
        build_ssh: None,
        volumes: None,
        dependencies: None,
        environment: Some(environment),
        run_as_current_user: None,
        additional_hostnames: None,
        additional_hosts: None,
        ports: None,
        health_check: None,
        setup_commands: None,
        working_directory: None,
        command: None,
        entrypoint: None,
        labels: None,
        capabilities_to_add: None,
        capabilities_to_drop: None,
        privileged: None,
        shm_size: None,
        devices: None,
        enable_init_process: None,
        log_driver: None,
        log_options: None,
    }
}

#[test]
fn resolve_expressions_expands_host_var() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "$HOST_VAR".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(
            Path::new("/base"),
            &HashMap::new(),
            &HashMap::new(),
            |name| (name == "HOST_VAR").then(|| "host-value".to_string()),
        )
        .unwrap();

    assert_eq!(
        config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
        "host-value"
    );
}

#[test]
fn resolve_expressions_uses_default_when_host_var_unset() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "${HOST_VAR:-fallback}".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    assert_eq!(
        config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
        "fallback"
    );
}

#[test]
fn resolve_expressions_errors_when_host_var_unset_without_default() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "$HOST_VAR".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        |_| None,
    );
    assert!(result.is_err());
}

#[test]
fn resolve_expressions_prefers_cli_override_over_default() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "<env_name".to_string());
    let mut config_variables = HashMap::new();
    config_variables.insert(
        "env_name".to_string(),
        ConfigVariable {
            default: Some("dev".to_string()),
            description: None,
        },
    );
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: Some(config_variables),
        forbid_telemetry: None,
    };

    let overrides = HashMap::from([("env_name".to_string(), "prod".to_string())]);
    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &overrides, |_| None)
        .unwrap();

    assert_eq!(
        config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
        "prod"
    );
}

#[test]
fn resolve_expressions_falls_back_to_config_variable_default() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "<env_name".to_string());
    let mut config_variables = HashMap::new();
    config_variables.insert(
        "env_name".to_string(),
        ConfigVariable {
            default: Some("dev".to_string()),
            description: None,
        },
    );
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: Some(config_variables),
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    assert_eq!(
        config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
        "dev"
    );
}

#[test]
fn resolve_expressions_errors_on_undeclared_config_variable_reference() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "<missing".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        |_| None,
    );
    assert!(result.is_err());
}

#[test]
fn resolve_expressions_errors_on_declared_config_variable_with_no_value() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "<env_name".to_string());
    let mut config_variables = HashMap::new();
    config_variables.insert(
        "env_name".to_string(),
        ConfigVariable {
            default: None,
            description: None,
        },
    );
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: Some(config_variables),
        forbid_telemetry: None,
    };

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        |_| None,
    );
    assert!(result.is_err());
}

#[test]
fn resolve_expressions_errors_on_unknown_cli_override() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::new(),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let overrides = HashMap::from([("unknown".to_string(), "value".to_string())]);
    let result =
        config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), &overrides, |_| None);
    assert!(result.is_err());
}

#[test]
fn resolve_expressions_leaves_literal_values_unchanged() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "literal-value".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    assert_eq!(
        config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
        "literal-value"
    );
}

#[test]
fn resolve_expressions_resolves_built_in_project_directory_var_in_environment() {
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "<batect.project_directory".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    assert_eq!(
        config.containers["build-env"].environment.as_ref().unwrap()["FOO"],
        "/base"
    );
}

#[test]
fn resolve_expressions_resolves_built_in_project_directory_var_in_volumes() {
    let mut container = container_with_environment(HashMap::new());
    container.volumes = Some(vec![VolumeMount::Local(LocalVolumeMount {
        local: "<{batect.project_directory}/scripts".to_string(),
        container: "/scripts".to_string(),
        options: None,
    })]);
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([("build-env".to_string(), container)]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new("/base"), &HashMap::new(), &HashMap::new(), |_| {
            None
        })
        .unwrap();

    let volume = expect_local(&config.containers["build-env"].volumes.as_ref().unwrap()[0]);
    assert_eq!(volume.local, "/base/scripts");
    assert_eq!(volume.container, "/scripts");
}

#[test]
fn resolve_expressions_cleans_project_directory_var_when_base_path_is_empty() {
    // An empty `base_path` is what `main.rs` passes for a bare `-f
    // batect.yml` (no directory prefix) — `Path::parent()` on that
    // returns `Some("")`, not `None`. Without cleaning, joining an empty
    // path leaves a trailing slash on every value derived from it.
    let mut environment = HashMap::new();
    environment.insert("FOO".to_string(), "<batect.project_directory".to_string());
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::from([(
            "build-env".to_string(),
            container_with_environment(environment),
        )]),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    config
        .resolve_expressions_with(Path::new(""), &HashMap::new(), &HashMap::new(), |_| None)
        .unwrap();

    let resolved = &config.containers["build-env"].environment.as_ref().unwrap()["FOO"];
    assert!(
        !resolved.ends_with('/'),
        "batect.project_directory shouldn't have a trailing slash: {resolved}"
    );
}

#[test]
fn resolve_expressions_errors_if_project_directory_is_declared_in_config_variables() {
    let mut config_variables = HashMap::new();
    config_variables.insert(
        "batect.project_directory".to_string(),
        ConfigVariable {
            default: Some("/somewhere".to_string()),
            description: None,
        },
    );
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::new(),
        tasks: HashMap::new(),
        config_variables: Some(config_variables),
        forbid_telemetry: None,
    };

    let result = config.resolve_expressions_with(
        Path::new("/base"),
        &HashMap::new(),
        &HashMap::new(),
        |_| None,
    );
    assert!(result.is_err());
}

#[test]
fn resolve_expressions_errors_if_project_directory_is_given_as_a_cli_override() {
    let mut config = Config {
        project_name: "demo".to_string(),
        containers: HashMap::new(),
        tasks: HashMap::new(),
        config_variables: None,
        forbid_telemetry: None,
    };

    let overrides = HashMap::from([(
        "batect.project_directory".to_string(),
        "/hijacked".to_string(),
    )]);
    let result =
        config.resolve_expressions_with(Path::new("/base"), &HashMap::new(), &overrides, |_| None);
    assert!(result.is_err());
}
