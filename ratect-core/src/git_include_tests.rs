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

    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("Could not check out reference 'does-not-exist'"),
        "{message}"
    );

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
    // git's own default policy for `ext` is `never`, which is what refuses
    // it (ratect sets no transport policy of its own, as Batect sets none).
    // If this test ever fails because the marker file *was* created, that's
    // a command-injection regression, not a flaky test.
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
async fn system_git_client_leaves_transport_policy_to_git_itself() {
    // Batect's own `GitClient` sets no transport restriction, so git's
    // default policy applies: a remote helper and `ftp` are both `user`,
    // allowed on a directly invoked clone. Neither remote here can succeed
    // (no `git-remote-foo` is installed, nothing listens on port 1), so
    // what's asserted is *how* each fails — by git trying the transport,
    // not by refusing it.
    let destination = unique_temp_dir().join("clone");
    let helper_error = SystemGitClient
        .clone_repo("foo::bar", "v1.0.0", &destination)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        !helper_error.contains("transport 'foo' not allowed"),
        "{helper_error}"
    );
    assert!(helper_error.contains("remote-foo"), "{helper_error}");
    assert!(!destination.exists());

    let ftp_error = SystemGitClient
        .clone_repo("ftp://127.0.0.1:1/x", "v1.0.0", &destination)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        !ftp_error.contains("transport 'ftp' not allowed"),
        "{ftp_error}"
    );
    assert!(ftp_error.contains("unable to access"), "{ftp_error}");
    assert!(!destination.exists());

    std::fs::remove_dir_all(destination.parent().unwrap()).ok();
}

#[tokio::test]
async fn system_git_client_does_not_fetch_a_file_url_submodule() {
    // A malicious bundle's `.gitmodules` can point a submodule at an
    // arbitrary local path via a `file://` URL. It is never fetched, over
    // that or any transport: `checkout --recurse-submodules` only updates
    // *active* submodules, and a fresh `--no-checkout` clone has none —
    // true of Batect's identical sequence too. A local-path top-level
    // `repo` is still cloned (the caller's own config value).
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
    // `clone_repo` succeeds, leaving the submodule's directory
    // uninitialized. The security property under test is that the
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
    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
    let cache = GitIncludeCache::for_test(root.clone(), git, 1000).with_version("4.5.6");

    let working_copy = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    assert_eq!(working_copy, root.join(&key));

    let info = read_sidecar(&root, &key).await;
    assert_eq!(info["type"].as_str(), Some("git"));
    assert_eq!(
        info["repo"]["remote"].as_str(),
        Some("https://example.com/repo.git")
    );
    assert_eq!(info["repo"]["ref"].as_str(), Some("v1.0.0"));
    assert_eq!(info["cloned_with_version"].as_str(), Some("4.5.6"));
    assert_eq!(info["last_used"].as_integer(), Some(1000));

    // A different version reusing the clone bumps last_used but keeps
    // everything else, in particular the version that actually cloned it.
    let cache2 =
        GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 2000).with_version("1.2.3");
    cache2
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();
    let info2 = read_sidecar(&root, &key).await;
    assert_eq!(info2["type"].as_str(), Some("git"));
    assert_eq!(
        info2["repo"]["remote"].as_str(),
        Some("https://example.com/repo.git")
    );
    assert_eq!(info2["repo"]["ref"].as_str(), Some("v1.0.0"));
    assert_eq!(info2["cloned_with_version"].as_str(), Some("4.5.6"));
    assert_eq!(info2["last_used"].as_integer(), Some(2000));

    tokio::fs::remove_dir_all(&root).await.ok();
}

async fn read_sidecar(root: &Path, key: &str) -> toml::Table {
    let content = tokio::fs::read_to_string(root.join(format!("{key}.toml")))
        .await
        .unwrap();
    toml::from_str(&content).unwrap()
}

/// A sidecar written by some other ratect version, carrying a key this one
/// doesn't know at the top level and another under `repo`.
const SIDECAR_WITH_UNKNOWN_KEYS: &str = r#"type = "git"
cloned_with_version = "4.5.6"
last_used = 1
future_top_level = "kept"

[repo]
remote = "https://example.com/repo.git"
ref = "v1.0.0"
future_nested = 42
"#;

/// Lays out one cache entry's files by hand: a working copy (if asked for)
/// and a sidecar (if given), returning the entry's key.
async fn prepare_entry(root: &Path, working_copy: bool, sidecar: Option<&str>) -> String {
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    tokio::fs::create_dir_all(root).await.unwrap();
    if working_copy {
        tokio::fs::create_dir_all(root.join(&key)).await.unwrap();
    }
    if let Some(sidecar) = sidecar {
        tokio::fs::write(root.join(format!("{key}.toml")), sidecar)
            .await
            .unwrap();
    }
    key
}

fn assert_unknown_keys_kept(info: &toml::Table) {
    assert_eq!(info["future_top_level"].as_str(), Some("kept"));
    assert_eq!(info["repo"]["future_nested"].as_integer(), Some(42));
    assert_eq!(info["cloned_with_version"].as_str(), Some("4.5.6"));
}

