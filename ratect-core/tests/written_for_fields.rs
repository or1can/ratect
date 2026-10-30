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

//! The written-for detectors that read expressions find them through their
//! own list of the native fields that are expressions, `written_for`'s
//! `expressions`, since they read each file as plain TOML
//! ([decisions/0022](../../decisions/0022-written-for-refusal-says-what-applies.md)).
//! A field missing from that list would have them report "none found" for a
//! project using it — the one wrong answer that matters there (ratect#748).
//! So the loader is checked against it here, not another list:
//!
//! - [`the_backslash_row_looks_in_exactly_the_fields_the_loader_reads_as_expressions`]
//!   makes each string in each `ratect.toml` format twin malformed in turn,
//!   and requires the loader to refuse it as an expression exactly when the
//!   backslash row's detector finds it. The reference row's detector reads
//!   the same list, but can't be checked this way: a malformed expression is
//!   one it leaves to the trial load.
//! - [`every_string_location_in_the_native_schema_is_set_to_a_string_by_some_twin`]
//!   makes the twins reach every place the native schema admits a string,
//!   so a new expression field is checked once it exists, unless
//!   [`NOT_SETTABLE`] says why no twin can set it — and fails on a place it
//!   admits any value at, which can't be told either way, or a keyword the
//!   walk doesn't read, which could hide one.

