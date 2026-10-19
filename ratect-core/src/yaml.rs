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

//! Reading Batect-format YAML text — the one `noyalib` configuration every
//! read into the config model goes through (a config file, `project_name_of`'s
//! YAML branch, a config vars file, and the unit tests that parse one), so
//! none of them can drift from the others. A test reading YAML into a
//! `noyalib::Value` instead doesn't need it: the option only changes what a
//! string target receives.
//!
//! **A string is the scalar as written.** Batect reads every text field
//! with kaml's `decodeString`, which returns a scalar's source text whatever
//! type it looks like, so `F: 12.6000` is `"12.6000"` and `V: 0x10` is
//! `"0x10"` (#378, #376, #324). `noyalib`'s `plain_scalar_strings` does the
//! same for any `String` target. A visitor that accepts the resolved number
//! and calls `to_string()` instead (as `deserialize_scalar_string_map` once
//! did) can't: the number has already lost its spelling.
//!
//! **Except a null, which is no string at all.** `plain_scalar_strings`
//! hands a plain null — an empty value, `~`, `null` — to a `String` target as
//! its text too, so on its own it would turn every `KEY:` with nothing after
//! it into `""` where kaml's non-null `decodeString` refuses it. [`from_str`]
//! therefore wraps the deserializer in [`Refusing`], which reads every
//! string as an `Option` first and refuses the `None` — *inside* the parse,
//! so `noyalib` still locates the error at its entry. An `Option<String>`
//! field is untouched: serde asks it for an option itself, and a null there
//! still means left out.
//!
//! What `Refusing` can't reach, none of it used by the config model today:
//! a mapping key, which `noyalib` reads with its own key deserializer, so
//! `~: a` is the key `"~"` (as it was before this module existed); a `char`,
//! which `noyalib` reads through its own string read internally; and
//! anything serde buffers before deserializing it — `#[serde(untagged)]`,
//! `#[serde(flatten)]`, an internally tagged enum — where a string target
//! gets the resolved value, so `1.50` is refused as a float, as without the
//! option. Adding one of those to the model loses the text as written.
//!
//! **Not on `noyalib`'s AST path.** A YAML tag (`!!int 7`, `!custom`)
//! anywhere in a document sends the whole document through `noyalib`'s AST
//! loader instead, where `plain_scalar_strings` re-formats the resolved
//! value (`3.10` becomes `3.1`) — `TODO.md` 18.

use serde::de::{self, DeserializeOwned, DeserializeSeed, Deserializer, Visitor};
use std::fmt;

/// Deserializes `text` as Batect-format YAML — see the module notes.
pub(crate) fn from_str<T: DeserializeOwned + 'static>(text: &str) -> noyalib::Result<T> {
    let Document(value) = noyalib::from_str_with_config(
        text,
        &noyalib::ParserConfig::new().plain_scalar_strings(true),
    )?;
    Ok(value)
}

/// A whole document, read through [`Refusing`].
struct Document<T>(T);

impl<'de, T: de::Deserialize<'de>> de::Deserialize<'de> for Document<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(Refusing(deserializer)).map(Document)
    }
}

/// Wraps a deserializer — and every deserializer, visitor, seed and access
/// reached through it — so that a string target refuses a null. Everything
/// else is passed through untouched.
struct Refusing<X>(X);

/// The visitor a string target is read through: a `None` is refused, and
/// anything else is read as the string it was asked for.
struct NonNullString<V>(V);

impl<'de, V: Visitor<'de>> Visitor<'de> for NonNullString<V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.0.expecting(formatter)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(E::invalid_type(de::Unexpected::Other("null"), &self))
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self.0)
    }
}

