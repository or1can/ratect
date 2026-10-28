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
use super::super::CleanupCause;
use super::super::TaskContainerInfo;
use super::*;
use std::time::Duration;

/// A logger over three buffers — progress, stderr and the result (stdout)
/// console the task container's own lines go to — colour off on all three
/// unless `color`: expectations stay readable, and prefix padding is the
/// structural thing worth asserting.
fn logger_with_streams(
    color: bool,
) -> (
    InterleavedEventLogger,
    SharedBuffer,
    SharedBuffer,
    SharedBuffer,
) {
    let buffer = SharedBuffer::default();
    let stderr = SharedBuffer::default();
    let result = SharedBuffer::default();
    let logger = InterleavedEventLogger::new(
        Console::new(Box::new(buffer.clone()), color),
        Console::new(Box::new(stderr.clone()), color),
        Console::new(Box::new(result.clone()), color),
    );
    (logger, buffer, stderr, result)
}

/// Only the result stream's reader counts: a closed stderr never stops a run
/// (ratect#669), and the progress console is not where the run's output goes.
#[test]
fn the_result_reader_is_gone_only_when_the_result_console_refused_a_write() {
    use super::super::test_support::FailingWriter;

    let broken = || {
        Console::new(
            Box::new(FailingWriter(std::io::ErrorKind::BrokenPipe)),
            false,
        )
    };
    let working = || Console::new(Box::new(SharedBuffer::default()), false);
    let task_container = TaskContainerInfo {
        is_task_container: true,
        has_setup_commands: false,
        ..info("app", Some("alpine"), None)
    };
    let output = || TaskEvent::ContainerOutput {
        container: "app".into(),
        line: "one".into(),
    };

    let logger = InterleavedEventLogger::new(working(), working(), broken());
    start_task(&logger, vec![task_container.clone()]);
    assert!(!logger.result_reader_gone(), "nothing written to it yet");
    logger.post(output());
    assert!(logger.result_reader_gone());

    let logger = InterleavedEventLogger::new(broken(), broken(), working());
    start_task(&logger, vec![task_container]);
    logger.post(output());
    logger.post(TaskEvent::Notice {
        message: "a notice".into(),
    });
    assert!(!logger.result_reader_gone());
}

fn logger() -> (InterleavedEventLogger, SharedBuffer) {
    let (logger, buffer, _, _) = logger_with_streams(false);
    (logger, buffer)
}

fn info(name: &str, image: Option<&str>, build_tag: Option<&str>) -> TaskContainerInfo {
    TaskContainerInfo {
        name: name.to_string(),
        image: image.map(str::to_string),
        build_tag: build_tag.map(str::to_string),
        dependencies: Vec::new(),
        is_task_container: false,
        has_setup_commands: false,
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
                has_setup_commands: false,
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
        run_in: None,
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

/// A `run_in` setup command (ratect#111): the prefix already names the
/// container whose gate this is, so only the container it actually runs in
/// is added — and only because it differs.
#[test]
fn a_run_in_setup_command_names_the_container_it_runs_in() {
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            TaskContainerInfo {
                is_task_container: true,
                has_setup_commands: false,
                ..info("app", Some("alpine:3"), None)
            },
            info("db", Some("postgres:16"), None),
        ],
    );
    logger.post(TaskEvent::RunningSetupCommand {
        container: "db".into(),
        command: "./seed.sh".into(),
        index: 1,
        total: 1,
        run_in: Some("db-seed-client".into()),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             db   | Running setup command ./seed.sh (1 of 1) in db-seed-client...\n"
    );
}

/// A container's creation prints nothing here — Batect's own interleaved
/// logger has no line for a create either; the container's first line is
/// its start.
#[test]
fn container_creation_prints_nothing() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::ContainerCreating {
        container: "db".into(),
    });
    logger.post(TaskEvent::ContainerCreated {
        container: "db".into(),
    });
    logger.post(TaskEvent::DependencyStarting {
        container: "db".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             db   | Starting container...\n"
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

/// Batect's `Building image...`/`Image built.`, under the container's own
/// prefix and without its inner `Batect | ` (a recorded divergence).
#[test]
fn an_image_build_starts_and_completes_under_the_containers_prefix() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("app", None, Some("proj-app"))]);
    logger.post(TaskEvent::ImageBuildStarting {
        container: "app".into(),
    });
    logger.post(TaskEvent::ImageBuildCompleted {
        container: "app".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             app  | Building image...\n\
             app  | Image built.\n"
    );
}

