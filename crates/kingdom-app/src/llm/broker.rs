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

use kingdom_core::{
    Kingdom, Lease, LeaseMode, PlanId, Resource, ResourceId, ResourceKind, Workspace,
};

/// Why a lease could not be granted, in words the King can act on.
#[derive(Debug)]
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

/// The crown resource standing for one city's git metadata.
///
/// Deliberately distinct from its files, because the two are contended
/// differently: any number of plans may *read* a repository at once, but
/// `git worktree add` mutates shared state under `.git`, so two decrees issued a
/// second apart would genuinely race it. This is the resource that serialises
/// that, and nothing else.
pub fn city_repo_resource(city: &kingdom_core::CityId) -> ResourceId {
    ResourceId::new(format!("res-git-{city}"))
}

/// The crown resource standing for the files one plan actually works on.
///
/// This is the whole payoff of worktrees. An isolated workspace has a checkout
/// of its own, so it is its own resource: two plans in two fresh worktrees of
/// one repository genuinely do not contend, and the map must stop drawing a
/// conflict that no longer exists. A plan working in place still shares the
/// city's own path resource, because it really is the same files.
pub fn workspace_resource(city: &kingdom_core::CityId, workspace: &Workspace) -> ResourceId {
    match &workspace.id {
        Some(id) => ResourceId::new(format!("res-path-{city}-{id}")),
        None => city_path_resource(city),
    }
}

/// Ensures a resource exists, so Crown Resources fills in with real entries as
/// the kingdom is worked in rather than showing only sample data.
fn ensure(kingdom: &mut Kingdom, id: &ResourceId, name: String, kind: ResourceKind) {
    if !kingdom.resources.iter().any(|r| &r.id == id) {
        kingdom.resources.push(Resource {
            id: id.clone(),
            name,
            kind,
            holders: Vec::new(),
            waiting: Vec::new(),
        });
    }
}

/// Claims one resource for a plan, recording a refusal as a visible wait.
///
/// The compatibility matrix itself lives in kingdom-core and is pinned by its
/// own test; this is only the caller. On refusal the plan is left in the
/// resource's `waiting` list, which is what draws the red thread on the map --
/// resolving the contention quietly here would defeat the entire product.
fn claim(
    kingdom: &mut Kingdom,
    plan: &PlanId,
    resource_id: ResourceId,
    mode: LeaseMode,
    reason: String,
    subject: &str,
) -> Result<Lease, Refusal> {
    let resource = kingdom
        .resources
        .iter_mut()
        .find(|r| r.id == resource_id)
        .expect("callers ensure the resource exists first");

    if !resource.can_grant(mode) {
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
                "Blocked: {blocker} holds {subject} exclusively. \
                 Waiting for it to be released."
            ),
        });
    }

    let lease = Lease {
        resource: resource_id,
        holder: plan.clone(),
        mode,
        reason,
    };
    resource.holders.push(lease.clone());
    resource.waiting.retain(|p| p != plan);

    Ok(lease)
}

/// Claims a shared read of the files a plan works on.
pub fn acquire_workspace_read(
    kingdom: &mut Kingdom,
    plan: &PlanId,
    city: &kingdom_core::CityId,
    workspace: &Workspace,
) -> Result<Lease, Refusal> {
    let city_name = kingdom
        .city(city)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| city.to_string());

    // Named for the checkout rather than the project when the two differ, so the
    // King reading Crown Resources can tell an isolated plan from one working in
    // the folder he is sitting in.
    let subject = match &workspace.branch {
        Some(branch) if workspace.is_isolated() => format!("{city_name} on {branch}"),
        _ => format!("{city_name} files"),
    };

    let resource_id = workspace_resource(city, workspace);
    ensure(kingdom, &resource_id, subject.clone(), ResourceKind::Path);
    claim(
        kingdom,
        plan,
        resource_id,
        LeaseMode::Shared,
        format!("Reading {city_name} to draft a plan"),
        &subject,
    )
}

