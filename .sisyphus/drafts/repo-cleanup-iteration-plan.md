# Draft: Repo Cleanup Iteration Plan

## Requirements (confirmed)
- repo status: greenfield repo, broad changes allowed
- goal: enable rapid iteration on a learning project
- decision needed: whether to clean up before moving to next features
- cleanup depth: broader refactor before new features
- testing policy: tests-after for upcoming work

## Technical Decisions
- planning mode: assess cleanup-first vs feature-first based on actual repo state
- likely direction: recommend a short cleanup pass focused on iteration speed, not broad refactor
- final direction: cleanup-first, but optimize for rapid iteration outcomes rather than abstract purity
- refactor guardrail: preserve behavior and avoid adding net-new product features during cleanup

## Research Findings
- initial observation: Rust build artifacts exist under `target/`
- root structure: `Cargo.toml`, `Cargo.lock`, `src/`, `target/`
- source layout: `src/main.rs`, `src/agent.rs`, `src/events.rs`, `src/auth/`, `src/store/`
- main entrypoint: `src/main.rs` exposes `login` and `chat` CLI commands
- test baseline exists: inline unit and async tests in `src/store/mod.rs`, `src/auth/mod.rs`, `src/auth/openai.rs`
- repo hygiene gap: no `.gitignore` found while `git status --short` shows `target/` and all source files untracked
- iteration hotspot: `src/agent.rs` is a large multi-responsibility file handling chat loop and provider transports

## Open Questions
- none blocking for initial plan generation

## Scope Boundaries
- INCLUDE: repo assessment, cleanup recommendation, and execution plan direction
- INCLUDE: broader refactor that improves iteration speed, structure, and workflow
- EXCLUDE: net-new end-user features during cleanup pass
- EXCLUDE: implementing cleanup or feature work in this session
