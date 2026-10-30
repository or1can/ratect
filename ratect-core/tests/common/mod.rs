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

//! What `ratect-core`'s integration tests share about the format twins
//! corpus, `tests/twins/` — where its cases and files are, a scratch copy of
//! one, and how a document's properties, and the places it holds a string,
//! are found by walking it alongside a committed schema. A module of
//! `tests/`, not a test crate of its own, so Cargo compiles it into each
//! test that declares `mod common`, and an item one of them doesn't use is
//! dead code there.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where the cases live, one directory each.
pub fn twins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("twins")
}

/// Every case directory, in name order.
pub fn cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(twins_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

/// A fresh, empty directory under the system temporary directory, removed
/// when dropped — so a panic mid-case doesn't leave it behind.
pub struct ScratchDir(pub PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn scratch_dir(label: &str) -> ScratchDir {
    let dir = std::env::temp_dir().join(format!(
        "ratect-twins-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    ScratchDir(dir)
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// What a schema declares, or a document uses, by location.
#[derive(Default)]
struct Locations {
    /// Each property's label.
    properties: BTreeSet<String>,
    /// Each location a string can be: a property's label, or a map's values',
    /// an array's items' or an alternative's, labelled as their own.
    strings: BTreeSet<String>,
    /// Each location a schema admits any value at — no `type`, fixed value,
    /// `$ref` or alternatives of its own, nor a typed node's alternative, or
    /// a map's values or an array's items given as `true` — labelled as a
    /// string location is.
    untyped: BTreeSet<String>,
    /// Each keyword, with where it sits, that [`declared`]'s walk doesn't
    /// read, so a location inside it would go unseen.
    unhandled: BTreeSet<String>,
}

/// Every keyword [`declared`]'s walk reads, or can pass over because it holds
/// no subschema of a value: an annotation, a constraint on a value, or
/// `propertyNames`, which constrains only a map's keys. [`ROOT_ONLY`]'s are
/// read only at the root.
const HANDLED: &[&str] = &[
    "$ref",
    "additionalProperties",
    "anyOf",
    "const",
    "default",
    "description",
    "enum",
    "examples",
    "format",
    "items",
    "maximum",
    "minimum",
    "oneOf",
    "pattern",
    "properties",
    "propertyNames",
    "required",
    "title",
    "type",
    "uniqueItems",
];

/// The keywords [`declared`] reads at the schema's root and nowhere else —
/// so a nested `definitions`, whose definitions it wouldn't walk, is
/// unhandled.
const ROOT_ONLY: &[&str] = &["$schema", "definitions"];

/// Whether the schema `node` admits a string other than a fixed one — a
/// `const` or `enum` value is a choice, not a place a value is written.
fn admits_string(node: &Value) -> bool {
    if node.get("const").is_some() || node.get("enum").is_some() {
        return false;
    }
    match node.get("type") {
        Some(Value::String(t)) => t == "string",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "string"),
        _ => false,
    }
}

/// Every property `schema` declares, labelled by where it sits: a
/// definition's name or `root`, then `.name` for a property, `[]` for an
/// array's items, `{}` for a map's values and `#i` for the `i`th `oneOf`/
/// `anyOf` alternative. A `$ref` is labelled by the definition it names.
pub fn schema_properties(schema: &Value) -> BTreeSet<String> {
    declared(schema).properties
}

/// Every location `schema` admits a string at, labelled as
/// [`schema_properties`] labels a property.
pub fn schema_string_locations(schema: &Value) -> BTreeSet<String> {
    declared(schema).strings
}

/// Every location `schema` admits any value at, so neither does nor doesn't
/// admit a string — labelled as [`schema_string_locations`] labels one. An
/// alternative with no `type` of its own, such as a `format`, narrows its
/// typed parent's, so isn't one.
pub fn schema_untyped_locations(schema: &Value) -> BTreeSet<String> {
    declared(schema).untyped
}

/// Every keyword `schema` uses that its walk here doesn't read, as
/// `location: keyword` — a `$ref` with anything beside it but a
/// `description`, or an `items` neither a schema nor `true`, included.
pub fn schema_unhandled_keywords(schema: &Value) -> BTreeSet<String> {
    declared(schema).unhandled
}

fn declared(schema: &Value) -> Locations {
    fn walk(node: &Value, location: &str, typed_parent: bool, out: &mut Locations) {
        let is_ref = node.get("$ref").is_some();
        for keyword in node.as_object().into_iter().flat_map(|node| node.keys()) {
            let handled = if is_ref {
                ["$ref", "description"].contains(&keyword.as_str())
            } else {
                HANDLED.contains(&keyword.as_str())
                    || (location == "root" && ROOT_ONLY.contains(&keyword.as_str()))
            };
            if !handled {
                out.unhandled.insert(format!("{location}: {keyword}"));
            }
        }
        if is_ref {
            return;
        }
        let constrained = ["type", "const", "enum", "oneOf", "anyOf"]
            .iter()
            .any(|keyword| node.get(keyword).is_some());
        if !constrained && !typed_parent {
            out.untyped.insert(location.to_string());
        }
        if admits_string(node) {
            out.strings.insert(location.to_string());
        }
        if let Some(properties) = node.get("properties").and_then(Value::as_object) {
            for (name, child) in properties {
                let label = format!("{location}.{name}");
                out.properties.insert(label.clone());
                walk(child, &label, false, out);
            }
        }
        match node.get("additionalProperties") {
            Some(Value::Bool(true)) => {
                out.untyped.insert(format!("{location}{{}}"));
            }
            Some(values) if values.is_object() => {
                walk(values, &format!("{location}{{}}"), false, out);
            }
            _ => {}
        }
        match node.get("items") {
            Some(Value::Bool(true)) => {
                out.untyped.insert(format!("{location}[]"));
            }
            Some(items) if items.is_object() => walk(items, &format!("{location}[]"), false, out),
            Some(_) => {
                out.unhandled.insert(format!("{location}: items"));
            }
            None => {}
        }
        let typed = typed_parent || node.get("type").is_some();
        for keyword in ["oneOf", "anyOf"] {
            for (index, alternative) in node
                .get(keyword)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                walk(alternative, &format!("{location}#{index}"), typed, out);
            }
        }
    }
    let mut out = Locations::default();
    walk(schema, "root", false, &mut out);
    for (name, definition) in schema["definitions"].as_object().unwrap() {
        walk(definition, name, false, &mut out);
    }
    out
}

/// Every property label (see [`schema_properties`]) `document` uses, found by
/// walking it alongside `schema`. A key the schema doesn't declare — a YAML
/// anchor's `.extension`, a `<<` merge key — is skipped.
pub fn properties_used(schema: &Value, document: &Value) -> BTreeSet<String> {
    used(schema, document).properties
}

/// Every location (see [`schema_string_locations`]) `document` holds a
/// string at, found as [`properties_used`] finds a property.
pub fn string_locations_used(schema: &Value, document: &Value) -> BTreeSet<String> {
    used(schema, document).strings
}

fn used(schema: &Value, document: &Value) -> Locations {
    fn resolve<'s>(schema: &'s Value, node: &'s Value, location: &str) -> (&'s Value, String) {
        match node.get("$ref").and_then(Value::as_str) {
            Some(reference) => {
                let name = reference.trim_start_matches("#/definitions/");
                (&schema["definitions"][name], name.to_string())
            }
            None => (node, location.to_string()),
        }
    }
    fn type_matches(node: &Value, value: &Value) -> bool {
        let actual = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        match node.get("type") {
            None => true,
            Some(Value::String(t)) => t == actual || (t == "number" && actual == "integer"),
            Some(Value::Array(ts)) => ts.iter().any(|t| t == actual),
            Some(_) => true,
        }
    }
    /// Whether `value` could be this alternative: its type, and for an
    /// object, keys it declares, the ones it requires and any `const`s.
    fn alternative_matches(node: &Value, value: &Value) -> bool {
        if !type_matches(node, value) {
            return false;
        }
        if let Some(constant) = node.get("const") {
            return constant == value;
        }
        let (Some(object), Some(properties)) = (
            value.as_object(),
            node.get("properties").and_then(Value::as_object),
        ) else {
            return true;
        };
        let closed = node.get("additionalProperties") == Some(&Value::Bool(false));
        let required = node
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        (!closed || object.keys().all(|key| properties.contains_key(key)))
            && required.into_iter().all(|key| object.contains_key(key))
            && properties.iter().all(|(key, property)| {
                match (property.get("const"), object.get(key)) {
                    (Some(constant), Some(actual)) => constant == actual,
                    _ => true,
                }
            })
    }
    fn walk(schema: &Value, node: &Value, location: &str, value: &Value, out: &mut Locations) {
        let (node, location) = resolve(schema, node, location);
        for keyword in ["oneOf", "anyOf"] {
            for (index, alternative) in node
                .get(keyword)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let (resolved, _) = resolve(schema, alternative, "");
                if alternative_matches(resolved, value) {
                    walk(
                        schema,
                        alternative,
                        &format!("{location}#{index}"),
                        value,
                        out,
                    );
                }
            }
        }
        match value {
            Value::String(_) if admits_string(node) => {
                out.strings.insert(location);
            }
            Value::Object(object) => {
                let properties = node.get("properties").and_then(Value::as_object);
                let values = node.get("additionalProperties").filter(|v| v.is_object());
                for (key, child) in object {
                    if let Some(property) = properties.and_then(|p| p.get(key)) {
                        let label = format!("{location}.{key}");
                        out.properties.insert(label.clone());
                        walk(schema, property, &label, child, out);
                    } else if let Some(values) = values {
                        walk(schema, values, &format!("{location}{{}}"), child, out);
                    }
                }
            }
            Value::Array(items) => {
                if let Some(item_schema) = node.get("items") {
                    for item in items {
                        walk(schema, item_schema, &format!("{location}[]"), item, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Locations::default();
    walk(schema, schema, "root", document, &mut out);
    out
}

/// Every file in every case with `extension`, recursively.
pub fn case_files(extension: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, extension, out);
            } else if path.extension().is_some_and(|ext| ext == extension) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&twins_dir(), extension, &mut out);
    out.sort();
    out
}

/// The `ratect` version a native load in these tests runs as. No case has a
/// Git include, so no bundle's written-for version is checked.
pub fn running_0_12() -> ratect_core::written_for::Version {
    "0.12.0".parse().unwrap()
}
