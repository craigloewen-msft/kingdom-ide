//! Asking the user.
//!
//! The most on-metaphor tool in the set, and the only one that does not act on
//! the world at all: the model stops mid-work, turns, and asks *there are two
//! ways to do this, which do you want?* The user answers and the work carries
//! on. That is the product's whole stance -- a sovereign reviewing proposals --
//! happening inside a single turn rather than only at its end.
//!
//! # Why this needed the socket
//!
//! Every other tool answers its own call. This one cannot: the answer comes
//! from a person, through a different HTTP request, minutes later. The call
//! *parks* -- its future waits on a oneshot -- and the question reaches the
//! user because the server can now speak first. Without push this would have
//! to be a queue the browser polled, which is a worse socket built by accident.
//! It is the reason the socket landed before the tool loop did.
//!
//! # Why a parked call is dangerous, and what stops it
//!
//! A plan waiting on a human is a plan that can wait forever. The user closes
//! the tab, goes to lunch, forgets. Left alone the plan stays busy, and a busy
//! plan cannot be spoken to -- the conversation disables its composer -- so it
//! is not merely idle, it is unusable, and nothing on the server would ever
//! free it. Two things stop that, and both are load-bearing rather than polish:
//!
//! 1. [`PATIENCE`] below. An unanswered question gives up and reports why,
//!    which returns through the ordinary path and clears the busy mark.
//! 2. `store::reconcile`, for the harder case: if the *server* stops while a
//!    question is parked, the timeout dies with it. A plan is repaired on load
//!    instead, which is the only place that can know.

use super::{Refusal, Sandbox, Tool};
use kingdom_core::{PlanId, ToolOutcome, WaitBudget, ASK_USER_QUESTION};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::oneshot;

/// How long a question waits before giving up on being answered.
///
/// Long, because the user is a person and this is his machine: a question that
/// expires while he is at lunch throws away work that was one click from
/// continuing. Finite, because the alternative is a plan that can never be used
/// again -- see the module docs.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Questions in front of the user, keyed by the plan and tool call that asked,
/// each holding the half of a oneshot its call is blocked on.
///
/// A named type because the map appears in both the static and the accessor
/// below; clippy's `type_complexity` flags spelling it out twice.
type Pending = HashMap<(PlanId, String), oneshot::Sender<String>>;

/// Questions currently in front of the user, keyed by the plan and tool call
/// that asked.
///
/// Deliberately not persisted. A oneshot cannot outlive its process, so writing
/// the *question* to disk while its waiting half stayed in memory would leave a
/// record the server could never resolve -- a question the conversation renders
/// as answerable and that nothing is listening for. The tool call is on disk;
/// the waiting is not, and `store::reconcile` closes the gap on restart.
static PENDING: OnceLock<Mutex<Pending>> = OnceLock::new();

fn pending() -> &'static Mutex<Pending> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Hands the user's answer to the call waiting for it.
///
/// Returns false when nothing was waiting: a question already answered in
/// another tab, or one whose server has restarted since. The caller reports
/// that to the user rather than pretending it landed, because an answer that
/// vanishes silently is one he will sit waiting on.
pub fn answer(plan: &PlanId, tool_call: &str, answer: String) -> bool {
    let Ok(mut pending) = pending().lock() else {
        return false;
    };
    match pending.remove(&(plan.clone(), tool_call.to_string())) {
        Some(tx) => tx.send(answer).is_ok(),
        None => false,
    }
}

/// Abandons a question whose turn has stopped.
///
/// Called when the user calls a halt: the parked call is about to be settled as
/// refused, so its sender must go with it. Without this, `PENDING` would keep a
/// oneshot nobody will ever send on until [`PATIENCE`] expired, and an answer
/// typed into a stale tab would arrive for a tool call the transcript has
/// already closed.
pub fn abandon(plan: &PlanId, tool_call: &str) {
    if let Ok(mut pending) = pending().lock() {
        pending.remove(&(plan.clone(), tool_call.to_string()));
    }
}

pub struct AskUserQuestion;

