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

//! Two text formats, one model: a `batect.yml` and a `ratect.toml` that say
//! the same thing load and evaluate to the same [`Config`], except where
//! [`DIVERGENCES`] says they don't (ratect#572). "The same" is compared as
//! serialized JSON, so a field `Config` marks `#[serde(skip)]` is never
//! compared.
//!
//! The corpus is `tests/twins/<case>/`, each case a hand-written
//! `batect.yml` and `ratect.toml` side by side. Hand-written, never generated
//! by `ratect config convert`: the TOML loader is checked against input
//! written independently of it, and the converter gets its own check
//! ([`converting_each_twins_yaml_gives_its_toml_twins_config`]) rather than
//! being part of the thing under test — a field dropped by both the converter
//! and the TOML loader would otherwise round-trip equal and pass.
//!
//! Every difference is listed, one [`Divergence`] each, citing the issue or
//! reference page that makes it deliberate, and pinning both sides' values
//! (or saying, with [`ANY`], that one side isn't worth pinning). A new
//! difference fails until someone lists it on purpose; a listed one that no
//! longer occurs fails until someone removes it. So the list stays the
//! executable record of how the formats differ — and, for the converter, of
//! any known bug in it, cited by its issue, until a fix makes the entry stale.
//!
//! [`the_twins_cover_every_property_in_both_schemas`] makes "is the corpus
//! enough?" a failing test: every property in either committed schema has to
//! appear in some case, on that schema's side, unless [`UNCOVERABLE`] says why
//! no case can use it.
//!
//! These twins belong to `ratect-core`, not to either binary: the claim is
//! about its two front ends. Don't borrow `ratect-compat`'s fixtures or
//! conformance projects as cases — they prove engine and Docker behaviour, and
//! an edit to one shouldn't fail a test here.

use ratect_core::config::{load_project, load_project_native, to_native_toml, Config};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Where the cases live, one directory each.
fn twins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("twins")
}

/// Every case directory, in name order.
fn cases() -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(twins_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

fn case_name(case: &Path) -> &str {
    case.file_name().unwrap().to_str().unwrap()
}

/// Which two configurations a [`Divergence`] is between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Between {
    /// A case's `batect.yml` (`left`) and its `ratect.toml` twin (`right`).
    Twins,
    /// `ratect config convert`'s output for the case's `batect.yml` (`left`)
    /// and its `ratect.toml` twin (`right`).
    Converted,
}

/// One known, deliberate difference: at `pointer` (a JSON pointer into the
/// evaluated configuration), `left` and `right` are these values, as JSON
/// text with the case directory written `<case>` — or [`ANY`].
struct Divergence {
    between: &'static [Between],
    case: &'static str,
    pointer: &'static str,
    left: &'static str,
    right: &'static str,
    /// The issue or reference page that makes it deliberate.
    because: &'static str,
}

/// A [`Divergence`] side that may hold any value, so long as it differs —
/// for a value worth listing but not worth pinning, such as a generated
/// companion container whose image digest moves on every bump.
const ANY: &str = "*";

/// For a difference the converter's output shares with the `batect.yml` — a
/// native-only field, for example, which a `batect.yml` can't write and so the
/// converter has nothing to carry across.
const BOTH: &[Between] = &[Between::Twins, Between::Converted];
/// A difference between the twins that the converter doesn't share: it writes
/// the `batect.yml`'s text into a `ratect.toml`, which reads it as the twin
/// does.
const TWINS: &[Between] = &[Between::Twins];
/// A difference only the converter makes: its output says something the
/// `batect.yml` doesn't, which the hand-written twin doesn't either.
const CONVERTED: &[Between] = &[Between::Converted];

/// Where `external_health_check`, and the companion container it generates,
/// are documented.
const EXTERNAL_HEALTH_CHECK: &str =
    "docs/ratect-config-reference.md#external_health_check-checking-a-container-from-outside-it";

