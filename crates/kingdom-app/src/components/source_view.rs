//! The source panel: one file, as it stands, with a note against any line.
//!
//! Sits where the spyglass and the diff sit, and for their reason: the King
//! reads the transcript and the thing it is describing at once, without either
//! being shortened to make room for the other. All three are alternatives rather
//! than neighbours -- see `Aside` in `conversation.rs`, which is the one place
//! that decides which is showing.
//!
//! # Why this exists beside the diff
//!
//! The diff answers "what did my agent change?". This answers "what is in this
//! file?", which is the question the files tree offers -- and most files in a
//! project have no diff at all, so opening one from the tree through the diff
//! panel would show an empty comparison for nearly everything in it.
//!
//! # Why it holds still while a note is being written
//!
//! The panel refetches when the court touches the file, so an open file follows
//! the work. That is right while the King is only reading and wrong the moment
//! he is typing against line 34: the lines would shift under him and the note
//! would land on something he never read. So a composer being open suspends the
//! refetch, and the update is taken when it closes. The `quote` carried on every
//! note is the second half of that guarantee -- see
//! [`kingdom_core::ReviewNote::quote`].

use crate::api::plan_source;
use crate::components::note_composer::NoteComposer;
use kingdom_core::{NoteSide, PlanId, ReviewNote, SourceLine, SourceText};
use leptos::prelude::*;
use std::collections::HashSet;

