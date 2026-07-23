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

//! Git includes (`type: git` entries in a config file's top-level
//! `include`) — cloning a repository/ref once into a local cache and
//! reusing it forever, matching the design validated against Batect's own
//! `libs/git-client`/`app/.../config/includes` (see ROADMAP.md's 0.8.0
//! entry for the full rationale). [`config.rs`](crate::config) drives this
//! module; nothing here knows about `batect.yml` parsing.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::Instant;

/// Shells out to the system `git` binary, so tests can inject a fake
/// instead of needing a real network and a real `git` — same shape as
/// `docker.rs`'s `ContainerRuntime`.
#[async_trait::async_trait]
pub trait GitClient: Send + Sync {
    /// Clones `remote` at `ref`, checked out (including submodules) at
    /// `destination`. `destination` is guaranteed not to exist yet — the
    /// caller (`GitIncludeCache`) only calls this once, under its own lock,
    /// after confirming that.
    async fn clone_repo(&self, remote: &str, git_ref: &str, destination: &Path) -> Result<()>;
}

/// The real `GitClient`: `git clone --quiet --no-checkout` into a sibling
/// temporary directory, `git checkout --recurse-submodules <ref>`, then an
/// atomic rename into `destination` — matching Batect's own `GitClient`
/// exactly (no embedded Git library, kept dependency-light).
pub struct SystemGitClient;

#[async_trait::async_trait]
impl GitClient for SystemGitClient {
    async fn clone_repo(&self, remote: &str, git_ref: &str, destination: &Path) -> Result<()> {
        // Defense against argv flag smuggling: a `repo`/`ref` from a config
        // file (possibly itself from a git-included bundle) that starts
        // with `-` could otherwise be parsed as a git flag rather than a
        // positional argument. `clone` below also has a `--` separator
        // before `remote`; `checkout` can't safely use one (see the comment
        // there), so this check is what protects `git_ref` there.
        if remote.starts_with('-') {
            anyhow::bail!("Git include 'repo' must not start with '-': '{remote}'");
        }
        if git_ref.starts_with('-') {
            anyhow::bail!("Git include 'ref' must not start with '-': '{git_ref}'");
        }

        let parent = destination
            .parent()
            .context("Git include cache destination has no parent directory")?;
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create directory {parent:?}"))?;

        let temp_dir = parent.join(format!(
            "{}.tmp",
            destination
                .file_name()
                .context("Git include cache destination has no file name")?
                .to_string_lossy()
        ));
        if temp_dir.exists() {
            tokio::fs::remove_dir_all(&temp_dir)
                .await
                .with_context(|| format!("Failed to clean up stale directory {temp_dir:?}"))?;
        }

        let clone_output = Command::new("git")
            // Restricts which transports git will honor for this remote —
            // without this, a `repo` of the form `ext::sh -c ...` is (by
            // default, since it's given directly on the command line
            // rather than embedded in fetched content) trusted at git's
            // "user" level and would execute arbitrary shell commands.
            // `remote` ultimately comes from a config file, possibly
            // itself from a git-included bundle, so it's not fully
            // trusted input. `file` stays allowed here (unlike the
            // checkout step below): a local-path `repo` is a documented,
            // supported feature (see docs/config-reference.md's `repo`
            // field), and it's the caller's own config value, not
            // third-party content the way a submodule URL is.
            .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh")
            .args(["clone", "--quiet", "--no-checkout", "--", remote])
            .arg(&temp_dir)
            .output()
            .await
            .context("Failed to run 'git clone' — is git installed and on PATH?")?;
        if !clone_output.status.success() {
            anyhow::bail!(
                "Could not clone repository '{remote}': git exited with {}: {}",
                clone_output.status,
                String::from_utf8_lossy(&clone_output.stderr).trim()
            );
        }

        let checkout_output = Command::new("git")
            // `file` is deliberately *not* in this list, unlike the clone
            // step above: `--recurse-submodules` fetches whatever
            // submodule URLs the checked-out ref's own `.gitmodules`
            // declares, and that ref may itself have come from an
            // untrusted git-included bundle — a `file://` submodule URL
            // would otherwise let such a bundle pull an arbitrary sibling
            // local repository on the host running `ratect` into its own
            // clone. Unlike `remote` above, a submodule URL is never the
            // caller's own config value, so there's no local-path use
            // case to preserve here.
            .env("GIT_ALLOW_PROTOCOL", "git:http:https:ssh")
            .args(["-c", "advice.detachedHead=false", "-C"])
            .arg(&temp_dir)
            // No `--` here: unlike `clone`, `checkout`'s `--` means "the
            // rest are pathspecs, not a ref" — adding one would break every
            // checkout (verified: `git checkout <ref> --` errors with
            // "pathspec did not match any files"). The `git_ref.starts_with('-')`
            // check above is what protects this call instead.
            .args(["checkout", "--quiet", "--recurse-submodules", git_ref])
            .output()
            .await
            .context("Failed to run 'git checkout'")?;
        if !checkout_output.status.success() {
            anyhow::bail!(
                "Could not check out reference '{git_ref}' for repository '{remote}': git exited with {}: {}",
                checkout_output.status,
                String::from_utf8_lossy(&checkout_output.stderr).trim()
            );
        }

        tokio::fs::rename(&temp_dir, destination)
            .await
            .with_context(|| {
                format!("Failed to move {temp_dir:?} into place at {destination:?}")
            })?;

        Ok(())
    }
}

