//! Per-plan Chrome sessions and the CDP operations shared by browser tools.
//!
//! The manager owns state instead of each tool value because Kingdom rebuilds
//! its tool list for every Deed. State on a tool would therefore look persistent
//! in one call and vanish in the next, breaking login and any other flow whose
//! meaning spans multiple browser actions.

use chromiumoxide::{
    browser::{Browser, BrowserConfig},
    cdp::{
        browser_protocol::{
            emulation::SetDeviceMetricsOverrideParams,
            input::{DispatchKeyEventParams, DispatchKeyEventType},
            page::CaptureScreenshotFormat,
        },
        js_protocol::runtime::{EvaluateParams, EventConsoleApiCalled},
    },
    page::ScreenshotParams,
    Page,
};
use futures::StreamExt;
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use thiserror::Error;
use tokio::{sync::RwLock, task::JoinHandle};

const INIT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONSOLE_LOGS: usize = 1_000;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("Chrome is not available. Install Google Chrome or Chromium, or set KINGDOM_CHROME_EXECUTABLE to its executable path. Details: {0}")]
    ChromeUnavailable(String),
    #[error("browser operation failed: {0}")]
    Operation(String),
    #[error("browser operation timed out after {0:?}")]
    Timeout(Duration),
}

impl From<chromiumoxide::error::CdpError> for BrowserError {
    fn from(error: chromiumoxide::error::CdpError) -> Self {
        Self::Operation(error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
}

pub struct Screenshot {
    pub png: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMethod {
    Cdp,
    Js,
}

struct BrowserSession {
    // Dropping this handle closes Chrome. Keeping it beside the page makes that
    // lifetime explicit; retaining only `Page` produces a dead CDP connection.
    _browser: Browser,
    _handler: JoinHandle<()>,
    _console: JoinHandle<()>,
    page: Page,
    console: Arc<Mutex<VecDeque<ConsoleEntry>>>,
}

impl BrowserSession {
    async fn launch(plan: &str) -> Result<Self, BrowserError> {
        // Profiles persist between Deeds because the Chrome process persists.
        // Reusing one after a server crash leaves Chromium's SingletonLock
        // behind and turns every future launch into a false "Chrome missing".
        let profile = profile_dir(plan);
        let _ = std::fs::remove_dir_all(&profile);
        let mut builder = BrowserConfig::builder()
            .new_headless_mode()
            .no_sandbox()
            .arg("--disable-gpu")
            .viewport(chromiumoxide::handler::viewport::Viewport {
                width: 1024,
                height: 768,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                is_landscape: true,
                has_touch: false,
            })
            .user_data_dir(profile);

        if let Ok(executable) = std::env::var("KINGDOM_CHROME_EXECUTABLE") {
            let path = PathBuf::from(&executable);
            if !path.is_file() {
                return Err(BrowserError::ChromeUnavailable(format!(
                    "KINGDOM_CHROME_EXECUTABLE points to {}, which is not a file",
                    path.display()
                )));
            }
            builder = builder.chrome_executable(path);
        }

        let config = builder.build().map_err(BrowserError::ChromeUnavailable)?;
        let (mut browser, mut handler) =
            tokio::time::timeout(INIT_TIMEOUT, Browser::launch(config))
                .await
                .map_err(|_| BrowserError::ChromeUnavailable("launch timed out".to_string()))?
                .map_err(|error| BrowserError::ChromeUnavailable(error.to_string()))?;

        let handler_task =
            tokio::spawn(async move { while let Some(_event) = handler.next().await {} });
        let page = match tokio::time::timeout(INIT_TIMEOUT, browser.new_page("about:blank")).await {
            Ok(Ok(page)) => page,
            Ok(Err(error)) => {
                handler_task.abort();
                let _ = browser.kill().await;
                return Err(BrowserError::ChromeUnavailable(error.to_string()));
            }
            Err(_) => {
                handler_task.abort();
                let _ = browser.kill().await;
                return Err(BrowserError::ChromeUnavailable(
                    "Chrome launched but its first page timed out".to_string(),
                ));
            }
        };

        let console = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_CONSOLE_LOGS)));
        let mut events = page.event_listener::<EventConsoleApiCalled>().await?;
        let captured = Arc::clone(&console);
        let console_task = tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let text = event
                    .args
                    .iter()
                    .map(|arg| {
                        arg.value
                            .as_ref()
                            .map(|value| match value {
                                Value::String(text) => text.clone(),
                                other => other.to_string(),
                            })
                            .or_else(|| arg.description.clone())
                            .unwrap_or_else(|| "undefined".to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut logs = captured
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if logs.len() == MAX_CONSOLE_LOGS {
                    logs.pop_front();
                }
                logs.push_back(ConsoleEntry {
                    level: format!("{:?}", event.r#type).to_lowercase(),
                    text,
                });
            }
        });

        Ok(Self {
            _browser: browser,
            _handler: handler_task,
            _console: console_task,
            page,
            console,
        })
    }
}

/// Owns one live Chrome session per plan.
///
/// A process-global browser would leak cookies and page state between plans;
/// per-Deed browsers lose exactly the continuity these tools exist to inspect.
/// The plan id is therefore the session boundary, just as it is for tmux.
pub struct BrowserSessionManager {
    sessions: RwLock<HashMap<String, Arc<RwLock<BrowserSession>>>>,
}

impl Default for BrowserSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserSessionManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    async fn session(&self, plan: &str) -> Result<Arc<RwLock<BrowserSession>>, BrowserError> {
        if let Some(session) = self.sessions.read().await.get(plan).cloned() {
            return Ok(session);
        }
        // Keep the write lock through launch. Launch is rare, and allowing two
        // concurrent first Deeds to race would orphan one Chrome process.
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get(plan).cloned() {
            return Ok(session);
        }
        let session = Arc::new(RwLock::new(BrowserSession::launch(plan).await?));
        sessions.insert(plan.to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub async fn navigate(
        &self,
        plan: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        timed(timeout, guard.page.goto(url)).await?;
        Ok(())
    }

    pub async fn evaluate(
        &self,
        plan: &str,
        expression: &str,
        await_promise: bool,
        timeout: Duration,
    ) -> Result<String, BrowserError> {
        let params = EvaluateParams::builder()
            .expression(expression)
            .await_promise(await_promise)
            .build()
            .map_err(|error| BrowserError::Operation(error.to_string()))?;
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let result = timed(timeout, guard.page.evaluate(params)).await?;
        Ok(match result.value() {
            Some(value) => {
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
            }
            None => "undefined".to_string(),
        })
    }

    pub async fn screenshot(
        &self,
        plan: &str,
        selector: Option<&str>,
        timeout: Duration,
    ) -> Result<Screenshot, BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let png = match selector {
            Some(selector) => {
                let element = timed(timeout, guard.page.find_element(selector)).await?;
                timed(timeout, element.screenshot(CaptureScreenshotFormat::Png)).await?
            }
            None => {
                timed(
                    timeout,
                    guard.page.screenshot(ScreenshotParams::builder().build()),
                )
                .await?
            }
        };
        Ok(Screenshot { png })
    }

