//! The lease broker: nothing drafts without a claim on what it touches.
//!
//! `AGENTS.md` is emphatic about this. Every capability that touches something
//! shared must take a lease first, and contention must be *visible* rather than
//! quietly resolved. Drafting reads a project's files, so it takes a `Shared`
//! lease on that project's path before it calls a model, and gives it back
//! afterwards -- including when the model fails.
//!
//! There is no queue yet. A refused request surfaces as a blocked plan sitting
//! in the resource's `waiting` list, which is what draws the red thread on the
//! map. Resolving the contention automatically would defeat the purpose.

use kingdom_core::{Kingdom, Lease, LeaseMode, PlanId, Resource, ResourceId, ResourceKind};

/// Why a lease could not be granted, in words the King can act on.
pub struct Refusal {
    pub reason: String,
}

/// The crown resource standing for one city's files on disk.
///
/// Derived from the city id rather than stored, so the same city always maps to
/// the same resource without a registry to keep in sync.
pub fn city_path_resource(city: &kingdom_core::CityId) -> ResourceId {
    ResourceId::new(format!("res-path-{city}"))
}

/// Claims a shared read of a city's files for a plan.
///
/// Creates the resource on first use, so Crown Resources fills in with real
/// entries as the kingdom is worked in rather than showing only sample data.
/// On refusal the plan is recorded as waiting, which is what makes the
/// contention visible.
pub fn acquire_city_read(
    kingdom: &mut Kingdom,
    plan: &PlanId,
    city: &kingdom_core::CityId,
) -> Result<Lease, Refusal> {
    let resource_id = city_path_resource(city);
    let city_name = kingdom
        .city(city)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| city.to_string());

    if !kingdom.resources.iter().any(|r| r.id == resource_id) {
        kingdom.resources.push(Resource {
            id: resource_id.clone(),
            name: format!("{city_name} files"),
            kind: ResourceKind::Path,
            holders: Vec::new(),
            waiting: Vec::new(),
        });
    }

    let resource = kingdom
        .resources
        .iter_mut()
        .find(|r| r.id == resource_id)
        .expect("just inserted");

    // The compatibility matrix itself lives in kingdom-core and is pinned by
    // its own test; this is only the caller.
    if !resource.can_grant(LeaseMode::Shared) {
        if !resource.waiting.contains(plan) {
            resource.waiting.push(plan.clone());
        }
        let blocker = resource
            .holders
            .first()
            .map(|l| l.holder.to_string())
            .unwrap_or_else(|| "another plan".to_string());
        return Err(Refusal {
            reason: format!(
                "Blocked: {blocker} holds {city_name} files exclusively. \
                 Waiting for it to be released."
            ),
        });
    }

    let lease = Lease {
        resource: resource_id,
        holder: plan.clone(),
        mode: LeaseMode::Shared,
        reason: format!("Reading {city_name} to draft a plan"),
    };
    resource.holders.push(lease.clone());
    resource.waiting.retain(|p| p != plan);

    Ok(lease)
}

/// Gives back every lease a plan holds.
///
/// Must run on the failure path too: a plan that died holding the city's files
/// would block every later decree for that city with no way to clear it.
pub fn release_all(kingdom: &mut Kingdom, plan: &PlanId) {
    for resource in &mut kingdom.resources {
        resource.holders.retain(|l| &l.holder != plan);
        resource.waiting.retain(|p| p != plan);
    }
    if let Some(p) = kingdom.plans.iter_mut().find(|p| &p.id == plan) {
        p.leases.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::CityId;

    fn kingdom_with_city() -> Kingdom {
        let mut k = Kingdom::unopened();
        k.root = "/dev".into();
        k.cities.push(kingdom_core::City {
            id: CityId::new("c1"),
            name: "Testburg".into(),
            path: "testburg".into(),
            kind: kingdom_core::CityKind::Rust,
            file_count: 3,
            has_git: true,
            dirty_files: 0,
            structure: None,
        });
        k
    }

    /// The rule the whole product rests on: work does not proceed past a lease
    /// it could not get, and the refusal is left visible rather than swallowed.
    #[test]
    fn an_exclusively_held_city_refuses_and_records_the_waiting_plan() {
        let mut k = kingdom_with_city();
        let city = CityId::new("c1");
        let resource = city_path_resource(&city);

        // Somebody else already holds the city exclusively.
        k.resources.push(Resource {
            id: resource.clone(),
            name: "Testburg files".into(),
            kind: ResourceKind::Path,
            holders: vec![Lease {
                resource: resource.clone(),
                holder: PlanId::new("plan-incumbent"),
                mode: LeaseMode::Exclusive,
                reason: "Rewriting everything".into(),
            }],
            waiting: Vec::new(),
        });

        let latecomer = PlanId::new("plan-latecomer");
        let outcome = acquire_city_read(&mut k, &latecomer, &city);

        assert!(outcome.is_err(), "must not draft while held exclusively");

        let r = k.resources.iter().find(|r| r.id == resource).unwrap();
        assert!(
            r.waiting.contains(&latecomer),
            "a refused plan must be visibly queued, or the map cannot draw the contention"
        );
        assert!(r.is_contended());
    }

    /// Two plans reading the same city is the common case and must be allowed;
    /// releasing must fully clear the holder so the next decree is not blocked.
    #[test]
    fn shared_reads_compose_and_release_clears_the_field() {
        let mut k = kingdom_with_city();
        let city = CityId::new("c1");
        let first = PlanId::new("plan-a");
        let second = PlanId::new("plan-b");

        assert!(acquire_city_read(&mut k, &first, &city).is_ok());
        assert!(acquire_city_read(&mut k, &second, &city).is_ok());

        release_all(&mut k, &first);
        release_all(&mut k, &second);

        let r = k
            .resources
            .iter()
            .find(|r| r.id == city_path_resource(&city))
            .unwrap();
        assert!(!r.is_held(), "released leases must not linger");
    }
}
