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
//! `identity` edits them instead (decisions/0011): [`edit_passwd_home`] where the
//! running user stays the image's, and [`edit_passwd_for_host_user`] and
//! [`edit_group_for_host_user`] where it becomes the host user. Their input is
//! whatever an image shipped, so it is bytes throughout, and a line they aren't
//! editing is never decoded, reflowed or re-terminated.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The host user Ratect itself is running as — looked up once per
/// `run_as_current_user`-enabled container (see
/// `TaskEngine::resolve_user_mapping`) and once per `identity` container on a
/// rootful Linux daemon (`identity::host_user`), not cached, since there's only
/// ever one real host user per process.
///
/// A uid or gid the host has no passwd or group entry for has no name, which
/// is ordinary in a container run with a numeric `-u`: `None`, not a failed
/// lookup. Whoever uses a name decides what its absence means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    pub uid: u32,
    pub gid: u32,
    pub username: Option<String>,
    pub groupname: Option<String>,
}

impl CurrentUser {
    /// This user with both names, as `run_as_current_user`'s generated files
    /// need: an absent one is its id's digits, as in Batect
    /// (`UnixNativeMethods.getUserName`/`getGroupName`).
    pub fn named(self) -> NamedUser {
        NamedUser {
            uid: self.uid,
            gid: self.gid,
            username: self.username.unwrap_or_else(|| self.uid.to_string()),
            groupname: self.groupname.unwrap_or_else(|| self.gid.to_string()),
        }
    }
}

/// A [`CurrentUser`] with both names, an absent one filled in with its id's
/// digits ([`CurrentUser::named`]): the one `run_as_current_user` maps a
/// container onto, since its generated files name both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedUser {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
    pub groupname: String,
}

/// Looks up the real host user Ratect is running as. An entry the host lacks is
/// a name of `None`, and only a lookup that fails is an error. Unix-only — Ratect's
/// own testing has been Unix-only so far (see the `crossterm` caveat for
/// interactive mode), and there's no meaningful "current uid/gid" concept to
/// map a container onto on other platforms, so this errors clearly rather
/// than guessing, saying `asked_by`, what needed the user.
#[cfg(unix)]
pub fn current_user(_asked_by: &str) -> Result<CurrentUser> {
    use nix::unistd::{Gid, Group, Uid, User};

    let uid = Uid::current();
    let gid = Gid::current();

    let user = User::from_uid(uid).context("Failed to look up the current user")?;
    let group = Group::from_gid(gid).context("Failed to look up the current group")?;

    Ok(CurrentUser {
        uid: uid.as_raw(),
        gid: gid.as_raw(),
        username: user.map(|user| user.name),
        groupname: group.map(|group| group.name),
    })
}

#[cfg(not(unix))]
pub fn current_user(asked_by: &str) -> Result<CurrentUser> {
    Err(only_on_unix(asked_by))
}

/// The ids Ratect runs as, without [`current_user`]'s passwd and group lookups.
/// `None` on a host with no uid or gid.
#[cfg(unix)]
pub fn current_owner() -> Option<DirectoryOwner> {
    Some(DirectoryOwner {
        uid: nix::unistd::getuid().as_raw(),
        gid: nix::unistd::getgid().as_raw(),
    })
}

/// See the Unix form.
#[cfg(not(unix))]
pub fn current_owner() -> Option<DirectoryOwner> {
    None
}

/// Whether `path` exists on this host as something other than a directory, a
/// socket or a file, following a symbolic link as a bind mount does.
pub fn is_non_directory(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_dir())
}

/// [`current_user`]'s failure on a host with no uid or gid to map onto.
#[cfg_attr(unix, allow(dead_code))]
fn only_on_unix(asked_by: &str) -> anyhow::Error {
    anyhow::anyhow!("{asked_by} is only supported on Unix hosts")
}

/// The current host user's home directory: an absolute `$HOME`, else the passwd
/// entry's (decisions/0024). The Docker CLI's rule, so Ratect's `~/.docker` is the
/// one `docker login` wrote, and the home can be overridden. Anchors `~/.docker`,
/// `~/.ratect` and `~` in a path. `None` is a host user with no home, which is
/// ordinary under a numeric `-u`: each caller says what that means for it, or
/// asks [`home_directory`] to refuse. Only a failed lookup is an error. Unix-only,
/// same rationale as `current_user`.
#[cfg(unix)]
pub fn home() -> Result<Option<PathBuf>> {
    #[cfg(test)]
    if WITHOUT_HOME.get() > 0 {
        return Ok(None);
    }
    chosen_home(std::env::var_os("HOME"), || {
        Ok(nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .context("Failed to look up the current user")?
            .map(|user| user.dir))
    })
}

