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

//! Docker's image-reference grammar, checked before a build so a bad tag
//! fails the command rather than the build that would have used it
//! ([decisions/0018](https://github.com/or1can/ratect/blob/main/decisions/0018-built-images-named-for-people-found-by-labels.md)).
//!
//! What `docker tag` checks before it asks the daemon for anything: a port of
//! `ParseNormalizedNamed` from `github.com/distribution/reference`, plus the
//! CLI's own refusal of a digest. docker-client — and so Batect — runs an
//! older copy of the same function (`docker/distribution`'s) on every tag it
//! builds; the two accept the same tags except in corners few real tags reach
//! (an uppercase first component like `MyOrg/app`, which the current one
//! reads as a registry host; an IPv6 host; a name near 255 characters), and this
//! follows the current one, since it's what the docker CLI a user has
//! installed reports with. Batect accepts a digest and leaves the daemon to
//! refuse it; refusing it here is the same outcome without the build.
//!
//! Ported rather than approximated because the grammar has corners a
//! hand-written rule gets wrong: uppercase is allowed in a registry host and a
//! tag but not in a repository path, a one-component Docker Hub name is
//! checked as `library/<name>`, and a bare 64-digit hex string is refused
//! because it would read as an image ID. The reasons returned are that
//! package's own error text.
//!
//! Only the grammar is checked: a valid reference can still fail on the
//! daemon, as any tag can.

use regex::Regex;
use std::sync::LazyLock;

const REPOSITORY_NAME_TOTAL_LENGTH_MAX: usize = 255;
const DEFAULT_DOMAIN: &str = "docker.io";
const LEGACY_DEFAULT_DOMAIN: &str = "index.docker.io";
const OFFICIAL_REPO_PREFIX: &str = "library/";

/// One `/`-separated component of a repository path: lowercase alphanumeric
/// runs joined by a `.`, one or two `_`, or any number of `-`. A macro so
/// [`NAME_COMPONENT_PATTERN`] can `concat!` it.
macro_rules! path_component {
    () => {
        r"[a-z0-9]+(?:(?:[._]|__|[-]+)[a-z0-9]+)*"
    };
}

/// The package's reference pattern, assembled from the same pieces: an
/// optional registry host (group 1) and `/`, the repository path (group 2),
/// an optional `:tag` (group 3) and an optional `@digest` (group 4). Go's
/// `\w` is ASCII-only, so it's spelled out; Rust's would also match
/// non-ASCII letters.
static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    let path_component = path_component!();
    let domain_component = r"(?:[a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9-]*[a-zA-Z0-9])";
    let host = format!(r"(?:{domain_component}(?:\.{domain_component})*|\[(?:[a-fA-F0-9:]+)\])");
    let domain = format!(r"{host}(?::[0-9]+)?");
    let path = format!(r"{path_component}(?:/{path_component})*");
    let tag = r"[0-9A-Za-z_][0-9A-Za-z_.-]{0,127}";
    let digest = r"[A-Za-z][A-Za-z0-9]*(?:[-_+.][A-Za-z][A-Za-z0-9]*)*:[[:xdigit:]]{32,}";
    Regex::new(&format!(
        r"^(?:({domain})/)?({path})(?::({tag}))?(?:@({digest}))?$"
    ))
    .unwrap()
});

/// [`is_name_component`]'s rule as one anchored pattern, which the config
/// schemas state too.
pub const NAME_COMPONENT_PATTERN: &str = concat!("^", path_component!(), "$");

static NAME_COMPONENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(NAME_COMPONENT_PATTERN).unwrap());

static IMAGE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-f0-9]{64}$").unwrap());

