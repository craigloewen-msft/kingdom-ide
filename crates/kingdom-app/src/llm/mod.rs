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
pub mod system_prompt;

use kingdom_core::{City, CredentialState, ModelChoice, ModelOption, Turn, Workspace};
pub use system_prompt::SystemPrompt;

/// Everything a model is told about the work.
///
/// The city context is the point: without it this is a generic chat box, and
/// with it the user is talking to something that knows which project he means.
#[derive(Debug, Clone)]
pub struct Brief {
    /// What the model is told before it is asked anything: the project, where
    /// it is standing, what it may touch, and the project's own guidance.
    ///
    /// Replaces the bare [`CityBrief`] this used to carry. The city is still in
    /// there -- see [`SystemPrompt::city`] -- but a provider now renders one
    /// assembled document rather than deciding for itself what a model should
    /// be told, which is a decision that belongs to Kingdom rather than to a
    /// gateway.
    pub system_prompt: SystemPrompt,
    /// The whole exchange so far, oldest first: what was said and what was
    /// done, interleaved exactly as it happened.
    ///
    /// [`Turn`] rather than `kingdom_core::Entry` on purpose: Kingdom's own
    /// notices -- a failed call or a workspace event -- are not part of the
    /// exchange, and replaying them as the model's own prior turns would teach
    /// it to answer in the voice of the plumbing. The type is what prevents
    /// that; see `Plan::turns`.
    ///
    /// Unlike the old shape this *includes* the message being answered. Once a
    /// turn can end in a tool call rather than words, "the last thing the user
    /// said" is no longer where the conversation is -- the model may be
    /// answering its own tool result, with the user's last words several turns
    /// back.
    pub turns: Vec<Turn>,
    /// What the model may do with its own hands this turn.
    ///
    /// Empty means a prose-only turn, which is what a model that cannot call
    /// tools gets. A provider must not invent tools when this is empty.
    pub tools: Vec<ToolSpec>,
}

/// One tool, as a model is told about it.
///
/// A flattened copy of what [`crate::tools::Tool`] declares, rather than the
/// trait object itself, so that the `llm` layer never holds something runnable.
/// Describing a tool and running one are different jobs and this is the seam
/// between them: a provider builds a request, and cannot execute anything.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

impl ToolSpec {
    /// Describes every tool available under a permission level.
    ///
    /// Goes through [`crate::tools::all`] rather than filtering here, so the
    /// list a model is *shown* and the list it may *run* are the same list.
    /// Two filters would eventually disagree, and the direction that fails
    /// quietly is the dangerous one: a tool offered but refused is confusing,
    /// while a tool refused but runnable is a hole in the boundary.
    pub fn all(permissions: crate::tools::Permissions) -> Vec<Self> {
        crate::tools::all(permissions)
            .iter()
            .map(|t| Self {
                name: t.name().to_string(),
                description: t.description(),
                schema: t.input_schema(),
            })
            .collect()
    }

    /// Describes the tools this model can actually make use of, under a
    /// permission level.
    ///
    /// Two narrowings, and they compose rather than compete. The permissions is
    /// about what a *plan* may do to the world -- a subagent surveys and cannot
    /// write. The capabilities are about what a *model* can make sense of --
    /// one with no vision would call `read_image`, be handed something it
    /// cannot look at, and have spent one of the user's turns discovering that.
    ///
    /// Both live here so neither is a check a caller has to remember.
    pub fn for_model(model: &dyn Model, permissions: crate::tools::Permissions) -> Vec<Self> {
        if !model.can_act() {
            // Not merely "no tools worth offering" -- sending a `tools` array to
            // a gateway that does not accept one fails the request outright.
            return Vec::new();
        }
        let sighted = model.can_see();
        Self::all(permissions)
            .into_iter()
            .filter(|t| sighted || t.name != "read_image")
            .collect()
    }
}

/// The facts about a project worth spending tokens on.
#[derive(Debug, Clone, Default)]
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
    /// One or two lines, shown on hover.
    pub summary: String,
    /// The full reply, shown in the dock.
    pub body: String,
}

