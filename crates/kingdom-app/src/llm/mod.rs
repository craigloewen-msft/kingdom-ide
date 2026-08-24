//! Drafting plans with a model.
//!
//! Server-only: none of this compiles into the wasm bundle. It is deliberately
//! named for what it is (`llm`) rather than given a metaphor noun -- the
//! metaphor is carried by [`kingdom_core::Plan`], and inventing a second name
//! for the plumbing would only obscure where the HTTP call lives.

pub mod broker;
pub mod copilot;
pub mod credential;
pub mod mock;

use kingdom_core::{City, CredentialState, ModelProvider, ModelStatus, Utterance};

/// Everything a model is told about the work.
///
/// The city context is the point: without it this is a generic chat box, and
/// with it the King is talking to something that knows which project he means.
#[derive(Debug, Clone)]
pub struct Brief {
    pub city: CityBrief,
    /// The conversation so far, oldest first, excluding `prompt`.
    pub transcript: Vec<Utterance>,
    /// The decree being answered now.
    pub prompt: String,
}

/// The facts about a project worth spending tokens on.
#[derive(Debug, Clone)]
pub struct CityBrief {
    pub name: String,
    /// Absolute path on the host machine.
    pub path: String,
    pub stack: String,
    pub file_count: usize,
    pub has_git: bool,
    pub dirty_files: usize,
    /// A handful of real paths from the city, so the model can name actual
    /// files rather than inventing plausible ones.
    pub notable_paths: Vec<String>,
}

impl CityBrief {
    /// Builds a brief from a scanned city and the kingdom root it sits under.
    pub fn from_city(city: &City, kingdom_root: &str) -> Self {
        Self {
            name: city.name.clone(),
            path: format!("{}/{}", kingdom_root.trim_end_matches('/'), city.path),
            stack: city.kind.label().to_string(),
            file_count: city.file_count,
            has_git: city.has_git,
            dirty_files: city.dirty_files,
            notable_paths: notable_paths(city, 40),
        }
    }

    /// Renders the brief as the system prompt's project section.
    pub fn render(&self) -> String {
        let mut out = format!(
            "Project: {}\nAbsolute path: {}\nStack: {}\nFiles: {}\n",
            self.name, self.path, self.stack, self.file_count
        );
        if self.has_git {
            out.push_str(&format!(
                "Git: yes ({} uncommitted file(s))\n",
                self.dirty_files
            ));
        } else {
            out.push_str("Git: no\n");
        }
        if !self.notable_paths.is_empty() {
            out.push_str("\nSome files in this project:\n");
            for p in &self.notable_paths {
                out.push_str(&format!("  {p}\n"));
            }
        }
        out
    }
}

/// A drafted plan.
#[derive(Debug, Clone)]
pub struct Draft {
    /// Short headline for the sidebar.
    pub title: String,
    /// One or two lines, shown on hover.
    pub summary: String,
    /// Paths the plan proposes to touch, which light up on the map.
    pub touches: Vec<String>,
    /// The full reply, shown in the dock.
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no credential: {0}")]
    Credential(#[from] credential::CredentialError),
    #[error("could not reach the model: {0}")]
    Transport(String),
    #[error("{0}")]
    Refused(String),
}

#[async_trait::async_trait]
pub trait Model: Send + Sync {
    async fn draft(&self, brief: &Brief) -> Result<Draft, ModelError>;

    /// The model's name, recorded on the plan so the King can see what drew it.
    fn name(&self) -> &str;
}

/// Which provider is configured. Defaults to the mock, so a fresh clone drafts
/// plans offline with no setup at all.
pub fn provider() -> ModelProvider {
    match std::env::var("KINGDOM_MODEL_PROVIDER")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "copilot" => ModelProvider::Copilot,
        _ => ModelProvider::Mock,
    }
}

/// Builds the configured model.
pub async fn configured() -> Result<Box<dyn Model>, ModelError> {
    match provider() {
        ModelProvider::Mock => Ok(Box::new(mock::MockModel)),
        ModelProvider::Copilot => {
            let cred = credential::resolve(Some(credential::DEFAULT_COPILOT_HELPER)).await?;
            Ok(Box::new(copilot::CopilotModel::new(cred.token)))
        }
    }
}

/// Reports how plans will be drafted, for the dock's provider badge.
///
/// Resolves the credential to answer honestly, because "configured" and
/// "actually works" are different questions and only the second one matters to
/// the King. Returns a description only -- never the credential itself.
pub async fn status() -> ModelStatus {
    let provider = provider();

    match provider {
        ModelProvider::Mock => ModelStatus {
            provider,
            model: mock::MODEL_NAME.to_string(),
            credential: CredentialState::Ready,
            detail: "Offline mock. Set KINGDOM_MODEL_PROVIDER=copilot to draft with a real model."
                .to_string(),
        },
        ModelProvider::Copilot => {
            let model = copilot::model_name();
            match credential::resolve(Some(credential::DEFAULT_COPILOT_HELPER)).await {
                Ok(cred) => ModelStatus {
                    provider,
                    model,
                    credential: CredentialState::Ready,
                    detail: format!("Credential from {}.", cred.source.describe()),
                },
                Err(credential::CredentialError::NotConfigured) => ModelStatus {
                    provider,
                    model,
                    credential: CredentialState::Missing,
                    detail: "Set KINGDOM_API_KEY to a token, or KINGDOM_API_KEY_HELPER to a \
                             command that prints one."
                        .to_string(),
                },
                Err(e) => ModelStatus {
                    provider,
                    model,
                    credential: CredentialState::Failed,
                    detail: e.to_string(),
                },
            }
        }
    }
}

/// Real paths from a city, preferring source files, so a model can ground its
/// answer in files that exist.
fn notable_paths(city: &City, want: usize) -> Vec<String> {
    use kingdom_core::{District, Ward};

    fn walk(d: &District, out: &mut Vec<(bool, u64, String)>) {
        for b in &d.buildings {
            let is_source = matches!(
                b.ward,
                Ward::Rust | Ward::Web | Ward::Python | Ward::Go | Ward::Systems
            );
            out.push((is_source, b.bulk, b.path.clone()));
        }
        for child in &d.children {
            walk(child, out);
        }
    }

    let Some(structure) = &city.structure else {
        return Vec::new();
    };

    let mut found = Vec::new();
    walk(structure, &mut found);
    // Source first, then larger files, then alphabetical -- deterministic, so
    // the same project always produces the same brief.
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    found
        .into_iter()
        .take(want)
        .map(|(_, _, path)| path)
        .collect()
}
