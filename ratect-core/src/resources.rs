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

//! What previous runs left behind, and how it is removed — the selection and
//! removal behind `ratect resources list` / `ratect resources clean`, found by
//! the ownership labels in [`crate::labels`].
//!
//! Leftovers happen after a crash, a `kill -9`, a run that used
//! `--no-cleanup`, or a cleanup that itself failed. Labels alone can't tell
//! one from a task run still going: until it finishes, it carries exactly the
//! same labels. What tells them apart is the **task run lock**
//! ([`crate::run_lock`], decisions/0025): each task run that can take one
//! holds it for as long as its process drives it, and [`find`] reads it as a
//! [`RunState`] — **dead**, selected by default; **alive**, never selected;
//! **unknown**, selected only when asked for all of them. Container state was
//! tried first and fails both ways (0025's Context): a task run that is
//! starting has a task container that isn't running, and one draining its
//! output into a slow reader can have one that has exited.
//!
//! **A task run is the unit.** The containers and network sharing one `run`
//! label value are selected or skipped together, and `older_than` measures
//! the task run's age, its oldest resource's. What carries the project label
//! and no `run` label is not a task run (a service instance, decisions/0026):
//! it is never selected, and counted nowhere.
//!
//! **This module is where the rules live; it prints nothing.** It was in
//! `ratect/src/main.rs` until 0.6.0, bound to the concrete `DockerClient`
//! even though every call it makes is a [`ResourceInventory`] method — so
//! leftover selection, the containers-before-networks ordering rule and
//! partial-failure behaviour were provable only against a live daemon. The
//! seam existed and was simply not used. Four things follow from that
//! history and should survive:
//!
//! - **Selection takes `now` rather than reading the clock**, so an age test
//!   is a table, not a sleep.
//! - **Selection takes the lock read as a [`RunStates`]**, so the three
//!   states are a table too, with no real lock.
//! - **Removal reports through a callback**, so `clean` still prints as each
//!   resource goes rather than falling silent on a slow daemon, without this
//!   module owning any wording. Errors are handed back rather than logged
//!   here: the caller knows what its own verbs are called.
//! - **Nothing without the project label is ever a candidate**, however the
//!   daemon filtered. That check is deliberately duplicated on this side —
//!   see [`find`].

use crate::docker::LabelledResource;
pub use crate::run_lock::RunState;
use anyhow::Result;
use std::collections::BTreeMap;

/// The container/network inventory operations `find`/`remove` need — a
/// narrower contract than [`crate::docker::ContainerRuntime`]'s full 16
/// methods, which this module doesn't otherwise touch. `ContainerRuntime`
/// requires this as a supertrait, so `DockerClient` implements both without
/// restating any method.
#[async_trait::async_trait]
pub trait ResourceInventory {
    /// Containers carrying every one of `labels`, whether running or not —
    /// a leftover has usually exited, so listing only running ones would
    /// miss most of what's being looked for.
    ///
    /// Each entry is a key and an optional value: `Some` matches that exact
    /// value, `None` matches merely *having* the key, which is Docker's own
    /// `label=key` filter form. The `None` form is what "every project"
    /// means — every project *Ratect* created, not every container on the
    /// machine.
    ///
    /// An empty slice therefore means no filter at all, i.e. everything on
    /// the daemon. That is almost never what a caller wants; anything that
    /// might remove what it finds should pass at least a key-existence
    /// filter.
    ///
    /// Filtering happens daemon-side rather than by listing everything and
    /// matching here: on a machine with thousands of containers that's the
    /// difference between one cheap query and a large response, and Docker
    /// implements exactly this filter natively.
    async fn list_containers(
        &self,
        labels: &[(&str, Option<&str>)],
    ) -> Result<Vec<LabelledResource>>;

    /// Networks carrying every one of `labels` — the counterpart of
    /// [`list_containers`](Self::list_containers), with the same warning
    /// about an empty slice. Docker's own built-in `bridge`/`host`/`none`
    /// networks carry no labels at all, so any key-existence filter
    /// excludes them; an unfiltered call does not.
    async fn list_networks(&self, labels: &[(&str, Option<&str>)])
        -> Result<Vec<LabelledResource>>;

    /// Removes network `name`. One already gone is no failure: there is
    /// nothing left to remove (decisions/0016).
    async fn remove_network(&self, name: &str) -> Result<()>;

