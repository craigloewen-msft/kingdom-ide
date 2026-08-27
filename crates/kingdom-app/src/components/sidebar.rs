//! The left rail: cities, and the plans drawn up inside each of them.
//!
//! Deliberately one list rather than a set of tabbed panels. The user's scarce
//! resource is attention, so the rail answers exactly one question — what is
//! being proposed, and where — and leaves agent status and resource contention
//! to the map, which shows them spatially.
//!
//! It is also the app's primary navigator: every row goes somewhere. A city
//! goes to the fixture with that city selected; a plan goes to its
//! conversation. A row that silently changed state on a screen that cannot show
//! the result would be a dead end.

use crate::app::{KingdomState, DEFAULT_MAP_HEIGHT, DEFAULT_SIDEBAR_WIDTH};
use crate::components::conversation::clock;
use crate::components::resizer::{restore_width, Bounds, Grows, Resizer};
use kingdom_core::{Attention, City, CityId, Plan, PlanStatus, Timestamp};
use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::{use_location, use_navigate};
use std::collections::HashSet;

/// How far the rail may be dragged. Narrower than the minimum and city names
/// are unreadable; wider than the maximum and the map stops being the hero.
const BOUNDS: Bounds = Bounds {
    min: 200.0,
    max: 560.0,
    default: DEFAULT_SIDEBAR_WIDTH,
};

const WIDTH_KEY: &str = "kingdom.sidebar_width";

/// How tall the map pane at the foot of the rail may be dragged.
///
/// The floor is a few rows of world rather than zero, for the reason the files
/// rail's split has one: a pane dragged shut cannot be found again. The ceiling
/// leaves room for the registry, which is what the rail is actually for -- a
/// map that could eat the whole column would hide the plans it is meant to sit
/// beside.
const MAP_BOUNDS: Bounds = Bounds {
    min: 140.0,
    max: 620.0,
    default: DEFAULT_MAP_HEIGHT,
};

const MAP_HEIGHT_KEY: &str = "kingdom.map_height";

/// How often the rail re-reads the clock, in milliseconds.
///
/// Half a minute, because the finest thing the age line ever says is a whole
/// minute -- so half of that bounds the lag at a number nobody can catch being
/// wrong, at a fraction of the wakeups a per-second tick would cost. The
/// chamber's `ticking_clock` runs at one second because it is drawing tenths of
/// a second on a running deed; this is drawing minutes on thirty rows.
///
/// Browser-only, like every other cadence and storage key that only the client
/// half reads: there is no timer under SSR to give it to.
#[cfg(feature = "hydrate")]
const RAIL_TICK_MS: u64 = 30_000;

