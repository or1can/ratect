# Review brief

What every reviewer of a design or a change is briefed with: how to class a
finding, what to do with one that argues against a recorded decision, and the
checks a reviewer applies. `settle-design` briefs its break-the-design passes
with this file, and `ready` briefs both its reviews with it.
[decisions/0028](../decisions/0028-design-and-implementation-review.md) says
why there is one file: the wording exists once, so the tools can't drift
apart on it.

This file holds only what a reviewer can act on while reading. The habits an
author follows while writing a fix stay with the author, in
`.claude/skills/ready/SKILL.md`'s "Author habits".

## Classing what a reviewer raises

Each thing a reviewer raises is read against the issue's **design record**,
and tagged with one class. The reviewer tags it; the author confirms the
tag.

- **Bug** (`ready` only): the code departs from the record or the spec. Fix
  it if it's worth fixing.
- **Design gap**: the record is silent on the case. Fix it in flow, and
  append the case to the record.
- **Design flaw**: the record is wrong, or the fix would change one of its
  decisions; cite that decision's id. This is the **design stop**: the
  mechanism goes back to `settle-design`, through the maintainer.
- **Dispute**: argues against a recorded decision without new evidence.

## Disputes

A dispute is not a finding, and nothing is fixed for it. It goes to the
maintainer once per decision, as "reopen D*n*?", whatever number of passes
raise it. New evidence is what separates a design flaw from a dispute: the
record lists each decision's rejected alternatives and why, so a finding
that only restates one of them is a dispute.

## Reviewer checks

- **A comment the change made false means an invariant moved.** Check the
  invariant before accepting a reworded comment.
- **Watch for coverage shaped by the test harness rather than the
  behaviour.** A test or comment that presents the fake's behaviour as the
  daemon's is one: ask what the daemon does there. If the fake can only
  express one ordering of something inherently timing-dependent, the
  untestable orderings are where the bug will be — extend the harness
  instead of concluding the cases are covered.
- **A real-daemon test can mask a missing unit test.** The `#[ignore]`d
  Docker tests don't run in the default suite, so a path they cover can be
  unprotected in `cargo test --workspace`. Assert each effect separately —
  one assertion per thing removed, not one for "cleanup happened".
- **Two tests sharing a fixture directory need a lock, not luck.** `cargo
  test` runs a binary's tests on several threads, and contention over a
  shared fixture surfaces as a behaviour failure ("the cache did not
  persist"), not as a race. A `static Mutex` around the project is the cheap
  fix — see `CACHE_MOUNT_PROJECT`.
- **A mutation check counts once it has shown red.** A green result from a
  mutation that disabled nothing proves nothing.
- **A design record names the axes its probes didn't vary.**
- **For each file the design writes that another tool also writes, name
  whose parser defines its format.** A file two tools write is a mechanism
  (ratect#920): in ratect#905, the bug review found five problems where
  `--approve` rewrote `claims.checked.toml` by its own reading of a format
  `claims` defines.
- **A comment calling a behaviour deliberate should say whose decision it
  was.** Behaviour carried over from Batect can be a platform default it
  inherited (the JVM's `user.home`,
  [decisions/0024](../decisions/0024-host-identity.md)) or a bug it
  shipped (no final newline), rather than a choice. Ask where it came from,
  then judge it by
  [decisions/0013](../decisions/0013-when-ratect-compat-copies-batect.md)'s
  rules like any other Batect behaviour, since knowing its origin doesn't
  settle whether to copy it. 0024 declined to copy Batect's `user.home` home into
  `ratect-compat` under rule 3, since it reads a different `~/.docker` from
  the one `docker login` wrote (ratect#813, the comment of 2026-11-10).
- **A brief is a specification, not a source for what exists.** A ticket's
  "settled" or "don't" may only flag something as hard; check what was
  actually decided.
- **A behaviour that depends on which format/mode you're in needs one
  derived value, not a guard per call site.** Derive it once
  (`include_trust::restricting`, returning `Option<&Bundle>`) and have every
  site consume it, so the divergence is unrepresentable. Guarding
  site-by-site means the next site added is unguarded by default.
- **An error names something the user actually wrote.** A lower layer
  speaks its own vocabulary — `docker.rs` knows an ssh agent id, not which
  container declared it — so an error crossing up from one needs the caller
  to attach that.
- **Never spell config syntax in an error message — name the field.** `set
  'x' to true`, not `add 'x: true'` or `'x = true'`. Unconditional, even
  where the message fires in one format only: `ConfigFormat::Native` is the
  **project's** format, not the **file's**, so a native project can locally
  include a `.yml` and the entry the message names may be YAML.
