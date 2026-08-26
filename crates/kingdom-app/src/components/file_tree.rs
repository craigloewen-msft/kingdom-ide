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
    /// Bumped when the workspace's *shape* changes under the tree -- today, when
    /// the King deletes a file himself.
    ///
    /// The cache below is never invalidated on its own, deliberately (see
    /// [`fetch`]): re-listing on every tick would re-walk the whole open tree
    /// whenever the court did anything. But a file the King has just deleted
    /// would then sit in the rail forever, and clicking it would open a panel
    /// reporting that it is gone. This is the one signal that empties the cache,
    /// re-listing the root and every folder he has open.
    revision: RwSignal<usize>,
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
    // The scrolling box, so the open file's row can be brought into view.
    let body = NodeRef::<leptos::html::Div>::new();

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
    //
    // ...and again whenever `revision` moves, which is the one thing that means
    // "the listing you are holding is wrong". Emptying the cache first is what
    // makes the refetch happen at all -- [`fetch`] returns early for a path it
    // already holds, which is exactly the behaviour being overridden here.
    Effect::new(move |_| {
        revision.track();

        let open = expanded.get_untracked();
        listings.update(|l| l.clear());

        fetch(plan.get_value(), ROOT.to_string(), listings, fetching);
        // Every folder the King had open, so the tree comes back as he left it
        // rather than collapsed to the root. They are listed in whatever order
        // the set yields, which is fine: each row is placed by `walk` from the
        // cache once its parent has arrived, so a child landing before its
        // parent is simply not drawn yet.
        for path in open {
            fetch(plan.get_value(), path, listings, fetching);
        }
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

    // Revealing the file the panel is showing, wherever it came from.
    //
    // The tree already marks the open file's row selected, but a file several
    // folders deep sits inside folders the King never opened -- so there is no
    // row to mark, and the rail looks like it has ignored him. That was
    // tolerable while the only way to open a file was to press a row that was
    // by definition already visible. It is not now that a file can be opened by
    // pressing its building on the map, or from the review drawer.
    //
    // Two halves: open the folders on the way down, then scroll to what they
    // uncovered.
    Effect::new(move |_| {
        let Some(path) = open_file.get() else {
            return;
        };

        // Every folder on the way to the file. `src/engine/camera.rs` needs
        // `src` and `src/engine` -- the file's own path is not a folder and is
        // deliberately not included.
        let mut ancestors = Vec::new();
        let mut walked = String::new();
        let mut parts: Vec<&str> = path.split('/').collect();
        parts.pop();
        for part in parts {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(part);
            ancestors.push(walked.clone());
        }

        // Only the ones that are not already open, so a file opened inside a
        // folder the King has expanded himself writes nothing and cannot
        // re-trigger this effect.
        let shut: Vec<String> = expanded.with_untracked(|open| {
            ancestors
                .iter()
                .filter(|path| !open.contains(*path))
                .cloned()
                .collect()
        });
        if !shut.is_empty() {
            expanded.update(|open| {
                for path in &shut {
                    open.insert(path.clone());
                }
            });
            // A folder whose listing has not arrived yet contributes no rows,
            // and `walk` places a child only once its parent has landed -- so
            // these may complete in any order and the tree assembles itself as
            // they do. `fetch` ignores a path it already holds, so a folder
            // opened before costs nothing here.
            for path in shut {
                fetch(plan.get_value(), path, listings, fetching);
            }
        }

        // And then bring the row into view. Tracking `rows` as well as the path
        // is what makes this work for a folder that had to be fetched: the row
        // does not exist on this run, and the effect re-runs when the listing
        // lands and builds it.
        rows.track();
        reveal_selected_row(body);
    });

    // "Never fetched" and "fetched, and empty" read differently: the first is a
    // wait, the second is an answer.
    let empty = Memo::new(move |_| rows.get().is_empty());
    let loading_root = Memo::new(move |_| fetching.get().contains(ROOT));

    view! {
        // Just the body: the column, its head and its resizer belong to the
        // rail that holds this, because the review drawer shares all three.
        <div class="file-tree-body" node_ref=body>
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

/// Scrolls the row of the open file into view, if it has one and it is off
/// screen.
///
/// Found by the `selected` class the rows already compute rather than by
/// building a selector out of the path. That is not a shortcut: a path may hold
/// quotes, brackets and spaces, and escaping one into a CSS attribute selector
/// correctly is a job with a wrong answer. The class is already exactly "the
/// row of the file the panel is showing".
///
/// Deferred by a frame, because the caller runs inside the effect that *causes*
/// the row to exist: Leptos has not flushed the new rows to the DOM yet, so
/// looking now would find the tree as it was.
///
/// Only scrolls when the row is genuinely outside the box. Pressing a row that
/// is already on screen -- the ordinary case, and the only one before the map
/// could open files -- must not jerk the list about under the King's pointer.
#[cfg(feature = "hydrate")]
fn reveal_selected_row(body: NodeRef<leptos::html::Div>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let scroll = Closure::once_into_js(move || {
        let Some(box_el) = body.get_untracked() else {
            return;
        };
        let box_el: web_sys::HtmlElement = box_el.into();
        let Ok(Some(row)) = box_el.query_selector(".file-row.selected") else {
            return;
        };
        let Ok(row) = row.dyn_into::<web_sys::HtmlElement>() else {
            return;
        };

        // `offset_top` is measured against the offset parent rather than the
        // scroll box, so the two are subtracted rather than one being trusted.
        let top = row.offset_top() - box_el.offset_top();
        let bottom = top + row.offset_height();
        let view_top = box_el.scroll_top();
        let view_bottom = view_top + box_el.client_height();

        if top >= view_top && bottom <= view_bottom {
            return;
        }
        // Centred, so a file revealed from the map arrives with its neighbours
        // around it rather than jammed against an edge -- the King is being
        // shown *where* the file lives, not only that it exists.
        let centred = top - (box_el.client_height() - row.offset_height()) / 2;
        box_el.set_scroll_top(centred.max(0));
    });
    let _ = window.request_animation_frame(scroll.unchecked_ref());
}

/// The server has no tree to scroll and no DOM to look in.
///
/// This crate builds on both targets, so the browser's version needs a
/// counterpart here -- the same split `browser_view.rs` makes for its
/// screencast, and for the same reason.
#[cfg(not(feature = "hydrate"))]
fn reveal_selected_row(_body: NodeRef<leptos::html::Div>) {}
