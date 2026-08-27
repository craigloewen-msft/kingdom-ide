//! Placeholder data for the throne room.
//!
//! Cities are **real** and come from scanning the chosen folder. Plans opened
//! from the prompt bar are **real** too, and drafted by an actual model. What
//! this module fabricates is a *starting* set of plans, so a freshly opened
//! kingdom has something to show before the user has issued a single prompt.
//!
//! It deliberately seeds a **failed plan** alongside ones awaiting review, and
//! a plan with a **proposal standing in front of the user**. A starting set
//! that is all-quiet makes the states worth noticing unreachable during
//! development, which is exactly when a refactor would quietly lose them. A
//! test pins this.

use crate::ids::*;
use crate::model::*;

/// Fabricates a set of plans for the given cities, so the map has something
/// to show on first run.
pub fn starter_plans(cities: &[City]) -> Vec<Plan> {
    if cities.is_empty() {
        return Vec::new();
    }

    // Each scripted plan: (id, prompt, status).
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
            NetworkMode::Shared,
        );
        plan.title = format!("{} of {}", plan_title(i), city.name);
        plan.summary = match status {
            PlanStatus::Failed => format!("The court could not reach a model for {}.", city.name),
            _ => format!("Proposes structural changes to {}.", city.name),
        };
        plan.status = status;
        match status {
            PlanStatus::Failed => plan.note(NoteKind::Failed, plan.summary.clone()),
            _ => plan.say(Speaker::Assistant, plan.summary.clone()),
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
        NetworkMode::Shared,
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
        NetworkMode::Shared,
    );
    archived.title = format!("The Folly of {}", first.name);
    archived.summary = "Proposed rewriting the scanner. Set aside.".into();
    archived.settle(Outcome::Archived {
        branch: "kingdom/fabricated".into(),
        tip: "0000000fabricated".into(),
        base: "main".into(),
        base_commit: "0000000fabricatedbase".into(),
        patch: None,
        pruned: false,
    });
    plans.push(archived);

    // A plan waiting on the user's word, which is the state the whole product
    // turns on: the model has drawn something up and stopped, and nothing moves
    // until they decide. Without one here the proposal card is unreachable on a
    // fresh kingdom, and an unreachable state is one a refactor breaks quietly.
    let mut proposing = Plan::opened(
        PlanId::new("plan-proposing"),
        first.id.clone(),
        "Speed up the start-up path",
        &mock,
        Workspace::in_place(&first.path),
        NetworkMode::Shared,
    );
    proposing.title = format!("The Swift Gates of {}", first.name);
    proposing.summary = "A plan for the start-up path, awaiting your word.".into();
    proposing.status = PlanStatus::AwaitingReview;
    proposing.propose(
        "Cache the scan between runs",
        format!(
            "## What I would do\n\n\
             {} rescans every file on each start, which is most of the wait. I would \
             keep the previous scan and re-walk only what changed.\n\n\
             ```mermaid\n\
             flowchart LR\n  \
               Open[\"open kingdom\"] --> Cached{{\"record on disk?\"}}\n  \
               Cached -- no --> Full[\"full walk\"]\n  \
               Cached -- yes --> Diff[\"compare mtimes\"]\n  \
               Diff --> Partial[\"walk what moved\"]\n\
             ```\n\n\
             ## The changes\n\n\
             1. Record the scan under `.kingdom/`, keyed by mtime.\n\
             2. Compare on open, and walk only the folders that moved.\n\
             3. Fall back to a full scan when the record is missing or unreadable.\n\n\
             | Case | Today | After |\n\
             |---|---|---|\n\
             | unchanged tree | full walk | one `stat` per folder |\n\
             | one file moved | full walk | that folder only |\n\n\
             ## What I am assuming\n\n\
             That mtime is trustworthy here. I have *not* checked what happens on a \
             network mount.\n\n\
             (Placeholder court \u{2014} no real work was done.)",
            first.name
        ),
    );
    plans.push(proposing);

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

    /// The model must open showing something worth the user's attention.
    ///
    /// A model of nothing but tidy settled plans makes the states the UI exists
    /// to render unreachable during development -- and a refactor is exactly
    /// when that would happen quietly. It used to be a blocked plan and a
    /// contended resource; with lease arbitration gone, a *failed* plan is the
    /// honest equivalent, because it is a state the running product can
    /// genuinely produce.
    ///
    /// A standing proposal joined them for the same reason and matters most of
    /// the four: it is the state the product's whole stance rests on, and the
    /// only one whose UI is a decision rather than a display.
    #[test]
    fn the_starter_plans_always_show_trouble_and_history() {
        let cities = vec![city("c1", "Alpha"), city("c2", "Beta")];
        let plans = starter_plans(&cities);

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
        assert!(
            plans.iter().any(|p| p.standing_proposal().is_some()),
            "a plan awaiting the user's word must be visible, or the proposal card \
             — the state this whole product turns on — is unreachable on a fresh kingdom"
        );
    }
}
