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
    pub resources: Vec<Resource>,
}

impl Kingdom {
    /// An empty kingdom, shown before any folder has been chosen.
    pub fn unopened() -> Self {
        Self {
            name: "No Kingdom".into(),
            root: String::new(),
            cities: Vec::new(),
            plans: Vec::new(),
            resources: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        !self.root.is_empty()
    }

    pub fn city(&self, id: &CityId) -> Option<&City> {
        self.cities.iter().find(|c| &c.id == id)
    }

    /// Plans drawn up in a given city.
    pub fn plans_in<'a>(&'a self, id: &'a CityId) -> impl Iterator<Item = &'a Plan> + 'a {
        self.plans.iter().filter(move |p| &p.city == id)
    }

    pub fn plan(&self, id: &PlanId) -> Option<&Plan> {
        self.plans.iter().find(|p| &p.id == id)
    }

    /// Plans still awaiting the King's judgement.
    pub fn pending_plans(&self) -> impl Iterator<Item = &Plan> {
        self.plans
            .iter()
            .filter(|p| p.status == PlanStatus::AwaitingReview)
    }

    /// Every resource that more than one plan is currently contending for.
    ///
    /// This is the signal the map draws in red: it is the moment two agents are
    /// about to trip over each other.
    pub fn contended_resources(&self) -> impl Iterator<Item = &Resource> {
        self.resources.iter().filter(|r| r.is_contended())
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
    /// Path relative to the city root. This is the join key that lets a
    /// [`Plan`]'s `touches` list light up an exact building on the map.
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
    pub summary: String,
    /// Files the plan proposes to touch. This is the join key that lights up
    /// exact buildings on the map.
    pub touches: Vec<String>,
    /// The decree that opened this plan, verbatim.
    pub prompt: String,
    /// Which model is drafting it, e.g. `"mock"` or `"copilot/claude-opus-5"`.
    pub model: String,
    /// How hard that model was asked to think. `None` is its own default,
    /// which is a different request from any explicit level.
    pub effort: Option<ModelEffort>,
    /// The conversation so far, King and court alternating.
    pub transcript: Vec<Utterance>,
    pub status: PlanStatus,
    /// Crown resources this plan holds while drafting.
    pub leases: Vec<Lease>,
}

impl Plan {
    /// A plan that has just been opened by a decree, before any drafting.
    pub fn opened(
        id: PlanId,
        city: CityId,
        prompt: impl Into<String>,
        choice: &ModelChoice,
    ) -> Self {
        let prompt = prompt.into();
        Self {
            id,
            city,
            title: title_from_prompt(&prompt),
            summary: String::new(),
            touches: Vec::new(),
            transcript: vec![Utterance {
                speaker: Speaker::King,
                body: prompt.clone(),
            }],
            prompt,
            model: choice.model.clone(),
            effort: choice.effort,
            status: PlanStatus::Drafting,
            leases: Vec::new(),
        }
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
        !matches!(self.status, PlanStatus::Approved | PlanStatus::Rejected)
    }

    pub fn say(&mut self, speaker: Speaker, body: impl Into<String>) {
        self.transcript.push(Utterance {
            speaker,
            body: body.into(),
        });
    }
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

/// One line of a plan's conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Utterance {
    pub speaker: Speaker,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Speaker {
    /// The user.
    King,
    /// The model drafting the plan.
    Court,
}

/// Where a plan stands.
///
/// This absorbs what a separate architect status used to carry: `Drafting` is
/// an agent working, `Blocked` is an agent that could not get a lease. They were
/// always two views of one state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlanStatus {
    /// A model is drafting it right now.
    Drafting,
    /// Drafted, and waiting on the King.
    AwaitingReview,
    /// Cannot proceed: wants a crown resource another plan holds.
    Blocked,
    /// The model could not be reached, or refused.
    Failed,
    Approved,
    Rejected,
}

impl PlanStatus {
    /// Every state, in the order the map legend lists them: live states first,
    /// then settled history.
    pub const ALL: [PlanStatus; 6] = [
        PlanStatus::Drafting,
        PlanStatus::AwaitingReview,
        PlanStatus::Blocked,
        PlanStatus::Failed,
        PlanStatus::Approved,
        PlanStatus::Rejected,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            PlanStatus::Drafting => "Drafting",
            PlanStatus::AwaitingReview => "Awaiting review",
            PlanStatus::Blocked => "Blocked",
            PlanStatus::Failed => "Failed",
            PlanStatus::Approved => "Approved",
            PlanStatus::Rejected => "Rejected",
        }
    }

