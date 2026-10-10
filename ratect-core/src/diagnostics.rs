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

//! The checks behind `ratect doctor` and `ratect config validate` — what a
//! [`Finding`] is, and every producer of one that needs no daemon connection
//! of its own to run. Moved out of `ratect/src/main.rs` in 0.6.0, alongside
//! [`crate::resources`], for the same reason: these were pure functions bound
//! to nothing, sitting in a binary because nobody had moved them yet.
//!
//! **What stays in the binary, and why:** rendering a set of findings to
//! stdout, counting problems into an exit code, and the two Docker-daemon
//! findings (`Docker connection options are unusable`, `Docker daemon
//! (not )?reachable`). That last pair needs `DockerClient::server_version`,
//! which answers "what does the daemon call itself" — a property of *this*
//! connection, not of [`crate::resources::ResourceInventory`]'s container/
//! network vocabulary, so it has no seam to cross here. [`leftover_finding`] is the
//! one check downstream of a daemon connection that *is* here: it takes
//! `Option<&D>` rather than requiring one, so a caller whose connection
//! already failed can skip it without this module needing any opinion of its
//! own on what "no connection" means or how that got decided.

use crate::config::Config;
use crate::written_for::WrittenFor;
use std::collections::BTreeMap;
use std::path::Path;

/// One thing `doctor` (or `config validate`) looked at.
#[derive(Debug, PartialEq, Eq)]
pub enum Finding {
    /// Checked, nothing wrong.
    Fine(String),
    /// Works, but is likely to bite — a reproducibility hazard, or a
    /// readiness gate that isn't really gating anything.
    Warning(String),
    /// Will fail a run, or already has.
    Problem(String),
}

impl Finding {
    pub fn render(&self) -> String {
        match self {
            Finding::Fine(message) => format!("  ok      {message}"),
            Finding::Warning(message) => format!("  warning {message}"),
            Finding::Problem(message) => format!("  problem {message}"),
        }
    }
}

/// The checks that need only the configuration — pure, so they're testable
/// without a daemon or a project on disk.
pub fn config_findings(config: &Config) -> Vec<Finding> {
    let mut findings = Vec::new();

    // A floating tag defeats the entire point of pinning a task's
    // environment: the same config gives a different image next week.
    let mut floating: Vec<&str> = config
        .containers
        .iter()
        .filter(|(_, container)| container.image.as_deref().is_some_and(floating_image_tag))
        .map(|(name, _)| name.as_str())
        .collect();
    floating.sort_unstable();
    for name in floating {
        findings.push(Finding::Warning(format!(
            "container '{name}' uses a floating image tag — pin it, or the same \
             configuration will run a different image later"
        )));
    }

    // A dependency with no health check counts as ready the moment it
    // starts, which is where "connection refused" on the first run comes
    // from. Ratect can't see whether the *image* defines one, so this is
    // phrased as something to check rather than something wrong.
    //
    // A `health_check` is only one of the three ways a dependency's
    // readiness can be decided, though, and the warning is simply false of
    // the other two: a `run_to_completion` dependency is ready when it
    // exits 0 (and cannot have a health check at all), and one with an
    // `external_health_check` is ready when a check Ratect runs from
    // outside it passes. Both would otherwise be told to add a field they
    // are forbidden from having.
    let mut unguarded: Vec<&str> = dependency_names(config)
        .into_iter()
        .filter(|name| {
            config.containers.get(*name).is_some_and(|container| {
                container.health_check.is_none()
                    && container.external_health_check.is_none()
                    && !container.run_to_completion.unwrap_or(false)
            })
        })
        .collect();
    unguarded.sort_unstable();
    for name in unguarded {
        findings.push(Finding::Warning(format!(
            "dependency '{name}' has no health_check — unless its image defines one, \
             it counts as ready the moment it starts"
        )));
    }

    // Already resolved to an absolute path by `load_project`, so this is
    // the path Ratect will actually hand to Docker.
    let mut missing: Vec<String> = Vec::new();
    for (name, container) in &config.containers {
        let Some(directory) = &container.build_directory else {
            continue;
        };
        let directory = Path::new(directory);
        if !directory.is_dir() {
            missing.push(format!(
                "container '{name}' has build_directory '{}', which doesn't exist",
                directory.display()
            ));
            continue;
        }
        let dockerfile = directory.join(container.dockerfile.as_deref().unwrap_or("Dockerfile"));
        if !dockerfile.is_file() {
            missing.push(format!(
                "container '{name}' has no '{}' in its build_directory",
                dockerfile.display()
            ));
        }
    }
    missing.sort();
    findings.extend(missing.into_iter().map(Finding::Problem));

    findings
}

