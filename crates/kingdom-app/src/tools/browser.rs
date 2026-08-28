//! Browser Tool calls over Kingdom's native Chrome engine.
//!
//! These adapters contain argument validation and model-facing wording only.
//! Keeping CDP and session ownership in `kingdom-browser` avoids coupling a
//! native subprocess driver to Leptos, which would make an accidental wasm
//! dependency possible.
//!
//! Sessions are selected with [`Sandbox::plan`], not the workspace path. Two
//! plans may deliberately share a city path, but sharing browser cookies would
//! let one model member inherit another's login. This is the same per-plan
//! isolation boundary as the tmux tool.
//!
//! The same per-plan session is what the user's screencast attaches to; see
//! `crate::screencast`. An earlier note here called the screencast deliberately
//! absent because it served a Phoenix UI feature Kingdom lacked -- that has
//! since been built, and the reasoning no longer holds.

use super::{Refusal, Sandbox, Tool};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use kingdom_browser::{BrowserError, BrowserSessionManager, KeyMethod};
use kingdom_core::{ToolArtifact, ToolImage, ToolOutcome, WaitBudget};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::OnceLock, time::Duration};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const LARGE_OUTPUT: usize = 4 * 1024;

static BROWSERS: OnceLock<BrowserSessionManager> = OnceLock::new();
/// The one browser session manager, shared by the tools and by the screencast.
///
/// Public so `crate::screencast` can attach a viewer to a session the tools
/// created. It deliberately does *not* create one -- see
/// [`BrowserSessionManager::watch`].
pub(crate) fn browsers() -> &'static BrowserSessionManager {
    BROWSERS.get_or_init(BrowserSessionManager::new)
}

/// Closes a plan's browser when the plan itself is over.
///
/// The exact counterpart of [`crate::tools::tmux::dismiss`], called from the
/// same place on the same terms, and here for the same reason: a browser that
/// outlives the plan that opened it is nine processes and the better part of a
/// gigabyte held by nobody, with nothing left in the records that knows what it
/// was for. That is the orphaned-resource collision this product exists to
/// prevent, so it must not be left for the user to notice.
///
/// Deliberately infallible and quiet, exactly as `tmux::dismiss` is. It runs on
/// the success path of merging or archiving, where the work has already landed;
/// failing a completed merge because a CDP socket would not close would be a
/// far worse outcome than a stray Chrome, which the next server's
/// `sweep_orphans` reclaims anyway.
pub async fn dismiss(plan: &kingdom_core::PlanId) {
    browsers().close(plan.as_str()).await;
}

/// Starts the housekeeping a long-lived server needs: a sweep, then a reaper.
///
/// Called once from `main`. Two different failures, in the order they happened:
/// the sweep clears what a *previous* server left behind when it died without
/// closing anything, and the reaper stops *this* server accumulating the same
/// thing while it runs.
///
/// Returns how many orphaned profiles the sweep reclaimed, so the banner can
/// say so -- a number the user should see, because it is the size of a problem
/// that used to be invisible.
pub fn start_housekeeping() -> usize {
    // Teach the browser crate how to enter a plan's network namespace, before
    // any browser can be launched. Without this a plan with its own network
    // would drive a browser on the *host's* network, and `localhost:3000` in
    // that browser would be somebody else's server -- see
    // `kingdom_browser::on_enter_namespace`.
    kingdom_browser::on_enter_namespace(|plan| {
        crate::namespaces::enter_prefix(&kingdom_core::PlanId::new(plan))
    });

    // And how to reserve a fixed CDP port and its relay -- the companion hook,
    // for the same reason: a namespaced plan's browser needs a knowable port
    // *before* Chrome launches, and only `kingdom-app` knows what a namespace
    // is. See `kingdom_browser::on_reserve_cdp_port`.
    kingdom_browser::on_reserve_cdp_port(|plan| {
        let plan = kingdom_core::PlanId::new(plan);
        async move { crate::namespaces::net::reserve_cdp_port(&plan).await }
    });

    let reclaimed = kingdom_browser::sweep_orphans();
    // The handle is dropped deliberately: the reaper lives as long as the
    // server does, and there is no shutdown path that would want to stop it
    // early. Dropping a `JoinHandle` detaches the task rather than cancelling
    // it.
    let _ = browsers().start_reaper();
    reclaimed
}
fn plan(shop: &Sandbox) -> String {
    shop.plan().to_string()
}

