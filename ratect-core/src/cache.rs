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

//! Resolves `cache` volume mounts ([`crate::config::CacheVolumeMount`]) to an
//! actual Docker bind-mount string — a named volume that persists between
//! separate `ratect` invocations ([`CacheType::Volume`], the default) or a
//! host directory ([`CacheType::Directory`], `--cache-type=directory`) via
//! [`resolve_cache_mount`] — and, through [`CacheStore`], the concept behind
//! `ratect caches`/`ratect-compat --clean`/`--clean-cache`: what cache
//! storage exists, and how to list or remove it.
//!
//! These are two different questions answered by one module, on purpose:
//! *what does this `cache` mount become at container-create time* and *what
//! storage exists and how do I remove it* don't share a caller, and folding
//! them into one type would make that type answer both again.
//!
//! Ported from Batect's own `CacheManager`/`VolumeMountResolver`/`CacheType`/
//! `CleanupCachesCommand`, and kept byte-for-byte compatible with its
//! `.batect/caches/` location and `batect-cache-<project key>-<name>` volume
//! naming *on purpose*: this is `ratect-compat`'s territory, so a project
//! migrating from real `batect` finds its existing caches reused rather than
//! orphaned. Batect has no *shared* cache — [`CacheStore`]'s scope handling
//! is `ratect`-only.
//!
//! Deliberate divergences:
//!
//! - A freshly generated [`project_cache_key`] is a full
//!   `uuid::Uuid::new_v4()`, not Batect's 6-character `a-z0-9` id, whose
//!   alphabet is meaningfully more collision-prone across many projects on
//!   one machine. An existing Batect-written key file is still read and
//!   its key reused, tolerant of its `#`-comment header — nothing
//!   depends on matching the *generation* format, only on the file's path
//!   and layout.
//! - An indented `#` line in the key file is a comment, where Batect tests
//!   for the `#` before trimming and so counts it as a key line: beside the
//!   key it makes Batect fail, and alone it is Batect's key and a "no key
//!   line" error here.
//! - A key path that is a symbolic link to nothing is an error, where Batect
//!   writes a new key file through it, wherever it points.
//! - Cleaning directory caches when the project has no `.batect/caches`
//!   directory removes nothing, with a warning saying so, where Batect fails.
//! - A cache directory entry that is a symbolic link is skipped when
//!   cleaning, with a warning naming it, where Batect follows it and deletes
//!   whatever it points to.
//!
//! The removal *decision* — which volumes or directories match this project,
//! restricted to `--clean-cache`'s allowlist — lives in plain synchronous
//! functions (`matching_cache_volumes`/`matching_cache_directories`),
//! deliberately separate from the async I/O around them, so it is testable
//! against plain `Vec<String>`/tempdir fixtures with no fake
//! `VolumeStore`.

use crate::config::{CacheScope, CacheVolumeMount};
use crate::container_spec::{BindMount, BindSource};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The volume operations [`CacheStore`] needs — a narrower contract than
/// [`crate::docker::ContainerRuntime`]'s full 16 methods, which this module
/// doesn't otherwise touch. `ContainerRuntime` requires this as a
/// supertrait, so `DockerClient` implements both without restating either
/// method.
#[async_trait::async_trait]
pub trait VolumeStore {
    /// Lists every Docker volume's name on the daemon — used by
    /// `--clean`/`--clean-cache` (see `clean_volume_caches`) to find this
    /// project's own `batect-cache-<key>-*` volumes among them. No
    /// filtering here; the caller matches the prefix itself, the same way
    /// Batect's own `CleanupCachesCommand` does.
    async fn list_volumes(&self) -> Result<Vec<String>>;

    /// Removes the named Docker volume — used by
    /// `--clean`/`--clean-cache` once `list_volumes` has identified it as
    /// one of this project's own cache volumes.
    async fn remove_volume(&self, name: &str) -> Result<()>;
}

/// Where a `cache` mount's contents actually live. Selected by `--cache-type`
/// (default `Volume`), matching Batect's own `CacheType` — except Batect
/// additionally forces `Directory` for Windows containers; Ratect has no
/// Windows support to special-case yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheType {
    #[default]
    Volume,
    Directory,
}

