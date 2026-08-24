//! The Kingdom domain model.

use crate::ids::*;
use crate::permissions::Permissions;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Kingdom & Cities
// ---------------------------------------------------------------------------

/// The dev folder the user has opened. Everything else hangs off this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kingdom {
    /// Display name, normally the folder's own name.
    pub name: String,
    /// Absolute path to the dev folder on the host machine.
    pub root: String,
    pub cities: Vec<City>,
    pub plans: Vec<Plan>,
    /// True when this kingdom is a seeded proving ground rather than real work.
    ///
    /// The UI renders this loudly. A synthetic fixture is *designed* to be
    /// indistinguishable from a real one on the map, which makes an unlabelled
    /// one a trap -- for the user glancing at it, and equally for a model shown
    /// a screenshot of it later. Same instinct as `AGENTS.md` being explicit
    /// about what is real versus faked, applied to the running UI.
    #[serde(default)]
    pub sandbox: bool,
}

impl Kingdom {
    /// An empty kingdom, shown before any folder has been chosen.
    pub fn unopened() -> Self {
        Self {
            name: "No Kingdom".into(),
            root: String::new(),
            cities: Vec::new(),
            plans: Vec::new(),
            sandbox: false,
        }
    }

    pub fn is_open(&self) -> bool {
        !self.root.is_empty()
    }

    pub fn city(&self, id: &CityId) -> Option<&City> {
        self.cities.iter().find(|c| &c.id == id)
    }

    /// Plans drawn up in a given city.
    ///
    /// Subagents are excluded, and this is the reader that decides it for the
    /// map. A subagent holds no worktree of its own -- it works in its parent's
    /// -- so a second pip on the same city would draw one piece of work twice.
    pub fn plans_in<'a>(&'a self, id: &'a CityId) -> impl Iterator<Item = &'a Plan> + 'a {
        self.plans
            .iter()
            .filter(move |p| &p.city == id && !p.is_subagent())
    }

    /// The subagents one call sent, in the order they were sent.
    ///
    /// Keyed by the tool call as well as the parent, so a plan that sends
    /// subagents twice does not show the first round's under the second round's
    /// call.
    ///
    /// This is the *only* way the parent's conversation finds its subagents,
    /// and the direction is deliberate: the link is a field on the subagent
    /// rather than a list on the [`ToolCall`], so there is one place it can be
    /// wrong. A list on the tool call would have to be kept in step with the
    /// plans themselves, and the failure -- a named subagent that does not
    /// exist, or a subagent no call admits to -- would be silent.
    pub fn subagents_of<'a>(
        &'a self,
        parent: &'a PlanId,
        tool_call: &'a str,
    ) -> impl Iterator<Item = &'a Plan> + 'a {
        self.plans.iter().filter(move |p| match &p.spawned_by {
            Some(subagent) => &subagent.parent == parent && subagent.tool_call == tool_call,
            None => false,
        })
    }

    /// Files a plan into the kingdom, replacing whatever was there under its id.
    ///
    /// The receiving half of push: the server publishes a whole plan and the
    /// browser absorbs it. Replacing rather than merging is the point -- see
    /// `events.rs` for why the wire carries whole plans rather than deltas.
    ///
    /// An unknown id is appended rather than dropped, so a plan opened in one
    /// tab appears in another without a full refetch.
    pub fn insert(&mut self, plan: Plan) {
        match self.plans.iter_mut().find(|p| p.id == plan.id) {
            Some(existing) => *existing = plan,
            None => self.plans.push(plan),
        }
    }

    pub fn plan(&self, id: &PlanId) -> Option<&Plan> {
        self.plans.iter().find(|p| &p.id == id)
    }

    /// Plans still awaiting the user's judgement.
    ///
    /// Never a subagent: a subagent reports to the model that sent it, and
    /// nothing about it is ever waiting on the user.
    pub fn pending_plans(&self) -> impl Iterator<Item = &Plan> {
        self.plans
            .iter()
            .filter(|p| p.status == PlanStatus::AwaitingReview && !p.is_subagent())
    }
}

/// A single project directory: one city in the kingdom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct City {
    pub id: CityId,
    pub name: String,
    /// Path relative to the kingdom root.
    pub path: String,
    pub kind: CityKind,
    /// Rough size signal, used to scale the city on the map.
    pub file_count: usize,
    /// Whether the project is a git repository.
    pub has_git: bool,
    /// Uncommitted changes, if known.
    pub dirty_files: usize,
    /// The project's folder tree, from which the map builds its skyline.
    ///
    /// `None` when the structure was not scanned; the map then falls back to a
    /// plain keep glyph, so every caller predating the skyline still works.
    pub structure: Option<Folder>,
}

// ---------------------------------------------------------------------------
// City structure -- the raw shape of a project on disk
// ---------------------------------------------------------------------------

/// One file in a project: a single building in its city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFile {
    pub name: String,
    /// Path relative to the city root, which is what identifies this exact
    /// building on the map.
    pub path: String,
    pub language: Language,
    /// Size in bytes, which drives the building's height.
    pub bytes: u64,
}

/// One folder in a project: a district of its city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    /// Path relative to the city root; empty for the root district.
    pub path: String,
    pub source_files: Vec<SourceFile>,
    pub children: Vec<Folder>,
    /// Files the scanner pruned rather than listing individually.
    ///
    /// Carrying the remainder as a count and a weight (instead of dropping it)
    /// is what keeps the map honest: a folder with ten thousand files still
    /// renders as heavy, even though only its largest files are named.
    pub extra_files: usize,
    pub extra_bytes: u64,
}

impl Folder {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            source_files: Vec::new(),
            children: Vec::new(),
            extra_files: 0,
            extra_bytes: 0,
        }
    }

    /// Every file beneath this district, including pruned remainders.
    pub fn total_files(&self) -> usize {
        self.source_files.len()
            + self.extra_files
            + self
                .children
                .iter()
                .map(Folder::total_files)
                .sum::<usize>()
    }

    /// Total bytes beneath this district, including pruned remainders.
    pub fn total_bytes(&self) -> u64 {
        self.source_files.iter().map(|b| b.bytes).sum::<u64>()
            + self.extra_bytes
            + self.children.iter().map(Folder::total_bytes).sum::<u64>()
    }

    pub fn is_empty(&self) -> bool {
        self.total_files() == 0
    }
}

/// The language a file is written in, which tints its building.
///
/// Colour is the fastest channel the user has for reading a city's composition
/// at a glance, so this is a small, visually distinct set rather than an
/// exhaustive language list; anything unrecognised falls to
/// [`Language::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    Web,
    Python,
    Go,
    Systems,
    Shell,
    Markup,
    Style,
    Config,
    Docs,
    Other,
}

impl Language {
    /// Every language, in legend order.
    pub const ALL: [Language; 11] = [
        Language::Rust,
        Language::Web,
        Language::Python,
        Language::Go,
        Language::Systems,
        Language::Shell,
        Language::Markup,
        Language::Style,
        Language::Config,
        Language::Docs,
        Language::Other,
    ];

    /// Classifies a file by extension.
    pub fn from_path(path: &str) -> Language {
        let name = path.rsplit('/').next().unwrap_or(path);

        // Extensionless files that are nonetheless recognisable.
        match name {
            "Makefile" | "Dockerfile" | "Justfile" | "justfile" => return Language::Config,
            "LICENSE" | "NOTICE" | "AUTHORS" => return Language::Docs,
            _ => {}
        }

        let ext = match name.rsplit_once('.') {
            // A leading dot means a dotfile, not an extension.
            Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
            _ => return Language::Config,
        };

        match ext.as_str() {
            "rs" => Language::Rust,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => Language::Web,
            "py" | "pyi" | "ipynb" => Language::Python,
            "go" => Language::Go,
            "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "zig" | "java" | "kt" | "swift" | "cs" => {
                Language::Systems
            }
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" => Language::Shell,
            "html" | "htm" | "xml" | "svg" | "jsx.html" => Language::Markup,
            "css" | "scss" | "sass" | "less" | "styl" => Language::Style,
            "toml" | "json" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "lock" | "env" => {
                Language::Config
            }
            "md" | "mdx" | "txt" | "rst" | "adoc" => Language::Docs,
            _ => Language::Other,
        }
    }

    /// The building's face colour.
    ///
    /// Deliberately avoids the status palette (green/amber/red), which is
    /// reserved for what agents are doing; language colour says what the code
    /// *is*.
    pub fn tint(&self) -> &'static str {
        match self {
            Language::Rust => "#fb923c",
            Language::Web => "#38bdf8",
            Language::Python => "#60a5fa",
            Language::Go => "#2dd4bf",
            Language::Systems => "#c084fc",
            Language::Shell => "#a3e635",
            Language::Markup => "#f472b6",
            Language::Style => "#818cf8",
            Language::Config => "#94a3b8",
            Language::Docs => "#cbd5e1",
            Language::Other => "#546076",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Web => "JS/TS",
            Language::Python => "Python",
            Language::Go => "Go",
            Language::Systems => "Systems",
            Language::Shell => "Shell",
            Language::Markup => "Markup",
            Language::Style => "Styles",
            Language::Config => "Config",
            Language::Docs => "Docs",
            Language::Other => "Other",
        }
    }
}

