# Base: shared system deps + Rust toolchain
FROM ubuntu:24.04 AS rust-base

RUN apt-get update && apt-get install -y \
    curl \
    build-essential \
    pkg-config \
    libclang-dev \
    clang \
    libssl-dev \
    libfontconfig-dev \
    libglib2.0-dev \
    unzip \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.88
ENV PATH="/root/.cargo/bin:${PATH}"

RUN cargo install cargo-chef sccache --locked
ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_DIR=/sccache

WORKDIR /build

# Stage 1: Compute dependency recipe (manifests only — source changes must not invalidate this)
FROM rust-base AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/api/Cargo.toml crates/api/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/worker/Cargo.toml crates/worker/Cargo.toml
COPY crates/desktop/Cargo.toml crates/desktop/Cargo.toml
RUN mkdir -p crates/core/src crates/api/src crates/server/src crates/worker/src crates/desktop/src && \
    touch crates/core/src/lib.rs crates/api/src/lib.rs crates/desktop/src/lib.rs && \
    echo "fn main() {}" > crates/server/src/main.rs && \
    echo "fn main() {}" > crates/worker/src/main.rs && \
    echo "fn main() {}" > crates/desktop/src/main.rs && \
    touch crates/desktop/build.rs
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2: Cache dependencies (compiled into image layer, not a cache mount)
FROM rust-base AS cacher
COPY --from=planner /build/recipe.json recipe.json
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/root/.cache/dfbin \
    --mount=type=cache,target=/sccache \
    cargo chef cook --release --bin wilkes-server --bin wilkes-rust-worker --recipe-path recipe.json

# Stage 3: Build binaries
FROM rust-base AS rust-builder
COPY --from=cacher /build/target target
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/root/.cache/dfbin \
    --mount=type=cache,target=/sccache \
    cargo build --release --bin wilkes-server --bin wilkes-rust-worker && \
    cp target/release/wilkes-server /wilkes-server && \
    cp target/release/wilkes-rust-worker /wilkes-rust-worker

# Stage 4: Build frontend
FROM node:22-bookworm AS ui-builder

WORKDIR /build/ui
COPY ui/package.json ui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci

COPY ui/ ./
RUN npm run build

# Stage 5: Runtime
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libstdc++6 \
    libssl3 \
    libfontconfig1 \
    libfreetype6 \
    python3 \
    python3-venv \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /app/dist /data

COPY --from=rust-builder /wilkes-server /app/wilkes-server
COPY --from=rust-builder /wilkes-rust-worker /app/wilkes-rust-worker
COPY --from=ui-builder /build/ui/dist /app/dist
COPY crates/worker/wilkes_python_worker /app/worker/wilkes_python_worker
COPY crates/worker/requirements.txt /app/worker/requirements.txt

ENV RUST_LOG=info,hf_hub=warn

VOLUME /data
EXPOSE 3000

CMD ["/app/wilkes-server", "--data-dir", "/data", "--dist-dir", "/app/dist", "--port", "3000"]
