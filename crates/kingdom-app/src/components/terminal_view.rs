//! The terminal panel: the King's own shell, beside the transcript.
//!
//! The client half of [`crate::terminal`]. That module's docs carry the
//! reasoning; this one draws it.
//!
//! # Why xterm.js and not a `<pre>`
//!
//! A shell emits ANSI: colour, cursor movement, alternate screens. `cargo`'s
//! progress bar and `vim` are both unreadable without an emulator that
//! understands them, and writing one is not a side quest. xterm.js is vendored
//! at `public/vendor/xterm.js` and loaded lazily on first open, exactly as
//! `markdown.rs` treats mermaid -- 289 KB nobody pays for unless they open a
//! terminal.

use leptos::prelude::*;

/// A live shell in one plan's workspace.
#[component]
pub fn TerminalView(
    plan: kingdom_core::PlanId,
    /// Whether this plan has a network of its own, for the header line.
    isolated: bool,
    /// The panel's width in pixels, driven by the resizer beside it.
    width: RwSignal<f64>,
    /// Whether the panel has the conversation's room as well as its own.
    focused: Signal<bool>,
    on_focus: Callback<()>,
    on_close: Callback<()>,
) -> impl IntoView {
    let stage = NodeRef::<leptos::html::Div>::new();

    open_shell(plan, stage);

    view! {
        <div
            class="terminal-panel chamber-aside"
            style:width=move || (!focused.get()).then(|| format!("{}px", width.get()))
        >
            <div class="terminal-bar">
                <span class="terminal-title">"Terminal"</span>
                // Says which network this shell is in, because that is the
                // whole reason it exists. A King who has forgotten which kind
                // of plan this is would otherwise have to guess why `:3000`
                // does or does not answer.
                <span class="terminal-note">
                    {if isolated { "in this plan's network" } else { "shared network" }}
                </span>
                <button
                    class="diff-chip"
                    class:on=move || focused.get()
                    title="Give this panel the conversation's room as well as its own"
                    on:click=move |_| on_focus.run(())
                >
                    {move || if focused.get() { "Show conversation" } else { "Focus" }}
                </button>
                <button
                    class="terminal-close"
                    title="Close this terminal"
                    on:click=move |_| on_close.run(())
                >
                    "\u{00d7}"
                </button>
            </div>
            <div class="terminal-stage" node_ref=stage></div>
        </div>
    }
}

/// Opens the socket and wires xterm.js to it.
///
/// One JavaScript function rather than a pile of `Reflect` calls, for the
/// reason `markdown.rs::draw_diagrams` gives: this loads a script, constructs a
/// terminal, and binds three event handlers, which is a dozen lines of JS and
/// several dozen of glue.
#[cfg(feature = "hydrate")]
fn open_shell(plan: kingdom_core::PlanId, stage: NodeRef<leptos::html::Div>) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::JsValue;

    // Takes its two values from `arguments` rather than from named parameters.
    //
    // `js_sys::Function::new_with_args` compiles the body with the names it is
    // given already declared, so a body that *also* wrote `const stage =
    // arguments[0]` is a redeclaration -- a SyntaxError thrown at construction,
    // before a line of it runs. `markdown.rs` uses `new_no_args` and reads
    // `arguments`, which is why it never met this; matching that shape here
    // keeps the two files reading the same way.
    const OPEN: &str = r#"
const stage = arguments[0];
const url = arguments[1];

const load = () => {
  if (window.Terminal) return Promise.resolve();
  if (!window.__kingdomXterm) {
    window.__kingdomXterm = new Promise((resolve, reject) => {
      const css = document.createElement('link');
      css.rel = 'stylesheet';
      css.href = '/vendor/xterm.css';
      document.head.appendChild(css);

      const script = document.createElement('script');
      script.src = '/vendor/xterm.js';
      script.onload = () => {
        const fit = document.createElement('script');
        fit.src = '/vendor/xterm-addon-fit.js';
        // The fit addon is a convenience, not a requirement: without it the
        // terminal simply keeps its default size.
        fit.onload = () => resolve();
        fit.onerror = () => resolve();
        document.head.appendChild(fit);
      };
      script.onerror = () => reject(new Error('xterm could not be loaded'));
      document.head.appendChild(script);
    });
  }
  return window.__kingdomXterm;
};

