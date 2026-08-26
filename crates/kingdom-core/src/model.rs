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

    /// This kingdom as a browser should receive it.
    ///
    /// [`Plan::for_wire`] applied to every plan, and the reason to have it here
    /// as well is that the opening fetch is the *largest* single transfer in
    /// the app: it carries every plan at once, where a push carries one. On a
    /// real kingdom that was a 13.9 MB page.
    pub fn for_wire(&self) -> Self {
        Self {
            plans: self.plans.iter().map(Plan::for_wire).collect(),
            ..self.clone()
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

/// One rung of a directory listing: a single name inside a folder.
///
/// Deliberately **not** a variant of [`Folder`]. A `Folder` is a whole recursive
/// subtree carrying aggregate weights, built once by the scanner for the map;
/// this is one flat rung of a ladder, fetched on demand as the King opens a
/// folder in the files rail. Nothing here is aggregated and nothing is pruned,
/// which is the entire difference: the map's tree keeps the *largest* files and
/// the files rail must keep *every* file or it is lying about the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    /// Path relative to the city root, which is what identifies this entry and
    /// what is handed back to list a directory one level down.
    pub path: String,
    pub is_dir: bool,
    /// What tints the entry, reusing the map's own language colours so a `.rs`
    /// file reads the same in the rail as it does on the skyline. Meaningless
    /// for a directory, where it is [`Language::Other`].
    pub language: Language,
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
            + self.children.iter().map(Folder::total_files).sum::<usize>()
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
    /// What the user said while a turn was in flight, in the order they said
    /// it, waiting to be heard.
    ///
    /// Deliberately **not** in the transcript. The transcript is what was said
    /// and done; these are words nobody has heard yet. Keeping them apart is
    /// what lets the conversation draw them as pending, lets the user withdraw
    /// one before it lands, and -- most importantly -- keeps them out of
    /// [`Plan::turns`], so a turn already in flight cannot pick them up halfway
    /// through a round.
    ///
    /// They are moved across by [`Plan::hear_queued`], which the turn loop
    /// calls at a round boundary: the one moment where nothing is half-done.
    ///
    /// `#[serde(default)]` because every plan record already on disk predates
    /// the field.
    #[serde(default)]
    pub queued: Vec<QueuedMessage>,
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
    /// How full the model's context window stood at the end of the last turn.
    ///
    /// `None` until the court has answered once -- and for a provider that
    /// reports no usage or declares no window, `None` forever. That is the
    /// honest reading: this is measured, not estimated, and there is nothing to
    /// show until something has actually been sent and counted.
    ///
    /// Overwritten each turn rather than accumulated. A conversation's cost is
    /// not a running total: every turn resends the whole exchange, so the last
    /// count *is* how full the window is.
    #[serde(default)]
    pub context: Option<ContextUsage>,
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
    /// Applied to [`Plan::title`] by [`Plan::propose`]: it is the name the user
    /// read on the card, so it is the name the rail should show. The plan's
    /// `slug` deliberately does not follow it -- that is the git branch, and it
    /// is already cut on disk.
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
    /// What the user has written in the margin, and has not yet sent.
    ///
    /// The same shape and the same reasoning as [`Plan::queued`]: a note typed
    /// but not sent must survive a reload, a second tab and a server restart,
    /// and it is deliberately nowhere near the transcript until it is sent.
    /// Nothing here is ever handed to a model -- [`Plan::take_notes`] is the
    /// only way out, and it empties this in the same breath as composing the
    /// message that carries it.
    ///
    /// Cleared by [`Plan::propose`] when a revision arrives: a note the court
    /// has answered is no longer pending, and the decree that carried it is in
    /// the transcript where the history of a plan lives.
    #[serde(default)]
    pub notes: Vec<ProposalNote>,
    /// The body of the proposal this one revises, when it revises one.
    ///
    /// `None` on a plan's first proposal, which is what the view reads to decide
    /// whether there is anything to show a diff against.
    ///
    /// A whole body rather than a stored diff. The diff is computed for display
    /// and thrown away; keeping one would be a second rendering of prose that
    /// already exists twice, free to drift from both -- the same liability
    /// `AGENTS.md` records against the old `approved/` ledger. Written exactly
    /// once, by [`Plan::propose`], from the body it is replacing.
    #[serde(default)]
    pub revises: Option<String>,
}

/// Something the user wrote against one part of a proposal.
///
/// Its own type rather than a bare string because a note without its anchor is
/// an opinion about an unnamed thing: the whole point is that the court is told
/// *which* paragraph the objection is to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalNote {
    /// Names this note so it can be withdrawn before it is sent. Derived like
    /// [`Plan::queue`]'s id, and for the same purpose.
    pub id: String,
    /// Where the annotated block starts in [`Proposal::body`], 1-based.
    ///
    /// What orders the notes and what puts each one beside the right block
    /// while the card is open. See [`crate::proposal::blocks`].
    pub line: usize,
    /// The annotated text itself, as it stood when the note was written.
    ///
    /// Carried rather than looked up again at sending time, and that is the
    /// point of the field. A line number is a reference into a document that is
    /// about to be replaced: if the court revises while a note is open, line 34
    /// is a different paragraph and the note would be put to the model against
    /// prose the user never read. The text he actually annotated cannot move.
    pub quote: String,
    /// What he wrote.
    pub body: String,
    /// When he wrote it. See [`Timestamp`].
    #[serde(default)]
    pub at: Option<Timestamp>,
}

impl Proposal {
    /// A proposal as the model has just made it: put, not yet accepted.
    pub fn put(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            at: Timestamp::now(),
            approved: false,
            notes: Vec::new(),
            revises: None,
        }
    }

    /// The parts of this proposal the user can write against.
    ///
    /// Here rather than only in the view so the server quotes exactly what the
    /// browser offered -- see [`crate::proposal`] for why one answer to that
    /// question is worth more than two.
    pub fn blocks(&self) -> Vec<crate::proposal::Block> {
        crate::proposal::blocks(&self.body)
    }

    /// This proposal read against the one it revises, or `None` if it is the
    /// first.
    pub fn changes(&self) -> Option<Vec<crate::proposal::DiffLine>> {
        let previous = self.revises.as_deref()?;
        Some(crate::proposal::diff(previous, &self.body))
    }
}

/// How much of a model's context window a conversation is filling.
///
/// Both numbers are carried together, written at the same moment, because
/// separately they lie. A token count is meaningless without the window it was
/// measured against, and a window read from today's catalogue against a count
/// taken last week is a percentage of the wrong thing. Held as one value, they
/// cannot drift apart.
///
/// The count is the *provider's*, never ours -- see [`ContextUsage::percent`]
/// for why an estimate would be worse than nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// Tokens the last turn actually cost, as the provider reported them.
    pub tokens: usize,
    /// The window those tokens were measured against.
    pub window: usize,
}