/// Whether `value` is an image reference Docker would accept as a tag for a
/// built image, and if not, why not — in the reference parser's own words
/// (`invalid reference format`, `… must be lowercase`, …), for the caller to
/// put after whatever names the value.
pub fn check(value: &str) -> Result<(), String> {
    if IMAGE_ID.is_match(value) {
        return Err(format!(
            "invalid repository name ({value}), cannot specify 64-byte hexadecimal strings"
        ));
    }
    let (domain, remainder) = split_docker_domain(value);
    let remote = remainder.split(':').next().unwrap_or_default();
    if remote.to_lowercase() != remote {
        return Err(format!(
            "invalid reference format: repository name ({remote}) must be lowercase"
        ));
    }
    parse(&format!("{domain}/{remainder}"))
}

/// [`check`], phrased for a tag the user gave as is: the value, then why
/// Docker refuses it.
pub fn check_tag(tag: &str) -> Result<(), String> {
    check(tag).map_err(|reason| format!("'{tag}' is not a valid image tag: {reason}"))
}

/// Whether `name` is one component of a repository path — Batect's
/// `ImageNameValidator` rule, which a `project_name` and every container
/// name must satisfy, since a built image is tagged `<project>-<container>`.
/// Two components joined by `-` are one, so each can be checked on its own.
pub fn is_name_component(name: &str) -> bool {
    NAME_COMPONENT.is_match(name)
}

/// The package's `splitDockerDomain`: the part before the first `/` is a
/// registry host only if it looks like one — `localhost`, a `.` or `:`, or
/// uppercase, which a repository path can't contain — and otherwise the name
/// is Docker Hub's, where a one-component name is an official image under
/// `library/`.
fn split_docker_domain(name: &str) -> (&str, String) {
    let (domain, remote) = match name.split_once('/') {
        None => (DEFAULT_DOMAIN, name),
        Some((LEGACY_DEFAULT_DOMAIN, tail)) => (DEFAULT_DOMAIN, tail),
        Some((head, tail))
            if head == "localhost" || head.contains(['.', ':']) || head.to_lowercase() != head =>
        {
            (head, tail)
        }
        Some(_) => (DEFAULT_DOMAIN, name),
    };
    if domain == DEFAULT_DOMAIN && !remote.contains('/') {
        return (domain, format!("{OFFICIAL_REPO_PREFIX}{remote}"));
    }
    (domain, remote.to_string())
}

/// The package's `Parse`, on the already-normalised name, then `docker tag`'s
/// refusal of a digest — which comes after parsing, so a malformed digest is
/// reported as malformed first.
fn parse(reference: &str) -> Result<(), String> {
    let Some(captures) = REFERENCE.captures(reference) else {
        let lowercase = reference.to_lowercase();
        if lowercase != reference && REFERENCE.is_match(&lowercase) {
            return Err("repository name must be lowercase".to_string());
        }
        return Err("invalid reference format".to_string());
    };
    if captures[2].len() > REPOSITORY_NAME_TOTAL_LENGTH_MAX {
        return Err(format!(
            "repository name must not be more than {REPOSITORY_NAME_TOTAL_LENGTH_MAX} characters"
        ));
    }
    match captures.get(4) {
        Some(digest) => {
            check_digest(digest.as_str())?;
            Err("refusing to create a tag with a digest reference".to_string())
        }
        None => Ok(()),
    }
}

/// `github.com/opencontainers/go-digest`'s `Digest.Validate`, for a digest the
/// reference grammar has already matched: only the three algorithms it
/// registers, each with its exact length, in lowercase hex.
fn check_digest(digest: &str) -> Result<(), String> {
    let (algorithm, encoded) = digest.split_once(':').unwrap_or_default();
    let length = match algorithm {
        "sha256" => 64,
        "sha384" => 96,
        "sha512" => 128,
        // go-digest's own algorithm grammar is the reference grammar's,
        // lowercase only.
        _ if algorithm.chars().any(|c| c.is_ascii_uppercase()) => {
            return Err("invalid checksum digest format".to_string())
        }
        _ => return Err("unsupported digest algorithm".to_string()),
    };
    if encoded.len() != length {
        return Err("invalid checksum digest length".to_string());
    }
    if encoded.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("invalid checksum digest format".to_string());
    }
    Ok(())
}

#[cfg(test)]
#[path = "image_reference_tests.rs"]
mod tests;
