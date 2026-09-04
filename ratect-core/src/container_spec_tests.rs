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
use crate::labels::{ContainerRole, RunLabels};
use std::collections::HashMap;

fn sample_container() -> Container {
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
        environment: Some(HashMap::from([("FOO".to_string(), "bar".to_string())])),
        run_as_current_user: None,
        additional_hostnames: Some(vec!["alias".to_string()]),
        additional_hosts: None,
        ports: None,
        working_directory: Some("/app".to_string()),
        command: Some("echo hi".to_string()),
        entrypoint: Some("/bin/sh".to_string()),
        labels: Some(HashMap::from([(
            "team".to_string(),
            "platform".to_string(),
        )])),
        capabilities_to_add: None,
        capabilities_to_drop: None,
        privileged: None,
        shm_size: None,
        devices: None,
        enable_init_process: None,
        log_driver: None,
        log_options: None,
        health_check: None,
        setup_commands: None,
    }
}

fn empty_run() -> TaskRun {
    TaskRun {
        container: "app".to_string(),
        command: None,
        environment: None,
        ports: None,
        working_directory: None,
        entrypoint: None,
    }
}

/// The property this module's own doc comment promises: a task-derived and
/// a dependency-derived spec for the *same* container must agree on
/// `shared` when neither overlay contributes anything — proving the two
/// call sites in `engine.rs` (the task's own container, and
/// `ensure_container_ready`'s dependency start) really do share one
/// assembly path now, rather than two that happen to agree today. Before
/// this candidate, there was no single function whose output both paths
/// could be compared against — each built its own `NetworkOptions`/
/// `ContainerOptions` by hand, so this property could only ever be
/// eyeballed from the two hand-written blocks, not asserted.
#[test]
fn shared_is_identical_between_an_empty_run_overlay_and_no_customise_overlay() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", Some("0.6.0"));
    let run = empty_run();

    let task_spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&run),
        image: "alpine:3.18",
        network: "ratect-run-id",
        interactive: true,
        additional_args: &["extra".to_string()],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    let dependency_spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Customise(None),
        image: "alpine:3.18",
        network: "ratect-run-id",
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Dependency,
        run_labels: &run_labels,
    });

    assert_eq!(task_spec.shared, dependency_spec.shared);

    // The fields deliberately kept *outside* `shared` had better actually
    // vary, or the split above would be proving nothing.
    assert_eq!(task_spec.role, ContainerRole::Task);
    assert_eq!(dependency_spec.role, ContainerRole::Dependency);
    assert!(task_spec.interactive);
    assert!(!dependency_spec.interactive);
    assert_eq!(task_spec.additional_args, vec!["extra".to_string()]);
    assert!(dependency_spec.additional_args.is_empty());
    // `role` is baked into `labels`' own values, so the two must differ
    // here too even though `shared` agrees on everything else.
    assert_ne!(task_spec.labels, dependency_spec.labels);
}

/// A non-empty `Run` overlay overrides the container's own `command`/
/// `working_directory`/`entrypoint`/`environment`/`ports` — exercising
/// every arm of `derive_spec`'s exhaustive `Overlay::Run` destructure.
#[test]
fn a_run_overlay_overrides_the_containers_own_fields() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);
    let run = TaskRun {
        container: "app".to_string(),
        command: Some("echo overridden".to_string()),
        environment: Some(HashMap::from([(
            "FOO".to_string(),
            "overridden".to_string(),
        )])),
        ports: None,
        working_directory: Some("/overridden".to_string()),
        entrypoint: Some("/overridden-entrypoint".to_string()),
    };

    let spec = derive_spec(ContainerSpecInputs {
        name: "app",
        container_config: &container_config,
        overlay: Overlay::Run(&run),
        image: "alpine:3.18",
        network: "ratect-run-id",
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Task,
        run_labels: &run_labels,
    });

    assert_eq!(spec.shared.command, Some("echo overridden".to_string()));
    assert_eq!(
        spec.shared.environment,
        Some(HashMap::from([(
            "FOO".to_string(),
            "overridden".to_string()
        )]))
    );
    assert_eq!(
        spec.shared.options.working_directory,
        Some("/overridden".to_string())
    );
    assert_eq!(
        spec.shared.options.entrypoint,
        Some("/overridden-entrypoint".to_string())
    );
}

/// A `Customise` overlay overrides `working_directory`/`environment`/
/// `ports`, but has no `command`/`entrypoint` field to override with at
/// all (matching Batect's own `TaskContainerCustomisation`) — so those two
/// always fall through to the container's own value regardless.
#[test]
fn a_customise_overlay_has_no_command_or_entrypoint_override() {
    let container_config = sample_container();
    let run_labels = RunLabels::new("demo", "task", "run-id", None);
    let customisation = TaskContainerCustomisation {
        environment: Some(HashMap::from([(
            "FOO".to_string(),
            "customised".to_string(),
        )])),
        ports: None,
        working_directory: Some("/customised".to_string()),
    };

    let spec = derive_spec(ContainerSpecInputs {
        name: "database",
        container_config: &container_config,
        overlay: Overlay::Customise(Some(&customisation)),
        image: "alpine:3.18",
        network: "ratect-run-id",
        interactive: false,
        additional_args: &[],
        user_mapping: None,
        volumes: None,
        term_var: None,
        proxy: None,
        publish_ports: true,
        role: ContainerRole::Dependency,
        run_labels: &run_labels,
    });

    // Overridden.
    assert_eq!(
        spec.shared.environment,
        Some(HashMap::from([(
            "FOO".to_string(),
            "customised".to_string()
        )]))
    );
    assert_eq!(
        spec.shared.options.working_directory,
        Some("/customised".to_string())
    );
    // Not overridden — the container's own values pass through untouched.
    assert_eq!(spec.shared.command, container_config.command);
    assert_eq!(spec.shared.options.entrypoint, container_config.entrypoint);
}
