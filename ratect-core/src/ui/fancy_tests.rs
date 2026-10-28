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

/// A logger whose progress console is the first buffer and whose stderr
/// console is the second. Color disabled so expectations stay readable —
/// bold/color are covered by Console's own tests; cursor movement is
/// emitted regardless (the independent-axes design).
fn logger_with_width_and_stderr(width: u16) -> (FancyEventLogger, SharedBuffer, SharedBuffer) {
    let buffer = SharedBuffer::default();
    let stderr = SharedBuffer::default();
    (
        FancyEventLogger {
            console: Console::new(Box::new(buffer.clone()), false),
            stderr: Console::new(Box::new(stderr.clone()), false),
            stderr_on_screen: true,
            fixed_width: AtomicU16::new(width),
            state: Mutex::new(State::default()),
        },
        buffer,
        stderr,
    )
}

fn logger_with_width(width: u16) -> (FancyEventLogger, SharedBuffer) {
    let (logger, buffer, _) = logger_with_width_and_stderr(width);
    (logger, buffer)
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

/// Regression test for ratect#73: if the terminal's width
/// changed since the last paint, a counted cursor-up can no longer be
/// trusted (the emulator may already have reflowed the old frame onto a
/// different number of rows) — this can't reproduce an actual reflow
/// (that's the terminal's own rendering, not anything `SharedBuffer`
/// simulates), only that `repaint_startup` stops *assuming* the old frame's
/// row count survived and instead starts a fresh block, which is safe
/// regardless of what the emulator actually did to the old one.
#[test]
fn a_terminal_resize_between_repaints_starts_a_fresh_block_instead_of_a_stale_cursor_up() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    let first_frame = buffer.contents();

    logger.fixed_width.store(40, Ordering::Relaxed);
    logger.post(TaskEvent::ImagePullStarting {
        image: "postgres:15".into(),
    });
    let after_resize = &buffer.contents()[first_frame.len()..];
    assert_eq!(
        after_resize,
        "\n\
             \r\x1b[2Kdb: pulling image postgres:15...\n\
             \r\x1b[2Kapp: ready to pull image app:1\n",
        "a width change since the last paint should start a fresh block \
             (a lone newline, no cursor-up) rather than move the cursor over \
             a frame whose on-screen row count may no longer be accurate"
    );

    // Subsequent repaints at the now-stable new width go back to a normal
    // in-place cursor-up, over the block just started above.
    logger.post(TaskEvent::ImagePullStarting {
        image: "app:1".into(),
    });
    assert!(
        buffer.contents().ends_with(
            "\x1b[2A\
             \r\x1b[2Kdb: pulling image postgres:15...\n\
             \r\x1b[2Kapp: pulling image app:1...\n"
        ),
        "{}",
        buffer.contents()
    );
}

/// A width change that the skip-unchanged-content optimization (see
/// `last_rendered`'s own docs) swallows — nothing was written for that
/// event at all, since the newly-clipped text happened to be byte-identical
/// to what's already on screen — must still update `painted_width`.
/// Otherwise the *next* real repaint compares against the stale
/// pre-resize width, sees a spurious mismatch, and takes the width-changed
/// fallback for a resize that produced no visible difference and needs no
/// such caution.
#[test]
fn a_width_change_with_no_visible_effect_does_not_leave_painted_width_stale() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    let before_resize = buffer.contents();

    // Narrow, but not enough to clip anything this line renders — the skip
    // optimization fires below, so this event writes nothing at all.
    logger.fixed_width.store(60, Ordering::Relaxed);
    logger.post(TaskEvent::ImageResolved {
        container: "app".into(),
    });
    assert_eq!(
        buffer.contents(),
        before_resize,
        "a no-op event at the new width should still skip the write, same as \
             `image_resolved_does_not_undo_progress_from_a_real_pull`"
    );

    // A genuine content change at that same (already-current) width must
    // use a normal cursor-up, not the width-changed fallback — the resize
    // above produced no visible difference, so there's nothing for that
    // fallback to protect against here.
    logger.post(TaskEvent::ContainerBecameHealthy {
        container: "app".into(),
    });
    assert!(
        buffer.contents()[before_resize.len()..].starts_with("\x1b[1A"),
        "painted_width should have followed the skipped repaint to 60, so \
             this repaint at the same width takes the normal cursor-up path, \
             not the fresh-block fallback: {:?}",
        &buffer.contents()[before_resize.len()..]
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
fn dependency_exiting_while_the_block_is_still_live_repaints_that_line_in_place() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 137,
    });
    // In place — the exact same two-line block shape as any other
    // in-place update (see `a_progress_event_repaints_the_block_in_place`),
    // not an extra line appended: an unrelated `println` here would
    // desync every repaint after it, the same row-count hazard ratect#73
    // names, just self-inflicted instead of terminal-inflicted.
    assert!(
        buffer.contents().ends_with(
            "\x1b[2A\
             \r\x1b[2Kdb: exited unexpectedly with exit code 137\n\
             \r\x1b[2Kapp: ready to pull image app:1\n"
        ),
        "{}",
        buffer.contents()
    );
}

