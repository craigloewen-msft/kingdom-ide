//! Drafting plans with a model.
//!
//! Server-only: none of this compiles into the wasm bundle. It is deliberately
//! named for what it is (`llm`) rather than given a metaphor noun -- the
//! metaphor is carried by [`kingdom_core::Plan`], and inventing a second name
//! for the plumbing would only obscure where the HTTP call lives.

pub mod catalogue;
pub mod copilot;
pub mod credential;
pub mod mock;

use kingdom_core::{
    City, CredentialState, ModelChoice, ModelProvider, ModelStatus, Utterance, Workspace,
};

/// Everything a model is told about the work.
///
/// The city context is the point: without it this is a generic chat box, and
/// with it the King is talking to something that knows which project he means.
#[derive(Debug, Clone)]
pub struct Brief {
    pub city: CityBrief,
    /// What has actually been *said*, oldest first, excluding `prompt`.
    ///
    /// [`Utterance`] rather than `kingdom_core::Entry` on purpose: Kingdom's own
    /// notices -- a failed call or a workspace event -- are not words anybody spoke,
    /// and replaying them as the model's own prior turns would teach it to
    /// answer in the voice of the plumbing. The type is what prevents that;
    /// see `Plan::said`.
    pub transcript: Vec<Utterance>,
    /// The message being answered now.
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
    /// Builds a brief from a scanned city and the workspace the plan works in.
    ///
    /// The path is the *workspace's*, not the project's: a plan drafting against
    /// an isolated worktree must be told where it is actually working, or every
    /// file it names is a file in somebody else's checkout.
    pub fn from_city(city: &City, workspace: &Workspace) -> Self {
        Self {
            name: city.name.clone(),
            path: workspace.path.clone(),
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

/// Which provider the environment names as the opening default. The King's own
/// choice, once made, is carried on the plan and overrides this entirely.
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

/// The namespaced model id the picker opens on before the King has chosen.
///
/// Defaults to the offline mock so a fresh clone drafts with no credential and
/// no network, exactly as before this became a choice.
pub fn default_model_id() -> String {
    match provider() {
        ModelProvider::Mock => mock::MODEL_NAME.to_string(),
        ModelProvider::Copilot => {
            let name = std::env::var("KINGDOM_MODEL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| copilot::DEFAULT_MODEL.to_string());
            format!("copilot/{name}")
        }
    }
}

/// Builds the model a choice names.
///
/// The provider comes off the choice's own id rather than the environment, so
/// the model that drafts is always the one the plan says drafted it.
pub async fn configured(choice: &ModelChoice) -> Result<Box<dyn Model>, ModelError> {
    match choice.provider() {
        ModelProvider::Mock => Ok(Box::new(mock::MockModel)),
        ModelProvider::Copilot => {
            let cred = credential::resolve(Some(credential::DEFAULT_COPILOT_HELPER)).await?;
            Ok(Box::new(copilot::CopilotModel::new(
                cred.token,
                choice.api_name(),
                choice.effort,
            )))
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
            detail: "Offline mock. Choose a Copilot model in the picker, or set \
                     KINGDOM_MODEL_PROVIDER=copilot to open on one."
                .to_string(),
        },
        ModelProvider::Copilot => {
            let model = default_model_id();
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