/// The `<hash>.toml` sidecar written alongside each cached clone — see
/// `GitIncludeCache::update_info_file`. `last_used` is a Unix timestamp
/// (seconds), not `atime`/`mtime` (unreliable across platforms, especially
/// CI) and not a full RFC3339 string (no consumer needs one yet, and it
/// keeps this module dependency-free of a date/time crate).
#[derive(Debug, Serialize, Deserialize)]
struct CacheInfo {
    #[serde(rename = "type")]
    kind: String,
    repo: CacheInfoRepo,
    cloned_with_version: String,
    last_used: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheInfoRepo {
    remote: String,
    #[serde(rename = "ref")]
    git_ref: String,
}

/// One entry in the Git include cache — what `ratect includes list`
/// reports, and what `clean`/`refresh` return as having acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedInclude {
    /// The hashed directory name under `~/.ratect/incl`. Not meaningful to
    /// a user, but it's what identifies the entry on disk.
    pub key: String,
    pub remote: String,
    pub git_ref: String,
    /// Seconds since the Unix epoch, from the entry's own sidecar — bumped
    /// on every use, so this is "when a task last needed it", not when it
    /// was cloned.
    pub last_used: u64,
    /// The working copy's own directory.
    pub path: PathBuf,
    /// Bytes on disk. Only populated by [`GitIncludeCache::list`], which is
    /// the only caller that needs it; zero elsewhere rather than paying for
    /// a directory walk nothing reads.
    pub size_bytes: u64,
}

/// Removes an entry's working copy and sidecar, tolerating either being
/// absent already. `false` if something is left behind, logged.
async fn remove_entry_files(working_copy: &Path, info_path: &Path) -> bool {
    if working_copy.exists() {
        if let Err(e) = tokio::fs::remove_dir_all(working_copy).await {
            tracing::warn!("Failed to remove Git include cache clone {working_copy:?}: {e}");
            return false;
        }
    }
    if let Err(e) = tokio::fs::remove_file(info_path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("Failed to remove Git include cache info file {info_path:?}: {e}");
            return false;
        }
    }
    true
}

/// Bytes on disk under `path`, following no symlinks and giving up quietly
/// on anything unreadable — a size is worth reporting approximately rather
/// than not at all. Synchronous: [`GitIncludeCache::list`] runs it on a
/// blocking thread, one per entry.
fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// The clock `GitIncludeCache` reads `last_used` from — boxed so the real
/// `SystemTime::now`-backed closure and a fixed test closure share one
/// field type, same idiom as `engine.rs`'s `HostEnv`.
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

/// How long an entry may go unused before [`GitIncludeCache::cleanup_stale`]
/// removes it — matches Batect's own `GitRepositoryCacheCleanupTask`
/// exactly (a fixed 30 days, not configurable in Batect either).
const STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

