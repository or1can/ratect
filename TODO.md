# TODO

Ratect's engineering backlog: deferred follow-on work from specific code
reviews — real findings not worth fixing immediately, and narrower
architectural notes surfaced along the way. Distinct from ROADMAP.md's
**Future Vision** section, which is undecided *product* ideas with no code
behind them yet — everything here is tied to code that already shipped.

A review finding that needs **no fix at all** doesn't get an entry here
either, past or present — this file used to keep a standing "Reviewed, no
action needed" section for those, and it only ever grew: almost any dismissed
finding can be argued to meet "a future reviewer might rediscover this,"
so the bar didn't actually bound anything. The reasoning behind a non-fix
belongs as a code comment at the site it's about instead (see
[decisions/0006](decisions/0006-code-and-documentation-locality.md)) — it
surfaces exactly when someone is reading that code, rather than requiring a
separate list to be checked, and it can't accrete the way a list does.

Findings from the pre-release code review of 0.16.0's output-modes work
(`git diff origin/main...HEAD` at the time, covering `ratect-core/src/ui/`,
the `engine.rs`/`docker.rs` event-posting refactor, and the `--output`/
`--no-color` CLI surface) — all fixed; see `git log` for what landed.
Everything below is unfixed. Grouped by severity; pick up top-down.

**Item numbers are identifiers, not positions.** They carry gaps because a
completed item is deleted and the survivors keep their numbers — code cites
them (`ratect-compat/tests/cli.rs` names "TODO.md item 4"), so renumbering
would silently repoint those references. That is also why these are bullets
carrying a bold number rather than a Markdown ordered list: an `ordered list`
renders `9, 13, 14, 15` as `9, 10, 11, 12`, taking only the first number and
counting from there, so the identifiers would exist only in the source and
every rendered view — GitHub, the docs site — would show something else.
Confirmed against GitHub's own `/markdown` API, not assumed.

## Maintainability / latent hazards

- **9.** **The `claims` plugin's checks have no CI-level backstop, only the local
  `pre-commit` hook** (`decisions/0009`, `.pre-commit-config.yaml`) — a PR
  from a clone where `pre-commit install` was never run, or from a bot
  account like Renovate, merges without `check-links`/`check-citations`/
  `executable-claims` ever running against it. `or1can/claims` now tags
  its releases, so a CI job can pin the same tag the hook does; what's
  left to decide is what it checks — the hook compares the working tree
  with `HEAD`, which on a CI checkout is no change at all, so the job
  needs a range (`--diff-range origin/main..HEAD`) rather than the hook
  as is.

