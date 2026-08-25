//! Writing in the margin of a plan, and sending what was written.
//!
//! Two components that between them own everything about a note except where it
//! is anchored: [`NoteComposer`] is the box that opens against a block, and
//! [`NoteMargin`] is the gathered notes with the one button that puts them to
//! the court.
//!
//! Neither talks to the server. Every call in this view is owned by
//! `ConversationBody`, as it already was before annotation existed, and these
//! reach it through callbacks -- so a note written here and a note withdrawn
//! three components away take the same path and land in the same place.

use crate::components::prompt_bar::autogrow;
use kingdom_core::ProposalNote;
use leptos::prelude::*;

/// Writing one note, against one block.
///
/// Opens in place under the block it is about. Deliberately not a dialog: the
/// text being objected to has to stay on screen while the objection is written,
/// or the King is composing from memory about something he can no longer see --
/// the same reasoning `Question` is inline rather than modal for.
#[component]
pub fn NoteComposer(
    /// What the note is against, for the placeholder. The block's own words are
    /// the clearest possible label for where the note will land.
    #[prop(into)]
    quote: String,
    /// The note, as written. The parent owns sending it, because the parent is
    /// what knows the line it belongs to.
    on_write: Callback<String>,
    on_cancel: Callback<()>,
) -> impl IntoView {
    let (text, set_text) = signal(String::new());
    let box_ref = NodeRef::<leptos::html::Textarea>::new();

    // Grows with the note, as every other composer in the chamber does.
    Effect::new(move |_| {
        text.track();
        if let Some(el) = box_ref.get() {
            autogrow(&el);
        }
    });

    // Focused on open, because the King clicked to write: making him click a
    // second time in the box he just summoned is a step that says nothing.
    Effect::new(move |_| {
        if let Some(el) = box_ref.get() {
            let _ = el.focus();
        }
    });

    let write = move || {
        let note = text.get().trim().to_string();
        if note.is_empty() {
            return;
        }
        set_text.set(String::new());
        on_write.run(note);
    };

    let placeholder = {
        let opening: String = quote.chars().take(40).collect();
        format!("What would you change about \u{201c}{opening}\u{2026}\u{201d}?")
    };

    view! {
        <div class="note-composer">
            <textarea
                class="note-input"
                node_ref=box_ref
                rows="2"
                placeholder=placeholder
                prop:value=move || text.get()
                on:input=move |ev| set_text.set(event_target_value(&ev))
                on:keydown=move |ev| {
                    // Enter writes and Shift+Enter makes a line, as the chamber's
                    // composer does. Escape closes -- a note begun by accident
                    // must be abandonable without reaching for the mouse.
                    if ev.key() == "Enter" && !ev.shift_key() {
                        ev.prevent_default();
                        write();
                    } else if ev.key() == "Escape" {
                        ev.prevent_default();
                        on_cancel.run(());
                    }
                }
            />
            <div class="note-composer-actions">
                <button
                    class="note-write"
                    disabled=move || text.get().trim().is_empty()
                    on:click=move |_| write()
                >
                    "Add note"
                </button>
                <button class="note-cancel" on:click=move |_| on_cancel.run(())>
                    "Cancel"
                </button>
            </div>
        </div>
    }
}

/// The notes standing against this proposal, and the one way to send them.
///
/// Draws nothing at all when the margin is empty. An empty list with a dead
/// "send" button beside it would be a permanent invitation to press something
/// that does nothing -- the objection the Stop button's `busy` guard already
/// answers.
#[component]
pub fn NoteMargin(
    /// The notes, read live off the plan so one written in another tab appears
    /// here.
    notes: Memo<Vec<ProposalNote>>,
    /// True while a send is in flight. Sending is not refused during a *turn* --
    /// the notes queue and are heard at the next round boundary, exactly as
    /// words typed into the composer are.
    sending: Signal<bool>,
    on_withdraw: Callback<String>,
    on_send: Callback<()>,
) -> impl IntoView {
    view! {
        <Show when=move || !notes.get().is_empty()>
            <div class="note-margin">
                <div class="note-margin-head">
                    {move || {
                        match notes.get().len() {
                            1 => "1 note in the margin".to_string(),
                            n => format!("{n} notes in the margin"),
                        }
                    }}
                </div>

                <ul class="note-list">
                    <For
                        each=move || notes.get()
                        key=|note| note.id.clone()
                        let:note
                    >
                        {
                            let id = note.id.clone();
                            view! {
                                <li class="note-item">
                                    <span class="note-quote">{note.quote.clone()}</span>
                                    <span class="note-body">{note.body.clone()}</span>
                                    <button
                                        class="note-withdraw"
                                        title="Take this note back"
                                        on:click=move |_| on_withdraw.run(id.clone())
                                    >
                                        "\u{00d7}"
                                    </button>
                                </li>
                            }
                        }
                    </For>
                </ul>

                <button
                    class="note-send"
                    title="Send these notes back for the court to answer"
                    disabled=move || sending.get()
                    on:click=move |_| on_send.run(())
                >
                    {move || if sending.get() {
                        "Sending\u{2026}".to_string()
                    } else {
                        match notes.get().len() {
                            1 => "Send this note back".to_string(),
                            n => format!("Send these {n} notes back"),
                        }
                    }}
                </button>
            </div>
        </Show>
    }
}
