//! `browser_profile`: measuring a page, and finding out why it is slow.
//!
//! # One tool, many actions
//!
//! Kingdom's norm is one struct per tool name. This is the deliberate
//! exception, and it is Phoenix's reasoning carried over intact: the actions
//! form three start/stop sub-machines over state that lives on the *session*,
//! plus one-shot reads that share it. Split into separate `Tool` structs, that
//! state would be scattered across a dozen types and the preconditions --
//! "stop needs a start" -- would have nowhere to live.
//!
//! # The invariant that matters
//!
//! `run_scenario` returns the raw per-run samples and computes **no** mean, no
//! variance, no significance. This is not laziness. A profiler that averages
//! for you is a profiler that hides the bimodal distribution which was the
//! actual finding -- "120ms" reads as a steady cost when the truth was nine
//! runs at 40ms and one at 840ms, and the one is the bug. There is exactly one
//! place samples are emitted and it hands back the untouched vector.
//!
//! Methodology warnings travel *alongside* the samples rather than modifying
//! them, for the same reason: a caller must be told its measurement is shaky
//! without having the data quietly adjusted underneath it.

use super::{Refusal, Tool, Workshop};
use kingdom_browser::{profile, BrowserError, PerfReading};
use kingdom_core::ToolOutcome;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use super::browser::browsers;

/// Output past this goes to a file, matching `browser_eval`'s escape.
const LARGE_OUTPUT: usize = 4 * 1024;

/// Every action, in one place so the schema and the dispatch cannot drift.
const ACTIONS: &[&str] = &[
    "help",
    "metrics",
    "throttle",
    "gc_heap",
    "run_scenario",
    "cpu_start",
    "cpu_stop",
    "trace_start",
    "trace_stop",
    "why_render",
    "heap_snapshot",
    "coverage_start",
    "coverage_stop",
];

#[derive(Deserialize)]
struct ProfileInput {
    action: String,
    #[serde(default)]
    rate: Option<f64>,
    #[serde(default)]
    categories: Option<String>,
    #[serde(default)]
    steps: Option<Vec<Step>>,
    #[serde(default)]
    runs: Option<u32>,
    #[serde(default)]
    warmup: Option<u32>,
    #[serde(default)]
    throttle_rate: Option<f64>,
    #[serde(default)]
    gc_per_run: Option<bool>,
    #[serde(default)]
    reset: Option<ResetSpec>,
}

/// One step of a scenario.
///
/// `navigate` and `reload` are absent on purpose: navigation belongs in
/// `reset`, where it happens *before* the measurement window opens. A navigate
/// inside the measured steps would put page load into the numbers, which is
/// almost never what was being asked and is invisible in the result.
#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    Click {
        selector: String,
    },
    Type {
        selector: String,
        text: String,
    },
    Key {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Eval {
        expression: String,
    },
    WaitSelector {
        selector: String,
        #[serde(default)]
        timeout: Option<String>,
    },
    WaitEval {
        expression: String,
        #[serde(default)]
        timeout: Option<String>,
    },
}

impl Step {
    /// Whether this step is a readiness gate. The first one closes setup and
    /// opens the measured window.
    fn is_wait(&self) -> bool {
        matches!(self, Step::WaitSelector { .. } | Step::WaitEval { .. })
    }
}

/// How each run returns to a known state before it is measured.
#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum ResetSpec {
    /// The literal `"none"`. Its own type so that a misspelt opt-out fails to
    /// parse rather than silently meaning "do not reset".
    None(NoneLiteral),
    Action(ResetAction),
}