/// Batect's `Container started.` — posted for a dependency only, so the
/// task container never gets one, as in Batect.
#[test]
fn a_dependency_starting_says_its_container_started() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("database", Some("postgres:15"), None)]);
    logger.post(TaskEvent::DependencyStarted {
        container: "database".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test     | Running test...\n\
             database | Container started.\n"
    );
}

/// Silent, as Batect is: `ImagePullProgress` mirrors Batect ignoring
/// `ImagePullProgressEvent` in `all`; `ImageResolved` has no Batect
/// counterpart. Creation's silence is `container_creation_prints_nothing`'s;
/// network creation and the start of a health wait have no event at all, so
/// their silence is structural — the exhaustive match guards it.
#[test]
fn pull_progress_and_image_resolution_print_nothing() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    let before = buffer.contents();
    logger.post(TaskEvent::ImagePullProgress {
        image: "postgres:15".into(),
        message: "Downloading 3/7".into(),
    });
    logger.post(TaskEvent::ImageResolved {
        container: "db".into(),
    });
    assert_eq!(buffer.contents(), before);
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
fn a_run_ending_before_the_graph_resolves_still_flushes_the_preamble() {
    // An infrastructure failure early enough that TaskGraphResolved
    // never posts (e.g. a `--use-network` validation failure) must not
    // leave this task's line unprinted forever — RunEnded flushes the
    // deferred preamble itself in that case, before the `Error` event
    // that reports the failure.
    let (logger, buffer) = logger();
    logger.post(TaskEvent::TaskStarting {
        task: "test".into(),
    });
    logger.post(TaskEvent::RunEnded {
        task: "test".into(),
    });
    assert_eq!(buffer.contents(), "test | Running test...\n");

    // TaskFailed after it must not print it again, only its summary line.
    logger.post(TaskEvent::TaskFailed {
        task: "test".into(),
        duration: Duration::from_millis(3200),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
         test | test failed after 3.2s.\n"
    );
}

/// The failed summary goes where the finished one does: on the task's own
/// prefix, on the progress console (ratect#473).
#[test]
fn task_failed_prints_the_failed_summary_on_the_task_prefix() {
    let (logger, buffer, stderr, result) = logger_with_streams(false);
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::RunEnded {
        task: "test".into(),
    });
    let before = buffer.contents();
    logger.post(TaskEvent::TaskFailed {
        task: "test".into(),
        duration: Duration::from_millis(3200),
    });
    assert_eq!(
        &buffer.contents()[before.len()..],
        "test | test failed after 3.2s.\n"
    );
    assert_eq!(stderr.contents(), "");
    assert_eq!(result.contents(), "");
}

#[test]
fn task_level_lines_use_the_task_name_prefix() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::CleanupStarting { cause: None });
    logger.post(TaskEvent::CleanupStarting { cause: None });
    logger.post(TaskEvent::RemovingNetwork);
    logger.post(TaskEvent::NetworkRemoved);
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
             test | Task network removed.\n\
             test | test finished with exit code 0 in 2.1s.\n"
    );
}

/// A milestone, not a warning (decisions/0017): under its own prefix, with
/// no `Warning:` marker.
#[test]
fn dependency_exiting_unexpectedly_is_a_milestone_with_its_own_prefix() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::DependencyExitedUnexpectedly {
        container: "db".into(),
        exit_code: 137,
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             db   | exited unexpectedly with exit code 137.\n"
    );
}

/// The three stderr classes are unprefixed (ratect#367) and go to stderr,
/// whatever the progress console is.
#[test]
fn errors_warnings_and_notices_are_unprefixed_on_stderr() {
    let (logger, buffer, stderr, _) = logger_with_streams(false);
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::Error {
        message: "Cleanup failed: no".into(),
    });
    logger.post(TaskEvent::Warning {
        message: "careful".into(),
    });
    logger.post(TaskEvent::Notice {
        message: "Interrupted; cleaning up.".into(),
    });
    assert_eq!(
        stderr.contents(),
        "Error: Cleanup failed: no\nWarning: careful\nInterrupted; cleaning up.\n"
    );
    assert_eq!(buffer.contents(), "test | Running test...\n");
}

