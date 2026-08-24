//! Assembling one catalogue out of every provider.
//!
//! This file used to *be* the catalogue: it fetched Copilot's `/models`, parsed
//! it, and pinned a hard-coded mock entry at the head of the list. Both of those
//! now belong to the providers themselves, and what is left here is the only
//! part that was ever about the catalogue as a whole -- putting the lists
//! together, and deciding what the King lands on before he has chosen.

use super::providers;
use kingdom_core::{CredentialState, ModelCatalogue};

/// The last-resort default. Only reached if every provider offers nothing,
/// which today cannot happen -- the mock always does.
const FALLBACK_ID: &str = super::mock::MODEL_ID;

/// Every model the King can choose between, from every backend.
pub async fn catalogue() -> ModelCatalogue {
    let mut options = Vec::new();
    let mut credential = CredentialState::Ready;
    let mut details = Vec::new();

    for provider in providers() {
        let reported = provider.catalogue().await;
        options.extend(reported.options);
        credential = worse_of(credential, reported.credential);
        if !reported.detail.is_empty() {
            details.push(reported.detail);
        }
    }

    ModelCatalogue {
        default_id: default_id(&options),
        options,
        credential,
        detail: details.join(" "),
    }
}

/// What the picker opens on before the King has chosen.
///
/// The environment's `KINGDOM_MODEL` wins, but only if it names a model
/// somebody actually serves -- otherwise it is a default pointing at nothing.
/// Failing that: the best on offer, which means the first recommended model,
/// then simply the first. With no working credential the only entry is the
/// mock, so a fresh clone still drafts offline with no setup -- not because the
/// mock is privileged, but because it is what is left.
fn default_id(options: &[kingdom_core::ModelOption]) -> String {
    if let Some(wanted) = super::preferred_model_id() {
        if options.iter().any(|o| o.id == wanted) {
            return wanted;
        }
    }

    options
        .iter()
        .find(|o| o.recommended)
        .or_else(|| options.first())
        .map(|o| o.id.clone())
        .unwrap_or_else(|| FALLBACK_ID.to_string())
}

/// Combines two providers' credential states into the one the picker shows.
///
/// Deliberately pessimistic. The mock is always `Ready`, so an optimistic rule
/// would report a healthy credential while Copilot's is broken -- and the King
/// would learn his token had expired only by wondering where the models went.
fn worse_of(a: CredentialState, b: CredentialState) -> CredentialState {
    fn rank(state: CredentialState) -> u8 {
        match state {
            CredentialState::Ready => 0,
            CredentialState::Missing => 1,
            CredentialState::Failed => 2,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::ModelOption;

    fn option(id: &str, recommended: bool) -> ModelOption {
        ModelOption {
            id: id.to_string(),
            label: id.to_string(),
            vendor: "Test".to_string(),
            context_window: 0,
            recommended,
            efforts: Vec::new(),
        }
    }

    /// A fresh clone with no credential must still be able to draft, *and* must
    /// still be told its credential is broken. Those two used to be separate
    /// mechanisms -- a hard-coded mock entry, and a badge in the decree bar.
    /// Both are gone, so this is the only thing standing between the King and a
    /// picker that either strands him with no models or lies about why it is
    /// short.
    #[test]
    fn with_no_credential_the_mock_is_left_and_the_failure_is_still_reported() {
        // What the provider list yields when Copilot cannot authenticate: it
        // contributes nothing but its failure, and the mock contributes itself.
        let options = vec![option("mock", false)];

        assert_eq!(
            default_id(&options),
            "mock",
            "the last model standing is the one the King lands on"
        );
        assert_eq!(
            worse_of(CredentialState::Ready, CredentialState::Failed),
            CredentialState::Failed,
            "the mock being fine must not mask Copilot being broken"
        );
    }

    /// With a working credential the King opens on a real model, not on the
    /// offline one -- the mock is a fallback, not a default.
    #[test]
    fn the_best_available_model_wins_over_the_mock() {
        let options = vec![
            option("copilot/some-old-thing", false),
            option("copilot/claude-opus-5", true),
            option("mock", false),
        ];
        assert_eq!(default_id(&options), "copilot/claude-opus-5");
    }
}