/// Bundles the per-invocation settings [`resolve_cache_mount`] needs beyond
/// the mount itself.
#[derive(Debug, Clone)]
pub struct CacheOptions {
    pub cache_type: CacheType,
    pub project_directory: PathBuf,
}

/// The project-local directory Ratect's cache mechanism uses — `.batect/`,
/// not `.ratect/`, deliberately: this is where an existing Batect project
/// already keeps its own `key` file and any `directory`-type cache contents,
/// and reusing them (rather than starting cold under a Ratect-only
/// directory name) is the entire point of `ratect-compat`'s parity goal.
pub fn cache_directory(project_directory: &Path) -> PathBuf {
    project_directory.join(".batect").join("caches")
}

/// The per-project key embedded in every cache volume's name
/// (`batect-cache-<key>-<name>`) — without it, two unrelated projects that
/// happen to declare a same-named cache (e.g. `gradle-cache`) would collide
/// on the exact same Docker volume, since Docker volumes live in one flat,
/// global namespace, not scoped by project directory.
///
/// Reads `<project_directory>/.batect/caches/key` if it already exists, in
/// Batect's own file format, so a project already run under real Batect has
/// its existing key discovered and reused, preserving the exact volume names
/// Batect itself would use. Blank lines and comments are skipped, and the
/// one line left, trimmed, is the key. As in `CacheManager.projectCacheKey`,
/// a file left with no such line or with more than one is an error, as is
/// one that can't be read, and bytes that aren't UTF-8 are decoded with
/// replacement characters rather than refused. Unlike it, a line is trimmed
/// before testing for the `#` that starts a comment — see the module's
/// deliberate divergences.
///
/// An existing file is never written over: its key names every cache volume
/// the project already has, so replacing it would orphan them all where
/// `--clean` could no longer find them (ratect#337).
///
/// Only when no file exists yet does this generate and persist a new one: a full
/// `uuid::Uuid::new_v4()` rather than Batect's 6-char `a-z0-9` id. This
/// doesn't affect compatibility — Batect's reader has no length/charset
/// check, it just takes whatever's on that one line, so the value's shape
/// is opaque to both tools; only the file's *path* and *read-compatible
/// format* matter for interop. A full UUID is simply safer: Batect's own
/// 6-char alphabet only has ~2.18 billion combinations, meaningfully more
/// collision-prone across many projects on one machine than a UUID, with no
/// upside since nothing depends on matching that format for a freshly
/// generated key.
pub fn project_cache_key(project_directory: &Path) -> Result<String> {
    let key_path = cache_directory(project_directory).join("key");

    match fs::read(&key_path) {
        Ok(contents) => return read_project_cache_key(&key_path, &contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to read cache key file {key_path:?}"))
        }
    }

    // Not found, yet a link is there: a symbolic link to nothing, which
    // writing would follow and create a file wherever it points.
    if key_path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        anyhow::bail!(
            "The cache key file {key_path:?} is a symbolic link to something that doesn't \
             exist. Remove the link, or point it at the project's key file."
        );
    }

    let key = uuid::Uuid::new_v4().to_string();
    let parent = key_path
        .parent()
        .expect("cache_directory() always returns a path with a parent");
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create cache directory {parent:?}"))?;
    fs::write(
        &key_path,
        format!(
            "# This file was autogenerated to track which Docker volumes are associated with \
             this project.\n# Do not modify it, and do not commit it to source control.\n{key}\n"
        ),
    )
    .with_context(|| format!("Failed to write cache key file {key_path:?}"))?;

    Ok(key)
}

/// The key in an existing key file's `contents` — see [`project_cache_key`].
fn read_project_cache_key(key_path: &Path, contents: &[u8]) -> Result<String> {
    let contents = String::from_utf8_lossy(contents);
    let mut key_lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    match (key_lines.next(), key_lines.next()) {
        (Some(key), None) => Ok(key.to_string()),
        (None, _) => anyhow::bail!(
            "The cache key file {key_path:?} has no key line. It should hold exactly one line \
             that isn't blank or a comment: the key every one of this project's cache volumes \
             is named after, the `<key>` in their `batect-cache-<key>-<name>` names. Restore \
             that line, or delete the file to start a new key, leaving the project's existing \
             cache volumes behind."
        ),
        (Some(_), Some(_)) => anyhow::bail!(
            "The cache key file {key_path:?} has more than one key line. It should hold exactly \
             one line that isn't blank or a comment: the key every one of this project's cache \
             volumes is named after. Keep only that line."
        ),
    }
}

