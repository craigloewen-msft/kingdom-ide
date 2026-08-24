//! Native headless-browser engine for the court's browser Deeds.
//!
//! A session is retained per plan rather than launched per call: relaunching
//! would discard the cookies, DOM and JavaScript state that make a multi-step
//! flow testable. The same boundary keeps one plan from observing another
//! plan's authenticated page, matching the isolation used by Kingdom's tmux
//! servers.
//!
//! This crate intentionally has no `Tool` dependency; the thin Kingdom-facing
//! adapters live in `kingdom-app`. Phoenix's profiling engine and screencast
//! broker are not ported: both feed Phoenix UI features Kingdom has no consumer
//! for, and carrying them here would add long-lived capture state with nobody
//! able to inspect it.
//!
//! Native-only by design. Adding this crate to a client feature would drag a
//! Chrome subprocess driver toward wasm, where process spawning cannot work.

mod session;

pub use session::{BrowserError, BrowserSessionManager, ConsoleEntry, KeyMethod, Screenshot};
