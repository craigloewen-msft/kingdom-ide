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
    /// Something Kingdom needs the model to know that nobody said.
    ///
    /// Today there is one user: the turn before this one came back empty, and
    /// this is how the next turn differs from it. Without it a plan whose reply
    /// arrived empty is stuck -- `settle` records a [`kingdom_core::Note`],
    /// notes are deliberately excluded from [`Turn`], and so the King saying
    /// "keep going" rebuilds a byte-identical request and receives a
    /// byte-identical silence. That loop is what made the failure feel
    /// unfixable.
    ///
    /// **Not a [`Turn`], and that is the point.** A provider renders this on
    /// the wire only; it is never a `Turn::Message`, never in the transcript,
    /// and never attributed to the user or the court. Kingdom's plumbing must
    /// not be replayed to a model as something a participant said -- the doc on
    /// [`kingdom_core::Turn`] is where that rule is argued, and the images in
    /// `copilot::shown` are the other place it is kept this same way.
    pub aside: Option<String>,
    /// What the model may do with its own hands this turn.
    ///
    /// Empty means a prose-only turn, which is what a model that cannot call
    /// tools gets. A provider must not invent tools when this is empty.
    pub tools: Vec<ToolSpec>,
    /// How many bytes of conversation this request may carry.
    ///
    /// On the `Brief` rather than on the provider because it is a property of
    /// *this attempt*, not of the gateway: `api::converse` lowers it and asks
    /// again when a request comes back too large, and a provider that held its
    /// own budget would have no way to be told.
    ///
    /// What it bounds is the history -- see [`Budget`]. The system prompt, the
    /// tool schemas and the King's own words are never shed, so this is not a
    /// promise about the size of the finished body; it is the ceiling on the
    /// part that grows without limit.
    pub budget: Budget,
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
    ///
    /// Carries what the model produced *alongside* the calls, because a reply
    /// is one decision even when it asks for six things. Dropping the thinking
    /// and the narration here is what left a model re-deriving its strategy
    /// from raw tool output every round; see [`kingdom_core::Reasoning`].
    Acts(Acts),
}

/// One reply's worth of tool calls, and the thinking that produced them.
#[derive(Debug, Clone, Default)]
pub struct Acts {
    /// What to do. Never empty -- a reply with no calls is [`Reply::Spoke`].
    pub calls: Vec<Act>,
    /// The model's own reasoning, to be handed back to it next round.
    pub reasoning: Option<kingdom_core::Reasoning>,
    /// Prose the model wrote in the same reply as the calls, usually saying
    /// what it is about to do and why.
    pub narration: Option<String>,
}

