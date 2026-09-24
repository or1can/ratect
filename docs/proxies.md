# Proxies

When `http_proxy`, `https_proxy`, `ftp_proxy` or `no_proxy` are set in the
environment Ratect itself runs in — in either case, so `HTTP_PROXY` too —
they are injected into every container's environment and every image
build's build arguments. A task or a build that needs to reach the network
through a proxy just works, without repeating the proxy settings in
`environment` or `build_args` by hand. There is no config field for this in
either format; it is automatic, on both binaries, and
[`--no-proxy-vars`](#turning-it-off) turns it off. This page is the one
description of what is injected, how it is adjusted for a container's point
of view, and what it can't fix.

## Precedence

Where an injected variable sits among a container's other environment
variables, and among a build's `build_args` — what overrides it on a key
collision — is [Environment
precedence](ratect-compat-config-reference.md#environment-precedence).

## `no_proxy` is extended

Every container sharing a task's network — the task's own container and each
of its dependencies — has its own name appended to `no_proxy`/`NO_PROXY`, so
traffic between them isn't sent through the proxy. Not done for image
builds: nothing is running yet during a build, so there is nothing to
exempt.

## A proxy on `localhost`

An `http_proxy`/`https_proxy`/`ftp_proxy` value that points at `localhost`,
`127.0.0.1` or `::1` is rewritten to `host.docker.internal`, since
`localhost` from *inside* a container refers to the container itself, not
the machine running the proxy. A value that isn't an `http`/`https` URL, or
doesn't refer to the local machine, is left unchanged.

This happens on every platform, including Linux — where, unlike under Docker
Desktop, nothing supplies `host.docker.internal` by itself. So a run that
rewrote a URL also adds `host.docker.internal:host-gateway` to every
container it starts and every image it builds, using Docker's own
`--add-host` mechanism, which the daemon resolves to the machine running it.
That entry is added on **every** platform too, not only the one that needs
it: on macOS and Windows it names the same gateway Desktop would have
answered with, so it is a new `/etc/hosts` line rather than a change in what
the container can reach. What gates it is the rewrite, not the platform — no
rewrite, no entry — and a container's own
[`additional_hosts`](ratect-compat-config-reference.md#container) entry for
that name always wins over it.

### A proxy bound to loopback only

Rewriting the URL can't make an unreachable proxy reachable. A proxy bound
only to `127.0.0.1` — which is what `cntlm` and similar default to — still
refuses a connection from a container, so on Linux Ratect checks
`/proc/net/tcp`/`tcp6` and warns, once per run and naming the port:

> The proxy on port 3333 is listening on loopback addresses only, so containers
> in this run cannot reach it even though its URL now names the host. Bind the
> proxy to 0.0.0.0 to make it reachable — which also exposes it to anything else
> that can reach this machine, so do that only on a network you trust. Use
> `--no-proxy-vars` if this run doesn't need the proxy.

It's a warning, not a failure: the run may never use the proxy. Note the
security cost it names — binding a proxy to `0.0.0.0` opens it to everything
that can reach the machine, so weigh that rather than applying it
reflexively. Some proxies offer the same thing as a narrower setting:
[`cntlm`](https://manpages.debian.org/trixie/cntlm/cntlm.1.en.html), for
instance, binds `127.0.0.1:3128` by default and listens on every interface
only under its `Gateway` option.

### A host firewall

A host firewall can block the container's traffic even when the proxy is
bound wide enough, and that case Ratect can't detect — so if the warning
doesn't fire and the proxy still isn't reachable, check there next.

What to look for: the connection arrives on the bridge interface of the
*user-defined* network the run is using, never Docker's default `docker0`
bridge, so a firewall rule written for `docker0` won't cover it. Ratect
creates a network per task, or uses the one
[`--use-network`](ratect-cli.md#run-options) names — and a
run can't fall back to the default bridge even if you point `--use-network`
at it, because Ratect gives every container a network-scoped alias and
Docker only allows those on user-defined networks (`docker run` refuses with
`network-scoped aliases are only supported for user-defined networks`).

Match the rule on that network's **subnet**, which `docker network inspect
<network>` reports under `IPAM.Config`. Its interface name is generally not
in that output — Docker only records one there when the network was created
with `com.docker.network.bridge.name`, which is how `docker0` itself gets
its name — so a rule pinned to an interface is both harder to write and
easier to get wrong.

## Turning it off

`--no-proxy-vars` disables all of the above for one run: nothing is injected
into any container or build, `no_proxy` is left alone, and no URL is
rewritten. It is a [`ratect run`](ratect-cli.md#run-options) option, and a
[`ratect-compat`](ratect-compat-cli.md#task-execution) flag.

## Coming from Batect?

Two deliberate differences: a `localhost` proxy is rewritten on Linux too,
where Batect propagates it verbatim, and a loopback-bound proxy is diagnosed
rather than left to fail. Both are listed under [Differences from
Batect](differences-from-batect.md#runtime-behavior-gaps).
