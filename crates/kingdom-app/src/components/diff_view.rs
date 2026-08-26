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
//!
//! # Writing against a line
//!
//! Either column takes a note, and which one it was matters: a note on the old
//! side is a note about a line that is *gone*, so it travels as
//! [`NoteSide::Base`] and is reported to the court as "in the version before
//! your changes". A bare line number would point at whatever now occupies that
//! position in the file.
//!
//! The composer opens as a **full-width row beneath**, spanning both columns:
//! the grid already knows how to do that (`.diff-gap` does it), and a box inside
//! one cell would be half a panel wide and would push its own column out of step
//! with the other.
//!
//! # Why it holds still while a note is being written
//!
//! The panel refetches when the file's counts move, so an open diff follows the
//! court's edits. That is right while the King is only reading and wrong the
//! moment he is typing against a line: the rows would shift under him. So an
//! open composer suspends the refetch, exactly as it does in the source view.

use crate::api::plan_diff;
use crate::components::note_composer::NoteComposer;
use kingdom_core::{DiffLine, DiffRow, FileDiff, NoteSide, PlanId, ReviewNote};
use leptos::prelude::*;
use std::collections::HashSet;

/// Which column a cell belongs to. Two constants rather than an enum: they are
/// only ever CSS class names, and the view reads better for saying so.
const OLD: &str = "old";
const NEW: &str = "new";

/// Names one annotatable cell: which side, and which line of it.
///
/// A pair rather than a line number alone, because the two columns number
/// independently -- line 12 of the old file and line 12 of the new one are both
/// on screen and are different lines.
type Cell = (NoteSide, u32);

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
    /// The notes already standing against this file, so a line that carries one
    /// can say so.
    notes: Memo<Vec<ReviewNote>>,
    /// The panel's width in pixels, driven by the resizer beside it.
    width: RwSignal<f64>,
    /// Writes a note: the line, which side of the comparison it is on, the
    /// line's own text, and what the King wrote. All four travel because the
    /// server needs the quote and the view needs the line -- see `ReviewNote`.
    on_note: Callback<(u32, NoteSide, String, String)>,
    /// Closes the panel. The King's own way out, since nothing else will take
    /// the space back.
    on_close: Callback<()>,
) -> impl IntoView {
    let (diff, set_diff) = signal(None::<FileDiff>);
    let (failed, set_failed) = signal(None::<String>);

    // Which cell has a composer open. One at a time, as the proposal's blocks
    // and the source view's lines are.
    let (writing, set_writing) = signal(None::<Cell>);

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

            // ...unless a note is being written against a row right now. See
            // the module note: re-reading under an open composer moves the rows
            // out from under it.
            if writing.get_untracked().is_some() {
                return;
            }

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

    // Which cells already carry a note. A set rather than a scan per row: a
    // diff is hundreds of rows and a review is a handful of notes, so this is
    // built once per change instead of once per cell.
    let marked = Memo::new(move |_| {
        notes
            .get()
            .into_iter()
            .map(|n| (n.side, n.line))
            .collect::<HashSet<Cell>>()
    });

    view! {
        <div
            class="diff-panel chamber-aside"
            style:width=move || format!("{}px", width.get())
            // The panel's own width, published for the note composer below.
            // The grid it sits in is `max-content` wide -- wider than the panel
            // whenever a line is long -- so a composer sized as a percentage of
            // its parent would run off the right edge. This is the one number
            // that knows how much room there actually is.
            style:--panel-width=move || format!("{}px", width.get())
        >
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
                                        // Which cell of this row, if either, has
                                        // the composer open. Held here rather
                                        // than in `Side` so the box can be a
                                        // sibling of both cells and span the
                                        // grid -- see the module note.
                                        let old_line = row.old.clone();
                                        let new_line = row.new.clone();
                                        let writing_here = Memo::new(move |_| {
                                            let open = writing.get()?;
                                            let (side, line) = open;
                                            let here = match side {
                                                NoteSide::Base => old_line.as_ref(),
                                                NoteSide::Working => new_line.as_ref(),
                                            }?;
                                            (here.number == line)
                                                .then(|| (side, line, here.text()))
                                        });

                                        view! {
                                            <div class="diff-row" class:context=context>
                                                <Side
                                                    line=row.old
                                                    side=OLD
                                                    note_side=NoteSide::Base
                                                    marked=marked
                                                    writing=writing
                                                    set_writing=set_writing
                                                />
                                                <Side
                                                    line=row.new
                                                    side=NEW
                                                    note_side=NoteSide::Working
                                                    marked=marked
                                                    writing=writing
                                                    set_writing=set_writing
                                                />
                                            </div>

                                            // The box, spanning both columns
                                            // beneath the row it is about. A
                                            // sibling of `.diff-row` rather than
                                            // a child, because `.diff-row` is
                                            // `display: contents` and has no box
                                            // of its own to put anything in.
                                            <Show when=move || writing_here.get().is_some()>
                                                {move || {
                                                    let (side, line, quote) = writing_here.get()?;
                                                    let quote = StoredValue::new(quote);
                                                    Some(view! {
                                                        <div class="diff-composer">
                                                            <NoteComposer
                                                                quote=quote.get_value()
                                                                about=match side {
                                                                    NoteSide::Base => format!(
                                                                        "line {line}, as it was"
                                                                    ),
                                                                    NoteSide::Working => {
                                                                        format!("line {line}")
                                                                    }
                                                                }
                                                                on_write=Callback::new(
                                                                    move |note: String| {
                                                                        set_writing.set(None);
                                                                        on_note.run((
                                                                            line,
                                                                            side,
                                                                            quote.get_value(),
                                                                            note,
                                                                        ));
                                                                    }
                                                                )
                                                                on_cancel=Callback::new(
                                                                    move |_| set_writing.set(None)
                                                                )
                                                            />
                                                        </div>
                                                    })
                                                }}
                                            </Show>
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
/// the blank on the right to be *there*. An empty cell is deliberately **not**
/// pressable either -- there is no line there to write a note against.
#[component]
fn Side(
    line: Option<DiffLine>,
    /// The CSS class naming this column.
    side: &'static str,
    /// Which version of the file this column shows. The old column is the file
    /// before the plan touched it, so a note there is a note about a line that
    /// may no longer exist.
    note_side: NoteSide,
    /// Every cell that already carries a note, so this one can say whether it
    /// is one of them.
    marked: Memo<HashSet<Cell>>,
    writing: ReadSignal<Option<Cell>>,
    set_writing: WriteSignal<Option<Cell>>,
) -> impl IntoView {
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
    let cell: Cell = (note_side, number);

    let noted = Memo::new(move |_| marked.get().contains(&cell));
    let open = Memo::new(move |_| writing.get() == Some(cell));

    view! {
        <div
            class="diff-cell"
            class=(side, true)
            class:changed=changed
            class:noted=move || noted.get()
            class:writing=move || open.get()
        >
            // The number is the affordance, as it is in the source view: making
            // the *text* pressable would take selecting a line to copy it --
            // which is what the King does most while reading a diff -- and turn
            // it into opening a box he did not ask for.
            <button
                class="diff-number"
                title="Write a note against this line"
                on:click=move |_| {
                    // Clicking the open cell's own number closes it, so the
                    // affordance that opened the box also puts it away.
                    set_writing.update(|w| {
                        *w = if *w == Some(cell) { None } else { Some(cell) };
                    });
                }
            >
                {number}
                <Show when=move || noted.get()>
                    <span class="diff-note-mark">"\u{270e}"</span>
                </Show>
            </button>
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
