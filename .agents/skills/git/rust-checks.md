# Rust Pre-Commit Checks

Run before every commit in a Rust project (detected by `Cargo.toml` in project root):

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build
```

Only commit if all four pass clean.

If `clippy` warns, fix the warning. Do not `#[allow]` without written justification.
