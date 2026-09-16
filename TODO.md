# TODO

Ratect's engineering backlog: deferred follow-on work from specific code
reviews — real findings not worth fixing immediately, deliberate non-fixes
worth recording so they aren't re-investigated from scratch, and narrower
architectural notes surfaced along the way. Distinct from ROADMAP.md's
**Future Vision** section, which is undecided *product* ideas with no code
behind them yet — everything here is tied to code that already shipped.

Findings from the pre-release code review of 0.16.0's output-modes work
(`git diff origin/main...HEAD` at the time, covering `ratect-core/src/ui/`,
the `engine.rs`/`docker.rs` event-posting refactor, and the `--output`/
`--no-color` CLI surface) — all fixed; see `git log` for what landed.
Everything below is unfixed. Grouped by severity; pick up top-down.

## Correctness

1. **Fancy: terminal narrowing mid-run can desync the cursor-up count**
   (`ratect-core/src/ui/fancy.rs`) — PLAUSIBLE, depends on terminal
   emulator reflow behavior (confirmed on reflowing emulators like
   iTerm2/GNOME Terminal/kitty; classic xterm doesn't reflow so is
   unaffected). Width is re-queried per repaint (fixes *future* clipping),
   but nothing accounts for rows a *previously* painted long line now
   occupies after the terminal narrowed and the emulator rewrapped it —
   the next repaint's `\x1b[{painted_lines}A` then lands mid-block.

## Correctness — cosmetic / narrow

2. **Interleaved: `LineBuffer` only splits on `\n`, buffers CR-only
   progress redraws unboundedly** (`ratect-core/src/ui/interleaved.rs`) —
   a container emitting lone-`\r` progress (pip/curl/apt-style redraws)
   produces no output until the stream ends, then dumps one giant
   concatenated line. **Verified faithful to Batect's own
   `InterleavedContainerOutputSink`**, which has the identical `\n`-only
   splitting behavior — not a Ratect-specific bug. Also mitigated in
   practice by the interleaved policy's `TERM=dumb` (most tools fall back
   to newline-based non-interactive output without a real TTY). Low
   priority; matches upstream Batect exactly.

## Maintainability / latent hazards

3. ~~**`CleanupStarting` doesn't post under `--use-network` with no
   dependencies** (`ratect-core/src/engine.rs`) — correct/honest
   behavior (nothing is actually cleaned up in that case, and
   `TaskEvent::CleanupStarting`'s own doc comment documents
   non-posting for exactly this), not a bug. Flagged only because
   `tests/cli.rs`'s `task_output` helper's fallback (`end =
   lines.len()` when no `"Cleaning up..."` line is found) would
   silently sweep the summary line into an extracted chunk if a future
   test combined `--use-network` with `task_output`. No existing test
   is affected — the two current `--use-network` tests never call it.~~
   — gone in 0.25.0: unifying cleanup ownership made the task's own
   container the engine's to remove, so such a run now has something to
   report and posts the stage like any other. The `task_output` fallback
   it warned about is no longer reachable that way.

9. **The `claims` plugin's checks have no CI-level backstop, only the local
   `git commit` hook** (`decisions/0009`) — a PR opened without Claude Code
   (a plain shell commit, another editor's Git integration, or a bot account
   like Renovate) merges without `check-links`/`check-citations`/
   `executable-claims` ever running against it. Wiring `python3 -m
   claims.cli --repo-root .` into `.github/workflows/ci.yml` would close
   this, but `or1can/claims` has no tagged releases yet to pin a CI checkout
   against — revisit once it does, rather than pinning CI to an arbitrary
   commit SHA in the meantime.

