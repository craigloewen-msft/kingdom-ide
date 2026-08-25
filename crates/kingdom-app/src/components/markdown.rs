//! Markdown, rendered — the court's prose as the King is meant to read it.
//!
//! The model writes markdown whether or not anything renders it, so for most of
//! Kingdom's life a proposal arrived as a wall of `##` and `-` in a `<pre>`.
//! This is the renderer that ends that, and the reason
//! `llm/system_prompt.rs` may now tell a model its diagrams are drawn.
//!
//! Two rules hold this module up.
//!
//! **Raw HTML is escaped, never passed through.** The text here is model
//! output and it lands in the DOM through `inner_html`, so an `Event::Html`
//! forwarded verbatim is a `<script>` the court wrote and the browser ran.
//! `pulldown_cmark`'s events are filtered before the HTML writer ever sees
//! them, which is the same instinct `artifact.rs` follows in refusing rather
//! than guessing.
//!
//! Escaped rather than *dropped*, which was the first attempt: CommonMark's
//! HTML blocks run to the next blank line, so a stray `<script>` tag takes the
//! sentence after it with it, and the King reads a paragraph the court never
//! wrote. Showing angle brackets is a visible refusal; a silent hole is not one.
//! It costs the model nothing either way: it has markdown for everything it
//! actually wants to say.
//!
//! **A mermaid fence is not a code block.** ```` ```mermaid ```` becomes
//! `<pre class="mermaid">` — the shape mermaid's own `run()` looks for — rather
//! than `<pre><code class="language-mermaid">`. Everything else keeps the
//! ordinary code-block shape; syntax highlighting is deliberately not in scope.
//!
//! On weight: `pulldown-cmark` is pure Rust with `default-features = false`,
//! so it compiles to wasm alongside everything else. It costs the debug bundle
//! 828 KB (28.8 MB to 29.6 MB), which is mostly debuginfo -- the release
//! profile strips and `opt-level = "z"`s it, and cannot be measured here
//! because `cargo leptos build --release` fails on this toolchain with
//! "queries overflow the depth limit" on `main` as well as on this branch.
//! Mermaid itself is 3.5 MB of JavaScript and is *not* in the bundle at all --
//! see [`Prose`] for how it is fetched.

use leptos::prelude::*;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// A rendered fragment, and whether it contains anything mermaid must draw.
pub struct Rendered {
    /// The HTML fragment. Safe to hand to `inner_html`: see the module docs.
    pub html: String,
    /// True if at least one mermaid fence made it into the fragment. The one
    /// thing that decides whether 3.5 MB of JavaScript is fetched.
    pub has_diagram: bool,
}

/// Markdown in, sanitised HTML out.
pub fn render(text: &str) -> Rendered {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    // Smart punctuation stays off: the court quotes code and paths in prose,
    // and turning its straight quotes into curly ones makes them wrong to copy.

    let mut has_diagram = false;
    // Depth is a counter rather than a bool because a mermaid fence's body
    // arrives as one or more Text events and nothing may nest inside it -- but
    // counting means a malformed stream cannot leave us stuck in the state.
    let mut in_diagram = 0usize;
    let mut events: Vec<Event> = Vec::new();

    for event in Parser::new_ext(text, options) {
        match event {
            // Turned into text, not forwarded: see the module docs. The HTML
            // writer escapes a `Text` event on the way out, so the model's
            // markup reaches the King as visible angle brackets -- the only
            // refusal that does not also hide the prose around it.
            Event::Html(html) => events.push(Event::Text(html)),
            Event::InlineHtml(html) => events.push(Event::Text(html)),

            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info)))
                if is_mermaid(info) =>
            {
                has_diagram = true;
                in_diagram += 1;
                events.push(Event::Html("<pre class=\"mermaid\">".into()));
            }
            Event::End(TagEnd::CodeBlock) if in_diagram > 0 => {
                in_diagram -= 1;
                events.push(Event::Html("</pre>".into()));
            }
            // The fence's body is written by hand so it lands as the `<pre>`'s
            // direct text, with no `<code>` between: mermaid reads
            // `textContent`, and the escaping here is what keeps that safe.
            Event::Text(ref t) if in_diagram > 0 => {
                events.push(Event::Html(escape(t).into()));
            }

            other => events.push(other),
        }
    }

    let mut html = String::with_capacity(text.len() * 2);
    pulldown_cmark::html::push_html(&mut html, events.into_iter());

    Rendered { html, has_diagram }
}

