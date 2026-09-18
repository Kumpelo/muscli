# Contributing

Thank you for improving muscli.

1. Open an issue for large behavior or schema changes.
2. Create a focused branch from `main`.
3. Keep Linux and Windows behavior aligned unless the feature is explicitly
   platform-specific.
4. Run formatting, Clippy, tests, and a release build before opening a PR.
5. Do not commit copyrighted music or album artwork. Generate test tones and
   original placeholder art for fixtures.

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

PRs should explain user-visible behavior, migration impact, and how the change
was verified on each affected platform.
