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

//! Ports `patternmatcher.go` from
//! [`github.com/moby/patternmatcher`](https://github.com/moby/patternmatcher)
//! (Copyright 2012-2017 Docker, Inc., Apache License, Version 2.0) — see
//! this repository's own `NOTICE` file, and the crate root's `# Attribution`
//! section.

use regex::Regex;
use std::fmt;

/// A pattern was rejected either because it's syntactically invalid (e.g. an
/// unterminated character class), or because it's a lone `!` with nothing to
/// negate.
///
/// Ports `moby/patternmatcher`'s error cases. One deliberate difference:
/// upstream Go compiles a pattern's regex lazily, on first match, so a
/// malformed pattern can construct a `PatternMatcher` successfully and only
/// fail later. This port compiles eagerly in [`PatternMatcher::new`], so a
/// malformed pattern is rejected immediately — consistent with this
/// project's fail-fast conventions elsewhere.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPattern(String),
    EmptyExclusion,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidPattern(p) => write!(f, "invalid pattern '{p}'"),
            Error::EmptyExclusion => write!(f, "illegal exclusion pattern: \"!\""),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone)]
enum MatchType {
    Exact,
    /// Pattern ends in `**` (e.g. `dir/**`); `cleaned_pattern` still
    /// includes the trailing `**`, stripped at match time.
    Prefix,
    /// Pattern starts with `**` (e.g. `**/foo`, `**file`, or bare `**`);
    /// `cleaned_pattern` still includes the leading `**`, stripped at match
    /// time.
    Suffix,
    Regexp(Box<Regex>),
}

#[derive(Debug, Clone)]
struct Pattern {
    cleaned_pattern: String,
    exclusion: bool,
    match_type: MatchType,
}

impl Pattern {
    fn matches(&self, path: &str) -> bool {
        match &self.match_type {
            MatchType::Exact => path == self.cleaned_pattern,
            MatchType::Prefix => {
                let prefix = &self.cleaned_pattern[..self.cleaned_pattern.len() - 2];
                path.starts_with(prefix)
            }
            MatchType::Suffix => {
                let suffix = &self.cleaned_pattern[2..];
                if path.ends_with(suffix) {
                    return true;
                }
                suffix.starts_with('/') && path == &suffix[1..]
            }
            MatchType::Regexp(re) => re.is_match(path),
        }
    }
}

