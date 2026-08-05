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
