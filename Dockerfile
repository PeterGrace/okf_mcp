# syntax=docker/dockerfile:1
# The okf-mcp server image (M7). Built by .github/workflows/image.yml and
# published to ghcr.io/petergrace/okf-mcp; see docs/operations.md.

# Build stage: the same Rust as the workstation's toolchain (1.99.1). Bump both
# together.
FROM rust:1.99.1-slim-bookworm AS build
WORKDIR /build
COPY . .
# Cache mounts keep the crate registry and target/ between builds on one
# builder. target/ is a cache, not a layer, so the binary is copied out of it.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin okf-mcp \
    && cp target/release/okf-mcp /usr/local/bin/okf-mcp

# Runtime stage: no git and no OpenSSL (sqlx uses rustls); CA certificates
# for completeness. A fixed, non-root uid that manifests can name.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin okf
COPY --from=build /usr/local/bin/okf-mcp /usr/local/bin/okf-mcp
USER 10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/okf-mcp"]
CMD ["serve"]
