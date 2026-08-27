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
//! # Why the lines wrap by default
//!
//! They did not, and the panel was reported as showing "only the left side".
//! It was: `.diff-grid` was `width: max-content` with columns of
//! `minmax(50%, 1fr)`, so the grid grew to the longest line and each column
//! took half of *that*. Measured on one real line of this repository in a panel
//! at its 640px default, the columns resolved to 856.8px each -- so the new
//! side began at x=857 inside a box 640 wide, off screen until the King thought
//! to scroll sideways. Short files were fine, which is why it survived: the
//! failure bites exactly on the real code a diff is opened for.
//!
//! So the rows wrap, and the grid is the panel's width. A wrapped pair stays
//! level for free -- a grid row is as tall as its tallest cell -- which is the
//! part a hand-rolled two-pane view gets wrong. Wrapping can be turned off for
//! a King who would rather scroll, and the choice is remembered ([`WRAP_KEY`]).
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
//!
//! # Seeing further than the diff shows
//!
//! Three lines of context either side of a change say *what* moved and often
//! not *where*: the function a hunk sits inside starts above the first row
//! drawn. So every break between hunks -- and the run before the first and after
//! the last, which the panel never used to mark at all -- is a [`Gap`], and the
//! King opens it twenty lines at a time from either end, or all at once.
//!
//! **The lines are fetched, not shipped with the diff.** Sending the whole file
//! and folding it here would be simpler and would undo the row cap the panel is
//! built around: a 40,000-line file with one changed line is cheap today
//! precisely because the unchanged 39,990 never leave the server. See
//! `crate::api::plan_diff_context`, and `review::context` for why nothing is
//! re-diffed to answer.

use crate::api::{plan_diff, plan_diff_context};
use crate::components::note_composer::NoteComposer;
use crate::components::resizer::{restore_flag, store_flag};
use kingdom_core::{DiffLine, DiffRow, FileDiff, NoteSide, PlanId, ReviewNote};
use leptos::prelude::*;
use leptos::server_fn::ServerFnError;
use std::collections::HashSet;

/// Lines one press of a gap's control reveals.
///
/// Twenty, as GitHub's is. Small enough that the answer lands instantly and the
/// King keeps his place on screen, large enough that finding an enclosing `fn`
/// is usually one press rather than four.
const STEP: u32 = 20;

