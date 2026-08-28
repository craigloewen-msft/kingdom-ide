//! The ledger of wells: every shared resource, who it is shared with, and how
//! to declare another.
//!
//! Shown to the King as "shared resources"; a shared service in code.
//!
//! # Why this is a screen and the badge is not enough
//!
//! `ports_badge.rs` answers *"what can this plan reach?"* -- a glance, inside
//! one conversation. The questions this screen answers are different in kind
//! and none of them fit in a popover:
//!
//! - *What does this machine share at all?* Not answerable from a chamber,
//!   because a chamber is one project and the answer spans every project plus
//!   the King's own profile.
//! - *Who is in this database right now?* The badge counts them; a decision
//!   about a shared database needs their names.
//! - *Where do I go to change this?* The manifest is the source of truth and
//!   the King edits it by hand, so the path to it is the single most useful
//!   thing this screen prints.
//!
//! # Why it does not start or stop anything
//!
//! A well is raised when a kingdom or a plan opens and stopped when the last
//! agent that could reach it is gone -- `services::reconcile` holds that
//! invariant, keyed on the live population. A stop button would fight that
//! count in front of five working agents, and a start button would raise a
//! database nobody had asked for. So this screen **reports** state and never
//! commands it. The one thing it writes is a new declaration, which is a change
//! to a file rather than to anything running.
//!
//! # Why the form appends text
//!
//! See [`kingdom_core::ServiceSpec::render`]. The short version: the manifest
//! is a file people comment, and a form that re-serialised the document would
//! eat those comments. So the form is a typist -- it shows the exact block it
//! is about to append, and the King can see it before it is written.

use crate::api::{declare_shared_resource, shared_resources};
use crate::app::KingdomState;
use kingdom_core::{ResourceInventory, ServiceScope, ServiceSpec, ServiceState, SharedResource};
use leptos::prelude::*;

/// How often the ledger re-reads itself while the screen is open, in
/// milliseconds.
///
/// Five seconds, which is well under the time it takes to wonder whether the
/// screen is broken and well over the cost of answering. See [`watch_wells`].
///
/// Browser-only, like the timer it feeds: the server renders this screen once
/// and never polls, so under `ssr` this is genuinely dead rather than merely
/// unused.
#[cfg(feature = "hydrate")]
const LEDGER_POLL_MS: u64 = 5_000;

