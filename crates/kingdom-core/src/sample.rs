//! Placeholder data for the throne room.
//!
//! Cities are **real** and come from scanning the chosen folder. Plans opened
//! from the decree bar are **real** too, and drafted by an actual model. What
//! this module fabricates is a *starting* court, so a freshly opened kingdom
//! has something to show before the King has issued a single decree.
//!
//! It deliberately seeds a **failed plan** alongside ones awaiting review. A
//! court that opens all-quiet makes the states worth noticing unreachable
//! during development, which is exactly when a refactor would quietly lose
//! them. A test pins this.

use crate::ids::*;
use crate::model::*;

/// Fabricates a court of plans for the given cities, so the map has something
/// to show on first run.
pub fn populate_court(cities: &[City]) -> Vec<Plan> {
    if cities.is_empty() {
        return Vec::new();
    }

    // Each scripted plan: (id, decree, status).
    let scripted = [
        (
            "plan-ramparts",
            "Refactor the auth module",
            PlanStatus::Drafting,
        ),
        (
            "plan-aqueduct",
            "Run the integration tests end to end",
            PlanStatus::Failed,
        ),
        (
            "plan-foundations",
            "Design a new storage layer",
            PlanStatus::AwaitingReview,
        ),
    ];

    let mock = ModelChoice::new("mock", None);
    let mut plans = Vec::new();
    for (i, (id, prompt, status)) in scripted.into_iter().enumerate() {
        let city = &cities[i % cities.len()];
        let id = PlanId::new(id);

        let mut plan = Plan::opened(
            id.clone(),
            city.id.clone(),
            prompt,
            &mock,
            Workspace::in_place(&city.path),
        );
        plan.title = format!("{} of {}", plan_title(i), city.name);
        plan.summary = match status {
            PlanStatus::Failed => format!("The court could not reach a model for {}.", city.name),
            _ => format!("Proposes structural changes to {}.", city.name),
        };
        plan.status = status;
        match status {
            PlanStatus::Failed => plan.note(NoteKind::Failed, plan.summary.clone()),
            _ => plan.say(Speaker::Court, plan.summary.clone()),
        }
        // A plan mid-draft is the one state that carries a `working_on`, and it
        // is what puts a crane over the city on the map.
        if status == PlanStatus::Drafting {
            plan.working_on = Some(format!("Reading {} to draft a plan", city.name));
        }

        plans.push(plan);
    }

    // Settled plans, so the sidebar's "All" filter has history to reveal.
    // Without at least one merged and one archived plan, the filter looks
    // broken rather than empty.
    let first = &cities[0];
    let mut merged = Plan::opened(
        PlanId::new("plan-settled-merged"),
        first.id.clone(),
        "Harden the error paths",
        &mock,
        Workspace::in_place(&first.path),
    );
    merged.title = format!("The Old Ramparts of {}", first.name);
    merged.summary = "Hardened the error paths. Landed on main.".into();
    merged.settle(Outcome::Merged {
        commit: "0000000fabricated".into(),
        into: "main".into(),
    });
    plans.push(merged);

    let mut archived = Plan::opened(
        PlanId::new("plan-settled-archived"),
        first.id.clone(),
        "Rewrite the scanner from scratch",
        &mock,
        Workspace::in_place(&first.path),
    );
    archived.title = format!("The Folly of {}", first.name);
    archived.summary = "Proposed rewriting the scanner. Set aside.".into();
    archived.settle(Outcome::Archived {
        branch: "kingdom/fabricated".into(),
        tip: "0000000fabricated".into(),
        base: "main".into(),
        patch: None,
    });
    plans.push(archived);

    plans
}

fn plan_title(i: usize) -> &'static str {
    match i % 4 {
        0 => "The Great Refactoring",
        1 => "The Aqueduct",
        2 => "The New Foundations",
        _ => "The Curtain Wall",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn city(id: &str, name: &str) -> City {
        City {
            id: CityId::new(id),
            name: name.into(),
            path: name.to_lowercase(),
            kind: CityKind::Rust,
            file_count: 10,
            has_git: true,
            dirty_files: 0,
            structure: None,
        }
    }

    /// The court must open showing something worth the King's attention.
    ///
    /// A court of nothing but tidy settled plans makes the states the UI exists
    /// to render unreachable during development -- and a refactor is exactly
    /// when that would happen quietly. It used to be a blocked plan and a
    /// contended resource; with lease arbitration gone, a *failed* plan is the
    /// honest equivalent, because it is a state the running product can
    /// genuinely produce.
    #[test]
    fn the_opening_court_always_shows_trouble_and_history() {
        let cities = vec![city("c1", "Alpha"), city("c2", "Beta")];
        let plans = populate_court(&cities);

        assert!(
            plans.iter().any(|p| p.status == PlanStatus::Failed),
            "a plan in trouble must be visible on first run"
        );
        assert!(
            plans.iter().any(|p| p.status == PlanStatus::Drafting),
            "a plan mid-flight must be visible, so the map draws a crane"
        );
        assert!(
            plans.iter().any(|p| !p.is_live()),
            "settled history must exist, or the rail's All filter looks broken"
        );
    }
}
