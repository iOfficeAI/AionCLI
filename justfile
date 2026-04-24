# Default: list available recipes
default:
    @just --list

# Build in release mode and install to ~/.cargo/bin
build:
    cargo build --release
    cp target/release/aionui-backend ~/.cargo/bin/

# Build in debug mode
build-debug:
    cargo build

# Run all tests
test:
    cargo test --workspace

# Lint (warnings = errors)
lint:
    cargo clippy --workspace -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting (CI)
fmt-check:
    cargo fmt --all -- --check

# Lint + format check + test
check: lint fmt-check test

# Run the server (debug)
run *ARGS:
    cargo run --bin aionui-backend -- {{ARGS}}

# Run the server (release)
run-release *ARGS:
    cargo run --release --bin aionui-backend -- {{ARGS}}

# Clean build artifacts
clean:
    cargo clean

# Decode dev config and copy to clipboard
cat-config:
    @base64 -D -i ~/.aionui-config-dev/aionui-config.txt | python3 -c 'import sys, urllib.parse; print(urllib.parse.unquote(sys.stdin.read()))' | pbcopy
