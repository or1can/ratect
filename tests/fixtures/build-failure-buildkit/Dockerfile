FROM alpine:3.18.2
RUN --mount=type=secret,id=token echo "this buildkit line should reach the user" && false
