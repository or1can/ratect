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

//! The JSON schemas for `batect.yml` ([`config_file_schema`]) and its native
//! counterpart `ratect.toml` ([`native_config_file_schema`]), for editor
//! autocompletion/validation — both generated from [`crate::config`]'s own
//! types (via `schemars`) rather than hand-maintained, so they can't drift
//! from what Ratect actually accepts. The native one is the compat schema
//! transformed for the native format's shape (object-only, plus `extends`);
//! everything below applies to both.
//!
//! Deliberately **not** Batect's own published schema (SchemaStore's catalog
//! entry for `batect.yml`, hosted at `ide-integration.batect.dev`): that one
//! describes Batect's full field set, so it would green-light fields Ratect
//! doesn't support — a false pass in the editor, on exactly the fields a
//! migrating project most needs to hear about. See
//! [Differences from Batect](../../docs/differences-from-batect.md) for the
//! itemized status of each.
//!
//! Two deliberate choices worth knowing:
//!
//! - It describes one *file*'s shape ([`ConfigFile`] — including `include`,
//!   which only exists per-file), not the merged [`Config`](crate::config::Config)
//!   that several included files add up to. That's what an editor has open.
//! - Draft-07, not schemars' own default (2020-12): that's what
//!   `yaml-language-server` (the VS Code YAML extension, and JetBrains'
//!   YAML support) actually implements fully. A 2020-12 schema still mostly
//!   works there, but `$ref` alongside sibling keywords silently loses the
//!   siblings — which is exactly how every description on a `$ref`'d field
//!   would go missing.
//!
//! Both generated schemas are committed — at
//! [`schema/batect-config.schema.json`](../../schema/batect-config.schema.json)
//! and [`schema/ratect-config.schema.json`](../../schema/ratect-config.schema.json)
//! — since those files are what editors actually consume;
//! [`tests::committed_schema_is_up_to_date`] and its native counterpart fail if
//! either drifts, and print the one command that regenerates both.

use crate::config::{BuildSecret, DeviceMapping, PortMapping, PortRange, VolumeMount};
use schemars::generate::SchemaSettings;
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use std::borrow::Cow;

/// The generated schema, as JSON — see the module docs.
pub fn config_file_schema() -> serde_json::Value {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<crate::config::ConfigFile>();
    let mut json = serde_json::to_value(&schema).expect("a generated schema is always valid JSON");
    summarize_descriptions(&mut json);
    let object = json
        .as_object_mut()
        .expect("a root schema is always an object");
    object.insert("title".to_string(), "Ratect configuration".into());
    object.insert(
        "description".to_string(),
        concat!(
            "A Ratect (batect-compatible) task configuration file. Describes the subset of ",
            "Batect's own configuration format that Ratect actually accepts — see ",
            "https://github.com/or1can/ratect/blob/main/docs/differences-from-batect.md",
        )
        .into(),
    );
    // Batect *extensions*: a top-level key starting with `.` holds an anchor
    // for the rest of the file and is otherwise ignored (see
    // `config::parse_yaml_config_file`). `additionalProperties: false` only
    // applies to what neither `properties` nor `patternProperties` matched, so
    // this admits them without loosening anything else — an editor must not
    // flag configuration Ratect actually accepts.
    object.insert(
        "patternProperties".to_string(),
        serde_json::json!({
            "^\\.": {
                "description": "An extension: ignored by Ratect, and used only to hold a YAML \
                                anchor that the rest of the file aliases.",
            },
        }),
    );
    json
}

