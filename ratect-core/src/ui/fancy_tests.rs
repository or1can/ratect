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
        has_setup_commands: false,
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
    // Cut after the name's own reset, which already closed the style — so
    // nothing more is appended.
    assert_eq!(clip_to_width(styled, Some(10)), "\x1b[1mapp\x1b[0m: so...");
}

/// TRUN-18: a cut exactly at the end of a styled element puts the ellipsis
/// inside it — the escapes after the last kept character follow the `...`.
#[test]
fn clip_to_width_styles_the_ellipsis_like_the_last_kept_character() {
    assert_eq!(
        clip_to_width("\x1b[1mapp\x1b[0m: ready...", Some(6)),
        "\x1b[1mapp...\x1b[0m"
    );
}

/// TRUN-17: a cut inside a styled element keeps the style on the ellipsis,
/// closes it once, and drops every later element.
#[test]
fn clip_to_width_inside_a_styled_element_closes_it_after_the_ellipsis() {
    assert_eq!(
        clip_to_width("\x1b[1mlong-container-name\x1b[0m: ready", Some(10)),
        "\x1b[1mlong-co...\x1b[0m"
    );
    // An element that opens after the cut doesn't style the ellipsis.
    assert_eq!(
        clip_to_width("ab\x1b[1mcd\x1b[0m", Some(4)),
        "ab\x1b[1mcd\x1b[0m"
    );
    assert_eq!(clip_to_width("abc\x1b[1mde\x1b[0m", Some(4)), "a...");
}

/// TRUN-19, TRUN-20: at 3 columns or fewer only the ellipsis is left, cut to
/// fit, in the formatting of the text's first character.
#[test]
fn clip_to_width_below_four_columns_is_the_ellipsis_alone() {
    let styled = "\x1b[1mapp\x1b[0m: ready";
    assert_eq!(clip_to_width(styled, Some(3)), "\x1b[1m...\x1b[0m");
    assert_eq!(clip_to_width(styled, Some(2)), "\x1b[1m..\x1b[0m");
    assert_eq!(clip_to_width(styled, Some(1)), "\x1b[1m.\x1b[0m");
    let plain = "Cleaning up...";
    assert_eq!(clip_to_width(plain, Some(3)), "...");
    assert_eq!(clip_to_width(plain, Some(2)), "..");
    assert_eq!(clip_to_width(plain, Some(1)), ".");
    // Doesn't panic.
    clip_to_width(styled, Some(0));
}

/// TRUN-13, TRUN-22, CON-25/TRUN-34/SPD-8: what fits — exactly, or because
/// the width is unknown — comes back unchanged.
#[test]
fn clip_to_width_leaves_what_fits_unchanged() {
    let styled = "\x1b[1mapp\x1b[0m: \x1b[1mready\x1b[0m";
    assert_eq!(clip_to_width(styled, Some(10)), styled);
    assert_eq!(clip_to_width("", Some(5)), "");
    let long = format!("\x1b[1mapp\x1b[0m: {}", "x".repeat(10_000));
    assert_eq!(clip_to_width(&long, None), long);
}

/// Whatever the width, the result never takes more columns than it.
#[test]
fn clip_to_width_never_exceeds_the_width() {
    for text in [
        "\x1b[1mapp\x1b[0m: waiting for \x1b[1mdb\x1b[0m and \x1b[1mcache\x1b[0m",
        "Cleaning up: 2 containers left to remove...",
        "数据库: ready",
    ] {
        for width in 1..=display_width(text) as u16 + 2 {
            let clipped = clip_to_width(text, Some(width));
            assert!(
                display_width(&clipped) <= width as usize,
                "{clipped:?} at {width}"
            );
        }
    }
}

/// CPDL-14/CPD-1: the cleanup line is clipped like the startup block.
#[test]
fn the_cleanup_line_is_clipped_to_the_terminal_width() {
    let (logger, buffer) = logger_with_width(20);
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::CleanupStarting { cause: None });
    assert_eq!(buffer.contents(), "\nCleaning up: 1 co...\n");
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

/// The block's lines in Batect's order: a depth-first post-order of the
/// graph from the task container, each container's dependencies visited in
/// the order they were declared — so every container comes after what it
/// depends on, and the task container last.
fn painted_order(containers: Vec<TaskContainerInfo>) -> Vec<String> {
    let rows = containers.len();
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved { containers });
    last_frame(&buffer, rows)
        .into_iter()
        .map(|row| row.split(':').next().unwrap().to_string())
        .collect()
}