/// Port of `shouldEscape`: regex-special characters that aren't also
/// glob-special, so need escaping when carried into a generated regex.
fn should_escape(c: char) -> bool {
    matches!(c, '.' | '+' | '(' | ')' | '|' | '{' | '}' | '$')
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Kind {
    Exact,
    Prefix,
    Suffix,
    Regexp,
}

/// Port of `Pattern.compile`, always using `/` as the path separator (this
/// project doesn't target Windows path semantics anywhere else yet either).
fn compile(cleaned_pattern: &str) -> Result<MatchType, Error> {
    let mut reg_str = String::from("^");
    let mut kind = Kind::Exact;
    let chars: Vec<char> = cleaned_pattern.chars().collect();
    let mut i = 0;
    let mut first = true;

    while i < chars.len() {
        let ch = chars[i];
        i += 1;

        match ch {
            '*' if chars.get(i) == Some(&'*') => {
                i += 1;
                if chars.get(i) == Some(&'/') {
                    i += 1;
                }
                if i >= chars.len() {
                    if kind == Kind::Exact {
                        kind = Kind::Prefix;
                    } else {
                        reg_str.push_str(".*");
                        kind = Kind::Regexp;
                    }
                } else {
                    reg_str.push_str("(.*/)?");
                    kind = Kind::Regexp;
                }
                if first {
                    kind = Kind::Suffix;
                }
            }
            '*' => {
                reg_str.push_str("[^/]*");
                kind = Kind::Regexp;
            }
            '?' => {
                reg_str.push_str("[^/]");
                kind = Kind::Regexp;
            }
            c if should_escape(c) => {
                reg_str.push('\\');
                reg_str.push(c);
            }
            '\\' => {
                if i < chars.len() {
                    reg_str.push('\\');
                    reg_str.push(chars[i]);
                    i += 1;
                    kind = Kind::Regexp;
                } else {
                    reg_str.push('\\');
                }
            }
            '[' | ']' => {
                reg_str.push(ch);
                kind = Kind::Regexp;
            }
            c => reg_str.push(c),
        }

        first = false;
    }

    match kind {
        Kind::Exact => Ok(MatchType::Exact),
        Kind::Prefix => Ok(MatchType::Prefix),
        Kind::Suffix => Ok(MatchType::Suffix),
        Kind::Regexp => {
            reg_str.push('$');
            let re = Regex::new(&reg_str)
                .map_err(|_| Error::InvalidPattern(cleaned_pattern.to_string()))?;
            Ok(MatchType::Regexp(Box::new(re)))
        }
    }
}

/// A compiled set of `.dockerignore`-style patterns.
///
/// Construct with [`PatternMatcher::new`] (patterns typically come from
/// [`crate::read_ignore_file`]), then check paths with
/// [`matches_or_parent_matches`](Self::matches_or_parent_matches).
#[derive(Debug)]
pub struct PatternMatcher {
    patterns: Vec<Pattern>,
    exclusions: bool,
}

impl PatternMatcher {
    /// Compiles `patterns` in order. Later patterns take precedence over
    /// earlier ones for a given path — a `!`-prefixed pattern re-includes a
    /// path an earlier pattern excluded, matching `.dockerignore`'s
    /// last-match-wins rule.
    pub fn new(patterns: &[String]) -> Result<Self, Error> {
        let mut pm = PatternMatcher {
            patterns: Vec::new(),
            exclusions: false,
        };

        for raw in patterns {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut cleaned = path_clean::clean(trimmed).to_string_lossy().into_owned();

            let exclusion = cleaned.starts_with('!');
            if exclusion {
                if cleaned.len() == 1 {
                    return Err(Error::EmptyExclusion);
                }
                cleaned = cleaned[1..].to_string();
                pm.exclusions = true;
            }

            let match_type = compile(&cleaned)?;
            pm.patterns.push(Pattern {
                cleaned_pattern: cleaned,
                exclusion,
                match_type,
            });
        }

        Ok(pm)
    }

    /// Whether any pattern is a `!` exclusion (re-inclusion).
    pub fn exclusions(&self) -> bool {
        self.exclusions
    }

    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Returns whether `file` (a `/`-delimited relative path) matches any
    /// pattern, directly or via one of its parent directories — the modern
    /// (non-deprecated) port of `MatchesOrParentMatches`. This is *why* a
    /// bare pattern like `node_modules` only matches at the root: parent
    /// directories are checked as root-anchored prefixes
    /// (`parentPathDirs[..1]`, `[..2]`, ...), never as an isolated middle
    /// path component.
    pub fn matches_or_parent_matches(&self, file: &str) -> bool {
        let file = path_clean::clean(file).to_string_lossy().into_owned();
        if file == "." {
            return false;
        }

        let parent_path = parent_of(&file);
        let parent_dirs: Vec<&str> = if parent_path == "." {
            Vec::new()
        } else {
            parent_path.split('/').collect()
        };

        let mut matched = false;
        for pattern in &self.patterns {
            if pattern.exclusion != matched {
                continue;
            }

            let mut is_match = pattern.matches(&file);
            if !is_match && !parent_dirs.is_empty() {
                for i in 0..parent_dirs.len() {
                    is_match = pattern.matches(&parent_dirs[..=i].join("/"));
                    if is_match {
                        break;
                    }
                }
            }

            if is_match {
                matched = !pattern.exclusion;
            }
        }

        matched
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => {
            let dir = &path[..idx];
            if dir.is_empty() {
                "/".to_string()
            } else {
                path_clean::clean(dir).to_string_lossy().into_owned()
            }
        }
        None => ".".to_string(),
    }
}

#[cfg(test)]
#[path = "pattern_tests.rs"]
mod tests;