10. ~~**`executable-claims` runs automatically on every `git commit`, sweeping
    the whole tree for a `<!-- verify: -->` marker and executing whatever it
    names** (`decisions/0009`) — a malicious branch/PR could plant one
    anywhere and have it run, with a maintainer's full local privileges, the
    next time they commit anything while that branch is checked out. Filed
    upstream as [or1can/claims#15](https://github.com/or1can/claims/issues/15)
    (diff-scoping or a per-check hook opt-out); revisit once one ships.
    Meanwhile: don't run `git commit` while reviewing an untrusted branch in
    a Claude Code session with the plugin enabled.~~
    — closed upstream: `executable-claims` now denies execution by default,
    gated on an exact-string grant in a git-ignored, per-machine
    `claims.local.toml` (`docs/adr/0001-executable-claims-deny-by-default.md`
    in the plugin's own repo). Committed config can no longer authorize
    execution on its own, closing the reported path rather than narrowing
    it. This repo's own setup is in `AGENTS.md`'s Tooling & CI section and
    `decisions/0009`.

11. **`ci.yml`'s `Release Pipeline Config` check (ratect#34) isn't in the
    `main branch protection` ruleset's required status checks** — it runs
    and reports on every PR (same as `release.yml`'s own `plan` job, which
    it deliberately duplicates so the signal exists in `ci.yml` at all),
    but a broken `dist-workspace.toml` shows a red X without actually
    blocking a merge, same as before #34. Add `Release Pipeline Config` to
    ruleset `19050737`'s required checks list (a live repo-settings change,
    left to a maintainer rather than made unilaterally while implementing
    the ticket) to close the gap #34's own problem statement describes.

12. **Released archives don't include `NOTICE`** — `dist-workspace.toml`
    has no `include`/similar key adding it, so each archive ships
    `LICENSE`/`README.md`/`RELEASES.md` (dist's own defaults) but not the
    file carrying the `moby/patternmatcher` (Apache-2.0) and
    andrej-karpathy-skills (MIT) attributions this repo's own `NOTICE`
    records. Apache-2.0 §4(d) asks for it to travel with redistributions.
    Found reviewing `docs/installation.md`'s "what's in the archive" claim
    (ratect#38); fixing it is a `dist-workspace.toml` config change, out
    of scope for a docs ticket.

## Test coverage

4. **`tests/cli.rs`'s `task_output` helper weakens ~18 converted e2e
   assertions** — replaced `assert_eq!(stdout.trim(), expected)` (whole-
   stdout equality) with a windowed extract between the last
   `Running ... in ...` milestone and `Cleaning up...`. Stray output
   before or after that window is now silently tolerated. No remaining
   test pins whole-stdout purity in the default (non-quiet) mode —
   `simple_output_format_frames_task_output_via_docker` asserts
   presence/order only, and the exact-stdout tests pin `quiet` mode
   specifically. Whole-stdout exactness may be inherently hard to
   restore now (pull/build lines are conditional, duration is variable),
   but worth a second look.

5. **`task_output`'s frame-finding heuristic is fragile**
   (`tests/cli.rs`) — `rposition` of a line matching
   `starts_with("Running ") && contains(" in ") && ends_with("...")`
   can match a line the *container itself* printed (e.g.
   `"Running tests in release mode..."`), silently truncating the
   extract. It also never matches simple mode's command-less
   `"Running <container>..."` phrasing (no `" in "`) — a test for a
   task relying on the image's default `CMD` would get the whole
   stdout including milestones and fail loudly instead. No current
   fixture triggers either case (future-fragility only).

## Efficiency

6. **`Console`'s `std::sync::Mutex` can block tokio worker threads on a
    stalled stdout** (`ratect-core/src/ui/mod.rs`) — `post()` runs
    synchronously from tokio worker threads, so a stalled stdout (closed
    pipe reader, `Ctrl-S`'d terminal) blocks whichever holds the Console
    mutex mid-write and queues every concurrent poster behind it. (The
    redundant explicit `flush()` after every `println` — stdout's own
    `LineWriter` already flushes on the newline `println` always writes —
    is already fixed.) A real fix (an mpsc channel draining to one
    dedicated writer thread/task, the `tracing-appender` pattern) is a
    bigger change than the other items here; low likelihood in practice
    (`ratect | head` closing early is the realistic trigger), not
    attempted yet.

7. **Fancy queries terminal width via a `crossterm::terminal::size()`
    ioctl on every repaint** — negligible on its own (a non-blocking
    ioctl, not a syscall that can stall), and the identical-frame skip
    (already fixed) cuts how often it matters in practice, but the query
    itself still runs even on a *suppressed* repaint (needed to build the
    frame the skip then compares). A cached width refreshed via the
    `SIGWINCH` pattern the codebase already uses for interactive-mode
    resize (`docker.rs`) would remove it entirely — lowest priority of
    the efficiency items, given how cheap a single ioctl already is.

## Reuse / duplication

8. **`engine.rs`'s setup-command output splitting re-implements
    `LineBuffer`'s framing rule** (`.lines()` + `trim_end_matches('\r')`
    vs. `LineBuffer::push`/`flush`) — not strictly identical on a
    multi-`\r` edge case (`"a\r\r\n"` → `"a"` today vs. `"a\r"` via
    `LineBuffer`), and the engine holds an owned `String` where
    `LineBuffer` wants bytes + an `FnMut` closure, so switching over is
    arguably *more* ceremony than the current 3 lines. Real but shallow
    duplication — low priority.

## Reviewed, no action needed

These were investigated during the review and found not to need a fix —
recorded so nobody re-investigates them from scratch.

- **Fancy's `keep_updating_startup` re-arm on `TaskGraphResolved`**
  (`fancy.rs`) — theoretically fragile (a bare bool set/cleared at five
  call sites), but unreachable today: the engine always posts
  `TaskGraphResolved` immediately after `TaskStarting` (which fully
  resets logger state) for every task, so no graph event can ever arrive
  after a freeze under the current event-posting order.
- **`OutputStyleArg` mirroring `ui::OutputStyle` in `main.rs`** — the
  mirror enum + `From` impl is the documented, deliberate price of
  keeping `clap` a `ratect`-only dependency (see AGENTS.md's CLI-vs-core
  dependency split); `ValueEnum`'s derived `[possible values: ...]` help
  text and typo-suggestion error have no equivalent-complexity
  string-table replacement.
- **Default non-TTY stdout no longer being pipe-purity by default** — not
  a defect: this is the deliberate, CHANGELOG-documented Batect-`simple`-
  parity change 0.16.0 exists to make. `-o quiet` is the documented
  escape hatch for scripts that need exact container-output-only stdout.

---

# ratect 0.3.0 native config format review

Findings from the focused review of the 0.3.0 native TOML config work
(`git diff 5023a9d..HEAD`) — no correctness bugs found; all findings (a
handful of papercuts plus six test-coverage gaps) closed. See `git log`.

---

# ratect-core/src/docker.rs: a second wide-positional-seam cluster

`docker.rs`'s image/BuildKit helpers immediately above the connection block
that became `docker/connection.rs` in 0.6.0 (`split_image_reference`,
`docker_buildkit_env_value`, `select_builder_version`) are the same shape of
finding the architecture review that motivated that split named — a
self-contained concept sharing a module with container lifecycle by history,
not by relationship. Deliberately not swept into that same change: they have
one call site each (`resolve_image`, `build_image`), so there's no
duplication to buy back, only call-site readability — a different, weaker
class than the one the split was justified by. Worth a look if `docker.rs`'s
size becomes a problem on its own terms, not before.
