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
use crate::config::{load_project_native, BuildSecret, Config, VolumeMount};
use crate::git_include::{FakeGitClient, GitIncludeCache};

/// A fresh, empty directory under the system temporary directory, removed
/// when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "ratect-convert-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        count
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// Writes `files` (relative path → contents) under `dir`.
fn write_project(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

const VERSION: &str = "0.11.0";

fn version() -> crate::written_for::Version {
    VERSION.parse().unwrap()
}

async fn convert(root: &Path) -> Result<ConvertedProject> {
    let loaded = Config::load_from_file(root).await?;
    convert_project(&loaded, version())
}

/// Writes every converted file where it belongs.
fn write_converted(converted: &ConvertedProject) {
    for file in &converted.files {
        std::fs::write(&file.target, &file.toml).unwrap();
    }
}

/// A single file renders exactly as one pretty TOML document of the merged
/// [`Config`], `ratect_version` first — the shape the converter wrote before
/// it converted per file, which a project with no includes is promised to
/// keep byte for byte. The three-part rendering ([`render`]'s scalars,
/// `include`, tables) must therefore join back into `toml`'s own one-part
/// rendering of the same [`sorted`] value when there's no `include`.
#[test]
fn a_file_with_no_includes_renders_as_one_pretty_document() {
    let yaml = r#"
project_name: demo
containers:
  app:
    image: alpine:3.18
    environment:
      TZ: UTC
    volumes:
      - .:/code
    ports:
      - "8080:80"
tasks:
  build:
    run:
      container: app
      command: echo hi
"#;
    let document: Document = crate::yaml::from_str(yaml).unwrap();
    let rendered = render(&document, Some(version())).unwrap();
    let config: Config = crate::yaml::from_str(yaml).unwrap();
    let whole = toml::to_string_pretty(&sorted(toml::Value::try_from(&config).unwrap())).unwrap();
    assert_eq!(rendered, format!("ratect_version = \"{VERSION}\"\n{whole}"));

    // Pins `config convert`'s documented limitation: `ports`/`volumes` come
    // out in the form they were read in — here the compact string form.
    assert!(rendered.contains("\"8080:80/tcp\""), "{rendered}");
    assert!(rendered.contains("\".:/code\""), "{rendered}");

    // Scalars only, and tables only, each still render as `toml` does.
    let scalars: Document = crate::yaml::from_str("project_name: demo").unwrap();
    assert_eq!(render(&scalars, None).unwrap(), "project_name = \"demo\"\n");
    let tables: Document =
        crate::yaml::from_str("containers:\n  app:\n    image: alpine:3.18").unwrap();
    assert_eq!(
        render(&tables, None).unwrap(),
        "[containers.app]\nimage = \"alpine:3.18\"\n"
    );
}

/// The `include` list is rendered inline between the scalars and the tables,
/// each entry's keys in the documented order, and an empty list isn't
/// rendered at all.
#[test]
fn the_include_list_renders_inline_after_the_scalars() {
    let document: Document = crate::yaml::from_str(
        "project_name: demo\ninclude:\n  - a.yml\n  - type: git\n    repo: https://example.com/b.git\n    ref: v1\n    path: custom.yml\n    allow_host_paths: true\ncontainers:\n  app:\n    image: alpine:3.18\n",
    )
    .unwrap();
    assert_eq!(
        render(&document, None).unwrap(),
        "project_name = \"demo\"\n\
         include = [\n    \
             { path = \"a.yml\" },\n    \
             { type = \"git\", repo = \"https://example.com/b.git\", ref = \"v1\", path = \"custom.yml\", allow_host_paths = true },\n\
         ]\n\
         \n\
         [containers.app]\n\
         image = \"alpine:3.18\"\n"
    );
}

/// ratect#574: the root and every local include, at any depth, each become
/// their own `.toml` beside the source, with each `include` entry pointing
/// at the converted file — so a relative path in `a/b/y.yml` still resolves
/// against `a/b/` once converted, as it did under `ratect-compat`. A file
/// included by two routes is converted once.
#[tokio::test]
async fn each_owned_file_converts_beside_itself_with_its_includes_rewritten() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[
            (
                "batect.yml",
                "project_name: demo\ninclude:\n  - a/x.yml\n  - type: file\n    path: a/b/y.yml\n",
            ),
            ("a/x.yml", "include:\n  - b/y.yml\ntasks:\n  t:\n    run:\n      container: y\n"),
            (
                "a/b/y.yml",
                "containers:\n  y:\n    build_directory: ctx\n    build_secrets:\n      s:\n        path: secret.txt\n    build_ssh:\n      - id: k\n        paths:\n          - key\n    volumes:\n      - local: .\n        container: /c\n      - data:/d\n",
            ),
        ],
    );
    let converted = convert(&dir.join("batect.yml")).await.unwrap();

    let targets: Vec<&Path> = converted.files.iter().map(|f| f.target.as_path()).collect();
    assert_eq!(
        targets,
        vec![
            dir.join("ratect.toml").as_path(),
            dir.join("a/x.toml").as_path(),
            dir.join("a/b/y.toml").as_path(),
        ]
    );
    let sources: Vec<&Path> = converted.files.iter().map(|f| f.source.as_path()).collect();
    assert_eq!(
        sources,
        vec![
            dir.join("batect.yml").as_path(),
            dir.join("a/x.yml").as_path(),
            dir.join("a/b/y.yml").as_path(),
        ]
    );
    assert_eq!(
        converted.files[0].toml,
        format!(
            "ratect_version = \"{VERSION}\"\nproject_name = \"demo\"\ninclude = [\n    {{ path = \"a/x.toml\" }},\n    {{ path = \"a/b/y.toml\" }},\n]\n"
        )
    );
    assert!(
        converted.files[1]
            .toml
            .starts_with("include = [\n    { path = \"b/y.toml\" },\n]\n\n[tasks.t.run]"),
        "{}",
        converted.files[1].toml
    );
    assert!(
        !converted.files[1].toml.contains("ratect_version"),
        "only the root declares it:\n{}",
        converted.files[1].toml
    );
    assert!(
        converted.files[2].toml.starts_with("[containers.y]"),
        "{}",
        converted.files[2].toml
    );

    // Loaded natively, every path in the converted include resolves under
    // its own directory.
    write_converted(&converted);
    let project = load_project_native(
        &dir.join("ratect.toml"),
        running_0_12(),
        &crate::ui::LoadOutput::default(),
    )
    .await
    .unwrap();
    let whole = project
        .evaluate_whole_project(&HashMap::new(), |_| None)
        .unwrap();
    assert!(whole.failures.is_empty(), "{:#?}", whole.failures);
    let y = &whole.evaluated.containers["y"];
    let under_b = |rest: &str| dir.join("a/b").join(rest).display().to_string();
    assert_eq!(y.build_directory.as_deref(), Some(under_b("ctx").as_str()));
    assert_eq!(
        y.build_secrets.as_ref().unwrap()["s"],
        BuildSecret::Path(under_b("secret.txt"))
    );
    assert_eq!(y.build_ssh.as_ref().unwrap()[0].paths, vec![under_b("key")]);
    let locals: Vec<&str> = y
        .volumes
        .iter()
        .flatten()
        .map(|volume| match volume {
            VolumeMount::Local(local) => local.local.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        locals,
        vec![dir.join("a/b").display().to_string(), under_b("data")]
    );
    assert!(whole.evaluated.tasks.contains_key("t"));
}

