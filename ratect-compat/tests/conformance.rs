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

//! Runs `ratect-compat` against test projects vendored verbatim from
//! Batect's own journey-test suite, asserting the same observable
//! behaviour Batect's acceptance tests do — the strongest evidence that
//! `ratect-compat` is a drop-in replacement, since the scenarios are
//! Batect's rather than ours. See `tests/conformance/README.md` for
//! provenance, licensing, and what is (and isn't) asserted.
//!
//! These need a real Docker daemon and are `#[ignore]`d, like the rest of
//! the end-to-end suite:
//!
//! ```text
//! cargo test -p ratect-compat --test conformance -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

fn ratect_command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ratect-compat"))
}

/// One vendored Batect project, and the behaviour running its task should
/// produce — the parts that are observable regardless of Batect's exact UI
/// wording (which `ratect-compat` deliberately diverges from). This is what
/// makes a Batect journey scenario portable: assert what the container did,
/// not how Batect framed it.
struct ConformanceCase {
    /// Directory name under `tests/conformance/batect-journey/`.
    project: &'static str,
    /// The task to run — Batect's journey projects conventionally call it
    /// `the-task`.
    task: &'static str,
    /// The exit code `ratect-compat` must return. Batect propagates the
    /// task container's own exit code, and so do we (`docker run`'s
    /// convention).
    expected_exit_code: i32,
    /// Substrings the task's own output must contain. Deliberately not an
    /// exact-transcript match: the milestone/framing lines around the
    /// output differ from Batect's, so only the container's own output is
    /// pinned.
    expected_output_contains: &'static [&'static str],
    /// Set when `ratect-compat`'s behaviour diverges from Batect's own
    /// journey assertion *on purpose* — a documented simplification, not a
    /// bug. Recording it here makes the difference an asserted fact and
    /// keeps `differences-from-batect.md` honest. `None` means "behaves
    /// exactly as Batect's own test asserts".
    divergence: Option<&'static str>,
}

fn project_dir(project: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/conformance/batect-journey")
        .join(project)
}

/// Runs one case and asserts its observable behaviour.
fn run_case(case: &ConformanceCase) {
    let output = ratect_command()
        .arg("-f")
        .arg(project_dir(case.project).join("batect.yml"))
        .arg(case.task)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", case.project));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let context = || {
        format!(
            "project {}{}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            case.project,
            case.divergence
                .map(|note| format!(" (expected divergence: {note})"))
                .unwrap_or_default(),
        )
    };

    assert_eq!(
        output.status.code(),
        Some(case.expected_exit_code),
        "exit code should match Batect's — {}",
        context()
    );
    for expected in case.expected_output_contains {
        assert!(
            stdout.contains(expected),
            "task output should contain {expected:?} — {}",
            context()
        );
    }
}

/// Batect's own `simple-task-using-image` journey scenario: a task that
/// prints a line and exits 123. Its Batect assertions are `output
/// shouldContain "This is some output from the task"` and `exitCode
/// shouldBe 123` — both purely behavioural, so they port unchanged. Proves
/// exact exit-code propagation end to end, against Batect's own project.
#[test]
#[ignore]
fn simple_task_using_image() {
    run_case(&ConformanceCase {
        project: "simple-task-using-image",
        task: "the-task",
        expected_exit_code: 123,
        expected_output_contains: &["This is some output from the task"],
        divergence: None,
    });
}
