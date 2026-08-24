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

use super::{
    credential, Act, Brief, Draft, Model, ModelError, Provider, ProviderCatalogue, Reply, ToolSpec,
};
use kingdom_core::{CredentialState, ModelChoice, ModelEffort, ModelOption, Speaker, Turn};
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
        // Read from the catalogue rather than recorded on the plan: whether a
        // model takes tools is a fact about the model that can change under us,
        // and a plan opened last week must not keep sending tools to something
        // that has since stopped accepting them. The catalogue is cached, so
        // this is normally free.
        let entry = self
            .catalogue()
            .await
            .options
            .into_iter()
            .find(|o| o.id == choice.model);
        let can_act = entry.as_ref().is_some_and(|o| o.can_act);
        let can_see = entry.as_ref().is_some_and(|o| o.can_see);

        Ok(Box::new(CopilotModel::new(
            cred.token,
            &choice.model,
            choice.effort,
            can_act,
            can_see,
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
        // Absent is taken as "no", not as "probably": a model that cannot take
        // tools and is sent them anyway fails the whole turn with an opaque
        // gateway error, where one that can and is not merely answers in prose.
        // The costs of guessing wrong are not symmetric.
        can_act: capabilities["supports"]["tool_calls"]
            .as_bool()
            .unwrap_or(false),
        // Copilot spells this on the model itself rather than under `supports`,
        // so both are read: the catalogue is not ours and has moved this sort
        // of flag before. Absent from both is taken as "cannot see", for the
        // same asymmetry as `can_act` above.
        can_see: capabilities["supports"]["vision"]
            .as_bool()
            .or_else(|| capabilities["vision"].as_bool())
            .or_else(|| model["vision"].as_bool())
            .unwrap_or(false),
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
    /// Whether this model takes tools, as its catalogue entry declared.
    can_act: bool,
    /// Whether this model can be shown an image, as its catalogue declared.
    can_see: bool,
    http: reqwest::Client,
}

impl CopilotModel {
    pub fn new(
        token: String,
        id: impl Into<String>,
        effort: Option<ModelEffort>,
        can_act: bool,
        can_see: bool,
    ) -> Self {
        Self {
            token,
            id: id.into(),
            effort,
            can_act,
            can_see,
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
fn request_body(
    model: &str,
    effort: Option<ModelEffort>,
    messages: Vec<Value>,
    tools: &[ToolSpec],
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": 4096,
    });
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort.wire_name());
    }
    // Absent rather than empty when there are none. Some gateways reject
    // `"tools": []` outright, and "this turn has no tools" and "this model has
    // no tools" are the same request as far as the wire is concerned.
    if !tools.is_empty() {
        body["tools"] = json!(tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.schema,
                }
            }))
            .collect::<Vec<_>>());
    }
    body
}

/// Rebuilds the conversation as Copilot's message list.
///
/// A tool call becomes *two* messages: the assistant turn that requested it and
/// a `tool` message carrying its result. Both are required -- a result with no
/// preceding call is rejected by the gateway, and a call with no result leaves
/// the model believing it is still waiting.
///
/// A call that produced a *picture* becomes three, and the third is a lie told
/// carefully. See [`shown`].
fn messages(brief: &Brief, can_see: bool) -> Vec<Value> {
    let mut out = vec![json!({
        "role": "system",
        "content": system_prompt(brief),
    })];

    for turn in &brief.turns {
        match turn {
            Turn::Said(u) => out.push(json!({
                "role": match u.speaker {
                    Speaker::King => "user",
                    Speaker::Court => "assistant",
                },
                "content": u.body,
            })),
            Turn::Did(deed) => {
                out.push(json!({
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "id": deed.id,
                        "type": "function",
                        "function": {
                            "name": deed.tool,
                            "arguments": deed.input.to_string(),
                        }
                    }],
                }));
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": deed.id,
                    // A call still in flight cannot happen here -- the loop
                    // settles every deed before asking again -- but saying so
                    // beats sending an empty result, which the model would read
                    // as a command that printed nothing.
                    "content": if deed.in_flight() {
                        "(still running)"
                    } else {
                        deed.report()
                    },
                }));
                // Belt as well as braces: `ToolSpec::for_model` already keeps
                // `read_image` away from a model with no vision, so a deed with
                // pictures should be unreachable here. Checking anyway costs a
                // branch and avoids failing a whole turn if that filter is ever
                // bypassed.
                if can_see {
                    if let Some(message) = shown(deed) {
                        out.push(message);
                    }
                }
            }
        }
    }

    out
}