/// An item about a container goes in the file declaring that container, not
/// the root's.
#[tokio::test]
async fn a_containers_items_go_in_the_file_declaring_it() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[
            (
                "batect.yml",
                "project_name: demo\ninclude:\n  - more/c.yml\ncontainers:\n  a:\n    image: alpine:3.18\n    volumes:\n      - $HOME/a:/a\n",
            ),
            (
                "more/c.yml",
                "containers:\n  b:\n    image: alpine:3.18\n    volumes:\n      - $HOME/b:/b\n",
            ),
        ],
    );
    let converted = convert(&dir.join("batect.yml")).await.unwrap();
    let named = |file: &ConvertedFile| -> Vec<Option<String>> {
        file.checks
            .iter()
            .map(|check| check.container.clone())
            .collect()
    };
    assert_eq!(named(&converted.files[0]), vec![Some("a".to_string())]);
    assert_eq!(named(&converted.files[1]), vec![Some("b".to_string())]);
}

fn git_cache(scratch: &Scratch, git: FakeGitClient) -> GitIncludeCache<FakeGitClient> {
    GitIncludeCache::for_test(scratch.0.join("incl"), git, 1000)
}

async fn convert_with_git(
    root: &Path,
    cache: &GitIncludeCache<FakeGitClient>,
) -> Result<ConvertedProject> {
    let loaded = Config::load_from_file_with_git_cache(root, cache).await?;
    convert_project(&loaded, version())
}

