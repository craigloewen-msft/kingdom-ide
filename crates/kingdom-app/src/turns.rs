//! The turns running right now, and the King's way of stopping one.
//!
//! # Why a registry, when the plan already says it is busy
//!
//! [`kingdom_core::Plan::working_on`] answers "is someone working on this?" for
//! *everyone*: it survives a restart, it is on disk, and the browser reads it.
//! That durability is exactly what makes it the wrong answer to a different
//! question -- **is a turn running in this process, right now?** -- because it
//! is still set after a panic, after a dropped future, and after the server was
//! killed mid-round. `api::say` clears it for precisely that reason.
//!
//! This module answers the second question, and only that. An entry exists
//! while a `converse` loop is between its first line and its last, and cannot
//! outlive it: the guard removes it on drop, which covers every return path and
//! an unwind as well.
//!
//! The distinction is load-bearing in two places:
//!
//! 1. `say` queues a message only when a turn is genuinely running. If it
//!    branched on `is_busy()` instead, a plan whose busy mark outlived its turn
//!    would swallow every message into a queue that nothing would ever drain --
//!    turning today's recoverable wedge into a permanent one.
//! 2. `stop_plan` repairs such a plan rather than failing on it, because
//!    finding no entry here *is* the diagnosis: the mark is stale.
//!
//! # Why `watch` and not a cancellation token
//!
//! `tokio::sync::watch` carries a signal that is sent once, seen by any number
//! of waiters, and still seen by one that arrives late -- which is the whole
//! requirement. `tools::bash` reaches for it for the same shape of problem (a
//! process that ends once, with callers that may ask before or after). Adding
//! `tokio-util` for `CancellationToken` would buy a nicer name for machinery
//! that is already in the dependency list.
//!
//! # What stopping actually does
//!
//! Nothing is aborted. The signal is *cooperative*: `converse` races it against
//! its two long awaits -- the model call and the tool call -- and returns
//! through its own exit path when it wins. That is deliberate. An aborted task
//! would skip the code that clears the busy mark and settles the in-flight
//! deed, which is the difference between a stopped plan and a wedged one.

use kingdom_core::PlanId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

/// The turns running in this process, keyed by the plan each is drawing up.
///
/// The value carries a *token* as well as the signal. Two turns can briefly
/// overlap on one plan -- `draft_plan`'s busy check races a turn that has
/// settled but whose guard is still alive -- and without the token the older
/// guard's `Drop` would deregister the newer turn. That would make `say` take
/// the direct path over a live turn and `stop_plan` misdiagnose a running plan
/// as wedged, which are exactly the two things this registry exists to get
/// right.
///
/// Deliberately not persisted, for the same reason as
/// `tools::ask_user_question::PENDING`: the thing being stored is a live
/// channel into a running future, and writing a record of it to disk would
/// leave the next process holding a handle to a turn that no longer exists.
/// `store::reconcile` repairs what a restart leaves behind.
static RUNNING: OnceLock<Mutex<HashMap<PlanId, (u64, watch::Sender<bool>)>>> = OnceLock::new();

/// Names each registration, so a guard can only ever remove its own.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn running() -> &'static Mutex<HashMap<PlanId, (u64, watch::Sender<bool>)>> {
    RUNNING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a turn as running and hands back the signal it must watch.
///
/// The returned guard is the registration: hold it for the length of the turn
/// and drop it when the turn ends, however it ends.
///
/// A second turn over the same plan replaces the first's entry rather than
/// being refused, because refusing here would be a guard in the wrong place --
/// `draft_plan` already declines to start a turn over a busy plan, and by the
/// time anything reaches this function the decision has been made. Replacing
/// means the newer turn is the one the King's Stop reaches, which is the one he
/// can see.
pub fn begin(plan: &PlanId) -> TurnGuard {
    let (tx, rx) = watch::channel(false);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut running) = running().lock() {
        running.insert(plan.clone(), (token, tx));
    }
    TurnGuard {
        plan: plan.clone(),
        token,
        halted: rx,
    }
}

/// True while a turn for this plan is genuinely running in this process.
///
/// Not the same question as [`kingdom_core::Plan::is_busy`] -- see the module
/// docs, where the difference is the whole point.
pub fn is_running(plan: &PlanId) -> bool {
    running()
        .lock()
        .map(|running| running.contains_key(plan))
        .unwrap_or(false)
}

/// Asks a running turn to stop, and reports whether there was one to ask.
///
/// False is not an error: it means the plan's busy mark has outlived its turn,
/// and the caller should repair the plan rather than wait for a turn that will
/// never answer.
///
/// The entry is left in place. Removing it here would race the turn's own
/// cleanup, and the guard is about to do it anyway -- the signal is enough.
pub fn halt(plan: &PlanId) -> bool {
    let Ok(running) = running().lock() else {
        return false;
    };
    match running.get(plan) {
        // `send` fails only when every receiver is gone, which means the turn
        // has already finished; that is the same answer as not finding it.
        Some((_, tx)) => tx.send(true).is_ok(),
        None => false,
    }
}

/// A turn's registration, and its half of the halt signal.
///
/// Removes itself from the registry on drop, which is what makes the registry
/// trustworthy: there is no return path, and no panic, that can leave an entry
/// behind for a turn that has stopped.
pub struct TurnGuard {
    plan: PlanId,
    token: u64,
    halted: watch::Receiver<bool>,
}

