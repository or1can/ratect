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

use std::panic;

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
        eprintln!(
            "{}",
            report_message(binary_name, version, backtrace_enabled)
        );
    }));
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
