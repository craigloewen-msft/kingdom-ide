//! The plan put to the King, and everything he can do with it.
//!
//! Moved out of `conversation.rs` when annotation arrived. The card was one
//! 90-line component there and is now four modules, because it grew three jobs:
//! reading a plan, writing in its margin, and reading a revision against the
//! version that was annotated.
//!
//! | Module | What it owns |
//! |---|---|
//! | this one | the frame, the head, the two decisions, which view is showing |
//! | [`body`] | the plan as annotatable blocks |
//! | [`notes`] | writing one note, and the gathered margin |
//! | [`diff`] | a revision drawn against its predecessor |
//!
//! **Nothing here talks to the server.** `ConversationBody` owns every call in
//! this view and always has; these components reach it through callbacks. That
//! is what keeps "what happens when the King writes a note" answerable in one
//! place rather than four.

mod body;
mod diff;
mod notes;

use body::ProposalBody;
use diff::ProposalDiff;
use kingdom_core::{Proposal, ProposalNote};
use leptos::prelude::*;
use notes::NoteMargin;

/// Which reading of the plan is on screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// The plan itself, rendered, with its blocks open to annotation.
    Written,
    /// What this revision changed about the one before it.
    Changes,
}

/// A plan the model has put to the user, and the things they can do with it.
///
/// Follows the `.chat-question` idiom deliberately: that card already means
/// "this is not something to watch, it is something to do", and a proposal is
/// the same kind of thing at a larger scale. What differs is the stakes, so the
/// accepting button is the loud one and the setting-aside is quiet.
///
/// The body is **rendered markdown** -- headings, lists, tables, code fences and
/// mermaid diagrams -- through [`crate::components::Prose`], now one block at a
/// time rather than in one piece. A proposal is the one artefact the King is
/// asked to read and judge in full, so it is the place where structure earns its
/// keep most; the renderer was built for it.
#[component]
pub fn ProposalCard(
    proposal: Proposal,
    /// True while a turn is in flight. The buttons go dead rather than
    /// disappearing, so the card does not jump under the user's cursor.
    busy: Memo<bool>,
    /// The notes standing in the margin, read live off the plan -- so one
    /// written in another tab appears here, and the margin empties the moment
    /// they are sent.
    notes: Memo<Vec<ProposalNote>>,
    /// True while a send is in flight.
    sending: Signal<bool>,
    on_accept: Callback<()>,
    on_set_aside: Callback<()>,
    on_note: Callback<(usize, String, String)>,
    on_withdraw_note: Callback<String>,
    on_send_notes: Callback<()>,
) -> impl IntoView {
    // Locked the instant they decide, so a double-click cannot grant twice or
    // race a set-aside against an acceptance. The same guard `Question` uses,
    // and for the same reason -- except that here the thing being handed over
    // is the ability to change their files.
    let (decided, set_decided) = signal(false);
    let deciding = move || decided.get() || busy.get();

    // A proposal is the one thing in the chamber that is *his* to do, so it
    // takes the whole column and the log goes behind it: nothing else in view
    // to read while he is judging it. Collapsible rather than absolute, because
    // the reasoning that led to the plan is often what he wants to check before
    // deciding, and hiding it with no way back would make the card a wall.
    let (full, set_full) = signal(true);

    // What this revision changed, or `None` on a first proposal -- which is what
    // decides whether the switch is offered at all. Computed once, here, rather
    // than inside the diff view: re-diffing on every render would re-read a
    // whole document each time he opened a fold.
    let changes = StoredValue::new(proposal.changes());
    let revises = changes.get_value().is_some();

    // Changes first when there are any. That is the whole point of a revision:
    // he asked for something and wants to see whether he got it. A first
    // proposal has nothing to be read against, so it opens as written -- and the
    // switch is not drawn at all, rather than drawn dead.
    let (reading, set_reading) = signal(if revises {
        Reading::Changes
    } else {
        Reading::Written
    });

    let body = StoredValue::new(proposal.body.clone());

    let accept = move |_| {
        if deciding() {
            return;
        }
        set_decided.set(true);
        on_accept.run(());
    };
    let set_aside = move |_| {
        if deciding() {
            return;
        }
        set_decided.set(true);
        on_set_aside.run(());
    };

    view! {
        <div
            class="chat-proposal"
            class:decided=move || decided.get()
            class:full=move || full.get()
        >
            <div class="proposal-head">
                <span class="proposal-mark">"\u{1F4DC}"</span>
                <span class="proposal-who">
                    {if revises { "The court proposes again" } else { "The court proposes" }}
                </span>
                <span class="proposal-at">{clock(proposal.at)}</span>

                // Only when there is something to read against. A switch on a
                // first proposal would be a control that does nothing the first
                // time it is pressed.
                <Show when=move || revises>
                    <button
                        class="proposal-diff-toggle"
                        title="Read this against the plan it revises"
                        on:click=move |_| set_reading.update(|r| {
                            *r = match *r {
                                Reading::Changes => Reading::Written,
                                Reading::Written => Reading::Changes,
                            };
                        })
                    >
                        {move || match reading.get() {
                            Reading::Changes => "Show as written",
                            Reading::Written => "Show what changed",
                        }}
                    </button>
                </Show>

                <button
                    class="proposal-expand"
                    title="Show the conversation behind this proposal"
                    on:click=move |_| set_full.update(|f| *f = !*f)
                >
                    {move || if full.get() { "Show conversation" } else { "Read in full" }}
                </button>
            </div>

            <p class="proposal-title">{proposal.title}</p>

            // Annotation is offered on the written view only. Marking up a diff
            // would mean deciding what a note against a *removed* line means,
            // and it does not mean anything.
            <Show
                when=move || reading.get() == Reading::Written
                fallback=move || {
                    view! {
                        <ProposalDiff lines=changes.get_value().unwrap_or_default()/>
                    }
                }
            >
                <ProposalBody body=body.get_value() on_note=on_note/>
            </Show>

            <NoteMargin
                notes=notes
                sending=sending
                on_withdraw=on_withdraw_note
                on_send=on_send_notes
            />

            <div class="proposal-actions">
                <button
                    class="proposal-accept"
                    title="Let the court carry out this plan"
                    disabled=deciding
                    on:click=accept
                >
                    {move || if decided.get() { "Starting\u{2026}" } else { "Start with this" }}
                </button>
                <button
                    class="proposal-aside"
                    title="Put this plan aside and say what you want instead"
                    disabled=deciding
                    on:click=set_aside
                >
                    "Set aside"
                </button>
                <span class="proposal-hint">
                    // Names the third and fourth options, neither of which is
                    // one of the two buttons: writing in the margin, and typing.
                    "Or write \u{270e} against any part of it, or say what you would change below."
                </span>
            </div>
        </div>
    }
}

/// A timestamp as the King's own local time.
///
/// Borrowed from the conversation rather than reimplemented: a proposal's clock
/// and a message's clock must read identically, and two spellings of "turn UTC
/// milliseconds into a wall clock" is how they come to differ by an hour on one
/// side of a daylight-saving change.
fn clock(at: Option<kingdom_core::Timestamp>) -> String {
    crate::components::conversation::clock(at)
}
