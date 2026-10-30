# Two stages BuildKit runs at once, each printing lines a second apart, so
# their output interleaves.
FROM alpine:3.18.2 AS one
ARG CACHE_BUST
RUN for i in 1 2 3; do echo "$CACHE_BUST one says $i"; sleep 1; done

FROM alpine:3.18.2 AS two
ARG CACHE_BUST
RUN for i in 1 2 3; do echo "$CACHE_BUST two says $i"; sleep 1; done

FROM alpine:3.18.2
ARG FAIL
COPY --from=one /etc/hostname /one
COPY --from=two /etc/hostname /two
RUN [ -z "$FAIL" ]
