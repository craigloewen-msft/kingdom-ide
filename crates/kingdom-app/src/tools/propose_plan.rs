//! Putting a plan to the King.
//!
//! The gateway from counsel to work, and the only tool that does not act on the
//! world at all: it ends the turn. The court says what it would do, and stops.
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
//! # Why the proposal travels in the arguments
//!
//! Phoenix carries a *path*: the agent writes markdown to
//! `tasks/NNNNN-pX-status--slug.md` and the tool call names it. That buys
//! revision history for free but costs a whole substrate -- a scoped `patch`
//! allowlist, an ID-allocation hint in the prompt, a `_TEMPLATE.md` marker, a
//! revisions table, and a status rename committed on approval.
//!
//! Kingdom needs none of it, because a plan is already a document. The body
//! rides in the arguments, so it lands on the deed, so it is in the transcript,
//! so it is on disk, so it is pushed to the chamber, and so it is in the
//! model's own context next round -- all by machinery that already exists. The
//! King's project stays free of files Kingdom invented, which is the point.
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

use super::{Refusal, Remit, Tool, Workshop};
use kingdom_core::DeedOutcome;
use serde_json::{json, Value};

/// What the tool reports when a proposal is accepted for review.
///
/// Matched by [`crate::api::converse`] to know the turn is over. A constant
/// rather than a literal in two places: the check and the message it looks for
/// must not drift, and the failure would be silent -- a court that proposed and
/// then carried straight on to do the work anyway.
pub const PROPOSED: &str = "Plan put to the King. He will read it and either start you on it \
                            or send back changes.";

pub struct ProposePlan;

#[async_trait::async_trait]
impl Tool for ProposePlan {
    fn name(&self) -> &'static str {
        "propose_plan"
    }

    fn description(&self) -> String {
        "Put a plan to the King for review. This is how you get the ability to change \
         anything: you have no hands until he accepts one.\n\n\
         Say what you would change, in which files, and why. Say what you checked and \
         what you are still assuming. Be concrete -- name real paths you have actually \
         looked at, not plausible ones.\n\n\
         Your turn ends here. He will either start you on this plan or send back changes, \
         and either way you will be asked again with his answer in front of you."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "A short headline for the work, at most about 60 characters."
                },
                "body": {
                    "type": "string",
                    "description": "The plan itself, as markdown. Start with what you would \
                                    do and why, then the specific changes by file."
                }
            },
            "required": ["title", "body"]
        })
    }

    async fn run(&self, input: Value, shop: &Workshop) -> DeedOutcome {
        // Each refusal is written so the model can recover in one turn: a court
        // told only "refused" retries exactly the same call.
        match shop.remit() {
            Remit::Counsel => {}
            Remit::Full => {
                return Refusal::Refused(
                    "You are already carrying out a plan the King approved, so there is \
                     nothing to propose. Do the work; if the plan turns out to be wrong, \
                     say so."
                        .to_string(),
                )
                .into()
            }
            Remit::Survey => {
                return Refusal::Refused(
                    "You were sent to answer a question, not to propose work. Report what \
                     you found to the court that sent you."
                        .to_string(),
                )
                .into()
            }
        }

        let title = text(&input, "title");
        let body = text(&input, "body");

        // Both halves are load-bearing and neither can be filled in later: an
        // empty title leaves the rail unreadable, and an empty body is a card
        // asking the King to approve nothing at all.
        let missing = match (title.is_empty(), body.is_empty()) {
            (true, true) => Some("a `title` and a `body`"),
            (true, false) => Some("a `title`"),
            (false, true) => Some("a `body`"),
            (false, false) => None,
        };
        if let Some(missing) = missing {
            return Refusal::BadArguments {
                tool: self.name().to_string(),
                detail: format!("a plan needs {missing}"),
            }
            .into();
        }

        DeedOutcome::done(PROPOSED)
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

/// The title and body of a proposal, read back off the deed that made it.
///
/// The turn loop needs these to record the proposal on the plan, and reads them
/// from the deed rather than being handed them by the tool -- the deed is what
/// was actually recorded, so there is one source of truth and no way for the
/// stored arguments and the stored proposal to disagree.
pub fn proposed(input: &Value) -> Option<(String, String)> {
    let title = text(input, "title");
    let body = text(input, "body");
    (!title.is_empty() && !body.is_empty()).then_some((title, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    fn shop(remit: Remit) -> Workshop {
        Workshop::new(Workspace::in_place("/dev/city")).under(remit)
    }

    /// The guard that keeps the ladder one-way.
    ///
    /// A plan with full hands is already carrying out a proposal the King
    /// accepted. Letting it propose again would let a court manufacture a
    /// second approval card for authority it already holds -- and, worse, would
    /// park a working plan in `AwaitingReview` mid-job. The tool is not even
    /// offered at that rung; this is the belt to that braces, because models
    /// invent tool names.
    #[tokio::test]
    async fn only_a_counselling_plan_may_propose() {
        let refused = ProposePlan
            .run(
                json!({ "title": "Do it", "body": "Change things." }),
                &shop(Remit::Full),
            )
            .await;
        assert!(matches!(refused, DeedOutcome::Refused { .. }));

        let accepted = ProposePlan
            .run(
                json!({ "title": "Do it", "body": "Change things." }),
                &shop(Remit::Counsel),
            )
            .await;
        assert_eq!(accepted, DeedOutcome::done(PROPOSED));
    }
}