#[component]
pub fn Sidebar() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Collapse is opt-in and lives only here: it is a view preference, not
    // kingdom state, and nothing outside the rail cares about it.
    let collapsed = RwSignal::new(HashSet::<CityId>::new());

    let cities = move || state.kingdom.with(|k| k.cities.clone());
    let city_count = move || state.kingdom.with(|k| k.cities.len());

    restore_width(state.sidebar_width, WIDTH_KEY, BOUNDS);
    restore_width(state.map_height, MAP_HEIGHT_KEY, MAP_BOUNDS);

    let collapsed_rail = move || state.rail_collapsed.get();

    // One clock for the whole rail, read by every plan row's age line. One per
    // row would be thirty timers redrawing thirty strings that change once a
    // minute; see `rail_clock`.
    let now = rail_clock();

    // Whether the map is standing at the foot of this rail, which it does only
    // in a chamber -- on `/` the map has the whole screen and the rail is beside
    // it rather than around it. The same question `ThroneRoom` asks to place the
    // overlay, asked here to reserve the room for it.
    let location = use_location();
    let map_in_rail = Memo::new(move |_| location.pathname.get() != "/");

    // Clearing the kingdom locally is what returns the app to the opening
    // screen; the server call is what stops the next start reopening it.
    let leave = Action::new(move |(): &()| async move {
        if let Err(e) = crate::api::leave_kingdom().await {
            leptos::logging::warn!("could not leave the kingdom: {e}");
        }
        state.kingdom.set(kingdom_core::Kingdom::unopened());
    });

    view! {
        <aside class="sidebar" class:collapsed=collapsed_rail>
            // Folded away, the rail keeps exactly two things: the crown, so the
            // strip still reads as the kingdom's, and the control that brings it
            // back. It is never zero-width -- every route in the app is reached
            // from this rail, so a rail with no way back would be a dead end.
            <Show when=collapsed_rail>
                <div class="rail-strip">
                    <button
                        class="rail-toggle"
                        title="Show cities and plans"
                        on:click=move |_| state.toggle_rail()
                    >"\u{bb}"</button>
                    <div class="crown-small strip-crown">"\u{265a}"</div>
                    <div class="strip-legend">"CITIES"</div>
                </div>
            </Show>

            <Show when=move || !collapsed_rail()>
            <header class="kingdom-header">
                <div class="crown-small">"♚"</div>
                <div class="kingdom-id">
                    <div class="kingdom-name">{move || state.kingdom.with(|k| k.name.clone())}</div>
                    // A proving ground is *designed* to be indistinguishable
                    // from a real kingdom on the map, which makes an unlabelled
                    // one a trap -- for the user glancing at it, and for anyone
                    // reading a screenshot of it later. So the label sits with
                    // the kingdom's identity, not somewhere it scrolls away.
                    <Show when=move || state.kingdom.with(|k| k.sandbox)>
                        <div
                            class="sandbox-tag"
                            title="Synthetic data. Nothing in this kingdom is real work."
                        >
                            "PROVING GROUNDS"
                        </div>
                    </Show>
                    <div class="kingdom-path" title=move || state.kingdom.with(|k| k.root.clone())>
                        {move || state.kingdom.with(|k| k.root.clone())}
                    </div>
                </div>
                // The way out. A kingdom now reopens itself on every start, so
                // without this the opening screen is unreachable once one has
                // been chosen -- it shows only when nothing is open.
                <button
                    class="leave-kingdom"
                    title="Close this kingdom and choose another"
                    on:click=move |_| { leave.dispatch(()); }
                >
                    "Change"
                </button>
            </header>

            <div class="sidebar-toolbar">
                <span class="toolbar-label">
                    {move || format!("Cities {}", city_count())}
                </span>
                <div class="filter-toggle">
                    <button
                        class="filter-btn"
                        class:active=move || !state.show_all_plans.get()
                        title="Only plans still in play"
                        on:click=move |_| state.show_all_plans.set(false)
                    >"Active"</button>
                    <button
                        class="filter-btn"
                        class:active=move || state.show_all_plans.get()
                        title="Include merged and archived plans"
                        on:click=move |_| state.show_all_plans.set(true)
                    >"All"</button>
                </div>
                <button
                    class="rail-toggle"
                    title="Fold the rail away"
                    on:click=move |_| state.toggle_rail()
                >"\u{ab}"</button>
            </div>

            <div class="sidebar-body">
                <ul class="registry">
                    <For each=cities key=|c: &City| c.id.clone() let:city>
                        <CityBranch city=city collapsed=collapsed now=now/>
                    </For>
                </ul>
            </div>

            // The room the map stands in, and nothing else.
            //
            // Deliberately **empty**. The canvas is not a child of this rail --
            // it may be mounted exactly once for the life of the page, so it is
            // an overlay positioned by `ThroneRoom` (see its note) -- and this
            // slot exists only so `.sidebar-body` stops scrolling where the map
            // begins instead of running underneath it.
            //
            // Both read `state.map_height`, so the reserved space and the
            // rectangle drawn over it are the same number rather than two that
            // could drift. The alternative was measuring one into the other,
            // and a measured layout that lags a frame during a drag is a whole
            // class of bug avoided by not having a second number.
            //
            // `Grows::Upwards`, so dragging up makes the map taller: the pane
            // being measured is the one *below* the handle. That is the mirror
            // of the files rail's split, and it is which way round it has to be
            // here -- the registry should absorb a change in window height and
            // the map should keep what it was given.
            <Show when=move || map_in_rail.get()>
                <Resizer
                    width=state.map_height
                    grows=Grows::Upwards
                    bounds=MAP_BOUNDS
                    storage_key=MAP_HEIGHT_KEY
                    class="rail-split map-split"
                />
                <div
                    class="rail-map-slot"
                    style:height=move || format!("{}px", state.map_height.get())
                ></div>
            </Show>

            // The handle exists only while there is a panel to size: a divider
            // on a folded rail is a control that does nothing.
            <Resizer
                width=state.sidebar_width
                grows=Grows::Rightwards
                bounds=BOUNDS
                storage_key=WIDTH_KEY
                class="sidebar-resizer"
            />
            </Show>
        </aside>
    }
}

