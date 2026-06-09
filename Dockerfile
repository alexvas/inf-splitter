# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    CURL_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt

# Если не пустой certs/ca-bundle.crt присутствует в контексте сборки —
# установить его как доверенный CA и выполнить cargo build.
# В корпоративной среде без этого cargo не сможет скачать крейты.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=bind,source=certs/ca-bundle.crt,target=/tmp/ca-bundle.crt \
    if [ -s /tmp/ca-bundle.crt ]; then \
      cp /tmp/ca-bundle.crt /etc/ssl/certs/ca-certificates.crt; \
    fi; \
    cargo build --locked --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    CURL_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt

WORKDIR /app
COPY --from=builder /app/target/release/inf-splitter /usr/local/bin/inf-splitter
COPY LICENSE THIRD_PARTY_NOTICES ./
COPY licenses ./licenses
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q --spider http://127.0.0.1:3000/health || exit 1
ENTRYPOINT ["/usr/local/bin/inf-splitter"]