/// A milestone, not a warning (decisions/0017): no `Warning:` marker, the
/// same phrase as the in-block line and as `simple`.
#[test]
fn dependency_exiting_after_the_block_has_frozen_prints_a_plain_milestone() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("cargo test".into()),
    });
    let after_freeze = buffer.contents();

    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 137,
    });

    // Appended as a plain line, not a repaint — nothing here should touch
    // the already-frozen block above it.
    assert_eq!(
        buffer.contents(),
        format!("{after_freeze}db exited unexpectedly with exit code 137.\n")
    );
}

/// A stderr-class line lands below the live block, so nothing may move the
/// cursor up over it: the block is painted afresh below the line on its
/// next update — not at once — and is live from there.
#[test]
fn a_warning_mid_startup_is_followed_by_a_fresh_block_on_the_next_update() {
    let (logger, buffer, stderr) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::ImagePullStarting {
        image: "app:1".into(),
    });
    let before = buffer.contents();

    logger.post(TaskEvent::Warning {
        message: "credential helper failed".into(),
    });
    assert_eq!(buffer.contents(), before, "nothing repainted at once");
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    let fresh = buffer.contents()[before.len()..].to_string();
    logger.post(TaskEvent::ImageResolved {
        container: "app".into(),
    });
    let after = buffer.contents()[before.len() + fresh.len()..].to_string();

    assert_eq!(stderr.contents(), "Warning: credential helper failed\n");
    assert!(
        !fresh.contains("\x1b[1A") && fresh.contains("\x1b[2Kapp: "),
        "painted fresh below the warning, no cursor-up: {fresh:?}"
    );
    assert!(
        after.is_empty() || after.starts_with("\x1b[1A"),
        "and live again in place from there: {after:?}"
    );
}

/// Several warnings in a row leave one fresh block below them, not one per
/// warning — the repaint waits for the next update.
#[test]
fn a_burst_of_warnings_is_followed_by_one_block() {
    let (logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    let before = buffer.contents();

    for registry in ["a", "b", "c"] {
        logger.post(TaskEvent::Warning {
            message: format!("registry {registry}"),
        });
    }
    logger.post(TaskEvent::ImagePullStarting {
        image: "app:1".into(),
    });

    let after = &buffer.contents()[before.len()..];
    assert_eq!(after.matches("app: ").count(), 1, "{after:?}");
}

/// The engine says when the run is over (`RunEnded`), and from then on the
/// startup block stops where it is — nothing is repainted below a line
/// reporting how the run ended. Whether a block is live comes from the
/// engine, never from the class of the line printed.
#[test]
fn after_run_ended_nothing_repaints_below_a_stderr_line() {
    for event in [
        TaskEvent::Notice {
            message: "Interrupted; not cleaning up.".into(),
        },
        TaskEvent::Warning {
            message: "late warning".into(),
        },
    ] {
        let (logger, buffer, stderr) = logger_with_width_and_stderr(120);
        logger.post(TaskEvent::TaskGraphResolved {
            containers: vec![info("app", Some("app:1"), &[], true)],
        });
        logger.post(TaskEvent::ImagePullStarting {
            image: "app:1".into(),
        });
        logger.post(TaskEvent::RunEnded { task: "t".into() });
        let before = buffer.contents();

        logger.post(event);
        logger.post(TaskEvent::ImagePullCompleted {
            image: "app:1".into(),
        });

        assert!(!stderr.contents().is_empty());
        assert_eq!(buffer.contents(), before);
    }
}

/// A notice mid-cleanup (the escalation rung's) is followed at once by the
/// countdown painted afresh below it — never a cursor-up over the notice,
/// and not deferred to the next cleanup step, which may be a whole forced
/// removal away or never come; from there it is live in place again.
#[test]
fn a_notice_during_cleanup_is_followed_by_a_fresh_countdown() {
    let (logger, buffer, stderr) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &[], true),
            info("db", Some("db:1"), &[], false),
        ],
    });
    for container in ["app", "db"] {
        logger.post(TaskEvent::ContainerCreated {
            container: container.into(),
        });
    }
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::CleanupStarting);
    let painted = buffer.contents();

    logger.post(TaskEvent::Notice {
        message: "Interrupted during cleanup; force-removing what's left.".into(),
    });
    let fresh = buffer.contents()[painted.len()..].to_string();
    logger.post(TaskEvent::ContainerRemoved {
        container: "app".into(),
    });
    let next = buffer.contents()[painted.len() + fresh.len()..].to_string();

    assert_eq!(
        stderr.contents(),
        "Interrupted during cleanup; force-removing what's left.\n"
    );
    assert!(
        fresh.starts_with("\nCleaning up: 2 containers") && !fresh.contains("\x1b[1A"),
        "{fresh:?}"
    );
    assert!(next.starts_with(CURSOR_UP_ONE_AND_CLEAR), "{next:?}");
}

