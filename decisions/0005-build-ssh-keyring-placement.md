# 0005 — Where `build_ssh`'s ssh-agent keyring lives

## Status

Accepted — implemented in `ratect-compat` 0.25.0, as the two commits described
below. Revises [issue #1](https://github.com/or1can/ratect/issues/1), which put
all of this on the `bollard` fork.

Built as decided, with one correction to the sketch below: `paths` is not a
plain "keyring" input. Following Go BuildKit's own `sshprovider`, a path that
*is* a Unix socket forwards that agent instead (and must then be the entry's
only path), so the keyring serves the *remaining* case — ordinary key files.
The classification lives in `docker.rs` (`classify_ssh_agent_paths`), leaving
the keyring itself with no notion of `build_ssh` at all, which is what keeps
property 2 below honest.

## Context

Batect's `build_ssh` supports **multiple named agents** and — the practically
important part — **explicit private key files** (`paths`), served with no agent
running at all. Ratect supports exactly one thing: forwarding the host's own
`SSH_AUTH_SOCK` under BuildKit's implicit `default` id. Everything else is
rejected at config load (`ratect-core/src/config.rs`), which is honest but
leaves the gap open. It is the last known *feature* gap in the [Batect
Parity](../ROADMAP.md#batect-parity) field tables, so it sits on the path to
`ratect-compat` 1.0.0.

BuildKit's `sshforward` service is two RPCs: `CheckAgent(id)` and
`ForwardAgent` (a bidirectional byte stream, agent id in request metadata). The
client bridges each stream to *anything that speaks the ssh-agent protocol*, so
two backends cover the whole feature:

- **Socket** — relay bytes to a running agent's Unix socket.
- **Keyring** — answer the agent protocol in-process from loaded private keys.

The keyring is what makes `paths` work, and it is the case that matters in CI,
where there is usually no agent to forward.

Two constraints shape where the work goes.

**The fork is meant to stop existing.** Ratect consumes `bollard` through a
`[patch.crates-io]` fork carrying session-provider support on `build_image`
([#731](https://github.com/fussybeaver/bollard/pull/731)) and `ping_info`
([#732](https://github.com/fussybeaver/bollard/pull/732)). Both have merged
upstream; the patch remains only until bollard 0.22 is published, at which
point it is dropped outright. Anything added to the fork extends that window and
works against its purpose.

**The fork's SSH support is a stub, not a partial implementation.**
`SshProvider` (`src/grpc/mod.rs`) carries a `src: HashMap<String, PathBuf>` that
nothing reads and a `struct SshSource { agent: (), socket: () }` that is pure
placeholder, while the live path hardcodes the single-agent case in both RPCs:
`check_agent` rejects any id that is not empty or `default`, and
`forward_agent` connects straight to `SSH_AUTH_SOCK`. So there is no multi-agent
scaffolding to extend — the dispatch layer has to be built either way.

**Rust has no free keyring.** Go BuildKit gets one from the standard-adjacent
library: `sshprovider` over `x/crypto`'s `agent.NewKeyring()`. The Rust
equivalent means OpenSSH private-key parsing, per-algorithm signing, and
`rsa-sha2-256/512` flag negotiation — hand-rolled, or a new dependency tree.

## Decision

Split `build_ssh` in two, along the line between plumbing and cryptography.

**1. Named agents, by socket — in the fork.** `SshProvider` gains a real
id → backend map and dispatches on it: `check_agent(id)` succeeds for any known
id (empty meaning `default`), and `forward_agent` selects the backend by the id
in the request metadata. A public `SshAgentSource::Socket(PathBuf)` and
`ImageBuildSessionProviders::set_ssh_agent(id, source)` expose it, with
`enable_ssh(true)` kept as sugar for `("default", Socket($SSH_AUTH_SOCK))` so
existing callers are unaffected. **No cryptography and no new dependencies**,
and this is the half that gets PR'd upstream as the third contribution.

**2. The keyring (`paths`) — in Ratect.** An ssh-agent protocol *server*, over
a temporary Unix socket whose path is handed to the fork's named-agent API. To
BuildKit it is indistinguishable from a real agent, because that is precisely
what a real agent is. It is not a full agent: two request types need real
answers — `SSH_AGENTC_REQUEST_IDENTITIES` (11) → `IDENTITIES_ANSWER` (12), and
`SSH_AGENTC_SIGN_REQUEST` (13) → `SIGN_RESPONSE` (14) — and everything else
(add, remove, lock, unlock, smartcard, extensions) answers `SSH_AGENT_FAILURE`
(5).

Four properties any later change must preserve:

1. **The fork half stays crypto-free.** Its value upstream is that it is
   plumbing; a dependency tree makes it a different conversation.
2. **The keyring stays extractable** — self-contained, with no ratect-specific
   types, following [`dockerignore`](../AGENTS.md)'s own precedent of being kept
   independently publishable without committing to publish it. This is what
   makes the upstream *offer* below a copy rather than a rewrite.
3. **Private keys never cross the session.** Only signatures do — the same
   property Go BuildKit relies on, and the reason serving keys this way is
   acceptable at all.
4. **The socket is private.** It grants signing to anything that can connect, so
   it lives in a directory only the current user can traverse, is created with
   restrictive permissions, and is removed when the build ends. A socket in a
   world-traversable temp directory would let any local user sign with the
   project's keys for as long as the build runs.

**Offer the keyring upstream; don't propose it.** The PR carrying the fork half
says that a working keyring backend exists in our own client and that we would
be glad to contribute it if it is wanted in bollard. Maintainers can take it,
decline it, or take it later, and none of those block shipping here.

## Alternatives considered

- **Hand-roll the keyring in the fork** (issue #1 as written). Rejected: it puts
  a security-sensitive crypto surface into someone else's library, via a fork
  whose entire purpose is to disappear, and makes shipping the feature hostage
  to a review of the hardest possible patch. The 0.21.0 exclusion already cost
  this feature one release cycle waiting on upstream; repeating that with a
  larger ask is the predictable version of the same mistake.
- **Take `ssh-agent-lib`/`ssh-key` as fork dependencies.** Rejected for the fork
  for the same reason — a new dependency tree in a library we do not own is not
  meaningfully easier to land than hand-rolled code, and is arguably harder.
  Both remain live candidates for Ratect's *own* implementation, where the
  choice is ours; `ssh-agent-lib` implements the protocol's server side over
  `ssh-key`, and its maintenance state is worth checking before depending on it.
- **Spawn a real `ssh-agent` and `ssh-add` the keys**, then point the socket
  backend at it — avoiding cryptography entirely. Rejected: it requires those
  binaries to exist on the host (they often do not in a slim CI image),
  introduces process lifecycle and orphan-cleanup problems, cannot handle a
  passphrase-protected key without prompting, and puts the keys in another
  process's memory. More moving parts than serving the protocol ourselves, and
  worse failure modes.
- **Ship only the socket half and never support `paths`.** Rejected: `paths` is
  the half that matters most, precisely because it works where no agent is
  running. Shipping half and declaring parity would misrepresent the gap.
- **Keep waiting for upstream.** That was 0.21.0's decision and it was right at
  the time — the session-provider API was unsettled and building on it risked
  rework. Both PRs have now merged, which settles the API; the reason to wait
  has expired.

## Consequences

- The upstream PR is small, mechanical, and easy to argue for. If it stalls
  anyway, nothing here is blocked: the fork already carries it.
- **Ratect takes on a cryptographic dependency**, in a security-sensitive role.
  That is the real cost of this split, accepted deliberately because owning the
  choice is better than pushing it onto a library that has not asked for it.
  It deserves the same scrutiny as any dependency in `AGENTS.md`'s list, plus an
  eye on advisories — `cargo audit` already runs in CI.
- **Unix-only**, unchanged from today: `forward_agent` is already
  `#[cfg(not(windows))]`, so the Unix-socket assumption predates this decision
  rather than being introduced by it. Windows support for `build_ssh` would need
  named pipes on both sides and is out of scope here.
- **The keyring is reachable through a filesystem socket, where Go BuildKit's is
  not** — the one place this design is measurably weaker than what it ports, so
  it is recorded rather than left to be rediscovered. `sshprovider` serves its
  keyring over an in-memory `net.Pipe()`: no filesystem object exists, so there
  is no permissions question and no local attack surface at all. Ratect needs a
  socket only because the fork's `SshAgentSource` (decision 1 above) is
  socket-based — `SshProvider::connect` returns a concrete `UnixStream`.

  The position taken, deliberately: **match OpenSSH's own `ssh-agent`**, which
  is the reference implementation of an agent that *does* expose a socket. That
  means both of its barriers, not one — an unpredictably-named `0700` directory
  (`mkdtemp`) *and* the socket file itself at `0600` (`umask(0177)` around the
  bind; a `chmod` here, since our umask is process-global and we are
  multi-threaded). Either alone suffices in isolation, which is exactly why
  taking only one would rest the whole protection on a single `mode` argument.
  Combined with the exposure being one build long, and only ever the keys the
  config already names, that is judged good enough.

  The gap could be closed outright by having the provider serve from an
  in-process duplex stream instead of a socket path, matching `net.Pipe()`
  exactly. Not taken *now* because it costs the fork's public API its
  plain-data enum — `SshAgentSource` would have to carry a stream factory,
  losing `Clone`/`PartialEq`/`Debug` — and the upstream PR's whole argument is
  that it is small and mechanical. Worth offering if upstream shows appetite for
  it; revisit here rather than deciding it inside a review.
- Extractability is a standing constraint, not a preference. Coupling the
  keyring to Ratect's config or error types costs nothing visible and silently
  forfeits the upstream offer.
- The work lands as two commits, in order: the fork's socket dispatch (which is
  the same work under any of the alternatives above), then the keyring.
