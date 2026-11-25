# screentime-server, plus the screentime CLI for `docker exec`.
#
#   docker build -t screentime-server .
#   docker run -d --name screentime -p 8470:8470 -v screentime:/data \
#     -e TZ=America/Chicago --restart unless-stopped screentime-server
#
# Set TZ to the PCs' time zone: "today" (for overrides and status) is the
# server's local date. Builds on x86_64 and aarch64 (e.g. a Raspberry Pi).
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY examples examples
RUN cargo build --release --locked --bin screentime-server --bin screentime

FROM alpine:3
RUN apk add --no-cache tzdata
COPY --from=build /src/target/release/screentime-server /src/target/release/screentime /usr/local/bin/
ENV SCREENTIME_SERVER_DATA=/data \
    SCREENTIME_SERVER=http://127.0.0.1:8470
VOLUME /data
EXPOSE 8470
CMD ["screentime-server"]
