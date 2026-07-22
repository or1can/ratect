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
//! actual Docker bind-mount string — either a named volume that persists
//! between separate `ratect` invocations, or a host directory under
//! `--cache-type=directory` — and implements `--clean`/`--clean-cache`
//! ([`clean_volume_caches`]/[`clean_directory_caches`]), which remove them.
//! Ported from Batect's own `CacheManager`/`VolumeMountResolver`/`CacheType`/
//! `CleanupCachesCommand`, with one deliberate divergence: the project cache
//! key is a full UUID rather than Batect's 6-char `a-z0-9` id (see
//! [`project_cache_key`]'s own doc comment for why) — everything else,
//! including the `.batect/caches/` location and `batect-cache-` volume
//! prefix, is kept byte-for-byte compatible with Batect's own convention on
//! purpose: this is `ratect-compat`'s territory (see `ROADMAP.md`'s
//! `## Two Binaries` section), and a project migrating from real `batect`
//! should find its existing cache volumes/directories reused, not orphaned.

use crate::config::CacheVolumeMount;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

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
/// Reads `<project_directory>/.batect/caches/key` if it already exists —
/// tolerating Batect's own file format exactly (skip blank lines and any
/// line starting with `#`, take the one remaining line as the key,
/// mirroring `CacheManager.projectCacheKey`'s own read logic), so a project
/// already run under real Batect has its existing key discovered and
/// reused, preserving the exact volume names Batect itself would use.
///
/// When no file exists yet, generates and persists a new one: a full
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

    if let Ok(contents) = fs::read_to_string(&key_path) {
        if let Some(key) = contents
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
        {
            return Ok(key.to_string());
        }
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

/// The Docker volume name a `cache` mount named `name` resolves to under
/// `CacheType::Volume` — `batect-cache-<project_cache_key>-<name>`, Batect's
/// own literal prefix (see the module's own doc comment for why it isn't
/// `ratect-cache-`).
pub fn cache_volume_name(project_cache_key: &str, name: &str) -> String {
    format!("batect-cache-{project_cache_key}-{name}")
}

/// Resolves `mount` to a Docker bind-mount string (`"source:container[:options]"`,
/// the same shape `docker.rs`'s `HostConfig.binds` already expects) —
/// `source` is a bare Docker volume name under `CacheType::Volume` (Docker
/// itself auto-creates a named volume on first use), or an absolute host
/// directory under `.batect/caches/<name>/` under `CacheType::Directory`
/// (created here if missing, matching Batect's own
/// `Files.createDirectories`).
pub fn resolve_cache_mount(
    options: &CacheOptions,
    project_cache_key: &str,
    mount: &CacheVolumeMount,
) -> Result<String> {
    let source = match options.cache_type {
        CacheType::Volume => cache_volume_name(project_cache_key, &mount.name),
        CacheType::Directory => {
            let dir = cache_directory(&options.project_directory).join(&mount.name);
            fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create cache directory {dir:?}"))?;
            dir.display().to_string()
        }
    };

    Ok(match &mount.options {
        Some(mount_options) => format!("{source}:{}:{mount_options}", mount.container),
        None => format!("{source}:{}", mount.container),
    })
}

/// Filters `existing_volumes` (from [`crate::docker::ContainerRuntime::list_volumes`])
/// down to this project's own cache volumes — those with the
/// `batect-cache-<project_cache_key>-` prefix — further restricted to
/// `only` when non-empty (the `--clean-cache <name>` allowlist; empty means
/// "every one of this project's cache volumes", matching plain `--clean`).
/// A pure, synchronous decision function deliberately kept separate from
/// the I/O in [`clean_volume_caches`], so it's unit-testable against plain
/// `Vec<String>` fixtures without needing a fake `ContainerRuntime`.
fn matching_cache_volumes<'a>(
    existing_volumes: &'a [String],
    project_cache_key: &str,
    only: &HashSet<String>,
) -> Vec<&'a str> {
    let prefix = cache_volume_name(project_cache_key, "");
    existing_volumes
        .iter()
        .filter_map(|name| {
            let cache_name = name.strip_prefix(&prefix)?;
            (only.is_empty() || only.contains(cache_name)).then_some(name.as_str())
        })
        .collect()
}

/// This project's own existing cache volumes, by their *cache* name (what
/// a `volumes` entry calls them) rather than the prefixed Docker volume
/// name — for `ratect caches list`, which has no equivalent in
/// `ratect-compat`/Batect (both only ever offered removal).
///
/// Knowing what's there is the prerequisite for removing one by name, so
/// this is a deliberate addition rather than a parity gap. Sorted, so
/// repeated invocations agree with each other; Docker's own volume listing
/// order isn't specified.
pub async fn list_volume_caches(
    runtime: &impl crate::docker::ContainerRuntime,
    project_cache_key: &str,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let prefix = cache_volume_name(project_cache_key, "");
    let mut names: Vec<String> =
        matching_cache_volumes(&existing, project_cache_key, &HashSet::new())
            .into_iter()
            .map(|volume| volume.strip_prefix(&prefix).unwrap_or(volume).to_string())
            .collect();
    names.sort();
    Ok(names)
}

