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

## Steps

1. **Find what's unreviewed.** The change is the branch since
   `git merge-base origin/main HEAD`, plus anything uncommitted. For each
   review, find the last time it ran in this session, including inside
   `mattpocock-skills:implement`: a run that ranked bugs was the bug review, one reporting
   Standards and Spec was the two-axis review. A review **covers** the change
   only if nothing has changed since it ran. Otherwise its gap is the diff
   from the commit it reviewed to now, and an edit made in response to a
   review is part of that gap.
   *Done when* each review is marked covered, or has a gap named as a diff.

2. **Bug review over its gap.** Invoke the built-in `code-review` at `high`
   over the gap, briefed with the review rules below. For a change that
   drives the Docker daemon, brief it to read the daemon's own sources
   (moby and runc in the Go module cache) for side effects, and probe each
   finding against a real daemon before ranking it: `agents/probing.md`
   says how, for each daemon class. Fix what it confirms, test-first.
   *Done when* each finding is fixed, or listed with why not.

3. **Two-axis review over its gap,** after step 2's fixes, so it reads the
   fixed code. Invoke `mattpocock-skills:code-review` and give both its
   reviewers the review rules below. Fix what it confirms.
   *Done when* each finding is fixed, or listed with why not.

4. **Review the fixes.** Fixes from steps 2 and 3 are a new gap for both
   reviews. Run each over that diff alone, bug review first, and repeat
   until a pass changes nothing. If a pass finds more of the same kind of
   problem in the mechanism the last pass fixed, stop and put the design to
   the user instead (guideline 16: the design is the finding).
   *Done when* a pass changes no file, each of its findings listed with why
   not, or the design is with the user.

5. **Size the claims work,** before reporting the work as done or opening
   its PR, once the change is committed (before a commit, skip to step 6
   and say the count is pending). Run `git fetch origin main`, then
   `pre-commit run --hook-stage pre-push claims-unmarked > "/tmp/claims-$(git rev-parse --short HEAD).log" 2>&1`
   and read the whole log. Its unmarked count is the one to quote: the commit
   hook's list covers only the last commit.
   *Done when* the count comes from a run over the current `HEAD`, or is
   reported as pending.

6. **Report** which review covered which commit, what each found, what was
   fixed, what wasn't and why, and the unmarked claims count.

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
