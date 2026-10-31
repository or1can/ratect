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
use crate::ui::test_support::SharedBuffer;
use crate::ui::{Console, Text};

/// A console on `buffer`, uncoloured, that believes it writes to a
/// terminal iff `is_terminal`.
fn console(buffer: &SharedBuffer, is_terminal: bool) -> Console {
    Console::new(Box::new(buffer.clone()), false).on_terminal(is_terminal)
}

/// The state set for one test, under the lock, and clear again after —
/// even if the test fails.
struct Raw(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

impl Raw {
    fn set() -> Self {
        let lock = test_lock();
        set_active(true);
        Self(lock)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        set_active(false);
    }
}

#[test]
fn a_terminal_console_ends_every_line_with_a_carriage_return_in_raw_mode() {
    let _raw = Raw::set();
    let buffer = SharedBuffer::default();
    let console = console(&buffer, true);

    console.println(Text::raw("one\ntwo".to_string()));
    console.print(Text::raw("three\nfour\n".to_string()));

    assert_eq!(buffer.contents(), "one\r\ntwo\r\nthree\r\nfour\r\n");
}

#[test]
fn a_terminal_console_writes_bare_line_feeds_outside_raw_mode() {
    let _lock = test_lock();
    let buffer = SharedBuffer::default();
    let console = console(&buffer, true);

    console.println(Text::raw("one\ntwo".to_string()));
    console.print(Text::raw("three\n".to_string()));

    assert_eq!(buffer.contents(), "one\ntwo\nthree\n");
}

/// Raw mode starts only when stdin and stdout are terminals; stderr may
/// still be `2> log`, which must never get a `\r`.
#[test]
fn a_console_not_on_a_terminal_never_adds_a_carriage_return() {
    let _raw = Raw::set();
    let buffer = SharedBuffer::default();
    let console = console(&buffer, false);

    console.println(Text::raw("one\ntwo".to_string()));
    console.print(Text::raw("three\n".to_string()));

    assert_eq!(buffer.contents(), "one\ntwo\nthree\n");
}

/// Translation runs on what escaping produced: a `\r\n` Ratect wrote itself
/// stays one carriage return, and a `\r` in configuration stays the two
/// characters `\` `r` — so the line break after it still gets its own.
#[test]
fn translation_neither_doubles_a_carriage_return_nor_unescapes_one() {
    let _raw = Raw::set();
    let buffer = SharedBuffer::default();
    let console = console(&buffer, true);

    console.println(Text::raw("already\r\nthere".to_string()));
    console.println("from config\r\nforged");

    assert_eq!(
        buffer.contents(),
        "already\r\nthere\r\nfrom config\\r\r\nforged\r\n"
    );
}

#[test]
fn the_tracing_writer_translates_only_on_a_terminal_in_raw_mode() {
    let write = |is_terminal: bool| {
        let mut buffer = SharedBuffer::default();
        LineEndings::new(&mut buffer, is_terminal)
            .write_all(b"a\nb\r\n")
            .unwrap();
        buffer.contents()
    };

    {
        let _lock = test_lock();
        assert_eq!(write(true), "a\nb\r\n");
        assert_eq!(write(false), "a\nb\r\n");
    }
    let _raw = Raw::set();
    assert_eq!(write(true), "a\r\nb\r\n");
    assert_eq!(write(false), "a\nb\r\n");
}
