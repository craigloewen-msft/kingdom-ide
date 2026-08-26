//! The source panel: one file, as it stands, with a note against any line --
//! or, in the other mode, open for the King to edit.
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
//! # The two modes
//!
//! **Notes** is the panel's original self: every line is a button that opens a
//! composer, and what is written lands in the review the King sends to the
//! court. **Edit** replaces the lines with one textarea and lets him change the
//! file himself, save it, or delete it.
//!
//! A mode of one panel rather than a fourth [`Aside`], because it is the same
//! file in the same slot answering the same question -- and because a King who
//! spots a typo while reading should not have to close what he is looking at to
//! fix it.
//!
//! Two things about Edit mode are load-bearing and easy to undo by accident:
//!
//! - **The buffer is fetched, not rebuilt from the lines above.** Those lines
//!   come from `str::lines()`, which drops a trailing newline and eats a `\r`;
//!   joining them back would rewrite every CRLF file in the project as LF. See
//!   [`kingdom_core::FileText`], which exists to say so.
//! - **Unsaved text survives leaving the file.** Dirty buffers are held in a map
//!   keyed by path for the chamber's lifetime, so glancing at another file and
//!   coming back does not silently discard what was typed. There is no modal
//!   asking him to confirm, because there is nothing to lose.
//!
//! # Why it holds still while a note is being written, or an edit made
//!
//! The panel refetches when the court touches the file, so an open file follows
//! the work. That is right while the King is only reading and wrong the moment
//! he is typing against line 34: the lines would shift under him and the note
//! would land on something he never read. So a composer being open suspends the
//! refetch, and the update is taken when it closes. The `quote` carried on every
//! note is the second half of that guarantee -- see
//! [`kingdom_core::ReviewNote::quote`].
//!
//! Edit mode suspends it for the same reason and more sharply -- text moving
//! under a cursor is worse than under a quote -- and has a second guarantee of
//! its own: the [`kingdom_core::FileStamp`] taken at the read, which is what
//! makes a save refuse rather than overwrite work the King never saw.

use crate::api::{plan_file_text, plan_source};
use crate::components::note_composer::NoteComposer;
use kingdom_core::{
    DiffVerdict, FileStamp, FileText, NoteSide, PlanId, ReviewNote, SourceLine, SourceText,
};
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};

/// What the King is doing with the file in front of him.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Reading it, and writing notes for the court against its lines.
    Notes,
    /// Changing it himself.
    Edit,
}

