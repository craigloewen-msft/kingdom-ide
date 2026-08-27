//! The ports badge: what this plan is listening on, and where to reach it.
//!
//! The visible half of network isolation. A plan with its own namespace can
//! bind `:3000` freely -- but that `:3000` is not one the King's browser can
//! open, so without this the isolation would be a feature he is told about and
//! cannot use.
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
    /// Whether this plan has a network of its own at all.
    isolated: Memo<bool>,
) -> impl IntoView {
    let (open, set_open) = signal(false);

    // Shown only for an isolated plan. On a shared-network plan every port the
    // agent opens is already on the King's own loopback at the number the agent
    // used, so a badge would be reporting a fact he already has.
    view! {
        <Show when=move || isolated.get()>
            <div class="ports-holder">
                <button
                    class="ports-toggle"
                    class:open=move || open.get()
                    class:live=move || !ports.get().is_empty()
                    title="Ports this plan has open, and where to reach them"
                    on:click=move |_| set_open.update(|o| *o = !*o)
                >
                    "\u{1F50C}"
                    // The count is the whole point of the badge: it is what
                    // makes "something is running" visible without opening
                    // anything. Hidden at zero rather than shown as "0", which
                    // would be a number to read rather than a state to notice.
                    <Show when=move || !ports.get().is_empty()>
                        <span class="ports-count">{move || ports.get().len()}</span>
                    </Show>
                </button>

                <Show when=move || open.get()>
                    <div class="ports-popover">
                        <div class="ports-head">
                            <span class="ports-title">"This plan's ports"</span>
                            <button
                                class="ports-close"
                                title="Close"
                                on:click=move |_| set_open.set(false)
                            >
                                "\u{00d7}"
                            </button>
                        </div>

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
                             plan's."
                        </p>
                    </div>
                </Show>
            </div>
        </Show>
    }
}
