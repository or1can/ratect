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

use std::collections::HashMap;

/// The prefix every key below shares — reverse-DNS of `ratect.orican.eu`,
/// the project's own (planned) home; see `ROADMAP.md` for why this rather
/// than a new `.dev` domain. Public so a caller can recognize Ratect's own
/// labels as a group without matching each key individually.
pub const NAMESPACE: &str = "eu.orican.ratect";

/// The project the resource belongs to — `Config::project_name`.
pub const PROJECT: &str = "eu.orican.ratect.project";
/// The task that created it.
pub const TASK: &str = "eu.orican.ratect.task";
/// The single task execution that created it — see [`RunLabels::run_id`].
pub const RUN: &str = "eu.orican.ratect.run";
/// Which container in the configuration this is (`build-env`, `database`),
/// as opposed to Docker's own randomly generated container name.
pub const CONTAINER: &str = "eu.orican.ratect.container";
/// [`ContainerRole`] — whether this is the task's own container or one of
/// its dependencies.
pub const ROLE: &str = "eu.orican.ratect.role";
/// The Ratect version that created it, for when this label set itself
/// changes. The two binaries are on independent version lines, so the
/// version also says which one created the resource.
pub const VERSION: &str = "eu.orican.ratect.version";

/// Every label key Ratect sets, for a caller that needs to strip or
/// recognize them wholesale.
pub const ALL: &[&str] = &[PROJECT, TASK, RUN, CONTAINER, ROLE, VERSION];

/// What a labelled container was to the task that created it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRole {
    /// The container the task itself runs in.
    Task,
    /// A dependency/sidecar started alongside it.
    Dependency,
}

impl ContainerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerRole::Task => "task",
            ContainerRole::Dependency => "dependency",
        }
    }
}

/// The labels shared by every resource one task execution creates.
#[derive(Debug, Clone)]
pub struct RunLabels {
    project: String,
    task: String,
    /// Unique per task execution — the same value the per-task network is
    /// named after, reused rather than minting a second id, so a network
    /// and the containers that joined it agree about which run they belong
    /// to even when `--use-network` means no network was created at all.
    run_id: String,
    /// The creating binary's own version. `None` only in tests, which have
    /// no binary version to speak of.
    version: Option<String>,
}

impl RunLabels {
    pub fn new(project: &str, task: &str, run_id: &str, version: Option<&str>) -> Self {
        Self {
            project: project.to_string(),
            task: task.to_string(),
            run_id: run_id.to_string(),
            version: version.map(str::to_string),
        }
    }

    /// The labels for a network created for this run.
    pub fn for_network(&self) -> HashMap<String, String> {
        self.shared()
    }

    /// The labels for one container in this run, merged over `configured`
    /// — the user's own `labels` for that container.
    ///
    /// Ratect's own keys win on an exact collision. They're load-bearing
    /// for cleanup: a configuration that set `eu.orican.ratect.run`, by
    /// accident or otherwise, would otherwise make its own containers
    /// unfindable by the thing meant to find them. Every other user label
    /// is passed through untouched, since this is additive metadata, not a
    /// replacement for theirs.
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
        let mut labels = HashMap::from([
            (PROJECT.to_string(), self.project.clone()),
            (TASK.to_string(), self.task.clone()),
            (RUN.to_string(), self.run_id.clone()),
        ]);
        if let Some(version) = &self.version {
            labels.insert(VERSION.to_string(), version.clone());
        }
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> RunLabels {
        RunLabels::new("demo", "build", "run-id", Some("0.21.1"))
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

    /// See [`RunLabels::for_container`]: a configuration that sets one of
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
        let labels = RunLabels::new("demo", "build", "run-id", None);
        assert!(!labels.for_network().contains_key(VERSION));
        assert!(!labels
            .for_container("app", ContainerRole::Task, None)
            .contains_key(VERSION));
    }
}