/// What sort of project a city is, inferred from marker files.
///
/// Purely cosmetic today (it picks the banner colour), but it is the natural
/// hook for per-stack behaviour later, e.g. knowing that two Rust cities will
/// contend for the same `~/.cargo` lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CityKind {
    Rust,
    Node,
    Python,
    Go,
    Mixed,
    Unknown,
}

impl CityKind {
    /// Banner colour for the city's banner on the map.
    pub fn banner_color(&self) -> &'static str {
        match self {
            CityKind::Rust => "#d97706",
            CityKind::Node => "#16a34a",
            CityKind::Python => "#2563eb",
            CityKind::Go => "#0891b2",
            CityKind::Mixed => "#7c3aed",
            CityKind::Unknown => "#64748b",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            CityKind::Rust => "Rust",
            CityKind::Node => "Node",
            CityKind::Python => "Python",
            CityKind::Go => "Go",
            CityKind::Mixed => "Mixed",
            CityKind::Unknown => "Unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Workspaces -- where a plan actually works
// ---------------------------------------------------------------------------

/// How isolated a plan's working copy is.
///
/// This is the user's answer to "can this agent trample the folder I am in?".
/// It is chosen per prompt because the honest answer differs per prompt: a
/// survey wants the folder as it stands, a change wants somewhere of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceMode {
    /// A throwaway worktree cut from the city's current HEAD.
    Fresh,
    /// A named branch, checked out into its own worktree.
    Branch(String),
    /// The project directory itself. No isolation.
    InPlace,
}

impl WorkspaceMode {
    /// Short label for the prompt bar's chip and the conversation header.
    pub fn label(&self) -> String {
        match self {
            WorkspaceMode::Fresh => "fresh worktree".to_string(),
            WorkspaceMode::Branch(b) => format!("branch: {b}"),
            WorkspaceMode::InPlace => "in place".to_string(),
        }
    }
}

impl Default for WorkspaceMode {
    /// Isolation by default: the surprising outcome should be the one the user
    /// asked for, not the one that quietly edits the folder he is sitting in.
    fn default() -> Self {
        WorkspaceMode::Fresh
    }
}

/// A prepared place on disk for one plan to work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub mode: WorkspaceMode,
    /// Absolute path the plan actually reads and writes.
    pub path: String,
    /// The branch checked out there, when there is one.
    pub branch: Option<String>,
    /// The GUID naming the worktree folder, for `Fresh` and `Branch`.
    pub id: Option<String>,
    /// The branch this workspace was cut from, recorded at the moment it was
    /// true.
    ///
    /// This is what makes "merge this work back" honest. Reading the city's
    /// current HEAD at merge time would land a plan wherever the user happens
    /// to have wandered since it was opened -- which is the collision this
    /// product exists to prevent, committed by the product itself.
    #[serde(default)]
    pub base: Option<String>,
}

impl Workspace {
    /// The city's own directory, untouched.
    pub fn in_place(path: impl Into<String>) -> Self {
        Self {
            mode: WorkspaceMode::InPlace,
            path: path.into(),
            branch: None,
            id: None,
            base: None,
        }
    }

    /// True when this plan has a checkout of its own, and so cannot collide
    /// with a plan working elsewhere in the same repository.
    pub fn is_isolated(&self) -> bool {
        self.id.is_some()
    }
}

// ---------------------------------------------------------------------------
// Plans -- the unit of work AND the unit of review
// ---------------------------------------------------------------------------

/// An architectural plan: a proposal drafted by a model, awaiting the user's
/// review.
///
/// A plan is deliberately the *only* agent-shaped noun in the model. An earlier
/// design had a separate `Architect` entity that owned plans, but the user
/// never reviews an architect -- he reviews a plan. Collapsing the two removes
/// a state machine that had to be kept in sync with this one for no gain: which
/// model is drafting is an attribute of the work, not an actor with a life of
/// its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    /// The city this plan is drawn up for.
    pub city: CityId,
    pub title: String,
    /// The title as a git-safe slug. The plan's branch is cut from this, so the
    /// name the user reads in the rail and the name he reads in `git branch`
    /// are the same name.
    ///
    /// `#[serde(default)]` because plan records written before plans had slugs
    /// are still on disk, and their branch already exists under its old name.
    #[serde(default)]
    pub slug: String,
    pub summary: String,
    /// The prompt that opened this plan, verbatim.
    pub prompt: String,
    /// Which model is drafting it, e.g. `"mock"` or `"copilot/claude-opus-5"`.
    pub model: String,
    /// How hard that model was asked to think. `None` is its own default,
    /// which is a different request from any explicit level.
    pub effort: Option<ModelEffort>,
    /// The chat log so far: what was said, and what happened, in order.
    pub transcript: Vec<Entry>,
    pub status: PlanStatus,
    /// How it ended, and what it left behind. `None` while the plan is still in
    /// play; always present once the status is settled.
    #[serde(default)]
    pub outcome: Option<Outcome>,
    /// Where on disk this plan works. Settled when the plan opens and never
    /// changed afterwards.
    pub workspace: Workspace,
    /// What this plan is busy with right now, in plain language, or `None`
    /// when nothing is in flight.
    ///
    /// This is the whole of what the old lease machinery actually did: it
    /// answers "is someone working on this right now?", which is what stops a
    /// reload or a second tab starting a duplicate model call. It is a
    /// description rather than a lock -- nothing is arbitrated, and no other
    /// plan is prevented from working.
    #[serde(default)]
    pub working_on: Option<String>,
    /// The call this plan was sent to answer, when it is a subagent.
    ///
    /// `None` for a plan the user opened, which is every plan they see in the
    /// rail or on the map. A subagent is a plan the *model* sent, to answer a
    /// question it had while working on its own.
    ///
    /// Being a plan rather than a type of its own is what gives a subagent a
    /// conversation, a watch socket and a record on disk for free -- see the
    /// module docs on [`Plan::spawned`] for what that buys and what it costs.
    #[serde(default)]
    pub spawned_by: Option<SpawnedBy>,
    /// What this plan is allowed to do to the world, right now.
    ///
    /// The one field on a plan the user changes directly, and they change it
    /// exactly once: from [`Permissions::Propose`] to [`Permissions::Full`], by
    /// accepting a proposal. Everything else here is written by the model or by
    /// Kingdom.
    ///
    /// Defaults to `Full` when absent so a plan recorded before proposals
    /// existed keeps the tools it was drafted with. The opposite default would
    /// strand every old plan: reloaded without a proposal it could never make
    /// one, because the composer would only ever hand it back to a model with
    /// no `patch`.
    #[serde(default = "Permissions::full")]
    pub permissions: Permissions,
    /// The plan the model has put to the user, if there is one standing.
    ///
    /// A *standing* proposal is one that is still the live question in the
    /// conversation -- see [`Plan::propose`], [`Plan::approve`] and
    /// [`Plan::set_aside_proposal`] for the three transitions, which together
    /// are the whole of this state machine.
    #[serde(default)]
    pub proposal: Option<Proposal>,
}

/// A plan the model has drawn up and put to the user.
///
/// Held on the plan rather than only in the transcript, even though the tool
/// call that made it is already there. The transcript is the *record*; this is
/// the *question currently on the table*, and the conversation view needs to
/// answer "is there one, and has it been accepted?" without re-parsing tool
/// arguments to find out. It also means the body survives independently of how
/// the tool call was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// The model's own headline for the work.
    ///
    /// Not applied to [`Plan::title`] today. Retitling a plan means renaming
    /// its `kingdom/<slug>` branch, which is its own piece of work; until then
    /// this is the better name sitting ready for it.
    pub title: String,
    /// The proposal itself, as markdown.
    pub body: String,
    /// When it was put to the user. See [`Timestamp`].
    #[serde(default)]
    pub at: Option<Timestamp>,
    /// True once the user has said to start with it.
    ///
    /// An approved proposal is no longer a question -- it is the plan being
    /// carried out, which is why it survives the model speaking again where an
    /// unapproved one does not.
    #[serde(default)]
    pub approved: bool,
}

impl Proposal {
    /// A proposal as the model has just made it: put, not yet accepted.
    pub fn put(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            at: Timestamp::now(),
            approved: false,
        }
    }
}