/// A dependency's exit reported after cleanup has started — its watcher can
/// still be mid-report as cleanup begins — is printed below the countdown,
/// so the next update paints the countdown afresh below it rather than
/// moving up over the exit line and erasing it.
#[test]
fn a_dependency_exit_during_cleanup_is_not_painted_over() {
    let (logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &[], true),
            info("db", Some("db:1"), &[], false),
        ],
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::CleanupStarting);
    let painted = buffer.contents();

    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 1,
    });
    logger.post(TaskEvent::ContainerRemoved {
        container: "app".into(),
    });

    // The exit line, a fresh countdown below it, then an update that moves
    // up over the countdown only.
    assert_eq!(
        &buffer.contents()[painted.len()..],
        "db exited unexpectedly with exit code 1.\n\
         \nCleaning up: 1 container (app) left to remove...\n\
         \x1b[1A\r\x1b[2KCleaning up...\n"
    );
}

/// When cleanup finishes, the countdown is cleared — left on screen under
/// a line reporting what cleanup left, it would say cleanup was still under
/// way — and the failed summary line then prints in its place (ratect#473).
#[test]
fn the_countdown_is_cleared_when_cleanup_finishes() {
    let (logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::RemovingNetwork);
    let painted = buffer.contents();

    logger.post(TaskEvent::CleanupFinished);
    assert_eq!(&buffer.contents()[painted.len()..], CURSOR_UP_ONE_AND_CLEAR);

    logger.post(TaskEvent::TaskFailed {
        task: "t".into(),
        duration: Duration::from_millis(3200),
    });
    assert_eq!(
        &buffer.contents()[painted.len()..],
        format!("{CURSOR_UP_ONE_AND_CLEAR}t failed after 3.2s.\n")
    );
}

/// The failed summary replaces a still-live cleanup line in place, exactly
/// as the finished summary does (ratect#473).
#[test]
fn the_failed_summary_replaces_a_live_cleanup_line() {
    let (logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::RemovingNetwork);
    let painted = buffer.contents();

    logger.post(TaskEvent::TaskFailed {
        task: "t".into(),
        duration: Duration::from_millis(3200),
    });
    assert_eq!(
        &buffer.contents()[painted.len()..],
        format!("{CURSOR_UP_ONE_AND_CLEAR}t failed after 3.2s.\n")
    );
}