/// See the Unix form.
#[cfg(not(unix))]
pub fn home() -> Result<Option<PathBuf>> {
    Err(only_on_unix("A home directory"))
}

/// [`home`], for `needed_by`, which can't do without one: a refusal naming it
/// ([`no_home`]) where there is none.
pub fn home_directory(needed_by: &str) -> Result<PathBuf> {
    home()
        .with_context(|| format!("{needed_by} needs a home directory"))?
        .ok_or_else(|| no_home(needed_by))
}

/// The refusal for `needed_by`, which needs a home directory the host user has
/// none of: it names both places one is looked for. A [`NoHome`], so a layer that
/// hides an error's detail can tell this one, which has none to hide, apart.
pub fn no_home(needed_by: &str) -> anyhow::Error {
    NoHome {
        needed_by: needed_by.to_string(),
        uid: current_owner().map(|owner| owner.uid),
    }
    .into()
}

/// See [`no_home`].
#[derive(Debug)]
pub struct NoHome {
    needed_by: String,
    uid: Option<u32>,
}

impl std::fmt::Display for NoHome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let uid = self
            .uid
            .map_or_else(|| "this user".to_string(), |uid| format!("uid {uid}"));
        write!(
            f,
            "{} needs a home directory, and neither HOME nor a passwd entry for {uid} gives one",
            self.needed_by
        )
    }
}

impl std::error::Error for NoHome {}

#[cfg(test)]
thread_local! {
    static WITHOUT_HOME: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Makes [`home`] find no home, on this thread, until the guard drops: for a test
/// of what a caller does then, since the real case needs a uid with no passwd
/// entry and `HOME` unset, which a unit test can't arrange. Thread-local, so it
/// reaches only code running on the test's own thread: a plain `#[test]`, or a
/// default (current-thread) `#[tokio::test]` reaching `home` outside
/// `spawn_blocking`. Guards nest.
#[cfg(test)]
pub(crate) fn without_home() -> WithoutHome {
    WITHOUT_HOME.set(WITHOUT_HOME.get() + 1);
    WithoutHome(std::marker::PhantomData)
}

/// See [`without_home`]. Built only by it, and not `Send`, so it drops on the
/// thread whose count it raised.
#[cfg(test)]
pub(crate) struct WithoutHome(std::marker::PhantomData<*const ()>);

#[cfg(test)]
impl Drop for WithoutHome {
    fn drop(&mut self) {
        WITHOUT_HOME.set(WITHOUT_HOME.get() - 1);
    }
}

/// The home given `home`, the value of `$HOME`, and `passwd_home`, the passwd
/// entry's, asked only when `home` is unset, empty or not absolute. The Docker CLI
/// takes a relative `$HOME` as written, but every use here builds an absolute path
/// from the home, so a relative one counts as unset, and so does a passwd entry's.
/// `None` is no home from either.
#[cfg_attr(not(unix), allow(dead_code))]
fn chosen_home(
    home: Option<std::ffi::OsString>,
    passwd_home: impl FnOnce() -> Result<Option<PathBuf>>,
) -> Result<Option<PathBuf>> {
    match home.filter(|home| Path::new(home).is_absolute()) {
        Some(home) => Ok(Some(PathBuf::from(home))),
        None => Ok(passwd_home()?.filter(|home| home.is_absolute())),
    }
}

/// Generates a minimal `/etc/passwd` for `user`, granting `home_directory` as
/// their home. Ported from Batect's `RunAsCurrentUserConfigurationProvider.
/// generatePasswdFile` — when the current user's `uid` is already `0`
/// (i.e. Ratect itself is running as root), there's only one `root` entry
/// (using the configured `home_directory` as its home) rather than two
/// conflicting ones.
pub fn generate_passwd_file(user: &NamedUser, home_directory: &str) -> String {
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
pub fn generate_shadow_file(user: &NamedUser) -> String {
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
pub fn generate_group_file(user: &NamedUser) -> String {
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

/// One of an `identity`'s additional users, as the file declares it, and as
/// [`with_additional_users`] takes it: here rather than in `config` since
/// [`crate::container_spec::IdentityPlan`] carries it to the runtime's side of that
/// boundary. The ids are required, since Ratect has no way to allocate one that an
/// image it hasn't seen leaves free. What can be judged from the file alone is
/// judged by `config::validate_additional_users`, and a clash with the image's own
/// entries or the running user by [`with_additional_users`], when both are known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdditionalUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
}

/// A file's or directory's owner by numeric ids, as a stat reports them. Who
/// `identity` makes a directory's owner is one, since Docker's archive API has no
/// use for a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryOwner {
    pub uid: u32,
    pub gid: u32,
}

/// Who owns `path` on this host, following a symbolic link as a bind mount does.
/// `None` for a path that can't be read, or on a host with no uid or gid.
#[cfg(unix)]
pub fn path_owner(path: &std::path::Path) -> Option<DirectoryOwner> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).ok()?;
    Some(DirectoryOwner {
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

/// See the Unix form.
#[cfg(not(unix))]
pub fn path_owner(_path: &std::path::Path) -> Option<DirectoryOwner> {
    None
}

/// What [`edit_passwd_home`] made of an image's `/etc/passwd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswdEdit {
    /// The file's new contents.
    pub contents: Vec<u8>,
    /// Who the running user turned out to be. `None` for a named user with no
    /// entry, where `contents` is the file unchanged: the container can't start
    /// as a user its image lacks, and the runtime says so when it tries.
    pub home_owner: Option<DirectoryOwner>,
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

/// Where in `line` its entry is, without the spaces around it. `None` for a
/// comment or a blank line, which aren't entries.
fn entry_range(line: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = line.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let end = line.iter().rposition(|byte| !byte.is_ascii_whitespace())? + 1;
    (line[start] != b'#').then_some(start..end)
}

fn parse_id(field: Option<&&[u8]>) -> Option<u32> {
    std::str::from_utf8(field?).ok()?.parse().ok()
}

fn parse_passwd_entry(line: &[u8]) -> Option<PasswdEntry<'_>> {
    let std::ops::Range { start, end } = entry_range(line)?;
    let text = &line[start..end];
    let fields: Vec<&[u8]> = text.splitn(7, |byte| *byte == b':').collect();
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
        uid: parse_id(fields.get(2)),
        gid: parse_id(fields.get(3)),
        home,
        separators_needed,
    })
}