/// Whether a fence's info string names mermaid.
///
/// Only the first word counts, because `mermaid` and `mermaid title="..."` are
/// both things models write, and the rest is not ours to interpret.
fn is_mermaid(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("mermaid"))
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Prose the court wrote, rendered.
///
/// Used for a proposal's body and for the assistant's own messages. Not for the
/// King's messages -- he typed those, and re-rendering his `#` as a heading
/// would be a small lie about what he said -- and not for notes or tool output,
/// which are the app's words and a machine's respectively.
///
/// Mermaid is loaded **lazily and once**, the first time a fence is actually on
/// screen, by appending a `<script>` to the head. Three things about that:
///
/// - It is 3.5 MB. Putting it in the document shell would make every chamber
///   pay for a feature most never use.
/// - It is vendored at `public/vendor/mermaid.min.js` rather than fetched from
///   a CDN. Kingdom is a local tool and a diagram must not stop drawing because
///   the machine is offline.
/// - It is a classic script that assigns `globalThis.mermaid`, not an ES
///   module, so this is a `<script>` tag and not an `import()`.
///
/// A diagram that fails to parse has its **source text put back**, rather than
/// leaving mermaid's red error card or an empty box. The King should still be
/// able to read what the court meant.
#[component]
pub fn Prose(
    /// The markdown to render.
    text: String,
    /// Extra classes for the wrapper, so a call site can keep its own styling
    /// (`proposal-body`, `msg-body`) without a wrapper element in between.
    #[prop(optional, into)]
    class: String,
) -> impl IntoView {
    let Rendered { html, has_diagram } = render(&text);
    let node = NodeRef::<leptos::html::Div>::new();

    #[cfg(feature = "hydrate")]
    {
        Effect::new(move |_| {
            if !has_diagram {
                return;
            }
            if let Some(element) = node.get() {
                draw_diagrams(&element);
            }
        });
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = has_diagram;

    view! {
        <div
            class=format!("prose {class}")
            node_ref=node
            inner_html=html
        ></div>
    }
}

/// Hand a freshly-rendered subtree to mermaid, fetching mermaid if need be.
///
/// Written as one JavaScript function rather than a pile of `Reflect` calls: it
/// is asynchronous, it retries per node, and it has to restore a failed node's
/// text -- all of which read as three lines of JS and as thirty of glue. The
/// source is this constant and nothing from the model reaches it.
///
/// Scoped to `element`. A global `mermaid.run()` would re-process every diagram
/// already on the page, which in a long chamber is every diagram the court has
/// ever drawn.
#[cfg(feature = "hydrate")]
fn draw_diagrams(element: &web_sys::HtmlElement) {
    use wasm_bindgen::JsValue;

    const DRAW: &str = r#"
const root = arguments[0];
const nodes = Array.from(root.querySelectorAll('pre.mermaid'));
if (!nodes.length) return;

const load = () => {
  if (window.mermaid) return Promise.resolve();
  if (!window.__kingdomMermaid) {
    window.__kingdomMermaid = new Promise((resolve, reject) => {
      const script = document.createElement('script');
      script.src = '/vendor/mermaid.min.js';
      script.onload = () => resolve();
      script.onerror = () => reject(new Error('mermaid could not be loaded'));
      document.head.appendChild(script);
    });
  }
  return window.__kingdomMermaid;
};

(async () => {
  try { await load(); } catch (e) { return; }
  window.mermaid.initialize({
    startOnLoad: false,
    // 'base' rather than 'dark': the named themes compute their own palette and
    // ignore most of what is handed to them, which is how the kingdom's ink
    // came out as mermaid's default grey. 'base' is the one theme documented to
    // derive itself from these variables.
    theme: 'base',
    darkMode: true,
    fontFamily: getComputedStyle(root).fontFamily,
    themeVariables: {
      darkMode: true,
      background: '#0f172a',
      primaryColor: '#16203a',
      primaryTextColor: '#e2e8f0',
      primaryBorderColor: '#2d3d5c',
      secondaryColor: '#1e293b',
      tertiaryColor: '#0b1120',
      lineColor: '#94a3b8',
      textColor: '#e2e8f0',
      mainBkg: '#16203a',
      nodeBorder: '#2d3d5c',
      nodeTextColor: '#e2e8f0',
      edgeLabelBackground: '#0f172a',
      clusterBkg: '#0b1120',
      clusterBorder: '#1e293b',
      titleColor: '#d4af37',
    },
  });
  for (const node of nodes) {
    if (node.dataset.kingdomDrawn) continue;
    const source = node.textContent;
    node.dataset.kingdomDrawn = '1';
    try {
      await window.mermaid.run({ nodes: [node], suppressErrors: false });
    } catch (e) {
      // Put the source back: an unreadable diagram must not become an
      // unreadable blank.
      node.removeAttribute('data-processed');
      node.textContent = source;
      node.classList.add('mermaid-failed');
    }
  }
})();
"#;

    let draw = js_sys::Function::new_no_args(DRAW);
    let _ = draw.call1(&JsValue::NULL, element);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_markdown_becomes_ordinary_html() {
        let out = render("# Title\n\n- one\n- two\n\nSome `code` and *emphasis*.");
        assert!(out.html.contains("<h1>Title</h1>"));
        assert!(out.html.contains("<li>one</li>"));
        assert!(out.html.contains("<code>code</code>"));
        assert!(out.html.contains("<em>emphasis</em>"));
        assert!(!out.has_diagram);
    }

    #[test]
    fn tables_render() {
        let out = render("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(out.html.contains("<table>"), "{}", out.html);
        assert!(out.html.contains("<td>1</td>"), "{}", out.html);
    }

    /// The body is model output rendered with `inner_html`. A model that emits
    /// a script tag must get visible text, never a running script -- and the
    /// prose around it must survive, which is why this escapes rather than
    /// drops: an HTML block runs to the next blank line.
    #[test]
    fn raw_html_never_reaches_the_dom_as_html() {
        let out = render("before\n\n<script>alert(1)</script> and after\n\ntail <b>x</b>");
        assert!(!out.html.contains("<script"), "{}", out.html);
        assert!(!out.html.contains("<b>"), "{}", out.html);
        assert!(out.html.contains("&lt;script&gt;"), "{}", out.html);
        assert!(out.html.contains("before"));
        assert!(out.html.contains("and after"), "{}", out.html);
        assert!(out.html.contains("tail"), "{}", out.html);
    }

    #[test]
    fn a_mermaid_fence_is_a_diagram_not_a_code_block() {
        let out = render("```mermaid\nflowchart LR\n  A --> B\n```");
        assert!(out.has_diagram);
        assert!(out.html.contains("<pre class=\"mermaid\">"), "{}", out.html);
        assert!(!out.html.contains("<code"), "{}", out.html);
        assert!(out.html.contains("flowchart LR"), "{}", out.html);
    }

    /// Mermaid reads `textContent`, so the fence's body must be escaped on the
    /// way in -- a label containing `<` is markup otherwise.
    #[test]
    fn a_diagram_s_source_is_escaped() {
        let out = render("```mermaid\ngraph TD\n  A[\"<b>\"] --> B\n```");
        assert!(out.html.contains("&lt;b&gt;"), "{}", out.html);
        assert!(!out.html.contains("<b>"), "{}", out.html);
    }

    #[test]
    fn other_fences_stay_code_blocks() {
        let out = render("```rust\nfn main() {}\n```");
        assert!(!out.has_diagram);
        assert!(out.html.contains("<code class=\"language-rust\">"), "{}", out.html);
    }

    #[test]
    fn a_fence_with_a_title_is_still_a_diagram() {
        let out = render("```mermaid title=\"x\"\nflowchart LR\n  A --> B\n```");
        assert!(out.has_diagram);
    }
}