/// One file of the plan's workspace, line by line -- or open for editing.
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
    /// Saves what the King typed: the buffer, and the stamp of the file it was
    /// opened from. The panel is presentational, as its siblings are -- the
    /// chamber owns every call, because the chamber is what holds the plan.
    ///
    /// Answers back through `saved` rather than by returning, because a server
    /// call is not something a `Callback` can hand back.
    on_save: Callback<(String, FileStamp)>,
    /// Deletes the file being shown, with the same stamp for the same reason.
    on_delete: Callback<FileStamp>,
    /// The stamp of what was last written, set by the chamber when a save
    /// lands. The panel takes it as the buffer's new stamp -- otherwise the
    /// second save of a session would be refused as stale by the first.
    saved: RwSignal<Option<FileStamp>>,
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

    // -- Editing ------------------------------------------------------------

    let mode = RwSignal::new(Mode::Notes);
    // The file as fetched for editing: the bytes, and the stamp they were read
    // at. `None` until Edit is pressed, so a King who only ever reads pays for
    // no second request.
    let opened = RwSignal::new(None::<FileText>);
    let (fetching_edit, set_fetching_edit) = signal(false);
    // What is in the textarea now.
    let buffer = RwSignal::new(String::new());
    // Unsaved buffers, keyed by path, for the chamber's lifetime. This is what
    // lets the King glance at another file mid-edit without losing his typing --
    // see the module note. Not persisted anywhere: it is worth surviving a
    // click, not a reload, and a durable draft of a file is a different feature
    // with different questions (whose is it? when does it expire?).
    let stashed = RwSignal::new(HashMap::<String, String>::new());
    // Whether the delete button has been pressed once. Two steps rather than a
    // modal -- the `done-picker` pattern -- because unlike finishing a plan this
    // is the one act in the panel that an untracked file does not come back
    // from.
    let (confirming_delete, set_confirming_delete) = signal(false);

    let editing = Memo::new(move |_| mode.get() == Mode::Edit);
    let dirty = Memo::new(move |_| {
        opened
            .get()
            .map(|o| o.text != buffer.get())
            .unwrap_or(false)
    });

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

            // ...or the King is editing, where the same reasoning is sharper
            // still: the lines behind the textarea are not what he is typing
            // into, but refetching would also churn the panel around him for no
            // gain. What protects the *save* is the stamp, not this.
            if editing.get_untracked() {
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

    // Changing file stashes whatever was being typed and puts the panel back in
    // Notes -- the mode is a property of *reading a file*, not of the panel, so
    // clicking a second file should not drop the King into an editor he did not
    // ask for. Coming back restores the stash below.
    //
    // Runs on the path alone: `on_cleanup` would be the tidier hook, but the
    // panel is not remounted between files (see `path`'s own note), so there is
    // no cleanup to hang it on.
    let previous = StoredValue::new(None::<String>);
    Effect::new(move |_| {
        let now = path.get();
        let before = previous.get_value();
        if before == now {
            return;
        }
        previous.set_value(now);

        if let Some(before) = before {
            // Only a *dirty* buffer is worth keeping. Stashing a clean one would
            // mean a file edited and saved reopens from the stash rather than
            // from disk, and would go stale the moment the court touched it.
            if dirty.get_untracked() {
                let text = buffer.get_untracked();
                stashed.update(|s| {
                    s.insert(before, text);
                });
            }
        }

        mode.set(Mode::Notes);
        opened.set(None);
        buffer.set(String::new());
        set_confirming_delete.set(false);
        set_writing.set(None);
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

    // Whether Edit may be pressed at all, and why not.
    //
    // Read off the *reading* verdict, which the panel already has, so the button
    // can be disabled before anything is fetched. The edit fetch is still
    // authoritative -- a file that turns out to be unreadable reports it and
    // stays in Notes -- because between the two reads the court may have
    // replaced the file with something else entirely.
    let cannot_edit = Memo::new(move |_| {
        let text = shown.get()?;
        match text.verdict {
            DiffVerdict::Shown => None,
            DiffVerdict::Binary => Some("This file is not text, so there is nothing to edit."),
            DiffVerdict::TooLarge => Some("This file is too large to edit here."),
            // A file cut short renders honestly and must not be *edited*: the
            // buffer would be missing its tail, and saving it would delete that
            // tail from disk. `edit::text` refuses it too; this is the button
            // saying so before it is pressed.
            DiffVerdict::Truncated(_) => {
                Some("This file is too long to edit here \u{2014} only part of it is shown.")
            }
            DiffVerdict::Unreadable(_) => Some(
                "This file could not be read, so it cannot be \
                                                edited.",
            ),
        }
    });

    let editor_ref = NodeRef::<leptos::html::Textarea>::new();

    // Entering Edit mode: fetch the bytes, or restore what was being typed.
    let begin_edit = {
        let plan = plan.clone();
        move || {
            let Some(path) = path.get_untracked() else {
                return;
            };
            if fetching_edit.get_untracked() {
                return;
            }
            set_fetching_edit.set(true);
            set_writing.set(None);
            set_failed.set(None);

            let plan = plan.to_string();
            leptos::task::spawn_local(async move {
                let fetched = plan_file_text(plan, path.clone()).await;
                set_fetching_edit.set(false);

                match fetched {
                    Ok(file) => {
                        if file.verdict != DiffVerdict::Shown {
                            // Named rather than opened empty: a blank buffer
                            // saved back over the original is the worst outcome
                            // available here.
                            set_failed.set(Some(
                                file.verdict
                                    .tell()
                                    .unwrap_or_else(|| "This file cannot be edited.".into()),
                            ));
                            return;
                        }

                        // Whatever was being typed when he last left this file
                        // wins over what is on disk -- that is the whole point
                        // of the stash. The stamp is still the *fetched* one, so
                        // a save of restored text is checked against the file as
                        // it stands now rather than as it stood then.
                        let restored = stashed.get_untracked().get(&path).cloned();
                        buffer.set(restored.unwrap_or_else(|| file.text.clone()));
                        opened.set(Some(file));
                        mode.set(Mode::Edit);
                    }
                    Err(e) => set_failed.set(Some(e.to_string())),
                }
            });
        }
    };

    // Leaving it. A dirty buffer is stashed rather than dropped, exactly as
    // changing file does, so the two ways out behave the same.
    let leave_edit = move || {
        if dirty.get_untracked() {
            if let Some(path) = path.get_untracked() {
                let text = buffer.get_untracked();
                stashed.update(|s| {
                    s.insert(path, text);
                });
            }
        }
        mode.set(Mode::Notes);
        set_confirming_delete.set(false);
    };

    let save = move || {
        let Some(file) = opened.get_untracked() else {
            return;
        };
        if !dirty.get_untracked() {
            return;
        }
        on_save.run((buffer.get_untracked(), file.stamp));
    };

    // A save that landed. The chamber reports the new stamp here, and the panel
    // takes it as the buffer's own -- without this the *second* save of a
    // session would be checked against the stamp of the file before the first,
    // and refused as stale by the King's own edit.
    Effect::new(move |_| {
        let Some(stamp) = saved.get() else {
            return;
        };
        saved.set(None);

        let text = buffer.get_untracked();
        opened.update(|o| {
            if let Some(file) = o {
                file.text = text.clone();
                file.stamp = stamp;
            }
        });
        // Saved text is no longer unsaved text.
        if let Some(path) = path.get_untracked() {
            stashed.update(|s| {
                s.remove(&path);
            });
        }
    });

    // Focus the editor when it appears, as the note composer does: the King
    // pressed Edit to type, and making him click again in the box he summoned is
    // a step that says nothing.
    Effect::new(move |_| {
        if editing.get() {
            if let Some(el) = editor_ref.get() {
                let _ = el.focus();
            }
        }
    });

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
            class:editing=move || editing.get()
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

                // The mode switch. Two segments rather than one toggle button,
                // so the state the King is *not* in is named as well as the one
                // he is -- a lone "Edit" button leaves "what am I looking at
                // now?" to be inferred from whether the lines are clickable.
                <div class="source-mode" role="group">
                    <button
                        class="mode-seg"
                        class:on=move || !editing.get()
                        title="Read the file and write notes to the court against its lines"
                        on:click=move |_| {
                            if editing.get_untracked() {
                                leave_edit();
                            }
                        }
                    >
                        "Notes"
                    </button>
                    <button
                        class="mode-seg"
                        class:on=move || editing.get()
                        disabled=move || cannot_edit.get().is_some() || fetching_edit.get()
                        title=move || {
                            cannot_edit
                                .get()
                                .unwrap_or("Change this file yourself")
                                .to_string()
                        }
                        on:click=move |_| {
                            if !editing.get_untracked() {
                                begin_edit();
                            }
                        }
                    >
                        {move || if fetching_edit.get() { "Opening\u{2026}" } else { "Edit" }}
                    </button>
                </div>

                <button class="diff-close" title="Close" on:click=move |_| on_close.run(())>
                    "\u{00d7}"
                </button>
            </div>

            // The editor's own strip: what can be done to the file, and the one
            // dangerous thing, kept apart from it at the far end.
            <Show when=move || editing.get()>
                <div class="source-actions">
                    <button
                        class="source-save"
                        disabled=move || !dirty.get()
                        title="Save this file (Ctrl+S)"
                        on:click=move |_| save()
                    >
                        {move || if dirty.get() { "Save" } else { "Saved" }}
                    </button>
                    <button
                        class="source-revert"
                        disabled=move || !dirty.get()
                        title="Throw away what you have typed and go back to the file on disk"
                        on:click=move |_| {
                            if let Some(file) = opened.get_untracked() {
                                buffer.set(file.text.clone());
                            }
                            if let Some(path) = path.get_untracked() {
                                stashed.update(|s| { s.remove(&path); });
                            }
                        }
                    >
                        "Revert"
                    </button>

                    <span class="source-dirty" class:on=move || dirty.get()>
                        {move || if dirty.get() { "unsaved changes" } else { "" }}
                    </span>

                    // Two steps, and no modal: the `done-picker`'s reasoning,
                    // except that this one *is* worth a second press, because an
                    // untracked file deleted here is gone for good.
                    <Show
                        when=move || confirming_delete.get()
                        fallback=move || view! {
                            <button
                                class="source-delete"
                                title="Delete this file from the plan's workspace"
                                on:click=move |_| set_confirming_delete.set(true)
                            >
                                "Delete file"
                            </button>
                        }
                    >
                        <span class="source-confirm">
                            <span class="confirm-ask">"Delete it?"</span>
                            <button
                                class="confirm-yes"
                                on:click=move |_| {
                                    set_confirming_delete.set(false);
                                    if let Some(file) = opened.get_untracked() {
                                        on_delete.run(file.stamp);
                                    }
                                }
                            >
                                "Delete"
                            </button>
                            <button
                                class="confirm-no"
                                on:click=move |_| set_confirming_delete.set(false)
                            >
                                "Keep"
                            </button>
                        </span>
                    </Show>
                </div>
            </Show>

            <div class="diff-stage">
                <Show when=move || waiting.get() && !editing.get()>
                    <p class="diff-empty">"Reading\u{2026}"</p>
                </Show>

                <Show when=move || failed.get().is_some()>
                    <p class="diff-failed">
                        "This file could not be read: "{move || failed.get().unwrap_or_default()}
                    </p>
                </Show>

                // -- Editing -------------------------------------------------
                //
                // One textarea and no line numbers. A gutter synced to a
                // textarea's scroll position is a hack, and this is a panel for
                // a quick fix rather than a second editor -- the line numbers
                // are one press away in Notes.
                <Show when=move || editing.get()>
                    <textarea
                        class="source-editor"
                        node_ref=editor_ref
                        spellcheck="false"
                        prop:value=move || buffer.get()
                        on:input=move |ev| buffer.set(event_target_value(&ev))
                        on:keydown=move |ev| {
                            // Ctrl/Cmd+S saves, because every editor the King
                            // has ever used does and his hands will do it
                            // whether or not this listens.
                            if ev.key() == "s" && (ev.ctrl_key() || ev.meta_key()) {
                                ev.prevent_default();
                                save();
                            } else if ev.key() == "Tab" {
                                // Tab types a tab. Moving focus out of a code
                                // editor is right for a form and wrong here --
                                // Escape is the way out, below.
                                ev.prevent_default();
                                if let Some(el) = editor_ref.get() {
                                    insert_tab(&el, buffer);
                                }
                            } else if ev.key() == "Escape" {
                                ev.prevent_default();
                                leave_edit();
                            }
                        }
                    />
                </Show>

                // -- Reading -------------------------------------------------
                <Show when=move || !editing.get()>
                    <Show when=move || verdict.get().is_some()>
                        <p class="diff-verdict">{move || verdict.get().unwrap_or_default()}</p>
                    </Show>

                    // An empty file is a real answer and reads as one, rather
                    // than as a panel that failed quietly.
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
                </Show>
            </div>
        </div>
    }
}

/// Types a tab at the cursor, keeping the cursor after it.
///
/// The browser's own Tab moves focus, which is right for a form and wrong for a
/// code editor, so the keydown handler prevents it and calls this. The DOM is
/// written **and** the signal is set, because `prop:value` is bound to the
/// signal: setting only the element would be overwritten on the next render, and
/// setting only the signal would put the cursor back at the end of the file on
/// every tab.
///
/// One tab character rather than spaces: what a project indents with is the
/// project's business, and this box is for a quick fix rather than for writing a
/// file from scratch. `autogrow` is deliberately *not* used here -- the editor
/// fills the stage and scrolls, where a composer grows to fit.
fn insert_tab(el: &leptos::web_sys::HtmlTextAreaElement, buffer: RwSignal<String>) {
    let value = el.value();
    let start = el.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = el.selection_end().ok().flatten().unwrap_or(0) as usize;

    // The DOM counts UTF-16 code units and Rust counts bytes, so a file with any
    // non-ASCII in it above the cursor would splice at the wrong offset. Walking
    // the char indices converts between the two exactly; a file long enough for
    // this to be slow is one `edit::text` has already refused.
    let byte_at = |units: usize| {
        let mut seen = 0usize;
        for (byte, ch) in value.char_indices() {
            if seen >= units {
                return byte;
            }
            seen += ch.len_utf16();
        }
        value.len()
    };

    let (from, to) = (byte_at(start), byte_at(end.max(start)));
    let mut next = String::with_capacity(value.len() + 1);
    next.push_str(&value[..from]);
    next.push('\t');
    next.push_str(&value[to..]);

    el.set_value(&next);
    let cursor = (start + 1) as u32;
    let _ = el.set_selection_start(Some(cursor));
    let _ = el.set_selection_end(Some(cursor));
    buffer.set(next);
}
