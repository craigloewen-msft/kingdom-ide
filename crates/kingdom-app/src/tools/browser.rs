//! Browser Deeds over Kingdom's native Chrome engine.
//!
//! These adapters contain argument validation and model-facing wording only.
//! Keeping CDP and session ownership in `kingdom-browser` avoids coupling a
//! native subprocess driver to Leptos, which would make an accidental wasm
//! dependency possible.
//!
//! Sessions are selected with [`Sandbox::plan`], not the workspace path. Two
//! plans may deliberately share a city path, but sharing browser cookies would
//! let one court member inherit another's login. This is the same per-plan
//! isolation boundary as the tmux tool.
//!
//! The same per-plan session is what the King's spyglass attaches to; see
//! `crate::spyglass`. An earlier note here called the screencast deliberately
//! absent because it served a Phoenix UI feature Kingdom lacked -- that has
//! since been built, and the reasoning no longer holds.

use super::{Refusal, Tool, Sandbox};
use kingdom_browser::{BrowserError, BrowserSessionManager, KeyMethod};
use kingdom_core::ToolOutcome;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::OnceLock, time::Duration};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const LARGE_OUTPUT: usize = 4 * 1024;

static BROWSERS: OnceLock<BrowserSessionManager> = OnceLock::new();
/// The one browser session manager, shared by the tools and by the spyglass.
///
/// Public so `crate::spyglass` can attach a viewer to a session the tools
/// created. It deliberately does *not* create one -- see
/// [`BrowserSessionManager::watch`].
pub(crate) fn browsers() -> &'static BrowserSessionManager {
    BROWSERS.get_or_init(BrowserSessionManager::new)
}
fn plan(shop: &Sandbox) -> String {
    shop.plan().to_string()
}

fn duration(value: Option<&str>, default: Duration) -> Result<Duration, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    let value = value.trim();
    let parsed = if let Some(n) = value.strip_suffix("ms") {
        n.trim().parse().ok().map(Duration::from_millis)
    } else if let Some(n) = value.strip_suffix('s') {
        n.trim().parse().ok().map(Duration::from_secs)
    } else if let Some(n) = value.strip_suffix('m') {
        n.trim()
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(60))
            .map(Duration::from_secs)
    } else {
        value.parse().ok().map(Duration::from_secs)
    };
    parsed
        .ok_or_else(|| format!("`{value}` is not a duration; use values such as 500ms, 15s or 1m"))
}

fn bad(tool: &str, detail: impl Into<String>) -> ToolOutcome {
    Refusal::BadArguments {
        tool: tool.to_string(),
        detail: detail.into(),
    }
    .into()
}
fn outcome(result: Result<String, BrowserError>) -> ToolOutcome {
    match result {
        Ok(output) => ToolOutcome::done(output),
        Err(BrowserError::ChromeUnavailable(reason)) => Refusal::Refused(reason).into(),
        Err(error) => ToolOutcome::done(error.to_string()),
    }
}
fn parse<T: for<'de> Deserialize<'de>>(tool: &str, input: Value) -> Result<T, ToolOutcome> {
    serde_json::from_value(input).map_err(|error| bad(tool, error.to_string()))
}
fn artifact(shop: &Sandbox, stem: &str, extension: &str) -> PathBuf {
    let serial = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    shop.root()
        .join(format!(".kingdom-{stem}-{serial}.{extension}"))
}
async fn write_artifact(path: &PathBuf, bytes: &[u8]) -> Result<(), BrowserError> {
    tokio::fs::write(path, bytes).await.map_err(|error| {
        BrowserError::Operation(format!("could not save {}: {error}", path.display()))
    })
}

#[derive(Deserialize)]
struct NavigateInput {
    url: String,
    timeout: Option<String>,
}
pub struct BrowserNavigate;
#[async_trait::async_trait]
impl Tool for BrowserNavigate {
    fn name(&self) -> &'static str {
        "browser_navigate"
    }
    fn description(&self) -> String {
        "Navigate to a URL and wait for loading. This plan's browser persists across Deeds, preserving cookies, JavaScript state and DOM.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["url"],"properties":{"url":{"type":"string"},"timeout":{"type":"string","description":"Default 15s; examples: 500ms, 15s, 1m."}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let input: NavigateInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let timeout = match duration(input.timeout.as_deref(), DEFAULT_TIMEOUT) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        outcome(
            browsers()
                .navigate(&plan(shop), &input.url, timeout)
                .await
                .map(|_| format!("Navigated to {}.", input.url)),
        )
    }
}