/// Batect's own wrapper scripts (`batect`/`batect.cmd`) left in a project
/// that's moved to Ratect. Not inert: `./batect` still downloads and runs
/// the unmaintained JVM binary, so during a migration you can believe
/// you've switched over while `./batect` quietly still runs the old tool.
///
/// Only flags a script that *still runs Batect* — matched by content, not
/// name, so a `batect` file that no longer does (deleted and replaced, or a
/// hand-written shim that execs `ratect-compat`) is correctly left alone.
/// The recommended migration is to delete the wrapper and run Ratect from
/// the PATH, since Ratect is an ordinary installed binary rather than a
/// downloaded-on-demand wrapper the way Batect was — see docs/ratect-cli.md.
pub fn wrapper_script_findings(project_directory: &Path) -> Vec<Finding> {
    ["batect", "batect.cmd"]
        .iter()
        .filter_map(|name| {
            let path = project_directory.join(name);
            // Small scripts (~200 lines); the marker is on line 2, so a
            // partial read would do, but reading the whole thing is
            // simpler and the file is tiny.
            let content = std::fs::read(&path).ok()?;
            is_batect_wrapper(&String::from_utf8_lossy(&content)).then(|| {
                Finding::Warning(format!(
                    "'{name}' is a Batect wrapper script and still runs Batect, not Ratect — \
                     delete it and run ratect (or ratect-compat) from your PATH"
                ))
            })
        })
        .collect()
}

/// A native project that doesn't say which Ratect it was written for — see
/// [`crate::written_for`]. Only a warning: it changes nothing until a release
/// changes what a configuration means, at which point the project counts as
/// written for 0.3.0 and refuses until someone has looked.
pub fn written_for_finding(written_for: &WrittenFor) -> Option<Finding> {
    let field = crate::written_for::FIELD;
    match written_for {
        WrittenFor::Declared(_) => None,
        WrittenFor::Unmarked => Some(Finding::Warning(format!(
            "'{field}' isn't set — set it to the Ratect version this project was written \
             for, so a later Ratect can say when one of its changes affects this project"
        ))),
        WrittenFor::YamlRoot => Some(Finding::Warning(format!(
            "the root file is Batect-format YAML, so it can't set '{field}' — convert it \
             with `ratect config convert` to declare which Ratect it was written for"
        ))),
    }
}

/// Whether `content` is one of Batect's own wrapper scripts, by the notice
/// line its authors put near the top of both the Unix and Windows forms —
/// a deliberate, stable marker (`# This file is part of Batect.` /
/// `rem This file is part of Batect.`). Matched as a substring so the
/// comment character doesn't matter. A script repointed at Ratect won't
/// carry it, which is the whole point.
fn is_batect_wrapper(content: &str) -> bool {
    content.contains("This file is part of Batect.")
}

/// `image` with no tag at all, or an explicitly floating one. Docker treats
/// a missing tag as `latest`, so both are the same hazard.
fn floating_image_tag(image: &str) -> bool {
    // A colon before the last slash is a registry port, not a tag —
    // `registry:5000/app` is untagged.
    let tag = match image.rsplit_once('/') {
        Some((_, last)) => last.rsplit_once(':').map(|(_, tag)| tag),
        None => image.rsplit_once(':').map(|(_, tag)| tag),
    };
    match tag {
        None => true,
        Some(tag) => tag == "latest",
    }
}

