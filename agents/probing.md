# Probing a daemon class from a Mac

A Mac's own Docker Desktop or OrbStack daemon, over its local socket,
classifies as `desktop`, and its file sharing makes a container's writes
yours. The other classes still run on a Mac, in minutes, by the methods
below (ratect#813 has the notes they came from).

## Rootful Linux: a Linux client on the Mac's socket

A Linux client classifies OrbStack's daemon as `rootful Linux`, so the
rootful path runs from a Linux container with the socket mounted:

- Mount `/var/run/docker.sock` into a Linux image (`rust:1`, or the
  dev-env base image to build Linux binaries of Ratect) and run the
  `docker-integration` tests, `doctor`, or a probe script from there.
- Mount the scratch directory at the same path on both sides. The daemon
  resolves a bind mount's source on its own side, so a path that differs
  fails in a way that looks like a product bug.
- Run as a non-root user through `setpriv`. For most probes give it real
  entries first (`groupadd`, `useradd`), since a harness of your own may
  fail without them in ways that say nothing about the code.
- Probe a user with **no** entries too: Ratect supports one
  ([decisions/0024](../decisions/0024-host-identity.md)), and leaving it
  out is how a run of failures in that case went unseen. Run it as uid and
  gid 4242 with `setpriv --reuid 4242 --regid 4242 --groups <socket gid>
  -- env -i PATH=/usr/bin:/bin`, with and without `HOME`, as
  `ratect/tests/support/unnamed_host_user.rs` does for the real-daemon
  tests.
- Reach the socket by adding that user to the group that owns it (on
  OrbStack, `root`). The mount is
  OrbStack's real socket, so a `chmod` inside the container changes the
  host's socket, and that change lasts until OrbStack restarts.
- For a capture that will be published, run as uid 1001 named `runner`,
  which matches the GitHub runner and keeps a personal username off the page.

## Rootless and userns-remap: an OrbStack machine

`orb create ubuntu:noble <name>` gives a Linux machine with no KVM needed.
Install Docker in rootless mode on one, or set `userns-remap` in
`/etc/docker/daemon.json` on another, following Docker's own docs. Two
things to record alongside a result:

- An OrbStack machine shares the Mac's kernel. It isn't a separate VM.
- The machine mounts `/Users` with ownership mapped, so a bind mount from
  the Mac's share can't show a root-owned file. Copy a probe project onto
  the machine's own filesystem first.

## Faking a class: a proxy that rewrites `/info`

About 40 lines of Python in front of the socket, rewriting the security
options `/info` reports, drives the real binary as `rootless` or
`userns-remap`. For `rootful Linux` the proxy has to listen on TCP
(`DOCKER_HOST=tcp://…`): a Mac's local socket classifies as `desktop`
unless the security options say otherwise. That checks
what Ratect prints for a class (each output style, `RUST_LOG=off`), not
what the daemon does with files.

## Before trusting a result

- **Run a control first.** Probe on rootful before a remapped daemon. On
  ratect#820 a script bug (`$$` reaching `sh` as its PID) failed every
  write, root's included, which is what a remapped daemon was expected to
  show.
- **Name the axes the probe didn't vary.** A design round that varied the
  daemon and the user missed what the image and the mounts did; record
  the axes left fixed beside the result, in the ADR if there is one.
- **Spend ten minutes trying before writing "not probed".**