- **13.** **No confirmation prompt on `ratect resources clean --all-projects`**
  (`ratect/src/main.rs`) — the one thing `list`-before-`clean` can't catch
  is typing the dangerous command by accident, which only a prompt does,
  since a dry run only helps if you remembered to run it first. Deferred
  rather than rejected when the verb shipped ([0.2.0](RELEASES.md#ratect)):
  it would be the first interactive prompt in either binary (Batect has
  none, so there's no precedent), it needs a `--yes` escape for CI, and the
  two-layer guard on what `--all-projects` can even reach already removes
  the catastrophic version of the mistake. Worth revisiting on the first
  report of a near-miss.

- **14.** **`resources` can't distinguish a concurrently-running task's containers
  from an orphan** (`ratect-core/src/resources.rs`) — they're labelled
  identically, because until the run ends they *are* the same thing, and
  the daemon can't say whether some other `ratect` process still cares
  about a container. `list` reporting age and `clean` taking
  `--older-than` is the honest mitigation; claiming to detect liveness
  would be a lie. If this bites in practice, the next step would be a
  heartbeat (a running invocation touching its own resources
  periodically) rather than any attempt to infer liveness after the fact.

- **15.** **The generated JSON schemas' `description`s name private Rust items**
  (`ratect-core/src/config.rs` doc comments, rendered by
  `ratect-core/src/schema.rs`) — a description is what an editor shows a
  user writing a `batect.yml`/`ratect.toml`, and about two dozen of them
  cite things like `TaskEngine::resolve_image`,
  `TaskEngine::run_task_internal` or `BuildSecret::Path`, which name
  nothing the reader can look up. The mechanism is already in place to fix
  it — only a doc comment's *first paragraph* becomes the description
  (`schema.rs`'s summarizer, pinned by
  `a_description_keeps_its_first_paragraph_only_reflowed`), so each one
  needs its user-facing sentence kept first and the implementation detail
  moved to a second paragraph, exactly as `Task::customise` now does.
  Deliberately not done piecemeal while passing through: fixing two of
  two dozen leaves the tree less consistent than finding it, and the
  remedy is per-field prose rather than one mechanical edit. Count it by
  walking the committed schemas' `description` values for a backticked
  `Foo::bar`, not by grepping the source, since only the first paragraph
  ever reaches them.

- **17.** **A YAML type mismatch's `found` is vaguer than it was before
  config files were parsed straight from text** (#415): noyalib's streaming
  walker reports `expected string, found non-string scalar` or `expected
  bool, found other` where the `Value`-based path said `found float` /
  `found string`, and `from_str` keeps the streaming wording by design
  (its `locate_streaming_error`). The location and path now attached point
  at the value, so nothing is lost that a user can't see; the fix is
  upstream, in `noyalib`'s `streaming.rs` `TypeMismatch` arms, which have
  consumed the event by the time they name what they found.

- **18.** **A tag anywhere in a YAML file re-formats its unquoted
  numbers** (#324, #378): every Batect-format read — a config file, a config
  vars file — keeps a plain scalar's source text through `crate::yaml`, i.e.
  `noyalib`'s `plain_scalar_strings`, which only does so on its streaming
  path. A tag (`w: !!int 7`) sends the whole document down the AST fallback,
  where the same option re-formats the resolved value, so `v: 3.10` beside
  it loads as `3.1`. Raised upstream as
  [sebastienrousseau/noyalib#494](https://github.com/sebastienrousseau/noyalib/issues/494);
  `yaml::tests::a_document_with_a_tag_reformats_an_unquoted_number` pins
  it, so a release that fixes it fails that test: then make it assert the
  text as written, and delete this.

## Test coverage

- **4.** **`ratect-compat/tests/cli.rs`'s `task_output` helper weakens ~18 converted e2e
  assertions** — replaced `assert_eq!(stdout.trim(), expected)` (whole-
  stdout equality) with a windowed extract between the task-container
  milestone and `Cleaning up...`. Stray output before or after that window
  is still silently tolerated. Revisited for ratect#72: whole-stdout
  exactness is confirmed still impractical to restore in the default
  (non-quiet) mode — pull/build lines are conditional and duration is
  variable, so nothing short of ignoring both would let a whole-stdout
  `assert_eq!` pass reliably. What #72 did fix is the window's own frame
  line, previously found by a loose heuristic that could match a line the
  container itself printed, or miss simple mode's command-less
  `"Running <container>..."` phrasing entirely (a task relying on the
  image's default `CMD`) — `task_output` now anchors on the task's own
  container name, matching both real milestone shapes exactly, so the
  only tolerance left is the inherently-variable milestone content this
  note already names, not a heuristic that could miss the real frame
  line. `simple_output_format_frames_task_output_via_docker` still
  asserts presence/order only, and the exact-stdout tests still pin
  `quiet` mode specifically.

## Efficiency

- **6.** **`Console`'s `std::sync::Mutex` can block tokio worker threads on a
   stalled stdout** (`ratect-core/src/ui.rs`) — `post()` runs
   synchronously from tokio worker threads, so a stalled stdout (closed
   pipe reader, `Ctrl-S`'d terminal) blocks whichever holds the Console
   mutex mid-write and queues every concurrent poster behind it, which
   in turn stalls whatever is draining the Docker attach/log stream that
   feeds it — the same container-blocks-because-nobody's-reading effect
   Docker's own log buffering produces, just triggered from the host
   side instead of the daemon side. (The redundant explicit `flush()`
   after every `println` — stdout's own `LineWriter` already flushes on
   the newline `println` always writes — is already fixed.) A real fix
   (an mpsc channel draining to one dedicated writer thread/task, the
   `tracing-appender` pattern) is a bigger change than the other items
   here; low likelihood in practice (`ratect | head` closing early is
   the realistic trigger), not attempted yet.

---

# ratect-core/src/docker.rs: a second wide-positional-seam cluster

`docker.rs`'s image/BuildKit helpers immediately above the connection block
that became `ratect-core/src/docker/connection.rs` in 0.6.0 (`split_image_reference`,
`docker_buildkit_env_value`, `select_builder_version`) are the same shape of
finding the architecture review that motivated that split named — a
self-contained concept sharing a module with container lifecycle by history,
not by relationship. Deliberately not swept into that same change: they have
one call site each (`resolve_image`, `build_image`), so there's no
duplication to buy back, only call-site readability — a different, weaker
class than the one the split was justified by. Worth a look if `docker.rs`'s
size becomes a problem on its own terms, not before.