/// Every container named as a dependency, by another container or by a
/// task — the ones whose readiness actually gates something.
fn dependency_names(config: &Config) -> Vec<&str> {
    let mut names: Vec<&str> = config
        .containers
        .values()
        .filter_map(|container| container.dependencies.as_ref())
        .chain(
            config
                .tasks
                .values()
                .filter_map(|task| task.dependencies.as_ref()),
        )
        .flatten()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Leftovers are worth reporting unasked — the whole reason
/// [`crate::resources`] exists is that nobody thinks to look. `docker` is
/// `None` when the connection this run of `doctor` opened already failed;
/// that failure has its own finding, so this contributes nothing rather than
/// repeating it.
///
/// Goes through [`crate::resources::find`] — the same selection `ratect
/// resources list` uses — rather than listing containers and networks
/// itself, so the two can never disagree about what counts as a leftover.
/// A listing failure reads as "no leftovers", matching this check's own
/// unasked, best-effort nature: it should never be the reason `doctor`
/// itself fails.
pub async fn leftover_finding<D: crate::resources::ResourceInventory + Send + Sync>(
    docker: Option<&D>,
    project: &str,
    now: i64,
) -> Option<Finding> {
    let docker = docker?;
    let leftovers = crate::resources::find(docker, Some(project), None, now)
        .await
        .unwrap_or_default();
    Some(if leftovers.is_empty() {
        Finding::Fine("no leftovers from previous runs".to_string())
    } else {
        Finding::Warning(format!(
            "{} resource(s) left over from previous runs — see `ratect resources list`",
            leftovers.len()
        ))
    })
}

/// `config convert`'s warning for each container whose
/// `run_as_current_user.home_directory` holds what `ratect.toml` would read as
/// an expression (#462). A Batect-format file uses the value as written; the
/// converted file evaluates the same text, and has no escape that would keep
/// it literal — so a conversion can't carry it across unchanged, and the
/// reader has to decide what they meant. Sorted by container name.
///
/// Not one of [`config_findings`] for the same reason as
/// [`ungated_task_container_warnings`]: it's only news to someone moving a
/// file from one format to the other.
pub fn home_directory_expression_warnings(config: &Config) -> Vec<String> {
    let mut containers: Vec<(&String, &str)> = config
        .containers
        .iter()
        .filter_map(|(name, container)| {
            let home = container
                .run_as_current_user
                .as_ref()?
                .home_directory
                .as_deref()?;
            crate::expressions::contains_expression(home).then_some((name, home))
        })
        .collect();
    containers.sort_unstable();
    containers
        .into_iter()
        .map(|(name, home)| {
            format!(
                "Container '{name}' has run_as_current_user.home_directory '{home}', which \
                 batect.yml uses as written but ratect evaluates as an expression — check \
                 what it resolves to under ratect."
            )
        })
        .collect()
}

/// What `ratect config convert` warns about: one of the two ways a converted
/// project behaves differently from its source (ratect#267; the other is
/// [`home_directory_expression_warnings`]). `ratect` runs a
/// task's own container with no readiness gate (decisions/0012), so a
/// `health_check` on a container some task runs as its own `run.container`
/// isn't waited on for that task, and its `setup_commands` don't run. One
/// message per container, naming the tasks and only the fields it has.
///
/// Not one of [`config_findings`]: in a native project those fields are
/// valid and inert there by design — the same container may be another
/// task's dependency, where they apply — so `doctor` and `config validate`
/// would be flagging configurations that are exactly what their author
/// meant. It is only news to someone converting from `ratect-compat`, which
/// does gate a task's own container. An image's own `HEALTHCHECK` isn't
/// waited on either, but can't be seen without the image, so it isn't
/// covered here.
pub fn ungated_task_container_warnings(config: &Config) -> Vec<String> {
    let mut tasks_by_container: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (task_name, task) in &config.tasks {
        if let Some(run) = &task.run {
            tasks_by_container
                .entry(run.container.as_str())
                .or_default()
                .push(task_name.as_str());
        }
    }
    tasks_by_container
        .into_iter()
        .filter_map(|(container_name, mut task_names)| {
            let container = config.containers.get(container_name)?;
            let health_check = container
                .health_check
                .is_some()
                .then_some("the container's 'health_check' isn't waited on");
            let setup_commands = container
                .setup_commands
                .as_ref()
                .is_some_and(|commands| !commands.is_empty())
                .then_some(match health_check {
                    Some(_) => "its 'setup_commands' don't run",
                    None => "the container's 'setup_commands' don't run",
                });
            let effects: Vec<&str> = [health_check, setup_commands].into_iter().flatten().collect();
            if effects.is_empty() {
                return None;
            }
            task_names.sort_unstable();
            let quoted: Vec<String> = task_names.iter().map(|name| format!("'{name}'")).collect();
            let tasks_run = match quoted.as_slice() {
                [one] => format!("Task {one} runs container '{container_name}' as its own"),
                [rest @ .., last] => format!(
                    "Tasks {} and {last} run container '{container_name}' as their own",
                    rest.join(", ")
                ),
                [] => unreachable!("every entry was created by pushing a task name"),
            };
            Some(format!(
                "{tasks_run}, so under ratect {} — a task's own container has no readiness \
                 gate (see https://github.com/or1can/ratect/blob/main/docs/dependency-readiness.md#the-tasks-own-container).",
                effects.join(" and ")
            ))
        })
        .collect()
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