#[test]
fn graph_resolution_paints_one_line_per_container() {
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
fn the_block_follows_the_declared_order_of_dependencies() {
    assert_eq!(
        painted_order(vec![
            info("alpha", None, &[], false),
            info("app", None, &["zeta", "alpha"], true),
            info("zeta", None, &[], false),
        ]),
        ["zeta", "alpha", "app"]
    );
}

#[test]
fn the_block_puts_each_container_after_what_it_depends_on() {
    assert_eq!(
        painted_order(vec![
            info("alpha", None, &["zeta"], false),
            info("app", None, &["alpha"], true),
            info("zeta", None, &[], false),
        ]),
        ["zeta", "alpha", "app"]
    );
}

#[test]
fn the_block_paints_a_shared_dependency_once_where_first_reached() {
    assert_eq!(
        painted_order(vec![
            info("app", None, &["b", "c"], true),
            info("b", None, &["d"], false),
            info("c", None, &["d"], false),
            info("d", None, &[], false),
        ]),
        ["d", "b", "c", "app"]
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

/// The exit code is the user's own value, so it is bold (decisions/0017) —
/// on the live block's line and on the plain line printed once it froze.
#[test]
fn a_dependency_exit_code_is_bold_in_the_block_and_after_it_froze() {
    let (logger, buffer) = colored_logger(120);
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
    assert!(
        buffer.contents().contains(&format!(
            "{}: exited unexpectedly with exit code {}\n",
            bold("db"),
            bold("137")
        )),
        "{}",
        buffer.contents()
    );

    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    let after_freeze = buffer.contents();
    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 1,
    });
    assert_eq!(
        &buffer.contents()[after_freeze.len()..],
        format!("db exited unexpectedly with exit code {}.\n", bold("1"))
    );
}

/// Clipping counts only visible characters, so a line cut inside the bold
/// exit code keeps what fits, and closes the bold after the ellipsis.
#[test]
fn a_dependency_line_clipped_inside_its_bold_exit_code_closes_the_bold() {
    let (logger, buffer) = colored_logger(43);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 12345,
    });
    assert!(
        buffer.contents().contains(&format!(
            "{}: exited unexpectedly with exit code \x1b[1m1...\x1b[0m\n",
            bold("db")
        )),
        "{:?}",
        buffer.contents()
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
    logger.post(TaskEvent::CleanupStarting { cause: None });
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
    logger.post(TaskEvent::CleanupStarting { cause: None });
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
    logger.post(TaskEvent::CleanupStarting { cause: None });
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
    logger.post(TaskEvent::CleanupStarting { cause: None });
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
    logger.post(TaskEvent::CleanupStarting { cause: None });
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
    assert!(contents.contains("app: ready to start"), "{contents}");
}

#[test]
fn image_resolved_advances_a_stalled_line_when_no_pull_or_build_ever_fired() {
    // Without a pull or build actually happening this task (an
    // already-local image, or a resolution reused from an earlier task), a
    // line would otherwise sit at "ready to pull image X" until its
    // container starts — ImageResolved is the fallback signal that advances
    // it anyway.
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
    assert!(buffer.contents().contains("app: ready to start"));
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
        added.contains("app: ready to start"),
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

    logger.post(TaskEvent::CleanupStarting { cause: None });
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

    logger.post(TaskEvent::CleanupStarting { cause: None });
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
    logger.post(TaskEvent::CleanupStarting { cause: None });

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
    logger.post(TaskEvent::CleanupStarting { cause: None });
    // The task's own container counts too — it is removed during the
    // cleanup stage like any other (see `engine.rs`), so leaving it out
    // would under-report what is still to do. Batect's own
    // `CleanupProgressDisplayLine` counts it for the same reason.
    assert!(
        buffer
            .contents()
            .ends_with("Cleaning up: 2 containers (app and db) left to remove...\n"),
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
    logger.post(TaskEvent::CleanupStarting { cause: None });
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

/// A `build_directory` container: no image to pull, a tag its build output
/// is reported under.
fn built(name: &str, tag: &str, deps: &[&str], is_task: bool) -> TaskContainerInfo {
    TaskContainerInfo {
        image: None,
        build_tag: Some(tag.to_string()),
        ..info(name, None, deps, is_task)
    }
}

/// The text of the last `rows` rows painted — each row's whole content, with
/// its clear-line prefix (and any cursor-up before it) stripped — so a test
/// can assert every line of the current frame, not only the one an event
/// was meant to change.
fn last_frame(buffer: &SharedBuffer, rows: usize) -> Vec<String> {
    let contents = buffer.contents();
    let painted: Vec<&str> = contents.trim_end_matches('\n').split('\n').collect();
    painted[painted.len() - rows..]
        .iter()
        .map(|row| match row.rfind("\x1b[2K") {
            Some(at) => row[at + "\x1b[2K".len()..].to_string(),
            None => row.to_string(),
        })
        .collect()
}

/// Two build containers and the task container that depends on both —
/// alphabetical order and dependency order agree, task container last.
fn two_builds_and_a_task() -> (FancyEventLogger, SharedBuffer) {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            built("build-a", "p-build-a", &[], false),
            built("build-b", "p-build-b", &[], false),
            info("task", Some("task:1"), &["build-a", "build-b"], true),
        ],
    });
    (logger, buffer)
}

