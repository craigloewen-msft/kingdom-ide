//! The prompt bar: where the user starts a new task.
//!
//! Deliberately *only* the composer, plus the two controls that answer "what
//! will draft this, and will it work?" before a prompt is spent. A plan's
//! conversation lives in its own conversation at `/plan/:id`, so this bar turns
//! a sentence and a chosen city into a plan and then gets out of the way by
//! navigating there.

use crate::api::{
    begin_plan, declare_mount, list_branches, list_models, mount_offers, network_available,
    withdraw_mount,
};
use crate::app::KingdomState;
use kingdom_core::{
    City, CredentialState, Isolation, ModelCatalogue, ModelChoice, ModelEffort, ModelOption,
    WorkspaceMode,
};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

#[component]
pub fn PromptBar() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let navigate = use_navigate();

    let (draft, set_draft) = signal(String::new());
    let (showing_models, set_showing_models) = signal(false);
    let (showing_workspace, set_showing_workspace) = signal(false);
    let (showing_isolation, set_showing_isolation) = signal(false);

    let catalogue = Resource::new(|| (), |_| list_models());

    // The prompt targets whichever city is selected, so choosing on the map and
    // typing here are one continuous gesture.
    let target_name = Memo::new(move |_| {
        let id = state.selected.get()?;
        // `with`, not `get`: reading one name should not clone the kingdom.
        state
            .kingdom
            .with(|k| k.city(&id).map(|c: &City| c.name.clone()))
    });

    // What the chip shows, and what the next prompt will carry: the user's own
    // choice if he has made one, otherwise the catalogue's default.
    //
    // Passed through the catalogue before it is shown, because the server
    // resolves the same way before drafting -- a chip advertising a model that
    // has left the catalogue would be a promise the prompt cannot keep.
    let choice = Memo::new(move |_| {
        let wanted = state.choice.get();
        match catalogue.get() {
            Some(Ok(c)) => Some(c.resolve(wanted.as_ref())),
            // Before the catalogue lands there is nothing to check against, so
            // show the user's own choice rather than a placeholder.
            _ => wanted,
        }
    });

    let start = Action::new(move |prompt: &String| {
        let prompt = prompt.clone();
        let city = state.selected.get_untracked().map(|c| c.to_string());
        // Send what the chip promised, not the raw stored value: they differ
        // exactly when a remembered model has left the catalogue.
        let chosen = choice.get_untracked();
        let workspace = state.workspace.get_untracked();
        let isolation = state.isolation.get_untracked();
        let navigate = navigate.clone();

        async move {
            match begin_plan(prompt, city, chosen, Some(workspace), Some(isolation)).await {
                // Opening makes no model call, so the user
                // lands in the conversation while the model is still thinking.
                // The conversation itself kicks off the drafting.
                Ok(plan) => {
                    state.error.set(None);
                    let href = format!("/plan/{}", plan.id);
                    // Insert rather than refetch: opening claimed nothing, so
                    // the new plan is the only thing that changed. Navigating
                    // without it would land the conversation on a plan its own
                    // copy of the kingdom does not yet know about.
                    state.kingdom.update(|k| k.plans.push(plan));
                    navigate(&href, Default::default());
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        }
    });

    let ready = Memo::new(move |_| target_name.get().is_some() && !start.pending().get());

    // The composer grows with what is typed into it, and shrinks back when the
    // decree is spent. Driven off the draft rather than off `input` so that
    // clearing it on submit resets the height too.
    let composer = NodeRef::<leptos::html::Textarea>::new();
    Effect::new(move |_| {
        draft.track();
        if let Some(el) = composer.get() {
            autogrow(&el);
        }
    });

    let submit = move || {
        let text = draft.get().trim().to_string();
        if text.is_empty() || !ready.get_untracked() {
            return;
        }
        set_draft.set(String::new());
        start.dispatch(text);
    };

    view! {
        <section class="decree-bar">
            <div class="decree-row">
                <span class="decree-target" class:none={move || target_name.get().is_none()}>
                    {move || match target_name.get() {
                        Some(name) => format!("\u{2192} {name}"),
                        None => "\u{2192} choose a city".to_string(),
                    }}
                </span>

                // A textarea, so a decree can have more than one line: Enter
                // sends, Shift+Enter is left to the browser and makes a line.
                <textarea
                    class="decree-input"
                    node_ref=composer
                    rows="1"
                    placeholder=move || match target_name.get() {
                        Some(name) => format!("Describe the work for {name}\u{2026}"),
                        None => "Choose a city on the map first\u{2026}".to_string(),
                    }
                    prop:value=move || draft.get()
                    disabled={move || !ready.get()}
                    on:input=move |ev| set_draft.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" && !ev.shift_key() {
                            // Otherwise the newline lands in the box we are
                            // about to clear.
                            ev.prevent_default();
                            submit();
                        }
                    }
                />

                <button
                    class="start-btn"
                    disabled={move || !ready.get()}
                    on:click=move |_| submit()
                >
                    {move || if start.pending().get() { "Opening\u{2026}" } else { "Start" }}
                </button>

                // Which model the next plan opens with. The choice is recorded
                // on the plan, so it is settled here rather than mid-draft.
                // This chip is the only answer to "what will draft this?" --
                // there is no separate provider badge, because the mock is a
                // model in the list like any other.
                <button
                    class="model-chip"
                    title="Choose the model and how hard it thinks"
                    on:click=move |_| {
                        set_showing_workspace.set(false);
                        set_showing_isolation.set(false);
                        set_showing_models.update(|s| *s = !*s);
                    }
                >
                    {move || match choice.get() {
                        Some(c) => c.label(),
                        None => "\u{2026}".to_string(),
                    }}
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>

                // Where the next plan will work. Beside the model chip because
                // they are the same kind of decision: both are settled before a
                // prompt is spent, and both are recorded on the plan.
                <button
                    class="workspace-chip"
                    class:isolated={move || state.workspace.get() != WorkspaceMode::InPlace}
                    title="Choose where this work happens"
                    on:click=move |_| {
                        set_showing_models.set(false);
                        set_showing_isolation.set(false);
                        set_showing_workspace.update(|s| *s = !*s);
                    }
                >
                    {move || state.workspace.get().label()}
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>

                // How far the next plan is walled off. Beside the other two
                // because it is the same kind of decision -- settled before a
                // prompt is spent, recorded on the plan, and not changeable
                // afterwards.
                //
                // The chip reads a constant "Isolation" rather than the current
                // network mode, because it now opens a panel that will hold
                // more than one setting: a chip that named one of them would
                // stop being true the moment a second tab arrives. What it does
                // keep is a mark when anything inside is off its default, so
                // the King can still see he asked for something out of the
                // ordinary without opening it.
                <button
                    class="isolation-chip"
                    class:isolated={move || state.isolation.get().is_isolated()}
                    title="Choose how far this plan is walled off"
                    on:click=move |_| {
                        set_showing_models.set(false);
                        set_showing_workspace.set(false);
                        set_showing_isolation.update(|s| *s = !*s);
                    }
                >
                    "Isolation"
                    <Show when=move || state.isolation.get().is_isolated()>
                        <span class="chip-mark">"\u{2022}"</span>
                    </Show>
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>
            </div>

            <Show when={move || showing_models.get()}>
                <ModelPicker
                    catalogue=catalogue
                    chosen={Signal::derive(move || choice.get())}
                    on_close=move || set_showing_models.set(false)
                />
            </Show>

            <Show when={move || showing_workspace.get()}>
                <WorkspacePicker on_close=move || set_showing_workspace.set(false)/>
            </Show>

            <Show when={move || showing_isolation.get()}>
                <IsolationPicker on_close=move || set_showing_isolation.set(false)/>
            </Show>

            <Show when={move || state.error.get().is_some()}>
                <p class="decree-error">{move || state.error.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// Size a composer to its own contents, up to a cap.
///
/// Capped because the composer shares the screen with the thing being decided
/// on -- the map here, the chamber log there -- and a pasted essay must not
/// swallow it. Past the cap the box scrolls instead.
///
/// **The reset-then-measure is not redundant, and cannot be skipped.** Reading
/// `scroll_height` after setting `height:auto` forces a synchronous reflow, and
/// this runs on every keystroke -- so it looks like an obvious thing to guard
/// with "only measure if the height would change". It is not, because
/// `scroll_height` never reports less than the height already set. A box grown
/// to 80px reports 80 even when its content now needs 20, so the guard reads as
/// "nothing to do" in exactly the case that needs doing, and the composer grows
/// with a long decree and never shrinks back after it is sent. Deciding it
/// wants to be *shorter* requires the reset; there is no cheaper question to
/// ask first.
pub(crate) fn autogrow(el: &web_sys::HtmlTextAreaElement) {
    const MAX_PX: i32 = 160;

    // Fully qualified: leptos's own `style()` extension trait is in scope here
    // and shadows web-sys's.
    let style = web_sys::HtmlElement::style(el);
    // Measured from `auto`: scroll_height never shrinks below the height
    // already set, so without this the box could only ever grow.
    let _ = style.set_property("height", "auto");
    let wanted = el.scroll_height();
    let _ = style.set_property("height", &format!("{}px", wanted.min(MAX_PX)));
    let _ = style.set_property(
        "overflow-y",
        match wanted > MAX_PX {
            true => "auto",
            false => "hidden",
        },
    );
}

/// The picker: which model, and how hard it thinks.
///
/// Recommended models first, the rest behind a toggle -- the full Copilot
/// catalogue runs to dozens of entries, most of which the user will never pick,
/// and a wall of them costs more attention than it saves.
///
/// This is also where a broken credential surfaces. There is no separate status
/// badge: a thin list and the reason it is thin belong in the same place, at
/// the moment the user notices the models he expected are missing.
#[component]
fn ModelPicker(
    catalogue: Resource<Result<ModelCatalogue, ServerFnError>>,
    chosen: Signal<Option<ModelChoice>>,
    on_close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (show_all, set_show_all) = signal(false);

    /// Named so a user who cannot see the model he wants knows exactly what to
    /// set, rather than reading the source to find out.
    const EXAMPLE: &str = "# .kingdom.env \u{2014} either credential path works

# 1. a token you already hold
KINGDOM_API_KEY=gho_\u{2026}

# 2. or a command that prints one (the default)
KINGDOM_API_KEY_HELPER=agency auth github

# optional: which model the picker opens on
KINGDOM_MODEL=copilot/claude-opus-5";

    // Shown only when something is actually wrong, so a healthy model does not
    // spend the user's attention on setup instructions he does not need.
    let needs_help = Memo::new(
        move |_| matches!(catalogue.get(), Some(Ok(c)) if c.credential != CredentialState::Ready),
    );

    let options = Memo::new(move |_| match catalogue.get() {
        Some(Ok(c)) => c.options,
        _ => Vec::new(),
    });

    // Recommended only, until the user asks for everything -- except when
    // nothing is recommended at all. That happens exactly when no credential
    // works and the offline mock is the only model left: filtering it out would
    // leave the user staring at an empty picker with no way to draft.
    let visible = Memo::new(move |_| {
        let all = show_all.get();
        let options = options.get();
        let any_recommended = options.iter().any(|o| o.recommended);
        options
            .into_iter()
            .filter(|o| all || !any_recommended || o.recommended)
            .collect::<Vec<_>>()
    });

    let hidden_count = Memo::new(move |_| {
        let options = options.get();
        match options.iter().any(|o| o.recommended) {
            true => options.iter().filter(|o| !o.recommended).count(),
            false => 0,
        }
    });

    // The effort row belongs to the chosen model: offering a level it does not
    // declare would earn an opaque 400 rather than a harder answer.
    let efforts = Memo::new(move |_| {
        let id = chosen.get()?.model;
        options
            .get()
            .into_iter()
            .find(|o| o.id == id)
            .map(|o| o.efforts)
    });

    // Changing model keeps the user's standing effort, even when this model
    // cannot honour it. Whether a level is sendable is `resolve`'s decision --
    // made on the `choice` memo above and again server-side in `begin_plan` --
    // so dropping it here would protect nothing and would mean that merely
    // passing through the mock erases a preference set weeks ago.
    //
    // Read from `state.choice` rather than from `chosen`, and that distinction
    // is the whole fix: `chosen` is the *resolved* view, already stripped of any
    // level the currently selected model does not declare. Carrying that across
    // would lose the wish on the way out of an effortless model instead of on
    // the way in -- the same bug, one click later.
    let pick_model = move |option: &ModelOption| {
        let next = match state.choice.get_untracked() {
            Some(wish) => wish.with_model(option.id.clone()),
            // Nothing chosen yet, so there is no wish to carry.
            None => ModelChoice::new(option.id.clone(), None),
        };
        state.choose_model(next);
    };

    // The one caller for which `None` is itself a choice: the user asking for
    // the model's own default, which is what clears the remembered level. Takes
    // the model from the resolved `chosen`, which is the one actually selected.
    let pick_effort = move |effort: Option<ModelEffort>| {
        if let Some(current) = chosen.get_untracked() {
            state.choose_model(ModelChoice::new(current.model, effort));
        }
    };

    view! {
        <div class="model-picker">
            <div class="picker-head">
                <span class="picker-title">"Draft with"</span>
                <button class="picker-close" on:click=move |_| on_close()>"\u{2715}"</button>
            </div>

            <p class="setup-detail">
                {move || match catalogue.get() {
                    Some(Ok(c)) => c.detail,
                    Some(Err(e)) => e.to_string(),
                    None => "Asking the court what it can think with\u{2026}".to_string(),
                }}
            </p>

            <Show when={move || needs_help.get()}>
                <pre class="setup-code">{EXAMPLE}</pre>
            </Show>

            <ul class="model-list">
                <For each={move || visible.get()} key=|o: &ModelOption| o.id.clone() let:option>
                    {
                        let id = option.id.clone();
                        let is_chosen = Memo::new(move |_| {
                            chosen.get().is_some_and(|c| c.model == id)
                        });
                        let picked = option.clone();
                        view! {
                            <li>
                                <button
                                    class="model-row"
                                    class:chosen={move || is_chosen.get()}
                                    on:click=move |_| pick_model(&picked)
                                >
                                    <span class="model-name">{option.label.clone()}</span>
                                    // Copilot ships dated aliases that share a
                                    // display name (three distinct "GPT-4o"s),
                                    // so the api name -- which is what the chip
                                    // and the plan record -- is what tells them
                                    // apart.
                                    <span class="model-api-name">
                                        {option.id.rsplit('/').next().unwrap_or(&option.id).to_string()}
                                    </span>
                                    <span class="model-vendor">{option.vendor.clone()}</span>
                                    <span class="model-window">
                                        {kingdom_core::window_label(option.context_window)}
                                    </span>
                                </button>
                            </li>
                        }
                    }
                </For>
            </ul>

            <Show when={move || hidden_count.get() > 0 && !show_all.get()}>
                <button class="picker-more" on:click=move |_| set_show_all.set(true)>
                    {move || format!("Show all {} models", options.get().len())}
                </button>
            </Show>

            <Show when={move || efforts.get().is_some_and(|e| !e.is_empty())}>
                <div class="effort-row">
                    <span class="effort-label">"Effort"</span>
                    // "Default" is not a level: it sends no field at all, which
                    // is a different request from any named effort.
                    <button
                        class="effort-btn"
                        class:chosen={move || chosen.get().is_some_and(|c| c.effort.is_none())}
                        on:click=move |_| pick_effort(None)
                    >
                        "default"
                    </button>
                    <For
                        each={move || efforts.get().unwrap_or_default()}
                        key=|e: &ModelEffort| *e
                        let:effort
                    >
                        <button
                            class="effort-btn"
                            class:chosen={move || {
                                chosen.get().is_some_and(|c| c.effort == Some(effort))
                            }}
                            on:click=move |_| pick_effort(Some(effort))
                        >
                            {effort.wire_name()}
                        </button>
                    </For>
                </div>
            </Show>
        </div>
    }
}

/// The workspace picker: where the next plan's work actually happens.
///
/// Three options rather than a toggle, because they are three genuinely
/// different bargains -- isolation with a new branch, isolation continuing
/// existing work, and no isolation at all -- and collapsing any two of them
/// would hide the one the user most needs to make on purpose.
#[component]
fn WorkspacePicker(on_close: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (showing_branches, set_showing_branches) = signal(false);

    let current = Memo::new(move |_| state.workspace.get());

    // Reloaded whenever the selected city changes: branches are per repository,
    // and offering another project's would be worse than offering none.
    let branches = Resource::new(
        move || state.selected.get().map(|c| c.to_string()),
        |city| async move {
            match city {
                Some(c) => list_branches(c).await.unwrap_or_default(),
                None => Vec::new(),
            }
        },
    );

    let choose = move |mode: WorkspaceMode| {
        state.choose_workspace(mode);
        on_close();
    };

    let is_fresh = Memo::new(move |_| current.get() == WorkspaceMode::Fresh);
    let is_in_place = Memo::new(move |_| current.get() == WorkspaceMode::InPlace);
    let is_branch = Memo::new(move |_| matches!(current.get(), WorkspaceMode::Branch(_)));

    view! {
        <div class="workspace-picker">
            <div class="picker-head">
                <span class="picker-title">"Work in"</span>
                <button class="picker-close" on:click=move |_| on_close()>"\u{2715}"</button>
            </div>

            <ul class="workspace-list">
                <li>
                    <button
                        class="workspace-row"
                        class:chosen={move || is_fresh.get()}
                        on:click=move |_| choose(WorkspaceMode::Fresh)
                    >
                        <span class="workspace-name">"A fresh worktree"</span>
                        <span class="workspace-detail">
                            "A new checkout under .kingdom/, on a branch of its own."
                        </span>
                    </button>
                </li>

                <li>
                    <button
                        class="workspace-row"
                        class:chosen={move || is_branch.get()}
                        on:click=move |_| set_showing_branches.update(|s| *s = !*s)
                    >
                        <span class="workspace-name">
                            {move || match current.get() {
                                WorkspaceMode::Branch(b) => format!("A branch \u{2014} {b}"),
                                _ => "A specific branch\u{2026}".to_string(),
                            }}
                        </span>
                        <span class="workspace-detail">
                            "Checked out into its own worktree, same as above."
                        </span>
                    </button>

                    <Show when={move || showing_branches.get()}>
                        <ul class="branch-list">
                            {move || match branches.get() {
                                Some(list) if !list.is_empty() => list
                                    .into_iter()
                                    .map(|b| {
                                        let picked = b.clone();
                                        let name = b.clone();
                                        view! {
                                            <li>
                                                <button
                                                    class="branch-row"
                                                    class:chosen={move || {
                                                        current.get()
                                                            == WorkspaceMode::Branch(name.clone())
                                                    }}
                                                    on:click=move |_| {
                                                        choose(WorkspaceMode::Branch(picked.clone()))
                                                    }
                                                >
                                                    {b.clone()}
                                                </button>
                                            </li>
                                        }
                                    })
                                    .collect_view()
                                    .into_any(),
                                // Distinguishing "no branches" from "still
                                // looking" matters: the first means this city
                                // is not a git repository at all.
                                Some(_) => view! {
                                    <li class="branch-empty">
                                        "No branches here \u{2014} this project is not under git."
                                    </li>
                                }
                                .into_any(),
                                None => view! {
                                    <li class="branch-empty">"Reading the branches\u{2026}"</li>
                                }
                                .into_any(),
                            }}
                        </ul>
                    </Show>
                </li>

                <li>
                    <button
                        class="workspace-row"
                        class:chosen={move || is_in_place.get()}
                        on:click=move |_| choose(WorkspaceMode::InPlace)
                    >
                        <span class="workspace-name">"This folder"</span>
                        <span class="workspace-detail">
                            "The project directory itself. No isolation."
                        </span>
                    </button>
                </li>
            </ul>
        </div>
    }
}

/// Which half of the isolation panel is showing.
///
/// Two tabs rather than one list, because the King asks two questions --
/// "whose ports?" and "whose files?" -- and the old panel made him read three
/// paragraphs to find the row that answered both at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IsolationTab {
    Network,
    Files,
}

/// How far the next plan is walled off from the machine and from its siblings.
///
/// # Two tabs over a three-rung ladder
///
/// [`kingdom_core::Isolation`] is unchanged and still a ladder: `Shared`,
/// `Isolated`, `Sealed`. This panel is a **projection** of it onto the two axes
/// the King actually thinks in, and the mapping is total in one direction:
///
/// | Network | Files | is |
/// |---|---|---|
/// | host | host | `Shared` |
/// | its own | host | `Isolated` |
/// | its own | its own | `Sealed` |
///
/// The fourth square -- a filesystem of its own on the King's network -- does
/// not exist, and not by omission: the holder is one `unshare`, and `--mount`
/// is added to `--net` rather than used instead of it (see `namespaces::net`).
/// So while the Files tab says "its own", the Network tab shows "Host network"
/// **disabled, with the reason** -- the same shape this panel already uses for
/// a missing `slirp4netns`, and for the same argument: an option quietly
/// missing teaches nothing.
///
/// # What this panel deliberately no longer does
///
/// It does not list the project's shared services. That footnote cost a
/// `shared_resources()` call on every open, which reaches `docker version` with
/// no timeout -- so on a machine where Docker is slow or absent the panel sat
/// waiting on a daemon in order to print a sentence about databases, and then
/// changed height when it landed. `/resources` is where a well is reviewed.
#[component]
fn IsolationPicker(on_close: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (tab, set_tab) = signal(IsolationTab::Network);

    // Asked once per opening rather than cached: slirp4netns can be installed
    // while Kingdom runs, and the King who just installed it on our own advice
    // should not have to restart the server to be believed.
    let available = Resource::new(|| (), |_| network_available());

    // `None` -> still asking; `Some(None)` -> available; `Some(Some(why))` -> not.
    let refusal = Signal::derive(move || match available.get() {
        Some(Ok(reason)) => reason,
        // A server that could not answer is treated as "cannot", with its own
        // words: the alternative is offering an option that then fails.
        Some(Err(e)) => Some(e.to_string()),
        None => None,
    });
    // Disabled while the answer is still coming, so a fast click cannot choose
    // an option that turns out to be unavailable.
    let barred = Signal::derive(move || match available.get() {
        Some(_) => refusal.get().is_some(),
        None => true,
    });

    // Fetched when the **panel** opens rather than when the Files tab is
    // reached, so switching tabs shows a list instead of a spinner. It is a
    // `PATH` walk and a handful of `stat`s -- cheap enough to pay for eagerly,
    // unlike the Docker call this panel used to make.
    let city = Memo::new(move |_| state.selected.get());
    let revision = RwSignal::new(0_u32);
    let offers = Resource::new(
        move || (city.get(), revision.get()),
        |(city, _)| async move { mount_offers(city.map(|c| c.to_string())).await.ok() },
    );

    let is_network = Memo::new(move |_| tab.get() == IsolationTab::Network);

    view! {
        <div class="workspace-picker isolation-picker">
            <div class="picker-head">
                <span class="picker-title">"Isolation"</span>

                <div class="iso-tabs">
                    // Each tab carries the answer it currently holds, so both
                    // settings can be read without opening both.
                    <button
                        class="iso-tab"
                        class:chosen={move || is_network.get()}
                        on:click=move |_| set_tab.set(IsolationTab::Network)
                    >
                        "Network"
                        <span class="iso-tab-value">
                            {move || match state.isolation.get().is_isolated() {
                                true => "its own",
                                false => "host",
                            }}
                        </span>
                    </button>
                    <button
                        class="iso-tab"
                        class:chosen={move || !is_network.get()}
                        on:click=move |_| set_tab.set(IsolationTab::Files)
                    >
                        "Files"
                        <span class="iso-tab-value">
                            {move || match state.isolation.get().is_sealed() {
                                true => "its own",
                                false => "host",
                            }}
                        </span>
                    </button>
                </div>

                <button class="picker-close" on:click=move |_| on_close()>"\u{2715}"</button>
            </div>

            // One box of a fixed height for both tabs. The panel sits directly
            // above the composer, and a body that grew as an answer landed --
            // or as a tab was switched -- moved the Start button out from under
            // the King's cursor. It scrolls inside instead.
            <div class="iso-body">
                <Show
                    when=move || is_network.get()
                    fallback=move || view! {
                        <FileChoices
                            offers=offers
                            revision=revision
                            refusal=refusal
                            barred=barred
                        />
                    }
                >
                    <NetworkChoices refusal=refusal barred=barred/>
                </Show>
            </div>
        </div>
    }
}

/// Whose ports the next plan takes.
///
/// # Why the unavailable case is a disabled row and not a hidden one
///
/// Without `slirp4netns` an isolated plan cannot reach DNS, crates.io or git,
/// so Kingdom refuses to open one at all. The option is therefore shown
/// *disabled, with the reason and the command to fix it*, rather than quietly
/// omitted -- a King who never learns the feature exists cannot decide he wants
/// it, and one whose prompt is refused after he has typed it learns the same
/// thing at a worse moment.
#[component]
fn NetworkChoices(refusal: Signal<Option<String>>, barred: Signal<bool>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let current = Memo::new(move |_| state.isolation.get());

    // A filesystem of its own always brings a network of its own with it, so
    // this row is not the King's to press while the Files tab says "its own".
    // Said on the row rather than by removing it.
    let held_by_files = Memo::new(move |_| current.get().is_sealed());

    view! {
        <ul class="workspace-list">
            <li>
                <button
                    class="workspace-row"
                    class:chosen={move || !current.get().is_isolated()}
                    disabled={move || held_by_files.get()}
                    on:click=move |_| state.choose_isolation(Isolation::Shared)
                >
                    <span class="workspace-name">"Host network"</span>
                    <span class="workspace-detail">
                        "Your ports, shared with everything else on this \
                         machine: two plans that both want :3000 will collide."
                    </span>
                </button>
            </li>

            <li>
                <button
                    class="workspace-row"
                    class:chosen={move || current.get().is_isolated()}
                    disabled={move || barred.get()}
                    on:click=move |_| {
                        // Sealed is already a network of its own; pressing this
                        // must not quietly give the plan the King's files back.
                        if !current.get_untracked().is_sealed() {
                            state.choose_isolation(Isolation::Isolated);
                        }
                    }
                >
                    <span class="workspace-name">"Its own network"</span>
                    <span class="workspace-detail">
                        "Its own loopback: it can take :3000 without touching \
                         yours, and the ports it opens are forwarded back to you."
                    </span>
                </button>
            </li>

            <Show when=move || held_by_files.get()>
                <p class="iso-note">
                    "A plan with a file system of its own always has its own \
                     network too. Set Files back to the host machine to share \
                     yours."
                </p>
            </Show>

            // The reason, and the command that fixes it. Kingdom does not
            // install anything on the King's machine; it says what to install.
            <Show when=move || refusal.get().is_some()>
                <p class="network-unavailable">{move || refusal.get().unwrap_or_default()}</p>
            </Show>
        </ul>
    }
}

/// Whose files the next plan sees, and which of the King's it is lent.
///
/// The checkboxes sit under the second row only, because what a plan may see is
/// a question the first row does not raise: a plan on the host machine has
/// everything already.
///
/// It takes the same refusal the Network tab does, and for a reason that is not
/// obvious from this side: a filesystem of its own is built by the *same*
/// holder as a network of its own, so a machine without `slirp4netns` cannot
/// give a plan either. Offering it here and refusing it at Start would move the
/// bad news to the worst possible moment.
#[component]
fn FileChoices(
    offers: Resource<Option<Vec<kingdom_core::services::MountCandidate>>>,
    revision: RwSignal<u32>,
    refusal: Signal<Option<String>>,
    barred: Signal<bool>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let current = Memo::new(move |_| state.isolation.get());
    let sealed = Memo::new(move |_| current.get().is_sealed());

    view! {
        <ul class="workspace-list">
            <li>
                <button
                    class="workspace-row"
                    class:chosen={move || !sealed.get()}
                    on:click=move |_| {
                        // Down one rung, not two: the network is a separate
                        // question, answered on the tab beside this one.
                        // Dropping it here would undo a choice this control
                        // does not name.
                        if current.get_untracked().is_sealed() {
                            state.choose_isolation(Isolation::Isolated);
                        }
                    }
                >
                    <span class="workspace-name">"Host machine"</span>
                    <span class="workspace-detail">
                        "Your files are its files, writable: it can change \
                         anything you have."
                    </span>
                </button>
            </li>

            <li>
                <button
                    class="workspace-row"
                    class:chosen={move || sealed.get()}
                    disabled={move || barred.get()}
                    on:click=move |_| state.choose_isolation(Isolation::Sealed)
                >
                    <span class="workspace-name">"Own file system"</span>
                    <span class="workspace-detail">
                        "Its workspace, its own processes and a read-only \
                         system \u{2014} and nothing else of yours but what you \
                         tick below."
                    </span>
                </button>

                <Show when=move || sealed.get()>
                    <MountChecklist offers=offers revision=revision/>
                </Show>
            </li>

            // The same refusal the Network tab prints, under the row it refuses.
            <Show when=move || refusal.get().is_some()>
                <p class="network-unavailable">{move || refusal.get().unwrap_or_default()}</p>
            </Show>
        </ul>
    }
}

/// The folders a sealed plan may see, as boxes that tick and untick.
///
/// # Why this is here and not only on the resources screen
///
/// A mount *is* a shared resource and `/resources` is where they live -- but
/// this is the one moment the King is actually deciding what a plan may see,
/// and sending him to another screen to make the decision he is already making
/// is how a feature goes unused. The screen remains the place to review every
/// project's at once; this is the place to say yes and no.
///
/// # Why a box writes to his profile
///
/// A box ticked here always declares at the **host** scope. A toolchain is a
/// fact about his machine, not about one project: `~/.cargo` is where cargo
/// lives whatever he is working on, and writing that into a project's committed
/// manifest would put his home directory's layout into somebody else's
/// repository.
///
/// The converse is why a folder the *project* declared is shown ticked and
/// **fixed**: it lives in a file belonging to whoever else works on that
/// repository, and Kingdom will not edit that because a box was clicked. It is
/// shown all the same, because it is part of what the plan will see.
#[component]
fn MountChecklist(
    offers: Resource<Option<Vec<kingdom_core::services::MountCandidate>>>,
    revision: RwSignal<u32>,
) -> impl IntoView {
    let failure = RwSignal::new(Option::<String>::None);
    // One flag for the whole list rather than one per row: these are writes to
    // a single file, and two in flight at once is a manifest whose contents
    // depend on which landed first.
    let writing = RwSignal::new(false);

    let toggle = move |folders: Vec<kingdom_core::services::MountSpec>, on: bool| {
        if writing.get_untracked() {
            return;
        }
        writing.set(true);
        failure.set(None);
        leptos::task::spawn_local(async move {
            let done = match on {
                true => declare_mount("host".to_string(), None, folders).await,
                false => withdraw_mount("host".to_string(), None, folders).await,
            };
            if let Err(e) = done {
                failure.set(Some(e.to_string()));
            }
            writing.set(false);
            // Bumped either way, which is what puts a box that failed back
            // where it was: what the list draws is the manifest, never the
            // click.
            revision.update(|r| *r += 1);
        });
    };

    view! {
        <div class="quick-add">
            <p class="quick-add-head">"Also let it see:"</p>

            <Suspense fallback=|| view! { <p class="quick-add-empty">"Looking\u{2026}"</p> }>
                {move || {
                    let found = offers.get().flatten().unwrap_or_default();
                    if found.is_empty() {
                        return view! {
                            <p class="quick-add-empty">
                                "Nothing to add: every tool on your PATH is already \
                                 part of the read-only system a sealed plan gets."
                            </p>
                        }
                            .into_any();
                    }
                    view! {
                        <ul class="quick-add-list">
                            {found
                                .into_iter()
                                .map(|offer| {
                                    let folders = offer.folders.clone();
                                    let shown = folders
                                        .iter()
                                        .map(|f| f.path.clone())
                                        .collect::<Vec<_>>()
                                        .join("  ");
                                    // Said per offer, because "it can write
                                    // there" is the part worth being sure of.
                                    let writable = folders.iter().any(|f| f.mode.is_writable());
                                    let on = offer.already();
                                    // Declared by the project rather than by
                                    // him: shown, and not this panel's to undo.
                                    let fixed = on && !offer.removable();
                                    view! {
                                        <li>
                                            <label class="mount-row" class:fixed=fixed>
                                                <input
                                                    class="mount-box"
                                                    type="checkbox"
                                                    prop:checked=on
                                                    disabled={move || fixed || writing.get()}
                                                    on:change={
                                                        let folders = folders.clone();
                                                        move |_| toggle(folders.clone(), !on)
                                                    }
                                                />
                                                <span class="quick-add-why">{offer.why.clone()}</span>
                                                <span class="quick-add-paths">{shown}</span>
                                                <span class="quick-add-mark">
                                                    {if fixed {
                                                        "this project".to_string()
                                                    } else if writable {
                                                        "writable".to_string()
                                                    } else {
                                                        String::new()
                                                    }}
                                                </span>
                                            </label>
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </Suspense>

            <Show when=move || failure.get().is_some()>
                <p class="network-unavailable">{move || failure.get().unwrap_or_default()}</p>
            </Show>
        </div>
    }
}
