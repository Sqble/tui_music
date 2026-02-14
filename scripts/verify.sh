#!/usr/bin/env bash
set -euo pipefail

echo "Running format check..."
cargo fmt -- --check

echo "Running clippy (deny warnings)..."
cargo clippy --all-targets -- -D warnings

echo "Running test suite..."
cargo test

echo "All verification checks passed."