#[async_trait::async_trait]
impl Tool for AskUserQuestion {
    /// The name is [`ASK_USER_QUESTION`], defined in `kingdom-core` because the
    /// browser has to recognise a parked question by it and cannot see this
    /// crate. The tool still answers for its own name; it just no longer spells
    /// it out a second time.
    fn name(&self) -> &'static str {
        ASK_USER_QUESTION
    }

    fn description(&self) -> String {
        "Ask the user clarifying questions when you need input to proceed. \
         Use when there are multiple valid approaches and user preference \
         matters. Provide 1-4 questions with 2-4 options each. Users can \
         also type custom answers. This must be the only tool call in \
         the response (do not combine with other tool calls)."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "description": "The questions to put to the user.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The full question."
                            },
                            "header": {
                                "type": "string",
                                "description": "Short label, at most 12 characters."
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "Whether several options may be chosen."
                            },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 4,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": {
                                            "type": "string",
                                            "description": "What choosing this means."
                                        }
                                    },
                                    "required": ["label"]
                                }
                            }
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let questions = input.get("questions").and_then(Value::as_array);
        if questions.is_none_or(|q| q.is_empty()) {
            return Refusal::BadArguments {
                tool: self.name().to_string(),
                detail: "no `questions` were given".to_string(),
            }
            .into();
        }

        // Outside a turn there is no tool call for an answer to name, so there
        // is no way for one to come back. Refusing beats parking forever.
        let Some(tool_call) = shop.tool_call() else {
            return Refusal::Refused(
                "A question can only be asked during a turn, and this call is not part of one."
                    .to_string(),
            )
            .into();
        };

        let (tx, rx) = oneshot::channel();
        let key = (shop.plan().clone(), tool_call.to_string());

        // Registered before the wait, so an answer arriving the instant the
        // conversation renders the question cannot find nothing listening.
        match pending().lock() {
            Ok(mut waiting) => {
                waiting.insert(key.clone(), tx);
            }
            Err(_) => {
                return Refusal::Refused(
                    "Kingdom could not put that question to the King.".to_string(),
                )
                .into()
            }
        }

        match tokio::time::timeout(PATIENCE, rx).await {
            Ok(Ok(answer)) => ToolOutcome::done(answer),

            // Timed out, or the waiting half was dropped. Either way nothing is
            // going to answer, so the entry is cleaned up rather than left to
            // accumulate for the life of the process.
            _ => {
                if let Ok(mut waiting) = pending().lock() {
                    waiting.remove(&key);
                }
                Refusal::Refused(
                    "The King did not answer, so this question has expired. Carry on with \
                     the most reasonable option and say which you chose, or stop and \
                     explain what you need to know."
                        .to_string(),
                )
                .into()
            }
        }
    }

    /// [`PATIENCE`], as a deadline: an unanswered question really does expire,
    /// and what comes back is a refusal rather than a handle.
    ///
    /// Recorded but not currently drawn, and deliberately so. The chamber shows
    /// a budget only while a call is in flight, and a question in flight is not
    /// rendered as a deed at all -- it is rendered as the thing to *do*, by
    /// `Question`. Putting a countdown under a decision the King is in the
    /// middle of taking would hurry him over exactly the judgement this product
    /// exists to let him make slowly. It is answered here anyway because the
    /// question is a fact about the call, and the alternative -- a tool that
    /// waits half an hour and reports no wait -- is the kind of quiet
    /// inconsistency the next reader has to rediscover.
    fn waits_for(&self, _input: &Value) -> Option<WaitBudget> {
        Some(WaitBudget::Deadline {
            seconds: PATIENCE.as_secs(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    fn shop() -> Sandbox {
        Sandbox::new(Workspace::in_place("/dev/city"))
            .for_plan(PlanId::new("plan-1"))
            .for_tool_call("call-1")
    }

    fn one_question() -> Value {
        json!({
            "questions": [{
                "question": "Which way?",
                "header": "Approach",
                "options": [{ "label": "Left" }, { "label": "Right" }]
            }]
        })
    }

    /// The whole mechanism: a call that parks, an answer arriving from
    /// somewhere else entirely, and the call resuming with it. This is the one
    /// thing in the tool surface that request/response could not do, so it is
    /// the one worth pinning.
    #[tokio::test]
    async fn a_parked_question_resumes_when_the_king_answers() {
        let asking = tokio::spawn(async { AskUserQuestion.run(one_question(), &shop()).await });

        // Wait for the call to have registered before answering, exactly as the
        // user would be waiting for the conversation to render it.
        let plan = PlanId::new("plan-1");
        for _ in 0..200 {
            if answer(&plan, "call-1", "Left".to_string()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        assert_eq!(asking.await.unwrap(), ToolOutcome::done("Left"),);
    }

    /// An answer with nothing waiting for it must say so. Reporting success
    /// would leave the user believing he had replied while the model sat
    /// waiting on a question he can no longer see.
    #[test]
    fn answering_a_question_nobody_asked_is_reported_not_swallowed() {
        assert!(!answer(
            &PlanId::new("plan-nobody"),
            "call-nobody",
            "Left".to_string()
        ));
    }
}
