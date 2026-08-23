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
