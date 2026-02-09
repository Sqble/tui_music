$ErrorActionPreference = "Stop"

Write-Host "Running format check..." -ForegroundColor Cyan
cargo fmt -- --check

Write-Host "Running clippy (deny warnings)..." -ForegroundColor Cyan
cargo clippy --all-targets -- -D warnings

Write-Host "Running test suite..." -ForegroundColor Cyan
cargo test

Write-Host "All verification checks passed." -ForegroundColor Green