/// The pictures from a tool result, as a message a model will actually look at.
///
/// **Why this is a `user` message.** Chat-completions has no image part for a
/// `role:"tool"` message -- its content is a string, full stop. The image
/// therefore has to arrive as the one role the format *does* accept
/// `image_url` parts on. Phoenix hits this same wall and gives up, dropping the
/// images outright (`phoenix-llm/src/openai.rs`); a shown picture is worth a
/// synthetic turn.
///
/// **Why this is safe.** The message is built here, from a [`Deed`], and exists
/// only inside this request body. It is never a `Turn::Said`, never an
/// `Utterance`, never in the transcript. That containment is the whole defence:
/// the doc on [`kingdom_core::Turn`] argues that Kingdom's plumbing must not be
/// replayed to a model in the King's voice, and a `user` message the King never
/// said is exactly that hazard -- so it is never allowed to exist as a domain
/// value where something could mistake it for one.
///
/// **What should replace it.** Copilot's Responses API carries images natively
/// on a `FunctionCallOutput`, with no invented turn. That is the correct wire
/// format and the eventual answer here; it is a rewrite of this module's
/// request and response shapes, which is why this shim exists in the meantime.
fn shown(deed: &kingdom_core::Deed) -> Option<Value> {
    let images = deed.shown();
    if images.is_empty() {
        return None;
    }

    // The leading text matters: a bare picture with no antecedent leaves the
    // model to guess why it is suddenly looking at something.
    let mut parts = vec![json!({
        "type": "text",
        "text": format!("The image from the {} call above:", deed.tool),
    })];
    parts.extend(images.iter().map(|image| {
        json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", image.media_type, image.data),
            }
        })
    }));

    Some(json!({ "role": "user", "content": parts }))
}

#[async_trait::async_trait]
impl Model for CopilotModel {
    async fn take_turn(&self, brief: &Brief) -> Result<Reply, ModelError> {
        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.token)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("Editor-Version", EDITOR_VERSION)
            .json(&request_body(
                self.api_name(),
                self.effort,
                messages(brief, self.can_see),
                &brief.tools,
            ))
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

        let message = &parsed["choices"][0]["message"];

        // Tool calls take precedence over any prose alongside them. A model
        // often narrates what it is about to do in the same message; treating
        // that narration as the finished answer would settle the plan while the
        // court still had work it wanted to do.
        let acts = parse_acts(&message["tool_calls"]);
        if !acts.is_empty() {
            return Ok(Reply::Acts(acts));
        }

        let text = message["content"].as_str().unwrap_or_default().trim().to_string();

        if text.is_empty() {
            return Err(ModelError::Refused(
                "Copilot returned an empty reply.".to_string(),
            ));
        }

        Ok(Reply::Spoke(Draft {
            title: headline(&text, &brief.city.name),
            summary: first_sentence(&text),
            body: text,
        }))
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn can_act(&self) -> bool {
        self.can_act
    }

    fn can_see(&self) -> bool {
        self.can_see
    }
}

