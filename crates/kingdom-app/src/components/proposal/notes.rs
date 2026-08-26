//! The gathered notes on a plan, and the one button that sends them.
//!
//! What is left here after [`crate::components::note_composer`] took the box
//! itself: [`NoteMargin`] is the notes standing against this proposal, with the
//! one way to put them to the court. The composer moved out when line review
//! arrived and needed the same box against a line of code -- one component, so
//! the two kinds of note behave identically.
//!
//! Neither talks to the server. Every call in this view is owned by
//! `ConversationBody`, as it already was before annotation existed, and these
//! reach it through callbacks -- so a note written here and a note withdrawn
//! three components away take the same path and land in the same place.

use kingdom_core::ProposalNote;
use leptos::prelude::*;

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
