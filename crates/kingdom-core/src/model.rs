//! The Kingdom domain model.

use crate::ids::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Kingdom & Cities
// ---------------------------------------------------------------------------

/// The dev folder the King has opened. Everything else hangs off this.
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
    /// The UI renders this loudly. A synthetic realm is *designed* to be
    /// indistinguishable from a real one on the map, which makes an unlabelled
    /// one a trap -- for the King glancing at it, and equally for a model shown
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
    /// Errands are excluded, and this is the reader that decides it for the map.
    /// An errand holds no worktree of its own -- it works in its parent's -- so
    /// a second pip on the same city would draw one piece of work twice.
    pub fn plans_in<'a>(&'a self, id: &'a CityId) -> impl Iterator<Item = &'a Plan> + 'a {
        self.plans
            .iter()
            .filter(move |p| &p.city == id && !p.is_errand())
    }

    /// The errands one call sent, in the order they were sent.
    ///
    /// Keyed by the deed as well as the parent, so a plan that sends errands
    /// twice does not show the first round's under the second round's call.
    ///
    /// This is the *only* way the parent's chamber finds its errands, and the
    /// direction is deliberate: the link is a field on the errand rather than a
    /// list on the [`ToolCall`], so there is one place it can be wrong. A list on
    /// the deed would have to be kept in step with the plans themselves, and the
    /// failure -- a named errand that does not exist, or an errand no call
    /// admits to -- would be silent.
    pub fn errands_of<'a>(
        &'a self,
        parent: &'a PlanId,
        tool_call: &'a str,
    ) -> impl Iterator<Item = &'a Plan> + 'a {
        self.plans.iter().filter(move |p| match &p.errand_for {
            Some(errand) => &errand.parent == parent && errand.tool_call == tool_call,
            None => false,
        })
    }

    /// Files a plan into the kingdom, replacing whatever was there under its id.
    ///
    /// The receiving half of push: the server proclaims a whole plan and the
    /// browser absorbs it. Replacing rather than merging is the point -- see
    /// `herald.rs` for why the wire carries whole plans rather than deltas.
    ///
    /// An unknown id is appended rather than dropped, so a plan opened in one
    /// tab appears in another without a full refetch.
    pub fn absorb(&mut self, plan: Plan) {
        match self.plans.iter_mut().find(|p| p.id == plan.id) {
            Some(existing) => *existing = plan,
            None => self.plans.push(plan),
        }
    }

    pub fn plan(&self, id: &PlanId) -> Option<&Plan> {
        self.plans.iter().find(|p| &p.id == id)
    }

    /// Plans still awaiting the King's judgement.
    ///
    /// Never an errand: an errand reports to the court that sent it, and nothing
    /// about it is ever waiting on the King.
    pub fn pending_plans(&self) -> impl Iterator<Item = &Plan> {
        self.plans
            .iter()
            .filter(|p| p.status == PlanStatus::AwaitingReview && !p.is_errand())
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
    pub structure: Option<District>,
}

// ---------------------------------------------------------------------------
// City structure -- the raw shape of a project on disk
// ---------------------------------------------------------------------------

/// One file in a project: a single building in its city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    pub name: String,
    /// Path relative to the city root, which is what identifies this exact
    /// building on the map.
    pub path: String,
    pub ward: Ward,
    /// Size in bytes, which drives the building's height.
    pub bulk: u64,
}

/// One folder in a project: a district of its city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct District {
    pub name: String,
    /// Path relative to the city root; empty for the root district.
    pub path: String,
    pub buildings: Vec<Building>,
    pub children: Vec<District>,
    /// Files the scanner pruned rather than listing individually.
    ///
    /// Carrying the remainder as a count and a weight (instead of dropping it)
    /// is what keeps the map honest: a folder with ten thousand files still
    /// renders as heavy, even though only its largest files are named.
    pub extra_files: usize,
    pub extra_bulk: u64,
}

impl District {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            buildings: Vec::new(),
            children: Vec::new(),
            extra_files: 0,
            extra_bulk: 0,
        }
    }

    /// Every file beneath this district, including pruned remainders.
    pub fn total_files(&self) -> usize {
        self.buildings.len()
            + self.extra_files
            + self
                .children
                .iter()
                .map(District::total_files)
                .sum::<usize>()
    }

    /// Total bytes beneath this district, including pruned remainders.
    pub fn total_bulk(&self) -> u64 {
        self.buildings.iter().map(|b| b.bulk).sum::<u64>()
            + self.extra_bulk
            + self.children.iter().map(District::total_bulk).sum::<u64>()
    }

    pub fn is_empty(&self) -> bool {
        self.total_files() == 0
    }
}