impl Acts {
    /// A reply that asked for these calls and said nothing about them. The
    /// shape a provider with no reasoning to report produces.
    pub fn plain(calls: Vec<Act>) -> Self {
        Self {
            calls,
            reasoning: None,
            narration: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// The same calls, with what the model said in words as it asked for them.
    ///
    /// A builder rather than a fourth constructor because a provider reads the
    /// two halves from different places in the same payload, and the mock adds
    /// them one at a time to whichever scenario is worth rehearsing.
    pub fn saying(mut self, narration: impl Into<String>) -> Self {
        self.narration = Some(narration.into());
        self
    }

    /// The same calls, with the thinking that produced them.
    ///
    /// Only the prose half: the opaque half is a provider's signature, and
    /// nothing but a provider can produce one.
    pub fn thinking(mut self, text: impl Into<String>) -> Self {
        self.reasoning = Some(kingdom_core::Reasoning {
            text: Some(text.into()),
            opaque: Default::default(),
        });
        self
    }

    pub fn len(&self) -> usize {
        self.calls.len()
    }
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
    /// The provider answered, and the answer had nothing in it.
    ///
    /// Its own variant rather than a [`ModelError::Refused`] because the two
    /// want opposite handling. A refusal is an answer -- the model considered
    /// the request and declined -- and asking again changes nothing. An empty
    /// reply is the *absence* of an answer, and the same request resampled
    /// usually produces one, which is what makes it worth retrying.
    ///
    /// This distinction is the whole reason a plan no longer dies on the first
    /// one. See [`ModelError::is_transient`].
    #[error("{0}")]
    Empty(String),
    /// The gateway refused to *read* the request, because it was too big.
    ///
    /// Its own variant rather than a [`ModelError::Refused`] for the same reason
    /// [`ModelError::Empty`] is: the two want opposite handling. A refusal is a
    /// considered answer to the question asked, and nothing about asking it
    /// again will change it. A 413 is the question never having been read, and
    /// the remedy is a *smaller* question -- which is a thing Kingdom can
    /// actually do, by shedding what a request need not carry.
    ///
    /// Folding this into `Refused` is what made a real plan unrecoverable: 413
    /// is a 4xx, `Refused` is fatal, and every "keep going" rebuilt the same
    /// oversized body from the same transcript and was rejected identically.
    /// See [`Budget`] for what shrinking means, and `api::converse` for where it
    /// happens.
    #[error("{0}")]
    TooLarge(String),
}

impl ModelError {
    /// Whether asking again, unchanged, could plausibly answer differently.
    ///
    /// The question a retry has to be able to answer honestly, and the reason
    /// it is asked of the error rather than of the message: a plan used to die
    /// on the first empty reply because every failure looked alike to the loop,
    /// and "Copilot returned an empty reply" was as fatal as a missing
    /// credential.
    ///
    /// Deliberately narrow. A credential that is missing stays missing, a
    /// refusal is a considered answer, and both would only waste the user's
    /// time -- and his quota -- three times over. What is left is the reply that
    /// never arrived and the gateway that was briefly unwell, which are exactly
    /// the failures a second attempt fixes.
    ///
    /// [`ModelError::TooLarge`] is deliberately **not** transient, and that is
    /// not the same as saying it is fatal. Resending the identical body would
    /// be rejected identically, so it fails this question honestly; what it
    /// earns instead is a retry that *changes* the request first, which
    /// `api::converse` asks for separately through [`ModelError::is_shrinkable`].
    /// Answering `true` here would have this loop resend the same oversized
    /// request twice more and call the result a retry.
    pub fn is_transient(&self) -> bool {
        match self {
            ModelError::Empty(_) | ModelError::Transport(_) => true,
            ModelError::Credential(_) | ModelError::Refused(_) | ModelError::TooLarge(_) => false,
        }
    }

    /// Whether asking again with *less* in the request could answer differently.
    ///
    /// The counterpart to [`ModelError::is_transient`], asked separately because
    /// the two remedies are different: one resends, this one rebuilds. Only a
    /// request the gateway declined to read qualifies -- a model that considered
    /// the question and refused it will refuse a shorter version of the same
    /// question just as firmly.
    pub fn is_shrinkable(&self) -> bool {
        matches!(self, ModelError::TooLarge(_))
    }
}

/// How many bytes of conversation one request may carry.
///
/// A gateway limits the *size of the body*, and that limit is nothing to do with
/// the model's context window: a real plan was rejected with a 413 while its own
/// header honestly read 257k of 1M tokens used. Tokens were never the scarce
/// thing; bytes were, and nothing was counting them.
///
/// The number is a guess with headroom, and deliberately so. The only hard fact
/// available is that ~5.3 MB was refused; the limit itself is not published and
/// differs per gateway. So this is set well clear of the one figure known to
/// fail, while leaving room for the two recent screenshots a provider is
/// expected to carry (`copilot::RECENT_REPLIES`) -- a budget too mean to hold
/// those would shed them on every request and quietly undo the fix it exists to
/// support.
///
/// Being wrong in either direction is survivable, which is what makes a guess
/// acceptable here: too generous costs one refused request before
/// [`Budget::tighter`] shrinks and retries, and too mean costs some old signed
/// thinking that nothing in the UI has ever drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub bytes: usize,
}

impl Budget {
    /// What a request starts with.
    pub const FULL: Budget = Budget {
        bytes: 3 * 1024 * 1024,
    };

    /// Below this, shedding has stopped buying anything worth the loss.
    ///
    /// A floor rather than an endless halving because a request eventually
    /// consists of the system prompt, the tool schemas and the King's own words,
    /// none of which this may drop. Past that point the honest answer is to fail
    /// and say what was too big, not to send a request with the conversation
    /// filed off.
    const FLOOR: usize = 256 * 1024;

    /// Half as much, or `None` at the floor.
    ///
    /// Halving rather than stepping down politely: the limit is unknown, so the
    /// aim is to cross under it in two or three attempts rather than to discover
    /// it precisely at the cost of the user's afternoon.
    pub fn tighter(self) -> Option<Budget> {
        let bytes = self.bytes / 2;
        (bytes >= Self::FLOOR).then_some(Budget { bytes })
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::FULL
    }
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
                Language::Rust
                    | Language::Web
                    | Language::Python
                    | Language::Go
                    | Language::Systems
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
