default:
    just --list

# Install UI dependencies
ui-install:
    npm --prefix ui install

# Run the Tauri desktop app in dev mode (hot-reloads both Rust and UI)
# Requires: cargo install tauri-cli --locked
dev: ui-install
    cd crates/desktop && cargo tauri dev

# Same as `dev` but uses the npm-bundled Tauri CLI (no cargo install needed)
dev-npm: ui-install
    cd crates/desktop && ../../ui/node_modules/.bin/tauri dev

# Build for release
build: ui-install
    cd crates/desktop && cargo tauri build

# Run all Rust tests
test:
    cargo test --workspace

# Cargo check only (fast)
check:
    cargo check --workspace

# Format Rust code
fmt:
    cargo fmt --all

# Lint
clippy:
    cargo clippy --workspace -- -D warnings

# Vite dev server only (no Tauri, for UI-only iteration)
ui-dev:
    npm --prefix ui run dev

# Run the HTTP server in dev mode (requires a separate `just ui-build` or `just ui-dev`)
server-dev: ui-install
    cargo run --bin wilkes-server -- --data-dir ./data --dist-dir ./ui/dist --port 3000

# Build the server binary for release
server-build:
    cargo build --release --bin wilkes-server

# Build the frontend for server mode (output to ui/dist)
ui-build: ui-install
    npm --prefix ui run build

# Build the Docker image
docker-build:
    docker build -t wilkes-server .

# Run the Docker container (mounts ./data for persistence)
docker-run:
    docker run -p 3000:3000 -v "$(pwd)/data:/data" wilkes-server