/// The generated schema for the native `ratect.toml` format — the same base as
/// [`config_file_schema`] (both describe one *file*'s shape, generated from
/// [`ConfigFile`](crate::config::ConfigFile)), transformed into the native
/// format's stricter shape: the compact string shorthands for
/// `volumes`/`ports`/`devices`/`include` are dropped (native is object-only),
/// and the native-only `extends` field is added. See
/// [decisions/0003](../../decisions/0003-ratect-native-config-format.md) and
/// the [`ratect.toml` reference](../../docs/ratect-config-reference.md).
///
/// JSON Schema validates TOML as readily as YAML — a TOML-aware editor
/// extension (taplo / "Even Better TOML") consumes exactly this. Committed at
/// [`schema/ratect-config.schema.json`](../../schema/ratect-config.schema.json).
pub fn native_config_file_schema() -> serde_json::Value {
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<crate::config::ConfigFile>();
    let mut json = serde_json::to_value(&schema).expect("a generated schema is always valid JSON");
    summarize_descriptions(&mut json);
    make_native(&mut json);
    let object = json
        .as_object_mut()
        .expect("a root schema is always an object");
    object.insert("title".to_string(), "Ratect native configuration".into());
    object.insert(
        "description".to_string(),
        concat!(
            "A ratect.toml native task configuration file — the format the `ratect` binary ",
            "reads by default. See ",
            "https://github.com/or1can/ratect/blob/main/docs/ratect-config-reference.md",
        )
        .into(),
    );
    json
}

/// Transforms the base (compat-shaped) schema into the native format's: the
/// list entries become object-only, and a container gains the native-only
/// `extends` field. Everything else — field names, types, descriptions, the
/// `deny_unknown_fields` strictness — is identical, because both formats parse
/// into the same [`Config`](crate::config::Config).
fn make_native(json: &mut serde_json::Value) {
    let Some(definitions) = json
        .get_mut("definitions")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    // Drop the compact string form from each string-or-object field: a native
    // `ratect.toml` spells these out as objects (inline tables or `[[...]]`).
    // `PortRange` is deliberately left alone — its string form is a port
    // *range* ("6000-6010"), which native still uses.
    for name in ["PortMapping", "DeviceMount", "VolumeMount", "Include"] {
        if let Some(definition) = definitions.get_mut(name) {
            drop_string_shorthand(definition);
        }
    }

    // Add the native-only `extends` field to a container — skipped from the
    // compat schema (`Container::extends`'s `schemars(skip)`, since
    // `ratect-compat` rejects it), but valid and worth completing here.
    if let Some(properties) = definitions
        .get_mut("Container")
        .and_then(|container| container.get_mut("properties"))
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.insert(
            "extends".to_string(),
            serde_json::json!({
                "type": "string",
                "description": "Inherit every field from another container by name, then \
                                override only the fields set here — ratect's native replacement \
                                for YAML anchors. Shallow and per-field: a field set here \
                                replaces the inherited one; a field left unset is taken from the \
                                named container.",
            }),
        );
    }
}

/// Removes the `type: string` shorthand from a definition's `oneOf` of accepted
/// forms, leaving the object form(s). If a single form remains, the `oneOf`
/// wrapper is unwrapped so the definition *is* that object.
fn drop_string_shorthand(definition: &mut serde_json::Value) {
    let Some(forms) = definition
        .get_mut("oneOf")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    forms.retain(|form| form.get("type") != Some(&serde_json::Value::from("string")));
    if forms.len() == 1 {
        *definition = forms.remove(0);
    }
}

/// Rewrites every `description` in the generated schema from the Rust doc
/// comment it came from into something an editor tooltip can usefully show
/// — see [`summarize`]. Done as a pass over the finished JSON rather than
/// by writing `schemars(description = "...")` on all ~90 config fields:
/// that would be a second, silently-driftable copy of every field's
/// documentation, and the whole point of generating this schema is that it
/// can't drift.
fn summarize_descriptions(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(description)) = object.get_mut("description") {
                *description = summarize(description);
            }
            for (_, child) in object.iter_mut() {
                summarize_descriptions(child);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(summarize_descriptions),
        _ => {}
    }
}

/// One doc comment, as a tooltip: its first paragraph only (rustdoc's own
/// summary convention — everything after it is the contributor-facing
/// "why", which an editor has no use for), reflowed onto one line, with
/// rustdoc's intra-doc link brackets removed.
///
/// Descriptions render as Markdown in every editor that consumes this
/// (`yaml-language-server` and JetBrains both do), so ordinary Markdown —
/// including real links, and the backticks around code — is left alone.
/// Only rustdoc-specific syntax, which would render as literal brackets, is
/// rewritten.
fn summarize(description: &str) -> String {
    let first_paragraph = description.split("\n\n").next().unwrap_or(description);
    let reflowed = first_paragraph
        .split('\n')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    strip_intra_doc_links(reflowed.trim())
}

