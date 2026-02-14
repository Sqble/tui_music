# Agent Quality Contract (TuneTUI)

This repository optimizes for three goals: performance, rigorous correctness, and maximum practical audio quality.

## Priority Order

1. Correctness and safety
2. Performance and memory efficiency
3. Audio quality and playback reliability
4. UX responsiveness in terminal environments

## Non-Negotiable Standards

- Keep hot paths allocation-aware and avoid per-frame/per-event unnecessary work.
- Keep strict lint quality: `cargo clippy --all-targets -- -D warnings`.
- Keep full test suite green: `cargo test`.
- Do not reduce audio format support or playback quality behavior without explicit rationale.
- Do not add heavy dependencies without clear benefit and cost notes.

## Update-All-Affected Rule

Any behavior change must include all impacted updates in the same change:

- Code path updates
- Unit/integration/property tests
- Command/help text and docs (`README.md`)
- CI/scripts as needed

No partial updates that leave tests/docs/commands stale.

## Definition Of Done

All items below must be true before considering work complete:

- [ ] `cargo fmt -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] Affected tests were added or updated
- [ ] User-visible behavior changes are documented

## Preferred Workflow

Use `scripts/verify.ps1` for local verification on Windows and `scripts/verify.sh` on Linux/macOS:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/verify.ps1
```

```bash
bash scripts/verify.sh
```
