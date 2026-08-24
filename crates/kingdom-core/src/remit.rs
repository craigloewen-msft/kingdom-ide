//! How much of the world a plan is allowed to touch.
//!
//! Lives in the domain rather than beside the tools because it now crosses the
//! wire: the chamber renders differently under counsel, and the rail's badge
//! changes wording. It began in `kingdom-app::tools`, which is still the only
//! place it becomes an actual list of tools -- `tools::all` reads this and
//! nothing else does.
//!
//! Pure data, no I/O, so it compiles to wasm along with the rest of the domain.

use serde::{Deserialize, Serialize};

/// What a plan may do, in ascending order of authority.
///
/// The ladder exists because two different things need limiting, for two
/// different reasons.
///
/// [`Remit::Survey`] is about *collision*. Errands share their parent's
/// worktree, and several agents writing to one checkout at once is precisely
/// what this product exists to prevent. Nothing here arbitrates, so instead of
/// detecting the collision afterwards, a survey makes it unrepresentable. That
/// is what lets errands run in parallel with no lease machinery behind them.
///
/// [`Remit::Counsel`] is about *stance*. A counselling plan may look at
/// anything and run anything, and is trusted not to change the project. It is
/// not a sandbox and does not pretend to be one -- see `Workshop::root`, which
/// is explicit that a shell escapes the path boundary. What it withholds is
/// `patch`: offering the editing tool says *you may edit*, and withholding it
/// says *you may not*. The charter says the rest in words.
///
/// [`Remit::Full`] is what the King grants, once, on a proposal he accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Remit {
    /// Reads and reports, and cannot touch the world.
    ///
    /// No writing, no commands, no browser -- and no sending errands of its
    /// own, which is what keeps the fan-out one level deep. A tree of agents
    /// needs an answer to "who is blocked behind whom" that Kingdom does not
    /// have yet.
    Survey,
    /// May look at anything and run anything, but changes nothing and proposes
    /// instead. What a decree starts under.
    Counsel,
    /// Everything the court has. Granted by the King, on a proposal.
    Full,
}

impl Remit {
    /// The default for a plan whose record predates counsel.
    ///
    /// Named rather than a `Default` impl because `#[serde(default)]` on the
    /// field needs a path, and because "what an old record gets" is a
    /// deliberately different question from "what a new plan gets" -- the two
    /// answers are `Full` and `Counsel`, and conflating them would silently
    /// re-open old plans as unable to work.
    pub fn full() -> Self {
        Remit::Full
    }

    /// True when this plan has the court's full hands.
    pub fn is_full(&self) -> bool {
        matches!(self, Remit::Full)
    }

    /// True when the plan may act on the world but not change the project.
    pub fn is_counsel(&self) -> bool {
        matches!(self, Remit::Counsel)
    }

    /// How the chamber names this remit to the King.
    pub fn label(&self) -> &'static str {
        match self {
            Remit::Survey => "Surveying",
            Remit::Counsel => "Drawing up a plan",
            Remit::Full => "Working",
        }
    }
}
