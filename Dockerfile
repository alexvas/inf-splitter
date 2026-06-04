FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/inf-splitter /usr/local/bin/inf-splitter
COPY LICENSE THIRD_PARTY_NOTICES ./
COPY licenses ./licenses
EXPOSE 3383
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q --spider http://127.0.0.1:3383/health || exit 1
ENTRYPOINT ["/usr/local/bin/inf-splitter"]
