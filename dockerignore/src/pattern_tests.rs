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
    let pm =
        PatternMatcher::new(&["docs".to_string(), "config".to_string(), "".to_string()]).unwrap();
    assert_eq!(pm.len(), 2);
}

#[test]
fn new_reports_exclusions() {
    let pm = PatternMatcher::new(&["docs".to_string(), "!docs/README.md".to_string()]).unwrap();
    assert!(pm.exclusions());
}

#[test]
fn new_trims_whitespace_around_patterns() {
    let pm = PatternMatcher::new(&["docs".to_string(), "  !docs/README.md".to_string()]).unwrap();
    assert!(pm.exclusions());

    let pm = PatternMatcher::new(&["docs".to_string(), "!docs/README.md  ".to_string()]).unwrap();
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