const BUNDLE: &str = "https://example.com/bundle.git";
const BUNDLE_YML: &str = "containers:\n  bundled:\n    image: alpine:3.18\n";

/// A Git include stays a Git include, with what the source wrote carried
/// across; nothing from the bundle is copied into the output, and no file of
/// the bundle is converted.
#[tokio::test]
async fn a_git_include_is_carried_as_an_entry_and_its_bundle_left_alone() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[(
            "batect.yml",
            &format!("project_name: demo\ninclude:\n  - type: git\n    repo: {BUNDLE}\n    ref: v1\n    allow_host_paths: true\n"),
        )],
    );
    let git = FakeGitClient::new().with_files(
        BUNDLE,
        "v1",
        HashMap::from([("batect-bundle.yml".to_string(), BUNDLE_YML.to_string())]),
    );
    let converted = convert_with_git(&dir.join("batect.yml"), &git_cache(&scratch, git))
        .await
        .unwrap();

    assert_eq!(converted.files.len(), 1, "{converted:#?}");
    assert_eq!(
        converted.files[0].toml,
        format!(
            "ratect_version = \"{VERSION}\"\nproject_name = \"demo\"\ninclude = [\n    {{ type = \"git\", repo = \"{BUNDLE}\", ref = \"v1\", allow_host_paths = true }},\n]\n"
        )
    );
    assert!(
        converted.files[0].checks.is_empty(),
        "{:#?}",
        converted.files[0].checks
    );
}

/// A container declared in a bundle stays in a YAML file, so an item about a
/// Batect-format literal (`home_directory`, a compact volume) doesn't apply
/// to it; the readiness gate follows the binary, not the file, so that item
/// does — placed under the owned file whose Git include reached the bundle,
/// since no bundle file is converted.
#[tokio::test]
async fn a_bundled_containers_items_go_under_the_owned_file_including_its_bundle() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[(
            "batect.yml",
            &format!(
                "project_name: demo\ninclude:\n  - type: git\n    repo: {BUNDLE}\n    ref: v1\ntasks:\n  t:\n    run:\n      container: bundled\n"
            ),
        )],
    );
    let git = FakeGitClient::new().with_files(
        BUNDLE,
        "v1",
        HashMap::from([(
            "batect-bundle.yml".to_string(),
            "containers:\n  bundled:\n    image: alpine:3.18\n    health_check:\n      command: \"true\"\n    run_as_current_user:\n      enabled: true\n      home_directory: /home/$USER\n    volumes:\n      - $HOME/x:/x\n"
                .to_string(),
        )]),
    );
    let converted = convert_with_git(&dir.join("batect.yml"), &git_cache(&scratch, git))
        .await
        .unwrap();

    assert_eq!(converted.files.len(), 1);
    let checks = &converted.files[0].checks;
    assert_eq!(checks.len(), 1, "{checks:#?}");
    assert_eq!(checks[0].kind, ConversionCheckKind::UngatedTaskContainer);
    assert_eq!(checks[0].container.as_deref(), Some("bundled"));
}