/// What the user is recorded as saying when they accept a proposal.
///
/// The grant reaches the model as an ordinary [`Speaker::User`] turn rather
/// than as a new kind of message, so no provider has to learn anything -- and
/// it is also simply true, since they did say to start.
///
/// A constant because it is not only *written*. Anything that reasons about
/// what the user actually asked for has to be able to tell their prompt from
/// Kingdom's phrasing of their click, and comparing against a literal in two
/// places is how those two drift apart.
pub const APPROVAL: &str = "Approved. Carry out the plan as proposed. If you find it was \
                            wrong, say so rather than quietly doing something else.";

/// Which call a subagent was sent to answer.
///
/// The tool call is carried as well as the plan because a plan may send
/// subagents more than once: without it, a second round's conversation would
/// show the first round's subagents under its call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnedBy {
    /// The plan that sent this one.
    pub parent: PlanId,
    /// The [`ToolCall::id`] of the call that sent it.
    pub tool_call: String,
}

impl Plan {
    /// A plan that has just been opened by a prompt, before any drafting.
    pub fn opened(
        id: PlanId,
        city: CityId,
        prompt: impl Into<String>,
        choice: &ModelChoice,
        workspace: Workspace,
    ) -> Self {
        let prompt = prompt.into();
        let title = title_from_prompt(&prompt);
        Self {
            id,
            city,
            // Derived here, beside the title, so the two cannot drift: a plan
            // whose branch does not match its rail label is exactly the
            // confusion this field exists to prevent.
            slug: slug_for_prompt(&prompt),
            title,
            summary: String::new(),
            transcript: vec![Entry::Message(Message::new(Speaker::User, prompt.clone()))],
            prompt,
            model: choice.model.clone(),
            effort: choice.effort,
            status: PlanStatus::Drafting,
            outcome: None,
            workspace,
            working_on: None,
            spawned_by: None,
            // A prompt opens under Propose: the model draws up a plan and puts
            // it to the user before it touches anything. This one line is what
            // inverts the product's stance -- the user reviews a proposal
            // rather than a fait accompli.
            permissions: Permissions::Propose,
            proposal: None,
        }
    }

    /// A plan sent by another plan to answer one question.
    ///
    /// # What a subagent shares with its parent, and why
    ///
    /// Its **workspace, verbatim**. A subagent is another agent working in the
    /// same place on the same files, which is the whole point -- it is sent to
    /// look at the work in progress, not at a pristine copy of the project. It
    /// therefore owns nothing on disk and must never be finished: merging it
    /// would land its parent's half-done work and then delete the worktree from
    /// under a plan still running in it. `api::finish_plan` refuses on
    /// [`Plan::is_subagent`], and that guard is load-bearing.
    ///
    /// Its **model and effort**, so a fan-out is drafted by the thing the user
    /// chose rather than by whatever the default happens to be that day.
    ///
    /// # Why the task is recorded as the user speaking
    ///
    /// It was the parent's model that said it, not the user, so this is a small
    /// lie -- and it is the right one. [`Speaker`] maps directly onto the
    /// wire's `user`/`assistant` roles, and a third variant would need a
    /// decision at every match over a transcript -- the provider, the mock, the
    /// store's repair pass, the conversation -- to buy nothing but a label. The
    /// label is fixed where labels belong: the conversation renders a
    /// subagent's own turns as "Commission".
    pub fn spawned(id: PlanId, parent: &Plan, tool_call: &str, task: impl Into<String>) -> Self {
        let task = task.into();
        Self {
            id,
            city: parent.city.clone(),
            slug: crate::naming::slugify(&title_from_prompt(&task)),
            title: title_from_prompt(&task),
            summary: String::new(),
            transcript: vec![Entry::Message(Message::new(Speaker::User, task.clone()))],
            prompt: task,
            model: parent.model.clone(),
            effort: parent.effort,
            status: PlanStatus::Drafting,
            outcome: None,
            workspace: parent.workspace.clone(),
            working_on: None,
            spawned_by: Some(SpawnedBy {
                parent: parent.id.clone(),
                tool_call: tool_call.to_string(),
            }),
            // A subagent reads and reports and never writes, which is the
            // whole reason several may share one worktree safely. Carried on
            // the subagent rather than passed to the turn loop, so the
            // invariant lives on the thing it constrains.
            permissions: Permissions::ReadOnly,
            // A subagent answers to the plan that sent it. Nothing about it is
            // ever put to the user.
            proposal: None,
        }
    }

    /// True when this plan was spawned by another plan rather than opened by
    /// the user.
    ///
    /// Read by every guard and every filter that has to tell the user's work
    /// from the model's own: the rail, the map, `say`, `draft_plan` and -- most
    /// importantly -- `finish_plan`.
    pub fn is_subagent(&self) -> bool {
        self.spawned_by.is_some()
    }

    /// True while a turn is in flight, so a second one is not started over it.
    pub fn is_busy(&self) -> bool {
        self.working_on.is_some()
    }

    /// What this plan is being drafted with, for re-use on the next turn.
    pub fn choice(&self) -> ModelChoice {
        ModelChoice {
            model: self.model.clone(),
            effort: self.effort,
        }
    }

    /// True while the plan is still in play, as opposed to settled history.
    pub fn is_live(&self) -> bool {
        !self.status.is_settled()
    }

    /// Closes the plan: records how it ended and moves it into history.
    ///
    /// The one door from live to settled, so a status and an outcome cannot
    /// disagree -- a `Merged` plan with no commit recorded would be a plan the
    /// conversation claims to have landed and cannot say where.
    pub fn settle(&mut self, outcome: Outcome) {
        self.status = match &outcome {
            Outcome::Merged { .. } => PlanStatus::Merged,
            Outcome::Archived { .. } => PlanStatus::Archived,
        };
        self.working_on = None;
        self.outcome = Some(outcome);
    }

    /// Records words a participant produced. These, and only these, are ever
    /// sent to a model.
    pub fn say(&mut self, speaker: Speaker, body: impl Into<String>) {
        self.transcript
            .push(Entry::Message(Message::new(speaker, body)));
    }

    /// Records something Kingdom itself reports. Never leaves the machine.
    pub fn note(&mut self, kind: NoteKind, body: impl Into<String>) {
        self.transcript.push(Entry::Note(Note::new(kind, body)));
    }

    /// Puts a plan to the user, replacing whatever was standing before it.
    ///
    /// Replacing rather than accumulating is the point: there is one question
    /// on the table at a time. A revised proposal supersedes the one it
    /// revises, and the superseded version is not lost -- the tool call that
    /// made it is still in the transcript, which is where the history of a plan
    /// lives.
    pub fn propose(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.proposal = Some(Proposal::put(title, body));
    }

    /// The user accepts the standing proposal, and the model gains its tools.
    ///
    /// The single door from proposing to working, so a plan cannot end up with
    /// `Permissions::Full` and no accepted proposal to answer for it. Returns
    /// false when nothing was standing, which the caller must report rather
    /// than swallow: granting authority on a question that is no longer being
    /// asked is exactly the mistake worth being loud about.
    pub fn approve(&mut self) -> bool {
        match &mut self.proposal {
            Some(proposal) => {
                proposal.approved = true;
                self.permissions = Permissions::Full;
                true
            }
            None => false,
        }
    }

    /// Clears a proposal the user has not accepted.
    ///
    /// Called when they set one aside, and again whenever the model speaks --
    /// see [`Plan::standing_proposal`] for why the second case matters. An
    /// *approved* proposal is deliberately untouched: it is no longer a
    /// question, it is the work in progress.
    pub fn set_aside_proposal(&mut self) {
        if self.proposal.as_ref().is_some_and(|p| !p.approved) {
            self.proposal = None;
        }
    }

    /// The proposal awaiting the user's word, if there is one.
    ///
    /// The single reader behind the conversation view's card, so "is there a
    /// decision to make here?" is answered the same way everywhere. Three
    /// conditions, and each rules out a card that would be wrong rather than
    /// merely redundant: a proposal already accepted is the work in progress
    /// and not a question; a plan that already has full permissions has nothing
    /// left to grant; and a plan still drafting may be in the middle of
    /// revising the very proposal being looked at.
    pub fn standing_proposal(&self) -> Option<&Proposal> {
        if self.permissions.is_full() || self.status != PlanStatus::AwaitingReview {
            return None;
        }
        self.proposal.as_ref().filter(|p| !p.approved)
    }

    /// The proposal the model is carrying out, if the user accepted one.
    ///
    /// What the conversation header names while the work is under way, and what
    /// tells the system prompt to remind the model whose plan it is following.
    pub fn approved_proposal(&self) -> Option<&Proposal> {
        self.proposal.as_ref().filter(|p| p.approved)
    }

    /// Just the messages, in order.
    ///
    /// What the user and the model *said*, with tool calls and Kingdom's own
    /// notices both left out. Useful for questions about the conversation --
    /// "has the model replied yet?" -- rather than for building a request.
    /// [`Plan::turns`] is what a model is handed.
    pub fn messages(&self) -> impl Iterator<Item = &Message> {
        self.transcript.iter().filter_map(|e| match e {
            Entry::Message(u) => Some(u),
            Entry::Note(_) | Entry::Tool(_) => None,
        })
    }