#[derive(Deserialize, Clone)]
enum NoneLiteral {
    #[serde(rename = "none")]
    None,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ResetAction {
    Navigate { url: String },
    Reload,
}

/// The resolved per-run reset, after the default is applied.
#[derive(Clone)]
enum Reset {
    /// Reload whatever the page is on. The default: without it, run two starts
    /// from the state run one left behind, and the runs are not comparable.
    ReloadCurrent,
    Navigate(String),
    Skip,
}

impl Reset {
    fn resolve(spec: Option<&ResetSpec>) -> Self {
        match spec {
            None => Reset::ReloadCurrent,
            Some(ResetSpec::None(_)) => Reset::Skip,
            Some(ResetSpec::Action(ResetAction::Reload)) => Reset::ReloadCurrent,
            Some(ResetSpec::Action(ResetAction::Navigate { url })) => Reset::Navigate(url.clone()),
        }
    }
}

/// One run's measurements, exactly as taken.
#[derive(Debug, Clone, serde::Serialize)]
struct Sample {
    run: u32,
    script_ms: f64,
    long_tasks: u64,
    wall_ms: Option<f64>,
    dom_nodes: u64,
    gc_ran: bool,
    /// `None` when GC was disabled: a heap read taken at an arbitrary point in
    /// a collection cycle measures when you asked, not what is retained.
    js_heap_used: Option<f64>,
    react_status: String,
    react_commits: Option<u64>,
    react_actual_ms: Option<f64>,
}

impl Sample {
    fn from(run: u32, reading: PerfReading, gc_ran: bool, heap: Option<f64>) -> Self {
        Self {
            run,
            script_ms: reading.script_ms,
            long_tasks: reading.long_tasks,
            wall_ms: reading.wall_ms,
            dom_nodes: reading.dom_nodes,
            gc_ran,
            js_heap_used: heap,
            react_status: reading.react_status,
            react_commits: reading.react_commits,
            react_actual_ms: reading.react_actual_ms,
        }
    }
}

fn bad(detail: impl Into<String>) -> ToolOutcome {
    Refusal::BadArguments {
        tool: "browser_profile".to_string(),
        detail: detail.into(),
    }
    .into()
}

fn failed(error: BrowserError) -> ToolOutcome {
    match error {
        BrowserError::ChromeUnavailable(reason) => Refusal::Refused(reason).into(),
        other => ToolOutcome::done(other.to_string()),
    }
}

fn duration(value: Option<&str>) -> Duration {
    let Some(value) = value else {
        return Duration::from_secs(30);
    };
    let value = value.trim();
    if let Some(ms) = value.strip_suffix("ms") {
        ms.trim().parse().ok().map(Duration::from_millis)
    } else if let Some(s) = value.strip_suffix('s') {
        s.trim().parse().ok().map(Duration::from_secs)
    } else {
        value.parse().ok().map(Duration::from_secs)
    }
    .unwrap_or(Duration::from_secs(30))
}

pub struct BrowserProfile;

#[async_trait::async_trait]
impl Tool for BrowserProfile {
    fn name(&self) -> &'static str {
        "browser_profile"
    }

