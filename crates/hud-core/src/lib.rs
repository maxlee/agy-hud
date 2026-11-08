#![forbid(unsafe_code)]

//! `hud-core` — Pure, deterministic, I/O-free logic and rendering engine for `agy-hud`.
//!
//! Security boundary invariant (enforced by `tests/arch_guard.rs`):
//! This crate NEVER accesses `std::fs`, `std::process`, `std::net`, or `std::env`.

pub mod ansi;
pub mod commands;
pub mod config;
pub mod i18n;
pub mod probes;
pub mod render;
pub mod stdin;
pub mod transcript;
pub mod types;
