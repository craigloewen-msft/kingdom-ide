//! A deterministic, offline model.
//!
//! This exists so the whole drafting path -- busy mark, draft, transcript, status
//! transitions -- can be exercised without a credential, a network, or a bill.
//! It is an ordinary entry in the picker: a provider serving one model, chosen
//! the same way any other model is chosen. Being the only one that can never
//! fail to be available is what makes it the fallback, and that falls out of
//! the list rather than being written into it.
//!
//! Determinism is the entire point, so there is no randomness and no clock
//! here. The scenario is chosen from a byte sum of the prompt, and can be
//! pinned outright with a `[[scenario:NAME]]` marker, which is what makes
//! end-to-end flows authorable: a test can demand the blocked case rather than
//! hoping for it.

use super::{
    Act, Acts, Answer, Brief, Draft, Model, ModelError, Provider, ProviderCatalogue, Reply,
};
use kingdom_core::{CredentialState, ModelChoice, ModelOption, Speaker, Turn};

/// The namespaced id, which for a single-model provider is just its namespace.
pub const MODEL_ID: &str = "mock";

#[derive(Debug, Default)]
pub struct MockProvider;

#[derive(Debug, Default)]
pub struct MockModel;

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn namespace(&self) -> &'static str {
        MODEL_ID
    }

    /// One model, always available, needing nothing.
    ///
    /// Not `recommended`, so it sinks below real models once any are on offer --
    /// but when a credential is missing it is the only entry, and therefore
    /// still what the user lands on.
    async fn catalogue(&self) -> ProviderCatalogue {
        ProviderCatalogue {
            options: vec![ModelOption {
                id: MODEL_ID.to_string(),
                label: "Mock (offline)".to_string(),
                vendor: "Offline".to_string(),
                context_window: 0,
                recommended: false,
                efforts: Vec::new(),
                // The mock drives the whole tool loop offline. That is the
                // point of it: without a model that can act and needs no
                // credential, the loop is only testable against a paid gateway.
                can_act: true,
                // Claimed for the same reason: `read_image` must be reachable
                // without a credential, or the only way to exercise the image
                // path is against a real gateway. The mock discards what it is
                // shown -- it has no eyes -- but the tool runs and the picture
                // travels, which is the part worth testing offline.
                can_see: true,
                // No wire, so no budget to declare. `None` exercises the
                // fallback path, which is the one every model that declines to
                // publish a limit takes.
                max_output_tokens: None,
            }],
            credential: CredentialState::Ready,
            detail: "The offline mock needs no credential.".to_string(),
        }
    }

    async fn open(&self, _choice: &ModelChoice) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(MockModel))
    }
}

/// The shapes of reply the UI needs to be able to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Describes the project. The common case.
    Survey,
    /// Proposes concrete changes to specific files.
    Plan,
    /// Declines the work.
    Refuse,
    /// Takes visible time, so the drafting state is observable.
    Slow,
    /// Fails, so the failure path is reachable on demand.
    Error,
    /// Asks the user a question, then answers from what he chose.
    ///
    /// The offline rehearsal for the one thing request/response could never do:
    /// the server speaking first, and a tool call that parks until a person
    /// replies.
    Ask,
    /// Uses a tool, then answers from its result.
    ///
    /// The offline rehearsal for the whole turn loop: a first call that asks to
    /// act and a second that speaks, decided by what it finds in the
    /// transcript. Without this the loop could only be exercised against a real
    /// gateway, which is exactly the dependency the mock exists to remove.
    Work,
    /// Sends two subagents, then answers from what they report.
    ///
    /// The offline rehearsal for the fan-out: two sub-agents running at once,
    /// each a real plan with its own conversation. It exercises the part that
    /// has no other test -- subagents being created, drafted concurrently and
    /// collected -- and it is the only way to see the parent's subagent rows go
    /// live without a credential.
    Subagents,
    /// Proposes a plan, then carries it out once the user accepts.
    ///
    /// The offline rehearsal for the whole point of the product: a model that
    /// draws something up and stops, a user who reads it and says start, and
    /// the same conversation continuing with tools it did not have before.
    /// Without this the approval path could only be exercised against a real
    /// gateway, which is exactly the dependency the mock exists to remove --
    /// and this is now the most important path there is to rehearse.
    Propose,
}

