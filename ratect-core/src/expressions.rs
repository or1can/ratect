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

use anyhow::{bail, Result};
use std::collections::HashMap;

/// Scans `input` for Batect-style expressions — `$VAR`, `${VAR}`,
/// `${VAR:-default}` for host environment variables, and `<name`, `<{name}`
/// for config variables — and substitutes each with its resolved value.
/// Literal text (including a `$`/`<` not followed by a valid identifier, or
/// an unterminated `${`/`<{`) is passed through unchanged.
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
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '$' || c == '<' {
            if let Some((token_len, resolved)) =
                parse_token(&chars[i..], c, &host_env, config_vars)?
            {
                result.push_str(&resolved);
                i += token_len;
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
/// or `<`, matching `sigil`). Returns `None` (not a valid/complete token —
/// the sigil should be treated as a literal character) or `Some((length,
/// resolved_value))` on success; errors if the token is well-formed but its
/// variable can't be resolved.
fn parse_token(
    chars: &[char],
    sigil: char,
    host_env: &impl Fn(&str) -> Option<String>,
    config_vars: &HashMap<String, Option<String>>,
) -> Result<Option<(usize, String)>> {
    if chars.len() < 2 {
        return Ok(None);
    }

    if chars[1] == '{' {
        let close = match chars[2..].iter().position(|&c| c == '}') {
            Some(pos) => pos + 2,
            None => return Ok(None),
        };
        let inner: String = chars[2..close].iter().collect();

        let (name, default) = if sigil == '$' {
            match inner.split_once(":-") {
                Some((name, default)) => (name.to_string(), Some(default.to_string())),
                None => (inner, None),
            }
        } else {
            (inner, None)
        };

        if !is_valid_ident(&name, sigil) {
            return Ok(None);
        }

        let resolved = resolve(sigil, &name, default.as_deref(), host_env, config_vars)?;
        Ok(Some((close + 1, resolved)))
    } else {
        if !is_ident_start(chars[1]) {
            return Ok(None);
        }
        let mut end = 1;
        while end < chars.len() && is_ident_char(chars[end], sigil) {
            end += 1;
        }
        let name: String = chars[1..end].iter().collect();
        let resolved = resolve(sigil, &name, None, host_env, config_vars)?;
        Ok(Some((end, resolved)))
    }
}

fn resolve(
    sigil: char,
    name: &str,
    default: Option<&str>,
    host_env: &impl Fn(&str) -> Option<String>,
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
