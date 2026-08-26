//! The files rail: the column left of the transcript, split in two.
//!
//! A supporting column, not a peer of the transcript. It is narrower than the
//! cities rail by default and capped lower (see [`BOUNDS`]), because the King
//! came to read the conversation.
//!
//! This module owns the column itself -- its width, its drag handle -- and
//! stacks both of the things worth putting in it, one above the other:
//!
//! - **Files**, the plan's workspace as it stands on disk ([`super::FileTree`]).
//! - **Review**, every file this plan has changed ([`super::ReviewDrawer`]).
//!
//! # Why a split rather than tabs
//!
//! These two are read *together*. The question "what did my agent change?" is
//! answered against "what is in this project?", and tabs make holding both in
//! view impossible -- every glance from one to the other costs a click, and the
//! half that is hidden is invisible rather than merely small. A split costs the
//! same total height and never hides either one, which is what the King asked
//! for. The divider between them is the ordinary [`Resizer`], now able to drag
//! an axis it could not before.
//!
//! # Why the summary is not fetched here
//!
//! The badge has to be right whether or not the drawer is mounted, and the
//! diff panel needs the same numbers to know when a file it is showing has
//! moved on. So the signals are held one level up, in `conversation.rs`, where
//! both readers can see them, and passed down: this rail drives the *fetching*
//! through [`super::review_drawer::fetch_changes`] but does not own the answer.
//! Two copies would be two lists that could disagree.

use crate::app::{KingdomState, DEFAULT_TREE_WIDTH};
use crate::components::resizer::{restore_width, Bounds, Grows, Resizer};
use crate::components::review_drawer::fetch_changes;
use crate::components::{FileTree, ReviewDrawer};
use kingdom_core::{ChangeSummary, PlanId};
use leptos::prelude::*;

/// How far the files rail may be dragged.
///
/// The ceiling is deliberately below the cities rail's 560: this is the
/// *second* supporting column on the left, and two columns that could each grow
/// to the rail's maximum would leave the transcript fighting for the middle of
/// the screen. A tree of names also needs less room than a rail of titles,
/// badges and model names.
const BOUNDS: Bounds = Bounds {
    min: 180.0,
    max: 420.0,
    default: DEFAULT_TREE_WIDTH,
};

const WIDTH_KEY: &str = "kingdom.tree_width";

/// How tall the tree may be dragged, leaving the rest to the review drawer.
///
/// The floor is a few rows rather than zero: a pane dragged shut cannot be
/// found again, and the whole point of the split is that neither half
/// disappears. The ceiling is a pixel count rather than a fraction because the
/// drawer beneath it needs a usable minimum too, and a short window would
/// otherwise let 80% leave it with two rows.
const SPLIT_BOUNDS: Bounds = Bounds {
    min: 120.0,
    max: 900.0,
    default: 320.0,
};

const SPLIT_KEY: &str = "kingdom.rail_split";

