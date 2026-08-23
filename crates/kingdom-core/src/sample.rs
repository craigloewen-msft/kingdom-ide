//! Placeholder data for the throne room.
//!
//! Cities are **real** and come from scanning the chosen folder. Plans opened
//! from the chat dock are **real** too, and drafted by an actual model. What
//! this module fabricates is a *starting* court, so a freshly opened kingdom
//! has something to show before the King has issued a single decree.
//!
//! It deliberately seeds a **blocked plan** and a **contended resource**. Those
//! are the states the whole product exists to make visible, so they have to be
//! reachable on day one -- tidying this into an all-quiet court would make the
//! most important visuals unreachable during development.

use crate::ids::*;
use crate::model::*;

/// Fabricates a court of plans and crown resources for the given cities, so the
/// map has something to show on first run.
pub fn populate_court(cities: &[City]) -> (Vec<Plan>, Vec<Resource>) {
    if cities.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let port = ResourceId::new("res-port-3000");
    let cargo = ResourceId::new("res-cargo-lock");
    let gpu = ResourceId::new("res-gpu");

    // Each scripted plan: (id, decree, status, the leases it holds).
    let scripted = [
        (
            "plan-ramparts",
            "Refactor the auth module",
            PlanStatus::Drafting,
            vec![(port.clone(), LeaseMode::Exclusive, "Running the dev server")],
        ),
        (
            "plan-aqueduct",
            "Run the integration tests end to end",
            PlanStatus::Blocked,
            vec![],
        ),
        (
            "plan-foundations",
            "Design a new storage layer",
            PlanStatus::AwaitingReview,
            vec![(cargo.clone(), LeaseMode::Shared, "Building dependencies")],
        ),
    ];

    let mut plans = Vec::new();
    for (i, (id, prompt, status, leases)) in scripted.into_iter().enumerate() {
        let city = &cities[i % cities.len()];
        let id = PlanId::new(id);

        let mut plan = Plan::opened(id.clone(), city.id.clone(), prompt, "mock");
        plan.title = format!("{} of {}", plan_title(i), city.name);
        plan.summary = match status {
            PlanStatus::Blocked => format!(
                "Waiting on the dev server port before it can test {}.",
                city.name
            ),
            _ => format!("Proposes structural changes to {}.", city.name),
        };
        plan.status = status;
        plan.touches = notable_files(city, 3);
        plan.say(Speaker::Court, &plan.summary.clone());
        plan.leases = leases
            .into_iter()
            .map(|(resource, mode, reason)| Lease {
                resource,
                holder: id.clone(),
                mode,
                reason: reason.to_string(),
            })
            .collect();

        plans.push(plan);
    }

    // Settled plans, so the sidebar's "All" filter has history to reveal.
    // Without at least one approved and one rejected plan, the filter looks
    // broken rather than empty.
    let first = &cities[0];
    let mut approved = Plan::opened(
        PlanId::new("plan-settled-approved"),
        first.id.clone(),
        "Harden the error paths",
        "mock",
    );
    approved.title = format!("The Old Ramparts of {}", first.name);
    approved.summary = "Hardened the error paths. Approved and built.".into();
    approved.status = PlanStatus::Approved;
    approved.touches = vec!["src/lib.rs".into()];
    plans.push(approved);

    let mut rejected = Plan::opened(
        PlanId::new("plan-settled-rejected"),
        first.id.clone(),
        "Rewrite the scanner from scratch",
        "mock",
    );
    rejected.title = format!("The Folly of {}", first.name);
    rejected.summary = "Proposed rewriting the scanner. Refused.".into();
    rejected.status = PlanStatus::Rejected;
    rejected.touches = vec!["src/scan.rs".into()];
    plans.push(rejected);

    let holders_of = |id: &ResourceId| -> Vec<Lease> {
        plans
            .iter()
            .flat_map(|p| p.leases.iter())
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
            // The aqueduct plan is queued behind the ramparts plan: this is the
            // contention the map renders as a red thread between two cities.
            waiting: vec![PlanId::new("plan-aqueduct")],
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

    (plans, resources)
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
/// this module seeds a blocked plan rather than a tidy idle court: the
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
