---
description: Run every gate (fmt, clippy incl. corvus_link tls, tests, i18n, manual) and report each result as it is.
agent: review
subtask: true
---

Run these gates from the repository root, one after the other, and report each result with its count. A failure is reported verbatim with the first error; a skipped gate is reported as skipped. Fix nothing. $ARGUMENTS

```
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p corvus_link --all-targets --features tls --locked -- -D warnings
cargo test --workspace --locked
cargo run -q -p corvus_tools --bin corvus-build -- i18n check
cargo run -q -p corvus_tools --bin corvus-build -- guide --check
```

Finish with the handoff block from `AGENTS.md`.