/// The task container's own output lines are the run's result, so they go
/// to the result console (stdout in both binaries), prefixed like every
/// other line; a dependency's follow the progress console. This is what
/// makes `ratect run x -o all 2>/dev/null` the task's own lines alone.
#[test]
fn the_task_containers_output_goes_to_the_result_console_and_a_dependencys_to_progress() {
    let (logger, buffer, _, result) = logger_with_streams(false);
    let mut app = info("app", Some("app:1"), None);
    app.is_task_container = true;
    start_task(&logger, vec![info("db", Some("postgres:15"), None), app]);
    logger.post(TaskEvent::ContainerOutput {
        container: "db".into(),
        line: "ready to accept connections".into(),
    });
    logger.post(TaskEvent::ContainerOutput {
        container: "app".into(),
        line: "built".into(),
    });
    assert_eq!(
        buffer.contents(),
        "test | Running test...\n\
             db   | ready to accept connections\n"
    );
    assert_eq!(result.contents(), "app  | built\n");
}

/// No white (SGR 37) or black (SGR 30) anywhere, bright or not — the
/// task-level prefix is bold in the default foreground, a container's is
/// bold in its palette colour, and a container the graph never named gets
/// the default foreground too (decisions/0017).
#[test]
fn no_prefix_is_ever_white_or_black() {
    let (logger, buffer, stderr, result) = logger_with_streams(true);
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::ContainerOutput {
        container: "db".into(),
        line: "hi".into(),
    });
    logger.post(TaskEvent::ContainerOutput {
        container: "unknown".into(),
        line: "hi".into(),
    });
    logger.post(TaskEvent::CleanupStarting { cause: None });
    let everything = format!(
        "{}{}{}",
        buffer.contents(),
        stderr.contents(),
        result.contents()
    );
    for code in [
        "\x1b[37m", "\x1b[30m", "\x1b[97m", "\x1b[90m", ";37m", ";30m",
    ] {
        assert!(
            !everything.contains(code),
            "{code:?} found in {everything:?}"
        );
    }
    assert!(
        buffer
            .contents()
            .starts_with("\x1b[1mtest\x1b[0m | Running \x1b[1mtest\x1b[0m...\n"),
        "{:?}",
        buffer.contents()
    );
    assert!(
        buffer.contents().contains("\x1b[1;36mdb  \x1b[0m | hi\n"),
        "{:?}",
        buffer.contents()
    );
    assert!(
        buffer.contents().contains("\x1b[1munknown\x1b[0m | hi\n"),
        "{:?}",
        buffer.contents()
    );
}

/// A container's prefix is one bold, coloured span with a single reset —
/// Batect's styling minus the separator, which stays unstyled
/// (decisions/0017: separators are never emphasised).
#[test]
fn a_prefix_is_one_styled_span_and_the_separator_is_plain() {
    let (logger, buffer, _, _) = logger_with_streams(true);
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    logger.post(TaskEvent::ContainerOutput {
        container: "db".into(),
        line: "hi".into(),
    });
    assert!(
        buffer
            .contents()
            .ends_with("\n\x1b[1;36mdb  \x1b[0m | hi\n"),
        "{:?}",
        buffer.contents()
    );
    assert!(!buffer.contents().contains("\x1b[0m\x1b[0m"));
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

#[test]
fn line_buffer_flushes_on_a_lone_carriage_return_mid_stream() {
    let mut buffer = LineBuffer::new();
    let mut lines: Vec<String> = Vec::new();
    buffer.push(
        b"progress: 1%\rprogress: 2%\rprogress: 3%\n",
        &mut |line: &str| lines.push(line.to_string()),
    );
    assert_eq!(lines, vec!["progress: 1%", "progress: 2%", "progress: 3%"]);
}

#[test]
fn line_buffer_folds_a_split_crlf_pair_into_one_line_break() {
    // The CRLF pair split across two separate `push` calls — Docker's
    // stream can chunk anywhere, including between the two bytes of one
    // pair — must still fold to a single line break, not two (which would
    // otherwise emit a spurious empty line between "first" and "second").
    let mut buffer = LineBuffer::new();
    let mut lines: Vec<String> = Vec::new();
    buffer.push(b"first\r", &mut |line: &str| lines.push(line.to_string()));
    assert!(
        lines.is_empty(),
        "a lone \\r isn't resolved until the next byte arrives"
    );
    buffer.push(b"\nsecond\n", &mut |line: &str| {
        lines.push(line.to_string())
    });
    assert_eq!(lines, vec!["first", "second"]);
}

#[test]
fn line_buffer_flushes_a_run_of_bare_carriage_returns_separately() {
    let mut buffer = LineBuffer::new();
    let mut lines: Vec<String> = Vec::new();
    buffer.push(b"a\r\r\rb\n", &mut |line: &str| {
        lines.push(line.to_string())
    });
    // Each bare `\r` is its own flush boundary — including the two
    // back-to-back ones with nothing between them, which flush an empty
    // line each, the same as `\n\n` already would.
    assert_eq!(lines, vec!["a", "", "", "b"]);
}

#[test]
fn line_buffer_drops_an_unresolved_trailing_carriage_return_on_flush() {
    let mut buffer = LineBuffer::new();
    let mut lines: Vec<String> = Vec::new();
    buffer.push(b"last", &mut |line: &str| lines.push(line.to_string()));
    buffer.push(b"\r", &mut |line: &str| lines.push(line.to_string()));
    assert!(
        lines.is_empty(),
        "a trailing \\r with the stream not yet ended isn't resolved"
    );
    buffer.flush(&mut |line: &str| lines.push(line.to_string()));
    assert_eq!(lines, vec!["last"]);
}

/// A prerequisites-only task's line, Batect's words with its name in bold,
/// on that task's own prefix — not the previous task's (ratect#364).
#[test]
fn a_prerequisites_only_task_says_there_is_nothing_more_to_do() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("database", Some("postgres:15"), None)]);
    logger.post(TaskEvent::PrerequisitesOnly { task: "all".into() });
    assert!(
        buffer.contents().ends_with(
            "\nall | The task all only defines prerequisite tasks, nothing more to do.\n"
        ),
        "{:?}",
        buffer.contents()
    );

    let (logger, buffer, _, _) = logger_with_streams(true);
    logger.post(TaskEvent::PrerequisitesOnly { task: "all".into() });
    assert_eq!(
        buffer.contents(),
        "\x1b[1mall\x1b[0m | The task \x1b[1mall\x1b[0m only defines prerequisite tasks, \
         nothing more to do.\n"
    );
}