/// The language a file is written in, which tints its building.
///
/// Colour is the fastest channel the King has for reading a city's composition
/// at a glance, so this is a small, visually distinct set rather than an
/// exhaustive language list; anything unrecognised falls to [`Ward::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ward {
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

impl Ward {
    /// Every ward, in legend order.
    pub const ALL: [Ward; 11] = [
        Ward::Rust,
        Ward::Web,
        Ward::Python,
        Ward::Go,
        Ward::Systems,
        Ward::Shell,
        Ward::Markup,
        Ward::Style,
        Ward::Config,
        Ward::Docs,
        Ward::Other,
    ];

    /// Classifies a file by extension.
    pub fn from_path(path: &str) -> Ward {
        let name = path.rsplit('/').next().unwrap_or(path);

        // Extensionless files that are nonetheless recognisable.
        match name {
            "Makefile" | "Dockerfile" | "Justfile" | "justfile" => return Ward::Config,
            "LICENSE" | "NOTICE" | "AUTHORS" => return Ward::Docs,
            _ => {}
        }

        let ext = match name.rsplit_once('.') {
            // A leading dot means a dotfile, not an extension.
            Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
            _ => return Ward::Config,
        };

        match ext.as_str() {
            "rs" => Ward::Rust,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => Ward::Web,
            "py" | "pyi" | "ipynb" => Ward::Python,
            "go" => Ward::Go,
            "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "zig" | "java" | "kt" | "swift" | "cs" => {
                Ward::Systems
            }
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" => Ward::Shell,
            "html" | "htm" | "xml" | "svg" | "jsx.html" => Ward::Markup,
            "css" | "scss" | "sass" | "less" | "styl" => Ward::Style,
            "toml" | "json" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "lock" | "env" => {
                Ward::Config
            }
            "md" | "mdx" | "txt" | "rst" | "adoc" => Ward::Docs,
            _ => Ward::Other,
        }
    }

    /// The building's face colour.
    ///
    /// Deliberately avoids the status palette (green/amber/red), which is
    /// reserved for what agents are doing; ward colour says what the code *is*.
    pub fn tint(&self) -> &'static str {
        match self {
            Ward::Rust => "#fb923c",
            Ward::Web => "#38bdf8",
            Ward::Python => "#60a5fa",
            Ward::Go => "#2dd4bf",
            Ward::Systems => "#c084fc",
            Ward::Shell => "#a3e635",
            Ward::Markup => "#f472b6",
            Ward::Style => "#818cf8",
            Ward::Config => "#94a3b8",
            Ward::Docs => "#cbd5e1",
            Ward::Other => "#546076",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Ward::Rust => "Rust",
            Ward::Web => "JS/TS",
            Ward::Python => "Python",
            Ward::Go => "Go",
            Ward::Systems => "Systems",
            Ward::Shell => "Shell",
            Ward::Markup => "Markup",
            Ward::Style => "Styles",
            Ward::Config => "Config",
            Ward::Docs => "Docs",
            Ward::Other => "Other",
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
    /// Heraldic colour for the city's banner on the map.
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
/// This is the King's answer to "can this agent trample the folder I am in?".
/// It is chosen per decree because the honest answer differs per decree: a
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
    /// Short label for the decree bar's chip and the chamber header.
    pub fn label(&self) -> String {
        match self {
            WorkspaceMode::Fresh => "fresh worktree".to_string(),
            WorkspaceMode::Branch(b) => format!("branch: {b}"),
            WorkspaceMode::InPlace => "in place".to_string(),
        }
    }
}

