//! Opening courts a realm can choose between.
//!
//! A court is the plans a kingdom starts with, before the King has issued a
//! decree. [`crate::sample::populate_court`] already builds one over whatever
//! cities were scanned; the only limitation was that there was exactly one of
//! it. So these are simply more functions of the same signature, and a realm
//! names the one it wants.
//!
//! There used to be a `three_way_contention` court here, staging a fight over
//! port 3000 to make the map draw red threads. It went with lease arbitration:
//! nothing in the running product could ever produce that state, so the realm
//! was rehearsing a scenario the King would never actually see. It belongs back
//! here when plans can genuinely collide.

use crate::ids::PlanId;
use crate::model::{City, ModelChoice, Plan, PlanStatus, Speaker, Workspace};

/// The standard opening court: whatever [`crate::sample::populate_court`] makes.
///
/// Most realms want this. It is the same court the real-folder path gets, so a
/// proving ground rehearses what the King actually sees.
pub fn default_court(cities: &[City]) -> Vec<Plan> {
    crate::sample::populate_court(cities)
}

/// A single settled plan, and nothing else.
///
/// For looking at a realm whose map is almost empty -- useful when the thing
/// being examined is the terrain or the skyline rather than the court.
pub fn quiet_court(cities: &[City]) -> Vec<Plan> {
    let Some(city) = cities.first() else {
        return Vec::new();
    };

    let mut plan = Plan::opened(
        PlanId::new("plan-quiet"),
        city.id.clone(),
        "Tidy the documentation",
        &ModelChoice::new("mock", None),
        Workspace::in_place(&city.path),
    );
    plan.title = format!("The Quiet Works of {}", city.name);
    plan.summary = "Nothing in flight. All is well.".into();
    plan.status = PlanStatus::AwaitingReview;
    plan.say(Speaker::Court, plan.summary.clone());

    vec![plan]
}
