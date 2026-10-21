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

/// A held lock is the forced failure: unlike an unwritable directory, it
/// still fails when the tests run as root. The sweep visits entries in
/// `read_dir` order, which the test can't choose, so each entry takes a turn as
/// the failing one: unless that order differs between the two runs, one of
/// them puts the failure first, which is the order that matters.
#[tokio::test]
async fn cleanup_stale_still_removes_other_stale_entries_when_one_removal_fails() {
    let remotes = ["https://example.com/a.git", "https://example.com/b.git"];
    for (locked, other) in [(remotes[0], remotes[1]), (remotes[1], remotes[0])] {
        let root = unique_temp_dir();
        let (locked_key, locked_copy, locked_info) =
            cached_entry_used_at(&root, locked, 1_000).await;
        let (other_key, other_copy, other_info) = cached_entry_used_at(&root, other, 1_000).await;
        tokio::fs::write(root.join(format!("{locked_key}.lock")), b"")
            .await
            .unwrap();

        let now = 1_000 + STALE_AFTER.as_secs() + 1;
        let mut sweeper = GitIncludeCache::for_test(root.clone(), FakeGitClient::new(), now);
        sweeper.lock_timeout = Duration::from_millis(250);
        sweeper.cleanup_stale().await.unwrap();

        assert!(locked_copy.exists(), "{locked} should be left in place");
        assert!(locked_info.exists(), "{locked} should be left in place");
        assert!(!other_copy.exists(), "{other} should be removed");
        assert!(!other_info.exists(), "{other} should be removed");

        // The per-entry results the sweep ignores.
        cached_entry_used_at(&root, other, 1_000).await;
        assert!(!sweeper.remove_entry(&root, &locked_key).await);
        assert!(sweeper.remove_entry(&root, &other_key).await);

        tokio::fs::remove_dir_all(&root).await.ok();
    }
}

#[tokio::test]
async fn remove_entry_files_succeeds_when_neither_file_exists() {
    let root = unique_temp_dir();

    assert!(remove_entry_files(&root.join("absent"), &root.join("absent.toml")).await);
}

#[tokio::test]
async fn remove_entry_files_removes_a_sidecar_with_no_working_copy() {
    let root = unique_temp_dir();
    tokio::fs::create_dir_all(&root).await.unwrap();
    let info_path = root.join("orphan.toml");
    tokio::fs::write(&info_path, b"").await.unwrap();

    assert!(remove_entry_files(&root.join("orphan"), &info_path).await);
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