    pub fn color(&self) -> &'static str {
        match self {
            PlanStatus::Drafting => "#22c55e",
            PlanStatus::AwaitingReview => "#eab308",
            PlanStatus::Blocked => "#ef4444",
            PlanStatus::Failed => "#f97316",
            PlanStatus::Approved => "#38bdf8",
            PlanStatus::Rejected => "#64748b",
        }
    }

    /// CSS class suffix, e.g. `status-drafting`.
    pub fn css_suffix(&self) -> &'static str {
        match self {
            PlanStatus::Drafting => "drafting",
            PlanStatus::AwaitingReview => "review",
            PlanStatus::Blocked => "blocked",
            PlanStatus::Failed => "failed",
            PlanStatus::Approved => "approved",
            PlanStatus::Rejected => "rejected",
        }
    }
}

// ---------------------------------------------------------------------------
// Model access -- what the King can see about how plans get drafted
// ---------------------------------------------------------------------------

/// Which backend drafts plans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelProvider {
    /// Deterministic, offline, no credential. The default, so a fresh clone
    /// works with no setup.
    Mock,
    /// GitHub Copilot's chat completions API.
    Copilot,
}

impl ModelProvider {
    pub fn label(&self) -> &'static str {
        match self {
            ModelProvider::Mock => "mock",
            ModelProvider::Copilot => "copilot",
        }
    }
}

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

/// What the dock's provider badge renders, and what its panel explains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStatus {
    pub provider: ModelProvider,
    /// The model plans will be drafted with, e.g. `"claude-sonnet-4.6"`.
    pub model: String,
    pub credential: CredentialState,
    /// Plain-language detail: where the credential came from, or what to set to
    /// fix it. Safe to display.
    pub detail: String,
}

impl ModelStatus {
    pub fn is_ready(&self) -> bool {
        self.credential == CredentialState::Ready
    }
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

    /// Which backend this choice routes to. Derived from the id rather than
    /// held separately, so provider and model cannot disagree.
    pub fn provider(&self) -> ModelProvider {
        match self.model.split_once('/') {
            Some(("copilot", _)) => ModelProvider::Copilot,
            _ => ModelProvider::Mock,
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
// Crown Resources & Leases -- the coordination core
// ---------------------------------------------------------------------------

/// A shared machine resource that agents must coordinate over.
///
/// This is the heart of Kingdom IDE. Two agents running `cargo test` in the
/// same worktree, or both binding port 3000, is the failure mode the whole
/// product exists to make visible and then to prevent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub id: ResourceId,
    pub name: String,
    pub kind: ResourceKind,
    /// Leases currently granted over this resource.
    pub holders: Vec<Lease>,
    /// Plans queued for it, in request order.
    pub waiting: Vec<PlanId>,
}

impl Resource {
    /// True when somebody is waiting behind a current holder.
    pub fn is_contended(&self) -> bool {
        !self.waiting.is_empty()
    }

    pub fn is_held(&self) -> bool {
        !self.holders.is_empty()
    }

    /// Whether a new lease in `mode` could be granted right now.
    ///
    /// Exclusive access requires an empty field; shared access composes with
    /// other shared holders but never with an exclusive one.
    pub fn can_grant(&self, mode: LeaseMode) -> bool {
        match mode {
            LeaseMode::Exclusive => self.holders.is_empty(),
            LeaseMode::Shared => self.holders.iter().all(|l| l.mode == LeaseMode::Shared),
        }
    }
}

/// The category of a crown resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceKind {
    /// A TCP port, e.g. a dev server on 3000.
    Port(u16),
    /// A database or other stateful service.
    Database,
    /// The GPU.
    Gpu,
    /// A git worktree or branch.
    Worktree,
    /// A filesystem path.
    Path,
    /// A build or package-manager lock, e.g. `~/.cargo`.
    BuildLock,
}

impl ResourceKind {
    pub fn label(&self) -> String {
        match self {
            ResourceKind::Port(p) => format!("Port {p}"),
            ResourceKind::Database => "Database".into(),
            ResourceKind::Gpu => "GPU".into(),
            ResourceKind::Worktree => "Worktree".into(),
            ResourceKind::Path => "Path".into(),
            ResourceKind::BuildLock => "Build lock".into(),
        }
    }

    /// Glyph shown beside the resource in the sidebar.
    pub fn glyph(&self) -> &'static str {
        match self {
            ResourceKind::Port(_) => "⚓",
            ResourceKind::Database => "🗄",
            ResourceKind::Gpu => "⚙",
            ResourceKind::Worktree => "🌿",
            ResourceKind::Path => "📁",
            ResourceKind::BuildLock => "🔒",
        }
    }
}

