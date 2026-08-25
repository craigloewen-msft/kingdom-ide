//! The diff panel: one changed file, old beside new.
//!
//! Sits where the spyglass sits, and for the same reason: the King reads the
//! transcript and the thing it is describing at once, without either being
//! shortened to make room for the other. The two panels are alternatives
//! rather than neighbours -- see `Aside` in `conversation.rs`, which is the one
//! place that decides which is showing.
//!
//! # What it is given, and why
//!
//! Everything reactive arrives as a prop, exactly as [`super::BrowserView`]'s
//! does: the plan and path to fetch, the width to occupy, a way to close. The
//! panel is then a thing that renders a diff for whatever it is handed.
//!
//! # Why the pairing is not done here
//!
//! A [`kingdom_core::DiffRow`] arrives already paired -- an old side and a new
//! side together. Deciding which deletion sits opposite which insertion needs
//! the differ that knows a replacement *was* a replacement, so it happens in
//! `crate::review` on the server and this view renders two columns without
//! re-deciding anything. The module note there has the full reasoning.
//!
//! # One scroll container, not two
//!
//! Both columns live in a single scrolling grid, so they are synchronised by
//! construction. Two panes kept in step by a scroll handler is the usual
//! implementation and the usual bug: it drifts on a fast scroll and fights the
//! trackpad's own momentum.

use crate::api::plan_diff;
use kingdom_core::{DiffLine, DiffRow, FileDiff, PlanId};
use leptos::prelude::*;

/// Which column a cell belongs to. Two constants rather than an enum: they are
/// only ever CSS class names, and the view reads better for saying so.
const OLD: &str = "old";
const NEW: &str = "new";