/// `[`Container::volumes`]` -> `` `Container::volumes` ``, and
/// `[expressions](#expressions)` -> `expressions` — rustdoc's two link
/// forms, neither of which means anything outside rustdoc. A link to a real
/// URL is a genuine Markdown link and survives untouched.
fn strip_intra_doc_links(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|index| open + index) else {
            break;
        };
        let label = &rest[open + 1..close];
        output.push_str(&rest[..open]);
        rest = &rest[close + 1..];
        let link_target = rest
            .strip_prefix('(')
            .and_then(|target| target.find(')').map(|end| (target, end)));
        match link_target {
            // A Markdown link to somewhere an editor can actually follow.
            Some((target, end)) if target[..end].starts_with("http") => {
                output.push('[');
                output.push_str(label);
                output.push_str("](");
                output.push_str(&target[..end]);
                output.push(')');
                rest = &target[end + 1..];
            }
            // A link relative to the rendered rustdoc/repository — the text
            // is all that's meaningful here.
            Some((target, end)) => {
                output.push_str(label);
                rest = &target[end + 1..];
            }
            None => output.push_str(label),
        }
    }
    output.push_str(rest);
    output
}

/// Batect's Go-style duration strings (`health_check`'s `interval`/
/// `start_period`/`timeout`) — see [`crate::config::parse_duration`], which
/// is what actually enforces this. The `pattern` here is the same grammar
/// stated declaratively, so an editor can flag a typo without running
/// Ratect; it's deliberately no stricter than the parser (a schema that
/// rejected something Ratect accepts would be worse than one that didn't
/// check at all).
pub(crate) fn duration_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "string",
        "pattern": r"^\+?(0|([0-9]*\.?[0-9]+(ns|us|µs|μs|ms|s|m|h))+)$",
        "description": "A duration, in Batect's Go-style format: one or more \
                        <number><unit> components, where a unit is one of ns, us (or µs/μs), \
                        ms, s, m, h — for example \"500ms\", \"2s\", \"1m30s\", \"1.5h\". A \
                        bare \"0\" is also accepted. Must not be negative.",
        "examples": ["2s", "1m30s", "500ms", "0"],
    })
}

/// `shm_size`: Batect's own size-string format, or a plain integer number of
/// bytes — see [`crate::config::parse_byte_size`], the actual enforcement.
pub(crate) fn byte_size_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "oneOf": [
            {
                "type": "string",
                "pattern": "^[0-9]+[bkmgBKMG]?$",
                "description": "A size, as a number optionally suffixed with a unit: b (bytes, \
                                the default), k, m or g — for example \"128m\".",
                "examples": ["128m", "1g", "512k"],
            },
            {
                "type": "integer",
                "minimum": 0,
                "description": "A size in bytes.",
            },
        ],
    })
}

/// The shared shape of the string-or-object fields below: Batect accepts a
/// compact `"a:b[:c]"`-style string *or* a spelled-out object for the same
/// thing, so each one's schema is a `oneOf` of the two.
fn string_or_object(string: Schema, object: Schema) -> Schema {
    json_schema!({ "oneOf": [string, object] })
}

impl JsonSchema for PortRange {
    fn schema_name() -> Cow<'static, str> {
        "PortRange".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 65535,
                    "description": "A single port.",
                },
                {
                    "type": "string",
                    "pattern": "^[0-9]+(-[0-9]+)?$",
                    "description": "A single port (\"8080\") or an inclusive range of \
                                    consecutive ports (\"1000-1010\", given in ascending \
                                    order).",
                },
            ],
        })
    }
}

