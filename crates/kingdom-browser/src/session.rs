//! Per-plan Chrome sessions and the CDP operations shared by browser tools.
//!
//! The manager owns state instead of each tool value because Kingdom rebuilds
//! its tool list for every Tool call. State on a tool would therefore look
//! persistent in one call and vanish in the next, breaking login and any other
//! flow whose meaning spans multiple browser actions.

use crate::profile::{PerfReading, ProfilingState};
use crate::screencast::{ScreencastBroker, ScreencastEvent};
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
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{sync::RwLock, task::JoinHandle};

const INIT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONSOLE_LOGS: usize = 1_000;

/// How long a browser is given to close politely before it is killed.
///
/// Short on purpose. This runs on paths where the decision is already made --
/// the plan has settled, or the session has gone cold -- so the only question
/// is whether Chrome gets to flush its profile on the way out. Waiting minutes
/// for a wedged socket to answer that would hold up every session behind it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the reaper looks for sessions that have gone cold.
///
/// Much shorter than the idle window it enforces, so that a browser is closed
/// near the moment it qualifies rather than up to a full window later.
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// The viewport a plan's browser opens at, unless [`VIEWPORT_VAR`] says
/// otherwise.
///
/// Chosen against *Kingdom's own* responsive thresholds rather than picked as a
/// round number. `kingdom_app::app::RAIL_FOLDS_BELOW` folds the cities rail
/// under 1250px and `style/components/_file-tree.scss` hides the files rail
/// under its own; the previous default of 1024x768 sat on the wrong side of
/// both, so a plan opening the chamber to check its work was shown a *folded*
/// interface and had to resize before its first screenshot was worth taking.
/// 1440x900 clears every threshold this product has, with room to spare for the
/// next one.
const DEFAULT_VIEWPORT: (u32, u32) = (1440, 900);

/// Overrides [`DEFAULT_VIEWPORT`], as `WIDTHxHEIGHT` -- for deliberately
/// testing a narrow layout, which is the one case the default is wrong for.
pub const VIEWPORT_VAR: &str = "KINGDOM_BROWSER_VIEWPORT";

/// Takes WebGL *away* from a plan's browser, for a machine that wants it back.
///
/// **On by default**, which is the opposite of what this variable used to mean,
/// and the reversal is worth explaining because the old default was chosen from
/// a measurement that blamed the wrong thing.
///
/// # What was actually expensive
///
/// Not WebGL. Pace. A headless browser has no GPU, so Chrome rasterises in
/// SwiftShader on the CPU, where cost is very nearly linear in frames drawn --
/// measured on one full-viewport WebGL page at 1440x900: 5.46 cores uncapped,
/// 1.04 at ten frames a second, 0.20 at two.
///
/// Kingdom's own map made the point sharply. Forty cities, headless, rasteriser
/// on: **9.50 cores** on the map's own screen, and **0.00** in the rail beside
/// a conversation -- the same scene drawn just as completely, differing only in
/// how often. The engine now holds itself to `engine::AUTOMATED_WAKE` whenever
/// `navigator.webdriver` is set (see `kingdom_citymap::mode`), so the expensive
/// case is gone at its source and the rasteriser is affordable to leave on.
///
/// # What turning it off still buys
///
/// The cap is Kingdom's own, and reaches only Kingdom's own map. A plan sent to
/// *someone else's* WebGL page -- a globe, a game, a three.js demo -- meets
/// whatever frame rate that page asks for, and pays the uncapped price for it.
/// `KINGDOM_BROWSER_WEBGL=off` is the blunt instrument for that, and for any
/// machine where a plan has no business rendering at all.
///
/// [`crate::session::CPUS_VAR`] is the gentler answer to the same worry: it
/// bounds what any page can take without taking WebGL from all of them.
pub const WEBGL_VAR: &str = "KINGDOM_BROWSER_WEBGL";

/// How many CPUs a plan's browser may spread itself across.
///
/// Defaults to [`DEFAULT_BROWSER_CPUS`]. `0` -- or any of the usual words for
/// no -- lifts the limit entirely.
///
/// # Why a browser is confined at all, measured
///
/// Because of how software rendering scales, which is the opposite of the way
/// one would hope. With no GPU, Chrome rasterises in SwiftShader, and
/// SwiftShader sizes its thread pool from the machine: on a twenty-core box it
/// will happily use most of them, whether or not the page needs it.
///
/// Kingdom's own map, headless, world already standing and nothing happening:
///
/// | CPUs allowed | Cost |
/// |---|---|
/// | all twenty | 7.5 cores |
/// | six | 2.06 cores |
/// | four | 1.72 cores |
/// | two | 1.29 cores |
///
/// Note what that table is *not*. It is not the frame rate -- the same map at
/// one frame a second still cost 4.09 cores unconfined. Most of what
/// SwiftShader spends is spent whether or not there is a frame to draw, which
/// is why pacing alone could not fix this and why the ceiling is a default
/// rather than an option.
///
/// A count, not a mask: the CPUs chosen are this process's own, so a Kingdom
/// already confined to some cores hands out a subset of those rather than
/// escaping onto cores it was denied.
pub const CPUS_VAR: &str = "KINGDOM_BROWSER_CPUS";

/// How many CPUs a browser gets unless [`CPUS_VAR`] says otherwise.
///
/// Four, from the table above: the knee of the curve. Two is cheaper still but
/// slows the work that is genuinely *bounded* -- a world took eight seconds to
/// stand on two CPUs against about five on four -- and that is time a plan
/// spends waiting rather than cost the machine absorbs. Four keeps a browser
/// under two cores while leaving enough parallelism to get the expensive,
/// finite things over with.
const DEFAULT_BROWSER_CPUS: usize = 4;

/// How long a browser may sit untouched before it is closed, as a duration.
///
/// See [`idle_timeout`] for the default and the reasoning.
pub const IDLE_VAR: &str = "KINGDOM_BROWSER_IDLE";

/// How long the pointer rests on a target before the button goes down.
///
/// Not politeness: a hover-driven UI cannot react to a press that arrives in
/// the same batch as the move. chromiumoxide's `click` dispatches `mouseMoved`
/// and `mousePressed` back to back over one CDP connection, so a page whose
/// click handler reads *what is currently hovered* -- Kingdom's own map is one,
/// and so is every tooltip and menu that opens on hover -- sees the press
/// before any frame has been drawn with the pointer in its new place. A beat
/// between the two is what makes a synthetic click behave like a human one.
const HOVER_SETTLE: Duration = Duration::from_millis(120);

/// The viewport to launch at: the environment's, else [`DEFAULT_VIEWPORT`].
///
/// A value that will not parse falls back rather than failing the launch. A
/// typo'd size is worth ignoring; it is not worth leaving a plan with no
/// browser at all, and the fallback is a working viewport rather than a guess.
fn configured_viewport() -> (u32, u32) {
    let Ok(raw) = std::env::var(VIEWPORT_VAR) else {
        return DEFAULT_VIEWPORT;
    };
    parse_viewport(&raw).unwrap_or(DEFAULT_VIEWPORT)
}

fn parse_viewport(raw: &str) -> Option<(u32, u32)> {
    let (width, height) = raw.trim().split_once(['x', 'X'])?;
    let width: u32 = width.trim().parse().ok()?;
    let height: u32 = height.trim().parse().ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

/// Whether this machine's browsers are to have WebGL, per [`WEBGL_VAR`].
///
/// Yes unless explicitly refused. The asymmetry with the old reading is
/// deliberate: a typo in this variable now leaves a *working* browser rather
/// than a silently crippled one, which is the right way round for a setting
/// whose failure mode used to be "the map never appears and nobody knows why".
fn webgl_wanted() -> bool {
    !std::env::var(WEBGL_VAR).is_ok_and(|raw| reads_as_no(&raw))
}

/// Whether a setting's value is an explicit no.
///
/// Separated from the environment read so it can be tested without mutating
/// process-wide state, which two tests running in parallel would race over.
///
/// Only an unambiguous no counts, in the same spirit the old yes-reading had:
/// a value nobody can read as a refusal leaves the default alone.
fn reads_as_no(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "off" | "0" | "false" | "no"
    )
}

