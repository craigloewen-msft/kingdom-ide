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
    pub architects: Vec<Architect>,
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
            architects: Vec::new(),
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

    /// Architects currently stationed in a given city.
    pub fn architects_in<'a>(&'a self, id: &'a CityId) -> impl Iterator<Item = &'a Architect> + 'a {
        self.architects.iter().filter(move |a| &a.city == id)
    }

    /// Plans still awaiting the King's judgement.
    pub fn pending_plans(&self) -> impl Iterator<Item = &Plan> {
        self.plans
            .iter()
            .filter(|p| p.status == PlanStatus::AwaitingReview)
    }

    /// Every resource that more than one architect is currently contending for.
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
// Architects (agents)
// ---------------------------------------------------------------------------

/// An agent at work in the kingdom. Always stationed in exactly one city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Architect {
    pub id: ArchitectId,
    pub name: String,
    /// The city this architect is working in.
    pub city: CityId,
    pub status: ArchitectStatus,
    /// One-line description of what it is doing right now.
    pub activity: String,
    /// Crown resources this architect currently holds.
    pub leases: Vec<Lease>,
}

/// What an architect is doing. Drives both the sidebar badge and the map glow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchitectStatus {
    /// Available, awaiting a decree.
    Idle,
    /// Actively working.
    Working,
    /// Has submitted a plan and is waiting on the King.
    AwaitingReview,
    /// Cannot proceed: wants a resource another architect holds.
    Blocked,
}

impl ArchitectStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ArchitectStatus::Idle => "Idle",
            ArchitectStatus::Working => "Working",
            ArchitectStatus::AwaitingReview => "Awaiting review",
            ArchitectStatus::Blocked => "Blocked",
        }
    }

    pub fn color(&self) -> &'static str {
        match self {
            ArchitectStatus::Idle => "#64748b",
            ArchitectStatus::Working => "#22c55e",
            ArchitectStatus::AwaitingReview => "#eab308",
            ArchitectStatus::Blocked => "#ef4444",
        }
    }

    /// CSS class suffix, e.g. `status-working`.
    pub fn css_suffix(&self) -> &'static str {
        match self {
            ArchitectStatus::Idle => "idle",
            ArchitectStatus::Working => "working",
            ArchitectStatus::AwaitingReview => "review",
            ArchitectStatus::Blocked => "blocked",
        }
    }
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/// An architectural plan: an agent's proposal, submitted for royal review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub title: String,
    pub summary: String,
    pub city: CityId,
    pub author: ArchitectId,
    pub status: PlanStatus,
    /// Files the plan proposes to touch.
    pub touches: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    Draft,
    AwaitingReview,
    Approved,
    Rejected,
}

impl PlanStatus {
    pub fn label(&self) -> &'static str {
        match self {
            PlanStatus::Draft => "Draft",
            PlanStatus::AwaitingReview => "Awaiting review",
            PlanStatus::Approved => "Approved",
            PlanStatus::Rejected => "Rejected",
        }
    }
}

// ---------------------------------------------------------------------------
// Tasks (decrees)
// ---------------------------------------------------------------------------

/// A unit of work issued by the King from the chat dock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// What the King asked for, verbatim.
    pub prompt: String,
    /// Target city, if one was chosen.
    pub city: Option<CityId>,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Assigned,
    Complete,
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
    /// Architects queued for it, in request order.
    pub waiting: Vec<ArchitectId>,
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
    pub holder: ArchitectId,
    pub mode: LeaseMode,
    /// Why the architect needs it, in plain language.
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
                    holder: ArchitectId::new(format!("a{i}")),
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
}