/// The Docker volume name a `cache` mount named `name` resolves to under
/// `CacheType::Volume` — `batect-cache-<project_cache_key>-<name>`, Batect's
/// own literal prefix (see the module's own doc comment for why it isn't
/// `ratect-cache-`).
fn cache_volume_name(project_cache_key: &str, name: &str) -> String {
    format!("batect-cache-{project_cache_key}-{name}")
}

/// The Docker volume name a **shared** cache resolves to —
/// `ratect-shared-cache-<name>`, with no project key, which is the whole
/// point: every project naming it gets the same storage.
///
/// The `ratect-` prefix is deliberate on two counts. Batect has no shared
/// cache, so there is no naming convention to stay compatible with; and
/// because it differs from [`cache_volume_name`]'s `batect-cache-` prefix,
/// `matching_cache_volumes` cannot match a shared cache even by accident,
/// so a bare `--clean` can never discard storage other projects are using.
fn shared_cache_volume_name(name: &str) -> String {
    format!("ratect-shared-cache-{name}")
}

/// The host directory a **shared** cache resolves to under
/// `CacheType::Directory` — `~/.ratect/caches/<name>`, beside
/// `~/.ratect/incl`'s Git-include clones, for the same reason: it belongs to
/// the user's machine rather than to any one project.
fn shared_cache_directory(name: &str) -> Result<PathBuf> {
    Ok(shared_cache_root()?.join(name))
}

/// Where every shared cache directory lives — `~/.ratect/caches`.
pub fn shared_cache_root() -> Result<PathBuf> {
    Ok(crate::user::home_directory()?
        .join(".ratect")
        .join("caches"))
}

/// Resolves `mount` to a [`BindMount`] — its source a bare Docker volume
/// name under `CacheType::Volume` (Docker itself auto-creates a named volume
/// on first use), or an absolute host directory under
/// `.batect/caches/<name>/` under `CacheType::Directory` (created here if
/// missing, matching Batect's own `Files.createDirectories`).
///
/// `project_cache_key` is called only for a project-scoped mount under
/// `CacheType::Volume`, the one source named after the key: Batect reads
/// `CacheManager.projectCacheKey` only in `VolumeMountResolver`'s
/// `CacheType.Volume` branch, so a directory-mode run never reads, creates
/// or rewrites the key file (ratect#325), and neither does a shared cache.
pub fn resolve_cache_mount(
    options: &CacheOptions,
    project_cache_key: impl FnOnce() -> Result<String>,
    mount: &CacheVolumeMount,
) -> Result<BindMount> {
    let shared = mount.scope() == crate::config::CacheScope::Shared;
    let source = match options.cache_type {
        CacheType::Volume if shared => BindSource::Volume(shared_cache_volume_name(&mount.name)),
        CacheType::Volume => {
            BindSource::Volume(cache_volume_name(&project_cache_key()?, &mount.name))
        }
        CacheType::Directory => {
            let dir = if shared {
                shared_cache_directory(&mount.name)?
            } else {
                cache_directory(&options.project_directory).join(&mount.name)
            };
            fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create cache directory {dir:?}"))?;
            BindSource::Host(dir.display().to_string())
        }
    };

    Ok(BindMount {
        source,
        container: mount.container.clone(),
        options: mount.options.clone(),
    })
}

/// Which shared cache volumes `only` names — the removal *decision*, kept
/// synchronous and separate from the I/O around it, the same way
/// `matching_cache_volumes` is.
///
/// **An empty `only` matches nothing**, the opposite of the project-scoped
/// rule where empty means "all of this project's". A shared cache holds
/// storage other projects are still using, so a bare `caches clean` must
/// never reach one. Stating that as a precondition on the caller was not
/// enough — it read as satisfied and wasn't, and every shared cache on the
/// machine was swept.
fn matching_shared_cache_volumes<'a>(
    existing_volumes: &'a [String],
    only: &HashSet<String>,
) -> Vec<&'a str> {
    if only.is_empty() {
        return Vec::new();
    }
    matching_cache_volumes(existing_volumes, &shared_cache_volume_name(""), only)
}