/// How many CPUs to confine a browser to, per [`CPUS_VAR`].
///
/// `None` means do not confine at all, which the King can ask for with `0` or
/// with any of the words [`reads_as_no`] accepts. Anything unparsable falls
/// back to [`DEFAULT_BROWSER_CPUS`] rather than to no limit, because a typo
/// should not quietly hand a browser the whole machine.
fn configured_cpus() -> Option<usize> {
    let Ok(raw) = std::env::var(CPUS_VAR) else {
        return Some(DEFAULT_BROWSER_CPUS);
    };
    parse_cpus(&raw)
}

/// Reads a CPU ceiling, separated from the environment so it can be tested.
///
/// `None` for an explicit refusal; [`DEFAULT_BROWSER_CPUS`] for anything that
/// does not parse as a number of CPUs.
fn parse_cpus(raw: &str) -> Option<usize> {
    let trimmed = raw.trim();
    if reads_as_no(trimmed) {
        return None;
    }
    match trimmed.parse::<usize>() {
        Ok(0) => None,
        Ok(count) => Some(count),
        Err(_) => Some(DEFAULT_BROWSER_CPUS),
    }
}

/// How long a session may go untouched before the reaper closes it.
///
/// Fifteen minutes, not one. Relaunching is cheap in time -- a quarter of a
/// second to a live CDP socket -- but it is not cheap in *meaning*: a session
/// carries the cookies and page state that make a multi-step flow testable, and
/// that is the whole reason sessions are per-plan rather than per-call. The
/// window is therefore long enough that a model thinking between Tool calls
/// never loses its login, and short enough that a plan which browsed once this
/// morning is not still holding nine processes tonight.
///
/// `0` disables the reaper, for anyone who would rather keep every browser.
fn idle_timeout() -> Option<Duration> {
    const DEFAULT: Duration = Duration::from_secs(15 * 60);
    let Ok(raw) = std::env::var(IDLE_VAR) else {
        return Some(DEFAULT);
    };
    match parse_idle(&raw) {
        // An explicit zero is an instruction, not a failure: keep everything.
        Some(zero) if zero.is_zero() => None,
        Some(wanted) => Some(wanted),
        // Unparseable falls back rather than failing, exactly as the viewport
        // does -- a typo is not worth leaving every browser immortal.
        None => Some(DEFAULT),
    }
}

/// Reads an idle window: bare seconds, or a `s`/`m`/`h` suffix.
fn parse_idle(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    let (number, scale) = match raw.strip_suffix(['s', 'S']) {
        Some(number) => (number, 1),
        None => match raw.strip_suffix(['m', 'M']) {
            Some(number) => (number, 60),
            None => match raw.strip_suffix(['h', 'H']) {
                Some(number) => (number, 60 * 60),
                None => (raw, 1),
            },
        },
    };
    let value: u64 = number.trim().parse().ok()?;
    Some(Duration::from_secs(value.checked_mul(scale)?))
}

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
    //
    // Not `_browser` any more: [`BrowserSession::shut_down`] needs it by
    // mutable reference to ask Chrome to leave politely before killing it.
    browser: Browser,
    handler: JoinHandle<()>,
    console_task: JoinHandle<()>,
    /// This session's user-data directory, so closing can take it with it.
    profile: PathBuf,
    /// The namespace shim this session launched through, if it had one.
    ///
    /// Kept only so shutting down can delete it. See
    /// [`write_namespace_wrapper`].
    namespace_wrapper: Option<PathBuf>,
    /// When this session was last asked to do anything.
    ///
    /// Read by the reaper, written by every operation that goes through
    /// [`BrowserSessionManager::session`]. A `Mutex` rather than the session's
    /// own `RwLock` because touching must be possible under a *read* guard --
    /// browser calls hold one for their whole duration, and a clock that
    /// required exclusive access would serialise them all behind each other.
    last_used: Mutex<Instant>,
    page: Page,
    console: Arc<Mutex<VecDeque<ConsoleEntry>>>,
    /// The live screencast, if anyone is watching.
    ///
    /// `Weak` deliberately: the viewers own the broker, so it dies when the last
    /// of them goes and Chrome stops painting frames nobody is looking at. A
    /// strong reference here would keep every screencast alive for the life of
    /// the session, which is the exact cost the lazy start exists to avoid.
    ///
    /// It is also what pins a session against the reaper: a browser somebody is
    /// *watching* is a browser in use, whatever the clock says.
    screencast: tokio::sync::Mutex<std::sync::Weak<ScreencastBroker>>,
    /// Which profiling machines this session has running.
    ///
    /// On the session because that is their scope: a CPU profile belongs to a
    /// browser, not to the call that started it, and the call that stops it is
    /// a different one entirely.
    profiling: Arc<Mutex<ProfilingState>>,
}

impl BrowserSession {
    /// Attaches a viewer, starting the screencast if this is the first.
    async fn watch(
        &self,
    ) -> Result<
        (
            Arc<ScreencastBroker>,
            tokio::sync::broadcast::Receiver<ScreencastEvent>,
            Option<String>,
        ),
        BrowserError,
    > {
        let mut slot = self.screencast.lock().await;
        if let Some(broker) = slot.upgrade() {
            let (frames, url) = broker.subscribe().await;
            return Ok((broker, frames, url));
        }
        // First viewer pays for starting the capture; the rest share it.
        let broker = ScreencastBroker::start(self.page.clone()).await?;
        *slot = Arc::downgrade(&broker);
        let (frames, url) = broker.subscribe().await;
        Ok((broker, frames, url))
    }

    /// Marks this session as in use, now.
    fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    /// How long this session has been left alone.
    fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .elapsed()
    }

    /// Whether anybody is watching this session's screencast.
    ///
    /// The `Weak` upgrading is the whole answer: viewers hold the only strong
    /// references, so a broker that still upgrades has at least one viewer.
    async fn is_watched(&self) -> bool {
        self.screencast.lock().await.upgrade().is_some()
    }

    /// Ends this browser and everything it was holding.
    ///
    /// Asks Chrome to close first and kills only if that does not take: a
    /// browser given the chance to exit flushes its profile and reaps its own
    /// children, where a kill leaves both to the operating system. Bounded,
    /// because a wedged CDP socket must not stall the reaper behind it.
    ///
    /// Deliberately infallible. Every caller is on a path where the work is
    /// already done -- a plan has settled, or a session has gone cold -- and
    /// there is nothing useful to do with a failure to close something that is
    /// being thrown away.
    async fn shut_down(&mut self) {
        // Stop reading before stopping the browser, so neither task wakes up to
        // find its socket gone and logs about it.
        self.console_task.abort();

        let closed = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.browser.close())
            .await
            .is_ok();
        if !closed {
            let _ = self.browser.kill().await;
        }
        // Either way, collect the child rather than leaving a zombie.
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.browser.wait()).await;

        self.handler.abort();

        // The profile is this session's alone -- named from the plan id -- so
        // it goes with it. This is the reclaim that keeps /tmp from filling.
        let _ = std::fs::remove_dir_all(&self.profile);
        // And the shim that put it in a namespace, for the same reason.
        if let Some(wrapper) = &self.namespace_wrapper {
            let _ = std::fs::remove_file(wrapper);
        }
    }
}

/// How a plan's Chrome is placed inside that plan's network namespace: given a
/// plan id, the argv prefix to launch under, or empty for no namespace.
type EnterNamespace = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// How a plan's Chrome is placed inside that plan's network namespace.
///
/// A hook rather than a direct call because `kingdom-browser` must not know
/// what a network namespace is: it is the crate that drives a browser, and
/// `kingdom-app` is the crate that owns namespaces. `kingdom-app` installs this
/// at startup and this crate simply asks.
///
/// **Why Chrome must be in the namespace at all.** A plan with its own network
/// runs its dev server on *its* `127.0.0.1:3000`. A Chrome on the host asked to
/// navigate there would reach the host's `:3000` -- which belongs to the King,
/// or to another plan entirely -- and screenshot the wrong project while
/// reporting success. That is a silent wrong answer, which is worse than a
/// failure, so the browser goes where the server is.
static ENTER_NAMESPACE: OnceLock<EnterNamespace> = OnceLock::new();

