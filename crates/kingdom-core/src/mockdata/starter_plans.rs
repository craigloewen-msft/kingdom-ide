//! Opening models a fixture can choose between.
//!
//! A model is the plans a kingdom starts with, before the user has issued a
//! prompt. [`crate::sample::starter_plans`] already builds one over whatever
//! cities were scanned; the only limitation was that there was exactly one of
//! it. So these are simply more functions of the same signature, and a fixture
//! names the one it wants.
//!
//! There used to be a `three_way_contention` model here, staging a fight over
//! port 3000 to make the map draw red threads. It went with lease arbitration:
//! nothing in the running product could ever produce that state, so the fixture
//! was rehearsing a scenario the user would never actually see. It belongs back
//! here when plans can genuinely collide.

use crate::ids::PlanId;
use crate::model::{City, Isolation, ModelChoice, Plan, PlanStatus, Speaker, Workspace};

/// The standard opening model: whatever [`crate::sample::starter_plans`] makes.
///
/// Most fixtures want this. It is the same model the real-folder path gets, so
/// a proving ground rehearses what the user actually sees.
pub fn default_plans(cities: &[City]) -> Vec<Plan> {
    crate::sample::starter_plans(cities)
}

/// A single settled plan, and nothing else.
///
/// For looking at a fixture whose map is almost empty -- useful when the thing
/// being examined is the terrain or the skyline rather than the model.
pub fn quiet_plans(cities: &[City]) -> Vec<Plan> {
    let Some(city) = cities.first() else {
        return Vec::new();
    };

    let mut plan = Plan::opened(
        PlanId::new("plan-quiet"),
        city.id.clone(),
        "Tidy the documentation",
        &ModelChoice::new("mock", None),
        Workspace::in_place(&city.path),
        Isolation::Shared,
    );
    plan.title = format!("The Quiet Works of {}", city.name);
    plan.summary = "Nothing in flight. All is well.".into();
    plan.status = PlanStatus::AwaitingReview;
    plan.say(Speaker::Assistant, plan.summary.clone());

    vec![plan]
}
