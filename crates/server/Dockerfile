# Multi-stage build for sparrow-server (Issue 13.2).
#
# Build context MUST be the pacificnm/nest framework repo root, not this
# sparrow checkout alone — checked directly against pacificnm/sparrow's
# real, committed Cargo.toml (via `gh api`, not assumed): every nest-*
# dependency is a path dependency (`nest-ai = { path =
# "../../core/crates/nest-ai" }`, etc.), no `.cargo/config.toml` patch to a
# git dependency exists. That means this crate cannot compile from a build
# context containing only the sparrow repo — it needs `core/` and
# `modules/` two directories up, exactly per apps/README.md's documented
# convention: clone pacificnm/nest, then clone pacificnm/sparrow into
# nest/apps/sparrow/. See deploy/README.md for the exact `docker build`/
# `docker compose build` invocation this implies.
#
# No existing Nest product has a reference Dockerfile to copy from —
# checked pacificnm/loon and pacificnm/airtable-sync directly on GitHub
# (neither has one; loon's server ships a systemd unit only, no Docker at
# all), so this is a fresh multi-stage build, not a copied pattern.

FROM rust:1-slim-bookworm AS build

# pkg-config isn't strictly needed by sqlx (postgres, runtime-tokio: pure
# Rust wire protocol, no libpq) or rustls (no OpenSSL), but ca-certificates
# is needed at build time for any crate that fetches over HTTPS during
# build scripts, and is needed again in the runtime stage for the server's
# own outbound HTTPS calls to Ollama/webhooks.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
# Full nest framework repo root (this Dockerfile's build context) — copies
# core/, modules/, and apps/sparrow/ (this crate's actual home) in one
# shot, since sparrow-server's path dependencies span all three.
COPY . .

RUN cargo build --release --manifest-path apps/sparrow/Cargo.toml -p sparrow-server --bin sparrow-server

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home sparrow-server

COPY --from=build /build/apps/sparrow/target/release/sparrow-server /usr/local/bin/sparrow-server

USER sparrow-server
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/sparrow-server"]
# --config is a global flag (nest-cli convention), must precede the
# subcommand, not follow it - `run --config ...` fails with "unexpected
# argument '--config'" (found running this for real).
CMD ["--config", "/etc/sparrow/server.toml", "run"]