    fn description(&self) -> String {
        "Measure web performance and find the cause. One tool, many actions via \
         `action`; call action=\"help\" for the reference. run_scenario is the \
         harness: it returns RAW per-run samples and never averages them, so \
         the statistics are yours to compute. Also: metrics, throttle, gc_heap, \
         cpu_start/cpu_stop, trace_start/trace_stop, coverage_start/\
         coverage_stop, why_render, heap_snapshot."
            .into()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": { "type": "string", "enum": ACTIONS },
                "rate": { "type": "number", "description": "throttle only: CPU slowdown factor (>= 1; 1 clears it)." },
                "categories": { "type": "string", "description": "trace_start only: comma-separated trace categories." },
                "steps": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "run_scenario only. Each step has a `kind`: click{selector}, type{selector,text}, key{key,modifiers?}, eval{expression}, wait_selector{selector,timeout?}, wait_eval{expression,timeout?}. The measured window opens AFTER the FIRST wait_* step satisfies, so put a readiness wait first and page load plus framework mount stay out of the numbers. navigate/reload are NOT steps -- put navigation in `reset`."
                },
                "runs": { "type": "integer", "minimum": 1, "description": "run_scenario only: measured runs." },
                "warmup": { "type": "integer", "minimum": 0, "description": "run_scenario only: discarded runs. Default 1, which excludes cold JIT and first paint." },
                "throttle_rate": { "type": "number", "description": "run_scenario only: CPU slowdown for the scenario, restored afterwards." },
                "gc_per_run": { "type": "boolean", "description": "run_scenario only: force a GC once per run, outside the measured window, and read the heap there. Default true." },
                "reset": { "description": "run_scenario only: per-run reset. Omitted reloads the current URL; {\"kind\":\"navigate\",\"url\":...}; or \"none\" to opt out (warned)." }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Workshop) -> ToolOutcome {
        let input: ProfileInput = match serde_json::from_value(input) {
            Ok(input) => input,
            Err(error) => return bad(error.to_string()),
        };

        // `help` needs no browser. Answered before anything is launched, so a
        // model can read the reference without starting a Chrome to do it.
        if input.action == "help" {
            return ToolOutcome::done(help());
        }

        let plan = shop.plan().to_string();

        // Scenario arguments are checked before the browser is touched. The
        // other order reports "could not start Chrome" for what is really an
        // empty `steps` array, sending the model to fix the wrong thing.
        if input.action == "run_scenario" {
            return match scenario(&input, &plan, shop).await {
                Ok(outcome) => outcome,
                Err(outcome) => outcome,
            };
        }

        match input.action.as_str() {
            "metrics" => match browsers()
                .profiling(&plan, |page, _| Box::pin(profile::metrics(page)))
                .await
            {
                Ok(metrics) => ToolOutcome::done(
                    serde_json::to_string_pretty(&metrics).unwrap_or_else(|_| "{}".into()),
                ),
                Err(error) => failed(error),
            },

            "throttle" => {
                let Some(rate) = input.rate else {
                    return bad("throttle needs `rate` (a number >= 1; 1 clears it)");
                };
                if rate < 1.0 {
                    return bad(format!(
                        "a slowdown factor of {rate} is not meaningful: use >= 1, where 1 is no throttle"
                    ));
                }
                match browsers()
                    .profiling(&plan, move |page, state| {
                        Box::pin(async move {
                            profile::throttle(page, rate).await?;
                            if let Ok(mut state) = state.lock() {
                                state.throttle = (rate > 1.0).then_some(rate);
                            }
                            Ok(())
                        })
                    })
                    .await
                {
                    Ok(()) if rate > 1.0 => {
                        ToolOutcome::done(format!("CPU throttled to {rate}x slowdown."))
                    }
                    Ok(()) => ToolOutcome::done("CPU throttling cleared."),
                    Err(error) => failed(error),
                }
            }

            "gc_heap" => match browsers()
                .profiling(&plan, |page, _| Box::pin(profile::gc_heap(page)))
                .await
            {
                Ok(Some(used)) => {
                    ToolOutcome::done(format!("Forced GC. JSHeapUsedSize = {used} bytes."))
                }
                Ok(None) => ToolOutcome::done("Forced GC, but the heap size was not reported."),
                Err(error) => failed(error),
            },

            "cpu_start" => gated_start(&plan, Machine::Cpu).await,
            "cpu_stop" => cpu_stop(&plan, shop).await,
            "coverage_start" => gated_start(&plan, Machine::Coverage).await,
            "coverage_stop" => coverage_stop(&plan, shop).await,
            "trace_start" => trace_start(&plan, input.categories.as_deref()).await,
            "trace_stop" => trace_stop(&plan, shop).await,

            "heap_snapshot" => match browsers()
                .profiling(&plan, |page, _| Box::pin(profile::heap_snapshot(page)))
                .await
            {
                Ok(snapshot) if snapshot.is_empty() => {
                    ToolOutcome::done("The heap snapshot came back empty.")
                }
                Ok(snapshot) => {
                    let path = artifact(shop, "heap", "heapsnapshot");
                    match tokio::fs::write(&path, snapshot).await {
                        Ok(()) => ToolOutcome::done(format!(
                            "Heap snapshot saved to {}. Open it in Chrome DevTools \u{2192} Memory.",
                            path.display()
                        )),
                        Err(error) => ToolOutcome::done(format!("Could not save it: {error}")),
                    }
                }
                Err(error) => failed(error),
            },

            "why_render" => match browsers()
                .profiling(&plan, |page, _| Box::pin(async move {
                    Ok(profile::why_render(page).await)
                }))
                .await
            {
                Ok(Some(found)) => ToolOutcome::done(
                    serde_json::to_string_pretty(&found).unwrap_or_else(|_| "{}".into()),
                ),
                Ok(None) => ToolOutcome::done(
                    "Nothing to report: this page has no React, or nothing has re-rendered yet. \
                     Interact with it or run a scenario first.",
                ),
                Err(error) => failed(error),
            },

            other => bad(format!(
                "there is no `{other}` action; call action=\"help\" for the list"
            )),
        }
    }
}

#[derive(Clone, Copy)]
enum Machine {
    Cpu,
    Coverage,
}

