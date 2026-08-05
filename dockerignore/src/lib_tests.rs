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
use std::io::Cursor;

#[test]
fn read_all_empty_reader_returns_no_entries() {
    let result = read_ignore_file(Cursor::new("")).unwrap();
    assert!(result.is_empty());
}

/// Ported from `ignorefile_test.go`'s `TestReadAll`.
#[test]
fn read_all_matches_upstream_reference_case() {
    let content =
        "test1\n/test2\n/a/file/here\n\nlastfile\n# this is a comment\n! /inverted/abs/path\n!\n! ";

    let expected = vec![
        "test1".to_string(),
        "test2".to_string(),
        "a/file/here".to_string(),
        "lastfile".to_string(),
        "!inverted/abs/path".to_string(),
        "!".to_string(),
        "!".to_string(),
    ];

    let actual = read_ignore_file(Cursor::new(content)).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn read_all_strips_leading_bom() {
    let content = "\u{FEFF}foo\nbar";
    let actual = read_ignore_file(Cursor::new(content)).unwrap();
    assert_eq!(actual, vec!["foo".to_string(), "bar".to_string()]);
}

#[test]
fn read_all_ignores_comment_only_at_column_one() {
    let content = "  # not a comment, has leading whitespace\n# a real comment";
    let actual = read_ignore_file(Cursor::new(content)).unwrap();
    assert_eq!(
        actual,
        vec!["# not a comment, has leading whitespace".to_string()]
    );
}
