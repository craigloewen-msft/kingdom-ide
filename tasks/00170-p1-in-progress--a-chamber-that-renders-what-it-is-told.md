# A chamber that renders what it is told

The court writes markdown. Kingdom prints it verbatim. A proposal — the one
artefact the product's whole stance rests on, the thing the King is meant to
*read and judge* — arrives as a wall of `##`, `-` and code fences in a `<pre>`.

This task builds the renderer, points the proposal card and the court's messages
at it, and then — and only then — restores the one Phoenix sentence that was
deliberately deleted for saying Kingdom had one.

## Why the mermaid note comes last

`llm/system_prompt.rs` carries a long comment and a test
(`the_prompt_does_not_claim_output_is_rendered`) forbidding the word "mermaid"
anywhere in the prompt. That is not an oversight to route around: Kingdom
shipped the claim once, and a plan asked to make proposals render markdown found
the contradiction and spent 25 of its 30 reasoning blocks arguing with the
prompt instead of proposing anything.

The comment ends "Restore this the day a renderer exists." This is that day —
but the ordering is load-bearing. The hint goes back in the same commit that
makes it true, never before.

## Shape

```mermaid
flowchart LR
  Body["proposal.body / Message.body"] --> MD["markdown.rs — pulldown-cmark"]
  MD --> HTML["sanitised HTML fragment"]
  MD --> Fences["mermaid fences to pre.mermaid"]
  HTML --> DOM["inner_html on .prose"]
  Fences --> DOM
  DOM --> Init["mermaid.run() on the new subtree"]
  Vend["public/vendor/mermaid.min.js"] -.->|"lazy, first fence only"| Init
```

## The work

**1. `components/markdown.rs` — a new module, wasm-side.**

- `pulldown-cmark`, default features off — pure Rust, compiles to wasm. Record
  the bundle delta in the module docs.
- Tables, strikethrough, footnotes on; smart punctuation off.
- **Drop raw HTML.** Filter `Event::Html` / `Event::InlineHtml` rather than
  passing them through. The body is model output rendered with `inner_html`; a
  model that emits `<script>` must not get one. Same instinct `artifact.rs`
  follows in refusing rather than guessing.
- A fence whose info string is `mermaid` becomes `<pre class="mermaid">` with the
  source escaped inside — the shape mermaid's own `run()` looks for — not
  `<pre><code>`.
- A `<Prose text=String/>` component: renders the fragment, and after mount calls
  the mermaid initialiser only if it contained a fence.

**2. The mermaid library — vendored.**

- `public/` does not exist yet; `Cargo.toml` already names it as `assets-dir`, so
  `public/vendor/mermaid.min.js` will be served at `/vendor/mermaid.min.js`.
  Confirm against a running server, not by reading.
- **Not** in the document `<head>`. It is megabytes of JS most chambers never
  need. Dynamic `import()` from `Prose`, once per session, the first time a
  fence is actually rendered; memoise the promise.
- `startOnLoad: false`, dark theme matched to `style/abstracts` tokens, then
  `mermaid.run({ nodes })` scoped to the fresh subtree — a global `run()` would
  re-process every diagram already on the page.
- A diagram that fails to parse must leave the fence's **source text** visible,
  not an empty box or mermaid's red error card. The King should still be able to
  read what the court meant.

**3. Point the call sites at it.**

- `ProposalCard`: `<pre class="proposal-body">` becomes `<Prose/>` inside
  `div.proposal-body.prose`. Its doc comment currently explains at length why the
  body is plain text — rewrite it, do not leave the old reasoning standing.
- `Transcript`, `Entry::Message` where the speaker is the assistant: same
  treatment. **The King's own messages stay verbatim** — he typed them, and
  re-rendering his prose as headings would be a small lie about what he said.
  `Entry::Note` stays plain too; a note is app text, not prose.
- `msg-body` is a `<span>` in a grid row today. Prose is block content, so this is
  a real layout change in `_conversation.scss`, not a swap.

**4. Styles — `style/components/_prose.scss`.**

Headings, lists, `code`, `pre`, tables, blockquote, links, sized to sit in a chat
column without shouting: h1/h2 barely larger than body, tight margins, no web
font. The proposal body keeps its `max-height: 320px` scroll.

**5. Restore the Phoenix mermaid hint — last.**

- Replace the deletion comment in `system_prompt.rs` with Phoenix's own sentence,
  worded for Kingdom's surface (Phoenix says "conversation view"; Kingdom's is
  the chamber, and the hint should cover the proposal body too, since
  `propose_plan` is where diagrams are most wanted). Phoenix's parenthetical
  about quoting labels that contain punctuation goes back as well — it is the
  half that prevents actually broken diagrams.
- It renders where BASE's neighbours do, i.e. **before** the remit. The remit
  stays last and the existing ordering test must pass untouched.
- Invert `the_prompt_does_not_claim_output_is_rendered` into a test asserting the
  hint *is* present, keeping the history in its doc comment — the next reader
  needs to know why this sentence is fussed over.

**6. `AGENTS.md`.** Two passages in §4 assert Kingdom has no markdown renderer.
Both become false. Update them, and say where the renderer is and what it
refuses.

## Done when

- A proposal with headings, a list, a table, inline code, a code fence and a
  mermaid graph renders all six correctly — verified in a real browser against a
  seeded proving ground, with a screenshot.
- A body containing `<script>alert(1)</script>` renders as visible text.
- A chamber with no mermaid fence never requests `/vendor/mermaid.min.js`.
- A malformed mermaid fence still shows its source.
- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features` pass, and
  `cargo leptos build` produces a bundle whose size delta is stated.

## Deliberately not in scope

Syntax highlighting for ordinary code fences (another heavy dependency), and
markdown in the rail, the map or tool output. Tool output is a machine's, not
prose.
