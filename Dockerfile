FROM rust:1.94-slim-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 app

COPY --from=builder /build/target/release/itmo-calendar-sync /usr/local/bin/itmo-calendar-sync

USER app
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/itmo-calendar-sync"]