/// Every cache volume this project can see, with the scope each one's name
/// implies — one `list_volumes` call covering both scopes. With no
/// `project_cache_key`, only shared ones: a project volume is recognised by
/// its key alone.
async fn list_all_volume_caches(
    runtime: &impl VolumeStore,
    project_cache_key: Option<&str>,
) -> Result<Vec<(String, CacheScope)>> {
    let existing = runtime.list_volumes().await?;
    let project_prefix = project_cache_key.map(|key| cache_volume_name(key, ""));
    let shared_prefix = shared_cache_volume_name("");

    let mut found: Vec<(String, CacheScope)> = existing
        .iter()
        .filter_map(|volume| {
            if let Some(name) = project_prefix
                .as_ref()
                .and_then(|prefix| volume.strip_prefix(prefix))
            {
                Some((name.to_string(), CacheScope::Project))
            } else {
                volume
                    .strip_prefix(&shared_prefix)
                    .map(|name| (name.to_string(), CacheScope::Shared))
            }
        })
        .collect();
    found.sort();
    Ok(found)
}

/// Removes the named shared cache volumes, returning the cache names
/// actually removed.
///
/// **An empty `only` removes nothing**, the opposite of
/// [`clean_volume_caches`], where empty means "this project's, all of
/// them". A shared cache holds storage other projects are still using, so
/// sweeping them can never be the default — and stating that as a
/// precondition on the caller was not enough: it read as satisfied and
/// wasn't, so a bare `caches clean` removed every shared cache on the
/// machine. The rule lives here now, where it cannot be forgotten.
async fn clean_shared_volume_caches(
    runtime: &impl VolumeStore,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let matched: Vec<String> = matching_shared_cache_volumes(&existing, only)
        .into_iter()
        .map(str::to_string)
        .collect();

    for name in &matched {
        runtime.remove_volume(name).await?;
    }

    let prefix = shared_cache_volume_name("");
    Ok(matched
        .iter()
        .map(|name| name.strip_prefix(&prefix).unwrap_or(name).to_string())
        .collect())
}

/// Removes the named shared cache directories under `root`, returning those
/// removed.
///
/// **An empty `only` removes nothing** — see [`clean_shared_volume_caches`]
/// for why that is the opposite of the project-scoped rule. `root` is a
/// parameter rather than resolved here (as it once was, via
/// `shared_cache_root()`): [`CacheStore`] resolves it once, tolerating a host
/// with no home directory by treating "no root" as "no shared caches",
/// rather than this function silently swallowing that failure on every call.
fn clean_shared_directory_caches(root: &Path, only: &HashSet<String>) -> Result<Vec<String>> {
    if only.is_empty() {
        return Ok(Vec::new());
    }
    let matched = matching_cache_directories(root, only)?;
    for name in &matched {
        let dir = root.join(name);
        fs::remove_dir_all(&dir).with_context(|| format!("Failed to remove {dir:?}"))?;
    }
    Ok(matched)
}

/// Filters `existing_volumes` (from [`VolumeStore::list_volumes`])
/// down to those whose name starts with `prefix`, further restricted to
/// `only` when non-empty (the `--clean-cache <name>` allowlist; empty means
/// "everything under this prefix").
///
/// Takes the prefix rather than a project key so both scopes share it —
/// `batect-cache-<key>-` for a project's own, `ratect-shared-cache-` for
/// the machine's. **The empty-`only` rule differs by scope and is decided by
/// the caller**: for a project cache empty means "all of them", and for a
/// shared one it must mean "none", which is why
/// [`matching_shared_cache_volumes`] guards it before delegating here.
///
/// A pure, synchronous decision function deliberately kept separate from
/// the I/O in [`clean_volume_caches`], so it's unit-testable against plain
/// `Vec<String>` fixtures without needing a fake `VolumeStore`.
fn matching_cache_volumes<'a>(
    existing_volumes: &'a [String],
    prefix: &str,
    only: &HashSet<String>,
) -> Vec<&'a str> {
    existing_volumes
        .iter()
        .filter_map(|name| {
            let cache_name = name.strip_prefix(prefix)?;
            (only.is_empty() || only.contains(cache_name)).then_some(name.as_str())
        })
        .collect()
}

