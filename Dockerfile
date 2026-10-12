# syntax=docker/dockerfile:1.7
#
# The open-chess server and fair-play worker. One image, two roles:
#
#   docker build -t open-chess .
#   docker run … open-chess serve      the Lichess-compatible API (default)
#   docker run … open-chess analyse    the fair-play worker (Stockfish is in the image)
#
# Run any number of either: state is in Postgres, API replicas hear each
# other through LISTEN/NOTIFY, and workers claim games with SKIP LOCKED.
# `--target slim` is a smaller, shell-less API-only image (no Stockfish).
#
# Configuration is all environment: DATABASE_URL (Postgres),
# CHESS_PUBLIC_URL, ESPORTS_PUBLIC_URL (the host site), CHESS_FAIRPLAY_MODEL, and the CHESS_* settings in
# crates/chess-server/README.md. Ports: 8090 the API, 9465 /metrics,
# /healthz and /readyz. The Chess lab training app (web/chess with its
# wasm engine) is built in and served at /app/.

ARG RUST_VERSION=1.97
ARG DEBIAN=bookworm

FROM rust:${RUST_VERSION}-slim-${DEBIAN} AS chef
# cmake and clang: aws-lc-rs, rustls' crypto provider.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang pkg-config \
 && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /build

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG PROFILE=dist
COPY --from=planner /build/recipe.json recipe.json
# Only the chess server's dependencies: none of the web app's video stack.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --profile "${PROFILE}" --locked --recipe-path recipe.json -p open-chess-server
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --profile "${PROFILE}" --locked -p open-chess-server --bin open-chess \
 && install -D -m 0755 "target/${PROFILE}/open-chess" /out/open-chess

# ── app: the Chess lab PWA, with the engine compiled to WebAssembly.
FROM chef AS app
RUN rustup target add wasm32-unknown-unknown
COPY Cargo.lock Cargo.lock
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    v=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/version = |"/,""); print; exit}' Cargo.lock) \
 && cargo install wasm-bindgen-cli --version "$v" --locked
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    scripts/build-chess-app.sh /out/app

# ── slim (`--target slim`): the API only, distroless, non-root, no shell.
FROM gcr.io/distroless/cc-debian12:nonroot AS slim
LABEL org.opencontainers.image.title="open-chess" \
      org.opencontainers.image.description="open-chess: the Lichess Board, Bot, Challenge and OAuth APIs" \
      org.opencontainers.image.source="https://github.com/OffeneDatenmodellierung/open-chess" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=builder /out/open-chess /usr/local/bin/open-chess
COPY --from=app /out/app /usr/share/open-chess/app
ENV CHESS_BIND=0.0.0.0:8090 \
    CHESS_METRICS_BIND=0.0.0.0:9465 \
    CHESS_APP_DIR=/usr/share/open-chess/app \
    RUST_LOG=info
USER nonroot
EXPOSE 8090 9465
HEALTHCHECK --interval=30s --timeout=6s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/open-chess", "healthcheck"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/open-chess"]
CMD ["serve"]

# ── runtime (default): Debian slim with Stockfish for `analyse` (a separate
#    GPL program, run as a child process, not linked).
FROM debian:${DEBIAN}-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends stockfish ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 65532 --no-create-home nonroot
LABEL org.opencontainers.image.title="open-chess" \
      org.opencontainers.image.description="open-chess server and fair-play worker" \
      org.opencontainers.image.source="https://github.com/OffeneDatenmodellierung/open-chess" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=builder /out/open-chess /usr/local/bin/open-chess
COPY --from=app /out/app /usr/share/open-chess/app
ENV CHESS_BIND=0.0.0.0:8090 \
    CHESS_METRICS_BIND=0.0.0.0:9465 \
    CHESS_APP_DIR=/usr/share/open-chess/app \
    CHESS_STOCKFISH=/usr/games/stockfish \
    RUST_LOG=info
USER nonroot
EXPOSE 8090 9465
HEALTHCHECK --interval=30s --timeout=6s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/open-chess", "healthcheck"]
# SIGTERM: stop accepting and let requests finish; open streams end and
# clients reconnect to another replica (Lichess clients do).
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/open-chess"]
CMD ["serve"]
