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
use std::sync::Mutex;

fn resource(id: &str, labels: &[(&str, &str)], state: Option<&str>) -> LabelledResource {
    LabelledResource {
        id: id.to_string(),
        name: format!("name-of-{id}"),
        labels: labels
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        created: Some(1_000),
        state: state.map(str::to_string),
    }
}

/// A container of `shop`'s instance of `group`.
fn member(id: &str, group: &str, container: &str) -> LabelledResource {
    resource(
        id,
        &[
            (labels::PROJECT, "shop"),
            (labels::SERVICE, group),
            (labels::INSTANCE, labels::DEFAULT_INSTANCE),
            (labels::START_DIRECTORY, "/work/shop"),
            (labels::CONTAINER, container),
            (labels::ROLE, "service"),
        ],
        Some("running"),
    )
}

fn network(id: &str, group: &str) -> LabelledResource {
    resource(
        id,
        &[
            (labels::PROJECT, "shop"),
            (labels::SERVICE, group),
            (labels::INSTANCE, labels::DEFAULT_INSTANCE),
            (labels::START_DIRECTORY, "/work/shop"),
        ],
        None,
    )
}

/// Implements the four methods this module calls, returning whatever it
/// holds whatever the filter — so the module's own label check is what's
/// under test — and recording each removal in order.
#[derive(Default)]
struct FakeRuntime {
    containers: Vec<LabelledResource>,
    networks: Vec<LabelledResource>,
    removed: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ResourceInventory for FakeRuntime {
    async fn list_containers(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        Ok(self.containers.clone())
    }

    async fn list_networks(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        Ok(self.networks.clone())
    }

    async fn stop_and_remove_container(
        &self,
        container_id: &str,
        _stop_signal: Option<&str>,
        _stop_grace_period: Option<std::time::Duration>,
        stopped: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        if let Some(stopped) = stopped {
            let _ = stopped.send(());
        }
        self.removed.lock().unwrap().push(container_id.to_string());
        Ok(())
    }

    async fn remove_network(&self, name: &str) -> Result<()> {
        self.removed.lock().unwrap().push(name.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn instances_are_grouped_by_service_group_and_exclude_a_task_runs_resources() {
    // A task run's container whose image carries a service instance's
    // labels: Docker copies an image's labels onto the container for each
    // key the create doesn't set, and Ratect sets its `role`.
    let mut task_container = member("t1", "stack", "api");
    task_container
        .labels
        .insert(labels::RUN.to_string(), "run-id".to_string());
    task_container
        .labels
        .insert(labels::ROLE.to_string(), "task".to_string());
    let docker = FakeRuntime {
        containers: vec![
            member("c2", "stack", "db"),
            member("c1", "stack", "api"),
            member("c3", "tools", "cli"),
            task_container,
        ],
        networks: vec![network("n1", "stack")],
        ..Default::default()
    };

    let instances = find(&docker, "shop", None).await.unwrap();

    let summary: Vec<(String, Vec<String>, Vec<String>)> = instances
        .iter()
        .map(|instance| {
            (
                instance.key.group.clone(),
                instance.containers.iter().map(|c| c.id.clone()).collect(),
                instance.networks.iter().map(|n| n.id.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "stack".to_string(),
                vec!["c1".to_string(), "c2".to_string()],
                vec!["n1".to_string()]
            ),
            ("tools".to_string(), vec!["c3".to_string()], vec![]),
        ]
    );
    assert_eq!(
        instances[0].start_directories(),
        BTreeSet::from(["/work/shop"])
    );
}

/// Docker copies an image's labels onto the container for each key the
/// create doesn't set, so a member whose image was committed from a task
/// run's container carries a `run` label; its `role`, which Ratect always
/// sets, still says it is the instance's.
#[tokio::test]
async fn a_member_whose_image_carries_a_run_label_is_still_the_instances() {
    let mut inherited = member("c1", "stack", "api");
    inherited
        .labels
        .insert(labels::RUN.to_string(), "run-id".to_string());
    let docker = FakeRuntime {
        containers: vec![inherited],
        networks: vec![network("n1", "stack")],
        ..Default::default()
    };

    let key = InstanceKey::default_instance("shop", "stack");
    let instance = find_one(&docker, &key).await.unwrap().unwrap();
    assert_eq!(instance.containers.len(), 1);
}

#[tokio::test]
async fn another_projects_resources_are_never_part_of_an_instance() {
    let mut elsewhere = member("c9", "stack", "api");
    elsewhere
        .labels
        .insert(labels::PROJECT.to_string(), "other".to_string());
    let docker = FakeRuntime {
        containers: vec![elsewhere],
        ..Default::default()
    };

    let key = InstanceKey::default_instance("shop", "stack");
    assert_eq!(find_one(&docker, &key).await.unwrap(), None);
}

#[tokio::test]
async fn take_down_removes_containers_layer_by_layer_and_then_the_network() {
    let docker = FakeRuntime {
        containers: vec![
            member("db-id", "stack", "db"),
            member("api-id", "stack", "api"),
        ],
        networks: vec![network("net-id", "stack")],
        ..Default::default()
    };
    let instance = find_one(&docker, &InstanceKey::default_instance("shop", "stack"))
        .await
        .unwrap()
        .unwrap();
    let plan = StopPlan {
        layers: vec![vec!["api".to_string()], vec!["db".to_string()]],
        options: HashMap::new(),
    };
    let mut steps = Vec::new();

    let failures = take_down(&docker, &instance, &plan, |step| steps.push(step)).await;

    assert!(failures.is_empty());
    assert_eq!(
        *docker.removed.lock().unwrap(),
        ["api-id", "db-id", "net-id"]
    );
    assert_eq!(
        steps,
        [
            TakeDownStep::ContainerStopped("api".to_string()),
            TakeDownStep::ContainerRemoved("api".to_string()),
            TakeDownStep::ContainerStopped("db".to_string()),
            TakeDownStep::ContainerRemoved("db".to_string()),
            TakeDownStep::RemovingNetwork("name-of-net-id".to_string()),
            TakeDownStep::NetworkRemoved("name-of-net-id".to_string()),
        ]
    );
}

/// An instance another client is taking down at the same time, as the
/// daemon has it: a removal of a container the other client is already
/// removing comes back at once (Docker's 409, which counts as removed) while
/// the container stays listed, `removing`, for `linger` more looks. The
/// network can't be removed while a container on it is running or paused
/// (Docker's 403, "has active endpoints"), and can be while one has merely
/// stopped — a stop returns only once the container has. `held` more network
/// removals are refused as if something outside the instance, a task run
/// with `--use-network`, were still on it. And `newer`, another `up`'s next
/// instance under the same labels, appears on the second look.
struct ContendedRuntime {
    containers: Mutex<Vec<LabelledResource>>,
    networks: Mutex<Vec<LabelledResource>>,
    linger: Mutex<HashMap<String, usize>>,
    held: Mutex<usize>,
    newer: Mutex<Vec<LabelledResource>>,
    looks: Mutex<usize>,
}

impl ContendedRuntime {
    fn new(containers: Vec<LabelledResource>, linger: &[(&str, usize)]) -> Self {
        Self {
            containers: Mutex::new(containers),
            networks: Mutex::new(vec![network("net-id", "stack")]),
            linger: Mutex::new(
                linger
                    .iter()
                    .map(|(id, looks)| (id.to_string(), *looks))
                    .collect(),
            ),
            held: Mutex::new(0),
            newer: Mutex::new(Vec::new()),
            looks: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl ResourceInventory for ContendedRuntime {
    async fn list_containers(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        let mut looks = self.looks.lock().unwrap();
        *looks += 1;
        let mut containers = self.containers.lock().unwrap();
        if *looks == 2 {
            containers.append(&mut self.newer.lock().unwrap());
        }
        let listed = containers.clone();
        let mut linger = self.linger.lock().unwrap();
        for (id, remaining) in linger.iter_mut() {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                containers.retain(|container| &container.id != id);
            }
        }
        linger.retain(|_, remaining| *remaining > 0);
        Ok(listed)
    }

    async fn list_networks(
        &self,
        _labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>> {
        Ok(self.networks.lock().unwrap().clone())
    }

    async fn stop_and_remove_container(
        &self,
        container_id: &str,
        _stop_signal: Option<&str>,
        _stop_grace_period: Option<std::time::Duration>,
        stopped: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        if let Some(stopped) = stopped {
            let _ = stopped.send(());
        }
        if !self.linger.lock().unwrap().contains_key(container_id) {
            self.containers
                .lock()
                .unwrap()
                .retain(|container| container.id != container_id);
        }
        Ok(())
    }

    async fn remove_network(&self, id: &str) -> Result<()> {
        let mut held = self.held.lock().unwrap();
        let endpoint = self
            .containers
            .lock()
            .unwrap()
            .iter()
            .any(|container| matches!(container.state.as_deref(), Some("running" | "paused")));
        if endpoint || *held > 0 {
            *held = held.saturating_sub(1);
            anyhow::bail!("error while removing network: network {id} has active endpoints");
        }
        self.networks
            .lock()
            .unwrap()
            .retain(|network| network.id != id);
        Ok(())
    }
}

fn in_state(mut resource: LabelledResource, state: &str) -> LabelledResource {
    resource.state = Some(state.to_string());
    resource
}

async fn take_down_found(docker: &ContendedRuntime) -> (Vec<anyhow::Error>, Vec<TakeDownStep>) {
    let instance = find_one(docker, &InstanceKey::default_instance("shop", "stack"))
        .await
        .unwrap()
        .unwrap();
    let mut steps = Vec::new();
    let failures = take_down_until_gone(docker, &instance, &StopPlan::default(), |step| {
        steps.push(step)
    })
    .await;
    (failures, steps)
}

/// A container the other client has stopped and is still removing doesn't
/// hold the network, but the take-down waits until it has gone — and says
/// it was removed once.
#[tokio::test(start_paused = true)]
async fn a_take_down_waits_for_a_container_another_client_is_removing() {
    let docker = ContendedRuntime::new(
        vec![
            member("api-id", "stack", "api"),
            in_state(member("db-id", "stack", "db"), "removing"),
        ],
        &[("db-id", 3)],
    );

    let (failures, steps) = take_down_found(&docker).await;

    assert!(failures.is_empty(), "{failures:?}");
    assert!(docker.containers.lock().unwrap().is_empty());
    assert!(docker.networks.lock().unwrap().is_empty());
    let db_removed = steps
        .iter()
        .filter(|step| **step == TakeDownStep::ContainerRemoved("db".to_string()))
        .count();
    assert_eq!(db_removed, 1, "{steps:?}");
}

/// A network something else is still on is refused, and tried again until
/// it has left.
#[tokio::test(start_paused = true)]
async fn a_take_down_tries_the_network_again_until_nothing_else_is_on_it() {
    let docker = ContendedRuntime::new(vec![member("db-id", "stack", "db")], &[]);
    *docker.held.lock().unwrap() = 2;

    let (failures, _) = take_down_found(&docker).await;

    assert!(failures.is_empty(), "{failures:?}");
    assert!(docker.networks.lock().unwrap().is_empty());
}

/// What another `up` brings up under the same labels once this take-down has
/// begun is a newer instance, and is left alone.
#[tokio::test(start_paused = true)]
async fn a_take_down_leaves_a_newer_instance_alone() {
    let docker = ContendedRuntime::new(
        vec![in_state(member("db-id", "stack", "db"), "removing")],
        &[("db-id", 2)],
    );
    docker
        .newer
        .lock()
        .unwrap()
        .push(in_state(member("db-id-2", "stack", "db"), "created"));

    let (failures, _) = take_down_found(&docker).await;

    assert!(failures.is_empty(), "{failures:?}");
    let left: Vec<String> = docker
        .containers
        .lock()
        .unwrap()
        .iter()
        .map(|container| container.id.clone())
        .collect();
    assert_eq!(left, ["db-id-2"]);
}

#[tokio::test(start_paused = true)]
async fn a_take_down_that_keeps_failing_hands_back_its_last_failures() {
    let docker = ContendedRuntime::new(vec![member("db-id", "stack", "db")], &[]);
    *docker.held.lock().unwrap() = usize::MAX;

    let (failures, _) = take_down_found(&docker).await;

    assert_eq!(failures.len(), 1);
    assert!(format!("{:#}", failures[0]).contains("active endpoints"));
}

#[tokio::test(start_paused = true)]
async fn a_take_down_that_leaves_something_still_there_says_so() {
    let docker = ContendedRuntime::new(
        vec![in_state(member("db-id", "stack", "db"), "removing")],
        &[("db-id", usize::MAX)],
    );

    let (failures, _) = take_down_found(&docker).await;

    assert_eq!(failures.len(), 1);
    assert_eq!(
        format!("{:#}", failures[0]),
        "container name-of-db-id is still there"
    );
}
