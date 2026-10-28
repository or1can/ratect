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

use super::*;

#[test]
fn format_duration_below_a_minute_is_seconds_with_one_decimal() {
    assert_eq!(format_duration(Duration::from_millis(3450)), "3.5s");
    assert_eq!(format_duration(Duration::from_millis(120)), "0.1s");
}

#[test]
fn format_duration_from_a_minute_up_includes_minutes() {
    assert_eq!(format_duration(Duration::from_secs(63)), "1m 3.0s");
    assert_eq!(format_duration(Duration::from_millis(150_500)), "2m 30.5s");
}

#[test]
fn allows_interactive_is_true_only_for_task_container_only() {
    assert!(ContainerIoStreaming::TaskContainerOnly.allows_interactive());
    assert!(!ContainerIoStreaming::Interleaved.allows_interactive());
}

/// Every `Color`'s SGR code, the same as Batect's `ConsoleColor` for each.
/// The `match` has no wildcard, so a new variant fails to compile until it
/// has a row here.
#[test]
fn console_colored_wraps_text_in_each_colors_sgr_code_only_when_enabled() {
    let expected_code = |color: Color| match color {
        Color::Red => "31",
        Color::Green => "32",
        Color::Yellow => "33",
        Color::Blue => "34",
        Color::Magenta => "35",
        Color::Cyan => "36",
    };
    let colored = Console::new(Box::new(std::io::sink()), true);
    let plain = Console::new(Box::new(std::io::sink()), false);
    for color in [
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
    ] {
        assert_eq!(
            colored.colored(color, "x"),
            format!("\x1b[{}mx\x1b[0m", expected_code(color)),
            "{color:?}"
        );
        assert_eq!(plain.colored(color, "x"), "x", "{color:?}");
    }
}

#[test]
fn console_bold_wraps_text_in_the_bold_sgr_only_when_enabled() {
    let colored = Console::new(Box::new(std::io::sink()), true);
    assert_eq!(colored.bold("x"), "\x1b[1mx\x1b[0m");
    let plain = Console::new(Box::new(std::io::sink()), false);
    assert_eq!(plain.bold("x"), "x");
}

#[test]
fn console_stdout_forced_on_colors_even_though_the_test_harness_captures_stdout() {
    // `cargo test`'s own stdout capture means `std::io::stdout().is_terminal()`
    // is false here — exactly the non-terminal case `ForcedOn` exists to
    // override. This only passes if it actually does.
    let console = Console::stdout(ColorMode::ForcedOn);
    assert_eq!(console.colored(Color::Green, "0"), "\x1b[32m0\x1b[0m");
}

#[test]
fn console_stdout_off_never_colors_regardless_of_the_terminal() {
    let console = Console::stdout(ColorMode::Off);
    assert_eq!(console.colored(Color::Green, "0"), "0");
}

/// stderr's colour is stderr's own decision: `Off` and `ForcedOn` apply to
/// it exactly as to stdout, and `Auto` asks stderr's `is_terminal()` —
/// which `cargo test`'s capture makes false here, so `Auto` is plain.
#[test]
fn console_stderr_decides_colour_from_its_own_stream_and_the_color_mode() {
    assert!(!Console::stderr(ColorMode::Off).color_enabled());
    assert!(Console::stderr(ColorMode::ForcedOn).color_enabled());
    assert_eq!(
        Console::stderr(ColorMode::Auto).color_enabled(),
        std::io::stderr().is_terminal()
    );
    assert!(Console::for_stream(ProgressStream::Stderr, ColorMode::ForcedOn).color_enabled());
    assert!(!Console::for_stream(ProgressStream::Stdout, ColorMode::Off).color_enabled());
}

