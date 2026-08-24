//! Measuring a page: metrics, profiles, traces, and the scenario harness's
//! per-run reading.
//!
//! The CDP half of `browser_profile`. It lives here rather than in the tool for
//! the reason the whole crate exists: `kingdom-app` compiles to wasm as well as
//! native, and a Chrome protocol client must never drift into that build. The
//! tool above this is argument validation and wording.
//!
//! # The sub-machines
//!
//! CPU sampling, tracing and coverage are each start/stop pairs over state that
//! outlives a single call, so the session carries a [`ProfilingState`] and the
//! operations here are gated on it. Starting something already started is a
//! no-op rather than a restart -- a restart would silently discard the profile
//! the caller believed it was collecting. Stopping something never started is
//! an error rather than an empty result, because "you forgot to start" is
//! actionable and an empty profile looks like a finding.

use crate::session::BrowserError;
use chromiumoxide::cdp::browser_protocol::{
    emulation::SetCpuThrottlingRateParams,
    performance::{EnableParams as PerfEnable, GetMetricsParams},
    tracing::{EndParams as TraceEnd, StartParams as TraceStart, TraceConfig},
};
use chromiumoxide::cdp::js_protocol::heap_profiler::{
    CollectGarbageParams, EnableParams as HeapEnable, EventAddHeapSnapshotChunk,
    TakeHeapSnapshotParams,
};
use chromiumoxide::cdp::js_protocol::profiler::{
    DisableParams as ProfilerDisable, EnableParams as ProfilerEnable,
    StartParams as ProfilerStart, StartPreciseCoverageParams, StopParams as ProfilerStop,
    StopPreciseCoverageParams, TakePreciseCoverageParams,
};
use chromiumoxide::Page;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// How long to wait for `Tracing.tracingComplete` after asking it to end.
const TRACE_COMPLETE_TIMEOUT: Duration = Duration::from_secs(30);

/// Which of the start/stop machines are currently running, plus the throttle.
///
/// Per session, because that is what they are per: a CPU profile belongs to a
/// browser, not to a call.
#[derive(Debug, Default)]
pub struct ProfilingState {
    pub cpu_active: bool,
    pub tracing_active: bool,
    pub coverage_active: bool,
    /// `None` is the browser's own speed. A `Some` always holds a rate `>= 1`,
    /// because a slowdown factor below 1 is not a thing and the only setter
    /// checks.
    pub throttle: Option<f64>,
    /// Buffered `Tracing.dataCollected` events, non-empty only while tracing:
    /// cleared when a trace is armed, drained when it ends.
    pub trace_events: Vec<serde_json::Value>,
}

/// What the page reports about one measurement window.
///
/// Read out of the page in a single call by `__perfRead`, rather than assembled
/// from host-side observations. See `perf.rs` for why that distinction is the
/// whole design.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfReading {
    /// Sum of in-window long-task durations, in milliseconds. Not a CDP
    /// `ScriptDuration` delta -- see `perf.rs`.
    pub script_ms: f64,
    /// How many tasks over 50ms blocked the page during the window.
    pub long_tasks: u64,
    /// `performance.now()` span of the window.
    pub wall_ms: Option<f64>,
    pub dom_nodes: u64,
    /// `measured`, `absent`, or `no_profiling_build`.
    ///
    /// Three states rather than a bool because "React is here but this build
    /// cannot report timings" is a different fact from "there is no React", and
    /// a caller told the wrong one goes looking for the wrong problem.
    pub react_status: String,
    /// `None` when React is absent -- never `0`, which would read as a real
    /// measurement of no renders.
    pub react_commits: Option<u64>,
    /// `Some` only when `react_status` is `measured`.
    pub react_actual_ms: Option<f64>,
}

impl PerfReading {
    /// What a page with no helper reports: nothing, said honestly.
    pub fn absent() -> Self {
        Self {
            script_ms: 0.0,
            long_tasks: 0,
            wall_ms: None,
            dom_nodes: 0,
            react_status: "absent".to_string(),
            react_commits: None,
            react_actual_ms: None,
        }
    }
}

/// `Performance.getMetrics`, as a sorted map.
pub async fn metrics(page: &Page) -> Result<BTreeMap<String, f64>, BrowserError> {
    page.execute(PerfEnable::default()).await?;
    let response = page.execute(GetMetricsParams::default()).await?;
    Ok(response
        .result
        .metrics
        .iter()
        .map(|m| (m.name.clone(), m.value))
        .collect())
}

/// `Emulation.setCPUThrottlingRate`. A rate of 1 is the identity, i.e. no
/// throttle.
pub async fn throttle(page: &Page, rate: f64) -> Result<(), BrowserError> {
    page.execute(SetCpuThrottlingRateParams::new(rate)).await?;
    Ok(())
}

/// Forces a full GC, then reads the live heap.
///
/// The two belong together: a heap number taken at an arbitrary point in a GC
/// cycle measures when you asked, not what is retained.
pub async fn gc_heap(page: &Page) -> Result<Option<f64>, BrowserError> {
    page.execute(HeapEnable::default()).await?;
    page.execute(CollectGarbageParams::default()).await?;
    Ok(metrics(page).await?.get("JSHeapUsedSize").copied())
}

pub async fn cpu_start(page: &Page) -> Result<(), BrowserError> {
    page.execute(ProfilerEnable::default()).await?;
    page.execute(ProfilerStart::default()).await?;
    Ok(())
}

