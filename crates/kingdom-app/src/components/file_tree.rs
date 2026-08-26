//! The files rail's tree: the plan's workspace, as it actually stands on disk.
//!
//! The **body** of a rail rather than the rail itself. [`super::city_rail`] owns
//! the column, its width and its split; this is one of the two views inside it,
//! and the other is the review drawer. It was the whole rail until the drawer
//! arrived, and the split is why the head and the resizer are no longer here.
//!
//! Part of a plan's chamber, not of the throne room. It describes the ground a
//! *conversation* stands on, so it is rendered by `conversation.rs` as the
//! column left of the transcript and exists only while a plan is open. On the
//! map it had nothing to belong to and nothing to say but an instruction to go
//! and choose a city, standing next to the screen whose whole job is choosing
//! one.
//!
//! # Why the plan's workspace and not the city's checkout
//!
//! An isolated plan works in a **worktree**, which is a different copy of the
//! project from the one the city's directory holds. This tree used to list the
//! city, which was tolerable while it was read-only decoration and is not now
//! that a row here opens a file the King can write notes against: line 34 of the
//! city's checkout is not line 34 of the plan's worktree, so the court would be
//! sent an objection about code it cannot see. `api::list_directory` carries the
//! full reasoning.
//!
//! # Why it fetches rather than reading the city it already has
//!
//! [`kingdom_core::City`] carries a whole `Folder` tree, and drawing that would
//! need no server call at all -- but it is the *map's* tree: the scanner keeps
//! only the largest files per folder, sorted by size, to a bounded depth. Right
//! for a skyline, and a lie in a panel whose whole promise is "these are the
//! files". So each directory is listed on demand by
//! [`crate::api::list_directory`], which means nothing walks a large repository
//! until the King opens the folder.
//!
//! # Why the rows are flat
//!
//! A tree renders as a **flat `Vec` of rows carrying a depth**, walked out of
//! one cache, rather than as a component that renders itself recursively. A
//! recursive component needs `.into_any()` at every level and gives each level
//! its own fetch state, which is three ways to be half-loaded; flattening keeps
//! one cache, one `For`, and turns indentation into arithmetic.

use crate::api::list_directory;
use kingdom_core::{DirEntry, PlanId};
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};

/// The root listing's key in the cache. The workspace root has no name of its
/// own, and every other path is relative to it.
const ROOT: &str = "";

/// How far one level of nesting shifts a row, in pixels. Small: the column is
/// narrow, and a deep path must not indent its way off the edge.
const INDENT: f64 = 12.0;

/// One line of the rendered tree: an entry, and how deep it sits.
#[derive(Clone, PartialEq)]
struct Row {
    entry: DirEntry,
    depth: usize,
}

