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
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        let pm = PatternMatcher::new(&[pattern.to_string()]).unwrap();
        pm.matches_or_parent_matches(path)
    }

    /// Ported verbatim from `patternmatcher_test.go`'s `TestMatches`
    /// (`tests` table, run via `MatchesOrParentMatches`) — the load-bearing
    /// verification that this port is behaviorally faithful to real
    /// Docker's `.dockerignore` matching, not just "looks right".
    #[test]
    fn matches_upstream_reference_cases() {
        let cases: &[(&str, &str, bool)] = &[
            ("**", "file", true),
            ("**", "file/", true),
            ("**/", "file", true), // weird one
            ("**/", "file/", true),
            ("**", "/", true),
            ("**/", "/", true),
            ("**", "dir/file", true),
            ("**/", "dir/file", true),
            ("**", "dir/file/", true),
            ("**/", "dir/file/", true),
            ("**/**", "dir/file", true),
            ("**/**", "dir/file/", true),
            ("dir/**", "dir/file", true),
            ("dir/**", "dir/file/", true),
            ("dir/**", "dir/dir2/file", true),
            ("dir/**", "dir/dir2/file/", true),
            ("**/dir", "dir", true),
            ("**/dir", "dir/file", true),
            ("**/dir2/*", "dir/dir2/file", true),
            ("**/dir2/*", "dir/dir2/file/", true),
            ("**/dir2/**", "dir/dir2/dir3/file", true),
            ("**/dir2/**", "dir/dir2/dir3/file/", true),
            ("**file", "file", true),
            ("**file", "dir/file", true),
            ("**/file", "dir/file", true),
            ("**file", "dir/dir/file", true),
            ("**/file", "dir/dir/file", true),
            ("**/file*", "dir/dir/file", true),
            ("**/file*", "dir/dir/file.txt", true),
            ("**/file*txt", "dir/dir/file.txt", true),
            ("**/file*.txt", "dir/dir/file.txt", true),
            ("**/file*.txt*", "dir/dir/file.txt", true),
            ("**/**/*.txt", "dir/dir/file.txt", true),
            ("**/**/*.txt2", "dir/dir/file.txt", false),
            ("**/*.txt", "file.txt", true),
            ("**/**/*.txt", "file.txt", true),
            ("a**/*.txt", "a/file.txt", true),
            ("a**/*.txt", "a/dir/file.txt", true),
            ("a**/*.txt", "a/dir/dir/file.txt", true),
            ("a/*.txt", "a/dir/file.txt", false),
            ("a/*.txt", "a/file.txt", true),
            ("a/*.txt**", "a/file.txt", true),
            ("a[b-d]e", "ae", false),
            ("a[b-d]e", "ace", true),
            ("a[b-d]e", "aae", false),
            ("a[^b-d]e", "aze", true),
            (".*", ".foo", true),
            (".*", "foo", false),
            ("abc.def", "abcdef", false),
            ("abc.def", "abc.def", true),
            ("abc.def", "abcZdef", false),
            ("abc?def", "abcZdef", true),
            ("abc?def", "abcdef", false),
            ("a\\\\", "a\\", true),
            ("**/foo/bar", "foo/bar", true),
            ("**/foo/bar", "dir/foo/bar", true),
            ("**/foo/bar", "dir/dir2/foo/bar", true),
            ("abc/**", "abc", false),
            ("abc/**", "abc/def", true),
            ("abc/**", "abc/def/ghi", true),
            ("**/.foo", ".foo", true),
            ("**/.foo", "bar.foo", false),
            ("a(b)c/def", "a(b)c/def", true),
            ("a(b)c/def", "a(b)c/xyz", false),
            ("a.|)$(}+{bc", "a.|)$(}+{bc", true),
            (
                "dist/proxy.py-2.4.0rc3.dev36+g08acad9-py3-none-any.whl",
                "dist/proxy.py-2.4.0rc3.dev36+g08acad9-py3-none-any.whl",
                true,
            ),
            (
                "dist/*.whl",
                "dist/proxy.py-2.4.0rc3.dev36+g08acad9-py3-none-any.whl",
                true,
            ),
            ("a\\*b", "a*b", true),
        ];

        for (pattern, path, expected) in cases {
            assert_eq!(
                matches(pattern, path),
                *expected,
                "pattern={pattern:?} path={path:?}"
            );
        }
    }

    /// The root-only-for-bare-patterns behavior specifically — the reason
    /// this port exists rather than reusing a `.gitignore` crate. Not in
    /// upstream's table verbatim, but directly implied by it (no bare,
    /// wildcard-free pattern in the table is ever checked against a path
    /// where it's nested more than one level deep and still expected to
    /// match).
    #[test]
    fn bare_pattern_only_excludes_at_the_root() {
        assert!(matches("node_modules", "node_modules/foo.js"));
        assert!(!matches("node_modules", "packages/foo/node_modules/bar.js"));
        assert!(matches(
            "**/node_modules",
            "packages/foo/node_modules/bar.js"
        ));
    }

    /// Ported from `TestMatches`'s `multiPatternTests`.
    #[test]
    fn multi_pattern_negation_matches_upstream_reference_cases() {
        let cases: &[(&[&str], &str, bool)] = &[
            (&["**", "!util/docker/web"], "util/docker/web/foo", false),
            (
                &["**", "!util/docker/web", "util/docker/web/foo"],
                "util/docker/web/foo",
                true,
            ),
            (
                &[
                    "**",
                    "!dist/proxy.py-2.4.0rc3.dev36+g08acad9-py3-none-any.whl",
                ],
                "dist/proxy.py-2.4.0rc3.dev36+g08acad9-py3-none-any.whl",
                false,
            ),
            (
                &["**", "!dist/*.whl"],
                "dist/proxy.py-2.4.0rc3.dev36+g08acad9-py3-none-any.whl",
                false,
            ),
        ];

        for (patterns, path, expected) in cases {
            let patterns: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
            let pm = PatternMatcher::new(&patterns).unwrap();
            assert_eq!(
                pm.matches_or_parent_matches(path),
                *expected,
                "patterns={patterns:?} path={path:?}"
            );
        }
    }

    /// Ported from `TestPatternMatchesFolderExclusions`/
    /// `TestPatternMatchesFolderWithSlashExclusions`/
    /// `TestPatternMatchesFolderWildcardExclusions`: an exclusion of a whole
    /// directory, followed by a re-inclusion of one file within it.
    #[test]
    fn negation_re_includes_a_file_within_an_excluded_directory() {
        for exclude in ["docs", "docs/", "docs/*"] {
            let pm =
                PatternMatcher::new(&[exclude.to_string(), "!docs/README.md".to_string()]).unwrap();
            assert!(
                !pm.matches_or_parent_matches("docs/README.md"),
                "exclude={exclude:?}"
            );
        }
    }

    #[test]
    fn exclusion_pattern_after_inclusion_wins() {
        let pm = PatternMatcher::new(&["*.go".to_string(), "!fileutils.go".to_string()]).unwrap();
        assert!(!pm.matches_or_parent_matches("fileutils.go"));
    }

    #[test]
    fn exclusion_pattern_before_inclusion_is_overridden() {
        let pm = PatternMatcher::new(&["!fileutils.go".to_string(), "*.go".to_string()]).unwrap();
        assert!(pm.matches_or_parent_matches("fileutils.go"));
    }

    #[test]
    fn new_strips_empty_patterns() {
        let pm = PatternMatcher::new(&["docs".to_string(), "config".to_string(), "".to_string()])
            .unwrap();
        assert_eq!(pm.len(), 2);
    }

    #[test]
    fn new_reports_exclusions() {
        let pm = PatternMatcher::new(&["docs".to_string(), "!docs/README.md".to_string()]).unwrap();
        assert!(pm.exclusions());
    }

    #[test]
    fn new_trims_whitespace_around_patterns() {
        let pm =
            PatternMatcher::new(&["docs".to_string(), "  !docs/README.md".to_string()]).unwrap();
        assert!(pm.exclusions());

        let pm =
            PatternMatcher::new(&["docs".to_string(), "!docs/README.md  ".to_string()]).unwrap();
        assert!(pm.exclusions());
    }

    #[test]
    fn new_errors_on_lone_exclamation_point() {
        let err = PatternMatcher::new(&["!".to_string()]).unwrap_err();
        assert_eq!(err, Error::EmptyExclusion);
    }

    /// Upstream's `TestMatchesOrParentMatchesMalformedPatternDoesNotPanicOnRepeatedCall`
    /// exercises a malformed pattern (`[Local-Only]/` — an invalid character
    /// class range, `l` > `O`) that Go's lazy compilation only rejects at
    /// match time. This port compiles eagerly, so the equivalent guarantee
    /// (never panics, never silently misbehaves) surfaces as a construction
    /// error instead — see the doc comment on [`Error`].
    #[test]
    fn malformed_pattern_is_rejected_at_construction() {
        let err = PatternMatcher::new(&["[Local-Only]/".to_string()]).unwrap_err();
        assert!(matches!(err, Error::InvalidPattern(_)));
    }

    /// Leading-slash normalization is `read_ignore_file`'s job (matching
    /// `ignorefile.ReadAll`), not `PatternMatcher::new`'s — a pattern handed
    /// to `new` directly with a leading `/` is a genuinely rooted/absolute
    /// pattern and won't match a relative query path, same as upstream.
    #[test]
    fn read_ignore_file_normalizes_leading_slash_so_patterns_match_relative_paths() {
        let patterns = crate::read_ignore_file(std::io::Cursor::new("/foo/bar")).unwrap();
        let pm = PatternMatcher::new(&patterns).unwrap();
        assert!(pm.matches_or_parent_matches("foo/bar"));
    }

    #[test]
    fn trailing_slash_matches_both_files_and_directories() {
        // Unlike plain .gitignore, a trailing slash is a no-op for Docker
        // (confirmed via moby/patternmatcher/ignorefile.ReadAll's use of
        // filepath.Clean, which drops trailing slashes) rather than
        // restricting the match to directories only.
        assert!(matches("build/", "build"));
    }
}
