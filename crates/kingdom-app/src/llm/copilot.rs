//! Drafting via GitHub Copilot's chat completions API.
//!
//! Non-streaming, single turn. Streaming is deliberately out of scope until the
//! WebSocket layer exists -- without push, a streamed reply has nowhere to go.

use super::{Brief, Draft, Model, ModelError};
use kingdom_core::{ModelEffort, Speaker};
use serde_json::{json, Value};

const ENDPOINT: &str = "https://api.githubcopilot.com/chat/completions";
pub const DEFAULT_MODEL: &str = "claude-sonnet-4.6";

/// Copilot's gateway rejects requests that do not identify a known integration,
/// so these two headers are required, not decorative.
pub const INTEGRATION_ID: &str = "copilot-cli";
pub const EDITOR_VERSION: &str = "CopilotCLI/1.0.78";

pub struct CopilotModel {
    token: String,
    /// The name Copilot knows this model by, with no `copilot/` namespace.
    model: String,
    /// `None` means the model's own default, which is a *different request*
    /// from any explicit level -- see [`request_body`].
    effort: Option<ModelEffort>,
    http: reqwest::Client,
}

impl CopilotModel {
    pub fn new(token: String, model: impl Into<String>, effort: Option<ModelEffort>) -> Self {
        Self {
            token,
            model: model.into(),
            effort,
            http: reqwest::Client::new(),
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
            .json(&request_body(&self.model, self.effort, messages))
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
            body: text,
        })
    }

    fn name(&self) -> &str {
        &self.model
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
}
