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

fn labels() -> OwnershipLabels {
    OwnershipLabels::new("demo", "build", "run-id", Some("0.21.1"))
}

/// Every key is namespaced, and namespaced consistently — a stray one
/// would be invisible to the label filters that find these again.
#[test]
fn every_key_is_under_the_one_namespace() {
    for key in ALL {
        assert!(
            key.starts_with(&format!("{NAMESPACE}.")),
            "{key} should be under {NAMESPACE}"
        );
    }
}

#[test]
fn a_network_carries_the_run_identifying_labels() {
    let network = labels().for_network();
    assert_eq!(network[PROJECT], "demo");
    assert_eq!(network[TASK], "build");
    assert_eq!(network[RUN], "run-id");
    assert_eq!(network[VERSION], "0.21.1");
    // A network isn't a container: these would be meaningless on one.
    assert!(!network.contains_key(CONTAINER));
    assert!(!network.contains_key(ROLE));
}

#[test]
fn a_container_also_carries_its_config_name_and_role() {
    let container = labels().for_container("database", ContainerRole::Dependency, None);
    assert_eq!(container[CONTAINER], "database");
    assert_eq!(container[ROLE], "dependency");
    assert_eq!(container[RUN], "run-id");
    assert_eq!(
        labels().for_container("app", ContainerRole::Task, None)[ROLE],
        "task"
    );
}

#[test]
fn a_containers_own_configured_labels_are_kept_alongside() {
    let configured = HashMap::from([("com.example.team".to_string(), "platform".to_string())]);
    let container = labels().for_container("app", ContainerRole::Task, Some(&configured));
    assert_eq!(container["com.example.team"], "platform");
    assert_eq!(container[PROJECT], "demo");
}

/// See [`OwnershipLabels::for_container`]: a configuration that sets one of
/// these — however it came to — must not be able to make its own
/// containers unfindable.
#[test]
fn ratects_own_labels_win_over_a_configured_one_of_the_same_name() {
    let configured = HashMap::from([
        (RUN.to_string(), "not-the-real-run".to_string()),
        (CONTAINER.to_string(), "not-the-real-container".to_string()),
    ]);
    let container = labels().for_container("app", ContainerRole::Task, Some(&configured));
    assert_eq!(container[RUN], "run-id");
    assert_eq!(container[CONTAINER], "app");
}

/// Tests build engines with no binary version to report; the label is
/// omitted rather than invented.
#[test]
fn an_unknown_version_omits_that_label_rather_than_guessing() {
    let labels = OwnershipLabels::new("demo", "build", "run-id", None);
    assert!(!labels.for_network().contains_key(VERSION));
    assert!(!labels
        .for_container("app", ContainerRole::Task, None)
        .contains_key(VERSION));
}

fn service_labels() -> OwnershipLabels {
    OwnershipLabels::for_service_instance(
        "shop",
        "stack",
        DEFAULT_INSTANCE,
        "/work/shop",
        Some("0.14.0"),
    )
}

/// decisions/0026's table: a service instance's resources carry its group,
/// instance and start directory, and no `task` or `run` label.
#[test]
fn a_service_instances_network_and_containers_carry_its_own_labels_and_no_runs() {
    let network = service_labels().for_network();
    let container = service_labels().for_container("api", ContainerRole::Service, None);
    for labels in [&network, &container] {
        assert_eq!(labels[PROJECT], "shop");
        assert_eq!(labels[SERVICE], "stack");
        assert_eq!(labels[INSTANCE], "default");
        assert_eq!(labels[START_DIRECTORY], "/work/shop");
        assert_eq!(labels[VERSION], "0.14.0");
        assert!(!labels.contains_key(TASK) && !labels.contains_key(RUN));
    }
    assert_eq!(container[ROLE], "service");
    assert_eq!(container[CONTAINER], "api");
    assert!(!network.contains_key(ROLE) && !network.contains_key(CONTAINER));
}

#[test]
fn a_task_runs_labels_carry_no_service_instance_labels() {
    let container = labels().for_container("app", ContainerRole::Task, None);
    for key in [SERVICE, INSTANCE, START_DIRECTORY] {
        assert!(!container.contains_key(key), "{key}");
    }
}

/// A key Ratect doesn't write for a container's owner can come from its
/// configured `labels` or, since Docker copies an image's labels onto a
/// container for each key the create doesn't set, from its image. Neither
/// changes who owns it: its `role`, which Ratect always sets, decides.
#[test]
fn a_containers_role_says_who_owns_it_whatever_other_labels_it_carries() {
    let foreign: HashMap<String, String> = ALL
        .iter()
        .map(|key| (key.to_string(), "foreign".to_string()))
        .collect();
    let member = service_labels().for_container("api", ContainerRole::Service, Some(&foreign));
    assert!(owned_by_service_instance(&member));
    for role in [ContainerRole::Task, ContainerRole::Dependency] {
        let task_container = labels().for_container("app", role, Some(&foreign));
        assert_eq!(task_container[ROLE], role.as_str());
        assert!(!owned_by_service_instance(&task_container), "{role:?}");
    }
}

/// A network has no `role`, and takes no image's labels: a `service` label
/// and no `run` label make it an instance's.
#[test]
fn a_network_is_an_instances_with_a_service_label_and_no_run_label() {
    assert!(owned_by_service_instance(&service_labels().for_network()));
    assert!(!owned_by_service_instance(&labels().for_network()));
    let mut both = service_labels().for_network();
    both.insert(RUN.to_string(), "run-id".to_string());
    assert!(!owned_by_service_instance(&both));
}