/// Teaches this crate how to enter a plan's network namespace.
///
/// Called once, by `kingdom-app`, before any browser is launched. A process
/// that never calls it gets ordinary host-network browsers, which is exactly
/// what a plan on the shared network should have.
pub fn on_enter_namespace(hook: impl Fn(&str) -> Vec<String> + Send + Sync + 'static) {
    let _ = ENTER_NAMESPACE.set(Box::new(hook));
}

/// How a plan reserves a fixed CDP port inside its own namespace, and puts a
/// relay in place that lets the host reach it.
///
/// A companion to [`ENTER_NAMESPACE`] rather than folded into it, because the
/// two questions are asked at different times: the namespace prefix is needed
/// to launch Chrome at all, where the port has to be **known before launch**,
/// since it is handed to Chrome as `--remote-debugging-port` and cannot be
/// read back afterwards the way a kernel-chosen one can. `None` from the hook,
/// or no hook installed, means an ordinary kernel-chosen port -- the ordinary
/// path is unaffected.
type ReserveCdpPort = Box<
    dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<u16>> + Send>>
        + Send
        + Sync,
>;

static RESERVE_CDP_PORT: OnceLock<ReserveCdpPort> = OnceLock::new();

/// Teaches this crate how to reserve a plan's CDP port. Called once, beside
/// [`on_enter_namespace`], by `kingdom-app`.
pub fn on_reserve_cdp_port<F, Fut>(hook: F)
where
    F: Fn(&str) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Option<u16>> + Send + 'static,
{
    let _ = RESERVE_CDP_PORT.set(Box::new(move |plan| Box::pin(hook(plan))));
}

async fn reserved_cdp_port(plan: &str) -> Option<u16> {
    match RESERVE_CDP_PORT.get() {
        Some(hook) => hook(plan).await,
        None => None,
    }
}

/// The argv prefix that puts this plan's Chrome in its own namespace, if any.
fn enter_prefix(plan: &str) -> Vec<String> {
    ENTER_NAMESPACE
        .get()
        .map(|hook| hook(plan))
        .unwrap_or_default()
}

impl BrowserSession {
    async fn launch(plan: &str) -> Result<Self, BrowserError> {
        // Profiles persist between Tool calls because the Chrome process
        // persists. Reusing one after a server crash leaves Chromium's
        // SingletonLock behind and turns every future launch into a false
        // "Chrome missing".
        let profile = profile_dir(plan);
        let _ = std::fs::remove_dir_all(&profile);
        let (width, height) = configured_viewport();

        // A fixed CDP port, only for a plan with a namespace of its own. See
        // `on_reserve_cdp_port` for why this has to be known *before* launch:
        // handed to Chrome as `--remote-debugging-port` via `BrowserConfig`'s
        // own `port()`, which leaves chromiumoxide's ordinary `launch()` --
        // and its 127.0.0.1 rewrite on the `connect()` path only -- entirely
        // unchanged. `None` here, the ordinary case, keeps `port: 0` and
        // today's ephemeral-port behaviour exactly.
        let cdp_port = reserved_cdp_port(plan).await;

        let mut builder = BrowserConfig::builder()
            .new_headless_mode()
            .no_sandbox()
            // chromiumoxide's `arg()` already prepends "--" to the key it is
            // given, so passing "--disable-gpu" here produced the literal flag
            // "----disable-gpu" -- unknown to Chrome, and silently ignored.
            // Spelled correctly it turns off *hardware* acceleration, which a
            // headless browser on a server has no use for.
            //
            // It does NOT stop the software GPU. Chrome falls back to ANGLE's
            // SwiftShader and rasterises WebGL on the CPU regardless of this
            // flag -- so on a machine with no usable GPU, which is every
            // machine a headless plan browser runs on, this changes nothing
            // about what WebGL costs. What that costs is decided by how many
            // frames a page asks for; see [`WEBGL_VAR`].
            .arg("disable-gpu")
            // Caps the disk caches. Chromium's own default is a fraction of
            // free space, which on a developer machine is enormous: six
            // navigations to Kingdom's own chamber grew one profile to 167 MB,
            // and the profiles left behind in /tmp reached 672 MB each, almost
            // all of it `Default/Code Cache`. Capped, the same six navigations
            // leave 3 MB. A plan's browser is a scratch browser; it does not
            // need a year of compiled JavaScript.
            .arg("disk-cache-size=52428800")
            .arg("media-cache-size=52428800")
            .viewport(chromiumoxide::handler::viewport::Viewport {
                width,
                height,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                is_landscape: true,
                has_touch: false,
            })
            .user_data_dir(profile.clone());

        if let Some(port) = cdp_port {
            builder = builder.port(port);
        }

        // Withheld only when the King has asked for it to be, which is the
        // reverse of how this once read.
        //
        // Without the software rasteriser there is no WebGL at all on a
        // headless browser -- there is no hardware path to fall back to -- so
        // this flag is the difference between a plan that can look at
        // Kingdom's own map and one that watches a loading card forever. The
        // cost that once justified it belonged to *pace*, and is now held down
        // where it arises, in the engine. See [`WEBGL_VAR`] for the figures.
        if !webgl_wanted() {
            builder = builder.arg("disable-software-rasterizer");
        }

        // Two features want to be "the executable", and they have to nest
        // rather than overwrite each other:
        //
        //   nsenter --> taskset --> chrome
        //
        // The network namespace is entered first, the CPU mask is applied
        // inside it, and Chrome is what finally execs. Setting
        // `chrome_executable` twice would silently keep only the last one --
        // which is exactly what happened when these two were merged, leaving an
        // isolated plan's browser unconfined and free to take every core.
        //
        // `inner` is what a browser would have been launched as with no
        // namespace: the CPU shim when there is one, the real Chrome otherwise.
        let confined = cpu_shim(&profile);
        let namespace_wrapper = match enter_prefix(plan).is_empty() {
            true => None,
            false => {
                let inner = match &confined {
                    Some(shim) => shim.clone(),
                    None => match chrome_executable()? {
                        Some(path) => path,
                        None => chromiumoxide::detection::default_executable(Default::default())
                            .map_err(BrowserError::ChromeUnavailable)?,
                    },
                };
                // A wrapper script because chromiumoxide takes an *executable*,
                // not an argv: there is nowhere to put `nsenter` and its flags
                // except inside something that looks like a browser binary.
                // See [`ENTER_NAMESPACE`] for why the browser must move at all
                // -- in short, a host Chrome told to open `localhost:3000`
                // would silently screenshot another project.
                Some(write_namespace_wrapper(plan, &inner)?)
            }
        };

        // Most specific wins, and each already contains the one below it.
        match (&namespace_wrapper, &confined) {
            (Some(wrapper), _) => builder = builder.chrome_executable(wrapper.clone()),
            (None, Some(shim)) => builder = builder.chrome_executable(shim.clone()),
            (None, None) => {
                if let Some(executable) = chrome_executable()? {
                    builder = builder.chrome_executable(executable);
                }
            }
        }

        let config = builder.build().map_err(BrowserError::ChromeUnavailable)?;
        let (mut browser, mut handler) =
            tokio::time::timeout(INIT_TIMEOUT, Browser::launch(config))
                .await
                .map_err(|_| BrowserError::ChromeUnavailable("launch timed out".to_string()))?
                .map_err(|error| BrowserError::ChromeUnavailable(error.to_string()))?;

        // Claim the profile for this server, so [`sweep_orphans`] can tell a
        // browser that is still owned from one whose owner died. Written only
        // once the launch succeeded: a profile with no browser in it is not a
        // thing anyone needs to reason about.
        claim(&profile);

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

        // Install the in-page helper into every future document, before the
        // page's own scripts. React registers its fiber roots into whatever
        // hook exists at startup, and the longtask observer has to be running
        // before there is any work to observe -- so installing it when
        // profiling begins would be too late for both. Harmless on a page that
        // uses neither.
        //
        // Best effort and bounded: the browser is perfectly usable without it,
        // and a wedged socket here must not hang the launch.
        let helper =
            chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams::new(
                crate::perf::HELPER_SCRIPT.to_string(),
            );
        let _ = tokio::time::timeout(INIT_TIMEOUT, page.execute(helper)).await;

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
            browser,
            handler: handler_task,
            console_task,
            profile,
            namespace_wrapper,
            last_used: Mutex::new(Instant::now()),
            page,
            console,
            screencast: tokio::sync::Mutex::new(std::sync::Weak::new()),
            profiling: Arc::new(Mutex::new(ProfilingState::default())),
        })
    }
}

