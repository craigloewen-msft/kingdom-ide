//! Putting a plan to the user.
//!
//! The gateway from proposing to working, and the only tool that does not act
//! on the world at all: it ends the turn. The model says what it would do, and
//! stops.
//!
//! # Why this is an ordinary tool
//!
//! Phoenix's equivalent (`propose_task`) is intercepted in its state machine
//! before it ever reaches the executor, leaving `run()` as a bug fallback.
//! Kingdom has no state machine to intercept in -- the turn loop is a `for` loop
//! in `api::converse` -- so this runs like any other tool and signals the ending
//! through its *outcome*. `converse` checks for it by name after settling the
//! deed, which keeps the recording, the pushing and the persistence on exactly
//! the path every other tool already takes.
//!
//! # Why the proposal travels as a path
//!
//! It used to travel inline, as a `title` and a `body` argument, and the
//! reasoning written here was that a plan is already a document so Kingdom
//! needed none of Phoenix's file machinery. That was wrong, and a real plan
//! showed why: it spent 21 rounds and 33 tool calls investigating and never
//! proposed at all. Its own reasoning had settled the design by round 11 and
//! then re-derived it eight times over, renaming the same component on each
//! pass.
//!
//! The cause was that **nothing it decided was ever written down**. An inline
//! proposal has to be produced whole, from memory, in a single call -- so every
//! round the model faces the same choice between emitting everything at once and
//! looking a little further, and looking always wins. Phoenix does not have this
//! problem because its agent drafts a task *file* first: the plan leaves its
//! head incrementally, and revising is a patch rather than a re-emission.
//!
//! So Kingdom does the same. [`DRAFT`] is a file inside the plan's own
//! workspace, `patch` is scoped to it while proposing
//! ([`super::patch::Patch::for_draft`]), and writing it returns a `<next_step>`
//! cue pointing back here -- Phoenix's mechanism, part for part.
//!
//! Nothing is lost by reading the body off disk at this moment instead of taking
//! it from the arguments: it still lands on the tool call, so it is still in the
//! transcript, on disk, pushed to the conversation view, and in the model's own
//! context next round. The user's project stays free of files Kingdom invented,
//! because the draft lives under `.kingdom/`, which `worktree.rs` already
//! excludes from the repository.
//!
//! **One way to do it.** There is no inline form to fall back to. Two ways to
//! propose would let the model skip the drafting step that is the whole point of
//! this change, and would need prose telling it which to prefer -- which is
//! exactly what Phoenix does not send and does not need.
//!
//! # Why it does not park
//!
//! `ask_user_question` is the closest existing pattern and the wrong one here.
//! A question is answered in seconds; a proposal is read for minutes and slept
//! on for hours. Parking would hold an HTTP request open for the whole review,
//! expire a good proposal on a timeout, and lose the review entirely if the
//! server restarted -- `store::reconcile` would settle the deed as refused and
//! mark the plan failed. Ending the turn costs nothing by comparison, because
//! everything needed to resume is already written down.

use super::{Permissions, Refusal, Sandbox, Tool};
use kingdom_core::ToolOutcome;
use serde_json::{json, Value};

/// Where a proposing plan drafts, relative to its workspace.
///
/// Under `.kingdom/` on purpose: `worktree.rs::exclude_worktree_dir` already
/// adds that directory to the repository's `.git/info/exclude`, and git shares
/// one exclude file across every worktree cut from a repo -- so the draft never
/// shows up as uncommitted work in the user's checkout. Verified against a real
/// worktree rather than assumed.
///
/// A single fixed file rather than a directory the model names. The draft is
/// this plan's one plan; letting it choose where to put it would reintroduce the
/// deliberation this change exists to remove.
pub const DRAFT: &str = ".kingdom/draft.md";

/// What the tool reports when a proposal is accepted for review.
///
/// Matched by [`crate::api::converse`] to know the turn is over. A constant
/// rather than a literal in two places: the check and the message it looks for
/// must not drift, and the failure would be silent -- a model that proposed and
/// then carried straight on to do the work anyway.
pub const PROPOSED: &str = "Plan put to the user. They will read it and either start you on \
                            it or send back changes.";