/// Starts a sub-machine, or says it is already running.
///
/// Already-started is a success no-op rather than a restart: restarting would
/// throw away the profile the caller believes it is collecting, and it would
/// do so silently.
async fn gated_start(plan: &str, machine: Machine) -> ToolOutcome {
    let result = browsers()
        .profiling(plan, move |page, state| {
            Box::pin(async move {
                let already = state
                    .lock()
                    .map(|s| match machine {
                        Machine::Cpu => s.cpu_active,
                        Machine::Coverage => s.coverage_active,
                    })
                    .unwrap_or(false);
                if already {
                    return Ok(true);
                }
                match machine {
                    Machine::Cpu => profile::cpu_start(page).await?,
                    Machine::Coverage => profile::coverage_start(page).await?,
                }
                if let Ok(mut state) = state.lock() {
                    match machine {
                        Machine::Cpu => state.cpu_active = true,
                        Machine::Coverage => state.coverage_active = true,
                    }
                }
                Ok(false)
            })
        })
        .await;

    let what = match machine {
        Machine::Cpu => "CPU profiling",
        Machine::Coverage => "Coverage collection",
    };
    match result {
        Ok(true) => ToolOutcome::done(format!("{what} was already running.")),
        Ok(false) => ToolOutcome::done(format!("{what} started.")),
        Err(error) => failed(error),
    }
}

async fn cpu_stop(plan: &str, shop: &Workshop) -> ToolOutcome {
    let result = browsers()
        .profiling(plan, |page, state| {
            Box::pin(async move {
                // Stopping something never started is an error, not an empty
                // profile: "you forgot to start" is something a model can act
                // on, whereas an empty profile reads as a finding.
                if !state.lock().map(|s| s.cpu_active).unwrap_or(false) {
                    return Ok(None);
                }
                let profile = profile::cpu_stop(page).await?;
                if let Ok(mut state) = state.lock() {
                    state.cpu_active = false;
                }
                Ok(Some(profile))
            })
        })
        .await;

    match result {
        Ok(None) => Refusal::Refused(
            "CPU profiling is not running; call cpu_start before cpu_stop.".into(),
        )
        .into(),
        Ok(Some(profile)) => {
            let path = artifact(shop, "cpu-profile", "cpuprofile");
            let body = serde_json::to_string(&profile).unwrap_or_else(|_| "{}".into());
            match tokio::fs::write(&path, body).await {
                Ok(()) => ToolOutcome::done(format!(
                    "CPU profile saved to {}. Open it in Chrome DevTools \u{2192} Performance.",
                    path.display()
                )),
                Err(error) => ToolOutcome::done(format!("Could not save it: {error}")),
            }
        }
        Err(error) => failed(error),
    }
}

async fn coverage_stop(plan: &str, shop: &Workshop) -> ToolOutcome {
    let result = browsers()
        .profiling(plan, |page, state| {
            Box::pin(async move {
                if !state.lock().map(|s| s.coverage_active).unwrap_or(false) {
                    return Ok(None);
                }
                let coverage = profile::coverage_stop(page).await?;
                if let Ok(mut state) = state.lock() {
                    state.coverage_active = false;
                }
                Ok(Some(coverage))
            })
        })
        .await;

    match result {
        Ok(None) => Refusal::Refused(
            "Coverage is not being collected; call coverage_start before coverage_stop.".into(),
        )
        .into(),
        Ok(Some(coverage)) => {
            let scripts = coverage.as_array().map_or(0, Vec::len);
            let path = artifact(shop, "coverage", "json");
            let body = serde_json::to_string(&coverage).unwrap_or_else(|_| "[]".into());
            match tokio::fs::write(&path, body).await {
                Ok(()) => ToolOutcome::done(format!(
                    "Coverage for {scripts} script(s) saved to {}.",
                    path.display()
                )),
                Err(error) => ToolOutcome::done(format!("Could not save it: {error}")),
            }
        }
        Err(error) => failed(error),
    }
}

async fn trace_start(plan: &str, categories: Option<&str>) -> ToolOutcome {
    let categories = categories.map(str::to_string);
    let result = browsers()
        .profiling(plan, move |page, state| {
            Box::pin(async move {
                if state.lock().map(|s| s.tracing_active).unwrap_or(false) {
                    return Ok(true);
                }
                profile::trace_start(page, categories.as_deref()).await?;
                if let Ok(mut state) = state.lock() {
                    state.tracing_active = true;
                    // Armed with an empty buffer, so what a trace holds is what
                    // that trace saw.
                    state.trace_events.clear();
                }
                Ok(false)
            })
        })
        .await;

    match result {
        Ok(true) => ToolOutcome::done("Tracing was already running."),
        Ok(false) => ToolOutcome::done("Tracing started."),
        Err(error) => failed(error),
    }
}