/// The exact bytes of the two markers (decisions/0017): bold and colour
/// as one SGR sequence, the message in the default colour after it; a
/// notice carries none. Pinned byte for byte since nothing type-checks
/// an escape sequence (ratect#493).
#[test]
fn error_and_warning_markers_are_bold_red_and_bold_yellow_and_a_notice_is_unmarked() {
    let buffer = test_support::SharedBuffer::default();
    let console = Console::new(Box::new(buffer.clone()), true);
    console.error("it broke");
    console.warning("it might");
    console.notice("it happened");
    assert_eq!(
        buffer.contents(),
        "\x1b[1;31mError:\x1b[0m it broke\n\
         \x1b[1;33mWarning:\x1b[0m it might\n\
         it happened\n"
    );

    let plain_buffer = test_support::SharedBuffer::default();
    let plain = Console::new(Box::new(plain_buffer.clone()), false);
    plain.error("it broke");
    plain.warning("it might");
    assert_eq!(
        plain_buffer.contents(),
        "Error: it broke\nWarning: it might\n"
    );
}

/// `print_stderr_class` renders exactly the three stderr classes and says so,
/// and leaves every other event alone.
#[test]
fn print_stderr_class_renders_only_the_three_stderr_classes() {
    let buffer = test_support::SharedBuffer::default();
    let console = Console::new(Box::new(buffer.clone()), false);
    assert!(console.print_stderr_class(&TaskEvent::Error {
        message: "e".into()
    }));
    assert!(console.print_stderr_class(&TaskEvent::Warning {
        message: "w".into()
    }));
    assert!(console.print_stderr_class(&TaskEvent::Notice {
        message: "n".into()
    }));
    assert!(!console.print_stderr_class(&TaskEvent::CleanupStarting));
    assert_eq!(buffer.contents(), "Error: e\nWarning: w\nn\n");
}

/// `quiet` means no progress, not nothing: the three classes still print.
#[test]
fn the_quiet_logger_prints_the_stderr_classes_and_no_progress() {
    let buffer = test_support::SharedBuffer::default();
    let logger = QuietEventLogger::new(Console::new(Box::new(buffer.clone()), false));
    logger.post(TaskEvent::TaskStarting {
        task: "build".into(),
    });
    logger.post(TaskEvent::Notice {
        message: "Left in place (--no-cleanup): app (abc)".into(),
    });
    // Progress like any other, so silent here — ratect#364's recorded
    // divergence from Batect, which prints it even in `quiet`.
    logger.post(TaskEvent::PrerequisitesOnly { task: "all".into() });
    logger.post(TaskEvent::TaskFinished {
        task: "build".into(),
        exit_code: 0,
        duration: Duration::from_secs(1),
    });
    // `quiet` has no summary line of either form (ratect#473).
    logger.post(TaskEvent::TaskFailed {
        task: "build".into(),
        duration: Duration::from_secs(1),
    });
    assert_eq!(
        buffer.contents(),
        "Left in place (--no-cleanup): app (abc)\n"
    );
}

#[test]
fn console_println_writes_line_with_newline() {
    let buffer = test_support::SharedBuffer::default();
    let console = Console::new(Box::new(buffer.clone()), false);
    console.println("hello");
    console.println("world");
    assert_eq!(buffer.contents(), "hello\nworld\n");
}

/// A write a gone reader refused is still ignored — the command carries on —
/// but the console remembers it, so a run can ask whether its output still
/// has anywhere to go (ratect#670). Only a gone reader counts: any other
/// failure is not the user deciding they have read enough.
#[test]
fn console_remembers_a_reader_that_has_gone() {
    use test_support::FailingWriter;

    let console = Console::new(Box::new(test_support::SharedBuffer::default()), false);
    console.println("hello");
    assert!(!console.reader_gone());

    let console = Console::new(
        Box::new(FailingWriter(std::io::ErrorKind::BrokenPipe)),
        false,
    );
    assert!(!console.reader_gone(), "nothing has been written yet");
    console.println("hello");
    assert!(console.reader_gone());

    let console = Console::new(
        Box::new(FailingWriter(std::io::ErrorKind::BrokenPipe)),
        false,
    );
    console.write_raw("hello");
    assert!(console.reader_gone());

    let console = Console::new(Box::new(FailingWriter(std::io::ErrorKind::Other)), false);
    console.println("hello");
    assert!(!console.reader_gone());
}

#[test]
fn once_flag_fires_exactly_once_until_reset() {
    let mut flag = OnceFlag::default();
    assert!(flag.fire_once(), "first call should fire");
    assert!(!flag.fire_once(), "second call before reset should not");
    assert!(!flag.fire_once(), "neither should a third");
    flag.reset();
    assert!(flag.fire_once(), "fires again after reset");
}

