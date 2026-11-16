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
use crate::docker::LabelledResource;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

fn resource(
    id: &str,
    name: &str,
    labels: &[(&str, &str)],
    created: i64,
    state: Option<&str>,
) -> LabelledResource {
    LabelledResource {
        id: id.to_string(),
        name: name.to_string(),
        labels: labels
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        created: Some(created),
        state: state.map(str::to_string),
    }
}

/// A container of this project's task run `run`, created at `created`.
fn container_of(run: &str, id: &str, created: i64) -> LabelledResource {
    resource(
        id,
        "nostalgic_hopper",
        &[
            (crate::labels::PROJECT, "demo"),
            (crate::labels::RUN, run),
            (crate::labels::TASK, "check"),
            (crate::labels::CONTAINER, id),
        ],
        created,
        Some("exited"),
    )
}

/// A network of this project's task run `run`, created at `created`.
/// Networks are told apart from containers by having no state — see
/// `LabelledResource`.
fn network_of(run: &str, id: &str, created: i64) -> LabelledResource {
    resource(
        id,
        "ratect-xyz",
        &[(crate::labels::PROJECT, "demo"), (crate::labels::RUN, run)],
        created,
        None,
    )
}

/// A container that is the only resource of a task run of its own, named
/// for it.
fn container(id: &str, created: i64) -> LabelledResource {
    container_of(id, id, created)
}

/// A network that is the only resource of a task run of its own.
fn network(id: &str, created: i64) -> LabelledResource {
    network_of(id, id, created)
}

/// A faked lock read: each task run's state, dead where unlisted, and every
/// task run whose lock file was deleted.
#[derive(Default)]
struct FakeLocks {
    states: HashMap<String, RunState>,
    forgotten: Mutex<Vec<String>>,
}

impl FakeLocks {
    fn with(states: &[(&str, RunState)]) -> Self {
        Self {
            states: states
                .iter()
                .map(|(run, state)| (run.to_string(), *state))
                .collect(),
            ..Default::default()
        }
    }

    fn forgotten(&self) -> Vec<String> {
        self.forgotten.lock().unwrap().clone()
    }
}

impl RunStates for FakeLocks {
    fn state(&self, run: &str) -> RunState {
        self.states.get(run).copied().unwrap_or(RunState::Dead)
    }

    fn forget(&self, run: &str) -> std::io::Result<()> {
        self.forgotten.lock().unwrap().push(run.to_string());
        Ok(())
    }
}

/// Every selected resource's id, in order.
fn ids(selection: &Selection) -> Vec<&str> {
    selection
        .resources()
        .map(|leftover| leftover.resource.id.as_str())
        .collect()
}

/// A selection of every dead task run, at any age.
async fn find_dead(docker: &FakeRuntime, project: Option<&str>) -> Selection {
    find(docker, project, &FakeLocks::default(), None, false, 1_000)
        .await
        .unwrap()
}

/// Each of `resources` as a task run of its own, read dead.
fn dead_runs(resources: Vec<LabelledResource>) -> Vec<TaskRun> {
    resources
        .into_iter()
        .map(|resource| {
            let leftover = Leftover::new(resource, 1_000);
            TaskRun {
                run: leftover.run.clone(),
                task: leftover.task.clone(),
                state: RunState::Dead,
                age_seconds: leftover.age_seconds,
                resources: vec![leftover],
            }
        })
        .collect()
}

/// One recorded label filter, in the owned form a test can compare against —
/// the borrowed `&[(&str, Option<&str>)]` the trait takes can't outlive the
/// call that made it. Named (like `engine_tests.rs`'s `Captured*` aliases)
/// because the nesting is otherwise unreadable at the field.
type RecordedFilter = Vec<(String, Option<String>)>;