/// Owns one live Chrome session per plan.
///
/// A process-global browser would leak cookies and page state between plans;
/// per-Tool call browsers lose exactly the continuity these tools exist to
/// inspect. The plan id is therefore the session boundary, just as it is for
/// tmux.
///
/// # A session ends
///
/// It did not, once, and that was the largest cost this crate imposed: an
/// insert-only map meant a plan which took one screenshot in the morning still
/// held nine processes and most of a gigabyte at midnight. There are now three
/// endings, and between them they cover every way a browser stops being
/// wanted:
///
/// - [`Self::close`], when a plan settles -- the browser's work is over
///   because the plan's is;
/// - [`Self::reap_idle`], when nobody has used one for a while and nobody is
///   watching it;
/// - [`sweep_orphans`] at startup, for the browsers a previous server did not
///   live long enough to close.
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
            // Every operation goes through here, so this is the one place that
            // has to remember to wind the clock the reaper reads.
            session.read().await.touch();
            return Ok(session);
        }
        // Keep the write lock through launch. Launch is rare, and allowing two
        // concurrent first Tool calls to race would orphan one Chrome process.
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get(plan).cloned() {
            session.read().await.touch();
            return Ok(session);
        }
        let session = Arc::new(RwLock::new(BrowserSession::launch(plan).await?));
        sessions.insert(plan.to_string(), Arc::clone(&session));
        Ok(session)
    }

    /// Closes a plan's browser, if it has one.
    ///
    /// The counterpart of `kingdom_app::tools::tmux::dismiss`, and called from
    /// the same place for the same reason: when a plan settles, the resources
    /// it was holding are nobody's, and leaving them for the user to notice
    /// would be this product committing the exact mistake it exists to prevent.
    ///
    /// Idempotent and quiet. Closing a plan that never browsed does nothing,
    /// which is what an honest answer to "end this" looks like when there is
    /// nothing to end.
    pub async fn close(&self, plan: &str) {
        // Taken out of the map first, under the write lock, so that a Tool call
        // arriving mid-shutdown launches a fresh browser rather than finding a
        // half-closed one.
        let removed = self.sessions.write().await.remove(plan);
        let Some(session) = removed else {
            return;
        };
        session.write().await.shut_down().await;
    }

    /// Closes every session that has gone cold.
    ///
    /// "Cold" is idle for longer than [`IDLE_VAR`] *and* unwatched: a browser
    /// the user has the spyglass open on is in use by definition, whatever the
    /// clock says, and closing one out from under him would replace a picture
    /// with an error while he was looking at it.
    ///
    /// Returns which plans were closed, for the test and for anyone who wants
    /// to log it.
    pub async fn reap_idle(&self, after: Duration) -> Vec<String> {
        // Decided under a read lock, so a long-running browser call is not
        // blocked behind a survey it is not part of.
        let mut cold = Vec::new();
        for (plan, session) in self.sessions.read().await.iter() {
            let guard = session.read().await;
            if guard.idle_for() >= after && !guard.is_watched().await {
                cold.push(plan.clone());
            }
        }

        // `close` re-checks under the write lock, so a session that was touched
        // between the survey and here is simply closed a minute later instead.
        let mut closed = Vec::new();
        for plan in cold {
            self.close(&plan).await;
            closed.push(plan);
        }
        closed
    }

    /// Starts the reaper, and returns the handle that stops it.
    ///
    /// Spawned rather than driven by the caller because there is no loop in
    /// this process that a browser could hang its housekeeping off: Tool calls
    /// arrive and finish, and the thing being cleaned up is precisely what is
    /// left when they stop arriving.
    ///
    /// Takes `&'static self` because that is how the manager is actually held
    /// -- one `OnceLock` for the life of the server, shared by the tools and
    /// the screencast. Asking for an `Arc` instead would mean wrapping a value
    /// that already outlives everything, purely to satisfy a spawn.
    ///
    /// `None` when [`IDLE_VAR`] disables reaping, so "no reaper is running" is
    /// visible in the type rather than being a task that wakes to do nothing.
    pub fn start_reaper(&'static self) -> Option<JoinHandle<()>> {
        let after = idle_timeout()?;
        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAP_INTERVAL).await;
                self.reap_idle(after).await;
            }
        }))
    }

    /// How many browsers are alive. For diagnostics and tests.
    pub async fn live(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Attaches a viewer to a plan's browser, if it already has one.
    ///
    /// **Never launches Chrome.** `Ok(None)` means this plan has no browser and
    /// the caller should say so. That restraint is the point: looking must not
    /// be an act that spawns a process. A screencast that started a browser by
    /// being opened would manufacture exactly the invisible resource this
    /// product exists to make visible -- and the user would be watching a blank
    /// page his model never asked for.
    ///
    /// The returned `Arc` is what keeps the screencast alive; dropping it
    /// detaches this viewer, and dropping the last one stops the capture.
    pub async fn watch(
        &self,
        plan: &str,
    ) -> Result<
        Option<(
            Arc<ScreencastBroker>,
            tokio::sync::broadcast::Receiver<ScreencastEvent>,
            Option<String>,
        )>,
        BrowserError,
    > {
        let Some(session) = self.sessions.read().await.get(plan).cloned() else {
            return Ok(None);
        };
        let guard = session.read().await;
        guard.watch().await.map(Some)
    }

    /// Runs one profiling operation against a plan's browser.
    ///
    /// The whole profiling surface goes through one method taking a closure,
    /// rather than a dozen forwarding methods, because every one of them wants
    /// the same two things -- the page, and the session's profiling state -- and
    /// writing that pairing out a dozen times is a dozen chances to take the
    /// locks in a different order.
    pub async fn profiling<T, F>(&self, plan: &str, work: F) -> Result<T, BrowserError>
    where
        F: for<'a> FnOnce(
            &'a Page,
            Arc<Mutex<ProfilingState>>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, BrowserError>> + Send + 'a>,
        >,
    {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let state = Arc::clone(&guard.profiling);
        work(&guard.page, state).await
    }

    /// Opens a measurement window, runs `steps`, and reads what happened.
    ///
    /// The bracket is here rather than in the caller so that the window cannot
    /// be left open: whatever `steps` does, the read happens.
    pub async fn measure<F>(&self, plan: &str, steps: F) -> Result<PerfReading, BrowserError>
    where
        F: for<'a> FnOnce(
            &'a Page,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), BrowserError>> + Send + 'a>,
        >,
    {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        crate::profile::perf_reset(&guard.page).await?;
        steps(&guard.page).await?;
        Ok(crate::profile::perf_read(&guard.page).await)
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
        // chromiumoxide dispatches mousePressed/mouseReleased through CDP. JS
        // `click()` is intentionally not used: it bypasses the trusted input
        // path frameworks and browser defaults listen to.
        //
        // The pointer is moved onto the element and left there for a beat
        // before the press -- see [`HOVER_SETTLE`]. `Element::click` would move
        // and press in one batch, which a page that decides what a click means
        // from what is hovered cannot answer in time. That is also why the
        // point is resolved here and clicked through the page: it is the only
        // way to put a wait between the two.
        let point = timed(timeout, guard.page.find_element(selector))
            .await?
            .scroll_into_view()
            .await?
            .clickable_point()
            .await?;
        timed(timeout, guard.page.move_mouse(point)).await?;
        tokio::time::sleep(HOVER_SETTLE).await;
        timed(timeout, guard.page.click(point)).await?;
        Ok(())
    }

    /// Clicks a point in the viewport, for what no selector can name.
    ///
    /// The map is the case this exists for: it is one `<canvas>`, so every town
    /// and every holding on it is the same element and a selector cannot
    /// distinguish them. Coordinates are the only handle a caller has.
    ///
    /// Same settle as [`Self::click`], and for the map the same *reason* --
    /// what a click there means is decided by what the engine has drawn under
    /// the pointer.
    pub async fn click_at(
        &self,
        plan: &str,
        x: f64,
        y: f64,
        timeout: Duration,
    ) -> Result<(), BrowserError> {
        let session = self.session(plan).await?;
        let guard = session.read().await;
        let point = chromiumoxide::layout::Point { x, y };
        timed(timeout, guard.page.move_mouse(point)).await?;
        tokio::time::sleep(HOVER_SETTLE).await;
        timed(timeout, guard.page.click(point)).await?;
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

/// Where the local browser caches keep a Chromium, relative to `$HOME`.
///
/// These are the ones a developer machine tends to already have because some
/// other tool downloaded them. Searched only after the ordinary places, so a
/// real system Chrome always wins.
const CACHED_BROWSERS: &[&str] = &[
    ".cache/ms-playwright",
    ".cache/puppeteer",
    ".local/share/ms-playwright",
];

/// Confines a browser to a few CPUs, by launching it behind `taskset`.
///
/// Returns the shim to run instead of Chrome, or `None` to launch Chrome
/// directly -- which is the answer whenever confinement was not asked for, is
/// not possible, or would need a tool this machine does not have.
///
/// # Why a shim script rather than an affinity call
///
/// Because the affinity has to be in force *before* Chrome forks, and Chrome
/// forks its zygote almost immediately. Setting it on the browser process after
/// launch would race the very children that do the rendering. A wrapper that
/// `exec`s under `taskset` cannot lose that race: the mask is set before Chrome
/// exists at all, and Linux passes it down through every fork -- verified, with
/// the renderer and GPU children all reporting the confined mask.
///
/// # Why it may return `None` without complaint
///
/// Every failure here is a reason to run an *unconfined* browser rather than no
/// browser. A missing `taskset`, an unwritable profile, a Chrome that could not
/// be located: none of them is worth denying a plan its tools over, and the
/// setting is a precaution rather than a guarantee anyone is relying on.
#[cfg(unix)]
fn cpu_shim(profile: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let cpus = configured_cpus()?;
    // `taskset` is what actually applies the mask, and it is not on every
    // machine. Checked before anything is written, because a shim that cannot
    // run would turn every launch into a "Chrome missing" -- and this is now
    // the default path, so that failure would be everyone's.
    let taskset = which_taskset()?;
    // The real browser has to be named in the script, so the path is resolved
    // here rather than left to chromiumoxide to find later.
    let chrome = match chrome_executable() {
        Ok(Some(path)) => path,
        Ok(None) => chromiumoxide::detection::default_executable(Default::default()).ok()?,
        Err(_) => return None,
    };

    // Clamped to what this process may actually use, so a Kingdom already
    // confined hands out a subset of its own CPUs rather than naming ones it
    // was denied. `taskset -c` takes an inclusive range, hence the minus one.
    let available = std::thread::available_parallelism().map_or(1, |count| count.get());
    let last = cpus.min(available).saturating_sub(1);

    std::fs::create_dir_all(profile).ok()?;
    let shim = profile.join("chrome-confined.sh");
    let script = format!(
        "#!/bin/sh\nexec {} -c 0-{last} {} \"$@\"\n",
        taskset.display(),
        chrome.display()
    );
    std::fs::write(&shim, script).ok()?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).ok()?;
    Some(shim)
}

/// Where `taskset` is, if this machine has one.
///
/// Looked up rather than assumed at `/usr/bin/taskset`: it is `util-linux` on
/// most distributions but not all, and the shim names it absolutely so that
/// Chrome's own environment cannot change what runs.
#[cfg(unix)]
fn which_taskset() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("taskset"))
        .find(|candidate| candidate.is_file())
}

