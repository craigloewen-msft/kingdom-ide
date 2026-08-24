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

use super::{Act, Brief, Draft, Model, ModelError, Provider, ProviderCatalogue, Reply};
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
    /// still what the King lands on.
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
    /// Asks the King a question, then answers from what he chose.
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
    /// Sends two errands, then answers from what they report.
    ///
    /// The offline rehearsal for the fan-out: two sub-agents running at once,
    /// each a real plan with its own chamber. It exercises the part that has no
    /// other test -- errands being created, drafted concurrently and collected
    /// -- and it is the only way to see the parent's errand rows go live
    /// without a credential.
    Errand,
}

impl Scenario {
    /// Picks a scenario for a prompt: an explicit marker if present, otherwise a
    /// stable hash so the same decree always produces the same draft.
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
            "errand" | "errands" => Some(Scenario::Errand),
            "ask" => Some(Scenario::Ask),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl Model for MockModel {
    fn can_see(&self) -> bool {
        true
    }

    async fn take_turn(&self, brief: &Brief) -> Result<Reply, ModelError> {
        let prompt = latest_decree(brief);
        let scenario = Scenario::for_prompt(&prompt);

        if scenario == Scenario::Slow {
            // Long enough for the drafting state to be visible on the map, short
            // enough not to make testing tedious.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        let city = &brief.city;

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
                None => Reply::Acts(vec![Act {
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
                }]),
                Some(chosen) => Reply::Spoke(Draft {
                    title: format!("Counsel on {}", city.name),
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
                None => Reply::Acts(vec![Act {
                    id: "mock-call-1".to_string(),
                    tool: "think".to_string(),
                    input: serde_json::json!({
                        "thoughts": format!(
                            "The decree is {prompt:?}. {} is a {} project of {} files, so I \
                             should start by reading what is already there.",
                            city.name, city.stack, city.file_count
                        )
                    }),
                }]),
                Some(result) => Reply::Spoke(Draft {
                    title: format!("Works upon {}", city.name),
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
            // The errands themselves are drafted by this same mock: their task
            // text hashes to an ordinary speaking scenario, which is exactly
            // the behaviour being rehearsed.
            Scenario::Errand => Ok(match done_already(brief) {
                None => Reply::Acts(vec![Act {
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
                }]),
                Some(reports) => Reply::Spoke(Draft {
                    title: format!("Errands upon {}", city.name),
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
                    title: format!("Works upon {}", city.name),
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

            Scenario::Survey => Ok(Reply::Spoke(Draft {
                title: format!("Survey of {}", city.name),
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

    fn id(&self) -> &str {
        MODEL_ID
    }
}

/// The last thing the King said, which is what a scenario is chosen from.
///
/// Read out of the transcript rather than handed over separately, because after
/// a tool call the most recent turn is a deed, not a decree -- and the scenario
/// must stay the one the King asked for across every pass of the loop, or the
/// mock would change its mind halfway through its own rehearsal.
fn latest_decree(brief: &Brief) -> String {
    brief
        .turns
        .iter()
        .rev()
        .find_map(|t| match t {
            Turn::Said(u) if u.speaker == Speaker::King => Some(u.body.clone()),
            _ => None,
        })
        .unwrap_or_default()
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
            city: CityBrief {
                name: "Testburg".into(),
                path: "/dev/testburg".into(),
                stack: "Rust".into(),
                file_count: 12,
                has_git: true,
                dirty_files: 2,
                notable_paths: vec!["src/lib.rs".into(), "src/main.rs".into()],
            },
            turns: vec![Turn::Said(kingdom_core::Utterance::new(
                Speaker::King,
                prompt,
            ))],
            tools: Vec::new(),
        }
    }

    fn spoken(reply: Reply) -> Draft {
        match reply {
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
        assert_eq!(first.title, second.title);

        let refused = model.take_turn(&brief("anything [[scenario:refuse]]")).await;
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

        let Reply::Acts(acts) = model.take_turn(&brief).await.unwrap() else {
            panic!("the first pass must ask to act, not speak");
        };
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0].tool, "think");

        // Exactly what the loop does between passes: record the call, settle it
        // with a result, ask again.
        let mut tool_call = kingdom_core::ToolCall::started(
            acts[0].id.clone(),
            acts[0].tool.clone(),
            acts[0].input.clone(),
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
