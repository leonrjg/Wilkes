# Stage 1: Build Rust server
FROM rust:1.87-bookworm AS rust-builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libclang-dev \
    clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace manifests and lock file first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/api/Cargo.toml crates/api/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/worker/Cargo.toml crates/worker/Cargo.toml
# desktop is in the workspace but not built here; provide stubs so cargo resolves deps
COPY crates/desktop/Cargo.toml crates/desktop/Cargo.toml

# Create stub sources so `cargo fetch` can resolve all deps
RUN mkdir -p crates/core/src crates/api/src crates/server/src crates/worker/src crates/desktop/src && \
    echo "fn main() {}" > crates/server/src/main.rs && \
    echo "fn main() {}" > crates/worker/src/main.rs && \
    echo "" > crates/core/src/lib.rs && \
    echo "" > crates/api/src/lib.rs && \
    echo "fn main() {}" > crates/desktop/src/main.rs && \
    touch crates/desktop/build.rs

RUN cargo fetch

# Copy actual sources
COPY crates/ crates/

RUN cargo build --release --bin wilkes-server --bin wilkes-rust-worker

# Stage 2: Build frontend
FROM node:22-bookworm AS ui-builder

WORKDIR /build/ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci

COPY ui/ ./
RUN npm run build

# Stage 3: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /app/dist /data

COPY --from=rust-builder /build/target/release/wilkes-server /app/wilkes-server
COPY --from=rust-builder /build/target/release/wilkes-rust-worker /app/wilkes-rust-worker
COPY --from=ui-builder /build/ui/dist /app/dist

VOLUME /data
EXPOSE 3000

CMD ["/app/wilkes-server", "--data-dir", "/data", "--dist-dir", "/app/dist", "--port", "3000"]