/// Whether long lines wrap inside their column, remembered between visits.
///
/// Its own key rather than a mode of the panel's width: it is a reading
/// preference that should survive closing the diff, and it is the answer to the
/// bug in the module note above -- so it is on unless the King turns it off.
const WRAP_KEY: &str = "kingdom.diff_wrap";

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
    ///
    /// Ignored while focused: there the panel takes the room the conversation
    /// was in, and a pixel width would be the one number claiming otherwise.
    width: RwSignal<f64>,
    /// Whether the panel has been given the conversation's room as well as its
    /// own. Owned by the chamber, because all three panels share one slot and
    /// therefore one answer -- see `Aside` in `conversation.rs`.
    focused: Signal<bool>,
    /// Asks for that, or gives it back. The panel reports a press and decides
    /// nothing, exactly as it does for `on_close`.
    on_focus: Callback<()>,
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

    // Whether long lines wrap. On unless the King has said otherwise -- see the
    // module note: off is the behaviour that hid half of every real diff.
    let wrapping = RwSignal::new(true);
    restore_flag(wrapping, WRAP_KEY);

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

    // The plan, kept where both the per-hunk gaps and the trailing one can take
    // a copy: the first is used inside a `<For>` closure, which moves what it
    // captures, and a second `.clone()` afterwards would be a borrow of a value
    // already gone.
    let plan_id = StoredValue::new(plan.clone());

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
            // Absent while focused, rather than overridden. An inline style
            // beats the stylesheet, so a width left standing here would be the
            // one thing keeping the panel narrow -- and `Option` is how tachys
            // spells "remove this property" without an `!important` on the
            // other side.
            style:width=move || (!focused.get()).then(|| format!("{}px", width.get()))
        >
            <div class="diff-bar">
                <span class="diff-path" title=move || name.get()>{move || name.get()}</span>
                // What this is being compared against, stated in the panel as
                // well as in the drawer: the King may have opened the diff and
                // then switched the rail back to the tree.
                <Show when=move || !base.get().is_empty()>
                    <span class="diff-against">"vs "{move || base.get()}</span>
                </Show>

                // Wrapping is a property of *reading a diff*, so its control is
                // here rather than in the chamber's chrome. Named for what it
                // does rather than for the state it is in, because it is a
                // switch and reads as one.
                <button
                    class="diff-chip"
                    class:on=move || wrapping.get()
                    title="Wrap long lines so both sides fit the panel"
                    on:click=move |_| {
                        let next = !wrapping.get_untracked();
                        wrapping.set(next);
                        store_flag(WRAP_KEY, next);
                    }
                >
                    "Wrap"
                </button>

                // Naming the state he is *not* in, as the source view's mode
                // segments and the proposal card's own toggle do: a lone
                // "Focus" leaves "what am I looking at now?" to be inferred.
                <button
                    class="diff-chip"
                    class:on=move || focused.get()
                    title="Give this panel the conversation's room as well as its own"
                    on:click=move |_| on_focus.run(())
                >
                    {move || if focused.get() { "Show conversation" } else { "Focus" }}
                </button>

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
                <div class="diff-grid" class:wrap=move || wrapping.get()>
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
                                // What is hidden before this hunk, and the way
                                // to open it. Hunk 0 included: a change on line
                                // 400 otherwise simply starts the panel, with
                                // no sign that 399 lines come first.
                                <Gap
                                    plan=plan_id.get_value()
                                    path=path
                                    gap=Memo::new(move |_| {
                                        shown.get().filter(|d| d.may_expand())?.gap_before(index)
                                    })
                                    parted={index > 0}
                                    marked=marked
                                    writing=writing
                                    set_writing=set_writing
                                    on_note=on_note
                                />
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
                                        view! {
                                            <Row
                                                row=row
                                                marked=marked
                                                writing=writing
                                                set_writing=set_writing
                                                on_note=on_note
                                            />
                                        }
                                    }
                                </For>
                            }
                        }
                    </For>

                    // And the tail of the file, for the same reason the leading
                    // gap exists: a change forty lines from the end should say
                    // so rather than look like the end of the file.
                    <Gap
                        plan=plan_id.get_value()
                        path=path
                        gap=Memo::new(move |_| {
                            shown.get().filter(|d| d.may_expand())?.gap_after_last()
                        })
                        parted=false
                        marked=marked
                        writing=writing
                        set_writing=set_writing
                        on_note=on_note
                    />
                </div>
            </div>
        </div>
    }
}

/// A server function's failure, in the words the server actually wrote.
///
/// `ServerFnError`'s own `Display` prefixes "error running server function: ",
/// which is a fact about the transport and reads as noise in a strip a few
/// words wide -- what the King is being told is *this file has changed since it
/// was compared*, and the plumbing around that sentence is not for him.
fn plainly(error: ServerFnError) -> String {
    match error {
        ServerFnError::ServerError(said) => said,
        // Anything else genuinely is about the request rather than about the
        // file, and its own wording is the best available.
        other => other.to_string(),
    }
}