/// One city and, beneath it, the plans drawn up there.
#[component]
fn CityBranch(
    city: City,
    collapsed: RwSignal<HashSet<CityId>>,
    /// The rail's one clock, for the age line under each plan. Passed down
    /// rather than made here, so a kingdom of twelve cities still has one timer.
    now: Memo<Option<Timestamp>>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let id = city.id.clone();

    // Memos throughout: a plain closure capturing `id` is `FnOnce` and cannot
    // be read from two places in the view.
    let selected = {
        let id = id.clone();
        Memo::new(move |_| state.selected.get().as_ref() == Some(&id))
    };

    let plans = {
        let id = id.clone();
        Memo::new(move |_| {
            let show_all = state.show_all_plans.get();
            // `with` rather than `get`, and it matters more here than anywhere:
            // this runs once *per city*, so `get` cloned the entire kingdom --
            // every plan and every transcript in it -- once for each row in the
            // rail, on every watch-socket push. Six cities meant six full
            // copies to build six short lists.
            state.kingdom.with(|k| {
                k.plans
                    .iter()
                    // Subagents are excluded here as they are on the map: the rail
                    // is the list of what the *user* asked for, and filling it with
                    // work the model sent itself makes it worse at that job. A
                    // subagent is reached from the conversation of the plan that
                    // sent it.
                    .filter(|p| p.city == id && !p.is_subagent() && (show_all || p.is_live()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    };

    let is_open = {
        let id = id.clone();
        Memo::new(move |_| !collapsed.get().contains(&id))
    };

    let toggle = {
        let id = id.clone();
        move |ev: ev::MouseEvent| {
            // The chevron sits inside the row, whose click selects the city.
            ev.stop_propagation();
            collapsed.update(|set| {
                if !set.remove(&id) {
                    set.insert(id.clone());
                }
            });
        }
    };

    let select = {
        let id = id.clone();
        let navigate = use_navigate();
        move |_| {
            state.selected.set(Some(id.clone()));
            // A city is a place on the map, so selecting one goes to the map.
            // Otherwise clicking a city from inside a conversation would change
            // a selection the user cannot see the effect of.
            navigate("/", Default::default());
        }
    };

    let has_plans = Memo::new(move |_| !plans.get().is_empty());

    // Whether anything in this city is waiting on the King.
    //
    // Drawn on the city row because a branch can be *collapsed*, and a question
    // that only shows on the plan row would then be hidden behind a chevron --
    // which is precisely the state the King is in when he is not already
    // watching that plan, and so exactly when he needs telling.
    let asking = Memo::new(move |_| {
        plans
            .get()
            .iter()
            .any(|p| state.attention_of(p) == Some(Attention::Question))
    });

    // Prominence follows *live* work, not whatever the filter happens to show.
    // A city whose plans are all approved or rejected has nothing awaiting the
    // user, so it recedes even in "All" -- otherwise switching filters would
    // re-clutter the rail with settled history. The selected city never
    // recedes: the user is looking at it deliberately.
    let dormant = {
        let id = id.clone();
        Memo::new(move |_| {
            !selected.get()
                && !state
                    .kingdom
                    .get()
                    .plans
                    .iter()
                    .any(|p| p.city == id && p.is_live() && !p.is_subagent())
        })
    };

    view! {
        <li class="city-branch" class:dormant=dormant>
            <div class="city-row" class:selected=selected on:click=select>
                <button
                    class="city-chevron"
                    class:empty={move || !has_plans.get()}
                    on:click=toggle
                >
                    {move || if is_open.get() { "▾" } else { "▸" }}
                </button>
                <span class="kind-dot" style:background=city.kind.banner_color()></span>
                <span class="city-name-text" title=city.path.clone()>{city.name.clone()}</span>
                // Before the count rather than after it: this is the one thing
                // in the row that is a call to act, and the count is provenance.
                <Show when={move || asking.get()}>
                    <span class="city-asking" title="A plan here is waiting on your answer">
                        "\u{2637}"
                    </span>
                </Show>
                <Show when={move || has_plans.get()}>
                    <span class="plan-count">{move || plans.get().len()}</span>
                </Show>
            </div>

            <Show when={move || is_open.get() && has_plans.get()}>
                <ul class="plan-list">
                    // Keyed on what the row actually draws, not just the id: a
                    // `For` reuses a row whose key is unchanged, so keying on
                    // the id alone would leave a plan reading "Drafting" in the
                    // rail long after its conversation showed the finished
                    // draft.
                    //
                    // The proposal is in the key for the same reason and is
                    // easier to miss: a plan can go from speaking to proposing
                    // without its status moving at all -- both are
                    // `AwaitingReview` -- so without it the badge would never
                    // change to "Proposal".
                    //
                    // What the plan wants is in the key for the third instance
                    // of exactly that trap, and the sharpest one: a plan that
                    // stops to ask a question does not move its status *or* its
                    // proposal -- it is still `Drafting` throughout -- so
                    // without this the row would go on reading "Drafting" for
                    // the whole time the court sat waiting on an answer, which
                    // is the fault this badge exists to fix.
                    //
                    // The model stays in the key even though the row no longer
                    // draws it: it is in the tooltip, which is drawn text like
                    // any other.
                    <For
                        each={move || plans.get()}
                        key=move |p: &Plan| {
                            (
                                p.id.clone(),
                                p.status,
                                p.title.clone(),
                                p.choice().label(),
                                state.attention_of(p),
                            )
                        }
                        let:plan
                    >
                        {
                            let href = format!("/plan/{}", plan.id);
                            let current = {
                                let href = href.clone();
                                let location = use_location();
                                Memo::new(move |_| location.pathname.get() == href)
                            };
                            // Read off the plan before the view, which moves it.
                            let title = plan.title.clone();
                            // Everything the row cannot fit, on hover. The title
                            // leads because it is the thing being clipped -- a
                            // rail two lines wide still cuts the longest of
                            // them, and this is where the rest of it is.
                            //
                            // The model is here rather than in the row: it is
                            // provenance, it repeats identically down the whole
                            // rail, and the chamber header states it where the
                            // question is actually live. The summary sits
                            // between them and is often empty, so it is skipped
                            // rather than left as a blank line.
                            let hover = {
                                let mut lines = vec![plan.title.clone()];
                                if !plan.summary.trim().is_empty() {
                                    lines.push(plan.summary.clone());
                                }
                                lines.push(plan.choice().label());
                                lines.join("\n\n")
                            };
                            let (badge, tint) = badge_for(plan.status, state.attention_of(&plan));
                            // When this plan last moved, as the row will draw
                            // it. Two halves, because they are known at
                            // different times: the plan's own transcript is read
                            // *now*, once, and kept as the fallback -- so the
                            // row does not hold a whole plan alive to ask again
                            // -- while the cache is read reactively, because the
                            // rail's socket is the only thing that keeps a shut
                            // chamber's plan current.
                            let recorded = plan.last_activity();
                            let plan_id = plan.id.clone();
                            // A `Memo` rather than a closure, as `current` above
                            // is, and for the same reason: it is read twice --
                            // by the line and by its tooltip -- and a closure
                            // capturing the id is `FnOnce`. It also earns its
                            // keep: it does not read the clock, so it wakes only
                            // when the cache moves, and a push about some other
                            // plan settles to the same moment and re-renders
                            // neither string.
                            let moved_at =
                                Memo::new(move |_| state.activity_of(&plan_id).or(recorded));
                            // Deliberately **not** in the `For` key above. Every
                            // other member of that key is a value captured when
                            // the row is built, so a change to one must rebuild
                            // the row; this one moves on every tick of the
                            // clock, and keying on it would rebuild every row in
                            // the rail twice a minute to change one word. A
                            // closure re-renders just this span.
                            let age = move || since(now.get(), moved_at.get());
                            // The coarse number resolves to an exact one on
                            // hover. Its own tooltip rather than a fourth line
                            // on the row's, because the row's is built once from
                            // values that do not move and this one changes with
                            // the plan.
                            let age_hover = move || match moved_at.get() {
                                Some(at) => format!("Last activity at {}", clock(Some(at))),
                                None => String::new(),
                            };
                            // Which colour this agent's work is drawn in.
                            // `preferred` rather than the city-wide assignment:
                            // the rail lists every plan including settled ones,
                            // and a collision is only resolved among those
                            // actually working. For the common case -- no
                            // collision -- the two agree exactly.
                            let banner = kingdom_core::palette::preferred(&plan.id);
                            let live = plan.is_live();
                            view! {
                                <li>
                                    <A href=href attr:class="plan-row" attr:title=hover>
                                        <span class="plan-row-inner" class:current=current>
                                            <span class="plan-row-head">
                                            // This agent's banner: the key to
                                            // every colour it wears on the map
                                            // and in the review drawer. Shown
                                            // only while the plan is live,
                                            // because a settled plan has no
                                            // works standing anywhere.
                                            <Show when=move || live>
                                                <span
                                                    class="agent-pip"
                                                    style:background=banner.growth
                                                    title=format!(
                                                        "This agent's colour on the map: {}",
                                                        banner.name,
                                                    )
                                                ></span>
                                            </Show>
                                            <span class="plan-title">{title}</span>
                                            <span class=format!("plan-badge plan-{tint}")>
                                                {badge}
                                            </span>
                                            </span>
                                            // How long since anything happened
                                            // here. Its own line under the row
                                            // rather than a fourth thing
                                            // competing for the first: it is
                                            // context for the badge above it,
                                            // not a call to act.
                                            //
                                            // Always rendered, empty when
                                            // nothing is known, so the server's
                                            // markup and the hydrated markup
                                            // have the same shape -- the age is
                                            // browser-only for the same reason
                                            // `clock` is.
                                            <span class="plan-row-meta" title=age_hover>
                                                {age}
                                            </span>
                                        </span>
                                    </A>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </Show>
        </li>
    }
}

/// How long ago something happened, written for a glance in a rail.
///
/// Four bands, chosen for what the number is *for* -- "has this agent gone
/// quiet?" -- rather than for precision:
///
/// | Since | Reads as | Why |
/// |---|---|---|
/// | under a minute | `just now` | still moving; a number here would be noise |
/// | under an hour | `6m ago` | the range where quiet becomes suspicious |
/// | under a day | `3h ago` | it stopped, and roughly when |
/// | -- | `5d ago` | history |
///
/// Empty for a moment nothing is known about, and that is deliberate: a plan
/// recorded before the log was timed has no age, and every alternative --
/// `0m ago`, `unknown`, a dash -- is either a claim or a word taking up a line
/// the King reads thirty of. Silence is what "not known" looks like.
///
/// A moment in the *future* reads `just now` rather than a negative. Both stamps
/// come from the same machine in practice, so the only way to get one is a clock
/// stepping under a running rail, and "just now" is the least wrong thing to say
/// about an entry made a second in the future.
fn since(now: Option<Timestamp>, at: Option<Timestamp>) -> String {
    let (Some(Timestamp(now)), Some(Timestamp(at))) = (now, at) else {
        return String::new();
    };

    let seconds = (now - at).max(0) / 1_000;
    if seconds < 60 {
        return "just now".to_string();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// The rail's one clock, ticking every `RAIL_TICK_MS`.
///
/// The sibling of `conversation.rs`'s `ticking_clock`, and the same shape for
/// the same two reasons: one timer for every row rather than one per row,
/// because a rail of thirty plans would otherwise wake thirty times to redraw
/// thirty strings; and browser-only, because [`Timestamp::now`] is deliberately
/// `None` on wasm and the server has no reason to guess at a number it cannot
/// keep current.
///
/// It differs in two respects. **It never stops**: the chamber's clock ticks
/// only while a deed is in flight, because a settled deed's elapsed time cannot
/// change, but an age can and does -- a plan that has done nothing for an hour
/// is exactly what the rail is trying to show -- so stopping when nothing runs
/// would freeze precisely the rows worth reading. Twice a minute is what makes
/// that affordable. And because it never stops it is never *restarted* either,
/// which is why plain `on_cleanup` suffices here: that function owns its
/// interval handle to survive an effect that re-runs on every turn, and this one
/// is started once, when the rail is built.
fn rail_clock() -> Memo<Option<Timestamp>> {
    let (now, set_now) = signal(None::<Timestamp>);

    #[cfg(feature = "hydrate")]
    {
        use crate::components::conversation::browser_now;

        // Read straight away, so an age appears on first paint rather than
        // thirty seconds into the visit.
        set_now.set(browser_now());

        if let Ok(handle) = leptos::leptos_dom::helpers::set_interval_with_handle(
            move || set_now.set(browser_now()),
            std::time::Duration::from_millis(RAIL_TICK_MS),
        ) {
            on_cleanup(move || handle.clear());
        }
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = set_now;

    Memo::new(move |_| now.get())
}

/// What one plan's badge says, and what colour it is.
///
/// Pure, and tested below, because it is the whole of the rail's answer to "is
/// anything waiting on me?" -- the question this rail exists for -- and every
/// wrong answer is a plan the King either chases for nothing or leaves blocked
/// for an hour.
///
/// What a plan *wants* outranks the status it is in, and that is the point
/// rather than a nicety. A status describes where a plan is in its life; an
/// [`Attention`] describes whose move it is. They are genuinely independent: a
/// plan parked on a question is `Drafting` and blocked, and painting it the
/// working green says the opposite of the truth.
fn badge_for(status: PlanStatus, needs: Option<Attention>) -> (&'static str, &'static str) {
    match needs {
        Some(needs) => (needs.label(), needs.css_suffix()),
        None => (status.label(), status.css_suffix()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fault this feature exists for. A plan that has stopped to ask
    /// something is still `Drafting`, so a badge read off the status alone says
    /// "Drafting" in the working green -- the same thing it says for a plan
    /// cheerfully running a build, while this one sits blocked on the King.
    #[test]
    fn a_plan_waiting_on_the_king_says_so_rather_than_drafting() {
        assert_eq!(
            badge_for(PlanStatus::Drafting, Some(Attention::Question)),
            ("Question", "asking"),
        );
        assert_eq!(
            badge_for(PlanStatus::Drafting, None),
            ("Drafting", "drafting"),
            "a plan actually working must keep reading as work in progress"
        );
    }

    /// The distinction the rail already drew, kept: "Awaiting review" is true
    /// both of a plan that merely finished speaking and of one holding a plan
    /// out to be started, and only the second is something to act on.
    #[test]
    fn a_standing_proposal_still_reads_as_a_proposal() {
        assert_eq!(
            badge_for(PlanStatus::AwaitingReview, Some(Attention::Proposal)),
            ("Proposal", "review"),
        );
        assert_eq!(
            badge_for(PlanStatus::AwaitingReview, None),
            ("Awaiting review", "review"),
        );
    }

    /// Settled history wants nothing and must not be tinted as though it did.
    #[test]
    fn a_settled_plan_asks_for_nothing() {
        assert_eq!(badge_for(PlanStatus::Merged, None), ("Merged", "merged"));
        assert_eq!(badge_for(PlanStatus::Failed, None), ("Failed", "failed"));
    }

    /// The four bands, at each boundary.
    ///
    /// Worth pinning at the edges rather than in the middle of each range,
    /// because every one of them is an integer division that is off by one if
    /// the comparison is: `59m ago` must not become `0h ago`, and the last
    /// second of a day must not become `0d ago`.
    #[test]
    fn an_age_says_the_coarsest_true_thing() {
        let now = Some(Timestamp(100 * 86_400_000));
        let ago = |ms: i64| since(now, Some(Timestamp(100 * 86_400_000 - ms)));

        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59_000), "just now");
        assert_eq!(ago(60_000), "1m ago");
        assert_eq!(ago(6 * 60_000), "6m ago");
        assert_eq!(ago(59 * 60_000), "59m ago");
        assert_eq!(ago(60 * 60_000), "1h ago");
        assert_eq!(ago(3 * 3_600_000), "3h ago");
        assert_eq!(ago(23 * 3_600_000 + 3_599_000), "23h ago");
        assert_eq!(ago(24 * 3_600_000), "1d ago");
        assert_eq!(ago(5 * 86_400_000), "5d ago");
    }

    /// Nothing known reads as nothing said.
    ///
    /// Both halves can be missing independently and for different reasons: a
    /// plan whose log predates timing has no moment, and the clock itself is
    /// `None` under SSR, where there is no browser to read it. Either way the
    /// row draws an empty line rather than inventing a number -- and the span is
    /// rendered regardless, so hydration finds the shape the server left.
    #[test]
    fn an_unknown_moment_says_nothing_at_all() {
        assert_eq!(since(Some(Timestamp(60_000)), None), "");
        assert_eq!(since(None, Some(Timestamp(0))), "");
        assert_eq!(since(None, None), "");
    }

    /// A stamp from the future is a clock that stepped, not a plan that will
    /// move later. `just now` is the least wrong thing to say about it; the
    /// arithmetic must not run backwards into `-1m ago`.
    #[test]
    fn a_moment_in_the_future_reads_as_now() {
        assert_eq!(
            since(Some(Timestamp(0)), Some(Timestamp(600_000))),
            "just now"
        );
    }
}