/// A pathless entry whose clone offers a `ratect-bundle.toml` is pinned to
/// the `batect-bundle.yml` the Batect-format load read, with an item saying
/// so; one whose clone doesn't is left pathless; and an explicit `path` is
/// carried across unprobed.
#[tokio::test]
async fn a_pathless_git_include_is_pinned_to_the_batect_bundle_a_native_load_would_skip() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[(
            "batect.yml",
            &format!(
                "project_name: demo\ninclude:\n  - type: git\n    repo: {BUNDLE}\n    ref: both\n  - type: git\n    repo: {BUNDLE}\n    ref: yaml-only\n  - type: git\n    repo: {BUNDLE}\n    ref: both\n    path: custom.yml\n"
            ),
        )],
    );
    let git = FakeGitClient::new()
        .with_files(
            BUNDLE,
            "both",
            HashMap::from([
                ("batect-bundle.yml".to_string(), BUNDLE_YML.to_string()),
                (
                    "ratect-bundle.toml".to_string(),
                    "[containers.native]\nimage = \"alpine:3.18\"\n".to_string(),
                ),
                (
                    "custom.yml".to_string(),
                    "tasks:\n  custom:\n    run:\n      container: bundled\n".to_string(),
                ),
            ]),
        )
        .with_files(
            BUNDLE,
            "yaml-only",
            HashMap::from([(
                "batect-bundle.yml".to_string(),
                "tasks:\n  t:\n    run:\n      container: bundled\n".to_string(),
            )]),
        );
    let converted = convert_with_git(&dir.join("batect.yml"), &git_cache(&scratch, git))
        .await
        .unwrap();

    assert_eq!(
        converted.files[0].toml,
        format!(
            "ratect_version = \"{VERSION}\"\nproject_name = \"demo\"\ninclude = [\n    \
             {{ type = \"git\", repo = \"{BUNDLE}\", ref = \"both\", path = \"batect-bundle.yml\" }},\n    \
             {{ type = \"git\", repo = \"{BUNDLE}\", ref = \"yaml-only\" }},\n    \
             {{ type = \"git\", repo = \"{BUNDLE}\", ref = \"both\", path = \"custom.yml\" }},\n]\n"
        )
    );
    let checks = &converted.files[0].checks;
    assert_eq!(checks.len(), 1, "{checks:#?}");
    assert_eq!(checks[0].kind, ConversionCheckKind::BundleOffersNativeFile);
    assert!(
        checks[0].message.contains(BUNDLE)
            && checks[0].message.contains("'both'")
            && checks[0].message.contains("'path'"),
        "{}",
        checks[0].message
    );
}

/// A bundle declaring Git includes of its own — in the file the entry loads,
/// or one that file reaches through its own local includes — gets an item: a
/// native project refuses them without `allow_nested_git_includes`, which is
/// never written. An entry into the same clone that loads a file declaring
/// none gets no item.
#[tokio::test]
async fn a_bundle_declaring_git_includes_of_its_own_is_an_item() {
    let scratch = scratch();
    let dir = &scratch.0;
    const OTHER: &str = "https://example.com/other.git";
    write_project(
        dir,
        &[(
            "batect.yml",
            &format!(
                "project_name: demo\ninclude:\n  - type: git\n    repo: {BUNDLE}\n    ref: v1\n  - type: git\n    repo: {BUNDLE}\n    ref: v1\n    path: plain.yml\n"
            ),
        )],
    );
    let git = FakeGitClient::new()
        .with_files(
            BUNDLE,
            "v1",
            HashMap::from([
                (
                    "batect-bundle.yml".to_string(),
                    "include:\n  - nested/more.yml\n".to_string(),
                ),
                (
                    "nested/more.yml".to_string(),
                    format!("include:\n  - type: git\n    repo: {OTHER}\n    ref: v2\n"),
                ),
                (
                    "plain.yml".to_string(),
                    "tasks:\n  t:\n    run:\n      container: bundled\n".to_string(),
                ),
            ]),
        )
        .with_files(
            OTHER,
            "v2",
            HashMap::from([("batect-bundle.yml".to_string(), BUNDLE_YML.to_string())]),
        );
    let converted = convert_with_git(&dir.join("batect.yml"), &git_cache(&scratch, git))
        .await
        .unwrap();

    let checks = &converted.files[0].checks;
    assert_eq!(checks.len(), 1, "{checks:#?}");
    assert_eq!(
        checks[0].kind,
        ConversionCheckKind::BundleDeclaresGitIncludes
    );
    assert!(
        checks[0].message.contains(BUNDLE)
            && checks[0].message.contains("'allow_nested_git_includes'"),
        "{}",
        checks[0].message
    );
    assert!(
        !converted.files[0]
            .toml
            .contains("allow_nested_git_includes")
            && !converted.files[0].toml.contains(OTHER),
        "{}",
        converted.files[0].toml
    );
}

