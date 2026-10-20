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
//!
//! The grammar is Batect's `Expression.parse` (#383, #384): every `$` or `<`
//! not escaped starts a reference, and one that is malformed — no name, an
//! unclosed `{`, a `:` not followed by `-` — is an error naming the problem
//! and its column, never literal text (#385). The loader runs
//! `check_syntax` while it reads each expression field, so a malformed one
//! fails the load, located in its file.

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
/// Every `$`/`<` not escaped starts a reference — see `parse_token` for
/// how far its name runs — and a malformed one is an error, as
/// `check_syntax` reports it, not literal text.
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
/// two cannot disagree about what counts — a detector written separately
/// would drift from where a name ends or what escapes a sigil, on exactly the
/// inputs where being wrong is a false rejection.
///
/// A malformed value is no expression, whatever references come before its
/// mistake: it fails the load wherever it is an expression, and where it is
/// not — a `batect.yml`'s `image` — it is used as written, so `img$` and
/// `$HOME/a$` are no expressions. So this is true only of a value
/// [`check_syntax`] accepts, and the two never both flag one value.
///
/// A well-formed token whose variable happens not to resolve here counts as
/// present: it *is* an expression, it just cannot be evaluated without the
/// caller's environment, which this deliberately does not take.
pub(crate) fn contains_expression(input: &str) -> bool {
    let mut found = false;
    let well_formed = scan(input, |_| {
        found = true;
        Ok(String::new())
    })
    .is_ok();
    found && well_formed
}

/// Refuses what [`interpolate`] would refuse whatever its variables held —
/// a mistake in the value as written, not in the machine it is evaluated
/// on — so a caller can judge it before any variable is known.
pub(crate) fn check_syntax(input: &str) -> Result<()> {
    scan(input, |_| Ok(String::new())).map(drop)
}

/// What a value read as plain text — a Batect-format file's compact volume
/// host path or `run_as_current_user.home_directory` — becomes as the
/// expression a `ratect.toml` reads in its place (`config convert`).
///
/// Each `\` stays a backslash, since plain text has no escapes (#628), so
/// it is escaped. Each `$`/`<` stays what it would be if the field were an
/// expression: a reference, which is the point of reading it as one — so
/// `a\$HOME` becomes `a\\$HOME`, a reference to `HOME`, and
/// [`contains_expression`] on the result says so. Unless that leaves no
/// valid expression (`img$`, `img\$`), when it can't have meant one and is
/// kept wholly literal (#385). A `\` inside a braced name is doubled too,
/// so it names a different variable; no variable name holds one.
pub(crate) fn literal_as_expression(literal: &str) -> String {
    let backslashes_kept = escape(literal, &['\\']);
    if check_syntax(&backslashes_kept).is_ok() {
        backslashes_kept
    } else {
        literal_kept(literal)
    }
}

/// `literal` as an expression that evaluates to exactly `literal`, holding
/// no reference: every `\`, `$` and `<` escaped.
pub(crate) fn literal_kept(literal: &str) -> String {
    escape(literal, &['\\', '$', '<'])
}

/// `input` with each of `special` escaped, so it evaluates to itself.
fn escape(input: &str, special: &[char]) -> String {
    let mut escaped = String::with_capacity(input.len());
    for c in input.chars() {
        if special.contains(&c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
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
        match chars[i] {
            '\\' => {
                let Some(&escaped) = chars.get(i + 1) else {
                    bail!(
                        "invalid escape sequence: '\\' at column {} must be immediately \
                         followed by a character to escape",
                        i + 1
                    );
                };
                result.push(escaped);
                i += 2;
            }
            '$' | '<' => {
                let token = parse_token(&chars, i)?;
                result.push_str(&on_token(&token)?);
                i += token.len;
            }
            c => {
                result.push(c);
                i += 1;
            }
        }
    }

    Ok(result)
}

/// Whether `c` continues an unbraced name: Batect's `isLetterOrDigit() ||
/// c == '_'`, for either sigil. Rust's `char::is_alphanumeric` is the
/// nearest test — Unicode alphabetic or numeric — and is wider than
/// Kotlin's at the margins: it also takes letter-like numbers (`Ⅻ`),
/// other numerics (`½`, `²`) and the combining marks Unicode counts as
/// alphabetic, all of which end a name in Batect. Pinned here rather than
/// matched, since no name anyone writes is told apart by them.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Parses the reference whose sigil — `$` or `<` — is `chars[start]`, as
/// Batect's `readVariable` does, or says what is wrong with it and at which
/// column (1-based, in characters) of the value.
///
/// Unbraced, the name is the longest run of [`is_name_char`]. Braced, it is
/// everything up to the closing `}` — or, for `$`, up to a `:`, which must
/// be followed by `-` and then a default running to the first `}` not
/// escaped by a `\`. No escape applies in a name, as in Batect.
fn parse_token(chars: &[char], start: usize) -> Result<Token> {
    let sigil = chars[start];
    let kind = if sigil == '$' {
        "environment"
    } else {
        "config"
    };
    let fail = |problem: String| anyhow::anyhow!("invalid {kind} variable reference: {problem}");

    let Some(&after) = chars.get(start + 1) else {
        return Err(fail(format!(
            "'{sigil}' at column {} must be followed by a variable name",
            start + 1
        )));
    };

    if after != '{' {
        let end = (start + 1..chars.len())
            .find(|&i| !is_name_char(chars[i]))
            .unwrap_or(chars.len());
        if end == start + 1 {
            return Err(fail(format!(
                "'{sigil}' at column {} does not contain a variable name",
                start + 1
            )));
        }
        return Ok(Token {
            len: end - start,
            sigil,
            name: chars[start + 1..end].iter().collect(),
            default: None,
        });
    }

    let mut i = start + 2;
    let mut name = String::new();
    let mut closed = false;
    let mut has_default = false;
    while let Some(&c) = chars.get(i) {
        match c {
            '}' => {
                closed = true;
                break;
            }
            ':' if sigil == '$' => {
                has_default = true;
                break;
            }
            c => {
                name.push(c);
                i += 1;
            }
        }
    }

    let default = if has_default {
        if chars.get(i + 1) != Some(&'-') {
            return Err(fail(format!(
                "':' at column {} must be immediately followed by '-'",
                i + 1
            )));
        }
        i += 2;
        let mut default = String::new();
        while let Some(&c) = chars.get(i) {
            match c {
                '}' => {
                    closed = true;
                    break;
                }
                '\\' => {
                    let Some(&escaped) = chars.get(i + 1) else {
                        return Err(fail(format!(
                            "'\\' at column {} must be immediately followed by a character \
                             to escape",
                            i + 1
                        )));
                    };
                    default.push(escaped);
                    i += 2;
                }
                c => {
                    default.push(c);
                    i += 1;
                }
            }
        }
        Some(default)
    } else {
        None
    };

    if !closed {
        return Err(fail(format!(
            "'{{' at column {} must be followed by a closing '}}'",
            start + 2
        )));
    }
    if name.is_empty() {
        let written: String = chars[start..=i].iter().collect();
        return Err(fail(format!(
            "'{written}' at column {} does not contain a variable name",
            start + 1
        )));
    }

    Ok(Token {
        len: i + 1 - start,
        sigil,
        name,
        default,
    })
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
