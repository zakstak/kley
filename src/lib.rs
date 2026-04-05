//! Kley — minimal coding agent library.
//!
//! This library exposes the core modules so they can be used by both the
//! `kley` binary and by integration tests in `tests/`.

pub mod agent;
pub mod auth;
pub mod compact;
pub mod diagnostics;
pub mod events;
pub mod harness;
pub mod lsp;
pub mod preflight;
pub use preflight::preflight_test_support;
pub mod pricing;
pub mod provider;
pub mod runtime;
pub mod skills;
pub mod store;
pub mod test_openai;
mod text;
pub mod tools;
pub mod web;