/// The `CacheType::Directory` counterpart of [`matching_cache_volumes`]'s
/// listing use — already sorted, by `matching_cache_directories`.
fn list_directory_caches(project_directory: &Path) -> Result<Vec<String>> {
    matching_cache_directories(&cache_directory(project_directory), &HashSet::new())
}

/// Removes this project's own cache volumes (or, with `only` non-empty,
/// just the named ones) — `--clean`/`--clean-cache` under
/// `CacheType::Volume`. Mirrors Batect's own `CleanupCachesCommand.runForVolumes`.
/// Returns the *cache* names actually removed (the prefix stripped), matching
/// [`clean_shared_volume_caches`]'s convention — [`CacheStore`] reconstructs
/// the full volume name from a cache name whenever it needs to, so the two
/// removal functions it sits beside can agree on one contract.
async fn clean_volume_caches(
    runtime: &impl VolumeStore,
    project_cache_key: &str,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let prefix = cache_volume_name(project_cache_key, "");
    let matched: Vec<String> = matching_cache_volumes(&existing, &prefix, only)
        .into_iter()
        .map(str::to_string)
        .collect();

    for name in &matched {
        runtime.remove_volume(name).await?;
    }

    Ok(matched
        .iter()
        .map(|name| name.strip_prefix(&prefix).unwrap_or(name).to_string())
        .collect())
}

/// The synchronous counterpart of `matching_cache_volumes` for
/// `CacheType::Directory`: this project's own cache directories are exactly
/// [`cache_directory`]'s own subdirectories (the `key` file living
/// alongside them is a plain file, not a directory, so it's never matched
/// here) — restricted to `only` when non-empty, same convention as above.
fn matching_cache_directories(cache_dir: &Path, only: &HashSet<String>) -> Result<Vec<String>> {
    Ok(scan_cache_directories(cache_dir, only)?
        .map(|found| found.matched)
        .unwrap_or_default())
}

/// What [`scan_cache_directories`] found in a cache directory that exists.
struct CacheDirectoryScan {
    /// The matching subdirectories' names, sorted.
    matched: Vec<String>,
    /// The entries `only` would have matched that are symbolic links, and so
    /// were skipped rather than followed — sorted, as full paths.
    skipped_links: Vec<PathBuf>,
}

/// The subdirectories of `cache_dir` that `only` selects, plus what was
/// passed over: `None` when `cache_dir` doesn't exist, and every selected
/// entry that is a symbolic link, skipped rather than followed —
/// [`clean_directory_caches`] warns about both rather than staying silent.
fn scan_cache_directories(
    cache_dir: &Path,
    only: &HashSet<String>,
) -> Result<Option<CacheDirectoryScan>> {
    if !cache_dir.is_dir() {
        return Ok(None);
    }

    let mut found = CacheDirectoryScan {
        matched: Vec::new(),
        skipped_links: Vec::new(),
    };
    for entry in fs::read_dir(cache_dir).with_context(|| format!("Failed to read {cache_dir:?}"))? {
        let entry = entry.with_context(|| format!("Failed to read an entry in {cache_dir:?}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(only.is_empty() || only.contains(&name)) {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to inspect {:?}", entry.path()))?;
        if file_type.is_symlink() {
            found.skipped_links.push(entry.path());
        } else if file_type.is_dir() {
            found.matched.push(name);
        }
    }
    found.matched.sort();
    found.skipped_links.sort();

    Ok(Some(found))
}

/// Removes this project's own cache directories (or, with `only` non-empty,
/// just the named ones) — `--clean`/`--clean-cache` under
/// `CacheType::Directory`. Mirrors Batect's own
/// `CleanupCachesCommand.runForDirectories`, except that a missing cache
/// directory removes nothing rather than failing, and an entry that is a
/// symbolic link is skipped rather than followed — each passed to `warn`,
/// once, since both would otherwise read exactly like a clean that found
/// nothing to do. Returns the names actually removed.
fn clean_directory_caches(
    project_directory: &Path,
    only: &HashSet<String>,
    warn: impl Fn(String),
) -> Result<Vec<String>> {
    let cache_dir = cache_directory(project_directory);
    let Some(found) = scan_cache_directories(&cache_dir, only)? else {
        warn(format!(
            "There is no cache directory at '{}', so there is nothing to remove.",
            cache_dir.display()
        ));
        return Ok(Vec::new());
    };
    for link in &found.skipped_links {
        warn(format!(
            "Skipped '{}': it is a symbolic link, and a cache clean never follows one.",
            link.display()
        ));
    }
    let matched = found.matched;

    for name in &matched {
        let dir = cache_dir.join(name);
        fs::remove_dir_all(&dir).with_context(|| format!("Failed to remove {dir:?}"))?;
    }

    Ok(matched)
}

