# syntax=docker/dockerfile:1
#
# The saneha server as a container image: a Rust builder stage produces the
# one binary, and the runtime stage carries nothing but that binary and the
# few libraries it links.
#
# It runs `saneha serve` on 0.0.0.0:7343 with the database on a volume at
# /data/saneha.db. Both are set through SANEHA_BIND and SANEHA_DB, so the
# Quadlet unit can override either without changing the entrypoint.

FROM rust:1.98-slim-trixie AS builder

# rusqlite's bundled SQLite is C, so the build needs a compiler and libc
# headers. ureq is on rustls with the ring provider, which needs neither cmake
# nor the aws-lc-rs toolchain, so nothing else is installed here.
RUN apt-get update \
 && apt-get install --yes --no-install-recommends gcc libc6-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Dependencies are built from the manifests alone, against a stand-in crate, so
# editing src/ reuses this layer instead of rebuilding the whole tree. The
# stand-in's own artifacts are then dropped: cargo decides by mtime, and COPY
# keeps the source mtimes from the build context, which can be older than the
# stand-in's output.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
 && echo 'fn main() {}' > src/main.rs \
 && touch src/lib.rs \
 && cargo build --release --locked \
 && rm -rf src \
      target/release/saneha \
      target/release/deps/saneha* \
      target/release/libsaneha* \
      target/release/.fingerprint/saneha-*

COPY src ./src
RUN cargo build --release --locked \
 && strip target/release/saneha

FROM debian:trixie-slim AS runtime

# ca-certificates for the client subcommands' outbound TLS; curl so the Quadlet
# unit can health-check the server from inside the container.
RUN apt-get update \
 && apt-get install --yes --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/saneha /usr/local/bin/saneha

# The database lives on a volume mounted at /data. saneha creates the file, and
# any missing parent directories, on first start.
RUN mkdir -p /data && chown 10001:10001 /data
USER 10001:10001

ENV SANEHA_BIND=0.0.0.0:7343 \
    SANEHA_DB=/data/saneha.db

EXPOSE 7343

ENTRYPOINT ["/usr/local/bin/saneha"]
CMD ["serve"]
