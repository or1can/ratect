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
            .args(["-c", "advice.detachedHead=false", "-C"])
            .arg(&temp_dir)
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

/// The clock `GitIncludeCache` reads `last_used` from — boxed so the real
/// `SystemTime::now`-backed closure and a fixed test closure share one
/// field type, same idiom as `engine.rs`'s `HostEnv`.
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

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
pub(crate) fn cache_key(remote: &str, git_ref: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(format!("git {remote} @{git_ref}").as_bytes());
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
}
