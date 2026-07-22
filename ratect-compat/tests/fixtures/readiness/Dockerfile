# A dependency container that only becomes "ready" after a delay: its
# command creates /tmp/now-healthy two seconds in, then stays running — the
# readiness e2e test's health check probes for that marker, so anything that
# runs "after the container is healthy" provably ran at least two seconds
# after it merely started.
FROM alpine:3.18.2
CMD ["sh", "-c", "sleep 2 && touch /tmp/now-healthy && sleep 300"]
