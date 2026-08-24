//! Drafting via GitHub Copilot's chat completions API, and the catalogue of
//! models it will serve.
//!
//! Both halves live here because they are the same backend's business: what
//! Copilot offers and how Copilot is called change together, and splitting them
//! was what made "which provider" a concept separate from "which model".
//!
//! Drafting is non-streaming, single turn. Streaming is deliberately out of
//! scope until the WebSocket layer exists -- without push, a streamed reply has
//! nowhere to go.

use super::{credential, Brief, Draft, Model, ModelError, Provider, ProviderCatalogue};
use kingdom_core::{CredentialState, ModelChoice, ModelEffort, ModelOption, Speaker};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://api.githubcopilot.com/chat/completions";
const MODELS_ENDPOINT: &str = "https://api.githubcopilot.com/models";

/// The id namespace this provider owns. Every option it offers is `copilot/…`,
/// which is what routes a choice back here.
pub const NAMESPACE: &str = "copilot";

/// Copilot's gateway rejects requests that do not identify a known integration,
/// so these two headers are required, not decorative.
pub const INTEGRATION_ID: &str = "copilot-cli";
pub const EDITOR_VERSION: &str = "CopilotCLI/1.0.78";

/// How long a fetched catalogue is reused. Opening the picker must not cost an
/// HTTP round trip every time, and the model list moves far slower than this.
const TTL: Duration = Duration::from_secs(10 * 60);

/// Surfaced above the fold, and the pool the opening default is drawn from.
/// Everything else is still listed, behind the picker's "show all" toggle.
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

// ---------------------------------------------------------------------------
// The provider: what Copilot will serve, and how to open one of its models
// ---------------------------------------------------------------------------

pub struct CopilotProvider;

#[async_trait::async_trait]
impl Provider for CopilotProvider {
    fn namespace(&self) -> &'static str {
        NAMESPACE
    }

    /// Read live from `/models` rather than hard-coded, because the catalogue
    /// changes on the order of weeks and a stale local list would offer models
    /// that 404 while hiding ones that work.
    ///
    /// A failure yields no models and says why, rather than erroring: the
    /// picker stays usable and the King is told what to fix, which is more
    /// useful than an empty list and a toast.
    async fn catalogue(&self) -> ProviderCatalogue {
        if let Some(options) = cached() {
            return ProviderCatalogue {
                options,
                credential: CredentialState::Ready,
                detail: "Catalogue from GitHub Copilot.".to_string(),
            };
        }

        let cred = match credential::resolve(Some(credential::DEFAULT_COPILOT_HELPER)).await {
            Ok(cred) => cred,
            Err(credential::CredentialError::NotConfigured) => {
                return ProviderCatalogue {
                    options: Vec::new(),
                    credential: CredentialState::Missing,
                    detail: "No Copilot models: set KINGDOM_API_KEY to a token, or \
                             KINGDOM_API_KEY_HELPER to a command that prints one."
                        .to_string(),
                };
            }
            Err(e) => {
                return ProviderCatalogue {
                    options: Vec::new(),
                    credential: CredentialState::Failed,
                    detail: format!("No Copilot models: {e}."),
                };
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
                ProviderCatalogue {
                    options,
                    credential: CredentialState::Ready,
                    detail,
                }
            }
            Err(e) => ProviderCatalogue {
                options: Vec::new(),
                credential: CredentialState::Failed,
                detail: e,
            },
        }
    }

    async fn open(&self, choice: &ModelChoice) -> Result<Box<dyn Model>, ModelError> {
        let cred = credential::resolve(Some(credential::DEFAULT_COPILOT_HELPER)).await?;
        Ok(Box::new(CopilotModel::new(
            cred.token,
            &choice.model,
            choice.effort,
        )))
    }
}

async fn fetch(token: &str) -> Result<Vec<ModelOption>, String> {
    let response = reqwest::Client::new()
        .get(MODELS_ENDPOINT)
        .bearer_auth(token)
        .header("Copilot-Integration-Id", INTEGRATION_ID)
        .header("Editor-Version", EDITOR_VERSION)
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
        id: format!("{NAMESPACE}/{api_name}"),
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

// ---------------------------------------------------------------------------
// The model: one Copilot model, ready to draft
// ---------------------------------------------------------------------------

pub struct CopilotModel {
    token: String,
    /// The namespaced id, e.g. `copilot/claude-opus-5`. Stored namespaced and
    /// sliced for the wire name, so what the plan records and what is sent can
    /// never disagree.
    id: String,
    /// `None` means the model's own default, which is a *different request*
    /// from any explicit level -- see [`request_body`].
    effort: Option<ModelEffort>,
    http: reqwest::Client,
}

impl CopilotModel {
    pub fn new(token: String, id: impl Into<String>, effort: Option<ModelEffort>) -> Self {
        Self {
            token,
            id: id.into(),
            effort,
            http: reqwest::Client::new(),
        }
    }

    /// The name Copilot knows this model by, with the namespace stripped.
    fn api_name(&self) -> &str {
        match self.id.split_once('/') {
            Some((_, name)) => name,
            None => &self.id,
        }
    }
}

/// Builds the chat completions payload.
///
/// `reasoning_effort` is present only when the King explicitly chose a level.
/// Omitting it asks for the model's native default; sending `"none"` asks for a
/// specific level that only some models accept. Conflating the two either
/// silently changes how hard the model thinks or earns an opaque 400, so the
/// distinction is carried all the way down to the wire.
fn request_body(model: &str, effort: Option<ModelEffort>, messages: Vec<Value>) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": 2048,
    });
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort.wire_name());
    }
    body
}