/// The caches one `ratect caches`/`ratect-compat --clean` invocation is
/// working with: what this project can see, split by whether the project
/// *owns* them.
///
/// The split is the point. Before shared caches, `caches` rested on an
/// unstated invariant — everything it showed you belonged to this project,
/// so anything it showed you, you could delete. That is why the heading
/// could say "this project's", why an empty name set could mean "all of
/// them", and why `-o quiet` was safe to pipe into `clean`.
///
/// Shared caches invalidated it. Carrying scope as a bare tag alongside each
/// name meant every site re-derived what it implied — the heading, the quiet
/// filter, the ambiguity check, the removal gate — and each could be wrong
/// on its own. Every defect in this area was one of them being wrong
/// separately. This answers the question once instead.
pub struct CacheSelection {
    /// This project's own caches — what a bare `clean` sweeps.
    pub owned: Vec<String>,
    /// Caches shared with every project on the machine: visible from here,
    /// but not this project's, and removed only when named.
    pub shared: Vec<String>,
    /// The `--scope` this invocation was given, which narrows both.
    pub scope: Option<CacheScope>,
}

impl CacheSelection {
    pub fn is_empty(&self) -> bool {
        self.owned.is_empty() && self.shared.is_empty()
    }

    /// What `-o quiet` prints — and, by construction, exactly what a `clean`
    /// carrying the same flags would act on.
    ///
    /// Holding those two together is what makes the machine-readable listing
    /// safe to pipe straight back: everything it emits, the matching `clean`
    /// may remove. Deriving them separately is how a bare listing came to
    /// emit every shared cache on the machine into a command that deletes.
    pub fn actionable(&self) -> &[String] {
        match self.scope {
            Some(CacheScope::Shared) => &self.shared,
            _ => &self.owned,
        }
    }

    /// Which of `wanted` name a shared cache and *not* one of this
    /// project's — the names a `clean` without `--scope shared` must refuse.
    ///
    /// Covers both the case where the name is only shared and the case where
    /// it is both: in either, removing the shared storage is something the
    /// caller has to ask for explicitly. That is one rule where there were
    /// previously two, and the second (ambiguity) only ever fired on
    /// collision — which is why naming a cache this project had never
    /// created removed another project's without a word.
    pub fn shared_only<'a>(&'a self, wanted: &'a HashSet<String>) -> Vec<&'a String> {
        let mut names: Vec<&String> = self
            .shared
            .iter()
            .filter(|name| wanted.contains(*name))
            .collect();
        names.sort();
        names
    }

    /// Whether this invocation may remove caches of `scope` at all.
    pub fn covers(&self, scope: CacheScope) -> bool {
        self.scope.is_none_or(|wanted| wanted == scope)
    }
}

/// One cache [`CacheStore::remove`] actually removed.
#[derive(Debug, PartialEq, Eq)]
pub struct RemovedCache {
    /// What a `volumes` entry calls this cache — what `ratect` reports.
    pub name: String,
    /// Where it actually lived: a Docker volume name, or a full host path —
    /// what `ratect-compat` reports, matching Batect's own wording, which
    /// never spoke in terms of cache names at all.
    pub storage: String,
}

/// Why [`CacheStore::remove`] declined to remove a *shared* cache — returned
/// rather than turned into an error message here, since the flag it names
/// (`--scope shared`) belongs to `ratect`'s CLI, not to this crate.
///
/// **Removing a shared cache always takes `--scope shared`.** Not "when
/// named", not "when unambiguous" — always. That is the one rule, replacing
/// three special cases that each had to be discovered separately: a bare
/// `clean` sweeping every shared cache, `-o quiet` feeding machine-wide names
/// into a pipe, and naming a cache this project has never had and silently
/// removing someone else's. All three were the same invariant — an
/// unqualified operation touches only what this project owns — being
/// violated somewhere new.
#[derive(Debug, PartialEq, Eq)]
pub enum CacheRefusal {
    /// `only` names a cache that's shared (or shared and this project's
    /// both), but `scope` wasn't `Shared`.
    SharedNotNamed(String),
    /// `scope` was `Shared`, but `only` is empty — a shared cache is only
    /// ever removed by name.
    SharedSweepNotNamed,
}

