FROM rust:1.86.0-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    musl-tools \
    make \
    gcc

RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app

COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml
COPY ./src ./src

RUN cargo build --release --locked --target x86_64-unknown-linux-musl

FROM scratch AS runtime

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/kaiwadb-agent /usr/local/bin/kaiwadb-agent

EXPOSE 9100

CMD ["kaiwadb-agent", "--token", "$KAIWADB_AGENT_TOKEN"]