    /// Stops and removes `container_id`: any container a run created, a
    /// task's own or a dependency's, started or not, or a leftover
    /// `ratect resources clean` found.
    /// `stop_signal`/`stop_grace_period` override Docker's own default stop
    /// signal/timeout — `None` for either leaves that half of Docker's own
    /// default behavior alone (see `config::Container::stop_signal`/
    /// `stop_grace_period`, ratect#112).
    ///
    /// `stopped`, given, is signaled once the stop has succeeded — the
    /// container is stopped, by this request or already, or is already gone
    /// altogether — and before the removal is asked for, never if the stop
    /// fails, so cleanup can say a container stopped while its removal is
    /// still under way. A container already gone is no failure: there is
    /// nothing left to remove (decisions/0016).
    async fn stop_and_remove_container(
        &self,
        container_id: &str,
        stop_signal: Option<&str>,
        stop_grace_period: Option<std::time::Duration>,
        stopped: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()>;
}

/// One leftover, with the labels already pulled out of the map — reporting
/// reads them several times each, and a resource missing one (not Ratect's, or
/// from a version that didn't set it) should read as unknown rather than
/// panic.
pub struct Leftover {
    pub resource: LabelledResource,
    pub task: String,
    pub run: String,
    pub age_seconds: i64,
    pub is_network: bool,
}

impl Leftover {
    /// `now` is seconds since the Unix epoch, passed in rather than read here
    /// so that age is a function of its inputs.
    pub fn new(resource: LabelledResource, now: i64) -> Self {
        let label = |key: &str| {
            resource
                .labels
                .get(key)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string())
        };
        Self {
            task: label(crate::labels::TASK),
            run: label(crate::labels::RUN),
            age_seconds: resource.created.map(|created| now - created).unwrap_or(0),
            // Only a container has a state; see `LabelledResource`.
            is_network: resource.state.is_none(),
            resource,
        }
    }

    /// What this is, in the terms the configuration uses — a container's own
    /// Docker name carries its project and run suffix as well, which is
    /// noise when recognizing it.
    pub fn describe(&self) -> String {
        if self.is_network {
            return format!("network {}", self.resource.name);
        }
        let container = self
            .resource
            .labels
            .get(crate::labels::CONTAINER)
            .cloned()
            .unwrap_or_else(|| self.resource.name.clone());
        match self.resource.state.as_deref() {
            Some(state) => format!("container {container} ({state})"),
            None => format!("container {container}"),
        }
    }
}

/// The task run lock, read: what [`find`] asks of each task run, and what
/// [`remove`] tells once one is gone. [`crate::run_lock::RunLocks`] is the
/// real one; a test fakes it.
pub trait RunStates {
    /// What task run `run`'s lock says about its process.
    fn state(&self, run: &str) -> RunState;
    /// Deletes task run `run`'s lock file, once its resources are removed.
    fn forget(&self, run: &str) -> std::io::Result<()>;
}

impl RunStates for crate::run_lock::RunLocks {
    fn state(&self, run: &str) -> RunState {
        crate::run_lock::RunLocks::state(self, run)
    }

    fn forget(&self, run: &str) -> std::io::Result<()> {
        crate::run_lock::RunLocks::forget(self, run)
    }
}

/// Everything one task run left on the daemon, with what its lock says.
pub struct TaskRun {
    /// Its `run` label value.
    pub run: String,
    /// The task it ran, from its first resource's label.
    pub task: String,
    /// What its lock read: dead, or with `all` unknown — never alive.
    pub state: RunState,
    /// Its oldest resource's age.
    pub age_seconds: i64,
    /// In the daemon's listing order: containers, then its network.
    pub resources: Vec<Leftover>,
}

/// What [`find`] selected, and how many task runs old enough to be selected
/// it left out for their state: the caller's notice says so.
#[derive(Default)]
pub struct Selection {
    pub selected: Vec<TaskRun>,
    /// Left out because their lock is held.
    pub alive: usize,
    /// Left out because their state is unknown; always 0 with `all`.
    pub unknown: usize,
}

impl Selection {
    /// Every resource selected, task run by task run: each one's containers,
    /// then its network. Not the order [`remove`] reports in, which is
    /// every container first.
    pub fn resources(&self) -> impl Iterator<Item = &Leftover> {
        self.selected.iter().flat_map(|run| &run.resources)
    }
}

