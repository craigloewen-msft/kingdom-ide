//! Handing the court's findings on a pull request back to the King.
//!
//! The sibling of [`super::propose_plan`], and the same mechanism part for
//! part: the court writes its report to [`super::propose_plan::DRAFT`] with a
//! `patch` scoped to that one file, revises it as it learns, and this call
//! names the file. It ends the turn; `turn::converse` checks for it by name
//! exactly as it checks for `propose_plan`, and records the report on the plan
//! as a [`kingdom_core::PullRequestReview`].
//!
//! # Why the same file as a plan's draft
//!
//! Because everything downstream already knows that file. It is excluded from
//! the repository, it is filed into the King's records by `store::file_plan`
//! when the plan closes, and it is read before the worktree goes. A second path
//! would need each of those taught again.
//!
//! # Why it never leads to more authority
//!
//! A proposal ends with a button that widens what the plan may touch. A report
//! ends with nothing of the kind: the King reads it, may ask about it, and
//! closes the review. See [`kingdom_core::pull_request`] for the reasoning.

use super::propose_plan::{headline, text, DRAFT};
use super::{Permissions, Refusal, Sandbox, Tool};
use kingdom_core::{Recommendation, ToolOutcome};
use serde_json::{json, Value};

/// What a successful call returns, and what the turn loop matches to know the
/// findings were accepted.
pub const SUBMITTED: &str = "Findings handed to the user. Nothing has been posted to GitHub. \
                             They will read it and may ask you about it.";

pub struct SubmitReview;

#[async_trait::async_trait]
impl Tool for SubmitReview {
    fn name(&self) -> &'static str {
        "submit_review"
    }

    fn description(&self) -> String {
        let keys: Vec<&str> = Recommendation::ALL
            .iter()
            .map(Recommendation::key)
            .collect();
        format!(
            "Hand your review of this pull request to the user. Nothing is posted to \
             GitHub -- the user reads your findings here and decides what to do.\n\n\
             Pass the path to the markdown report you wrote with `patch` -- `{DRAFT}`. Its \
             first `# H1` is the headline the user sees. Pass `recommendation` as one of: \
             {}.\n\n\
             Your turn ends here. If the user asks a follow-up, you will be asked again \
             with their question in front of you; revise the report and submit it again. \
             This must be the only tool call in the response.",
            keys.join(", ")
        )
    }

    fn input_schema(&self) -> Value {
        let keys: Vec<&str> = Recommendation::ALL
            .iter()
            .map(Recommendation::key)
            .collect();
        json!({
            "type": "object",
            "properties": {
                "report": {
                    "type": "string",
                    "description": format!(
                        "Path to the markdown report, relative to your working directory. \
                         Write it with `patch` first; `{DRAFT}` is the only path `patch` \
                         will accept."
                    )
                },
                "recommendation": {
                    "type": "string",
                    "enum": keys,
                    "description": "merge: works as described and is sound enough to merge. \
                                    changes: should not merge as it stands. discuss: a \
                                    question the user must settle, or something you could \
                                    not test."
                }
            },
            "required": ["report", "recommendation"]
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        if shop.permissions() != Permissions::Review {
            return Refusal::Refused(
                "Only a plan reviewing a pull request hands back findings. Say what you \
                 found in your reply instead."
                    .to_string(),
            )
            .into();
        }

        match read(&input, shop) {
            Ok(_) => ToolOutcome::done(SUBMITTED),
            Err(refusal) => refusal.into(),
        }
    }
}

/// The headline, report and recommendation the call names, or why not.
///
/// Shared by [`Tool::run`] and [`submitted`], so the tool refusing a call and
/// the turn loop recording one cannot disagree about what counts.
fn read(input: &Value, shop: &Sandbox) -> Result<(String, String, Recommendation), Refusal> {
    let path = text(input, "report");
    if path.is_empty() {
        return Err(Refusal::BadArguments {
            tool: "submit_review".to_string(),
            detail: format!("findings need `report`, the path you wrote them to (`{DRAFT}`)"),
        });
    }

    let Some(recommendation) = Recommendation::from_key(&text(input, "recommendation")) else {
        let keys: Vec<&str> = Recommendation::ALL
            .iter()
            .map(Recommendation::key)
            .collect();
        return Err(Refusal::BadArguments {
            tool: "submit_review".to_string(),
            detail: format!("`recommendation` must be one of {}", keys.join(", ")),
        });
    };

    let resolved = shop.resolve(&path)?;
    let body = match std::fs::read_to_string(&resolved) {
        Ok(body) => body,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Refusal::Refused(format!(
                "There is no file at `{path}`. Write your findings there first with `patch` \
                 (operation `overwrite`), then submit them."
            )))
        }
        Err(e) => {
            return Err(Refusal::Refused(format!(
                "`{path}` could not be read: {e}."
            )))
        }
    };

    if body.trim().is_empty() {
        return Err(Refusal::Refused(format!(
            "`{path}` is empty, so there are no findings to hand back."
        )));
    }
    let Some(title) = headline(&body) else {
        return Err(Refusal::Refused(format!(
            "`{path}` has no `# H1` heading, so the report has no headline. Start the file \
             with one, then submit it again."
        )));
    };

    Ok((title, body, recommendation))
}