/// `TaskFailed` neither repaints nor touches the startup block, and leaves
/// it frozen for any later event (FEL-30/31/32/46): it only adds its line.
#[test]
fn task_failed_leaves_the_startup_block_alone() {
    let (logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    let painted = buffer.contents();

    logger.post(TaskEvent::TaskFailed {
        task: "t".into(),
        duration: Duration::from_millis(3200),
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });

    assert_eq!(
        &buffer.contents()[painted.len()..],
        "t failed after 3.2s.\n"
    );
}

/// Once cleanup has finished, a line reporting what it left (the
/// cleanup-failure error) is never followed by the countdown — it would
/// say cleanup is still running, under the line saying it gave up.
#[test]
fn nothing_repaints_the_countdown_after_cleanup_finished() {
    let (logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::CleanupFinished);
    let before = buffer.contents();

    logger.post(TaskEvent::Error {
        message: "Cleanup abandoned; left behind: app (abc).".into(),
    });
    logger.post(TaskEvent::ContainerRemoved {
        container: "app".into(),
    });
    assert_eq!(buffer.contents(), before);

    logger.post(TaskEvent::TaskFailed {
        task: "t".into(),
        duration: Duration::from_millis(3200),
    });
    assert_eq!(&buffer.contents()[before.len()..], "t failed after 3.2s.\n");
}

/// Under `ratect-compat` with stderr redirected, a stderr line never reaches
/// the screen, so the display's own cursor arithmetic still holds and it
/// carries on in place — no second copy of the block drawn below itself.
#[test]
fn a_stderr_line_off_screen_leaves_the_display_updating_in_place() {
    let (mut logger, buffer, _) = logger_with_width_and_stderr(120);
    logger.stderr_on_screen = false;
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    let before = buffer.contents();

    logger.post(TaskEvent::Warning {
        message: "to a file".into(),
    });
    logger.post(TaskEvent::ImagePullStarting {
        image: "app:1".into(),
    });

    let after = &buffer.contents()[before.len()..];
    assert!(after.starts_with("\x1b[1A"), "{after:?}");
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
    logger.post(TaskEvent::ContainerCreated {
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
/// `RunningTaskContainer` used to be posted *before* creation was even
/// attempted, so counting the container from that event left this stuck at
/// "1 container (app) left to remove..." for the whole cleanup, never
/// reaching the network line. Counting from `ContainerCreated` instead is
/// what fixes it — and it now follows creation, so a run posting it has a
/// container to count.
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
    // No ContainerCreated — creation failed.

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

/// A dependency Docker created but never started — its start failed, or a
/// sibling's failure dropped it — is still removed at cleanup, so it is
/// counted from `ContainerCreated`, not from `DependencyStarted`, which it
/// never reaches (ratect#499; Batect counts from its `ContainerCreatedEvent`
/// for the same reason). The task's own container was never created here,
/// so it isn't counted.
#[test]
fn a_dependency_created_but_never_started_is_counted_for_cleanup() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::ContainerCreating {
        container: "db".into(),
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "db".into(),
    });
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::CleanupStarting);

    assert!(
        buffer
            .contents()
            .ends_with("Cleaning up: 1 container (db) left to remove...\n"),
        "{:?}",
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
    logger.post(TaskEvent::ContainerCreated {
        container: "db".into(),
    });
    logger.post(TaskEvent::DependencyStarted {
        container: "db".into(),
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
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

/// Regression test for ratect#73, the cleanup line's own
/// copy of the same fixed-row-count shape: a resize since the line was last
/// painted must not attempt a one-row cursor-up over content that may have
/// already reflowed onto more rows.
#[test]
fn a_terminal_resize_since_the_cleanup_line_was_shown_starts_a_fresh_line() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::CleanupStarting);
    logger.post(TaskEvent::ContainerRemoved {
        container: "app".into(),
    });
    let before_resize = buffer.contents();

    logger.fixed_width.store(40, Ordering::Relaxed);
    logger.post(TaskEvent::RemovingNetwork);
    assert_eq!(
        &buffer.contents()[before_resize.len()..],
        "\nCleaning up: removing task network...\n",
        "a width change since the cleanup line was last painted should \
             move to a fresh line (no cursor-up) rather than overwrite a \
             line whose on-screen row count may no longer be accurate"
    );

    // The task-finished summary hits the very same fallback, one event
    // later, resizing again first.
    logger.fixed_width.store(30, Ordering::Relaxed);
    let before_finish = buffer.contents();
    logger.post(TaskEvent::TaskFinished {
        task: "test".into(),
        exit_code: 0,
        duration: Duration::from_millis(1500),
    });
    assert_eq!(
        &buffer.contents()[before_finish.len()..],
        "\ntest finished with exit code 0 in 1.5s.\n"
    );
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

/// A prerequisites-only task's line, Batect's words with its name in bold,
/// a blank line after the task before it (ratect#364).
#[test]
fn a_prerequisites_only_task_says_there_is_nothing_more_to_do() {
    let (logger, buffer) = logger_with_width(80);
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

    let buffer = SharedBuffer::default();
    let logger = FancyEventLogger {
        console: Console::new(Box::new(buffer.clone()), true),
        stderr: Console::new(Box::new(SharedBuffer::default()), true),
        stderr_on_screen: true,
        fixed_width: AtomicU16::new(80),
        state: Mutex::new(State::default()),
    };
    logger.post(TaskEvent::PrerequisitesOnly { task: "all".into() });
    assert_eq!(
        buffer.contents(),
        "The task \x1b[1mall\x1b[0m only defines prerequisite tasks, nothing more to do.\n"
    );
}
