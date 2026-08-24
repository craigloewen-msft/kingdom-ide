//! The catalogue of models the King can choose from.
//!
//! Read live from Copilot's `/models` rather than hard-coded, because the
//! catalogue changes on the order of weeks and a stale local list would offer
//! models that 404 while hiding ones that work. What each model *declares* --
//! its context window, and which reasoning efforts it accepts -- is taken from
//! the same response, so the picker can never offer an effort the gateway would
//! refuse.

use super::{copilot, credential, mock};
use kingdom_core::{CredentialState, ModelCatalogue, ModelEffort, ModelOption};
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MODELS_ENDPOINT: &str = "https://api.githubcopilot.com/models";

/// How long a fetched catalogue is reused. Opening the picker must not cost an
/// HTTP round trip every time, and the model list moves far slower than this.
const TTL: Duration = Duration::from_secs(10 * 60);

/// The id namespace, so a choice carries its own provider. See
/// [`kingdom_core::ModelChoice::provider`].
const COPILOT_PREFIX: &str = "copilot/";

/// Surfaced above the fold. Everything else is still listed, behind the
/// picker's "show all" toggle -- this only decides what the King sees first.
const RECOMMENDED: &[&str] = &[
    "claude-opus-5",
    "claude-sonnet-5",
    "gpt-5.6-sol",
    "gemini-3.6-flash",
];

/// Legacy models that add noise to the picker without adding capability, plus
/// one internal utility model that is not a chat model at all.
const SKIP: &[&str] = &[
    "gpt-3.5-turbo",
    "gpt-3.5-turbo-0613",
    "gpt-4",
    "gpt-4-0613",
    "gpt-4-o-preview",
    "gpt-4o-2024-05-13",
    "trajectory-compaction",
];

/// Copilot reports a raw model family; the King thinks in vendors.
const VENDOR_BY_PREFIX: &[(&str, &str)] = &[
    ("claude", "Anthropic"),
    ("gemini", "Google"),
    ("grok", "xAI"),
    ("mai-", "Microsoft"),
    ("gpt-", "OpenAI"),
    ("o1", "OpenAI"),
    ("o3", "OpenAI"),
];

/// We only speak chat completions today. A Responses-only model is filtered
/// out rather than listed and broken.
const CHAT_ENDPOINT: &str = "/chat/completions";

static CACHE: Mutex<Option<(Vec<ModelOption>, Instant)>> = Mutex::new(None);

/// The offline model, always offered, so the King can fall back to something
/// that needs no credential without editing a dotfile.
fn mock_option() -> ModelOption {
    ModelOption {
        id: mock::MODEL_NAME.to_string(),
        label: "Mock (offline)".to_string(),
        vendor: "Offline".to_string(),
        context_window: 0,
        recommended: true,
        efforts: Vec::new(),
    }
}

/// The full catalogue: the mock plus whatever Copilot will actually serve.
///
/// A failure to reach Copilot is reported, not thrown: the dock stays usable
/// with the mock and says what broke, which is more useful than an empty list
/// and an error toast.
pub async fn catalogue() -> ModelCatalogue {
    let default_id = super::default_model_id();

    if let Some(cached) = cached() {
        return assemble(
            cached,
            default_id,
            CredentialState::Ready,
            "Catalogue from GitHub Copilot.".to_string(),
        );
    }

    let cred = match credential::resolve(Some(credential::DEFAULT_COPILOT_HELPER)).await {
        Ok(cred) => cred,
        Err(credential::CredentialError::NotConfigured) => {
            return assemble(
                Vec::new(),
                default_id,
                CredentialState::Missing,
                "Set KINGDOM_API_KEY to a token, or KINGDOM_API_KEY_HELPER to a command that \
                 prints one. Only the offline mock is available until then."
                    .to_string(),
            );
        }
        Err(e) => {
            return assemble(
                Vec::new(),
                default_id,
                CredentialState::Failed,
                format!("{e}. Only the offline mock is available until that is fixed."),
            );
        }
    };

    match fetch(&cred.token).await {
        Ok(options) => {
            store(&options);
            let detail = format!(
                "{} models from GitHub Copilot, via {}.",
                options.len(),
                cred.source.describe()
            );
            assemble(options, default_id, CredentialState::Ready, detail)
        }
        Err(e) => assemble(
            Vec::new(),
            default_id,
            CredentialState::Failed,
            format!("{e} Only the offline mock is available until that is fixed."),
        ),
    }
}

/// Puts the mock at the head of the list and makes sure the default is a real
/// entry -- a `default_id` naming a model nobody offers would leave the picker
/// pointing at nothing.
fn assemble(
    copilot_models: Vec<ModelOption>,
    default_id: String,
    credential: CredentialState,
    detail: String,
) -> ModelCatalogue {
    let mut options = vec![mock_option()];
    options.extend(copilot_models);

    let default_id = if options.iter().any(|o| o.id == default_id) {
        default_id
    } else {
        mock::MODEL_NAME.to_string()
    };

    ModelCatalogue {
        options,
        default_id,
        credential,
        detail,
    }
}

async fn fetch(token: &str) -> Result<Vec<ModelOption>, String> {
    let response = reqwest::Client::new()
        .get(MODELS_ENDPOINT)
        .bearer_auth(token)
        .header("Copilot-Integration-Id", copilot::INTEGRATION_ID)
        .header("Editor-Version", copilot::EDITOR_VERSION)
        .send()
        .await
        .map_err(|e| format!("Could not reach the model catalogue: {e}."))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Could not read the model catalogue: {e}."))?;

    if !status.is_success() {
        return Err(format!(
            "Copilot returned {} for the model catalogue.",
            status.as_u16()
        ));
    }

    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("Unreadable model catalogue: {e}."))?;

    Ok(parse(&parsed))
}