    /// Everything addressed to a model, in order: what was said and what was
    /// done, interleaved exactly as it happened.
    ///
    /// The single doorway between a plan's log and anything that talks to a
    /// model. Because it yields [`Turn`] rather than [`Entry`], a caller
    /// downstream of it *cannot* forward a note by accident -- the exclusion is
    /// the type, not a filter each caller has to remember.
    ///
    /// Order is the point, and is why this is not two separate iterators: a
    /// provider rebuilding the conversation must see a tool call before its
    /// result and both before the words that followed them, or the model is
    /// handed a history that never happened.
    pub fn turns(&self) -> impl Iterator<Item = Turn> + '_ {
        self.transcript.iter().filter_map(|e| match e {
            Entry::Message(u) => Some(Turn::Message(u.clone())),
            Entry::Tool(d) => Some(Turn::Tool(d.clone())),
            Entry::Note(_) => None,
        })
    }

    /// Records a tool call as begun, before it has run.
    ///
    /// Written down *before* the work rather than after it, so the conversation
    /// can show a command while it is still running. A tool call recorded only
    /// on completion would make a five-minute build look like five minutes of
    /// an agent doing nothing at all, which is the exact question this product
    /// exists to answer.
    pub fn begin_tool_call(&mut self, tool_call: ToolCall) {
        self.transcript.push(Entry::Tool(tool_call));
    }

    /// Settles a tool call that was begun earlier.
    ///
    /// Returns false if there is no such call still in flight, which the caller
    /// should treat as a bug rather than ignore: it means a result arrived for
    /// something never recorded as started, and the log the user reads is
    /// missing an event the model believes happened.
    pub fn settle_tool_call(&mut self, id: &str, outcome: ToolOutcome) -> bool {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool(tool_call) = entry {
                if tool_call.id == id && tool_call.in_flight() {
                    tool_call.outcome = Some(outcome);
                    return true;
                }
            }
        }
        false
    }
}

/// The slug a prompt will produce, before there is a [`Plan`] to ask.
///
/// Exists because of an ordering knot: the branch is named from the slug, but a
/// plan cannot be built until its workspace -- and therefore its branch --
/// exists. Rather than let the caller derive the name its own way and hope it
/// matches, both it and [`Plan::opened`] go through here.
pub fn slug_for_prompt(prompt: &str) -> String {
    crate::naming::slugify(&title_from_prompt(prompt))
}

/// A first-line title for a freshly opened plan, before the model has proposed
/// a better one. Keeps the sidebar readable during the seconds a draft takes.
fn title_from_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= 60 {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(57).collect();
    // Break on a word boundary so the ellipsis does not land mid-word.
    match cut.rsplit_once(' ') {
        Some((head, _)) if head.chars().count() > 20 => format!("{head}…"),
        _ => format!("{cut}…"),
    }
}

/// One line of a plan's chat log: something was said, something was done, or
/// something happened.
///
/// A notice is deliberately **not** a third [`Speaker`]. Nothing utters it,
/// nothing can reply to it, and it is never part of the exchange -- it is
/// information *about* the chat, shown inside the chat. As a speaker variant it
/// would have to be excluded by hand at every match, and the first place that
/// forgot would feed Kingdom's own plumbing back to a model as its own prior
/// words. Splitting it out one level up makes that mistake unrepresentable.
///
/// A [`ToolCall`] is not a speaker either, for the first half of the same
/// reason: nobody said it. But it parts company with a note on the second half
/// -- a tool call **does** go back to the model, because a tool result the
/// model is never shown is a tool call it will immediately make again. So the
/// log now holds three kinds of thing and exactly two of them are addressed to
/// a model; see [`Plan::turns`], which is the only door between this log and
/// one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Entry {
    /// Words a participant produced.
    Message(Message),
    /// Something Kingdom itself reports: a failed call or a workspace cut.
    /// Never sent anywhere.
    Note(Note),
    /// Something the model did with its own hands, and what came back.
    Tool(ToolCall),
}

/// A tool call the model made, and its result.
///
/// Both halves live in one entry rather than two. A call and its outcome are
/// one event in the user's reading of the conversation -- "it ran the tests,
/// and they failed" -- and splitting them would let the log hold a result with
/// no call, or two results for one call, neither of which is a thing that can
/// happen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's own correlation id for this call.
    ///
    /// Not ours to invent: it is what the next request must quote back so the
    /// model can match a result to the call it made. Two calls in one turn are
    /// otherwise indistinguishable.
    pub id: String,
    /// Which instrument: `"bash"`, `"patch"`, `"browser_click"`.
    pub tool: String,
    /// The arguments, as the model sent them.
    ///
    /// JSON rather than a typed shape because this is not a shape we declared --
    /// each tool has its own, and a model is perfectly capable of sending one
    /// that fits none of them. When the arguments will not even parse, the raw
    /// text is kept here as a JSON string: a record of what was actually
    /// attempted is worth more than a tidy `None`.
    pub input: serde_json::Value,
    /// What came back. `None` while the call is still running, which is a state
    /// the conversation renders -- it is how the user sees what an agent is
    /// doing *right now* rather than only what it did.
    pub outcome: Option<ToolOutcome>,
    /// When the call was made. See [`Timestamp`].
    #[serde(default)]
    pub at: Option<Timestamp>,
    /// Which reply this call arrived in, if the provider told us.
    ///
    /// A model routinely asks for several things at once -- read these three
    /// files -- and those calls are one decision, not three. Without a marker
    /// there is no way to tell "three calls in one reply" from "three replies
    /// with one call each" when the log is read back, and replaying the second
    /// shape teaches the model it deliberated between reads it actually made
    /// together.
    ///
    /// `None` on a record written before this field existed, which is read as
    /// "stands alone" -- the behaviour those records already had.
    #[serde(default)]
    pub batch: Option<String>,
    /// The model's own thinking, as it arrived with this call.
    ///
    /// Carried on the *first* call of a [`ToolCall::batch`] and `None` on the
    /// rest, because one reply produced one piece of reasoning however many
    /// calls it asked for. See [`Reasoning`] for why this is kept at all.
    #[serde(default)]
    pub reasoning: Option<Reasoning>,
    /// What the model said in words alongside asking for this call.
    ///
    /// A model often narrates the move it is about to make -- "I'll check how
    /// the sidebar reads its title" -- in the same reply as the call. That is
    /// its statement of intent, and dropping it leaves the next round with the
    /// action and no reason for it. Carried on the first call of a batch, for
    /// the same reason as [`ToolCall::reasoning`].
    #[serde(default)]
    pub narration: Option<String>,
}

/// A model's own reasoning, kept so it can be handed back to it.
///
/// **Why this is in the domain at all.** It looks like provider bookkeeping,
/// and half of it is. But a reasoning model that is not shown its own prior
/// thinking loses the thread of its investigation between rounds: on round N it
/// sees N tool results and no record of why it asked for any of them, so it
/// re-derives a strategy from raw output and re-reads what it has already read.
/// The thinking is part of the exchange, so it lives with the exchange.
///
/// **Why two fields.** Some providers return reasoning as text; some return an
/// additionally signed or encrypted blob that must be echoed back *unmodified*
/// or the next request is rejected. We can read the first and must not touch
/// the second, and a shape that conflated them would invite something to
/// normalise a value whose whole purpose is to survive unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Reasoning {
    /// The thinking as prose, where the provider gave us prose.
    #[serde(default)]
    pub text: Option<String>,
    /// Provider-opaque fields that must be quoted back exactly as they came --
    /// a signature, an encrypted trace. Never parsed, never rewritten: it is
    /// carried, not understood.
    #[serde(default)]
    pub opaque: Option<serde_json::Value>,
}

impl Reasoning {
    /// True when there is nothing here worth sending, so a caller can skip it
    /// rather than emit an empty object a gateway may reject.
    pub fn is_empty(&self) -> bool {
        self.text.as_ref().is_none_or(|t| t.trim().is_empty()) && self.opaque.is_none()
    }
}

