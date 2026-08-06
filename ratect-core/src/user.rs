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

//! Host user lookup (`current_user`, via the `nix`
//! crate — Unix-only) and the pure `/etc/passwd`/`/etc/shadow`/`/etc/group` content
//! generators `docker.rs` uses — ported from Batect's
//! `RunAsCurrentUserConfigurationProvider`, including its `uid == 0`/`gid == 0`
//! special-casing so running as the current user doesn't produce a duplicate
//! conflicting `root` entry.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// The host user Ratect itself is running as — looked up once per
/// `run_as_current_user`-enabled container (see
/// `TaskEngine::resolve_user_mapping`), not cached, since there's only ever
/// one real host user per process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
    pub groupname: String,
}

/// Looks up the real host user Ratect is running as. Unix-only — Ratect's
/// own testing has been Unix-only so far (see the `crossterm` caveat for
/// interactive mode), and there's no meaningful "current uid/gid" concept to
/// map a container onto on other platforms, so this errors clearly rather
/// than guessing.
#[cfg(unix)]
pub fn current_user() -> Result<CurrentUser> {
    use nix::unistd::{Gid, Group, Uid, User};

    let uid = Uid::current();
    let gid = Gid::current();

    let user = User::from_uid(uid)
        .context("Failed to look up the current user")?
        .with_context(|| format!("No passwd entry found for the current user (uid {uid})"))?;
    let group = Group::from_gid(gid)
        .context("Failed to look up the current group")?
        .with_context(|| format!("No group entry found for the current group (gid {gid})"))?;

    Ok(CurrentUser {
        uid: uid.as_raw(),
        gid: gid.as_raw(),
        username: user.name,
        groupname: group.name,
    })
}

#[cfg(not(unix))]
pub fn current_user() -> Result<CurrentUser> {
    Err(anyhow::anyhow!(
        "'run_as_current_user' is only supported on Unix hosts"
    ))
}

/// The current host user's home directory — used to anchor the Git include
/// cache (`~/.ratect/incl`, see `git_include.rs`). Unix-only, same rationale
/// as `current_user` above; deliberately not `$HOME` (which a user could
/// override to something inconsistent with their actual passwd entry) —
/// looked up the same way `current_user` looks up everything else.
#[cfg(unix)]
pub fn home_directory() -> Result<PathBuf> {
    use nix::unistd::{Uid, User};

    let uid = Uid::current();
    let user = User::from_uid(uid)
        .context("Failed to look up the current user")?
        .with_context(|| format!("No passwd entry found for the current user (uid {uid})"))?;

    Ok(user.dir)
}

#[cfg(not(unix))]
pub fn home_directory() -> Result<PathBuf> {
    Err(anyhow::anyhow!(
        "Git includes are only supported on Unix hosts"
    ))
}

/// Generates a minimal `/etc/passwd` for `user`, granting `home_directory` as
/// their home. Ported from Batect's `RunAsCurrentUserConfigurationProvider.
/// generatePasswdFile` — when the current user's `uid` is already `0`
/// (i.e. Ratect itself is running as root), there's only one `root` entry
/// (using the configured `home_directory` as its home) rather than two
/// conflicting ones.
pub fn generate_passwd_file(user: &CurrentUser, home_directory: &str) -> String {
    if user.uid == 0 {
        format!("root:x:0:0:root:{home_directory}:/bin/sh")
    } else {
        format!(
            "root:x:0:0:root:/root:/bin/sh\n{}:x:{}:{}:{}:{}:/bin/sh",
            user.username, user.uid, user.gid, user.username, home_directory
        )
    }
}

/// Generates a minimal `/etc/shadow` for `user`. Ported from Batect's
/// `generateShadowFile`.
pub fn generate_shadow_file(user: &CurrentUser) -> String {
    if user.uid == 0 {
        "root:*:19500:0:99999:7:::".to_string()
    } else {
        format!(
            "root:*:19500:0:99999:7:::\n{}:*:19500:0:99999:7:::",
            user.username
        )
    }
}

/// Generates a minimal `/etc/group` for `user`. Ported from Batect's
/// `generateGroupFile`.
pub fn generate_group_file(user: &CurrentUser) -> String {
    let root_group = "root:x:0:root";

    if user.gid == 0 {
        root_group.to_string()
    } else {
        format!(
            "{root_group}\n{}:x:{}:{}",
            user.groupname, user.gid, user.username
        )
    }
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
