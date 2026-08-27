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
//! | Architectural Plan  | [`Plan`]    | a proposal, drafted by a model, awaiting review |
//!
//! A plan is deliberately both the unit of work and the unit of review. There
//! is no separate agent entity: the user reviews proposals, not agents, and
//! which model is drafting is an attribute of the plan.
//!
//! See `AGENTS.md` at the repository root for the philosophy behind this.

pub mod ids;
pub mod mockdata;
pub mod model;
pub mod naming;
pub mod palette;
pub mod permissions;
pub mod proposal;
pub mod review;
pub mod sample;
pub mod services;

pub use ids::*;
pub use model::*;
pub use palette::{assign_banners, AgentPalette, BANNERS};
pub use permissions::*;
pub use review::*;
pub use services::{ManifestError, ServiceManifest, ServiceSpec};
