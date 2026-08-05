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
    let git =
        FakeGitClient::new().with_files("https://example.com/repo.git", "v1.0.0", HashMap::new());
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
