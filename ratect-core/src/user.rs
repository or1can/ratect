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
//!
//! Those replace the image's files, which is `ratect-compat`'s contract.
//! `identity` edits them instead ([`edit_passwd_home`], decisions/0011). Its
//! input is whatever an image shipped, so it is bytes throughout, and a line it
//! isn't editing is never decoded, reflowed or re-terminated.

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

/// Who owns a home directory `identity` creates: the numeric ids an archive
/// entry takes, since Docker's archive API has no use for a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeOwner {
    pub uid: u32,
    pub gid: u32,
}

/// What [`edit_passwd_home`] made of an image's `/etc/passwd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswdEdit {
    /// The file's new contents.
    pub contents: Vec<u8>,
    /// Who the running user turned out to be. `None` for a named user with no
    /// entry, where `contents` is the file unchanged: the container can't start
    /// as a user its image lacks, and the runtime says so when it tries.
    pub home_owner: Option<HomeOwner>,
}

/// One `/etc/passwd` line, read as the runtime reads it: a name, and whatever
/// ids it has, tolerating spaces around it and fields missing or not numeric.
/// Only a comment or a blank line isn't one: libc skips the first, and neither
/// names anybody.
struct PasswdEntry<'a> {
    name: &'a [u8],
    uid: Option<u32>,
    gid: Option<u32>,
    /// Where in the line the home field is, empty and at its end if it has none.
    home: std::ops::Range<usize>,
    /// How many separators that missing field needs before it.
    separators_needed: usize,
}

const PASSWD_HOME_FIELD: usize = 5;

fn parse_passwd_entry(line: &[u8]) -> Option<PasswdEntry<'_>> {
    let start = line.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let end = line.iter().rposition(|byte| !byte.is_ascii_whitespace())? + 1;
    let text = &line[start..end];
    if text[0] == b'#' {
        return None;
    }
    let fields: Vec<&[u8]> = text.splitn(7, |byte| *byte == b':').collect();
    let number = |index: usize| {
        std::str::from_utf8(fields.get(index)?)
            .ok()?
            .parse::<u32>()
            .ok()
    };
    let (home, separators_needed) = match fields.get(PASSWD_HOME_FIELD) {
        Some(home) => {
            // Every field before the home, each with its separator.
            let before: usize = fields[..PASSWD_HOME_FIELD]
                .iter()
                .map(|field| field.len() + 1)
                .sum();
            (start + before..start + before + home.len(), 0)
        }
        None => (end..end, PASSWD_HOME_FIELD + 1 - fields.len()),
    };
    Some(PasswdEntry {
        name: fields[0],
        uid: number(2),
        gid: number(3),
        home,
        separators_needed,
    })
}

/// Makes `home` the home of the running user in `passwd`, an image's
/// `/etc/passwd`: the user its `configured_user` (`Config.User`: empty, `name`,
/// `uid`, `name:group` or `uid:gid`) says, root when that is empty.
///
/// A name selects the entry with that name, and a uid the first entry with that
/// uid, the one the runtime uses. All-digit text is always a uid, as it is to
/// the runtime. Only that entry's home field changes, added if its line is too
/// short to have one. An id missing from a name's line is 0, as it is to the
/// runtime.
///
/// A numeric user with no entry gets one appended, since without it `whoami`
/// fails and `$HOME` is `/`: named with its digits (`root` for uid 0, since the
/// process isn't the host user on the daemons this serves), password `*`, and
/// gid from `configured_user`'s numeric group, else 0. See
/// [`PasswdEdit::home_owner`] for a named user with none.
///
/// A *named* group isn't resolved, which would take the image's `/etc/group`:
/// the process runs with that group, while the home's owner and an appended
/// entry have the entry's gid, or 0.
///
/// `home` was judged by `config::check_home_directory`, so it can't shift a
/// field or start a line.
pub fn edit_passwd_home(passwd: &[u8], configured_user: &str, home: &str) -> PasswdEdit {
    let (user, group) = configured_user
        .split_once(':')
        .unwrap_or((configured_user, ""));
    let uid = if user.is_empty() {
        Some(0)
    } else {
        user.parse::<u32>().ok()
    };
    let group = group.parse::<u32>().ok();

    let mut position = 0;
    for line in passwd.split_inclusive(|byte| *byte == b'\n') {
        let selected = parse_passwd_entry(line).filter(|entry| match uid {
            Some(uid) => entry.uid == Some(uid),
            None => entry.name == user.as_bytes(),
        });
        if let Some(entry) = selected {
            let mut contents = passwd[..position + entry.home.start].to_vec();
            contents.resize(contents.len() + entry.separators_needed, b':');
            contents.extend_from_slice(home.as_bytes());
            contents.extend_from_slice(&passwd[position + entry.home.end..]);
            return PasswdEdit {
                contents,
                home_owner: Some(HomeOwner {
                    uid: entry.uid.unwrap_or(0),
                    gid: group.or(entry.gid).unwrap_or(0),
                }),
            };
        }
        position += line.len();
    }

    let Some(uid) = uid else {
        return PasswdEdit {
            contents: passwd.to_vec(),
            home_owner: None,
        };
    };
    let gid = group.unwrap_or(0);
    let name = if uid == 0 {
        "root".to_string()
    } else {
        uid.to_string()
    };
    let mut contents = passwd.to_vec();
    if !contents.is_empty() && contents.last() != Some(&b'\n') {
        contents.push(b'\n');
    }
    contents.extend_from_slice(format!("{name}:*:{uid}:{gid}::{home}:/bin/sh\n").as_bytes());
    PasswdEdit {
        contents,
        home_owner: Some(HomeOwner { uid, gid }),
    }
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