/// The task runs this project (or every project, with `project` `None`) left
/// behind, in `run` label order: those `locks` reads dead, and with `all`
/// those it reads unknown too — never one it reads alive — each no younger
/// than `older_than`. `older_than` is applied first, so a task run too young
/// is neither selected nor counted as left out.
///
/// `project` `None` still filters on *having* the project label, never on
/// nothing: an unfiltered listing is every container on the machine, which for
/// a `clean` would mean stopping and removing other tools' work. "Every
/// project" means every project *Ratect* created.
///
/// The same check is then applied again to what the daemon returned. That is
/// deliberate rather than redundant: everything returned here is a removal
/// candidate, and the cost of a wrong one is someone else's container, so
/// nothing without Ratect's own project label is ever a leftover of ours —
/// however the listing was filtered, and whatever a future caller passes.
///
/// The daemon is listed before any lock is read. A task run that starts in
/// between isn't listed; one that ends in between reads dead or unknown, and
/// whatever of it is gone by then is no failure to remove.
///
/// `now` is seconds since the Unix epoch; `older_than` is compared against it,
/// so a caller that wants "everything" passes `None` rather than a zero age.
pub async fn find<D, L>(
    docker: &D,
    project: Option<&str>,
    locks: &L,
    older_than: Option<std::time::Duration>,
    all: bool,
    now: i64,
) -> Result<Selection>
where
    D: ResourceInventory + Send + Sync,
    L: RunStates + ?Sized,
{
    let filters = [(crate::labels::PROJECT, project)];
    let mut found = docker.list_containers(&filters).await?;
    found.extend(docker.list_networks(&filters).await?);

    let mut runs: BTreeMap<String, Vec<Leftover>> = BTreeMap::new();
    for resource in found {
        if !resource.labels.contains_key(crate::labels::PROJECT) {
            continue;
        }
        let Some(run) = resource.labels.get(crate::labels::RUN).cloned() else {
            continue;
        };
        runs.entry(run)
            .or_default()
            .push(Leftover::new(resource, now));
    }

    let mut selection = Selection::default();
    for (run, mut resources) in runs {
        let age_seconds = resources.iter().map(|l| l.age_seconds).max().unwrap_or(0);
        if older_than.is_some_and(|older_than| age_seconds < older_than.as_secs() as i64) {
            continue;
        }
        let state = locks.state(&run);
        match state {
            RunState::Alive => {
                selection.alive += 1;
                continue;
            }
            RunState::Unknown if !all => {
                selection.unknown += 1;
                continue;
            }
            RunState::Dead | RunState::Unknown => {}
        }
        resources.sort_by_key(|leftover| leftover.is_network);
        selection.selected.push(TaskRun {
            task: resources[0].task.clone(),
            run,
            state,
            age_seconds,
            resources,
        });
    }
    Ok(selection)
}

/// Removes every resource of every task run in `runs`, reporting each one to
/// `progress` as it is attempted, and returns how many were actually removed.
/// A dead task run whose every resource went then has its lock file deleted
/// through `locks` (decisions/0025's Decision 4); an unknown one's is left,
/// since it may be another kernel's live task run's. Only a task run in
/// `runs`, which this daemon listed, can have its file deleted: one whose
/// resources are on another daemon is never named here.
///
/// **Containers first, then networks.** A network still holding an endpoint
/// can't be removed, so the reverse order fails on every task that had one.
/// The partition is here rather than left to the caller's ordering precisely
/// so that a caller cannot get it wrong.
///
/// One failure doesn't abandon the rest: a resource someone else removed in
/// the meantime, or one still in use, shouldn't leave the remaining leftovers
/// behind too. That is why there is no `Result` here at all — every outcome,
/// success or error, goes to `progress`, which is what decides how it reads,
/// and the count of what actually went is the only summary this can give. A
/// lock file that can't be deleted is said at debug level only: it reads
/// dead, with nothing left to remove.
pub async fn remove<D, L, F>(docker: &D, runs: &[TaskRun], locks: &L, mut progress: F) -> usize
where
    D: ResourceInventory + Send + Sync,
    L: RunStates + ?Sized,
    F: FnMut(&Leftover, &Result<()>),
{
    // Each resource with the index of the task run it belongs to.
    type Indexed<'a> = Vec<(usize, &'a Leftover)>;
    let (networks, containers): (Indexed<'_>, Indexed<'_>) = runs
        .iter()
        .enumerate()
        .flat_map(|(index, run)| run.resources.iter().map(move |leftover| (index, leftover)))
        .partition(|(_, leftover)| leftover.is_network);

    let mut removed = 0;
    let mut failed = vec![false; runs.len()];
    for (index, leftover) in containers.iter().chain(networks.iter()) {
        let result = if leftover.is_network {
            docker.remove_network(&leftover.resource.id).await
        } else {
            // No `Container` config is available for a leftover — it's found
            // by label scan, not loaded from a project's own file — so this
            // always uses Docker's own default stop behavior, same as
            // before ratect#112 added the two fields.
            docker
                .stop_and_remove_container(&leftover.resource.id, None, None, None)
                .await
        };
        match result {
            Ok(()) => removed += 1,
            Err(_) => failed[*index] = true,
        }
        progress(leftover, &result);
    }
    for (run, failed) in runs.iter().zip(failed) {
        if run.state == RunState::Dead && !failed {
            if let Err(e) = locks.forget(&run.run) {
                tracing::debug!("Failed to delete task run {}'s lock file: {e}", run.run);
            }
        }
    }
    removed
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
