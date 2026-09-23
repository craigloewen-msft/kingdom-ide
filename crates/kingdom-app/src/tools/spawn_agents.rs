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
//! [`Permissions::Browse`], so the tools they have are `think`, `read_file`,
//! `search`, `read_image`, `skill` and the `browser_*` family. Several agents
//! writing to one worktree at once is exactly the collision this product exists
//! to prevent, and nothing in Kingdom arbitrates yet -- so rather than detect
//! it, the permission level makes it impossible. Give subagents hands and this
//! file needs a lease before it needs anything else.
//!
//! A browser is not hands. Each subagent drives a Chrome of its own, keyed by
//! its own plan id (`tools::browser`), inside whatever network its parent is
//! working in (`namespaces::owner_of`) -- so it can open the app its parent
//! started without steering the session the King is watching.

use super::{Permissions, Refusal, Sandbox, Tool};
use kingdom_core::{ModelChoice, ToolOutcome};
use serde_json::{json, Value};

/// The most subagents one call may send.
///
/// Phoenix's number. Kingdom previously capped at six, justified by concurrent
/// load on one gateway -- a guess rather than an observation, and Phoenix runs
/// ten against the same gateways without trouble.
const MOST_SUBAGENTS: usize = 10;

/// How many rounds a subagent gets when it is not told otherwise.
///
/// Phoenix's explore default. This bounds the same failure the parent's cap
/// does -- an agent burning a paid model quietly -- multiplied by however many
/// subagents are in flight.
pub const DEFAULT_SUBAGENT_ROUNDS: usize = 20;

/// How long the whole call may take before it reports what it has.
///
/// The parent's turn is blocked for as long as the slowest subagent, so without
/// this a gateway that never answers parks the plan indefinitely -- and a
/// parked plan cannot be spoken to, which is the trap `ask_user_question`'s
/// `PATIENCE` exists to avoid. Subagents still running when this expires are
/// reported as timed out and the parent gets whatever the others found: a
/// partial answer it can act on beats a turn that never returns.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// One subagent to send: what to ask it, how long to let it look, and who to
/// ask.
#[derive(Debug, Clone)]
pub struct Errand {
    /// The question, in full. The subagent sees this and the project, not the
    /// parent's conversation.
    pub task: String,
    /// Rounds this one may take before it is stopped.
    pub max_turns: usize,
    /// The model to send it to, when the parent asked for one other than its
    /// own. `None` means "whatever I am", which is what `Plan::spawned`
    /// already does.
    pub choice: Option<ModelChoice>,
}

pub struct SpawnAgents;

#[async_trait::async_trait]
impl Tool for SpawnAgents {
    fn name(&self) -> &'static str {
        "spawn_agents"
    }

    fn description(&self) -> String {
        // Phoenix's wording, minus the clause that would be false here: Kingdom
        // has no named personas (`agent_type`). The capability sentence is
        // Kingdom's own -- a subagent here holds a browser, and a model that is
        // not told so will never send one to look at a page.
        "Spawn sub-agents to execute tasks in parallel. Each sub-agent runs independently \
         and returns a result. Sub-agents can read, search, look at images, and drive a \
         browser of their own -- navigate, click, screenshot, profile and read the console \
         -- so they can verify a running page as well as read code. They cannot run \
         commands or edit files, so use them to find things out, then act on what they \
         find yourself. Anything they are asked to look at must already be running: they \
         cannot start it. Use for: checking that a page really behaves as intended, \
         multiple perspectives on code review, exploring unfamiliar parts of a codebase, \
         parallel research, or divide-and-conquer problem solving."
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
                    "description": format!(
                        "List of tasks to execute in parallel (max {MOST_SUBAGENTS}). They \
                         run at the same time and cannot see each other, so each task must \
                         stand alone."
                    ),
                    "items": {
                        "type": "object",
                        "required": ["task"],
                        "properties": {
                            "task": {
                                "type": "string",
                                "description": "Task description for the sub-agent. It sees \
                                                only this and the project -- not your \
                                                conversation -- so include everything it needs."
                            },
                            "max_turns": {
                                "type": "integer",
                                "minimum": 1,
                                "description": format!(
                                    "Maximum LLM turns before forced completion. Defaults to \
                                     {DEFAULT_SUBAGENT_ROUNDS}."
                                )
                            },
                            "model": {
                                "type": "string",
                                "description": "Send this one to a different model, by id \
                                                (e.g. `copilot/claude-sonnet-4`). Defaults \
                                                to your own. Use it when you want a second \
                                                opinion rather than a second copy of \
                                                yourself -- checking your own work is worth \
                                                more from a model that is not you."
                            },
                            "effort": {
                                "type": "string",
                                "description": "Reasoning effort for this one: none, \
                                                minimal, low, medium, high, xhigh or max. \
                                                Defaults to yours. Ignored by models with \
                                                no effort control."
                            }
                        }
                    }
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        // Every model anybody can be sent to, fetched once for the whole call
        // rather than per task: it is one provider round trip, and ten tasks
        // naming the same model must not make ten of them.
        let served: Vec<String> = crate::llm::catalogue::catalogue()
            .await
            .options
            .into_iter()
            .map(|o| o.id)
            .collect();

        let tasks = match errands(&input, &served) {
            Ok(tasks) => tasks,
            Err(detail) => {
                return Refusal::BadArguments {
                    tool: self.name().to_string(),
                    detail,
                }
                .into()
            }
        };

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

        match crate::turn::spawn_subagents(shop.plan(), tool_call, tasks, PATIENCE).await {
            Ok(reports) => ToolOutcome::done(reports),
            Err(why) => Refusal::Refused(why).into(),
        }
    }
}

