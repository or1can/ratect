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

use super::{Color, Console, EventSink, TaskEvent};
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
    started_cleanup: bool,
}

impl SimpleEventLogger {
    pub fn new(console: Console) -> Self {
        Self {
            console,
            state: Mutex::new(State::default()),
        }
    }

    /// The logger `main.rs` actually wires up: real stdout, color iff
    /// stdout is a terminal.
    pub fn stdout() -> Self {
        Self::new(Console::stdout())
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
                state.started_cleanup = false;
                self.console.println(&format!("Running {task}..."));
            }
            TaskEvent::TaskFinished {
                task,
                exit_code,
                duration,
            } => {
                let color = if exit_code == 0 {
                    Color::Green
                } else {
                    Color::Red
                };
                let exit_code = self.console.colored(color, &exit_code.to_string());
                self.console.println(&format!(
                    "{task} finished with exit code {exit_code} in {}.",
                    super::format_duration(duration)
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
                self.console
                    .println(&format!("{container} has become healthy."));
            }
            TaskEvent::RunningSetupCommand {
                container,
                command,
                index,
                total,
            } => {
                self.console.println(&format!(
                    "Running setup command {command} ({index} of {total}) in {container}..."
                ));
            }
            TaskEvent::SetupCommandsCompleted { container } => {
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
                if !state.started_cleanup {
                    state.started_cleanup = true;
                    self.console.println("");
                    self.console.println("Cleaning up...");
                }
            }
            // No live progress detail in simple mode — the whole point of
            // the mode (matching Batect: `ImagePullProgressEvent`/
            // `ImageBuildProgressEvent` are unhandled there too).
            TaskEvent::ImagePullProgress { .. } | TaskEvent::ImageBuildProgress { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::SharedBuffer;
    use super::*;
    use std::time::Duration;

    fn logger() -> (SimpleEventLogger, SharedBuffer) {
        let buffer = SharedBuffer::default();
        let console = Console::new(Box::new(buffer.clone()), false);
        (SimpleEventLogger::new(console), buffer)
    }

    fn colored_logger() -> (SimpleEventLogger, SharedBuffer) {
        let buffer = SharedBuffer::default();
        let console = Console::new(Box::new(buffer.clone()), true);
        (SimpleEventLogger::new(console), buffer)
    }

    #[test]
    fn renders_lifecycle_milestones_as_plain_lines() {
        let (logger, buffer) = logger();
        logger.post(TaskEvent::TaskStarting {
            task: "build".into(),
        });
        logger.post(TaskEvent::ImagePullStarting {
            image: "alpine:3".into(),
        });
        logger.post(TaskEvent::ImagePullCompleted {
            image: "alpine:3".into(),
        });
        logger.post(TaskEvent::ImageBuildStarting {
            container: "app".into(),
        });
        logger.post(TaskEvent::ImageBuildCompleted {
            container: "app".into(),
        });
        logger.post(TaskEvent::DependencyStarting {
            container: "db".into(),
        });
        logger.post(TaskEvent::DependencyStarted {
            container: "db".into(),
        });
        logger.post(TaskEvent::ContainerBecameHealthy {
            container: "db".into(),
        });
        logger.post(TaskEvent::RunningSetupCommand {
            container: "db".into(),
            command: "./init.sh".into(),
            index: 1,
            total: 2,
        });
        logger.post(TaskEvent::SetupCommandsCompleted {
            container: "db".into(),
        });
        logger.post(TaskEvent::RunningTaskContainer {
            container: "app".into(),
            command: Some("cargo test".into()),
        });
        assert_eq!(
            buffer.contents(),
            "Running build...\n\
             Pulling alpine:3...\n\
             Pulled alpine:3.\n\
             Building app...\n\
             Built app.\n\
             Starting db...\n\
             Started db.\n\
             db has become healthy.\n\
             Running setup command ./init.sh (1 of 2) in db...\n\
             db has completed all setup commands.\n\
             Running cargo test in app...\n"
        );
    }

    #[test]
    fn progress_detail_is_ignored() {
        let (logger, buffer) = logger();
        logger.post(TaskEvent::ImagePullProgress {
            image: "alpine:3".into(),
            message: "Downloading".into(),
        });
        logger.post(TaskEvent::ImageBuildProgress {
            tag: "proj-app".into(),
            message: "Step 1/4".into(),
        });
        assert_eq!(buffer.contents(), "");
    }

    #[test]
    fn task_without_command_renders_container_only_run_line() {
        let (logger, buffer) = logger();
        logger.post(TaskEvent::RunningTaskContainer {
            container: "app".into(),
            command: None,
        });
        assert_eq!(buffer.contents(), "Running app...\n");
    }

    #[test]
    fn cleanup_prints_once_per_task() {
        let (logger, buffer) = logger();
        logger.post(TaskEvent::CleanupStarting);
        logger.post(TaskEvent::CleanupStarting);
        assert_eq!(buffer.contents(), "\nCleaning up...\n");
    }

    #[test]
    fn blank_line_separates_tasks_and_cleanup_guard_resets() {
        let (logger, buffer) = logger();
        logger.post(TaskEvent::TaskStarting {
            task: "prereq".into(),
        });
        logger.post(TaskEvent::CleanupStarting);
        logger.post(TaskEvent::TaskStarting {
            task: "main".into(),
        });
        logger.post(TaskEvent::CleanupStarting);
        assert_eq!(
            buffer.contents(),
            "Running prereq...\n\
             \nCleaning up...\n\
             \n\
             Running main...\n\
             \nCleaning up...\n"
        );
    }

    #[test]
    fn task_finished_colors_exit_code_by_outcome() {
        let (logger, buffer) = colored_logger();
        logger.post(TaskEvent::TaskFinished {
            task: "build".into(),
            exit_code: 0,
            duration: Duration::from_millis(2300),
        });
        logger.post(TaskEvent::TaskFinished {
            task: "lint".into(),
            exit_code: 3,
            duration: Duration::from_secs(61),
        });
        assert_eq!(
            buffer.contents(),
            "build finished with exit code \x1b[32m0\x1b[0m in 2.3s.\n\
             lint finished with exit code \x1b[31m3\x1b[0m in 1m 1.0s.\n"
        );
    }
}
