$ErrorActionPreference = "Stop"

$toolchain = (rustup show active-toolchain).Split(" ")[0]
Write-Host "Ensuring rustfmt and clippy are installed for $toolchain..." -ForegroundColor Cyan
rustup component add --toolchain $toolchain rustfmt clippy | Out-Null

Write-Host "Running format check..." -ForegroundColor Cyan
cargo fmt -- --check

Write-Host "Running clippy (deny warnings)..." -ForegroundColor Cyan
cargo clippy --all-targets -- -D warnings

Write-Host "Running test suite..." -ForegroundColor Cyan
cargo test

Write-Host "All verification checks passed." -ForegroundColor Green