/// What a model does when asked.
///
/// Two endings, because a turn can now finish in two ways: the model has
/// something to say, or it wants to do something first. Making this an enum
/// rather than a `Draft` with an optional list of calls is deliberate -- the
/// two are mutually exclusive in the loop that consumes them, and a shape that
/// can hold both invites a caller to settle a plan *and* run its tools.
#[derive(Debug, Clone)]
pub enum Reply {
    /// Words. The turn is over.
    Spoke(Draft),
    /// Tool calls to make before the model will answer. Never empty.
    Acts(Vec<Act>),
}

/// One completed turn: what the model did, and what it cost.
///
/// A wrapper around [`Reply`] rather than a field on each of its variants. The
/// cost is true of the *turn*, not of one of its two endings -- a turn that
/// ends in tool calls filled the window exactly as much as one that ends in
/// words -- and duplicating it across the variants would make every `match` in
/// the turn loop responsible for carrying it past.
#[derive(Debug, Clone)]
pub struct Answer {
    pub reply: Reply,
    /// Tokens this turn cost, as the provider reported them.
    ///
    /// `None` means the provider said nothing about it, which is a different
    /// thing from zero and must stay tellable apart: zero would be drawn as an
    /// empty window, which is a claim rather than an absence.
    pub tokens: Option<usize>,
}

impl Answer {
    /// A turn whose cost the provider did not report.
    pub fn untallied(reply: Reply) -> Self {
        Self {
            reply,
            tokens: None,
        }
    }
}

/// One tool call a model has asked for.
#[derive(Debug, Clone)]
pub struct Act {
    /// The provider's correlation id, quoted back with the result. See
    /// [`kingdom_core::ToolCall::id`].
    pub id: String,
    pub tool: String,
    /// The arguments as the model sent them. Already parsed where it sent valid
    /// JSON; the raw text as a JSON string where it did not, so that a
    /// malformed call is still recorded as the thing that was attempted.
    pub input: serde_json::Value,
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
    /// Takes one turn: either speaks, or asks to act.
    ///
    /// One call, not a loop. Driving the conversation is [`crate::api`]'s job,
    /// because that is where the plan lives and where a tool call can be
    /// recorded before it runs -- a provider that ran its own tools would do so
    /// with nothing watching, and the conversation would show a long silence
    /// followed by an answer.
    async fn take_turn(&self, brief: &Brief) -> Result<Answer, ModelError>;

    /// The namespaced id, recorded on the plan so the user can see exactly what
    /// drew it. Namespaced rather than bare because that is what routes the
    /// *next* turn back to the same backend.
    fn id(&self) -> &str;

    /// Whether this model can be given tools.
    ///
    /// A model that cannot call them is still perfectly able to draft a plan,
    /// so it is offered a prose-only turn rather than withheld from the picker:
    /// the user choosing a weaker model should get a weaker answer, not an
    /// error. The loop asks this and sends no tools when it is false, which is
    /// what stops a gateway rejecting the request outright.
    fn can_act(&self) -> bool {
        true
    }

    /// Whether this model can be shown an image.
    ///
    /// Defaults to *false*, the opposite of [`Model::can_act`], because the
    /// costs are the opposite way round. A provider that silently ignores a
    /// tool it was given wastes nothing; a provider handed an image it cannot
    /// parse rejects the whole request. A backend that can see says so.
    fn can_see(&self) -> bool {
        false
    }

    /// How many tokens this model will hold, or `0` when it does not say.
    ///
    /// Asked of the model rather than looked up beside it, because the window
    /// is a fact about the model and can change under a conversation that has
    /// been running for a week. Zero means "unknown", which is what the offline
    /// mock honestly is -- and a window of zero yields no reading at all rather
    /// than an invented one. See [`kingdom_core::ContextUsage::percent`].
    fn context_window(&self) -> usize {
        0
    }
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

/// The namespaced model id the picker opens on before the user has chosen,
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
    use kingdom_core::{Folder, Language};

    fn walk(d: &Folder, out: &mut Vec<(bool, u64, String)>) {
        for b in &d.source_files {
            let is_source = matches!(
                b.language,
                Language::Rust | Language::Web | Language::Python | Language::Go | Language::Systems
            );
            out.push((is_source, b.bytes, b.path.clone()));
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
