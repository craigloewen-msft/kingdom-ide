//! What a plan is allowed to do to the world.
//!
//! Lives in the domain rather than beside the tools because it now crosses the
//! wire: the conversation view renders differently while a plan is only
//! proposing, and the sidebar's badge changes wording. It began in
//! `kingdom-app::tools`, which is still the only place it becomes an actual
//! list of tools -- `tools::all` reads this and nothing else does.
//!
//! Pure data, no I/O, so it compiles to wasm along with the rest of the domain.

use serde::{Deserialize, Serialize};

/// What a plan may do, in ascending order of authority.
///
/// The ladder exists because two different things need limiting, for two
/// different reasons.
///
/// [`Permissions::ReadOnly`] is about *collision*. Subagents share their
/// parent's worktree, and several agents writing to one checkout at once is
/// precisely what this product exists to prevent. Nothing here arbitrates, so
/// instead of detecting the collision afterwards, read-only makes it
/// unrepresentable. That is what lets subagents run in parallel with no lease
/// machinery behind them.
///
/// [`Permissions::Propose`] is about *stance*. A plan at this level may look at
/// anything and run anything, and is trusted not to change the project. It is
/// not a sandbox and does not pretend to be one -- see `Sandbox::root`, which
/// is explicit that a shell escapes the path boundary. What it withholds is
/// `patch`: offering the editing tool says *you may edit*, and withholding it
/// says *you may not*. The system prompt says the rest in words.
///
/// [`Permissions::Full`] is what the user grants, once, on a proposal they
/// accept.
///
/// `Hash` is derived for the rail's `<For>` key, which must be `Eq + Hash` and
/// has to include this: approval widens the permissions **without moving the
/// status**, so a key without it would reuse a row that has stopped being true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permissions {
    /// Reads and reports, and cannot touch the world.
    ///
    /// No writing, no commands, no browser -- and no spawning subagents of its
    /// own, which is what keeps the fan-out one level deep. A tree of agents
    /// needs an answer to "who is blocked behind whom" that Kingdom does not
    /// have yet.
    ReadOnly,
    /// May look at anything and run anything, but changes nothing and puts a
    /// plan to the user instead. What a prompt starts under.
    Propose,
    /// Everything the model has. Granted by the user, on a proposal.
    Full,
}

impl Permissions {
    /// The default for a plan whose record predates proposals.
    ///
    /// Named rather than a `Default` impl because `#[serde(default)]` on the
    /// field needs a path, and because "what an old record gets" is a
    /// deliberately different question from "what a new plan gets" -- the two
    /// answers are `Full` and `Propose`, and conflating them would silently
    /// re-open old plans as unable to work.
    pub fn full() -> Self {
        Permissions::Full
    }

    /// True when this plan has every tool.
    pub fn is_full(&self) -> bool {
        matches!(self, Permissions::Full)
    }

    /// True when the plan may act on the world but not change the project.
    pub fn can_propose(&self) -> bool {
        matches!(self, Permissions::Propose)
    }

    /// What stage of the work this level *is*, as the user reads it.
    ///
    /// The rail and the chamber both badge a live plan with this, and it is
    /// the only thing that separates the two halves of the product's stance:
    /// an agent reading the code to draw a plan up, and an agent changing files
    /// under a plan that was accepted. Both are `PlanStatus::Drafting` and
    /// always will be -- a status is where a plan is in its *life*, and a sixth
    /// variant to say one word would ripple through `ALL`, the map legend and
    /// every match on plan state. Exactly the argument [`crate::Attention`]
    /// already makes for a different question.
    ///
    /// A subagent says "Surveying" and no rail ever draws it: subagents are
    /// excluded from the rail, and reach the user only in the errand list of
    /// the chamber that sent them.
    pub fn label(&self) -> &'static str {
        match self {
            Permissions::ReadOnly => "Surveying",
            Permissions::Propose => "Exploring",
            Permissions::Full => "Working",
        }
    }

    /// CSS class suffix, e.g. `plan-exploring`.
    ///
    /// The sibling of [`crate::PlanStatus::css_suffix`], so the one place a
    /// state turns into pixels -- `style/_status.scss` -- keeps being the only
    /// one. Deliberately *not* the same suffixes: `drafting` still exists and
    /// still means the status, which the errand dots read.
    pub fn css_suffix(&self) -> &'static str {
        match self {
            Permissions::ReadOnly => "surveying",
            Permissions::Propose => "exploring",
            Permissions::Full => "working",
        }
    }
}
