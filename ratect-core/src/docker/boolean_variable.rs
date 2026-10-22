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

//! The one reading of a Docker boolean environment variable —
//! `DOCKER_TLS_VERIFY` (see `connection::tls_enabled`) and `DOCKER_BUILDKIT`
//! (see `select_builder_version`) — so the two can't drift apart. Owned by
//! `docker` rather than either of them, as both are its.
//!
//! The vocabulary is Batect's (`ValueConverters.boolean`): `1`, `yes` and
//! `true` are true, `0`, `no` and `false` are false. Two divergences from
//! it, both recorded in `docs/differences-from-batect.md`:
//!
//! - **Any case** — `True` and `Yes` are accepted, where Batect takes only
//!   the all-lower and all-upper spellings. Harmless leniency: the value
//!   still does what it evidently means.
//! - **Empty is unset**, as for an empty `DOCKER_HOST`/`DOCKER_CONTEXT` and
//!   as in the Docker CLI. Batect rejects it.
//!
//! Anything else is an
//! [`InvalidOptionValue`](crate::exit_code::InvalidOptionValue) naming the
//! variable and quoting the value as written, never a silent default. The
//! Docker CLI itself reads `DOCKER_BUILDKIT` with Go's `strconv.ParseBool`,
//! which refuses `yes`/`no`; this follows Batect's vocabulary instead.

use crate::exit_code::InvalidOptionValue;

/// `value`, the content of environment variable `name`: `Ok(None)` when it
/// is unset or empty, `Ok(Some(_))` for one of the six spellings in any
/// case, and an error naming `name` and `value` otherwise.
pub(crate) fn parse(name: &str, value: Option<&str>) -> Result<Option<bool>, InvalidOptionValue> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "yes" | "true" => Ok(Some(true)),
        "0" | "no" | "false" => Ok(Some(false)),
        _ => Err(InvalidOptionValue::new(format!(
            "{name} is '{value}', which is neither true nor false: use 1, yes or true, or 0, \
             no or false, in any case."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batects_spellings_in_any_case_are_true_or_false() {
        for value in ["1", "yes", "YES", "Yes", "true", "TRUE", "True"] {
            assert_eq!(parse("V", Some(value)).unwrap(), Some(true), "{value}");
        }
        for value in ["0", "no", "NO", "No", "false", "FALSE", "False"] {
            assert_eq!(parse("V", Some(value)).unwrap(), Some(false), "{value}");
        }
    }

    #[test]
    fn an_empty_value_is_unset() {
        assert_eq!(parse("V", Some("")).unwrap(), None);
        assert_eq!(parse("V", None).unwrap(), None);
    }

    #[test]
    fn anything_else_is_an_error_quoting_the_value_as_written() {
        for value in ["maybe", "on", "garbage", "BANANA", " 1"] {
            let message = parse("DOCKER_TLS_VERIFY", Some(value))
                .unwrap_err()
                .to_string();
            assert!(message.contains("DOCKER_TLS_VERIFY"), "{message}");
            assert!(message.contains(&format!("'{value}'")), "{message}");
            assert!(message.contains("yes"), "{message}");
        }
    }
}
