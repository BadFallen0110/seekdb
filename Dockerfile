FROM rust:1.90.0-slim-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests

RUN cargo test --locked && cargo build --release --locked

FROM debian:bookworm-slim

RUN mkdir -p /data

COPY --from=builder /build/target/release/minivectordb /usr/local/bin/minivectordb

WORKDIR /app
VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/minivectordb"]
CMD ["--data-dir", "/data"]
