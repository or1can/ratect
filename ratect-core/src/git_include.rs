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
//!
//! Git includes (`type: git` entries
//! in `include`) — `GitIncludeCache::ensure_cached`, driven by
//! `config.rs`'s own include-resolution loop, clones a `(remote, ref)` pair
//! once into `~/.ratect/incl/<sha256 key>/` and reuses it forever (0.8.0);
//! a `<key>.toml` sidecar (`CacheInfo`) records `last_used` (a Unix
//! timestamp, not `atime`/`mtime` — unreliable across platforms/CI),
//! bumped on every `ensure_cached` call regardless of whether a clone
//! actually happened. `GitIncludeCache::cleanup_stale` (0.19.0) sweeps that
//! same cache: any entry whose `last_used` is more than 30 days old gets
//! both its working copy and its `.toml` sidecar removed, matching
//! Batect's own `GitRepositoryCacheCleanupTask` exactly except that it's a
//! `tokio::spawn`ed async task, not a literal OS thread (Batect's own JVM
//! daemon thread is the equivalent to port the *behavior* of — unconditional,
//! fire-and-forget, never awaited — not literally a `std::thread::spawn`).
//! Started unconditionally from `main.rs`'s "run a task" branch (not
//! `--list-tasks`), before the Docker connectivity check, mirroring where
//! Batect's own `BackgroundTaskManager` fires it. One stale entry failing
//! to delete (unreadable/unparsable sidecar, filesystem error) is logged
//! and skipped rather than aborting the whole sweep — same per-entry
//! try/catch Batect's own cleanup task has. `cached_working_copy` (0.3.0) is
//! the read-only counterpart to `ensure_cached`: it computes the same
//! `~/.ratect/incl` path and returns it only if the clone already exists —
//! never cloning, locking, or touching the network — for offline callers
//! (`config::task_names_for_completion`) that must not stall a shell `<TAB>`.

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

/// The working copy for `(remote, git_ref)` if it's *already* cached —
/// without cloning, locking, or any network. For offline consumers (shell
/// completion, [`crate::config::task_names_for_completion`]) that must never
/// block a `<TAB>` on a fetch: an uncached include simply contributes nothing.
/// Returns `None` if it isn't cached, or if the cache directory itself cannot
/// be located.
///
/// `root` is the cache directory to look in, `None` meaning the real
/// `~/.ratect/incl` — the same seam `GitIncludeCache::for_test` has (named
/// without a link, since it is `#[cfg(test)]` and rustdoc cannot resolve it),
/// and for the same reason: without it, everything reachable only through this
/// function is untestable in-process, which is how completion's own walk came
/// to enforce a containment rule with no test behind it.
pub(crate) fn cached_working_copy(
    remote: &str,
    git_ref: &str,
    root: Option<&Path>,
) -> Option<PathBuf> {
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => CacheRoot::Home.resolve().ok()?,
    };
    let working_copy = root.join(cache_key(remote, git_ref));
    working_copy.is_dir().then_some(working_copy)
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
    /// `STALE_AFTER` old — matching Batect's own
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
    /// oldest first — the order someone clearing space wants to read —
    /// and by key within a second, so the listing is stable between runs.
    ///
    /// An unreadable or unparsable sidecar is logged and skipped rather
    /// than failing the listing, the same per-entry tolerance
    /// [`cleanup_stale`](Self::cleanup_stale) has: one corrupt file
    /// shouldn't make the whole cache unreportable.
    pub async fn list(&self) -> Result<Vec<CachedInclude>> {
        let root = self.root.resolve()?;
        let mut entries = self.read_entries(&root).await?;
        // The key breaks ties: `last_used` is whole seconds, so includes
        // resolved by the same run routinely share one, and `read_entries`
        // walks a directory — leaving those equal would order them by
        // whatever `read_dir` happened to yield.
        entries.sort_by(|left, right| {
            left.last_used
                .cmp(&right.last_used)
                .then_with(|| left.key.cmp(&right.key))
        });

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
#[path = "git_include_tests.rs"]
mod tests;