#[async_trait::async_trait]
impl Model for CopilotModel {
    async fn draft(&self, brief: &Brief) -> Result<Draft, ModelError> {
        let mut messages = vec![json!({
            "role": "system",
            "content": system_prompt(brief),
        })];

        for turn in &brief.transcript {
            messages.push(json!({
                "role": match turn.speaker {
                    Speaker::King => "user",
                    Speaker::Court => "assistant",
                },
                "content": turn.body,
            }));
        }
        messages.push(json!({ "role": "user", "content": brief.prompt }));

        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.token)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("Editor-Version", EDITOR_VERSION)
            .json(&request_body(self.api_name(), self.effort, messages))
            .send()
            .await
            .map_err(|e| ModelError::Transport(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| ModelError::Transport(e.to_string()))?;

        if !status.is_success() {
            // Surface the provider's own words. An opaque "request failed" here
            // is the fastest way to waste the King's afternoon.
            return Err(ModelError::Refused(format!(
                "Copilot returned {}: {}",
                status.as_u16(),
                provider_message(&body).unwrap_or_else(|| truncate(&body, 300))
            )));
        }

        let parsed: Value = serde_json::from_str(&body)
            .map_err(|e| ModelError::Transport(format!("unreadable response: {e}")))?;

        let text = parsed["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();

        if text.is_empty() {
            return Err(ModelError::Refused(
                "Copilot returned an empty reply.".to_string(),
            ));
        }

        Ok(Draft {
            title: headline(&text, &brief.city.name),
            summary: first_sentence(&text),
            touches: mentioned_paths(&text, &brief.city.notable_paths),
            body: text,
        })
    }

    fn id(&self) -> &str {
        &self.id
    }
}

/// The system prompt.
///
/// Deliberately plain. Kingdom's metaphor -- kings, courts, decrees -- exists to
/// give the *user* a stance toward his agents; it is not information about the
/// work, and sending it only nudges the model into answering in costume instead
/// of answering the question.
fn system_prompt(brief: &Brief) -> String {
    format!(
        "You are a senior software engineer helping with one project.\n\n\
         {}\n\
         Answer concisely and concretely, referring to real files above where relevant. \
         You cannot run commands or edit files: you are writing a proposal for review. \
         Do not invent files that are not listed.",
        brief.city.render()
    )
}

/// Pulls `{"error":{"message":…}}` out of an error body, falling back to the
/// raw text when the shape is unfamiliar.
fn provider_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v["error"]["message"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| v["message"].as_str().map(|s| s.to_string()))
}

/// A sidebar headline. Models often lead with a markdown heading; use it when
/// present, otherwise fall back to something honest rather than a truncated
/// mid-sentence fragment.
fn headline(text: &str, city: &str) -> String {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('#') {
            let cleaned = rest.trim_start_matches('#').trim();
            if !cleaned.is_empty() {
                return truncate(cleaned, 60);
            }
        }
    }
    format!("Counsel on {city}")
}

fn first_sentence(text: &str) -> String {
    let prose = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("");
    match prose.find(". ") {
        Some(i) => truncate(&prose[..=i], 200),
        None => truncate(prose, 200),
    }
}

/// Which of the project's real files the reply actually names.
///
/// Matching against the scanned paths rather than parsing arbitrary strings out
/// of the prose means a hallucinated filename can never light up a building that
/// does not exist on the map.
fn mentioned_paths(text: &str, known: &[String]) -> Vec<String> {
    known
        .iter()
        .filter(|p| text.contains(p.as_str()))
        .take(8)
        .cloned()
        .collect()
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    format!("{}…", trimmed.chars().take(max).collect::<String>())
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

    /// "The model's own default" and "explicitly the lowest level" are two
    /// different requests, and the difference is invisible until a gateway
    /// either thinks harder than asked or refuses with an opaque 400. The wire
    /// body is where that distinction has to survive.
    #[test]
    fn effort_reaches_the_wire_only_when_the_king_chose_one() {
        let native = request_body("claude-opus-5", None, Vec::new());
        assert!(
            native.get("reasoning_effort").is_none(),
            "no chosen effort must send no field, not a fabricated default"
        );

        let chosen = request_body("claude-opus-5", Some(ModelEffort::Xhigh), Vec::new());
        assert_eq!(chosen["reasoning_effort"], "xhigh");

        let explicit_none = request_body("gpt-5.4", Some(ModelEffort::None), Vec::new());
        assert_eq!(
            explicit_none["reasoning_effort"], "none",
            "an explicit `none` is a level in its own right, not the absent case"
        );
    }

    /// The plan records a namespaced id, but the gateway must receive a bare
    /// name. Storing one and deriving the other is what stops them drifting --
    /// sending `copilot/claude-opus-5` as the model name earns a 404.
    #[test]
    fn the_recorded_id_is_namespaced_and_the_wire_name_is_not() {
        let model = CopilotModel::new("t".into(), "copilot/claude-opus-5", None);
        assert_eq!(model.id(), "copilot/claude-opus-5");
        assert_eq!(model.api_name(), "claude-opus-5");
    }
}
