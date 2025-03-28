# Run with
# docker build --platform linux/amd64 -f crates/dragonfly-agent/Dockerfile .
# for amd64 specific builds.

FROM rust:1.85.1-alpine3.21 AS builder

WORKDIR /usr/src/dragonfly
COPY . .
RUN cargo build --release -p dragonfly-agent

FROM alpine:3.21
COPY --from=builder /usr/src/dragonfly/target/release/dragonfly-agent /usr/local/bin/
CMD ["dragonfly-agent"]