/// The prefix palette (decisions/0017): cyan, yellow, green, magenta, blue,
/// by sorted name, wrapping at a sixth — and the task's own prefix bold in
/// the default foreground.
#[test]
fn prefix_colours_cycle_the_palette_in_sorted_name_order() {
    let (logger, buffer, _, _) = logger_with_streams(true);
    let names = ["f", "e", "d", "c", "b", "a"];
    start_task(
        &logger,
        names
            .iter()
            .map(|name| info(name, Some("x:1"), None))
            .collect(),
    );
    for name in names.iter().rev() {
        logger.post(TaskEvent::DependencyStarted {
            container: (*name).into(),
        });
    }
    assert_eq!(
        buffer.contents(),
        "\x1b[1mtest\x1b[0m | Running \x1b[1mtest\x1b[0m...\n\
         \x1b[1;36ma   \x1b[0m | Container started.\n\
         \x1b[1;33mb   \x1b[0m | Container started.\n\
         \x1b[1;32mc   \x1b[0m | Container started.\n\
         \x1b[1;35md   \x1b[0m | Container started.\n\
         \x1b[1;34me   \x1b[0m | Container started.\n\
         \x1b[1;36mf   \x1b[0m | Container started.\n"
    );
}

/// A container's colour doesn't depend on the order the graph lists it in.
#[test]
fn prefix_colours_dont_depend_on_graph_order() {
    let render = |containers: Vec<TaskContainerInfo>| {
        let (logger, buffer, _, _) = logger_with_streams(true);
        start_task(&logger, containers);
        for name in ["app", "cache", "db"] {
            logger.post(TaskEvent::DependencyStarted {
                container: name.into(),
            });
        }
        buffer.contents()
    };
    let app = || TaskContainerInfo {
        is_task_container: true,
        has_setup_commands: false,
        ..info("app", Some("app:1"), None)
    };
    let cache = || info("cache", Some("redis:7"), None);
    let db = || info("db", Some("postgres:15"), None);
    assert_eq!(
        render(vec![db(), cache(), app()]),
        render(vec![app(), cache(), db()])
    );
}

