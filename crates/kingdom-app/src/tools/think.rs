//! Thinking out loud.
//!
//! The one tool that does nothing. It exists because a model given only
//! *acting* tools will act in order to think -- running a command it does not
//! need so it has somewhere to reason -- and because a plan whose transcript
//! shows the court's reasoning is a plan the King can actually review.
//!
//! It also earns its place as the first tool: it exercises the whole loop
//! (schema out, call in, deed recorded, result back) without touching the
//! filesystem, so a failure here is a failure in the machinery rather than in
//! anything it drives.

use super::{Tool, Sandbox};
use kingdom_core::ToolOutcome;
use serde_json::{json, Value};

pub struct Think;

#[async_trait::async_trait]
impl Tool for Think {
    fn name(&self) -> &'static str {
        "think"
    }

    fn description(&self) -> String {
        "Reason through a problem before acting: plan an approach, weigh \
         trade-offs, or work out what went wrong. Writes nothing and changes \
         nothing. Use it before a complex change rather than working it out by \
         running commands."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thoughts": {
                    "type": "string",
                    "description": "The reasoning, notes, or plan."
                }
            },
            "required": ["thoughts"]
        })
    }

    /// Echoes the thought back as the result.
    ///
    /// Not a no-op return: the model's own reasoning has to reappear in the
    /// next request or the turn after this one is built from a conversation in
    /// which it never thought anything. The echo *is* the mechanism.
    async fn run(&self, input: Value, _shop: &Sandbox) -> ToolOutcome {
        let thoughts = input
            .get("thoughts")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();

        if thoughts.is_empty() {
            return super::Refusal::BadArguments {
                tool: "think".to_string(),
                detail: "no `thoughts` were given".to_string(),
            }
            .into();
        }

        ToolOutcome::done(thoughts)
    }
}
