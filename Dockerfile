FROM rust:1.94.0-slim-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim@sha256:74d56e3931e0d5a1dd51f8c8a2466d21de84a271cd3b5a733b803aa91abf4421
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/vea-validator /app/vea-validator

ENV ARBITRUM_RPC_URL="" \
    ETHEREUM_RPC_URL="" \
    GNOSIS_RPC_URL="" \
    WETH_GNOSIS="" \
    VEA_INBOX_ARB_TO_ETH="" \
    VEA_OUTBOX_ARB_TO_ETH="" \
    VEA_INBOX_ARB_TO_GNOSIS="" \
    VEA_OUTBOX_ARB_TO_GNOSIS="" \
    ARB_OUTBOX="" \
    MAKE_CLAIMS="false" \
    SEQUENCER_INBOX=""

ENTRYPOINT ["/app/vea-validator"]