impl Scenario {
    /// Picks a scenario for a prompt: an explicit marker if present, otherwise a
    /// stable hash so the same prompt always produces the same draft.
    pub fn for_prompt(prompt: &str) -> Scenario {
        if let Some(named) = Scenario::from_marker(prompt) {
            return named;
        }
        // Sum of bytes, not DefaultHasher: the latter is explicitly not stable
        // across Rust releases, which would break determinism silently.
        let sum: usize = prompt.trim().bytes().map(usize::from).sum();
        match sum % 3 {
            0 => Scenario::Survey,
            1 => Scenario::Plan,
            _ => Scenario::Survey,
        }
    }

    fn from_marker(prompt: &str) -> Option<Scenario> {
        let start = prompt.find("[[scenario:")? + "[[scenario:".len();
        let rest = &prompt[start..];
        let end = rest.find("]]")?;
        match rest[..end].trim().to_ascii_lowercase().as_str() {
            "survey" => Some(Scenario::Survey),
            "plan" => Some(Scenario::Plan),
            "refuse" | "blocked" => Some(Scenario::Refuse),
            "slow" => Some(Scenario::Slow),
            "error" => Some(Scenario::Error),
            "work" | "tools" => Some(Scenario::Work),
            "errand" | "errands" => Some(Scenario::Subagents),
            "ask" => Some(Scenario::Ask),
            "propose" | "proposal" => Some(Scenario::Propose),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl Model for MockModel {
    fn can_see(&self) -> bool {
        true
    }

    /// Always untallied: the mock has no gateway to count anything, and
    /// inventing a number would put a bar on screen measuring nothing. It
    /// declares no context window either, so there would be nothing to measure
    /// it against.
    async fn take_turn(&self, brief: &Brief) -> Result<Answer, ModelError> {
        self.reply_to(brief).await.map(Answer::untallied)
    }

    fn id(&self) -> &str {
        MODEL_ID
    }
}

impl MockModel {
    /// The scenario's reply, before it is wrapped with what it cost.
    ///
    /// Split out only so the cost is added in one place rather than at every
    /// `Ok` in the match below.
    async fn reply_to(&self, brief: &Brief) -> Result<Reply, ModelError> {
        let prompt = latest_prompt(brief);
        // A conversation that has put a plan to the user and not been approved
        // is still *in* the proposing conversation, whatever their latest words
        // hash to. Without this the mock falls out of the scenario the moment
        // they ask for a change -- and the revision loop, which is half of what
        // this scenario exists to rehearse, could never be exercised offline.
        let scenario = if proposed_already(brief) && !approved(brief) {
            Scenario::Propose
        } else {
            Scenario::for_prompt(&prompt)
        };

        if scenario == Scenario::Slow {
            // Long enough for the drafting state to be visible on the map, short
            // enough not to make testing tedious.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        let city = &brief.system_prompt.city;

        match scenario {
            Scenario::Error => Err(ModelError::Transport(
                "the mock model was asked to fail (scenario: error)".to_string(),
            )),

            Scenario::Refuse => Err(ModelError::Refused(format!(
                "This decree cannot be drafted for {}: the mock model was asked to refuse \
                 (scenario: refuse).",
                city.name
            ))),

            Scenario::Ask => Ok(match done_already(brief) {
                None => Reply::Acts(
                    Acts::plain(vec![Act {
                        id: "mock-question-1".to_string(),
                        tool: "ask_user_question".to_string(),
                        input: serde_json::json!({
                            "questions": [{
                                "question": format!(
                                    "There are two ways to go about {}. Which do you want?",
                                    city.name
                                ),
                                "header": "Approach",
                                "options": [
                                    {
                                        "label": "The careful way",
                                        "description": "Read everything first, change one thing, \
                                                        and check it before going further."
                                    },
                                    {
                                        "label": "The quick way",
                                        "description": "Make the obvious change and see whether \
                                                        it builds."
                                    }
                                ]
                            }]
                        }),
                    }])
                    // A remark above a question the King has to answer: the
                    // sentence that says why he is being asked, which is the
                    // moment it is worth the most.
                    .saying(format!(
                        "Before I go further on {}, there's a fork here I'd rather not \
                         guess at.",
                        city.name
                    )),
                ),
                Some(chosen) => Reply::Spoke(Draft {
                    summary: format!("Asked the King, and he chose: {}", chosen.trim()),
                    body: format!(
                        "You chose \"{}\", so that is how I would proceed on {}.\n\n\
                         (Drafted by the mock model \u{2014} no real work was done.)",
                        chosen.trim(),
                        city.name
                    ),
                }),
            }),

            // Act on the first pass, speak on the second. Which pass this is
            // comes from the transcript rather than from any state held here:
            // the model is asked afresh each time, so a mock that remembered
            // would be rehearsing something the real loop does not do.
            Scenario::Work => Ok(match done_already(brief) {
                None => Reply::Acts(
                    Acts::plain(vec![Act {
                        id: "mock-call-1".to_string(),
                        tool: "think".to_string(),
                        input: serde_json::json!({
                            "thoughts": format!(
                                "The decree is {prompt:?}. {} is a {} project of {} files, so I \
                                 should start by reading what is already there.",
                                city.name, city.stack, city.file_count
                            )
                        }),
                    }])
                    // The words a real reply carries alongside its calls. Here
                    // so the chamber's remark is reachable offline: without a
                    // provider saying anything, the one state that view exists
                    // to draw could not be seen in a proving ground at all.
                    .saying(format!(
                        "I'll start by working out the shape of {} before I touch anything \
                         \u{2014} it's a {} project, and I don't yet know what calls what.",
                        city.name, city.stack
                    ))
                    .thinking(format!(
                        "The decree is {prompt:?}.\n\
                         Two ways at this: read the entry point and follow it outwards, or \
                         search for the names in the decree and work back from there.\n\
                         {} has {} files, which is small enough that following the entry \
                         point costs little, and it gives me the call graph rather than a \
                         pile of matches.\n\
                         Starting there.",
                        city.name, city.file_count
                    )),
                ),
                Some(result) => Reply::Spoke(Draft {
                    summary: format!("Used a tool, then reported on {}.", city.name),
                    body: format!(
                        "On the decree \"{}\":\n\n\
                         I thought it through first, and concluded:\n\n  {}\n\n\
                         (Drafted by the mock model \u{2014} no real work was done.)",
                        prompt.trim(),
                        result.trim()
                    ),
                }),
            }),

            // Send on the first pass, speak on the second -- the same shape as
            // `Work`, and read from the transcript for the same reason.
            //
            // The subagents themselves are drafted by this same mock: their
            // task text hashes to an ordinary speaking scenario, which is
            // exactly the behaviour being rehearsed.
            Scenario::Subagents => Ok(match done_already(brief) {
                None => Reply::Acts(
                    Acts::plain(vec![Act {
                        id: "mock-errand-1".to_string(),
                        tool: "spawn_agents".to_string(),
                        input: serde_json::json!({
                            "tasks": [
                                {
                                    "task": format!(
                                        "Survey the shape of {}: what kind of project is \
                                         it, and what are its largest parts?",
                                        city.name
                                    )
                                },
                                {
                                    "task": format!(
                                        "Look for anything in {} that looks unfinished \
                                         or inconsistent, and say where it is.",
                                        city.name
                                    )
                                }
                            ]
                        }),
                    }])
                    // A remark on a call the chamber draws as something other
                    // than a deed line -- which is the case the view's grouping
                    // has to get right, and the one nothing else rehearses.
                    .saying(format!(
                        "Two questions about {} that don't depend on each other, so I'll \
                         send them out together rather than answer them in turn.",
                        city.name
                    )),
                ),
                Some(reports) => Reply::Spoke(Draft {
                    summary: format!("Sent two errands into {} and read them back.", city.name),
                    body: format!(
                        "On the decree \"{}\":\n\n\
                         I sent two errands to look at {} in parallel. They \
                         reported:\n\n{}\n\n\
                         (Drafted by the mock model \u{2014} no real work was done.)",
                        prompt.trim(),
                        city.name,
                        reports.trim()
                    ),
                }),
            }),

            Scenario::Plan | Scenario::Slow => {
                // Still named in the reply *body*: that is chat output, and it
                // is what makes the mock a useful rehearsal. What it no longer
                // does is hand back a structured file list as though a model's
                // guess were fact.
                let named: Vec<&String> = city.notable_paths.iter().take(3).collect();
                let listed = if named.is_empty() {
                    "  (no files were scanned in this project)\n".to_string()
                } else {
                    named
                        .iter()
                        .map(|p| format!("  - {p}\n"))
                        .collect::<String>()
                };
                Ok(Reply::Spoke(Draft {
                    summary: format!(
                        "Proposes changes to {} file(s) in {}.",
                        named.len(),
                        city.name
                    ),
                    body: format!(
                        "On the decree \"{}\":\n\n\
                         {} is a {} project of {} files. I would begin here:\n\n{}\n\
                         (Drafted by the mock model — no real work was done.)",
                        prompt.trim(),
                        city.name,
                        city.stack,
                        city.file_count,
                        listed
                    ),
                }))
            }

            // Propose on the first pass; once the user has accepted, act and
            // then speak. Which stage this is comes entirely from the
            // transcript, for the same reason as `Work` -- and here it matters
            // more, because the real thing genuinely spans two separate turns
            // with a human decision between them.
            //
            // Two calls, in the order the real flow takes them: the plan is
            // written to the draft and then proposed by path. The mock is the
            // offline demonstration of the flow, so a mock that proposed out of
            // thin air would show a workflow that no longer exists -- and would
            // be refused, since `propose_plan` reads the file.
            Scenario::Propose if !approved(brief) => {
                // A revision quotes what they asked for, so the user can see their
                // feedback was actually read rather than a fresh plan appearing
                // that happens to look the same.
                let revising = revision_note(brief);
                let nth = proposals_so_far(brief) + 1;
                let draft = crate::tools::propose_plan::DRAFT;
                let body = format!(
                    "# Tidy the edges of {}\n\n\
                     {revising}On the decree \"{}\":\n\n\
                     ## What I would do\n\n\
                     {} is a {} project of {} files. I would work through it in \
                     three steps, checking after each.\n\n\
                     ## The changes\n\n\
                     1. Read the entry point and map what calls what.\n\
                     2. Make the change itself, in one place.\n\
                     3. Run the tests and report what moved.\n\n\
                     ## What I am assuming\n\n\
                     That the tests currently pass. I have not run them.\n\n\
                     (Drafted by the mock model \u{2014} no real work was done.)",
                    city.name,
                    prompt.trim(),
                    city.name,
                    city.stack,
                    city.file_count
                );

                Ok(Reply::Acts(
                    Acts::plain(vec![
                        Act {
                            id: format!("mock-draft-{nth}"),
                            tool: "patch".to_string(),
                            input: serde_json::json!({
                                "path": draft,
                                "patches": [{ "operation": "overwrite", "newText": body }]
                            }),
                        },
                        Act {
                            id: format!("mock-proposal-{nth}"),
                            tool: "propose_plan".to_string(),
                            input: serde_json::json!({ "draft": draft }),
                        },
                    ])
                    // Two calls, one reply, one remark. The chamber must draw
                    // this sentence above the first deed and not above both,
                    // which is the whole of what `remark` is guarding.
                    .saying(
                        "I'll write the plan down and then put it to you \u{2014} it's easier \
                         to argue with on the page than in a paragraph.",
                    ),
                ))
            }

            Scenario::Propose => Ok(match done_since_approval(brief) {
                // Approved, and nothing done since: the model now has tools it
                // did not have a moment ago, so it uses one.
                None => Reply::Acts(Acts::plain(vec![Act {
                    id: "mock-approved-1".to_string(),
                    tool: "think".to_string(),
                    input: serde_json::json!({
                        "thoughts": format!(
                            "The user approved the plan for {}. Step one was to read the \
                             entry point, so that is where I start.",
                            city.name
                        )
                    }),
                }])),
                Some(result) => Reply::Spoke(Draft {
                    summary: format!("Carried out the approved plan for {}.", city.name),
                    body: format!(
                        "The plan is done.\n\n\
                         I began as proposed, and concluded:\n\n  {}\n\n\
                         (Drafted by the mock model \u{2014} no real work was done.)",
                        result.trim()
                    ),
                }),
            }),

            Scenario::Survey => Ok(Reply::Spoke(Draft {
                summary: format!("A reading of {} as it stands.", city.name),
                body: format!(
                    "On the decree \"{}\":\n\n\
                     {} sits at {}. It is a {} project of {} files, {}.\n\n\
                     (Drafted by the mock model — no real work was done.)",
                    prompt.trim(),
                    city.name,
                    city.path,
                    city.stack,
                    city.file_count,
                    if city.has_git {
                        format!("under git with {} uncommitted file(s)", city.dirty_files)
                    } else {
                        "not under git".to_string()
                    }
                ),
            })),
        }
    }
}

/// The last thing the user *asked for*, which is what a scenario is chosen from.
///
/// Read out of the transcript rather than handed over separately, because after
/// a tool call the most recent turn is a tool call, not a prompt -- and the
/// scenario must stay the one the user asked for across every pass of the loop,
/// or the mock would change its mind halfway through its own rehearsal.
///
/// Accepting a proposal is skipped for exactly that reason. It is recorded as
/// the user speaking (see [`kingdom_core::APPROVAL`]) because that is the
/// honest shape and it needs no provider to learn a new message type -- but it
/// is Kingdom's phrasing of a click rather than a prompt, and treating it as
/// one would have every approved plan re-roll its scenario from a sentence the
/// user never typed.
fn latest_prompt(brief: &Brief) -> String {
    brief
        .turns
        .iter()
        .rev()
        .find_map(|t| match t {
            Turn::Message(u) if u.speaker == Speaker::User && u.body != kingdom_core::APPROVAL => {
                Some(u.body.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// True once the model has put a plan to the user in this conversation.
fn proposed_already(brief: &Brief) -> bool {
    proposals_so_far(brief) > 0
}

/// How many plans the model has put to the user so far.
///
/// Also what gives each `propose_plan` call a distinct id. Reusing one id
/// across a revision would leave the second call unable to settle -- the loop
/// looks for a deed *still in flight* under that id, and the first one is long
/// since answered.
fn proposals_so_far(brief: &Brief) -> usize {
    brief
        .turns
        .iter()
        .filter(|t| matches!(t, Turn::Tool(d) if d.tool == "propose_plan"))
        .count()
}

/// A line acknowledging the user's notes, for a revised proposal.
///
/// Empty on a first proposal. The mock cannot actually revise anything, but it
/// can show the shape a real revision takes -- and the user seeing their own
/// words quoted back is the difference between a rehearsal of the feedback loop
/// and a plan that merely reappeared.
fn revision_note(brief: &Brief) -> String {
    if !proposed_already(brief) {
        return String::new();
    }
    match latest_prompt(brief) {
        notes if notes.is_empty() => String::new(),
        notes => format!("Revised, having read: \"{}\".\n\n", notes.trim()),
    }
}

/// True once the user has accepted a proposal in this conversation.
///
/// The mock's stand-in for the real signal the loop uses (`Plan::remit`), which
/// a model is deliberately never handed: what a plan may do is Kingdom's
/// business, not something a provider gets to read. The transcript is what a
/// model actually sees, so this is what a model could actually know.
fn approved(brief: &Brief) -> bool {
    approval_at(brief).is_some()
}

/// Where the user's acceptance sits in the exchange, if they have given one.
fn approval_at(brief: &Brief) -> Option<usize> {
    brief.turns.iter().position(|t| {
        matches!(t, Turn::Message(u)
            if u.speaker == Speaker::User && u.body == kingdom_core::APPROVAL)
    })
}

/// The result of a tool call made *after* the user accepted the plan.
///
/// Deliberately not [`done_already`], and the difference is the whole reason
/// this exists. The `propose_plan` call is itself a settled deed sitting in the
/// transcript, so "has a tool been used?" is always true once a proposal has
/// been made -- and the mock would skip straight from approval to a closing
/// speech, rehearsing none of the work it had just been given permission to do.
fn done_since_approval(brief: &Brief) -> Option<String> {
    let at = approval_at(brief)?;
    brief.turns.iter().skip(at).rev().find_map(|t| match t {
        Turn::Tool(d) if !d.in_flight() => Some(d.report().to_string()),
        _ => None,
    })
}

/// The result of a tool call already made, if there is one.
fn done_already(brief: &Brief) -> Option<String> {
    brief.turns.iter().rev().find_map(|t| match t {
        Turn::Tool(d) if !d.in_flight() => Some(d.report().to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::CityBrief;

    fn brief(prompt: &str) -> Brief {
        Brief {
            system_prompt: crate::llm::SystemPrompt {
                city: CityBrief {
                    name: "Testburg".into(),
                    path: "/dev/testburg".into(),
                    stack: "Rust".into(),
                    file_count: 12,
                    has_git: true,
                    dirty_files: 2,
                    notable_paths: vec!["src/lib.rs".into(), "src/main.rs".into()],
                },
                ..Default::default()
            },
            turns: vec![Turn::Message(kingdom_core::Message::new(
                Speaker::User,
                prompt,
            ))],
            aside: None,
            tools: Vec::new(),
            budget: crate::llm::Budget::FULL,
        }
    }

    fn spoken(answer: Answer) -> Draft {
        match answer.reply {
            Reply::Spoke(d) => d,
            Reply::Acts(a) => panic!("expected words, got {} tool call(s)", a.len()),
        }
    }

    /// Being predictable is the mock's entire reason to exist: a test that
    /// drives the UI must get the same draft every run, and a marker must be
    /// able to demand a specific case rather than hoping the hash lands on it.
    #[tokio::test]
    async fn drafts_are_deterministic_and_markers_pin_the_scenario() {
        let model = MockModel;

        let first = spoken(
            model
                .take_turn(&brief("What is this project?"))
                .await
                .unwrap(),
        );
        let second = spoken(
            model
                .take_turn(&brief("What is this project?"))
                .await
                .unwrap(),
        );
        assert_eq!(first.body, second.body);

        let refused = model
            .take_turn(&brief("anything [[scenario:refuse]]"))
            .await;
        assert!(matches!(refused, Err(ModelError::Refused(_))));

        let failed = model.take_turn(&brief("anything [[scenario:error]]")).await;
        assert!(matches!(failed, Err(ModelError::Transport(_))));
    }

    /// The whole turn loop, offline. This is what the mock is *for* now: the
    /// loop is the hard part of the feature, and without a model that can act
    /// and needs no credential it could only be exercised against a paid
    /// gateway.
    ///
    /// The two passes must be driven by the transcript alone. A mock that
    /// remembered which pass it was on would rehearse something the real loop
    /// never does -- each call to a real model is stateless, and the whole
    /// conversation is rebuilt from the plan every time.
    #[tokio::test]
    async fn the_work_scenario_acts_first_and_speaks_second() {
        let model = MockModel;
        let mut brief = brief("Do some work [[scenario:work]]");

        let Reply::Acts(acts) = model.take_turn(&brief).await.unwrap().reply else {
            panic!("the first pass must ask to act, not speak");
        };
        assert_eq!(acts.len(), 1);
        assert_eq!(acts.calls[0].tool, "think");

        // Exactly what the loop does between passes: record the call, settle it
        // with a result, ask again.
        let mut tool_call = kingdom_core::ToolCall::started(
            acts.calls[0].id.clone(),
            acts.calls[0].tool.clone(),
            acts.calls[0].input.clone(),
        );
        tool_call.outcome = Some(kingdom_core::ToolOutcome::done("a conclusion was reached"));
        brief.turns.push(Turn::Tool(tool_call));

        let draft = spoken(model.take_turn(&brief).await.unwrap());
        assert!(
            draft.body.contains("a conclusion was reached"),
            "the second pass must answer *from* the tool's result, not ignore it: {}",
            draft.body
        );
    }
}
