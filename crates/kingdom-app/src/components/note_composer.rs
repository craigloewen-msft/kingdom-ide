//! The box the King writes one note in.
//!
//! Shared by everything he can annotate: a block of a proposal
//! ([`super::proposal::notes`]) and a line of a file (the source view and the
//! diff). One component rather than three, so a note written in the margin of a
//! plan and a note written against line 34 behave identically -- Enter writes,
//! Shift+Enter makes a line, Escape abandons, and the box grows with what is
//! typed into it. Three copies would be three places for those four rules to
//! drift apart.
//!
//! It talks to nothing. The parent owns sending, because the parent is what
//! knows where the note is anchored -- a line, or a markdown block.

use crate::components::prompt_bar::autogrow;
use leptos::prelude::*;

/// Writing one note, against whatever the caller has anchored it to.
///
/// Opens **in place**, under the thing it is about. Deliberately not a dialog:
/// the text being objected to has to stay on screen while the objection is
/// written, or the King is composing from memory about something he can no
/// longer see -- the same reasoning `Question` is inline rather than modal for.
#[component]
pub fn NoteComposer(
    /// What the note is against, for the placeholder. The annotated text's own
    /// words are the clearest possible label for where the note will land.
    #[prop(into)]
    quote: String,
    /// How to name the target in the placeholder, when the quote alone would
    /// not say. A line note passes `"line 34"`; a block note passes nothing,
    /// because a paragraph is identified perfectly well by its opening words.
    #[prop(into, optional)]
    about: Option<String>,
    /// The note, as written. The parent owns sending it, because the parent is
    /// what knows where it belongs.
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

    let placeholder = match about {
        Some(about) => format!("What would you change about {about}?"),
        None => {
            let opening: String = quote.chars().take(40).collect();
            format!("What would you change about \u{201c}{opening}\u{2026}\u{201d}?")
        }
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
