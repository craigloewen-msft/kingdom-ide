//! Drafting via GitHub Copilot's chat completions API.
//!
//! Non-streaming, single turn. Streaming is deliberately out of scope until the
//! WebSocket layer exists -- without push, a streamed reply has nowhere to go.

use super::{Brief, Draft, Model, ModelError};
use kingdom_core::Speaker;
use serde_json::{json, Value};

const ENDPOINT: &str = "https://api.githubcopilot.com/chat/completions";
const DEFAULT_MODEL: &str = "claude-sonnet-4.6";

/// Copilot's gateway rejects requests that do not identify a known integration,
/// so these two headers are required, not decorative.
const INTEGRATION_ID: &str = "copilot-cli";
const EDITOR_VERSION: &str = "CopilotCLI/1.0.78";

pub fn model_name() -> String {
    std::env::var("KINGDOM_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

pub struct CopilotModel {
    token: String,
    model: String,
    http: reqwest::Client,
}

impl CopilotModel {
    pub fn new(token: String) -> Self {
        Self {
            token,
            model: model_name(),
            http: reqwest::Client::new(),
        }
    }
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
            .json(&json!({
                "model": self.model,
                "messages": messages,
                "max_tokens": 2048,
            }))
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

    fn name(&self) -> &str {
        &self.model
    }
}

fn system_prompt(brief: &Brief) -> String {
    format!(
        "You are an architect in Kingdom IDE, advising the King on one project.\n\n\
         {}\n\
         Answer concisely and concretely, referring to real files above where relevant. \
         You cannot run commands or edit files: you are drawing up a proposal for review. \
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