#[test]
fn format_task_summary_colors_exit_code_by_outcome() {
    let console = Console::new(Box::new(std::io::sink()), true);
    assert_eq!(
        format_task_summary(&console, "build", 0, Duration::from_millis(2300)),
        "build finished with exit code \x1b[32m0\x1b[0m in 2.3s."
    );
    assert_eq!(
        format_task_summary(&console, "lint", 3, Duration::from_secs(61)),
        "lint finished with exit code \x1b[31m3\x1b[0m in 1m 1.0s."
    );
}

/// The summary line a task that failed to run ends with: the name bold, as
/// every user-supplied name is (decisions/0017), and no colour — it says
/// the run ended, never why (ratect#473).
#[test]
fn format_task_failed_summary_bolds_the_name_and_colours_nothing() {
    let console = Console::new(Box::new(std::io::sink()), true);
    assert_eq!(
        format_task_failed_summary(&console, "build", Duration::from_millis(2300)),
        "\x1b[1mbuild\x1b[0m failed after 2.3s."
    );
    let plain = Console::new(Box::new(std::io::sink()), false);
    assert_eq!(
        format_task_failed_summary(&plain, "lint", Duration::from_secs(61)),
        "lint failed after 1m 1.0s."
    );
}

/// `select_output_style(requested, no_color, &TerminalFacts { term,
/// stdout_is_terminal: tty, console_dimensions_available: dimensions },
/// Stdout)` shorthand for the decision-table tests below — the progress
/// console on stdout, so `tty` is the stream that matters.
fn auto(no_color: bool, tty: bool, term: Option<&str>, dimensions: bool) -> OutputStyle {
    let terminal = TerminalFacts {
        term: term.map(str::to_string),
        stdout_is_terminal: tty,
        stderr_is_terminal: false,
        console_dimensions_available: dimensions,
    };
    select_output_style(None, no_color, &terminal, ProgressStream::Stdout)
}

/// The decision is the progress console's stream's, not stdout's: with the
/// console on stderr (`ratect`), stdout redirected and stderr a terminal
/// still gets `fancy` — the pipe took the result, not the conversation —
/// and a terminal stdout with stderr redirected doesn't (decisions/0017).
#[test]
fn fancy_is_decided_by_the_progress_consoles_own_stream() {
    let facts = |stdout: bool, stderr: bool| TerminalFacts {
        term: Some("xterm".to_string()),
        stdout_is_terminal: stdout,
        stderr_is_terminal: stderr,
        console_dimensions_available: true,
    };
    let select =
        |facts: &TerminalFacts, progress| select_output_style(None, false, facts, progress);

    assert_eq!(
        select(&facts(false, true), ProgressStream::Stderr),
        OutputStyle::Fancy
    );
    assert_eq!(
        select(&facts(true, false), ProgressStream::Stderr),
        OutputStyle::Simple
    );
    assert_eq!(
        select(&facts(false, true), ProgressStream::Stdout),
        OutputStyle::Simple
    );
    assert_eq!(
        select(&facts(true, false), ProgressStream::Stdout),
        OutputStyle::Fancy
    );
}

/// The refusal names the stream it checked — stderr for `ratect`, stdout
/// for `ratect-compat` — so the advice fits the binary that gave it.
#[test]
fn an_explicit_fancy_refusal_names_the_progress_consoles_stream() {
    let terminal = TerminalFacts {
        term: Some("xterm".to_string()),
        stdout_is_terminal: false,
        stderr_is_terminal: false,
        console_dimensions_available: true,
    };
    for (progress, name) in [
        (ProgressStream::Stderr, "stderr"),
        (ProgressStream::Stdout, "stdout"),
    ] {
        let error = create_event_sink(
            Some(OutputStyle::Fancy),
            ColorMode::Off,
            &terminal,
            progress,
        )
        .err()
        .expect("fancy without an interactive console is refused");
        assert!(
            error.to_string().starts_with(&format!(
                "Fancy output requires an interactive console ({name} isn't"
            )),
            "{error}"
        );
    }
}

