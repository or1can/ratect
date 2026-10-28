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

use super::super::test_support::SharedBuffer;
use super::*;
use std::time::Duration;

/// A logger whose progress console is `buffer`; its stderr console is a
/// buffer of its own, returned third — colour off on both.
fn logger_with_stderr() -> (SimpleEventLogger, SharedBuffer, SharedBuffer) {
    let buffer = SharedBuffer::default();
    let stderr = SharedBuffer::default();
    let console = Console::new(Box::new(buffer.clone()), false);
    let stderr_console = Console::new(Box::new(stderr.clone()), false);
    (
        SimpleEventLogger::new(console, stderr_console),
        buffer,
        stderr,
    )
}

fn logger() -> (SimpleEventLogger, SharedBuffer) {
    let (logger, buffer, _) = logger_with_stderr();
    (logger, buffer)
}

fn colored_logger() -> (SimpleEventLogger, SharedBuffer) {
    let buffer = SharedBuffer::default();
    let console = Console::new(Box::new(buffer.clone()), true);
    let stderr = Console::new(Box::new(SharedBuffer::default()), true);
    (SimpleEventLogger::new(console, stderr), buffer)
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
    // A container's creation prints nothing — Batect's own simple logger
    // has no line for a create either; the next line is its start.
    logger.post(TaskEvent::ContainerCreating {
        container: "db".into(),
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "db".into(),
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
        run_in: None,
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

/// A `run_in` setup command (ratect#111) runs in a container other than the
/// one whose gate declared it. This line is flat — no per-container prefix
/// to carry the declaring container — so it has to name both, or it reads as
/// though the command ran in the wrong place.
#[test]
fn a_run_in_setup_command_names_the_container_it_runs_in_and_the_one_that_declared_it() {
    let (logger, buffer) = logger();
    logger.post(TaskEvent::RunningSetupCommand {
        container: "db".into(),
        command: "./seed.sh".into(),
        index: 1,
        total: 1,
        run_in: Some("db-seed-client".into()),
    });
    assert_eq!(
        buffer.contents(),
        "Running setup command ./seed.sh (1 of 1) in db-seed-client, for db...\n"
    );
}

/// A milestone, not a warning (decisions/0017): no `Warning:` marker, on
/// the progress console like any other milestone, and worded by the one
/// phrase `fancy` renders too (`ui::exited_unexpectedly`).
#[test]
fn dependency_exiting_unexpectedly_is_a_milestone_not_a_warning() {
    let (logger, buffer, stderr) = logger_with_stderr();
    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 137,
    });
    assert_eq!(
        buffer.contents(),
        "db exited unexpectedly with exit code 137.\n"
    );
    assert_eq!(stderr.contents(), "");
}

/// The three stderr classes go to the stderr console, never the progress
/// one, with the markers decisions/0017 gives them — and nothing else does.
#[test]
fn errors_warnings_and_notices_go_to_stderr_with_their_markers() {
    let (logger, buffer, stderr) = logger_with_stderr();
    logger.post(TaskEvent::Error {
        message: "Cleanup failed: no".into(),
    });
    logger.post(TaskEvent::Warning {
        message: "the proxy is on loopback".into(),
    });
    logger.post(TaskEvent::Notice {
        message: "Interrupted; cleaning up.".into(),
    });
    logger.post(TaskEvent::TaskStarting {
        task: "build".into(),
    });
    assert_eq!(
        stderr.contents(),
        "Error: Cleanup failed: no\n\
         Warning: the proxy is on loopback\n\
         Interrupted; cleaning up.\n"
    );
    assert_eq!(buffer.contents(), "Running build...\n");
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

fn info(name: &str, is_task_container: bool) -> super::super::TaskContainerInfo {
    super::super::TaskContainerInfo {
        name: name.to_string(),
        image: None,
        build_tag: None,
        dependencies: Vec::new(),
        is_task_container,
    }
}

/// The task's own readiness milestones would otherwise land in the
/// middle of its own raw output — see [`SimpleEventLogger::is_task_container`].
#[test]
fn readiness_milestones_are_dropped_for_the_tasks_own_container_only() {
    let (logger, buffer) = logger();
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", true), info("db", false)],
    });
    for container in ["db", "app"] {
        logger.post(TaskEvent::ContainerBecameHealthy {
            container: container.into(),
        });
        logger.post(TaskEvent::RunningSetupCommand {
            container: container.into(),
            command: "./init.sh".into(),
            index: 1,
            total: 1,
            run_in: None,
        });
        logger.post(TaskEvent::SetupCommandsCompleted {
            container: container.into(),
        });
    }
    assert_eq!(
        buffer.contents(),
        "db has become healthy.\n\
             Running setup command ./init.sh (1 of 1) in db...\n\
             db has completed all setup commands.\n"
    );
}

/// Without the per-task reset, a prerequisite's task container would go
/// on suppressing its own milestones through the next task, where it may
/// be a plain dependency instead.
#[test]
fn a_new_task_resets_which_container_is_the_tasks_own() {
    let (logger, buffer) = logger();
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", true)],
    });
    logger.post(TaskEvent::TaskStarting {
        task: "main".into(),
    });
    logger.post(TaskEvent::ContainerBecameHealthy {
        container: "app".into(),
    });
    assert_eq!(
        buffer.contents(),
        "Running main...\n\
             app has become healthy.\n"
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

/// A task that failed to run ends with its own summary line on the
/// progress console, where a finished one's goes, and nothing on stderr:
/// the error was reported when it happened (ratect#473).
#[test]
fn task_failed_prints_the_failed_summary_line() {
    let buffer = SharedBuffer::default();
    let stderr = SharedBuffer::default();
    let logger = SimpleEventLogger::new(
        Console::new(Box::new(buffer.clone()), true),
        Console::new(Box::new(stderr.clone()), true),
    );
    logger.post(TaskEvent::TaskFailed {
        task: "build".into(),
        duration: Duration::from_millis(3200),
    });
    assert_eq!(
        buffer.contents(),
        "\x1b[1mbuild\x1b[0m failed after 3.2s.\n"
    );
    assert_eq!(stderr.contents(), "");
}

/// A prerequisites-only task's line, Batect's words with its name in bold,
/// separated from the task before it as a task's own `Running` line is
/// (ratect#364).
#[test]
fn a_prerequisites_only_task_says_there_is_nothing_more_to_do() {
    let (logger, buffer) = logger();
    logger.post(TaskEvent::TaskStarting {
        task: "build".into(),
    });
    logger.post(TaskEvent::PrerequisitesOnly { task: "all".into() });
    assert_eq!(
        buffer.contents(),
        "Running build...\n\
         \n\
         The task all only defines prerequisite tasks, nothing more to do.\n"
    );

    let (logger, buffer) = colored_logger();
    logger.post(TaskEvent::PrerequisitesOnly { task: "all".into() });
    assert_eq!(
        buffer.contents(),
        "The task \x1b[1mall\x1b[0m only defines prerequisite tasks, nothing more to do.\n"
    );
}