/// Every way the twins, or the converter's output, differ — the executable
/// record of how the two formats differ. Behaviour keyed on
/// `LoadedProject::dialect` rather than on the configuration, such as a task's
/// own container having no readiness gate in `ratect`, isn't a difference in
/// [`Config`] and isn't listed; nor is a field one format refuses at load.
const DIVERGENCES: &[Divergence] = &[
    Divergence {
        between: CONVERTED,
        case: "includes",
        pointer: "/containers/helper/volumes/0/local",
        left: r#""<case>""#,
        right: r#""<case>/more""#,
        because: "ratect#574: flattening an include re-anchors its relative paths at the root",
    },
    Divergence {
        between: TWINS,
        case: "literal-in-yaml",
        pointer: "/containers/app/run_as_current_user/home_directory",
        left: r#""/home/$TWIN_HOST_VAR""#,
        right: r#""/home/from-the-host""#,
        because: "ratect#462: batect.yml uses home_directory as written",
    },
    Divergence {
        between: TWINS,
        case: "literal-in-yaml",
        pointer: "/containers/app/volumes/0",
        left: r#""<case>/$TWIN_HOST_VAR/literal:/literal""#,
        right: r#""<case>/from-the-host/literal:/literal""#,
        because: "ratect#379: batect.yml uses a compact volume's host path as written",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/api/external_health_check",
        left: r#"null"#,
        right: r#"{"expected_status":204,"interval":"2000000000ns","path":"/healthz","port":8080,"retries":3,"timeout":"1000000000ns","type":"http"}"#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/probe/external_health_check",
        left: r#"null"#,
        right: r#"{"expected_status":null,"interval":"2000000000ns","path":null,"port":5432,"retries":3,"timeout":"1000000000ns","type":"tcp"}"#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/ratect-health-check-api",
        left: r#"null"#,
        right: r#"*"#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/ratect-health-check-probe",
        left: r#"null"#,
        right: r#"*"#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/dependencies/4",
        left: r#"null"#,
        right: r#""ratect-health-check-api""#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/dependencies/5",
        left: r#"null"#,
        right: r#""ratect-health-check-probe""#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/sharer/dependencies/1",
        left: r#"null"#,
        right: r#""ratect-health-check-api""#,
        because: EXTERNAL_HEALTH_CHECK,
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/dns",
        left: r#"null"#,
        right: r#"["10.0.0.53"]"#,
        because: "docs/ratect-config-reference.md#dnsdns_searchdns_options-name-resolution",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/dns_options",
        left: r#"null"#,
        right: r#"["ndots:2"]"#,
        because: "docs/ratect-config-reference.md#dnsdns_searchdns_options-name-resolution",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/dns_search",
        left: r#"null"#,
        right: r#"["corp.example.com"]"#,
        because: "docs/ratect-config-reference.md#dnsdns_searchdns_options-name-resolution",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/stop_grace_period",
        left: r#"null"#,
        right: r#""30000000000ns""#,
        because: "docs/ratect-config-reference.md#stop_signalstop_grace_period-graceful-shutdown",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/stop_signal",
        left: r#"null"#,
        right: r#""SIGINT""#,
        because: "docs/ratect-config-reference.md#stop_signalstop_grace_period-graceful-shutdown",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/child/ulimits",
        left: r#"null"#,
        right: r#"[{"hard":2048,"name":"nofile","soft":1024},{"hard":0,"name":"core","soft":0}]"#,
        because: "docs/ratect-config-reference.md#ulimits-per-resource-limits",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/migrate/run_to_completion",
        left: r#"null"#,
        right: r#"true"#,
        because: "docs/ratect-config-reference.md#run_to_completion-init-containers",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/seeder/setup_commands/0/run_in",
        left: r#"null"#,
        right: r#""db-client""#,
        because: "docs/ratect-config-reference.md#run_in-setup-commands-in-another-container",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/sharer/network_mode",
        left: r#"null"#,
        right: r#""container:api""#,
        because: "docs/ratect-config-reference.md#network_mode-leaving-the-tasks-network",
    },
    Divergence {
        between: BOTH,
        case: "native-only",
        pointer: "/containers/sharer/volumes/0/scope",
        left: r#"null"#,
        right: r#""shared""#,
        because: "docs/ratect-config-reference.md#shared-caches",
    },
];

/// The host environment every case is evaluated against — fixed, so a case's
/// `$VAR`s mean the same on every machine.
fn host_env(name: &str) -> Option<String> {
    match name {
        "TWIN_HOST_VAR" => Some("from-the-host".to_string()),
        "TWIN_TAG" => Some("3.18".to_string()),
        _ => None,
    }
}

/// `project` evaluated whole, as JSON: the configuration as loaded, with its
/// containers and tasks replaced by their evaluated selves, and `dir` written
/// `<case>` so two copies of a case compare equal.
fn evaluated(project: &ratect_core::config::LoadedProject, dir: &Path) -> Value {
    let whole = project
        .evaluate_whole_project(&HashMap::new(), host_env)
        .unwrap();
    assert!(
        whole.failures.is_empty(),
        "{} failed to evaluate: {:#?}",
        dir.display(),
        whole.failures
    );
    let mut value = serde_json::to_value(&project.config).unwrap();
    value["containers"] = serde_json::to_value(&whole.evaluated.containers).unwrap();
    value["tasks"] = serde_json::to_value(&whole.evaluated.tasks).unwrap();
    let mut prefixes = vec![dir.display().to_string()];
    if let Ok(canonical) = dir.canonicalize() {
        prefixes.push(canonical.display().to_string());
    }
    normalise(&mut value, &prefixes);
    value
}