async fn trace_stop(plan: &str, shop: &Workshop) -> ToolOutcome {
    let result = browsers()
        .profiling(plan, |page, state| {
            Box::pin(async move {
                if !state.lock().map(|s| s.tracing_active).unwrap_or(false) {
                    return Ok(None);
                }
                profile::trace_stop(page).await?;
                let events = state
                    .lock()
                    .map(|mut s| {
                        s.tracing_active = false;
                        std::mem::take(&mut s.trace_events)
                    })
                    .unwrap_or_default();
                Ok(Some(events))
            })
        })
        .await;

    match result {
        Ok(None) => {
            Refusal::Refused("Tracing is not running; call trace_start before trace_stop.".into())
                .into()
        }
        Ok(Some(events)) => {
            let path = artifact(shop, "trace", "json");
            let body = serde_json::to_string(&events).unwrap_or_else(|_| "[]".into());
            match tokio::fs::write(&path, body).await {
                Ok(()) => ToolOutcome::done(format!(
                    "Trace with {} event(s) saved to {}. Open it in Chrome DevTools \u{2192} \
                     Performance.",
                    events.len(),
                    path.display()
                )),
                Err(error) => ToolOutcome::done(format!("Could not save it: {error}")),
            }
        }
        Err(error) => failed(error),
    }
}

fn artifact(shop: &Workshop, stem: &str, extension: &str) -> std::path::PathBuf {
    let serial = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    shop.root()
        .join(format!(".kingdom-{stem}-{serial}.{extension}"))
}

/// Checks a scenario's arguments and, if they hold, runs it.
///
/// The validation is separated from the running so that it can happen before a
/// browser is involved -- see the note at the call site.
async fn scenario(
    input: &ProfileInput,
    plan: &str,
    shop: &Workshop,
) -> Result<ToolOutcome, ToolOutcome> {
    let steps = input.steps.clone().unwrap_or_default();
    if steps.is_empty() {
        return Err(bad("run_scenario needs a non-empty `steps` array"));
    }
    let runs = input.runs.unwrap_or(0);
    if runs == 0 {
        return Err(bad("run_scenario needs `runs` (an integer >= 1)"));
    }
    if let Some(rate) = input.throttle_rate {
        if rate < 1.0 {
            return Err(bad(format!(
                "a throttle_rate of {rate} is not meaningful: use >= 1"
            )));
        }
    }

    // Default 1: the first run pays for cold JIT and first paint, which is a
    // real cost but not the one being compared between runs.
    let warmup = input.warmup.unwrap_or(1);
    let gc_per_run = input.gc_per_run.unwrap_or(true);
    let reset = Reset::resolve(input.reset.as_ref());

    let mut warnings: Vec<String> = Vec::new();
    if input.throttle_rate.is_none() {
        warnings.push(
            "No throttle_rate: an unthrottled measurement on a fast machine is \
             dominated by host noise, and differences between runs may not \
             reproduce anywhere else."
                .into(),
        );
    }
    if input.warmup == Some(0) {
        warnings.push(
            "warmup is 0: the first run includes cold JIT and first paint, so \
             it is not comparable with the rest."
                .into(),
        );
    }
    if matches!(reset, Reset::Skip) {
        warnings.push(
            "reset is \"none\": each run starts from whatever the last one left \
             behind, so the runs are not independent."
                .into(),
        );
    }
    if !steps.iter().any(Step::is_wait) {
        warnings.push(
            "No wait_* step: the measured window opens immediately after reset, \
             so page load, framework mount and async settle are inside it. Put \
             a readiness wait first to exclude them."
                .into(),
        );
    }

    let mut samples: Vec<Sample> = Vec::new();
    for index in 0..(warmup + runs) {
        let sample = match one_run(plan, &steps, index, &reset, gc_per_run).await {
            Ok(sample) => sample,
            Err(error) => return Err(failed(error)),
        };
        // Warmups are discarded here rather than filtered later, so there is no
        // point at which a warmup could be mistaken for a measurement.
        if index >= warmup {
            samples.push(Sample { run: index - warmup, ..sample });
        }
    }

    // The one place samples are emitted, and it hands back the vector
    // untouched. See the module doc: any reduction here would be the profiler
    // deciding what the finding was.
    let report = json!({
        "runs": samples.len(),
        "warmup_discarded": warmup,
        "raw_samples": samples,
        "methodology_warnings": warnings,
    });
    let body = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());

    if body.len() > LARGE_OUTPUT {
        let path = artifact(shop, "scenario", "json");
        return Ok(match tokio::fs::write(&path, &body).await {
            Ok(()) => ToolOutcome::done(format!("Scenario samples saved to {}.", path.display())),
            Err(error) => ToolOutcome::done(format!("Could not save them: {error}")),
        });
    }
    Ok(ToolOutcome::done(body))
}

