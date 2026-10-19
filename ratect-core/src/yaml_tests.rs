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
use serde::Deserialize;
use std::collections::HashMap;

/// The message `from_str` fails with — `noyalib`'s, location included.
fn error<T: DeserializeOwned + 'static>(yaml: &str) -> String {
    match from_str::<T>(yaml) {
        Ok(_) => panic!("expected {yaml:?} to be refused"),
        Err(e) => e.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct Text {
    value: String,
}

#[derive(Debug, Deserialize)]
struct MaybeText {
    value: Option<String>,
}

#[test]
fn an_unquoted_scalar_read_as_a_string_is_its_text_as_written() {
    for written in ["12.6000", "0x10", "010", "1e3", "+5", ".inf", "True", "yes"] {
        let Text { value } = from_str(&format!("value: {written}")).unwrap();
        assert_eq!(value, written);
    }
}

#[test]
fn a_quoted_scalar_is_its_contents_whatever_it_looks_like() {
    for (written, contents) in [("'12.6000'", "12.6000"), ("\"~\"", "~"), ("''", "")] {
        let Text { value } = from_str(&format!("value: {written}")).unwrap();
        assert_eq!(value, contents, "{written}");
    }
}

/// A target that isn't a string still gets the resolved value: only a
/// string read keeps the text.
#[test]
fn a_non_string_target_still_reads_the_resolved_value() {
    #[derive(Deserialize)]
    struct Typed {
        port: u16,
        ratio: f64,
        enabled: bool,
    }
    let typed: Typed = from_str("port: 0x50\nratio: 1.50\nenabled: true").unwrap();
    assert_eq!(typed.port, 80);
    assert_eq!(typed.ratio, 1.5);
    assert!(typed.enabled);
}

#[test]
fn every_spelling_of_a_null_is_refused_as_a_string_at_its_entry() {
    for null in ["", " ~", " null", " Null", " NULL"] {
        let message = error::<Text>(&format!("other: 1\nvalue:{null}\n"));
        assert!(
            message.contains("line 2, column ") && message.contains("value: "),
            "{null:?}: {message}"
        );
    }
}

#[test]
fn a_null_is_left_out_where_an_option_is_asked_for() {
    for null in ["", "~", "null"] {
        let MaybeText { value } = from_str(&format!("value: {null}")).unwrap();
        assert_eq!(value, None, "{null:?}");
    }
    let MaybeText { value } = from_str("value: 3.10").unwrap();
    assert_eq!(value.as_deref(), Some("3.10"));
    let entries: Vec<Option<String>> = from_str("[~, 1.0]").unwrap();
    assert_eq!(entries, [None, Some("1.0".to_string())]);
}

/// The wrapper reaches every place serde can ask for a string: a list
/// entry, a map value, a newtype, and each kind of enum variant. Each is
/// one of the paths `Refusing` forwards, so a forward that dropped the
/// wrapper would let that path's null through as text.
#[test]
fn a_null_is_refused_as_a_string_wherever_one_is_nested() {
    #[derive(Debug, Deserialize)]
    struct Name(#[allow(dead_code)] String);

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    enum Shape {
        Newtype(String),
        Tuple(String, String),
        Struct { field: String },
    }

    assert!(error::<Vec<String>>("[a, ~]").contains("line 1"));
    assert!(error::<HashMap<String, String>>("key: ~").contains("key: "));
    assert!(error::<Name>("~").contains("line 1"));
    assert!(error::<Shape>("Newtype: ~").contains("line 1"));
    assert!(error::<Shape>("Tuple: [a, ~]").contains("line 1"));
    assert!(error::<Shape>("Struct: {field: ~}").contains("line 1"));

    // And the same paths still read a value as written.
    let Name(name) = from_str("1.0").unwrap();
    assert_eq!(name, "1.0");
    match from_str::<Shape>("Struct: {field: 0x10}").unwrap() {
        Shape::Struct { field } => assert_eq!(field, "0x10"),
        other => panic!("{other:?}"),
    }
    match from_str::<Shape>("Tuple: [True, 1e3]").unwrap() {
        Shape::Tuple(a, b) => assert_eq!((a.as_str(), b.as_str()), ("True", "1e3")),
        other => panic!("{other:?}"),
    }
}

/// A flag, a number and an enum are no more nullable than a string: a null
/// read as one is refused as a null, not as a mismatch or as a variant
/// named `~` — on both of `noyalib`'s paths. An `Option` of one still reads
/// a null as left out.
#[test]
fn a_null_is_refused_as_a_flag_number_or_enum() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    enum Unit {
        A,
    }
    for message in [
        error::<Vec<bool>>("[true, ~]"),
        error::<Vec<u16>>("[1, ~]"),
        error::<Vec<f64>>("[1.5, ~]"),
        error::<Vec<Unit>>("[A, ~]"),
        error::<Vec<Unit>>("[A, !!null ~]"),
    ] {
        assert!(
            message.contains("[1]: invalid type: null, expected "),
            "{message}"
        );
    }

    let flags: Vec<Option<bool>> = from_str("[~, true]").unwrap();
    assert_eq!(flags, [None, Some(true)]);
    let numbers: Vec<Option<u16>> = from_str("[~, 0x50]").unwrap();
    assert_eq!(numbers, [None, Some(80)]);
    let units: Vec<Option<Unit>> = from_str("[~, A]").unwrap();
    assert!(matches!(units[..], [None, Some(Unit::A)]));
}

/// A visitor that doesn't read a null is refused in the same words as a
/// string target; one that refuses a null in its own words keeps them.
#[test]
fn a_null_refused_by_a_visitor_says_null_unless_the_visitor_said_otherwise() {
    struct Silent;
    impl<'de> Deserialize<'de> for Silent {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Silent;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("a silent thing")
                }
            }
            deserializer.deserialize_any(V)
        }
    }
    struct Outspoken;
    impl<'de> Deserialize<'de> for Outspoken {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Outspoken;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("an outspoken thing")
                }
                fn visit_none<E: de::Error>(self) -> Result<Outspoken, E> {
                    Err(E::custom("no null here, thank you"))
                }
            }
            deserializer.deserialize_any(V)
        }
    }

    let message = error::<Vec<Silent>>("[~]");
    assert!(
        message.contains("[0]: invalid type: null, expected a silent thing"),
        "{message}"
    );
    let message = error::<Vec<Outspoken>>("[~]");
    assert!(
        message.contains("[0]: no null here, thank you"),
        "{message}"
    );
}