/// One row of the comparison: two cells that must stay level, and the composer
/// that opens beneath them.
///
/// A component rather than the inline block it used to be, because revealed
/// context lines need exactly this and a second renderer for them would be a
/// second place for "which side is this note about?" to be answered. A line the
/// King opened up takes a note the same way a line the diff chose to show does.
#[component]
fn Row(
    row: DiffRow,
    marked: Memo<HashSet<Cell>>,
    writing: ReadSignal<Option<Cell>>,
    set_writing: WriteSignal<Option<Cell>>,
    on_note: Callback<(u32, NoteSide, String, String)>,
) -> impl IntoView {
    let context = row.is_context();

    // Which cell of this row, if either, has the composer open. Held here
    // rather than in `Side` so the box can be a sibling of both cells and span
    // the grid -- see the module note.
    let old_line = row.old.clone();
    let new_line = row.new.clone();
    let writing_here = Memo::new(move |_| {
        let (side, line) = writing.get()?;
        let here = match side {
            NoteSide::Base => old_line.as_ref(),
            NoteSide::Working => new_line.as_ref(),
        }?;
        (here.number == line).then(|| (side, line, here.text()))
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

        // The box, spanning both columns beneath the row it is about. A sibling
        // of `.diff-row` rather than a child, because `.diff-row` is
        // `display: contents` and has no box of its own to put anything in.
        <Show when=move || writing_here.get().is_some()>
            {move || {
                let (side, line, quote) = writing_here.get()?;
                let quote = StoredValue::new(quote);
                Some(view! {
                    <div class="diff-composer">
                        <NoteComposer
                            quote=quote.get_value()
                            about=match side {
                                NoteSide::Base => format!("line {line}, as it was"),
                                NoteSide::Working => format!("line {line}"),
                            }
                            on_write=Callback::new(move |note: String| {
                                set_writing.set(None);
                                on_note.run((line, side, quote.get_value(), note));
                            })
                            on_cancel=Callback::new(move |_| set_writing.set(None))
                        />
                    </div>
                })
            }}
        </Show>
    }
}

/// Lines the diff is not showing, and the ways to open them.
///
/// # What it draws, and in what order
///
/// Rows revealed from the top, the strip, then rows revealed from the bottom --
/// as a **fragment**, never wrapped in an element. `.diff-grid` is the grid
/// itself and `.diff-row` is `display: contents`; a box around any of this would
/// take its cells out of the grid and put the two columns out of step.
///
/// # Why the reveal is local, and forgotten on a refetch
///
/// The counts live here rather than in the panel, so this is emptied whenever
/// the gap it was given moves -- which is exactly when the court has edited the
/// file. Revealed lines left standing across that would be text the King is
/// still reading and the workspace no longer holds.
#[component]
fn Gap(
    plan: PlanId,
    path: Memo<Option<String>>,
    /// What is hidden here, or `None` when nothing is -- two hunks that touch,
    /// or a comparison too partial to measure (`FileDiff::may_expand`).
    gap: Memo<Option<kingdom_core::Gap>>,
    /// Whether this gap sits *between* two hunks rather than at an end of the
    /// file. Only the break between hunks is drawn once there is nothing left
    /// to reveal: at the ends, an empty strip would be a line announcing that
    /// the file begins where it begins.
    parted: bool,
    marked: Memo<HashSet<Cell>>,
    writing: ReadSignal<Option<Cell>>,
    set_writing: WriteSignal<Option<Cell>>,
    on_note: Callback<(u32, NoteSide, String, String)>,
) -> impl IntoView {
    // What has been revealed from each end. Two runs rather than one, because
    // "read on from the change above" and "see what encloses the change below"
    // are different questions, and the answer to one must not shift the other.
    let (above, set_above) = signal(Vec::<DiffRow>::new());
    let (below, set_below) = signal(Vec::<DiffRow>::new());
    let (failed, set_failed) = signal(None::<String>);
    let fetching = RwSignal::new(false);

    // A gap that has moved is a gap whose revealed rows are about a different
    // part of the file. Clearing on change is what keeps a refetch from leaving
    // the King reading lines that were true a moment ago.
    Effect::new(move |_| {
        gap.track();
        set_above.set(Vec::new());
        set_below.set(Vec::new());
        set_failed.set(None);
    });

    // What is still hidden, once both ends have been opened as far as they have.
    let left = Memo::new(move |_| {
        gap.get()?
            .narrowed(above.get().len() as u32, below.get().len() as u32)
    });

    // Asks for a run and files it under the end it was taken from. One closure
    // for both directions: which end this was is the only thing that differs,
    // and two would be two places to get the append order wrong.
    let reveal = {
        let plan = plan.clone();
        move |run: kingdom_core::Gap, from_top: bool| {
            if fetching.get_untracked() {
                return;
            }
            let Some(path) = path.get_untracked() else {
                return;
            };
            fetching.set(true);
            set_failed.set(None);

            let plan = plan.to_string();
            leptos::task::spawn_local(async move {
                let asked =
                    plan_diff_context(plan, path, run.old_from, run.new_from, run.count).await;
                match asked {
                    Ok(rows) => {
                        if from_top {
                            // Appended after what was already revealed from the
                            // top: this run starts where that one ended.
                            set_above.update(|had| had.extend(rows));
                        } else {
                            // Prepended, for the mirror reason -- this run sits
                            // above whatever was revealed upwards before it.
                            set_below.update(|had| {
                                let mut rows = rows;
                                rows.append(had);
                                *had = rows;
                            });
                        }
                    }
                    Err(e) => set_failed.set(Some(plainly(e))),
                }
                fetching.set(false);
            });
        }
    };
    let reveal = StoredValue::new(reveal);

    let revealed = move |rows: ReadSignal<Vec<DiffRow>>| {
        view! {
            <For
                each={move || rows.get().into_iter().enumerate().collect::<Vec<_>>()}
                key=|(_, row): &(usize, DiffRow)| {
                    // Keyed on the line itself rather than its position: the run
                    // grows at one end as more is revealed, and an index key
                    // would renumber every row already on screen.
                    (
                        row.old.as_ref().map(|l| l.number),
                        row.new.as_ref().map(|l| l.number),
                    )
                }
                let:entry
            >
                {
                    let (_, row) = entry;
                    view! {
                        <Row
                            row=row
                            marked=marked
                            writing=writing
                            set_writing=set_writing
                            on_note=on_note
                        />
                    }
                }
            </For>
        }
    };

    view! {
        {revealed(above)}

        <Show when=move || left.get().is_some() || (parted && gap.get().is_some())>
            <div class="diff-gap">
                // Sticky, and sized against the stage rather than the grid, for
                // the reason `.diff-composer` is: unwrapped, the grid is as wide
                // as the longest line in the file, and a strip that scrolled
                // with it would put its own buttons off the panel's edge.
                <div class="diff-gap-strip">
                    <span class="diff-gap-mark">"\u{22ef}"</span>
                    <span class="diff-gap-count">
                        {move || match left.get() {
                            Some(gap) if gap.count == 1 => "1 line not shown".to_string(),
                            Some(gap) => format!("{} lines not shown", gap.count),
                            // Nothing left, and the strip is only still drawn
                            // because the break between two hunks is worth
                            // marking however much of it has been opened.
                            None => String::new(),
                        }}
                    </span>

                    <Show when=move || left.get().is_some()>
                        <span class="diff-gap-acts">
                            // Upwards first, reading left to right: it is the
                            // one the King reaches for, since the `fn` a change
                            // sits inside starts above it.
                            <button
                                class="diff-gap-act"
                                title="Show the lines just above the change below"
                                disabled=move || fetching.get()
                                on:click=move |_| {
                                    if let Some(gap) = left.get_untracked() {
                                        reveal.with_value(|f| f(gap.tail(STEP), false));
                                    }
                                }
                            >
                                {format!("\u{2191} {STEP}")}
                            </button>

                            // Only while the whole of what remains fits one
                            // answer. Offered against a larger gap it would
                            // reveal the cap and leave the rest -- a button that
                            // does less than it says.
                            <Show when=move || {
                                left.get().is_some_and(|g| g.count <= kingdom_core::MOST_CONTEXT)
                            }>
                                <button
                                    class="diff-gap-act"
                                    title="Show every line hidden here"
                                    disabled=move || fetching.get()
                                    on:click=move |_| {
                                        if let Some(gap) = left.get_untracked() {
                                            reveal.with_value(|f| f(gap, true));
                                        }
                                    }
                                >
                                    {move || {
                                        let n = left.get().map(|g| g.count).unwrap_or_default();
                                        format!("Show all {n}")
                                    }}
                                </button>
                            </Show>

                            <button
                                class="diff-gap-act"
                                title="Show the lines just below the change above"
                                disabled=move || fetching.get()
                                on:click=move |_| {
                                    if let Some(gap) = left.get_untracked() {
                                        reveal.with_value(|f| f(gap.head(STEP), true));
                                    }
                                }
                            >
                                {format!("\u{2193} {STEP}")}
                            </button>
                        </span>
                    </Show>
                </div>
            </div>
        </Show>

        // Reported in the strip's own place rather than over the panel: the
        // ordinary cause is the court having rewritten this stretch of the file,
        // which is a fact about these lines and not about the comparison.
        <Show when=move || failed.get().is_some()>
            <div class="diff-gap">
                <div class="diff-gap-strip">
                    <span class="diff-gap-failed">
                        "These lines could not be shown: "
                        {move || failed.get().unwrap_or_default()}
                    </span>
                </div>
            </div>
        </Show>

        {revealed(below)}
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
