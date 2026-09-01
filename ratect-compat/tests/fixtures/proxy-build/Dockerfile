# Proves the host-gateway entry reaches an image *build*, not just a
# container: `build_image` passes it as the `/build` endpoint's `extrahosts`
# parameter, a wire format no unit test can exercise.
#
# The grep is the assertion. `/etc/hosts` is written by the daemon from
# `extrahosts` alone — Docker Desktop resolves `host.docker.internal`
# through its own DNS without ever writing the file — so the entry being
# present here means this build asked for it and got it, on Linux and on
# Docker Desktop alike. Without it the build fails on this line.
FROM alpine:3.18.2
RUN grep host.docker.internal /etc/hosts
