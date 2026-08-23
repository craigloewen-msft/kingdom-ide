//! # kingdom-core
//!
//! The domain model for Kingdom IDE. This crate is deliberately free of I/O and
//! of any dependency on Leptos, Axum or the filesystem, because it compiles to
//! **both** the native server and the `wasm32` browser bundle. Every type the UI
//! renders and the server produces is defined exactly once, right here.
//!
//! ## The metaphor, as types
//!
//! | Metaphor            | Type        | Meaning                                    |
//! |---------------------|-------------|--------------------------------------------|
//! | Kingdom             | [`Kingdom`] | the dev folder you opened                  |
//! | City                | [`City`]    | one project directory inside it            |
//! | Architect           | [`Architect`] | an agent, always scoped to a city        |
//! | Architectural Plan  | [`Plan`]    | a proposal awaiting the King's review      |
//! | Decree              | [`Task`]    | work the King starts from the chat dock    |
//! | Crown Resource      | [`Resource`] / [`Lease`] | contended machine resources   |
//!
//! See `AGENTS.md` at the repository root for the philosophy behind this.

pub mod ids;
pub mod layout;
pub mod model;
pub mod sample;

pub use ids::*;
pub use model::*;