/// Where a project's caches live and how to reach them — one value per
/// `ratect caches`/`ratect-compat --clean` invocation, hiding the
/// volume-or-directory × project-or-shared matrix `list`/`remove` used to
/// leave every caller to reassemble.
///
/// `Volume` needs a `VolumeStore`, because Docker holds the caches;
/// `Directory` needs none, because the filesystem does. Representing that as
/// one struct with an `Option<&D>` field left "a volume cache needs a
/// daemon" as a runtime `.expect()` no caller could see coming from the
/// type — this enum makes the missing case unrepresentable instead.
pub enum CacheStore<'a, D: VolumeStore + Send + Sync> {
    Volume {
        docker: &'a D,
        project_directory: PathBuf,
        /// The project's cache key, once something has needed it — see
        /// `CacheStore::project_cache_key`.
        project_cache_key: OnceLock<String>,
    },
    Directory {
        project_directory: PathBuf,
        /// Where every shared cache directory lives, resolved once by the
        /// caller — `None` when this host has no home directory to resolve
        /// it against, which `list`/`remove` then treat as "no shared
        /// caches" rather than a failure. Injected rather than resolved
        /// here so a test can point it at a fixture directory instead of
        /// the real `~/.ratect/caches`.
        shared_root: Option<PathBuf>,
    },
}

impl<'a, D: VolumeStore + Send + Sync> CacheStore<'a, D> {
    /// `docker` is required for `CacheType::Volume` and ignored for
    /// `CacheType::Directory` — a caller that never opened a Docker
    /// connection for a directory-type invocation can simply pass `None`.
    ///
    /// Doesn't resolve the project's cache key: only a listing or removal
    /// that includes project caches needs it, and that resolves it — see
    /// `CacheStore::project_cache_key`.
    pub fn new(
        cache_type: CacheType,
        docker: Option<&'a D>,
        project_directory: PathBuf,
        shared_root: Option<PathBuf>,
    ) -> Result<Self> {
        Ok(match cache_type {
            CacheType::Volume => Self::Volume {
                docker: docker.expect("a volume cache store needs a Docker client"),
                project_directory,
                project_cache_key: OnceLock::new(),
            },
            CacheType::Directory => Self::Directory {
                project_directory,
                shared_root,
            },
        })
    }

