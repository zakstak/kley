#![allow(dead_code)]
//! Kley — minimal coding agent library.
//!
//! This library exposes the core modules so they can be used by both the
//! `kley` binary and by integration tests in `tests/`.

pub mod agent;
pub mod auth;
pub mod compact;
pub mod events;
pub mod preflight;
pub mod runtime;
pub mod skills;
pub mod store;
pub mod tools;
pub mod web;
