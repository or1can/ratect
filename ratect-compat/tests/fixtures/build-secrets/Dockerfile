FROM alpine:3.18.2
RUN --mount=type=secret,id=token cat /run/secrets/token > /secret-out.txt
