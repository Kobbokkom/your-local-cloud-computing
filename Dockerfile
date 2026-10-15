FROM docker:28-cli AS docker-cli

FROM rust:1.96-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim AS base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git bash curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home --home-dir /data ylcc \
    && mkdir -p /data && chown 10001:10001 /data
ENV HOME=/data YLCC_CONTAINERIZED=1
WORKDIR /data
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/ylcc"]
CMD ["--help"]

# Release CI supplies the already tested, native architecture binary.
FROM base AS runtime
COPY --chmod=755 dist/ylcc /usr/local/bin/ylcc

# Optional sibling-container control; contains a client, never a daemon.
FROM runtime AS runtime-client
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker

# The default target also supports a normal `docker build .` from source.
FROM base AS source
COPY --from=builder /build/target/release/ylcc /usr/local/bin/ylcc
