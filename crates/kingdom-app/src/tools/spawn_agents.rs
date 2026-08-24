//! Sending subagents: the model delegating a question to another agent.
//!
//! The model-facing name is `spawn_agents`, matching Phoenix IDE and the wider
//! ecosystem, for the same reason `ask_user_question` is not called
//! `put_it_to_the_king`: a tool name is the one string a model has strong priors
//! about, and a novel one buys metaphor consistency at the cost of malformed
//! calls. The domain noun is a *subagent* everywhere a person reads it.
//!
//! # What a subagent is
//!
//! Another agent, working in the same place as the one that sent it, on the
//! same files. It is a real [`kingdom_core::Plan`] -- which is what gives it a
//! conversation at its own URL, a watch socket, and a record on disk -- but one
//! the user never asked for, so it is kept out of the rail and off the map.
//!
//! # Why this is safe to run in parallel
//!
//! Because subagents cannot write. They are run under
//! [`Permissions::ReadOnly`], so the only tools they have are `think`,
//! `read_file` and `search`. Several agents writing to one worktree at once is
//! exactly the collision this product exists to prevent, and nothing in Kingdom
//! arbitrates yet -- so rather than detect it, the permission level makes it
//! impossible. Give subagents hands and this file needs a lease before it needs
//! anything else.

use super::{Refusal, Permissions, Tool, Sandbox};
use kingdom_core::ToolOutcome;
use serde_json::{json, Value};

/// The most subagents one call may send.
///
/// Lower than Phoenix's ten: these are concurrent calls to one gateway, and six
/// is already the point where rate limits start answering instead of models.
const MOST_SUBAGENTS: usize = 6;

/// How long the whole call may take before it reports what it has.
///
/// The parent's turn is blocked for as long as the slowest subagent, so without
/// this a gateway that never answers parks the plan indefinitely -- and a
/// parked plan cannot be spoken to, which is the trap `ask_user_question`'s
/// `PATIENCE` exists to avoid. Subagents still running when this expires are
/// reported as timed out and the parent gets whatever the others found: a
/// partial answer it can act on beats a turn that never returns.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub struct SpawnAgents;

#[async_trait::async_trait]
impl Tool for SpawnAgents {
    fn name(&self) -> &'static str {
        "spawn_agents"
    }

    fn description(&self) -> String {
        "Send sub-agents to investigate things in parallel and report back. Each \
         one works in this same directory, reads with its own tools, and returns \
         a written answer. They are read-only: they cannot edit files or run \
         commands, so use them to find things out and then act on what they \
         find yourself. Good for questions that are independent of each other -- \
         surveying unfamiliar code from several angles at once, or checking \
         several places something might be handled. Do not use one for work you \
         could do with a single read or search."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["tasks"],
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MOST_SUBAGENTS,
                    "description": "What to send each sub-agent to find out. They \
                                    run at the same time and cannot see each \
                                    other, so each task must stand alone.",
                    "items": {
                        "type": "object",
                        "required": ["task"],
                        "properties": {
                            "task": {
                                "type": "string",
                                "description": "The question to answer, in full. The \
                                                sub-agent sees only this and the \
                                                project -- not your conversation \
                                                -- so include everything it needs."
                            }
                        }
                    }
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let tasks: Vec<String> = input
            .get("tasks")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t.get("task").and_then(Value::as_str))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .take(MOST_SUBAGENTS)
            .collect();

        if tasks.is_empty() {
            return Refusal::BadArguments {
                tool: self.name().to_string(),
                detail: "no `tasks` were given, so there was nobody to send".to_string(),
            }
            .into();
        }

        // Outside a turn there is no call for a subagent to belong to, and
        // `subagents_of` is keyed by it -- a subagent recorded against no tool
        // call would be invisible in every conversation. Refusing beats
        // orphaning it.
        let Some(tool_call) = shop.tool_call() else {
            return Refusal::Refused(
                "Errands can only be sent during a turn, and this call is not part of one."
                    .to_string(),
            )
            .into();
        };

        match crate::api::spawn_subagents(shop.plan(), tool_call, tasks, PATIENCE).await {
            Ok(reports) => ToolOutcome::done(reports),
            Err(why) => Refusal::Refused(why).into(),
        }
    }
}

/// The permissions a subagent works under. Named here rather than at the call
/// site so the reason travels with the tool that depends on it.
pub const SUBAGENT_PERMISSIONS: Permissions = Permissions::ReadOnly;

/// How many rounds a subagent may take before it is stopped.
///
/// Lower than a user-opened plan's cap, because a subagent has three read-only
/// tools and a single question: a survey that has not answered in this many
/// rounds is looping, not thinking. It bounds the same failure the parent's cap
/// does -- an agent burning a paid model quietly -- multiplied by however many
/// subagents are in flight.
pub const MOST_SUBAGENT_ROUNDS: usize = 12;