/// Confinement is a Unix affair; elsewhere a browser runs as it always did.
#[cfg(not(unix))]
fn cpu_shim(_profile: &Path) -> Option<PathBuf> {
    None
}

/// Which Chrome to launch, or `None` to let chromiumoxide decide.
///
/// Three steps, most explicit first:
///
/// 1. `KINGDOM_CHROME_EXECUTABLE`, if set. The override for a machine where the
///    guessing is wrong; pointing it at something that is not a file is a
///    mistake worth reporting rather than silently falling through.
/// 2. Nothing -- chromiumoxide's own detection, which covers `$CHROME`, the
///    usual binary names on `PATH`, and the standard install locations. That is
///    the common case and needs no help from us.
/// 3. Only if that finds nothing: the caches below, because a machine with no
///    system Chrome very often still has one that Playwright or Puppeteer
///    downloaded.
///
/// Step 3 is why this function exists. Without it the browser tools are dead on
/// a machine that demonstrably *can* run a browser, and the user is told to go
/// and install something he already has.
fn chrome_executable() -> Result<Option<PathBuf>, BrowserError> {
    if let Ok(executable) = std::env::var("KINGDOM_CHROME_EXECUTABLE") {
        let path = PathBuf::from(&executable);
        if !path.is_file() {
            return Err(BrowserError::ChromeUnavailable(format!(
                "KINGDOM_CHROME_EXECUTABLE points to {}, which is not a file",
                path.display()
            )));
        }
        return Ok(Some(path));
    }

    // Let chromiumoxide look first: PATH and the standard installs are where a
    // browser usually is, and its detection is already thorough there.
    if chromiumoxide::detection::default_executable(Default::default()).is_ok() {
        return Ok(None);
    }

    Ok(cached_chrome())
}

/// Writes the shim that enters a plan's network namespace.
///
/// chromiumoxide is handed an executable path and builds the argument list
/// itself, so `nsenter` cannot be prepended to the command -- there is no
/// command to prepend it to yet. A tiny `exec` wrapper is the one place the
/// prefix fits, and `exec` matters: it replaces the shell rather than forking,
/// so the pid chromiumoxide waits on is Chrome's own and killing the session
/// still kills the browser.
///
/// `inner` is **not necessarily Chrome**. It is whatever would have been
/// launched without a namespace, which is [`cpu_shim`] when confinement is on --
/// so the two nest as `nsenter -> taskset -> chrome` and neither feature is
/// lost. See the call site.
///
/// `"$@"` quoted, because Chrome's flags contain paths that can contain spaces.
fn write_namespace_wrapper(plan: &str, inner: &Path) -> Result<PathBuf, BrowserError> {
    use std::io::Write as _;

    let prefix = enter_prefix(plan)
        .into_iter()
        .map(|part| shell_quote(&part))
        .collect::<Vec<_>>()
        .join(" ");

    let mut hasher = DefaultHasher::new();
    plan.hash(&mut hasher);
    let path = std::env::temp_dir().join(format!("kingdom-chrome-ns-{:016x}.sh", hasher.finish()));

    let script = format!(
        "#!/bin/sh\nexec {prefix} {} \"$@\"\n",
        shell_quote(&inner.to_string_lossy())
    );

    let mut file = std::fs::File::create(&path).map_err(|e| {
        BrowserError::ChromeUnavailable(format!("the namespace wrapper could not be written: {e}"))
    })?;
    file.write_all(script.as_bytes()).map_err(|e| {
        BrowserError::ChromeUnavailable(format!("the namespace wrapper could not be written: {e}"))
    })?;
    drop(file);

    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| {
        BrowserError::ChromeUnavailable(format!("the namespace wrapper is not executable: {e}"))
    })?;

    Ok(path)
}

