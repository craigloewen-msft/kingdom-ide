//! Pull requests the King can have reviewed, and the review that comes back.
//!
//! Shown to the King as the **pull requests** screen and the court's
//! **findings**; named here for what they are.
//!
//! Pure data, like the rest of this crate. Listing a repository's pull requests
//! and fetching one into a worktree are `kingdom_app::github` and
//! `kingdom_app::worktree`, which are server-only; this crate only describes
//! the answers, so the browser and the server agree on their shape by
//! construction.
//!
//! # Why a review is not a proposal
//!
//! A proposal is a question -- *may I do this?* -- and approving it widens what
//! the plan may touch. A review asks nothing of the sort. Its deliverable is the
//! report itself, it must never lead to the plan changing the code it was asked
//! to judge, and nothing it says is ever posted anywhere. Modelling it as a
//! proposal would put a "Start with this" button under a code review, which is
//! a button that grants authority over somebody else's pull request. So it has
//! its own type, its own tool (`submit_review`) and its own remit
//! ([`crate::Permissions::Review`]), and no door from any of them leads to
//! [`crate::Permissions::Full`].

use crate::model::Timestamp;
use serde::{Deserialize, Serialize};

/// One open pull request, as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    /// The author's login.
    pub author: String,
    /// The branch the change is on, as GitHub names it.
    pub head: String,
    /// The branch it asks to be merged into.
    pub base: String,
    /// The commit at the tip of the pull request when it was read.
    pub head_sha: String,
    pub url: String,
    pub draft: bool,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// When it last moved, as GitHub wrote it (RFC 3339). Kept as text because
    /// this crate carries no calendar; the browser formats it.
    pub updated_at: String,
    /// Where GitHub's own review process stands, when it says.
    pub decision: Option<ReviewDecision>,
    /// True when the change comes from a fork. It still checks out the same
    /// way -- `pull/N/head` lives on the base repository -- but the King may
    /// want to know whose code he is about to run.
    pub from_fork: bool,
    /// The description, verbatim. Empty in a listing, which does not ask for
    /// it; filled when a single pull request is read to be reviewed.
    #[serde(default)]
    pub body: String,
}

/// GitHub's `reviewDecision`, for the list's badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl ReviewDecision {
    /// Reads GitHub's spelling. Anything else -- including the empty string
    /// GitHub sends for a repository with no review rules -- is no decision.
    pub fn from_github(value: &str) -> Option<Self> {
        match value {
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUESTED" => Some(Self::ChangesRequested),
            "REVIEW_REQUIRED" => Some(Self::ReviewRequired),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Approved => "Approved",
            Self::ChangesRequested => "Changes requested",
            Self::ReviewRequired => "Review required",
        }
    }

    pub fn css_suffix(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes",
            Self::ReviewRequired => "required",
        }
    }
}

/// What asking for a project's pull requests produced.
///
/// Not a `Result`. `gh` missing, `gh` not logged in, and a project with no
/// GitHub remote are all ordinary states of a machine, and each is an answer
/// the screen should *say* -- with what to do about it -- rather than an error
/// attached to nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullRequestList {
    Listed {
        /// `owner/name`, as GitHub knows the repository.
        repository: String,
        pulls: Vec<PullRequest>,
    },
    Unavailable(String),
}

/// The pull request a review plan's workspace was checked out from.
///
/// Carried on [`crate::WorkspaceMode::PullRequest`], so it is settled when the
/// plan opens and never changes afterwards -- a review is of one commit, and a
/// push to the pull request after that is a different thing to review.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The branch it asks to be merged into, which the changes drawer diffs
    /// against.
    pub base: String,
    /// The branch the change is on, for display only. Nothing is ever pushed
    /// to it.
    pub head: String,
    /// The commit that was checked out. What the review is *of*.
    pub head_sha: String,
    /// The git remote the pull request was fetched from, e.g. `origin`.
    pub remote: String,
}

impl PullRequestRef {
    /// Taken from a pull request as it was read, against the remote it was
    /// found on.
    pub fn of(pull: &PullRequest, remote: impl Into<String>) -> Self {
        Self {
            number: pull.number,
            title: pull.title.clone(),
            url: pull.url.clone(),
            base: pull.base.clone(),
            head: pull.head.clone(),
            head_sha: pull.head_sha.clone(),
            remote: remote.into(),
        }
    }

    /// The commit, abbreviated the way git does.
    pub fn short_sha(&self) -> &str {
        match self.head_sha.char_indices().nth(7) {
            Some((i, _)) => &self.head_sha[..i],
            None => &self.head_sha,
        }
    }
}

/// The court's bottom line on a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recommendation {
    /// Works as described and the code is sound enough to merge.
    Merge,
    /// Should not merge as it stands; the report says what to change.
    Changes,
    /// Not a code problem the court can settle -- a design question, a missing
    /// requirement, something it could not test. The King's call.
    Discuss,
}

impl Recommendation {
    /// Every value, in the order a model is offered them.
    pub const ALL: [Recommendation; 3] = [
        Recommendation::Merge,
        Recommendation::Changes,
        Recommendation::Discuss,
    ];

    /// The word the model sends, and the word stored. Lowercase and stable,
    /// because it is part of a tool's schema.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Changes => "changes",
            Self::Discuss => "discuss",
        }
    }

    pub fn from_key(value: &str) -> Option<Self> {
        let value = value.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|r| r.key() == value)
    }

    /// What the King reads on the card.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Merge => "Ready to merge",
            Self::Changes => "Needs changes",
            Self::Discuss => "Needs discussion",
        }
    }

    pub fn css_suffix(&self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Changes => "changes",
            Self::Discuss => "discuss",
        }
    }
}

