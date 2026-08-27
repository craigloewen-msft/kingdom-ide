//! Assembling one catalogue out of every provider.
//!
//! This file used to *be* the catalogue: it fetched Copilot's `/models`, parsed
//! it, and pinned a hard-coded mock entry at the head of the list. Both of those
//! now belong to the providers themselves, and what is left here is the only
//! part that was ever about the catalogue as a whole -- putting the lists
//! together, and deciding what the user lands on before he has chosen.

use super::providers;
use kingdom_core::{CredentialState, ModelCatalogue};

/// The last-resort default. Only reached if every provider offers nothing,
/// which today cannot happen -- the mock always does.
const FALLBACK_ID: &str = super::mock::MODEL_ID;

/// Every model the user can choose between, from every backend.
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
        default_id: default_id(&options, super::preferred_model_id().as_deref()),
        options,
        credential,
        detail: details.join(" "),
    }
}

/// What the picker opens on before the user has chosen.
///
/// `preferred` -- the King's `KINGDOM_MODEL` -- wins, but only if it names a
/// model somebody actually serves; otherwise it is a default pointing at
/// nothing. Failing that: the best on offer, which means the first recommended
/// model, then simply the first. With no working credential the only entry is
/// the mock, so a fresh clone still drafts offline with no setup -- not because
/// the mock is privileged, but because it is what is left.
///
/// The preference is a *parameter* rather than a read of the environment, and
/// that is load-bearing. This function used to call `preferred_model_id()`
/// itself, which made its answer depend on the process's environment -- so the
/// tests below passed on a bare machine and failed under Kingdom's own tooling,
/// which pins `KINGDOM_MODEL=mock` for a plan working in a Kingdom checkout
/// (see `tools::child_environment`). A plan reviewing this repository was shown
/// a red suite it had not caused. Injecting it keeps the decision pure and lets
/// the override be tested rather than only suffered.
fn default_id(options: &[kingdom_core::ModelOption], preferred: Option<&str>) -> String {
    if let Some(wanted) = preferred {
        if options.iter().any(|o| o.id == wanted) {
            return wanted.to_string();
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
/// would report a healthy credential while Copilot's is broken -- and the user
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
            can_act: true,
            can_see: false,
            max_output_tokens: None,
        }
    }

    /// A fresh clone with no credential must still be able to draft, *and* must
    /// still be told its credential is broken. Those two used to be separate
    /// mechanisms -- a hard-coded mock entry, and a badge in the prompt bar.
    /// Both are gone, so this is the only thing standing between the user and a
    /// picker that either strands him with no models or lies about why it is
    /// short.
    #[test]
    fn with_no_credential_the_mock_is_left_and_the_failure_is_still_reported() {
        // What the provider list yields when Copilot cannot authenticate: it
        // contributes nothing but its failure, and the mock contributes itself.
        let options = vec![option("mock", false)];

        assert_eq!(
            default_id(&options, None),
            "mock",
            "the last model standing is the one the King lands on"
        );
        assert_eq!(
            worse_of(CredentialState::Ready, CredentialState::Failed),
            CredentialState::Failed,
            "the mock being fine must not mask Copilot being broken"
        );
    }

    /// With a working credential the user opens on a real model, not on the
    /// offline one -- the mock is a fallback, not a default.
    #[test]
    fn the_best_available_model_wins_over_the_mock() {
        let options = vec![
            option("copilot/some-old-thing", false),
            option("copilot/claude-opus-5", true),
            option("mock", false),
        ];
        assert_eq!(default_id(&options, None), "copilot/claude-opus-5");
    }

    /// The King's `KINGDOM_MODEL` overrides the recommendation.
    ///
    /// This is the path Kingdom's own tooling takes when a plan works on a
    /// Kingdom checkout -- `tools::child_environment` pins it to the mock so a
    /// rehearsal never spends a real token. Until the preference became a
    /// parameter, this behaviour had no test of its own: it was only ever
    /// observed by *breaking* the test above, on exactly the machines that
    /// mattered most.
    #[test]
    fn a_named_preference_beats_the_recommendation() {
        let options = vec![option("copilot/claude-opus-5", true), option("mock", false)];

        assert_eq!(
            default_id(&options, Some("mock")),
            "mock",
            "a model the King asked for by name must be the one he gets"
        );
    }

    /// A preference naming a model no provider serves is ignored rather than
    /// honoured, because honouring it would open the picker on nothing.
    #[test]
    fn a_preference_nobody_serves_falls_back_to_the_best_on_offer() {
        let options = vec![option("copilot/claude-opus-5", true), option("mock", false)];

        assert_eq!(
            default_id(&options, Some("copilot/a-model-that-retired")),
            "copilot/claude-opus-5"
        );
    }
}