impl Default for WorkspaceMode {
    /// Isolation by default: the surprising outcome should be the one the King
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
    /// current HEAD at merge time would land a plan wherever the King happens to
    /// have wandered since it was opened -- which is the collision this product
    /// exists to prevent, committed by the product itself.
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

/// An architectural plan: a proposal drafted by a model, awaiting the King's
/// review.
///
/// A plan is deliberately the *only* agent-shaped noun in the model. An earlier
/// design had a separate `Architect` entity that owned plans, but the King never
/// reviews an architect -- he reviews a plan. Collapsing the two removes a state
/// machine that had to be kept in sync with this one for no gain: which model is
/// drafting is an attribute of the work, not an actor with a life of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    /// The city this plan is drawn up for.
    pub city: CityId,
    pub title: String,
    /// The title as a git-safe slug. The plan's branch is cut from this, so the
    /// name the King reads in the rail and the name he reads in `git branch`
    /// are the same name.
    ///
    /// `#[serde(default)]` because plan records written before plans had slugs
    /// are still on disk, and their branch already exists under its old name.
    #[serde(default)]
    pub slug: String,
    pub summary: String,
    /// The decree that opened this plan, verbatim.
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
    /// The call this plan was sent to answer, when it is an errand.
    ///
    /// `None` for a plan the King decreed, which is every plan he sees in the
    /// rail or on the map. An errand is a plan the *court* sent, to answer a
    /// question it had while working on its own.
    ///
    /// Being a plan rather than a type of its own is what gives an errand a
    /// chamber, a watch socket and a record on disk for free -- see the module
    /// docs on [`Plan::sent`] for what that buys and what it costs.
    #[serde(default)]
    pub errand_for: Option<Errand>,
}

/// Which call an errand was sent to answer.
///
/// The deed is carried as well as the plan because a plan may send errands more
/// than once: without it, a second round's chamber would show the first round's
/// errands under its call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Errand {
    /// The plan that sent this one.
    pub parent: PlanId,
    /// The [`ToolCall::id`] of the call that sent it.
    pub tool_call: String,
}