    /// The project's cache key, resolved (and, the first time, persisted) by
    /// the first call that needs it and reused after, so one invocation
    /// reads the key file at most once — its listing and its removal agree
    /// on one key. Called only for project caches: a shared-scope invocation
    /// never reads, creates or depends on the key file (ratect#697).
    fn project_cache_key<'k>(
        project_directory: &Path,
        resolved: &'k OnceLock<String>,
    ) -> Result<&'k str> {
        if let Some(key) = resolved.get() {
            return Ok(key);
        }
        let key = project_cache_key(project_directory)?;
        Ok(resolved.get_or_init(|| key))
    }

    /// Every cache this project can see, split by ownership and narrowed by
    /// `scope`.
    ///
    /// Both halves are read from *storage*, never from a configuration
    /// file — a cache belongs to the project *directory*, so this works on a
    /// project whose config doesn't parse, or isn't there at all. A project
    /// cache is found by its `batect-cache-<key>-` prefix, a shared one by
    /// `ratect-shared-cache-`.
    pub async fn list(&self, scope: Option<CacheScope>) -> Result<CacheSelection> {
        let (mut owned, mut shared) = (Vec::new(), Vec::new());
        match self {
            Self::Volume {
                docker,
                project_directory,
                project_cache_key,
            } => {
                let key = match scope {
                    Some(CacheScope::Shared) => None,
                    _ => Some(Self::project_cache_key(
                        project_directory,
                        project_cache_key,
                    )?),
                };
                for (name, found_scope) in list_all_volume_caches(*docker, key).await? {
                    match found_scope {
                        CacheScope::Project => owned.push(name),
                        CacheScope::Shared => shared.push(name),
                    }
                }
            }
            Self::Directory {
                project_directory,
                shared_root,
            } => {
                owned = list_directory_caches(project_directory)?;
                shared = match shared_root {
                    Some(root) => matching_cache_directories(root, &HashSet::new())?,
                    None => Vec::new(),
                };
            }
        }

        // `scope` narrows what the invocation is working with, so every
        // question below is answered against the narrowed set rather than
        // each site remembering to re-apply it.
        if scope == Some(CacheScope::Shared) {
            owned.clear();
        }
        if scope == Some(CacheScope::Project) {
            shared.clear();
        }
        owned.sort();
        shared.sort();
        Ok(CacheSelection {
            owned,
            shared,
            scope,
        })
    }

    /// Removes the caches `only` names (or, if empty, every one of this
    /// project's own) from `found` — the [`CacheSelection`] a prior
    /// [`list`](Self::list) call with the same scope already produced, so
    /// this never re-lists what the caller already has.
    ///
    /// Refuses (rather than removing) a shared cache reached without
    /// `--scope shared` — see [`CacheRefusal`] — checked against `found`
    /// exactly as narrowed: an explicit `--scope project` clears `found`'s
    /// shared half, so a same-named shared cache is correctly not flagged as
    /// ambiguous when the caller has already said which one they mean.
    ///
    /// `warn` receives each warning about what a project-scoped directory
    /// clean passed over (see `clean_directory_caches`) — a callback, as
    /// [`crate::docker::DockerClient::new`]'s is, since how a warning is
    /// printed is each binary's own. [`list`](Self::list) has none: only
    /// cleaning skips anything, so a list-then-remove run warns once.
    pub async fn remove(
        &self,
        found: &CacheSelection,
        only: &HashSet<String>,
        warn: impl Fn(String),
    ) -> Result<Result<Vec<RemovedCache>, CacheRefusal>> {
        if only.is_empty() && found.scope == Some(CacheScope::Shared) {
            return Ok(Err(CacheRefusal::SharedSweepNotNamed));
        }
        if found.scope != Some(CacheScope::Shared) {
            if let Some(name) = found.shared_only(only).first() {
                return Ok(Err(CacheRefusal::SharedNotNamed((*name).clone())));
            }
        }

        let mut removed = Vec::new();
        if found.covers(CacheScope::Project) {
            removed.extend(self.remove_project(only, warn).await?);
        }
        if found.covers(CacheScope::Shared) {
            removed.extend(self.remove_shared(only).await?);
        }
        Ok(Ok(removed))
    }

    async fn remove_project(
        &self,
        only: &HashSet<String>,
        warn: impl Fn(String),
    ) -> Result<Vec<RemovedCache>> {
        Ok(match self {
            Self::Volume {
                docker,
                project_directory,
                project_cache_key,
            } => {
                let key = Self::project_cache_key(project_directory, project_cache_key)?;
                clean_volume_caches(*docker, key, only)
                    .await?
                    .into_iter()
                    .map(|name| {
                        let storage = cache_volume_name(key, &name);
                        RemovedCache { name, storage }
                    })
                    .collect()
            }
            Self::Directory {
                project_directory, ..
            } => clean_directory_caches(project_directory, only, warn)?
                .into_iter()
                .map(|name| {
                    let storage = cache_directory(project_directory)
                        .join(&name)
                        .display()
                        .to_string();
                    RemovedCache { name, storage }
                })
                .collect(),
        })
    }

    async fn remove_shared(&self, only: &HashSet<String>) -> Result<Vec<RemovedCache>> {
        Ok(match self {
            Self::Volume { docker, .. } => clean_shared_volume_caches(*docker, only)
                .await?
                .into_iter()
                .map(|name| {
                    let storage = shared_cache_volume_name(&name);
                    RemovedCache { name, storage }
                })
                .collect(),
            Self::Directory { shared_root, .. } => {
                let Some(root) = shared_root else {
                    return Ok(Vec::new());
                };
                clean_shared_directory_caches(root, only)?
                    .into_iter()
                    .map(|name| {
                        let storage = root.join(&name).display().to_string();
                        RemovedCache { name, storage }
                    })
                    .collect()
            }
        })
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