macro_rules! forward_deserialize {
    ($($method:ident($($arg:ident: $ty:ty),*)),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, $($arg: $ty,)* visitor: V) -> Result<V::Value, D::Error> {
            self.0.$method($($arg,)* Refusing(visitor))
        }
    )*};
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Refusing<D> {
    type Error = D::Error;

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.0.deserialize_option(NonNullString(visitor))
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        self.deserialize_str(visitor)
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }

    forward_deserialize! {
        deserialize_any(), deserialize_bool(), deserialize_i8(), deserialize_i16(),
        deserialize_i32(), deserialize_i64(), deserialize_i128(), deserialize_u8(),
        deserialize_u16(), deserialize_u32(), deserialize_u64(), deserialize_u128(),
        deserialize_f32(), deserialize_f64(), deserialize_char(), deserialize_bytes(),
        deserialize_byte_buf(), deserialize_option(), deserialize_unit(),
        deserialize_unit_struct(name: &'static str),
        deserialize_newtype_struct(name: &'static str), deserialize_seq(),
        deserialize_tuple(len: usize),
        deserialize_tuple_struct(name: &'static str, len: usize), deserialize_map(),
        deserialize_struct(name: &'static str, fields: &'static [&'static str]),
        deserialize_enum(name: &'static str, variants: &'static [&'static str]),
        deserialize_identifier(), deserialize_ignored_any(),
    }
}

macro_rules! forward_visit {
    ($($method:ident($ty:ty)),* $(,)?) => {$(
        fn $method<E: de::Error>(self, v: $ty) -> Result<Self::Value, E> {
            self.0.$method(v)
        }
    )*};
}

impl<'de, V: Visitor<'de>> Visitor<'de> for Refusing<V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.0.expecting(formatter)
    }

    forward_visit! {
        visit_bool(bool), visit_i8(i8), visit_i16(i16), visit_i32(i32), visit_i64(i64),
        visit_i128(i128), visit_u8(u8), visit_u16(u16), visit_u32(u32), visit_u64(u64),
        visit_u128(u128), visit_f32(f32), visit_f64(f64), visit_char(char),
        visit_str(&str), visit_borrowed_str(&'de str), visit_string(String),
        visit_bytes(&[u8]), visit_borrowed_bytes(&'de [u8]), visit_byte_buf(Vec<u8>),
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.0.visit_none()
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.0.visit_unit()
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.0.visit_some(Refusing(deserializer))
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        self.0.visit_newtype_struct(Refusing(deserializer))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        self.0.visit_seq(Refusing(seq))
    }

    fn visit_map<A: de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.0.visit_map(Refusing(map))
    }

    fn visit_enum<A: de::EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        self.0.visit_enum(Refusing(data))
    }
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Refusing<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<S::Value, D::Error> {
        self.0.deserialize(Refusing(deserializer))
    }
}

impl<'de, A: de::SeqAccess<'de>> de::SeqAccess<'de> for Refusing<A> {
    type Error = A::Error;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, A::Error> {
        self.0.next_element_seed(Refusing(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

impl<'de, A: de::MapAccess<'de>> de::MapAccess<'de> for Refusing<A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, A::Error> {
        // `noyalib` reads a key — and locates an unknown one — through its
        // own key deserializer, which must see the seed unwrapped. A null
        // key is therefore its text, not refused (see the module notes).
        self.0.next_key_seed(seed)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, A::Error> {
        self.0.next_value_seed(Refusing(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

impl<'de, A: de::EnumAccess<'de>> de::EnumAccess<'de> for Refusing<A> {
    type Error = A::Error;
    type Variant = Refusing<A::Variant>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), A::Error> {
        self.0
            .variant_seed(Refusing(seed))
            .map(|(value, variant)| (value, Refusing(variant)))
    }
}

impl<'de, A: de::VariantAccess<'de>> de::VariantAccess<'de> for Refusing<A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        self.0.unit_variant()
    }

    fn newtype_variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<S::Value, A::Error> {
        self.0.newtype_variant_seed(Refusing(seed))
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, A::Error> {
        self.0.tuple_variant(len, Refusing(visitor))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, A::Error> {
        self.0.struct_variant(fields, Refusing(visitor))
    }
}

#[cfg(test)]
#[path = "yaml_tests.rs"]
mod tests;