/// Reads the call's `tasks` into errands, or says what was wrong with it.
///
/// Separate from [`SpawnAgents::run`] so it can be tested without a kingdom, a
/// turn or a gateway behind it: everything here is a decision about the
/// model's arguments, and that is the part worth pinning. `served` is every
/// model id the catalogue offers, passed in for the reason
/// `llm::catalogue::default_id` takes its preference as a parameter -- a
/// decision that reads the world cannot be tested, only suffered.
///
/// `Err` carries the `detail` of a [`Refusal::BadArguments`], written for the
/// model so it can correct itself in one turn.
fn errands(input: &Value, served: &[String]) -> Result<Vec<Errand>, String> {
    let given = input
        .get("tasks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut tasks: Vec<Errand> = Vec::new();
    for t in given.iter().take(MOST_SUBAGENTS) {
        let Some(task) = t.get("task").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if task.is_empty() {
            continue;
        }

        // A model the parent named that nobody serves is refused rather than
        // quietly swapped for the parent's own. The whole reason to name one is
        // to get a *different* opinion, so silently returning the same one
        // would answer a question that was not asked -- and the refusal names
        // what is on offer, so the model can correct itself in one turn.
        let choice = match t.get("model").and_then(Value::as_str).map(str::trim) {
            Some(wanted) if !wanted.is_empty() => {
                if !served.iter().any(|id| id == wanted) {
                    return Err(format!(
                        "no model is served under the id `{wanted}`. The ids available \
                         are: {}. Leave `model` out to use your own.",
                        served.join(", ")
                    ));
                }
                // The effort is only read alongside a model: an effort on its
                // own would have to be attached to the parent's model, and a
                // level that model does not offer is rejected by the gateway
                // rather than by us. An unreadable level is dropped rather than
                // refused -- the model that was asked for is the substance of
                // the request, and the level is a preference about it.
                let effort = t
                    .get("effort")
                    .and_then(Value::as_str)
                    .and_then(kingdom_core::ModelEffort::from_wire);
                Some(ModelChoice::new(wanted, effort))
            }
            _ => None,
        };

        tasks.push(Errand {
            task: task.to_string(),
            // A cap of zero would be a subagent that cannot take a single turn,
            // which is a request to do nothing rather than an instruction worth
            // honouring.
            max_turns: t
                .get("max_turns")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_SUBAGENT_ROUNDS, |n| (n as usize).max(1)),
            choice,
        });
    }

    if tasks.is_empty() {
        return Err("no `tasks` were given, so there was nobody to send".to_string());
    }
    Ok(tasks)
}

/// The permissions a subagent works under. Named here rather than at the call
/// site so the reason travels with the tool that depends on it.
///
/// [`Permissions::Browse`] rather than `ReadOnly`: a subagent is sent to find
/// something out, and whether the page actually does what it should is one of
/// the things worth finding out. It still cannot write -- see the module docs
/// for why that is what makes running several at once safe.
pub const SUBAGENT_PERMISSIONS: Permissions = Permissions::Browse;

#[cfg(test)]
mod tests {
    use super::*;

    fn served() -> Vec<String> {
        vec!["mock".to_string(), "copilot/claude-opus-5".to_string()]
    }

    fn one(task: Value) -> Result<Vec<Errand>, String> {
        errands(&json!({ "tasks": [task] }), &served())
    }