/// An include naming the root itself, or named from a subdirectory, is
/// rewritten to the file the conversion actually writes — the root's
/// `ratect.toml`, not a `batect.toml` nothing writes — with the entry's own
/// directory spelling kept.
#[tokio::test]
async fn an_include_entry_names_the_converted_file_however_it_is_spelled() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[
            (
                "batect.yml",
                "project_name: demo\ninclude:\n  - ./batect.yml\n  - ./a/x.yml\n",
            ),
            (
                "a/x.yml",
                "include:\n  - ../batect.yml\n  - ../a/../a/x.yml\ncontainers:\n  c:\n    image: alpine:3.18\n",
            ),
        ],
    );
    let converted = convert(&dir.join("batect.yml")).await.unwrap();
    assert!(
        converted.files[0].toml.contains(
            "include = [\n    { path = \"./ratect.toml\" },\n    { path = \"./a/x.toml\" },\n]\n"
        ),
        "{}",
        converted.files[0].toml
    );
    assert!(
        converted.files[1].toml.contains(
            "include = [\n    { path = \"../ratect.toml\" },\n    { path = \"../a/../a/x.toml\" },\n]\n"
        ),
        "{}",
        converted.files[1].toml
    );
    write_converted(&converted);
    let project = load_project_native(
        &dir.join("ratect.toml"),
        running_0_12(),
        &crate::ui::LoadOutput::default(),
    )
    .await
    .unwrap();
    assert!(project.config.containers.contains_key("c"));
}

/// Two sources that would convert to one target is an error naming both,
/// before anything is rendered — `c.yml` beside `c.yaml`, or an include
/// named like the root's own target. So is a source that is its own target:
/// a `.toml`-named include, which the Batect-format load reads as YAML.
#[tokio::test]
async fn two_sources_converting_to_one_target_is_an_error() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[
            (
                "batect.yml",
                "project_name: demo\ninclude:\n  - c.yml\n  - c.yaml\n",
            ),
            ("c.yml", "containers:\n  a:\n    image: alpine:3.18\n"),
            ("c.yaml", "containers:\n  b:\n    image: alpine:3.18\n"),
        ],
    );
    let error = convert(&dir.join("batect.yml"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&dir.join("c.yml").display().to_string())
            && error.contains(&dir.join("c.yaml").display().to_string())
            && error.contains(&dir.join("c.toml").display().to_string()),
        "{error}"
    );

    write_project(
        dir,
        &[
            (
                "batect.yml",
                "project_name: demo\ninclude:\n  - ratect.yml\n",
            ),
            ("ratect.yml", "containers:\n  a:\n    image: alpine:3.18\n"),
        ],
    );
    let error = convert(&dir.join("batect.yml"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&dir.join("batect.yml").display().to_string())
            && error.contains(&dir.join("ratect.yml").display().to_string())
            && error.contains(&dir.join("ratect.toml").display().to_string()),
        "{error}"
    );

    write_project(
        dir,
        &[
            (
                "batect.yml",
                "project_name: demo\ninclude:\n  - shared.toml\n",
            ),
            ("shared.toml", "containers:\n  a:\n    image: alpine:3.18\n"),
        ],
    );
    let error = convert(&dir.join("batect.yml"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&dir.join("shared.toml").display().to_string())
            && error.contains("overwrite it with its own conversion"),
        "{error}"
    );
}

/// Every table is written with its keys sorted, at every depth and inside
/// arrays — the alphabetical output `config convert` had before `toml`'s
/// `preserve_order`. Checked against literal text, independently of
/// [`sorted`] (the test above uses it to check the join, not the order).
/// Containers declared `zz` first, a struct's fields (`local` before
/// `container`) and an object-form volume inside an array each fail
/// unsorted every time; `environment`, a `HashMap`, almost every time.
#[test]
fn every_table_is_written_in_key_order() {
    let yaml = "project_name: demo\ncontainers:\n  zz:\n    image: alpine\n    volumes:\n      - {local: ., container: /code}\n    environment: {J: '10', I: '9', H: '8', G: '7', F: '6', E: '5', D: '4', C: '3', B: '2', A: '1'}\n  aa:\n    image: alpine\n";
    let document: Document = crate::yaml::from_str(yaml).unwrap();
    assert_eq!(
        render(&document, None).unwrap(),
        "project_name = \"demo\"\n\n[containers.aa]\nimage = \"alpine\"\n\n[containers.zz]\nimage = \"alpine\"\n\n[containers.zz.environment]\nA = \"1\"\nB = \"2\"\nC = \"3\"\nD = \"4\"\nE = \"5\"\nF = \"6\"\nG = \"7\"\nH = \"8\"\nI = \"9\"\nJ = \"10\"\n\n[[containers.zz.volumes]]\ncontainer = \"/code\"\nlocal = \".\"\n"
    );
}

