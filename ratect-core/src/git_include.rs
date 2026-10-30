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
//! Batect's own `GitRepositoryCacheCleanupTask` except in how it handles a
//! bad sidecar (below), in counting those 30 days as a fixed number of
//! seconds rather than calendar days (see `STALE_AFTER`), and in being a
//! `tokio::spawn`ed async task, not a literal OS thread (Batect's own JVM
//! daemon thread is the equivalent to port the *behavior* of — unconditional,
//! fire-and-forget, never awaited — not literally a `std::thread::spawn`).
//! Started unconditionally from `main.rs`'s "run a task" branch (not
//! `--list-tasks`), before the Docker connectivity check, mirroring where
//! Batect's own `BackgroundTaskManager` fires it. One stale entry failing
//! to delete (unreadable/unparsable sidecar, filesystem error) is logged
//! and skipped rather than aborting the whole sweep. Batect's own cleanup
//! task catches a failed deletion per entry too, but reads every sidecar
//! outside that catch, so one it can't read or parse aborts its whole sweep,
//! every run, until it's removed. `cached_working_copy` (0.3.0) is
//! the read-only counterpart to `ensure_cached`: it computes the same
//! `~/.ratect/incl` path and returns it only if the clone already exists —
//! never cloning, locking, or touching the network — for offline callers
//! (`config::task_names_for_completion`) that must not stall a shell `<TAB>`.
//!
//! A clone is staged in a `<key>.tmp-<unique>` directory of its own and
//! renamed into place once complete, so no lock is needed for a working copy
//! to be whole. The **entry lock** (CONTEXT.md's **Caches**) only stops two
//! processes cloning the same entry at once: [`GitIncludeCache`] describes
//! it, and the `entry_lock` submodule takes it. A lock whose **holder** has
//! exited — killed during a first clone, say — is taken over by the next
//! load that needs the entry, rather than blocking it until someone deletes
//! the file, which Batect's would; a wait on a lock says so on stderr, and
//! a clone, once under the lock and still needed, is announced as progress.
//! See [decisions/0023](https://github.com/or1can/ratect/blob/main/decisions/0023-git-include-entry-lock.md).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::Instant;

use crate::ui::LoadOutput;

mod entry_lock;

/// A Git include's remote — its `repo` — wherever one is held: the config
/// model, the cache's entries and sidecars, a bundle's identity (#756).
///
/// `Display` and `Debug` both show it as Ratect shows a remote in every
/// message and listing: a URL's credentials — everything in its authority
/// before the last `@` — as `***`, so a token in a `repo` never reaches a
/// terminal or a log (#751); the rest as written, byte for byte, and a remote
/// that isn't a URL (`git@host:path`, a local path) unchanged. `git`'s own
/// messages already leave credentials out. So a new message, log record or
/// `{:?}` naming one hides them without its author having to remember to.
///
/// The remote in full is `Remote::unredacted`, crate-private so neither
/// binary can print it, and only what must have it reads it: the `git`
/// invocation (its argv, and the leading-`-` refusal that guards it), the
/// cache key, and serialisation — which is transparent, so a sidecar and
/// `ratect config convert`'s output keep the URL that clones. Equality and hashing compare it in full too, so two
/// remotes differing only in credentials are different remotes that display
/// alike.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Remote(String);

impl Remote {
    /// The remote as written, credentials included. Never for a message.
    pub(crate) fn unredacted(&self) -> &str {
        &self.0
    }

    /// Whether `Display` leaves credentials out — the only way two different
    /// remotes can show alike, so what tells `ratect includes` to add an
    /// entry's cache key to tell them apart.
    pub fn hides_credentials(&self) -> bool {
        matches!(self.redacted(), Cow::Owned(_))
    }

    /// The remote as `Display` shows it. Written out rather than parsed with
    /// `url`, which would normalise the rest (lowercase the host, add a
    /// trailing `/`) and can't parse what isn't a URL.
    fn redacted(&self) -> Cow<'_, str> {
        let remote = self.0.as_str();
        let Some(scheme_end) = remote.find("://") else {
            return Cow::Borrowed(remote);
        };
        let (head, rest) = remote.split_at(scheme_end + "://".len());
        // To the first `/` only: a credential may hold an unencoded `?` or `#`,
        // and taking too much of a URL with neither only hides more of it.
        let authority = &rest[..rest.find('/').unwrap_or(rest.len())];
        match authority.rfind('@') {
            Some(at) => Cow::Owned(format!("{head}***{}", &rest[at..])),
            None => Cow::Borrowed(remote),
        }
    }
}

