//! Library surface for `zerostack` (Pilser fork).
//!
//! Upstream ships this crate as a binary only (`src/main.rs`). This file adds
//! a `lib` target that re-exposes the same module tree, so downstream crates
//! (e.g. zerowrapper's `zw-agent-embed`) can embed the headless
//! [`engine::Engine`] in-process via `run_string` instead of shelling out.
//! The binary is untouched: `src/main.rs` keeps its own `mod` list, allocator
//! and `fn main`.
#![deny(unsafe_code)]

pub mod agent;
pub mod auth;
pub mod cli;
pub mod config;
pub mod context;
pub mod docs;
pub mod engine;
pub mod event;
pub mod extras;
pub mod fs;
pub mod logging;
pub mod models_catalog;
pub mod permission;
pub mod pricing;
pub mod print;
pub mod provider;
pub mod retry;
pub mod sandbox;
pub mod session;
pub mod setup;
pub mod startup;
pub mod ui;
