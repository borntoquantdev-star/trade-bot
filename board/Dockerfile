# The board as one image: Rust builds both halves, the runtime stage carries
# a single binary plus the WASM bundle. Nothing else goes in.
#
#   docker build -t tradebot-board .
#   docker run -p 4340:4340 -v "$PWD/../logs:/logs" tradebot-board
#
# The stage cards and the code card also read the bot's own source, to show
# the lines it ran. Mount the whole bot folder read-only for those:
#
#   docker run -p 4340:4340 -v "$PWD/..:/bot:ro" #     -e BOARD_CODE=/bot -e BOARD_LOGS=/bot/logs -e BOARD_CONFIG=config/config.yaml #     tradebot-board
#
# Without it the board still runs; the code cards say which file they cannot
# open. The order-trip card needs the account key (GRVT_* in the environment,
# or BOARD_ENV_FILE pointing at a mounted .env) to follow an order past "sent".
#
# On Railway, PORT is set for you and no volume is mounted: the board still
# opens and shows the live market, because the market stream is public and
# runs in the browser, not here.

# ─── build ────────────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# Trunk as a prebuilt binary rather than `cargo install`: building it from
# source adds ten minutes and enough memory pressure that it took the Docker
# daemon down with it on the first attempt. The version is pinned so the
# image stays reproducible.
#
# Only trunk is installed. It downloads the wasm-bindgen CLI that matches the
# lockfile by itself; installing a second copy here is how the two versions
# drift apart and the build dies with "failed to find intrinsics".
ARG TRUNK_VERSION=0.21.14
RUN rustup target add wasm32-unknown-unknown \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
    | tar -xzf - -C /usr/local/bin trunk \
 && trunk --version

# Rust compiles with one process per core by default. On a laptop - or on a
# Windows host where the Docker VM is promised more memory than the host has
# left - eight rustc processes is enough to get the whole VM killed, which
# shows up as "error reading from server: EOF" rather than as an OOM. Two is
# slower and finishes.
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY app/Cargo.toml app/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml

# Warm the dependency cache with stub sources, so a code change does not
# recompile the whole tree. Rust builds are slow enough to make this worth it.
RUN mkdir -p app/src server/src \
 && echo "" > app/src/lib.rs \
 && echo "fn main() {}" > app/src/main.rs \
 && echo "fn main() {}" > server/src/main.rs \
 && cargo build --release -p board-server \
 && rm -rf app/src server/src

COPY . .
# Touch so cargo does not trust the stub fingerprints above.
RUN find . -name "*.rs" -exec touch {} +

RUN cargo build --release -p board-server
RUN cd app && trunk build --release

# ─── runtime ──────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 board

WORKDIR /app
COPY --from=build /src/target/release/board-server /usr/local/bin/board-server
COPY --from=build /src/app/dist /app/dist

ENV BOARD_DIST=/app/dist \
    BOARD_LOGS=/logs \
    PORT=4340
EXPOSE 4340

# Read-only by construction: the board never needs to write anything.
USER board
CMD ["board-server"]
