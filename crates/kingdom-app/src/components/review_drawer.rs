//! The review drawer: every file this plan has changed, and by how much.
//!
//! The second view in the files rail. The tree beside it answers "what is in
//! this project?"; this answers the question the King actually opened the plan
//! to ask -- **what did my agent do?** -- and a row here is the way into the
//! side-by-side diff, which opens in the panel beside the transcript.
//!
//! # Why it is keyed on the plan rather than the city
//!
//! A plan works in its own worktree. The city's checkout would show whatever
//! the King himself has half-finished there, which is precisely not the work
//! under review. `plan_changes` compares the *plan's* workspace, and every
//! decision behind that comparison -- the merge base, the untracked files -- is
//! made in `crate::review` and documented there.
//!
//! # Why it refetches on the transcript rather than on a timer
//!
//! The court is writing these files while the King reads them. The watch socket
//! already pushes every transcript entry, so the length of the transcript is a
//! free change signal that moves exactly when work happens -- no polling, and
//! nothing to stop when the plan settles. `--numstat` on a repository is
//! milliseconds; the fetch is guarded against overlap and nothing else.

use crate::api::plan_changes;
use kingdom_core::{ChangeSummary, ChangedFile, PlanId};
use leptos::prelude::*;

/// The changed files of one plan, as a list of rows.
#[component]
pub fn ReviewDrawer(
    plan: PlanId,
    /// What the rail has already fetched. Held by the rail rather than here so
    /// the tab badge can count the files without this view being mounted --
    /// the King should see there is something to review before he switches to
    /// it.
    summary: RwSignal<Option<ChangeSummary>>,
    /// Set while a fetch is in flight, so the drawer can say it is looking
    /// rather than that there is nothing.
    looking: RwSignal<bool>,
    /// The file the panel is currently showing, if it is showing one. Drives
    /// the selected row.
    open_file: Memo<Option<String>>,
    /// Called with a path when the King picks a file to read.
    on_open: Callback<String>,
) -> impl IntoView {
    let _ = plan;

    let files = Memo::new(move |_| summary.get().map(|s| s.files).unwrap_or_default());
    let note = Memo::new(move |_| summary.get().and_then(|s| s.note));
    let base = Memo::new(move |_| summary.get().map(|s| s.base).unwrap_or_default());
    // `None` is "never fetched", which reads as a wait; an empty list that has
    // been fetched is a real answer and reads as one.
    let asked = Memo::new(move |_| summary.get().is_some());

    view! {
        <div class="review-body">
            // What the comparison is against, said once at the top rather than
            // repeated on every row. Without it a list of files is an
            // assertion with no subject.
            <Show when=move || asked.get() && !base.get().is_empty()>
                <div class="review-against">
                    "against "<span class="review-base">{move || base.get()}</span>
                </div>
            </Show>

            <Show when=move || looking.get() && !asked.get()>
                <p class="review-hint">"Reading the ledger\u{2026}"</p>
            </Show>

            // An empty list is ambiguous, so the server's note is shown
            // whenever there is one -- "not a repository" and "nothing changed"
            // are different answers and must not render identically.
            <Show when=move || note.get().is_some()>
                <p class="review-note">{move || note.get().unwrap_or_default()}</p>
            </Show>

            <Show when=move || asked.get() && files.get().is_empty() && note.get().is_none()>
                <p class="review-hint">"Nothing has changed yet."</p>
            </Show>

            <ul class="review-list">
                <For
                    each=move || files.get()
                    // Keyed on the counts as well as the path: a file whose
                    // diff has grown must redraw its numbers, and a key of the
                    // path alone would leave yesterday's `+3 -1` beside
                    // today's work.
                    key=|f: &ChangedFile| (f.path.clone(), f.added, f.removed, f.kind)
                    let:file
                >
                    {
                        let path = file.path.clone();
                        let (folder, name) = file.split();
                        let folder = folder.to_string();
                        let name = name.to_string();
                        let tint = file.language.tint().to_string();
                        let kind = file.kind;
                        let binary = file.binary;
                        let added = file.added;
                        let removed = file.removed;
                        let title = match &file.old_path {
                            Some(old) => format!("{} \u{2014} {}, from {old}", file.path, kind.label()),
                            None => format!("{} \u{2014} {}", file.path, kind.label()),
                        };
                        let selected = {
                            let path = path.clone();
                            Memo::new(move |_| open_file.get().as_deref() == Some(path.as_str()))
                        };
                        let open = {
                            let path = path.clone();
                            move |_| on_open.run(path.clone())
                        };

                        view! {
                            <li>
                                // A button rather than a div: every row here is
                                // pressable, unlike the tree above where only a
                                // folder is, so it should carry the keyboard and
                                // the cursor that says so.
                                <button
                                    class="review-row"
                                    class:selected=move || selected.get()
                                    title=title
                                    on:click=open
                                >
                                    <span
                                        class="review-mark"
                                        class:added=kind == kingdom_core::ChangeKind::Added
                                        class:deleted=kind == kingdom_core::ChangeKind::Deleted
                                        class:untracked=kind == kingdom_core::ChangeKind::Untracked
                                    >
                                        {kind.mark()}
                                    </span>
                                    <span class="review-dot" style:background=tint></span>
                                    <span class="review-name">
                                        // Dimmed folder, bright name: in a
                                        // narrow column the name is what is
                                        // being looked for, and the path is
                                        // only there to disambiguate it.
                                        <Show when={
                                            let folder = folder.clone();
                                            move || !folder.is_empty()
                                        }>
                                            <span class="review-folder">
                                                {folder.clone()}"/"
                                            </span>
                                        </Show>
                                        <span class="review-file">{name.clone()}</span>
                                    </span>
                                    // A binary file says so rather than showing
                                    // `+0 -0`, which would read as "unchanged".
                                    <Show
                                        when=move || binary
                                        fallback=move || view! {
                                            <span class="review-count">
                                                <Show when=move || { added > 0 }>
                                                    <span class="count-added">"+"{added}</span>
                                                </Show>
                                                <Show when=move || { removed > 0 }>
                                                    <span class="count-removed">"\u{2212}"{removed}</span>
                                                </Show>
                                            </span>
                                        }
                                    >
                                        <span class="review-count binary">"bin"</span>
                                    </Show>
                                </button>
                            </li>
                        }
                    }
                </For>
            </ul>
        </div>
    }
}

/// Fetches one plan's changed files into `summary`.
///
/// Lives here rather than in the rail so the drawer owns both halves of its own
/// data, and takes the signals as arguments so the rail can hold them: the tab
/// badge needs the count whether or not this view is mounted.
///
/// Guarded on `looking` rather than debounced. A refetch that arrives while one
/// is in flight is dropped, which is right for a summary -- the next transcript
/// entry will ask again in a moment anyway, and two overlapping answers could
/// land out of order.
pub fn fetch_changes(
    plan: PlanId,
    summary: RwSignal<Option<ChangeSummary>>,
    looking: RwSignal<bool>,
) {
    if looking.get_untracked() {
        return;
    }
    looking.set(true);

    leptos::task::spawn_local(async move {
        let fetched = plan_changes(plan.to_string()).await;
        if let Ok(fetched) = fetched {
            summary.set(Some(fetched));
        }
        // A failed fetch deliberately leaves the last good summary standing:
        // the server function fails when the plan is gone or the request was
        // dropped, and blanking a list the King is reading would be a worse
        // answer than a slightly stale one.
        looking.set(false);
    });
}
