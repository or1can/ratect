FROM alpine:3.18.2
ARG CACHE_BUST
ARG FAIL
# The sleeps make Docker send each printf as a chunk of its own, so the
# indented line and the blank line each start a chunk.
RUN echo "cache bust: $CACHE_BUST"; echo first; sleep 1; printf '  --> src/main.rs:3:5\n'; sleep 1; printf '\n'; sleep 1; echo after-blank; printf 'a\rb\n'; [ -z "$FAIL" ]
