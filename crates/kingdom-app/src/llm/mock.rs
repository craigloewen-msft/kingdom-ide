//! A deterministic, offline model.
//!
//! This exists so the whole drafting path -- lease, draft, transcript, status
//! transitions -- can be exercised without a credential, a network, or a bill.
//! It is the default provider precisely so a fresh clone works immediately.
//!
//! Determinism is the entire point, so there is no randomness and no clock
//! here. The scenario is chosen from a byte sum of the prompt, and can be
//! pinned outright with a `[[scenario:NAME]]` marker, which is what makes
//! end-to-end flows authorable: a test can demand the blocked case rather than
//! hoping for it.

use super::{Brief, Draft, Model, ModelError};

pub const MODEL_NAME: &str = "mock";

#[derive(Debug, Default)]
pub struct MockModel;

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
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl Model for MockModel {
    async fn draft(&self, brief: &Brief) -> Result<Draft, ModelError> {
        let scenario = Scenario::for_prompt(&brief.prompt);

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

            Scenario::Plan | Scenario::Slow => {
                let touches: Vec<String> = city.notable_paths.iter().take(3).cloned().collect();
                let listed = if touches.is_empty() {
                    "  (no files were scanned in this project)\n".to_string()
                } else {
                    touches
                        .iter()
                        .map(|p| format!("  - {p}\n"))
                        .collect::<String>()
                };
                Ok(Draft {
                    title: format!("Works upon {}", city.name),
                    summary: format!(
                        "Proposes changes to {} file(s) in {}.",
                        touches.len(),
                        city.name
                    ),
                    touches,
                    body: format!(
                        "On the decree \"{}\":\n\n\
                         {} is a {} project of {} files. I would begin here:\n\n{}\n\
                         (Drafted by the mock model — no real work was done.)",
                        brief.prompt.trim(),
                        city.name,
                        city.stack,
                        city.file_count,
                        listed
                    ),
                })
            }

            Scenario::Survey => Ok(Draft {
                title: format!("Survey of {}", city.name),
                summary: format!("A reading of {} as it stands.", city.name),
                touches: Vec::new(),
                body: format!(
                    "On the decree \"{}\":\n\n\
                     {} sits at {}. It is a {} project of {} files, {}.\n\n\
                     (Drafted by the mock model — no real work was done.)",
                    brief.prompt.trim(),
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
            }),
        }
    }

    fn name(&self) -> &str {
        MODEL_NAME
    }
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
            transcript: Vec::new(),
            prompt: prompt.to_string(),
        }
    }

    /// Being predictable is the mock's entire reason to exist: a test that
    /// drives the UI must get the same draft every run, and a marker must be
    /// able to demand a specific case rather than hoping the hash lands on it.
    #[tokio::test]
    async fn drafts_are_deterministic_and_markers_pin_the_scenario() {
        let model = MockModel;

        let first = model.draft(&brief("What is this project?")).await.unwrap();
        let second = model.draft(&brief("What is this project?")).await.unwrap();
        assert_eq!(first.body, second.body);
        assert_eq!(first.title, second.title);

        let refused = model.draft(&brief("anything [[scenario:refuse]]")).await;
        assert!(matches!(refused, Err(ModelError::Refused(_))));

        let failed = model.draft(&brief("anything [[scenario:error]]")).await;
        assert!(matches!(failed, Err(ModelError::Transport(_))));
    }
}
