FROM rust:1.94-slim-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

COPY --from=builder \
    /etc/ssl/certs/ca-certificates.crt \
    /etc/ssl/certs/ca-certificates.crt
COPY --from=builder \
    /build/target/release/itmo-calendar-sync \
    /usr/local/bin/itmo-calendar-sync

USER 10001:10001
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/itmo-calendar-sync"]
