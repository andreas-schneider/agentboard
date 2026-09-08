# Contributing

Contributions are welcome, but this project has no support or response-time
commitment. An issue or pull request may receive no response.

## Local checks

Run these before opening a pull request:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Keep changes focused, include tests when behavior changes, and update the
README when a user-facing command, configuration value, or workflow changes.

## Pull requests

Explain what changed and how you tested it. Do not include credentials,
personal data, or generated local state such as `.agentboard` data.

Before sumbitting your proposal, run your coding agent with the [review instructions](REVIEW-INSTRUCTIONS.md) and fix the issues worth fixing.