impl Plan {
    /// A plan that has just been opened by a decree, before any drafting.
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
            slug: slug_for_decree(&prompt),
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
            errand_for: None,
        }
    }

    /// A plan sent by another plan to answer one question.
    ///
    /// # What an errand shares with its parent, and why
    ///
    /// Its **workspace, verbatim**. An errand is another agent working in the
    /// same place on the same files, which is the whole point -- it is sent to
    /// look at the work in progress, not at a pristine copy of the project. It
    /// therefore owns nothing on disk and must never be finished: merging it
    /// would land its parent's half-done work and then delete the worktree from
    /// under a plan still running in it. `api::finish_plan` refuses on
    /// [`Plan::is_errand`], and that guard is load-bearing.
    ///
    /// Its **model and effort**, so a fan-out is drafted by the thing the King
    /// chose rather than by whatever the default happens to be that day.
    ///
    /// # Why the task is recorded as the King speaking
    ///
    /// It was the parent's court that said it, not the King, so this is a small
    /// lie -- and it is the right one. [`Speaker`] maps directly onto the wire's
    /// `user`/`assistant` roles, and a third variant would need a decision at
    /// every match over a transcript -- the provider, the mock, the store's
    /// repair pass, the chamber -- to buy nothing but a label. The label is
    /// fixed where labels belong: the chamber renders an errand's King turns as
    /// "Commission".
    pub fn sent(id: PlanId, parent: &Plan, tool_call: &str, task: impl Into<String>) -> Self {
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
            errand_for: Some(Errand {
                parent: parent.id.clone(),
                tool_call: tool_call.to_string(),
            }),
        }
    }

    /// True when this plan was sent by another plan rather than decreed.
    ///
    /// Read by every guard and every filter that has to tell the King's work
    /// from the court's own: the rail, the map, `say`, `draft_plan` and -- most
    /// importantly -- `finish_plan`.
    pub fn is_errand(&self) -> bool {
        self.errand_for.is_some()
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
    /// chamber claims to have landed and cannot say where.
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

    /// Just the utterances, in order.
    ///
    /// What the King and the court *said*, with tool calls and Kingdom's own
    /// notices both left out. Useful for questions about the conversation --
    /// "has the court replied yet?" -- rather than for building a request.
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
    /// Written down *before* the work rather than after it, so the chamber can
    /// show a command while it is still running. A deed recorded only on
    /// completion would make a five-minute build look like five minutes of an
    /// agent doing nothing at all, which is the exact question this product
    /// exists to answer.
    pub fn begin_tool_call(&mut self, tool_call: ToolCall) {
        self.transcript.push(Entry::Tool(tool_call));
    }

    /// Settles a tool call that was begun earlier.
    ///
    /// Returns false if there is no such call still in flight, which the caller
    /// should treat as a bug rather than ignore: it means a result arrived for
    /// something never recorded as started, and the log the King reads is
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

/// The slug a decree will produce, before there is a [`Plan`] to ask.
///
/// Exists because of an ordering knot: the branch is named from the slug, but a
/// plan cannot be built until its workspace -- and therefore its branch --
/// exists. Rather than let the caller derive the name its own way and hope it
/// matches, both it and [`Plan::opened`] go through here.
pub fn slug_for_decree(prompt: &str) -> String {
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
/// A [`ToolCall`] is not a speaker either, for the first half of the same reason:
/// nobody said it. But it parts company with a note on the second half -- a
/// deed **does** go back to the model, because a tool result the model is never
/// shown is a tool call it will immediately make again. So the log now holds
/// three kinds of thing and exactly two of them are addressed to a model; see
/// [`Plan::turns`], which is the only door between this log and one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Entry {
    /// Words a participant produced.
    Message(Message),
    /// Something Kingdom itself reports: a failed call or a workspace cut.
    /// Never sent anywhere.
    Note(Note),
    /// Something the court did with its own hands, and what came back.
    Tool(ToolCall),
}

/// A tool call the court made, and its result.
///
/// Both halves live in one entry rather than two. A call and its outcome are
/// one event in the King's reading of the chamber -- "it ran the tests, and they
/// failed" -- and splitting them would let the log hold a result with no call,
/// or two results for one call, neither of which is a thing that can happen.
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
    /// the chamber renders -- it is how the King sees what an agent is doing
    /// *right now* rather than only what it did.
    pub outcome: Option<ToolOutcome>,
    /// When the call was made. See [`Timestamp`].
    #[serde(default)]
    pub at: Option<Timestamp>,
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
    /// The tool ran. Note that a command exiting non-zero is still `Done` --
    /// a failing test suite is a successful tool call with bad news in it, and
    /// conflating the two would have the chamber cry error over exactly the
    /// result the King asked for.
    Done {
        output: String,
        /// Pictures the tool produced.
        ///
        /// A separate channel from `output` rather than encoded into it. The
        /// text is what the chamber renders and what a model without vision is
        /// told; a megabyte of base64 spliced into it would be unreadable in
        /// the one place and useless in the other. Keeping them apart is also
        /// what lets the store drop the pictures and keep the words -- see
        /// `store.rs`.
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
    /// The words are still required: they are what the chamber shows, what the
    /// store keeps, and what a model that cannot see is given instead.
    pub fn seen(output: impl Into<String>, images: Vec<ToolImage>) -> Self {
        ToolOutcome::Done {
            output: output.into(),
            images,
        }
    }

    /// Suffix for the CSS class the chamber styles a settled deed with.
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

/// One turn of the exchange between the King and the court: the entries that
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
    /// one: an utterance whose time is chosen by hand is an utterance that can
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
    /// zero: **the browser never authors a log entry**. Every utterance and note
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
    /// What happened when the King moved to finish the plan: work landing, a
    /// conflict git refused, a worktree disposed of.
    Merge,
}

impl NoteKind {
    /// Suffix for the CSS class the chamber styles a note with.
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
/// the King could approve anything. `Merged` and `Archived` name what actually
/// happens to a branch, and both are reachable from the chamber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlanStatus {
    /// A model is drafting it right now.
    Drafting,
    /// Drafted, and waiting on the King.
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

/// What the King chose to do with a plan when he closed it.
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
    /// recoverable either way -- but the King must not be told to check out a
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
    /// How the chamber's footer states what became of the plan.
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
// Model access -- what the King can see about how plans get drafted
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

/// What a decree is drafted with.
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
    /// Surfaced above the fold, before the King expands the full list.
    pub recommended: bool,
    /// The effort levels this model declares. Empty means it has no effort
    /// control at all, and the picker hides the row rather than offering
    /// levels that would be refused.
    pub efforts: Vec<ModelEffort>,
    /// Whether this model can be handed tools.
    ///
    /// A model that cannot is still offered -- it drafts perfectly good prose,
    /// and the King choosing a cheaper model should get a weaker answer rather
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
    /// withholding it from one that could merely means the King's court works
    /// the way it did last week. The costs of guessing wrong are not
    /// symmetric, so absent is taken as "no".
    #[serde(default)]
    pub can_see: bool,
}

/// Everything the picker needs, plus why it might be shorter than expected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogue {
    pub options: Vec<ModelOption>,
    /// What a King who has never chosen gets.
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
    /// degrades to the nearest valid thing instead of erroring. The King's
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
                },
            ],
            default_id: "copilot/claude-opus-5".into(),
            credential: CredentialState::Ready,
            detail: String::new(),
        }
    }

    /// The King's browser remembers a choice for longer than any catalogue
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
mod transcript_tests {
    use super::*;

