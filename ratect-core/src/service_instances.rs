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

//! Finding and removing **service instances** by their labels — what
//! `ratect services list` and `services down` do, and what `services up`
//! takes an instance down with (decisions/0026).
//!
//! A service instance belongs to no process, so the daemon is its only
//! record: its containers and network carry the project, service group and
//! instance labels ([`crate::labels`]), and nothing else finds one — not
//! its Docker names, which are only the guard against a concurrent `up`
//! ([`crate::resource_names::ServiceNames`]), and not its start directory,
//! which is for [`Instance::start_directories`] to show. A resource is part
//! of an instance only if [`crate::labels::owned_by_service_instance`] says
//! so — for a container, by its `role` — so a task run's container is never
//! one, whatever service labels its image or its configured `labels` gave
//! it: `ratect resources` owns those, and leaves instances alone in turn.
//!
//! Like [`crate::resources`], this module prints nothing: removal reports
//! each step through a callback and hands its failures back, so each caller
//! words them.

use crate::docker::LabelledResource;
use crate::labels;
use crate::resources::ResourceInventory;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Which instance: a project's service group's, and which of its instances.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceKey {
    pub project: String,
    pub group: String,
    pub instance: String,
}

impl InstanceKey {
    /// The one instance a group has, until ratect#873 allows more.
    pub fn default_instance(project: &str, group: &str) -> Self {
        Self {
            project: project.to_string(),
            group: group.to_string(),
            instance: labels::DEFAULT_INSTANCE.to_string(),
        }
    }

    fn filters(&self) -> [(&str, Option<&str>); 3] {
        [
            (labels::PROJECT, Some(self.project.as_str())),
            (labels::SERVICE, Some(self.group.as_str())),
            (labels::INSTANCE, Some(self.instance.as_str())),
        ]
    }

    /// Whether `resource` carries this instance's labels and is a service
    /// instance's ([`labels::owned_by_service_instance`]) — checked here as
    /// well as by the daemon's filter, as [`crate::resources::find`] checks
    /// its own.
    fn owns(&self, resource: &LabelledResource) -> bool {
        let label = |key| resource.labels.get(key).map(String::as_str);
        labels::owned_by_service_instance(&resource.labels)
            && label(labels::PROJECT) == Some(self.project.as_str())
            && label(labels::SERVICE) == Some(self.group.as_str())
            && label(labels::INSTANCE) == Some(self.instance.as_str())
    }
}

/// One service instance as the daemon has it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub key: InstanceKey,
    /// Its containers, by their `container` label.
    pub containers: Vec<LabelledResource>,
    /// Its networks: one, unless a concurrent `up` on a daemon that takes
    /// two networks of one name has just made a second (decisions/0026).
    pub networks: Vec<LabelledResource>,
}

impl Instance {
    /// Each container's name in the configuration — its `container` label.
    pub fn container_name(resource: &LabelledResource) -> &str {
        resource
            .labels
            .get(labels::CONTAINER)
            .map_or(resource.name.as_str(), String::as_str)
    }

    /// The container for configured container `name`, if the instance has
    /// one.
    pub fn container(&self, name: &str) -> Option<&LabelledResource> {
        self.containers
            .iter()
            .find(|resource| Self::container_name(resource) == name)
    }

    /// Its containers and networks among `ids` alone — what of it one
    /// generation made — or `None` when none of them is.
    pub fn retaining(mut self, ids: &std::collections::HashSet<String>) -> Option<Self> {
        self.containers
            .retain(|resource| ids.contains(&resource.id));
        self.networks.retain(|resource| ids.contains(&resource.id));
        (!self.containers.is_empty() || !self.networks.is_empty()).then_some(self)
    }

    /// Every start directory its resources were labelled with: one, unless
    /// `up`s from two checkouts each created part of it.
    pub fn start_directories(&self) -> BTreeSet<&str> {
        self.containers
            .iter()
            .chain(&self.networks)
            .filter_map(|resource| resource.labels.get(labels::START_DIRECTORY))
            .map(String::as_str)
            .collect()
    }
}

