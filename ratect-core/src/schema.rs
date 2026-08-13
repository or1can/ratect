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
//! - It describes one *file*'s shape ([`crate::config::ConfigFile`] — including `include`,
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
//! `committed_schema_is_up_to_date` and its native counterpart fail if
//! either drifts, and print the one command that regenerates both.
//!
//! generates **two** JSON schemas from `config.rs`'s own types —
//! `batect.yml`'s (`config_file_schema`, committed at
//! `schema/batect-config.schema.json`) and, since 0.3.0, `ratect.toml`'s
//! (`native_config_file_schema`, at `schema/ratect-config.schema.json`) — see
//! [config reference](https://github.com/or1can/ratect/blob/main/docs/config-reference.md#editor-autocompletion-and-validation)
//! and the [`ratect.toml` reference](https://github.com/or1can/ratect/blob/main/docs/ratect-config-reference.md#editor-support)
//! for the user-facing halves. The native one is the same generated base put
//! through `make_native`, which applies *exactly* the two differences that define
//! the format: it drops the compact string form from the
//! `volumes`/`ports`/`devices`/`include` `oneOf`s (object-only), and adds the
//! native-only `extends` field the compat schema skips. Everything else is shared
//! because both formats parse into one `Config`. One asymmetry that's deliberate:
//! only the compat schema carries a `patternProperties` entry admitting top-level
//! `.`-prefixed keys (YAML extensions — TOML has no anchors for one to hold). The
//! same `RATECT_UPDATE_SCHEMA=1` run regenerates both, and a drift in either fails
//! its own test. Things to know before touching it: the schema is
//! generated from `ConfigFile` (`pub(crate)` for exactly this reason), not
//! `Config` — one *file*'s shape, `include` and all, is what an editor has open,
//! not the merged result; it's emitted as draft-07 rather than schemars' own
//! default 2020-12, because `yaml-language-server` (what VS Code and JetBrains
//! run) only implements draft-07 fully — under 2020-12 it drops keywords sitting
//! beside a `$ref`, which is every description on a `$ref`'d field; every type
//! with a hand-written `Deserialize` impl needs a hand-written `JsonSchema` impl
//! to match, and they live here rather than in `config.rs` (`PortMapping`,
//! `PortRange`, `DeviceMapping`, `VolumeMount`, `BuildSecret`, `IncludeEntry` —
//! add one here whenever a new string-or-object config type lands, or the derive
//! won't compile); and field documentation is the config types' own doc comments,
//! run through `summarize` (first paragraph, reflowed, rustdoc link syntax
//! stripped) rather than a second `schemars(description = ...)` copy per field,
//! which would be free to drift. So a new config field needs a doc comment whose
//! *first paragraph* stands alone as user-facing documentation — everything after
//! it is for contributors and never reaches the schema. The `schema` feature also
//! pulls in `jsonschema` (an optional normal dependency, not a dev-dependency —
//! Cargo won't let those be optional) purely for this module's own tests, which
//! validate every fixture in the repository against the generated schema.

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

    if let Some(definition) = definitions.get_mut("VolumeMount") {
        add_native_property(
            definition,
            "cache",
            "scope",
            serde_json::json!({
                "enum": ["project", "shared"],
                "description": "Whether this cache is private to the project (the default) \
                                or shared with every other project on this machine. A \
                                shared cache carries no project key, so one Cargo registry \
                                or npm cache is populated once and reused everywhere.",
            }),
        );
    }

    if let Some(definition) = definitions.get_mut("Include") {
        // Corrects, rather than adds: the shared description names compat's
        // only candidate, but `git_bundle_candidates` probes
        // `ratect-bundle.toml` first here. Overwriting the property is how
        // `add_native_property` already behaves, so one helper covers both.
        add_native_property(
            definition,
            "git",
            "path",
            serde_json::json!({
                "type": "string",
                "description": "The file to include from within the repository. \
                                Defaults to ratect-bundle.toml, then batect-bundle.yml.",
            }),
        );
        add_native_property(
            definition,
            "git",
            "allow_nested_git_includes",
            serde_json::json!({
                "type": "boolean",
                "description": "Let this bundle declare Git includes of its own, fetching \
                                configuration from remotes you have not named. Refused by \
                                default. Applies only to the bundle named here — the bundles \
                                it admits cannot pass the permission on — and only when set \
                                in your own configuration. Defaults to false.",
            }),
        );
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
        // Corrects the shared description on both counts it is wrong about
        // here: this format resolves expressions in `image`, and `extends`
        // means the compat "exactly one of image/build_directory" rule does
        // not hold (see the divergence table in the native reference).
        properties.insert(
            "image".to_string(),
            serde_json::json!({
                "type": ["string", "null"],
                "description": "The image to run, in Docker's own `name:tag` form. Supports \
                                expressions, so a tag can be chosen per run — e.g. \
                                \"my-repo/my-image:${IMAGE_TAG:-latest}\". Usually paired with \
                                `build_directory` as the alternative, though `extends` makes \
                                both — or neither — legal on one container.",
            }),
        );
    }
}

/// Adds a native-only `property` to the one form of a tagged `oneOf` whose
/// `type` const is `tag` — the `cache` form of a volume mount, the `git` form
/// of an include, and whatever comes next.
///
/// These are kept out of the compat schema for the same reason
/// `ratect-compat` rejects them at load: Batect has no such field, so
/// advertising it there would autocomplete a config that real `batect`
/// refuses.
///
/// Finds the form by its `type` const rather than by index, so reordering the
/// `oneOf` can't silently attach a field to `local`, `tmpfs` or `file`.
fn add_native_property(
    definition: &mut serde_json::Value,
    tag: &str,
    property: &str,
    schema: serde_json::Value,
) {
    let forms = match definition.get_mut("oneOf").and_then(|f| f.as_array_mut()) {
        Some(forms) => forms,
        // `drop_string_shorthand` unwraps a single remaining form, so a
        // one-form definition is the object itself.
        None => std::slice::from_mut(definition),
    };
    for form in forms {
        let tagged = form
            .get("properties")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.get("const"))
            .and_then(serde_json::Value::as_str)
            == Some(tag);
        if !tagged {
            continue;
        }
        if let Some(properties) = form
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        {
            properties.insert(property.to_string(), schema.clone());
        }
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
                        "allow_host_paths": {
                            "type": "boolean",
                            "description": "Let this bundle's containers resolve 'volumes'/\
                                            'build_directory' paths outside the usual \
                                            containment (its own clone, or your project \
                                            directory) — for a bundle you trust that needs, \
                                            say, a shared cache under your home directory. \
                                            Applies only to the bundle named here, never to \
                                            bundles it includes itself, and only when set in \
                                            your own configuration. Defaults to false.",
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
#[path = "schema_tests.rs"]
mod tests;