fn normalise(value: &mut Value, prefixes: &[String]) {
    match value {
        Value::String(text) => {
            for prefix in prefixes {
                if text.contains(prefix.as_str()) {
                    *text = text.replace(prefix.as_str(), "<case>");
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| normalise(item, prefixes)),
        Value::Object(map) => map.values_mut().for_each(|item| normalise(item, prefixes)),
        _ => {}
    }
}

/// Every place `left` and `right` differ, as `(pointer, left, right)`. A key
/// or element one side lacks counts as `null` there, so an absent field and an
/// unset one compare equal.
fn differences(left: &Value, right: &Value) -> Vec<(String, Value, Value)> {
    fn walk(pointer: String, left: &Value, right: &Value, out: &mut Vec<(String, Value, Value)>) {
        match (left, right) {
            (Value::Object(l), Value::Object(r)) => {
                let keys: BTreeSet<&String> = l.keys().chain(r.keys()).collect();
                for key in keys {
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    walk(
                        format!("{pointer}/{escaped}"),
                        l.get(key).unwrap_or(&Value::Null),
                        r.get(key).unwrap_or(&Value::Null),
                        out,
                    );
                }
            }
            (Value::Array(l), Value::Array(r)) => {
                for index in 0..l.len().max(r.len()) {
                    walk(
                        format!("{pointer}/{index}"),
                        l.get(index).unwrap_or(&Value::Null),
                        r.get(index).unwrap_or(&Value::Null),
                        out,
                    );
                }
            }
            _ if left != right => out.push((pointer, left.clone(), right.clone())),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(String::new(), left, right, &mut out);
    out
}

/// What's wrong with the differences between `left` and `right`, measured
/// against the [`DIVERGENCES`] listed for this comparison and case: one line
/// per unlisted difference, listed one whose values changed, and listed one
/// that no longer occurs.
fn unlisted_differences(between: Between, case: &str, left: &Value, right: &Value) -> Vec<String> {
    let found: BTreeMap<String, (Value, Value)> = differences(left, right)
        .into_iter()
        .map(|(pointer, l, r)| (pointer, (l, r)))
        .collect();
    let listed: BTreeMap<&str, &Divergence> = DIVERGENCES
        .iter()
        .filter(|d| d.between.contains(&between) && d.case == case)
        .map(|d| (d.pointer, d))
        .collect();
    let agrees = |want: &str, got: &Value| {
        want == ANY
            || serde_json::from_str::<Value>(want)
                .unwrap_or_else(|e| panic!("{case}: listed value {want} isn't JSON: {e}"))
                == *got
    };

    let mut problems = Vec::new();
    for (pointer, (l, r)) in &found {
        match listed.get(pointer.as_str()) {
            None => problems.push(format!(
                "{case}: unlisted difference at {pointer}: left {l}, right {r}"
            )),
            Some(d) if !agrees(d.left, l) || !agrees(d.right, r) => problems.push(format!(
                "{case}: the difference at {pointer} ({}) changed: listed left {}, right {}; \
                 found left {l}, right {r}",
                d.because, d.left, d.right
            )),
            Some(_) => {}
        }
    }
    for (pointer, d) in &listed {
        if !found.contains_key(*pointer) {
            problems.push(format!(
                "{case}: the listed difference at {pointer} ({}) no longer occurs — unlist it",
                d.because
            ));
        }
    }
    problems
}

#[tokio::test]
async fn each_twin_pair_evaluates_to_the_same_config_except_listed_divergences() {
    let mut problems = Vec::new();
    for case in cases() {
        let yaml = load_project(&case.join("batect.yml")).await.unwrap();
        let toml = load_project_native(&case.join("ratect.toml"))
            .await
            .unwrap();
        problems.extend(unlisted_differences(
            Between::Twins,
            case_name(&case),
            &evaluated(&yaml, &case),
            &evaluated(&toml, &case),
        ));
    }
    assert!(
        problems.is_empty(),
        "batect.yml (left) and ratect.toml (right) twins differ:\n  {}",
        problems.join("\n  ")
    );
}

/// A fresh, empty directory under the system temporary directory, removed
/// when dropped — so a panic mid-case doesn't leave it behind.
struct ScratchDir(PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_dir(label: &str) -> ScratchDir {
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

fn copy_dir(from: &Path, to: &Path) {
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

/// `ratect config convert` checked on its own terms: its output for each
/// case's `batect.yml`, loaded the way `ratect` loads it, against the
/// hand-written twin. Converted in a copy of the case, so the output resolves
/// its paths against the same files without writing into the source tree.
#[tokio::test]
async fn converting_each_twins_yaml_gives_its_toml_twins_config() {
    // Only `ratect_version`, which isn't part of `Config`, depends on it.
    let version = "0.11.0".parse().unwrap();
    let mut problems = Vec::new();
    for case in cases() {
        let loaded = Config::load_from_file(&case.join("batect.yml"))
            .await
            .unwrap();
        let converted = to_native_toml(&loaded.config, version).unwrap();

        let scratch = scratch_dir(case_name(&case));
        let copy = &scratch.0;
        copy_dir(&case, copy);
        let converted_file = copy.join("converted.toml");
        std::fs::write(&converted_file, converted).unwrap();
        let from_converted = load_project_native(&converted_file).await.unwrap();
        let twin = load_project_native(&case.join("ratect.toml"))
            .await
            .unwrap();

        problems.extend(unlisted_differences(
            Between::Converted,
            case_name(&case),
            &evaluated(&from_converted, copy),
            &evaluated(&twin, &case),
        ));
    }
    assert!(
        problems.is_empty(),
        "converted batect.yml (left) and ratect.toml twin (right) differ:\n  {}",
        problems.join("\n  ")
    );
}

/// Why a `type: git` include's fields are [`UNCOVERABLE`]: loading one clones
/// into the real `~/.ratect/incl`, whose root isn't configurable outside
/// `ratect-core`'s own unit tests — which cover these fields with a fake Git
/// client instead.
const GIT_INCLUDE: &str = "a git include clones into the real ~/.ratect/incl";

/// A schema property no case can use, and why.
struct Uncoverable {
    /// The schema file, under the repository's `schema/`.
    schema: &'static str,
    /// The property, labelled as [`schema_properties`] labels it.
    label: &'static str,
    because: &'static str,
}

/// Every schema property no case can use.
const UNCOVERABLE: &[Uncoverable] = &[
    Uncoverable {
        schema: "batect-config.schema.json",
        label: "Include#2.allow_host_paths",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "batect-config.schema.json",
        label: "Include#2.path",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "batect-config.schema.json",
        label: "Include#2.ref",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "batect-config.schema.json",
        label: "Include#2.repo",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "batect-config.schema.json",
        label: "Include#2.type",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "ratect-config.schema.json",
        label: "Include#1.allow_host_paths",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "ratect-config.schema.json",
        label: "Include#1.allow_nested_git_includes",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "ratect-config.schema.json",
        label: "Include#1.path",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "ratect-config.schema.json",
        label: "Include#1.ref",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "ratect-config.schema.json",
        label: "Include#1.repo",
        because: GIT_INCLUDE,
    },
    Uncoverable {
        schema: "ratect-config.schema.json",
        label: "Include#1.type",
        because: GIT_INCLUDE,
    },
];

/// Every property `schema` declares, labelled by where it sits: a
/// definition's name or `root`, then `.name` for a property, `[]` for an
/// array's items, `{}` for a map's values and `#i` for the `i`th `oneOf`/
/// `anyOf` alternative. A `$ref` is labelled by the definition it names.
fn schema_properties(schema: &Value) -> BTreeSet<String> {
    fn walk(node: &Value, location: &str, out: &mut BTreeSet<String>) {
        if node.get("$ref").is_some() {
            return;
        }
        if let Some(properties) = node.get("properties").and_then(Value::as_object) {
            for (name, child) in properties {
                let label = format!("{location}.{name}");
                out.insert(label.clone());
                walk(child, &label, out);
            }
        }
        if let Some(values) = node.get("additionalProperties").filter(|v| v.is_object()) {
            walk(values, &format!("{location}{{}}"), out);
        }
        if let Some(items) = node.get("items") {
            walk(items, &format!("{location}[]"), out);
        }
        for keyword in ["oneOf", "anyOf"] {
            for (index, alternative) in node
                .get(keyword)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                walk(alternative, &format!("{location}#{index}"), out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(schema, "root", &mut out);
    for (name, definition) in schema["definitions"].as_object().unwrap() {
        walk(definition, name, &mut out);
    }
    out
}

/// Every property label (see [`schema_properties`]) `document` uses, found by
/// walking it alongside `schema`. A key the schema doesn't declare — a YAML
/// anchor's `.extension`, a `<<` merge key — is skipped.
fn properties_used(schema: &Value, document: &Value) -> BTreeSet<String> {
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
    fn walk(
        schema: &Value,
        node: &Value,
        location: &str,
        value: &Value,
        out: &mut BTreeSet<String>,
    ) {
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
            Value::Object(object) => {
                let properties = node.get("properties").and_then(Value::as_object);
                let values = node.get("additionalProperties").filter(|v| v.is_object());
                for (key, child) in object {
                    if let Some(property) = properties.and_then(|p| p.get(key)) {
                        let label = format!("{location}.{key}");
                        out.insert(label.clone());
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
    let mut out = BTreeSet::new();
    walk(schema, schema, "root", document, &mut out);
    out
}

/// Every file in every case with `extension`, recursively.
fn case_files(extension: &str) -> Vec<PathBuf> {
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

/// Fails unless every property in `schema_file` is used by some case file
/// parsed by `parse`, or listed in [`UNCOVERABLE`] — and nothing listed there
/// is in fact covered.
fn assert_schema_covered(schema_file: &str, files: &[PathBuf], parse: impl Fn(&str) -> Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("schema")
        .join(schema_file);
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let declared = schema_properties(&schema);
    let mut used = BTreeSet::new();
    for file in files {
        let document = parse(&std::fs::read_to_string(file).unwrap());
        used.extend(properties_used(&schema, &document));
    }
    let exempt: BTreeMap<&str, &str> = UNCOVERABLE
        .iter()
        .filter(|row| row.schema == schema_file)
        .map(|row| (row.label, row.because))
        .collect();

    let uncovered: Vec<&String> = declared
        .iter()
        .filter(|label| !used.contains(*label) && !exempt.contains_key(label.as_str()))
        .collect();
    let needlessly_exempt: Vec<String> = exempt
        .iter()
        .filter(|(label, _)| used.contains(**label))
        .map(|(label, because)| format!("{label} ({because})"))
        .collect();
    let unknown_exempt: Vec<&&str> = exempt
        .keys()
        .filter(|label| !declared.contains(**label))
        .collect();
    assert!(
        uncovered.is_empty() && needlessly_exempt.is_empty() && unknown_exempt.is_empty(),
        "{schema_file}: no case uses {uncovered:#?}; covered after all, so unlist from \
         UNCOVERABLE: {needlessly_exempt:#?}; listed in UNCOVERABLE but not in the schema: \
         {unknown_exempt:#?}"
    );
}

#[test]
fn the_twins_cover_every_property_in_both_schemas() {
    assert_schema_covered("batect-config.schema.json", &case_files("yml"), |text| {
        noyalib::from_str(text).unwrap()
    });
    assert_schema_covered("ratect-config.schema.json", &case_files("toml"), |text| {
        toml::from_str(text).unwrap()
    });
}

/// What makes [`the_twins_cover_every_property_in_both_schemas`] mean
/// anything: it reads every `.yml` and `.toml` under `tests/twins/`, so a
/// file no case loads would count as coverage while proving nothing. Each one
/// has to be a case's root or named by that root's `include`. Nested
/// includes aren't followed: no case has one yet, and the first would fail
/// here as unloaded rather than pass unnoticed.
#[test]
fn every_case_file_is_loaded_by_its_case() {
    fn included(root: &Path, document: &Value) -> Vec<PathBuf> {
        document
            .get("include")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.as_str().or_else(|| entry.get("path")?.as_str()))
            .map(|path| root.parent().unwrap().join(path))
            .collect()
    }
    let mut loaded = BTreeSet::new();
    for case in cases() {
        let yaml = case.join("batect.yml");
        let toml = case.join("ratect.toml");
        let yaml_doc: Value = noyalib::from_str(&std::fs::read_to_string(&yaml).unwrap()).unwrap();
        let toml_doc: Value = toml::from_str(&std::fs::read_to_string(&toml).unwrap()).unwrap();
        loaded.extend(included(&yaml, &yaml_doc));
        loaded.extend(included(&toml, &toml_doc));
        loaded.extend([yaml, toml]);
    }
    let unloaded: Vec<PathBuf> = case_files("yml")
        .into_iter()
        .chain(case_files("toml"))
        .filter(|file| !loaded.contains(file))
        .collect();
    assert!(unloaded.is_empty(), "no case loads {unloaded:#?}");
}