impl JsonSchema for PortMapping {
    fn schema_name() -> Cow<'static, str> {
        "PortMapping".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let port_range = generator.subschema_for::<PortRange>();
        let port_range2 = port_range.clone();
        string_or_object(
            json_schema!({
                "type": "string",
                "pattern": "^[0-9]+(-[0-9]+)?:[0-9]+(-[0-9]+)?(/[a-zA-Z]+)?$",
                "description": "A port mapping, as \"local:container\", with optional ranges \
                                and protocol: \"local:container\", \"from-to:from-to\", \
                                \"local:container/protocol\". The protocol defaults to tcp.",
                "examples": ["8080:80", "1000-1010:2000-2010", "8080:80/tcp"],
            }),
            json_schema!({
                "type": "object",
                "properties": {
                    "local": port_range,
                    "container": port_range2,
                    "protocol": {
                        "type": "string",
                        "description": "The protocol to map. Defaults to tcp.",
                    },
                },
                "required": ["local", "container"],
                "additionalProperties": false,
            }),
        )
    }
}

impl JsonSchema for DeviceMapping {
    fn schema_name() -> Cow<'static, str> {
        "DeviceMount".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        string_or_object(
            json_schema!({
                "type": "string",
                "description": "A device mount, as \"local_path:container_path\" or \
                                \"local_path:container_path:options\", where options is \
                                Docker's cgroup permissions string.",
                "examples": ["/dev/kvm:/dev/kvm", "/dev/sda:/dev/xvda:rwm"],
            }),
            json_schema!({
                "type": "object",
                "properties": {
                    "local": {
                        "type": "string",
                        "description": "The path to the device on the host.",
                    },
                    "container": {
                        "type": "string",
                        "description": "The path the device is available at inside the \
                                        container.",
                    },
                    "options": {
                        "type": "string",
                        "description": "Docker's cgroup permissions string (for example \
                                        \"rwm\"). Docker's own default applies when omitted.",
                    },
                },
                "required": ["local", "container"],
                "additionalProperties": false,
            }),
        )
    }
}

impl JsonSchema for VolumeMount {
    fn schema_name() -> Cow<'static, str> {
        "VolumeMount".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "string",
                    "description": "A host path bind-mounted into the container, as \
                                    \"local_path:container_path\" or \
                                    \"local_path:container_path:options\". Only local mounts \
                                    have this compact form — cache and tmpfs mounts must use \
                                    the object form.",
                    "examples": [".:/code", "./data:/data:ro"],
                },
                {
                    "type": "object",
                    "properties": {
                        "type": {
                            "const": "local",
                            "description": "A host path bind-mounted into the container. The \
                                            default when 'type' is omitted.",
                        },
                        "local": {
                            "type": "string",
                            "description": "The path on the host, resolved relative to the \
                                            directory of the file declaring it. Supports \
                                            expressions.",
                        },
                        "container": {
                            "type": "string",
                            "description": "The path inside the container.",
                        },
                        "options": {
                            "type": "string",
                            "description": "Docker mount options (for example \"ro\").",
                        },
                    },
                    "required": ["local", "container"],
                    "additionalProperties": false,
                },
                {
                    "type": "object",
                    "properties": {
                        "type": {
                            "const": "cache",
                            "description": "A cache that persists between ratect invocations \
                                            — a Docker volume by default, or a directory \
                                            under .batect/caches with --cache-type=directory.",
                        },
                        "name": {
                            "type": "string",
                            "description": "The cache's name, unique within this project.",
                        },
                        "container": {
                            "type": "string",
                            "description": "The path inside the container.",
                        },
                        "options": {
                            "type": "string",
                            "description": "Docker mount options (for example \"ro\").",
                        },
                    },
                    "required": ["type", "name", "container"],
                    "additionalProperties": false,
                },
                {
                    "type": "object",
                    "properties": {
                        "type": {
                            "const": "tmpfs",
                            "description": "An in-memory filesystem, lost when the container \
                                            exits.",
                        },
                        "container": {
                            "type": "string",
                            "description": "The path inside the container.",
                        },
                        "options": {
                            "type": "string",
                            "description": "tmpfs options, forwarded to Docker verbatim (for \
                                            example \"size=64m,mode=1770\").",
                        },
                    },
                    "required": ["type", "container"],
                    "additionalProperties": false,
                },
            ],
        })
    }
}