/// `create_event_sink(Some(Fancy), …)` on a stdout console with these facts
/// (stderr not a terminal), as `ratect-compat` builds it.
fn explicit_fancy(
    tty: bool,
    term: Option<&str>,
    dimensions: bool,
) -> anyhow::Result<Arc<dyn EventSink>> {
    let terminal = TerminalFacts {
        term: term.map(str::to_string),
        stdout_is_terminal: tty,
        stderr_is_terminal: false,
        console_dimensions_available: dimensions,
    };
    create_event_sink(
        Some(OutputStyle::Fancy),
        ColorMode::Off,
        &terminal,
        ProgressStream::Stdout,
    )
}

/// An explicit `fancy` asks less of the console than auto-selection does:
/// a terminal with `TERM` unset is very likely capable, and Batect renders
/// fancy there too (ratect#480).
#[test]
fn an_explicit_fancy_is_honoured_on_a_terminal_with_term_unset() {
    assert!(explicit_fancy(true, None, true).is_ok());
}

/// `TERM=dumb` says the terminal can't move the cursor, so an explicit
/// `fancy` is still refused there — a recorded divergence, since Batect
/// writes the escapes anyway — and the refusal says that's why.
#[test]
fn an_explicit_fancy_is_refused_on_a_dumb_terminal_naming_term() {
    let error = explicit_fancy(true, Some("dumb"), true)
        .err()
        .expect("TERM=dumb is refused");
    let message = error.to_string();
    assert!(message.contains("TERM is 'dumb'"), "{message}");
    assert!(message.contains("use --output simple"), "{message}");
    assert!(!message.contains("TERM is unset"), "{message}");
}

#[test]
fn an_explicit_fancy_is_refused_when_stdout_is_not_a_terminal() {
    let error = explicit_fancy(false, Some("xterm"), true)
        .err()
        .expect("a non-terminal stdout is refused");
    assert!(
        error.to_string().contains("stdout isn't a terminal"),
        "{error}"
    );
}

#[test]
fn an_explicit_fancy_is_refused_when_the_terminal_size_is_unknown() {
    let error = explicit_fancy(true, Some("xterm"), false)
        .err()
        .expect("unqueryable dimensions are refused");
    assert!(
        error
            .to_string()
            .contains("terminal size can't be determined"),
        "{error}"
    );
}

/// Picking the style picks the container I/O policy, as Batect's
/// `EventLoggerProvider` does: only `all` follows dependency output; every
/// other style streams the task container alone.
#[test]
fn only_all_output_streams_more_than_the_task_container() {
    let terminal = TerminalFacts {
        term: Some("xterm".to_string()),
        stdout_is_terminal: true,
        stderr_is_terminal: true,
        console_dimensions_available: true,
    };
    for (style, policy) in [
        (OutputStyle::Simple, ContainerIoStreaming::TaskContainerOnly),
        (OutputStyle::Quiet, ContainerIoStreaming::TaskContainerOnly),
        (OutputStyle::Fancy, ContainerIoStreaming::TaskContainerOnly),
        (OutputStyle::All, ContainerIoStreaming::Interleaved),
    ] {
        let sink = create_event_sink(
            Some(style),
            ColorMode::Off,
            &terminal,
            ProgressStream::Stdout,
        )
        .expect("every style can be built on an interactive console");
        assert_eq!(sink.container_io_streaming(), policy, "{style:?}");
    }
}

#[test]
fn an_explicit_request_always_wins() {
    let terminal = TerminalFacts {
        term: None,
        stdout_is_terminal: false,
        stderr_is_terminal: false,
        console_dimensions_available: false,
    };
    // Even on a console that couldn't support it — an explicitly
    // requested style is never second-guessed here (fancy's own
    // interactive-console requirement is enforced at wiring time, with
    // a clear error, not silently overridden).
    for style in [
        OutputStyle::Fancy,
        OutputStyle::Simple,
        OutputStyle::Quiet,
        OutputStyle::All,
    ] {
        assert_eq!(
            select_output_style(Some(style), true, &terminal, ProgressStream::Stdout),
            style
        );
    }
}

