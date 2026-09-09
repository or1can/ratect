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

#[test]
fn a_bare_single_segment_name_has_no_domain() {
    assert_eq!(registry_hostname("nginx"), "docker.io");
}

#[test]
fn a_bare_single_segment_name_has_no_domain_even_if_it_would_otherwise_look_like_one() {
    // No `/` at all means no domain, regardless of the name's own spelling
    // — `localhost`/`MyRegistry`/a dotted name alone are all just image
    // names here, not registries.
    assert_eq!(registry_hostname("localhost"), "docker.io");
    assert_eq!(registry_hostname("MyRegistry"), "docker.io");
    assert_eq!(registry_hostname("example.com"), "docker.io");
}

#[test]
fn a_two_segment_name_with_no_domain_like_first_segment_has_no_domain() {
    assert_eq!(registry_hostname("myuser/myimage"), "docker.io");
}

#[test]
fn a_dotted_first_segment_is_the_domain() {
    assert_eq!(
        registry_hostname("myregistry.example.com/foo/bar"),
        "myregistry.example.com"
    );
}

#[test]
fn localhost_is_always_a_domain() {
    assert_eq!(registry_hostname("localhost/foo"), "localhost");
}

#[test]
fn localhost_with_a_port_is_the_domain_including_the_port() {
    assert_eq!(registry_hostname("localhost:5000/foo"), "localhost:5000");
}

#[test]
fn index_docker_io_canonicalizes_to_docker_io() {
    assert_eq!(
        registry_hostname("index.docker.io/library/nginx"),
        "docker.io"
    );
}

#[test]
fn any_uppercase_character_in_the_first_segment_makes_it_a_domain() {
    assert_eq!(registry_hostname("MyRegistry/foo/bar"), "MyRegistry");
}