/// One run: reset, untimed setup, measured steps, then the reading.
async fn one_run(
    plan: &str,
    steps: &[Step],
    index: u32,
    reset: &Reset,
    gc_per_run: bool,
) -> Result<Sample, BrowserError> {
    match reset {
        Reset::Navigate(url) => {
            browsers()
                .navigate(plan, url, Duration::from_secs(30))
                .await?;
        }
        Reset::ReloadCurrent => {
            browsers()
                .profiling(plan, |page, _| {
                    Box::pin(async move {
                        page.reload().await?;
                        Ok(())
                    })
                })
                .await?;
        }
        Reset::Skip => {}
    }

    // Everything up to and including the first readiness gate is setup, and is
    // not measured. That is the whole point of the gate: page load, framework
    // mount and async settle happen before the window opens, so what is timed
    // is the interaction rather than the arrival.
    let opens_at = steps.iter().position(Step::is_wait).map_or(0, |i| i + 1);
    for step in &steps[..opens_at] {
        run_step(plan, step).await?;
    }

    let measured: Vec<Step> = steps[opens_at..].to_vec();
    let plan_owned = plan.to_string();
    let reading = browsers()
        .measure(plan, move |_page| {
            let plan = plan_owned.clone();
            let measured = measured.clone();
            Box::pin(async move {
                for step in &measured {
                    run_step(&plan, step).await?;
                }
                Ok(())
            })
        })
        .await?;

    // Strictly after the window closes. A forced GC inside it would be measured
    // as the page's own work, which it is not.
    let heap = if gc_per_run {
        browsers()
            .profiling(plan, |page, _| Box::pin(profile::gc_heap(page)))
            .await?
    } else {
        None
    };

    Ok(Sample::from(index, reading, gc_per_run, heap))
}