impl ContextUsage {
    /// How full the window is, rounded, or `None` when there is nothing
    /// truthful to say.
    ///
    /// A window of zero is a real, reachable state rather than a defensive
    /// check: the offline mock declares no window at all, and it is the model
    /// the user lands on whenever no credential works. `None` rather than a
    /// fabricated figure, because a percentage is precisely the sort of number
    /// a reader acts on -- deciding whether to keep going or start again.
    pub fn percent(&self) -> Option<u8> {
        if self.window == 0 {
            return None;
        }
        // Saturating rather than capped after the fact: a provider counting the
        // reply as well as the prompt can report slightly over its own limit,
        // and "104%" reads as a bug in Kingdom rather than as a full window.
        Some(((self.tokens * 100 / self.window).min(100)) as u8)
    }
}

/// A context window as it is written for a reader: `1000K`.
///
/// One spelling, shared by the model picker and the chamber header. Two copies
/// of `w / 1000` is exactly how the number the user chose a model by and the
/// number he later measures a conversation against come to disagree.
///
/// An undeclared window is the empty string rather than `0K`, so a caller that
/// prints it unconditionally says nothing instead of claiming a limit of
/// nothing.
pub fn window_label(tokens: usize) -> String {
    match tokens {
        0 => String::new(),
        w => format!("{}K", w / 1000),
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
            queued: Vec::new(),
            spawned_by: None,
            // A prompt opens under Propose: the model draws up a plan and puts
            // it to the user before it touches anything. This one line is what
            // inverts the product's stance -- the user reviews a proposal
            // rather than a fait accompli.
            permissions: Permissions::Propose,
            proposal: None,
            // Nothing has been sent yet, so there is nothing counted.
            context: None,
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
            // A subagent answers to the model that sent it and renders no
            // composer, so nobody can queue anything against it.
            queued: Vec::new(),
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
            // Its own window, not its parent's: it is a separate conversation
            // with a separate exchange, sharing only the model.
            context: None,
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

    /// Sets words aside to be heard when the court next comes up for air.
    ///
    /// Returns the id naming them, which is how [`Plan::unqueue`] finds them
    /// again. The id is derived from the plan and the queue's length rather
    /// than from a random source, because this crate compiles to wasm and must
    /// not reach for one -- and it only has to be unique among the handful of
    /// messages waiting on *this* plan at *this* moment, which it is: a message
    /// only leaves the queue by being heard or withdrawn, and both are
    /// server-side under the kingdom lock.
    pub fn queue(&mut self, body: impl Into<String>) -> String {
        let at = Timestamp::now();
        let id = format!(
            "{}-{}-{}",
            self.id.as_str(),
            self.queued.len(),
            at.map(|Timestamp(ms)| ms).unwrap_or_default()
        );
        self.queued.push(QueuedMessage {
            id: id.clone(),
            body: body.into(),
            at,
        });
        id
    }

    /// Moves everything queued into the transcript, oldest first, and returns
    /// how many moved.
    ///
    /// The entries are stamped *now* rather than carrying the time they were
    /// queued, because now is when they entered the log -- which is the rule
    /// [`Message::new`] exists to enforce. The moment they were spoken is not
    /// lost: it stays on the [`QueuedMessage`] until this call, which is what
    /// the conversation shows while they wait.
    pub fn hear_queued(&mut self) -> usize {
        let heard = std::mem::take(&mut self.queued);
        let count = heard.len();
        for message in heard {
            self.say(Speaker::User, message.body);
        }
        count
    }

    /// Withdraws queued words before anyone has heard them.
    ///
    /// False when there is no such message, which is not an error: the caller
    /// raced [`Plan::hear_queued`] and lost, so the words are already in the
    /// transcript and withdrawing them would mean editing a conversation that
    /// has happened. The caller should say so rather than swallow it.
    pub fn unqueue(&mut self, id: &str) -> bool {
        let before = self.queued.len();
        self.queued.retain(|message| message.id != id);
        self.queued.len() != before
    }

    /// Puts a plan to the user, replacing whatever was standing before it.
    ///
    /// Replacing rather than accumulating is the point: there is one question
    /// on the table at a time. A revised proposal supersedes the one it
    /// revises, and the superseded version is not lost -- the tool call that
    /// made it is still in the transcript, which is where the history of a plan
    /// lives.
    ///
    /// The proposal also **names the plan**. Until it proposes, a plan is
    /// labelled with the opening decree's first clause, which is a placeholder;
    /// the headline the model put to the user is the first name the work
    /// actually has. Renaming here rather than at the call site means a plan's
    /// name and its standing proposal cannot drift apart, and a revised
    /// proposal retitles for free.
    ///
    /// [`Plan::slug`] is left alone: the branch is already cut on disk under it,
    /// and renaming a branch mid-flight is its own decision.
    ///
    /// The body being replaced is carried onto the new proposal as
    /// [`Proposal::revises`], which is what lets the view show a revision
    /// against the plan the user actually annotated. Taking the old proposal
    /// rather than overwriting it also drops its notes, which is right: a note
    /// this revision answers is no longer pending, and the decree that carried
    /// it is in the transcript.
    pub fn propose(&mut self, title: impl Into<String>, body: impl Into<String>) {
        let previous = self.proposal.take().map(|p| p.body);
        let mut proposal = Proposal::put(title, body);
        proposal.revises = previous;
        self.title = proposal.title.clone();
        self.proposal = Some(proposal);
    }

    /// The user writes a note against one part of the standing proposal.
    ///
    /// Returns the note's id, or `None` when there is nothing standing to
    /// annotate -- a stale tab, or a proposal the court has revised since the
    /// browser drew it. The caller reports that rather than swallowing it: a
    /// note silently written onto nothing is one the user believes he has made.
    ///
    /// Only ever the *standing* proposal. Annotating an approved one would be
    /// marking up work already under way, which is a different act with a
    /// different answer -- speaking to the plan.
    pub fn annotate(
        &mut self,
        line: usize,
        quote: impl Into<String>,
        body: impl Into<String>,
    ) -> Option<String> {
        // The same three conditions `standing_proposal` reads, asked here
        // because that returns a shared borrow and this needs a unique one.
        if self.permissions.is_full() || self.status != PlanStatus::AwaitingReview {
            return None;
        }
        let proposal = self.proposal.as_mut().filter(|p| !p.approved)?;

        let at = Timestamp::now();
        let id = format!(
            "{}-note-{}-{}",
            self.id.as_str(),
            proposal.notes.len(),
            at.map(|Timestamp(ms)| ms).unwrap_or_default()
        );
        proposal.notes.push(ProposalNote {
            id: id.clone(),
            line,
            quote: quote.into(),
            body: body.into(),
            at,
        });
        Some(id)
    }

    /// Takes a note back before the court has been told of it.
    ///
    /// False when there is no such note, which the caller should report for the
    /// same reason [`Plan::unqueue`] does: it means the note has already been
    /// sent, and quietly doing nothing would leave the user believing he had
    /// withdrawn something the model is about to read.
    pub fn unannotate(&mut self, id: &str) -> bool {
        let Some(proposal) = self.proposal.as_mut() else {
            return false;
        };
        let before = proposal.notes.len();
        proposal.notes.retain(|note| note.id != id);
        proposal.notes.len() != before
    }

    /// Empties the margin, for the moment the notes are put to the court.
    ///
    /// Draining rather than reading is deliberate: the one way notes leave a
    /// proposal is by being sent, so there is no path that composes the message
    /// and then leaves the notes standing to be sent a second time.
    pub fn take_notes(&mut self) -> Vec<ProposalNote> {
        self.proposal
            .as_mut()
            .map(|p| std::mem::take(&mut p.notes))
            .unwrap_or_default()
    }

    /// What the user has written in the margin and not yet sent.
    pub fn notes(&self) -> &[ProposalNote] {
        self.proposal.as_ref().map_or(&[], |p| &p.notes)
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

    /// This plan as a browser should receive it.
    ///
    /// The one boundary between server state and a watching conversation, and
    /// the reason it exists is bandwidth rather than secrecy. `events.rs` is
    /// deliberate about the wire carrying whole plans, and that decision is
    /// what makes reconnection free -- but it also means a plan's every byte is
    /// re-sent, re-parsed and re-cloned on every round of every turn. On a real
    /// kingdom that was 2.3 MB per push, and 41% of it was
    /// [`Reasoning::opaque`], which nothing in the UI has ever drawn.
    ///
    /// So this drops exactly that and nothing else. It is *not* a general
    /// "trim what a plan carries on the wire": everything the chamber renders
    /// is still here, including the thinking's prose and the images a deed left
    /// behind, and a browser handed this can still draw the whole conversation.
    ///
    /// **Never on the way to disk, and never on the way to a model.** The
    /// opaque half must survive both round trips byte for byte; see
    /// [`Reasoning::without_opaque`] for what is lost when it does not.
    pub fn for_wire(&self) -> Self {
        let mut plan = self.clone();
        for entry in &mut plan.transcript {
            if let Entry::Tool(tool_call) = entry {
                tool_call.reasoning = tool_call.reasoning.take().map(Reasoning::without_opaque);
            }
        }
        plan
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
        self.close_tool_call(id, outcome, Timestamp::now())
    }

    /// Settles a call whose end time is not known.
    ///
    /// The one caller is `store::reconcile`, closing a call the server died
    /// during. It reaches for this rather than [`Plan::settle_tool_call`]
    /// because *now* is the moment the server came back, not the moment the
    /// work stopped: stamping it would report the length of the outage as the
    /// length of the command, and a plan interrupted overnight would read as a
    /// nine-hour `cargo build`.
    pub fn settle_tool_call_at_an_unknown_time(&mut self, id: &str, outcome: ToolOutcome) -> bool {
        self.close_tool_call(id, outcome, None)
    }

    fn close_tool_call(&mut self, id: &str, outcome: ToolOutcome, at: Option<Timestamp>) -> bool {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool(tool_call) = entry {
                if tool_call.id == id && tool_call.in_flight() {
                    tool_call.outcome = Some(outcome);
                    tool_call.settled_at = at;
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
// A tool call is several times the size of a message -- it carries the model's
// own JSON arguments and everything that came back -- so a transcript of mostly
// messages pays for the largest variant on every entry. That has always been
// true; adding the two timing fields is only what pushed the gap past clippy's
// threshold. Boxing the variant is the fix, and it is deliberately not done
// here: it changes every `Entry::Tool(d)` and `Turn::Tool(d)` match across both
// crates, which is a refactor of its own rather than a rider on a UI change.
// The cost until then is tens of kilobytes on a long transcript.
#[allow(clippy::large_enum_variant)]
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
    /// When the result came back. See [`Timestamp`].
    ///
    /// Written down rather than derived, because nothing else can tell us
    /// again: a settled call's duration is not recoverable from a plan document
    /// that only records when it began, so a reload would lose every figure the
    /// user had been watching.
    ///
    /// `None` while the call is in flight, and also `None` on one settled by
    /// `store::reconcile` -- a server that died mid-call genuinely does not know
    /// when the work stopped, and stamping the moment of *loading* would report
    /// the length of the outage as the length of the command.
    #[serde(default)]
    pub settled_at: Option<Timestamp>,
    /// How long this call said it would wait, where it said anything.
    ///
    /// Recorded on the call rather than worked out when the conversation draws
    /// it, because the answer comes from the tool's own arguments and defaults
    /// -- see `Tool::waits_for`. Read once, where the call is recorded, so what
    /// the chamber claims and what the tool actually does cannot drift apart.
    #[serde(default)]
    pub waits: Option<WaitBudget>,
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
///
/// **Why the opaque half is a map and not a bare value.** "Echoed back
/// unmodified" includes *the key it arrived under*. A blob read from
/// `signature` and written back as `reasoning_opaque` is as unusable to the
/// gateway as one whose bytes were rewritten: it looks like an unknown field,
/// the thinking block it signs is discarded, and the model is handed its own
/// tool results with its reasoning stripped. That failure is silent -- the
/// request stays well-formed and the gateway keeps accepting it -- and it is
/// what made long investigations wander and repeat themselves. Keeping the key
/// beside the value makes the round trip total: whatever came in goes back out
/// under the same name, and there is no branch left that can invent one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Reasoning {
    /// The thinking as prose, where the provider gave us prose.
    #[serde(default)]
    pub text: Option<String>,
    /// Provider-opaque fields that must be quoted back exactly as they came --
    /// a signature, an encrypted trace -- each under the key it arrived under.
    /// Never parsed, never rewritten, never re-keyed: it is carried, not
    /// understood.
    #[serde(default, deserialize_with = "opaque_fields")]
    pub opaque: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Reads the opaque half, tolerating the shape that came before it had keys.
///
/// Records written earlier hold a bare value here rather than a map. That is
/// not a curiosity to be tidied later: [`crate::Plan`] documents are loaded with
/// `serde_json::from_str(..).ok()` and a document that will not parse is
/// **skipped**, so rejecting the old shape would not surface as an error the
/// user could act on -- it would silently empty his rail of every plan that ever
/// thought with a signed model.
///
/// The stale value is dropped rather than given an invented key. It cannot be
/// replayed whatever we do -- nothing recorded which field it belonged to, which
/// is precisely the bug this map exists to fix -- and guessing `signature` would
/// hand a gateway a blob under a name it may never have used. The prose half
/// still loads, so the plan keeps the part of its thinking that can be shown.
fn opaque_fields<'de, D>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Object(fields) => fields.into_iter().collect(),
        _ => std::collections::BTreeMap::new(),
    })
}

impl Reasoning {
    /// True when there is nothing here worth sending, so a caller can skip it
    /// rather than emit an empty object a gateway may reject.
    pub fn is_empty(&self) -> bool {
        self.text.as_ref().is_none_or(|t| t.trim().is_empty()) && self.opaque.is_empty()
    }

    /// The same thinking with only the half a reader can use.
    ///
    /// For the **wire**, and only for the wire. [`Reasoning::opaque`] is a
    /// signature or an encrypted trace: it is carried for the provider, is
    /// never drawn, and is the single largest thing in a long plan -- 41% of
    /// the bytes across a real kingdom's records, and 1.35 MB of one 2.43 MB
    /// plan. The conversation reads `text` alone, so every one of those bytes
    /// crosses to a browser that has no use for it, is re-parsed on every push,
    /// and is then deep-copied by each memo that reads the plan.
    ///
    /// The exact counterpart of [`ToolOutcome::without_images`], and the
    /// asymmetry between them is the thing to keep straight. Images are
    /// stripped for **disk** and kept on the wire, because the chamber draws
    /// them; this is stripped for the **wire** and kept on disk, because the
    /// model needs it echoed back byte for byte or the gateway silently
    /// discards the thinking it signs -- the failure this type's own docs
    /// describe.
    pub fn without_opaque(self) -> Self {
        Self {
            text: self.text,
            opaque: std::collections::BTreeMap::new(),
        }
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
            settled_at: None,
            waits: None,
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

    /// Says how long this call will wait before it stops waiting.
    ///
    /// Builder rather than a parameter on [`ToolCall::started`] for the same
    /// reason as [`ToolCall::in_reply`]: every caller records a call, and only
    /// the one that has just asked a tool about its own arguments has this to
    /// say.
    pub fn waiting(mut self, waits: Option<WaitBudget>) -> Self {
        self.waits = waits;
        self
    }

    /// True while this call is still running.
    pub fn in_flight(&self) -> bool {
        self.outcome.is_none()
    }

    /// How long this call took, in milliseconds, where both ends are known.
    ///
    /// `None` covers three genuinely different things -- still running, a record
    /// written before the end was kept, and a call the server died during -- and
    /// they collapse to one answer on purpose: the honest rendering of all three
    /// is to show nothing. A `0` would be a claim, and a wrong one.
    ///
    /// A negative span is `None` too. It should be impossible, but the two
    /// stamps are wall-clock readings taken minutes apart, and a clock that
    /// steps backwards between them must not produce a deed that took less than
    /// no time.
    ///
    /// **Two wall-clock stamps rather than a monotonic reading.** Phoenix times
    /// its tool calls with `Instant::elapsed`, which no clock adjustment can
    /// disturb, and reports the figure rather than the endpoints. That is
    /// strictly more accurate and was not copied, for a reason worth stating: an
    /// `Instant` cannot be serialised, so it would be a *third* field carrying
    /// the duration alongside the two stamps -- and the stamps have to stay,
    /// because the conversation counts up from `at` while the call is still
    /// running. The exposure that buys is narrow. Both stamps are taken by the
    /// same process, so nothing here cares what any other machine's clock says;
    /// only a step *during* a single call distorts anything, and the guard above
    /// keeps the worst case to a missing figure rather than a wrong one.
    pub fn elapsed_ms(&self) -> Option<i64> {
        let (Some(Timestamp(from)), Some(Timestamp(to))) = (self.at, self.settled_at) else {
            return None;
        };
        (to >= from).then_some(to - from)
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

    /// What this call left on disk that the *user* could look at.
    ///
    /// The counterpart to [`ToolCall::shown`], and deliberately not the same
    /// accessor: that one feeds a model for one turn and is dropped on save,
    /// this one feeds the conversation and outlives the process. See
    /// [`ToolArtifact`].
    pub fn artifacts(&self) -> &[ToolArtifact] {
        match &self.outcome {
            Some(ToolOutcome::Done { artifacts, .. }) => artifacts,
            _ => &[],
        }
    }
}

/// A file a tool produced that is worth looking at.
///
/// A path rather than the bytes, and that is the whole design. The file is
/// already in the plan's workspace, so naming it keeps a plan's record small
/// enough to rewrite on every update -- which is what [`ToolImage`] cannot be,
/// and why `store.rs` drops those and keeps these.
///
/// The two channels answer different questions. `images` is *what the model
/// was shown*, true for one turn. This is *what the work left behind*, true
/// for as long as the file exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolArtifact {
    /// Where the file is, relative to the plan's workspace.
    ///
    /// Relative because an absolute path is a fact about one machine, and this
    /// is written into a record that outlives it -- and because a viewer
    /// resolves it against the plan's own workspace, which is the only
    /// boundary that makes serving it safe.
    pub path: String,
    /// `image/png`, `image/jpeg`, and so on.
    pub media_type: String,
}

impl ToolArtifact {
    /// True when this is something a conversation could render inline.
    ///
    /// Artifacts are not all pictures -- a tool that saves a JSON payload may
    /// reasonably name it -- and the view needs to tell them apart without
    /// parsing paths.
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
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

/// How long a tool call is prepared to wait, and what happens when that runs
/// out.
///
/// **Why this is a type rather than a number.** The tools mean two genuinely
/// different things by "wait". A browser call that reaches its timeout has
/// failed and there is nothing left to come back to; a `bash` call that reaches
/// `wait_seconds` has not failed at all -- the command runs on and the model is
/// handed a handle for it, which is the whole design of that module. One number
/// for both would put the same figure on the King's line for "this is about to
/// go wrong" and "this is working exactly as intended", and he would learn to
/// ignore it.
///
/// Seconds, because every tool that has one of these states it in seconds and
/// nothing here is finer-grained than the once-a-second tick that draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitBudget {
    /// The call gives up at this point and the deed fails. A call still running
    /// past a deadline is worth the King's attention.
    Deadline { seconds: u64 },
    /// The call stops *watching* at this point, but the work carries on and can
    /// be returned to. Passing it is ordinary, not a problem.
    Patience { seconds: u64 },
}

impl WaitBudget {
    /// How long, whichever kind this is.
    pub fn seconds(&self) -> u64 {
        match self {
            WaitBudget::Deadline { seconds } | WaitBudget::Patience { seconds } => *seconds,
        }
    }

    /// True when running past this budget means something has gone wrong.
    ///
    /// The one question the conversation asks of this type, kept here so the
    /// view never matches on the variants itself -- a third kind of waiting
    /// would otherwise need finding in the components as well as here.
    pub fn overrunning_is_a_problem(&self) -> bool {
        matches!(self, WaitBudget::Deadline { .. })
    }
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
        /// Files the tool left in the workspace that are worth looking at.
        ///
        /// Named, not carried: see [`ToolArtifact`]. This is the channel the
        /// *conversation* reads, so unlike `images` it survives being written
        /// to disk -- a screenshot the user was shown must still be there when
        /// he reloads the plan tomorrow.
        ///
        /// Skipped on the wire when empty, on the same terms as `images`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<ToolArtifact>,
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
            artifacts: Vec::new(),
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
            artifacts: Vec::new(),
        }
    }

    /// A tool that ran and left something behind worth looking at.
    ///
    /// Sibling of [`ToolOutcome::seen`], and the distinction is the point: that
    /// one hands a picture to the model, this one tells the conversation where
    /// a file is. A tool that does both -- `read_image` -- says both.
    pub fn produced(output: impl Into<String>, artifacts: Vec<ToolArtifact>) -> Self {
        ToolOutcome::Done {
            output: output.into(),
            images: Vec::new(),
            artifacts,
        }
    }

    /// The same outcome, additionally naming what it left on disk.
    ///
    /// For the one caller that has both: `read_image` hands the model the
    /// bytes *and* names the file, so the King sees the picture the court was
    /// looking at rather than a line saying it looked.
    pub fn leaving(self, artifacts: Vec<ToolArtifact>) -> Self {
        match self {
            ToolOutcome::Done { output, images, .. } => ToolOutcome::Done {
                output,
                images,
                artifacts,
            },
            refused => refused,
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
    ///
    /// **Artifacts are kept.** They are paths, not payloads -- a handful of
    /// bytes each, and the only thing that lets a reloaded conversation show a
    /// screenshot again. Dropping them here would silently undo the feature
    /// they exist for, which is why they are named rather than falling out of
    /// a `..` pattern.
    pub fn without_images(self) -> Self {
        match self {
            ToolOutcome::Done {
                output, artifacts, ..
            } => ToolOutcome::produced(output, artifacts),
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
/// See the note on [`Entry`]: the same size gap, for the same reason.
#[allow(clippy::large_enum_variant)]
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

/// Words the user spoke while a turn was in flight, waiting to be heard.
///
/// Not a [`Message`] because it is not in the log yet, and the difference
/// matters to three readers: [`Plan::turns`] must not offer it to a model, the
/// conversation must draw it as pending rather than as said, and
/// [`Plan::unqueue`] must be able to find one by name -- which a [`Message`],
/// having no id, could not support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Names this message so it can be withdrawn before it is heard. See
    /// [`Plan::queue`] for how it is derived.
    pub id: String,
    pub body: String,
    /// When the user spoke, which is *not* when it enters the log. See
    /// [`Plan::hear_queued`].
    #[serde(default)]
    pub at: Option<Timestamp>,
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
    /// The user called a halt on a turn that was running.
    ///
    /// Its own kind rather than [`NoteKind::Failed`] because nothing failed:
    /// the user chose this, and dressing a deliberate act in the colour of a
    /// breakage misreports who did what. It is also what keeps the plan out of
    /// [`PlanStatus::Failed`], which is the status the conversation offers a
    /// retry against.
    Stopped,
    /// The model answered, and the answer had nothing in it.
    ///
    /// Its own kind rather than [`NoteKind::Failed`] because the next turn has
    /// to be able to *find* it. A plan whose reply came back empty must not
    /// resend a byte-identical request -- that is the loop that made this
    /// failure feel unfixable -- so `converse` reads the last entry of the
    /// transcript and, seeing this, tells the model what happened. Matching on
    /// a kind is how that stays honest; sniffing the note's prose for the word
    /// "empty" would break the first time the wording improved.
    EmptyReply,
}

impl NoteKind {
    /// Suffix for the CSS class the conversation styles a note with.
    pub fn css_suffix(self) -> &'static str {
        match self {
            NoteKind::Failed => "failed",
            NoteKind::Workspace => "workspace",
            NoteKind::Merge => "merge",
            NoteKind::Stopped => "stopped",
            // Styled as a failure because it is one. The kind exists to be
            // matched on by the turn loop, not to be coloured differently.
            NoteKind::EmptyReply => "failed",
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

    /// The same standing wish, aimed at another model.
    ///
    /// The effort is carried across **unfiltered**, on purpose. Whether a level
    /// can actually be sent is [`ModelCatalogue::resolve`]'s decision, and it
    /// makes it on every path that reaches a provider -- the chip's own memo and
    /// `api::begin_plan`. Filtering a second time here would not make the wire
    /// any safer; it would only mean that passing through a model with no effort
    /// control destroys a preference the user set deliberately.
    ///
    /// So the stored effort is a *standing wish*, not a promise about the model
    /// currently selected: forgotten when the user asks for the model's own
    /// default, and at no other time.
    pub fn with_model(&self, model: impl Into<String>) -> ModelChoice {
        ModelChoice {
            model: model.into(),
            effort: self.effort,
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

    /// A level the user set is a *standing wish*, not a promise about whichever
    /// model happens to be selected. Passing through a model that declares no
    /// efforts at all -- the offline mock, which is exactly where a user lands
    /// whenever no credential works -- must not destroy it.
    ///
    /// This pins the division of labour the bug came from: the picker
    /// **remembers**, `resolve` **decides**. Re-adding a filter to the
    /// remembering half looks like belt-and-braces and is actually the whole
    /// defect, because the wish is stored and outlives the round trip.
    #[test]
    fn a_standing_effort_survives_a_model_that_cannot_honour_it() {
        let catalogue = catalogue();
        let wish = ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::High));

        // Onto the mock, which declares nothing. The wish is kept...
        let on_mock = wish.with_model("mock");
        assert_eq!(on_mock.effort, Some(ModelEffort::High));

        // ...and yet never reaches the wire, because resolving still drops it.
        assert_eq!(
            catalogue.resolve(Some(&on_mock)),
            ModelChoice::new("mock", None),
            "a level the resolved model does not declare is still never sent"
        );

        // Back to a model that does declare it, and the wish is honoured again.
        let back = on_mock.with_model("copilot/claude-opus-5");
        assert_eq!(
            catalogue.resolve(Some(&back)),
            ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::High)),
            "the round trip through an effortless model must not have erased it"
        );
    }

    /// The mock declares a context window of zero, and it is the model the user
    /// lands on whenever no credential works -- so "a window of nothing" is a
    /// state the header genuinely renders, not a defensive hypothetical.
    ///
    /// `None` is the whole point: it is what makes the bar *absent* rather than
    /// drawn at some fabricated fraction of a limit nobody declared. The other
    /// half pinned here is the cap, because a provider that counts its own
    /// reply can report past its limit, and "104% of 200K" reads as a bug in
    /// Kingdom rather than as a full window.
    #[test]
    fn a_window_nobody_declared_yields_no_reading() {
        assert_eq!(
            ContextUsage {
                tokens: 4_000,
                window: 0
            }
            .percent(),
            None
        );

        assert_eq!(
            ContextUsage {
                tokens: 50_000,
                window: 200_000
            }
            .percent(),
            Some(25)
        );

        assert_eq!(
            ContextUsage {
                tokens: 208_000,
                window: 200_000
            }
            .percent(),
            Some(100),
            "a full window reads as full, never as more than full"
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

    /// A proposal names the plan.
    ///
    /// The regression this pins is the rail: before, a plan kept whatever the
    /// opening decree was truncated to, while the name the user actually read
    /// on the proposal card was never used. The branch is deliberately not
    /// renamed with it -- it is already cut on disk.
    #[test]
    fn a_proposal_retitles_the_plan_but_not_its_branch() {
        let mut plan = proposing();
        assert_eq!(plan.title, "Fix the off-by-one");
        assert_eq!(plan.slug, crate::naming::slugify("Fix the parser"));

        plan.propose("Rewrite the lexer instead", "Bigger than it looked.");
        assert_eq!(
            plan.title, "Rewrite the lexer instead",
            "a revised proposal renames the plan too"
        );
        assert_eq!(
            plan.slug,
            crate::naming::slugify("Fix the parser"),
            "the branch is already cut and must not drift"
        );
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

    /// The margin: written on, withdrawn from, and drained exactly once.
    ///
    /// The draining half is the one that matters. Notes leave a proposal only
    /// by being sent, so if `take_notes` left them standing, the next send would
    /// put the same objections to the court a second time -- and the user would
    /// have no way to tell that from the court ignoring them.
    #[test]
    fn notes_gather_in_the_margin_and_leave_it_only_once() {
        let mut plan = proposing();

        let first = plan
            .annotate(1, "# Fix the off-by-one", "Call it what it is.")
            .expect("a standing proposal can be annotated");
        let second = plan
            .annotate(
                3,
                "Change `lex.rs` line 42.",
                "Which line, in today's file?",
            )
            .expect("more than one note may stand at a time");
        assert_ne!(first, second, "each note is separately withdrawable");
        assert_eq!(plan.notes().len(), 2);

        assert!(plan.unannotate(&first));
        assert!(
            !plan.unannotate(&first),
            "withdrawing a note twice must be reported, not silently accepted"
        );
        assert_eq!(plan.notes().len(), 1);

        let sent = plan.take_notes();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].quote, "Change `lex.rs` line 42.");
        assert!(
            plan.notes().is_empty(),
            "sending the notes empties the margin, so nothing is put twice"
        );
    }

    /// A note is only ever written on the question actually on the table.
    ///
    /// Each branch is a stale tab: the user left a card open and something moved
    /// underneath it. Writing the note anyway would leave him believing he had
    /// objected to work that is already under way.
    #[test]
    fn there_is_nothing_to_annotate_unless_a_proposal_stands() {
        let mut nothing = Plan::opened(
            PlanId::new("plan-2"),
            CityId::new("c1"),
            "Fix the parser",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );
        assert!(
            nothing.annotate(1, "anything", "a note").is_none(),
            "a plan with no proposal has nothing to write against"
        );

        let mut drafting = proposing();
        drafting.status = PlanStatus::Drafting;
        assert!(
            drafting.annotate(1, "anything", "a note").is_none(),
            "the court may be mid-revision of the very block being annotated"
        );

        let mut approved = proposing();
        assert!(approved.approve());
        assert!(
            approved.annotate(1, "anything", "a note").is_none(),
            "marking up approved work is speaking to the plan, not annotating it"
        );
    }

    /// A revision remembers what it revised, and forgets what it answered.
    ///
    /// `revises` is the whole basis of the diff view, and it is written in
    /// exactly one place. The forgetting half is the other guarantee: a note the
    /// court has now answered must not still be standing in the margin of the
    /// answer, waiting to be sent back a second time.
    #[test]
    fn a_revision_remembers_the_plan_it_replaces() {
        let mut plan = proposing();
        assert!(
            plan.proposal.as_ref().unwrap().revises.is_none(),
            "a first proposal has nothing to be read against"
        );

        plan.annotate(3, "Change `lex.rs` line 42.", "Which line?")
            .expect("a standing proposal can be annotated");

        plan.propose("Fix the off-by-one", "Change `lex.rs` line 43.");

        let revised = plan.proposal.as_ref().unwrap();
        assert_eq!(
            revised.revises.as_deref(),
            Some("Change `lex.rs` line 42."),
            "the body being replaced is carried forward for the diff"
        );
        assert!(
            revised.notes.is_empty(),
            "a note this revision answers is no longer pending"
        );

        let changes = revised.changes().expect("a revision can be read as a diff");
        assert!(
            !crate::proposal::unchanged(&changes),
            "a revision that moved a line must read as having moved it"
        );
    }

    /// A proposal recorded before the margin existed still loads.
    ///
    /// The same guarantee `#[serde(default)]` gives everywhere else in this
    /// file, pinned on the two fields added last -- a record on disk that failed
    /// to load is a plan the user has lost, and disk cannot tell us again.
    #[test]
    fn a_proposal_recorded_before_notes_existed_still_loads() {
        let before_notes = r#"{
            "title": "Fix the off-by-one",
            "body": "Change `lex.rs` line 42.",
            "at": 1700000000000,
            "approved": false
        }"#;

        let proposal: Proposal =
            serde_json::from_str(before_notes).expect("an older proposal must still load");

        assert!(proposal.notes.is_empty());
        assert!(
            proposal.revises.is_none(),
            "an older proposal has no predecessor recorded, so it shows no diff"
        );
        assert!(proposal.changes().is_none());
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
            vec!["said:Run the tests", "did:bash:ok", "said:The tests pass.",],
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

    /// A plan recorded before tool calls could name what they left behind must
    /// still load, and must not sprout artifacts nobody wrote.
    ///
    /// Same reasoning as the record above, and worth its own test for the same
    /// reason: `#[serde(default)]` *should* make the field additive, but
    /// "should" and "does" are different claims and the cost of being wrong is
    /// a kingdom that will not open.
    #[test]
    fn a_plan_recorded_before_artifacts_existed_still_loads() {
        let before_artifacts = r#"{
            "id": "plan-old",
            "city": "c1",
            "title": "An older plan",
            "summary": "",
            "prompt": "Look at the page",
            "model": "mock",
            "effort": null,
            "transcript": [
                { "Tool": {
                    "id": "call-1",
                    "tool": "browser_take_screenshot",
                    "input": {},
                    "outcome": { "Done": { "output": "Screenshot saved to /tmp/a.png." } },
                    "at": 1
                } }
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
            serde_json::from_str(before_artifacts).expect("an older plan record must still load");

        let Some(Entry::Tool(tool_call)) = plan.transcript.first() else {
            panic!("the deed must survive the load");
        };
        assert_eq!(tool_call.report(), "Screenshot saved to /tmp/a.png.");
        assert!(
            tool_call.artifacts().is_empty(),
            "absent means nothing was left behind, not a parse failure"
        );
    }

    /// Thinking recorded before opaque fields carried their key must still load.
    ///
    /// Older records hold `opaque` as a bare string, and `store::load` *skips a
    /// plan it cannot parse* rather than failing loudly -- so a deserialiser
    /// that rejected the old shape would not raise an error, it would quietly
    /// empty the King's rail. The stale blob itself is unreplayable (nothing
    /// recorded which field it arrived in, which is the whole bug this shape
    /// fixes) but losing a signature must not mean losing the plan.
    #[test]
    fn thinking_recorded_before_opaque_fields_were_keyed_still_loads() {
        let before_keys = r#"{ "text": "the title is read in sidebar.rs", "opaque": "c2lnbmVk" }"#;

        let reasoning: Reasoning =
            serde_json::from_str(before_keys).expect("an older record must still load");

        assert_eq!(
            reasoning.text.as_deref(),
            Some("the title is read in sidebar.rs"),
            "the prose is the half that can still be replayed, so it must survive"
        );
        assert!(
            reasoning.opaque.is_empty(),
            "an unkeyed blob cannot go back under a key it never recorded"
        );
    }

    /// What the browser is handed keeps the thinking it draws and drops the
    /// blob it cannot.
    ///
    /// The whole of the wire-size fix, pinned at the type that performs it. A
    /// long plan's record is mostly `opaque` -- 41% of the bytes across a real
    /// kingdom -- and none of it is ever rendered, so it is stripped on the way
    /// to a conversation. `text` must survive, because the chamber folds it
    /// away behind `thinking (N lines)` and a reader can open it.
    #[test]
    fn a_plan_bound_for_a_browser_keeps_its_prose_and_sheds_its_signature() {
        let mut thinking = Reasoning {
            text: Some("the title is read in sidebar.rs".to_string()),
            ..Reasoning::default()
        };
        thinking.opaque.insert(
            "signature".to_string(),
            serde_json::json!("c2lnbmVkLXRoaW5raW5n"),
        );

        let mut plan = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Fix the parser",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );
        plan.begin_tool_call(
            ToolCall::started("call-1", "bash", serde_json::json!({})).in_reply(
                "reply-1",
                Some(thinking),
                None,
            ),
        );

        let sent = plan.for_wire();
        let Some(Entry::Tool(deed)) = sent.transcript.last() else {
            panic!("the deed must survive the crossing");
        };
        let carried = deed
            .reasoning
            .as_ref()
            .expect("the thinking is still there");

        assert_eq!(
            carried.text.as_deref(),
            Some("the title is read in sidebar.rs"),
            "the chamber draws the prose, so it must cross"
        );
        assert!(
            carried.opaque.is_empty(),
            "the signature is for the provider and is never drawn -- it must not cross"
        );
    }

    /// And the plan the *server* holds is untouched by having been sent.
    ///
    /// The failure this guards against is silent and expensive: the opaque half
    /// must be echoed back to the gateway byte for byte, and a gateway handed a
    /// thinking block whose signature has gone discards it and keeps accepting
    /// the request. So stripping in place -- rather than on a copy -- would
    /// not error anywhere. It would make long investigations wander and repeat
    /// themselves, which is the exact bug `Reasoning`'s own docs describe.
    #[test]
    fn sending_a_plan_to_a_browser_does_not_blind_the_one_the_server_keeps() {
        let mut thinking = Reasoning::default();
        thinking
            .opaque
            .insert("signature".to_string(), serde_json::json!("c2lnbmVk"));

        let mut plan = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Fix the parser",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );
        plan.begin_tool_call(
            ToolCall::started("call-1", "bash", serde_json::json!({})).in_reply(
                "reply-1",
                Some(thinking),
                None,
            ),
        );

        let _sent = plan.for_wire();

        let Some(Entry::Tool(deed)) = plan.transcript.last() else {
            panic!("the deed is still on the server's plan");
        };
        assert_eq!(
            deed.reasoning
                .as_ref()
                .and_then(|r| r.opaque.get("signature")),
            Some(&serde_json::json!("c2lnbmVk")),
            "the plan the model is replayed from must keep its signature, under its own key"
        );
    }

    /// Nothing said is not the same as saying nothing, and only one of them is
    /// worth recording.
    ///
    /// A gateway that returns `""` or a stray newline beside its tool calls is
    /// not a court that chose its words carefully. Stored, it would serialise
    /// into every plan document for no gain, replay to the model as an assistant
    /// turn that spoke to say nothing, and draw the King a bordered stripe of
    /// padding above a deed with no words in it.
    #[test]
    fn a_reply_that_said_nothing_records_nothing() {
        let blank = ToolCall::started("call-1", "bash", serde_json::json!({})).in_reply(
            "reply-1",
            None,
            Some("  \n ".to_string()),
        );
        assert_eq!(blank.narration, None, "whitespace is not a statement");

        let said = ToolCall::started("call-2", "bash", serde_json::json!({})).in_reply(
            "reply-1",
            None,
            Some("Running the tests before I touch anything.".to_string()),
        );
        assert_eq!(
            said.narration.as_deref(),
            Some("Running the tests before I touch anything."),
            "words the court actually wrote are kept exactly as it wrote them"
        );
    }

    /// An outcome that names a file round-trips, and the two channels stay
    /// apart on the way through.
    ///
    /// The failure this pins is the one the design turns on: if artifacts ever
    /// serialise as images (or the reverse), `store.rs` would either drop the
    /// paths the conversation needs or persist the megabytes it must not.
    #[test]
    fn an_outcome_can_name_what_it_left_behind() {
        let outcome = ToolOutcome::produced(
            "Screenshot saved to shot.png.",
            vec![ToolArtifact {
                path: ".kingdom-browser-screenshot-1.png".into(),
                media_type: "image/png".into(),
            }],
        );

        let json = serde_json::to_string(&outcome).expect("an outcome must serialise");
        let read: ToolOutcome = serde_json::from_str(&json).expect("and come back");
        assert_eq!(read, outcome);

        let ToolOutcome::Done {
            images, artifacts, ..
        } = read
        else {
            panic!("a produced outcome is Done");
        };
        assert!(
            images.is_empty(),
            "naming a file must not put its bytes in front of a model"
        );
        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].is_image());

        // An empty artifact list stays off the wire, so a plan document is not
        // littered with a key nearly every deed would carry empty.
        let plain = serde_json::to_string(&ToolOutcome::done("ok")).expect("serialises");
        assert!(!plain.contains("artifacts"), "got {plain}");
    }

    /// Stripping the pictures keeps the paths.
    ///
    /// The write path calls this on every save. Dropping artifacts alongside
    /// images would compile, pass every other test, and silently leave a
    /// reloaded chamber with nothing to show -- which is the whole feature.
    #[test]
    fn dropping_the_pictures_keeps_the_paths() {
        let outcome = ToolOutcome::seen(
            "Looked at shot.png.",
            vec![ToolImage {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
        )
        .leaving(vec![ToolArtifact {
            path: "shot.png".into(),
            media_type: "image/png".into(),
        }]);

        let ToolOutcome::Done {
            images, artifacts, ..
        } = outcome.without_images()
        else {
            panic!("still Done");
        };
        assert!(images.is_empty(), "the bytes must not reach the disk");
        assert_eq!(
            artifacts,
            vec![ToolArtifact {
                path: "shot.png".into(),
                media_type: "image/png".into(),
            }],
            "the path must survive, or a reloaded chamber has nothing to show"
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
            kingdom
                .plans_in(&city)
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
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

    /// A settled call must know when it ended, and a call the server died
    /// during must admit that it does not.
    ///
    /// The second half is the one worth pinning. Both paths settle a call and
    /// both look right in the transcript, but only one of them was actually
    /// watching a clock -- and the failure is silent: a plan interrupted
    /// overnight would report a nine-hour deed, which is wrong in a way that
    /// reads as information.
    #[test]
    fn a_settled_call_records_when_it_ended_unless_nobody_was_watching() {
        let mut plan = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Run the tests",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );

        plan.begin_tool_call(ToolCall::started("call-1", "bash", serde_json::json!({})));
        plan.begin_tool_call(ToolCall::started("call-2", "bash", serde_json::json!({})));

        let in_flight = tool_call(&plan, "call-1");
        assert!(
            in_flight.settled_at.is_none() && in_flight.elapsed_ms().is_none(),
            "a call still running has not taken any length of time yet"
        );

        assert!(plan.settle_tool_call("call-1", ToolOutcome::done("ok")));
        let settled = tool_call(&plan, "call-1");
        assert!(
            settled.settled_at.is_some(),
            "a settled call knows when it ended"
        );
        assert!(
            settled.elapsed_ms().is_some_and(|ms| ms >= 0),
            "and can say how long it took"
        );

        assert!(plan.settle_tool_call_at_an_unknown_time("call-2", ToolOutcome::done("ok")));
        let reconciled = tool_call(&plan, "call-2");
        assert!(
            reconciled.settled_at.is_none() && reconciled.elapsed_ms().is_none(),
            "a call the server died during must not claim to have been timed: \
             stamping the moment of recovery would report the outage as the deed"
        );
    }

    /// A record written before deeds were timed must still load, and must not
    /// have a duration invented for it. Both fields are `#[serde(default)]`,
    /// which is a claim rather than a fact until something checks.
    #[test]
    fn a_deed_recorded_before_it_was_timed_still_loads() {
        let before_timing = r#"{
            "id": "plan-old",
            "city": "c1",
            "title": "An older plan",
            "summary": "Drawn up before anyone was counting",
            "prompt": "Do the thing",
            "model": "mock",
            "effort": null,
            "transcript": [
                { "Tool": {
                    "id": "call-1",
                    "tool": "bash",
                    "input": { "cmd": "cargo test" },
                    "outcome": { "Done": { "output": "ok" } },
                    "at": 1
                } }
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
            serde_json::from_str(before_timing).expect("an older plan record must still load");

        let call = tool_call(&plan, "call-1");
        assert!(call.settled_at.is_none() && call.waits.is_none());
        assert!(
            call.elapsed_ms().is_none(),
            "a deed nobody timed shows nothing, rather than a figure of zero"
        );
    }

    /// The distinction the whole rendering rests on: overrunning is a problem
    /// for a deadline and ordinary for patience. Inverting this is a one-word
    /// edit that no other test would catch, and it would have the chamber cry
    /// alarm over every long-running build.
    #[test]
    fn only_a_deadline_makes_overrunning_a_problem() {
        assert!(WaitBudget::Deadline { seconds: 15 }.overrunning_is_a_problem());
        assert!(!WaitBudget::Patience { seconds: 30 }.overrunning_is_a_problem());
        assert_eq!(WaitBudget::Patience { seconds: 30 }.seconds(), 30);
    }

    /// The one deed with a given id, for the tests above.
    fn tool_call<'a>(plan: &'a Plan, id: &str) -> &'a ToolCall {
        plan.transcript
            .iter()
            .find_map(|e| match e {
                Entry::Tool(d) if d.id == id => Some(d),
                _ => None,
            })
            .expect("the deed is in the log")
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

    /// A plan with nothing but its opening decree in the log.
    fn working() -> Plan {
        Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Fix the parser",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        )
    }

    /// Queued words are heard oldest-first, and only when asked for.
    ///
    /// The order is the whole contract. `converse` drains at a round boundary
    /// and hands the result straight to the model, so a queue that reversed --
    /// or that dropped one in the middle -- would put the King's instructions
    /// in an order he never spoke them in, and the court would act on the wrong
    /// one last.
    #[test]
    fn queued_words_are_heard_in_the_order_they_were_spoken() {
        let mut plan = working();
        let before = plan.transcript.len();

        plan.queue("first");
        plan.queue("second");
        plan.queue("third");

        assert_eq!(
            plan.transcript.len(),
            before,
            "queuing must not touch the log -- nobody has heard these yet"
        );

        assert_eq!(plan.hear_queued(), 3);
        assert!(
            plan.queued.is_empty(),
            "hearing must empty the queue, or the next round hears them twice"
        );

        let said: Vec<String> = plan.transcript[before..]
            .iter()
            .map(|entry| match entry {
                Entry::Message(m) => {
                    assert_eq!(m.speaker, Speaker::User);
                    m.body.clone()
                }
                other => panic!("queued words must land as messages, got {other:?}"),
            })
            .collect();
        assert_eq!(said, vec!["first", "second", "third"]);

        assert_eq!(
            plan.hear_queued(),
            0,
            "hearing an empty queue is a no-op, not a repeat"
        );
    }

    /// The King can take words back, and only the ones he named.
    #[test]
    fn withdrawing_queued_words_leaves_their_neighbours_alone() {
        let mut plan = working();
        let first = plan.queue("first");
        let second = plan.queue("second");
        let third = plan.queue("third");

        assert!(plan.unqueue(&second));
        assert!(
            !plan.unqueue(&second),
            "withdrawing the same words twice must be reported, not silently \
             accepted -- the second call is a race with the drain, already lost"
        );
        assert!(!plan.unqueue("plan-1-nothing-like-this"));

        let left: Vec<&str> = plan.queued.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(left, vec![first.as_str(), third.as_str()]);

        plan.hear_queued();
        let said: Vec<&str> = plan
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Message(m) => Some(m.body.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !said.contains(&"second"),
            "words withdrawn before they were heard must never reach the court"
        );
    }

    /// The invariant that keeps a queue out of a turn already in flight.
    ///
    /// [`Plan::turns`] is the single doorway between a plan's log and anything
    /// that talks to a model, and `converse` reads it afresh on every round. A
    /// queued message visible there would be picked up mid-round and spliced
    /// between a tool call and its result -- a conversation that never
    /// happened -- which is the whole reason the queue is a field of its own
    /// rather than an early `say`.
    #[test]
    fn queued_words_are_not_a_turn_until_they_are_heard() {
        let mut plan = working();
        let before = plan.turns().count();

        plan.queue("do not read this yet");
        assert_eq!(
            plan.turns().count(),
            before,
            "a queued message must be invisible to anything briefing a model"
        );

        plan.hear_queued();
        assert_eq!(
            plan.turns().count(),
            before + 1,
            "and visible the moment it is heard"
        );
    }

    /// A plan written before the King could speak over the court still loads,
    /// with nothing waiting on it. Being wrong here is a kingdom that will not
    /// open, which is why the claim is tested rather than assumed of
    /// `#[serde(default)]`.
    #[test]
    fn a_plan_recorded_before_words_could_be_queued_still_loads() {
        let before_queueing = r#"{
            "id": "plan-old",
            "city": "c1",
            "title": "An older plan",
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
            serde_json::from_str(before_queueing).expect("an older plan record must still load");

        assert!(
            plan.queued.is_empty(),
            "a plan from before the queue existed has nothing waiting on it"
        );
    }
}