#[tokio::test]
async fn ensure_cached_keeps_sidecar_keys_it_does_not_recognise() {
    let root = unique_temp_dir();
    let key = prepare_entry(&root, true, Some(SIDECAR_WITH_UNKNOWN_KEYS)).await;
    let cache =
        GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 2000).with_version("1.2.3");

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    let info = read_sidecar(&root, &key).await;
    assert_unknown_keys_kept(&info);
    assert_eq!(info["last_used"].as_integer(), Some(2000));

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// The working copy's existence alone decides whether to clone.
#[tokio::test]
async fn ensure_cached_writes_a_fresh_sidecar_beside_a_working_copy_that_has_none() {
    let root = unique_temp_dir();
    let key = prepare_entry(&root, true, None).await;
    let git = FakeGitClient::new();
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 2000).with_version("1.2.3");

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(git.clone_count(), 0);
    let info = read_sidecar(&root, &key).await;
    assert_eq!(info["type"].as_str(), Some("git"));
    assert_eq!(
        info["repo"]["remote"].as_str(),
        Some("https://example.com/repo.git")
    );
    assert_eq!(info["repo"]["ref"].as_str(), Some("v1.0.0"));
    assert_eq!(info["cloned_with_version"].as_str(), Some("1.2.3"));
    assert_eq!(info["last_used"].as_integer(), Some(2000));

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A sidecar with no working copy is re-cloned, and the sidecar is
/// updated rather than replaced.
#[tokio::test]
async fn ensure_cached_re_clones_a_missing_working_copy_and_keeps_its_sidecar() {
    let root = unique_temp_dir();
    let key = prepare_entry(&root, false, Some(SIDECAR_WITH_UNKNOWN_KEYS)).await;
    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 2000).with_version("1.2.3");

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(git.clone_count(), 1);
    let info = read_sidecar(&root, &key).await;
    assert_unknown_keys_kept(&info);
    assert_eq!(info["last_used"].as_integer(), Some(2000));

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// Batect merges `lastUsed` into whatever the file holds; so does this.
#[tokio::test]
async fn ensure_cached_merges_last_used_into_a_sidecar_missing_a_known_field() {
    let root = unique_temp_dir();
    let key = prepare_entry(&root, true, Some("type = \"git\"\nlast_used = 1\n")).await;
    let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 2000);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    let info = read_sidecar(&root, &key).await;
    assert_eq!(info.len(), 2, "nothing should be added: {info:?}");
    assert_eq!(info["type"].as_str(), Some("git"));
    assert_eq!(info["last_used"].as_integer(), Some(2000));

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn list_reads_a_sidecar_carrying_keys_it_does_not_recognise() {
    let root = unique_temp_dir();
    let key = prepare_entry(&root, true, Some(SIDECAR_WITH_UNKNOWN_KEYS)).await;
    let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 2000);

    let listed = cache.list().await.unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, key);
    assert_eq!(listed[0].remote, "https://example.com/repo.git");
    assert_eq!(listed[0].git_ref, "v1.0.0");
    assert_eq!(listed[0].last_used, 1);

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

const LOCK_CONTENT: &[u8] = b"left behind by a killed process";

/// A cache whose entry `https://example.com/repo.git`@`v1.0.0` is already
/// cloned, with a sidecar last used at 500, but has a leftover lock file
/// beside it (holding `LOCK_CONTENT`) — as a process killed between the clone
/// and `release_lock` leaves it. The lock timeout is short, so a wait on that
/// lock fails fast instead of hanging. Returns the cache root and entry key.
async fn cloned_entry_with_a_stale_lock(
    git: FakeGitClient,
) -> (PathBuf, String, GitIncludeCache<FakeGitClient>) {
    let root = unique_temp_dir();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    tokio::fs::create_dir_all(root.join(&key)).await.unwrap();
    tokio::fs::write(root.join(format!("{key}.lock")), LOCK_CONTENT)
        .await
        .unwrap();
    let info = CacheInfo {
        kind: "git".to_string(),
        repo: CacheInfoRepo {
            remote: "https://example.com/repo.git".to_string(),
            git_ref: "v1.0.0".to_string(),
        },
        cloned_with_version: "0.0.0-earlier".to_string(),
        last_used: 500,
    };
    tokio::fs::write(
        root.join(format!("{key}.toml")),
        toml::to_string(&info).unwrap(),
    )
    .await
    .unwrap();

    let mut cache = GitIncludeCache::for_test(root.clone(), git, 1000);
    cache.lock_timeout = Duration::from_millis(250);
    (root, key, cache)
}

