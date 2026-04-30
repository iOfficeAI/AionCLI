# Default: list available recipes
default:
    @just --list

# Build in release mode and install to ~/.cargo/bin
# Use `just build --force` to skip cache check
build *FLAGS: lint fmt
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release
    new_sum=$(shasum -a 256 target/release/aionui-backend | cut -d' ' -f1)
    force=false
    for flag in {{FLAGS}}; do
        if [[ "$flag" == "--force" || "$flag" == "-f" ]]; then
            force=true
        fi
    done
    old_sum=""
    if [[ -f target/.build-sum ]] && [[ "$force" == "false" ]]; then
        old_sum=$(cat target/.build-sum)
    fi
    if [[ "$new_sum" == "$old_sum" ]]; then
        echo -e "\n⏭️  Binary unchanged — skipping install (sha256: ${new_sum:0:16}…)"
    else
        cp target/release/aionui-backend ~/.cargo/bin/
        codesign --force --sign - ~/.cargo/bin/aionui-backend
        echo "$new_sum" > target/.build-sum
        echo -e "\n✅ Build complete — sha256: ${new_sum:0:16}…"
    fi

# Build in debug mode
# Use `just build-debug --force` to skip cache check
build-debug *FLAGS:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build
    new_sum=$(shasum -a 256 target/debug/aionui-backend | cut -d' ' -f1)
    force=false
    for flag in {{FLAGS}}; do
        if [[ "$flag" == "--force" || "$flag" == "-f" ]]; then
            force=true
        fi
    done
    old_sum=""
    if [[ -f target/.build-debug-sum ]] && [[ "$force" == "false" ]]; then
        old_sum=$(cat target/.build-debug-sum)
    fi
    if [[ "$new_sum" == "$old_sum" ]]; then
        echo -e "\n⏭️  Debug binary unchanged (sha256: ${new_sum:0:16}…)"
    else
        echo "$new_sum" > target/.build-debug-sum
        echo -e "\n✅ Debug build complete — sha256: ${new_sum:0:16}…"
    fi

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