fn real_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A cache key stable for a given `(remote, ref)` pair — deliberately
/// collision-resistant (SHA-256) rather than anything reversible, since it
/// only needs to be a good directory name, not human-readable (that's what
/// the `.toml` sidecar's own `repo` field is for).
///
/// Each field is length-prefixed before being fed to the hasher, rather
/// than joined with a free-text separator (`format!("git {remote}
/// @{git_ref}")`, this function's own pre-0.10.0 implementation) — with a
/// bare separator, `remote`/`git_ref` pairs that themselves contain that
/// separator can collide: `("repo.git @evil-ref", "main")` and
/// `("repo.git", "evil-ref @main")` would otherwise hash identically.
/// `remote`/`git_ref` come straight from config (a project's own, or one
/// reached transitively through a Git-included bundle) with no restriction
/// on their content beyond rejecting a leading `-`, and the cache they key
/// into (`~/.ratect/incl`) is shared, clone-once-forever, across every
/// project on the machine — a collision would let one project's include
/// silently reuse another, unrelated project's cached clone. Length-
/// prefixing makes the two fields unambiguously separable regardless of
/// what characters they contain.
pub(crate) fn cache_key(remote: &str, git_ref: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(remote.len().to_le_bytes());
    hasher.update(remote.as_bytes());
    hasher.update(git_ref.len().to_le_bytes());
    hasher.update(git_ref.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Clones-once-and-reuses-forever cache for Git includes, rooted at
/// `~/.ratect/incl` in production (`GitIncludeCache::new`) — see
/// ROADMAP.md's 0.8.0 entry. A repo/ref already present on disk (by cache
/// key) is never re-fetched; this is why users are expected to pin
/// immutable tags/refs, not a corner being cut here.
///
/// Guards the clone step with a per-cache-entry lock file (create-exclusive,
/// polled, with a timeout), so concurrent `ratect` invocations targeting the
/// same repo/ref are safe — matching Batect's own `LockingRepositoryCloner`.
/// Where a `GitIncludeCache`'s cache directory lives. `Home` defers the
/// actual `~` lookup to first use (`ensure_cached`) rather than resolving it
/// in `GitIncludeCache::new` — so constructing a `GitIncludeCache` up front
/// (as `Config::load_from_file` does, since it doesn't know yet whether the
/// file it's about to parse even has a `type: git` include) can't fail for a
/// config that turns out not to use one. Same "only pay for it if you use
/// it" precedent as `crate::user::current_user`, called only when
/// `run_as_current_user` is actually enabled.
enum CacheRoot {
    #[cfg(test)]
    Fixed(PathBuf),
    Home,
}

impl CacheRoot {
    fn resolve(&self) -> Result<PathBuf> {
        match self {
            #[cfg(test)]
            CacheRoot::Fixed(path) => Ok(path.clone()),
            CacheRoot::Home => Ok(crate::user::home_directory()?.join(".ratect").join("incl")),
        }
    }
}

pub struct GitIncludeCache<G: GitClient> {
    root: CacheRoot,
    git: G,
    clock: Clock,
    version: String,
    lock_timeout: Duration,
}

impl GitIncludeCache<SystemGitClient> {
    /// The production cache: rooted at `~/.ratect/incl`, backed by the real
    /// `git` binary.
    pub fn new() -> Self {
        Self {
            root: CacheRoot::Home,
            git: SystemGitClient,
            clock: Box::new(real_now),
            version: env!("CARGO_PKG_VERSION").to_string(),
            lock_timeout: Duration::from_secs(5 * 60),
        }
    }
}

impl Default for GitIncludeCache<SystemGitClient> {
    fn default() -> Self {
        Self::new()
    }
}

impl<G: GitClient> GitIncludeCache<G> {
    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf, git: G, now: u64) -> Self {
        Self {
            root: CacheRoot::Fixed(root),
            git,
            clock: Box::new(move || now),
            version: "0.0.0-test".to_string(),
            lock_timeout: Duration::from_secs(5),
        }
    }

    /// Ensures `remote` at `ref` is cloned into this cache, returning the
    /// clone's directory. Safe to call repeatedly (across processes, or
    /// concurrently) for the same `(remote, ref)` — later calls are no-ops
    /// beyond bumping `last_used`.
    pub async fn ensure_cached(&self, remote: &str, git_ref: &str) -> Result<PathBuf> {
        let root = self.root.resolve()?;
        tokio::fs::create_dir_all(&root)
            .await
            .with_context(|| format!("Failed to create Git include cache directory {root:?}"))?;

        let key = cache_key(remote, git_ref);
        let working_copy = root.join(&key);
        let info_path = root.join(format!("{key}.toml"));
        let lock_path = root.join(format!("{key}.lock"));

        self.acquire_lock(&lock_path).await?;
        let clone_result = self.clone_if_missing(remote, git_ref, &working_copy).await;
        self.release_lock(&lock_path).await;
        clone_result?;

        self.update_info_file(remote, git_ref, &info_path).await?;

        Ok(working_copy)
    }

    async fn clone_if_missing(
        &self,
        remote: &str,
        git_ref: &str,
        destination: &Path,
    ) -> Result<()> {
        if destination.exists() {
            return Ok(());
        }
        self.git.clone_repo(remote, git_ref, destination).await
    }

    async fn acquire_lock(&self, lock_path: &Path) -> Result<()> {
        let start = Instant::now();
        loop {
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_path)
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() > self.lock_timeout {
                        anyhow::bail!(
                            "Timed out after {:?} waiting for lock file {:?} — another process may \
                             be cloning the same repository.",
                            self.lock_timeout,
                            lock_path
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("Failed to create lock file {lock_path:?}"))
                }
            }
        }
    }

    /// Best-effort: a failure here just leaves a stale lock file behind,
    /// which only affects the next caller's own timeout, not correctness.
    async fn release_lock(&self, lock_path: &Path) {
        if let Err(e) = tokio::fs::remove_file(lock_path).await {
            tracing::warn!("Failed to remove Git include lock file {lock_path:?}: {e}");
        }
    }

    /// Writes/updates the `<hash>.toml` sidecar — `type`/`repo`/
    /// `cloned_with_version` are preserved from any existing file (so a
    /// later `ratect` version reusing an old clone doesn't overwrite the
    /// version it was actually cloned with), only `last_used` is bumped.
    /// Written via write-to-temp-then-atomic-rename, so a concurrent reader
    /// (`listAll`-style tooling, not implemented yet — see ROADMAP.md) can
    /// never observe a torn file; a concurrent `last_used` bump can still be
    /// lost to a last-write-wins race, same as Batect accepts.
    async fn update_info_file(&self, remote: &str, git_ref: &str, info_path: &Path) -> Result<()> {
        let mut info = match tokio::fs::read_to_string(info_path).await {
            Ok(content) => toml::from_str(&content).with_context(|| {
                format!("Failed to parse Git include cache info file {info_path:?}")
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => CacheInfo {
                kind: "git".to_string(),
                repo: CacheInfoRepo {
                    remote: remote.to_string(),
                    git_ref: git_ref.to_string(),
                },
                cloned_with_version: self.version.clone(),
                last_used: 0,
            },
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to read Git include cache info file {info_path:?}")
                })
            }
        };
        info.last_used = (self.clock)();

        let content =
            toml::to_string_pretty(&info).context("Failed to serialize Git include cache info")?;
        let temp_path = info_path.with_extension("toml.tmp");
        tokio::fs::write(&temp_path, content)
            .await
            .with_context(|| format!("Failed to write {temp_path:?}"))?;
        tokio::fs::rename(&temp_path, info_path)
            .await
            .with_context(|| format!("Failed to finalize {info_path:?}"))?;

        Ok(())
    }

    /// Removes any cached repo whose `last_used` is more than
    /// [`STALE_AFTER`] old — matching Batect's own
    /// `GitRepositoryCacheCleanupTask`/`GitRepositoryCache.delete` exactly.
    /// Meant to be started unconditionally, once per invocation, as a
    /// detached background task (see `main.rs`) — never awaited, so a
    /// failure here is only ever logged. Each stale entry is removed
    /// independently: one entry's removal failing (its `.toml` sidecar
    /// unreadable/unparsable, or a filesystem error) is logged and skipped
    /// rather than aborting the whole sweep, same as Batect's own per-entry
    /// try/catch.
    /// Every entry currently in the cache — what `ratect includes list`
    /// reports.
    ///
    /// `size_bytes` is measured by walking each working copy, concurrently
    /// across entries: a bundle-sized clone (a few megabytes, ~1,000 files)
    /// walks in about 10ms, so the whole cache costs roughly the slowest
    /// one rather than their sum. Entries are sorted by `last_used`,
    /// oldest first — the order someone clearing space wants to read.
    ///
    /// An unreadable or unparsable sidecar is logged and skipped rather
    /// than failing the listing, the same per-entry tolerance
    /// [`cleanup_stale`](Self::cleanup_stale) has: one corrupt file
    /// shouldn't make the whole cache unreportable.
    pub async fn list(&self) -> Result<Vec<CachedInclude>> {
        let root = self.root.resolve()?;
        let mut entries = self.read_entries(&root).await?;
        entries.sort_by_key(|entry| entry.last_used);

        let sizes = futures::future::join_all(entries.iter().map(|entry| {
            let path = entry.path.clone();
            tokio::task::spawn_blocking(move || directory_size(&path))
        }))
        .await;
        for (entry, size) in entries.iter_mut().zip(sizes) {
            entry.size_bytes = size.unwrap_or(0);
        }

        Ok(entries)
    }

    /// Removes cached entries, returning the ones actually removed.
    ///
    /// `minimum_age` of `None` removes everything (`ratect includes clean
    /// --all`); `Some` removes only entries unused for at least that long,
    /// which is what both the automatic sweep and a bare `includes clean`
    /// do. Nothing here is unrecoverable — the worst case of removing too
    /// much is a re-clone — which is why this has no confirmation of any
    /// kind, unlike removing containers.
    pub async fn clean(&self, minimum_age: Option<Duration>) -> Result<Vec<CachedInclude>> {
        let root = self.root.resolve()?;
        let now = (self.clock)();
        let entries = self.read_entries(&root).await?;

        let mut removed = Vec::new();
        for entry in entries {
            let old_enough = match minimum_age {
                Some(age) => now.saturating_sub(entry.last_used) > age.as_secs(),
                None => true,
            };
            if !old_enough {
                continue;
            }
            if self.remove_entry(&root, &entry.key).await {
                removed.push(entry);
            }
        }

        Ok(removed)
    }

    /// Discards every cached working copy and clones it again from the
    /// `(remote, ref)` its own sidecar records — `ratect includes refresh`.
    ///
    /// This is the only way to pick up a moved `ref`. A cached pair is
    /// otherwise frozen for good, since
    /// [`ensure_cached`](Self::ensure_cached) only clones when the working
    /// copy is missing, and the staleness sweep never helps because an
    /// include in active use never goes unused long enough to be swept.
    ///
    /// A clone that fails leaves that entry removed rather than restoring
    /// it: the next `ensure_cached` will clone it again, and pretending a
    /// failed refresh succeeded would be worse than an entry that has to be
    /// re-fetched.
    pub async fn refresh(&self) -> Result<Vec<CachedInclude>> {
        let root = self.root.resolve()?;
        let entries = self.read_entries(&root).await?;

        let mut refreshed = Vec::new();
        for entry in entries {
            if !self.remove_entry(&root, &entry.key).await {
                continue;
            }
            match self.ensure_cached(&entry.remote, &entry.git_ref).await {
                Ok(_) => refreshed.push(entry),
                Err(e) => tracing::warn!(
                    "Failed to re-clone {} at {}: {e:#}",
                    entry.remote,
                    entry.git_ref
                ),
            }
        }

        Ok(refreshed)
    }

    /// Every sidecar in `root`, parsed. Shared by
    /// [`list`](Self::list)/[`clean`](Self::clean)/[`refresh`](Self::refresh)
    /// and the staleness sweep, so they can't disagree about what an entry
    /// is or which files make one up.
    async fn read_entries(&self, root: &Path) -> Result<Vec<CachedInclude>> {
        let mut entries = match tokio::fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("Failed to list {root:?}")),
        };

        let mut found = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("Failed to list {root:?}"))?
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }

            let content = match tokio::fs::read_to_string(&path).await {
                Ok(content) => content,
                Err(e) => {
                    tracing::warn!("Failed to read Git include cache info file {path:?}: {e}");
                    continue;
                }
            };
            let info: CacheInfo = match toml::from_str(&content) {
                Ok(info) => info,
                Err(e) => {
                    tracing::warn!("Failed to parse Git include cache info file {path:?}: {e}");
                    continue;
                }
            };
            let Some(key) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };

            found.push(CachedInclude {
                key: key.to_string(),
                remote: info.repo.remote,
                git_ref: info.repo.git_ref,
                last_used: info.last_used,
                path: root.join(key),
                size_bytes: 0,
            });
        }

        Ok(found)
    }

    /// Removes one entry's working copy and sidecar, under the same
    /// per-entry lock `ensure_cached` takes — without it, this can delete a
    /// directory another `ratect` process is cloning into or reading from.
    /// `false` if anything went wrong, already logged; a single unremovable
    /// entry shouldn't abandon the rest.
    async fn remove_entry(&self, root: &Path, key: &str) -> bool {
        let working_copy = root.join(key);
        let info_path = root.join(format!("{key}.toml"));
        let lock_path = root.join(format!("{key}.lock"));

        if let Err(e) = self.acquire_lock(&lock_path).await {
            tracing::warn!("Failed to lock Git include cache entry {key}: {e:#}");
            return false;
        }
        let removed = remove_entry_files(&working_copy, &info_path).await;
        self.release_lock(&lock_path).await;
        removed
    }

    pub async fn cleanup_stale(&self) -> Result<()> {
        let root = self.root.resolve()?;
        let now = (self.clock)();
        let mut stale_keys = Vec::new();
        for entry in self.read_entries(&root).await? {
            if now.saturating_sub(entry.last_used) > STALE_AFTER.as_secs() {
                stale_keys.push(entry.key);
            }
        }

        for key in stale_keys {
            self.remove_entry(&root, &key).await;
        }

        Ok(())
    }
}

