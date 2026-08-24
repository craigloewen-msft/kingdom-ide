//! Opening courts a realm can choose between.
//!
//! A court is the plans and crown resources a kingdom starts with, before the
//! King has issued a decree. [`crate::sample::populate_court`] already builds
//! one over whatever cities were scanned; the only limitation was that there
//! was exactly one of it. So these are simply more functions of the same
//! signature, and a realm names the one it wants.
//!
//! Per `AGENTS.md`, the *default* court must open showing trouble -- a blocked
//! plan and a contended resource are the states the whole product exists to make
//! visible, and an all-quiet opening court would make them unreachable during
//! development. [`quiet_court`] exists so that state can be looked at
//! deliberately, never by accident.

use crate::ids::{PlanId, ResourceId};
use crate::model::{
    City, Lease, LeaseMode, ModelChoice, Plan, PlanStatus, Resource, ResourceKind, Speaker,
};

/// The standard opening court: whatever [`crate::sample::populate_court`] makes.
///
/// Most realms want this. It is the same court the real-folder path gets, so a
/// proving ground rehearses what the King actually sees.
pub fn default_court(cities: &[City]) -> (Vec<Plan>, Vec<Resource>) {
    crate::sample::populate_court(cities)
}

/// Nothing blocked, nothing contended.
///
/// Deliberately not the default. Useful for looking at the calm state on
/// purpose -- e.g. checking that the map reads well *without* red threads, which
/// is otherwise impossible to see.
pub fn quiet_court(cities: &[City]) -> (Vec<Plan>, Vec<Resource>) {
    let Some(city) = cities.first() else {
        return (Vec::new(), Vec::new());
    };

    let mock = ModelChoice::new("mock", None);
    let mut plan = Plan::opened(
        PlanId::new("plan-quiet"),
        city.id.clone(),
        "Tidy the documentation",
        &mock,
    );
    plan.title = format!("The Quiet Works of {}", city.name);
    plan.summary = "Nothing is contended. All is well.".into();
    plan.status = PlanStatus::AwaitingReview;
    plan.say(Speaker::Court, plan.summary.clone());

    (vec![plan], Vec::new())
}

/// One scripted plan in a hand-built court: id, decree, status, leases held.
type Scripted = (
    &'static str,
    &'static str,
    PlanStatus,
    Vec<(ResourceId, LeaseMode, &'static str)>,
);

/// One port, one holder, two waiters -- plus a second contended resource.
///
/// The map's red threads at their worst. This is the scenario that answers
/// "what does the realm look like when three plans want the same thing?", which
/// is unreachable from the default court and is precisely the picture the
/// product exists to draw.
pub fn three_way_contention(cities: &[City]) -> (Vec<Plan>, Vec<Resource>) {
    if cities.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let port = ResourceId::new("res-port-3000");
    let cargo = ResourceId::new("res-cargo-lock");
    let gpu = ResourceId::new("res-gpu");
    let mock = ModelChoice::new("mock", None);

    // (id, decree, status, leases held)
    let scripted: [Scripted; 5] = [
        (
            "plan-ramparts",
            "Run the dev server and watch for regressions",
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
            "plan-cistern",
            "Smoke-test the login flow in a browser",
            PlanStatus::Blocked,
            vec![],
        ),
        (
            "plan-foundry",
            "Rebuild the workspace from scratch",
            PlanStatus::Drafting,
            vec![(
                cargo.clone(),
                LeaseMode::Exclusive,
                "Rebuilding dependencies",
            )],
        ),
        (
            "plan-bellows",
            "Compile the release profile",
            PlanStatus::Blocked,
            vec![],
        ),
    ];

    let mut plans = Vec::new();
    for (i, (id, prompt, status, leases)) in scripted.into_iter().enumerate() {
        // Spread across cities so the map draws threads *between* cities, which
        // is the whole visual: contention within one city needs no map to see.
        let city = &cities[i % cities.len()];
        let id = PlanId::new(id);

        let mut plan = Plan::opened(id.clone(), city.id.clone(), prompt, &mock);
        plan.status = status;
        plan.summary = match status {
            PlanStatus::Blocked => format!(
                "Waiting on a crown resource held elsewhere in {}.",
                city.name
            ),
            _ => format!("Holding a crown resource while it works in {}.", city.name),
        };
        plan.say(Speaker::Court, plan.summary.clone());
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
            waiting: vec![PlanId::new("plan-aqueduct"), PlanId::new("plan-cistern")],
        },
        Resource {
            id: cargo.clone(),
            name: "Cargo build lock".into(),
            kind: ResourceKind::BuildLock,
            holders: holders_of(&cargo),
            waiting: vec![PlanId::new("plan-bellows")],
        },
        Resource {
            id: gpu,
            name: "GPU 0".into(),
            kind: ResourceKind::Gpu,
            holders: vec![],
            waiting: vec![],
        },
    ];

    (plans, resources)
}

/// Checks a court for references that would silently vanish from the map.
///
/// A `waiting` entry naming a plan that does not exist drops the red thread
/// between two cities -- the single most important thing the map draws, and a
/// failure with no visible symptom other than the absence of a line nobody
/// noticed was missing. Shared rather than inlined into one test because that
/// mistake is now reachable from every court function.
pub fn audit(plans: &[Plan], resources: &[Resource]) -> Result<(), String> {
    for resource in resources {
        for waiter in &resource.waiting {
            if !plans.iter().any(|p| &p.id == waiter) {
                return Err(format!(
                    "resource {} waits on unknown plan {waiter}",
                    resource.id
                ));
            }
        }
        for lease in &resource.holders {
            if !plans.iter().any(|p| p.id == lease.holder) {
                return Err(format!(
                    "resource {} is held by unknown plan {}",
                    resource.id, lease.holder
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CityId;
    use crate::model::CityKind;

    fn cities() -> Vec<City> {
        ["Alpha", "Beta", "Gamma"]
            .into_iter()
            .map(|name| City {
                id: CityId::new(name.to_lowercase()),
                name: name.into(),
                path: name.to_lowercase(),
                kind: CityKind::Rust,
                file_count: 10,
                has_git: true,
                dirty_files: 0,
                structure: None,
            })
            .collect()
    }

    /// The realm named `contended` exists solely to draw a multi-way fight. If
    /// its court stopped producing one -- or produced one referencing plans that
    /// do not exist -- the realm would still seed and open, and the thing it was
    /// built to show would simply be absent from the map.
    #[test]
    fn three_way_contention_actually_contends() {
        let cities = cities();
        let (plans, resources) = three_way_contention(&cities);

        audit(&plans, &resources).expect("court must not reference plans that do not exist");

        let deepest = resources
            .iter()
            .map(|r| r.waiting.len())
            .max()
            .unwrap_or_default();
        assert!(
            deepest >= 2,
            "the contended realm must queue at least two plans behind one resource"
        );
        assert!(
            resources.iter().filter(|r| r.is_contended()).count() >= 2,
            "more than one resource should be contended, so threads cross"
        );
    }
}