/// The column left of the transcript: the city's files above, the plan's
/// changes below.
#[component]
pub fn CityRail(
    plan: PlanId,
    /// What the plan has changed, shared with the diff panel so the two cannot
    /// disagree about what is in the workspace. Filled by this rail.
    summary: RwSignal<Option<ChangeSummary>>,
    /// Set while a fetch is in flight.
    looking: RwSignal<bool>,
    /// How many transcript entries the plan has. Not read for its value -- it
    /// is the change signal the watch socket already gives us for free, and
    /// every tick of it means the court may have touched a file. See the module
    /// note in `review_drawer.rs`.
    activity: Memo<usize>,
    /// Bumped when the King deletes a file himself, so the tree can drop its
    /// cache and list the workspace again.
    ///
    /// Its own signal rather than folded into `activity`, because the two mean
    /// different things: `activity` says *something happened*, which is worth a
    /// cheap re-count of the diff, and this says *the shape of the workspace
    /// changed*, which is the only thing worth throwing a listing away for. The
    /// tree is deliberately cached against re-listing on every idle tick.
    revision: RwSignal<usize>,
    /// The file the panel beside the transcript is showing, if it is showing a
    /// file at all -- read whole or as a diff. Both panes highlight against it.
    open_file: Memo<Option<String>>,
    /// Called with a path when the King picks a file out of the **tree**, which
    /// opens it whole.
    on_read: Callback<String>,
    /// Called with a path when he picks one out of the **drawer**, which opens
    /// it as a diff.
    ///
    /// Two callbacks and not one, because the two rows mean different things:
    /// the tree offers every file in the project and the drawer offers the ones
    /// this plan changed, so "show me this" answers with the file in the first
    /// case and with what moved in the second. Deciding which is the chamber's
    /// -- see `Aside` in `conversation.rs`.
    on_diff: Callback<String>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    restore_width(state.tree_width, WIDTH_KEY, BOUNDS);

    // Where the divider sits: the tree's height in pixels. The drawer below
    // simply takes what is left, so only one of the two is ever measured and
    // they cannot drift out of sync with the rail's own height.
    let tree_height = RwSignal::new(SPLIT_BOUNDS.default);
    restore_width(tree_height, SPLIT_KEY, SPLIT_BOUNDS);

    let city_name = Memo::new(move |_| {
        let id = state.selected.get()?;
        // `with`, not `get`: reading one name should not clone the kingdom.
        state.kingdom.with(|k| k.city(&id).map(|c| c.name.clone()))
    });

    // Asked once on arrival and again whenever the court acts, so the list moves
    // while the work happens. `--numstat` on a repository is milliseconds.
    Effect::new({
        let plan = plan.clone();
        move |_| {
            activity.track();
            fetch_changes(plan.clone(), summary, looking);
        }
    });

    // The King's own way to ask again. It is the only one available when nothing
    // is running: the refetch above is driven by the court acting, so a settled
    // plan -- or one whose files he changed himself -- would otherwise show what
    // was true when he arrived, with nothing to press.
    let refresh = {
        let plan = plan.clone();
        move |_| fetch_changes(plan.clone(), summary, looking)
    };

    let changed = Memo::new(move |_| summary.get().map(|s| s.files.len()).unwrap_or(0));

    view! {
        // The width is set inline from the signal, as the focused panel's is:
        // this is a flex child of the chamber rather than a grid track, so the
        // resizer drives the element itself.
        <aside class="file-tree" style:width=move || format!("{}px", state.tree_width.get())>
            // --- The tree, above -------------------------------------------
            //
            // The only one of the two with a fixed height: the drawer takes the
            // remainder, so the divider has exactly one number behind it.
            <section
                class="rail-pane rail-files"
                style:height=move || format!("{}px", tree_height.get())
            >
                <div class="rail-pane-head">
                    <span class="rail-pane-label">"Files"</span>
                    <span class="file-tree-city" title=move || city_name.get()>
                        {move || city_name.get().unwrap_or_default()}
                    </span>
                </div>
                <FileTree
                    plan=plan.clone()
                    revision=revision
                    open_file=open_file
                    on_open=on_read
                />
            </section>

            // The divider. Between the two panes rather than at the rail's edge,
            // and dragging it down gives the tree the room the drawer loses.
            <Resizer
                width=tree_height
                grows=Grows::Downwards
                bounds=SPLIT_BOUNDS
                storage_key=SPLIT_KEY
                class="rail-split"
            />

            // --- The review drawer, below ----------------------------------
            //
            // `flex: 1` and no height of its own, so the two panes always add up
            // to the rail exactly however tall the window is.
            <section class="rail-pane rail-review">
                <div class="rail-pane-head">
                    <span class="rail-pane-label">"Review"</span>
                    // The count, not the line total: how many files there are to
                    // look at is the decision being made here, and a four-digit
                    // number in a badge is noise.
                    <Show when=move || { changed.get() > 0 }>
                        <span class="rail-pane-count">{move || changed.get()}</span>
                    </Show>
                    <span class="rail-pane-spacer"></span>
                    <button
                        class="rail-refresh"
                        class:looking=move || looking.get()
                        title="Look again for changes"
                        on:click=refresh
                    >
                        "\u{21bb}"
                    </button>
                </div>
                <ReviewDrawer
                    plan=plan.clone()
                    summary=summary
                    looking=looking
                    open_file=open_file
                    on_open=on_diff
                />
            </section>

            <Resizer
                width=state.tree_width
                grows=Grows::Rightwards
                bounds=BOUNDS
                storage_key=WIDTH_KEY
                class="file-tree-resizer"
            />
        </aside>
    }
}
