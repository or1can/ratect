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

//! A panic hook that tells the user this is a bug, not their mistake, and
//! gives them the two things a good bug report needs that Rust's own default
//! hook doesn't: where to file it, and what platform it happened on.
//!
//! [`install`] wraps the previous hook (installed once, at the very start of
//! `main`, so it also wraps whatever hook a test harness or another library
//! may have already set) rather than replacing it — the default hook's own
//! location/message/backtrace output is exactly what it already is, and
//! reproducing it here would drift from Rust's own formatting the next time
//! that changes upstream.
//!
//! The backtrace hint only fires when `RUST_BACKTRACE` isn't already
//! enabled, since a report is far more useful with one and this is the one
//! thing the default hook is deliberately silent about *unless* it's set.
//!
//! [`run_main`] is the other half: it makes sure a panic *ends* the process.
//! A panic that unwinds out of `#[tokio::main]` drops the runtime on its way,
//! and dropping a runtime waits for its blocking threads — one of which, on
//! any run that forwards stdin, is parked in a `read()` that only more input
//! or EOF ends. That is why both `main`s already leave with
//! `std::process::exit` rather than returning; a panic skipped that exit and
//! left the process alive, its containers never cleaned up, until something
//! killed it (ratect#670). Only a panic on the main thread unwinds that far:
//! one in a spawned task ends that task alone, and tokio reports it to
//! whatever awaits it.

use std::panic;

/// What a panic exits with — Rust's own default for one.
const PANIC_EXIT_CODE: i32 = 101;

/// Installs the hook. `binary_name`/`version` are almost always
/// `env!("CARGO_PKG_NAME")`/`env!("CARGO_PKG_VERSION")` from the calling
/// binary's own `Cargo.toml` — passed in rather than read here so this stays
/// a `ratect-core` concern shared by both binaries, not tied to either one's
/// package metadata.
pub fn install(binary_name: &'static str, version: &'static str) {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let backtrace_enabled = std::env::var_os("RUST_BACKTRACE").is_some_and(|v| v != "0");
        // The one write that may panic: this already is one, and a failure
        // here can't be reported anywhere else.
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "{}",
                report_message(binary_name, version, backtrace_enabled)
            );
        }
    }));
}

/// Runs `main` to completion on a new multi-threaded runtime — the one
/// `#[tokio::main]` would build — and exits `101` without
/// dropping the runtime if it panics, so the panic can't hang the process
/// (see the module doc). [`install`] first, so the panic is reported before
/// the exit.
pub fn run_main(main: impl std::future::Future<Output = ()>) {
    let runtime = tokio::runtime::Runtime::new().expect("failed to start the async runtime");
    let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| runtime.block_on(main)));
    if outcome.is_err() {
        std::process::exit(PANIC_EXIT_CODE);
    }
}

fn report_message(binary_name: &str, version: &str, backtrace_enabled: bool) -> String {
    let mut message = format!(
        "\nThis is a bug in {binary_name} ({version}), not something you did wrong. \
         Please report it: https://github.com/or1can/ratect/issues/new\n\
         Platform: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    if !backtrace_enabled {
        message.push_str(
            "\nRe-run with RUST_BACKTRACE=1 set to include a full backtrace in your report.",
        );
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_message_names_the_binary_version_and_issue_tracker() {
        let message = report_message("ratect-compat", "0.30.0", true);
        assert!(message.contains("ratect-compat"));
        assert!(message.contains("0.30.0"));
        assert!(message.contains("https://github.com/or1can/ratect/issues/new"));
        assert!(message.contains("not something you did wrong"));
    }

    #[test]
    fn report_message_includes_the_current_platform() {
        let message = report_message("ratect", "0.9.0", true);
        assert!(message.contains(std::env::consts::OS));
        assert!(message.contains(std::env::consts::ARCH));
    }

    #[test]
    fn report_message_suggests_a_backtrace_when_not_already_enabled() {
        let message = report_message("ratect", "0.9.0", false);
        assert!(message.contains("RUST_BACKTRACE=1"));
    }

    #[test]
    fn report_message_omits_the_backtrace_suggestion_when_already_enabled() {
        let message = report_message("ratect", "0.9.0", true);
        assert!(!message.contains("RUST_BACKTRACE=1"));
    }
}
