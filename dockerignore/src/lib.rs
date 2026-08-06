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

//! A faithful port of Docker's own `.dockerignore` pattern matching
//! (`github.com/moby/patternmatcher`, which Docker's own documentation
//! cites as the reference implementation for `.dockerignore` semantics).
//!
//! Its matching rules are **not** the same as `.gitignore`'s: most notably,
//! a bare pattern with no wildcard (e.g. `node_modules`) only excludes it at
//! the build context root, not at every depth — `**/node_modules` is needed
//! for that. See [`PatternMatcher::matches_or_parent_matches`].
//!
//! # Attribution
//!
//! Ports `ignorefile/ignorefile.go` (this file's [`read_ignore_file`]) and
//! `patternmatcher.go` (the private `pattern` module) from
//! [`github.com/moby/patternmatcher`](https://github.com/moby/patternmatcher),
//! including its test suite, carried over as this crate's own tests. That
//! project is Copyright 2012-2017 Docker, Inc., licensed under the Apache
//! License, Version 2.0 (<https://www.apache.org/licenses/LICENSE-2.0>) —
//! see this repository's own `NOTICE` file.

mod pattern;

pub use pattern::{Error, PatternMatcher};

use std::io::{self, BufRead, BufReader, Read};

/// Reads a `.dockerignore`-format file and returns the list of patterns to
/// pass to [`PatternMatcher::new`].
///
/// Ports `moby/patternmatcher/ignorefile.ReadAll`: strips a leading UTF-8
/// BOM, skips `#`-prefixed comment lines (recognized only at the very start
/// of a line, before whitespace trimming — so `  # not a comment` is a
/// literal pattern), trims whitespace, lexically cleans each pattern
/// (collapsing `.`/`..`/repeated or trailing slashes), and strips a leading
/// `/` (Docker treats `/foo/bar` and `foo/bar` identically) — all while
/// preserving a leading `!` negation prefix around that normalization.
pub fn read_ignore_file(reader: impl Read) -> io::Result<Vec<String>> {
    let buf_reader = BufReader::new(reader);
    let mut excludes = Vec::new();

    for (index, line) in buf_reader.lines().enumerate() {
        let mut line = line?;
        if index == 0 {
            if let Some(stripped) = line.strip_prefix('\u{FEFF}') {
                line = stripped.to_string();
            }
        }
        if line.starts_with('#') {
            continue;
        }

        let mut pattern = line.trim().to_string();
        if pattern.is_empty() {
            continue;
        }

        let invert = pattern.starts_with('!');
        if invert {
            pattern = pattern[1..].trim().to_string();
        }

        if !pattern.is_empty() {
            pattern = path_clean::clean(&pattern).to_string_lossy().into_owned();
            if pattern.len() > 1 && pattern.starts_with('/') {
                pattern = pattern[1..].to_string();
            }
        }

        if invert {
            pattern = format!("!{pattern}");
        }

        excludes.push(pattern);
    }

    Ok(excludes)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
