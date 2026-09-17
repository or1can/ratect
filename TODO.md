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

9. **The `claims` plugin's checks have no CI-level backstop, only the local
   `git commit` hook** (`decisions/0009`) — a PR opened without Claude Code
   (a plain shell commit, another editor's Git integration, or a bot account
   like Renovate) merges without `check-links`/`check-citations`/
   `executable-claims` ever running against it. Wiring `python3 -m
   claims.cli --repo-root .` into `.github/workflows/ci.yml` would close
   this, but `or1can/claims` has no tagged releases yet to pin a CI checkout
   against — revisit once it does, rather than pinning CI to an arbitrary
   commit SHA in the meantime.

12. **Released archives don't include `NOTICE`** — `dist-workspace.toml`
    has no `include`/similar key adding it, so each archive ships
    `LICENSE`/`README.md`/`RELEASES.md` (dist's own defaults) but not the
    file carrying the `moby/patternmatcher` (Apache-2.0) and
    andrej-karpathy-skills (MIT) attributions this repo's own `NOTICE`
    records. Apache-2.0 §4(d) asks for it to travel with redistributions.
    Found reviewing `docs/installation.md`'s "what's in the archive" claim
    (ratect#38); fixing it is a `dist-workspace.toml` config change, out
    of scope for a docs ticket.

13. **No confirmation prompt on `ratect resources clean --all-projects`**
    (`ratect/src/main.rs`) — the one thing `list`-before-`clean` can't catch
    is typing the dangerous command by accident, which only a prompt does,
    since a dry run only helps if you remembered to run it first. Deferred
    rather than rejected when the verb shipped ([0.2.0](RELEASES.md#ratect)):
    it would be the first interactive prompt in either binary (Batect has
    none, so there's no precedent), it needs a `--yes` escape for CI, and the
    two-layer guard on what `--all-projects` can even reach already removes
    the catastrophic version of the mistake. Worth revisiting on the first
    report of a near-miss.

14. **`resources` can't distinguish a concurrently-running task's containers
    from an orphan** (`ratect-core/src/resources.rs`) — they're labelled
    identically, because until the run ends they *are* the same thing, and
    the daemon can't say whether some other `ratect` process still cares
    about a container. `list` reporting age and `clean` taking
    `--older-than` is the honest mitigation; claiming to detect liveness
    would be a lie. If this bites in practice, the next step would be a
    heartbeat (a running invocation touching its own resources
    periodically) rather than any attempt to infer liveness after the fact.

## Test coverage

4. **`ratect-compat/tests/cli.rs`'s `task_output` helper weakens ~18 converted e2e
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
   (`ratect-compat/tests/cli.rs`) — `rposition` of a line matching
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

## Reviewed, no action needed

These were investigated during the review and found not to need a fix.
Kept here only when a future reviewer would plausibly rediscover the same
finding independently and burn time re-deciding it — not every dismissed
idea earns a permanent entry; a triaged, low-stakes finding unlikely to
resurface is just dropped once decided, no residue.

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
- **`engine.rs`'s setup-command output splitting re-implements
  `LineBuffer`'s framing rule** (`.lines()` + `trim_end_matches('\r')`
  vs. `LineBuffer::push`/`flush`) — not strictly identical on a
  multi-`\r` edge case (`"a\r\r\n"` → `"a"` today vs. `"a\r"` via
  `LineBuffer`), and the engine holds an owned `String` where
  `LineBuffer` wants bytes + an `FnMut` closure, so switching over is
  arguably *more* ceremony than the current 3 lines. Real but shallow
  duplication, not worth unifying.

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