/// The user and group of a `Config.User` value, either of them possibly empty.
fn split_configured_user(configured_user: &str) -> (&str, &str) {
    configured_user
        .split_once(':')
        .unwrap_or((configured_user, ""))
}

/// The uid `user`, the user half of a `Config.User` value, says outright: an
/// empty one is root, and all-digit text is always a uid. `None` for a name.
fn configured_uid(user: &str) -> Option<u32> {
    if user.is_empty() {
        Some(0)
    } else {
        user.parse().ok()
    }
}

/// Whether `user` is root, as far as its text says: a configured `user`, or an
/// image's `Config.User` in any of the forms [`edit_passwd_home`] takes. Another
/// name for uid 0 isn't known without the image's `/etc/passwd`, so it counts as
/// non-root.
pub fn is_root_user(user: &str) -> bool {
    let (user, _) = split_configured_user(user);
    user == "root" || configured_uid(user) == Some(0)
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
    let (user, group) = split_configured_user(configured_user);
    let uid = configured_uid(user);
    let group = group.parse::<u32>().ok();

    let selected = first_passwd_entry(passwd, |entry| match uid {
        Some(uid) => entry.uid == Some(uid),
        None => entry.name == user.as_bytes(),
    });
    if let Some((position, entry)) = selected {
        return PasswdEdit {
            contents: with_passwd_home(passwd, position, &entry, home),
            home_owner: Some(DirectoryOwner {
                uid: entry.uid.unwrap_or(0),
                gid: group.or(entry.gid).unwrap_or(0),
            }),
        };
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
    PasswdEdit {
        contents: with_passwd_entry_appended(passwd, &name, uid, gid, home),
        home_owner: Some(DirectoryOwner { uid, gid }),
    }
}

/// Makes an image's `/etc/passwd` agree with `user`, the host user the process
/// runs as on a rootful Linux daemon. Its uid's first entry, the one the runtime
/// uses, keeps its name and gid and gets `home`; the process's gid is `--user`'s
/// whatever the entry says. A uid with no entry gets one appended, named after
/// `user`, or with the uid's digits if the image uses that name for another uid or
/// the host has no name for it.
///
/// `home` is as [`edit_passwd_home`]'s is.
pub fn edit_passwd_for_host_user(passwd: &[u8], user: &CurrentUser, home: &str) -> Vec<u8> {
    if let Some((position, entry)) = first_passwd_entry(passwd, |entry| entry.uid == Some(user.uid))
    {
        return with_passwd_home(passwd, position, &entry, home);
    }
    let name = user
        .username
        .clone()
        .filter(|name| first_passwd_entry(passwd, |entry| entry.name == name.as_bytes()).is_none())
        .unwrap_or_else(|| user.uid.to_string());
    with_passwd_entry_appended(passwd, &name, user.uid, user.gid, home)
}

/// What an image's `/etc/group` should become for `user`, the host user the
/// process runs as on a rootful Linux daemon, or `None` to leave it alone: an
/// entry named after `user`'s group is appended only if neither its gid nor that
/// name is in the file. A group entry only gives `id` and `ls` a name to print, so
/// on a clash, or for a gid the host has no name for, there is nothing worth
/// inventing one for.
pub fn edit_group_for_host_user(group: &[u8], user: &CurrentUser) -> Option<Vec<u8>> {
    let name = user.groupname.as_deref()?;
    let taken = group_entry_taken(group, name, user.gid);
    (!taken).then(|| with_line_appended(group, &format!("{name}:x:{}:", user.gid)))
}

/// Who a clash found in the file can be with. Which of them it is can't be told
/// from the file, and a clash with the host user is only on some machines.
const CLASH_IS_WITH: &str = "that is the image's user or the running user";

/// `passwd`, by now holding the image's entries and the running user's, with an
/// entry appended for each of `users`: password `*`, and a home nothing creates
/// (`/nonexistent`, the name for that), since Ratect makes no directories for
/// them. A name or uid already there is an error rather than an entry overwritten
/// or a lookup left to find whichever comes first, and so is the running user's,
/// which can't be told apart from the image's here and isn't an additional user
/// either way. A gid already there isn't, since sharing one is ordinary.
///
/// `users` were judged by `config::validate_additional_users`, so a name can't
/// shift a field or start a line, and they can't clash with each other.
pub fn with_additional_users(passwd: &[u8], users: &[AdditionalUser]) -> Result<Vec<u8>> {
    let mut contents = passwd.to_vec();
    for AdditionalUser { name, uid, gid } in users {
        if let Some((_, entry)) = first_passwd_entry(passwd, |entry| entry.uid == Some(*uid)) {
            anyhow::bail!(
                "additional user '{name}' has uid {uid}, which '/etc/passwd' already gives to \
                 '{}': {CLASH_IS_WITH}",
                entry.name.escape_ascii()
            );
        }
        anyhow::ensure!(
            first_passwd_entry(passwd, |entry| entry.name == name.as_bytes()).is_none(),
            "additional user '{name}' has a name '/etc/passwd' already has: {CLASH_IS_WITH}"
        );
        contents = with_passwd_entry_appended(&contents, name, *uid, *gid, "/nonexistent");
    }
    Ok(contents)
}

/// `group` with an entry named after each of `users`, for its gid, by the rule
/// [`edit_group_for_host_user`] gives its reasons for.
pub fn with_additional_groups(group: &[u8], users: &[AdditionalUser]) -> Vec<u8> {
    let mut contents = group.to_vec();
    for AdditionalUser { name, gid, .. } in users {
        if !group_entry_taken(&contents, name, *gid) {
            contents = with_line_appended(&contents, &format!("{name}:x:{gid}:"));
        }
    }
    contents
}

/// Whether `group` has an entry named `name` or for `gid`.
fn group_entry_taken(group: &[u8], name: &str, gid: u32) -> bool {
    group.split_inclusive(|byte| *byte == b'\n').any(|line| {
        let Some(entry) = entry_range(line) else {
            return false;
        };
        let mut fields = line[entry].split(|byte| *byte == b':');
        let entry_name = fields.next();
        let entry_gid = fields.nth(1);
        entry_name == Some(name.as_bytes()) || parse_id(entry_gid.as_ref()) == Some(gid)
    })
}

/// The first entry of `passwd` that `selected` accepts, with where its line
/// starts.
fn first_passwd_entry<'a>(
    passwd: &'a [u8],
    selected: impl Fn(&PasswdEntry) -> bool,
) -> Option<(usize, PasswdEntry<'a>)> {
    let mut position = 0;
    for line in passwd.split_inclusive(|byte| *byte == b'\n') {
        if let Some(entry) = parse_passwd_entry(line).filter(&selected) {
            return Some((position, entry));
        }
        position += line.len();
    }
    None
}

/// `passwd` with `entry`, on the line at `position`, given `home`.
fn with_passwd_home(passwd: &[u8], position: usize, entry: &PasswdEntry, home: &str) -> Vec<u8> {
    let mut contents = passwd[..position + entry.home.start].to_vec();
    contents.resize(contents.len() + entry.separators_needed, b':');
    contents.extend_from_slice(home.as_bytes());
    contents.extend_from_slice(&passwd[position + entry.home.end..]);
    contents
}

/// `passwd` with a new entry for `uid`, password `*`.
fn with_passwd_entry_appended(
    passwd: &[u8],
    name: &str,
    uid: u32,
    gid: u32,
    home: &str,
) -> Vec<u8> {
    with_line_appended(passwd, &format!("{name}:*:{uid}:{gid}::{home}:/bin/sh"))
}

/// `file` with `line` as its new last line.
fn with_line_appended(file: &[u8], line: &str) -> Vec<u8> {
    let mut contents = file.to_vec();
    if !contents.is_empty() && contents.last() != Some(&b'\n') {
        contents.push(b'\n');
    }
    contents.extend_from_slice(line.as_bytes());
    contents.push(b'\n');
    contents
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