    pub async fn resize(
        &self,
        plan: &str,
        width: u32,
        height: u32,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let params = SetDeviceMetricsOverrideParams::builder()
            .width(width)
            .height(height)
            .device_scale_factor(1.0)
            .mobile(false)
            .build()
            .map_err(|error| BrowserError::Operation(error.to_string()))?;
        let session = self.session(plan).await?;
        let guard = session.read().await;
        timed(timeout, guard.page.execute(params)).await?;
        Ok(())
    }

    pub async fn wait_for_selector(
        &self,
        plan: &str,
        selector: &str,
        visible: bool,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let quoted = serde_json::to_string(selector).expect("a string always serializes");
        let script = if visible {
            format!("(() => {{ const el=document.querySelector({quoted}); if(!el) return false; const s=getComputedStyle(el); const r=el.getBoundingClientRect(); return s.display!=='none' && s.visibility!=='hidden' && s.opacity!=='0' && r.width>0 && r.height>0; }})()")
        } else {
            format!("document.querySelector({quoted}) !== null")
        };
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let started = tokio::time::Instant::now();
        loop {
            let found = guard
                .page
                .evaluate(script.clone())
                .await?
                .into_value::<bool>()
                .unwrap_or(false);
            if found {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(BrowserError::Timeout(timeout));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn click(
        &self,
        plan: &str,
        selector: &str,
        wait: bool,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        if wait {
            self.wait_for_selector(plan, selector, true, timeout)
                .await?;
        }
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let element = timed(timeout, guard.page.find_element(selector)).await?;
        // chromiumoxide's Element::click dispatches mousePressed/mouseReleased
        // through CDP. JS `click()` is intentionally not used: it bypasses the
        // trusted input path frameworks and browser defaults listen to.
        timed(timeout, element.click()).await?;
        Ok(())
    }

    pub async fn type_text(
        &self,
        plan: &str,
        selector: &str,
        text: &str,
        clear: bool,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let element = timed(timeout, guard.page.find_element(selector)).await?;
        timed(timeout, element.click()).await?;
        if clear {
            dispatch_key(&guard.page, "a", &["ctrl".to_string()]).await?;
            dispatch_key(&guard.page, "Backspace", &[]).await?;
        }
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                dispatch_key(&guard.page, "Enter", &[]).await?;
            }
            if !part.is_empty() {
                timed(timeout, element.type_str(part)).await?;
            }
        }
        Ok(())
    }

    pub async fn key_press(
        &self,
        plan: &str,
        key: &str,
        modifiers: &[String],
        method: KeyMethod,
    ) -> Result<(), BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        match method {
            KeyMethod::Cdp => dispatch_key(&guard.page, key, modifiers).await,
            KeyMethod::Js => {
                let (key, code, _) = key_info(key)?;
                let lower: Vec<String> = modifiers.iter().map(|m| m.to_lowercase()).collect();
                let script = format!(
                    "(() => {{ const o={{key:{},code:{},ctrlKey:{},shiftKey:{},altKey:{},metaKey:{},bubbles:true,cancelable:true,composed:true}}; window.dispatchEvent(new KeyboardEvent('keydown',o)); window.dispatchEvent(new KeyboardEvent('keyup',o)); }})()",
                    serde_json::to_string(&key).unwrap(), serde_json::to_string(&code).unwrap(),
                    lower.iter().any(|m| m == "ctrl" || m == "control"), lower.iter().any(|m| m == "shift"),
                    lower.iter().any(|m| m == "alt"), lower.iter().any(|m| m == "meta" || m == "cmd" || m == "command")
                );
                guard.page.evaluate(script).await?;
                Ok(())
            }
        }
    }

