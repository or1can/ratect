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

fn logger_with_width(width: u16) -> (FancyEventLogger, SharedBuffer) {
    let buffer = SharedBuffer::default();
    // Color disabled so expectations stay readable — bold/color are
    // covered by Console's own tests; cursor movement is emitted
    // regardless (the independent-axes design).
    let console = Console::new(Box::new(buffer.clone()), false);
    (
        FancyEventLogger {
            console,
            fixed_width: Some(width),
            state: Mutex::new(State::default()),
        },
        buffer,
    )
}

fn info(name: &str, image: Option<&str>, deps: &[&str], is_task: bool) -> TaskContainerInfo {
    TaskContainerInfo {
        name: name.to_string(),
        image: image.map(str::to_string),
        build_tag: None,
        dependencies: deps.iter().map(|d| d.to_string()).collect(),
        is_task_container: is_task,
    }
}

#[test]
fn clip_to_width_truncates_visible_chars_only() {
    assert_eq!(clip_to_width("hello", Some(10)), "hello");
    assert_eq!(clip_to_width("hello world", Some(8)), "hello...");
    // Escape sequences don't count toward the width, and a clipped
    // styled string gets a trailing reset.
    let styled = "\x1b[1mapp\x1b[0m: something quite long";
    assert_eq!(clip_to_width(styled, Some(30)), styled);
    assert_eq!(
        clip_to_width(styled, Some(10)),
        "\x1b[1mapp\x1b[0m: so...\x1b[0m"
    );
}

#[test]
fn clip_to_width_uses_real_display_width_not_char_count() {
    // Each of these three CJK characters occupies 2 terminal columns —
    // a plain char count would under-measure this as 3, letting it
    // pass a width check it doesn't actually fit, and would keep too
    // many characters when truncating.
    let wide = "数据库: ready";
    assert_eq!(display_width("数据库"), 6);
    // Fits: 6 (name) + 2 (": ") + 5 ("ready") = 13 columns exactly.
    assert_eq!(clip_to_width(wide, Some(13)), wide);
    // Doesn't fit at 8: keep = 8 - 3 = 5 columns. "数"(2) + "据"(2) = 4
    // fits; adding "库"(2) would make 6 > 5, so it's dropped.
    assert_eq!(clip_to_width(wide, Some(8)), "数据...");
}

#[test]
fn graph_resolution_paints_one_line_per_container_task_container_last() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskStarting {
        task: "test".into(),
    });
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    assert_eq!(
        buffer.contents(),
        "Running test...\n\
             \r\x1b[2Kdb: ready to pull image postgres:15\n\
             \r\x1b[2Kapp: ready to pull image app:1\n"
    );
}

#[test]
fn a_progress_event_repaints_the_block_in_place() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::ImagePullStarting {
        image: "postgres:15".into(),
    });
    assert_eq!(
        buffer.contents(),
        "\r\x1b[2Kdb: ready to pull image postgres:15\n\
             \r\x1b[2Kapp: ready to pull image app:1\n\
             \x1b[2A\
             \r\x1b[2Kdb: pulling image postgres:15...\n\
             \r\x1b[2Kapp: ready to pull image app:1\n"
    );
}

#[test]
fn identical_progress_message_does_not_repaint() {
    // Docker resends the same coarse status text ("Downloading", say)
    // many times per layer while streaming — the actual byte-progress
    // detail changing underneath it lives in a field Ratect doesn't
    // render at all, so consecutive ImagePullProgress events with the
    // same message must not trigger a repaint each time.
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::ImagePullProgress {
        image: "app:1".into(),
        message: "Downloading".into(),
    });
    let after_first = buffer.contents();

    logger.post(TaskEvent::ImagePullProgress {
        image: "app:1".into(),
        message: "Downloading".into(),
    });
    assert_eq!(
        buffer.contents(),
        after_first,
        "an identical status message shouldn't repaint"
    );

    // A genuinely different message still repaints normally.
    logger.post(TaskEvent::ImagePullProgress {
        image: "app:1".into(),
        message: "Extracting".into(),
    });
    assert!(
        buffer.contents().len() > after_first.len(),
        "a changed status message should still repaint"
    );
}