/// One file of the plan's workspace, line by line.
#[component]
pub fn SourceView(
    plan: PlanId,
    /// The file being read, relative to the plan's workspace. Changing this
    /// fetches the new file -- the panel is not remounted, so the scroll
    /// position of a file the King comes back to is deliberately not kept.
    path: Memo<Option<String>>,
    /// A stamp that changes when the plan's transcript grows, so an open file
    /// follows the court's edits. Every transcript entry is a moment the court
    /// may have written to disk -- the same free change signal the review
    /// drawer uses.
    version: Memo<usize>,
    /// The notes already standing against this file, so a line that carries one
    /// can say so.
    notes: Memo<Vec<ReviewNote>>,
    /// The panel's width in pixels, driven by the resizer beside it.
    width: RwSignal<f64>,
    /// Writes a note: the line, which version it is of, the line's own text,
    /// and what the King wrote. All four travel because the server needs the
    /// quote and the view needs the line -- see `ReviewNote`.
    on_note: Callback<(u32, NoteSide, String, String)>,
    /// Closes the panel. The King's own way out, since nothing else will take
    /// the space back.
    on_close: Callback<()>,
) -> impl IntoView {
    let (text, set_text) = signal(None::<SourceText>);
    let (failed, set_failed) = signal(None::<String>);

    // Which line has a composer open. One at a time, as the proposal's blocks
    // are: several open boxes is complexity nothing has asked for, and the King
    // writing two notes at once is not a thing that happens.
    let (writing, set_writing) = signal(None::<u32>);

    Effect::new({
        let plan = plan.clone();
        move |_| {
            let Some(path) = path.get() else {
                set_text.set(None);
                return;
            };
            // Tracked, not read: the court acting is a reason to look again.
            version.track();

            // ...unless a note is being written against a line right now. See
            // the module note: re-reading under an open composer moves the lines
            // out from under it.
            if writing.get_untracked().is_some() {
                return;
            }

            let plan = plan.to_string();
            set_failed.set(None);
            leptos::task::spawn_local(async move {
                match plan_source(plan, path).await {
                    Ok(fetched) => {
                        set_text.set(Some(fetched));
                        set_failed.set(None);
                    }
                    // Reported in the panel rather than through `state.error`,
                    // which belongs to the composer -- the same division the
                    // diff panel and the orders overlay keep.
                    Err(e) => set_failed.set(Some(e.to_string())),
                }
            });
        }
    });

    let name = Memo::new(move |_| path.get().unwrap_or_default());
    // The fetched file, but only while it is still the file being asked for.
    //
    // Switching files leaves the previous answer in the signal until the new
    // one lands, and rendering it would put one file's lines under another
    // file's name -- briefly, and wrongly. `SourceText` carries the path it is
    // for, so the check is exact rather than a guess at timing.
    let shown = Memo::new(move |_| {
        let text = text.get()?;
        (Some(text.path.as_str()) == path.get().as_deref()).then_some(text)
    });

    let lines = Memo::new(move |_| shown.get().map(|s| s.lines).unwrap_or_default());
    let tint = Memo::new(move |_| {
        shown
            .get()
            .map(|s| s.language.tint().to_string())
            .unwrap_or_default()
    });
    // A verdict is what the panel says instead of, or above, the lines: a
    // binary file, a truncated one, one that could not be read at all.
    let verdict = Memo::new(move |_| shown.get().and_then(|s| s.verdict.tell()));
    let waiting = Memo::new(move |_| shown.get().is_none() && failed.get().is_none());

    // Which lines already carry a note. A set rather than a scan per row: a
    // file is thousands of rows and a review is a handful of notes, so this is
    // built once per change instead of once per line.
    //
    // Only `Working` notes count here. A `Base` note can only have been written
    // in the diff panel, against a line of the file *before* this plan touched
    // it -- marking that number in the working copy would point at a line the
    // note is not about.
    let marked = Memo::new(move |_| {
        notes
            .get()
            .into_iter()
            .filter(|n| n.side == NoteSide::Working)
            .map(|n| n.line)
            .collect::<HashSet<u32>>()
    });

    view! {
        <div
            class="source-panel chamber-aside"
            style:width=move || format!("{}px", width.get())
            // The panel's own width, published for the note composers below.
            // The lines are `max-content` wide -- wider than the panel whenever
            // a line is long -- so a composer sized against its parent would run
            // off the right edge. See `_source.scss`.
            style:--panel-width=move || format!("{}px", width.get())
        >
            // Deliberately the diff's bar, in shape and in height: three panels
            // take this slot and the King should not have to re-learn a chrome
            // each time he switches between them.
            <div class="diff-bar">
                <span class="source-dot" style:background=move || tint.get()></span>
                <span class="diff-path" title=move || name.get()>{move || name.get()}</span>
                <button class="diff-close" title="Close" on:click=move |_| on_close.run(())>
                    "\u{00d7}"
                </button>
            </div>

            <div class="diff-stage">
                <Show when=move || waiting.get()>
                    <p class="diff-empty">"Reading\u{2026}"</p>
                </Show>

                <Show when=move || failed.get().is_some()>
                    <p class="diff-failed">
                        "This file could not be read: "{move || failed.get().unwrap_or_default()}
                    </p>
                </Show>

                <Show when=move || verdict.get().is_some()>
                    <p class="diff-verdict">{move || verdict.get().unwrap_or_default()}</p>
                </Show>

                // An empty file is a real answer and reads as one, rather than
                // as a panel that failed quietly.
                <Show when=move || {
                    shown.get().is_some() && lines.get().is_empty() && verdict.get().is_none()
                }>
                    <p class="diff-empty">"This file is empty."</p>
                </Show>

                <div class="source-lines">
                    <For
                        each=move || lines.get()
                        // Keyed on the number and the text: a refetch after an
                        // edit gives the same number different content, and a
                        // key of the number alone would leave the old line
                        // drawn under the new one's note.
                        key=|line: &SourceLine| (line.number, line.text.clone())
                        let:line
                    >
                        {
                            let number = line.number;
                            let body = line.text.clone();
                            let quote = StoredValue::new(line.text.clone());
                            let open = Memo::new(move |_| writing.get() == Some(number));
                            let noted = Memo::new(move |_| marked.get().contains(&number));

                            view! {
                                <div class="source-row" class:writing=move || open.get()>
                                    <button
                                        class="source-line"
                                        class:noted=move || noted.get()
                                        title="Write a note against this line"
                                        on:click=move |_| {
                                            // Clicking the open line's own row
                                            // closes it, so the affordance that
                                            // opened the box also puts it away.
                                            set_writing.update(|w| {
                                                *w = if *w == Some(number) {
                                                    None
                                                } else {
                                                    Some(number)
                                                };
                                            });
                                        }
                                    >
                                        <span class="source-number">{number}</span>
                                        // The mark sits in the gutter rather
                                        // than beside the text, so a noted line
                                        // is findable by scanning one column.
                                        <span class="source-mark">
                                            {move || if noted.get() {
                                                "\u{270e}"
                                            } else {
                                                ""
                                            }}
                                        </span>
                                        <span class="source-text">{body.clone()}</span>
                                    </button>

                                    <Show when=move || open.get()>
                                        <div class="source-composer">
                                            <NoteComposer
                                                quote=quote.get_value()
                                                about=format!("line {number}")
                                                on_write=Callback::new(move |note: String| {
                                                    set_writing.set(None);
                                                    on_note.run((
                                                        number,
                                                        NoteSide::Working,
                                                        quote.get_value(),
                                                        note,
                                                    ));
                                                })
                                                on_cancel=Callback::new(move |_| {
                                                    set_writing.set(None)
                                                })
                                            />
                                        </div>
                                    </Show>
                                </div>
                            }
                        }
                    </For>
                </div>
            </div>
        </div>
    }
}