/// Bold on the user's own names and values in a status line — the command,
/// the setup command, the image, the task and its exit code — and nowhere
/// else (decisions/0017).
#[test]
fn status_lines_bold_the_users_own_names_and_values() {
    let (logger, buffer, _, _) = logger_with_streams(true);
    start_task(
        &logger,
        vec![
            info("a", Some("shared:1"), None),
            info("b", Some("shared:1"), None),
        ],
    );
    let before = buffer.contents().len();
    logger.post(TaskEvent::ImagePullStarting {
        image: "shared:1".into(),
    });
    logger.post(TaskEvent::ImagePullCompleted {
        image: "shared:1".into(),
    });
    logger.post(TaskEvent::RunningSetupCommand {
        container: "a".into(),
        command: "./init.sh".into(),
        index: 1,
        total: 2,
        run_in: Some("b".into()),
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "b".into(),
        command: Some("make".into()),
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "b".into(),
        command: None,
    });
    logger.post(TaskEvent::TaskFinished {
        task: "test".into(),
        exit_code: 0,
        duration: Duration::from_millis(2100),
    });
    logger.post(TaskEvent::TaskFinished {
        task: "test".into(),
        exit_code: 3,
        duration: Duration::from_millis(2100),
    });
    let a = "\x1b[1;36ma   \x1b[0m";
    let b = "\x1b[1;33mb   \x1b[0m";
    let task = "\x1b[1mtest\x1b[0m";
    assert_eq!(
        &buffer.contents()[before..],
        format!(
            "{a} | Pulling \x1b[1mshared:1\x1b[0m...\n\
             {b} | Pulling \x1b[1mshared:1\x1b[0m...\n\
             {a} | Pulled \x1b[1mshared:1\x1b[0m.\n\
             {b} | Pulled \x1b[1mshared:1\x1b[0m.\n\
             {a} | Running setup command \x1b[1m./init.sh\x1b[0m (1 of 2) in b...\n\
             {b} | Running \x1b[1mmake\x1b[0m...\n\
             {b} | Running...\n\
             {task} | {task} finished with exit code \x1b[1;32m0\x1b[0m in 2.1s.\n\
             {task} | {task} finished with exit code \x1b[1;31m3\x1b[0m in 2.1s.\n"
        )
    );
}

/// The inner `Setup command N | ` and `Image build | ` labels stay in the
/// default foreground, where Batect paints them white (decisions/0017).
#[test]
fn inner_labels_are_unstyled() {
    let (logger, buffer, _, _) = logger_with_streams(true);
    start_task(&logger, vec![info("app", None, Some("proj-app"))]);
    let before = buffer.contents().len();
    logger.post(TaskEvent::SetupCommandOutput {
        container: "app".into(),
        index: 1,
        line: "seeded".into(),
    });
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "proj-app".into(),
        message: "Step 1/3".into(),
    });
    let app = "\x1b[1;36mapp \x1b[0m";
    assert_eq!(
        &buffer.contents()[before..],
        format!(
            "{app} | Setup command 1 | seeded\n\
             {app} | Image build | Step 1/3\n"
        )
    );
}

/// Every line of a multi-line build chunk is its own prefixed line, carrying
/// `Image build | ` as Batect's build sink gives each one — a blank line
/// included.
#[test]
fn every_line_of_a_multi_line_build_message_is_prefixed() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("app", None, Some("proj-app"))]);
    let before = buffer.contents().len();
    logger.post(TaskEvent::ImageBuildProgress {
        tag: "proj-app".into(),
        message: "bk-line-1\nbk-line-2\n\nbk-line-3".into(),
    });
    assert_eq!(
        &buffer.contents()[before..],
        "app  | Image build | bk-line-1\n\
         app  | Image build | bk-line-2\n\
         app  | Image build | \n\
         app  | Image build | bk-line-3\n"
    );
}

/// A multi-line command (`command: |`) is split as Batect splits it: each
/// line prefixed, the text after it joining the last.
#[test]
fn every_line_of_a_multi_line_command_is_prefixed() {
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            TaskContainerInfo {
                is_task_container: true,
                has_setup_commands: false,
                ..info("app", Some("alpine:3"), None)
            },
            info("dep", Some("alpine:3"), None),
        ],
    );
    let before = buffer.contents().len();
    logger.post(TaskEvent::RunningSetupCommand {
        container: "dep".into(),
        command: "echo\none\n".into(),
        index: 1,
        total: 1,
        run_in: None,
    });
    logger.post(TaskEvent::RunningSetupCommand {
        container: "dep".into(),
        command: "echo\none".into(),
        index: 1,
        total: 1,
        run_in: None,
    });
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("echo\nhello\n".into()),
    });
    assert_eq!(
        &buffer.contents()[before..],
        "dep  | Running setup command echo\n\
         dep  | one\n\
         dep  |  (1 of 1)...\n\
         dep  | Running setup command echo\n\
         dep  | one (1 of 1)...\n\
         app  | Running echo\n\
         app  | hello\n\
         app  | ...\n"
    );
}