/// `/resources` -- everything this machine shares, and the form for one more.
#[component]
pub fn SharedResourcesView() -> impl IntoView {
    // Bumped after a successful declaration, which is what re-runs the fetch.
    // A counter rather than `Resource::refetch` because the form needs to
    // re-read the ledger from a different component than the one holding it.
    let revision = RwSignal::new(0_u32);
    let ledger = Resource::new(
        move || revision.get(),
        |_| async move { shared_resources().await.ok() },
    );

    // And bumped on a timer too, so a well that comes up *after* this screen
    // was opened appears on it. See `watch_wells`.
    watch_wells(revision);

    // The last answer we actually believe, republished *only* when it differs.
    //
    // Why this exists: the poll changes the resource's source every five
    // seconds, which returns it to pending -- so rendering straight from
    // `ledger` inside a `Suspense` tore the whole ledger out of the DOM and put
    // the loading sentence back in its place on every tick, then rebuilt every
    // row from scratch. The screen flashed, and a click could land on nothing.
    //
    // `ResourceInventory` is `PartialEq`, so a tick that finds nothing new
    // touches no signal and therefore no DOM. The screen is still while the
    // machine is still, and repaints once when a well really starts or stops.
    // Holding the last good answer also means a failed tick keeps showing the
    // previous truth rather than flipping to a fallback.
    let latest = RwSignal::new(Option::<ResourceInventory>::None);
    Effect::new(move |_| {
        if let Some(fresh) = ledger.get().flatten() {
            if latest.with(|held| held.as_ref() != Some(&fresh)) {
                latest.set(Some(fresh));
            }
        }
    });

    // Which row's detail is open, as `scope name` -- unique across the whole
    // ledger, where a bare name is not: two projects may both declare `db`.
    let selected = RwSignal::new(Option::<String>::None);
    let composing = RwSignal::new(false);

    view! {
        <div class="wells-view">
            <header class="wells-head">
                <div class="wells-heading">
                    <h1>"Shared resources"</h1>
                    <p class="wells-sub">
                        "Containers Kingdom starts once and every agent reaches at \
                         one address \u{2014} started when the first plan needs them, \
                         stopped when the last one is done."
                    </p>
                </div>
                <div class="wells-actions">
                    <a class="wells-back" href="/" title="Back to the realm">
                        "\u{2190} Realm"
                    </a>
                    <button
                        class="wells-new"
                        class:open=move || composing.get()
                        on:click=move |_| composing.update(|c| *c = !*c)
                    >
                        {move || if composing.get() { "Cancel" } else { "+ New resource" }}
                    </button>
                </div>
            </header>

            <Show when=move || composing.get()>
                <NewResource
                    on_declared=move || {
                        composing.set(false);
                        revision.update(|r| *r += 1);
                    }
                />
            </Show>

            // First load only: once there is an inventory it is never taken
            // away again, so nothing here blanks on a refetch.
            //
            // The view reads `latest` and never the resource, so it never
            // subscribes to the resource's pending state and therefore never
            // returns to the fallback. The one cost is that the server, where
            // no effect runs, renders the loading line and the browser fills it
            // in on arrival -- paid once, on entry, instead of every five
            // seconds for as long as the screen is open.
            <Show
                when=move || latest.with(|held| held.is_some())
                fallback=|| view! {
                    <p class="wells-loading">"Reading the manifests\u{2026}"</p>
                }
            >
                {move || {
                    let inventory = latest.get().unwrap_or_default();

                    // One banner for the whole screen rather than a confusing
                    // "not started" on every row. See `services::docker_trouble`.
                    let docker = inventory.docker_trouble.clone();
                    let troubles = inventory.troubles.clone();
                    let empty = inventory.is_empty();

                    // Grouped by owner, in the order the server returned them:
                    // host first, then cities. A `BTreeMap` would re-sort the
                    // machine's own wells into the middle of the alphabet.
                    let mut groups: Vec<(String, Vec<SharedResource>)> = Vec::new();
                    for resource in inventory.resources {
                        let owner = resource.owner();
                        match groups.last_mut() {
                            Some((last, list)) if last == &owner => list.push(resource),
                            _ => groups.push((owner, vec![resource])),
                        }
                    }

                    let chosen = groups
                        .iter()
                        .flat_map(|(_, list)| list.iter())
                        .find(|r| Some(row_key(r)) == selected.get())
                        .cloned();

                    view! {
                        <Show when={
                            let docker = docker.clone();
                            move || docker.is_some()
                        }>
                            <p class="wells-banner">{docker.clone().unwrap_or_default()}</p>
                        </Show>

                        <For
                            each=move || troubles.clone()
                            key=|t| t.manifest_path.clone()
                            let:trouble
                        >
                            // A manifest that does not parse used to be silent
                            // until an agent's first turn was refused, minutes
                            // in, with a message about the model. Here it is a
                            // row with the path in it.
                            //
                            // The detail already opens with that path -- the
                            // reader attaches it, being the only layer that
                            // knows which of the two manifests it read -- so the
                            // row does not print it a second time underneath.
                            <div class="wells-trouble">
                                <span class="wells-trouble-mark">"\u{26a0}"</span>
                                <p class="wells-trouble-detail">{trouble.detail.clone()}</p>
                            </div>
                        </For>

                        <Show when=move || empty>
                            <p class="wells-empty">
                                "Nothing is shared yet. A shared resource is a container \
                                 every agent reaches at one address instead of each \
                                 starting its own \u{2014} a database, a cache, a message \
                                 broker. Declare one above."
                            </p>
                        </Show>

                        <div class="wells-panes">
                            <ul class="wells-groups">
                                <For
                                    each=move || groups.clone()
                                    key=|(owner, list)| (owner.clone(), list.len())
                                    let:group
                                >
                                    {
                                        let (owner, list) = group;
                                        let machine = list
                                            .first()
                                            .is_some_and(|r| r.scope == ServiceScope::Host);
                                        view! {
                                            <li class="wells-group">
                                                <p
                                                    class="wells-owner"
                                                    class:machine=machine
                                                    title=if machine {
                                                        "Declared in your profile and offered to \
                                                         every project you open"
                                                    } else {
                                                        "Declared in this project's own repository"
                                                    }
                                                >
                                                    {owner}
                                                </p>
                                                <ul class="wells-rows">
                                                    <For
                                                        each=move || list.clone()
                                                        key=|r| row_key(r)
                                                        let:resource
                                                    >
                                                        {
                                                            let key = row_key(&resource);
                                                            let mine = key.clone();
                                                            view! {
                                                                <li>
                                                                    <button
                                                                        class="wells-entry"
                                                                        class:chosen=move || {
                                                                            selected.get() == Some(mine.clone())
                                                                        }
                                                                        on:click=move |_| {
                                                                            selected.set(Some(key.clone()))
                                                                        }
                                                                    >
                                                                        <span class="wells-entry-name">
                                                                            {resource.spec.name.clone()}
                                                                        </span>
                                                                        <span class="wells-entry-image">
                                                                            {resource.spec.image.clone()}
                                                                        </span>
                                                                        // The address an agent uses,
                                                                        // on the row rather than only
                                                                        // in the detail: it is the one
                                                                        // fact the King is scanning
                                                                        // for, and it is the same for
                                                                        // every agent on the project.
                                                                        <code class="wells-entry-address">
                                                                            {format!(
                                                                                "localhost:{}",
                                                                                resource.spec.port,
                                                                            )}
                                                                        </code>
                                                                        <span
                                                                            class="wells-state"
                                                                            class:running=matches!(
                                                                                resource.state, ServiceState::Running
                                                                            )
                                                                        >
                                                                            {resource.state.label()}
                                                                        </span>
                                                                    </button>
                                                                </li>
                                                            }
                                                        }
                                                    </For>
                                                </ul>
                                            </li>
                                        }
                                    }
                                </For>
                            </ul>

                            <div class="wells-detail">
                                {match chosen {
                                    Some(resource) => Detail(DetailProps { resource }).into_any(),
                                    None => view! {
                                        <p class="wells-nothing">
                                            "Pick a resource to see its address, the plans \
                                             using it, and the file it is declared in."
                                        </p>
                                    }
                                    .into_any(),
                                }}
                            </div>
                        </div>
                    }
                }}
            </Show>
        </div>
    }
}