    /// The ordinary call: a task and nothing else. Both defaults are what the
    /// parent already is, which is what makes the two new fields optional
    /// rather than a thing every call has to think about.
    #[test]
    fn a_bare_task_inherits_everything() {
        let errands = one(json!({ "task": "Read the parser" })).unwrap();
        assert_eq!(errands.len(), 1);
        assert_eq!(errands[0].task, "Read the parser");
        assert_eq!(errands[0].max_turns, DEFAULT_SUBAGENT_ROUNDS);
        assert!(
            errands[0].choice.is_none(),
            "no model named means the parent's, decided by `Plan::spawned`"
        );
    }

    /// The point of the field: a different model, so the checker is not a copy
    /// of the thing being checked.
    #[test]
    fn a_named_model_and_effort_are_carried() {
        let errands = one(json!({
            "task": "Check the login page renders",
            "model": "copilot/claude-opus-5",
            "effort": "high",
        }))
        .unwrap();
        let choice = errands[0].choice.clone().expect("a model was named");
        assert_eq!(choice.model, "copilot/claude-opus-5");
        assert_eq!(choice.effort, Some(kingdom_core::ModelEffort::High));
    }

    /// A model nobody serves is refused, and the refusal names what is on
    /// offer.
    ///
    /// The naming is the load-bearing half. Falling back to the parent's model
    /// would answer a question that was not asked -- the parent wanted another
    /// opinion and would silently get its own -- and a refusal that did not
    /// list the ids would earn a retry of the same guess.
    #[test]
    fn a_model_nobody_serves_is_refused_by_name() {
        let why = one(json!({ "task": "Look", "model": "gpt-9-ultra" })).unwrap_err();
        assert!(why.contains("gpt-9-ultra"), "{why}");
        assert!(why.contains("copilot/claude-opus-5"), "{why}");
        assert!(why.contains("mock"), "{why}");
    }

    /// An effort level the wire does not know is dropped, not refused: the
    /// model asked for is the substance of the request, and the level is a
    /// preference about it.
    #[test]
    fn an_unreadable_effort_leaves_the_model_standing() {
        let errands = one(json!({
            "task": "Look",
            "model": "mock",
            "effort": "enthusiastic",
        }))
        .unwrap();
        let choice = errands[0].choice.clone().expect("a model was named");
        assert_eq!(choice.model, "mock");
        assert_eq!(choice.effort, None);
    }

    /// An effort with no model is ignored rather than attached to the parent's
    /// model, which may not offer that level at all.
    #[test]
    fn an_effort_without_a_model_changes_nothing() {
        let errands = one(json!({ "task": "Look", "effort": "high" })).unwrap();
        assert!(errands[0].choice.is_none());
    }

    /// The existing bounds still hold: empty tasks are dropped, a zero cap is
    /// raised to one, and no more than `MOST_SUBAGENTS` are sent.
    #[test]
    fn the_bounds_on_a_call_are_unchanged() {
        assert!(errands(&json!({ "tasks": [] }), &served()).is_err());
        assert!(errands(&json!({}), &served()).is_err());
        assert!(one(json!({ "task": "   " })).is_err());

        let capped = one(json!({ "task": "Look", "max_turns": 0 })).unwrap();
        assert_eq!(
            capped[0].max_turns, 1,
            "a subagent that cannot act is not one"
        );

        let many: Vec<Value> = (0..MOST_SUBAGENTS + 5)
            .map(|i| json!({ "task": format!("task {i}") }))
            .collect();
        let sent = errands(&json!({ "tasks": many }), &served()).unwrap();
        assert_eq!(sent.len(), MOST_SUBAGENTS);
    }

    /// What the King is really asking for, end to end through the arguments:
    /// two errands, one on another model, both of which will hold a browser.
    #[test]
    fn a_fan_out_can_mix_models() {
        let sent = errands(
            &json!({
                "tasks": [
                    { "task": "Read how the form validates" },
                    {
                        "task": "Open http://127.0.0.1:3000 and check the form rejects a bad email",
                        "model": "copilot/claude-opus-5"
                    }
                ]
            }),
            &served(),
        )
        .unwrap();

        assert_eq!(sent.len(), 2);
        assert!(sent[0].choice.is_none());
        assert_eq!(
            sent[1].choice.as_ref().map(|c| c.model.as_str()),
            Some("copilot/claude-opus-5")
        );
        assert_eq!(
            SUBAGENT_PERMISSIONS,
            Permissions::Browse,
            "both of them hold a browser -- that is not a per-task choice"
        );
    }
}