/// A review the court has handed back.
///
/// Replaced whole when the court revises it in answer to a follow-up: the
/// previous text is in the transcript, on the `submit_review` call that carried
/// it, so nothing is lost by keeping only the latest here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestReview {
    /// The report's own headline.
    pub title: String,
    /// The report, as markdown.
    pub body: String,
    pub recommendation: Recommendation,
    /// When it was handed back.
    #[serde(default)]
    pub at: Option<Timestamp>,
    /// How many times it has been handed back, counting this one. A card that
    /// says "revised" tells the King his follow-up was actually answered.
    #[serde(default = "first")]
    pub revision: u32,
}

fn first() -> u32 {
    1
}

/// The words that open a review plan: what the King asked for, and what the
/// pull request says about itself.
///
/// This is the King's first message in the plan's transcript, so it is also
/// what the rail's age line and the model's first turn are about. It carries
/// the description because the model is deliberately cut off from GitHub (see
/// `kingdom_app::tools::child_environment`), so this is the only place it can
/// read the author's own account of the change.
///
/// The how-to -- run it, open it in a browser, read the diff, write the report
/// -- is not here. That belongs to the remit, which the model is told in the
/// system prompt on every turn; this is only *what* to review.
pub fn review_prompt(pull: &PullRequest, notes: &str) -> String {
    let mut out = format!(
        "Review pull request #{} \u{2014} \"{}\" by @{}.\n\n\
         It asks to merge `{}` into `{}`: {} file(s), +{} \u{2212}{}. It is checked out \
         here at {}.\n",
        pull.number,
        pull.title.trim(),
        pull.author,
        pull.head,
        pull.base,
        pull.changed_files,
        pull.additions,
        pull.deletions,
        pull.head_sha.chars().take(12).collect::<String>(),
    );
    if pull.draft {
        out.push_str("\nIt is marked as a draft, so the author may not consider it finished.\n");
    }

    let notes = notes.trim();
    if notes.is_empty() {
        out.push_str(
            "\nI have nothing particular to add: test what it claims to do, and review the \
             code.\n",
        );
    } else {
        out.push_str("\nWhat I want you to look at:\n\n");
        out.push_str(notes);
        out.push('\n');
    }

    out.push_str("\n---\n\nThe pull request's description, as its author wrote it:\n\n");
    let body = pull.body.trim();
    if body.is_empty() {
        out.push_str("(The author left no description.)");
    } else {
        // Quoted, so a heading in somebody else's description cannot pass for
        // structure in the King's own message.
        for line in body.lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The title a review plan is opened with.
///
/// Fixed for the plan's life: unlike a proposal, a review does not rename the
/// plan, because "Review #42" is what the King will look for in the rail.
pub fn review_title(pull: &PullRequest) -> String {
    format!("Review #{}: {}", pull.number, pull.title.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull() -> PullRequest {
        PullRequest {
            number: 42,
            title: "Fix the login form".into(),
            author: "ada".into(),
            head: "fix-login".into(),
            base: "main".into(),
            head_sha: "0123456789abcdef0123".into(),
            url: "https://github.com/o/r/pull/42".into(),
            draft: false,
            additions: 10,
            deletions: 2,
            changed_files: 3,
            updated_at: "2025-01-01T00:00:00Z".into(),
            decision: None,
            from_fork: false,
            body: "## Why\nThe button did nothing.".into(),
        }
    }

    /// The prompt carries everything the model cannot fetch for itself: the
    /// branches, the King's own notes and the author's description.
    #[test]
    fn the_opening_message_carries_the_notes_and_the_description() {
        let text = review_prompt(&pull(), "  Check it on a phone-sized window.  ");
        assert!(text.contains("#42"));
        assert!(text.contains("`fix-login` into `main`"));
        assert!(text.contains("Check it on a phone-sized window."));
        assert!(text.contains("0123456789ab"), "the commit is named: {text}");
        // Quoted, so the author's heading is not the King's.
        assert!(text.contains("> ## Why"));
        assert!(text.contains("> The button did nothing."));
    }

    #[test]
    fn no_notes_and_no_description_are_both_said_plainly() {
        let mut bare = pull();
        bare.body = String::new();
        bare.draft = true;
        let text = review_prompt(&bare, "");
        assert!(text.contains("nothing particular to add"));
        assert!(text.contains("no description"));
        assert!(text.contains("draft"));
    }

    #[test]
    fn a_recommendation_round_trips_through_its_key() {
        for r in Recommendation::ALL {
            assert_eq!(Recommendation::from_key(r.key()), Some(r));
        }
        assert_eq!(
            Recommendation::from_key(" Merge "),
            Some(Recommendation::Merge)
        );
        assert_eq!(Recommendation::from_key("ship it"), None);
    }

    #[test]
    fn githubs_decisions_are_read_and_its_silence_is_none() {
        assert_eq!(
            ReviewDecision::from_github("CHANGES_REQUESTED"),
            Some(ReviewDecision::ChangesRequested)
        );
        assert_eq!(ReviewDecision::from_github(""), None);
    }

    #[test]
    fn the_reference_abbreviates_its_commit() {
        let r = PullRequestRef::of(&pull(), "origin");
        assert_eq!(r.short_sha(), "0123456");
        assert_eq!(r.remote, "origin");
    }
}
