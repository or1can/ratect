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

//! Batect's expression syntax (`$VAR`,
//! `${VAR:-default}`, `<name`/`<{name}` for config variables, including the built-in
//! `batect.project_directory`), and its `\` escape. Host environment and resolved config
//! variable values are injected as parameters rather than read from the real process
//! environment, so resolution stays deterministic and unit-testable.
//!
//! One parser for both formats: a `batect.yml` and a `ratect.toml` read every
//! expression the same way, escapes included, with no branch on which format a
//! value came from (#382). Which fields are expressions at all is the config
//! loader's business, not this module's.

use anyhow::{bail, Result};
use std::collections::HashMap;

/// Scans `input` for Batect-style expressions — `$VAR`, `${VAR}`,
/// `${VAR:-default}` for host environment variables, and `<name`, `<{name}`
/// for config variables — and substitutes each with its resolved value.
///
/// `\` escapes the character after it, as in Batect: the backslash is
/// dropped and that character kept literally, so `\$X` is `$X`, `a\b` is
/// `ab` and `\\` is `\`. In a `${VAR:-default}`'s default it does the same,
/// so `${U:-a\}b}` defaults to `a}b`; in a variable's name it is no escape.
/// A trailing `\`, with nothing to escape, is an error.
///
/// Otherwise lenient: a malformed sigil — a `$`/`<` not followed by a valid
/// identifier, or an unterminated `${`/`<{` — is passed through unchanged,
/// as literal text.
///
/// `host_env` looks up a host environment variable by name — injected so
/// callers/tests don't have to touch the real process environment.
/// `config_vars` maps every *declared* config variable name to its resolved
/// value (`None` if it has neither a CLI/file override nor a `default`).
pub fn interpolate(
    input: &str,
    host_env: impl Fn(&str) -> Option<String>,
    config_vars: &HashMap<String, Option<String>>,
) -> Result<String> {
    scan(input, |token| {
        resolve(
            token.sigil,
            &token.name,
            token.default.as_deref(),
            &host_env,
            config_vars,
        )
    })
}

/// Whether `input` holds anything [`interpolate`] would treat as an
/// expression rather than as literal text.
///
/// Shares [`scan`] with `interpolate` instead of scanning for `$`/`<`, so the
/// two cannot disagree about what counts. `interpolate` is deliberately
/// lenient — a `$` not followed by a valid identifier, or an unterminated
/// `${`, is literal — and a detector written separately would drift from that
/// on exactly the inputs where being wrong is a false rejection.
///
/// A well-formed token whose variable happens not to resolve here counts as
/// present: it *is* an expression, it just cannot be evaluated without the
/// caller's environment, which this deliberately does not take.
pub(crate) fn contains_expression(input: &str) -> bool {
    let mut found = false;
    // A syntax error (a trailing `\`) ends the walk; whatever was found by
    // then still counts, and the error is `check_syntax`'s to report.
    let _ = scan(input, |_| {
        found = true;
        Ok(String::new())
    });
    found
}

/// Refuses what [`interpolate`] would refuse whatever its variables held —
/// a mistake in the value as written, not in the machine it is evaluated
/// on — so a caller can judge it before any variable is known.
pub(crate) fn check_syntax(input: &str) -> Result<()> {
    scan(input, |_| Ok(String::new())).map(drop)
}

/// One well-formed reference, as [`parse_token`] read it.
struct Token {
    /// How many characters of the input it spans, sigil included.
    len: usize,
    sigil: char,
    name: String,
    default: Option<String>,
}

/// Walks `input` as [`interpolate`] does, replacing each reference with what
/// `on_token` makes of it — the one walk every reader of the syntax shares.
fn scan(input: &str, mut on_token: impl FnMut(&Token) -> Result<String>) -> Result<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if let Some(&escaped) = chars.get(i + 1) {
                result.push(escaped);
                i += 2;
                continue;
            }
            bail!(
                "invalid escape sequence: the '\\' at the end must be followed by a character \
                 to escape"
            );
        }
        if c == '$' || c == '<' {
            if let Some(token) = parse_token(&chars[i..], c) {
                result.push_str(&on_token(&token)?);
                i += token.len;
                continue;
            }
        }
        result.push(c);
        i += 1;
    }

    Ok(result)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Config variable names (`<name`, sigil `<`) may contain `.` — needed for
/// Batect's one built-in, `batect.project_directory`. Host environment
/// variable names (`$VAR`, sigil `$`) never do, so `.` isn't allowed there.
fn is_ident_char(c: char, sigil: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || (sigil == '<' && c == '.')
}

fn is_valid_ident(name: &str, sigil: char) -> bool {
    matches!(name.chars().next(), Some(c) if is_ident_start(c))
        && name.chars().all(|c| is_ident_char(c, sigil))
}

/// Tries to parse one expression token starting at `chars[0]` (which is `$`
/// or `<`, matching `sigil`). Returns `None` when it isn't a valid, complete
/// token — the sigil should then be treated as a literal character.
fn parse_token(chars: &[char], sigil: char) -> Option<Token> {
    if chars.len() < 2 {
        return None;
    }

    if chars[1] == '{' {
        // The name runs to the first `}` or, for a host variable, `:-`; no
        // escape applies in it, as in Batect.
        let mut end = 2;
        let mut has_default = false;
        loop {
            match chars.get(end)? {
                '}' => break,
                ':' if sigil == '$' && chars.get(end + 1) == Some(&'-') => {
                    has_default = true;
                    break;
                }
                _ => end += 1,
            }
        }
        let name: String = chars[2..end].iter().collect();

        // A default runs to the first `}` not escaped by a `\`, which is
        // dropped and the character after it kept.
        let default = if has_default {
            end += 2;
            let mut default = String::new();
            loop {
                match *chars.get(end)? {
                    '}' => break,
                    '\\' => {
                        default.push(*chars.get(end + 1)?);
                        end += 2;
                    }
                    c => {
                        default.push(c);
                        end += 1;
                    }
                }
            }
            Some(default)
        } else {
            None
        };

        if !is_valid_ident(&name, sigil) {
            return None;
        }

        Some(Token {
            len: end + 1,
            sigil,
            name,
            default,
        })
    } else {
        if !is_ident_start(chars[1]) {
            return None;
        }
        let mut end = 1;
        while end < chars.len() && is_ident_char(chars[end], sigil) {
            end += 1;
        }
        Some(Token {
            len: end,
            sigil,
            name: chars[1..end].iter().collect(),
            default: None,
        })
    }
}

fn resolve(
    sigil: char,
    name: &str,
    default: Option<&str>,
    host_env: impl Fn(&str) -> Option<String>,
    config_vars: &HashMap<String, Option<String>>,
) -> Result<String> {
    if sigil == '$' {
        match host_env(name) {
            Some(value) => Ok(value),
            None => match default {
                Some(default) => Ok(default.to_string()),
                None => bail!(
                    "Host environment variable '{name}' is not set, and no default was given \
                     (e.g. '${{{name}:-default}}')"
                ),
            },
        }
    } else {
        match config_vars.get(name) {
            None => bail!("Config variable '{name}' is not declared in 'config_variables'"),
            Some(None) => bail!(
                "Config variable '{name}' has no value: no --config-var/--config-vars-file \
                 override was given, and it has no 'default' in 'config_variables'"
            ),
            Some(Some(value)) => Ok(value.clone()),
        }
    }
}

#[cfg(test)]
#[path = "expressions_tests.rs"]
mod tests;
