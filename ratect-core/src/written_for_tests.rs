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

fn heading() -> String {
    "Refusing to load ratect.toml".to_string()
}

const ROW_0_12: BreakingChange = BreakingChange {
    version: "0.12.0",
    summary: "a backslash in an expression now escapes the next character",
    check: Check::NotChecked,
};

const ROW_0_13: BreakingChange = BreakingChange {
    version: "0.13.0",
    summary: "a task's environment no longer inherits the host's",
    check: Check::NotChecked,
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
        heading(),
        None,
        &WrittenFor::Declared(version("0.13.0")),
        version("0.11.0-dev"),
        &[],
    )
    .map_err(|error| format!("{error:#}"))
    .unwrap_err();
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
        heading(),
        None,
        &WrittenFor::Declared(version("0.11.9")),
        version("0.11.0-dev"),
        &[],
    )
    .unwrap();
}

#[test]
fn an_upgrade_crossing_no_breaking_change_is_silent() {
    check_against(
        heading(),
        None,
        &WrittenFor::Declared(version("0.13.0")),
        version("0.14.0"),
        &[ROW_0_12, ROW_0_13],
    )
    .unwrap();
}

#[test]
fn every_breaking_change_since_the_written_for_version_is_listed() {
    let error = check_against(
        heading(),
        None,
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
        heading(),
        None,
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
    check_against(
        heading(),
        None,
        &WrittenFor::Unmarked,
        version("0.11.0"),
        &[],
    )
    .unwrap();
    let error = check_against(
        heading(),
        None,
        &WrittenFor::Unmarked,
        version("0.12.0"),
        &[ROW_0_12],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(ROW_0_12.summary), "{error}");
    assert!(error.contains("'ratect_version'"), "{error}");
}

#[test]
fn a_yaml_root_is_told_to_make_its_root_a_ratect_toml() {
    check_against(
        heading(),
        None,
        &WrittenFor::YamlRoot,
        version("0.11.0"),
        &[],
    )
    .unwrap();
    let error = check_against(
        heading(),
        None,
        &WrittenFor::YamlRoot,
        version("0.12.0"),
        &[ROW_0_12],
    )
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

/// A YAML root follows Batect's rules, so before its includes are known
/// there is nothing to refuse it for — however many rows the table has.
#[test]
fn a_yaml_root_alone_is_not_refused_before_the_load() {
    let root = root_file("batect.yml", "project_name: p\n");
    check(&root, version("99.0.0")).unwrap();
}

/// Its TOML includes are what can have changed meaning, so a YAML root is
/// refused only once one is loaded — naming it, and saying to convert.
#[test]
fn a_yaml_root_is_refused_only_for_a_toml_file_it_loads() {
    let root = Path::new("/p/batect.yml");
    check_yaml_root_against(root, &[], version("0.12.0"), &[ROW_0_12]).unwrap();
    let toml = "\"/p/extra.toml\"".to_string();
    check_yaml_root_against(root, std::slice::from_ref(&toml), version("0.12.0"), &[]).unwrap();
    let error = format!(
        "{:#}",
        check_yaml_root_against(root, &[toml], version("0.12.0"), &[ROW_0_12]).unwrap_err()
    );
    assert!(error.contains("/p/extra.toml"), "{error}");
    assert!(error.contains(ROW_0_12.summary), "{error}");
    assert!(error.contains("ratect config convert"), "{error}");
}

// --- What a refusal says about each row (decisions/0022) ---

fn toml_file(name: &str, content: &str) -> TomlFile {
    TomlFile {
        name: name.to_string(),
        document: content.parse().unwrap(),
    }
}

/// A stand-in detector: every file with a top-level `marker`.
fn finds_marker(files: &[TomlFile]) -> Vec<Hit> {
    files
        .iter()
        .filter(|file| file.document.contains_key("marker"))
        .map(|file| Hit {
            field: "marker".to_string(),
            file: file.name.clone(),
        })
        .collect()
}

const LOAD_ROW: BreakingChange = BreakingChange {
    version: "0.12.0",
    summary: "a bad name fails the load",
    check: Check::FailsTheLoad,
};

const DETECTED_ROW: BreakingChange = BreakingChange {
    version: "0.12.0",
    summary: "a marker means something else",
    check: Check::Detected(finds_marker),
};

const UNCHECKED_ROW: BreakingChange = BreakingChange {
    version: "0.12.0",
    summary: "an escape means something else",
    check: Check::NotChecked,
};

/// An unmarked project refused for every kind of row.
fn refusal() -> Refusal {
    check_against(
        heading(),
        None,
        &WrittenFor::Unmarked,
        version("0.12.0"),
        &[LOAD_ROW, DETECTED_ROW, UNCHECKED_ROW],
    )
    .unwrap_err()
    .downcast::<Refusal>()
    .expect("a refusal for rows is a `Refusal`")
}

fn outcomes(refusal: &Refusal) -> Vec<Outcome> {
    refusal
        .rows()
        .into_iter()
        .map(|(_, outcome)| outcome.expect("assessed").clone())
        .collect()
}

#[test]
fn a_refusal_not_yet_assessed_lists_its_rows_without_outcomes() {
    let refusal = refusal();
    assert!(refusal.rows().iter().all(|(_, outcome)| outcome.is_none()));
    assert!(refusal.trial_load().is_none());
    let text = refusal.to_string();
    assert!(
        text.starts_with("Refusing to load ratect.toml: This project doesn't set"),
        "{text}"
    );
    assert!(
        text.contains("\n  - ratect 0.12.0: a bad name fails the load\n"),
        "{text}"
    );
}

/// A project that loads rules out every load-failing row, and each detected
/// row says what it found — naming the field and the file.
#[test]
fn a_project_that_loads_gets_an_outcome_for_every_row() {
    let files = [
        toml_file("\"/p/ratect.toml\"", "project_name = \"p\"\n"),
        toml_file("\"/p/extra.toml\"", "marker = true\n"),
    ];
    let assessed = refusal().with_outcomes(None, Readable::Every(&files));
    assert_eq!(
        outcomes(&assessed),
        [
            Outcome::Loads,
            Outcome::Found(vec![Hit {
                field: "marker".to_string(),
                file: "\"/p/extra.toml\"".to_string(),
            }]),
            Outcome::NotChecked,
        ]
    );
    let text = assessed.to_string();
    for expected in [
        "a bad name fails the load — doesn't apply: it loads under these rules",
        "a marker means something else — found: 'marker' in \"/p/extra.toml\"",
        "an escape means something else — not checked",
        "With this check set aside, the project loads under ratect 0.12.0's rules.",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    assert!(text.ends_with(assessed.remedy()), "{text}");

    let without = refusal().with_outcomes(None, Readable::Every(&files[..1]));
    assert_eq!(outcomes(&without)[1], Outcome::NoneFound);
}

/// A project that doesn't load shows the error once, and neither kind of
/// checked row claims anything it can't know: a load-failing row doesn't
/// say it caused the failure, and a detected row didn't see every file.
#[test]
fn a_project_that_does_not_load_shows_the_error_once() {
    let assessed =
        refusal().with_outcomes(Some("the error itself".to_string()), Readable::Only(&[]));
    assert_eq!(
        outcomes(&assessed),
        [Outcome::DoesNotLoad, Outcome::Unknown, Outcome::NotChecked]
    );
    let text = assessed.to_string();
    assert_eq!(text.matches("the error itself").count(), 1, "{text}");
    assert!(
        text.contains(
            "With this check set aside, the project doesn't load under ratect 0.12.0's rules: \
             the error itself"
        ),
        "{text}"
    );
    assert!(
        text.contains("may apply: it doesn't load under these rules"),
        "{text}"
    );
}

/// A file the trial load read but that can't be read back leaves a detected
/// row unknown — never "none found".
#[test]
fn a_detected_row_is_unknown_without_its_files() {
    let assessed = refusal().with_outcomes(None, Readable::Only(&[]));
    assert_eq!(outcomes(&assessed)[1], Outcome::Unknown);
}

#[test]
fn only_a_load_or_nothing_found_rules_a_row_out() {
    assert!(Outcome::Loads.rules_out());
    assert!(Outcome::NoneFound.rules_out());
    for outcome in [
        Outcome::DoesNotLoad,
        Outcome::Found(Vec::new()),
        Outcome::Unknown,
        Outcome::NotChecked,
    ] {
        assert!(!outcome.rules_out(), "{outcome:?}");
    }
}

/// The bundle row's detector, against both forms `include` takes and an
/// include that isn't a Git one.
#[test]
fn the_bundle_row_finds_each_file_declaring_a_git_include() {
    let files = [
        toml_file(
            "inline",
            "include = [\"a.toml\", { type = \"git\", repo = \"r\", ref = \"v1\" }]\n",
        ),
        toml_file(
            "tables",
            "[[include]]\ntype = \"git\"\nrepo = \"r\"\nref = \"v1\"\n",
        ),
        toml_file(
            "local",
            "include = [\"a.toml\", { type = \"file\", path = \"b.toml\" }]\n",
        ),
        toml_file("none", "project_name = \"p\"\n"),
    ];
    let files: Vec<String> = declares_git_include(&files)
        .into_iter()
        .map(|hit| {
            assert_eq!(hit.field, "include");
            hit.file
        })
        .collect();
    assert_eq!(files, ["inline", "tables"]);
}

/// decisions/0022: the three rows changing what a value means have no
/// detector yet and say so; the bundle row is detected; every other row
/// only fails the load.
#[test]
fn every_row_in_the_real_table_declares_how_it_is_checked() {
    let unchecked = [
        "a '\\' in an expression",
        "every '$' or '<'",
        "a local include's 'path'",
    ];
    for row in BREAKING_CHANGES {
        let expected = if unchecked.iter().any(|start| row.summary.starts_with(start)) {
            "unchecked"
        } else if row.summary.starts_with("a Git bundle") {
            "detected"
        } else {
            "load"
        };
        let actual = match row.check {
            Check::NotChecked => "unchecked",
            Check::Detected(_) => "detected",
            Check::FailsTheLoad => "load",
        };
        assert_eq!(actual, expected, "{}", row.summary);
    }
}

/// When the trial load fails, a detector reads what it still can: a use
/// found there is found, and finding nothing there proves nothing.
#[test]
fn a_detected_row_reports_what_it_could_read_when_the_load_fails() {
    let root = [toml_file("\"/p/ratect.toml\"", "marker = true\n")];
    let found = refusal().with_outcomes(Some("error".to_string()), Readable::Only(&root));
    assert_eq!(
        outcomes(&found)[1],
        Outcome::Found(vec![Hit {
            field: "marker".to_string(),
            file: "\"/p/ratect.toml\"".to_string(),
        }])
    );
    let nothing = [toml_file("\"/p/ratect.toml\"", "project_name = \"p\"\n")];
    let unknown = refusal().with_outcomes(Some("error".to_string()), Readable::Only(&nothing));
    assert_eq!(outcomes(&unknown)[1], Outcome::Unknown);
}

/// A TOML root's refusal reads the root itself when the trial load fails,
/// naming it as the rest of a refusal does; a YAML root's has nothing it
/// could read as TOML.
#[test]
fn a_failed_trial_load_still_reads_a_toml_root() {
    let root = root_file(
        "ratect.toml",
        "project_name = \"Demo\"\n\n[[include]]\ntype = \"git\"\nrepo = \"r\"\nref = \"v1\"\n",
    );
    let refusal = check(&root, version("0.12.0"))
        .unwrap_err()
        .downcast::<Refusal>()
        .unwrap();
    let assessed = refusal.assess(Err(&anyhow::anyhow!("doesn't load")));
    let bundle = assessed
        .rows()
        .into_iter()
        .find(|(line, _)| line.contains("a Git bundle is refused"))
        .and_then(|(_, outcome)| outcome.cloned())
        .unwrap();
    assert_eq!(
        bundle,
        Outcome::Found(vec![Hit {
            field: "include".to_string(),
            file: format!("{root:?}"),
        }])
    );

    let yaml = Path::new("/p/batect.yml");
    let toml = "\"/p/extra.toml\"".to_string();
    let refusal = check_yaml_root_against(yaml, &[toml], version("0.12.0"), BREAKING_CHANGES)
        .unwrap_err()
        .downcast::<Refusal>()
        .unwrap();
    let assessed = refusal.assess(Err(&anyhow::anyhow!("doesn't load")));
    assert!(
        assessed
            .rows()
            .iter()
            .any(|(_, outcome)| *outcome == Some(&Outcome::Unknown)),
        "{assessed}"
    );
}