#[test]
fn task_container_start_freezes_the_block_behind_a_blank_line() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("cargo test".into()),
    });
    let after_freeze = buffer.contents();
    assert!(after_freeze.ends_with(
        "\x1b[1A\
             \r\x1b[2Kapp: running cargo test\n\
             \n"
    ));

    // Once frozen, further progress events must not repaint.
    logger.post(TaskEvent::ImagePullProgress {
        image: "app:1".into(),
        message: "late".into(),
    });
    assert_eq!(buffer.contents(), after_freeze);
}

#[test]
fn dependency_becoming_healthy_unblocks_waiting_lines() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    assert!(buffer
        .contents()
        .contains("app: waiting for dependency db to be ready..."));
    logger.post(TaskEvent::ContainerBecameHealthy {
        container: "db".into(),
    });
    let contents = buffer.contents();
    assert!(contents.contains("db: ready"), "{contents}");
    assert!(contents.contains("app: waiting to start..."), "{contents}");
}

#[test]
fn image_resolved_advances_a_stalled_line_when_no_pull_or_build_ever_fired() {
    // Without a pull or build actually happening this task (an
    // already-local image, or a resolution reused from an earlier
    // task), a line would otherwise sit at "ready to pull image X" for
    // its entire dependency wait — ImageResolved is the fallback signal
    // that advances it anyway.
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &["db"], true)],
    });
    assert!(buffer.contents().contains("app: ready to pull image app:1"));

    logger.post(TaskEvent::ImageResolved {
        container: "app".into(),
    });
    assert!(
        buffer
            .contents()
            .contains("app: waiting for dependency db to be ready..."),
        "{}",
        buffer.contents()
    );
}

#[test]
fn image_resolved_does_not_undo_progress_from_a_real_pull() {
    // When a pull DID happen, ImagePullCompleted already advanced the
    // line (with whatever dependencies remain, some possibly already
    // healthy) — a later ImageResolved for the same container must not
    // reset that progress back to the full dependency list.
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    logger.post(TaskEvent::ContainerBecameHealthy {
        container: "db".into(),
    });
    assert!(buffer.contents().contains("app: waiting to start..."));
    let before = buffer.contents();

    // ImageResolved changes nothing visible here (app's line is
    // already past `Stage::Pending`) — the repaint-skip-when-unchanged
    // optimization (see `repaint_startup`'s own docs) correctly
    // suppresses the write entirely rather than repainting identical
    // content, so the buffer doesn't grow at all.
    logger.post(TaskEvent::ImageResolved {
        container: "app".into(),
    });
    assert_eq!(
        buffer.contents(),
        before,
        "a no-op ImageResolved shouldn't repaint anything"
    );

    // Confirm the guard genuinely didn't reset app's progress
    // underneath that suppressed repaint — force a further one (any
    // event touching db's own line) and check the newly repainted
    // frame specifically (not the whole history, which legitimately
    // contains an earlier "waiting for dependency db" line from before
    // `db` became healthy) still reflects app's satisfied state.
    logger.post(TaskEvent::DependencyStarting {
        container: "db".into(),
    });
    let added = &buffer.contents()[before.len()..];
    assert!(
        added.contains("app: waiting to start..."),
        "ImageResolved must not have reset app's already-satisfied dependencies: {added:?}"
    );
    assert!(
        !added.contains("app: waiting for dependency db to be ready..."),
        "ImageResolved must not have reset app's already-satisfied dependencies: {added:?}"
    );
}

