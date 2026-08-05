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

#[test]
fn console_colors_only_when_enabled() {
    let colored = Console::new(Box::new(std::io::sink()), true);
    assert_eq!(colored.colored(Color::Green, "0"), "\x1b[32m0\x1b[0m");
    let plain = Console::new(Box::new(std::io::sink()), false);
    assert_eq!(plain.colored(Color::Red, "1"), "1");
}

#[test]
fn console_println_writes_line_with_newline() {
    let buffer = test_support::SharedBuffer::default();
    let console = Console::new(Box::new(buffer.clone()), false);
    console.println("hello");
    console.println("world");
    assert_eq!(buffer.contents(), "hello\nworld\n");
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

/// `select_output_style(requested, no_color, stdout_is_terminal, term,
/// console_dimensions_available)` shorthand for the decision-table
/// tests below.
fn auto(no_color: bool, tty: bool, term: Option<&str>, dimensions: bool) -> OutputStyle {
    select_output_style(None, no_color, tty, term, dimensions)
}

#[test]
fn an_explicit_request_always_wins() {
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
            select_output_style(Some(style), true, false, None, false),
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