#[derive(Deserialize)]
struct EvalInput {
    expression: String,
    timeout: Option<String>,
    #[serde(default = "yes")]
    r#await: bool,
}
fn yes() -> bool {
    true
}
pub struct BrowserEval;
#[async_trait::async_trait]
impl Tool for BrowserEval {
    fn name(&self) -> &'static str {
        "browser_eval"
    }
    fn description(&self) -> String {
        "Evaluate JavaScript in the current page. Use click/type for interactions because they dispatch trusted CDP input. Results over 4KB are saved to a workspace file rather than consuming the model request.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["expression"],"properties":{"expression":{"type":"string"},"timeout":{"type":"string"},"await":{"type":"boolean","description":"Await promises; default true."}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let input: EvalInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let timeout = match duration(input.timeout.as_deref(), DEFAULT_TIMEOUT) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        match browsers()
            .evaluate(&plan(shop), &input.expression, input.r#await, timeout)
            .await
        {
            Ok(value) if value.len() > LARGE_OUTPUT => {
                let path = artifact(shop, "browser-eval", "json");
                outcome(
                    write_artifact(&path, value.as_bytes())
                        .await
                        .map(|_| format!("JavaScript output saved to {}.", path.display())),
                )
            }
            result => outcome(
                result.map(|value| format!("<javascript_result>{value}</javascript_result>")),
            ),
        }
    }
}

#[derive(Deserialize)]
struct ScreenshotInput {
    selector: Option<String>,
    timeout: Option<String>,
}
pub struct BrowserTakeScreenshot;
#[async_trait::async_trait]
impl Tool for BrowserTakeScreenshot {
    fn name(&self) -> &'static str {
        "browser_take_screenshot"
    }
    fn description(&self) -> String {
        "Capture the page or one element to a PNG file in the workspace. Returns its path; call read_image on that path to actually look at it.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"selector":{"type":"string"},"timeout":{"type":"string"}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let input: ScreenshotInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let timeout = match duration(input.timeout.as_deref(), DEFAULT_TIMEOUT) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        match browsers()
            .screenshot(&plan(shop), input.selector.as_deref(), timeout)
            .await
        {
            Ok(image) => {
                let path = artifact(shop, "browser-screenshot", "png");
                outcome(
                    write_artifact(&path, &image.png)
                        .await
                        .map(|_| format!("Screenshot saved to {}.", path.display())),
                )
            }
            Err(error) => outcome(Err(error)),
        }
    }
}

#[derive(Deserialize)]
struct ResizeInput {
    width: u32,
    height: u32,
    timeout: Option<String>,
}
pub struct BrowserResize;
#[async_trait::async_trait]
impl Tool for BrowserResize {
    fn name(&self) -> &'static str {
        "browser_resize"
    }
    fn description(&self) -> String {
        "Resize the viewport to test responsive layouts and device widths.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["width","height"],"properties":{"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"timeout":{"type":"string"}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: ResizeInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        if i.width == 0 || i.height == 0 {
            return bad(self.name(), "width and height must be positive");
        };
        let t = match duration(i.timeout.as_deref(), DEFAULT_TIMEOUT) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        outcome(
            browsers()
                .resize(&plan(shop), i.width, i.height, t)
                .await
                .map(|_| format!("Viewport resized to {}x{}.", i.width, i.height)),
        )
    }
}

#[derive(Deserialize)]
struct WaitInput {
    selector: String,
    timeout: Option<String>,
    #[serde(default)]
    visible: bool,
}
pub struct BrowserWaitForSelector;
#[async_trait::async_trait]
impl Tool for BrowserWaitForSelector {
    fn name(&self) -> &'static str {
        "browser_wait_for_selector"
    }
    fn description(&self) -> String {
        "Wait until a selector exists, or is visible, after asynchronous page work.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["selector"],"properties":{"selector":{"type":"string"},"timeout":{"type":"string","description":"Default 30s."},"visible":{"type":"boolean"}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: WaitInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let t = match duration(i.timeout.as_deref(), Duration::from_secs(30)) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        outcome(
            browsers()
                .wait_for_selector(&plan(shop), &i.selector, i.visible, t)
                .await
                .map(|_| {
                    format!(
                        "Selector `{}` is {}.",
                        i.selector,
                        if i.visible { "visible" } else { "present" }
                    )
                }),
        )
    }
}

