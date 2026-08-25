//! The files rail: the tree of the selected city, as it actually stands on disk.
//!
//! Part of a plan's chamber, not of the throne room. It describes the ground a
//! *conversation* stands on, so it is rendered by `conversation.rs` as the
//! column left of the transcript and exists only while a plan is open. On the
//! map it had nothing to belong to and nothing to say but an instruction to go
//! and choose a city, standing next to the screen whose whole job is choosing
//! one.
//!
//! A supporting column, not a peer of the transcript. It is narrower than the
//! cities rail by default and capped lower (see [`BOUNDS`]), because the King
//! came to read the conversation.
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
use crate::app::{KingdomState, DEFAULT_TREE_WIDTH};
use crate::components::resizer::{restore_width, Bounds, Grows, Resizer};
use kingdom_core::{CityId, DirEntry};
use leptos::prelude::*;
use std::collections::{HashMap, HashSet};

/// How far the files rail may be dragged.
///
/// The ceiling is deliberately below the cities rail's 560: this is the
/// *second* supporting column on the left, and two columns that could each grow
/// to the rail's maximum would leave the transcript fighting for the middle of
/// the screen. A tree of names also needs less room than a rail of titles,
/// badges and model names.
const BOUNDS: Bounds = Bounds {
    min: 180.0,
    max: 420.0,
    default: DEFAULT_TREE_WIDTH,
};

const WIDTH_KEY: &str = "kingdom.tree_width";

/// The root listing's key in the cache. The city root has no name of its own,
/// and every other path is relative to it.
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
pub fn WardTree() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Which folders the King has opened, and what each one contained. Both are
    // view state and neither belongs on the kingdom: nothing outside this rail
    // cares which folders are open.
    let expanded = RwSignal::new(HashSet::<String>::new());
    let listings = RwSignal::new(HashMap::<String, Vec<DirEntry>>::new());
    // Paths with a request in flight, so a double-click cannot send two.
    let fetching = RwSignal::new(HashSet::<String>::new());

    let city = Memo::new(move |_| state.selected.get());
    let city_name = Memo::new(move |_| {
        let id = city.get()?;
        state.kingdom.get().city(&id).map(|c| c.name.clone())
    });

    restore_width(state.tree_width, WIDTH_KEY, BOUNDS);

    /// Asks the server for one directory and files the answer.
    ///
    /// A listing already held is never re-fetched, which is what makes
    /// collapsing and re-opening a folder free. The cost is that the tree does
    /// not notice a file created behind its back; refreshing on every expand
    /// would, and would also re-fetch the whole open tree on every idle click.
    fn fetch(
        city: CityId,
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
            let listed = list_directory(city.to_string(), path.clone())
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

    // A new city is a new tree. Everything cached belongs to the old one, so it
    // is dropped rather than merged -- two cities' paths collide constantly
    // (`src`, `Cargo.toml`) and a merged cache would show one city's files under
    // another's name.
    Effect::new(move |_| {
        let Some(id) = city.get() else {
            expanded.set(HashSet::new());
            listings.set(HashMap::new());
            return;
        };
        expanded.set(HashSet::new());
        listings.set(HashMap::new());
        fetching.set(HashSet::new());
        fetch(id, ROOT.to_string(), listings, fetching);
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
        let Some(id) = city.get_untracked() else {
            return;
        };
        let opening = !expanded.get_untracked().contains(&path);
        expanded.update(|e| {
            if !e.remove(&path) {
                e.insert(path.clone());
            }
        });
        if opening {
            fetch(id, path, listings, fetching);
        }
    };

    // "Nothing selected" and "selected, still loading" are different states and
    // read differently: the first is an instruction, the second is a wait.
    let empty = Memo::new(move |_| rows.get().is_empty());
    let loading_root = Memo::new(move |_| fetching.get().contains(ROOT));

    view! {
        // The width is set inline from the signal, as the spyglass's is: this
        // is a flex child of the chamber now rather than a grid track, so the
        // resizer drives the element itself.
        <aside class="ward-tree" style:width=move || format!("{}px", state.tree_width.get())>
            <div class="ward-tree-head">
                <span class="ward-tree-label">"Wards"</span>
                <span class="ward-tree-city" title=move || city_name.get()>
                    {move || city_name.get().unwrap_or_default()}
                </span>
            </div>

            <div class="ward-tree-body">
                <Show when=move || city.get().is_none()>
                    <p class="ward-tree-hint">
                        "Choose a city to see what stands in it."
                    </p>
                </Show>

                <Show when=move || city.get().is_some() && empty.get()>
                    <p class="ward-tree-hint">
                        {move || if loading_root.get() { "Surveying\u{2026}" } else { "Nothing here." }}
                    </p>
                </Show>

                <ul class="ward-list">
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
                            let indent = format!("{}px", 6.0 + row.depth as f64 * INDENT);
                            let tint = row.entry.language.tint().to_string();
                            let name = row.entry.name.clone();
                            let title = row.entry.path.clone();
                            let on_click = {
                                let path = path.clone();
                                move |_| {
                                    // Only a folder does anything. A file row
                                    // has no handler at all rather than one that
                                    // shrugs -- opening a file is not built yet,
                                    // and a row that looks pressable and is not
                                    // is worse than one that plainly is not.
                                    if is_dir {
                                        toggle(path.clone());
                                    }
                                }
                            };

                            view! {
                                <li>
                                    <div
                                        class="ward-row"
                                        class:is-dir=is_dir
                                        style:padding-left=indent
                                        title=title
                                        on:click=on_click
                                    >
                                        <span class="ward-chevron" class:empty=!is_dir>
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
                                                        class="ward-dot"
                                                        style:background=tint
                                                    ></span>
                                                }
                                            }
                                        >
                                            <span class="ward-folder">"\u{1f5c0}"</span>
                                        </Show>
                                        <span class="ward-name">{name.clone()}</span>
                                    </div>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </div>

            <Resizer
                width=state.tree_width
                grows=Grows::Rightwards
                bounds=BOUNDS
                storage_key=WIDTH_KEY
                class="ward-tree-resizer"
            />
        </aside>
    }
}
