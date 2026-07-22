FROM alpine:3.18.2
RUN echo "an earlier step that should appear first"
RUN --mount=type=secret,id=token echo "this buildkit line should reach the user" && false