pub struct ProposePlan;

#[async_trait::async_trait]
impl Tool for ProposePlan {
    fn name(&self) -> &'static str {
        "propose_plan"
    }

    fn description(&self) -> String {
        // Phoenix's `propose_task` framing, pointed at Kingdom's draft file
        // rather than at a task directory. What carries over is the part that
        // matters -- that this is the gateway between the two modes, that it
        // names a file the agent has already written, and that it must be the
        // only call in the response.
        format!(
            "Propose a plan for the user to review and approve. This is the gateway from \
             Propose mode (read-only) to Work mode (write access): you have no ability to \
             change anything until they accept one.\n\n\
             Pass the path to the markdown file you drafted with `patch` in this \
             conversation -- `{DRAFT}`. Its first `# H1` is the title the user sees; the \
             rest is the plan.\n\n\
             Say what you would change, in which files, and why. Say what you checked and \
             what you are still assuming. Be concrete -- name real paths you have actually \
             looked at, not plausible ones.\n\n\
             The user will review and can approve, request revisions, or reject. Your turn \
             ends here either way, and you will be asked again with their answer in front \
             of you. This must be the only tool call in the response."
        )
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "draft": {
                    "type": "string",
                    "description": format!(
                        "Path to the markdown file holding the plan, relative to your \
                         working directory. Write it with `patch` first; while you are \
                         proposing, `{DRAFT}` is the only path `patch` will accept."
                    )
                }
            },
            "required": ["draft"]
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        // Each refusal is written so the model can recover in one turn: one
        // told only "refused" retries exactly the same call.
        match shop.permissions() {
            Permissions::Propose => {}
            Permissions::Full => {
                return Refusal::Refused(
                    "You are already carrying out a plan the user approved, so there is \
                     nothing to propose. Do the work; if the plan turns out to be wrong, \
                     say so."
                        .to_string(),
                )
                .into()
            }
            Permissions::ReadOnly => {
                return Refusal::Refused(
                    "You were sent to answer a question, not to propose work. Report what \
                     you found to the plan that sent you."
                        .to_string(),
                )
                .into()
            }
        }

        let path = text(&input, "draft");
        if path.is_empty() {
            return Refusal::BadArguments {
                tool: self.name().to_string(),
                detail: format!(
                    "a plan needs `draft`, the path to the file you wrote it to (`{DRAFT}`)"
                ),
            }
            .into();
        }

        let resolved = match shop.resolve(&path) {
            Ok(p) => p,
            Err(refusal) => return refusal.into(),
        };

        // A missing draft is the likely mistake -- proposing before writing --
        // so the refusal names the fix rather than reporting an io error.
        let body = match std::fs::read_to_string(&resolved) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Refusal::Refused(format!(
                    "There is no file at `{path}`. Write the plan there first with \
                     `patch` (operation `overwrite`), then propose it."
                ))
                .into()
            }
            Err(e) => return Refusal::Refused(format!("`{path}` could not be read: {e}.")).into(),
        };

        // Both halves are load-bearing and neither can be filled in later: an
        // empty title leaves the sidebar unreadable, and an empty body is a
        // card asking the user to approve nothing at all.
        if body.trim().is_empty() {
            return Refusal::Refused(format!(
                "`{path}` is empty, so there is no plan to put to the user."
            ))
            .into();
        }

        if headline(&body).is_none() {
            return Refusal::Refused(format!(
                "`{path}` has no `# H1` heading, so the plan has no title. Start the file \
                 with one, then propose it again."
            ))
            .into();
        }

        ToolOutcome::done(PROPOSED)
    }
}