/// CSPLA-2, CSPLA-3, CSPLA-6: a build line is matched by its container's
/// name when the build starts and by its tag for build output, and no other
/// line moves.
#[test]
fn a_build_line_tracks_its_own_build_and_no_other() {
    let (logger, buffer) = two_builds_and_a_task();
    assert_eq!(
        last_frame(&buffer, 3),
        [
            "build-a: ready to build image",
            "build-b: ready to build image",
            "task: ready to pull image task:1",
        ]
    );

    logger.post(TaskEvent::ImageBuildStarting {
        container: "build-a".into(),
    });
    assert_eq!(
        last_frame(&buffer, 3),
        [
            "build-a: building image...",
            "build-b: ready to build image",
            "task: ready to pull image task:1",
        ]
    );

    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-build-a".into(),
        message: "Step 1/3 : FROM alpine".into(),
    });
    assert_eq!(
        last_frame(&buffer, 3),
        [
            "build-a: building image: Step 1/3 : FROM alpine",
            "build-b: ready to build image",
            "task: ready to pull image task:1",
        ]
    );
}

/// A blank build line (ratect#518 posts one per blank line of output)
/// leaves the last line with content as the build's status.
#[test]
fn an_empty_build_line_keeps_the_previous_status() {
    let (logger, buffer) = two_builds_and_a_task();
    logger.post(TaskEvent::ImageBuildStarting {
        container: "build-a".into(),
    });
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-build-a".into(),
        message: "compiling".into(),
    });
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-build-a".into(),
        message: String::new(),
    });
    assert_eq!(
        last_frame(&buffer, 3)[0],
        "build-a: building image: compiling"
    );
}

/// A BuildKit build's blank line arrives as its step's bare label
/// (ratect#538), which is as blank as an empty one.
#[test]
fn a_blank_labelled_build_line_keeps_the_previous_status() {
    let (logger, buffer) = two_builds_and_a_task();
    logger.post(TaskEvent::ImageBuildStarting {
        container: "build-a".into(),
    });
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-build-a".into(),
        message: "#4 compiling".into(),
    });
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-build-a".into(),
        message: "#4".into(),
    });
    assert_eq!(
        last_frame(&buffer, 3)[0],
        "build-a: building image: #4 compiling"
    );
}

/// CSPLA-7, -10, -11, -16, -21, CSPLB-3, -6, -8, -10: every event about
/// another container leaves a still-pending build line as it was.
#[test]
fn a_pending_build_line_ignores_every_other_containers_events() {
    let (logger, buffer) = two_builds_and_a_task();
    let other = || "build-b".to_string();
    for event in [
        TaskEvent::ImagePullStarting {
            image: "task:1".into(),
        },
        TaskEvent::ImagePullCompleted {
            image: "task:1".into(),
        },
        TaskEvent::ImageBuildCompleted { container: other() },
        TaskEvent::ImageResolved { container: other() },
        TaskEvent::DependencyStarting { container: other() },
        TaskEvent::DependencyStarted { container: other() },
        TaskEvent::ContainerBecameHealthy { container: other() },
        TaskEvent::RunningSetupCommand {
            container: other(),
            command: "true".into(),
            index: 1,
            total: 1,
            run_in: None,
        },
        TaskEvent::SetupCommandsCompleted { container: other() },
    ] {
        let description = format!("{event:?}");
        logger.post(event);
        assert_eq!(
            last_frame(&buffer, 3)[0],
            "build-a: ready to build image",
            "after {description}"
        );
    }
}