/// Single-quotes a string for `/bin/sh`.
///
/// The one escape that matters: a `'` inside is closed, escaped and reopened.
/// Paths here come from `PATH` lookups and a plan id, so this is belt and
/// braces rather than a live threat -- but a browser that fails to launch
/// because someone's home directory has an apostrophe in it is a miserable bug
/// to track down.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Hunts for a Chromium that some other tool downloaded.
///
/// Deliberately shallow and ordered: each cache holds one directory per
/// installed version with a known layout inside. Walking the whole tree would
/// be slower and would risk turning up something that is not a browser.
///
/// Newest first, by sorting the version directories in reverse -- these caches
/// accumulate versions, and the most recent is the likeliest to work.
fn cached_chrome() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);

    for cache in CACHED_BROWSERS {
        let Ok(entries) = std::fs::read_dir(home.join(cache)) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        versions.sort();
        versions.reverse();

        for version in versions {
            // Playwright's layouts, then Puppeteer's. `headless_shell` is a real
            // Chromium with no UI, which is exactly what a headless session
            // wants.
            for relative in [
                "chrome-linux/chrome",
                "chrome-linux/headless_shell",
                "chrome-linux64/chrome",
                "chrome-headless-shell-linux64/chrome-headless-shell",
                "chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            ] {
                let candidate = version.join(relative);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

/// Where a plan's browser keeps its profile.
///
/// Derived from the plan id rather than handed out and remembered, on the same
/// reasoning as tmux's `socket_for`: the answer has to survive a restart of
/// this process, so that a directory left behind yesterday is recognisable
/// today. [`sweep_orphans`] depends on exactly that.
///
/// **Public because a sealed plan has to mount it.** The profile holds the
/// `chrome-confined.sh` shim, which is written out here on the host and then
/// executed *inside* the namespace by the `nsenter` wrapper. A sealed plan's
/// `/tmp` is a private tmpfs, so unless `kingdom-app` binds this exact
/// directory through, the shim is written to one filesystem and looked for on
/// another -- and every `browser_*` tool fails with `nsenter: failed to
/// execute ...: No such file or directory`. Measured from inside a sealed
/// plan. Exported rather than re-derived there because the hash has to agree
/// between the two crates, and two copies of a hash drift.
pub fn profile_dir(plan: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    plan.hash(&mut hasher);
    Path::new("/tmp").join(format!("{PROFILE_PREFIX}{:016x}", hasher.finish()))
}

/// The prefix every Kingdom profile directory carries.
///
/// Named because two things depend on it agreeing: [`profile_dir`] writes it
/// and [`sweep_orphans`] matches on it. A sweep that disagreed with the naming
/// would either miss every orphan or, far worse, delete something that was
/// never ours.
const PROFILE_PREFIX: &str = "kingdom-chrome-";

/// The file inside a profile that says which server owns it.
const OWNER_FILE: &str = "kingdom-owner";

/// Records this server as the owner of a profile directory.
///
/// Best effort. A profile with no claim reads as unowned, which makes it
/// sweepable -- the safe direction: the cost of sweeping a live browser is
/// bounded by the claim being written immediately after launch, while the cost
/// of *never* sweeping is the eleven gigabytes this was written to reclaim.
fn claim(profile: &Path) {
    let _ = std::fs::write(profile.join(OWNER_FILE), std::process::id().to_string());
}

/// Whether a process is alive, without disturbing it.
///
/// `/proc` rather than `kill(pid, 0)`: no unsafe, no signal, and no dependency
/// on the process being ours to signal.
fn is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).is_dir()
}

/// What the sweep may do with one profile directory.
///
/// Three outcomes rather than a boolean, because the honest answer differs in a
/// way that a boolean hid -- and hid dangerously. Discovered by running the
/// sweep against a real machine: an unclaimed profile is *not* the same thing
/// as an abandoned one. A server built before profiles were claimed writes
/// none, and its browsers are alive and in use. Collapsing those two cases
/// killed forty-six live Chrome processes belonging to a running Kingdom.
#[derive(Debug, PartialEq, Eq)]
enum Fate {
    /// Not ours, or owned by a server that is still running. Untouched.
    Keep,
    /// Ours, and its owning server is gone. Whatever still holds it is an
    /// orphan: kill it, then delete the directory.
    Abandoned,
    /// Ours, but with no claim to say whose. Delete it only if nothing is
    /// holding it, and never kill anything -- the holder may be a live browser
    /// belonging to a server from before claims existed.
    Unclaimed,
}

/// Decides what may be done with one directory.
///
/// The whole safety argument of the sweep lives here, which is why it is a
/// named function with tests rather than an `if` inside a loop:
///
/// - the name must be ours, so nothing outside Kingdom is ever a candidate;
/// - a claim naming a *live* pid means a running server is using it, so it is
///   left alone. That is what lets two Kingdoms share a machine, which a sweep
///   matching on Chrome's command line could not do;
/// - a claim naming a *dead* pid is the case this was built for: that server
///   died without closing its browsers, and they are still running;
/// - no claim at all is the ambiguous case, and it is resolved conservatively
///   by [`Fate::Unclaimed`] rather than by guessing.
fn fate_of(dir: &Path) -> Fate {
    let is_ours = dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(PROFILE_PREFIX));
    if !is_ours || !dir.is_dir() {
        return Fate::Keep;
    }

    match std::fs::read_to_string(dir.join(OWNER_FILE)) {
        Ok(owner) => match owner.trim().parse::<u32>() {
            Ok(pid) if is_alive(pid) => Fate::Keep,
            Ok(_) => Fate::Abandoned,
            // A claim we cannot read tells us nothing, so it is treated as no
            // claim rather than as permission.
            Err(_) => Fate::Unclaimed,
        },
        Err(_) => Fate::Unclaimed,
    }
}

/// Kills the browsers a previous server did not live long enough to close, and
/// deletes what they left behind.
///
/// # Why this is necessary at all
///
/// chromiumoxide sets `kill_on_drop`, which only fires on a *graceful* drop. A
/// SIGKILLed server -- or a `cargo leptos watch` restart, which happens on
/// every save -- leaves the whole Chrome tree running: measured, ten processes
/// survived the death of the process that spawned them. That is how one machine
/// accumulated forty-two profile directories totalling thirteen gigabytes.
///
/// # Why it is safe
///
/// Every decision goes through [`fate_of`], which never kills anything whose
/// owning server is alive, and never kills anything whose owner is *unknown*.
/// The worst it can do to a browser it should not have touched is nothing.
///
/// Runs at startup, from the server binary. Returns how many profiles were
/// reclaimed, so the caller can say so in its banner.
pub fn sweep_orphans() -> usize {
    sweep_in(Path::new("/tmp"))
}

/// The sweep, against a given directory.
///
/// Split from [`sweep_orphans`] so it can be tested against a directory the
/// test made, rather than against the real `/tmp` -- where the things it deletes
/// are the developer's own running browsers.
fn sweep_in(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };

    let mut reclaimed = 0;
    for entry in entries.filter_map(Result::ok) {
        let dir = entry.path();
        match fate_of(&dir) {
            Fate::Keep => continue,
            Fate::Abandoned => {
                kill_holders_of(&dir);
            }
            Fate::Unclaimed => {
                // Somebody is using it, and we cannot prove it is not a live
                // browser from an older build. Leave both alone.
                if is_held(&dir) {
                    continue;
                }
            }
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            reclaimed += 1;
        }
    }
    reclaimed
}

/// Every live process holding this profile as its `--user-data-dir`.
///
/// # Why this is not a simple argument comparison
///
/// It was, and it silently never matched anything. Chrome **rewrites its own
/// `argv` into one contiguous string** to set its process title, so by the time
/// a Chrome process appears in `/proc` its `cmdline` is a single field of
/// thirteen hundred bytes rather than the sixty NUL-separated arguments it was
/// launched with. Comparing whole arguments therefore found nothing, the
/// "is anything using this?" guard never fired, and the sweep deleted profiles
/// out from under running browsers.
///
/// So the search is a substring search, bounded at the end. The boundary is not
/// optional: `/tmp/kingdom-chrome-abc` is a prefix of
/// `/tmp/kingdom-chrome-abcd`, and without it a sweep of the first would
/// consider the second's browser to be holding it.
fn holders_of(profile: &Path) -> Vec<u32> {
    let Some(profile) = profile.to_str() else {
        return Vec::new();
    };
    let wanted = format!("--user-data-dir={profile}");

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            mentions(&cmdline, wanted.as_bytes()).then_some(pid)
        })
        .collect()
}

/// Whether a command line names exactly this argument.
///
/// Separated out because it is the whole of the correctness of the sweep's
/// guard, and because it can be tested against the two shapes a command line
/// really takes -- NUL-separated as most programs leave it, and one flat string
/// as Chrome rewrites it.
///
/// A match must be followed by a separator or the end of the buffer, so that a
/// path is never mistaken for one that merely begins with it.
fn mentions(cmdline: &[u8], argument: &[u8]) -> bool {
    cmdline
        .windows(argument.len())
        .enumerate()
        .filter(|(_, window)| *window == argument)
        .any(|(at, _)| {
            matches!(
                cmdline.get(at + argument.len()),
                // The end of the command line, or the end of this argument.
                None | Some(0) | Some(b' ')
            )
        })
}

/// Whether anything at all is using this profile.
fn is_held(profile: &Path) -> bool {
    !holders_of(profile).is_empty()
}