impl JsonSchema for crate::config::IncludeEntry {
    fn schema_name() -> Cow<'static, str> {
        "Include".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "string",
                    "description": "The path to another configuration file to merge into this \
                                    one, relative to this file's own directory.",
                    "examples": ["tasks/build.yml"],
                },
                {
                    "type": "object",
                    "properties": {
                        "type": {
                            "const": "file",
                            "description": "A local configuration file. The default when \
                                            'type' is omitted.",
                        },
                        "path": {
                            "type": "string",
                            "description": "The path to the file, relative to this file's own \
                                            directory.",
                        },
                    },
                    "required": ["path"],
                    "additionalProperties": false,
                },
                {
                    "type": "object",
                    "properties": {
                        "type": {
                            "const": "git",
                            "description": "A bundle from a Git repository, cloned once and \
                                            cached under ~/.ratect/incl.",
                        },
                        "repo": {
                            "type": "string",
                            "description": "The repository to clone.",
                        },
                        "ref": {
                            "type": "string",
                            "description": "The tag, branch or commit to check out.",
                        },
                        "path": {
                            "type": "string",
                            "description": "The file to include from within the repository. \
                                            Defaults to batect-bundle.yml.",
                        },
                    },
                    "required": ["type", "repo", "ref"],
                    "additionalProperties": false,
                },
            ],
        })
    }
}