impl From<String> for Remote {
    fn from(remote: String) -> Self {
        Remote(remote)
    }
}

impl From<&str> for Remote {
    fn from(remote: &str) -> Self {
        Remote(remote.to_string())
    }
}

impl std::fmt::Display for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.redacted())
    }
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.redacted(), f)
    }
}

/// A Git include as every message names it — `'<remote>' at '<ref>'` — so
/// no message spells the pair on its own.
pub fn describe_include(remote: &Remote, git_ref: &str) -> String {
    format!("'{remote}' at '{git_ref}'")
}

/// Where a sidecar failed to parse — `at line N` — and nothing else: toml's
/// own `Display` quotes the line, and serde's type errors quote the value,
/// either of which can be the remote in full (#751).
fn describe_parse_error(error: &toml::de::Error, content: &str) -> String {
    match error.span() {
        Some(span) => format!(
            "at line {}",
            content[..span.start].matches('\n').count() + 1
        ),
        None => "as TOML".to_string(),
    }
}

/// Shells out to the system `git` binary, so tests can inject a fake
/// instead of needing a real network and a real `git` — same shape as
/// `docker.rs`'s `ContainerRuntime`.
#[async_trait::async_trait]
pub trait GitClient: Send + Sync {
    /// Clones `remote` at `ref`, checked out at `destination`.
    /// `destination` doesn't exist yet: it is a staging directory fresh to
    /// this attempt, which the caller (`GitIncludeCache`) renames into place
    /// once this returns, and removes if it fails.
    async fn clone_repo(&self, remote: &Remote, git_ref: &str, destination: &Path) -> Result<()>;
}

/// The real `GitClient`: `git clone --quiet --no-checkout` into
/// `destination`, then `git checkout --recurse-submodules <ref>` — the same
/// argv sequence as Batect's own `GitClient` (no embedded Git library, kept
/// dependency-light), and like Batect's into a directory fresh to each
/// attempt, renamed into place only once both have finished, with
/// no transport restriction of its own, so git's protocol defaults and the
/// user's own Git configuration apply as they do under Batect. It differs
/// in refusing a `remote` or `ref` that starts with `-`, and in capturing
/// the clone's own output into its error rather than printing it.
/// `--recurse-submodules` fetches nothing here, in either tool: it only
/// updates *active* submodules, and a fresh `--no-checkout` clone has none.
pub struct SystemGitClient;

#[async_trait::async_trait]
impl GitClient for SystemGitClient {
    async fn clone_repo(&self, remote: &Remote, git_ref: &str, destination: &Path) -> Result<()> {
        // Defense against argv flag smuggling: a `repo`/`ref` from a config
        // file (possibly itself from a git-included bundle) that starts
        // with `-` could otherwise be parsed as a git flag rather than a
        // positional argument. `clone` below also has a `--` separator
        // before `remote`; `checkout` can't safely use one (see the comment
        // there), so this check is what protects `git_ref` there.
        if remote.unredacted().starts_with('-') {
            anyhow::bail!("Git include 'repo' must not start with '-': '{remote}'");
        }
        if git_ref.starts_with('-') {
            anyhow::bail!("Git include 'ref' must not start with '-': '{git_ref}'");
        }

        let clone_output = Command::new("git")
            .args([
                "clone",
                "--quiet",
                "--no-checkout",
                "--",
                remote.unredacted(),
            ])
            .arg(destination)
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
            .arg(destination)
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
    remote: Remote,
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
    pub remote: Remote,
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

/// What [`GitIncludeCache::clean`]/[`refresh`](GitIncludeCache::refresh) did:
/// the entries each acted on, and each one it couldn't, for the caller to
/// report — this module prints nothing of its own (#755).
#[derive(Debug, Default)]
pub struct Outcome {
    pub succeeded: Vec<CachedInclude>,
    pub failed: Vec<Failure>,
}

/// An entry `clean` couldn't remove, or `refresh` couldn't remove or clone
/// again, and why. `error` names a remote only as [`Remote`] displays it.
#[derive(Debug)]
pub struct Failure {
    pub entry: CachedInclude,
    pub error: anyhow::Error,
}

/// Removes an entry's working copy and sidecar, tolerating either being
/// absent already — including when another process removes the same entry
/// at the same time, since removal takes no lock. An error if something is
/// left behind.
async fn remove_entry_files(working_copy: &Path, info_path: &Path) -> Result<()> {
    if let Err(e) = tokio::fs::remove_dir_all(working_copy).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e).with_context(|| {
                format!("Failed to remove Git include cache clone {working_copy:?}")
            });
        }
    }
    if let Err(e) = tokio::fs::remove_file(info_path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e).with_context(|| {
                format!("Failed to remove Git include cache info file {info_path:?}")
            });
        }
    }
    Ok(())
}