/// Every instance `project` has on the daemon — only `group`'s, when given
/// — in group, then instance order. An instance with no containers left
/// still counts while its network is there.
pub async fn find<D: ResourceInventory + ?Sized>(
    docker: &D,
    project: &str,
    group: Option<&str>,
) -> Result<Vec<Instance>> {
    let filters = [
        (labels::PROJECT, Some(project)),
        (labels::SERVICE, group),
        (labels::INSTANCE, None),
    ];
    let (containers, networks) = futures::future::try_join(
        docker.list_containers(&filters),
        docker.list_networks(&filters),
    )
    .await?;
    let mut instances: BTreeMap<InstanceKey, Instance> = BTreeMap::new();
    for (resource, is_network) in containers
        .into_iter()
        .map(|resource| (resource, false))
        .chain(networks.into_iter().map(|resource| (resource, true)))
    {
        let (Some(group), Some(instance)) = (
            resource.labels.get(labels::SERVICE),
            resource.labels.get(labels::INSTANCE),
        ) else {
            continue;
        };
        let key = InstanceKey {
            project: project.to_string(),
            group: group.clone(),
            instance: instance.clone(),
        };
        if !key.owns(&resource) {
            continue;
        }
        let entry = instances.entry(key.clone()).or_insert_with(|| Instance {
            key,
            containers: Vec::new(),
            networks: Vec::new(),
        });
        if is_network {
            entry.networks.push(resource);
        } else {
            entry.containers.push(resource);
        }
    }
    let mut instances: Vec<Instance> = instances.into_values().collect();
    for instance in &mut instances {
        instance
            .containers
            .sort_by(|a, b| Instance::container_name(a).cmp(Instance::container_name(b)));
        instance
            .networks
            .sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    }
    Ok(instances)
}

/// The instance `key` names, as the daemon has it now: `None` when nothing
/// carries its labels.
pub async fn find_one<D: ResourceInventory + ?Sized>(
    docker: &D,
    key: &InstanceKey,
) -> Result<Option<Instance>> {
    let filters = key.filters();
    let (containers, networks) = futures::future::try_join(
        docker.list_containers(&filters),
        docker.list_networks(&filters),
    )
    .await?;
    let mut containers: Vec<LabelledResource> =
        containers.into_iter().filter(|r| key.owns(r)).collect();
    let mut networks: Vec<LabelledResource> =
        networks.into_iter().filter(|r| key.owns(r)).collect();
    if containers.is_empty() && networks.is_empty() {
        return Ok(None);
    }
    containers.sort_by(|a, b| Instance::container_name(a).cmp(Instance::container_name(b)));
    networks.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    Ok(Some(Instance {
        key: key.clone(),
        containers,
        networks,
    }))
}

/// How [`take_down`] stops an instance's containers.
#[derive(Debug, Clone, Default)]
pub struct StopPlan {
    /// Configured container names, in groups stopped one after another:
    /// each group's containers at once, and only once the group before has
    /// gone. A container named in no group is stopped with the first.
    /// Empty stops every container at once — what `services down` does for
    /// a group the configuration no longer has.
    pub layers: Vec<Vec<String>>,
    /// A container's `stop_signal` and `stop_grace_period`, by configured
    /// name.
    pub options: HashMap<String, (Option<String>, Option<std::time::Duration>)>,
}

/// One step of [`take_down`], as it happens.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TakeDownStep {
    ContainerStopped(String),
    ContainerRemoved(String),
    RemovingNetwork(String),
    NetworkRemoved(String),
}

/// Stops and removes every container of `instance` as `plan` orders them,
/// then removes its networks, reporting each step to `progress` as it
/// happens. A failure doesn't stop the rest: each is handed back, with the
/// networks attempted even after a container failed, since the daemon
/// refuses one with an endpoint left anyway. One already gone is no failure
/// ([`ResourceInventory`]'s own rule).
pub async fn take_down<D, F>(
    docker: &D,
    instance: &Instance,
    plan: &StopPlan,
    mut progress: F,
) -> Vec<anyhow::Error>
where
    D: ResourceInventory + ?Sized,
    F: FnMut(TakeDownStep),
{
    let mut failures = Vec::new();
    let mut remaining: Vec<&LabelledResource> = instance.containers.iter().collect();
    let mut layers: Vec<Vec<&LabelledResource>> = Vec::new();
    for layer in &plan.layers {
        let (this, rest): (Vec<&LabelledResource>, Vec<&LabelledResource>) =
            remaining.into_iter().partition(|resource| {
                layer
                    .iter()
                    .any(|n| n == Instance::container_name(resource))
            });
        layers.push(this);
        remaining = rest;
    }
    match layers.first_mut() {
        Some(first) => first.extend(remaining),
        None => layers.push(remaining),
    }
    for layer in layers {
        // Each removal reports its own stop as it happens, through a
        // channel, so a container's "stopped" isn't held back behind a
        // sibling's slow stop.
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TakeDownStep>();
        let removals = futures::future::join_all(layer.into_iter().map(|resource| {
            let name = Instance::container_name(resource).to_string();
            let (signal, grace) = plan.options.get(&name).cloned().unwrap_or_default();
            let sender = sender.clone();
            async move {
                let (stopped, on_stop) = tokio::sync::oneshot::channel();
                let forward = {
                    let sender = sender.clone();
                    let name = name.clone();
                    async move {
                        if on_stop.await.is_ok() {
                            let _ = sender.send(TakeDownStep::ContainerStopped(name));
                        }
                    }
                };
                let (removed, ()) = futures::future::join(
                    docker.stop_and_remove_container(
                        &resource.id,
                        signal.as_deref(),
                        grace,
                        Some(stopped),
                    ),
                    forward,
                )
                .await;
                if removed.is_ok() {
                    let _ = sender.send(TakeDownStep::ContainerRemoved(name));
                }
                removed
            }
        }));
        drop(sender);
        let mut removals = std::pin::pin!(removals);
        let results = loop {
            tokio::select! {
                Some(step) = receiver.recv() => progress(step),
                results = &mut removals => break results,
            }
        };
        while let Ok(step) = receiver.try_recv() {
            progress(step);
        }
        failures.extend(results.into_iter().filter_map(Result::err));
    }
    for network in &instance.networks {
        progress(TakeDownStep::RemovingNetwork(network.name.clone()));
        // By id: a second network of the same name is possible on an older
        // daemon, and the name alone would be ambiguous.
        match docker.remove_network(&network.id).await {
            Ok(()) => progress(TakeDownStep::NetworkRemoved(network.name.clone())),
            Err(error) => failures.push(error),
        }
    }
    failures
}