/// A trimmed string field, or empty when absent or of the wrong type.
fn text(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The document's first `# H1`, which is the title the user reads.
///
/// Phoenix's `TaskSource::PlainMarkdown` rule. Only `# `, never `##`: a plan
/// whose first heading is a subsection has not given itself a name, and taking
/// one from a subsection would put the wrong words on the rail and on the card.
fn headline(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
}

/// The title and body of a proposal, read off the draft the call named.
///
/// The turn loop needs these to record the proposal on the plan. It reads the
/// file rather than being handed what [`Tool::run`] already saw, because a tool
/// reports through its outcome alone -- threading state out of one would be a
/// second channel for exactly the sort of disagreement this function exists to
/// prevent.
///
/// `None` when the draft cannot be read or carries no title. The loop treats
/// that as "nothing was proposed", which agrees with `run` refusing the same
/// cases -- and disagreeing would park the user in front of a blank card.
pub fn proposed(input: &Value, shop: &Sandbox) -> Option<(String, String)> {
    let path = text(input, "draft");
    if path.is_empty() {
        return None;
    }

    let resolved = shop.resolve(&path).ok()?;
    let body = std::fs::read_to_string(resolved).ok()?;
    let title = headline(&body)?;

    Some((title, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    /// A sandbox over a real directory, since the draft is a real file now.
    fn shop(permissions: Permissions, dir: &std::path::Path) -> Sandbox {
        Sandbox::new(Workspace::in_place(dir.to_str().unwrap())).under(permissions)
    }

    fn draft(dir: &std::path::Path, body: &str) {
        let path = dir.join(DRAFT);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The guard that keeps the ladder one-way.
    ///
    /// A plan with full permissions is already carrying out a proposal the
    /// user accepted. Letting it propose again would let a model manufacture a
    /// second approval card for authority it already holds -- and, worse, would
    /// park a working plan in `AwaitingReview` mid-job. The tool is not even
    /// offered at that level; this is the belt to that braces, because models
    /// invent tool names.
    #[tokio::test]
    async fn only_a_proposing_plan_may_propose() {
        let dir = tempfile::tempdir().unwrap();
        draft(dir.path(), "# Do it\n\nChange things.\n");

        let refused = ProposePlan
            .run(
                json!({ "draft": DRAFT }),
                &shop(Permissions::Full, dir.path()),
            )
            .await;
        assert!(matches!(refused, ToolOutcome::Refused { .. }));

        let accepted = ProposePlan
            .run(
                json!({ "draft": DRAFT }),
                &shop(Permissions::Propose, dir.path()),
            )
            .await;
        assert_eq!(accepted, ToolOutcome::done(PROPOSED));
    }

    /// The title comes off the file, not the arguments.
    ///
    /// `proposed` is what the turn loop records on the plan, so this is the
    /// path by which the user's card and the rail get their words.
    #[tokio::test]
    async fn the_plan_is_read_off_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        draft(dir.path(), "# Remember the folder\n\nStore it in localStorage.\n");

        let shop = shop(Permissions::Propose, dir.path());
        let (title, body) = proposed(&json!({ "draft": DRAFT }), &shop).expect("a titled draft");

        assert_eq!(title, "Remember the folder");
        assert!(body.contains("localStorage"), "{body}");
    }

    /// Proposing before drafting is the mistake this flow invites, so the
    /// refusal has to name the fix rather than report an io error.
    #[tokio::test]
    async fn proposing_without_a_draft_says_how_to_fix_it() {
        let dir = tempfile::tempdir().unwrap();

        let shop = shop(Permissions::Propose, dir.path());
        let outcome = ProposePlan.run(json!({ "draft": DRAFT }), &shop).await;

        match outcome {
            ToolOutcome::Refused { reason } => {
                assert!(reason.contains("patch"), "must name the way out: {reason}");
            }
            ToolOutcome::Done { output, .. } => panic!("should have refused: {output}"),
        }

        // And the loop agrees with the tool, rather than parking the user in
        // front of a blank card.
        assert!(proposed(&json!({ "draft": DRAFT }), &shop).is_none());
    }

    /// A draft with no `# H1` has not named itself, and an untitled plan leaves
    /// the rail and the approval card unreadable.
    #[tokio::test]
    async fn a_draft_needs_a_title() {
        let dir = tempfile::tempdir().unwrap();
        draft(dir.path(), "## Only a subsection\n\nWords.\n");

        let shop = shop(Permissions::Propose, dir.path());
        let outcome = ProposePlan.run(json!({ "draft": DRAFT }), &shop).await;

        assert!(matches!(outcome, ToolOutcome::Refused { .. }), "{outcome:?}");
        assert!(proposed(&json!({ "draft": DRAFT }), &shop).is_none());
    }
}