    /// Kingdom's own notices must never reach a model.
    ///
    /// This is the bug the `Entry`/`Note` split exists to make unrepresentable.
    /// When every log line was an utterance, app notices and failed calls were
    /// stored as things the *model* had said, and the next turn replayed them to
    /// it as its own prior words -- teaching it to answer in the voice of the
    /// plumbing. `messages()` is the only door between a plan's log and a model,
    /// so it is the thing worth pinning: notes never come through it, ordering
    /// survives, and the last King turn is still findable as the live prompt.
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

        // The prompt is the last thing the King said, even though a note landed
        // between the turns.
        let i = messages
            .iter()
            .rposition(|u| u.speaker == Speaker::User)
            .expect("the King has spoken");
        assert_eq!(messages[i].body, "Second question");
    }

    /// The same exclusion, now that the log holds a third kind of thing. A deed
    /// must reach the model (a tool result it never sees is a tool call it
    /// makes again) while a note still must not, and both must arrive in the
    /// order they happened -- a provider that sees a result before its call is
    /// rebuilding a conversation that never took place.
    #[test]
    fn deeds_reach_the_model_in_order_and_notes_still_do_not() {
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
    /// before deeds existed must still load. `Entry` is externally tagged, which
    /// makes a new variant additive -- but "should be additive" and "is" are
    /// different claims, and the cost of being wrong is a kingdom that will not
    /// open.
    #[test]
    fn a_plan_recorded_before_the_court_had_hands_still_loads() {
        let before_deeds = r#"{
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
            serde_json::from_str(before_deeds).expect("an older plan record must still load");

        assert_eq!(plan.transcript.len(), 2);
        assert_eq!(
            plan.turns().count(),
            1,
            "the note is still excluded, and nothing invented a deed that never happened"
        );
    }

    /// An errand is a plan the King never asked for, so the readers that build
    /// *his* views must not show it -- while the reader the parent's chamber
    /// uses must find exactly the errands of the call that sent them.
    ///
    /// Worth one test because this is a filter applied in several places from
    /// one predicate: the map reads `plans_in`, the rail reads `plans` with the
    /// same condition, and the deed line reads `errands_of`. The failure is
    /// silent in both directions -- an errand in the rail is clutter, an errand
    /// missing from `errands_of` is a chamber that shows a call sending nothing.
    #[test]
    fn errands_are_hidden_from_the_kings_views_and_found_by_their_call() {
        let city = CityId::new("c1");
        let parent = Plan::opened(
            PlanId::new("plan-1"),
            city.clone(),
            "Work out what is slow",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        );

        let mut first = Plan::sent(PlanId::new("plan-2"), &parent, "call-1", "Read the parser");
        first.status = PlanStatus::AwaitingReview;
        let second = Plan::sent(PlanId::new("plan-3"), &parent, "call-1", "Read the loader");
        // A second round of errands, under a different call.
        let later = Plan::sent(PlanId::new("plan-4"), &parent, "call-2", "Read the cache");

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
                .errands_of(&parent.id, "call-1")
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
            vec![PlanId::new("plan-2"), PlanId::new("plan-3")],
            "a call finds its own errands, in the order they were sent, and not \
             the ones a later call sent"
        );
    }

    /// Plans are the one thing disk cannot tell us again, and `errand_for` is a
    /// new field on a type that is already recorded. Additive-serde is a claim,
    /// not a fact, and the cost of being wrong is a kingdom that will not open.
    #[test]
    fn a_plan_recorded_before_errands_existed_still_loads() {
        let before_errands = r#"{
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
            serde_json::from_str(before_errands).expect("an older plan record must still load");

        assert!(
            !plan.is_errand(),
            "a plan recorded before errands existed is the King's own work, and \
             must not be mistaken for something the court sent"
        );
    }
}