impl ToolCall {
    /// Records a call as made *now* and still in flight, for the same reason as
    /// [`Message::new`].
    pub fn started(
        id: impl Into<String>,
        tool: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            tool: tool.into(),
            input,
            outcome: None,
            at: Timestamp::now(),
            batch: None,
            reasoning: None,
            narration: None,
        }
    }

    /// Marks this call as part of one reply, optionally with the thinking and
    /// narration that came with it.
    ///
    /// Builder rather than more parameters on [`ToolCall::started`]: every
    /// caller records a call, and only the one driving a provider has any of
    /// this to say.
    pub fn in_reply(
        mut self,
        batch: impl Into<String>,
        reasoning: Option<Reasoning>,
        narration: Option<String>,
    ) -> Self {
        self.batch = Some(batch.into());
        // An empty reasoning is dropped rather than stored: it would serialise
        // into every plan document for no gain and read as "the model thought
        // nothing" rather than "the provider sent nothing".
        self.reasoning = reasoning.filter(|r| !r.is_empty());
        self.narration = narration.filter(|n| !n.trim().is_empty());
        self
    }

    /// True when this call and `other` came from the same reply.
    ///
    /// Two calls with no batch are never grouped, even though they are equal as
    /// `None`: a record written before batches existed says nothing about how
    /// its calls arrived, and guessing they were together would invent a
    /// deliberation that may not have happened.
    pub fn same_reply_as(&self, other: &ToolCall) -> bool {
        match (&self.batch, &other.batch) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// True while this call is still running.
    pub fn in_flight(&self) -> bool {
        self.outcome.is_none()
    }

    /// What the model should be told this call produced.
    ///
    /// A refusal is reported to the model as text rather than withheld, because
    /// "that path is outside the workspace" is information it can act on: the
    /// alternative is a model that retries the same rejected call forever,
    /// having been told only silence.
    pub fn report(&self) -> &str {
        match &self.outcome {
            Some(ToolOutcome::Done { output, .. }) => output,
            Some(ToolOutcome::Refused { reason }) => reason,
            None => "",
        }
    }

    /// What this call produced that a model could look at.
    ///
    /// Empty for all but a handful of tools, and empty for every call still in
    /// flight. A provider that cannot carry an image ignores this and sends
    /// [`ToolCall::report`] alone, which is why the two are separate accessors.
    pub fn shown(&self) -> &[ToolImage] {
        match &self.outcome {
            Some(ToolOutcome::Done { images, .. }) => images,
            _ => &[],
        }
    }
}

/// An image a tool produced, for a model with eyes.
///
/// Base64 rather than bytes because this type crosses the wasm boundary and is
/// serialised into a plan document; a `Vec<u8>` would become a JSON array of
/// integers, which is several times larger than the base64 it is trying to
/// avoid. No `data:` prefix -- the media type is a field, and gluing the two
/// together is the wire format's job, not the domain's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolImage {
    /// `image/png`, `image/jpeg`, and so on.
    pub media_type: String,
    /// The image, base64-encoded.
    pub data: String,
}

/// How a tool call ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolOutcome {
    /// The tool ran. Note that a command exiting non-zero is still `Done` -- a
    /// failing test suite is a successful tool call with bad news in it, and
    /// conflating the two would have the conversation cry error over exactly
    /// the result the user asked for.
    Done {
        output: String,
        /// Pictures the tool produced.
        ///
        /// A separate channel from `output` rather than encoded into it. The
        /// text is what the conversation renders and what a model without
        /// vision is told; a megabyte of base64 spliced into it would be
        /// unreadable in the one place and useless in the other. Keeping them
        /// apart is also what lets the store drop the pictures and keep the
        /// words -- see `store.rs`.
        ///
        /// Almost every tool leaves this empty, so it is skipped on the wire:
        /// a document written before this field existed still loads, and one
        /// written after is not littered with `"images": []`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ToolImage>,
    },
    /// The tool would not run: unknown name, unparseable arguments, or a path
    /// outside the workspace.
    Refused { reason: String },
}

impl ToolOutcome {
    /// A tool that ran and produced words.
    ///
    /// The constructor exists so that the next field added to `Done` is one
    /// line of change rather than forty. Nearly every tool wants this; the two
    /// that have pictures to show reach for [`ToolOutcome::seen`] instead.
    pub fn done(output: impl Into<String>) -> Self {
        ToolOutcome::Done {
            output: output.into(),
            images: Vec::new(),
        }
    }

    /// A tool that ran and produced something to look at.
    ///
    /// The words are still required: they are what the conversation shows, what
    /// the store keeps, and what a model that cannot see is given instead.
    pub fn seen(output: impl Into<String>, images: Vec<ToolImage>) -> Self {
        ToolOutcome::Done {
            output: output.into(),
            images,
        }
    }

    /// Suffix for the CSS class the conversation styles a settled tool call
    /// with.
    pub fn css_suffix(&self) -> &'static str {
        match self {
            ToolOutcome::Done { .. } => "done",
            ToolOutcome::Refused { .. } => "refused",
        }
    }

    /// The same outcome with nothing to look at.
    ///
    /// For the write path: a plan's record on disk keeps what was said about an
    /// image, not the image. See the note in `store.rs`.
    pub fn without_images(self) -> Self {
        match self {
            ToolOutcome::Done { output, .. } => ToolOutcome::done(output),
            refused => refused,
        }
    }
}

/// One turn of the exchange between the user and the model: the entries that
/// are addressed to a model, and only those.
///
/// This exists so that "what goes to the model" is a type rather than a filter
/// each caller remembers to apply. A [`Note`] cannot be built into one, so a
/// caller downstream of [`Plan::turns`] cannot replay Kingdom's own plumbing to
/// a model as its prior words even by accident.
///
/// Owned rather than borrowed because its one consumer builds a request inside
/// a detached task that outlives the lock the plan was read under. Cloning a
/// transcript costs a handful of small allocations on the way into an HTTP call
/// to a language model, which is not a cost worth a lifetime parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    Message(Message),
    Tool(ToolCall),
}

/// Something a participant said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub speaker: Speaker,
    pub body: String,
    /// When these words entered the log. See [`Timestamp`].
    #[serde(default)]
    pub at: Option<Timestamp>,
}

impl Message {
    /// Records words as said *now*, which is the only way a caller should make
    /// one: a message whose time is chosen by hand is a message that can
    /// disagree with its own position in the log.
    pub fn new(speaker: Speaker, body: impl Into<String>) -> Self {
        Self {
            speaker,
            body: body.into(),
            at: Timestamp::now(),
        }
    }
}

/// Something that happened, reported by Kingdom itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub kind: NoteKind,
    pub body: String,
    /// When this happened. See [`Timestamp`].
    #[serde(default)]
    pub at: Option<Timestamp>,
}

impl Note {
    /// Records something as having happened *now*, for the same reason as
    /// [`Message::new`].
    pub fn new(kind: NoteKind, body: impl Into<String>) -> Self {
        Self {
            kind,
            body: body.into(),
            at: Timestamp::now(),
        }
    }
}

/// When something entered a plan's log: milliseconds since the Unix epoch, UTC.
///
/// A bare integer rather than a date type, because this crate compiles to wasm
/// and every calendar crate wants a clock the browser does not hand out the
/// same way. Turning this into a local wall-clock time is the browser's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The current time, where there is a clock to read.
    ///
    /// `None` on wasm, and that absence is deliberately not papered over with a
    /// zero: **the browser never authors a log entry**. Every message and note
    /// is made server-side, so the wasm arm is unreachable in practice, and a
    /// `0` sentinel would render an impossible line as "01:00, 1 Jan 1970"
    /// rather than as the missing thing it actually is.
    pub fn now() -> Option<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| Timestamp(d.as_millis() as i64))
        }

        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteKind {
    /// The model could not be reached, or refused.
    Failed,
    /// Where this plan is working, and how it was prepared.
    Workspace,
    /// What happened when the user moved to finish the plan: work landing, a
    /// conflict git refused, a worktree disposed of.
    Merge,
}

impl NoteKind {
    /// Suffix for the CSS class the conversation styles a note with.
    pub fn css_suffix(self) -> &'static str {
        match self {
            NoteKind::Failed => "failed",
            NoteKind::Workspace => "workspace",
            NoteKind::Merge => "merge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Speaker {
    /// The user.
    User,
    /// The model drafting the plan.
    Assistant,
}

/// Where a plan stands.
///
/// This absorbs what a separate architect status used to carry: `Drafting` is
/// an agent working, `Failed` is one that could not finish. They were always
/// two views of one state machine.
///
/// There was a `Blocked` variant, for a plan that could not get a lease. It
/// went when lease arbitration did -- nothing could produce it any more, and an
/// unreachable state is a trap for whoever matches on it next. It comes back if
/// and when plans can genuinely block each other.
///
/// `Approved` and `Rejected` went the same way, and were replaced rather than
/// joined by the two states below. They named a judgement nobody could pass:
/// only `sample.rs` ever produced them, because there was no code path by which
/// the user could approve anything. `Merged` and `Archived` name what actually
/// happens to a branch, and both are reachable from the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlanStatus {
    /// A model is drafting it right now.
    Drafting,
    /// Drafted, and waiting on the user.
    AwaitingReview,
    /// The model could not be reached, or refused.
    Failed,
    /// Its work landed on the branch it was cut from. The worktree is gone.
    Merged,
    /// Set aside, with its work preserved. The worktree is gone.
    Archived,
}