#[test]
fn cleanup_starts_on_a_fresh_line_even_after_unterminated_container_output() {
    let (logger, mut buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    logger.post(TaskEvent::TaskContainerCreated {
        container: "app".into(),
    });
    // The task container's own output streams raw, outside this
    // logger's control (`docker.rs`) — simulate a final line with no
    // trailing newline landing directly on the console.
    use std::io::Write;
    write!(buffer, "answer: 42").unwrap();
    let before_cleanup = buffer.contents();

    logger.post(TaskEvent::CleanupStarting);
    let after_cleanup = buffer.contents();
    assert_eq!(
        after_cleanup,
        format!("{before_cleanup}\nCleaning up: 1 container (app) left to remove...\n"),
        "cleanup must move to a fresh line, not append to the unterminated \
             container output — otherwise the next repaint's cursor-up erases it"
    );

    // Prove the earlier output actually survives a further repaint —
    // the whole point of moving to a fresh line first.
    logger.post(TaskEvent::TaskFinished {
        task: "app".into(),
        exit_code: 0,
        duration: Duration::from_millis(100),
    });
    assert!(
        buffer.contents().contains("answer: 42"),
        "container output must not be erased by cleanup/summary repaints: {:?}",
        buffer.contents()
    );
}

/// A run whose task container is never created — a bad volume, or
/// Docker refusing `create_container` — has nothing of its own to clean
/// up, and the countdown must not wait on it.
///
/// `RunningTaskContainer` is posted *before* creation is even attempted,
/// so counting the container from that event left this stuck at
/// "1 container (app) left to remove..." for the whole cleanup, never
/// reaching the network line. Counting from `TaskContainerCreated`
/// instead is what fixes it.
#[test]
fn a_task_container_that_was_never_created_is_not_counted_for_cleanup() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    // No TaskContainerCreated — creation failed.

    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::RemovingNetwork);

    assert!(
        buffer
            .contents()
            .ends_with("Cleaning up: removing task network...\n"),
        "cleanup should move straight to the network, got: {:?}",
        buffer.contents()
    );
}

#[test]
fn cleanup_line_counts_down_then_summary_replaces_it() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::DependencyStarted {
        container: "db".into(),
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    logger.post(TaskEvent::TaskContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::CleanupStarting);
    // The task's own container counts too — it is removed during the
    // cleanup stage like any other (see `engine.rs`), so leaving it out
    // would under-report what is still to do. Batect's own
    // `CleanupProgressDisplayLine` counts it for the same reason.
    assert!(
        buffer
            .contents()
            .ends_with("Cleaning up: 2 containers (app, db) left to remove...\n"),
        "{:?}",
        buffer.contents()
    );

    // Task container first, then its dependency — the order `engine.rs`
    // actually removes them in.
    logger.post(TaskEvent::ContainerRemoved {
        container: "app".into(),
    });
    assert!(
        buffer
            .contents()
            .ends_with("Cleaning up: 1 container (db) left to remove...\n"),
        "{:?}",
        buffer.contents()
    );

    logger.post(TaskEvent::ContainerRemoved {
        container: "db".into(),
    });
    logger.post(TaskEvent::RemovingNetwork);
    assert!(buffer.contents().ends_with(
        "\x1b[1A\r\x1b[2K\
             Cleaning up: removing task network...\n"
    ));

    logger.post(TaskEvent::TaskFinished {
        task: "test".into(),
        exit_code: 0,
        duration: Duration::from_millis(1500),
    });
    assert!(buffer.contents().ends_with(
        "\x1b[1A\r\x1b[2K\
             test finished with exit code 0 in 1.5s.\n"
    ));
}

#[test]
fn a_second_task_resets_the_display_after_a_blank_separator() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskStarting {
        task: "prereq".into(),
    });
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::TaskStarting {
        task: "main".into(),
    });
    let contents = buffer.contents();
    assert!(
        contents.ends_with("\n\nRunning main...\n"),
        "expected a blank separator before the second task: {contents:?}"
    );
    // The new task starts with no lines — nothing repaints until its
    // own graph resolves.
    logger.post(TaskEvent::ImagePullProgress {
        image: "app:1".into(),
        message: "late".into(),
    });
    assert_eq!(buffer.contents(), contents);
}

#[test]
fn lines_are_clipped_to_the_terminal_width() {
    let (logger, buffer) = logger_with_width(20);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info(
            "a-container-with-a-really-long-name",
            Some("some-image:1"),
            &[],
            true,
        )],
    });
    assert_eq!(buffer.contents(), "\r\x1b[2Ka-container-with-...\n");
}
