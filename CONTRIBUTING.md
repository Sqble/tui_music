# Contributing

Thanks for contributing to TuneTUI.

## Quality Gates (Required)

Run all checks before opening a PR:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify.ps1
```

Equivalent manual commands:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Update-All-Affected Rule

If you change behavior, update all affected areas in the same PR:

- Implementation code
- Unit/integration/property tests
- Command/help text and docs (`README.md`)
- CI/scripts/config updates when needed

## Performance and Audio Guidelines

- Keep hot paths allocation-aware.
- Avoid adding heavy dependencies without clear justification.
- Do not reduce audio quality or format support without rationale and tests.

## Pull Request Checklist

- [ ] `cargo fmt -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] Added/updated impacted tests
- [ ] Updated docs/help text for any user-visible behavior changes