impl JsonSchema for BuildSecret {
    fn schema_name() -> Cow<'static, str> {
        "BuildSecret".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "environment": {
                            "type": "string",
                            "description": "The name of a host environment variable to read \
                                            the secret's value from.",
                        },
                    },
                    "required": ["environment"],
                    "additionalProperties": false,
                },
                {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The path to a file on the host containing the \
                                            secret's value. Supports expressions.",
                        },
                    },
                    "required": ["path"],
                    "additionalProperties": false,
                },
            ],
            "description": "A secret exposed to a build via BuildKit's secret mounts. Exactly \
                            one of 'environment' or 'path' is required.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The path of the committed schema, relative to the repository root.
    const COMMITTED: &str = "schema/batect-config.schema.json";

    /// Its native counterpart — see [`native_config_file_schema`].
    const NATIVE_COMMITTED: &str = "schema/ratect-config.schema.json";

    #[test]
    fn a_description_keeps_its_first_paragraph_only_reflowed() {
        assert_eq!(
            summarize("The first\nparagraph, wrapped.\n\nThe contributor-facing why."),
            "The first paragraph, wrapped."
        );
    }

    #[test]
    fn intra_doc_links_lose_their_brackets_but_real_links_survive() {
        assert_eq!(
            summarize("See [`Container::volumes`] and [expressions](#expressions)."),
            "See `Container::volumes` and expressions."
        );
        assert_eq!(
            summarize("See [moby#41563](https://github.com/moby/moby/pull/41563) for why."),
            "See [moby#41563](https://github.com/moby/moby/pull/41563) for why."
        );
    }

    /// The generated schema is what an editor validates against, so a
    /// mistake here is invisible until someone's valid config is flagged
    /// (or an invalid one isn't). These pin the parts hand-written above,
    /// where a derive isn't doing the work for us.
    #[test]
    fn both_forms_of_every_string_or_object_field_are_described() {
        let schema = config_file_schema();
        for (definition, expected_forms) in [
            ("PortMapping", 2),
            ("DeviceMount", 2),
            // local-string, local-object, cache, tmpfs.
            ("VolumeMount", 4),
            ("BuildSecret", 2),
            // path-string, file-object, git-object.
            ("Include", 3),
            // a bare port number, or a "from-to" string.
            ("PortRange", 2),
        ] {
            let forms = schema["definitions"][definition]["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{definition} should be a oneOf of its accepted forms"));
            assert_eq!(
                forms.len(),
                expected_forms,
                "{definition} should describe {expected_forms} accepted forms"
            );
        }
    }

    /// `deny_unknown_fields` is what makes a typo'd field name an error
    /// rather than a silently ignored one — the schema has to say so too,
    /// or the editor stays quiet about exactly that mistake.
    #[test]
    fn unknown_fields_are_rejected_by_the_schema_too() {
        let schema = config_file_schema();
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert_eq!(
            schema["definitions"]["Container"]["additionalProperties"],
            serde_json::json!(false)
        );
    }

    fn committed_path() -> std::path::PathBuf {
        // `CARGO_MANIFEST_DIR` is `ratect-core/`; the schema lives at the
        // workspace root beside it.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("ratect-core always has a parent directory")
            .join(COMMITTED)
    }

    fn rendered() -> String {
        let mut json = serde_json::to_string_pretty(&config_file_schema())
            .expect("a generated schema is always serializable");
        json.push('\n');
        json
    }

    /// The whole point of the schema: a config Ratect accepts must not be
    /// flagged in the editor. Every fixture in the repository that parses
    /// as a config file is validated against the generated schema — the
    /// direction that matters, since a schema that's merely too permissive
    /// costs a missed warning, while one that's too strict puts a red
    /// squiggle under working configuration.
    ///
    /// Only that direction is asserted: a fixture that *doesn't* parse
    /// isn't required to fail validation, because plenty of Ratect's own
    /// rules (a task needing `run` or `prerequisites`, `customise` naming a
    /// container in the graph, matching port-range sizes) are relationships
    /// between fields that JSON Schema has no way to express.
    #[test]
    fn every_config_ratect_accepts_validates_against_the_schema() {
        let validator = jsonschema::draft7::new(&config_file_schema())
            .expect("the generated schema should itself be a valid draft-07 schema");

        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("ratect-core always has a parent directory")
            .join("ratect-compat/tests/fixtures");
        let mut checked = 0;
        for entry in std::fs::read_dir(&fixtures).expect("failed to list the fixture directory") {
            let path = entry
                .expect("failed to read a fixture directory entry")
                .path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("yml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("failed to read a fixture");
            // Parsed twice deliberately: once into the real config type
            // (which decides whether this fixture is one Ratect accepts at
            // all), and once into plain JSON (which is what the schema
            // describes — the same document an editor sees).
            if noyalib::from_str::<crate::config::ConfigFile>(&text).is_err() {
                continue;
            }
            let document: serde_json::Value =
                noyalib::from_str(&text).expect("a fixture that parses as config is also JSON");
            if let Err(error) = validator.validate(&document) {
                panic!(
                    "{} is valid configuration but the schema rejects it: {error}",
                    path.display()
                );
            }
            checked += 1;
        }
        // A misspelled directory would otherwise make this pass vacuously.
        assert!(
            checked > 10,
            "expected to validate the repository's fixtures, but only found {checked}"
        );
    }

    /// The positive control for the test above: something the schema is
    /// supposed to catch really is caught, so a green run there means the
    /// fixtures passed rather than the validator having nothing to say.
    #[test]
    fn a_misspelled_field_is_rejected_by_the_schema() {
        let validator = jsonschema::draft7::new(&config_file_schema())
            .expect("the generated schema should itself be a valid draft-07 schema");
        let typo = serde_json::json!({
            "containers": {"app": {"imagee": "alpine:3.18"}},
            "tasks": {"check": {"run": {"container": "app"}}},
        });
        assert!(
            validator.validate(&typo).is_err(),
            "'imagee' should be rejected — deny_unknown_fields is what makes this a typo, \
             not a silently ignored field"
        );
    }

    /// The native `ratect.toml` schema differs from the `batect.yml` one in
    /// exactly two ways, and this pins both: it adds `extends` (which the
    /// compat schema must *not* have, since `ratect-compat` rejects it), and
    /// it drops the compact string shorthands (native is object-only).
    #[test]
    fn the_native_schema_adds_extends_and_drops_the_string_shorthands() {
        let native = native_config_file_schema();
        let compat = config_file_schema();

        assert!(
            native["definitions"]["Container"]["properties"]["extends"].is_object(),
            "extends should be a native Container field"
        );
        assert!(
            compat["definitions"]["Container"]["properties"]["extends"].is_null(),
            "extends must not appear in the batect.yml schema — ratect-compat rejects it"
        );

        // VolumeMount/Include keep a oneOf of *object* forms only; the string
        // shorthand is gone.
        for (definition, object_forms) in [("VolumeMount", 3), ("Include", 2)] {
            let forms = native["definitions"][definition]["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{definition} should still be a oneOf of object forms"));
            assert_eq!(
                forms.len(),
                object_forms,
                "{definition} keeps only its object forms"
            );
            for form in forms {
                assert_eq!(
                    form["type"],
                    serde_json::json!("object"),
                    "every native {definition} form is an object"
                );
            }
        }
        // PortMapping/DeviceMount had a single object form, so the oneOf is
        // unwrapped to just that object.
        for definition in ["PortMapping", "DeviceMount"] {
            assert_eq!(
                native["definitions"][definition]["type"],
                serde_json::json!("object"),
                "{definition} should collapse to its object form"
            );
            assert!(
                native["definitions"][definition].get("oneOf").is_none(),
                "{definition} should no longer be a oneOf"
            );
        }
    }

    /// The native counterpart of
    /// [`every_config_ratect_accepts_validates_against_the_schema`], for TOML:
    /// the repository's real native configs — the root `ratect.toml` dev
    /// config (object-form volumes and caches) and the `native.toml` fixture
    /// (which uses `extends`) — must not be flagged by the native schema.
    #[test]
    fn every_native_config_validates_against_the_native_schema() {
        let validator = jsonschema::draft7::new(&native_config_file_schema())
            .expect("the generated native schema should itself be a valid draft-07 schema");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("ratect-core always has a parent directory");
        for relative in ["ratect.toml", "ratect/tests/fixtures/native.toml"] {
            let path = root.join(relative);
            let text = std::fs::read_to_string(&path).expect("failed to read a native config");
            let document: serde_json::Value =
                toml::from_str(&text).expect("a native config is valid TOML");
            if let Err(error) = validator.validate(&document) {
                panic!(
                    "{} is valid native configuration but the schema rejects it: {error}",
                    path.display()
                );
            }
        }
    }

    fn native_committed_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("ratect-core always has a parent directory")
            .join(NATIVE_COMMITTED)
    }

    fn native_rendered() -> String {
        let mut json = serde_json::to_string_pretty(&native_config_file_schema())
            .expect("a generated schema is always serializable");
        json.push('\n');
        json
    }

    /// Regenerate with `RATECT_UPDATE_SCHEMA=1 cargo test -p ratect-core
    /// --features schema schema::` — the committed file is what editors
    /// actually consume, so it has to be checked in, and this is what keeps
    /// it honest when a config type changes.
    #[test]
    fn committed_schema_is_up_to_date() {
        let path = committed_path();
        let rendered = rendered();
        if std::env::var_os("RATECT_UPDATE_SCHEMA").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("failed to create schema dir");
            std::fs::write(&path, &rendered).expect("failed to write schema");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {COMMITTED} ({error}) — regenerate it with \
                    RATECT_UPDATE_SCHEMA=1 cargo test -p ratect-core --features schema schema::"
            )
        });
        assert_eq!(
            committed, rendered,
            "{COMMITTED} is out of date — regenerate it with RATECT_UPDATE_SCHEMA=1 cargo test \
             -p ratect-core --features schema schema::"
        );
    }

    /// The native schema's own up-to-date check — same `RATECT_UPDATE_SCHEMA=1`
    /// regenerates both committed files at once.
    #[test]
    fn committed_native_schema_is_up_to_date() {
        let path = native_committed_path();
        let rendered = native_rendered();
        if std::env::var_os("RATECT_UPDATE_SCHEMA").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("failed to create schema dir");
            std::fs::write(&path, &rendered).expect("failed to write schema");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {NATIVE_COMMITTED} ({error}) — regenerate it with \
                    RATECT_UPDATE_SCHEMA=1 cargo test -p ratect-core --features schema schema::"
            )
        });
        assert_eq!(
            committed, rendered,
            "{NATIVE_COMMITTED} is out of date — regenerate it with RATECT_UPDATE_SCHEMA=1 cargo \
             test -p ratect-core --features schema schema::"
        );
    }
}
