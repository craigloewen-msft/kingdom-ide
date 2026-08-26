//! The plan as the King reads it, split into parts he can write against.
//!
//! Each block is rendered markdown, exactly as the whole body was before this
//! existed -- headings, lists, tables, fences and mermaid diagrams all still
//! draw, because each block goes through the same [`Prose`] the card used to
//! pass the entire document to.
//!
//! The split itself is `kingdom_core::proposal::blocks`, not a rule invented
//! here. The server quotes what the King annotated using the same function, and
//! two answers to "where does a block begin" is precisely how his note and the
//! court's quote come to describe different paragraphs.

use crate::components::note_composer::NoteComposer;
use crate::components::Prose;
use kingdom_core::proposal::Block;
use leptos::prelude::*;

/// The proposal, block by block, each one annotatable.
#[component]
pub fn ProposalBody(
    /// The plan's markdown. Split here rather than by the caller so the card
    /// hands down a body and gets back a document.
    #[prop(into)]
    body: String,
    /// Writes a note: the line the block starts on, the block's text, and what
    /// the King wrote. All three travel because the server needs the quote and
    /// the view needs the line -- see `ProposalNote`.
    on_note: Callback<(usize, String, String)>,
) -> impl IntoView {
    let blocks = kingdom_core::proposal::blocks(&body);

    // Which block has a composer open, by its line. One at a time: several open
    // boxes is complexity nothing has asked for, and the King writing two notes
    // at once is not a thing that happens.
    let (writing, set_writing) = signal(None::<usize>);

    // A body with no blocks at all cannot happen through `propose_plan`, which
    // refuses an empty draft -- but a record from disk is not bound by that, and
    // an empty document must read as an empty document rather than as a plan
    // with nothing wrong with it.
    if blocks.is_empty() {
        return view! {
            <div class="proposal-body">
                <Prose text=body class=""/>
            </div>
        }
        .into_any();
    }

    view! {
        <div class="proposal-body annotatable">
            {blocks
                .into_iter()
                .map(|block| {
                    view! {
                        <ProposalBlock
                            block=block
                            writing=writing
                            set_writing=set_writing
                            on_note=on_note
                        />
                    }
                })
                .collect_view()}
        </div>
    }
    .into_any()
}

/// One part of the plan, and the way to object to it.
///
/// The affordance is a button in the gutter rather than a click on the prose
/// itself. Making the text clickable would take selecting a sentence to copy it
/// -- which is what the King does most while reading a plan -- and turn it into
/// opening a box he did not ask for.
#[component]
fn ProposalBlock(
    block: Block,
    writing: ReadSignal<Option<usize>>,
    set_writing: WriteSignal<Option<usize>>,
    on_note: Callback<(usize, String, String)>,
) -> impl IntoView {
    let line = block.line;
    // Held rather than captured: the quote is needed by the composer's
    // placeholder and again by the callback, and a captured `String` makes the
    // closure `FnOnce`.
    let quote = StoredValue::new(block.text.clone());
    let open = move || writing.get() == Some(line);

    view! {
        <div class="proposal-block" class:writing=open>
            <button
                class="block-annotate"
                title="Write a note against this"
                on:click=move |_| {
                    // Clicking the open block's own button closes it, so the
                    // affordance that opened the box also puts it away.
                    set_writing.update(|w| {
                        *w = if *w == Some(line) { None } else { Some(line) };
                    });
                }
            >
                "\u{270e}"
            </button>

            <Prose text=block.text class="block-prose"/>

            <Show when=open>
                <NoteComposer
                    quote=quote.get_value()
                    on_write=Callback::new(move |note: String| {
                        set_writing.set(None);
                        on_note.run((line, quote.get_value(), note));
                    })
                    on_cancel=Callback::new(move |_| set_writing.set(None))
                />
            </Show>
        </div>
    }
}
