# `doctor` check: daemon proxy settings vs. local environment

Ratect does not warn when the Docker daemon's own proxy configuration
(readable via the Docker API, `SystemInfo.http_proxy`/`https_proxy`/
`no_proxy`) differs from the local shell environment's proxy variables.

## Why this is out of scope

The signal is unreliable. `ratect-core` fully supports remote and
non-local Docker daemons (Docker CLI contexts, `DOCKER_HOST`, Docker
Desktop's own VM) — a remote or VM-hosted daemon legitimately has its own
network path and proxy needs independent of the local shell's environment,
so a mismatch there is expected, not a misconfiguration. Compounding that,
the Docker API itself documents that containers do not automatically
inherit the daemon's proxy configuration anyway, so even a genuine mismatch
wouldn't clearly predict a container-level problem. Likely to produce false
positives for anyone using a remote or Desktop-VM daemon — normal,
supported usage here — rather than catch real misconfigurations.

## Prior requests

- #101 — "`ratect doctor`: remaining checks" (item 6 of 6)
