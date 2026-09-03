## What

<!-- One or two sentences: what does this PR change? -->

## Why

<!-- The reason / problem being solved. Link issues: Closes #123 -->

## How

- <!-- key implementation decisions -->

## Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes (no new warnings)
- [ ] `cargo fmt` applied to touched files
- [ ] Docs updated (README / ADR journal / HARDWARE_TEST_RESULTS) if user-facing or a design decision
- [ ] Hardware check attached if behavior/performance changed
      (`tools/bench.sh`, `--diag-nets`, or telemetry summary)
- [ ] No new Python/C++ in the runtime path (ADR-001, Rust-first)

## Hardware results (if applicable)

```
<!-- paste bench/diag output here -->
```