#[test]
fn interactive_console_defaults_to_fancy() {
    assert_eq!(
        auto(false, true, Some("xterm-256color"), true),
        OutputStyle::Fancy
    );
}

#[test]
fn each_non_interactive_signal_alone_forces_simple() {
    // stdout isn't a terminal (piped/redirected/CI).
    assert_eq!(auto(false, false, Some("xterm"), true), OutputStyle::Simple);
    // TERM unset.
    assert_eq!(auto(false, true, None, true), OutputStyle::Simple);
    // TERM=dumb.
    assert_eq!(auto(false, true, Some("dumb"), true), OutputStyle::Simple);
    // Terminal dimensions unavailable.
    assert_eq!(auto(false, true, Some("xterm"), false), OutputStyle::Simple);
}

#[test]
fn no_color_forces_the_default_to_simple_even_on_an_interactive_console() {
    assert_eq!(auto(true, true, Some("xterm"), true), OutputStyle::Simple);
}

#[test]
fn resolve_no_color_is_false_when_neither_the_flag_nor_the_env_var_is_set() {
    assert!(!resolve_no_color(false, |_| None));
}

#[test]
fn resolve_no_color_is_true_when_the_flag_alone_is_set() {
    assert!(resolve_no_color(true, |_| None));
}

#[test]
fn resolve_no_color_is_true_when_no_color_alone_is_set() {
    assert!(resolve_no_color(false, |name| (name == "NO_COLOR").then(String::new)));
}

#[test]
fn resolve_no_color_treats_no_color_as_set_regardless_of_its_value() {
    // Per the spec (https://no-color.org): present, regardless of value —
    // even an empty string, which the test above already covers, and a
    // value that would be "falsy" in most other conventions.
    assert!(resolve_no_color(false, |name| (name == "NO_COLOR").then(|| "0".to_string())));
}

#[test]
fn resolve_no_color_ignores_other_variables() {
    assert!(!resolve_no_color(false, |name| (name == "CLICOLOR_FORCE")
        .then(|| "1".to_string())));
}

fn env_var(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |queried| (queried == name).then(|| value.to_string())
}

#[test]
fn resolve_color_mode_is_auto_when_nothing_is_set() {
    assert_eq!(resolve_color_mode(false, |_| None), ColorMode::Auto);
}

#[test]
fn resolve_color_mode_is_forced_on_when_clicolor_force_is_set() {
    assert_eq!(
        resolve_color_mode(false, env_var("CLICOLOR_FORCE", "1")),
        ColorMode::ForcedOn
    );
}

#[test]
fn resolve_color_mode_treats_clicolor_force_0_as_not_forced() {
    // Unlike NO_COLOR (any value counts), CLICOLOR_FORCE=0 explicitly
    // means "don't force" — the convention `ripgrep`/`bat` already use.
    assert_eq!(
        resolve_color_mode(false, env_var("CLICOLOR_FORCE", "0")),
        ColorMode::Auto
    );
}

#[test]
fn resolve_color_mode_is_off_when_no_color_is_already_true_even_with_clicolor_force_set() {
    // no_color=true here stands in for either --no-color or NO_COLOR
    // already having resolved to true (see `resolve_no_color`) — an
    // explicit "no color" always wins over a forced "yes color".
    assert_eq!(
        resolve_color_mode(true, env_var("CLICOLOR_FORCE", "1")),
        ColorMode::Off
    );
}

#[test]
fn resolve_color_mode_is_off_when_no_color_is_true_and_clicolor_force_is_unset() {
    assert_eq!(resolve_color_mode(true, |_| None), ColorMode::Off);
}

#[test]
fn quiet_and_all_are_never_auto_selected() {
    // Exhaustively: the default is only ever Fancy or Simple.
    for no_color in [false, true] {
        for tty in [false, true] {
            for term in [None, Some("dumb"), Some("xterm")] {
                for dimensions in [false, true] {
                    let style = auto(no_color, tty, term, dimensions);
                    assert!(
                        style == OutputStyle::Fancy || style == OutputStyle::Simple,
                        "auto({no_color}, {tty}, {term:?}, {dimensions}) = {style:?}"
                    );
                }
            }
        }
    }
}