impl PlanStatus {
    /// Every state, in the order the map legend lists them: live states first,
    /// then settled history.
    pub const ALL: [PlanStatus; 5] = [
        PlanStatus::Drafting,
        PlanStatus::AwaitingReview,
        PlanStatus::Failed,
        PlanStatus::Merged,
        PlanStatus::Archived,
    ];

    /// True once the plan is history rather than still in play.
    ///
    /// The single definition of "settled". The rail's filter, the map's plan
    /// list and the guards on `say`/`draft` all read it, so a sixth state cannot
    /// be added and quietly treated as live by one of them.
    pub fn is_settled(&self) -> bool {
        matches!(self, PlanStatus::Merged | PlanStatus::Archived)
    }

    pub fn label(&self) -> &'static str {
        match self {
            PlanStatus::Drafting => "Drafting",
            PlanStatus::AwaitingReview => "Awaiting review",
            PlanStatus::Failed => "Failed",
            PlanStatus::Merged => "Merged",
            PlanStatus::Archived => "Archived",
        }
    }

    pub fn color(&self) -> &'static str {
        match self {
            PlanStatus::Drafting => "#22c55e",
            PlanStatus::AwaitingReview => "#eab308",
            PlanStatus::Failed => "#f97316",
            PlanStatus::Merged => "#38bdf8",
            PlanStatus::Archived => "#64748b",
        }
    }

    /// CSS class suffix, e.g. `status-drafting`.
    pub fn css_suffix(&self) -> &'static str {
        match self {
            PlanStatus::Drafting => "drafting",
            PlanStatus::AwaitingReview => "review",
            PlanStatus::Failed => "failed",
            PlanStatus::Merged => "merged",
            PlanStatus::Archived => "archived",
        }
    }
}

/// What the user chose to do with a plan when he closed it.
///
/// Crosses the wire, so it lives here rather than in the server. Two options
/// because there are two honest endings: the work belongs in the project, or it
/// does not and should be kept somewhere it can be found again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    /// Land the work on the branch the workspace was cut from.
    Merge,
    /// Set the work aside, preserved, and reclaim the checkout.
    Archive,
}

/// How a plan ended, and what it left behind.
///
/// Held separately from [`PlanStatus`] because the two answer different
/// questions. A status is a `Copy` label the rail and the map paint; this is the
/// *evidence* -- the sha to `git show`, the branch to restore from. Folding the
/// detail into the status enum would make every match on state carry a payload
/// it does not want, and would cost `PlanStatus` its `Copy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// The work landed. `commit` is the merge commit, `into` the branch it
    /// landed on.
    Merged { commit: String, into: String },
    /// The work was set aside but kept. Everything needed to bring it back is
    /// here: a patch of `base..tip` on disk at `patch`, and `base_commit` --
    /// the sha it was cut from, which `base` (a branch *name*) will have
    /// wandered away from by the time anyone restores.
    ///
    /// `pruned` says whether the branch was reclaimed along with the checkout.
    /// It is only ever true when a patch was actually written, so the work is
    /// recoverable either way -- but the user must not be told to check out a
    /// branch that is no longer there.
    Archived {
        branch: String,
        tip: String,
        base: String,
        base_commit: String,
        patch: Option<String>,
        pruned: bool,
    },
}

impl Outcome {
    /// How the conversation's footer states what became of the plan.
    pub fn summary(&self) -> String {
        match self {
            Outcome::Merged { commit, into } => {
                format!("Merged into {into} as {}.", short_sha(commit))
            }
            Outcome::Archived {
                branch,
                tip,
                pruned,
                ..
            } => {
                if *pruned {
                    format!("Archived at {}, kept as a patch.", short_sha(tip))
                } else {
                    format!("Archived on {branch}, at {}.", short_sha(tip))
                }
            }
        }
    }
}

/// Abbreviates a sha the way git does, leaving anything shorter untouched.
fn short_sha(sha: &str) -> &str {
    match sha.char_indices().nth(7) {
        Some((i, _)) => &sha[..i],
        None => sha,
    }
}

// ---------------------------------------------------------------------------
// Model access -- what the user can see about how plans get drafted
// ---------------------------------------------------------------------------

/// Whether a credential could be obtained.
///
/// This is a *description* of the credential's state, never the credential
/// itself -- it crosses the wire to the browser, so it must stay free of
/// anything secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialState {
    /// Not needed, or successfully obtained.
    Ready,
    /// Nothing configured to obtain one with.
    Missing,
    /// Something was configured, and it failed.
    Failed,
}

/// How hard a model is asked to think.
///
/// Deliberately *not* a number: providers name discrete levels and accept only
/// the ones a given model declares, so an ordering with arbitrary intermediate
/// values would invent requests no gateway would accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ModelEffort {
    /// Every level, weakest first. Used to keep the picker's row in a stable,
    /// meaningful order regardless of what order a provider lists them in.
    pub const ALL: [ModelEffort; 7] = [
        ModelEffort::None,
        ModelEffort::Minimal,
        ModelEffort::Low,
        ModelEffort::Medium,
        ModelEffort::High,
        ModelEffort::Xhigh,
        ModelEffort::Max,
    ];

    /// The name the provider expects on the wire.
    pub fn wire_name(&self) -> &'static str {
        match self {
            ModelEffort::None => "none",
            ModelEffort::Minimal => "minimal",
            ModelEffort::Low => "low",
            ModelEffort::Medium => "medium",
            ModelEffort::High => "high",
            ModelEffort::Xhigh => "xhigh",
            ModelEffort::Max => "max",
        }
    }

    /// Parses a level as a provider spells it. Unknown levels are dropped
    /// rather than guessed at: sending an invented one earns an opaque 400.
    pub fn from_wire(name: &str) -> Option<ModelEffort> {
        ModelEffort::ALL
            .into_iter()
            .find(|e| e.wire_name().eq_ignore_ascii_case(name.trim()))
    }
}

/// What a prompt is drafted with.
///
/// `effort: None` means *the model's own default*, which is a different request
/// from any explicit level -- the field is omitted entirely rather than sent as
/// `"none"`, which is itself a level some models accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// Namespaced model id, e.g. `"copilot/claude-opus-5"` or `"mock"`.
    pub model: String,
    pub effort: Option<ModelEffort>,
}

impl ModelChoice {
    pub fn new(model: impl Into<String>, effort: Option<ModelEffort>) -> Self {
        Self {
            model: model.into(),
            effort,
        }
    }

    /// Which backend serves this model. Derived from the id rather than held
    /// separately, so the backend and the model can never disagree -- a plan
    /// drawn by Copilot cannot be re-drafted by the mock because some other
    /// setting drifted.
    ///
    /// The segment before the `/`, or the whole id when there is none:
    /// `copilot/claude-opus-5` is served by `copilot`, `mock` by `mock`.
    pub fn namespace(&self) -> &str {
        match self.model.split_once('/') {
            Some((namespace, _)) => namespace,
            None => &self.model,
        }
    }

    /// The name the provider knows this model by, with the namespace stripped.
    pub fn api_name(&self) -> &str {
        match self.model.split_once('/') {
            Some((_, name)) => name,
            None => &self.model,
        }
    }

    /// How the choice reads in the rail and on the map, e.g.
    /// `claude-opus-5 · high`.
    pub fn label(&self) -> String {
        match self.effort {
            Some(effort) => format!("{} \u{b7} {}", self.api_name(), effort.wire_name()),
            None => self.api_name().to_string(),
        }
    }
}

/// One selectable model, as the picker renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    /// Namespaced id, matching [`ModelChoice::model`].
    pub id: String,
    /// Human name, e.g. `"Claude Opus 5"`.
    pub label: String,
    /// Who makes it, for grouping: `"Anthropic"`, `"OpenAI"`, `"Offline"`.
    pub vendor: String,
    pub context_window: usize,
    /// Surfaced above the fold, before the user expands the full list.
    pub recommended: bool,
    /// The effort levels this model declares. Empty means it has no effort
    /// control at all, and the picker hides the row rather than offering
    /// levels that would be refused.
    pub efforts: Vec<ModelEffort>,
    /// Whether this model can be handed tools.
    ///
    /// A model that cannot is still offered -- it drafts perfectly good prose,
    /// and the user choosing a cheaper model should get a weaker answer rather
    /// than an error. What it changes is the request: sending tools to a model
    /// that does not take them earns an opaque rejection from the gateway, so
    /// the turn is built without them and the system prompt says so.
    ///
    /// Defaults to false on the wire so a record written before this field
    /// existed degrades to the safe direction: prose, rather than a request the
    /// gateway refuses.
    #[serde(default)]
    pub can_act: bool,
    /// Whether this model can be shown a picture.
    ///
    /// Same shape and same asymmetry as [`ModelOption::can_act`]: a model that
    /// cannot see is still perfectly good at everything else, so it stays in
    /// the picker and is simply never offered `read_image`. Sending an image to
    /// one that cannot take it fails the whole turn with a gateway error, while
    /// withholding it from one that could merely means the user's model works
    /// the way it did last week. The costs of guessing wrong are not
    /// symmetric, so absent is taken as "no".
    #[serde(default)]
    pub can_see: bool,
    /// The most output tokens this model will produce in one reply, as its
    /// catalogue entry declared.
    ///
    /// Reasoning is billed against this budget, so it is not merely "how long
    /// an answer can be": a model at high effort can spend the whole of a small
    /// budget thinking and return empty content, which reads as a refusal and
    /// fails the plan. A fixed constant here was wrong in both directions --
    /// too small for a reasoning model, wasteful for a small one -- and went
    /// stale as the catalogue moved.
    ///
    /// **Absent means fall back, not drop.** [`ModelOption::context_window`]
    /// treats a missing value as grounds to omit the model entirely, reasoning
    /// that a guess is something the user acts on. That does not apply here:
    /// this number is never shown to anyone, it only sizes a request, and
    /// hiding a working model over a field its provider declined to declare
    /// would be the worse outcome. So `None` is a usable state and the caller
    /// picks a generous default.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