/// CSPLB-14, CSPLB-2: a dependency's start, then the wait for its health.
#[test]
fn a_dependency_shows_its_start_then_its_health_wait() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    logger.post(TaskEvent::DependencyStarting {
        container: "db".into(),
    });
    assert_eq!(
        last_frame(&buffer, 2),
        [
            "db: starting container...",
            "app: ready to pull image app:1"
        ]
    );
    logger.post(TaskEvent::DependencyStarted {
        container: "db".into(),
    });
    assert_eq!(
        last_frame(&buffer, 2),
        [
            "db: container started, waiting for it to become healthy...",
            "app: ready to pull image app:1"
        ]
    );
}

/// CSPLB-11: a task container with no command configured.
#[test]
fn a_task_container_with_no_command_shows_a_bare_running() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    assert!(
        buffer.contents().ends_with("\r\x1b[2Kapp: running\n\n"),
        "{:?}",
        buffer.contents()
    );
}

/// FEL-7, FEL-8: a dependency starting repaints the block and adds nothing
/// else — no blank line, no cleanup line.
#[test]
fn a_dependency_starting_adds_only_one_repaint() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("postgres:15"), &[], false),
        ],
    });
    let before = buffer.contents();
    logger.post(TaskEvent::DependencyStarting {
        container: "db".into(),
    });
    assert_eq!(
        &buffer.contents()[before.len()..],
        "\x1b[2A\
         \r\x1b[2Kdb: starting container...\n\
         \r\x1b[2Kapp: ready to pull image app:1\n"
    );
}

/// CSPLB-20, CSPLB-23: a pull line is matched by its own image only.
#[test]
fn a_pull_line_ignores_another_images_pull() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("a-dep", Some("image-a"), &[], false),
            info("b-dep", Some("image-c"), &[], false),
            info("task", Some("task:1"), &["a-dep", "b-dep"], true),
        ],
    });
    logger.post(TaskEvent::ImagePullStarting {
        image: "image-a".into(),
    });
    logger.post(TaskEvent::ImagePullProgress {
        image: "image-b".into(),
        message: "Downloading".into(),
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "image-b".into(),
    });
    assert_eq!(
        last_frame(&buffer, 3),
        [
            "a-dep: pulling image image-a...",
            "b-dep: ready to pull image image-c",
            "task: ready to pull image task:1",
        ]
    );
}

/// Batect's ELP-18: only the task container's output is streamed, straight
/// to the console — no dependency's, and nothing posted as events.
#[test]
fn declares_the_task_container_only_io_policy() {
    let (logger, _) = logger_with_width(120);
    assert_eq!(
        logger.container_io_streaming(),
        super::super::ContainerIoStreaming::TaskContainerOnly
    );
}

/// A multi-line command (a `|` block, trailing newline included) is drawn
/// on one row, each line break a space, as Batect's `descriptionWhenRunning`
/// draws it.
#[test]
fn a_multi_line_task_command_is_drawn_on_one_row() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("echo hello\n  world\n".into()),
    });
    assert!(
        buffer.contents().ends_with(
            "\x1b[1A\
             \r\x1b[2Kapp: running echo hello   world \n\
             \n"
        ),
        "{:?}",
        buffer.contents()
    );
}

/// A multi-line setup command is drawn on one row too, so the next frame's
/// cursor-up covers exactly the rows the last one painted. Batect fails the
/// task here instead — a divergence (decisions/0013 rule 3).
#[test]
fn a_multi_line_setup_command_keeps_one_row_per_container() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["dep"], true),
            info("dep", Some("dep:1"), &[], false),
        ],
    });
    let setup = |command: &str, index| TaskEvent::RunningSetupCommand {
        container: "dep".into(),
        command: command.into(),
        index,
        total: 2,
        run_in: None,
    };
    logger.post(setup("echo\none", 1));
    let first = buffer.contents();
    logger.post(setup("echo two", 2));
    let second = &buffer.contents()[first.len()..];

    assert!(
        first.ends_with(
            "\x1b[2A\
             \r\x1b[2Kdep: running setup command echo one (1 of 2)...\n\
             \r\x1b[2Kapp: ready to pull image app:1\n"
        ),
        "{first:?}"
    );
    assert_eq!(
        second,
        "\x1b[2A\
         \r\x1b[2Kdep: running setup command echo two (2 of 2)...\n\
         \r\x1b[2Kapp: ready to pull image app:1\n"
    );
}

/// The guard is the row's, not each stage's: build output and a carriage
/// return are flattened alike, before the row is clipped.
#[test]
fn every_row_is_flattened_before_it_is_clipped() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![built("app", "p-app", &[], true)],
    });
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-app".into(),
        message: "line one\nline two\rline three".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: building image: line one line two line three"]
    );

    logger.fixed_width.store(24, Ordering::Relaxed);
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "p-app".into(),
        message: "a\nb\nc\nd\ne\nf\ng\nh".into(),
    });
    assert_eq!(last_frame(&buffer, 1), ["app: building image: ..."]);
}

