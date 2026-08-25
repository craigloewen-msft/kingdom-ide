//! A revised plan drawn against the one it revises.
//!
//! Source lines with gutters, not rendered markdown, and that is the one
//! deliberate concession in this path. Rendered markdown has no lines to mark:
//! diffing the output would mean marking DOM nodes, and a heading whose wording
//! moved by a word is an entirely new node -- so the marking would say *that*
//! something changed without ever saying *what*.
//!
//! The King reads the diff to find what moved and the plan itself to judge it,
//! so the card offers both and this is only ever one half of that pair.

use kingdom_core::proposal::{Change, DiffLine, Hunk};
use leptos::prelude::*;

/// The changes between a proposal and its predecessor.
#[component]
pub fn ProposalDiff(
    /// The diff, already computed. Passed in rather than computed here so the
    /// card decides once whether there is anything to draw -- a component that
    /// re-diffs on every render would recompute a forty-line document each time
    /// the King opened a fold.
    lines: Vec<DiffLine>,
) -> impl IntoView {
    // An unchanged revision is said in a sentence. Drawing forty identical
    // lines would leave him hunting for a difference that is not there, which
    // is the one outcome a diff exists to prevent.
    if kingdom_core::proposal::unchanged(&lines) {
        return view! {
            <div class="proposal-diff">
                <p class="diff-unchanged">
                    "The court proposed this again unchanged."
                </p>
            </div>
        }
        .into_any();
    }

    let grouped = kingdom_core::proposal::hunks(&lines);

    view! {
        <div class="proposal-diff">
            {grouped
                .into_iter()
                .map(|hunk| match hunk {
                    Hunk::Changed(lines) => view! { <DiffLines lines=lines/> }.into_any(),
                    Hunk::Unchanged(lines) => view! { <Fold lines=lines/> }.into_any(),
                })
                .collect_view()}
        </div>
    }
    .into_any()
}

/// A run of lines, each with the mark that says what became of it.
#[component]
fn DiffLines(lines: Vec<DiffLine>) -> impl IntoView {
    view! {
        {lines
            .into_iter()
            .map(|line| {
                let (class, mark) = match line.change {
                    Change::Same => ("diff-line", "\u{00a0}"),
                    Change::Added => ("diff-line added", "+"),
                    Change::Removed => ("diff-line removed", "\u{2212}"),
                };
                view! {
                    <div class=class>
                        <span class="diff-mark" aria-hidden="true">{mark}</span>
                        // A blank line still needs its height, or a paragraph
                        // break in the plan reads as two adjacent lines.
                        <span class="diff-text">
                            {if line.text.is_empty() {
                                "\u{00a0}".to_string()
                            } else {
                                line.text
                            }}
                        </span>
                    </div>
                }
            })
            .collect_view()}
    }
}

/// A stretch nothing happened in, folded until asked for.
///
/// The lines are carried rather than counted, so opening a fold is a local
/// signal flipping and not another request for text the browser already holds.
#[component]
fn Fold(lines: Vec<DiffLine>) -> impl IntoView {
    let (open, set_open) = signal(false);
    let count = lines.len();
    let lines = StoredValue::new(lines);

    view! {
        <Show
            when=move || open.get()
            fallback=move || view! {
                <button
                    class="diff-fold"
                    title="Show the lines this revision left alone"
                    on:click=move |_| set_open.set(true)
                >
                    {format!("\u{22ef} {count} unchanged lines")}
                </button>
            }
        >
            <DiffLines lines=lines.get_value()/>
        </Show>
    }
}