/// Implements the four methods `resources` actually calls, and nothing
/// else. Deliberately not `engine_tests.rs`'s fake: that one exists to
/// drive whole task runs, and its delays, failure injection and capture
/// maps are all noise for "did this list get filtered right". Two fakes
/// implementing one compiler-checked trait cannot drift.
#[derive(Default)]
struct FakeRuntime {
    containers: Vec<LabelledResource>,
    networks: Vec<LabelledResource>,
    /// Every label filter this fake was asked to list with, in call order —
    /// what proves a selection filtered on something rather than on nothing.
    filters: Mutex<Vec<RecordedFilter>>,
    /// The ids passed to a removal, in the order they were attempted.
    removed: Mutex<Vec<String>>,
    /// Ids whose removal fails.
    failing: HashSet<String>,
}

impl FakeRuntime {
    fn record(&self, labels: &[(&str, Option<&str>)]) {
        self.filters.lock().unwrap().push(
            labels
                .iter()
                .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
                .collect(),
        );
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.removed.lock().unwrap().push(id.to_string());
        if self.failing.contains(id) {
            anyhow::bail!("'{id}' is still in use");
        }
        Ok(())
    }

    fn filters(&self) -> Vec<RecordedFilter> {
        self.filters.lock().unwrap().clone()
    }

