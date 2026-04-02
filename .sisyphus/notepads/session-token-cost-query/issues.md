# Issues

- Tests for Task 1 currently emit a `dead_code` warning from
  `src/pricing/models_dev.rs:66` (`RawProvider.id` is never read). The file
  existed before this wave, but the warning surfaces whenever `cargo test` runs.