#[tokio::test]
async fn ensure_cached_ignores_a_stale_lock_beside_an_already_cloned_working_copy() {
    let git = FakeGitClient::new();
    let (root, key, cache) = cloned_entry_with_a_stale_lock(git.clone()).await;

    let working_copy = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(working_copy, root.join(&key));
    assert_eq!(git.clone_count(), 0);
    // Neither taken nor released: the lock guards the clone step only.
    let lock = tokio::fs::read(root.join(format!("{key}.lock")))
        .await
        .unwrap();
    assert_eq!(lock, LOCK_CONTENT);

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn ensure_cached_bumps_last_used_for_an_already_cloned_working_copy_beside_a_stale_lock() {
    let (root, key, cache) = cloned_entry_with_a_stale_lock(FakeGitClient::new()).await;

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    let info: CacheInfo = toml::from_str(
        &tokio::fs::read_to_string(root.join(format!("{key}.toml")))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info.last_used, 1000);
    assert_eq!(info.cloned_with_version, "0.0.0-earlier");

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// The entry `cloned_entry_with_a_stale_lock` set up is gone, and its lock
/// file is exactly as it was left: removal neither takes, releases nor
/// deletes it.
async fn assert_removed_beside_its_untouched_lock(root: &Path, key: &str) {
    assert!(!root.join(key).exists(), "working copy should be removed");
    assert!(
        !root.join(format!("{key}.toml")).exists(),
        "sidecar should be removed"
    );
    let lock = tokio::fs::read(root.join(format!("{key}.lock")))
        .await
        .unwrap();
    assert_eq!(lock, LOCK_CONTENT);
}

#[tokio::test]
async fn clean_removes_an_entry_beside_a_stale_lock_without_waiting_on_it() {
    let (root, key, cache) = cloned_entry_with_a_stale_lock(FakeGitClient::new()).await;

    let start = Instant::now();
    let removed = cache.clean(None).await.unwrap().succeeded;

    assert!(
        start.elapsed() < cache.lock_timeout,
        "clean waited on the lock"
    );
    assert_eq!(removed.len(), 1);
    assert_removed_beside_its_untouched_lock(&root, &key).await;

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn cleanup_stale_removes_an_entry_beside_a_stale_lock_without_waiting_on_it() {
    let (root, key, mut cache) = cloned_entry_with_a_stale_lock(FakeGitClient::new()).await;
    cache.clock = Box::new(|| 500 + STALE_AFTER.as_secs() + 1);

    let start = Instant::now();
    cache.cleanup_stale().await.unwrap();

    assert!(
        start.elapsed() < cache.lock_timeout,
        "the sweep waited on the lock"
    );
    assert_removed_beside_its_untouched_lock(&root, &key).await;

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A lock file at `root`'s entry for `https://example.com/repo.git`@`v1.0.0`
/// carrying the current marker, as a holder that has since exited leaves it:
/// its OS lock released with the process.
async fn abandoned_lock(root: &Path) -> PathBuf {
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    tokio::fs::create_dir_all(root).await.unwrap();
    let lock_path = root.join(format!("{key}.lock"));
    tokio::fs::write(&lock_path, format!("{}\n", entry_lock::MARKER))
        .await
        .unwrap();
    lock_path
}

/// The same lock with its holder still running: the returned file holds its
/// OS lock until dropped.
async fn lock_held_by_a_running_holder(root: &Path) -> (PathBuf, std::fs::File) {
    let lock_path = abandoned_lock(root).await;
    let file = std::fs::File::open(&lock_path).unwrap();
    file.lock().unwrap();
    (lock_path, file)
}

fn fake_with_the_entry() -> FakeGitClient {
    FakeGitClient::new().with_files(
        "https://example.com/repo.git",
        "v1.0.0",
        HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
    )
}

#[tokio::test]
async fn ensure_cached_takes_over_an_abandoned_lock_and_clones_without_waiting() {
    let root = unique_temp_dir();
    let lock_path = abandoned_lock(&root).await;
    let git = fake_with_the_entry();
    let (output, stderr, _) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000).with_output(output);

    let start = Instant::now();
    let working_copy = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert!(start.elapsed() < cache.lock_timeout, "waited on the lock");
    assert_eq!(git.clone_count(), 1);
    assert!(working_copy.join("bundle.yml").exists());
    assert!(
        !lock_path.exists(),
        "the taken-over lock should be released"
    );
    assert_eq!(stderr.contents(), "", "a takeover is not a wait");

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// #733: removal leaves the lock (Decision 6), and the next load takes it
/// over rather than timing out.
#[tokio::test]
async fn a_load_after_clean_takes_over_the_abandoned_lock_clean_left() {
    let git = fake_with_the_entry();
    let (root, key, mut cache) = cloned_entry_with_a_stale_lock(git.clone()).await;
    cache.lock_timeout = Duration::from_secs(5);
    abandoned_lock(&root).await;

    cache.clean(None).await.unwrap();
    assert!(
        root.join(format!("{key}.lock")).exists(),
        "clean kept the lock"
    );
    let start = Instant::now();
    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert!(start.elapsed() < cache.lock_timeout, "waited on the lock");
    assert_eq!(git.clone_count(), 1);
    assert!(!root.join(format!("{key}.lock")).exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn refresh_takes_over_an_abandoned_lock_and_reports_the_entry_refreshed() {
    let git = fake_with_the_entry();
    let (root, key, mut cache) = cloned_entry_with_a_stale_lock(git.clone()).await;
    cache.lock_timeout = Duration::from_secs(5);
    abandoned_lock(&root).await;

    let refreshed = cache.refresh().await.unwrap().succeeded;

    assert_eq!(refreshed.len(), 1);
    assert_eq!(git.clone_count(), 1);
    assert!(root.join(&key).join("bundle.yml").exists());
    assert!(!root.join(format!("{key}.lock")).exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn a_lock_whose_holder_is_running_is_waited_on_announced_once_and_timed_out() {
    let root = unique_temp_dir();
    let (lock_path, _held) = lock_held_by_a_running_holder(&root).await;
    let git = fake_with_the_entry();
    let (output, stderr, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Quiet);
    let mut cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000).with_output(output);
    cache.lock_timeout = Duration::from_millis(350);

    let error = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(git.clone_count(), 0);
    assert!(lock_path.exists());
    assert_eq!(
        stderr.contents(),
        "Waiting for another ratect process to finish cloning Git include \
         'https://example.com/repo.git' at 'v1.0.0' (giving up after 350ms).\n",
        "announced once, on stderr, under -o quiet"
    );
    assert_eq!(progress.contents(), "");
    assert!(
        error.starts_with("Timed out after 350ms waiting for another ratect process"),
        "{error}"
    );
    assert!(
        error.ends_with("It is still running: wait for it to finish, or try again."),
        "{error}"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// An older binary's lock: empty, so its holder can't be proved to have
/// exited, however old it is.
#[tokio::test]
async fn an_unmarked_lock_is_never_taken_over_and_its_timeout_names_the_file() {
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    let lock_path = root.join(format!("{key}.lock"));
    tokio::fs::write(&lock_path, b"").await.unwrap();
    std::fs::File::open(&lock_path)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let git = fake_with_the_entry();
    let (output, stderr, _) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let mut cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000).with_output(output);
    cache.lock_timeout = Duration::from_millis(250);

    let error = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(git.clone_count(), 0);
    assert_eq!(tokio::fs::read(&lock_path).await.unwrap(), b"");
    assert_eq!(
        stderr.contents(),
        format!(
            "Waiting for lock file {lock_path:?} on Git include 'https://example.com/repo.git' \
             at 'v1.0.0': ratect can't tell whether the process that holds it is still running \
             (giving up after 250ms).\n"
        )
    );
    assert!(
        error.starts_with(&format!(
            "Timed out after 250ms waiting for lock file {lock_path:?}"
        )),
        "{error}"
    );
    assert!(
        error.ends_with("it is safe to delete that file."),
        "{error}"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A first look between a new holder creating its file and marking it finds
/// it unmarked; the notice waits for a second look rather than calling a
/// running holder one that might be gone.
#[tokio::test]
async fn a_first_look_at_a_lock_not_yet_marked_does_not_announce_an_unknown_holder() {
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    let lock_path = root.join(format!("{key}.lock"));
    tokio::fs::write(&lock_path, b"").await.unwrap();
    let holder_path = lock_path.clone();
    let holder = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&holder_path)
            .unwrap();
        file.lock().unwrap();
        std::io::Write::write_all(&mut file, format!("{}\n", entry_lock::MARKER).as_bytes())
            .unwrap();
        file
    });
    let (output, stderr, _) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let mut cache =
        GitIncludeCache::for_test(root.clone(), fake_with_the_entry(), 1000).with_output(output);
    cache.lock_timeout = Duration::from_millis(350);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap_err();

    assert!(
        stderr
            .contents()
            .starts_with("Waiting for another ratect process to finish cloning"),
        "{}",
        stderr.contents()
    );
    drop(holder.await.unwrap());
    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn a_takeover_rewrites_the_marker_without_emptying_the_file_first() {
    let root = unique_temp_dir();
    let lock_path = abandoned_lock(&root).await;
    tokio::fs::write(&lock_path, format!("{}\nmore\n", entry_lock::MARKER))
        .await
        .unwrap();

    let lock = entry_lock::attempt(&lock_path).unwrap();

    assert!(matches!(lock, entry_lock::Attempt::Acquired(_)), "{lock:?}");
    assert_eq!(
        tokio::fs::read_to_string(&lock_path).await.unwrap(),
        format!("{}\nmore\n", entry_lock::MARKER)
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_waiters_on_one_abandoned_lock_clone_once() {
    let root = unique_temp_dir();
    abandoned_lock(&root).await;
    let git = fake_with_the_entry();
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000);

    let (first, second) = tokio::join!(
        cache.ensure_cached("https://example.com/repo.git", "v1.0.0"),
        cache.ensure_cached("https://example.com/repo.git", "v1.0.0"),
    );

    assert_eq!(first.unwrap(), second.unwrap());
    assert_eq!(git.clone_count(), 1);

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn only_one_of_two_looks_at_an_abandoned_lock_takes_it_over() {
    let root = unique_temp_dir();
    let lock_path = abandoned_lock(&root).await;

    let first = entry_lock::attempt(&lock_path).unwrap();
    let second = entry_lock::attempt(&lock_path).unwrap();

    assert!(
        matches!(first, entry_lock::Attempt::Acquired(_)),
        "{first:?}"
    );
    assert!(
        matches!(second, entry_lock::Attempt::HeldByRunningHolder),
        "{second:?}"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A waiter that opened the lock file just before its holder released it
/// would otherwise hold an OS lock on a file nobody else can see.
#[tokio::test]
async fn a_waiter_holding_a_released_lock_file_looks_again_rather_than_taking_it_over() {
    let root = unique_temp_dir();
    let lock_path = abandoned_lock(&root).await;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let entry_lock::Attempt::Acquired(holder) = entry_lock::attempt(&lock_path).unwrap() else {
        panic!("the abandoned lock should be taken over");
    };
    holder.release().unwrap();

    let gone = entry_lock::take_over_if_abandoned(opened, &lock_path);
    assert!(matches!(gone, entry_lock::TakeOver::Replaced), "{gone:?}");

    // And after a new holder has created the file again.
    let opened = std::fs::File::open(abandoned_lock(&root).await).unwrap();
    std::fs::remove_file(&lock_path).unwrap();
    let entry_lock::Attempt::Acquired(_new_holder) = entry_lock::attempt(&lock_path).unwrap()
    else {
        panic!("a free lock should be taken");
    };
    let replaced = entry_lock::take_over_if_abandoned(opened, &lock_path);
    assert!(
        matches!(replaced, entry_lock::TakeOver::Replaced),
        "{replaced:?}"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

fn is_close_on_exec(file: &std::fs::File) -> bool {
    let flags = nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_GETFD).unwrap();
    nix::fcntl::FdFlag::from_bits_truncate(flags).contains(nix::fcntl::FdFlag::FD_CLOEXEC)
}

/// Decision 2: the `git` a holder starts mustn't inherit its OS lock, or a
/// `git` outliving its holder would keep the lock from ever being abandoned.
#[tokio::test]
async fn an_entry_lock_is_held_through_a_close_on_exec_descriptor() {
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let created = entry_lock::attempt(&root.join("created.lock")).unwrap();
    let taken_over = entry_lock::attempt(&abandoned_lock(&root).await).unwrap();

    for attempt in [created, taken_over] {
        let entry_lock::Attempt::Acquired(lock) = attempt else {
            panic!("expected to hold the lock, found {attempt:?}");
        };
        assert!(is_close_on_exec(lock.file()));
    }

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn a_new_holder_marks_its_lock_file() {
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let lock_path = root.join("new.lock");

    let _lock = entry_lock::attempt(&lock_path).unwrap();

    assert_eq!(
        tokio::fs::read_to_string(&lock_path).await.unwrap(),
        format!("{}\n", entry_lock::MARKER)
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

fn staging_directories(root: &Path, key: &str) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&format!("{key}.tmp-")))
        .collect()
}

#[tokio::test]
async fn a_clone_that_loses_the_rename_uses_the_winners_working_copy() {
    let root = unique_temp_dir();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    let git = fake_with_the_entry().losing_the_rename_to(root.join(&key));
    let cache = GitIncludeCache::for_test(root.clone(), git, 1000);

    let working_copy = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(working_copy, root.join(&key));
    assert!(working_copy.join("winner").exists());
    assert!(!working_copy.join("bundle.yml").exists());
    assert_eq!(staging_directories(&root, &key), Vec::<String>::new());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn a_failed_clone_leaves_no_staging_directory() {
    let root = unique_temp_dir();
    let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new().failing("no"), 1000);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap_err();

    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    assert_eq!(staging_directories(&root, &key), Vec::<String>::new());

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// Decision 7: under the lock, a leftover staging directory belongs to an
/// interrupted clone, so the next holder removes it — but only its own
/// entry's, and nothing outside the lock does.
#[tokio::test]
async fn the_next_holder_removes_its_entrys_leftover_staging_directories() {
    let root = unique_temp_dir();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    let other = cache_key("https://example.com/other.git", "v1.0.0");
    for name in [
        format!("{key}.tmp-leftover"),
        format!("{other}.tmp-leftover"),
    ] {
        tokio::fs::create_dir_all(root.join(name).join(".git"))
            .await
            .unwrap();
    }
    let cache = GitIncludeCache::for_test(root.clone(), fake_with_the_entry(), 1000);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(staging_directories(&root, &key), Vec::<String>::new());
    assert!(root.join(format!("{other}.tmp-leftover")).exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn removal_and_the_sweep_leave_a_staging_directory_alone() {
    let (root, key, mut cache) = cloned_entry_with_a_stale_lock(FakeGitClient::new()).await;
    let staging = root.join(format!("{key}.tmp-leftover"));
    tokio::fs::create_dir_all(&staging).await.unwrap();

    cache.clean(None).await.unwrap();
    cache.clock = Box::new(|| 500 + STALE_AFTER.as_secs() + 1);
    cache.cleanup_stale().await.unwrap();

    assert!(staging.exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A load whose `exists()` check passed just before a concurrent removal
/// returns the removed path, then writes its sidecar after the removal —
/// leaving a sidecar with no working copy. No lock prevents that (#631);
/// it repairs itself, which the two tests below pin.
async fn sidecar_left_by_a_load_racing_a_removal(
    git: FakeGitClient,
) -> (PathBuf, String, GitIncludeCache<FakeGitClient>) {
    let root = unique_temp_dir();
    let cache = GitIncludeCache::for_test(root.clone(), git, 1_000);
    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    let info_path = root.join(format!("{key}.toml"));

    cache.clean(None).await.unwrap();
    cache
        .update_info_file("https://example.com/repo.git", "v1.0.0", &info_path)
        .await
        .unwrap();

    assert!(!root.join(&key).exists());
    assert!(info_path.exists());
    (root, key, cache)
}

#[tokio::test]
async fn the_next_load_re_clones_an_entry_removed_beneath_a_load() {
    let git = FakeGitClient::new().with_files(
        "https://example.com/repo.git",
        "v1.0.0",
        HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
    );
    let (root, key, cache) = sidecar_left_by_a_load_racing_a_removal(git.clone()).await;

    let working_copy = cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(git.clone_count(), 2);
    assert_eq!(working_copy, root.join(&key));
    assert!(working_copy.join("bundle.yml").exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn cleanup_stale_removes_a_sidecar_left_without_a_working_copy() {
    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
    let (root, key, _) = sidecar_left_by_a_load_racing_a_removal(git).await;

    let now = 1_000 + STALE_AFTER.as_secs() + 1;
    let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
    sweeper.cleanup_stale().await.unwrap();

    assert!(!root.join(format!("{key}.toml")).exists());

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

/// `last_used` is whole seconds, so every include a single run resolves shares
/// one — the common case, not a corner. Sorting on it alone leaves those tied
/// and hands the listing whatever order `read_dir` yielded, which is neither
/// stable across runs nor the same on two filesystems.
#[tokio::test]
async fn entries_sharing_a_second_are_listed_in_a_stable_order() {
    let root = unique_temp_dir();
    let remotes: Vec<String> = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .iter()
        .map(|name| format!("https://example.com/{name}.git"))
        .collect();
    let git = remotes.iter().fold(FakeGitClient::new(), |git, remote| {
        git.with_files(
            remote,
            "v1.0.0",
            HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
        )
    });

    // One cache, so one clock reading: every entry lands on the same second.
    let cache = GitIncludeCache::for_test(root.clone(), git, 1_000);
    for remote in &remotes {
        cache.ensure_cached(remote, "v1.0.0").await.unwrap();
    }

    let listed = cache.list().await.unwrap();

    assert_eq!(listed.len(), 6);
    assert!(
        listed.iter().all(|entry| entry.last_used == 1_000),
        "the fixture only proves anything while every entry ties: {listed:?}"
    );
    let keys: Vec<&str> = listed.iter().map(|entry| entry.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "tied entries were left in directory order");

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
    let removed = cache
        .clean(Some(Duration::from_secs(3_000)))
        .await
        .unwrap()
        .succeeded;

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

    let removed = cache.clean(None).await.unwrap().succeeded;

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

    let refreshed = cache.refresh().await.unwrap().succeeded;

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

/// #755: a re-clone that fails is returned as a failure, with its cause, for
/// the caller to report — not only logged — and the rest are still
/// refreshed. The failed entry stays removed, as `refresh` documents.
#[tokio::test]
async fn refresh_returns_an_entry_it_could_not_re_clone_as_failed() {
    let root = unique_temp_dir();
    cache_with_two_entries(&root).await;
    // The old remote has gone: only the new one can be cloned again.
    let git = FakeGitClient::new().with_files(
        "https://example.com/new.git",
        "v2.0.0",
        HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
    );
    let cache = GitIncludeCache::for_test(root.clone(), git, 5_000);

    let outcome = cache.refresh().await.unwrap();

    let succeeded: Vec<_> = outcome.succeeded.iter().map(|e| &e.remote).collect();
    assert_eq!(succeeded, ["https://example.com/new.git"]);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(
        outcome.failed[0].entry.remote,
        "https://example.com/old.git"
    );
    assert_eq!(outcome.failed[0].entry.git_ref, "v1.0.0");
    let cause = format!("{:#}", outcome.failed[0].error);
    assert!(cause.contains("no response configured"), "{cause}");
    assert_eq!(cache.list().await.unwrap().len(), 1);

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// #755: an entry whose files can't be removed is returned as a failure,
/// with its cause, and the rest are still removed.
#[cfg(unix)]
#[tokio::test]
async fn clean_returns_an_entry_it_could_not_remove_as_failed() {
    use std::os::unix::fs::PermissionsExt;

    let root = unique_temp_dir();
    let cache = cache_with_two_entries(&root).await;
    // A file in a read-only directory can't be deleted, so neither can the
    // working copy holding it.
    let sealed = root
        .join(cache_key("https://example.com/old.git", "v1.0.0"))
        .join("sealed");
    std::fs::create_dir(&sealed).unwrap();
    std::fs::write(sealed.join("file"), "").unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o555)).unwrap();

    let outcome = cache.clean(None).await;
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();
    let outcome = outcome.unwrap();

    let succeeded: Vec<_> = outcome.succeeded.iter().map(|e| &e.remote).collect();
    assert_eq!(succeeded, ["https://example.com/new.git"]);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(
        outcome.failed[0].entry.remote,
        "https://example.com/old.git"
    );
    let cause = format!("{:#}", outcome.failed[0].error);
    assert!(
        cause.starts_with("Failed to remove Git include cache clone"),
        "{cause}"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn cleanup_stale_removes_an_entry_unused_for_more_than_30_days() {
    let root = unique_temp_dir();
    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
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
    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
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

    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
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

/// Caches `remote`@`v1.0.0` in `root` as last used at `last_used`, returning
/// its key, working copy and sidecar paths.
async fn cached_entry_used_at(
    root: &Path,
    remote: &str,
    last_used: u64,
) -> (String, PathBuf, PathBuf) {
    let git = FakeGitClient::new().with_files(remote, "v1.0.0", HashMap::new());
    let cache = GitIncludeCache::for_test(root.to_path_buf(), git, last_used);
    let working_copy = cache.ensure_cached(remote, "v1.0.0").await.unwrap();
    let key = cache_key(remote, "v1.0.0");
    let info_path = root.join(format!("{key}.toml"));
    (key, working_copy, info_path)
}

#[tokio::test]
async fn cleanup_stale_removes_only_the_stale_entries_among_several() {
    let root = unique_temp_dir();
    let now = 10_000 + STALE_AFTER.as_secs();
    let stale = [
        cached_entry_used_at(&root, "https://example.com/stale-a.git", 1_000).await,
        cached_entry_used_at(&root, "https://example.com/stale-b.git", 9_999).await,
    ];
    // Exactly `STALE_AFTER` old is still fresh: only older than it is stale.
    let fresh = [
        cached_entry_used_at(&root, "https://example.com/fresh-a.git", 10_000).await,
        cached_entry_used_at(&root, "https://example.com/fresh-b.git", now).await,
    ];

    let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
    sweeper.cleanup_stale().await.unwrap();

    for (_, working_copy, info_path) in &stale {
        assert!(!working_copy.exists(), "{working_copy:?} should be removed");
        assert!(!info_path.exists(), "{info_path:?} should be removed");
    }
    for (_, working_copy, info_path) in &fresh {
        assert!(working_copy.exists(), "{working_copy:?} should be kept");
        assert!(info_path.exists(), "{info_path:?} should be kept");
    }

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A regular file where the working copy should be is the forced failure:
/// `remove_dir_all` refuses a non-directory, even when the tests run as
/// root, which an unwritable directory doesn't. The sweep visits entries in
/// `read_dir` order, which the test can't choose, so each entry takes a turn as
/// the failing one: unless that order differs between the two runs, one of
/// them puts the failure first, which is the order that matters.
#[tokio::test]
async fn cleanup_stale_still_removes_other_stale_entries_when_one_removal_fails() {
    let remotes = ["https://example.com/a.git", "https://example.com/b.git"];
    for (failing, other) in [(remotes[0], remotes[1]), (remotes[1], remotes[0])] {
        let root = unique_temp_dir();
        let (failing_key, failing_copy, failing_info) =
            cached_entry_used_at(&root, failing, 1_000).await;
        let (other_key, other_copy, other_info) = cached_entry_used_at(&root, other, 1_000).await;
        tokio::fs::remove_dir_all(&failing_copy).await.unwrap();
        tokio::fs::write(&failing_copy, b"not a directory")
            .await
            .unwrap();

        let now = 1_000 + STALE_AFTER.as_secs() + 1;
        let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
        sweeper.cleanup_stale().await.unwrap();

        assert!(failing_copy.exists(), "{failing} should be left in place");
        assert!(failing_info.exists(), "{failing} should be left in place");
        assert!(!other_copy.exists(), "{other} should be removed");
        assert!(!other_info.exists(), "{other} should be removed");

        // The per-entry results the sweep ignores.
        cached_entry_used_at(&root, other, 1_000).await;
        assert!(sweeper.remove_entry(&root, &failing_key).await.is_err());
        sweeper.remove_entry(&root, &other_key).await.unwrap();

        tokio::fs::remove_dir_all(&root).await.ok();
    }
}

#[tokio::test]
async fn remove_entry_files_succeeds_when_neither_file_exists() {
    let root = unique_temp_dir();

    remove_entry_files(&root.join("absent"), &root.join("absent.toml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn remove_entry_files_removes_a_sidecar_with_no_working_copy() {
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let info_path = root.join("orphan.toml");
    tokio::fs::write(&info_path, b"").await.unwrap();

    remove_entry_files(&root.join("orphan"), &info_path)
        .await
        .unwrap();
    assert!(!info_path.exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn a_stray_file_in_the_cache_root_is_neither_listed_nor_swept() {
    let root = unique_temp_dir();
    let (_, working_copy, _) =
        cached_entry_used_at(&root, "https://example.com/repo.git", 1_000).await;
    let stray = root.join("something-else.txt");
    tokio::fs::write(&stray, b"not an entry").await.unwrap();

    let now = 1_000 + STALE_AFTER.as_secs() + 1;
    let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
    let listed = sweeper.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, working_copy);

    sweeper.cleanup_stale().await.unwrap();
    assert!(!working_copy.exists());
    assert!(stray.exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// An in-progress clone, or a leftover temporary directory, has no sidecar
/// yet — so it isn't an entry, and the sweep has no business removing it.
#[tokio::test]
async fn a_directory_without_a_sidecar_is_neither_listed_nor_swept() {
    let root = unique_temp_dir();
    let (_, working_copy, _) =
        cached_entry_used_at(&root, "https://example.com/repo.git", 1_000).await;
    let in_progress = root.join(format!(
        "{}.tmp",
        cache_key("https://example.com/other.git", "v1.0.0")
    ));
    tokio::fs::create_dir_all(&in_progress).await.unwrap();

    let now = 1_000 + STALE_AFTER.as_secs() + 1;
    let sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
    let listed = sweeper.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, working_copy);

    sweeper.cleanup_stale().await.unwrap();
    assert!(!working_copy.exists());
    assert!(in_progress.exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// Set to a lock path, makes [`a_lock_holder_process`] take that lock and
/// hold it until killed.
const LOCK_HOLDER_ENV: &str = "RATECT_TEST_ENTRY_LOCK_HOLDER";

/// What [`a_lock_holder_process`] prints once it holds the lock.
const LOCK_HELD: &str = "entry-lock-held";

/// Not a test on its own: does nothing unless [`LOCK_HOLDER_ENV`] is set, as
/// it is when the test below runs this binary again to get a holder in a
/// process of its own.
#[test]
fn a_lock_holder_process() {
    let Some(path) = std::env::var_os(LOCK_HOLDER_ENV) else {
        return;
    };
    let lock = entry_lock::attempt(Path::new(&path)).unwrap();
    assert!(matches!(lock, entry_lock::Attempt::Acquired(_)), "{lock:?}");
    // libtest has already printed `test <name> ... ` without a newline, so
    // the parent looks for this anywhere in a line.
    let mut stdout = std::io::stdout();
    std::io::Write::write_all(&mut stdout, format!("{LOCK_HELD}\n").as_bytes()).unwrap();
    std::io::Write::flush(&mut stdout).unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

/// Decision 3's premise, with a real process rather than a second open file
/// standing in for one: the kernel releases a holder's OS lock when the holder
/// is killed, so its lock is then taken over.
#[test]
fn a_lock_whose_holder_process_was_killed_is_taken_over() {
    use std::io::BufRead;

    let root = unique_temp_dir();
    std::fs::create_dir_all(&root).unwrap();
    let lock_path = root.join("entry.lock");
    let mut holder = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "git_include::tests::a_lock_holder_process",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(LOCK_HOLDER_ENV, &lock_path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = std::io::BufReader::new(holder.stdout.take().unwrap());
    assert!(
        stdout.lines().any(|line| line.unwrap().contains(LOCK_HELD)),
        "the holder process never took the lock"
    );

    let while_running = entry_lock::attempt(&lock_path).unwrap();
    holder.kill().unwrap();
    holder.wait().unwrap();
    let after_kill = entry_lock::attempt(&lock_path).unwrap();

    assert!(
        matches!(while_running, entry_lock::Attempt::HeldByRunningHolder),
        "{while_running:?}"
    );
    assert!(
        matches!(after_kill, entry_lock::Attempt::Acquired(_)),
        "{after_kill:?}"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// #420: a clone is announced as progress before it starts, and followed by a
/// blank line once it has succeeded, as Batect's
/// `DefaultGitRepositoryCacheNotificationListener` does.
#[tokio::test]
async fn a_clone_is_announced_as_progress_and_followed_by_a_blank_line() {
    let root = unique_temp_dir();
    let (output, stderr, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let cache =
        GitIncludeCache::for_test(root.clone(), fake_with_the_entry(), 1000).with_output(output);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(
        progress.contents(),
        "Cloning https://example.com/repo.git v1.0.0...\n\n"
    );
    assert_eq!(stderr.contents(), "");

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn a_load_that_finds_the_entry_cloned_prints_nothing() {
    let root = unique_temp_dir();
    let git = fake_with_the_entry();
    GitIncludeCache::for_test(root.clone(), git.clone(), 1000)
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();
    let (output, stderr, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000).with_output(output);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(git.clone_count(), 1);
    assert_eq!(progress.contents(), "");
    assert_eq!(stderr.contents(), "");

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// A failed clone is reported as an error, so it gets no blank line.
#[tokio::test]
async fn a_failed_clone_is_announced_but_not_followed_by_a_blank_line() {
    let root = unique_temp_dir();
    let (output, _, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new().failing("boom"), 1000)
        .with_output(output);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap_err();

    assert_eq!(
        progress.contents(),
        "Cloning https://example.com/repo.git v1.0.0...\n"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// The clone ran, so it is announced, even though the working copy that ends
/// up in place is the one a concurrent clone renamed there first.
#[tokio::test]
async fn a_clone_that_loses_its_rename_is_still_announced_as_complete() {
    let root = unique_temp_dir();
    let key = cache_key("https://example.com/repo.git", "v1.0.0");
    let git = fake_with_the_entry().losing_the_rename_to(root.join(&key));
    let (output, _, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let cache = GitIncludeCache::for_test(root.clone(), git, 1000).with_output(output);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();

    assert_eq!(
        progress.contents(),
        "Cloning https://example.com/repo.git v1.0.0...\n\n"
    );

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// Unlike Batect, which announces the clone before its lock re-check: a load
/// that waited while another process cloned the entry announces only its
/// wait, not a clone it then skips.
#[tokio::test]
async fn a_load_that_waited_for_another_process_to_clone_does_not_announce_a_clone() {
    let root = unique_temp_dir();
    let (lock_path, held) = lock_held_by_a_running_holder(&root).await;
    let working_copy = root.join(cache_key("https://example.com/repo.git", "v1.0.0"));
    let (output, stderr, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    // Finishes its clone only once the load has announced its wait, so the
    // load can't find the working copy before it has waited.
    let announced = stderr.clone();
    let holder = tokio::spawn(async move {
        while announced.contents().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::fs::create_dir_all(&working_copy).await.unwrap();
        drop(held);
        tokio::fs::remove_file(&lock_path).await.unwrap();
    });
    let git = fake_with_the_entry();
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000).with_output(output);

    cache
        .ensure_cached("https://example.com/repo.git", "v1.0.0")
        .await
        .unwrap();
    holder.await.unwrap();

    assert_eq!(git.clone_count(), 0);
    assert!(
        stderr
            .contents()
            .starts_with("Waiting for another ratect process"),
        "{}",
        stderr.contents()
    );
    assert_eq!(progress.contents(), "");

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// #751: credentials in a remote are shown as `***`; everything else as
/// written.
#[test]
fn display_remote_hides_credentials_and_keeps_everything_else_as_written() {
    for (remote, shown) in [
        ("https://u:p@host/x.git", "https://***@host/x.git"),
        (
            "https://TOKEN@github.com/o/r.git",
            "https://***@github.com/o/r.git",
        ),
        ("ssh://git@host/r.git", "ssh://***@host/r.git"),
        ("https://u:p@ss@host/x", "https://***@host/x"),
        ("https://u:p@host", "https://***@host"),
        ("https://u:p@host?q", "https://***@host?q"),
        ("https://u:pa#ss@host/x", "https://***@host/x"),
        ("https://u:to?ken@host/x", "https://***@host/x"),
        ("helper::https://u:p@host/x", "helper::https://***@host/x"),
        ("git@github.com:o/r.git", "git@github.com:o/r.git"),
        ("/srv/git/r.git", "/srv/git/r.git"),
        ("https://GitHub.com/O/r", "https://GitHub.com/O/r"),
        ("https://host/a@b", "https://host/a@b"),
    ] {
        assert_eq!(display_remote(remote), shown, "{remote}");
    }
}

/// #751: the progress line and the lock notice name the include without its
/// credentials; the clone itself is given them.
#[tokio::test]
async fn the_clone_line_and_lock_notice_hide_a_remotes_credentials() {
    let remote = "https://u:s3cret@example.com/repo.git";
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let lock_path = root.join(format!("{}.lock", cache_key(remote, "v1.0.0")));
    tokio::fs::write(&lock_path, b"").await.unwrap();
    let git = FakeGitClient::new().with_files(
        remote,
        "v1.0.0",
        HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
    );
    let (output, stderr, progress) = LoadOutput::capturing(crate::ui::OutputStyle::Simple);
    let mut cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000).with_output(output);
    cache.lock_timeout = Duration::from_millis(250);

    let timeout = cache
        .ensure_cached(remote, "v1.0.0")
        .await
        .unwrap_err()
        .to_string();
    tokio::fs::remove_file(&lock_path).await.unwrap();
    cache.ensure_cached(remote, "v1.0.0").await.unwrap();

    assert_eq!(git.clone_count(), 1, "cloned with the full remote");
    for shown in [timeout, stderr.contents(), progress.contents()] {
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(
            shown.contains("https://***@example.com/repo.git"),
            "{shown}"
        );
    }

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn system_git_client_hides_a_remotes_credentials_in_its_errors() {
    let destination = unique_temp_dir().join("clone");

    let error = SystemGitClient
        .clone_repo(
            "https://u:s3cret@127.0.0.1:9/repo.git",
            "v1.0.0",
            &destination,
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(!error.contains("s3cret"), "{error}");
    assert!(
        error.starts_with("Could not clone repository 'https://***@127.0.0.1:9/repo.git'"),
        "{error}"
    );
    std::fs::remove_dir_all(destination.parent().unwrap()).ok();
}

/// #751: only what is shown is redacted — the sidecar keeps the remote in
/// full, so a refresh can clone it again.
#[tokio::test]
async fn refresh_reclones_a_credential_bearing_entry_with_its_full_remote() {
    let remote = "https://u:s3cret@example.com/repo.git";
    let root = unique_temp_dir();
    let git = FakeGitClient::new().with_files(
        remote,
        "v1.0.0",
        HashMap::from([("bundle.yml".to_string(), "tasks: {}".to_string())]),
    );
    let cache = GitIncludeCache::for_test(root.clone(), git.clone(), 1000);
    cache.ensure_cached(remote, "v1.0.0").await.unwrap();

    let refreshed = cache.refresh().await.unwrap().succeeded;

    assert_eq!(git.clone_count(), 2);
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].remote, remote);

    tokio::fs::remove_dir_all(&root).await.ok();
}

/// #751: toml's own error quotes the line it failed on, and serde's type
/// errors quote the value, either of which in a sidecar can be the remote in
/// full. A load's error and `list`'s warning carry neither.
#[tokio::test]
async fn a_sidecar_that_fails_to_parse_is_reported_without_its_contents() {
    let remote = "https://u:s3cret@example.com/repo.git";
    let root = unique_temp_dir();
    let key = cache_key(remote, "v1.0.0");
    let sidecar = root.join(format!("{key}.toml"));
    tokio::fs::create_dir_all(root.join(&key)).await.unwrap();
    tokio::fs::write(&sidecar, format!("[repo]\nremote = \"{remote}\" oops\n"))
        .await
        .unwrap();
    let cache = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), 1000);

    let error = format!(
        "{:#}",
        cache.ensure_cached(remote, "v1.0.0").await.unwrap_err()
    );

    assert_eq!(
        error,
        format!(
            "Failed to parse Git include cache info file {sidecar:?} at line 2; delete it to \
             have it written again"
        )
    );
    // What `list` reads it as: a value of the wrong type is quoted by serde.
    let wrong_type = format!("kind = \"git\"\nrepo = \"{remote}\"\n");
    let parsed = toml::from_str::<CacheInfo>(&wrong_type).unwrap_err();
    assert!(parsed.to_string().contains("s3cret"), "the case is real");
    assert_eq!(describe_parse_error(&parsed, &wrong_type), "at line 2");
    // As the error says: deleted, it is written again.
    tokio::fs::remove_file(&sidecar).await.unwrap();
    cache.ensure_cached(remote, "v1.0.0").await.unwrap();
    assert!(sidecar.exists());

    tokio::fs::remove_dir_all(&root).await.ok();
}

#[tokio::test]
async fn system_git_client_hides_credentials_when_refusing_a_remote_starting_with_a_dash() {
    let error = SystemGitClient
        .clone_repo(
            "-https://u:s3cret@example.com/r.git",
            "v1.0.0",
            &unique_temp_dir().join("clone"),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(!error.contains("s3cret"), "{error}");
}

/// #751: a checkout failure names the repository without its credentials.
/// `file://` accepts userinfo and ignores it, so the clone itself succeeds.
#[tokio::test]
async fn system_git_client_hides_credentials_when_a_checkout_fails() {
    let repo_dir = create_test_repo();
    let destination = unique_temp_dir().join("clone");
    let remote = format!("file://u:s3cret@localhost{}", repo_dir.display());

    let error = SystemGitClient
        .clone_repo(&remote, "does-not-exist", &destination)
        .await
        .unwrap_err()
        .to_string();

    assert!(!error.contains("s3cret"), "{error}");
    assert!(
        error.starts_with(&format!(
            "Could not check out reference 'does-not-exist' for repository \
             'file://***@localhost{}'",
            repo_dir.display()
        )),
        "{error}"
    );

    std::fs::remove_dir_all(&repo_dir).ok();
    std::fs::remove_dir_all(destination.parent().unwrap()).ok();
}

#[test]
fn a_cached_include_debugs_without_its_credentials() {
    let entry = CachedInclude {
        key: "k".to_string(),
        remote: "https://u:s3cret@host/r.git".to_string(),
        git_ref: "v1".to_string(),
        last_used: 0,
        path: PathBuf::from("/k"),
        size_bytes: 0,
    };

    let debugged = format!("{entry:?}");
    assert!(!debugged.contains("s3cret"), "{debugged}");
    assert!(debugged.contains("https://***@host/r.git"), "{debugged}");
}