    fn removed(&self) -> Vec<String> {
        self.removed.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ResourceInventory for FakeRuntime {
    async fn list_containers(
        &self,
        labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        self.record(labels);
        Ok(self.containers.clone())
    }

    async fn list_networks(
        &self,
        labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        self.record(labels);
        Ok(self.networks.clone())
    }

    async fn stop_and_remove_container(
        &self,
        container_id: &str,
        _stop_signal: Option<&str>,
        _stop_grace_period: Option<std::time::Duration>,
        _stopped: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        self.remove(container_id)
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.remove(name)
    }
}

/// A container is described by its *configured* name, not Docker's randomly
/// generated one, which is the whole reason the label exists.
#[test]
fn a_leftover_is_described_in_the_terms_the_config_uses() {
    let container = Leftover::new(
        resource(
            "abc",
            "nostalgic_hopper",
            &[
                (crate::labels::CONTAINER, "database"),
                (crate::labels::TASK, "check"),
            ],
            1_000,
            Some("exited"),
        ),
        2_000,
    );
    assert_eq!(container.describe(), "container database (exited)");
    assert_eq!(container.task, "check");
    assert_eq!(container.age_seconds, 1_000);
    assert!(!container.is_network);

    let network = Leftover::new(resource("def", "ratect-xyz", &[], 1_000, None), 2_000);
    assert_eq!(network.describe(), "network ratect-xyz");
    assert!(network.is_network);
}

/// A resource from a Ratect old enough not to have set every label should
/// still be listable — reporting is exactly when you don't want a panic.
#[test]
fn a_leftover_missing_labels_reads_as_unknown_rather_than_failing() {
    let leftover = Leftover::new(
        resource("abc", "some_name", &[], 1_000, Some("running")),
        2_000,
    );
    assert_eq!(leftover.task, "unknown");
    assert_eq!(leftover.run, "unknown");
    // Falls back to Docker's own name when there's no container label.
    assert_eq!(leftover.describe(), "container some_name (running)");
}

/// `--older-than` measures a task run's age, its oldest resource's: a task
/// run with one old resource and one young one is old, and is selected whole.
#[tokio::test]
async fn an_age_compares_a_task_runs_oldest_resource() {
    let docker = FakeRuntime {
        containers: vec![
            container("young", 970),
            container_of("mixed", "mixed-young", 990),
            container_of("mixed", "mixed-old", 100),
        ],
        ..Default::default()
    };

    let all = find_dead(&docker, Some("demo")).await;
    assert_eq!(
        all.selected.len(),
        2,
        "with no age, every dead run is selected"
    );

    let old = find(
        &docker,
        Some("demo"),
        &FakeLocks::default(),
        Some(std::time::Duration::from_secs(60)),
        false,
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(
        ids(&old),
        vec!["mixed-young", "mixed-old"],
        "30 seconds old is younger than the 60 asked for; the mixed run's oldest is 900"
    );
    assert_eq!(old.selected[0].age_seconds, 900);
    assert_eq!(
        (old.alive, old.unknown),
        (0, 0),
        "too young is not left out for its state"
    );
}

/// decisions/0025's table: dead is selected by default; alive never, with
/// `all` or without; unknown only with `all`. Each run left out for its
/// state is counted under that state.
#[tokio::test]
async fn each_state_is_selected_as_the_table_says() {
    let docker = FakeRuntime {
        containers: vec![
            container("dead", 100),
            container("alive", 100),
            container("unknown", 100),
        ],
        ..Default::default()
    };
    let locks = FakeLocks::with(&[("alive", RunState::Alive), ("unknown", RunState::Unknown)]);

    let default = find(&docker, Some("demo"), &locks, None, false, 1_000)
        .await
        .unwrap();
    assert_eq!(ids(&default), vec!["dead"]);
    assert_eq!((default.alive, default.unknown), (1, 1));

    let all = find(&docker, Some("demo"), &locks, None, true, 1_000)
        .await
        .unwrap();
    assert_eq!(ids(&all), vec!["dead", "unknown"]);
    assert_eq!((all.alive, all.unknown), (1, 0));
    assert_eq!(all.selected[1].state, RunState::Unknown);

    let old_enough = find(
        &docker,
        Some("demo"),
        &locks,
        Some(std::time::Duration::from_secs(60)),
        true,
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(
        ids(&old_enough),
        vec!["dead", "unknown"],
        "--older-than composes with --all"
    );
}

/// A task run is selected or skipped whole: its containers and its network
/// share its state, and its network comes after its containers.
#[tokio::test]
async fn a_task_runs_containers_and_network_go_together() {
    let docker = FakeRuntime {
        containers: vec![
            container_of("kept", "kept-task", 100),
            container_of("kept", "kept-dependency", 100),
            container_of("live", "live-task", 100),
        ],
        networks: vec![
            network_of("kept", "kept-net", 100),
            network_of("live", "live-net", 100),
        ],
        ..Default::default()
    };
    let locks = FakeLocks::with(&[("live", RunState::Alive)]);

    let found = find(&docker, Some("demo"), &locks, None, true, 1_000)
        .await
        .unwrap();
    assert_eq!(
        ids(&found),
        vec!["kept-task", "kept-dependency", "kept-net"]
    );
    assert_eq!(found.selected.len(), 1);
    assert_eq!(found.selected[0].task, "check");
    assert_eq!(
        found.alive, 1,
        "one task run, however many resources it has"
    );
}

/// What has the project label and no `run` label is not a task run: never
/// selected, and counted nowhere.
#[tokio::test]
async fn a_resource_with_no_run_label_is_never_selected_or_counted() {
    let docker = FakeRuntime {
        containers: vec![resource(
            "service",
            "demo-web",
            &[(crate::labels::PROJECT, "demo")],
            100,
            Some("running"),
        )],
        networks: vec![resource(
            "service-net",
            "demo",
            &[(crate::labels::PROJECT, "demo")],
            100,
            None,
        )],
        ..Default::default()
    };
    let locks = FakeLocks::with(&[]);

    for all in [false, true] {
        let found = find(&docker, Some("demo"), &locks, None, all, 1_000)
            .await
            .unwrap();
        assert!(found.selected.is_empty());
        assert_eq!((found.alive, found.unknown), (0, 0));
    }
}

/// A service instance, with the labels `services up` gives it, is never
/// selected or counted, running or exited, under every combination of
/// `resources`' flags (decisions/0026's Decision 5).
#[tokio::test]
async fn a_service_instance_is_never_selected_whatever_its_state_or_the_flags() {
    let labels = crate::labels::OwnershipLabels::for_service_instance(
        "demo",
        "stack",
        crate::labels::DEFAULT_INSTANCE,
        "/work/demo",
        Some("0.14.0"),
    );
    let member = |id: &str, state: &str| LabelledResource {
        id: id.to_string(),
        name: format!("demo..stack..default..{id}"),
        labels: labels.for_container(id, crate::labels::ContainerRole::Service, None),
        created: Some(100),
        state: Some(state.to_string()),
    };
    let docker = FakeRuntime {
        containers: vec![member("api", "running"), member("db", "exited")],
        networks: vec![LabelledResource {
            id: "net".to_string(),
            name: "demo..stack..default".to_string(),
            labels: labels.for_network(),
            created: Some(100),
            state: None,
        }],
        ..Default::default()
    };
    let locks = FakeLocks::with(&[]);

    for project in [Some("demo"), None] {
        for older_than in [None, Some(std::time::Duration::from_secs(1))] {
            for all in [false, true] {
                let found = find(&docker, project, &locks, older_than, all, 1_000_000)
                    .await
                    .unwrap();
                assert!(
                    found.selected.is_empty(),
                    "{project:?} {older_than:?} {all}"
                );
                assert_eq!((found.alive, found.unknown), (0, 0));
            }
        }
    }
}

/// Everything `find` returns is a removal candidate, and the cost of a wrong
/// one is someone else's container — so the project label is re-checked on
/// this side, whatever the daemon was asked for and whatever it returned.
#[tokio::test]
async fn a_resource_without_the_project_label_is_never_a_candidate() {
    let docker = FakeRuntime {
        containers: vec![
            container("ours", 100),
            // No `PROJECT` label at all: something else's, or a daemon-side
            // filter that didn't do what was asked.
            resource(
                "theirs",
                "some_other_tool",
                &[(crate::labels::RUN, "theirs")],
                100,
                Some("running"),
            ),
        ],
        networks: vec![resource("their-net", "bridge", &[], 100, None)],
        ..Default::default()
    };

    let found = find_dead(&docker, None).await;
    assert_eq!(ids(&found), vec!["ours"]);
}

/// "Every project" means every project *Ratect* created — so the all-projects
/// case filters on the label existing, never on nothing. An unfiltered listing
/// is every container on the machine, which for a `clean` would mean stopping
/// and removing other tools' work.
#[tokio::test]
async fn every_project_still_filters_on_having_the_project_label() {
    let docker = FakeRuntime::default();

    find_dead(&docker, None).await;

    for filter in docker.filters() {
        assert_eq!(
            filter,
            vec![(crate::labels::PROJECT.to_string(), None)],
            "a key-existence filter, not an empty one"
        );
    }
    assert_eq!(docker.filters().len(), 2, "containers and networks");
}

/// A named project narrows to that project's own label value.
#[tokio::test]
async fn a_named_project_filters_on_its_own_label_value() {
    let docker = FakeRuntime::default();

    find_dead(&docker, Some("demo")).await;

    for filter in docker.filters() {
        assert_eq!(
            filter,
            vec![(crate::labels::PROJECT.to_string(), Some("demo".to_string()))]
        );
    }
}

/// A network still holding an endpoint can't be removed, so the reverse
/// order fails on every task that had one. The order is `remove`'s to get
/// right, not the caller's — which is why the leftovers here are handed over
/// networks-first.
#[tokio::test]
async fn containers_are_removed_before_networks() {
    let docker = FakeRuntime::default();
    let runs = dead_runs(vec![
        network("net", 100),
        container("one", 100),
        container("two", 100),
    ]);

    let removed = remove(&docker, &runs, &FakeLocks::default(), |_, _| {}).await;

    assert_eq!(removed, 3);
    let attempted = docker.removed();
    assert_eq!(
        attempted.last().map(String::as_str),
        Some("net"),
        "the network goes last: {attempted:?}"
    );
    assert_eq!(attempted.len(), 3);
}

/// A resource someone else removed in the meantime, or one still in use,
/// shouldn't leave the remaining leftovers behind too.
#[tokio::test]
async fn one_failure_does_not_abandon_the_rest() {
    let docker = FakeRuntime {
        failing: HashSet::from(["stuck".to_string()]),
        ..Default::default()
    };
    let runs = dead_runs(vec![
        container("stuck", 100),
        container("fine", 100),
        network("net", 100),
    ]);

    let mut reported: Vec<(String, bool)> = Vec::new();
    let removed = remove(&docker, &runs, &FakeLocks::default(), |leftover, result| {
        reported.push((leftover.resource.id.clone(), result.is_ok()));
    })
    .await;

    assert_eq!(removed, 2, "the count reflects what actually went");
    assert_eq!(
        docker.removed(),
        vec!["stuck", "fine", "net"],
        "every one was attempted"
    );
    assert_eq!(
        reported,
        vec![
            ("stuck".to_string(), false),
            ("fine".to_string(), true),
            ("net".to_string(), true)
        ],
        "each outcome reaches the caller, which is what decides how it reads"
    );
}

/// Decision 4: a dead task run's lock file is deleted once every one of its
/// resources is removed — not one with a refused removal, and not an unknown
/// one's, which may be another kernel's live task run's.
#[tokio::test]
async fn a_dead_task_runs_lock_file_goes_only_once_all_of_it_has() {
    let docker = FakeRuntime {
        failing: HashSet::from(["stuck-net".to_string()]),
        ..Default::default()
    };
    let mut runs = dead_runs(vec![container("gone", 100), container("unknown", 100)]);
    runs[1].state = RunState::Unknown;
    runs.extend(
        dead_runs(vec![container_of("stuck", "stuck-task", 100)])
            .into_iter()
            .map(|mut run| {
                run.resources
                    .push(Leftover::new(network_of("stuck", "stuck-net", 100), 1_000));
                run
            }),
    );
    let locks = FakeLocks::default();

    remove(&docker, &runs, &locks, |_, _| {}).await;

    assert_eq!(locks.forgotten(), vec!["gone"]);
}

/// Docker copies an image's labels onto the container for each key the
/// create doesn't set, so a service member whose image was committed from a
/// task run's container carries that run's `run` label. Its `role`, which
/// Ratect always sets, keeps it out of the selection.
#[tokio::test]
async fn a_service_member_whose_image_carries_a_run_label_is_never_selected() {
    let labels = crate::labels::OwnershipLabels::for_service_instance(
        "demo",
        "stack",
        crate::labels::DEFAULT_INSTANCE,
        "/work/demo",
        Some("0.14.0"),
    );
    let mut member_labels =
        labels.for_container("api", crate::labels::ContainerRole::Service, None);
    member_labels.insert(crate::labels::RUN.to_string(), "old-run".to_string());
    member_labels.insert(crate::labels::TASK.to_string(), "build".to_string());
    let docker = FakeRuntime {
        containers: vec![LabelledResource {
            id: "api".to_string(),
            name: "demo..stack..default..api".to_string(),
            labels: member_labels,
            created: Some(100),
            state: Some("running".to_string()),
        }],
        ..Default::default()
    };
    let locks = FakeLocks::with(&[]);

    let found = find(&docker, Some("demo"), &locks, None, true, 1_000_000)
        .await
        .unwrap();
    assert!(found.selected.is_empty());
    assert_eq!((found.alive, found.unknown), (0, 0));
}

/// The other way round: a task run's container whose image or configured
/// `labels` gave it a service instance's labels is still selected with its
/// run.
#[tokio::test]
async fn a_task_containers_service_labels_from_elsewhere_leave_it_its_runs() {
    let labels = crate::labels::OwnershipLabels::new("demo", "build", "dead-run", Some("0.14.0"));
    let service_labels = HashMap::from([
        (crate::labels::SERVICE.to_string(), "stack".to_string()),
        (crate::labels::INSTANCE.to_string(), "default".to_string()),
    ]);
    let docker = FakeRuntime {
        containers: vec![LabelledResource {
            id: "app".to_string(),
            name: "demo-app-abcdef".to_string(),
            labels: labels.for_container(
                "app",
                crate::labels::ContainerRole::Task,
                Some(&service_labels),
            ),
            created: Some(100),
            state: Some("exited".to_string()),
        }],
        ..Default::default()
    };
    let locks = FakeLocks::with(&[]);

    let found = find(&docker, Some("demo"), &locks, None, true, 1_000_000)
        .await
        .unwrap();
    let selected: Vec<&str> = found
        .selected
        .iter()
        .flat_map(|run| {
            run.resources
                .iter()
                .map(|leftover| leftover.resource.id.as_str())
        })
        .collect();
    assert_eq!(selected, ["app"]);
}