/// The `CacheType::Directory` counterpart of [`list_volume_caches`] —
/// already sorted, by [`matching_cache_directories`].
pub fn list_directory_caches(project_directory: &Path) -> Result<Vec<String>> {
    matching_cache_directories(&cache_directory(project_directory), &HashSet::new())
}

/// Removes this project's own cache volumes (or, with `only` non-empty,
/// just the named ones) — `--clean`/`--clean-cache` under
/// `CacheType::Volume`. Mirrors Batect's own `CleanupCachesCommand.runForVolumes`.
/// Returns the names actually removed.
pub async fn clean_volume_caches(
    runtime: &impl crate::docker::ContainerRuntime,
    project_cache_key: &str,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let existing = runtime.list_volumes().await?;
    let matched: Vec<String> = matching_cache_volumes(&existing, project_cache_key, only)
        .into_iter()
        .map(str::to_string)
        .collect();

    for name in &matched {
        runtime.remove_volume(name).await?;
    }

    Ok(matched)
}

/// The synchronous counterpart of [`matching_cache_volumes`] for
/// `CacheType::Directory`: this project's own cache directories are exactly
/// [`cache_directory`]'s own subdirectories (the `key` file living
/// alongside them is a plain file, not a directory, so it's never matched
/// here) — restricted to `only` when non-empty, same convention as above.
fn matching_cache_directories(cache_dir: &Path, only: &HashSet<String>) -> Result<Vec<String>> {
    if !cache_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut matched = Vec::new();
    for entry in fs::read_dir(cache_dir).with_context(|| format!("Failed to read {cache_dir:?}"))? {
        let entry = entry.with_context(|| format!("Failed to read an entry in {cache_dir:?}"))?;
        if !entry
            .file_type()
            .with_context(|| format!("Failed to inspect {:?}", entry.path()))?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if only.is_empty() || only.contains(&name) {
            matched.push(name);
        }
    }
    matched.sort();

    Ok(matched)
}