/// Three dependencies and the task container waiting on them, in an order
/// alphabetical and post-order agree on.
fn three_dependencies(app: TaskContainerInfo) -> (FancyEventLogger, SharedBuffer) {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("dep-a", Some("dep:1"), &[], false),
            info("dep-b", Some("dep:1"), &[], false),
            info("dep-c", Some("dep:1"), &[], false),
            app,
        ],
    });
    (logger, buffer)
}

fn healthy(container: &str) -> TaskEvent {
    TaskEvent::ContainerBecameHealthy {
        container: container.into(),
    }
}

/// CSPLA-18/-20: a dependency that was ready before the dependant began
/// waiting is not waited for — readiness is remembered, whichever order the
/// events come in.
#[test]
fn a_dependant_never_waits_for_a_dependency_already_ready() {
    let app = || info("z-app", Some("app:1"), &["dep-a", "dep-b", "dep-c"], true);

    let (logger, buffer) = three_dependencies(app());
    for dep in ["dep-a", "dep-b", "dep-c"] {
        logger.post(healthy(dep));
    }
    logger.post(TaskEvent::ImageResolved {
        container: "z-app".into(),
    });
    assert_eq!(last_frame(&buffer, 1), ["z-app: ready to start"]);

    let (logger, buffer) = three_dependencies(app());
    logger.post(healthy("dep-b"));
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["z-app: waiting for dependencies dep-a and dep-c to be ready..."]
    );

    let (logger, buffer) = three_dependencies(built(
        "z-app",
        "p-z-app",
        &["dep-a", "dep-b", "dep-c"],
        true,
    ));
    logger.post(healthy("dep-a"));
    logger.post(TaskEvent::ContainerCreated {
        container: "z-app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["z-app: waiting for dependencies dep-b and dep-c to be ready..."]
    );

    let (logger, buffer) = three_dependencies(app());
    logger.post(TaskEvent::DependencyCompleted {
        container: "dep-c".into(),
    });
    logger.post(TaskEvent::ImageResolved {
        container: "z-app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["z-app: waiting for dependencies dep-a and dep-b to be ready..."]
    );
}

/// Readiness belongs to one task: the next task's containers, even under the
/// same names, are waited for again.
#[test]
fn readiness_does_not_carry_over_to_the_next_task() {
    let (logger, buffer) = logger_with_width(120);
    let graph = || TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["db"], true),
            info("db", Some("db:1"), &[], false),
        ],
    };
    logger.post(TaskEvent::TaskStarting { task: "one".into() });
    logger.post(graph());
    logger.post(healthy("db"));
    logger.post(TaskEvent::TaskStarting { task: "two".into() });
    logger.post(graph());
    logger.post(TaskEvent::ImageResolved {
        container: "app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: waiting for dependency db to be ready..."]
    );
}

/// CSPLB-4: a dependency with setup commands is not ready when it becomes
/// healthy — it shows it is about to run them, and is still waited on —
/// and is shown ready once, after the last of them.
#[test]
fn a_dependency_with_setup_commands_is_ready_only_after_them() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("app", Some("app:1"), &["dep"], true),
            TaskContainerInfo {
                has_setup_commands: true,
                ..info("dep", Some("dep:1"), &[], false)
            },
        ],
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    logger.post(healthy("dep"));
    assert_eq!(
        last_frame(&buffer, 2),
        [
            "dep: running setup commands...",
            "app: waiting for dependency dep to be ready...",
        ]
    );

    for index in [1, 2] {
        logger.post(TaskEvent::RunningSetupCommand {
            container: "dep".into(),
            command: format!("seed {index}"),
            index,
            total: 2,
            run_in: None,
        });
    }
    assert_eq!(
        last_frame(&buffer, 2),
        [
            "dep: running setup command seed 2 (2 of 2)...",
            "app: waiting for dependency dep to be ready...",
        ]
    );

    logger.post(TaskEvent::SetupCommandsCompleted {
        container: "dep".into(),
    });
    assert_eq!(
        last_frame(&buffer, 2),
        ["dep: ready", "app: ready to start"]
    );
    assert_eq!(buffer.contents().matches("dep: ready\n").count(), 1);
}

