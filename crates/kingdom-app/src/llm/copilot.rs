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
    credential, Act, Acts, Answer, Brief, Draft, Model, ModelError, Provider, ProviderCatalogue,
    Reply, ToolSpec,
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

/// Copilot reports a raw model family; the user thinks in vendors.
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
    /// picker stays usable and the user is told what to fix, which is more
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
        // Read from the same live entry, and for the same reason: a plan opened
        // last week must be measured against the window the model has today,
        // not the one it had when the plan was recorded.
        let context_window = entry.as_ref().map_or(0, |o| o.context_window);
        // Same road as `can_act`, and for the same reason: a fact about the
        // model, read from the model, rather than a constant that is wrong for
        // everything except whatever it was tuned against.
        let max_output_tokens = entry.as_ref().and_then(|o| o.max_output_tokens);

        Ok(Box::new(CopilotModel::new(
            cred.token,
            &choice.model,
            choice.effort,
            can_act,
            can_see,
            context_window,
            max_output_tokens,
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
/// a listed-but-broken model costs the user a prompt to discover.
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
    // a guess here would be a guess the user acts on.
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
        // Several spellings, for the same reason `can_see` reads three: the
        // catalogue is not ours. Absent is a usable state here rather than
        // grounds to drop the model -- see the field's own doc.
        max_output_tokens: ["max_output_tokens", "max_completion_tokens"]
            .iter()
            .find_map(|key| capabilities["limits"][*key].as_u64())
            .map(|n| n as usize),
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
    /// How much this model will hold, as its catalogue declared. `0` when it
    /// declared nothing, which yields no reading rather than a guess.
    context_window: usize,
    /// The model's declared output budget, or `None` to fall back. See
    /// [`kingdom_core::ModelOption::max_output_tokens`] and [`FALLBACK_OUTPUT_TOKENS`].
    max_output_tokens: Option<usize>,
    http: reqwest::Client,
}

/// The output budget for a model whose catalogue entry declares none.
///
/// Generous on purpose. The number this replaced was 4096, which a reasoning
/// model at high effort can spend entirely on thinking -- returning empty
/// content that surfaced as "Copilot returned an empty reply" and failed the
/// plan outright. The failure mode of too small is a dead plan; of too large,
/// nothing at all, since this caps a reply rather than reserving anything.
const FALLBACK_OUTPUT_TOKENS: usize = 32_768;

impl CopilotModel {
    pub fn new(
        token: String,
        id: impl Into<String>,
        effort: Option<ModelEffort>,
        can_act: bool,
        can_see: bool,
        context_window: usize,
        max_output_tokens: Option<usize>,
    ) -> Self {
        Self {
            token,
            id: id.into(),
            effort,
            can_act,
            can_see,
            context_window,
            max_output_tokens,
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
/// `reasoning_effort` is present only when the user explicitly chose a level.
/// Omitting it asks for the model's native default; sending `"none"` asks for a
/// specific level that only some models accept. Conflating the two either
/// silently changes how hard the model thinks or earns an opaque 400, so the
/// distinction is carried all the way down to the wire.
fn request_body(
    model: &str,
    effort: Option<ModelEffort>,
    messages: Vec<Value>,
    tools: &[ToolSpec],
    max_output_tokens: Option<usize>,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens.unwrap_or(FALLBACK_OUTPUT_TOKENS),
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
/// **Calls from one reply are replayed as one reply.** A model that asked for
/// three files in a single message made one decision, and emitting three
/// separate assistant turns would replay it as three -- teaching the model that
/// it deliberated between reads it actually made together. Consecutive calls
/// sharing a [`kingdom_core::ToolCall::batch`] are grouped back into the one
/// assistant message they arrived as, followed by their results in order.
///
/// **The model's own thinking rides along.** Its reasoning and whatever it said
/// while asking are put back on the assistant message, so a reasoning model
/// sees why it did what it did rather than a bare list of results. Dropping
/// this is what made long investigations wander and repeat themselves.
///
/// A call that produced a *picture* adds one more, and it is a lie told
/// carefully. See [`shown`].
fn messages(brief: &Brief, can_see: bool) -> Vec<Value> {
    let mut out = vec![json!({
        "role": "system",
        "content": system_prompt(brief),
    })];

    let mut turns = brief.turns.iter().peekable();
    while let Some(turn) = turns.next() {
        match turn {
            Turn::Message(u) => out.push(json!({
                "role": match u.speaker {
                    Speaker::User => "user",
                    Speaker::Assistant => "assistant",
                },
                "content": u.body,
            })),
            Turn::Tool(first) => {
                // Everything that arrived in the same reply as `first`. A call
                // with no batch stands alone, which is what a record written
                // before batches existed gets.
                let mut batch = vec![first];
                while let Some(Turn::Tool(next)) = turns.peek() {
                    if !next.same_reply_as(first) {
                        break;
                    }
                    batch.push(next);
                    turns.next();
                }

                let mut assistant = json!({
                    "role": "assistant",
                    // The narration the model wrote while asking, where it
                    // wrote any. Null rather than an empty string when it did
                    // not: an empty content beside tool calls is the shape the
                    // gateway expects, while "" reads as it having said nothing
                    // on purpose.
                    "content": match first.narration.as_deref() {
                        Some(said) => json!(said),
                        None => Value::Null,
                    },
                    "tool_calls": batch.iter().map(|tool_call| json!({
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.tool,
                            "arguments": tool_call.input.to_string(),
                        }
                    })).collect::<Vec<_>>(),
                });

                // The reasoning is written back under the key it was read from
                // where we know it, and the opaque half verbatim. Merged rather
                // than replacing the message, so a signed blob that arrived as
                // several fields goes back as several fields.
                if let Some(reasoning) = &first.reasoning {
                    if let Some(text) = &reasoning.text {
                        assistant["reasoning_content"] = json!(text);
                    }
                    match &reasoning.opaque {
                        Some(Value::Object(fields)) => {
                            for (key, value) in fields {
                                assistant[key] = value.clone();
                            }
                        }
                        Some(other) => assistant["reasoning_opaque"] = other.clone(),
                        None => {}
                    }
                }

                out.push(assistant);

                // Results follow the whole batch, in the order the calls were
                // made. The gateway matches them by id, but a reader of this
                // request should see them in the order they happened.
                for tool_call in batch {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call.id,
                        // A call still in flight cannot happen here -- the loop
                        // settles every tool call before asking again -- but saying
                        // so beats sending an empty result, which the model would
                        // read as a command that printed nothing.
                        "content": if tool_call.in_flight() {
                            "(still running)".to_string()
                        } else {
                            replayed(tool_call.report())
                        },
                    }));
                    // Belt as well as braces: `ToolSpec::for_model` already keeps
                    // `read_image` away from a model with no vision, so a tool call
                    // with pictures should be unreachable here. Checking anyway
                    // costs a branch and avoids failing a whole turn if that filter
                    // is ever bypassed.
                    if can_see {
                        if let Some(message) = shown(tool_call) {
                            out.push(message);
                        }
                    }
                }
            }
        }
    }

    out
}

/// How much of one tool result is worth replaying.
///
/// The transcript on disk keeps every byte -- this bounds only what goes back
/// on the wire, and it goes back on *every* round. A 90 KB file read costs its
/// 90 KB once as an answer and then again on each of the rounds that follow, so
/// an unbounded result is a bill that grows with the square of the
/// conversation. `MOST_ROUNDS` is 500.
///
/// Head and tail rather than head alone because the two ends are where the
/// information is: a command's first lines say what it did and its last say how
/// it went, while the middle of a long build log is the part nobody reads.
const MOST_REPLAYED: usize = 12 * 1024;

/// Head and tail of one, with an honest marker of what was dropped.
///
/// The marker matters more than the saving. A silently truncated result would
/// have the model draw conclusions from a file it believes it read in full;
/// being told the middle is missing, and how much, it can go back for the part
/// it needs with `offset` and `limit`.
fn replayed(report: &str) -> String {
    if report.len() <= MOST_REPLAYED {
        return report.to_string();
    }

    // Two thirds from the front: the head of a result is usually the answer,
    // and the tail is usually the verdict.
    let head_budget = MOST_REPLAYED / 3 * 2;
    let tail_budget = MOST_REPLAYED - head_budget;

    // Cut on character boundaries -- `report` is a `str`, and slicing mid-UTF-8
    // would panic on a file with any non-ASCII in it.
    let head_end = floor_boundary(report, head_budget);
    let tail_start = ceil_boundary(report, report.len() - tail_budget);
    let dropped = tail_start - head_end;

    format!(
        "{}\n\n[... {dropped} bytes of this result were not replayed. The full text is in \
         the conversation; read the file directly with `offset`/`limit`, or narrow with \
         `search`, if you need the part that is missing. ...]\n\n{}",
        &report[..head_end],
        &report[tail_start..],
    )
}

/// The largest character boundary at or below `at`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The smallest character boundary at or above `at`.
fn ceil_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
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
/// **Why this is safe.** The message is built here, from a [`ToolCall`], and
/// exists only inside this request body. It is never a `Turn::Message`, never
/// an `Message`, never in the transcript. That containment is the whole
/// defence: the doc on [`kingdom_core::Turn`] argues that Kingdom's plumbing
/// must not be replayed to a model in the user's voice, and a `user` message
/// the user never said is exactly that hazard -- so it is never allowed to
/// exist as a domain value where something could mistake it for one.
///
/// **What should replace it.** Copilot's Responses API carries images natively
/// on a `FunctionCallOutput`, with no invented turn. That is the correct wire
/// format and the eventual answer here; it is a rewrite of this module's
/// request and response shapes, which is why this shim exists in the meantime.
fn shown(tool_call: &kingdom_core::ToolCall) -> Option<Value> {
    let images = tool_call.shown();
    if images.is_empty() {
        return None;
    }

    // The leading text matters: a bare picture with no antecedent leaves the
    // model to guess why it is suddenly looking at something.
    let mut parts = vec![json!({
        "type": "text",
        "text": format!("The image from the {} call above:", tool_call.tool),
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
    async fn take_turn(&self, brief: &Brief) -> Result<Answer, ModelError> {
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
                self.max_output_tokens,
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
            // is the fastest way to waste the user's afternoon.
            return Err(ModelError::Refused(format!(
                "Copilot returned {}: {}",
                status.as_u16(),
                provider_message(&body).unwrap_or_else(|| truncate(&body, 300))
            )));
        }

        let parsed: Value = serde_json::from_str(&body)
            .map_err(|e| ModelError::Transport(format!("unreadable response: {e}")))?;

        let message = &parsed["choices"][0]["message"];
        // Read once, before any ending: how full the window is is true of the
        // turn, not of the way it happened to end.
        let tokens = tokens_used(&parsed);
        let finish = parsed["choices"][0]["finish_reason"].as_str().unwrap_or_default();

        // Tool calls take precedence over any prose alongside them. A model
        // often narrates what it is about to do in the same message; treating
        // that narration as the finished answer would settle the plan while the
        // model still had work it wanted to do. It is no longer *discarded*
        // either -- it rides along as `narration`, because "here is what I am
        // about to do and why" is exactly the thread the next round needs.
        let calls = parse_acts(&message["tool_calls"]);
        let text = message["content"].as_str().unwrap_or_default().trim().to_string();

        if !calls.is_empty() {
            return Ok(Answer {
                reply: Reply::Acts(Acts {
                    calls,
                    reasoning: parse_reasoning(message),
                    narration: (!text.is_empty()).then_some(text),
                }),
                tokens,
            });
        }

        if text.is_empty() {
            // An empty reply after a `length` finish is not the model declining
            // to answer -- it is the model spending its entire output budget on
            // reasoning and having nothing left to say with. Those are different
            // problems with different fixes, and reporting both as "empty reply"
            // sends the reader looking in the wrong place. This is how the 4096
            // token budget hid for as long as it did.
            if finish == "length" {
                return Err(ModelError::Refused(
                    "The model used its entire output budget before answering. This usually \
                     means a high reasoning effort on a long conversation."
                        .to_string(),
                ));
            }
            return Err(ModelError::Refused(
                "Copilot returned an empty reply.".to_string(),
            ));
        }

        Ok(Answer {
            reply: Reply::Spoke(Draft {
                summary: first_sentence(&text),
                body: text,
            }),
            tokens,
        })
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

    fn context_window(&self) -> usize {
        self.context_window
    }
}

/// How many tokens a reply says it cost, if it says.
///
/// `total_tokens` first, `prompt_tokens` second. The prompt alone is what was
/// *sent* and understates the window by a whole reply -- and the reply is
/// already part of the exchange the next turn will resend, so the total is the
/// honest reading of how full the window stands once this turn is over.
///
/// `None` rather than `0` when the block is absent. Zero would be drawn as an
/// empty window, which is a claim about the conversation; absence is the truth.
fn tokens_used(response: &Value) -> Option<usize> {
    let usage = &response["usage"];
    usage["total_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_tokens"].as_u64())
        .map(|t| t as usize)
}

/// Reads whatever thinking the gateway returned alongside a reply.
///
/// Spelled several ways because the catalogue is not ours and this is a young
/// part of the API: Copilot has used `reasoning_content` and `reasoning`, and
/// signed variants arrive under their own keys. Reading the plausible spellings
/// costs a few branches; guessing one and being wrong costs the model its train
/// of thought on every round, silently -- which is the failure this whole
/// change exists to fix, and it left no error behind while it was happening.
///
/// The opaque half is copied verbatim and never inspected -- see
/// [`kingdom_core::Reasoning::opaque`].
fn parse_reasoning(message: &Value) -> Option<kingdom_core::Reasoning> {
    let text = ["reasoning_content", "reasoning_text"]
        .iter()
        .find_map(|key| message[*key].as_str())
        .or_else(|| message["reasoning"].as_str())
        .or_else(|| message["reasoning"]["content"].as_str())
        .map(str::to_string);

    let opaque = ["reasoning_opaque", "encrypted_content", "signature"]
        .iter()
        .find_map(|key| {
            let found = &message[*key];
            (!found.is_null()).then(|| found.clone())
        })
        .or_else(|| {
            // Where reasoning came as an object rather than a string, anything
            // beyond the prose we already read may need echoing back.
            let found = &message["reasoning"];
            found.is_object().then(|| found.clone())
        });

    let reasoning = kingdom_core::Reasoning { text, opaque };
    (!reasoning.is_empty()).then_some(reasoning)
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

/// The system prompt: whatever `system_prompt.rs` assembled.
///
/// Deliberately thin. This used to *build* the prompt, which quietly made it
/// Copilot's prompt rather than Kingdom's -- a second provider would have had
/// to reinvent it, and the two would have drifted the first time either was
/// touched. What a model is told is content, and content belongs in
/// [`crate::llm::system_prompt`]; a provider's job is transport.
///
/// The metaphor is still deliberately absent from it. It exists to give the
/// *user* a stance toward their agents; it is not information about the work,
/// and sending it only nudges the model into answering in costume instead of
/// answering the question.
fn system_prompt(brief: &Brief) -> String {
    brief.system_prompt.render()
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
    fn brief_with_tool_call(images: Vec<kingdom_core::ToolImage>) -> Brief {
        let mut tool_call = kingdom_core::ToolCall::started(
            "call-1",
            "read_image",
            serde_json::json!({ "path": "shot.png" }),
        );
        tool_call.outcome = Some(kingdom_core::ToolOutcome::seen("Looked at shot.png.", images));

        Brief {
            system_prompt: crate::llm::SystemPrompt {
                city: crate::llm::CityBrief {
                    name: "Testburg".into(),
                    path: "/dev/testburg".into(),
                    stack: "Rust".into(),
                    file_count: 1,
                    has_git: false,
                    dirty_files: 0,
                    notable_paths: Vec::new(),
                },
                ..Default::default()
            },
            turns: vec![Turn::Tool(tool_call)],
            tools: Vec::new(),
        }
    }

    fn a_picture() -> Vec<kingdom_core::ToolImage> {
        vec![kingdom_core::ToolImage {
            media_type: "image/png".into(),
            data: "QUJD".into(),
        }]
    }

    /// A settled call, optionally tied to a reply and carrying its thinking.
    fn call(id: &str, batch: Option<&str>, reasoning: Option<&str>) -> kingdom_core::ToolCall {
        let mut tool_call = kingdom_core::ToolCall::started(
            id,
            "read_file",
            serde_json::json!({ "path": format!("{id}.rs") }),
        );
        if let Some(batch) = batch {
            tool_call = tool_call.in_reply(
                batch,
                reasoning.map(|text| kingdom_core::Reasoning {
                    text: Some(text.to_string()),
                    opaque: None,
                }),
                None,
            );
        }
        tool_call.outcome = Some(kingdom_core::ToolOutcome::done(format!("contents of {id}")));
        tool_call
    }

    fn brief_with(turns: Vec<Turn>) -> Brief {
        Brief {
            system_prompt: crate::llm::SystemPrompt::default(),
            turns,
            tools: Vec::new(),
        }
    }

    /// The regression this whole change exists for.
    ///
    /// A reasoning model that is not handed back its own thinking loses the
    /// thread of its investigation between rounds: it sees N tool results and
    /// no record of why it asked for any of them, so it re-derives a strategy
    /// from raw output and re-reads what it has already read. That failure is
    /// invisible at the type level -- the request is well-formed and the
    /// gateway accepts it happily -- and shows up only as an agent that wanders
    /// for 24 rounds and proposes nothing. Nothing but an assertion catches it.
    #[test]
    fn a_models_own_reasoning_is_handed_back_to_it() {
        let messages = messages(
            &brief_with(vec![Turn::Tool(call(
                "call-1",
                Some("reply-1"),
                Some("The title is read in sidebar.rs, so that is where to look."),
            ))]),
            false,
        );

        let assistant = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("the call must still be replayed");

        assert_eq!(
            assistant["reasoning_content"],
            "The title is read in sidebar.rs, so that is where to look.",
            "the model's thinking must survive the round trip: {assistant:#?}"
        );
    }

    /// Calls from one reply are one decision, and must be replayed as one.
    ///
    /// A model that asked for three files in a single message did not
    /// deliberate between them. Emitting three assistant turns would teach it
    /// that it had -- and, worse, would imply a reasoning step before each read
    /// that never happened.
    #[test]
    fn calls_from_one_reply_are_replayed_as_one_assistant_turn() {
        let messages = messages(
            &brief_with(vec![
                Turn::Tool(call("call-1", Some("reply-1"), Some("read both"))),
                Turn::Tool(call("call-2", Some("reply-1"), None)),
                // A separate reply: must not be folded in with the two above.
                Turn::Tool(call("call-3", Some("reply-2"), None)),
            ]),
            false,
        );

        let assistants: Vec<_> = messages.iter().filter(|m| m["role"] == "assistant").collect();
        assert_eq!(
            assistants.len(),
            2,
            "two replies asked for three calls, so there are two assistant turns: {assistants:#?}"
        );
        assert_eq!(
            assistants[0]["tool_calls"].as_array().map(Vec::len),
            Some(2),
            "the first reply's two calls belong to one turn"
        );
        assert_eq!(assistants[1]["tool_calls"].as_array().map(Vec::len), Some(1));

        // Every call still gets its result, whatever the grouping did.
        assert_eq!(messages.iter().filter(|m| m["role"] == "tool").count(), 3);
    }

    /// A record written before batches existed must still replay correctly.
    ///
    /// Those calls have no batch, and grouping them on the strength of two
    /// `None`s being equal would invent a joint decision that may never have
    /// happened. They stand alone, which is the behaviour they already had.
    #[test]
    fn calls_from_before_batches_existed_each_stand_alone() {
        let messages = messages(
            &brief_with(vec![
                Turn::Tool(call("call-1", None, None)),
                Turn::Tool(call("call-2", None, None)),
            ]),
            false,
        );

        assert_eq!(
            messages.iter().filter(|m| m["role"] == "assistant").count(),
            2,
            "unbatched calls must not be grouped on the strength of both being None"
        );
    }

    /// The budget must come from the model, not from a constant.
    ///
    /// A fixed 4096 here is what let a high-effort model spend its whole output
    /// allowance on reasoning and return nothing, failing the plan. The wiring
    /// from catalogue to request is several hops long and every one of them is
    /// silent when it breaks.
    #[test]
    fn the_output_budget_comes_from_the_models_own_catalogue_entry() {
        let declared = request_body("m", None, Vec::new(), &[], Some(64_000));
        assert_eq!(declared["max_tokens"], 64_000);

        // A model that declares no limit is still usable, and gets a budget
        // well clear of what reasoning costs -- see `FALLBACK_OUTPUT_TOKENS`.
        let silent = request_body("m", None, Vec::new(), &[], None);
        assert_eq!(silent["max_tokens"], FALLBACK_OUTPUT_TOKENS);
        assert!(
            silent["max_tokens"].as_u64().unwrap() > 4096,
            "the fallback must not reinstate the budget that caused this bug"
        );
    }

    /// A long result is bounded on the wire but never silently.
    ///
    /// Every tool result is resent on every round, so an unbounded one is a
    /// bill that grows with the square of the conversation. The marker is the
    /// half that matters: a model told the middle is missing can go back for
    /// it, while one that is not will reason about a file it believes it read
    /// in full.
    #[test]
    fn a_long_tool_result_is_bounded_and_says_so() {
        let long = "x".repeat(MOST_REPLAYED * 2);
        let out = replayed(&long);

        assert!(out.len() < long.len(), "a long result must be bounded");
        assert!(
            out.contains("were not replayed"),
            "truncation must be visible to the model, not silent: {out:.200}"
        );
        // Short results are untouched -- the common case must not grow a note.
        assert_eq!(replayed("a short result"), "a short result");
    }

    /// Multi-byte text must not be cut mid-character.
    ///
    /// Slicing a `str` off a byte boundary panics, and a panic here would take
    /// down a turn over a source file that merely contained an em dash.
    #[test]
    fn bounding_a_result_does_not_split_a_character() {
        let out = replayed(&"€".repeat(MOST_REPLAYED));
        assert!(out.contains("were not replayed"));
    }

    /// The shape chat-completions demands, which nothing but the live gateway
    /// would otherwise check.
    ///
    /// A `role:"tool"` message cannot carry an image part, so the picture has to
    /// follow as a `user` message -- and the tool message must stay a plain
    /// string, because that is the part the format is strict about. Getting
    /// either half wrong is an opaque 400 that costs the user a whole turn, so
    /// both are asserted rather than just "the image is in there somewhere".
    #[test]
    fn a_picture_follows_its_tool_result_as_a_user_message() {
        let messages = messages(&brief_with_tool_call(a_picture()), true);

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
    /// request, not a degraded one. And a call that produced no picture must
    /// not grow a stray empty `user` turn, which would put words in the user's
    /// mouth for every ordinary `bash` call in the transcript.
    #[test]
    fn nothing_is_shown_to_a_model_that_cannot_see_or_for_a_call_with_no_picture() {
        let blind = messages(&brief_with_tool_call(a_picture()), false);
        assert!(
            !blind.iter().any(|m| m["role"] == "user"),
            "a model without vision must not be sent the image at all: {blind:?}"
        );

        let textual = messages(&brief_with_tool_call(Vec::new()), true);
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
    /// the user's turn dies on a gateway rejection.
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
    /// Each dropped case below costs the user a wasted prompt if it leaks
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
        let native = request_body("claude-opus-5", None, Vec::new(), &[], None);
        assert!(
            native.get("reasoning_effort").is_none(),
            "no chosen effort must send no field, not a fabricated default"
        );

        let chosen = request_body("claude-opus-5", Some(ModelEffort::Xhigh), Vec::new(), &[], None);
        assert_eq!(chosen["reasoning_effort"], "xhigh");

        let explicit_none = request_body("gpt-5.4", Some(ModelEffort::None), Vec::new(), &[], None);
        assert_eq!(
            explicit_none["reasoning_effort"], "none",
            "an explicit `none` is a level in its own right, not the absent case"
        );
    }

    /// The `usage` block is the only honest source of how full the window is,
    /// and like the rest of the `/chat/completions` shape it is not ours and
    /// cannot fail at compile time.
    ///
    /// The absent case is the one that matters. `None` means "no bar"; a `0`
    /// would be drawn as an empty window, which is a *claim* about a
    /// conversation that may in fact be nearly full -- the exact misreading the
    /// bar exists to prevent.
    #[test]
    fn how_full_the_window_is_comes_from_the_reply_or_not_at_all() {
        let with_total = serde_json::json!({
            "usage": {"prompt_tokens": 900, "completion_tokens": 100, "total_tokens": 1000}
        });
        assert_eq!(
            tokens_used(&with_total),
            Some(1000),
            "the total is what the next turn will resend, so it is what fills the window"
        );

        let prompt_only = serde_json::json!({"usage": {"prompt_tokens": 900}});
        assert_eq!(tokens_used(&prompt_only), Some(900));

        assert_eq!(
            tokens_used(&serde_json::json!({"choices": []})),
            None,
            "a reply that says nothing about usage must yield no reading, not zero"
        );
    }

    /// The plan records a namespaced id, but the gateway must receive a bare
    /// name. Storing one and deriving the other is what stops them drifting --
    /// sending `copilot/claude-opus-5` as the model name earns a 404.
    #[test]
    fn the_recorded_id_is_namespaced_and_the_wire_name_is_not() {
        let model =
            CopilotModel::new("t".into(), "copilot/claude-opus-5", None, true, true, 0, None);
        assert_eq!(model.id(), "copilot/claude-opus-5");
        assert_eq!(model.api_name(), "claude-opus-5");
    }
}
