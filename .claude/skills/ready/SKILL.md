---
name: ready
description: Review a change until both reviews, the bug hunt and the two-axis conformance review, cover it. Use before each commit, and before reporting work as done or opening its PR.
---

Two reviews find different things here, and neither replaces the other. The
**bug review** is the built-in `code-review` skill, which hunts for behaviour
that's wrong. The **two-axis review** is `mattpocock-skills:code-review`,
which checks the change against this repo's standards and against its issue.
Both answer to the bare name `code-review`, so always invoke them by these
names. Over the 0.13.0 milestone the bug review found what the two-axis
review missed on six PRs, and only ran when the maintainer asked (ratect#813).

A `/ready` call **ends done** or **ends blocked**. Done means the change
may be committed and reported as done. Blocked means neither: don't commit,
don't report the work as done, and skip straight to step 6 to report
what blocked it. The user is **present** in an interactive session,
whoever started the call: ask, and wait for the answer. They are
**absent** in a non-interactive session or a subagent, where nothing
asked gets answered. A fix **changes code** if its diff changes anything
but prose. A comment or doc comment in a source file is prose; a test is
code, and so is a doc comment's code block (a doctest), a command a doc
gives to run (a code block, a `<!-- verify: -->` marker), and an
instruction in a skill or agent doc, which an agent carries out. State
(each review's coverage, and a call's round, ledger, restart and "keep
fixing" answers) lives in the session only, so a new session starts with
no coverage. What compaction loses counts the safe way: an id or ledger
as never recorded, the round as `fix 3`, the restart as used, and a
"keep fixing" answer as not given.

## Steps

1. **Find what's unreviewed.** The change is the branch since
   `git merge-base origin/main HEAD`, plus everything uncommitted,
   untracked files included. A run over the change that ranked bugs was
   the bug review, one reporting Standards and Spec was the two-axis
   review, wherever it ran. Each
   review's **coverage** is the id of the tree it last read in a run that
   counts, on this branch in this checkout. Record an id when a review
   starts reading, from a temporary index so the real one is untouched:

   ```sh
   idx=$(mktemp -u) && GIT_INDEX_FILE=$idx git read-tree HEAD &&
     GIT_INDEX_FILE=$idx git add -A && GIT_INDEX_FILE=$idx git write-tree;
     rm -f "$idx"
   ```

   A run with no recorded id covers nothing. A review's **gap** is `git diff <covered id> <current id>`, the current
   id made the same way, or with no coverage the merge-base to the current
   id; an empty gap means it covers the change. A tree, not a commit,
   because a review here reads before the commit (guideline 15). An id,
   not a description, because committing the reviewed changes unedited
   gives the same id, while an edit made after a review and then committed
   (to clear a claims-hook finding, say) still shows. Brief each review
   with its gap as that `git diff`, in place of its own: the two-axis
   review's step 1 otherwise pins a fixed point and diffs
   `<fixed-point>...HEAD`, committed history only, and the bug review's own
   diff leaves out untracked files.

   A run counts once each of its findings is fixed or listed with why
   not, and covers only what it read: one pointed at a single file, a PR,
   or a committed range while edits sat uncommitted covers no more, and
   one in a subagent the session sees only as a summary counts only if
   the summary gives the id. Runs inside a `/ready` call count only if the
   call ends done, and then both reviews cover its final tree, whose id
   the call records as it ends, though some
   of its last fixes were read by one review or neither (step 6 lists
   them). Runs in a call that ended blocked, or in an attempt a design-stop
   restart abandoned, count for nothing. A rebase onto, or merge of, a
   newer `origin/main` drops coverage, since the diff from the old tree
   would carry everyone else's merged work.
   *Done when* each review is marked covered, or has its gap as a diff
   between two ids.

2. **Bug review, then its fix loop.** If step 1 marked the bug review
   covered, go to step 3. Otherwise invoke the built-in `code-review` at
   `high` over its gap, briefed with the review rules below.
   For a change that drives the Docker daemon, brief it to read the
   daemon's own sources (moby and runc in the Go module cache) for side
   effects, and probe each finding against a real daemon before ranking
   it: `agents/probing.md` says how, for each daemon class. This pass is
   round `initial`. Check every pass's confirmed findings, this one's and
   each fix round's, against the design stop (below) before fixing them;
   then fix them, test-first for behaviour.

   Every fix a pass in this loop asks for is one of its round's fixes:
   docs, skill text and a fix to prose a finding showed false included. A
   docs-only change still needs the bug review: the ratect#877 note's review
   found the lock hazards that became ratect#880. A finding listed with why
   not (rejected or deferred) counts as no finding. After each round's
   fixes, sweep (below), then:

   | After round | If | Then |
   | --- | --- | --- |
   | any | the round fixed nothing | end the loop |
   | `initial`, `fix 1` | otherwise | bug pass over this round's fixes alone; next round |
   | `fix 2` | its fixes changed code | bug pass over them; round `fix 3` |
   | `fix 2` | its fixes changed only prose | end the loop |
   | `fix 3` | — | end the loop (the cap) |

   Read "changed code" off fix 2's diff, not off how its findings
   were classified. The fixes of the round that ends the loop aren't
   read by any bug pass, only by step 3's two-axis review, and step 6
   lists them. When the cap ends the loop, that is its accepted cost:
   over November's reviews, fix round 3 and later found no new mechanism,
   and their 5 behaviour findings all recurred on mechanisms earlier
   rounds had raised (ratect#911).

   Every pass is at `high`, fix rounds and docs-only changes included.
   `medium` for the fix rounds was considered and rejected: its motive, a
   `high` pass never coming back clean so the loop never ends, is gone with
   the cap, and fix round 2 found 28 behaviour findings in 18 passes
   (ratect#911). Brief every pass after `initial` with the **ledger**: each
   of this call's earlier findings, and any in a ledger it started with
   (below), and what was done with it (fixed, and
   how; or rejected or deferred, and why). Tell it to hunt its gap as
   before, not only the ledger's items; not to re-report a rejected finding
   without new evidence; and to tag a finding that recurs on a mechanism in
   the ledger. A tag feeds the design stop, but you confirm it. A call
   that follows a blocked one on this branch starts with that call's
   ledger, and briefs its `initial` pass with it too; its "keep fixing"
   answers don't carry over.
   *Done when* the loop has ended with each finding fixed or listed with
   why not, or the call has ended blocked at the design stop.

3. **Two-axis review, once,** after the loop ends: work out its gap
   again on the final tree, and if it isn't empty, invoke
   `mattpocock-skills:code-review` over it, so spec and standards are
   checked on what ships. Give it the issue the change is for, since with
   the change uncommitted it has no commit message to find the issue in.
   Give both its reviewers the review rules below, and ask for cosmetic findings
   separately. It runs once per call, never inside the fix loop: before the
   loop, the loop's fixes could break the spec unchecked, and in November
   half its findings were cosmetic (ratect#911). Fix what it confirms;
   these fixes don't go back into the loop. A fix that changes no code is
   swept, not reviewed. If any fix changes code, run **one** bug pass at `high` over
   those fixes, briefed with the ledger and outside the cap. Its findings
   go to the design stop first, as in step 2, where the two-axis review's
   fixes count as the previous round and "keep fixing" means fixing them
   here; sweep its fixes and list them as unreviewed, with no further
   loop.
   *Done when* each finding is fixed or listed with why not, and any
   bounded bug pass has run; or the call has ended blocked at the design
   stop.

4. **Check the Windows build,** last, with
   `uv run tools/check-windows-build.py`, which runs CI's Windows Check
   locally. Neither review compiles the `cfg(not(unix))` side of a gate,
   and Windows Check isn't a required status, so a break there is found
   after the PR opens unless it's found here (ratect#904). A change to
   nothing cargo reads (Rust sources, manifests, `Cargo.lock`, `.cargo/`
   config, a file a crate includes) and not to the check itself or CI's
   `windows-check` job skips it and says so. If it fails, fix it, list
   the fix as unreviewed, and re-run once. If it still doesn't exit 0, for
   whatever reason, including one the change didn't cause, the call ends
   blocked.
   *Done when* it exits 0 over the final tree or was skipped, and the call
   ends done; or the call ends blocked.

5. **Size the claims work,** before reporting the work as done or opening
   its PR, once the change is committed (before a commit, skip to step 6
   and say the count is pending). Run `git fetch origin main`, then
   `pre-commit run --hook-stage pre-push claims-unmarked > "/tmp/claims-$(git rev-parse --short HEAD).log" 2>&1`
   and read the whole log. Its unmarked count is the one to quote: the commit
   hook's list covers only the last commit.
   *Done when* the count comes from a run over the current `HEAD`, or is
   reported as pending.

6. **Report** whether the call ended done or blocked (and if blocked, the
   open point or the failing check), which tree id each review covered,
   what each found, what was fixed, what wasn't and why, what each sweep
   re-read and fixed and any grep it ran, the fixes no bug pass read (the
   last loop round's, which only the two-axis review read, and the two-axis
   review's prose fixes, the bounded pass's and the Windows check's, which
   no review read),
   the Windows check's result (or that it was skipped), and the unmarked
   claims count.

## The sweep

Fixes make the next round's findings: most prose-false findings from fix
round 2 on were made false by the round before (ratect#911). So before
every bug pass after `initial` and before the call ends, re-read every
comment, doc sentence, error and log string that names what the round's
fixes changed, and fix what they made false. Every round sweeps. Sweep
edits belong to the round they were made in.

If the fixes renamed an identifier or term, or changed what one means,
also run `git grep --untracked` over the whole checkout, not the diff, for
the old name and the old defining phrases. The reviews read only the diff,
so they find leftovers elsewhere only a few per pass (ratect#906). Fix
each hit or say why it stands; a shipped `CHANGELOG.md` or `RELEASES.md`
entry stands (guideline 9). Grep matches spelling, not meaning, so a
sentence asserting the old meaning in other words needs reading around the
hits, not another grep. The `claims` `restatement` check doesn't replace
this: it runs at commit or through `check-claims`, keyed on text you
deleted, and guideline 16 says what it misses.

## The design stop

Recurring findings on one mechanism mean the design is the finding
(guideline 16). Class each confirmed finding yourself, by ratect#911's
kinds: **behaviour** (wrong runtime behaviour, a leak, a wrong exit
code; in skill text, an instruction that leads an agent to a wrong
action), **test** (a missing test, or one that can't fail),
**prose-false** (a doc, comment or message saying something untrue that
no agent acts on), **spec** (doesn't do what the issue asked) and
**cosmetic** (wording, style or structure, with nothing untrue). The stop
fires when:

- a pass reports a confirmed `behaviour` or `test` finding that repeats a
  problem in the mechanism the previous round's fixes changed (prose
  made false by a fix is the sweep's, not this); or
- at `fix 3`, a pass reports a confirmed `behaviour` finding on a
  mechanism an earlier round of this call fixed.

Neither fires on a mechanism the user has answered "keep fixing" for in
this call; that answer survives a restart. When it fires:

- **User present, no restart used:** put two options, recommending the
  first: rethink the mechanism with `settle-design`, or keep fixing. Twice
  in 0.14.0 the recommendation was to carry on, and both times the
  maintainer reversed it (ratect#902). "Keep fixing" carries on at the
  current round. Once `settle-design` settles the design, rework the code
  to it and restart the call at step 1, keeping only that the restart is
  used, the "keep fixing" answers, and any ledger the call started with
  (step 2). The abandoned attempt's runs count
  for nothing (step 1), so the restarted gap is the call's original one
  plus the rework, and the triggering pass's findings elsewhere get read
  again. The rework has no previous round, so the first trigger can't
  fire on its first pass.
- **User present, restart used:** put two options: keep fixing, or end
  blocked.
- **User absent, `settle-design` left the part open, or the user chose to
  end:** the call ends blocked, with the code as it is. With an issue,
  post the open point on it under an "Open point to settle first" heading
  and move it to `ready-for-human`, unless `settle-design`'s own record
  already did.

So a call runs at most five bug passes (four in the loop, one after the
two-axis review) and one two-axis pass, doubled by its one restart, and
the Windows check at most twice. Only a present user's answer can take
unbounded time, and that wait is the user's.

## Review rules

For both reviewers:

- **A comment the change made false means an invariant moved.** Check the
  invariant before accepting a reworded comment.
- **A test or comment that presents the fake's behaviour as the daemon's**
  is coverage shaped by the harness. Ask what the daemon does there.
- **A mutation check counts once it has shown red.** A green result from a
  mutation that disabled nothing proves nothing.
- **A design record names the axes its probes didn't vary.**
- **A comment calling a behaviour deliberate should say whose decision it
  was.** Behaviour carried over from Batect can be a platform default it
  inherited (the JVM's `user.home`,
  [decisions/0024](../../../decisions/0024-host-identity.md)) or a bug it
  shipped (no final newline), rather than a choice. Ask where it came from,
  then judge it by
  [decisions/0013](../../../decisions/0013-when-ratect-compat-copies-batect.md)'s
  rules like any other Batect behaviour, since knowing its origin doesn't
  settle whether to copy it. 0024 declined to copy Batect's `user.home` home into
  `ratect-compat` under rule 3, since it reads a different `~/.docker` from
  the one `docker login` wrote (ratect#813, the comment of 2026-11-10).
- **A brief is a specification, not a source for what exists.** A ticket's
  "settled" or "don't" may only flag something as hard; check what was
  actually decided.

Each of these has caught something a review missed:

- **Anchor an inserted item on the preceding item's closing brace, never on
  the new one's attributes.** A Rust item's doc comment sits *above* its
  `#[test]`/`#[derive]` attributes, so anchoring there splices the new item
  into the previous one's documentation — silently, and the compiler is
  happy. The `claims` plugin's `spliced-docs` check finds the ones that
  get through; it's advisory (never blocks a commit), so read each
  candidate rather than treating a report as a defect.
- **Re-read every string you added, in its final control-flow position.**
  Nothing type-checks a log or error message, so they stay as written while
  the code around them moves. For errors specifically: name something the
  user actually wrote. A lower layer speaks its own vocabulary — `docker.rs`
  knows an ssh agent id, not which container declared it — so an error
  crossing up from one needs the caller to attach that.
- **When a change alters observable behaviour, re-read that behaviour's
  whole doc section and *run* each claim against the binary.** Not grep —
  execute. Every claim: the example output, the flag descriptions, the "this
  does X" sentences. Capture real output and `diff` it rather than editing
  what looks wrong, since what looks wrong is exactly the set you already
  believe. Grep is the fallback for claims nothing can be run against (a
  roadmap entry, a design note); it is not the check (this rule was once
  grep, and reported clean while missing three).

  To find which claims to re-read, the `claims` plugin's `stale-claims`
  check ranks prose by how much the code it names has moved since the
  claim was last touched. Treat a hit as "re-read this", never as "this
  is wrong" — it measures churn, so a true claim about a hot file looks
  suspicious. Don't record its candidate count anywhere: the number rises
  when code moves and falls when a doc is fixed, so it measures neither.
- **Verify a claim before writing it, not after a reviewer questions it** —
  the review-time half of the change loop's write-prose step.
- **A claim copied from the handoff is unverified.** The handoff is a
  *specification* — what to build, and why it is worth building. It is not
  a source for what exists. So every name in it that you carry into a doc
  comment, an error, or a page under `docs/` gets run first: a flag against
  `--help`, a mode against the code that would implement it, a path against
  the filesystem. This is the change loop's "the code includes whatever the
  prose *names*", applied to the one input that most looks like it has
  already been checked (`RELEASES.md`'s 0.26.0 entry corrects one that
  wasn't). Correct the handoff where it is still live;
  where it is already struck through, record the correction in its
  done-summary rather than editing history.
- **A behaviour that depends on which format/mode you're in needs one
  derived value, not a guard per call site.** Derive it once
  (`include_trust::restricting`, returning `Option<&Bundle>`) and have every
  site consume it, so the divergence is unrepresentable. Guarding
  site-by-site means the next site added is unguarded by default.
- **Never spell config syntax in an error message — name the field.** `set
  'x' to true`, not `add 'x: true'` or `'x = true'`. Unconditional, even
  where the message fires in one format only: `ConfigFormat::Native` is the
  **project's** format, not the **file's**, so a native project can locally
  include a `.yml` and the entry the message names may be YAML.
- **Watch for coverage shaped by the test harness rather than the
  behaviour.** If the fake can only express one ordering of something
  inherently timing-dependent, the untestable orderings are where the bug
  will be — extend the harness instead of concluding the cases are covered.
- **Run a self-cleaning test twice, asserting external state (`docker volume
  ls`) rather than the exit code.** One run passes whether or not the
  cleanup matched anything — the first run starts clean by definition.
- **Two tests sharing a fixture directory need a lock, not luck.** `cargo
  test` runs a binary's tests on several threads, and contention over a
  shared fixture surfaces as a behaviour failure ("the cache did not
  persist"), not as a race. A `static Mutex` around the project is the cheap
  fix — see `CACHE_MOUNT_PROJECT`.
- **A real-daemon test can mask a missing unit test.** The `#[ignore]`d
  Docker tests don't run in the default suite, so a path they cover can be
  unprotected in `cargo test --workspace`. Assert each effect separately —
  one assertion per thing removed, not one for "cleanup happened".