/// Turns Copilot's `/models` payload into picker entries.
///
/// Every filter here is a refusal to guess: a model whose shape we do not
/// recognise is dropped rather than listed with invented capabilities, because
/// a listed-but-broken model costs the King a decree to discover.
fn parse(payload: &Value) -> Vec<ModelOption> {
    let mut options: Vec<ModelOption> = payload["data"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(parse_one)
        .collect();

    // Recommended first, then alphabetical: a stable order, so the picker does
    // not reshuffle itself between openings.
    options.sort_by(|a, b| {
        b.recommended
            .cmp(&a.recommended)
            .then_with(|| a.id.cmp(&b.id))
    });
    options
}

fn parse_one(model: &Value) -> Option<ModelOption> {
    let api_name = model["id"].as_str()?;
    if SKIP.contains(&api_name) {
        return None;
    }

    let capabilities = &model["capabilities"];
    if capabilities["type"].as_str() != Some("chat") {
        return None;
    }

    // Older models omit the endpoint list entirely; those are chat-only.
    if let Some(endpoints) = model["supported_endpoints"].as_array() {
        if !endpoints.iter().any(|e| e.as_str() == Some(CHAT_ENDPOINT)) {
            return None;
        }
    }

    // No declared window means we do not know how much this model can hold, and
    // a guess here would be a guess the King acts on.
    let context_window = capabilities["limits"]["max_context_window_tokens"].as_u64()? as usize;

    let efforts = capabilities["supports"]["reasoning_effort"]
        .as_array()
        .map(|levels| {
            let declared: Vec<ModelEffort> = levels
                .iter()
                .filter_map(|l| l.as_str())
                .filter_map(ModelEffort::from_wire)
                .collect();
            // Weakest first, whatever order the provider listed them in.
            ModelEffort::ALL
                .into_iter()
                .filter(|e| declared.contains(e))
                .collect()
        })
        .unwrap_or_default();

    Some(ModelOption {
        id: format!("{COPILOT_PREFIX}{api_name}"),
        label: model["name"].as_str().unwrap_or(api_name).to_string(),
        vendor: vendor(api_name).to_string(),
        context_window,
        recommended: RECOMMENDED.contains(&api_name),
        efforts,
    })
}

fn vendor(api_name: &str) -> &'static str {
    VENDOR_BY_PREFIX
        .iter()
        .find(|(prefix, _)| api_name.starts_with(prefix))
        .map(|(_, vendor)| *vendor)
        .unwrap_or("Copilot")
}

fn cached() -> Option<Vec<ModelOption>> {
    let cache = CACHE.lock().ok()?;
    let (options, expires) = cache.as_ref()?;
    (Instant::now() < *expires).then(|| options.clone())
}

fn store(options: &[ModelOption]) {
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((options.to_vec(), Instant::now() + TTL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `/models` shape is not ours and cannot fail at compile time, so the
    /// filters that keep unusable models out of the picker are pinned here.
    /// Each dropped case below costs the King a wasted decree if it leaks
    /// through: a non-chat model, a Responses-only model, and one whose context
    /// window we would otherwise have to invent.
    #[test]
    fn the_catalogue_lists_only_models_it_can_actually_use() {
        let payload = serde_json::json!({"data": [
            {
                "id": "claude-opus-5",
                "name": "Claude Opus 5",
                "capabilities": {
                    "type": "chat",
                    "limits": {"max_context_window_tokens": 1000000},
                    "supports": {"reasoning_effort": ["high", "low", "max"]}
                },
                "supported_endpoints": ["/chat/completions"]
            },
            {
                "id": "claude-haiku-4.5",
                "name": "Claude Haiku 4.5",
                "capabilities": {
                    "type": "chat",
                    "limits": {"max_context_window_tokens": 200000},
                    "supports": {"tool_calls": true}
                }
            },
            {
                "id": "text-embedding-3-small",
                "capabilities": {"type": "embeddings", "limits": {"max_inputs": 16}}
            },
            {
                "id": "gpt-5.6-luna",
                "capabilities": {
                    "type": "chat",
                    "limits": {"max_context_window_tokens": 1050000}
                },
                "supported_endpoints": ["/responses"]
            },
            {
                "id": "mystery-model",
                "capabilities": {"type": "chat", "limits": {}}
            },
            {
                "id": "gpt-4",
                "capabilities": {
                    "type": "chat",
                    "limits": {"max_context_window_tokens": 8000}
                }
            }
        ]});

        let options = parse(&payload);
        let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["copilot/claude-opus-5", "copilot/claude-haiku-4.5"],
            "embeddings, Responses-only, window-less and skipped models must not be offered"
        );

        let opus = &options[0];
        assert_eq!(opus.vendor, "Anthropic");
        assert_eq!(opus.context_window, 1_000_000);
        assert_eq!(
            opus.efforts,
            vec![ModelEffort::Low, ModelEffort::High, ModelEffort::Max],
            "declared efforts are ordered weakest-first, not in the provider's order"
        );

        assert!(
            options[1].efforts.is_empty(),
            "a model that declares no reasoning_effort must offer no effort control"
        );
    }
}
