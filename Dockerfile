# syntax=docker/dockerfile:1
# The okf-mcp server image (M7). Built by .github/workflows/image.yml and
# published to ghcr.io/hypotheosis/okf-mcp; see docs/operations.md.

# Build stage: the same Rust as the workstation's toolchain (1.99.1). Bump both
# together.
# Base images go by tag, not digest, on purpose: rebuilds get Debian's security
# fixes. A pushed release keeps its digest; never rebuild an old tag.
FROM rust:1.99.1-slim-bookworm AS build
WORKDIR /build
COPY . .
# Cache mounts keep the crate registry and target/ between builds on one
# builder. target/ is a cache, not a layer, so the binary is copied out of it.
# They only help local or long-lived builders (CI runners start cold); a
# cargo-chef dependency layer is the fix if CI build time ever matters.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin okf-mcp \
    && cp target/release/okf-mcp /usr/local/bin/okf-mcp

# Runtime stage: no git, and the binary links no OpenSSL (sqlx uses rustls).
# CA certificates for completeness; that package pulls in Debian's openssl
# tools, which the server never runs. A fixed, non-root uid that manifests
# can name.
FROM debian:bookworm-slim
# The official MCP Registry checks this label against server.json's name.
LABEL io.modelcontextprotocol.server.name="io.github.hypotheosis/okf-mcp"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin okf
COPY --from=build /usr/local/bin/okf-mcp /usr/local/bin/okf-mcp
USER 10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/okf-mcp"]
CMD ["serve"]