/// A container whose health an `external_health_check` reports: its
/// companion's `ContainerBecameHealthy` names the checked container, which
/// is then treated like any other. The loader refuses setup commands on such
/// a container today, but nothing here depends on that.
#[test]
fn an_externally_checked_container_with_setup_commands_runs_them_first() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            TaskContainerInfo {
                has_setup_commands: true,
                ..info("checked", Some("db:1"), &[], false)
            },
            info("app", Some("app:1"), &["checked"], true),
        ],
    });
    logger.post(TaskEvent::DependencyStarted {
        container: "checked".into(),
    });
    logger.post(healthy("checked"));
    assert_eq!(
        last_frame(&buffer, 2)[0],
        "checked: running setup commands..."
    );
}

/// Batect's states between an image and a container: built, then being
/// created, then — once created — waiting for its dependencies.
#[test]
fn a_built_container_is_created_before_it_waits() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("db", Some("db:1"), &[], false),
            built("app", "p-app", &["db"], true),
        ],
    });
    logger.post(TaskEvent::ImageBuildCompleted {
        container: "app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: image built, ready to create container"]
    );
    logger.post(TaskEvent::ImageResolved {
        container: "app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: image built, ready to create container"]
    );
    logger.post(TaskEvent::ContainerCreating {
        container: "app".into(),
    });
    assert_eq!(last_frame(&buffer, 1), ["app: creating container..."]);
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: waiting for dependency db to be ready..."]
    );
}

/// A dependency is created like the task's own container, and one that
/// became ready while its dependant was being created is not waited for.
#[test]
fn a_dependency_is_created_too_and_readiness_survives_creation() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("db", Some("db:1"), &[], false),
            info("app", Some("app:1"), &["db"], true),
        ],
    });
    logger.post(TaskEvent::ContainerCreating {
        container: "db".into(),
    });
    logger.post(TaskEvent::ContainerCreating {
        container: "app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 2),
        ["db: creating container...", "app: creating container..."]
    );
    logger.post(healthy("db"));
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    assert_eq!(last_frame(&buffer, 2), ["db: ready", "app: ready to start"]);
}

/// Given the override the engine now reports as the line's image, the line
/// names it and follows its pull.
#[test]
fn an_overridden_image_is_the_one_named_and_tracked() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("alpine:3.19"), &[], true)],
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: ready to pull image alpine:3.19"]
    );
    logger.post(TaskEvent::ImagePullStarting {
        image: "alpine:3.19".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: pulling image alpine:3.19..."]
    );
    logger.post(TaskEvent::ImagePullProgress {
        image: "alpine:3.19".into(),
        message: "Downloading".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        ["app: pulling alpine:3.19: Downloading"]
    );
}

/// A logger like `logger_with_width`'s, but with colour on, for the tests
/// that pin which text is bold.
fn colored_logger(width: u16) -> (FancyEventLogger, SharedBuffer) {
    let buffer = SharedBuffer::default();
    (
        FancyEventLogger {
            console: Console::new(Box::new(buffer.clone()), true),
            stderr: Console::new(Box::new(SharedBuffer::default()), true),
            stderr_on_screen: true,
            fixed_width: AtomicU16::new(width),
            state: Mutex::new(State::default()),
        },
        buffer,
    )
}

fn bold(text: &str) -> String {
    format!("\x1b[1m{text}\x1b[0m")
}

/// Batect's `humanReadableList`: `a`, `a and b`, `a, b and c` — no Oxford
/// comma — each name bold and the separators not.
#[test]
fn name_lists_are_joined_with_and_and_each_name_is_bold() {
    let console = Console::new(Box::new(SharedBuffer::default()), false);
    let list = |names: &[&str]| {
        names_list(
            &console,
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
                .iter(),
        )
    };
    assert_eq!(list(&["a"]), "a");
    assert_eq!(list(&["a", "b"]), "a and b");
    assert_eq!(list(&["a", "b", "c"]), "a, b and c");
    assert_eq!(list(&["a", "b", "c", "d"]), "a, b, c and d");

    let colored = Console::new(Box::new(SharedBuffer::default()), true);
    assert_eq!(
        names_list(&colored, ["a".to_string(), "b".to_string()].iter()),
        format!("{} and {}", bold("a"), bold("b"))
    );
}

/// The cleanup countdown lists what is left sorted, joined with `and`, the
/// names bold.
#[test]
fn the_cleanup_line_joins_its_names_with_and() {
    let (logger, buffer) = colored_logger(120);
    for container in ["db", "app", "cache"] {
        logger.post(TaskEvent::ContainerCreated {
            container: container.into(),
        });
    }
    logger.post(TaskEvent::CleanupStarting { cause: None });
    assert_eq!(
        buffer.contents(),
        format!(
            "\nCleaning up: 3 containers ({}, {} and {}) left to remove...\n",
            bold("app"),
            bold("cache"),
            bold("db")
        )
    );
}

/// The waiting list keeps the order the dependencies were declared in —
/// as each becomes ready, too — joined with `and`, the names bold.
#[test]
fn the_waiting_list_is_in_declared_order_and_joined_with_and() {
    let (logger, buffer) = colored_logger(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("dep-c", None, &[], false),
            info("dep-a", None, &[], false),
            info("dep-b", None, &[], false),
            info("x-app", Some("app:1"), &["dep-c", "dep-a", "dep-b"], true),
        ],
    });
    logger.post(TaskEvent::ImageResolved {
        container: "x-app".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: waiting for dependencies {}, {} and {} to be ready...",
            bold("x-app"),
            bold("dep-c"),
            bold("dep-a"),
            bold("dep-b")
        )]
    );
    logger.post(healthy("dep-a"));
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: waiting for dependencies {} and {} to be ready...",
            bold("x-app"),
            bold("dep-c"),
            bold("dep-b")
        )]
    );
    logger.post(healthy("dep-c"));
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: waiting for dependency {} to be ready...",
            bold("x-app"),
            bold("dep-b")
        )]
    );
}

