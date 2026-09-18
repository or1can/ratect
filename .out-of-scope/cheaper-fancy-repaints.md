# Cheaper Repaints in `fancy` Output Mode

Ratect's `fancy` output mode repaints its whole status block on every event,
rather than diffing and rewriting only the lines that changed — which is what
Batect wanted to do, and what prompted this request. Ratect does skip a
repaint entirely when content hasn't changed since the last one, but when
content does change, the whole block is repainted regardless of how much of
it actually changed.

## Why this is out of scope

Nobody has reported this as a real problem, and the cost hasn't been
measured. The honest first step is measuring the actual cost (e.g. a task
with many dependencies emitting events rapidly) before considering an
optimization at all — building a diffing repaint mechanism speculatively, on
top of `fancy.rs`'s already-fragile cursor-repaint machinery (see ratect#73,
and the text-wrapping issue split from the same parent, #118), would be
exactly the kind of premature complexity this codebase avoids elsewhere.

Revisit once someone actually measures this and it turns out to matter.

## Prior requests

- #100 — "Improved progress UI: remaining Batect-roadmap items" (item 4 of 4)
