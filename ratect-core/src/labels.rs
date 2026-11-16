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

//! The Docker labels Ratect stamps on everything it creates, so that what's
//! left over from a previous run can be found afterwards — see
//! [ROADMAP.md](../../ROADMAP.md)'s orphaned-resource discovery entry.
//!
//! Runtime *ownership* metadata, in the shape Docker Compose's own
//! `com.docker.compose.*` labels have. Deliberately **not** OCI image
//! annotations: `org.opencontainers.image.*` is a fixed vocabulary about an
//! image's provenance (`source`, `revision`, `created`, `licenses`), with no
//! key that means "the task that started this container", and it doesn't
//! model runtime objects at all — Docker networks aren't OCI objects, so
//! half of what needs labelling here couldn't carry them regardless. OCI
//! annotations on the images a build produces are a separate, complementary
//! idea, and the project's own business rather than Ratect's.
//!
//! Both binaries stamp these. It's a divergence from Batect, which labels
//! nothing of its own, but a strictly additive one — see
//! [Differences from Batect](../../docs/differences-from-batect.md#runtime-behavior-gaps).
//!
//! [`OwnershipLabels`] is built once per task execution in
//! `engine.rs`'s `run_task_internal` and threaded down through
//! `ensure_container_ready`, so a task's containers and its network all agree
//! on one run id — generated there rather than alongside the network,
//! deliberately, since `--use-network` creates no network to take it from.
//! A **service instance** (decisions/0026) gets its own set instead, built
//! once per `services up`: no `task` or `run` label, and a service group,
//! an instance and a start directory in their place. Its labels are its
//! identity, as a task run's `run` label is a run's.
//! Three things to preserve when touching it: Ratect's own keys must win over a
//! container's configured `labels` on an exact collision (they're load-bearing
//! for cleanup — a config setting `eu.orican.ratect.run` would otherwise make
//! its own containers unfindable); a container's `role` is what says whether
//! a service instance or a task run owns it ([`owned_by_service_instance`]),
//! since a key Ratect doesn't write can come from the configuration or the
//! image; and the version label comes from the
//! *binary* (`TaskEngineSettings::ratect_version`, `env!("CARGO_PKG_VERSION")`
//! at each `main.rs`) rather than `ratect-core`'s own version, which isn't
//! what a user sees from `--version`.

use std::collections::HashMap;

/// The prefix every key below shares — reverse-DNS of `ratect.orican.eu`,
/// under the `orican.eu` domain the project already has; see
/// `decisions/0002` for why this rather than a new `.dev` domain (the docs
/// site launched at `ratect.apps.orican.eu` instead, which leaves the
/// namespace unchanged). Public so a caller can recognize Ratect's own
/// labels as a group without matching each key individually.
pub const NAMESPACE: &str = "eu.orican.ratect";

/// The project the resource belongs to — `Config::project_name`.
pub const PROJECT: &str = "eu.orican.ratect.project";
/// The task that created it.
pub const TASK: &str = "eu.orican.ratect.task";
/// The single task execution that created it: a v4 UUID drawn once per task
/// execution, so a network and the containers on it agree about which run
/// they belong to even when `--use-network` means no network was created.
pub const RUN: &str = "eu.orican.ratect.run";
/// Which container in the configuration this is (`build-env`, `database`),
/// as opposed to its Docker name, `<project>-<container>-<suffix>`, which
/// [`crate::resource_names`] makes for people to read and nothing parses.
pub const CONTAINER: &str = "eu.orican.ratect.container";
/// [`ContainerRole`] — whether this is the task's own container, one of its
/// dependencies, or a service instance's. Always set on a container, and
/// what [`owned_by_service_instance`] decides by.
pub const ROLE: &str = "eu.orican.ratect.role";
/// The Ratect version that created it, for when this label set itself
/// changes. The two binaries are on independent version lines, so the
/// version also says which one created the resource.
pub const VERSION: &str = "eu.orican.ratect.version";
/// The service group a service instance's container or network belongs to,
/// by its name in the configuration (decisions/0026).
pub const SERVICE: &str = "eu.orican.ratect.service";
/// Which instance of its service group it belongs to — always
/// [`DEFAULT_INSTANCE`] until a group can have more than one (ratect#873).
pub const INSTANCE: &str = "eu.orican.ratect.instance";
/// The directory of the configuration file the instance was brought up
/// from — for `services list` and the warning a second checkout's `up`
/// prints, never for finding an instance.
pub const START_DIRECTORY: &str = "eu.orican.ratect.start-directory";

/// The one instance a service group has until ratect#873 allows more.
/// Stamped, and part of the instance's Docker names, so that allowing more
/// changes neither.
pub const DEFAULT_INSTANCE: &str = "default";

/// Every label key Ratect sets, for a caller that needs to strip or
/// recognize them wholesale.
pub const ALL: &[&str] = &[
    PROJECT,
    TASK,
    RUN,
    CONTAINER,
    ROLE,
    VERSION,
    SERVICE,
    INSTANCE,
    START_DIRECTORY,
];