/// Each continuation line gets the same styled prefix as the first, and a
/// bold span split across lines stays bold on each, closed before every
/// newline so no prefix inherits it.
#[test]
fn a_continuation_line_keeps_its_prefix_and_its_styling() {
    let (logger, buffer, _, _) = logger_with_streams(true);
    start_task(&logger, vec![info("app", Some("alpine:3"), None)]);
    let before = buffer.contents().len();
    logger.post(TaskEvent::RunningTaskContainer {
        container: "app".into(),
        command: Some("echo\nhello\n".into()),
    });
    let app = "\x1b[1;36mapp \x1b[0m";
    assert_eq!(
        &buffer.contents()[before..],
        format!(
            "{app} | Running \x1b[1mecho\x1b[0m\n\
             {app} | \x1b[1mhello\x1b[0m\n\
             {app} | ...\n"
        )
    );
}

/// `split('\n')`, not `lines()`: a message ending in a newline gets a
/// trailing prefix-only line, as Batect's does. No production path sends
/// one today; this pins the contract.
#[test]
fn a_trailing_newline_is_a_prefix_only_line() {
    let (logger, buffer) = logger();
    start_task(&logger, vec![info("app", Some("alpine:3"), None)]);
    let before = buffer.contents().len();
    logger.post(TaskEvent::ContainerOutput {
        container: "app".into(),
        line: "last\n".into(),
    });
    assert_eq!(
        &buffer.contents()[before..],
        "app  | last\n\
         app  | \n"
    );
}

/// Each cleanup step on the prefix it belongs to (decisions/0017): a
/// container's stop and removal on its own, the network's on the task's.
/// Batect prints only `Cleaning up...` and each container's `Container
/// stopped.`.
#[test]
fn cleanup_reports_each_containers_stop_and_removal_and_the_networks() {
    let (logger, buffer) = logger();
    start_task(
        &logger,
        vec![
            TaskContainerInfo {
                is_task_container: true,
                has_setup_commands: false,
                ..info("app", Some("app:1"), None)
            },
            info("cache", Some("redis:7"), None),
            info("db", Some("postgres:15"), None),
        ],
    );
    let before = buffer.contents().len();
    logger.post(TaskEvent::CleanupStarting { cause: None });
    for container in ["app", "cache", "db"] {
        logger.post(TaskEvent::ContainerStopped {
            container: container.into(),
        });
        logger.post(TaskEvent::ContainerRemoved {
            container: container.into(),
        });
    }
    logger.post(TaskEvent::RemovingNetwork);
    logger.post(TaskEvent::NetworkRemoved);
    assert_eq!(
        &buffer.contents()[before..],
        "test  | Cleaning up...\n\
         app   | Container stopped.\n\
         app   | Container removed.\n\
         cache | Container stopped.\n\
         cache | Container removed.\n\
         db    | Container stopped.\n\
         db    | Container removed.\n\
         test  | Removing task network...\n\
         test  | Task network removed.\n"
    );
}

/// The opening cleanup line names the failure it follows, on the task's
/// prefix, the task and its exit code bold (decisions/0017).
#[test]
fn cleanup_names_a_failure_it_follows() {
    let (colored, buffer, _, _) = logger_with_streams(true);
    start_task(&colored, vec![info("db", Some("postgres:15"), None)]);
    let before = buffer.contents().len();
    colored.post(TaskEvent::CleanupStarting {
        cause: Some(CleanupCause::Exited {
            task: "test".into(),
            exit_code: 3,
        }),
    });
    assert_eq!(
        &buffer.contents()[before..],
        "\x1b[1mtest\x1b[0m | Cleaning up after \x1b[1mtest\x1b[0m exited with code \x1b[1m3\x1b[0m...\n"
    );

    let (logger, buffer) = logger();
    start_task(&logger, vec![info("db", Some("postgres:15"), None)]);
    let before = buffer.contents().len();
    logger.post(TaskEvent::CleanupStarting {
        cause: Some(CleanupCause::Failed {
            task: "test".into(),
        }),
    });
    assert_eq!(
        &buffer.contents()[before..],
        "test | Cleaning up after test failed...\n"
    );
}
