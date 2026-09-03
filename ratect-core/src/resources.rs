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
//! Leftovers happen after a crash, a `docker kill`, a run that used
//! `--no-cleanup`, or a cleanup that itself failed. The one thing labels can't
//! settle: a task running *right now* carries exactly the same labels as a
//! leftover, because until it finishes it is one. That is what `older_than`
//! exists for, and why nothing here claims to detect liveness — the daemon
//! can't say whether some other `ratect` process still cares about a
//! container.
//!
//! **This module is where the rules live; it prints nothing.** It was in
//! `ratect/src/main.rs` until 0.6.0, bound to the concrete `DockerClient`
//! even though every call it makes is a [`ContainerRuntime`] method — so
//! leftover selection, the containers-before-networks ordering rule and
//! partial-failure behaviour were provable only against a live daemon. The
//! seam existed and was simply not used. Three things follow from that
//! history and should survive:
//!
//! - **Selection takes `now` rather than reading the clock**, so an age test
//!   is a table, not a sleep.
//! - **Removal reports through a callback**, so `clean` still prints as each
//!   resource goes rather than falling silent on a slow daemon, without this
//!   module owning any wording. Errors are handed back rather than logged
//!   here: the caller knows what its own verbs are called.
//! - **Nothing without the project label is ever a candidate**, however the
//!   daemon filtered. That check is deliberately duplicated on this side —
//!   see [`find`].

use crate::docker::{ContainerRuntime, LabelledResource};
use anyhow::Result;

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
    /// Docker name is random words, which is no use for recognizing it.
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

/// Every container and network this project (or every project, with `project`
/// `None`) left behind, narrowed by `older_than`. In the daemon's own listing
/// order — grouping and sorting are the caller's, since they're decisions
/// about how a report reads.
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
/// `now` is seconds since the Unix epoch; `older_than` is compared against it,
/// so a caller that wants "everything" passes `None` rather than a zero age.
pub async fn find<D: ContainerRuntime + Send + Sync>(
    docker: &D,
    project: Option<&str>,
    older_than: Option<std::time::Duration>,
    now: i64,
) -> Result<Vec<Leftover>> {
    let filters = [(crate::labels::PROJECT, project)];
    let mut found = docker.list_containers(&filters).await?;
    found.extend(docker.list_networks(&filters).await?);

    Ok(found
        .into_iter()
        .filter(|resource| resource.labels.contains_key(crate::labels::PROJECT))
        .map(|resource| Leftover::new(resource, now))
        .filter(|leftover| match older_than {
            Some(older_than) => leftover.age_seconds >= older_than.as_secs() as i64,
            None => true,
        })
        .collect())
}

/// Removes every leftover in `leftovers`, reporting each one to `progress` as
/// it is attempted, and returns how many were actually removed.
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
/// and the count of what actually went is the only summary this can give.
pub async fn remove<D, F>(docker: &D, leftovers: &[Leftover], mut progress: F) -> usize
where
    D: ContainerRuntime + Send + Sync,
    F: FnMut(&Leftover, &Result<()>),
{
    let (networks, containers): (Vec<&Leftover>, Vec<&Leftover>) =
        leftovers.iter().partition(|leftover| leftover.is_network);

    let mut removed = 0;
    for leftover in containers.iter().chain(networks.iter()) {
        let result = if leftover.is_network {
            docker.remove_network(&leftover.resource.id).await
        } else {
            docker
                .stop_and_remove_container(&leftover.resource.id)
                .await
        };
        if result.is_ok() {
            removed += 1;
        }
        progress(leftover, &result);
    }
    removed
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
