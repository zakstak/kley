- Added a small `WebResourceUsageService` beside the existing auth service in
  `src/web/state.rs` so host metrics stay local to the web stack and can be
  mocked deterministically in integration tests.
- Used Linux-native `/proc` reads for RAM/CPU and `statvfs` for disk usage,
  avoiding a heavier subsystem while still delivering live snapshot data for the
  web UI.