/// A clipped line with several bold names still ends its styling.
#[test]
fn a_clipped_line_of_bold_names_closes_its_style() {
    let text = format!(
        "{}: waiting for dependencies {}, {} and {} to be ready...",
        bold("app"),
        bold("first-dependency"),
        bold("second"),
        bold("third")
    );
    for width in 1..=display_width(&text) as u16 {
        let clipped = clip_to_width(&text, Some(width));
        let last = clipped.rfind('\x1b').map(|at| &clipped[at..]);
        assert!(
            last.is_none_or(|escape| escape.starts_with("\x1b[0m")),
            "{clipped:?} at {width}"
        );
        assert!(display_width(&clipped) <= width as usize);
    }
}

/// The setup command is bold, as in Batect, and so is the container it runs
/// in when that isn't the row's own.
#[test]
fn a_setup_command_is_bold() {
    let (logger, buffer) = colored_logger(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("dep", None, &[], false)],
    });
    logger.post(TaskEvent::RunningSetupCommand {
        container: "dep".into(),
        command: "./seed.sh".into(),
        index: 1,
        total: 2,
        run_in: None,
    });
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: running setup command {} (1 of 2)...",
            bold("dep"),
            bold("./seed.sh")
        )]
    );
    logger.post(TaskEvent::RunningSetupCommand {
        container: "dep".into(),
        command: "./seed.sh".into(),
        index: 2,
        total: 2,
        run_in: Some("db".into()),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: running setup command {} (2 of 2) in {}...",
            bold("dep"),
            bold("./seed.sh"),
            bold("db")
        )]
    );
}

/// What Batect's fancy bolds beyond names: the task's command and the image
/// while it is pulled — and the image before that too, since it is the
/// user's value (decisions/0017) — and never anything in the palette's
/// white.
#[test]
fn the_command_and_a_pulling_image_are_bold() {
    let (logger, buffer) = colored_logger(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: ready to pull image {}",
            bold("app"),
            bold("app:1")
        )]
    );
    logger.post(TaskEvent::ImagePullStarting {
        image: "app:1".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: pulling image {}...",
            bold("app"),
            bold("app:1")
        )]
    );
    logger.post(TaskEvent::ImagePullProgress {
        image: "app:1".into(),
        message: "Downloading".into(),
    });
    assert_eq!(
        last_frame(&buffer, 1),
        [format!(
            "{}: pulling {}: Downloading",
            bold("app"),
            bold("app:1")
        )]
    );
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("cargo test".into()),
    });
    assert!(
        buffer.contents().ends_with(&format!(
            "\r\x1b[2K{}: running {}\n\n",
            bold("app"),
            bold("cargo test")
        )),
        "{:?}",
        buffer.contents()
    );
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::CleanupStarting { cause: None });
    logger.post(TaskEvent::TaskFinished {
        task: "t".into(),
        exit_code: 0,
        duration: Duration::from_millis(100),
    });
    assert!(
        !buffer.contents().contains("\x1b[37m"),
        "{:?}",
        buffer.contents()
    );
}

