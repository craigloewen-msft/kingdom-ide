//! The court's findings on a pull request, as the King reads them.
//!
//! The sibling of [`super::ProposalCard`] and deliberately not a variant of it:
//! a proposal asks for authority and ends in "Start with this", and nothing
//! here grants anything. What the King does with findings happens outside
//! Kingdom -- on GitHub, in his own words -- so the card's only controls are
//! for reading: the link to the pull request, and the way back to the
//! conversation that produced it.
//!
//! Like the proposal card, **nothing here talks to the server**. Asking a
//! follow-up is the chamber's composer, and closing the review is its Done
//! picker, both already owned by `ConversationBody`.

use crate::components::Prose;
use kingdom_core::{PullRequestRef, PullRequestReview};
use leptos::prelude::*;

#[component]
pub fn ReviewReport(
    review: PullRequestReview,
    /// The pull request the findings are about, as it was checked out.
    pull: PullRequestRef,
) -> impl IntoView {
    // Opens full, for the proposal's reason: reading this is the King's one
    // job at this moment. Collapsible, because the deeds behind it -- the
    // screenshots especially -- are often what he wants to check.
    let (full, set_full) = signal(true);
    let revised = review.revision > 1;
    let recommendation = review.recommendation;

    view! {
        <div class="chat-proposal chat-report" class:full=move || full.get()>
            <div class="proposal-head">
                <span class="proposal-mark">"\u{1F50E}"</span>
                <span class="proposal-who">
                    {if revised { "The court's findings, revised" } else { "The court's findings" }}
                </span>
                <span class=format!("report-verdict verdict-{}", recommendation.css_suffix())>
                    {recommendation.label()}
                </span>
                <span class="proposal-at">
                    {crate::components::conversation::clock(review.at)}
                </span>
                <button
                    class="proposal-expand"
                    title="Show the conversation behind these findings"
                    on:click=move |_| set_full.update(|f| *f = !*f)
                >
                    {move || if full.get() { "Show conversation" } else { "Read in full" }}
                </button>
            </div>

            <p class="proposal-title">{review.title}</p>
            <p class="report-of">
                {format!("#{} \u{2014} {} \u{00b7} at ", pull.number, pull.title)}
                <code>{pull.short_sha().to_string()}</code>
                {format!(" \u{00b7} {} \u{2192} {}", pull.head, pull.base)}
            </p>

            <Prose text=without_headline(&review.body) class="proposal-body"/>

            <div class="proposal-actions">
                <a
                    class="report-open"
                    href=pull.url.clone()
                    target="_blank"
                    rel="noopener noreferrer"
                    title="Open the pull request on GitHub, to act on these findings yourself"
                >
                    {format!("Open #{} on GitHub \u{2197}", pull.number)}
                </a>
                <span class="proposal-hint">
                    "Nothing has been posted to GitHub. Ask about any of it below, or \
                     close the review with Done."
                </span>
            </div>
        </div>
    }
}

/// The report without its own `# H1`, which the card already shows as its
/// title. Printed twice, the headline read as two reports.
fn without_headline(body: &str) -> String {
    let mut lines = body.lines();
    let mut out = Vec::new();
    for line in lines.by_ref() {
        if line.trim().starts_with("# ") {
            break;
        }
        // Blank lines before the headline go with it; anything else means the
        // report has no leading headline, so it is kept whole.
        if !line.trim().is_empty() {
            return body.to_string();
        }
    }
    out.extend(lines);
    out.join("\n").trim_start().to_string()
}

#[cfg(test)]
mod tests {
    use super::without_headline;

    #[test]
    fn the_headline_is_shown_once() {
        assert_eq!(
            without_headline("# Works\n\n## Summary\nok"),
            "## Summary\nok"
        );
        assert_eq!(without_headline("\n# Works\nbody"), "body");
        assert_eq!(without_headline("Preamble\n# Works"), "Preamble\n# Works");
    }
}