mod common;
use common::{
    case_files, cases, copy_dir, running_0_12, schema_string_locations, schema_unhandled_keywords,
    schema_untyped_locations, scratch_dir, string_locations_used,
};
use ratect_core::written_for::{Check, Hit, TomlFile, BREAKING_CHANGES};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What each value made malformed starts with: an escaped `\`, so the
/// backslash row's detector finds it wherever it looks, then an empty
/// braced name, which no expression may hold, so the loader refuses it
/// wherever it reads one. A prefix, so a compact `volumes` entry's host path
/// is what it lands in.
const MALFORMED: &str = r"\\${}";

/// A string location in the native schema no twin can set, and why.
struct NotSettable {
    /// The location, labelled as `common::schema_string_locations` labels it.
    location: &'static str,
    because: &'static str,
}

/// Why a `type = "git"` include's fields are [`NOT_SETTABLE`]: loading one
/// clones into the real `~/.ratect/incl`. None is an expression.
const GIT_INCLUDE: &str = "a git include clones into the real ~/.ratect/incl";

/// Every string location in the native schema no twin can set.
const NOT_SETTABLE: &[NotSettable] = &[
    NotSettable {
        location: "Include#1.path",
        because: GIT_INCLUDE,
    },
    NotSettable {
        location: "Include#1.ref",
        because: GIT_INCLUDE,
    },
    NotSettable {
        location: "Include#1.repo",
        because: GIT_INCLUDE,
    },
    NotSettable {
        location: "Include#1.reviewed_ratect_version",
        because: GIT_INCLUDE,
    },
];

#[test]
fn every_string_location_in_the_native_schema_is_set_to_a_string_by_some_twin() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../schema/ratect-config.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let unhandled = schema_unhandled_keywords(&schema);
    assert!(
        unhandled.is_empty(),
        "the native schema uses {unhandled:#?}, which the schema walk in tests/common/mod.rs \
         doesn't read, so a string location inside it would go unchecked: teach `declared` \
         there to walk it, or add it to `HANDLED` if it holds no subschema of a value"
    );
    let untyped = schema_untyped_locations(&schema);
    assert!(
        untyped.is_empty(),
        "the native schema admits any value at {untyped:#?}, so whether a string can go there, \
         and be an expression, isn't known: give it a type"
    );
    let declared = schema_string_locations(&schema);
    let mut used = BTreeSet::new();
    for file in case_files("toml") {
        let document = toml::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        used.extend(string_locations_used(&schema, &document));
    }
    let exempt: BTreeMap<&str, &str> = NOT_SETTABLE
        .iter()
        .map(|row| (row.location, row.because))
        .collect();

    let unset: Vec<&String> = declared
        .iter()
        .filter(|location| !used.contains(*location) && !exempt.contains_key(location.as_str()))
        .collect();
    let needlessly_exempt: Vec<String> = exempt
        .iter()
        .filter(|(location, _)| used.contains(**location))
        .map(|(location, because)| format!("{location} ({because})"))
        .collect();
    let unknown_exempt: Vec<&&str> = exempt
        .keys()
        .filter(|location| !declared.contains(**location))
        .collect();
    assert!(
        unset.is_empty() && needlessly_exempt.is_empty() && unknown_exempt.is_empty(),
        "no ratect.toml twin under tests/twins/ sets {unset:#?} to a string, so whether the \
         loader reads it as an expression, and the written-for detectors look in it, goes \
         unchecked: set it to a string in a twin, or list it in NOT_SETTABLE with the reason \
         none can; set by a twin after all, so unlist from NOT_SETTABLE: {needlessly_exempt:#?}; \
         listed in NOT_SETTABLE but not in the schema: {unknown_exempt:#?}"
    );
}

#[tokio::test]
async fn the_backslash_row_looks_in_exactly_the_fields_the_loader_reads_as_expressions() {
    let detect = backslash_row();
    let mut disagreements = Vec::new();
    for case in cases() {
        for path in case_files("toml")
            .into_iter()
            .filter(|path| path.starts_with(&case))
        {
            let original: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
            // What the detector reads holds no `\` but the one made
            // malformed, so a hit is that one's.
            let mut without_backslashes = original.clone();
            for_each_string(&mut without_backslashes, &mut |_, text| {
                text.retain(|c| c != '\\');
            });
            assert_eq!(detect(&[toml_file(without_backslashes.clone())]), []);
            let relative = path.strip_prefix(&case).unwrap();
            // A load failing for another reason would read as no field
            // being an expression.
            if let Err(error) = load_with(&case, relative, &original).await {
                panic!("{} doesn't load as written back: {error}", path.display());
            }
            for target in 0..string_count(&original) {
                let (malformed, field) = malformed_at(&original, target);
                let refused = load_with(&case, relative, &malformed)
                    .await
                    .is_err_and(|error| {
                        error.contains(&format!("invalid expression '{MALFORMED}"))
                    });
                let (malformed, _) = malformed_at(&without_backslashes, target);
                let found = !detect(&[toml_file(malformed)]).is_empty();
                let file = path.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap();
                match (refused, found) {
                    (true, false) => disagreements.push(format!(
                        "{file:?}'s `{field}`: the loader reads it as an expression, but the \
                         written-for detectors reading expressions don't look in it: add it to \
                         `expressions` in ratect-core/src/written_for.rs"
                    )),
                    (false, true) => disagreements.push(format!(
                        "{file:?}'s `{field}`: the written-for detectors read it as an \
                         expression, but the loader doesn't: remove it from `expressions` in \
                         ratect-core/src/written_for.rs"
                    )),
                    _ => {}
                }
            }
        }
    }
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
}

/// Loads a scratch copy of `case`, its file at `relative` written as
/// `document`, the way `ratect` loads it — failing with the whole error.
async fn load_with(case: &Path, relative: &Path, document: &toml::Table) -> Result<(), String> {
    let scratch = scratch_dir("written-for-fields");
    copy_dir(case, &scratch.0);
    std::fs::write(scratch.0.join(relative), toml::to_string(document).unwrap()).unwrap();
    ratect_core::config::load_project_native(
        &scratch.0.join("ratect.toml"),
        running_0_12(),
        &ratect_core::ui::LoadOutput::default(),
    )
    .await
    .map(drop)
    .map_err(|error| format!("{error:#}"))
}

/// The backslash row's detector, found by its summary — `BREAKING_CHANGES`
/// names no row otherwise.
fn backslash_row() -> fn(&[TomlFile]) -> Vec<Hit> {
    const SUMMARY: &str = "a '\\' in an expression";
    BREAKING_CHANGES
        .iter()
        .find_map(|change| match change.check {
            Check::Detected(detect) if change.summary.starts_with(SUMMARY) => Some(detect),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("no detected breaking change's summary starts {SUMMARY:?}: update it here")
        })
}

fn toml_file(document: toml::Table) -> TomlFile {
    TomlFile {
        name: "twin".to_string(),
        document,
    }
}

/// Calls `each` on every string in `table`, with where it sits: a dotted
/// path, `[]` marking an array's entry.
fn for_each_string(table: &mut toml::Table, each: &mut impl FnMut(&str, &mut String)) {
    fn walk(value: &mut toml::Value, at: &str, each: &mut impl FnMut(&str, &mut String)) {
        match value {
            toml::Value::String(text) => each(at, text),
            toml::Value::Array(entries) => {
                for entry in entries {
                    walk(entry, &format!("{at}[]"), each);
                }
            }
            toml::Value::Table(table) => {
                for (key, child) in table {
                    walk(child, &format!("{at}.{key}"), each);
                }
            }
            _ => {}
        }
    }
    for (key, value) in table {
        walk(value, key, each);
    }
}

fn string_count(table: &toml::Table) -> usize {
    let mut count = 0;
    for_each_string(&mut table.clone(), &mut |_, _| count += 1);
    count
}

/// `table` with its `target`th string made malformed, and where it sits.
fn malformed_at(table: &toml::Table, target: usize) -> (toml::Table, String) {
    let mut malformed = table.clone();
    let mut field = String::new();
    let mut seen = 0;
    for_each_string(&mut malformed, &mut |at, text| {
        if seen == target {
            field = at.to_string();
            text.insert_str(0, MALFORMED);
        }
        seen += 1;
    });
    (malformed, field)
}
