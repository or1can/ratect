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

//! The `simple` output mode — a port of Batect's `SimpleEventLogger`:
//! plain, append-only milestone lines, one per lifecycle event, with no
//! live-updating progress detail at all (pull/build progress events are
//! deliberately ignored — only their start/finish milestones print). The
//! mode every non-interactive console gets by default.

use super::{Console, EventSink, OnceFlag, TaskEvent};
use std::sync::Mutex;

pub struct SimpleEventLogger {
    console: Console,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Guards the blank separator line between one task's output and the
    /// next's ("Running x..." → work → blank → "Running y..."), matching
    /// Batect's `SessionRunner` printing a separator between prerequisite
    /// tasks — no blank before the very first.
    printed_a_task: bool,
    /// Guards "Cleaning up..." printing once per task even though several
    /// cleanup-worthy events may follow.
    started_cleanup: OnceFlag,
    /// The task's own container's name, from `TaskGraphResolved` — used only
    /// to suppress its own `ContainerBecameHealthy`/`RunningSetupCommand`/
    /// `SetupCommandsCompleted` lines (see [`SimpleEventLogger::is_task_container`]).
    /// Reset per task the same way `started_cleanup` is.
    task_container: Option<String>,
}

impl SimpleEventLogger {
    pub fn new(console: Console) -> Self {
        Self {
            console,
            state: Mutex::new(State::default()),
        }
    }

    /// The logger `main.rs` actually wires up: real stdout, color iff
    /// stdout is a terminal and `--no-color` wasn't given.
    pub fn stdout(no_color: bool) -> Self {
        Self::new(Console::stdout(no_color))
    }
}

impl EventSink for SimpleEventLogger {
    fn post(&self, event: TaskEvent) {
        match event {
            TaskEvent::TaskStarting { task } => {
                let mut state = self.state.lock().unwrap();
                if state.printed_a_task {
                    self.console.println("");
                }
                state.printed_a_task = true;
                // A fresh task execution gets a fresh cleanup guard — each
                // task tears down its own network/dependencies (see
                // docs/task-lifecycle.md), so each prints its own
                // "Cleaning up...".
                state.started_cleanup.reset();
                state.task_container = None;
                self.console.println(&format!("Running {task}..."));
            }
            TaskEvent::TaskFinished {
                task,
                exit_code,
                duration,
            } => {
                self.console.println(&super::format_task_summary(
                    &self.console,
                    &task,
                    exit_code,
                    duration,
                ));
            }
            TaskEvent::ImagePullStarting { image } => {
                self.console.println(&format!("Pulling {image}..."));
            }
            TaskEvent::ImagePullCompleted { image } => {
                self.console.println(&format!("Pulled {image}."));
            }
            TaskEvent::ImageBuildStarting { container } => {
                self.console.println(&format!("Building {container}..."));
            }
            TaskEvent::ImageBuildCompleted { container } => {
                self.console.println(&format!("Built {container}."));
            }
            TaskEvent::DependencyStarting { container } => {
                self.console.println(&format!("Starting {container}..."));
            }
            TaskEvent::DependencyStarted { container } => {
                self.console.println(&format!("Started {container}."));
            }
            TaskEvent::ContainerBecameHealthy { container } => {
                if self.is_task_container(&container) {
                    return;
                }
                self.console
                    .println(&format!("{container} has become healthy."));
            }
            TaskEvent::RunningSetupCommand {
                container,
                command,
                index,
                total,
            } => {
                if self.is_task_container(&container) {
                    return;
                }
                self.console.println(&format!(
                    "Running setup command {command} ({index} of {total}) in {container}..."
                ));
            }
            TaskEvent::SetupCommandsCompleted { container } => {
                if self.is_task_container(&container) {
                    return;
                }
                self.console
                    .println(&format!("{container} has completed all setup commands."));
            }
            TaskEvent::RunningTaskContainer { container, command } => {
                let line = match command {
                    Some(command) => format!("Running {command} in {container}..."),
                    None => format!("Running {container}..."),
                };
                self.console.println(&line);
            }
            TaskEvent::CleanupStarting => {
                let mut state = self.state.lock().unwrap();
                if state.started_cleanup.fire_once() {
                    self.console.println("");
                    self.console.println("Cleaning up...");
                }
            }
            // Recorded only so `is_task_container` can recognize the task's
            // own container above — simple mode otherwise has no use for the
            // graph itself (unlike fancy/interleaved, which use it for
            // layout).
            TaskEvent::TaskGraphResolved { containers } => {
                self.state.lock().unwrap().task_container = containers
                    .into_iter()
                    .find(|c| c.is_task_container)
                    .map(|c| c.name);
            }
            // No live progress detail in simple mode — the whole point of
            // the mode (matching Batect: `ImagePullProgressEvent`/
            // `ImageBuildProgressEvent` are unhandled there too). The
            // per-step cleanup events only feed fancy's live displays, and a
            // failure's error reaches stderr through the normal error
            // chain, so none of them get a line here either.
            TaskEvent::ImagePullProgress { .. }
            | TaskEvent::ImageBuildProgress { .. }
            | TaskEvent::ImageResolved { .. }
            | TaskEvent::TaskContainerCreated { .. }
            | TaskEvent::ContainerRemoved { .. }
            | TaskEvent::RemovingNetwork
            | TaskEvent::TaskFailed { .. }
            | TaskEvent::ContainerOutput { .. }
            | TaskEvent::SetupCommandOutput { .. } => {}
        }
    }
}

impl SimpleEventLogger {
    /// Whether `container` is the task's own — the three readiness
    /// milestones (`ContainerBecameHealthy`/`RunningSetupCommand`/
    /// `SetupCommandsCompleted`) are silently dropped for it, matching
    /// Batect's own `SimpleEventLogger` (each of its three equivalent
    /// handlers opens with `if (container == taskContainer) return`).
    ///
    /// Since 0.21.0 those events fire for the task's own container too (see
    /// `engine.rs`'s `run_task_container_readiness`), and they'd print
    /// *while* its raw command output is streaming to this same stdout —
    /// output that has no framing of its own (no prefix, and often no
    /// trailing newline), so a milestone line can land glued onto the tail
    /// of whatever the command last wrote, on the very same terminal row,
    /// at a nondeterministic point. Simple mode's whole contract is that it
    /// never touches the task's own output, so the milestone is what gives
    /// way. Nothing is lost that matters: a readiness *failure* still
    /// reaches stderr through the normal error chain, and `all` mode still
    /// reports every one of these for every container (its output is
    /// line-buffered and prefixed, so it has no collision to avoid — same
    /// split Batect's own `InterleavedEventLogger` makes).
    ///
    /// A dependency container is unaffected: its own readiness always
    /// completes before the task container starts, so its lines can't
    /// collide with anything.
    fn is_task_container(&self, container: &str) -> bool {
        self.state.lock().unwrap().task_container.as_deref() == Some(container)
    }
}

#[cfg(test)]
#[path = "simple_tests.rs"]
mod tests;
