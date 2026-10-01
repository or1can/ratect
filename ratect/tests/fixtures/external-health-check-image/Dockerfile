# `traefik/whoami` with a `HEALTHCHECK` that can never pass: the image is
# `scratch`-based, so there is no `/no-such-check` (or anything else) to
# exec, and Docker marks the container unhealthy after its first probe.
FROM traefik/whoami:v1.10.2
HEALTHCHECK --interval=1s --retries=1 CMD ["/no-such-check"]