/// How many times [`take_down_until_gone`] looks at the instance again.
const TAKE_DOWN_ROUNDS: usize = 10;

/// How long [`take_down_until_gone`] waits before looking again.
const TAKE_DOWN_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

/// What of `instance` the daemon still has: each of its containers and
/// networks still listed under its labels, by id, in its current state, and
/// nothing else — `None` once all of it has gone. A resource created since
/// under the same labels belongs to a newer instance and is never included.
pub async fn find_remaining<D: ResourceInventory + ?Sized>(
    docker: &D,
    instance: &Instance,
) -> Result<Option<Instance>> {
    let ids: std::collections::HashSet<String> = instance
        .containers
        .iter()
        .chain(&instance.networks)
        .map(|resource| resource.id.clone())
        .collect();
    Ok(find_one(docker, &instance.key)
        .await?
        .and_then(|now| now.retaining(&ids)))
}

/// Takes `instance` down, as [`take_down`] does, until the daemon has none
/// of it — `services down`'s, and every `services up`'s, way of ending one.
/// Another `up` or `down` may be taking the same instance down at once: a
/// container it is removing counts as removed, but is waited for until it
/// has gone, and the network, which the daemon refuses to remove while a
/// container on it is running or paused, is tried again. Each look is at what
/// `instance` held ([`find_remaining`]), so a newer instance another `up`
/// brings up meanwhile is left alone. Each step is reported once. Hands back
/// the last look's failures — or, when nothing failed but something is still
/// there, that it is — and none once all of it has gone.
pub async fn take_down_until_gone<D, F>(
    docker: &D,
    instance: &Instance,
    plan: &StopPlan,
    mut progress: F,
) -> Vec<anyhow::Error>
where
    D: ResourceInventory + ?Sized,
    F: FnMut(TakeDownStep),
{
    let mut reported = std::collections::HashSet::new();
    let mut report = |step: TakeDownStep| {
        if reported.insert(step.clone()) {
            progress(step);
        }
    };
    let mut remaining = instance.clone();
    let mut round = 0;
    loop {
        round += 1;
        let failures = take_down(docker, &remaining, plan, &mut report).await;
        remaining = match find_remaining(docker, instance).await {
            Ok(Some(remaining)) => remaining,
            Ok(None) => return Vec::new(),
            Err(error) => return vec![error],
        };
        if round >= TAKE_DOWN_ROUNDS {
            if !failures.is_empty() {
                return failures;
            }
            let left: Vec<String> = remaining
                .containers
                .iter()
                .map(|resource| format!("container {}", resource.name))
                .chain(
                    remaining
                        .networks
                        .iter()
                        .map(|resource| format!("network {} ({})", resource.name, resource.id)),
                )
                .collect();
            let verb = if left.len() == 1 { "is" } else { "are" };
            return vec![anyhow::anyhow!(
                "{} {verb} still there",
                crate::engine::human_readable_list(&left, "and")
            )];
        }
        tokio::time::sleep(TAKE_DOWN_PAUSE).await;
    }
}

#[cfg(test)]
#[path = "service_instances_tests.rs"]
mod tests;
