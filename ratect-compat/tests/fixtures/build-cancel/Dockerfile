# Each tick is a request to whatever `http_proxy` names — the test's own
# listener on the host, reached through the host-gateway entry a rewritten
# `localhost` proxy URL adds to the build. Ticks that keep arriving after
# ratect has exited mean the daemon is still running this `RUN` step.
FROM alpine:3.18.2
ARG CACHE_BUST
RUN echo "cache bust: $CACHE_BUST" && \
    for i in $(seq 1 120); do wget -q -O /dev/null "http://tick/$i" || true; sleep 1; done