/// What follows an entry's key in the name of a staging directory a clone of
/// it is made in: `<key>.tmp-<unique>`.
const STAGING_INFIX: &str = ".tmp-";

/// Removes every staging directory of the entry `key` in `root`. Only ever
/// called under that entry's lock. Best-effort: one left behind is removed by
/// the next holder.
async fn remove_staging_directories(root: &Path, key: &str) {
    let prefix = format!("{key}{STAGING_INFIX}");
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            remove_staging_directory(&entry.path()).await;
        }
    }
}

async fn remove_staging_directory(staging: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(staging).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("Failed to remove Git include staging directory {staging:?}: {e}");
        }
    }
}

/// `timeout` as a wait's notice and failure say it: whole minutes in words,
/// anything else as `Duration` prints it.
fn describe_wait(timeout: Duration) -> String {
    match timeout.as_secs() {
        60 if timeout.subsec_nanos() == 0 => "1 minute".to_string(),
        secs if secs % 60 == 0 && timeout.subsec_nanos() == 0 && secs > 0 => {
            format!("{} minutes", secs / 60)
        }
        _ => format!("{timeout:?}"),
    }
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
/// removes it: 30 days of 24 hours, compared against `last_used` in whole
/// seconds, and not configurable — nor is Batect's. Batect's
/// `GitRepositoryCacheCleanupTask` instead counts back 30 calendar days in
/// the system time zone, at sub-second precision, so across a daylight-saving
/// change its window is up to an hour longer or shorter than this one. Kept
/// fixed deliberately: when a clone is evicted changes nothing a config
/// loads, only when it's cloned again.
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
pub(crate) fn cache_key(remote: &Remote, git_ref: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    let remote = remote.unredacted();
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
    remote: &Remote,
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

/// Clones-once-and-reuses-forever cache for Git includes, rooted at
/// `~/.ratect/incl` in production (`GitIncludeCache::new`) — see
/// ROADMAP.md's 0.8.0 entry. A repo/ref already present on disk (by cache
/// key) is never re-fetched; this is why users are expected to pin
/// immutable tags/refs, not a corner being cut here.
///
/// Correctness doesn't depend on any lock: each clone attempt stages into a
/// directory of its own, renamed into place only once complete, so two clones
/// of one entry at once can't mix, and an interrupted one is never used. The
/// clone step is still guarded by the **entry lock**, `<key>.lock`, so that
/// concurrent invocations don't clone the same repo/ref twice: an existence
/// lock (create-exclusive, polled, given up after `lock_timeout`), as Batect's
/// `LockingRepositoryCloner` takes and every older Ratect still does, carrying
/// an OS lock and a format marker, so that a lock whose holder has exited is
/// taken over rather than waited on. Removal never touches it. See
/// [decisions/0023](https://github.com/or1can/ratect/blob/main/decisions/0023-git-include-entry-lock.md).
pub struct GitIncludeCache<G: GitClient> {
    root: CacheRoot,
    git: G,
    clock: Clock,
    version: String,
    lock_timeout: Duration,
    output: LoadOutput,
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
            output: LoadOutput::default(),
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
            output: LoadOutput::default(),
        }
    }

    /// Where a wait on an entry lock and a clone are announced — the loading
    /// binary's own, rather than [`LoadOutput::default`].
    pub fn with_output(mut self, output: LoadOutput) -> Self {
        self.output = output;
        self
    }

    /// Stands in for a different ratect version sharing the same cache, so a
    /// test can tell a sidecar's `cloned_with_version` kept from rewritten.
    #[cfg(test)]
    pub(crate) fn with_version(mut self, version: &str) -> Self {
        self.version = version.to_string();
        self
    }

    /// Ensures `remote` at `ref` is cloned into this cache, returning the
    /// clone's directory. Safe to call repeatedly (across processes, or
    /// concurrently) for the same `(remote, ref)` — later calls are no-ops
    /// beyond bumping `last_used`. A call that has to clone takes the entry
    /// lock first: it takes over a lock whose holder has exited, and waits on
    /// any other — announcing the wait on this cache's [`LoadOutput`] — for
    /// up to `lock_timeout` (see `acquire_lock`).
    pub async fn ensure_cached(&self, remote: &Remote, git_ref: &str) -> Result<PathBuf> {
        let root = self.root.resolve()?;
        tokio::fs::create_dir_all(&root)
            .await
            .with_context(|| format!("Failed to create Git include cache directory {root:?}"))?;

        let key = cache_key(remote, git_ref);
        let working_copy = root.join(&key);
        let info_path = root.join(format!("{key}.toml"));
        let lock_path = root.join(format!("{key}.lock"));

        // Batect's ordering (`GitRepositoryCache.cloneRepoIfMissing`): only a
        // caller that still needs to clone takes the lock, so a lock file left
        // beside a complete working copy can't block every later load.
        // `clone_if_missing` re-checks under the lock, for two callers racing
        // to clone the same entry.
        if !working_copy.exists() {
            let lock = self.acquire_lock(&lock_path, remote, git_ref).await?;
            let clone_result = self
                .clone_if_missing(remote, git_ref, &root, &key, &working_copy)
                .await;
            self.release_lock(lock, &lock_path).await;
            clone_result?;
        }

        self.update_info_file(remote, git_ref, &info_path).await?;

        Ok(working_copy)
    }

    /// Under the entry lock. Removes every `<key>.tmp-*` first: under the
    /// lock, any of them was left by an interrupted clone (decisions/0023's
    /// Decision 7) — nothing else removes one, and nothing judges one by its
    /// age. Then clones into a fresh one and renames it into place, announcing
    /// the clone as progress before it starts and following a successful one
    /// with a blank line, as Batect does — though only once the re-check has
    /// found it still needed, so a load that waited out another process's
    /// clone doesn't announce one it then skips, as Batect's would. Losing
    /// that rename to a clone that finished first — one by a holder whose lock
    /// this process took over, or by a waiter on a filesystem whose OS lock
    /// works per host only — is not an error: the winner's working copy is as
    /// good as this one, which is discarded.
    async fn clone_if_missing(
        &self,
        remote: &Remote,
        git_ref: &str,
        root: &Path,
        key: &str,
        destination: &Path,
    ) -> Result<()> {
        if destination.exists() {
            return Ok(());
        }
        remove_staging_directories(root, key).await;

        let staging = root.join(format!(
            "{key}{STAGING_INFIX}{}",
            uuid::Uuid::new_v4().simple()
        ));
        self.output.progress(&format!(
            "Cloning {} {}...",
            self.output.progress_bold(&remote.to_string()),
            self.output.progress_bold(git_ref)
        ));
        if let Err(e) = self.git.clone_repo(remote, git_ref, &staging).await {
            remove_staging_directory(&staging).await;
            return Err(e);
        }
        match tokio::fs::rename(&staging, destination).await {
            Ok(()) => {}
            Err(_) if destination.is_dir() => remove_staging_directory(&staging).await,
            Err(e) => {
                remove_staging_directory(&staging).await;
                return Err(e).with_context(|| {
                    format!("Failed to move {staging:?} into place at {destination:?}")
                });
            }
        }
        self.output.progress("");
        Ok(())
    }

    /// Waits for the entry lock at `lock_path` until it is free or abandoned
    /// — in either case taking it — or until `lock_timeout` has passed. The
    /// first look that finds it held prints a notice saying so (Decision 8);
    /// a wait that ends because the lock was released prints nothing more.
    /// Except that a holder that can't be told running is announced on the
    /// second look, not the first: a new holder creates its file before
    /// taking the OS lock and marking it, and a first look in that gap would
    /// otherwise announce a running holder as one that might be gone.
    /// `remote` and `git_ref` name the include in what is printed.
    async fn acquire_lock(
        &self,
        lock_path: &Path,
        remote: &Remote,
        git_ref: &str,
    ) -> Result<entry_lock::EntryLock> {
        let include = format!("Git include {}", describe_include(remote, git_ref));
        let limit = describe_wait(self.lock_timeout);
        let start = Instant::now();
        let mut announced = false;
        let mut held_looks = 0;
        loop {
            let path = lock_path.to_path_buf();
            let found = tokio::task::spawn_blocking(move || entry_lock::attempt(&path))
                .await
                .context("Git include lock check panicked")?
                .with_context(|| format!("Failed to create lock file {lock_path:?}"))?;
            let holder_running = match found {
                entry_lock::Attempt::Acquired(lock) => return Ok(lock),
                entry_lock::Attempt::HeldByRunningHolder => true,
                entry_lock::Attempt::HeldByUnknownHolder => false,
            };
            held_looks += 1;
            if !announced && (holder_running || held_looks > 1) {
                announced = true;
                self.output.notice(&if holder_running {
                    format!(
                        "Waiting for another ratect process to finish cloning {include} \
                         (giving up after {limit})."
                    )
                } else {
                    format!(
                        "Waiting for lock file {lock_path:?} on {include}: ratect can't tell \
                         whether the process that holds it is still running (giving up \
                         after {limit})."
                    )
                });
            }
            if start.elapsed() > self.lock_timeout {
                if holder_running {
                    anyhow::bail!(
                        "Timed out after {limit} waiting for another ratect process to finish \
                         cloning {include}. It is still running: wait for it to finish, or \
                         try again."
                    );
                }
                anyhow::bail!(
                    "Timed out after {limit} waiting for lock file {lock_path:?} on {include}. \
                     ratect can't tell whether the process that holds it is still running; \
                     once no ratect process is cloning that include, it is safe to delete \
                     that file."
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Best-effort: a failure here just leaves a lock file behind, which the
    /// next waiter takes over once this process has exited (its OS lock goes
    /// with it). Only cloning takes the lock — not a load once the working
    /// copy exists, nor `remove_entry` — so that only times out a caller that
    /// still needs to clone, as Batect's would.
    async fn release_lock(&self, lock: entry_lock::EntryLock, lock_path: &Path) {
        match tokio::task::spawn_blocking(move || lock.release()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("Failed to remove Git include lock file {lock_path:?}: {e}")
            }
            Err(e) => tracing::warn!("Git include lock release panicked for {lock_path:?}: {e}"),
        }
    }

    /// Writes/updates the `<hash>.toml` sidecar. An existing file is
    /// merged into, as Batect's `GitRepositoryCache.updateInfoFile` does:
    /// only `last_used` changes, and every other key keeps its value —
    /// `cloned_with_version` (so a later `ratect` version reusing an old
    /// clone doesn't overwrite the version it was actually cloned with), and
    /// any key this version doesn't know, at any depth, that another version
    /// sharing `~/.ratect/incl` wrote. A file missing a known field is merged
    /// into all the same, not refused or filled in — though `list` and the
    /// sweep, which need those fields, still skip it with a warning. Written
    /// via write-to-temp-then-atomic-rename, so a concurrent reader
    /// (`list`, or the sweep) can never observe a torn file; a concurrent
    /// `last_used` bump can still be lost to a last-write-wins race, same as
    /// Batect accepts.
    async fn update_info_file(
        &self,
        remote: &Remote,
        git_ref: &str,
        info_path: &Path,
    ) -> Result<()> {
        let mut info: toml::Table = match tokio::fs::read_to_string(info_path).await {
            Ok(content) => toml::from_str(&content).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse Git include cache info file {info_path:?} {}; delete it \
                     to have it written again",
                    describe_parse_error(&e, &content)
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                toml::Table::try_from(CacheInfo {
                    kind: "git".to_string(),
                    repo: CacheInfoRepo {
                        remote: remote.clone(),
                        git_ref: git_ref.to_string(),
                    },
                    cloned_with_version: self.version.clone(),
                    last_used: 0,
                })
                .context("Failed to serialize Git include cache info")?
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to read Git include cache info file {info_path:?}")
                })
            }
        };
        let last_used = i64::try_from((self.clock)()).with_context(|| {
            format!(
                "Failed to record the current time in Git include cache info file {info_path:?}"
            )
        })?;
        info.insert("last_used".to_string(), toml::Value::Integer(last_used));

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

    /// Removes cached entries, returning the ones removed and each one that
    /// couldn't be, which doesn't stop the rest.
    ///
    /// `minimum_age` of `None` removes everything (`ratect includes clean
    /// --all`); `Some` removes only entries unused for at least that long,
    /// which is what both the automatic sweep and a bare `includes clean`
    /// do. Nothing here is unrecoverable — the worst case of removing too
    /// much is a re-clone — which is why this has no confirmation of any
    /// kind, unlike removing containers.
    pub async fn clean(&self, minimum_age: Option<Duration>) -> Result<Outcome> {
        let root = self.root.resolve()?;
        let now = (self.clock)();
        let entries = self.read_entries(&root).await?;

        let mut outcome = Outcome::default();
        for entry in entries {
            let old_enough = match minimum_age {
                Some(age) => now.saturating_sub(entry.last_used) > age.as_secs(),
                None => true,
            };
            if !old_enough {
                continue;
            }
            match self.remove_entry(&root, &entry.key).await {
                Ok(()) => outcome.succeeded.push(entry),
                Err(error) => outcome.failed.push(Failure { entry, error }),
            }
        }

        Ok(outcome)
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
    /// re-fetched. It is returned as failed, as is an entry that couldn't be
    /// removed, and neither stops the rest.
    pub async fn refresh(&self) -> Result<Outcome> {
        let root = self.root.resolve()?;
        let entries = self.read_entries(&root).await?;

        let mut outcome = Outcome::default();
        for entry in entries {
            let refreshed = match self.remove_entry(&root, &entry.key).await {
                Ok(()) => self.ensure_cached(&entry.remote, &entry.git_ref).await,
                Err(error) => Err(error),
            };
            match refreshed {
                Ok(_) => outcome.succeeded.push(entry),
                Err(error) => outcome.failed.push(Failure { entry, error }),
            }
        }

        Ok(outcome)
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
                    tracing::warn!(
                        "Failed to parse Git include cache info file {path:?} {}",
                        describe_parse_error(&e, &content)
                    );
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

    /// Removes one entry's working copy and sidecar, without taking the
    /// entry lock `ensure_cached` clones under — as Batect's
    /// `GitRepositoryCache.delete` takes none either — and leaving any lock
    /// file in place (decisions/0023's Decision 6): a lock whose holder has
    /// exited is the next load's to take over, not removal's to guess at.
    /// Waiting for a clone in progress would only remove the finished clone
    /// afterwards, and a clone is built in a `<key>.tmp-*` staging directory,
    /// which this never touches, so there is nothing mid-clone to protect;
    /// while a lock file left beside a complete clone would stall every
    /// removal of that entry for the whole lock timeout. A load racing
    /// this can still return the removed path and write a sidecar for it,
    /// with or without the lock: the next load re-clones it, or once stale
    /// the sweep removes the sidecar.
    /// An error if anything was left behind, for the caller to report or log;
    /// a single unremovable entry shouldn't abandon the rest.
    async fn remove_entry(&self, root: &Path, key: &str) -> Result<()> {
        remove_entry_files(&root.join(key), &root.join(format!("{key}.toml"))).await
    }

    /// Removes any cached repo whose `last_used` is more than
    /// `STALE_AFTER` old — matching Batect's own
    /// `GitRepositoryCacheCleanupTask`/`GitRepositoryCache.delete`, except
    /// for a bad sidecar (below) and in how the window is measured (see
    /// `STALE_AFTER`).
    /// Meant to be started unconditionally, once per invocation, as a
    /// detached background task (see `main.rs`) — never awaited, so a
    /// failure here is only ever logged. Each stale entry is removed
    /// independently: one entry's removal failing (its `.toml` sidecar
    /// unreadable/unparsable, or a filesystem error) is logged and skipped
    /// rather than aborting the whole sweep. Batect's per-entry try/catch
    /// covers only the deletion: a sidecar it can't read or parse throws
    /// from `GitRepositoryCache.listAll`, before any entry is deleted.
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
            if let Err(e) = self.remove_entry(&root, &key).await {
                tracing::warn!("{e:#}");
            }
        }

        Ok(())
    }
}

/// Per `(remote, ref)`, each file's path within the clone and its contents.
#[cfg(test)]
type FakeGitClientResponses = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<(String, String), std::collections::HashMap<String, String>>,
    >,
>;

/// Per `(remote, ref)`, each symlink's path within the clone and its target,
/// written as given.
#[cfg(test)]
type FakeGitClientSymlinks = std::sync::Arc<
    std::sync::Mutex<std::collections::HashMap<(String, String), Vec<(String, PathBuf)>>>,
>;

/// A `GitClient` fake for tests: `clone_repo` writes pre-configured files
/// and symlinks into `destination` instead of touching the network or a real
/// `git` binary, matching `engine.rs`'s `FakeContainerRuntime` pattern.
/// `pub(crate)` (not module-private) so `config.rs`'s own tests, which drive
/// git includes end-to-end through `Config::load_from_file_with_git_cache`,
/// can use it too.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct FakeGitClient {
    responses: FakeGitClientResponses,
    symlinks: FakeGitClientSymlinks,
    fail: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    clone_calls: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    winner: std::sync::Arc<std::sync::Mutex<Option<PathBuf>>>,
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

    /// Also has `clone_repo(remote, git_ref, ...)` create a symlink at `link`,
    /// a relative path within the clone, pointing at `target` exactly as
    /// given — what a bundle that commits a symlink leaves in its checkout.
    pub(crate) fn with_symlink(
        self,
        remote: &str,
        git_ref: &str,
        link: &str,
        target: impl Into<PathBuf>,
    ) -> Self {
        self.symlinks
            .lock()
            .unwrap()
            .entry((remote.to_string(), git_ref.to_string()))
            .or_default()
            .push((link.to_string(), target.into()));
        self
    }

    /// Makes every `clone_repo` call fail with `message`.
    pub(crate) fn failing(self, message: &str) -> Self {
        *self.fail.lock().unwrap() = Some(message.to_string());
        self
    }

    /// Makes every `clone_repo` call lose its rename into place, as if a
    /// concurrent clone had finished first: before returning, it also writes
    /// `working_copy` holding a single file, `winner`.
    pub(crate) fn losing_the_rename_to(self, working_copy: PathBuf) -> Self {
        *self.winner.lock().unwrap() = Some(working_copy);
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
    async fn clone_repo(&self, remote: &Remote, git_ref: &str, destination: &Path) -> Result<()> {
        self.clone_calls
            .lock()
            .unwrap()
            .push((remote.unredacted().to_string(), git_ref.to_string()));

        if let Some(message) = self.fail.lock().unwrap().clone() {
            anyhow::bail!(message);
        }

        let files = self
            .responses
            .lock()
            .unwrap()
            .get(&(remote.unredacted().to_string(), git_ref.to_string()))
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
        let symlinks = self
            .symlinks
            .lock()
            .unwrap()
            .get(&(remote.unredacted().to_string(), git_ref.to_string()))
            .cloned()
            .unwrap_or_default();
        for (link, target) in symlinks {
            let path = destination.join(&link);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            #[cfg(unix)]
            tokio::fs::symlink(&target, &path).await?;
            #[cfg(not(unix))]
            anyhow::bail!("FakeGitClient: symlinks need a Unix host ('{link}' -> {target:?})");
        }

        let winner = self.winner.lock().unwrap().clone();
        if let Some(working_copy) = winner {
            tokio::fs::create_dir_all(&working_copy).await?;
            tokio::fs::write(working_copy.join("winner"), "").await?;
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "git_include_tests.rs"]
mod tests;