#[component]
pub fn FileTree(
    /// Whose workspace is being listed. A prop rather than read from
    /// `state.selected`, because the city a plan works in and the directory it
    /// works in are no longer the same place.
    plan: PlanId,
    /// The file the panel beside the transcript is showing, if any. Drives the
    /// selected row, so the tree and the drawer agree on what is open.
    open_file: Memo<Option<String>>,
    /// Called with a path when the King picks a file to read.
    on_open: Callback<String>,
) -> impl IntoView {
    // Which folders the King has opened, and what each one contained. Both are
    // view state and neither belongs on the kingdom: nothing outside this rail
    // cares which folders are open.
    let expanded = RwSignal::new(HashSet::<String>::new());
    let listings = RwSignal::new(HashMap::<String, Vec<DirEntry>>::new());
    // Paths with a request in flight, so a double-click cannot send two.
    let fetching = RwSignal::new(HashSet::<String>::new());

    let plan = StoredValue::new(plan);

    /// Asks the server for one directory and files the answer.
    ///
    /// A listing already held is never re-fetched, which is what makes
    /// collapsing and re-opening a folder free. The cost is that the tree does
    /// not notice a file created behind its back; refreshing on every expand
    /// would, and would also re-fetch the whole open tree on every idle click.
    fn fetch(
        plan: PlanId,
        path: String,
        listings: RwSignal<HashMap<String, Vec<DirEntry>>>,
        fetching: RwSignal<HashSet<String>>,
    ) {
        if listings.get_untracked().contains_key(&path) || fetching.get_untracked().contains(&path)
        {
            return;
        }
        fetching.update(|f| {
            f.insert(path.clone());
        });

        leptos::task::spawn_local(async move {
            let listed = list_directory(plan.to_string(), path.clone())
                .await
                .unwrap_or_default();
            listings.update(|l| {
                l.insert(path.clone(), listed);
            });
            fetching.update(|f| {
                f.remove(&path);
            });
        });
    }

    // The root, once. A new *plan* is a new chamber and therefore a new
    // component, so unlike the city version this has nothing to reset: the
    // cache is born with the tree it belongs to and dies with it. See the note
    // on `open_plan` in `conversation.rs` for why that is true.
    Effect::new(move |_| {
        fetch(plan.get_value(), ROOT.to_string(), listings, fetching);
    });

    // The whole visible tree, flattened. Recomputed when a folder opens, closes
    // or arrives -- all three are the same operation as far as this is
    // concerned, which is the point of keeping one cache.
    let rows = Memo::new(move |_| {
        let listings = listings.get();
        let expanded = expanded.get();
        let mut rows = Vec::new();

        // An explicit stack rather than a recursive closure: a closure that
        // calls itself needs to be named before it exists, and the depth here
        // is whatever the King has opened.
        fn walk(
            path: &str,
            depth: usize,
            listings: &HashMap<String, Vec<DirEntry>>,
            expanded: &HashSet<String>,
            rows: &mut Vec<Row>,
        ) {
            let Some(entries) = listings.get(path) else {
                return;
            };
            for entry in entries {
                rows.push(Row {
                    entry: entry.clone(),
                    depth,
                });
                if entry.is_dir && expanded.contains(&entry.path) {
                    walk(&entry.path, depth + 1, listings, expanded, rows);
                }
            }
        }

        walk(ROOT, 0, &listings, &expanded, &mut rows);
        rows
    });

    let toggle = move |path: String| {
        let opening = !expanded.get_untracked().contains(&path);
        expanded.update(|e| {
            if !e.remove(&path) {
                e.insert(path.clone());
            }
        });
        if opening {
            fetch(plan.get_value(), path, listings, fetching);
        }
    };

    // "Never fetched" and "fetched, and empty" read differently: the first is a
    // wait, the second is an answer.
    let empty = Memo::new(move |_| rows.get().is_empty());
    let loading_root = Memo::new(move |_| fetching.get().contains(ROOT));

    view! {
        // Just the body: the column, its head and its resizer belong to the
        // rail that holds this, because the review drawer shares all three.
        <div class="file-tree-body">
            <Show when=move || empty.get()>
                <p class="file-tree-hint">
                    {move || if loading_root.get() { "Surveying\u{2026}" } else { "Nothing here." }}
                </p>
            </Show>

            <ul class="file-list">
                // Keyed on what the row draws, not the path alone: the same
                // path re-renders when it opens or closes, and a key of just
                // the path would leave a folder showing a closed chevron
                // over its own open children.
                <For
                    each=move || rows.get()
                    key=|row: &Row| {
                        (row.entry.path.clone(), row.depth, row.entry.is_dir)
                    }
                    let:row
                >
                    {
                        let path = row.entry.path.clone();
                        let is_dir = row.entry.is_dir;
                        let open = {
                            let path = path.clone();
                            Memo::new(move |_| expanded.get().contains(&path))
                        };
                        let busy = {
                            let path = path.clone();
                            Memo::new(move |_| fetching.get().contains(&path))
                        };
                        // Which file the panel is showing. A file row is now
                        // pressable, so it needs to say when it is the one
                        // being read -- the review drawer's rows already do.
                        let selected = {
                            let path = path.clone();
                            Memo::new(move |_| {
                                !is_dir && open_file.get().as_deref() == Some(path.as_str())
                            })
                        };
                        let indent = format!("{}px", 6.0 + row.depth as f64 * INDENT);
                        let tint = row.entry.language.tint().to_string();
                        let name = row.entry.name.clone();
                        let title = row.entry.path.clone();
                        let on_click = {
                            let path = path.clone();
                            move |_| {
                                // Both kinds of row do something now: a folder
                                // opens in place, a file opens in the panel
                                // beside the transcript.
                                if is_dir {
                                    toggle(path.clone());
                                } else {
                                    on_open.run(path.clone());
                                }
                            }
                        };

                        view! {
                            <li>
                                // A button rather than a div: every row here is
                                // pressable now, so it should carry the keyboard
                                // and the cursor that say so -- the same change
                                // of element the review drawer's rows already
                                // are.
                                <button
                                    class="file-row"
                                    class:is-dir=is_dir
                                    class:selected=move || selected.get()
                                    style:padding-left=indent
                                    title=title
                                    on:click=on_click
                                >
                                    <span class="file-chevron" class:empty=!is_dir>
                                        {move || {
                                            if !is_dir {
                                                ""
                                            } else if busy.get() {
                                                "\u{22ef}"
                                            } else if open.get() {
                                                "\u{25be}"
                                            } else {
                                                "\u{25b8}"
                                            }
                                        }}
                                    </span>
                                    <Show
                                        when=move || is_dir
                                        fallback=move || {
                                            let tint = tint.clone();
                                            view! {
                                                <span
                                                    class="file-dot"
                                                    style:background=tint
                                                ></span>
                                            }
                                        }
                                    >
                                        <span class="file-folder">"\u{1f5c0}"</span>
                                    </Show>
                                    <span class="file-name">{name.clone()}</span>
                                </button>
                            </li>
                        }
                    }
                </For>
            </ul>
        </div>
    }
}