/// A null reached through an alias is refused too: `noyalib`'s streaming
/// path resolves the alias before the string read sees it. It is located
/// at the anchor, which is `noyalib`'s choice, not this module's.
#[test]
fn a_null_reached_through_an_alias_is_refused_as_a_string() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Aliased {
        anchor: Option<String>,
        value: String,
    }
    let message = error::<Aliased>(
        "anchor: &nothing ~
value: *nothing
",
    );
    assert!(message.contains("null"), "{message}");
}

/// A tag anywhere sends the document down `noyalib`'s AST path. A null is
/// refused there too, still at its entry.
#[test]
fn a_null_is_refused_as_a_string_in_a_document_with_a_tag() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Tagged {
        tagged: String,
        value: String,
    }
    let message = error::<Tagged>("tagged: !!str a\nvalue: ~\n");
    assert!(
        message.contains("line 2, column ") && message.contains("value: "),
        "{message}"
    );
}

/// Pins the known divergence `TODO.md` 18 records: on the AST path the
/// option re-formats the resolved value rather than keeping the text, so
/// `3.10` beside a tag is `3.1`. When `noyalib` fixes it this fails —
/// assert `3.10` here instead and delete the TODO entry.
#[test]
fn a_document_with_a_tag_reformats_an_unquoted_number() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Tagged {
        tagged: String,
        value: String,
    }
    let Tagged { value, .. } = from_str("tagged: !!str a\nvalue: 3.10\n").unwrap();
    assert_eq!(value, "3.1");
}