/// A `GitClient` fake for tests: `clone_repo` writes pre-configured file
/// contents into `destination` instead of touching the network or a real
/// `git` binary, matching `engine.rs`'s `FakeContainerRuntime` pattern.
/// `pub(crate)` (not module-private) so `config.rs`'s own tests, which drive
/// git includes end-to-end through `Config::load_from_file_with_git_cache`,
/// can use it too.
#[cfg(test)]
type FakeGitClientResponses = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<(String, String), std::collections::HashMap<String, String>>,
    >,
>;

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct FakeGitClient {
    responses: FakeGitClientResponses,
    fail: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    clone_calls: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

#[cfg(test)]
impl FakeGitClient {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers what `clone_repo(remote, git_ref, ...)` should write:
    /// `files` maps a relative path within the clone to its contents.
    pub(crate) fn with_files(
        self,
        remote: &str,
        git_ref: &str,
        files: std::collections::HashMap<String, String>,
    ) -> Self {
        self.responses
            .lock()
            .unwrap()
            .insert((remote.to_string(), git_ref.to_string()), files);
        self
    }

    /// Makes every `clone_repo` call fail with `message`.
    pub(crate) fn failing(self, message: &str) -> Self {
        *self.fail.lock().unwrap() = Some(message.to_string());
        self
    }

    /// How many times `clone_repo` was actually invoked — lets tests prove
    /// a second `ensure_cached` for the same `(remote, ref)` didn't re-clone.
    pub(crate) fn clone_count(&self) -> usize {
        self.clone_calls.lock().unwrap().len()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl GitClient for FakeGitClient {
    async fn clone_repo(&self, remote: &str, git_ref: &str, destination: &Path) -> Result<()> {
        self.clone_calls
            .lock()
            .unwrap()
            .push((remote.to_string(), git_ref.to_string()));

        if let Some(message) = self.fail.lock().unwrap().clone() {
            anyhow::bail!(message);
        }

        let files = self
            .responses
            .lock()
            .unwrap()
            .get(&(remote.to_string(), git_ref.to_string()))
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("FakeGitClient: no response configured for '{remote}'@'{git_ref}'")
            })?;

        tokio::fs::create_dir_all(destination).await?;
        for (relative_path, content) in files {
            let path = destination.join(&relative_path);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, content).await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn unique_temp_dir() -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let dir = std::env::temp_dir().join(format!(
            "ratect-git-include-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            count
        ));
        dir
    }

    /// A real local Git repository (no network involved — the "remote" is
    /// just another directory on disk, which `git clone` treats the same
    /// way) with one commit tagged `v1.0.0`, containing `file.txt`.
    /// Exercises `SystemGitClient` against the real `git` binary, not just
    /// `FakeGitClient` — proves the actual `clone --no-checkout` /
    /// `checkout --recurse-submodules` / atomic-rename sequence works, not
    /// just that `GitIncludeCache`'s own logic calls a `GitClient`
    /// correctly.
    fn create_test_repo() -> PathBuf {
        let repo_dir = unique_temp_dir();
        std::fs::create_dir_all(&repo_dir).unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_dir)
                .args(args)
                .status()
                .expect("git must be installed to run this test");
            assert!(status.success(), "git {args:?} failed");
        };

        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        // The host's global git config must not leak into the scratch repo's
        // commits/tags — commit.gpgsign in particular makes `git commit`
        // shell out to gpg, which fails intermittently when several tests
        // create commits in parallel (gpg-agent contention), and needlessly
        // couples the test to the host's signing setup.
        run(&["config", "commit.gpgsign", "false"]);
        run(&["config", "tag.gpgsign", "false"]);
        run(&["config", "tag.forceSignAnnotated", "false"]);
        std::fs::write(repo_dir.join("file.txt"), "hello").unwrap();
        run(&["add", "file.txt"]);
        run(&["commit", "--quiet", "-m", "initial commit"]);
        run(&["tag", "v1.0.0"]);

        repo_dir
    }

    #[tokio::test]
    async fn system_git_client_clones_and_checks_out_a_real_local_repository() {
        let repo_dir = create_test_repo();
        let destination = unique_temp_dir().join("clone");

        SystemGitClient
            .clone_repo(&repo_dir.to_string_lossy(), "v1.0.0", &destination)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(destination.join("file.txt")).unwrap(),
            "hello"
        );

        std::fs::remove_dir_all(&repo_dir).ok();
        std::fs::remove_dir_all(destination.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn system_git_client_surfaces_a_clear_error_for_an_unknown_ref() {
        let repo_dir = create_test_repo();
        let destination = unique_temp_dir().join("clone");

        let result = SystemGitClient
            .clone_repo(&repo_dir.to_string_lossy(), "does-not-exist", &destination)
            .await;

        assert!(result.is_err());
        assert!(!destination.exists());

        std::fs::remove_dir_all(&repo_dir).ok();
        std::fs::remove_dir_all(destination.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn system_git_client_rejects_a_remote_starting_with_a_dash() {
        let destination = unique_temp_dir().join("clone");

        let result = SystemGitClient
            .clone_repo("--upload-pack=touch pwned", "v1.0.0", &destination)
            .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not start with '-'"));
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn system_git_client_rejects_a_ref_starting_with_a_dash() {
        let repo_dir = create_test_repo();
        let destination = unique_temp_dir().join("clone");

        let result = SystemGitClient
            .clone_repo(
                &repo_dir.to_string_lossy(),
                "--upload-pack=touch pwned",
                &destination,
            )
            .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not start with '-'"));
        assert!(!destination.exists());

        std::fs::remove_dir_all(&repo_dir).ok();
    }

    #[tokio::test]
    async fn system_git_client_refuses_the_ext_transport() {
        // `ext::` runs an arbitrary shell command as git's "remote helper" —
        // GIT_ALLOW_PROTOCOL is what's supposed to block it. If this test
        // ever fails because the marker file *was* created, that's a
        // command-injection regression, not a flaky test.
        let marker = unique_temp_dir().join("pwned");
        let destination = unique_temp_dir().join("clone");

        let result = SystemGitClient
            .clone_repo(
                &format!("ext::sh -c touch\\ {}", marker.display()),
                "v1.0.0",
                &destination,
            )
            .await;

        assert!(result.is_err());
        assert!(!marker.exists(), "ext:: transport was not blocked");
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn system_git_client_refuses_a_file_url_submodule() {
        // A malicious bundle's `.gitmodules` can point a submodule at an
        // arbitrary local path via a `file://` URL — since
        // `--recurse-submodules` fetches whatever the checked-out ref
        // itself declares (untrusted content, unlike the top-level
        // `repo` value), this must stay blocked even though a local-path
        // top-level `repo` is itself fine (see `clone_repo`'s own
        // GIT_ALLOW_PROTOCOL for that step).
        let sibling = create_test_repo();
        let repo_dir = unique_temp_dir();
        std::fs::create_dir_all(&repo_dir).unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo_dir)
                .args(args)
                .status()
                .expect("git must be installed to run this test");
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        // Same host-signing-config isolation as `create_test_repo`.
        run(&["config", "commit.gpgsign", "false"]);
        run(&["config", "tag.gpgsign", "false"]);
        run(&["config", "tag.forceSignAnnotated", "false"]);
        run(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &format!("file://{}", sibling.display()),
            "evil",
        ]);
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "add evil submodule"]);
        run(&["tag", "v1.0.0"]);

        let destination = unique_temp_dir().join("clone");
        // Git doesn't fail the overall checkout when a submodule's
        // transport is disallowed — it silently leaves that submodule's
        // directory uninitialized instead — so `clone_repo` itself still
        // succeeds here. The security property under test is that the
        // submodule's *content* was never fetched, checked below.
        SystemGitClient
            .clone_repo(&repo_dir.to_string_lossy(), "v1.0.0", &destination)
            .await
            .unwrap();

        assert!(
            !destination.join("evil").join("file.txt").exists(),
            "the file:// submodule's content must not have been fetched"
        );

        std::fs::remove_dir_all(&repo_dir).ok();
        std::fs::remove_dir_all(&sibling).ok();
        std::fs::remove_dir_all(destination.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn ensure_cached_end_to_end_with_the_real_git_binary() {
        let repo_dir = create_test_repo();
        let cache_root = unique_temp_dir();
        let cache = GitIncludeCache::for_test(cache_root.clone(), SystemGitClient, 1000);

        let working_copy = cache
            .ensure_cached(&repo_dir.to_string_lossy(), "v1.0.0")
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(working_copy.join("file.txt")).unwrap(),
            "hello"
        );

        std::fs::remove_dir_all(&repo_dir).ok();
        std::fs::remove_dir_all(&cache_root).ok();
    }

    #[test]
    fn cache_key_is_stable_and_distinguishes_remote_and_ref() {
        let a = cache_key("https://example.com/repo.git", "v1.0.0");
        let b = cache_key("https://example.com/repo.git", "v1.0.0");
        let c = cache_key("https://example.com/repo.git", "v2.0.0");
        let d = cache_key("https://example.com/other.git", "v1.0.0");

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn cache_key_does_not_collide_across_the_remote_ref_boundary() {
        // SEC-001 (SECURITY_FINDINGS.md): the pre-0.10.0 implementation
        // joined the two fields with a bare `" @"` separator, so a
        // `remote` containing that separator could collide with a
        // differently-split (remote, ref) pair.
        let a = cache_key("https://example.com/repo.git @evil-ref", "main");
        let b = cache_key("https://example.com/repo.git", "evil-ref @main");
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn ensure_cached_clones_once_and_reuses_the_cache_on_a_second_call() {
        let root = unique_temp_dir();
        let mut files = HashMap::new();
        files.insert("bundle.yml".to_string(), "tasks: {}".to_string());
        let git = FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", files);
        let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000);

        let first = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();
        let second = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(git.clone_count(), 1);
        assert!(first.join("bundle.yml").is_file());

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn ensure_cached_writes_an_info_sidecar_preserving_repo_and_bumping_last_used() {
        let root = unique_temp_dir();
        let git = FakeGitClient::new().with_files(
            "https://example.com/repo.git",
            "v1.0.0",
            HashMap::new(),
        );
        let cache = GitIncludeCache::for_test(root.clone(), git, 1000);

        let working_copy = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();
        let key = cache_key("https://example.com/repo.git", "v1.0.0");
        assert_eq!(working_copy, root.join(&key));

        let info_content = tokio::fs::read_to_string(root.join(format!("{key}.toml")))
            .await
            .unwrap();
        let info: CacheInfo = toml::from_str(&info_content).unwrap();
        assert_eq!(info.kind, "git");
        assert_eq!(info.repo.remote, "https://example.com/repo.git");
        assert_eq!(info.repo.git_ref, "v1.0.0");
        assert_eq!(info.last_used, 1000);

        // A second ensure_cached (with a different clock) bumps last_used
        // but keeps everything else, in particular cloned_with_version.
        let git2 = FakeGitClient::new();
        let cache2 = GitIncludeCache::for_test(root.clone(), git2, 2000);
        cache2
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();
        let info_content = tokio::fs::read_to_string(root.join(format!("{key}.toml")))
            .await
            .unwrap();
        let info2: CacheInfo = toml::from_str(&info_content).unwrap();
        assert_eq!(info2.last_used, 2000);
        assert_eq!(info2.cloned_with_version, info.cloned_with_version);

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn ensure_cached_surfaces_a_clone_failure_and_does_not_leave_a_lock_file_behind() {
        let root = unique_temp_dir();
        let git = FakeGitClient::new().failing("simulated clone failure");
        let cache = GitIncludeCache::for_test(root.clone(), git, 1000);

        let result = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("simulated clone failure"));

        let key = cache_key("https://example.com/repo.git", "v1.0.0");
        assert!(!root.join(format!("{key}.lock")).exists());
        assert!(!root.join(&key).exists());

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn acquire_lock_times_out_if_the_lock_file_is_never_released() {
        let root = unique_temp_dir();
        tokio::fs::create_dir_all(&root).await.unwrap();
        let git = FakeGitClient::new();
        let mut cache = GitIncludeCache::for_test(root.clone(), git, 1000);
        cache.lock_timeout = Duration::from_millis(250);

        let key = cache_key("https://example.com/repo.git", "v1.0.0");
        let lock_path = root.join(format!("{key}.lock"));
        tokio::fs::write(&lock_path, b"").await.unwrap();

        let result = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Timed out"));

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    /// Two entries with different last-used times, so ordering and the
    /// per-entry fields can both be checked at once.
    async fn cache_with_two_entries(root: &std::path::Path) -> GitIncludeCache<FakeGitClient> {
        let git = FakeGitClient::new()
            .with_files(
                "https://example.com/old.git",
                "v1.0.0",
                HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
            )
            .with_files(
                "https://example.com/new.git",
                "v2.0.0",
                HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
            );

        // Cached at different times, so `last_used` differs.
        let old = GitIncludeCache::for_test(root.to_path_buf(), git.clone(), 1_000);
        old.ensure_cached("https://example.com/old.git", "v1.0.0")
            .await
            .unwrap();
        let new = GitIncludeCache::for_test(root.to_path_buf(), git.clone(), 5_000);
        new.ensure_cached("https://example.com/new.git", "v2.0.0")
            .await
            .unwrap();

        GitIncludeCache::for_test(root.to_path_buf(), git, 5_000)
    }

    #[tokio::test]
    async fn list_reports_each_entry_oldest_first_with_its_size() {
        let root = unique_temp_dir();
        let cache = cache_with_two_entries(&root).await;

        let listed = cache.list().await.unwrap();

        assert_eq!(listed.len(), 2);
        // Oldest first: the order someone clearing space reads down.
        assert_eq!(listed[0].remote, "https://example.com/old.git");
        assert_eq!(listed[0].git_ref, "v1.0.0");
        assert_eq!(listed[0].last_used, 1_000);
        assert_eq!(listed[1].remote, "https://example.com/new.git");
        assert_eq!(listed[1].last_used, 5_000);
        // The fake writes a real file into each clone, so a size of zero
        // would mean the walk never happened.
        assert!(
            listed.iter().all(|entry| entry.size_bytes > 0),
            "every entry should be sized: {listed:?}"
        );

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn list_is_empty_when_nothing_has_ever_been_cached() {
        let root = unique_temp_dir();
        let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 1_000);
        assert!(cache.list().await.unwrap().is_empty());
        tokio::fs::remove_dir_all(&root).await.ok();
    }

    /// `clean` with an age is the same rule the automatic sweep applies —
    /// which is what a bare `ratect includes clean` uses.
    #[tokio::test]
    async fn clean_with_a_minimum_age_removes_only_entries_older_than_it() {
        let root = unique_temp_dir();
        let cache = cache_with_two_entries(&root).await;

        // At t=5000, the older entry is 4000s unused and the newer 0s.
        let removed = cache.clean(Some(Duration::from_secs(3_000))).await.unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].remote, "https://example.com/old.git");
        let left = cache.list().await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].remote, "https://example.com/new.git");

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    /// `--all`: no age, everything goes. Safe in a way removing containers
    /// isn't — the worst case is a re-clone.
    #[tokio::test]
    async fn clean_without_a_minimum_age_removes_everything() {
        let root = unique_temp_dir();
        let cache = cache_with_two_entries(&root).await;

        let removed = cache.clean(None).await.unwrap();

        assert_eq!(removed.len(), 2);
        assert!(cache.list().await.unwrap().is_empty());
        // Both the working copy and its sidecar, or the next run would find
        // a sidecar with no clone.
        let mut left = tokio::fs::read_dir(&root).await.unwrap();
        while let Some(entry) = left.next_entry().await.unwrap() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                name.ends_with(".lock"),
                "only lock files should remain, found {name}"
            );
        }

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    /// The reason the verb exists: a moved `ref` is otherwise invisible
    /// forever, since `ensure_cached` only clones when the working copy is
    /// missing and an in-use entry never goes stale enough to be swept.
    #[tokio::test]
    async fn refresh_re_clones_and_so_picks_up_a_moved_ref() {
        let root = unique_temp_dir();
        let remote = "https://example.com/moving.git";

        let before = FakeGitClient::new().with_files(
            remote,
            "main",
            HashMap::from([("bundle.yml".to_string(), "old contents".to_string())]),
        );
        let cache = GitIncludeCache::for_test(root.clone(), before, 1_000);
        let working_copy = cache.ensure_cached(remote, "main").await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(working_copy.join("bundle.yml"))
                .await
                .unwrap(),
            "old contents"
        );

        // The branch moves. `ensure_cached` alone would never notice.
        let after = FakeGitClient::new().with_files(
            remote,
            "main",
            HashMap::from([("bundle.yml".to_string(), "new contents".to_string())]),
        );
        let cache = GitIncludeCache::for_test(root.clone(), after, 2_000);
        assert_eq!(
            tokio::fs::read_to_string(
                cache
                    .ensure_cached(remote, "main")
                    .await
                    .unwrap()
                    .join("bundle.yml")
            )
            .await
            .unwrap(),
            "old contents",
            "ensure_cached must not re-fetch — that's the behaviour refresh exists for"
        );

        let refreshed = cache.refresh().await.unwrap();

        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].remote, remote);
        assert_eq!(
            tokio::fs::read_to_string(working_copy.join("bundle.yml"))
                .await
                .unwrap(),
            "new contents"
        );

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn cleanup_stale_removes_an_entry_unused_for_more_than_30_days() {
        let root = unique_temp_dir();
        let git = FakeGitClient::new().with_files(
            "https://example.com/repo.git",
            "v1.0.0",
            HashMap::new(),
        );
        let cache = GitIncludeCache::for_test(root.clone(), git, 1000);
        let working_copy = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();
        let key = cache_key("https://example.com/repo.git", "v1.0.0");
        let info_path = root.join(format!("{key}.toml"));

        let now = 1000 + STALE_AFTER.as_secs() + 1;
        let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
        sweeper.cleanup_stale().await.unwrap();

        assert!(!working_copy.exists());
        assert!(!info_path.exists());

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn cleanup_stale_keeps_an_entry_used_within_the_last_30_days() {
        let root = unique_temp_dir();
        let git = FakeGitClient::new().with_files(
            "https://example.com/repo.git",
            "v1.0.0",
            HashMap::new(),
        );
        let cache = GitIncludeCache::for_test(root.clone(), git, 1000);
        let working_copy = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();
        let key = cache_key("https://example.com/repo.git", "v1.0.0");
        let info_path = root.join(format!("{key}.toml"));

        let now = 1000 + STALE_AFTER.as_secs() - 1;
        let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
        sweeper.cleanup_stale().await.unwrap();

        assert!(working_copy.exists());
        assert!(info_path.exists());

        tokio::fs::remove_dir_all(&root).await.ok();
    }

    #[tokio::test]
    async fn cleanup_stale_is_a_noop_when_the_cache_root_does_not_exist() {
        let root = unique_temp_dir();
        let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 1000);

        cache.cleanup_stale().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_stale_skips_an_unparsable_info_file_and_removes_other_stale_entries() {
        let root = unique_temp_dir();
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("not-toml.toml"), b"not valid toml {{{")
            .await
            .unwrap();

        let git = FakeGitClient::new().with_files(
            "https://example.com/repo.git",
            "v1.0.0",
            HashMap::new(),
        );
        let cache = GitIncludeCache::for_test(root.clone(), git, 1000);
        let working_copy = cache
            .ensure_cached("https://example.com/repo.git", "v1.0.0")
            .await
            .unwrap();

        let now = 1000 + STALE_AFTER.as_secs() + 1;
        let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
        sweeper.cleanup_stale().await.unwrap();

        assert!(!working_copy.exists());
        assert!(root.join("not-toml.toml").exists());

        tokio::fs::remove_dir_all(&root).await.ok();
    }
}
