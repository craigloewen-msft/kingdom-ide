//! Placeholder data for the throne room.
//!
//! Real architects and leases do not exist yet — no agents are actually being
//! run. This module fabricates a plausible court so the UI can be built and
//! judged against realistic shapes, including the states that matter most:
//! a blocked architect and a contended resource.
//!
//! Cities, by contrast, are **real** and come from scanning the chosen folder.
//! Only the agent layer is invented.

use crate::ids::*;
use crate::model::*;

/// Fabricates a court of architects, plans and crown resources for the given
/// cities, so the map has something to show on first run.
pub fn populate_court(cities: &[City]) -> (Vec<Architect>, Vec<Plan>, Vec<Resource>) {
    if cities.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let port = ResourceId::new("res-port-3000");
    let cargo = ResourceId::new("res-cargo-lock");
    let gpu = ResourceId::new("res-gpu");

    let mut architects = Vec::new();
    let mut plans = Vec::new();

    // Deliberately seed the interesting states rather than all-idle: the whole
    // point of the map is showing trouble, so trouble must be visible on day one.
    let scripted = [
        (
            "Vitruvius",
            ArchitectStatus::Working,
            "Refactoring the auth module",
            vec![(port.clone(), LeaseMode::Exclusive, "Running the dev server")],
        ),
        (
            "Imhotep",
            ArchitectStatus::Blocked,
            "Waiting on port 3000 to run integration tests",
            vec![],
        ),
        (
            "Hypatia",
            ArchitectStatus::AwaitingReview,
            "Proposed a new storage layer",
            vec![(cargo.clone(), LeaseMode::Shared, "Building dependencies")],
        ),
        (
            "Brunelleschi",
            ArchitectStatus::Idle,
            "Awaiting a decree",
            vec![],
        ),
    ];

    for (i, (name, status, activity, leases)) in scripted.into_iter().enumerate() {
        let city = &cities[i % cities.len()];
        let id = ArchitectId::new(format!("arch-{}", name.to_lowercase()));

        architects.push(Architect {
            id: id.clone(),
            name: name.to_string(),
            city: city.id.clone(),
            status,
            activity: activity.to_string(),
            leases: leases
                .into_iter()
                .map(|(resource, mode, reason)| Lease {
                    resource,
                    holder: id.clone(),
                    mode,
                    reason: reason.to_string(),
                })
                .collect(),
        });

        if matches!(
            status,
            ArchitectStatus::AwaitingReview | ArchitectStatus::Working
        ) {
            plans.push(Plan {
                id: PlanId::new(format!("plan-{i}")),
                title: format!("{} of {}", plan_title(i), city.name),
                summary: format!(
                    "{name} proposes structural changes to {}. Awaiting royal assent.",
                    city.name
                ),
                city: city.id.clone(),
                author: id,
                status: if status == ArchitectStatus::AwaitingReview {
                    PlanStatus::AwaitingReview
                } else {
                    PlanStatus::Draft
                },
                touches: notable_files(city, 3),
            });
        }
    }

    // Settled plans, so the sidebar's "All" filter has history to reveal.
    // Without at least one approved and one rejected plan, the filter looks
    // broken rather than empty.
    let first = &cities[0];
    plans.push(Plan {
        id: PlanId::new("plan-settled-approved"),
        title: format!("The Old Ramparts of {}", first.name),
        summary: "Hardened the error paths. Approved and built.".into(),
        city: first.id.clone(),
        author: ArchitectId::new("arch-vitruvius"),
        status: PlanStatus::Approved,
        touches: vec!["src/lib.rs".into()],
    });
    plans.push(Plan {
        id: PlanId::new("plan-settled-rejected"),
        title: format!("The Folly of {}", first.name),
        summary: "Proposed rewriting the scanner. Refused.".into(),
        city: first.id.clone(),
        author: ArchitectId::new("arch-hypatia"),
        status: PlanStatus::Rejected,
        touches: vec!["src/scan.rs".into()],
    });

    let holders_of = |id: &ResourceId| -> Vec<Lease> {
        architects
            .iter()
            .flat_map(|a| a.leases.iter())
            .filter(|l| &l.resource == id)
            .cloned()
            .collect()
    };

    let resources = vec![
        Resource {
            id: port.clone(),
            name: "Dev server :3000".into(),
            kind: ResourceKind::Port(3000),
            holders: holders_of(&port),
            // Imhotep is queued behind Vitruvius: this is the contention the
            // map renders as a red thread between two cities.
            waiting: vec![ArchitectId::new("arch-imhotep")],
        },
        Resource {
            id: cargo.clone(),
            name: "Cargo build lock".into(),
            kind: ResourceKind::BuildLock,
            holders: holders_of(&cargo),
            waiting: vec![],
        },
        Resource {
            id: gpu.clone(),
            name: "GPU 0".into(),
            kind: ResourceKind::Gpu,
            holders: vec![],
            waiting: vec![],
        },
    ];

    (architects, plans, resources)
}

fn plan_title(i: usize) -> &'static str {
    match i % 4 {
        0 => "The Great Refactoring",
        1 => "The Aqueduct",
        2 => "The New Foundations",
        _ => "The Curtain Wall",
    }
}

/// Picks real files from a city to stand in as a plan's touched paths.
///
/// Hardcoded paths like `src/lib.rs` match nothing in most projects, which
/// would leave the map's plan highlighting dead on arrival -- the same reason
/// this module seeds a blocked architect rather than a tidy idle court: the
/// states the UI exists to show have to be reachable on day one.
fn notable_files(city: &City, want: usize) -> Vec<String> {
    let Some(structure) = &city.structure else {
        return Vec::new();
    };

    // Prefer source over config or docs: a plan touching `Cargo.lock` is not a
    // convincing rehearsal of the King's review loop.
    let mut found: Vec<(bool, u64, String)> = Vec::new();
    collect(structure, &mut found);
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

fn collect(district: &District, out: &mut Vec<(bool, u64, String)>) {
    for b in &district.buildings {
        let is_source = matches!(
            b.ward,
            Ward::Rust | Ward::Web | Ward::Python | Ward::Go | Ward::Systems
        );
        // Absurdly large files are almost always vendored or generated, and a
        // plan claiming to touch one is not a believable rehearsal.
        if b.bulk > 400_000 {
            continue;
        }
        out.push((is_source, b.bulk, b.path.clone()));
    }
    for child in &district.children {
        collect(child, out);
    }
}
