FROM rust:1.90.0-slim-bookworm@sha256:64232e656c058f4468e8d024e990acff04f0fd5a5c0a88a574dc37773d7325c9 AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY mysqltest ./mysqltest

RUN cargo test --locked \
    && cargo test --release --locked --test benchmark -- --ignored --nocapture \
    && cargo build --release --locked

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

RUN mkdir -p /data

COPY --from=builder /build/target/release/minivectordb /usr/local/bin/minivectordb

WORKDIR /app
VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/minivectordb"]
CMD ["--data-dir", "/data"]
