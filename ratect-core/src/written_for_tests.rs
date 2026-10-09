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
use std::path::PathBuf;

fn version(text: &str) -> Version {
    text.parse().unwrap()
}

/// A root file holding `content`, in a directory of its own.
fn root_file(name: &str, content: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("ratect-written-for-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

const ROW_0_12: BreakingChange = BreakingChange {
    version: "0.12.0",
    summary: "a backslash in an expression now escapes the next character",
};

const ROW_0_13: BreakingChange = BreakingChange {
    version: "0.13.0",
    summary: "a task's environment no longer inherits the host's",
};

#[test]
fn a_version_parses_with_or_without_a_pre_release_suffix() {
    assert_eq!(
        version("0.11.0"),
        Version {
            major: 0,
            minor: 11,
            patch: 0
        }
    );
    assert_eq!(version("0.11.2-dev"), version("0.11.2"));
}

#[test]
fn a_version_must_have_three_numeric_parts() {
    for text in [
        "0.11", "0.11.0.1", "v0.11.0", "0.x.0", "", "0.11.0-", "0.+11.0", "0..0",
    ] {
        assert!(text.parse::<Version>().is_err(), "{text:?} parsed");
    }
}

#[test]
fn a_version_displays_without_its_pre_release_suffix() {
    assert_eq!(version("0.11.0-dev").to_string(), "0.11.0");
}

#[test]
fn a_toml_root_declares_its_version() {
    let root = root_file(
        "ratect.toml",
        "ratect_version = \"0.11.0\"\nproject_name = \"p\"\n",
    );
    assert_eq!(
        read(&root).unwrap(),
        Some(WrittenFor::Declared(version("0.11.0")))
    );
}

#[test]
fn a_toml_root_without_the_field_is_unmarked() {
    let root = root_file("ratect.toml", "project_name = \"p\"\n");
    assert_eq!(read(&root).unwrap(), Some(WrittenFor::Unmarked));
}

#[test]
fn a_yaml_root_is_its_own_case() {
    let root = root_file("batect.yml", "project_name: p\n");
    assert_eq!(read(&root).unwrap(), Some(WrittenFor::YamlRoot));
}

/// Reading only the one field is what lets a newer project's unknown field
/// get this check's explanation rather than a parse error.
#[test]
fn a_field_this_binary_does_not_know_does_not_stop_the_read() {
    let root = root_file(
        "ratect.toml",
        "ratect_version = \"0.99.0\"\nproject_name = \"p\"\nfuture_field = 1\n",
    );
    assert_eq!(
        read(&root).unwrap(),
        Some(WrittenFor::Declared(version("0.99.0")))
    );
}

/// A file that can't be read or isn't TOML at all is the loader's to
/// report, in its own words — this check has nothing to add.
#[test]
fn an_unreadable_or_malformed_root_is_left_to_the_loader() {
    let missing =
        std::env::temp_dir().join(format!("ratect-missing-{}.toml", uuid::Uuid::new_v4()));
    assert_eq!(read(&missing).unwrap(), None);
    let malformed = root_file("ratect.toml", "project_name = \n");
    assert_eq!(read(&malformed).unwrap(), None);
}

#[test]
fn a_version_that_is_not_a_version_names_the_field() {
    for content in ["ratect_version = \"latest\"\n", "ratect_version = 11\n"] {
        let root = root_file("ratect.toml", content);
        let error = format!("{:#}", read(&root).unwrap_err());
        assert!(error.contains("'ratect_version'"), "{error}");
    }
}

#[test]
fn a_project_written_for_a_newer_ratect_is_refused() {
    let error = check_against(
        &WrittenFor::Declared(version("0.13.0")),
        version("0.11.0-dev"),
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("0.13"), "{error}");
    assert!(error.contains("0.11.0"), "{error}");
}

/// Whichever way it's refused, the message names the file — the one thing
/// that tells a user with several projects, or a non-default `-f`, where to
/// look.
#[test]
fn a_refusal_names_the_root_file() {
    let root = root_file(
        "ratect.toml",
        "ratect_version = \"99.0.0\"\nproject_name = \"p\"\n",
    );
    let error = format!("{:#}", check(&root, version("0.11.0")).unwrap_err());
    assert!(error.contains(&root.display().to_string()), "{error}");
}

#[test]
fn only_major_and_minor_are_compared() {
    check_against(
        &WrittenFor::Declared(version("0.11.9")),
        version("0.11.0-dev"),
        &[],
    )
    .unwrap();
}

#[test]
fn an_upgrade_crossing_no_breaking_change_is_silent() {
    check_against(
        &WrittenFor::Declared(version("0.13.0")),
        version("0.14.0"),
        &[ROW_0_12, ROW_0_13],
    )
    .unwrap();
}

#[test]
fn every_breaking_change_since_the_written_for_version_is_listed() {
    let error = check_against(
        &WrittenFor::Declared(version("0.11.0")),
        version("0.13.0"),
        &[ROW_0_12, ROW_0_13],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(ROW_0_12.summary), "{error}");
    assert!(error.contains(ROW_0_13.summary), "{error}");
    assert!(error.contains("'ratect_version'"), "{error}");
    assert!(error.contains("0.13.0"), "{error}");
}

#[test]
fn a_breaking_change_the_project_already_accounts_for_is_not_listed() {
    let error = check_against(
        &WrittenFor::Declared(version("0.12.0")),
        version("0.13.0"),
        &[ROW_0_12, ROW_0_13],
    )
    .unwrap_err()
    .to_string();
    assert!(!error.contains(ROW_0_12.summary), "{error}");
    assert!(error.contains(ROW_0_13.summary), "{error}");
}

#[test]
fn an_unmarked_project_counts_as_written_for_the_first_native_release() {
    check_against(&WrittenFor::Unmarked, version("0.11.0"), &[]).unwrap();
    let error = check_against(&WrittenFor::Unmarked, version("0.12.0"), &[ROW_0_12])
        .unwrap_err()
        .to_string();
    assert!(error.contains(ROW_0_12.summary), "{error}");
    assert!(error.contains("'ratect_version'"), "{error}");
}

#[test]
fn a_yaml_root_is_told_to_convert_before_it_can_declare_a_version() {
    check_against(&WrittenFor::YamlRoot, version("0.11.0"), &[]).unwrap();
    let error = check_against(&WrittenFor::YamlRoot, version("0.12.0"), &[ROW_0_12])
        .unwrap_err()
        .to_string();
    assert!(error.contains(ROW_0_12.summary), "{error}");
    assert!(error.contains("ratect config convert"), "{error}");
}

#[test]
fn every_row_in_the_real_table_names_a_version() {
    for row in BREAKING_CHANGES {
        row.version.parse::<Version>().unwrap();
    }
}
