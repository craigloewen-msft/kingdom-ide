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

use kingdom_core::{City, CredentialState, ModelChoice, ModelOption, Utterance, Workspace};

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

    /// The namespaced id, recorded on the plan so the King can see exactly what
    /// drew it. Namespaced rather than bare because that is what routes the
    /// *next* turn back to the same backend.
    fn id(&self) -> &str;
}

/// A backend that serves models.
///
/// One implementation per backend, and no backend is privileged: the offline
/// mock is a provider that happens to serve exactly one model and need no
/// credential. That is the whole point -- "mock" is a model you can choose, not
/// a mode the application is in.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// The id namespace this provider owns: `"mock"`, `"copilot"`. Every model
    /// it serves has an id in this namespace, which is how a choice finds its
    /// way home.
    fn namespace(&self) -> &'static str;

    /// Every model this provider will actually serve *right now*, and why the
    /// list might be shorter than expected.
    ///
    /// Deliberately not a `Result`: a provider that cannot reach its gateway
    /// reports that in its own `detail` and yields nothing, so one broken
    /// backend leaves the picker thinner rather than empty.
    async fn catalogue(&self) -> ProviderCatalogue;

    /// Builds the model a choice names. The choice is already known to be in
    /// this provider's namespace.
    async fn open(&self, choice: &ModelChoice) -> Result<Box<dyn Model>, ModelError>;
}

/// One provider reporting on itself: what it can serve, and what state its
/// credential is in.
#[derive(Debug, Clone)]
pub struct ProviderCatalogue {
    pub options: Vec<ModelOption>,
    pub credential: CredentialState,
    /// Plain-language detail: where the credential came from, or what to set to
    /// fix it. Safe to display; never the credential itself.
    pub detail: String,
}

/// Every backend Kingdom knows how to draft with.
///
/// A plain list rather than a registry with registration macros: two providers
/// do not need a plugin system, and a literal is the version of this a reader
/// can take in at a glance. Order decides the picker's order within equal
/// recommendation, so the mock sits last.
pub fn providers() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(copilot::CopilotProvider),
        Box::new(mock::MockProvider),
    ]
}

/// The namespaced model id the picker opens on before the King has chosen,
/// when the environment names one.
///
/// `None` means "take the best the catalogue offers" -- resolved in
/// [`catalogue`], which is the only place the available options are known.
pub fn preferred_model_id() -> Option<String> {
    std::env::var("KINGDOM_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Builds the model a choice names, by handing it to the provider that owns its
/// namespace.
///
/// An unknown namespace is an error rather than a quiet fall back to the mock:
/// a typo'd id that silently drafts fake work is the one failure worth being
/// loud about, because the reply *looks* like an answer.
pub async fn open(choice: &ModelChoice) -> Result<Box<dyn Model>, ModelError> {
    let namespace = choice.namespace();

    match providers().into_iter().find(|p| p.namespace() == namespace) {
        Some(provider) => provider.open(choice).await,
        None => Err(ModelError::Refused(format!(
            "No provider serves \"{}\": nothing here answers to \"{namespace}\".",
            choice.model
        ))),
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