async fn run_step(plan: &str, step: &Step) -> Result<(), BrowserError> {
    match step {
        Step::Click { selector } => {
            browsers()
                .click(plan, selector, false, Duration::from_secs(30))
                .await
        }
        Step::Type { selector, text } => {
            browsers()
                .type_text(plan, selector, text, false, Duration::from_secs(30))
                .await
        }
        Step::Key { key, modifiers } => {
            browsers()
                .key_press(plan, key, modifiers, kingdom_browser::KeyMethod::Cdp)
                .await
        }
        Step::Eval { expression } => browsers()
            .evaluate(plan, expression, true, Duration::from_secs(30))
            .await
            .map(|_| ()),
        Step::WaitSelector { selector, timeout } => {
            browsers()
                .wait_for_selector(plan, selector, false, duration(timeout.as_deref()))
                .await
        }
        Step::WaitEval {
            expression,
            timeout,
        } => {
            // Polled rather than awaited, because the condition is the page's to
            // satisfy and there is no event to wait on.
            let deadline = tokio::time::Instant::now() + duration(timeout.as_deref());
            loop {
                let value = browsers()
                    .evaluate(plan, expression, true, Duration::from_secs(5))
                    .await?;
                if value.trim() == "true" {
                    return Ok(());
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(BrowserError::Operation(format!(
                        "wait_eval never became true: {expression}"
                    )));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

fn help() -> String {
    "browser_profile - measuring a page, and finding out why it is slow.

run_scenario is the one that answers questions. The rest are instruments.

  help            This.

  metrics         Performance.getMetrics as JSON: ScriptDuration, LayoutCount,
                  JSHeapUsedSize, Nodes, JSEventListeners and the rest.

  throttle        rate (>= 1). Sets CPU slowdown; 1 clears it. Persists on the
                  session until changed.

  gc_heap         Force a full GC, then read the heap -- so the number means
                  what is retained rather than when you asked.

  run_scenario    steps (non-empty), runs (>= 1), warmup (default 1),
                  throttle_rate, gc_per_run (default true), reset.

                  Steps: click{selector}, type{selector,text},
                  key{key,modifiers?}, eval{expression},
                  wait_selector{selector,timeout?}, wait_eval{expression,timeout?}.
                  navigate/reload are NOT steps: put navigation in `reset`, so it
                  happens before the measured window opens.

                  THE WINDOW: measurement starts after the FIRST wait_* step
                  satisfies. Steps up to and including it are untimed setup.
                  Put a readiness wait first and page load, framework mount and
                  async settle stay out of your numbers.

                  RESET: omitted reloads the current URL before each run;
                  {\"kind\":\"navigate\",\"url\":...} navigates; \"none\" opts out and
                  is warned about.

                  RETURNS raw per-run samples. No mean, no stddev, no verdict --
                  those are yours. This is deliberate: an average hides the
                  bimodal distribution that was probably the actual finding.
                  Each sample carries script_ms (sum of in-window long tasks),
                  long_tasks, wall_ms, dom_nodes, gc_ran, js_heap_used,
                  react_status (measured | absent | no_profiling_build),
                  react_commits and react_actual_ms. A React timing that was not
                  measured is null, never 0.

                  methodology_warnings arrives alongside the samples and never
                  alters them.

  cpu_start /     Sample the JS stack. cpu_stop writes a .cpuprofile into the
  cpu_stop        workspace for DevTools -> Performance.

  trace_start /   Chrome tracing. trace_start takes optional `categories`.
  trace_stop

  coverage_start/ Precise JS coverage: which code actually ran.
  coverage_stop

  heap_snapshot   A .heapsnapshot for DevTools -> Memory.

  why_render      Which React components re-rendered and what changed. Needs a
                  React page; interact with it or run a scenario first.

Starting something already started is a no-op, not a restart. Stopping
something that never started is an error, so a forgotten start is visible
rather than looking like an empty result."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    fn workshop() -> Workshop {
        Workshop::new(Workspace::in_place("/tmp"))
            .for_plan(kingdom_core::PlanId::new("profile-test"))
    }

    /// A scenario's arguments must be judged before a browser is involved.
    ///
    /// Otherwise an empty `steps` array on a machine with no Chrome reports
    /// "Chrome is not available" -- and the model goes off to install a browser
    /// to fix a typo in its own arguments.
    #[tokio::test]
    async fn a_malformed_scenario_is_refused_on_its_own_terms_not_the_browser_s() {
        for (args, expected) in [
            (json!({ "action": "run_scenario", "runs": 3 }), "steps"),
            (
                json!({
                    "action": "run_scenario",
                    "steps": [{ "kind": "click", "selector": "#go" }]
                }),
                "runs",
            ),
        ] {
            let outcome = BrowserProfile.run(args, &workshop()).await;
            let ToolOutcome::Refused { reason } = outcome else {
                panic!("a malformed scenario must be refused: {outcome:?}");
            };
            assert!(
                reason.contains(expected) && !reason.to_lowercase().contains("chrome"),
                "the refusal should name the bad argument, not the browser: {reason}"
            );
        }
    }

    /// `navigate` inside `steps` must not parse.
    ///
    /// It is the natural thing to reach for and it silently ruins the
    /// measurement -- page load lands inside the window and swamps whatever was
    /// being compared. Rejecting it at parse time sends the model to `reset`,
    /// which is where navigation belongs.
    #[tokio::test]
    async fn navigating_inside_the_measured_steps_is_not_expressible() {
        let outcome = BrowserProfile
            .run(
                json!({
                    "action": "run_scenario",
                    "runs": 1,
                    "steps": [{ "kind": "navigate", "url": "http://localhost:3000" }]
                }),
                &workshop(),
            )
            .await;

        assert!(
            matches!(outcome, ToolOutcome::Refused { .. }),
            "navigate is not a step: {outcome:?}"
        );
    }

    /// Help must work with no browser at all.
    ///
    /// A reference a model can only read by first launching Chrome is a
    /// reference it will guess at instead.
    #[tokio::test]
    async fn help_needs_no_browser_and_names_every_action() {
        let outcome = BrowserProfile
            .run(json!({ "action": "help" }), &workshop())
            .await;

        let ToolOutcome::Done { output, .. } = outcome else {
            panic!("help must not need a browser: {outcome:?}");
        };
        for action in ACTIONS {
            assert!(
                output.contains(action),
                "help should describe `{action}`, or the schema offers something undocumented"
            );
        }
    }
}
