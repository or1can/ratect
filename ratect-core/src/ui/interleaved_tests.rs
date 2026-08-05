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
use super::super::TaskContainerInfo;
use super::*;
use std::time::Duration;

fn logger() -> (InterleavedEventLogger, SharedBuffer) {
    let buffer = SharedBuffer::default();
    // Color disabled: expectations stay readable, and prefix padding is
    // the structural thing worth asserting.
    let console = Console::new(Box::new(buffer.clone()), false);
    (InterleavedEventLogger::new(console), buffer)
}

fn info(name: &str, image: Option<&str>, build_tag: Option<&str>) -> TaskContainerInfo {
    TaskContainerInfo {
        name: name.to_string(),
        image: image.map(str::to_string),
        build_tag: build_tag.map(str::to_string),
        dependencies: Vec::new(),
        is_task_container: false,
    }
}

fn start_task(logger: &InterleavedEventLogger, containers: Vec<TaskContainerInfo>) {
    logger.post(TaskEvent::TaskStarting {
        task: "test".into(),
    });
    logger.post(TaskEvent::TaskGraphResolved { containers });
}

#[test]
fn declares_the_interleaved_io_policy() {
    let (logger, _) = logger();
    assert_eq!(
        logger.container_io_streaming(),
        ContainerIoStreaming::Interleaved
    );
}

/// The deliberate asymmetry with `simple` mode, which drops exactly
/// these three for the task's own container (see
/// `SimpleEventLogger::is_task_container`): here every line is prefixed
/// and line-buffered, so a readiness milestone can't land mid-line in
/// the container's own output — there's nothing to protect it from, and
/// this is the only style that reports them at all. Matches Batect's own
/// `InterleavedEventLogger`, whose equivalent handlers have no
/// task-container guard either.
#[test]
fn readiness_milestones_are_reported_for_the_tasks_own_container_too() {
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            TaskContainerInfo {
                is_task_container: true,
                ..info("app", Some("alpine:3"), None)
            },
            info("db", Some("redis:7"), None),
        ],
    );
    logger.post(TaskEvent::ContainerBecameHealthy {
        container: "app".into(),
    });
    logger.post(TaskEvent::RunningSetupCommand {
        container: "app".into(),
        command: "./init.sh".into(),
        index: 1,
        total: 1,
    });
    logger.post(TaskEvent::SetupCommandsCompleted {
        container: "app".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             app  | Container became healthy.\n\
             app  | Running setup command ./init.sh (1 of 1)...\n\
             app  | Container has completed all setup commands.\n"
    );
}

#[test]
fn prefixes_are_padded_to_the_longest_name() {
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            info("db", Some("postgres:15"), None),
            info("app-server", Some("app:1"), None),
        ],
    );
    logger.post(TaskEvent::DependencyStarting {
        container: "db".into(),
    });
    logger.post(TaskEvent::ContainerOutput {
        container: "app-server".into(),
        line: "hello".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test       | Running test...\n\
             db         | Starting container...\n\
             app-server | hello\n"
    );
}

#[test]
fn prefix_padding_aligns_by_display_width_not_char_count() {
    // "数据库" is 3 `char`s but 6 terminal columns (2 per CJK
    // character) — padding by `char` count (Rust's own `{:width$}`)
    // would under-pad it by 3 columns and misalign every other
    // column; padding by display width keeps them lined up.
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            info("数据库", Some("postgres:15"), None),
            info("db", Some("redis:7"), None),
        ],
    );
    logger.post(TaskEvent::DependencyStarting {
        container: "数据库".into(),
    });
    logger.post(TaskEvent::DependencyStarting {
        container: "db".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test   | Running test...\n\
             数据库 | Starting container...\n\
             db     | Starting container...\n"
    );
}

#[test]
fn a_pull_fans_out_to_every_container_using_that_image() {
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            info("a", Some("shared:1"), None),
            info("b", Some("shared:1"), None),
            info("c", Some("other:1"), None),
        ],
    );
    logger.post(TaskEvent::ImagePullStarting {
        image: "shared:1".into(),
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "shared:1".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             a    | Pulling shared:1...\n\
             b    | Pulling shared:1...\n\
             a    | Pulled shared:1.\n\
             b    | Pulled shared:1.\n"
    );
}

#[test]
fn build_output_gets_an_inner_image_build_prefix() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("app", None, Some("proj-app"))]);
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "proj-app".into(),
        message: "Step 1/3 : FROM alpine".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             app  | Image build | Step 1/3 : FROM alpine\n"
    );
}

#[test]
fn setup_command_output_gets_an_inner_numbered_prefix() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::SetupCommandOutput {
        container: "db".into(),
        index: 2,
        line: "initialised".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             db   | Setup command 2 | initialised\n"
    );
}

#[test]
fn task_failed_before_the_graph_resolves_still_flushes_the_preamble() {
    // An infrastructure failure early enough that TaskGraphResolved
    // never posts (e.g. a `--use-network` validation failure) must not
    // leave this task's line unprinted forever — TaskFailed flushes the
    // deferred preamble itself in that case.
    let (logger, buffer) = logger();
    logger.post(TaskEvent::TaskStarting {
        task: "test".into(),
    });
    logger.post(TaskEvent::TaskFailed {
        task: "test".into(),
    });
    assert_eq!(buffer.contents(), "test | Running test...\n");

    // A second TaskFailed (shouldn't happen in practice, but the guard
    // must not double-print).
    logger.post(TaskEvent::TaskFailed {
        task: "test".into(),
    });
    assert_eq!(buffer.contents(), "test | Running test...\n");
}

#[test]
fn task_level_lines_use_the_task_name_prefix() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::RemovingNetwork);
    logger.post(TaskEvent::TaskFinished {
        task: "test".into(),
        exit_code: 0,
        duration: Duration::from_millis(2100),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             test | Cleaning up...\n\
             test | Removing task network...\n\
             test | test finished with exit code 0 in 2.1s.\n"
    );
}

#[test]
fn line_buffer_splits_on_newlines_and_strips_carriage_returns() {
    let mut buffer = LineBuffer::new();
    let mut lines: Vec<String> = Vec::new();
    buffer.push(b"partial", &mut |line: &str| lines.push(line.to_string()));
    assert!(lines.is_empty());
    buffer.push(b" line\r\nsecond\nthird", &mut |line: &str| {
        lines.push(line.to_string())
    });
    assert_eq!(lines, vec!["partial line", "second"]);
    buffer.flush(&mut |line: &str| lines.push(line.to_string()));
    assert_eq!(lines, vec!["partial line", "second", "third"]);
    // Nothing pending — flush again emits nothing.
    buffer.flush(&mut |_line: &str| panic!("nothing should be pending"));
}
