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

//! Currently just [`registry_hostname`], the extraction pull's credential
//! resolution will need. This module is the intended home for resolving
//! private registry credentials for pull and build via real Docker
//! credential helpers, too — a filesystem-and-subprocess concern of its own,
//! kept out of `docker.rs` (which stays focused on bollard/daemon
//! interaction) — but the resolver itself lands in a later change.

/// The registry hostname an image reference names — `"docker.io"` when the
/// reference has no explicit registry. Matches the real `docker` CLI's own
/// reference-parsing rule (`distribution/reference`'s `splitDockerDomain`,
/// verified directly against its source): the first path segment (before
/// the first `/`) is a registry domain only if it is exactly `localhost`,
/// equals `index.docker.io` (canonicalized here to `docker.io`), contains a
/// `.` or `:`, or has any uppercase character; otherwise there is no domain
/// and the whole reference is an image name under `docker.io`'s implicit
/// `library/` prefix — including a bare, single-segment reference, which
/// never has a domain regardless of its own spelling.
///
/// A lightweight community crate (`docker-image`, `sunsided/docker-image-rs`)
/// was considered and rejected during scoping: its regex requires a
/// dot-plus-TLD in the first segment, so it misses the `localhost`/
/// `index.docker.io`/uppercase cases entirely — a real correctness gap on a
/// common form (a local dev/CI registry), not a theoretical one. This rule
/// needs no regex, so it's hand-written instead.
const DOCKER_IO: &str = "docker.io";

pub fn registry_hostname(image_reference: &str) -> String {
    let Some((first_segment, _rest)) = image_reference.split_once('/') else {
        return DOCKER_IO.to_string();
    };

    if first_segment == "index.docker.io" {
        return DOCKER_IO.to_string();
    }

    let looks_like_domain = first_segment == "localhost"
        || first_segment.contains('.')
        || first_segment.contains(':')
        || first_segment.chars().any(|c| c.is_ascii_uppercase());

    if looks_like_domain {
        first_segment.to_string()
    } else {
        DOCKER_IO.to_string()
    }
}

#[cfg(test)]
#[path = "registry_auth_tests.rs"]
mod tests;