/// Reads the `tool_calls` array into calls we can actually make.
///
/// `arguments` arrives as a *string* of JSON rather than JSON, and a model is
/// perfectly capable of producing one that does not parse. Rather than drop
/// such a call -- leaving the model waiting forever for a result to a call
/// nothing recorded -- the raw text is kept as a JSON string. The tool then
/// refuses it for the honest reason, and the model is told what it actually
/// sent.
fn parse_acts(tool_calls: &Value) -> Vec<Act> {
    tool_calls
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|call| {
            let name = call["function"]["name"].as_str()?;
            let raw = call["function"]["arguments"].as_str().unwrap_or("{}");
            Some(Act {
                // A call with no id cannot be answered -- the result would have
                // nothing to quote back -- so it is skipped rather than given
                // one we invented.
                id: call["id"].as_str()?.to_string(),
                tool: name.to_string(),
                input: serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string())),
            })
        })
        .collect()
}

/// The system prompt.
///
/// Deliberately plain. Kingdom's metaphor -- kings, courts, decrees -- exists to
/// give the *user* a stance toward his agents; it is not information about the
/// work, and sending it only nudges the model into answering in costume instead
/// of answering the question.
fn system_prompt(brief: &Brief) -> String {
    let mut out = format!(
        "You are a senior software engineer helping with one project.\n\n{}\n",
        brief.city.render()
    );

    if brief.tools.is_empty() {
        out.push_str(
            "Answer concisely and concretely, referring to real files above where relevant. \
             You cannot run commands or edit files: you are writing a proposal for review. \
             Do not invent files that are not listed.",
        );
    } else {
        // The file list is a starting point once tools exist, not the whole
        // world -- telling a model with a filesystem in its hands not to name
        // unlisted files would forbid it from reading anything it discovered.
        out.push_str(
            "You have tools and are working in the directory above. Use them: read \
             before you change, and check your work by running it rather than by \
             assuming. The file list is a starting point, not the whole project. \
             When you have finished, reply with what you did and what it means \
             for the reader -- concisely, and without repeating the output of \
             commands they can already see.",
        );
    }

    out
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

    /// A brief holding one settled tool call, optionally with a picture.
    fn brief_with_deed(images: Vec<kingdom_core::DeedImage>) -> Brief {
        let mut deed = kingdom_core::Deed::begun(
            "call-1",
            "read_image",
            serde_json::json!({ "path": "shot.png" }),
        );
        deed.outcome = Some(kingdom_core::DeedOutcome::seen("Looked at shot.png.", images));

        Brief {
            city: crate::llm::CityBrief {
                name: "Testburg".into(),
                path: "/dev/testburg".into(),
                stack: "Rust".into(),
                file_count: 1,
                has_git: false,
                dirty_files: 0,
                notable_paths: Vec::new(),
            },
            turns: vec![Turn::Did(deed)],
            tools: Vec::new(),
        }
    }

    fn a_picture() -> Vec<kingdom_core::DeedImage> {
        vec![kingdom_core::DeedImage {
            media_type: "image/png".into(),
            data: "QUJD".into(),
        }]
    }

    /// The shape chat-completions demands, which nothing but the live gateway
    /// would otherwise check.
    ///
    /// A `role:"tool"` message cannot carry an image part, so the picture has to
    /// follow as a `user` message -- and the tool message must stay a plain
    /// string, because that is the part the format is strict about. Getting
    /// either half wrong is an opaque 400 that costs the King a whole turn, so
    /// both are asserted rather than just "the image is in there somewhere".
    #[test]
    fn a_picture_follows_its_tool_result_as_a_user_message() {
        let messages = messages(&brief_with_deed(a_picture()), true);

        let tool = messages
            .iter()
            .find(|m| m["role"] == "tool")
            .expect("the call still needs its result");
        assert!(
            tool["content"].is_string(),
            "a tool message's content must stay a plain string: {tool}"
        );

        let carrying = messages
            .last()
            .expect("the image message comes last, after the result it belongs to");
        assert_eq!(carrying["role"], "user");
        let parts = carrying["content"]
            .as_array()
            .expect("image content is an array of parts");
        assert_eq!(
            parts[0]["type"], "text",
            "a bare picture with no antecedent leaves the model guessing why"
        );
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"], "data:image/png;base64,QUJD",
            "the data URL is what the format actually reads"
        );
    }

    /// Two independent guards, because each fails silently on its own.
    ///
    /// A model with no vision must never be sent an image -- that is a rejected
    /// request, not a degraded one. And a call that produced no picture must not
    /// grow a stray empty `user` turn, which would put words in the King's mouth
    /// for every ordinary `bash` call in the transcript.
    #[test]
    fn nothing_is_shown_to_a_model_that_cannot_see_or_for_a_call_with_no_picture() {
        let blind = messages(&brief_with_deed(a_picture()), false);
        assert!(
            !blind.iter().any(|m| m["role"] == "user"),
            "a model without vision must not be sent the image at all: {blind:?}"
        );

        let textual = messages(&brief_with_deed(Vec::new()), true);
        assert!(
            !textual.iter().any(|m| m["role"] == "user"),
            "an ordinary tool result must not invent a turn the King never took"
        );
    }

    /// Where a model's declared vision lives in the payload is not settled --
    /// the catalogue is not ours, and this sort of flag has moved between
    /// `capabilities.supports` and the model itself before. All three spellings
    /// are read, and absence means "cannot see".
    ///
    /// Worth pinning because both failure directions are silent: read the wrong
    /// key and every model looks blind, so `read_image` is quietly never
    /// offered and the feature simply does not exist. Guess the other way and
    /// the King's turn dies on a gateway rejection.
    #[test]
    fn vision_is_recognised_wherever_the_catalogue_declares_it() {
        let seeing = |model: serde_json::Value| parse_one(&model).expect("a usable model").can_see;

        let base = |supports: serde_json::Value, extra: serde_json::Value| {
            let mut capabilities = serde_json::json!({
                "type": "chat",
                "limits": {"max_context_window_tokens": 1000},
                "supports": supports,
            });
            if let Some(pairs) = extra.as_object() {
                for (k, v) in pairs {
                    capabilities[k] = v.clone();
                }
            }
            serde_json::json!({ "id": "m", "name": "M", "capabilities": capabilities })
        };

        assert!(seeing(base(
            serde_json::json!({"vision": true}),
            serde_json::json!({})
        )));
        assert!(seeing(base(
            serde_json::json!({}),
            serde_json::json!({"vision": true})
        )));
        assert!(
            !seeing(base(serde_json::json!({}), serde_json::json!({}))),
            "undeclared must mean blind: sending an image to a model that \
             cannot take one fails the whole turn"
        );
    }

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

        // Sending tools to a model that does not take them fails the whole turn
        // with an opaque gateway error; not sending them to one that does
        // merely gets prose. The costs are not symmetric, so an undeclared
        // capability must read as "no".
        assert!(
            !opus.can_act,
            "a model that does not declare tool_calls must not be sent tools"
        );
        assert!(options[1].can_act, "a declared capability must be honoured");
    }

    /// "The model's own default" and "explicitly the lowest level" are two
    /// different requests, and the difference is invisible until a gateway
    /// either thinks harder than asked or refuses with an opaque 400. The wire
    /// body is where that distinction has to survive.
    #[test]
    fn effort_reaches_the_wire_only_when_the_king_chose_one() {
        let native = request_body("claude-opus-5", None, Vec::new(), &[]);
        assert!(
            native.get("reasoning_effort").is_none(),
            "no chosen effort must send no field, not a fabricated default"
        );

        let chosen = request_body(
            "claude-opus-5",
            Some(ModelEffort::Xhigh),
            Vec::new(),
            &[],
        );
        assert_eq!(chosen["reasoning_effort"], "xhigh");

        let explicit_none = request_body("gpt-5.4", Some(ModelEffort::None), Vec::new(), &[]);
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
        let model = CopilotModel::new("t".into(), "copilot/claude-opus-5", None, true, true);
        assert_eq!(model.id(), "copilot/claude-opus-5");
        assert_eq!(model.api_name(), "claude-opus-5");
    }
}