#[derive(Deserialize)]
struct ClickInput {
    selector: String,
    #[serde(default)]
    wait: bool,
    timeout: Option<String>,
}
pub struct BrowserClick;
#[async_trait::async_trait]
impl Tool for BrowserClick {
    fn name(&self) -> &'static str {
        "browser_click"
    }
    fn description(&self) -> String {
        "Click by selector with CDP mouse events, not JavaScript click(), so framework and browser handlers receive trusted input.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["selector"],"properties":{"selector":{"type":"string"},"wait":{"type":"boolean"},"timeout":{"type":"string","description":"Default 30s."}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: ClickInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let t = match duration(i.timeout.as_deref(), Duration::from_secs(30)) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        outcome(
            browsers()
                .click(&plan(shop), &i.selector, i.wait, t)
                .await
                .map(|_| format!("Clicked `{}`.", i.selector)),
        )
    }
}

#[derive(Deserialize)]
struct TypeInput {
    selector: String,
    text: String,
    #[serde(default)]
    clear: bool,
    timeout: Option<String>,
}
pub struct BrowserType;
#[async_trait::async_trait]
impl Tool for BrowserType {
    fn name(&self) -> &'static str {
        "browser_type"
    }
    fn description(&self) -> String {
        "Focus an input and type through CDP keyboard events, not by assigning value, so React/Vue/Angular input handlers fire.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["selector","text"],"properties":{"selector":{"type":"string"},"text":{"type":"string"},"clear":{"type":"boolean"},"timeout":{"type":"string","description":"Default 30s."}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: TypeInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let t = match duration(i.timeout.as_deref(), Duration::from_secs(30)) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        let count = i.text.chars().count();
        outcome(
            browsers()
                .type_text(&plan(shop), &i.selector, &i.text, i.clear, t)
                .await
                .map(|_| format!("Typed {count} characters into `{}`.", i.selector)),
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Method {
    Cdp,
    Js,
}
#[derive(Deserialize)]
struct KeyInput {
    key: String,
    #[serde(default)]
    modifiers: Vec<String>,
    method: Option<Method>,
}
pub struct BrowserKeyPress;
#[async_trait::async_trait]
impl Tool for BrowserKeyPress {
    fn name(&self) -> &'static str {
        "browser_key_press"
    }
    fn description(&self) -> String {
        "Send a key chord. CDP is trusted but Chrome intercepts browser shortcuts; method=js reaches the page for those shortcuts but isTrusted is false.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["key"],"properties":{"key":{"type":"string"},"modifiers":{"type":"array","items":{"type":"string","enum":["ctrl","shift","alt","meta"]}},"method":{"type":"string","enum":["cdp","js"]}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: KeyInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let method = if matches!(i.method, Some(Method::Js)) {
            KeyMethod::Js
        } else {
            KeyMethod::Cdp
        };
        let chord = if i.modifiers.is_empty() {
            i.key.clone()
        } else {
            format!("{}+{}", i.modifiers.join("+"), i.key)
        };
        outcome(
            browsers()
                .key_press(&plan(shop), &i.key, &i.modifiers, method)
                .await
                .map(|_| format!("Pressed {chord}.")),
        )
    }
}

#[derive(Deserialize)]
struct LogsInput {
    limit: Option<usize>,
}
pub struct BrowserRecentConsoleLogs;
#[async_trait::async_trait]
impl Tool for BrowserRecentConsoleLogs {
    fn name(&self) -> &'static str {
        "browser_recent_console_logs"
    }
    fn description(&self) -> String {
        "Read recent console messages accumulated by this plan's browser. Clear first to isolate one interaction.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"description":"Default 100."}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: LogsInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let logs = match browsers()
            .console_logs(&plan(shop), i.limit.unwrap_or(100))
            .await
        {
            Ok(v) => v,
            Err(e) => return outcome(Err(e)),
        };
        let value = serde_json::to_string_pretty(
            &logs
                .iter()
                .map(|e| json!({"level":e.level,"text":e.text}))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into());
        if value.len() > LARGE_OUTPUT {
            let path = artifact(shop, "browser-console", "json");
            outcome(
                write_artifact(&path, value.as_bytes())
                    .await
                    .map(|_| format!("Console logs saved to {}.", path.display())),
            )
        } else {
            ToolOutcome::done(value)
        }
    }
}

pub struct BrowserClearConsoleLogs;
#[async_trait::async_trait]
impl Tool for BrowserClearConsoleLogs {
    fn name(&self) -> &'static str {
        "browser_clear_console_logs"
    }
    fn description(&self) -> String {
        "Clear this plan's captured console messages before a focused interaction.".into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    async fn run(&self, _: Value, shop: &Sandbox) -> ToolOutcome {
        outcome(
            browsers()
                .clear_console_logs(&plan(shop))
                .await
                .map(|n| format!("Cleared {n} console log entries.")),
        )
    }
}