/// One file's changes, old beside new.
#[component]
pub fn DiffView(
    plan: PlanId,
    /// The file being read, relative to the plan's workspace. Changing this
    /// fetches the new file -- the panel is not remounted, so the scroll
    /// position of a file the King comes back to is deliberately not kept.
    path: Memo<Option<String>>,
    /// A stamp that changes when the file's own line counts do, so an open diff
    /// follows the court's edits. Cheap: it is read off the summary the rail
    /// has already fetched, rather than costing a request of its own.
    version: Memo<(u32, u32)>,
    /// The panel's width in pixels, driven by the resizer beside it.
    width: RwSignal<f64>,
    /// Closes the panel. The King's own way out, since nothing else will take
    /// the space back.
    on_close: Callback<()>,
) -> impl IntoView {
    let (diff, set_diff) = signal(None::<FileDiff>);
    let (failed, set_failed) = signal(None::<String>);

    Effect::new({
        let plan = plan.clone();
        move |_| {
            let Some(path) = path.get() else {
                set_diff.set(None);
                return;
            };
            // Tracked, not read: a file whose counts moved is a file worth
            // fetching again.
            version.track();

            let plan = plan.to_string();
            set_failed.set(None);
            leptos::task::spawn_local(async move {
                match plan_diff(plan, path).await {
                    Ok(fetched) => {
                        set_diff.set(Some(fetched));
                        set_failed.set(None);
                    }
                    // Reported in the panel rather than through `state.error`,
                    // which belongs to the composer -- the same division the
                    // orders overlay keeps.
                    Err(e) => set_failed.set(Some(e.to_string())),
                }
            });
        }
    });

    let name = Memo::new(move |_| path.get().unwrap_or_default());
    // The fetched diff, but only while it is still the file being asked for.
    //
    // Switching files leaves the previous answer in the signal until the new
    // one lands, and rendering it would put one file's rows under another
    // file's name -- briefly, and wrongly. `FileDiff` carries the path it is
    // for, so the check is exact rather than a guess at timing.
    let shown = Memo::new(move |_| {
        let diff = diff.get()?;
        (Some(diff.path.as_str()) == path.get().as_deref()).then_some(diff)
    });

    let base = Memo::new(move |_| shown.get().map(|d| d.base).unwrap_or_default());
    let hunks = Memo::new(move |_| shown.get().map(|d| d.hunks).unwrap_or_default());
    // A verdict is what the panel says *instead of*, or beneath, the rows: a
    // binary file, a truncated one, one that could not be read at all.
    let verdict = Memo::new(move |_| shown.get().and_then(|d| d.verdict.tell()));
    let waiting = Memo::new(move |_| shown.get().is_none() && failed.get().is_none());

    view! {
        <div class="diff-panel chamber-aside" style:width=move || format!("{}px", width.get())>
            <div class="diff-bar">
                <span class="diff-path" title=move || name.get()>{move || name.get()}</span>
                // What this is being compared against, stated in the panel as
                // well as in the drawer: the King may have opened the diff and
                // then switched the rail back to the tree.
                <Show when=move || !base.get().is_empty()>
                    <span class="diff-against">"vs "{move || base.get()}</span>
                </Show>
                <button class="diff-close" title="Close" on:click=move |_| on_close.run(())>
                    "\u{00d7}"
                </button>
            </div>

            <div class="diff-stage">
                <Show when=move || waiting.get()>
                    <p class="diff-empty">"Comparing\u{2026}"</p>
                </Show>

                <Show when=move || failed.get().is_some()>
                    <p class="diff-failed">
                        "This file could not be compared: "{move || failed.get().unwrap_or_default()}
                    </p>
                </Show>

                <Show when=move || verdict.get().is_some()>
                    <p class="diff-verdict">{move || verdict.get().unwrap_or_default()}</p>
                </Show>

                <Show when=move || {
                    shown.get().is_some() && hunks.get().is_empty() && verdict.get().is_none()
                }>
                    <p class="diff-empty">"This file matches the base exactly."</p>
                </Show>

                // One grid, both columns. See the module note: two panes kept
                // in step by a scroll handler drift under momentum.
                <div class="diff-grid">
                    <For
                        each={move || hunks.get().into_iter().enumerate().collect::<Vec<_>>()}
                        key=|(i, hunk): &(usize, kingdom_core::Hunk)| {
                            // Keyed on the shape of the hunk, not its index
                            // alone: a refetch after an edit gives the same
                            // index a different run of lines.
                            (
                                *i,
                                hunk.rows.len(),
                                hunk.rows.first().and_then(|r| r.old.as_ref().map(|l| l.number)),
                                hunk.rows.first().and_then(|r| r.new.as_ref().map(|l| l.number)),
                            )
                        }
                        let:entry
                    >
                        {
                            let (index, hunk) = entry;
                            view! {
                                // The break between two hunks, so a jump in the
                                // line numbers is announced rather than
                                // discovered.
                                <Show when=move || { index > 0 }>
                                    <div class="diff-gap">
                                        <span class="diff-gap-mark">"\u{22ef}"</span>
                                    </div>
                                </Show>
                                <For
                                    each={
                                        let rows = hunk.rows.clone();
                                        move || rows.clone().into_iter().enumerate().collect::<Vec<_>>()
                                    }
                                    key=|(i, _): &(usize, DiffRow)| *i
                                    let:row
                                >
                                    {
                                        let (_, row) = row;
                                        let context = row.is_context();
                                        view! {
                                            <div class="diff-row" class:context=context>
                                                <Side line=row.old side=OLD/>
                                                <Side line=row.new side=NEW/>
                                            </div>
                                        }
                                    }
                                </For>
                            }
                        }
                    </For>
                </div>
            </div>
        </div>
    }
}

/// One half of one row: a line number, and the text with its changed parts
/// emphasised.
///
/// An absent line renders as an empty cell rather than being skipped, because
/// the two columns must stay in step: a deletion with nothing opposite it needs
/// the blank on the right to be *there*.
#[component]
fn Side(line: Option<DiffLine>, side: &'static str) -> impl IntoView {
    let Some(line) = line else {
        return view! {
            <div class="diff-cell empty" class=(side, true)>
                <span class="diff-number"></span>
                <span class="diff-text"></span>
            </div>
        }
        .into_any();
    };

    let number = line.number;
    let changed = line.changed;
    let spans = line.spans;

    view! {
        <div class="diff-cell" class=(side, true) class:changed=changed>
            <span class="diff-number">{number}</span>
            <span class="diff-text">
                <For
                    each={
                        let spans = spans.clone();
                        move || spans.clone().into_iter().enumerate().collect::<Vec<_>>()
                    }
                    key=|(i, _): &(usize, kingdom_core::Span)| *i
                    let:span
                >
                    {
                        let (_, span) = span;
                        view! {
                            <span class="diff-span" class:emphasis=span.emphasis>
                                {span.text.clone()}
                            </span>
                        }
                    }
                </For>
            </span>
        </div>
    }
    .into_any()
}
