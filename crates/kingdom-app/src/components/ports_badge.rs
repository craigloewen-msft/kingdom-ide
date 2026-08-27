//! The ports badge: what this plan is listening on, what its city shares, and
//! where to reach both.
//!
//! The visible half of network isolation. A plan with its own namespace can
//! bind `:3000` freely -- but that `:3000` is not one the King's browser can
//! open, so without this the isolation would be a feature he is told about and
//! cannot use.
//!
//! It carries the city's **shared services** for the same reason: a container
//! shared by five plans is reachable from the King's own machine at its address, and an
//! address nobody is shown is an address nobody can use.
//!
//! # Why a badge and not a panel
//!
//! This answers "is anything running, and where do I click?", which is a
//! glance, not a reading task. The three full-height panels beside the
//! transcript are for things read *against* the conversation; a list of two
//! ports is not one of them. So it is a count in the header with a popover
//! behind it, in the manner of the context meter beside it.

use leptos::prelude::*;

/// What a plan has forwarded to the host right now.
#[component]
pub fn PortsBadge(
    /// The plan's live ports, as the watch socket last reported them.
    ports: Memo<Vec<kingdom_core::PortForward>>,
    /// The shared services this plan's city has standing. Shown as "wells".
    services: Memo<Vec<kingdom_core::SharedService>>,
    /// Whether this plan has a network of its own at all.
    isolated: Memo<bool>,
) -> impl IntoView {
    let (open, set_open) = signal(false);

    // Shown for an isolated plan, and for any plan whose city shares something.
    // The second half is why this is not simply `isolated`: a plan on the
    // shared network still draws from the same database as its four siblings,
    // and that is worth a glance even though its own ports are not.
    let shown = Memo::new(move |_| isolated.get() || !services.get().is_empty());
    // Both counts together: the badge answers "how many things are running that
    // I might want to reach", and a shared service is one of those things.
    let count = Memo::new(move |_| {
        let own = if isolated.get() { ports.get().len() } else { 0 };
        own + services.get().len()
    });
    // Split from `count` because `>` inside the `view!` macro reads as a tag
    // close. A named memo is clearer than the parenthesised comparison that
    // parses but trips the unused-parens lint.
    let live = Memo::new(move |_| count.get() > 0);

    view! {
        <Show when=move || shown.get()>
            <div class="ports-holder">
                <button
                    class="ports-toggle"
                    class:open=move || open.get()
                    class:live=move || live.get()
                    title="Ports this plan has open, and the services its project shares"
                    on:click=move |_| set_open.update(|o| *o = !*o)
                >
                    "\u{1F50C}"
                    // The count is the whole point of the badge: it is what
                    // makes "something is running" visible without opening
                    // anything. Hidden at zero rather than shown as "0", which
                    // would be a number to read rather than a state to notice.
                    <Show when=move || live.get()>
                        <span class="ports-count">{move || count.get()}</span>
                    </Show>
                </button>

                <Show when=move || open.get()>
                    <div class="ports-popover">
                        <div class="ports-head">
                            <span class="ports-title">"Ports and shared services"</span>
                            <button
                                class="ports-close"
                                title="Close"
                                on:click=move |_| set_open.set(false)
                            >
                                "\u{00d7}"
                            </button>
                        </div>

                        // The shared services first. They are the thing the King is least
                        // likely to know the address of, and the thing shared
                        // with other plans -- so it is the fact that changes
                        // what he does next.
                        <Show when=move || !services.get().is_empty()>
                            <p class="ports-section">"This project's shared services"</p>
                            <ul class="wells-list">
                                <For
                                    each=move || services.get()
                                    key=|s| (s.name.clone(), s.address.clone())
                                    let:service
                                >
                                    <li class="wells-row">
                                        <span class="wells-name">{service.name.clone()}</span>
                                        // Not a link: this is Mongo or Postgres
                                        // as often as it is HTTP, and a browser
                                        // cannot open those. Selectable text an
                                        // address is copied out of is the
                                        // honest affordance.
                                        <code class="wells-address">{service.address.clone()}</code>
                                        <span class="wells-image">{service.image.clone()}</span>
                                        // Who else is in here -- the question
                                        // the King actually has before he
                                        // changes something in a shared
                                        // database.
                                        <span
                                            class="wells-drawers"
                                            title="How many plans are using this right now"
                                        >
                                            {move || match service.users {
                                                1 => "1 plan".to_string(),
                                                n => format!("{n} plans"),
                                            }}
                                        </span>
                                    </li>
                                </For>
                            </ul>
                            <p class="ports-note">
                                "One set of services, shared by every plan on this \
                                 project -- started when the first plan needed them \
                                 and stopped when the last one is done. You can reach \
                                 them at these addresses too; they are not published \
                                 on your localhost."
                            </p>
                        </Show>

                        <Show when=move || isolated.get()>
                            <p class="ports-section">"This plan's ports"</p>
                            <Show
                                when=move || !ports.get().is_empty()
                                fallback=|| view! {
                                    <p class="ports-empty">
                                        "Nothing is listening yet. When this plan starts a \
                                         server, its port appears here with an address you \
                                         can open."
                                    </p>
                                }
                            >
                            <ul class="ports-list">
                                <For
                                    each=move || ports.get()
                                    key=|forward| (forward.guest, forward.host)
                                    let:forward
                                >
                                    <li class="ports-row">
                                        // What the agent thinks it bound, which
                                        // is the number it will have printed in
                                        // its own logs.
                                        <span class="ports-guest">{forward.guest}</span>
                                        <span class="ports-arrow">"\u{2192}"</span>
                                        // A real link: the point of the forward
                                        // is that this opens.
                                        <a
                                            class="ports-link"
                                            href=format!("http://127.0.0.1:{}", forward.host)
                                            target="_blank"
                                            rel="noreferrer"
                                        >
                                            {format!("127.0.0.1:{}", forward.host)}
                                        </a>
                                    </li>
                                </For>
                            </ul>
                            </Show>

                            <p class="ports-note">
                                "This plan has its own network, so these ports are its \
                                 own -- they do not collide with yours or with another \
                                 plan's. They stay open and clickable while it awaits \
                                 your review, and close only when it is merged or \
                                 archived."
                            </p>
                        </Show>
                    </div>
                </Show>
            </div>
        </Show>
    }
}
