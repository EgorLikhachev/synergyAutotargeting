# Contributing

Thanks for your interest in improving synergyAutotargeting! This guide covers
the day-to-day workflow. The project follows a **Rust-first** rule: the runtime
contains no Python or C++ (see ADR-001 in
[docs/sdd/decisions.md](docs/sdd/decisions.md)).

## Development environment

Follow the [Installation](README.md#installation) steps in the README. In short:

```bash
git clone https://github.com/EgorLikhachev/synergyAutotargeting.git
cd synergyAutotargeting
cargo test --workspace        # must pass before every commit
```

Working on NPU code additionally requires the ROCK 5A board:

```bash
# on the board
cargo build --release --features npu
./target/release/synergy --diag-nets    # verify tract↔rknn parity after changes
tools/bench.sh                          # compare against the perf baseline
```

## Branching model

GitHub Flow:

- `main` is always buildable and tested.
- Create a short-lived branch per change: `feat/<topic>`, `fix/<topic>`,
  `docs/<topic>`, `refactor/<topic>`.
- Rebase onto `main` before opening a PR; keep history readable.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(commander): add lead predictor with platform feedforward
fix(rknn-sys): always attempt set_input_shapes, ignore static rejection
docs(adr): record gst H.264 pipeline quirks (ADR-015)
test(pipeline): lock lost-state detection cadence
chore(ci): add markdown link check
```

Scope is a crate name (`commander`, `rknn-sys`, `pipeline`, …) or area
(`adr`, `ci`, `bench`). Keep the subject under ~72 characters, imperative mood.

## Before submitting

```bash
cargo test --workspace          # all green
cargo clippy --workspace -- -D warnings   # zero warnings (CI enforces this)
cargo fmt --check               # please format touched files
```

If your change affects tracking/detection behavior or performance, run the
board checks above and include the numbers in the PR.

## Pull request process

1. Open a PR against `main` and fill in the
   [PR template](.github/PULL_REQUEST_TEMPLATE.md).
2. CI must pass (tests + clippy + aarch64 check).
3. A maintainer reviews; address comments by pushing additional commits.
4. Squash-merge keeps `main` linear.

Review checklist (also in the template): tests added/updated, docs updated for
user-facing changes, no new warnings, hardware numbers attached when relevant.

## Reporting issues

Use the issue templates:
[bug report](.github/ISSUE_TEMPLATE/bug_report.md) or
[feature request](.github/ISSUE_TEMPLATE/feature_request.md).
For security problems, see [SECURITY.md](SECURITY.md) — do not open public
issues for those.

## Code style

- `rustfmt` defaults; comments in code may be Russian or English.
- New external dependencies need a justification (prefer std-only where
  practical — see the `streaming` crate for an example).
- Behavioral/design changes get an entry in
  [docs/sdd/decisions.md](docs/sdd/decisions.md) (ADR journal).