/// The wait a browser call is given when the model does not say.
///
/// Two figures rather than one, because the tools split into two honest
/// groups: an operation on a page that is already there should be quick, while
/// one that waits for the page to *become* something needs room for the app to
/// do its work. Named because the deed line reports these back to the King --
/// see `Tool::waits_for` -- and an inline `Duration::from_secs(30)` in each
/// `run` would leave the two halves free to disagree about the same call.
const DEFAULT_SETTLE: Duration = Duration::from_secs(30);

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

/// The wait every browser call reports, and the one place that reads a
/// `timeout` argument for the chamber rather than for the browser.
///
/// Always a [`WaitBudget::Deadline`]: when a browser call runs out of time it
/// has failed, and there is nothing left running to come back to. That is the
/// opposite of `bash`, and the reason the King can read one figure differently
/// from the other.
///
/// A `timeout` that will not parse reports the default rather than nothing. The
/// call is about to be refused for exactly that reason, and the line is better
/// showing the wait that was meant than showing silence.
fn deadline(input: &Value, default: Duration) -> Option<WaitBudget> {
    let asked = input.get("timeout").and_then(Value::as_str);
    Some(WaitBudget::Deadline {
        seconds: duration(asked, default).unwrap_or(default).as_secs(),
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
        if let Err(error) = browsers().navigate(&plan(shop), &input.url, timeout).await {
            return outcome(Err(error));
        }
        // The viewport is reported with the landing, so a model reasoning about
        // layout has the number without spending a whole round asking the page
        // for `innerWidth`. Asked *after* the navigation, since it is this
        // page's size that was wanted, and best effort: a page that will not
        // answer is still a page that was navigated to.
        let size = browsers()
            .evaluate(
                &plan(shop),
                "`${innerWidth}x${innerHeight}`",
                false,
                timeout,
            )
            .await
            .ok()
            .map(|value| value.trim_matches('"').to_string())
            .filter(|value| value.contains('x'));
        ToolOutcome::done(match size {
            Some(size) => format!("Navigated to {}. Viewport {size}.", input.url),
            None => format!("Navigated to {}.", input.url),
        })
    }

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_TIMEOUT)
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

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_TIMEOUT)
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
        "Capture the page or one element to a PNG file in the workspace. The \
         King is shown it in the chamber, and the picture comes back with this \
         call -- you do not need read_image to look at what you just captured."
            .into()
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
                let artifacts = shop
                    .relative(&path)
                    .map(|path| {
                        vec![ToolArtifact {
                            path,
                            media_type: "image/png".to_string(),
                        }]
                    })
                    .unwrap_or_default();

                match write_artifact(&path, &image.png).await {
                    // Saved, named *and* handed over. The file is written
                    // regardless -- the chamber renders it from disk and the
                    // King's copy must outlive the request -- but the bytes now
                    // ride back with the call that produced them.
                    //
                    // This used to return the path alone and leave the model to
                    // spend a second round on `read_image`, on the reasoning
                    // that "the bytes must not be spent on a model that may not
                    // need them". The records say it always needed them: across
                    // every plan in a real kingdom, 131 screenshots were
                    // followed by 128 `read_image` calls. 98% is not a model
                    // deciding; it is a round trip with a foregone conclusion,
                    // and 27 of them fell inside four plans alone.
                    //
                    // Nothing about the *weight* changes. `copilot::shown` puts
                    // an image on the wire only while it is within
                    // `RECENT_REPLIES`, so a picture delivered here decays
                    // exactly as one delivered by `read_image` did -- it simply
                    // starts one round earlier and costs one round less.
                    //
                    // The half of the old reasoning that was right is kept: a
                    // model that cannot see is handed the path and nothing more.
                    // `read_image` is unchanged and remains the way to look at a
                    // file this call did not create.
                    Ok(()) if shop.sighted() => ToolOutcome::seen(
                        format!("Screenshot saved to {}, and attached.", path.display()),
                        vec![ToolImage {
                            media_type: "image/png".to_string(),
                            data: BASE64.encode(&image.png),
                        }],
                    )
                    .leaving(artifacts),
                    Ok(()) => ToolOutcome::produced(
                        format!("Screenshot saved to {}.", path.display()),
                        artifacts,
                    ),
                    Err(error) => outcome(Err(error)),
                }
            }
            Err(error) => outcome(Err(error)),
        }
    }

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_TIMEOUT)
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

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_TIMEOUT)
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
        let t = match duration(i.timeout.as_deref(), DEFAULT_SETTLE) {
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

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_SETTLE)
    }
}

