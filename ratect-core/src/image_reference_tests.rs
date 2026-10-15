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

// Both tables were checked against `docker tag` from Docker 29.8.2: a value
// accepted here reached the daemon ("No such image"), a rejected one failed
// with exactly this reason.
#[test]
fn accepts_what_dockers_reference_parser_accepts() {
    let longest_tag = format!("app:{}", "a".repeat(128));
    // The 255-character limit is on the path, which for a bare name
    // includes the `library/` it's given on Docker Hub.
    let longest_bare_name = "a".repeat(247);
    let longest_named_registry = format!("example.com/{}", "a".repeat(255));
    let almost_an_id = "a".repeat(63);
    for value in [
        "app",
        "my-org/app",
        "a.b_c__d---e",
        "app:V1.0_rc-1",
        "app:_leading-underscore",
        "localhost/app",
        "localhost:5000/app",
        "registry.example.com:5000/team/app:1.2",
        "Registry.Example.COM/app",
        "MyOrg/app",
        "[::1]:5000/app",
        "my-org/app:latest",
        &longest_tag,
        &longest_bare_name,
        &longest_named_registry,
        &almost_an_id,
    ] {
        assert_eq!(check(value), Ok(()), "{value}");
    }
}

#[test]
fn rejects_what_dockers_reference_parser_rejects_with_its_reason() {
    let id = "0123456789abcdef".repeat(4);
    let digest = format!("app@sha256:{}", "0".repeat(64));
    let tagged_and_digested = format!("app:1.0@sha512:{}", "f".repeat(128));
    let too_long_tag = format!("app:{}", "a".repeat(129));
    let too_long_bare_name = "a".repeat(248);
    let too_long_named_registry = format!("example.com/{}", "a".repeat(256));
    let short_digest = format!("app@sha256:{}", "0".repeat(63));
    let uppercase_digest = format!("app@sha256:{}", "A".repeat(64));
    let unknown_algorithm = format!("app@md5:{}", "0".repeat(32));
    let uppercase_algorithm = format!("app:1@MD5:{}", "0".repeat(32));
    let invalid_id_reason =
        format!("invalid repository name ({id}), cannot specify 64-byte hexadecimal strings");
    for (value, reason) in [
        ("_nonsense", "invalid reference format"),
        ("-x", "invalid reference format"),
        ("app-", "invalid reference format"),
        ("a..b", "invalid reference format"),
        ("", "invalid reference format"),
        ("app:", "invalid reference format"),
        ("app:.bad", "invalid reference format"),
        ("app:-bad", "invalid reference format"),
        ("app/", "invalid reference format"),
        ("app name", "invalid reference format"),
        (
            "Foo",
            "invalid reference format: repository name (library/Foo) must be lowercase",
        ),
        (
            "my-org/App:1.0",
            "invalid reference format: repository name (my-org/App) must be lowercase",
        ),
        (
            "S4-i290-build-env",
            "invalid reference format: repository name (library/S4-i290-build-env) must be \
             lowercase",
        ),
        (&too_long_tag, "invalid reference format"),
        (
            &too_long_bare_name,
            "repository name must not be more than 255 characters",
        ),
        (
            &too_long_named_registry,
            "repository name must not be more than 255 characters",
        ),
        (&id, &invalid_id_reason),
        (&short_digest, "invalid checksum digest length"),
        (&uppercase_digest, "invalid checksum digest format"),
        (&unknown_algorithm, "unsupported digest algorithm"),
        (&uppercase_algorithm, "invalid checksum digest format"),
        (&digest, "refusing to create a tag with a digest reference"),
        (
            &tagged_and_digested,
            "refusing to create a tag with a digest reference",
        ),
    ] {
        assert_eq!(check(value), Err(reason.to_string()), "{value:?}");
    }
}