/// Re-reads the ledger on a timer for as long as this screen is open.
///
/// # Why the screen needs this at all
///
/// A well is raised when a kingdom or a plan opens, and that work is
/// *spawned* -- pulling an image can take minutes, and the King is not made to
/// wait on it. So a resource that reads `not started` when this screen mounts
/// may be up moments later, and the fetch here is one-shot. Without a poll the
/// King would be looking at a screen whose whole job is saying what is running,
/// showing him something that stopped being true seconds after he arrived, with
/// a reload as the only cure.
///
/// A timer rather than a socket: this state changes on the order of *minutes*,
/// belongs to no plan, and would mean a new field on `PlanPulse` that moves for
/// reasons unrelated to any plan. `inventory` also asks Docker exactly one
/// question for the whole screen, so a tick is cheap.
///
/// # Why the handle is owned
///
/// The same reason `conversation.rs::ticking_clock` owns its own: an interval
/// left running after the screen is gone keeps fetching forever, invisibly,
/// because nothing it updates is on screen to look wrong. `on_cleanup` clears
/// it when the King navigates away.
fn watch_wells(revision: RwSignal<u32>) {
    #[cfg(feature = "hydrate")]
    {
        if let Ok(handle) = leptos::leptos_dom::helpers::set_interval_with_handle(
            move || revision.update(|r| *r += 1),
            std::time::Duration::from_millis(LEDGER_POLL_MS),
        ) {
            on_cleanup(move || handle.clear());
        }
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = revision;
}

/// A row's identity across the whole ledger.
///
/// Scope and name together, because a bare name is not unique: two projects may
/// each declare a `db`, and keying on the name alone would highlight both.
fn row_key(resource: &SharedResource) -> String {
    format!(
        "{}:{}:{}",
        resource.scope.wire_name(),
        resource.city_name.clone().unwrap_or_default(),
        resource.spec.name
    )
}

/// Everything known about one resource, including where to go and edit it.
#[component]
fn Detail(resource: SharedResource) -> impl IntoView {
    let SharedResource {
        spec,
        scope,
        manifest_path,
        state,
        address,
        container,
        users,
        city_name,
        ..
    } = resource;

    let running = matches!(state, ServiceState::Running);
    let logs = format!("docker logs {container}");
    // The address an agent actually types. True whatever the state: it is a
    // property of the declaration -- the service's own port -- rather than of
    // the container, which is the entire point of relaying it onto the plan's
    // loopback.
    let local = format!("localhost:{}", spec.port);

    view! {
        <div class="well-card">
            <div class="well-title">
                <span class="well-name">{spec.name.clone()}</span>
                <span class="wells-state" class:running=running>{state.label()}</span>
            </div>

            // The thing the King came here to know, said first and said plainly.
            // Everything below is detail behind it.
            <div class="well-reach">
                <p class="well-reach-label">"Every agent reaches it at"</p>
                <code class="well-reach-address">{local.clone()}</code>
                <p class="well-reach-note">
                    "A plan with a network of its own has this service on its own \
                     loopback, at the port above \u{2014} so it connects the \
                     ordinary way, with nothing to configure. A plan on the \
                     machine's network is given the container address below \
                     instead, because a relay there would take the port from you."
                </p>
            </div>

            <dl class="well-facts">
                <dt>"Image"</dt>
                <dd><code>{spec.image.clone()}</code></dd>

                <dt>"Shared with"</dt>
                <dd>
                    {match scope {
                        ServiceScope::Host => {
                            "Every project on this machine".to_string()
                        }
                        ServiceScope::City => match &city_name {
                            Some(name) => format!("Every plan working on {name}"),
                            None => "Every plan on this project".to_string(),
                        },
                    }}
                </dd>

                <dt>"From your own machine"</dt>
                <dd>
                    {match &address {
                        // Not a link: this is Mongo or Postgres as often as it
                        // is HTTP, and a browser cannot open those. Selectable
                        // text an address is copied out of is the honest
                        // affordance -- the same call `ports_badge.rs` makes.
                        Some(address) => view! { <code>{address.clone()}</code> }.into_any(),
                        None => view! {
                            <span class="well-absent">
                                "Assigned when it first starts. Nothing is published \
                                 on your localhost, so it can never take a port \
                                 from you."
                            </span>
                        }
                        .into_any(),
                    }}
                </dd>

                <dt>"Container"</dt>
                <dd>
                    <code>{container.clone()}</code>
                    <span class="well-hint">{logs}</span>
                </dd>

                <dt>"Data"</dt>
                <dd>
                    {match &spec.volume {
                        Some(volume) => format!(
                            "Kept in the named volume `{volume}`, which outlives the container."
                        ),
                        // Stated rather than omitted: "no volume" is a decision
                        // with a consequence, and the consequence is that the
                        // data goes when the container does.
                        None => "No volume \u{2014} data goes when the container is removed."
                            .to_string(),
                    }}
                </dd>

                <dt>"In use by"</dt>
                <dd>
                    <Show
                        when={
                            let users = users.clone();
                            move || !users.is_empty()
                        }
                        fallback=|| view! {
                            <span class="well-absent">"No plan is drawing from it right now."</span>
                        }
                    >
                        <ul class="well-users">
                            <For each={
                                let users = users.clone();
                                move || users.clone()
                            } key=|t: &String| t.clone() let:title>
                                <li>{title}</li>
                            </For>
                        </ul>
                    </Show>
                </dd>
            </dl>

            // The thing this screen exists to print. The manifest is the source
            // of truth: changing or removing a resource is done by editing this
            // file, and Kingdom picks the change up next time the service is
            // started.
            <p class="well-section">"Declared in"</p>
            <code class="wells-path">{manifest_path.clone()}</code>
            <p class="well-note">
                "Edit that file to change or remove this. A change takes effect the \
                 next time the service starts \u{2014} not the moment it is saved, and \
                 not for a container that is already up."
            </p>
        </div>
    }
}

/// The form: declare one more, and say what level it runs at.
///
/// # Why it asks for so little
///
/// Image, name, and where it is shared. A resource is reached at `localhost` on
/// its own port, so there is no address to plumb and no variable to name -- and
/// a well-known image brings its own port and its own data directory, so those
/// are filled in rather than asked for. What is left is the two facts Kingdom
/// genuinely cannot infer: what to run, and how far to share it.
#[component]
fn NewResource(
    /// Called once the manifest has actually been written.
    on_declared: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let cities = move || state.kingdom.with(|k| k.cities.clone());

    let scope = RwSignal::new(ServiceScope::City);
    let city = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let image = RwSignal::new(String::new());
    // Both are filled in from the image when it is one Kingdom knows, and both
    // stay editable. `None` means "he has not touched this", which is different
    // from `Some("")` -- a box he deliberately emptied. Without that
    // distinction a cleared volume would refill itself from the name and the
    // King could never ask for data that goes with the container.
    let typed_port = RwSignal::new(Option::<String>::None);
    let typed_volume = RwSignal::new(Option::<String>::None);
    let error = RwSignal::new(Option::<String>::None);
    let written = RwSignal::new(Option::<String>::None);

    // Pre-select whatever city the rail has, so the common case is one fewer
    // decision.
    Effect::new(move |_| {
        if let Some(selected) = state.selected.get() {
            if city.get_untracked().is_empty() {
                city.set(selected.to_string());
            }
        }
    });

    // What Kingdom knows about the image, which is what saves the King two
    // fields. `None` for anything unrecognised, which simply means he is asked
    // for the port.
    let known = Memo::new(move |_| kingdom_core::services::known_image(image.get().trim()));

    // The port: his if he has typed in the box, the image's otherwise.
    let port = Memo::new(move |_| match typed_port.get() {
        Some(typed) => typed.trim().to_string(),
        None => known.get().map(|k| k.port.to_string()).unwrap_or_default(),
    });

    // The volume: his if he has touched the box -- including emptying it, which
    // is how "let the data go with the container" is asked for -- and otherwise
    // one derived from the resource itself. Named by default rather than left
    // empty, which is the opposite of what this form used to do: losing a
    // database because the King did not fill in an optional box is the worse of
    // the two mistakes, and a volume on a cache costs nothing.
    let volume = Memo::new(move |_| {
        if let Some(typed) = typed_volume.get() {
            return typed.trim().to_string();
        }
        let name = name.get().trim().to_string();
        if name.is_empty() {
            return String::new();
        }
        let where_ = match scope.get() {
            ServiceScope::Host => "host",
            ServiceScope::City => "project",
        };
        format!("kingdom-{where_}-{name}-data")
    });

    // What is about to be written, exactly. Built through the same `render`
    // the server appends, so the preview cannot show one thing and the file
    // receive another.
    let spec = Memo::new(move |_| ServiceSpec {
        name: name.get().trim().to_string(),
        image: image.get().trim().to_string(),
        port: port.get().parse().unwrap_or(0),
        volume: {
            let v = volume.get();
            (!v.is_empty()).then_some(v)
        },
    });

    // Said while he types rather than after the write, and asked of
    // `kingdom_core` so the form refuses exactly what the parser refuses.
    let complaint = Memo::new(move |_| {
        let spec = spec.get();
        if spec.name.is_empty() {
            return Some("Give it a name \u{2014} that is what the container is called.");
        }
        if !kingdom_core::services::is_usable_name(&spec.name) {
            return Some("A name may use letters, digits, `-` and `_` only.");
        }
        if spec.image.is_empty() {
            return Some("Name an image to run, tag included \u{2014} `postgres:16`.");
        }
        if spec.port == 0 {
            return Some(
                "Give the port this service listens on \u{2014} that is the port \
                 agents will reach it at.",
            );
        }
        if scope.get() == ServiceScope::City && city.get().is_empty() {
            return Some("Pick the project this belongs to.");
        }
        None
    });

    let declare = Action::new(move |(): &()| {
        let spec = spec.get_untracked();
        let scope = scope.get_untracked();
        let city = city.get_untracked();
        let volume = volume.get_untracked();
        async move {
            error.set(None);
            let result = declare_shared_resource(
                scope.wire_name().to_string(),
                (scope == ServiceScope::City).then_some(city),
                spec.name.clone(),
                spec.image.clone(),
                spec.port,
                volume,
            )
            .await;

            match result {
                Ok(path) => {
                    written.set(Some(path));
                    on_declared();
                }
                Err(e) => error.set(Some(plainly(&e.to_string()))),
            }
        }
    });

    view! {
        <form
            class="well-form"
            on:submit=move |ev| {
                ev.prevent_default();
                if complaint.get().is_none() {
                    declare.dispatch(());
                }
            }
        >
            <div class="well-field well-scope">
                <label>"Shared with"</label>
                <div class="scope-toggle">
                    <button
                        type="button"
                        class="scope-btn"
                        class:active=move || scope.get() == ServiceScope::City
                        on:click=move |_| scope.set(ServiceScope::City)
                    >
                        <span class="scope-name">"This project"</span>
                        <span class="scope-detail">
                            "Declared in the project's own repository, so every \
                             clone of it gets the same one."
                        </span>
                    </button>
                    <button
                        type="button"
                        class="scope-btn"
                        class:active=move || scope.get() == ServiceScope::Host
                        on:click=move |_| scope.set(ServiceScope::Host)
                    >
                        <span class="scope-name">"Every project on this machine"</span>
                        <span class="scope-detail">
                            "Declared in your own profile, never committed anywhere."
                        </span>
                    </button>
                </div>
            </div>

            <Show when=move || scope.get() == ServiceScope::City>
                <div class="well-field">
                    <label for="well-city">"Project"</label>
                    <select
                        id="well-city"
                        on:change=move |ev| city.set(event_target_value(&ev))
                        prop:value=move || city.get()
                    >
                        <option value="">"Pick a project\u{2026}"</option>
                        <For each=cities key=|c: &kingdom_core::City| c.id.clone() let:option>
                            <option value=option.id.to_string()>{option.name.clone()}</option>
                        </For>
                    </select>
                </div>
            </Show>

            // Image first: it is the decision, and it is what fills in the two
            // fields beside it.
            <div class="well-row">
                <div class="well-field">
                    <label for="well-image">"Image"</label>
                    <input
                        id="well-image"
                        placeholder="postgres:16"
                        on:input=move |ev| image.set(event_target_value(&ev))
                        prop:value=move || image.get()
                    />
                </div>
                <div class="well-field">
                    <label for="well-name">"Name"</label>
                    <input
                        id="well-name"
                        placeholder="db"
                        on:input=move |ev| name.set(event_target_value(&ev))
                        prop:value=move || name.get()
                    />
                </div>
                <div class="well-field well-narrow">
                    <label for="well-port">"Port"</label>
                    <input
                        id="well-port"
                        placeholder="5432"
                        inputmode="numeric"
                        // Filled in from the image, and overwritable. Bound to
                        // the effective port rather than to what he typed, so a
                        // recognised image visibly answers the question.
                        on:input=move |ev| typed_port.set(Some(event_target_value(&ev)))
                        prop:value=move || port.get()
                    />
                </div>
            </div>

            // The outcome, stated while he is still deciding. This is the whole
            // promise of the feature and the reason the form is this short.
            <Show
                when=move || spec.get().port != 0
                fallback=|| view! {
                    <p class="well-hint">
                        "Kingdom knows the port for `postgres`, `mongo`, `mysql`, \
                         `mariadb` and `redis`. For anything else, name the port it \
                         listens on."
                    </p>
                }
            >
                <p class="well-outcome">
                    "Agents will reach it at "
                    <code>{move || format!("localhost:{}", spec.get().port)}</code>
                    ". Nothing is published on your own localhost, so it cannot take \
                     a port from you."
                </p>
            </Show>

            <div class="well-field">
                <label for="well-volume">"Where its data is kept"</label>
                <input
                    id="well-volume"
                    placeholder="named automatically"
                    on:input=move |ev| typed_volume.set(Some(event_target_value(&ev)))
                    prop:value=move || volume.get()
                />
                <p class="well-hint">
                    "A named Docker volume, so the data outlives the container. \
                     Named for you from the resource; clear it and the data goes \
                     when the container does."
                </p>
            </div>

            // The exact text about to be appended. The King is the one who owns
            // this file, so he sees what is going into it before it goes.
            <p class="well-section">"What will be added to the file"</p>
            <pre class="well-preview">{move || spec.get().render()}</pre>

            <div class="well-submit">
                <button type="submit" class="well-declare" disabled=move || complaint.get().is_some()>
                    "Declare it"
                </button>
                <Show when=move || complaint.get().is_some()>
                    <span class="well-complaint">{move || complaint.get().unwrap_or_default()}</span>
                </Show>
            </div>

            <Show when=move || error.get().is_some()>
                <p class="well-error">{move || error.get().unwrap_or_default()}</p>
            </Show>
            <Show when=move || written.get().is_some()>
                <p class="well-written">
                    "Written to "
                    <code>{move || written.get().unwrap_or_default()}</code>
                </p>
            </Show>
        </form>
    }
}

/// A server function's error as a sentence rather than as plumbing.
///
/// `ServerFnError` renders as `error running server function: <message>`, and
/// every message this form can produce is already written for the King -- "that
/// name is taken, here is the file". The prefix only tells him something broke
/// inside a mechanism he did not know existed, ahead of the sentence that
/// actually says what to do.
fn plainly(message: &str) -> String {
    for noise in [
        "error running server function: ",
        "error deserializing server function arguments: ",
    ] {
        if let Some(rest) = message.strip_prefix(noise) {
            return rest.to_string();
        }
    }
    message.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The King reads the sentence written for him, not the plumbing wrapped
    /// around it.
    #[test]
    fn a_server_error_is_shown_as_the_sentence_it_carries() {
        assert_eq!(
            plainly("error running server function: `cache` is already declared in /x.toml."),
            "`cache` is already declared in /x.toml."
        );
        // Anything not wrapped is passed through untouched, so an unfamiliar
        // failure is never silently truncated.
        assert_eq!(plainly("the disk is full"), "the disk is full");
    }

    /// Two projects may each declare a `db`, so a row's identity cannot be its
    /// name -- selecting one would light up both.
    #[test]
    fn two_projects_may_each_have_a_db() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "mongo:7".to_string(),
            port: 27017,
            volume: None,
        };
        let row = |city: Option<&str>, scope| SharedResource {
            spec: spec.clone(),
            scope,
            city: None,
            city_name: city.map(str::to_string),
            manifest_path: String::new(),
            state: ServiceState::Idle,
            address: None,
            container: String::new(),
            users: Vec::new(),
        };

        assert_ne!(
            row_key(&row(Some("shopfront"), ServiceScope::City)),
            row_key(&row(Some("ledger"), ServiceScope::City)),
        );
        assert_ne!(
            row_key(&row(None, ServiceScope::Host)),
            row_key(&row(Some("shopfront"), ServiceScope::City)),
        );
    }
}