/// Whether `labels` say a service instance owns the resource, rather than a
/// task run. A container's `role` decides, since Ratect sets it on every
/// container it creates and nothing overrides it there, while a key Ratect
/// doesn't write for that owner can come from the configured `labels` or
/// the image: Docker copies an image's labels onto a container for each key
/// the create doesn't set, so an image committed from another Ratect
/// container can bring a `run` or `service` label with it. A network takes
/// no image's labels and no configured ones, and has no `role`, so it is an
/// instance's when it has a `service` label and no `run` label.
/// [`crate::resources::find`] and [`crate::service_instances::find`] both
/// ask this.
pub fn owned_by_service_instance(labels: &HashMap<String, String>) -> bool {
    match labels.get(ROLE) {
        Some(role) => role == ContainerRole::Service.as_str(),
        None => labels.contains_key(SERVICE) && !labels.contains_key(RUN),
    }
}

/// What a labelled container was to whatever created it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRole {
    /// The container the task itself runs in.
    Task,
    /// A dependency/sidecar started alongside it.
    Dependency,
    /// Any container of a service instance, member or not.
    Service,
}

impl ContainerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerRole::Task => "task",
            ContainerRole::Dependency => "dependency",
            ContainerRole::Service => "service",
        }
    }
}

/// What a set of [`OwnershipLabels`] says owns the resources.
#[derive(Debug, Clone)]
enum Owner {
    TaskRun {
        task: String,
        /// Unique per task execution, so a network and the containers on
        /// it agree about which run they belong to even when
        /// `--use-network` means no network was created at all. Not what
        /// names them: their Docker names carry a six-character suffix of
        /// their own ([`crate::resource_names`]).
        run_id: String,
    },
    ServiceInstance {
        group: String,
        instance: String,
        start_directory: String,
    },
}

/// The labels shared by every resource one task execution, or one service
/// instance, carries.
#[derive(Debug, Clone)]
pub struct OwnershipLabels {
    project: String,
    owner: Owner,
    /// The creating binary's own version. `None` only in tests, which have
    /// no binary version to speak of.
    version: Option<String>,
}

impl OwnershipLabels {
    /// A task run's.
    pub fn new(project: &str, task: &str, run_id: &str, version: Option<&str>) -> Self {
        Self {
            project: project.to_string(),
            owner: Owner::TaskRun {
                task: task.to_string(),
                run_id: run_id.to_string(),
            },
            version: version.map(str::to_string),
        }
    }

    /// A service instance's: `group` is its service group's name in the
    /// configuration, and `start_directory` the directory of the
    /// configuration file `services up` was given.
    pub fn for_service_instance(
        project: &str,
        group: &str,
        instance: &str,
        start_directory: &str,
        version: Option<&str>,
    ) -> Self {
        Self {
            project: project.to_string(),
            owner: Owner::ServiceInstance {
                group: group.to_string(),
                instance: instance.to_string(),
                start_directory: start_directory.to_string(),
            },
            version: version.map(str::to_string),
        }
    }

    /// The labels for a network created for this run or instance.
    pub fn for_network(&self) -> HashMap<String, String> {
        self.shared()
    }

    /// The labels for one container in this run or instance, merged over
    /// `configured`
    /// — the user's own `labels` for that container.
    ///
    /// Ratect's own keys win on an exact collision. They're load-bearing
    /// for cleanup: a configuration that set `eu.orican.ratect.run`, by
    /// accident or otherwise, would otherwise make a task run's containers
    /// unfindable by the thing meant to find them. A key this owner doesn't
    /// write is passed through like every other user label, since this is
    /// additive metadata, not a replacement for theirs; the `role` written
    /// here is what says who owns the container
    /// ([`owned_by_service_instance`]).
    pub fn for_container(
        &self,
        container: &str,
        role: ContainerRole,
        configured: Option<&HashMap<String, String>>,
    ) -> HashMap<String, String> {
        let mut labels: HashMap<String, String> = configured.cloned().unwrap_or_default();
        labels.extend(self.shared());
        labels.insert(CONTAINER.to_string(), container.to_string());
        labels.insert(ROLE.to_string(), role.as_str().to_string());
        labels
    }

    fn shared(&self) -> HashMap<String, String> {
        let mut labels = HashMap::from([(PROJECT.to_string(), self.project.clone())]);
        match &self.owner {
            Owner::TaskRun { task, run_id } => {
                labels.insert(TASK.to_string(), task.clone());
                labels.insert(RUN.to_string(), run_id.clone());
            }
            Owner::ServiceInstance {
                group,
                instance,
                start_directory,
            } => {
                labels.insert(SERVICE.to_string(), group.clone());
                labels.insert(INSTANCE.to_string(), instance.clone());
                labels.insert(START_DIRECTORY.to_string(), start_directory.clone());
            }
        }
        if let Some(version) = &self.version {
            labels.insert(VERSION.to_string(), version.clone());
        }
        labels
    }
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
