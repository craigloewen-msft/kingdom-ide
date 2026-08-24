# Modular style architecture

Replace the single 741-line `style/main.scss` with a layered SCSS module tree
that mirrors the component tree, so styling a component means opening one small
file next to its Rust sibling's name — not scrolling a monolith.

## Why

`style/main.scss` today holds palette, resets, opening screen, throne-room grid,
sidebar, map, skyline, chat dock and scrollbars in one file. Two concrete costs:

- **Locating rules is a scroll+grep exercise.** The map/skyline block alone runs
  ~300 lines with no boundary between "map chrome" and "city interior", even
  though those are separate Rust modules (`map/mod.rs`, `map/city.rs`).
- **The palette is invisible from Rust.** `kingdom-core::model` hardcodes hex
  literals for ward, city-kind and architect-status colours; `main.scss`
  hardcodes the same status colours again (`$working`, `$review`, `$blocked`,
  `$idle` — duplicated a third time for `circle.status-*`). Nothing pins them
  together, so they can silently drift.

## Constraints checked

- cargo-leptos keeps a single `style-file`. Verified locally that the bundled
  dart-sass 1.86 resolves `@use "abstracts/tokens" as t;` against partials, so
  `Cargo.toml` needs **no** change — `style/main.scss` stays the entry point and
  becomes a table of contents of `@use` lines only.
- `@use` is namespaced (unlike `@import`), which is the point: a file that wants
  the palette must say so.

## Plan

### 1. Structure

```
style/
  main.scss              // entry: @use lines and nothing else
  abstracts/
    _tokens.scss         // palette, radius, type scale — no output
    _mixins.scss         // .path-input/.chat-input, .claim-btn/.send-btn,
                         // the repeated ellipsis triple, glass panel
  base/
    _reset.scss          // *, html, body, button
    _scrollbars.scss
  layout/
    _throne-room.scss    // grid areas, body.resizing
  components/
    _choose-kingdom.scss
    _sidebar.scss        // header, toolbar, registry, city rows, plan rows
    _chat-dock.scss
    _map.scss            // region, svg, throne, controls, legend, zoomhint
    _skyline.scss        // districts, buildings, landmarks, cranes, keep
  _status.scss           // status/plan colour classes shared by rail + map
```

One file per component module, named to match `crates/kingdom-app/src/components/`.
Each keeps its existing explanatory comments — those are the most valuable thing
in the current file and none are to be dropped.

### 2. De-duplicate the obvious repeats (mixins, not new visuals)

- `.path-input` and `.chat-input` differ only in padding/font-size → `field()`.
- `.claim-btn` and `.send-btn` are the same gold gradient button → `royal-button()`.
- `.map-legend` / `.map-zoomhint` share the translucent-panel treatment →
  `glass-panel()`.
- The `white-space:nowrap; overflow:hidden; text-overflow:ellipsis` triple
  appears five times → `truncate()`.

### 3. Single source of truth for semantic colours

Emit the palette once as CSS custom properties in `base/_reset.scss`
(`--gold`, `--status-working`, …), with the SCSS `$` tokens defined *from* the
same map so both spellings stay in step. Then collapse the status-colour
duplication: `.status-*` sets `--status-colour` and the `background`/`fill`
rules read it, removing the `circle.status-*` restatement block.

Rust-side hex literals in `kingdom-core::model` stay as they are — they are
domain data returned to SVG attributes, and moving them is a separate decision.
A follow-up could have them return `var(--ward-rust)` style tokens; explicitly
out of scope here.

## Non-goals

- No visual change. This is a pure reorganisation.
- No CSS framework, no Tailwind, no CSS-modules-per-component build step.
- No renaming of existing class names (they are contracts with the `view!`
  macros in five Rust files).

## Verification

- `cargo leptos build` succeeds and the emitted CSS is compared against a
  pre-change capture of the same file: the diff must be empty modulo rule
  ordering and whitespace. That is the real acceptance test for a refactor that
  claims to change nothing visible.
- Load the app and screenshot the throne room at a wide and a narrow sidebar
  width; compare against pre-change screenshots (per AGENTS.md §6: map changes
  get looked at, not just compiled).
- No new unit tests: SCSS structure has no behaviour a caller depends on, and
  the compiled-CSS diff already pins the invariant that matters.