/// Kills whatever is still holding an abandoned profile.
///
/// Deleting the directory alone would leave the orphaned browser running with
/// its files pulled out from under it -- still holding memory, still burning
/// CPU, and now unable to say why.
fn kill_holders_of(profile: &Path) {
    for pid in holders_of(profile) {
        // SIGKILL rather than SIGTERM: this process has already lost the server
        // that could have shut it down politely, and a browser that ignored a
        // TERM would keep the resources this is reclaiming.
        //
        // Safe: `kill` takes a pid and a signal and touches no memory. The pid
        // came from /proc and may have exited since, which is reported as ESRCH
        // and deliberately ignored -- there is nothing to do about a process
        // that died on its own before we asked it to.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace wrapper wraps whatever it is given, not Chrome directly.
    ///
    /// This is the merge hazard between CPU confinement and network isolation,
    /// pinned. Both features want to be the executable chromiumoxide launches,
    /// and `chrome_executable` keeps only the last one set — so composing them
    /// is the difference between a confined browser and one silently free to
    /// take every core. It broke exactly this way once, and nothing about it
    /// fails to compile.
    ///
    /// The wrapper's *content* is what is checked, because that is what
    /// survives: whatever path it is handed is what it `exec`s.
    #[test]
    fn the_namespace_wrapper_execs_whatever_it_was_given() {
        // A stand-in for `cpu_shim`'s output. The test does not need a
        // namespace to exist -- with no hook installed the prefix is empty,
        // which is the shared-network case, so the script is asserted on
        // directly rather than through a launch.
        let confined = Path::new("/tmp/kingdom-chrome-abc/chrome-confined.sh");

        let wrapper = write_namespace_wrapper("plan-nesting", confined)
            .expect("the wrapper should be writable");
        let script = std::fs::read_to_string(&wrapper).expect("the wrapper should be readable");

        assert!(
            script.contains("chrome-confined.sh"),
            "the wrapper dropped the CPU shim it was handed:\n{script}"
        );
        // `exec`, so the pid chromiumoxide waits on is the browser's own and
        // killing the session still kills Chrome.
        assert!(script.starts_with("#!/bin/sh\nexec "), "{script}");
        // Chrome's own flags are forwarded, quoted, because they carry paths.
        assert!(script.trim_end().ends_with(r#""$@""#), "{script}");

        let _ = std::fs::remove_file(&wrapper);
    }

    /// A path with an apostrophe in it does not break out of its quoting.
    ///
    /// Belt and braces rather than a live threat — these come from `PATH`
    /// lookups — but a home directory with an apostrophe is a real thing and
    /// the resulting launch failure would be baffling.
    #[test]
    fn a_quoted_path_survives_an_apostrophe() {
        assert_eq!(
            shell_quote("/home/o'brien/chrome"),
            r"'/home/o'\''brien/chrome'"
        );
    }

    /// The default viewport must clear Kingdom's own responsive thresholds.
    ///
    /// This is the whole reason the number changed: at 1024 wide the cities
    /// rail folds (`RAIL_FOLDS_BELOW` is 1250) and a plan checking its work in
    /// the chamber is shown a narrowed interface it did not ask for. Pinned
    /// here so that lowering the default is a deliberate act with a test to
    /// argue with, rather than a tidy-up.
    #[test]
    fn the_default_viewport_is_wider_than_anything_kingdom_folds_at() {
        assert!(
            DEFAULT_VIEWPORT.0 >= 1250,
            "the default viewport {}px folds the cities rail",
            DEFAULT_VIEWPORT.0
        );
    }

    /// The override is read, in both spellings of the separator.
    #[test]
    fn a_viewport_can_be_asked_for() {
        assert_eq!(parse_viewport("800x600"), Some((800, 600)));
        assert_eq!(parse_viewport(" 1920 X 1080 "), Some((1920, 1080)));
    }

    /// A size that will not parse yields nothing, so the caller can fall back.
    ///
    /// Zero is refused along with the nonsense: a viewport with no area is a
    /// browser that can render nothing, which is a worse outcome than ignoring
    /// the setting.
    #[test]
    fn a_viewport_that_makes_no_sense_is_not_offered() {
        for bad in ["", "wide", "1440", "1440x", "0x900", "1440x0", "-1x-1"] {
            assert_eq!(parse_viewport(bad), None, "{bad} was accepted");
        }
    }

    /// Detection must only ever name a file that exists.
    ///
    /// The caches below are searched by guessing at known layouts, and a guess
    /// that returns a plausible-looking path to nothing would be handed to
    /// Chrome and fail at launch -- reported as "Chrome is not available" on a
    /// machine where it demonstrably is. Machine-independent: it asserts a
    /// property of whatever is found, not that anything is.
    #[test]
    fn detection_never_names_a_path_that_is_not_there() {
        if let Some(found) = cached_chrome() {
            assert!(
                found.is_file(),
                "detection offered {}, which is not a file",
                found.display()
            );
        }
    }

    /// A viewer must never be what launches a browser.
    ///
    /// The panel is opened by curiosity -- the user wondering what a plan is
    /// doing -- and if looking spawned a Chrome, then the act of looking would
    /// itself manufacture the kind of invisible resource this product exists to
    /// surface. `Ok(None)` is the honest answer for a plan that has never
    /// browsed, and the socket turns it into "no browser here".
    #[tokio::test]
    async fn watching_a_plan_that_never_browsed_launches_nothing() {
        let manager = BrowserSessionManager::new();

        let attached = manager.watch("never-browsed").await;

        assert!(
            matches!(attached, Ok(None)),
            "attaching a viewer must not create a session"
        );
    }

    /// Closing a plan that never browsed is a no-op, not a failure.
    ///
    /// `finish_plan` calls this for *every* settled plan, and most plans never
    /// open a browser at all. If closing nothing were an error -- or worse, if
    /// it launched a browser in order to close it -- then merging an ordinary
    /// plan would start a Chrome.
    #[tokio::test]
    async fn closing_a_plan_that_never_browsed_does_nothing() {
        let manager = BrowserSessionManager::new();

        manager.close("never-browsed").await;

        assert_eq!(manager.live().await, 0, "closing must not create a session");
    }

    /// The reaper must not close what it cannot see, and must not invent work.
    ///
    /// With no sessions there is nothing cold, whatever the window. Pinned
    /// because the reaper runs on a timer forever: a version of it that did
    /// something on an empty map would do that thing every minute for the life
    /// of the server.
    #[tokio::test]
    async fn reaping_an_empty_manager_closes_nothing() {
        let manager = BrowserSessionManager::new();

        let closed = manager.reap_idle(Duration::from_secs(0)).await;

        assert!(closed.is_empty(), "nothing was open, so nothing may close");
    }

    /// An idle window is read in the spellings a person would write.
    #[test]
    fn an_idle_window_can_be_asked_for() {
        assert_eq!(parse_idle("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_idle("45s"), Some(Duration::from_secs(45)));
        assert_eq!(parse_idle(" 15m "), Some(Duration::from_secs(900)));
        assert_eq!(parse_idle("2h"), Some(Duration::from_secs(7200)));
    }

    /// A window that makes no sense yields nothing, so the caller can fall back.
    ///
    /// The same stance as the viewport: the cost of misreading this setting is
    /// browsers that live forever or die instantly, and both are worse than
    /// ignoring a typo.
    #[test]
    fn an_idle_window_that_makes_no_sense_is_not_offered() {
        for bad in ["", "soon", "m", "-5", "5 minutes"] {
            assert_eq!(parse_idle(bad), None, "{bad} was accepted");
        }
    }

    /// WebGL goes away only when somebody says so, in so many words.
    ///
    /// The direction of this test is the change: WebGL is now on by default,
    /// so what must be deliberate is *refusing* it. The negative cases are the
    /// point, as they were before -- a misspelt value must leave the browser
    /// able to render rather than silently blind, which is the failure mode
    /// that made the map untestable in the first place.
    #[test]
    fn webgl_is_only_ever_switched_off_deliberately() {
        for no in ["off", "0", "false", "NO", " Off "] {
            assert!(reads_as_no(no), "{no} should have been read as no");
        }
        for yes in ["", "on", "1", "true", "yes", "offf", "no thanks"] {
            assert!(!reads_as_no(yes), "{yes} should not have been read as no");
        }
    }

    /// A CPU ceiling is read, and a mistyped one does not lift the limit.
    ///
    /// The asymmetry is the point. An explicit `0` or `off` means the King has
    /// decided to let a browser have the machine, and is obeyed. Nonsense means
    /// he did not decide anything, and falls back to the default rather than to
    /// no limit -- because this ceiling is what keeps a software-rendered page
    /// from taking seven cores, and a typo should not silently surrender it.
    #[test]
    fn a_mistyped_cpu_ceiling_falls_back_rather_than_lifting_the_limit() {
        assert_eq!(parse_cpus("3"), Some(3));
        assert_eq!(parse_cpus(" 12 "), Some(12));
        assert_eq!(parse_cpus("1"), Some(1));

        // Deliberate refusals, which are obeyed.
        for lifted in ["0", "off", "no", "false", " OFF "] {
            assert_eq!(parse_cpus(lifted), None, "{lifted} should lift the limit");
        }

        // Nonsense, which must not.
        for nonsense in ["", "-2", "half", "3.5", "3 cpus", "lots"] {
            assert_eq!(
                parse_cpus(nonsense),
                Some(DEFAULT_BROWSER_CPUS),
                "{nonsense} should have fallen back to the default"
            );
        }
    }

    /// A command line is searched in both the shapes one really takes.
    ///
    /// This is the bug that deleted profiles out from under running browsers,
    /// pinned so it cannot come back. Most programs leave `argv` as the kernel
    /// stored it, NUL-separated. **Chrome does not**: it rewrites its own argv
    /// into one contiguous string to set its process title, so its `/proc`
    /// entry is a single field. A matcher that split on NUL found nothing at
    /// all in a real Chrome, which made the sweep's "is anybody using this?"
    /// guard silently always answer no.
    ///
    /// The prefix case is the other half: paths here differ only in a hash, so
    /// one profile's name is very often a prefix of another's.
    #[test]
    fn a_command_line_is_searched_in_both_shapes_it_really_takes() {
        let wanted = b"--user-data-dir=/tmp/kingdom-chrome-abc";

        // As the kernel stores it for an ordinary program.
        let separated = b"/usr/bin/chromium\0--headless\0--user-data-dir=/tmp/kingdom-chrome-abc\0--no-sandbox\0";
        assert!(mentions(separated, wanted), "NUL-separated argv missed");

        // As Chrome rewrites it for its process title.
        let flattened =
            b"/usr/bin/chromium --headless --user-data-dir=/tmp/kingdom-chrome-abc --no-sandbox";
        assert!(mentions(flattened, wanted), "flattened argv missed");

        // At the very end, with nothing after it.
        let at_the_end = b"/usr/bin/chromium --user-data-dir=/tmp/kingdom-chrome-abc";
        assert!(mentions(at_the_end, wanted), "a trailing argument missed");

        // A different profile that merely begins with this one's name must
        // not count as using it.
        let longer = b"/usr/bin/chromium --user-data-dir=/tmp/kingdom-chrome-abcdef --no-sandbox";
        assert!(
            !mentions(longer, wanted),
            "a longer path was mistaken for this one"
        );

        // And an unrelated browser is not a holder either.
        let other = b"/usr/bin/chromium --user-data-dir=/tmp/kingdom-chrome-zzz";
        assert!(!mentions(other, wanted), "an unrelated profile matched");
    }

    /// The sweep only ever considers directories that are ours.
    ///
    /// This is the guard that keeps [`sweep_orphans`] from deleting somebody
    /// else's data. It matters far more than finding every orphan: missing one
    /// costs disk space, and a false positive costs a stranger's files.
    #[test]
    fn nothing_outside_kingdoms_own_naming_is_ever_touched() {
        let root = test_dir("naming");
        let stranger = root.join("important-user-data");
        std::fs::create_dir_all(&stranger).expect("the test needs a directory");

        assert_eq!(
            fate_of(&stranger),
            Fate::Keep,
            "a directory not named like ours must never be swept"
        );
        assert_eq!(sweep_in(&root), 0, "the sweep must have left it alone");
        assert!(stranger.is_dir(), "the stranger's directory is gone");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A profile claimed by a living server is left alone.
    ///
    /// This is what lets two Kingdom servers share a machine: the sweep at
    /// one's startup must not touch the browsers of the other, which is
    /// running.
    #[test]
    fn a_profile_owned_by_a_living_server_survives() {
        let root = test_dir("living");
        let mine = root.join(format!("{PROFILE_PREFIX}0000000000000001"));
        std::fs::create_dir_all(&mine).expect("the test needs a directory");

        // This very test process is the living owner -- no more convincing
        // "alive" pid exists than the one running the assertion.
        claim(&mine);

        assert_eq!(fate_of(&mine), Fate::Keep);
        assert_eq!(sweep_in(&root), 0);
        assert!(mine.is_dir(), "a live server's profile was deleted");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A profile whose owner is gone is reclaimed.
    ///
    /// The case the sweep exists for: a server killed without closing its
    /// browsers leaves them running and their profiles behind.
    #[test]
    fn a_profile_whose_owner_died_is_reclaimed() {
        let root = test_dir("dead");
        let dead = root.join(format!("{PROFILE_PREFIX}0000000000000002"));
        std::fs::create_dir_all(&dead).expect("the test needs a directory");
        // A pid that cannot be a Kingdom: pid 0 is the scheduler, and never
        // appears in /proc as a process directory.
        std::fs::write(dead.join(OWNER_FILE), "0").expect("the test needs a claim");

        assert_eq!(fate_of(&dead), Fate::Abandoned);
        assert_eq!(sweep_in(&root), 1);
        assert!(!dead.exists(), "the abandoned profile was not reclaimed");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// An unclaimed profile is deleted only when nothing is using it, and its
    /// holder is never killed.
    ///
    /// Found by running the sweep against a real machine rather than by
    /// reasoning about it. A server built before profiles carried a claim
    /// writes none, and its browsers are alive and in use -- forty-six
    /// processes, in the case that caught this. Treating "no claim" as
    /// "abandoned" would have killed every one of them.
    ///
    /// So the ambiguity is resolved by asking the operating system who is
    /// actually using the directory, and doing nothing whenever the answer is
    /// "somebody".
    #[test]
    fn an_unclaimed_profile_in_use_is_left_entirely_alone() {
        let root = test_dir("unclaimed");
        let held = root.join(format!("{PROFILE_PREFIX}0000000000000003"));
        let idle = root.join(format!("{PROFILE_PREFIX}0000000000000004"));
        for dir in [&held, &idle] {
            std::fs::create_dir_all(dir).expect("the test needs directories");
        }

        // A real process, really holding the profile, exactly as a browser
        // from an older build would: the argument is on its command line, and
        // `holders_of` finds it by reading /proc.
        //
        // The loop matters. `sh -c 'sleep 30' ARG` *execs* sleep and replaces
        // its own command line, taking the argument with it -- which the
        // assertion below caught. A shell that stays a shell keeps it.
        let mut holder = std::process::Command::new("sh")
            .arg("-c")
            .arg("while :; do sleep 1; done")
            .arg(format!("--user-data-dir={}", held.display()))
            .spawn()
            .expect("the test needs a process to stand in for a browser");

        // The stand-in is only meaningful if it is really running and really
        // holding the directory -- checked, because a holder that failed to
        // start would make the rest of this test pass for the wrong reason.
        assert_eq!(
            holders_of(&held),
            vec![holder.id()],
            "the stand-in process is not holding the profile"
        );

        assert_eq!(fate_of(&held), Fate::Unclaimed);
        assert_eq!(fate_of(&idle), Fate::Unclaimed);

        // Only the unheld one is reclaimed.
        assert_eq!(sweep_in(&root), 1);
        assert!(held.is_dir(), "a profile in use was deleted");
        assert!(!idle.exists(), "an unused profile was not reclaimed");

        // And the holder is still running: an unclaimed profile never earns
        // anything a signal.
        assert!(
            holder
                .try_wait()
                .expect("the holder must be waitable")
                .is_none(),
            "the sweep killed a process it could not prove was abandoned"
        );

        let _ = holder.kill();
        let _ = holder.wait();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory of this test's own, named so parallel tests cannot collide.
    fn test_dir(what: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "kingdom-sweep-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the test needs its own directory");
        root
    }
}