/// Claims exclusive use of a city's git metadata, for as long as it takes to cut
/// a worktree.
///
/// Held across one git command and given back the moment it returns. Exclusive
/// because that command writes shared state under `.git`.
pub fn acquire_repo_lock(
    kingdom: &mut Kingdom,
    plan: &PlanId,
    city: &kingdom_core::CityId,
) -> Result<Lease, Refusal> {
    let city_name = kingdom
        .city(city)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| city.to_string());

    let resource_id = city_repo_resource(city);
    let subject = format!("{city_name} git");

    ensure(
        kingdom,
        &resource_id,
        subject.clone(),
        ResourceKind::Worktree,
    );
    claim(
        kingdom,
        plan,
        resource_id,
        LeaseMode::Exclusive,
        format!("Preparing a workspace in {city_name}"),
        &subject,
    )
}

/// Gives back one specific lease, leaving anything else the plan holds alone.
///
/// The repo lock is deliberately short-lived -- taken to cut a worktree and
/// released immediately, while the plan goes on to hold its read lease for the
/// whole draft -- so it cannot use [`release_all`].
pub fn release(kingdom: &mut Kingdom, lease: &Lease) {
    if let Some(resource) = kingdom
        .resources
        .iter_mut()
        .find(|r| r.id == lease.resource)
    {
        resource.holders.retain(|l| l.holder != lease.holder);
        resource.waiting.retain(|p| p != &lease.holder);
    }
    if let Some(p) = kingdom.plans.iter_mut().find(|p| p.id == lease.holder) {
        p.leases.retain(|l| l.resource != lease.resource);
    }
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
    use kingdom_core::{CityId, Workspace, WorkspaceMode};

    /// A workspace with a checkout of its own, as `worktree::prepare` returns
    /// for `Fresh` and `Branch`.
    fn isolated(id: &str) -> Workspace {
        Workspace {
            mode: WorkspaceMode::Fresh,
            path: format!("/dev/testburg/.kingdom/{id}"),
            branch: Some(format!("kingdom/{id}")),
            id: Some(id.to_string()),
        }
    }

    fn in_place() -> Workspace {
        Workspace::in_place("/dev/testburg")
    }

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
        let outcome = acquire_workspace_read(&mut k, &latecomer, &city, &in_place());

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

        assert!(acquire_workspace_read(&mut k, &first, &city, &in_place()).is_ok());
        assert!(acquire_workspace_read(&mut k, &second, &city, &in_place()).is_ok());

        release_all(&mut k, &first);
        release_all(&mut k, &second);

        let r = k
            .resources
            .iter()
            .find(|r| r.id == city_path_resource(&city))
            .unwrap();
        assert!(!r.is_held(), "released leases must not linger");
    }

    /// The two halves of what isolation is *for*, and neither is obvious from
    /// the types alone.
    ///
    /// Cutting a worktree writes shared state under `.git`, so it must be
    /// serialised and the loser must be visibly waiting rather than quietly
    /// racing. But once two plans each have a checkout of their own they are no
    /// longer in each other's way at all -- if they still contended, the feature
    /// would have bought nothing and the map would keep drawing a conflict that
    /// does not exist.
    #[test]
    fn cutting_a_worktree_serialises_but_the_resulting_checkouts_do_not_contend() {
        let mut k = kingdom_with_city();
        let city = CityId::new("c1");
        let first = PlanId::new("plan-a");
        let second = PlanId::new("plan-b");

        let held = acquire_repo_lock(&mut k, &first, &city).expect("first cut may proceed");
        assert!(
            acquire_repo_lock(&mut k, &second, &city).is_err(),
            "two plans must not cut a worktree in the same repository at once"
        );

        let repo = k
            .resources
            .iter()
            .find(|r| r.id == city_repo_resource(&city))
            .unwrap();
        assert!(
            repo.waiting.contains(&second),
            "a refused cut must be visibly queued, not silently retried"
        );

        // The lock is held only across the git command.
        release(&mut k, &held);
        assert!(acquire_repo_lock(&mut k, &second, &city).is_ok());

        // Having cut them, the two checkouts are independent.
        assert!(acquire_workspace_read(&mut k, &first, &city, &isolated("aaa")).is_ok());
        assert!(
            acquire_workspace_read(&mut k, &second, &city, &isolated("bbb")).is_ok(),
            "separate worktrees of one city must not contend"
        );
        assert_ne!(
            workspace_resource(&city, &isolated("aaa")),
            workspace_resource(&city, &isolated("bbb"))
        );
        assert_eq!(
            workspace_resource(&city, &in_place()),
            city_path_resource(&city),
            "working in place really is the city's own files"
        );
    }
}