#[derive(Deserialize)]
struct ClickInput {
    selector: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
    #[serde(default)]
    wait: bool,
    timeout: Option<String>,
}

/// What a click was aimed at: an element, or a place.
#[derive(Debug)]
enum Target {
    Selector(String),
    Point(f64, f64),
}

/// Reads the target out of the arguments, or says why it could not.
///
/// Exactly one of the two forms, and a refusal that names which mistake was
/// made rather than restating the schema -- a model told "invalid arguments"
/// retries the same call, where one told it gave both corrects itself.
fn target_of(input: &ClickInput) -> Result<Target, String> {
    match (&input.selector, input.x, input.y) {
        (Some(selector), None, None) => Ok(Target::Selector(selector.clone())),
        (None, Some(x), Some(y)) => Ok(Target::Point(x, y)),
        (Some(_), _, _) => {
            Err("give either `selector` or `x`/`y`, not both: a click has one target.".to_string())
        }
        (None, None, None) => {
            Err("give a `selector`, or `x` and `y` to click a point in the viewport.".to_string())
        }
        (None, _, _) => {
            Err("clicking a point needs both `x` and `y`; only one was given.".to_string())
        }
    }
}

pub struct BrowserClick;
#[async_trait::async_trait]
impl Tool for BrowserClick {
    fn name(&self) -> &'static str {
        "browser_click"
    }
    fn description(&self) -> String {
        "Click by selector with CDP mouse events, not JavaScript click(), so \
         framework and browser handlers receive trusted input. Give `x`/`y` \
         instead of a selector to click a point in the viewport -- that is the \
         only way to hit something inside a <canvas>, where every drawn thing \
         is the same element. The pointer settles on the target before the \
         press, so hover-driven interfaces react as they would to a person."
            .into()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "selector":{"type":"string","description":"The element to click. Omit when clicking a point."},
            "x":{"type":"number","description":"Viewport x, in CSS pixels. Use with `y`, instead of a selector."},
            "y":{"type":"number","description":"Viewport y, in CSS pixels. Use with `x`, instead of a selector."},
            "wait":{"type":"boolean","description":"Wait for the selector to be visible first. Ignored for a point."},
            "timeout":{"type":"string","description":"Default 30s."}}})
    }
    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        let i: ClickInput = match parse(self.name(), input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let target = match target_of(&i) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        let t = match duration(i.timeout.as_deref(), DEFAULT_SETTLE) {
            Ok(v) => v,
            Err(e) => return bad(self.name(), e),
        };
        match target {
            Target::Selector(selector) => outcome(
                browsers()
                    .click(&plan(shop), &selector, i.wait, t)
                    .await
                    .map(|_| format!("Clicked `{selector}`.")),
            ),
            Target::Point(x, y) => outcome(
                browsers()
                    .click_at(&plan(shop), x, y, t)
                    .await
                    .map(|_| format!("Clicked ({x}, {y}).")),
            ),
        }
    }

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_SETTLE)
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
        let t = match duration(i.timeout.as_deref(), DEFAULT_SETTLE) {
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

    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        deadline(input, DEFAULT_SETTLE)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn clicking(value: Value) -> Result<Target, String> {
        let input: ClickInput = serde_json::from_value(value).expect("the fixture parses");
        target_of(&input)
    }

    /// Either form works, and nothing else is needed to express it.
    #[test]
    fn a_click_may_name_an_element_or_a_place() {
        assert!(matches!(
            clicking(json!({"selector": ".start-btn"})),
            Ok(Target::Selector(s)) if s == ".start-btn"
        ));
        assert!(matches!(
            clicking(json!({"x": 640.0, "y": 400.0})),
            Ok(Target::Point(x, y)) if x == 640.0 && y == 400.0
        ));
    }

    /// A click has one target, and a half-given point is not one.
    ///
    /// Each refusal names the *specific* mistake rather than restating the
    /// schema, for the reason `Refusal`'s own docs give: a model told only that
    /// its arguments were invalid retries the identical call, where one told it
    /// gave both forms drops one and gets on with it.
    #[test]
    fn a_click_with_no_single_target_is_refused_by_name() {
        let both = clicking(json!({"selector": "#map", "x": 1.0, "y": 2.0}))
            .expect_err("two targets is not a click");
        assert!(both.contains("not both"), "{both}");

        let neither = clicking(json!({})).expect_err("no target is not a click");
        assert!(neither.contains("`x` and `y`"), "{neither}");

        let half = clicking(json!({"x": 640.0})).expect_err("half a point is not a click");
        assert!(half.contains("both"), "{half}");
    }
}
