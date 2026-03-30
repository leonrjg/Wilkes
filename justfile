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
