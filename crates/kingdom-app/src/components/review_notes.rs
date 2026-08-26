//! The King's review of the code, gathered, and the one way to send it.
//!
//! The sibling of `proposal::notes::NoteMargin` and drawn in the same place --
//! the chamber column, above the composer -- for its reason: this is a decision
//! still to make, and it belongs where his attention already is. Deliberately
//! **not** inside the panel: a review spans several files and the panel shows
//! one, so a margin living there would hide most of what he has written the
//! moment he opened the next file.
//!
//! # Why this is separate from the proposal's margin
//!
//! Both can stand at once, and they are kept apart on purpose: a note in a
//! plan's margin asks the court to *revise a document and propose again*, and a
//! note against a line asks it to *change code*. Merged, they would send one
//! decree that means two things. In practice they rarely coincide -- a plan
//! still proposing has changed no files to write against -- and where they do,
//! the heads and the buttons name which is which.
//!
//! Talks to nothing, exactly as the proposal's card does not: every call in the
//! chamber is owned by `ConversationBody`, and this reaches it through
//! callbacks.

use kingdom_core::{NoteSide, ReviewNote};
use leptos::prelude::*;

/// The notes standing against this plan's code, and the one way to send them.
///
/// Draws nothing when the review is empty. An empty list with a dead "send"
/// button beside it would be a permanent invitation to press something that
/// does nothing -- the objection the Stop button's `busy` guard already answers.
#[component]
pub fn ReviewMargin(
    /// The notes, read live off the plan so one written in another tab appears
    /// here, and so the margin empties the moment they are sent.
    notes: Memo<Vec<ReviewNote>>,
    /// True while a send is in flight. Sending is not refused during a *turn* --
    /// the review queues and is heard at the next round boundary, exactly as
    /// words typed into the composer are.
    sending: Signal<bool>,
    /// Opens a file, so a note can be read back in its place. The King writes a
    /// review over several minutes and needs to get back to what he was
    /// objecting to.
    on_open: Callback<String>,
    on_withdraw: Callback<String>,
    on_send: Callback<()>,
) -> impl IntoView {
    // Grouped by file, in the order he first wrote against them -- the order he
    // was reading in, and the same grouping `api::file_notes_as_decree` sends.
    // A margin ordered differently from the decree would have him check his
    // review against a message that reads in another order.
    let by_file = Memo::new(move |_| {
        let notes = notes.get();
        let mut files: Vec<String> = Vec::new();
        for note in &notes {
            if !files.contains(&note.path) {
                files.push(note.path.clone());
            }
        }
        files
            .into_iter()
            .map(|path| {
                let mut on_this: Vec<ReviewNote> =
                    notes.iter().filter(|n| n.path == path).cloned().collect();
                on_this.sort_by_key(|n| n.line);
                (path, on_this)
            })
            .collect::<Vec<_>>()
    });

    // "4 notes across 2 files" rather than a bare count: how much is being sent
    // and how far it reaches are both part of the decision to send it.
    let head = Memo::new(move |_| {
        let notes = notes.get().len();
        let files = by_file.get().len();
        let n = match notes {
            1 => "1 note".to_string(),
            n => format!("{n} notes"),
        };
        match files {
            // One file needs no "across", and saying it would read as a
            // reminder that there could have been more.
            0 | 1 => format!("{n} in this review"),
            f => format!("{n} across {f} files"),
        }
    });

    view! {
        <Show when=move || !notes.get().is_empty()>
            <div class="review-margin">
                <div class="review-margin-head">{move || head.get()}</div>

                <div class="review-margin-list">
                    <For
                        each=move || by_file.get()
                        key=|(path, notes): &(String, Vec<ReviewNote>)| {
                            // Keyed on the ids as well as the path: withdrawing
                            // one note leaves the file's group the same length
                            // it was only if the key ignores what is in it.
                            (
                                path.clone(),
                                notes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
                            )
                        }
                        let:group
                    >
                        {
                            let (path, on_this) = group;
                            let open = {
                                let path = path.clone();
                                move |_| on_open.run(path.clone())
                            };
                            // Dimmed folder, bright name, as the review drawer
                            // does it: in a narrow column the name is what is
                            // being looked for.
                            let (folder, name) = match path.rsplit_once('/') {
                                Some((folder, name)) => (format!("{folder}/"), name.to_string()),
                                None => (String::new(), path.clone()),
                            };

                            view! {
                                <div class="review-margin-file">
                                    <button
                                        class="review-margin-path"
                                        title=format!("Open {path}")
                                        on:click=open
                                    >
                                        <Show when={
                                            let folder = folder.clone();
                                            move || !folder.is_empty()
                                        }>
                                            <span class="review-margin-folder">
                                                {folder.clone()}
                                            </span>
                                        </Show>
                                        <span class="review-margin-name">{name.clone()}</span>
                                    </button>

                                    <ul class="note-list">
                                        <For
                                            each=move || on_this.clone()
                                            key=|note: &ReviewNote| note.id.clone()
                                            let:note
                                        >
                                            {
                                                let id = note.id.clone();
                                                // The old side of a diff says so.
                                                // A bare number would point at
                                                // whatever now occupies that
                                                // position in the file.
                                                let at = match note.side {
                                                    NoteSide::Working => {
                                                        format!("{}", note.line)
                                                    }
                                                    NoteSide::Base => {
                                                        format!("{} (before)", note.line)
                                                    }
                                                };
                                                view! {
                                                    <li class="note-item">
                                                        <span class="note-line">{at}</span>
                                                        <span class="note-quote">
                                                            {note.quote.clone()}
                                                        </span>
                                                        <span class="note-body">
                                                            {note.body.clone()}
                                                        </span>
                                                        <button
                                                            class="note-withdraw"
                                                            title="Take this note back"
                                                            on:click=move |_| {
                                                                on_withdraw.run(id.clone())
                                                            }
                                                        >
                                                            "\u{00d7}"
                                                        </button>
                                                    </li>
                                                }
                                            }
                                        </For>
                                    </ul>
                                </div>
                            }
                        }
                    </For>
                </div>

                <button
                    class="note-send"
                    title="Send this review for the court to act on"
                    disabled=move || sending.get()
                    on:click=move |_| on_send.run(())
                >
                    {move || if sending.get() {
                        "Sending\u{2026}".to_string()
                    } else {
                        match notes.get().len() {
                            1 => "Send this note".to_string(),
                            n => format!("Send these {n} notes"),
                        }
                    }}
                </button>
            </div>
        </Show>
    }
}
