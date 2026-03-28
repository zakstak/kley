# Kley Development Rules

- Run `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build`
  before committing.
- If SQL changed, explain the query and index impact.
- Store layer: use typed `DateTime<Utc>` timestamps, `Turn::from_row`,
  `SharedStore` (`Arc<Mutex<Store>>`) for async access.
- Tool trait: domain errors (file not found, bad input) → `Ok(error_message)`.
  Implementation bugs → `Err`.
- Tools are sync. The agent loop can use `spawn_blocking` if needed.
- Skill authoring: routing-friendly descriptions, one skill per decision
  boundary, sub-files for language-specific content.
- Keep always-on rules compact. Load skill content on demand, never auto-inject
  full bodies.
- No `unwrap()` in library code. Use `anyhow::Result` or `?`.
- Prefer `eprintln!` for agent-visible output. `println!` is reserved for model
  response text.
- Product surfaces: terminal TUI output is a debugging-first surface; extra
  diagnostic context is expected and encouraged. The web UI is the
  polished/final user-facing form.
- Platform policy: Kley currently supports Linux only. Windows and other
  non-Linux compatibility work is out of scope unless repo policy changes; do
  not add fallback branches for unsupported platforms.
