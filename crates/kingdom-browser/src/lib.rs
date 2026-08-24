//! Native headless-browser engine for the court's browser Deeds.
//!
//! A session is retained per plan rather than launched per call: relaunching
//! would discard the cookies, DOM and JavaScript state that make a multi-step
//! flow testable. The same boundary keeps one plan from observing another
//! plan's authenticated page, matching the isolation used by Kingdom's tmux
//! servers.
//!
//! This crate intentionally has no `Tool` dependency; the thin Kingdom-facing
//! adapters live in `kingdom-app`.
//!
//! The screencast broker *is* here, unlike when this crate was first written.
//! The note that stood in its place argued it served a Phoenix UI feature
//! Kingdom had no consumer for, which was true at the time and is no longer:
//! the King now has a spyglass onto his court's browser, and [`screencast`] is
//! what feeds it. The profiling engine ([`profile`], with its in-page half in
//! [`perf`]) arrived with it, for a related reason -- it was left out as "a
//! Phoenix UI feature", but it answers to a model rather than to any UI, so
//! that reasoning never really applied to it.
//!
//! Native-only by design. Adding this crate to a client feature would drag a
//! Chrome subprocess driver toward wasm, where process spawning cannot work.

mod perf;
pub mod profile;
mod screencast;
mod session;

pub use profile::{PerfReading, ProfilingState};
pub use screencast::{ScreencastBroker, ScreencastEvent};
pub use session::{BrowserError, BrowserSessionManager, ConsoleEntry, KeyMethod, Screenshot};