(async () => {
  try { await load(); } catch (e) {
    stage.textContent = 'The terminal could not be loaded.';
    return;
  }
  if (stage.dataset.kingdomOpened) return;
  stage.dataset.kingdomOpened = '1';

  const term = new window.Terminal({
    fontSize: 13,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    theme: { background: '#0b1120', foreground: '#e2e8f0', cursor: '#d4af37' },
    cursorBlink: true,
  });
  const fit = window.FitAddon ? new window.FitAddon.FitAddon() : null;
  if (fit) term.loadAddon(fit);
  term.open(stage);
  if (fit) fit.fit();

  const socket = new WebSocket(url);
  socket.binaryType = 'arraybuffer';

  const send = (tag, payload) => {
    if (socket.readyState !== WebSocket.OPEN) return;
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = tag;
    frame.set(payload, 1);
    socket.send(frame);
  };

  const encoder = new TextEncoder();
  term.onData((data) => send(0x00, encoder.encode(data)));

  const resize = () => {
    if (fit) fit.fit();
    const dims = new Uint8Array(4);
    new DataView(dims.buffer).setUint16(0, term.cols);
    new DataView(dims.buffer).setUint16(2, term.rows);
    send(0x01, dims);
  };

  socket.onopen = () => { term.focus(); resize(); };
  socket.onmessage = (event) => {
    if (typeof event.data === 'string') { term.write(event.data); return; }
    term.write(new Uint8Array(event.data));
  };
  socket.onclose = () => term.write('\r\n[disconnected]\r\n');

  const observer = new ResizeObserver(() => resize());
  observer.observe(stage);

  // The panel is being torn down: close the socket, which is what kills the
  // shell at the other end. Without this a closed panel would leave a shell
  // running until the server noticed the socket was dead.
  stage.__kingdomCleanup = () => {
    observer.disconnect();
    socket.close();
    term.dispose();
  };
})();
"#;

    Effect::new(move |_| {
        let Some(element) = stage.get() else {
            return;
        };

        let location = web_sys::window().and_then(|w| w.location().host().ok());
        let Some(host) = location else { return };
        let scheme = web_sys::window()
            .and_then(|w| w.location().protocol().ok())
            .filter(|p| p == "https:")
            .map_or("ws", |_| "wss");
        let url = format!(
            "{scheme}://{host}{}",
            crate::terminal_route::ROUTE.replace("{id}", plan.as_str())
        );

        // Constructed with no declared parameters, so the body's own
        // `arguments` reads do not collide with them. See `OPEN`.
        let open = js_sys::Function::new_no_args(OPEN);
        // Reported rather than swallowed: a `let _` here is what hid a
        // SyntaxError in this very script behind a silently blank panel.
        if let Err(error) = open.call2(&JsValue::NULL, &element, &JsValue::from_str(&url)) {
            web_sys::console::error_2(&JsValue::from_str("the terminal failed to open:"), &error);
        }
    });

    // Closing the panel must close the shell. Leptos runs this when the
    // component is disposed, which is the moment the King pressed the button.
    on_cleanup(move || {
        if let Some(element) = stage.get_untracked() {
            let cleanup = js_sys::Reflect::get(&element, &JsValue::from_str("__kingdomCleanup"));
            if let Ok(cleanup) = cleanup {
                if let Ok(function) = cleanup.dyn_into::<js_sys::Function>() {
                    let _ = function.call0(&JsValue::NULL);
                }
            }
        }
    });
}

#[cfg(not(feature = "hydrate"))]
fn open_shell(_plan: kingdom_core::PlanId, _stage: NodeRef<leptos::html::Div>) {}