/// Removes this project's own cache directories (or, with `only` non-empty,
/// just the named ones) — `--clean`/`--clean-cache` under
/// `CacheType::Directory`. Mirrors Batect's own
/// `CleanupCachesCommand.runForDirectories`. Returns the names actually
/// removed.
pub fn clean_directory_caches(
    project_directory: &Path,
    only: &HashSet<String>,
) -> Result<Vec<String>> {
    let cache_dir = cache_directory(project_directory);
    let matched = matching_cache_directories(&cache_dir, only)?;

    for name in &matched {
        let dir = cache_dir.join(name);
        fs::remove_dir_all(&dir).with_context(|| format!("Failed to remove {dir:?}"))?;
    }

    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ratect-cache-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn project_cache_key_generates_and_persists_a_key() {
        let dir = unique_temp_dir();

        let first = project_cache_key(&dir).unwrap();
        let second = project_cache_key(&dir).unwrap();

        assert_eq!(first, second);
        assert!(cache_directory(&dir).join("key").exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn project_cache_key_reads_an_existing_batect_style_key_file() {
        let dir = unique_temp_dir();
        let caches_dir = cache_directory(&dir);
        fs::create_dir_all(&caches_dir).unwrap();
        fs::write(
            caches_dir.join("key"),
            "# This file was autogenerated by Batect to track which Docker volumes are \
             associated with this project.\n# Do not modify it, and do not commit it to source \
             control.\nabc123\n",
        )
        .unwrap();

        let key = project_cache_key(&dir).unwrap();

        assert_eq!(key, "abc123");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cache_volume_name_uses_the_batect_cache_prefix() {
        assert_eq!(
            cache_volume_name("abc123", "gradle-cache"),
            "batect-cache-abc123-gradle-cache"
        );
    }

    #[test]
    fn resolve_cache_mount_builds_a_volume_bind_string() {
        let dir = unique_temp_dir();
        let options = CacheOptions {
            cache_type: CacheType::Volume,
            project_directory: dir.clone(),
        };
        let mount = CacheVolumeMount {
            name: "gradle-cache".to_string(),
            container: "/root/.gradle".to_string(),
            options: None,
        };

        let resolved = resolve_cache_mount(&options, "abc123", &mount).unwrap();

        assert_eq!(resolved, "batect-cache-abc123-gradle-cache:/root/.gradle");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_cache_mount_includes_options_when_given() {
        let dir = unique_temp_dir();
        let options = CacheOptions {
            cache_type: CacheType::Volume,
            project_directory: dir.clone(),
        };
        let mount = CacheVolumeMount {
            name: "gradle-cache".to_string(),
            container: "/root/.gradle".to_string(),
            options: Some("ro".to_string()),
        };

        let resolved = resolve_cache_mount(&options, "abc123", &mount).unwrap();

        assert_eq!(
            resolved,
            "batect-cache-abc123-gradle-cache:/root/.gradle:ro"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_cache_mount_creates_and_uses_a_host_directory_for_directory_type() {
        let dir = unique_temp_dir();
        let options = CacheOptions {
            cache_type: CacheType::Directory,
            project_directory: dir.clone(),
        };
        let mount = CacheVolumeMount {
            name: "gradle-cache".to_string(),
            container: "/root/.gradle".to_string(),
            options: None,
        };

        let resolved = resolve_cache_mount(&options, "abc123", &mount).unwrap();

        let expected_dir = cache_directory(&dir).join("gradle-cache");
        assert_eq!(
            resolved,
            format!("{}:/root/.gradle", expected_dir.display())
        );
        assert!(expected_dir.is_dir());

        fs::remove_dir_all(&dir).unwrap();
    }

    // `clean_volume_caches`'s own async glue (list, filter, remove each) is
    // thin enough to be covered by the real end-to-end Docker test in
    // `tests/cli.rs` instead of a dedicated fake `ContainerRuntime` here —
    // the interesting decision logic is `matching_cache_volumes`, a plain
    // synchronous function these tests exercise directly.

    #[test]
    fn matching_cache_volumes_matches_only_this_projects_prefix() {
        let existing = vec![
            "batect-cache-abc123-gradle-cache".to_string(),
            "batect-cache-different-key-gradle-cache".to_string(),
            "some-unrelated-volume".to_string(),
        ];

        let matched = matching_cache_volumes(&existing, "abc123", &HashSet::new());

        assert_eq!(matched, vec!["batect-cache-abc123-gradle-cache"]);
    }

    #[test]
    fn matching_cache_volumes_restricts_to_the_only_set_when_given() {
        let existing = vec![
            "batect-cache-abc123-gradle-cache".to_string(),
            "batect-cache-abc123-npm-cache".to_string(),
        ];
        let only = HashSet::from(["npm-cache".to_string()]);

        let matched = matching_cache_volumes(&existing, "abc123", &only);

        assert_eq!(matched, vec!["batect-cache-abc123-npm-cache"]);
    }

    #[test]
    fn matching_cache_volumes_is_empty_when_nothing_matches_the_prefix() {
        let existing = vec!["some-unrelated-volume".to_string()];

        let matched = matching_cache_volumes(&existing, "abc123", &HashSet::new());

        assert!(matched.is_empty());
    }

    /// `ratect caches list` reports what a `volumes` entry calls a cache,
    /// not the prefixed Docker volume name it happens to be stored under —
    /// otherwise nothing it prints could be pasted back into `caches clean`.
    #[test]
    fn list_directory_caches_reports_cache_names_without_the_key_file() {
        let dir = unique_temp_dir();
        let caches_dir = cache_directory(&dir);
        fs::create_dir_all(caches_dir.join("npm-cache")).unwrap();
        fs::create_dir_all(caches_dir.join("gradle-cache")).unwrap();
        fs::write(caches_dir.join("key"), "abc123\n").unwrap();

        assert_eq!(
            list_directory_caches(&dir).unwrap(),
            vec!["gradle-cache", "npm-cache"]
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_directory_caches_is_empty_for_a_project_with_no_caches() {
        let dir = unique_temp_dir();
        assert!(list_directory_caches(&dir).unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_directory_caches_removes_matching_subdirectories_and_leaves_others() {
        let dir = unique_temp_dir();
        let caches_dir = cache_directory(&dir);
        fs::create_dir_all(caches_dir.join("gradle-cache")).unwrap();
        fs::create_dir_all(caches_dir.join("npm-cache")).unwrap();
        fs::write(caches_dir.join("key"), "abc123\n").unwrap();

        let removed = clean_directory_caches(&dir, &HashSet::new()).unwrap();

        let mut removed = removed;
        removed.sort();
        assert_eq!(removed, vec!["gradle-cache", "npm-cache"]);
        assert!(!caches_dir.join("gradle-cache").exists());
        assert!(!caches_dir.join("npm-cache").exists());
        // The key file is a plain file, not a directory — never matched or
        // removed as if it were a cache.
        assert!(caches_dir.join("key").exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_directory_caches_restricts_to_only_when_given() {
        let dir = unique_temp_dir();
        let caches_dir = cache_directory(&dir);
        fs::create_dir_all(caches_dir.join("gradle-cache")).unwrap();
        fs::create_dir_all(caches_dir.join("npm-cache")).unwrap();

        let only = HashSet::from(["npm-cache".to_string()]);
        let removed = clean_directory_caches(&dir, &only).unwrap();

        assert_eq!(removed, vec!["npm-cache"]);
        assert!(caches_dir.join("gradle-cache").is_dir());
        assert!(!caches_dir.join("npm-cache").exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_directory_caches_does_nothing_when_the_cache_directory_does_not_exist() {
        let dir = unique_temp_dir();

        let removed = clean_directory_caches(&dir, &HashSet::new()).unwrap();

        assert!(removed.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }
}