/// Stops sampling and returns the profile as JSON.
///
/// `Value` rather than the typed CDP profile because it goes straight to a file
/// for DevTools to open, and re-deriving the summary later reads it back the
/// same way.
pub async fn cpu_stop(page: &Page) -> Result<serde_json::Value, BrowserError> {
    let response = page.execute(ProfilerStop::default()).await?;
    // Best effort: the profile is already in hand, and failing here would throw
    // away a successful capture over a housekeeping call.
    let _ = page.execute(ProfilerDisable::default()).await;
    serde_json::to_value(&response.result.profile)
        .map_err(|e| BrowserError::Operation(format!("unreadable CPU profile: {e}")))
}

/// Arms a trace. The caller is responsible for having cleared the buffer.
pub async fn trace_start(page: &Page, categories: Option<&str>) -> Result<(), BrowserError> {
    let categories = categories.unwrap_or(
        "devtools.timeline,disabled-by-default-devtools.timeline,\
         disabled-by-default-devtools.timeline.frame,blink.user_timing",
    );
    let config = TraceConfig {
        included_categories: Some(categories.split(',').map(|c| c.trim().to_string()).collect()),
        ..Default::default()
    };
    let params = TraceStart::builder()
        .trace_config(config)
        .build();
    page.execute(params).await?;
    Ok(())
}

/// Ends a trace and waits for Chrome to finish flushing it.
///
/// `Tracing.end` returns immediately; the events arrive afterwards as
/// `dataCollected` and the flush is only over at `tracingComplete`. Returning
/// at `end` would hand back whatever happened to have arrived, which is a
/// truncated trace that looks like a complete one.
pub async fn trace_stop(page: &Page) -> Result<(), BrowserError> {
    let mut complete = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::tracing::EventTracingComplete>()
        .await?;
    page.execute(TraceEnd::default()).await?;
    let _ = tokio::time::timeout(TRACE_COMPLETE_TIMEOUT, complete.next()).await;
    Ok(())
}

pub async fn coverage_start(page: &Page) -> Result<(), BrowserError> {
    page.execute(ProfilerEnable::default()).await?;
    page.execute(StartPreciseCoverageParams {
        call_count: Some(true),
        detailed: Some(true),
        allow_triggered_updates: None,
    })
    .await?;
    Ok(())
}

/// Takes the coverage collected so far and stops collecting.
pub async fn coverage_stop(page: &Page) -> Result<serde_json::Value, BrowserError> {
    let response = page.execute(TakePreciseCoverageParams::default()).await?;
    let _ = page.execute(StopPreciseCoverageParams::default()).await;
    let _ = page.execute(ProfilerDisable::default()).await;
    serde_json::to_value(&response.result.result)
        .map_err(|e| BrowserError::Operation(format!("unreadable coverage: {e}")))
}

/// Takes a heap snapshot, reassembled from the chunks CDP streams it in.
///
/// The snapshot does not come back from the call: it arrives as a series of
/// `addHeapSnapshotChunk` events while `takeHeapSnapshot` runs, which is why
/// the listener is subscribed first.
pub async fn heap_snapshot(page: &Page) -> Result<String, BrowserError> {
    let mut chunks = page.event_listener::<EventAddHeapSnapshotChunk>().await?;
    page.execute(HeapEnable::default()).await?;

    let collector = tokio::spawn(async move {
        let mut snapshot = String::new();
        while let Some(chunk) = chunks.next().await {
            snapshot.push_str(&chunk.chunk);
        }
        snapshot
    });

    page.execute(TakeHeapSnapshotParams {
        report_progress: Some(false),
        capture_numeric_value: None,
        expose_internals: None,
    })
    .await?;

    // The stream has no end of its own, so give the tail a moment to arrive and
    // then take what there is.
    tokio::time::sleep(Duration::from_millis(500)).await;
    collector.abort();
    match collector.await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.is_cancelled() => Ok(String::new()),
        Err(error) => Err(BrowserError::Operation(error.to_string())),
    }
}

/// Opens a measurement window in the page.
///
/// Best effort: a page without the helper simply has no window, and the read
/// below returns the absent default rather than a fabricated zero.
pub async fn perf_reset(page: &Page) -> Result<(), BrowserError> {
    let _ = page
        .evaluate("window.__kingdom && window.__kingdom.__perfReset && window.__kingdom.__perfReset()")
        .await;
    Ok(())
}

/// Closes the window and reads what happened inside it.
pub async fn perf_read(page: &Page) -> PerfReading {
    let script = "window.__kingdom && window.__kingdom.__perfRead \
                  ? window.__kingdom.__perfRead() : null";
    match page.evaluate(script).await {
        Ok(result) => result
            .into_value::<Option<String>>()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(PerfReading::absent),
        Err(_) => PerfReading::absent(),
    }
}

/// What the page can say about why its components re-rendered.
///
/// `None` when there is no helper or no React, which the caller reports as such
/// rather than as an empty finding.
pub async fn why_render(page: &Page) -> Option<serde_json::Value> {
    let script = "(function(){ try { \
                  if (!window.__kingdom || !window.__kingdom.__getWhyRender) return null; \
                  return JSON.stringify(window.__kingdom.__getWhyRender()); \
                  } catch(e) { return null; } })()";
    let raw = page
        .evaluate(script)
        .await
        .ok()?
        .into_value::<Option<String>>()
        .ok()??;
    serde_json::from_str(&raw).ok()
}