/// A cut inside the bold command still closes its style, within the width.
#[test]
fn a_line_clipped_inside_a_bold_command_ends_its_style() {
    let (logger, buffer) = colored_logger(20);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("cargo test --workspace".into()),
    });
    let row = &last_frame(&buffer, 1)[0];
    assert_eq!(
        row,
        &format!("{}: running {}", bold("app"), "\x1b[1mcarg...\x1b[0m")
    );
    assert!(display_width(row) <= 20);
}

/// Runs a task container that writes `output` raw and then finishes with no
/// cleanup — `--no-cleanup-after-success` — returning what was printed
/// after the block froze.
fn finished_without_cleanup(output: &str) -> String {
    let (logger, mut buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "app".into(),
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    let frozen = buffer.contents();
    use std::io::Write;
    write!(buffer, "{output}").unwrap();
    logger.post(TaskEvent::RunEnded { task: "t".into() });
    logger.post(TaskEvent::TaskFinished {
        task: "t".into(),
        exit_code: 0,
        duration: Duration::from_millis(300),
    });
    buffer.contents()[frozen.len()..].to_string()
}

/// With cleanup off there is no cleanup line to separate the summary from
/// the task's output, so a line break does instead: it ends output that
/// didn't end its own line, and otherwise leaves Batect's blank line. No
/// cleanup line flashes up, as Batect's does, saying something will be
/// removed.
#[test]
fn the_summary_is_separated_from_task_output_without_cleanup() {
    assert_eq!(
        finished_without_cleanup("hello"),
        "hello\nt finished with exit code 0 in 0.3s.\n"
    );
    assert_eq!(
        finished_without_cleanup("hello\n"),
        "hello\n\nt finished with exit code 0 in 0.3s.\n"
    );
}

/// No task container ran, so there is no output to separate from.
#[test]
fn no_separator_when_the_task_container_never_ran() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    let before = buffer.contents();
    logger.post(TaskEvent::TaskFinished {
        task: "t".into(),
        exit_code: 0,
        duration: Duration::from_millis(300),
    });
    assert_eq!(
        &buffer.contents()[before.len()..],
        "t finished with exit code 0 in 0.3s.\n"
    );
}

/// The failed summary is separated the same way.
#[test]
fn the_failed_summary_is_separated_from_task_output_without_cleanup() {
    let (logger, mut buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![info("app", Some("app:1"), &[], true)],
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: None,
    });
    use std::io::Write;
    write!(buffer, "partial").unwrap();
    logger.post(TaskEvent::TaskFailed {
        task: "t".into(),
        duration: Duration::from_millis(300),
    });
    assert!(
        buffer
            .contents()
            .ends_with("partial\nt failed after 0.3s.\n"),
        "{:?}",
        buffer.contents()
    );
}

/// With colour off, the lines that bold a command, an image or names carry
/// no styling at all — the same text, byte for byte.
#[test]
fn with_colour_off_the_bold_lines_are_plain_text() {
    let (logger, buffer) = logger_with_width(120);
    logger.post(TaskEvent::TaskGraphResolved {
        containers: vec![
            info("dep", Some("dep:1"), &[], false),
            info("db", None, &[], false),
            info("app", Some("app:1"), &["dep", "db"], true),
        ],
    });
    assert_eq!(last_frame(&buffer, 3)[0], "dep: ready to pull image dep:1");
    logger.post(TaskEvent::ImagePullStarting {
        image: "dep:1".into(),
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "app:1".into(),
    });
    logger.post(TaskEvent::RunningSetupCommand {
        container: "db".into(),
        command: "./seed.sh".into(),
        index: 1,
        total: 1,
        run_in: Some("dep".into()),
    });
    assert_eq!(
        last_frame(&buffer, 3),
        [
            "dep: pulling image dep:1...",
            "db: running setup command ./seed.sh (1 of 1) in dep...",
            "app: waiting for dependencies dep and db to be ready...",
        ]
    );
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("cargo test".into()),
    });
    logger.post(TaskEvent::TaskFinished {
        task: "t".into(),
        exit_code: 0,
        duration: Duration::from_millis(100),
    });
    let contents = buffer.contents();
    assert!(
        contents.contains("\r\x1b[2Kapp: running cargo test\n"),
        "{contents:?}"
    );
    assert!(
        contents.ends_with("t finished with exit code 0 in 0.1s.\n"),
        "{contents:?}"
    );
    assert!(
        !contents.contains("\x1b[1m") && !contents.contains("\x1b[0m"),
        "{contents:?}"
    );
}