/// Everything the picker needs, plus why it might be shorter than expected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogue {
    pub options: Vec<ModelOption>,
    /// What a user who has never chosen gets.
    pub default_id: String,
    pub credential: CredentialState,
    /// Plain-language detail: where the catalogue came from, or why it is thin.
    pub detail: String,
}

impl ModelCatalogue {
    pub fn option(&self, id: &str) -> Option<&ModelOption> {
        self.options.iter().find(|o| o.id == id)
    }

    /// Resolves a remembered choice against what is actually available now.
    ///
    /// A model that has left the catalogue, or an effort it no longer declares,
    /// degrades to the nearest valid thing instead of erroring. The user's
    /// browser storage outlives any given catalogue, so a stale value there must
    /// never be able to wedge the dock.
    pub fn resolve(&self, wanted: Option<&ModelChoice>) -> ModelChoice {
        let id = wanted
            .map(|c| c.model.as_str())
            .filter(|id| self.option(id).is_some())
            .unwrap_or(&self.default_id)
            .to_string();

        // Only keep an effort the chosen model actually offers -- an effort
        // carried over from a different model is exactly how you earn a 400.
        let effort = wanted.and_then(|c| c.effort).filter(|e| {
            self.option(&id)
                .is_some_and(|option| option.efforts.contains(e))
        });

        ModelChoice { model: id, effort }
    }
}

// ---------------------------------------------------------------------------
// Crown Resources & Leases -- removed, deliberately
// ---------------------------------------------------------------------------
//
// This file used to carry `Resource`, `ResourceKind`, `Lease`, `LeaseMode` and
// a lease compatibility matrix, on the theory that arbitrating shared machine
// resources was the core of the product.
//
// They were removed because they never arbitrated anything. Exactly one code
// path ever took a lease (drafting, to read a city), it only ever asked for
// `Shared`, and shared always composes with shared -- so a refusal required an
// `Exclusive` holder that no runtime code ever created. Every blocked plan and
// every red thread on the map came from fabricated sample data. A mechanism
// nobody can reach is worse than no mechanism: it invites building on a
// guarantee that was never enforced.
//
// What survives is the half that was real: a plan records what it is busy with
// while it works. See `Plan::working_on`.
//
// Arbitration earns its place back the moment plans get hands -- running a
// command, binding a port, writing a file. That is when two plans can genuinely
// collide, and it should be rebuilt then, against a real collision, rather than
// kept warm for a caller that does not exist yet.

#[cfg(test)]
mod tests {
    use super::*;
    fn catalogue() -> ModelCatalogue {
        ModelCatalogue {
            options: vec![
                ModelOption {
                    id: "mock".into(),
                    label: "Mock (offline)".into(),
                    vendor: "Offline".into(),
                    context_window: 0,
                    recommended: false,
                    efforts: Vec::new(),
                    can_act: true,
                    can_see: false,
                    max_output_tokens: None,
                },
                ModelOption {
                    id: "copilot/claude-opus-5".into(),
                    label: "Claude Opus 5".into(),
                    vendor: "Anthropic".into(),
                    context_window: 1_000_000,
                    recommended: true,
                    efforts: vec![ModelEffort::Low, ModelEffort::High],
                    can_act: true,
                    can_see: true,
                    max_output_tokens: Some(64_000),
                },
            ],
            default_id: "copilot/claude-opus-5".into(),
            credential: CredentialState::Ready,
            detail: String::new(),
        }
    }

    /// The user's browser remembers a choice for longer than any catalogue
    /// lives. A withdrawn model or an effort a model no longer declares must
    /// degrade quietly -- if either could error, last week's localStorage would
    /// wedge today's dock, and sending an undeclared effort earns an opaque 400.
    ///
    /// The invariant is precisely: *the resolved choice never carries an effort
    /// the resolved model does not declare.* Not "a fallback drops the effort" --
    /// that was only ever true by accident, back when the fallback was the mock
    /// and the mock declares no efforts at all.
    #[test]
    fn a_stale_remembered_choice_degrades_rather_than_erroring() {
        let catalogue = catalogue();

        let withdrawn = ModelChoice::new("copilot/gone-last-year", Some(ModelEffort::High));
        assert_eq!(
            catalogue.resolve(Some(&withdrawn)),
            ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::High)),
            "an unknown model falls back to the default, keeping an effort that \
             default also declares"
        );

        let undeclared = ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::Max));
        assert_eq!(
            catalogue.resolve(Some(&undeclared)),
            ModelChoice::new("copilot/claude-opus-5", None),
            "an effort the model does not declare falls back to the model's own default"
        );

        // The two degradations compounding: an unknown model *and* an effort the
        // fallback does not declare. This is the case that would reach the wire
        // as a 400 if `resolve` checked the effort against the remembered model
        // rather than the resolved one.
        let doubly_stale = ModelChoice::new("copilot/gone-last-year", Some(ModelEffort::Max));
        assert_eq!(
            catalogue.resolve(Some(&doubly_stale)),
            ModelChoice::new("copilot/claude-opus-5", None)
        );

        let good = ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::Low));
        assert_eq!(catalogue.resolve(Some(&good)), good);
        assert_eq!(
            catalogue.resolve(None),
            ModelChoice::new("copilot/claude-opus-5", None)
        );
    }

    /// The backend is read off the id so the two cannot disagree; a plan drawn
    /// by Copilot must never be re-drafted by the mock because a separate
    /// setting drifted.
    #[test]
    fn a_choice_routes_by_its_own_id() {
        let copilot = ModelChoice::new("copilot/claude-opus-5", None);
        assert_eq!(copilot.namespace(), "copilot");
        assert_eq!(copilot.api_name(), "claude-opus-5");

        // The mock is not special-cased: a bare id is its own namespace, which
        // is what lets it be listed and chosen like any other model.
        let mock = ModelChoice::new("mock", None);
        assert_eq!(mock.namespace(), "mock");
        assert_eq!(mock.api_name(), "mock");
    }
}

#[cfg(test)]
mod proposal_tests {
    use super::*;

    fn proposing() -> Plan {
        let mut plan = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Fix the parser",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );
        plan.propose("Fix the off-by-one", "Change `lex.rs` line 42.");
        plan.status = PlanStatus::AwaitingReview;
        plan
    }

    /// The whole of the standing-proposal state machine, in the order a plan
    /// actually moves through it.
    ///
    /// Every branch here is one the conversation view renders, and getting any
    /// of them wrong shows the user a button that lies. The last is the subtle
    /// one: a proposal the user has *not* accepted is cleared when the model
    /// speaks
    /// again, because otherwise a plan that proposed, was asked a question, and
    /// answered it in prose would still be offering to start work the
    /// conversation had already moved past. An accepted proposal survives that,
    /// because it is no longer a question -- it is the work in progress, and
    /// the header names it for the rest of the plan's life.
    #[test]
    fn a_proposal_stands_until_it_is_accepted_or_superseded() {
        let mut plan = proposing();
        assert!(
            plan.standing_proposal().is_some(),
            "a fresh proposal awaiting review is the question on the table"
        );

        // Still drafting means the model may be mid-revision: nothing to
        // decide on yet.
        plan.status = PlanStatus::Drafting;
        assert!(plan.standing_proposal().is_none());
        plan.status = PlanStatus::AwaitingReview;

        // The user accepts. The question is settled and the tools are granted.
        assert!(plan.approve(), "there was a proposal to accept");
        assert_eq!(plan.permissions, Permissions::Full);
        assert!(
            plan.standing_proposal().is_none(),
            "an accepted proposal is the work, not a decision to make"
        );
        assert!(plan.approved_proposal().is_some());

        // Speaking does not revoke work already approved.
        plan.set_aside_proposal();
        assert!(
            plan.approved_proposal().is_some(),
            "the plan being carried out must survive the model speaking"
        );

        // An unapproved one does not survive it.
        let mut fresh = proposing();
        fresh.set_aside_proposal();
        assert!(
            fresh.proposal.is_none(),
            "an unaccepted proposal is superseded when the model speaks again"
        );
        assert_eq!(
            fresh.permissions,
            Permissions::Propose,
            "setting a proposal aside must not hand over the tools"
        );
        assert!(
            !fresh.approve(),
            "approving nothing must be reported, not silently granted"
        );
        assert_eq!(fresh.permissions, Permissions::Propose);
    }

