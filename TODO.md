# TODO

Findings from the pre-release code review of 0.16.0's output-modes work
(`git diff origin/main...HEAD` at the time, covering `ratect-core/src/ui/`,
the `engine.rs`/`docker.rs` event-posting refactor, and the `--output`/
`--no-color` CLI surface). Working through these — some are being fixed,
some may end up won't-fix. See `git log` for what's landed so far:

- fancy mode's cleanup line erasing unterminated task output
- `TaskFailed` not posting when `--use-network` setup fails early
- fatal errors being reported only via suppressible `tracing::error!`
- fancy mode's per-container line stalling when no pull/build ever fires
  (already-local image, or an image this invocation already resolved)
- fancy/`all` modes measuring plain `char` count instead of real terminal
  display width (CJK/zero-width characters) — new `unicode-width` dependency
- `all` mode dropping a container's buffered final line on a log-stream
  error, plus the duplicated log-follow pipeline that caused it (one shared
  `drain_interleaved_log_stream` helper now, with a `debug`-level breadcrumb
  on the background follower's stream errors)
- `all` mode's fire-and-forget dependency log follower racing cleanup or
  bleeding into the next task's transcript — `stop_and_remove_container`
  now awaits the matching follower before returning
- two latent (no live bug, but a future third `ContainerIoStreaming`
  variant could get it wrong) hazards hardened: `TERM=dumb`'s two-idiom
  duplication folded into `term_environment_variable`, and the two
  independent opposite-polarity interactive-gating checks in `engine.rs`/
  `docker.rs` unified onto one `ContainerIoStreaming::allows_interactive`
- `Console::println`'s redundant explicit `flush()` removed (stdout's own
  `LineWriter` already flushes on the newline `println` always writes);
  the triplicated task-summary-line formatting and the twice-duplicated
  "Cleaning up..." once-guard both consolidated into shared `ui/mod.rs`
  helpers (`format_task_summary`, `OnceFlag`) — pure internal cleanup, no
  behavior change
- `ImagePullProgress`/`ImageBuildProgress`/`SetupCommandOutput` no longer
  allocate or post at all under `simple`/`quiet`/`NullEventSink` — a new
  `EventSink::wants_progress_detail` (`false` by default, overridden by
  `fancy`/`all`) gates all four call sites; no behavior change (still
  rendered exactly as before under `fancy`/`all`, verified against real
  Docker)
- fancy mode now skips a repaint entirely when the rendered content hasn't
  actually changed since the last one — a `last_rendered` cache in
  `repaint_startup`, compared before touching the terminal at all. Docker
  resends the same coarse pull/build status text many times per layer
  while streaming (the byte-progress detail that *does* keep changing
  lives in a field Ratect doesn't render), so this suppresses the large
  majority of "hundreds of repaints/sec during a multi-layer pull" the
  original finding called out. No behavior change — verified against real
  Docker.
- `WidthSource` (a boxed `dyn Fn`) replaced with a plain `fixed_width:
  Option<u16>` field (`None` in production, `Some` only in tests) — pure
  internal simplification, no behavior change.
- `main.rs`'s style→sink selection/construction/validation match moved into
  a new `ui::create_event_sink`, reusable by the planned `ratect-compat`
  binary instead of needing its own copy; `main.rs` now gathers
  stdout/`TERM`/console-dimensions once and passes them to both it and
  `select_output_style` (previously the `-o fancy` validation re-queried
  the same facts a second time). No behavior change — verified against
  real Docker (including the fancy-without-an-interactive-console error
  path).

Everything below is unfixed. Grouped by severity; pick up top-down.

## Correctness

0. **A symlink committed inside a bundle's clone defeats `volumes`/
   `build_directory` containment** (`ratect-core/src/config.rs`,
   `GitBoundary::check_path_allowed`) — CONFIRMED by probe, pre-dating
   0.25.0. A Git-included bundle commits `escape -> /somewhere`, writes
   `local: escape`, and the resolved `<clone>/escape` passes containment
   because the check is purely lexical (`Path::starts_with`); Docker
   dereferences the symlink when it bind-mounts. That is a bundle reading
   and writing outside its clone without `allow_host_paths`, which
   `SECURITY.md` states is a vulnerability and [decisions/0004](decisions/0004-git-include-host-path-trust.md) is meant
   to prevent. The surviving sibling of the `..`-in-an-absolute-path
   escape fixed in 0.25.0; the two are the same class, and only the
   lexical half was closed.

   Not a lexical fix. `check_contains_canonical` solves it for *include*
   targets by canonicalizing, which it can do because an include must
   already exist and be readable — a `volumes` host path need not exist
   at config-resolution time (Ratect or Docker creates it), so the shape
   is to canonicalize the longest existing ancestor, re-append the
   missing tail, and canonicalize both allowed roots too, since a project
   or cache directory may itself sit under a symlink (`/tmp` on macOS).
   Wants its own tests: symlink inside a clone rejected, symlink-free
   path unchanged, nonexistent path still allowed, project directory
   under a symlink still allowed. Deliberately not squeezed into 0.25.0 —
   a security-control behaviour change with real regression surface earns
   its own release and its own review.

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
(`git diff 5023a9d..HEAD`, covering `config.rs`'s native load path, `extends`,
the include refactor and `to_native_toml`, plus `main.rs`'s `config` verbs). The
implementation review found no correctness bugs.

**All findings are now closed** (see `git log`): the `config convert`
non-atomic/TOCTOU overwrite, the cross-include-boundary `extends` anchoring
test, the `config convert` default-source papercut, the ADR-0003
self-verification wording, and — in a follow-up pass before the release — the
six test-coverage gaps below:

- **Cross-format `extends`** — a native container inheriting from a container
  defined in an included YAML file, plus per-extension parser selection for
  local `.yml`/`.toml` includes from a native root.
- **`config convert --stdout`** — prints the document and writes no file.
- **Config-var precedence** — `--config-var` beats the config-vars file, and an
  explicitly-named `.yml` file replaces the auto-discovered `ratect.local.toml`.
- **Base-only container** — no `image`/`build_directory`, used only via
  `extends`, loads and passes `config validate`.
- **Multi-candidate "No bundle file found"** — a pathless `type: git` include
  whose repo has neither candidate names both.
- **v1 limitations and minor branches** — `to_native_toml`'s compact
  string form for `ports`/`volumes` is pinned, and a 3-node `extends` cycle.

One thing the last pass turned up beyond the list: task `description` is a
plain string and is *not* interpolated (matching Batect's own typing), so a
code comment claiming otherwise was corrected.