    pub async fn console_logs(
        &self,
        plan: &str,
        limit: usize,
    ) -> Result<Vec<ConsoleEntry>, BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let logs = guard
            .console
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(logs.iter().rev().take(limit).cloned().collect())
    }

    pub async fn clear_console_logs(&self, plan: &str) -> Result<usize, BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let mut logs = guard
            .console
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = logs.len();
        logs.clear();
        Ok(count)
    }
}

async fn timed<T, E>(
    timeout: Duration,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, BrowserError>
where
    E: std::fmt::Display,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| BrowserError::Timeout(timeout))?
        .map_err(|error| BrowserError::Operation(error.to_string()))
}

fn profile_dir(plan: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    plan.hash(&mut hasher);
    Path::new("/tmp").join(format!("kingdom-chrome-{:016x}", hasher.finish()))
}

fn key_info(key: &str) -> Result<(String, String, i64), BrowserError> {
    let named = match key {
        "Escape" => ("Escape", "Escape", 27),
        "Enter" => ("Enter", "Enter", 13),
        "Tab" => ("Tab", "Tab", 9),
        "Backspace" => ("Backspace", "Backspace", 8),
        "Delete" => ("Delete", "Delete", 46),
        "Home" => ("Home", "Home", 36),
        "End" => ("End", "End", 35),
        "PageUp" => ("PageUp", "PageUp", 33),
        "PageDown" => ("PageDown", "PageDown", 34),
        "ArrowUp" => ("ArrowUp", "ArrowUp", 38),
        "ArrowDown" => ("ArrowDown", "ArrowDown", 40),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37),
        "ArrowRight" => ("ArrowRight", "ArrowRight", 39),
        key if key.starts_with('F')
            && key[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            let n = key[1..].parse::<i64>().unwrap();
            return Ok((key.to_string(), key.to_string(), 111 + n));
        }
        key if key.chars().count() == 1 => {
            let character = key.chars().next().unwrap();
            if character.is_ascii_alphabetic() {
                let upper = character.to_ascii_uppercase();
                return Ok((key.to_string(), format!("Key{upper}"), upper as i64));
            }
            if character.is_ascii_digit() {
                return Ok((
                    key.to_string(),
                    format!("Digit{character}"),
                    character as i64,
                ));
            }
            return Err(BrowserError::Operation(format!("unsupported key `{key}`")));
        }
        _ => return Err(BrowserError::Operation(format!("unsupported key `{key}`"))),
    };
    Ok((named.0.to_string(), named.1.to_string(), named.2))
}

async fn dispatch_key(page: &Page, key: &str, modifiers: &[String]) -> Result<(), BrowserError> {
    let (key_name, code, virtual_key) = key_info(key)?;
    let mask = modifiers.iter().fold(0_i64, |mask, modifier| {
        match modifier.to_lowercase().as_str() {
            "alt" => mask | 1,
            "ctrl" | "control" => mask | 2,
            "meta" | "cmd" | "command" => mask | 4,
            "shift" => mask | 8,
            _ => mask,
        }
    });
    let printable = key.chars().count() == 1 && !key.chars().next().unwrap().is_control();
    for event_type in [
        DispatchKeyEventType::RawKeyDown,
        DispatchKeyEventType::KeyDown,
        DispatchKeyEventType::KeyUp,
    ] {
        if event_type == DispatchKeyEventType::KeyDown && !printable {
            continue;
        }
        let mut builder = DispatchKeyEventParams::builder()
            .r#type(event_type.clone())
            .key(key_name.clone())
            .code(code.clone())
            .windows_virtual_key_code(virtual_key)
            .native_virtual_key_code(virtual_key)
            .modifiers(mask);
        // RawKeyDown carries shortcuts; KeyDown carries printable text. Sending
        // text on RawKeyDown duplicates characters in inputs, while omitting
        // KeyDown makes a plain `a` chord observable but unable to type.
        if event_type == DispatchKeyEventType::KeyDown {
            builder = builder.text(key.to_string());
        }
        let params = builder
            .build()
            .map_err(|error| BrowserError::Operation(error.to_string()))?;
        page.execute(params).await?;
    }
    Ok(())
}