    /// A plan recorded before proposals existed keeps the tools it was drafted
    /// with. The opposite default would strand every plan already on disk: no
    /// proposal to accept, and no `patch` to make one with.
    #[test]
    fn a_plan_recorded_before_proposals_still_has_its_tools() {
        let before_proposals = r#"{
            "id": "plan-1",
            "city": "c1",
            "title": "Old work",
            "summary": "",
            "prompt": "Do the thing",
            "model": "mock",
            "effort": null,
            "transcript": [],
            "status": "AwaitingReview",
            "workspace": {
                "mode": "InPlace",
                "path": "/dev/testburg",
                "branch": null,
                "id": null
            },
            "working_on": null
        }"#;

        let plan: Plan =
            serde_json::from_str(before_proposals).expect("an older plan record must still load");

        assert_eq!(plan.permissions, Permissions::Full);
        assert!(plan.proposal.is_none());
        assert!(
            plan.standing_proposal().is_none(),
            "an old plan must not sprout a proposal card it can never satisfy"
        );
    }
}

#[cfg(test)]
mod transcript_tests {
    use super::*;

    /// Kingdom's own notices must never reach a model.
    ///
    /// This is the bug the `Entry`/`Note` split exists to make unrepresentable.
    /// When every log line was a message, app notices and failed calls were
    /// stored as things the *model* had said, and the next turn replayed them
    /// to it as its own prior words -- teaching it to answer in the voice of
    /// the plumbing. `messages()` is the only door between a plan's log and a
    /// model, so it is the thing worth pinning: notes never come through it,
    /// ordering survives, and the last user turn is still findable as the live
    /// prompt.
    #[test]
    fn notes_never_reach_the_model_and_the_prompt_survives_them() {
        let mut plan = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "First question",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );
        plan.note(
            NoteKind::Workspace,
            "Working in /dev/testburg/.kingdom/abc.",
        );
        plan.say(Speaker::Assistant, "First answer");
        plan.note(NoteKind::Failed, "The first request failed.");
        plan.say(Speaker::User, "Second question");

        let messages: Vec<_> = plan.messages().cloned().collect();
        assert_eq!(
            messages.iter().map(|u| u.body.as_str()).collect::<Vec<_>>(),
            vec!["First question", "First answer", "Second question"],
            "only utterances pass, and they keep their order"
        );

        // The prompt is the last thing the user said, even though a note landed
        // between the turns.
        let i = messages
            .iter()
            .rposition(|u| u.speaker == Speaker::User)
            .expect("the King has spoken");
        assert_eq!(messages[i].body, "Second question");
    }

    /// The same exclusion, now that the log holds a third kind of thing. A tool
    /// call must reach the model (a tool result it never sees is a tool call it
    /// makes again) while a note still must not, and both must arrive in the
    /// order they happened -- a provider that sees a result before its call is
    /// rebuilding a conversation that never took place.
    #[test]
    fn tool_calls_reach_the_model_in_order_and_notes_still_do_not() {
        let mut plan = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Run the tests",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );
        plan.begin_tool_call(ToolCall::started(
            "call-1",
            "bash",
            serde_json::json!({ "cmd": "cargo test" }),
        ));
        plan.note(NoteKind::Failed, "The disk filled up.");
        assert!(plan.settle_tool_call("call-1", ToolOutcome::done("ok")));
        plan.say(Speaker::Assistant, "The tests pass.");

        let turns: Vec<_> = plan
            .turns()
            .map(|t| match t {
                Turn::Message(u) => format!("said:{}", u.body),
                Turn::Tool(d) => format!("did:{}:{}", d.tool, d.report()),
            })
            .collect();

        assert_eq!(
            turns,
            vec![
                "said:Run the tests",
                "did:bash:ok",
                "said:The tests pass.",
            ],
            "a deed reaches the model, in place; a note does not reach it at all"
        );
    }

    /// Plans are the one thing disk cannot tell us again, so a record written
    /// before tool calls existed must still load. `Entry` is externally tagged,
    /// which makes a new variant additive -- but "should be additive" and "is"
    /// are different claims, and the cost of being wrong is a kingdom that will
    /// not open.
    #[test]
    fn a_plan_recorded_before_the_model_had_hands_still_loads() {
        let before_tool_calls = r#"{
            "id": "plan-old",
            "city": "c1",
            "title": "An older plan",
            "summary": "Drawn up before any of this",
            "prompt": "Do the thing",
            "model": "mock",
            "effort": null,
            "transcript": [
                { "Message": { "speaker": "User", "body": "Do the thing", "at": 1 } },
                { "Note": { "kind": "Workspace", "body": "Working in /tmp", "at": 2 } }
            ],
            "status": "AwaitingReview",
            "workspace": {
                "mode": "InPlace",
                "path": "/dev/testburg",
                "branch": null,
                "id": null
            },
            "working_on": null
        }"#;

        let plan: Plan =
            serde_json::from_str(before_tool_calls).expect("an older plan record must still load");

        assert_eq!(plan.transcript.len(), 2);
        assert_eq!(
            plan.turns().count(),
            1,
            "the note is still excluded, and nothing invented a deed that never happened"
        );
    }

    /// A subagent is a plan the user never asked for, so the readers that build
    /// *his* views must not show it -- while the reader the parent's
    /// conversation uses must find exactly the subagents of the call that sent
    /// them.
    ///
    /// Worth one test because this is a filter applied in several places from
    /// one predicate: the map reads `plans_in`, the rail reads `plans` with the
    /// same condition, and the tool call line reads `subagents_of`. The failure
    /// is silent in both directions -- a subagent in the rail is clutter, a
    /// subagent missing from `subagents_of` is a conversation that shows a call
    /// sending nothing.
    #[test]
    fn subagents_are_hidden_from_the_users_views_and_found_by_their_call() {
        let city = CityId::new("c1");
        let parent = Plan::opened(
            PlanId::new("plan-1"),
            city.clone(),
            "Work out what is slow",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );

        let mut first = Plan::spawned(PlanId::new("plan-2"), &parent, "call-1", "Read the parser");
        first.status = PlanStatus::AwaitingReview;
        let second = Plan::spawned(PlanId::new("plan-3"), &parent, "call-1", "Read the loader");
        // A second round of subagents, under a different call.
        let later = Plan::spawned(PlanId::new("plan-4"), &parent, "call-2", "Read the cache");

        let kingdom = Kingdom {
            name: "Testburg".into(),
            root: "/dev".into(),
            cities: Vec::new(),
            plans: vec![parent.clone(), first, second, later],
            sandbox: false,
        };

        assert_eq!(
            kingdom.plans_in(&city).map(|p| p.id.clone()).collect::<Vec<_>>(),
            vec![PlanId::new("plan-1")],
            "the map must draw the decreed plan only: errands share its worktree, \
             so drawing them too would count one piece of work four times"
        );

        assert_eq!(
            kingdom.pending_plans().count(),
            0,
            "an errand reports to the court that sent it, never to the King"
        );

        assert_eq!(
            kingdom
                .subagents_of(&parent.id, "call-1")
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
            vec![PlanId::new("plan-2"), PlanId::new("plan-3")],
            "a call finds its own errands, in the order they were sent, and not \
             the ones a later call sent"
        );
    }

    /// Plans are the one thing disk cannot tell us again, and `spawned_by` is a
    /// new field on a type that is already recorded. Additive-serde is a claim,
    /// not a fact, and the cost of being wrong is a kingdom that will not open.
    #[test]
    fn a_plan_recorded_before_subagents_existed_still_loads() {
        let before_subagents = r#"{
            "id": "plan-old",
            "city": "c1",
            "title": "An older plan",
            "summary": "Drawn up before the court could send anyone",
            "prompt": "Do the thing",
            "model": "mock",
            "effort": null,
            "transcript": [
                { "Message": { "speaker": "User", "body": "Do the thing", "at": 1 } }
            ],
            "status": "AwaitingReview",
            "workspace": {
                "mode": "InPlace",
                "path": "/dev/testburg",
                "branch": null,
                "id": null
            },
            "working_on": null
        }"#;

        let plan: Plan =
            serde_json::from_str(before_subagents).expect("an older plan record must still load");

        assert!(
            !plan.is_subagent(),
            "a plan recorded before errands existed is the King's own work, and \
             must not be mistaken for something the court sent"
        );
    }
}
