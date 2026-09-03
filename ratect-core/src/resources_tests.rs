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

/// A container of this project's, created at `created`.
fn container(id: &str, created: i64) -> LabelledResource {
    resource(
        id,
        "nostalgic_hopper",
        &[
            (crate::labels::PROJECT, "demo"),
            (crate::labels::CONTAINER, id),
        ],
        created,
        Some("exited"),
    )
}

/// A network of this project's, created at `created`. Networks are told
/// apart from containers by having no state — see `LabelledResource`.
fn network(id: &str, created: i64) -> LabelledResource {
    resource(
        id,
        "ratect-xyz",
        &[(crate::labels::PROJECT, "demo")],
        created,
        None,
    )
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
impl ContainerRuntime for FakeRuntime {
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

    async fn stop_and_remove_container(&self, container_id: &str) -> Result<()> {
        self.remove(container_id)
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.remove(name)
    }

    async fn pull_image(&self, _image: &str) -> Result<()> {
        unimplemented!("resources never pulls an image")
    }

    async fn image_exists_locally(&self, _image: &str) -> Result<bool> {
        unimplemented!("resources never inspects an image")
    }

    async fn build_image(
        &self,
        _build_directory: &std::path::Path,
        _dockerfile: &str,
        _build_args: Option<&HashMap<String, String>>,
        _target: Option<&str>,
        _buildkit: Option<&crate::docker::BuildKitOptions>,
        _tag: &str,
        _force_pull: bool,
        _proxy_host_gateway: Option<crate::proxy::HostGateway>,
    ) -> Result<String> {
        unimplemented!("resources never builds an image")
    }

    async fn tag_image(&self, _image_id: &str, _tags: &[String]) -> Result<()> {
        unimplemented!("resources never tags an image")
    }

    async fn create_network(&self, _name: &str, _labels: &HashMap<String, String>) -> Result<()> {
        unimplemented!("resources never creates a network")
    }

    async fn network_exists(&self, _name: &str) -> Result<bool> {
        unimplemented!("resources never checks for a network")
    }

    async fn start_background_container(
        &self,
        _alias: &str,
        _image: &str,
        _command: Option<&str>,
        _volumes: Option<&Vec<String>>,
        _environment: Option<&HashMap<String, String>>,
        _network: &str,
        _user_mapping: Option<&crate::docker::UserMapping>,
        _network_options: &crate::docker::NetworkOptions,
        _health_check: Option<&crate::docker::HealthCheckOptions>,
        _container_options: &crate::docker::ContainerOptions,
    ) -> Result<String> {
        unimplemented!("resources never starts a container")
    }

    async fn wait_for_container_healthy(&self, _container_id: &str) -> Result<()> {
        unimplemented!("resources never waits on a container")
    }

    async fn exec_in_container(
        &self,
        _container_id: &str,
        _command: &str,
        _working_directory: Option<&str>,
        _environment: Option<&HashMap<String, String>>,
        _user_mapping: Option<&crate::docker::UserMapping>,
    ) -> Result<crate::docker::ExecResult> {
        unimplemented!("resources never execs in a container")
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_container(
        &self,
        _name: &str,
        _image: &str,
        _command: Option<&str>,
        _additional_args: &[String],
        _volumes: Option<&Vec<String>>,
        _environment: Option<&HashMap<String, String>>,
        _network: &str,
        _interactive: bool,
        _user_mapping: Option<&crate::docker::UserMapping>,
        _network_options: &crate::docker::NetworkOptions,
        _health_check: Option<&crate::docker::HealthCheckOptions>,
        _container_options: &crate::docker::ContainerOptions,
        _created: Option<tokio::sync::oneshot::Sender<String>>,
        _started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        unimplemented!("resources never runs a container")
    }

    async fn list_volumes(&self) -> Result<Vec<String>> {
        unimplemented!("resources never lists volumes")
    }

    async fn remove_volume(&self, _name: &str) -> Result<()> {
        unimplemented!("resources never removes a volume — that is `cache`")
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

/// `--older-than` is how a sweep avoids tearing down an in-flight run: a
/// task running right now is indistinguishable from a leftover by label, so
/// age is the only thing that tells them apart.
#[tokio::test]
async fn an_age_excludes_a_young_leftover_and_keeps_an_old_one() {
    let docker = FakeRuntime {
        containers: vec![container("young", 970), container("old", 100)],
        ..Default::default()
    };

    let all = find(&docker, Some("demo"), None, 1_000).await.unwrap();
    assert_eq!(all.len(), 2, "with no age, everything is a candidate");

    let old = find(
        &docker,
        Some("demo"),
        Some(std::time::Duration::from_secs(60)),
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(
        old.iter()
            .map(|l| l.resource.id.as_str())
            .collect::<Vec<_>>(),
        vec!["old"],
        "30 seconds old is younger than the 60 asked for"
    );
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
            resource("theirs", "some_other_tool", &[], 100, Some("running")),
        ],
        networks: vec![resource("their-net", "bridge", &[], 100, None)],
        ..Default::default()
    };

    let found = find(&docker, None, None, 1_000).await.unwrap();
    assert_eq!(
        found
            .iter()
            .map(|l| l.resource.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ours"]
    );
}

/// "Every project" means every project *Ratect* created — so the all-projects
/// case filters on the label existing, never on nothing. An unfiltered listing
/// is every container on the machine, which for a `clean` would mean stopping
/// and removing other tools' work.
#[tokio::test]
async fn every_project_still_filters_on_having_the_project_label() {
    let docker = FakeRuntime::default();

    find(&docker, None, None, 1_000).await.unwrap();

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

    find(&docker, Some("demo"), None, 1_000).await.unwrap();

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
    let leftovers = vec![
        Leftover::new(network("net", 100), 1_000),
        Leftover::new(container("one", 100), 1_000),
        Leftover::new(container("two", 100), 1_000),
    ];

    let removed = remove(&docker, &leftovers, |_, _| {}).await;

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
    let leftovers = vec![
        Leftover::new(container("stuck", 100), 1_000),
        Leftover::new(container("fine", 100), 1_000),
        Leftover::new(network("net", 100), 1_000),
    ];

    let mut reported: Vec<(String, bool)> = Vec::new();
    let removed = remove(&docker, &leftovers, |leftover, result| {
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
