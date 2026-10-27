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

fn is_suffix(suffix: &str) -> bool {
    suffix.len() == 6
        && suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Batect's `generateId(6)`: six characters from `a-z0-9`. Drawn many times
/// so a generator that only sometimes strays outside the alphabet is caught.
#[test]
fn a_suffix_is_six_lowercase_letters_or_digits() {
    for _ in 0..1000 {
        let suffix = generate_suffix();
        assert!(is_suffix(&suffix), "{suffix:?}");
    }
}

#[test]
fn separate_generators_draw_separate_suffixes() {
    let suffixes: std::collections::HashSet<String> = (0..100)
        .map(|_| ResourceNames::new("demo").suffix.clone())
        .collect();
    // 36^6 values: a repeat in 100 draws has about a 1-in-440,000 chance,
    // so one is almost certainly a broken generator, not luck.
    assert_eq!(suffixes.len(), 100);
}

/// Batect's `DockerResourceNameGenerator`: `<project>-<name>-<suffix>`, one
/// suffix per generator, shared by every name it gives.
#[test]
fn every_name_from_one_generator_shares_its_suffix() {
    let names = ResourceNames::new("my_project");
    let suffix = &names.suffix;
    assert!(is_suffix(suffix), "{suffix:?}");
    assert_eq!(
        names.container("container-1"),
        format!("my_project-container-1-{suffix}")
    );
    assert_eq!(
        names.container("container-2"),
        format!("my_project-container-2-{suffix}")
    );
    assert_eq!(names.network(), format!("my_project-network-{suffix}"));
}