impl TurnGuard {
    /// Deregisters this turn *now*, rather than waiting for the guard to drop.
    ///
    /// Exists so a turn can end its registration inside the same kingdom lock
    /// it makes its final decision under. `converse` needs that: it decides
    /// whether to return or go round again by reading the plan's queue, and
    /// `say` decides whether to queue by reading this registry -- both under
    /// the kingdom lock. Deregistering anywhere else leaves a window in which
    /// `say` sees a turn still running and queues for it, while the turn has
    /// already read an empty queue and is on its way out, stranding the words.
    ///
    /// Idempotent, and safe to call before the guard drops: `Drop` removes only
    /// an entry still bearing this guard's token, so neither call can take a
    /// newer turn's registration with it.
    pub fn stand_down(&self) {
        if let Ok(mut running) = running().lock() {
            if running
                .get(&self.plan)
                .is_some_and(|(t, _)| *t == self.token)
            {
                running.remove(&self.plan);
            }
        }
    }
    /// Resolves when the King calls a halt, and never otherwise.
    ///
    /// Written to be safe in a `select!` arm that is polled many times: it
    /// re-reads the current value on entry, so a halt that landed between two
    /// awaits is seen immediately rather than waiting for a change that has
    /// already happened.
    pub async fn halted(&mut self) {
        if *self.halted.borrow_and_update() {
            return;
        }
        // `changed` errors only when the sender is gone. That cannot happen
        // while this guard is alive -- it owns the entry holding the sender --
        // but if it ever did, never resolving is the right answer: it would
        // mean nobody can call a halt, not that one was called.
        while self.halted.changed().await.is_ok() {
            if *self.halted.borrow_and_update() {
                return;
            }
        }
        std::future::pending::<()>().await
    }

    /// Whether a halt has been called, without waiting for one.
    ///
    /// The cheap check the turn loop makes at the top of each round, so a halt
    /// landing between the model call and the tool call is not held until the
    /// next long await.
    pub fn was_halted(&self) -> bool {
        *self.halted.borrow()
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.stand_down();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `stand_down` must be idempotent and must never take a *newer* turn's
    /// registration with it.
    ///
    /// `converse` deregisters explicitly, under the kingdom lock, and then its
    /// guard drops moments later. If the second removal were unconditional, a
    /// turn that started in that window would be deregistered by a guard that
    /// no longer owns the plan -- and `say` would take the direct path over a
    /// live turn, which is the exact splice-mid-deed this whole design exists
    /// to prevent.
    #[test]
    fn standing_down_twice_cannot_deregister_the_turn_that_replaced_it() {
        let plan = PlanId::new("plan-handover");

        let first = begin(&plan);
        first.stand_down();
        assert!(!is_running(&plan));

        // A new turn takes the plan while the old guard is still alive.
        let _second = begin(&plan);
        assert!(is_running(&plan));

        // The old guard finally drops. It must leave the newer turn alone.
        drop(first);
        assert!(
            is_running(&plan),
            "a stale guard must not deregister the turn that succeeded it"
        );
    }

    /// The registry must not answer for a plan whose turn has ended, because
    /// `say` reads it to decide whether to queue. A stale `true` here is a
    /// message queued behind a turn that will never drain it.
    #[test]
    fn a_turn_is_registered_only_while_its_guard_lives() {
        let plan = PlanId::new("plan-guard");
        assert!(!is_running(&plan));

        {
            let _guard = begin(&plan);
            assert!(is_running(&plan));
        }

        assert!(!is_running(&plan), "the guard must deregister on drop");
    }

    /// The guard's cleanup runs on unwind as well as on return. This is the
    /// case `draft_plan` already handles for the busy mark, and the registry
    /// has to survive it for the same reason: a panicking turn must not leave
    /// the plan looking permanently alive.
    #[test]
    fn a_panicking_turn_deregisters_too() {
        let plan = PlanId::new("plan-panic");

        let panicked = std::panic::catch_unwind(|| {
            let _guard = begin(&plan);
            panic!("the turn came apart");
        });

        assert!(panicked.is_err());
        assert!(!is_running(&plan));
    }

    /// `stop_plan` distinguishes "asked a running turn to stop" from "found a
    /// stale busy mark and repaired it" on this return value alone.
    #[test]
    fn halting_nothing_says_so() {
        assert!(!halt(&PlanId::new("plan-never-started")));

        let plan = PlanId::new("plan-halt");
        let _guard = begin(&plan);
        assert!(halt(&plan));
    }

    /// The halt must be visible to a turn that only comes back to look for it
    /// later -- the signal is sent while the loop is deep inside a tool call,
    /// and read when that call returns.
    #[tokio::test]
    async fn a_halt_is_seen_by_a_turn_that_asks_afterwards() {
        let plan = PlanId::new("plan-late");
        let mut guard = begin(&plan);

        assert!(!guard.was_halted());
        assert!(halt(&plan));

        assert!(guard.was_halted());
        // And resolves immediately rather than waiting for a further change.
        guard.halted().await;
    }

    /// The other half: a turn already parked on the signal when the halt lands.
    #[tokio::test]
    async fn a_halt_wakes_a_turn_already_waiting() {
        let plan = PlanId::new("plan-waiting");
        let mut guard = begin(&plan);

        let waiting = tokio::spawn({
            let plan = plan.clone();
            async move {
                // Let the waiter park first.
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                halt(&plan)
            }
        });

        guard.halted().await;
        assert!(waiting.await.unwrap());
    }
}
