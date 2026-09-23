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
  `git commit` hook** (`decisions/0009`) — a PR opened without Claude Code
  (a plain shell commit, another editor's Git integration, or a bot account
  like Renovate) merges without `check-links`/`check-citations`/
  `executable-claims` ever running against it. Wiring `python3 -m
  claims.cli --repo-root .` into `.github/workflows/ci.yml` would close
  this, but `or1can/claims` has no tagged releases yet to pin a CI checkout
  against — revisit once it does, rather than pinning CI to an arbitrary
  commit SHA in the meantime.

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
  user writing a `batect.yml`/`ratect.toml`, and 29 mentions across 22 of
  them cite things like `TaskEngine::resolve_image`,
  `Config::resolve_expressions_with` or `BuildSecret::Path`, which name
  nothing the reader can look up. The mechanism is already in place to fix
  it — only a doc comment's *first paragraph* becomes the description
  (`schema.rs`'s summarizer, pinned by
  `a_description_keeps_its_first_paragraph_only_reflowed`), so each one
  needs its user-facing sentence kept first and the implementation detail
  moved to a second paragraph, exactly as `Task::customise` now does.
  Deliberately not done piecemeal while passing through: fixing two of
  twenty-two leaves the tree less consistent than finding it, and the
  remedy is per-field prose rather than one mechanical edit. Count it by
  walking the committed schemas' `description` values for a backticked
  `Foo::bar`, not by grepping the source, since only the first paragraph
  ever reaches them.

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
