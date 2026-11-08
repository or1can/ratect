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

/// Which `tmpfs` hides `home`, given the `tmpfs` and other mount destinations.
fn hiding<'a>(home: &str, tmpfs: &[&'a str], mounts: &[&'a str]) -> Option<&'a str> {
    tmpfs_hiding_home(home, tmpfs.iter().copied(), mounts.iter().copied())
}

#[test]
fn a_tmpfs_above_the_home_hides_it() {
    assert_eq!(hiding("/mnt/me", &["/mnt"], &[]), Some("/mnt"));
    assert_eq!(hiding("/mnt/up/me", &["/mnt"], &[]), Some("/mnt"));
}

#[test]
fn no_tmpfs_above_the_home_hides_nothing() {
    assert_eq!(hiding("/mnt/me", &[], &["/mnt"]), None);
    assert_eq!(hiding("/mnt/me", &["/srv", "/mnt/me/.cache"], &[]), None);
    // A path that shares a prefix of characters, not of components.
    assert_eq!(hiding("/mnt/me", &["/mn"], &[]), None);
}

/// A `tmpfs` at the home is the home, and so is any mount there.
#[test]
fn a_mount_or_tmpfs_at_the_home_is_the_home() {
    assert_eq!(hiding("/mnt/me", &["/mnt/me"], &[]), None);
    assert_eq!(hiding("/mnt/me", &["/mnt", "/mnt/me"], &[]), None);
    assert_eq!(hiding("/mnt/me", &["/mnt"], &["/mnt/me"]), None);
}

/// A mount between the deepest `tmpfs` and the home goes on top of it at start.
#[test]
fn a_mount_between_the_tmpfs_and_the_home_keeps_it_visible() {
    assert_eq!(hiding("/mnt/up/me", &["/mnt"], &["/mnt/up"]), None);
}

/// Docker refuses a bind at a `tmpfs`'s own path only as spelled the same, and
/// mounts the bind on top of one that merely resolves there.
#[test]
fn a_mount_at_the_path_a_tmpfs_resolves_to_decides_over_it() {
    assert_eq!(hiding("/mnt/me", &["/mnt/x/.."], &["/mnt"]), None);
}

/// What decides is the deepest `tmpfs` above the home: a mount above it doesn't
/// save what it hides, and neither does one beside the home.
#[test]
fn a_tmpfs_below_a_mount_still_hides_the_home() {
    assert_eq!(
        hiding("/mnt/up/me", &["/mnt/up"], &["/mnt"]),
        Some("/mnt/up")
    );
    assert_eq!(
        hiding("/mnt/up/inner/me", &["/mnt", "/mnt/up/inner"], &["/mnt/up"]),
        Some("/mnt/up/inner")
    );
    assert_eq!(hiding("/mnt/me", &["/mnt"], &["/mnt/other"]), Some("/mnt"));
}

/// Paths are compared cleaned, and the `tmpfs` is named as written.
#[test]
fn paths_are_compared_however_they_are_written() {
    for tmpfs in ["/mnt/", "//mnt", "/mnt/.", "/mnt/x/.."] {
        assert_eq!(hiding("/mnt/up/me", &[tmpfs], &[]), Some(tmpfs), "{tmpfs}");
        assert_eq!(hiding("/mnt/up/me/", &[tmpfs], &[]), Some(tmpfs), "{tmpfs}");
        assert_eq!(
            hiding("/mnt/up/me", &[tmpfs], &["/mnt/up"]),
            None,
            "{tmpfs}"
        );
    }
    assert_eq!(hiding("/mnt/me", &["/mnt/me/x/.."], &[]), None);
}

/// A mount destination written differently from its cleaned form, between a
/// `tmpfs` and the home, counts as where it resolves to.
#[test]
fn a_mount_written_differently_between_the_tmpfs_and_the_home_keeps_it_visible() {
    for mount in ["/mnt/up/", "//mnt/up", "/mnt/./up", "/mnt/x/../up"] {
        assert_eq!(hiding("/mnt/up/me", &["/mnt"], &[mount]), None, "{mount}");
    }
    assert_eq!(hiding("/mnt/me", &["/mnt"], &["/mnt/me/x/.."]), None);
}