/// #385: a Batect-format file uses its `image`, compact volume host paths
/// and `home_directory` as written, so a malformed sigil there (`img$`) is
/// valid; in `ratect.toml` those fields are expressions, where it would
/// fail the load. It can't have meant a reference, so it converts escaped,
/// and the converted project loads with the same values. A well-formed
/// reference is carried across as before, with its check — and only it: a
/// malformed value holding a reference before its mistake is escaped whole,
/// with no check, since it no longer becomes an expression.
#[tokio::test]
async fn a_malformed_literal_converts_escaped_and_keeps_its_value() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[(
            "batect.yml",
            "project_name: demo\ncontainers:\n  app:\n    image: 'img$'\n    volumes:\n      \
             - 'a$:/x'\n      - '$HOME/b:/b'\n      - '$HOME/c$:/c'\n    run_as_current_user:\n      enabled: true\n      \
             home_directory: '/home/a < b'\n",
        )],
    );
    let converted = convert(&dir.join("batect.yml")).await.unwrap();
    write_converted(&converted);
    let checks: Vec<ConversionCheckKind> = converted.files[0]
        .checks
        .iter()
        .map(|check| check.kind)
        .collect();
    assert_eq!(checks, vec![ConversionCheckKind::CompactVolumeExpression]);

    let native = load_project_native(
        &dir.join("ratect.toml"),
        running_0_12(),
        &crate::ui::LoadOutput::default(),
    )
    .await
    .unwrap();
    let evaluated = native
        .evaluate(["app"], [], &HashMap::new(), |name: &str| {
            (name == "HOME").then(|| "/h".to_string())
        })
        .unwrap();
    let app = &evaluated.containers["app"];
    assert_eq!(app.image.as_deref(), Some("img$"));
    let locals: Vec<&str> = app
        .volumes
        .iter()
        .flatten()
        .map(|volume| match volume {
            VolumeMount::Local(local) => local.local.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(locals[0].ends_with("/a$"), "{locals:?}");
    assert_eq!(locals[1], "/h/b");
    assert!(locals[2].ends_with("/$HOME/c$"), "{locals:?}");
    assert_eq!(
        app.run_as_current_user
            .as_ref()
            .unwrap()
            .home_directory
            .as_deref(),
        Some("/home/a < b")
    );
}

/// #628: a Batect-format file uses its `image`, compact volume host paths and
/// `home_directory` as written, so a `\` there is a backslash; in
/// `ratect.toml` it would escape the character after it. It converts
/// escaped, so the converted project loads with the same values — and a
/// `$`/`<` after one becomes a reference, as the source's would read if its
/// field were an expression, and is listed (`a\$HOME`); unless the result is
/// no expression (`img\$`), when the whole value is kept literal.
#[tokio::test]
async fn a_backslash_in_a_literal_field_converts_escaped_and_keeps_its_value() {
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[(
            "batect.yml",
            "project_name: demo\ncontainers:\n  app:\n    image: 'img:a\\b'\n    volumes:\n      \
             - 'foo\\bar:/x'\n      - '$HOME\\x:/y'\n      - 'a\\$HOME:/z'\n    \
             run_as_current_user:\n      enabled: true\n      home_directory: '/home/a\\b'\n  \
             tagged:\n    image: 'img\\$'\n  escaped:\n    image: 'alpine\\$HOME'\n",
        )],
    );
    let converted = convert(&dir.join("batect.yml")).await.unwrap();
    write_converted(&converted);
    let checks: Vec<ConversionCheckKind> = converted.files[0]
        .checks
        .iter()
        .map(|check| check.kind)
        .collect();
    assert_eq!(
        checks,
        vec![
            ConversionCheckKind::CompactVolumeExpression,
            ConversionCheckKind::CompactVolumeExpression
        ]
    );

    let native = load_project_native(
        &dir.join("ratect.toml"),
        running_0_12(),
        &crate::ui::LoadOutput::default(),
    )
    .await
    .unwrap();
    let evaluated = native
        .evaluate(
            ["app", "tagged", "escaped"],
            [],
            &HashMap::new(),
            |name: &str| (name == "HOME").then(|| "/h".to_string()),
        )
        .unwrap();
    assert_eq!(
        evaluated.containers["tagged"].image.as_deref(),
        Some(r"img\$")
    );
    // An `image` is kept wholly literal: a `batect.yml` refuses a reference
    // there, so a `$` that loaded was never meant as one, and no check would
    // list it.
    assert_eq!(
        evaluated.containers["escaped"].image.as_deref(),
        Some(r"alpine\$HOME")
    );
    let app = &evaluated.containers["app"];
    assert_eq!(app.image.as_deref(), Some(r"img:a\b"));
    let locals: Vec<&str> = app
        .volumes
        .iter()
        .flatten()
        .map(|volume| match volume {
            VolumeMount::Local(local) => local.local.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(locals[0].ends_with(r"/foo\bar"), "{locals:?}");
    assert_eq!(locals[1], r"/h\x");
    assert!(locals[2].ends_with(r"/a\/h"), "{locals:?}");
    assert_eq!(
        app.run_as_current_user
            .as_ref()
            .unwrap()
            .home_directory
            .as_deref(),
        Some(r"/home/a\b")
    );
}

/// #395: a root with no `project_name` converts to one naming the project
/// as `ratect-compat` names it, since `ratect.toml` requires the field — so
/// the converted project keeps its name wherever it is later moved.
#[tokio::test]
async fn an_inferred_project_name_is_written_into_the_root() {
    let scratch = scratch();
    let dir = scratch.0.join("MyProj");
    write_project(
        &dir,
        &[
            ("batect.yml", "include: [x.yml]\n"),
            ("x.yml", "tasks:\n  t:\n    run:\n      container: c\n"),
        ],
    );
    let converted = convert(&dir.join("batect.yml")).await.unwrap();
    assert!(
        converted.files[0]
            .toml
            .contains("project_name = \"myproj\"\n"),
        "{}",
        converted.files[0].toml
    );
    assert!(
        !converted.files[1].toml.contains("project_name"),
        "{}",
        converted.files[1].toml
    );
}

/// The `ratect` version a native load in these tests runs as.
fn running_0_12() -> crate::written_for::Version {
    "0.12.0".parse().unwrap()
}

/// #751: a conversion item names the Git include without its remote's
/// credentials; the converted entry keeps the remote in full, so it still
/// clones.
#[tokio::test]
async fn a_conversion_item_hides_the_includes_credentials() {
    const REMOTE: &str = "https://u:s3cret@example.com/bundle.git";
    let scratch = scratch();
    let dir = &scratch.0;
    write_project(
        dir,
        &[(
            "batect.yml",
            &format!(
                "project_name: demo\ninclude:\n  - type: git\n    repo: {REMOTE}\n    ref: v1\n"
            ),
        )],
    );
    let git = FakeGitClient::new()
        .with_files(
            REMOTE,
            "v1",
            HashMap::from([
                (
                    "batect-bundle.yml".to_string(),
                    format!("include:\n  - type: git\n    repo: {BUNDLE}\n    ref: v2\n"),
                ),
                (
                    "ratect-bundle.toml".to_string(),
                    "[containers.native]\nimage = \"alpine:3.18\"\n".to_string(),
                ),
            ]),
        )
        .with_files(
            BUNDLE,
            "v2",
            HashMap::from([("batect-bundle.yml".to_string(), BUNDLE_YML.to_string())]),
        );
    let converted = convert_with_git(&dir.join("batect.yml"), &git_cache(&scratch, git))
        .await
        .unwrap();

    let messages: Vec<&str> = converted.files[0]
        .checks
        .iter()
        .map(|check| check.message.as_str())
        .collect();
    assert_eq!(messages.len(), 2, "{messages:#?}");
    assert!(
        messages[0].starts_with(
            "Git include 'https://***@example.com/bundle.git' at 'v1' offers a ratect-bundle.toml"
        ),
        "{}",
        messages[0]
    );
    assert!(
        messages[1].starts_with(
            "Git include 'https://***@example.com/bundle.git' at 'v1' declares Git includes"
        ),
        "{}",
        messages[1]
    );
    assert!(
        converted.files[0]
            .toml
            .contains(&format!("repo = \"{REMOTE}\"")),
        "{}",
        converted.files[0].toml
    );
}