/// What the turn loop records, read off the arguments of a call it is about to
/// run. `None` when the call would be refused.
pub fn submitted(input: &Value, shop: &Sandbox) -> Option<(String, String, Recommendation)> {
    read(input, shop).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    fn shop(permissions: Permissions, dir: &std::path::Path) -> Sandbox {
        Sandbox::new(Workspace::in_place(dir.to_str().unwrap())).under(permissions)
    }

    fn report(dir: &std::path::Path, body: &str) {
        let path = dir.join(DRAFT);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn call(recommendation: &str) -> Value {
        json!({ "report": DRAFT, "recommendation": recommendation })
    }

    #[tokio::test]
    async fn a_written_report_with_a_headline_is_accepted_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        report(dir.path(), "# Works, one bug\n\n## Findings\n- x");
        let shop = shop(Permissions::Review, dir.path());

        let outcome = SubmitReview.run(call("changes"), &shop).await;
        assert!(matches!(&outcome, ToolOutcome::Done { output, .. } if output == SUBMITTED));

        let (title, body, recommendation) = submitted(&call("changes"), &shop).unwrap();
        assert_eq!(title, "Works, one bug");
        assert!(body.contains("## Findings"));
        assert_eq!(recommendation, Recommendation::Changes);
    }

    /// Each refusal names the fix, so the model recovers in one turn.
    #[tokio::test]
    async fn every_way_of_getting_it_wrong_is_refused_with_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let shop = shop(Permissions::Review, dir.path());

        let missing = SubmitReview.run(call("merge"), &shop).await;
        assert!(
            matches!(missing, ToolOutcome::Refused { reason } if reason.contains("Write your findings"))
        );

        report(dir.path(), "no heading here");
        let untitled = SubmitReview.run(call("merge"), &shop).await;
        assert!(matches!(untitled, ToolOutcome::Refused { reason } if reason.contains("H1")));

        report(dir.path(), "# Fine");
        let unknown = SubmitReview.run(call("ship it"), &shop).await;
        assert!(
            matches!(unknown, ToolOutcome::Refused { reason } if reason.contains("merge, changes, discuss"))
        );
        assert!(submitted(&call("ship it"), &shop).is_none());
    }

    /// Only a review hands back findings. A proposing plan that called this
    /// would be skipping the King's approval by another door.
    #[tokio::test]
    async fn only_a_review_may_submit() {
        let dir = tempfile::tempdir().unwrap();
        report(dir.path(), "# Fine");
        for other in [Permissions::Propose, Permissions::Full, Permissions::Browse] {
            let outcome = SubmitReview
                .run(call("merge"), &shop(other, dir.path()))
                .await;
            assert!(
                matches!(outcome, ToolOutcome::Refused { .. }),
                "{other:?} must not submit a review"
            );
        }
    }
}
