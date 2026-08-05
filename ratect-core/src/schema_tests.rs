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
