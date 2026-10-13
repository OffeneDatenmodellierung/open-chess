# syntax=docker/dockerfile:1.7
#
# The open-chess server and fair-play worker. One image, two roles:
#
#   docker build -t open-chess .
#   docker run … open-chess serve      the Lichess-compatible API (default)
#   docker run … open-chess analyse    the fair-play worker (Stockfish is in the image)
#
# Or pull the published one: ghcr.io/offenedatenmodellierung/open-chess.
#
# Nothing is compiled here: the image takes open-chess's release binaries
# and the Chess lab app (web/chess with its wasm engine) from the GitHub
# release for this checkout's version (Cargo.toml), checked against the
# release's SHA256SUMS. Build arguments:
#
#   VERSION=0.2.0      another release instead
#   BINARIES=local     dist/ in the build context instead of a release:
#                      dist/linux-amd64/open-chess (and/or linux-arm64) and
#                      dist/app/ (CI builds them, then the image)
#
# Run any number of either role: state is in Postgres, API replicas hear
# each other through LISTEN/NOTIFY, and workers claim games with SKIP
# LOCKED. `--target slim` is a smaller, shell-less image for the API and
# the app (no Stockfish, so no `analyse`).
#
# Configuration is all environment: DATABASE_URL (Postgres),
# CHESS_PUBLIC_URL, ESPORTS_PUBLIC_URL and ESPORTS_LOGIN (the host site
# and its sign-in), CHESS_FAIRPLAY_MODEL, and the CHESS_* settings in
# crates/chess-server/README.md. Ports: 8090 the API and the app (/app/),
# 9465 /metrics, /healthz and /readyz.

ARG DEBIAN=bookworm
ARG BINARIES=release

# ── fetch-release: the published binaries for this version.
FROM debian:${DEBIAN}-slim AS fetch-release
ARG TARGETARCH
ARG VERSION=
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml /src/Cargo.toml
WORKDIR /fetch
RUN set -eu; \
    v="${VERSION:-$(awk '/^\[workspace.package\]/{p=1; next} /^\[/{p=0} p && $1 == "version" {gsub(/"/, "", $3); print $3; exit}' /src/Cargo.toml)}"; \
    case "$TARGETARCH" in \
      amd64) t=x86_64-unknown-linux-gnu ;; \
      arm64) t=aarch64-unknown-linux-gnu ;; \
      *) echo "no open-chess release binary for $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    base="https://github.com/OffeneDatenmodellierung/open-chess/releases/download/v$v"; \
    server="open-chess-server-v$v-$t.tar.gz"; app="chess-app-v$v.tar.gz"; \
    curl -fsSLO "$base/$server"; curl -fsSLO "$base/$app"; curl -fsSLO "$base/SHA256SUMS"; \
    grep -E " ($server|$app)\$" SHA256SUMS | sha256sum -c -; \
    mkdir -p /out/app; \
    tar -xzf "$server" --strip-components=1 -C /out; \
    tar -xzf "$app" -C /out/app; \
    test -x /out/open-chess && test -f /out/app/index.html

# ── fetch-local: binaries built beforehand into dist/.
FROM scratch AS fetch-local
ARG TARGETARCH
COPY dist/linux-${TARGETARCH}/open-chess /out/open-chess
COPY dist/app /out/app

FROM fetch-${BINARIES} AS binaries

# ── slim (`--target slim`): the API and the app, distroless, non-root, no shell.
FROM gcr.io/distroless/cc-debian12:nonroot AS slim
LABEL org.opencontainers.image.title="open-chess" \
      org.opencontainers.image.description="open-chess: the Lichess Board, Bot, Challenge and OAuth APIs" \
      org.opencontainers.image.source="https://github.com/OffeneDatenmodellierung/open-chess" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
COPY --from=binaries --chmod=0755 /out/open-chess /usr/local/bin/open-chess
COPY --from=binaries /out/app /usr/share/open-chess/app
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
COPY --from=binaries --chmod=0755 /out/open-chess /usr/local/bin/open-chess
COPY --from=binaries /out/app /usr/share/open-chess/app
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
