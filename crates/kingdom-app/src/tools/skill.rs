//! Invoking a skill: the model fetching instructions a project left for it.
//!
//! The catalogue in the system prompt is metadata only -- a name and a
//! description per skill. This is how the body is actually collected, and
//! keeping the two apart is the whole point: a project with twenty skills costs
//! twenty lines of every prompt, and only the one that gets invoked costs its
//! full length.
//!
//! Sits with the reads at every permission level. Invoking a skill returns text
//! and changes nothing -- whether the *instructions* can then be carried out is
//! decided by the tools the plan holds, which is the boundary that already
//! exists.

use super::{Refusal, Sandbox, Tool};
use kingdom_core::ToolOutcome;
use serde_json::{json, Value};

pub struct Skill;

#[async_trait::async_trait]
impl Tool for Skill {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> String {
        "Invoke a skill by name. Skills are project-specific or user-level \
         capabilities discovered from .claude/skills/ and .agents/skills/ \
         directories. Use this when a skill would help accomplish the current task."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["skill_name"],
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Name of the skill to invoke (e.g., 'build', 'lint', 'deploy')"
                },
                "args": {
                    "type": "string",
                    "description": "Optional arguments to pass to the skill"
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let name = input
            .get("skill_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();

        if name.is_empty() {
            return Refusal::BadArguments {
                tool: self.name().to_string(),
                detail: "no `skill_name` was given, so there was nothing to invoke".to_string(),
            }
            .into();
        }

        let args = input
            .get("args")
            .and_then(Value::as_str)
            .unwrap_or_default();

        // Discovered fresh rather than carried on the sandbox: a skill added
        // while a plan is running should be invocable without restarting the
        // server, and the walk is a handful of `read_dir` calls.
        let skills = crate::skills::discover(shop.root());

        match crate::skills::invoke(name, args, &skills) {
            Ok(invocation) => ToolOutcome::done(invocation.body),
            Err(why) => Refusal::Refused(why).into(),
        }
    }
}
