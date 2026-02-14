#!/usr/bin/env bash
set -euo pipefail

toolchain="$(rustup show active-toolchain | awk '{print $1}')"
echo "Ensuring rustfmt and clippy are installed for ${toolchain}..."
rustup component add --toolchain "${toolchain}" rustfmt clippy >/dev/null

echo "Running format check..."
cargo fmt -- --check

echo "Running clippy (deny warnings)..."
cargo clippy --all-targets -- -D warnings

echo "Running test suite..."
cargo test

echo "All verification checks passed."