/// A granted claim on a crown resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub resource: ResourceId,
    pub holder: PlanId,
    pub mode: LeaseMode,
    /// Why the plan needs it, in plain language.
    pub reason: String,
}

/// Exclusive or shared access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseMode {
    /// Sole access. Nobody else, in any mode.
    Exclusive,
    /// Concurrent read-style access, compatible with other shared holders.
    Shared,
}

impl LeaseMode {
    pub fn label(&self) -> &'static str {
        match self {
            LeaseMode::Exclusive => "exclusive",
            LeaseMode::Shared => "shared",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource_held_by(modes: &[LeaseMode]) -> Resource {
        Resource {
            id: ResourceId::new("r"),
            name: "test".into(),
            kind: ResourceKind::Port(3000),
            holders: modes
                .iter()
                .enumerate()
                .map(|(i, m)| Lease {
                    resource: ResourceId::new("r"),
                    holder: PlanId::new(format!("p{i}")),
                    mode: *m,
                    reason: String::new(),
                })
                .collect(),
            waiting: Vec::new(),
        }
    }

    /// The compatibility matrix is the one rule that keeps two agents from
    /// silently colliding, so it is worth pinning precisely.
    #[test]
    fn lease_compatibility_matrix() {
        let free = resource_held_by(&[]);
        assert!(free.can_grant(LeaseMode::Exclusive));
        assert!(free.can_grant(LeaseMode::Shared));

        let shared = resource_held_by(&[LeaseMode::Shared, LeaseMode::Shared]);
        assert!(shared.can_grant(LeaseMode::Shared));
        assert!(
            !shared.can_grant(LeaseMode::Exclusive),
            "exclusive access must wait for shared readers to drain"
        );

        let exclusive = resource_held_by(&[LeaseMode::Exclusive]);
        assert!(!exclusive.can_grant(LeaseMode::Shared));
        assert!(!exclusive.can_grant(LeaseMode::Exclusive));
    }

    fn catalogue() -> ModelCatalogue {
        ModelCatalogue {
            options: vec![
                ModelOption {
                    id: "mock".into(),
                    label: "Mock".into(),
                    vendor: "Offline".into(),
                    context_window: 8_000,
                    recommended: true,
                    efforts: Vec::new(),
                },
                ModelOption {
                    id: "copilot/claude-opus-5".into(),
                    label: "Claude Opus 5".into(),
                    vendor: "Anthropic".into(),
                    context_window: 1_000_000,
                    recommended: true,
                    efforts: vec![ModelEffort::Low, ModelEffort::High],
                },
            ],
            default_id: "mock".into(),
            credential: CredentialState::Ready,
            detail: String::new(),
        }
    }

    /// The King's browser remembers a choice for longer than any catalogue
    /// lives. A withdrawn model or an effort a model no longer declares must
    /// degrade quietly -- if either could error, last week's localStorage would
    /// wedge today's dock, and sending an undeclared effort earns an opaque 400.
    #[test]
    fn a_stale_remembered_choice_degrades_rather_than_erroring() {
        let catalogue = catalogue();

        let withdrawn = ModelChoice::new("copilot/gone-last-year", Some(ModelEffort::High));
        assert_eq!(
            catalogue.resolve(Some(&withdrawn)),
            ModelChoice::new("mock", None),
            "an unknown model falls back to the default, and takes no effort with it"
        );

        let undeclared = ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::Max));
        assert_eq!(
            catalogue.resolve(Some(&undeclared)),
            ModelChoice::new("copilot/claude-opus-5", None),
            "an effort the model does not declare falls back to the model's own default"
        );

        let good = ModelChoice::new("copilot/claude-opus-5", Some(ModelEffort::Low));
        assert_eq!(catalogue.resolve(Some(&good)), good);
        assert_eq!(catalogue.resolve(None), ModelChoice::new("mock", None));
    }

    /// The provider is read off the id so the two cannot disagree; a plan drawn
    /// by Copilot must never be re-drafted by the mock because a separate
    /// provider setting drifted.
    #[test]
    fn a_choice_routes_by_its_own_id() {
        let copilot = ModelChoice::new("copilot/claude-opus-5", None);
        assert_eq!(copilot.provider(), ModelProvider::Copilot);
        assert_eq!(copilot.api_name(), "claude-opus-5");

        let mock = ModelChoice::new("mock", None);
        assert_eq!(mock.provider(), ModelProvider::Mock);
        assert_eq!(mock.api_name(), "mock");
    }
}
