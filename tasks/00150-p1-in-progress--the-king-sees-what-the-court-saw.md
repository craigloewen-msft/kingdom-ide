# The King sees what the court saw

When the court takes a screenshot, the chamber shows a filename. The picture
itself goes to the model and never to the user:

```
23:02  ✓  browser_take_screenshot                                    ⌄
           Screenshot saved to /home/…/.kingdom-browser-screenshot-1787626625.png.
23:03  ✓  read_image   .kingdom-browser-screenshot-1787626625.png   ⌄
```

That is exactly backwards for a product whose whole claim is *know what your
agents are doing*. The model has eyes (`read_image`, task 00090) and the King
does not. A plan can spend five minutes verifying a UI flow and leave behind a
column of tool names, and the one artefact that would settle "did it actually
work" in half a second is sitting on disk, unreferenced.

**Render the picture in the transcript, under the deed that produced it.**

---

## The one real decision: paths, not bytes

The tempting shortcut is to have `browser_take_screenshot` return
`ToolOutcome::seen(...)` — the images ride the plan over the existing
WebSocket, and the view renders a `data:` URL. It is about twenty lines and it
is wrong on three counts:

1. **It would not survive a reload.** `store.rs` deliberately strips images
   before writing a plan, because the document is rewritten on *every* update
   and would otherwise grow by a megabyte per screenshot forever. That
   reasoning is sound and this task must not undo it. A picture that vanishes
   on restart is worse than one that was never shown, because the King learns
   not to trust the chamber as a record.
2. **It would change what the model is told.** `copilot.rs::shown()` sends
   every image on a settled call to a model that can see. Attaching bytes to
   the screenshot call would silently start spending context on every capture,
   and spend it *twice* whenever the model then calls `read_image` — which is
   what its own tool description tells it to do.
3. **It would push megabytes down the watch socket** on every subsequent
   publish, since the wire carries whole plans.

The file is already on disk, inside the plan's workspace, where the path
boundary already applies. So: **the transcript records the path; the server
serves the file.** Bytes stay where they are. The domain gains a few strings,
not a few megabytes.

This keeps the two channels honest and separate, which is the shape the code
already has:

| Channel | For | Lifetime |
|---|---|---|
| `ToolOutcome::Done.images` | the model, this turn | in memory, stripped on save |
| `ToolOutcome::Done.artifacts` *(new)* | the King, in the chamber | persisted; the file is on disk |

---

## Piece 1 — an outcome can name what it left behind

`crates/kingdom-core/src/model.rs`.

```rust
/// A file a tool produced that is worth looking at.
///
/// A path rather than the bytes, and that is the whole design: the file is in
/// the plan's workspace already, so the record stays small enough to rewrite
/// on every update. See `ToolOutcome::Done.images` for the other channel and
/// why they are not the same one.
pub struct ToolArtifact {
    /// Workspace-relative. Absolute paths would leak a machine's layout into a
    /// record that outlives the machine, and cannot be resolved by a viewer.
    pub path: String,
    /// `image/png`, and so on.
    pub media_type: String,
}
```

Added to `ToolOutcome::Done` beside `images`, with the same
`#[serde(default, skip_serializing_if = "Vec::is_empty")]` treatment so older
documents still load and newer ones are not littered with empty arrays.
Constructor `ToolOutcome::produced(output, artifacts)`, sibling of `done` and
`seen`; `ToolCall::artifacts()` sibling of `shown()`.

`store.rs::without_images` keeps artifacts and keeps stripping images — add a
line to its doc comment saying so, because the two now look alike and only one
is dropped.

**Callers:**
- `tools/browser.rs::BrowserTakeScreenshot` — names the PNG it just wrote.
  Output text is unchanged; the model still gets a path and still calls
  `read_image`. Nothing about the model's flow changes in this task.
- `tools/read_image.rs` — names the file it read, so a picture the King left in
  the workspace shows up when the court looks at it. Free, same field.
- `tools/profile.rs` — leave alone. It writes JSON, which is not a thing to
  look at.

## Piece 2 — a route that serves one

New `crates/kingdom-app/src/artifact.rs` (ssr only), sibling of `watch.rs` and
`screencast.rs`, mounted in `main.rs` **before** the Leptos routes for the same
reason those two are.

```
GET /plan/{id}/artifact/{*path}
```

Rules, all of them refusals rather than surprises:

- Resolve `path` through the plan's own `Sandbox` — the same `resolve` every
  tool uses. Outside the workspace is a 403, not a 404: the distinction is
  free here and a silent 404 hides a bug.
- Only the media types `read_image` already accepts. Lift `READABLE` into
  something both can use rather than writing the list twice; two lists is how
  they come to disagree.
- Missing file is a 404. A plan whose worktree has been merged or archived is
  the *expected* case, not an error — see the placeholder below.
- `Cache-Control: immutable`. The names carry a nanosecond serial, so a given
  URL is the same bytes forever.
- No write verbs, no directory listing.

## Piece 3 — the chamber shows it

`components/conversation.rs::ToolCallLine`, plus
`style/components/_conversation.scss`.

Under the deed line, for each image artifact:

```rust
<figure class="deed-sight">
    <img class="sight-frame" src=/plan/{id}/artifact/{path} loading="lazy"/>
    <figcaption class="sight-caption">"The page as the court saw it"</figcaption>
</figure>
```

Decisions worth stating, because each has an obvious alternative:

- **Always visible, not behind the chevron.** The chevron governs the *text*
  detail and continues to. A picture hidden behind a click is a picture nobody
  looks at, and this is the highest-signal thing a transcript can contain.
- **Capped at ~260px tall**, `object-fit` from the top, so a page capture reads
  as a page and thirty of them do not turn the log into a scroll marathon.
  Click opens the full-size file in a new tab — the route serves it already.
- **`flex-shrink: 0`**, like `.chat-deed` above it: the log is a column
  flexbox, and a tall child otherwise gets squashed rather than scrolled to.
- **A broken image gets words, not a broken icon.** On `on:error`, swap in
  `.sight-gone`: *"The workshop this was taken in has been cleared away."*
  That is the true and common state for a merged or archived plan, and it is
  information rather than a fault.

The spyglass is not touched. It is the live view; this is the record, and a
plan that is finished has only the record.

## Piece 4 — the court is told the King can see

One clause in `llm/system_prompt.rs`: a screenshot is now shown to the user in
the chamber. A model that knows this stops narrating *"I've saved a screenshot
you can open at /home/…"* — which is the sort of line that reads as helpful and
is, now, simply false.

---

## Tests

No test launches a browser; none of these need to.

**`kingdom-core`**
- An outcome carrying artifacts round-trips through JSON.
- A document written before the field existed still loads (literal JSON, in the
  style of `a_plan_recorded_before_images_existed_still_loads`).

**`kingdom-app --features ssr`**
- `store`: saving keeps artifacts and still drops images. One test, both
  assertions — they are the same decision seen from two sides.
- `browser`: a screenshot outcome names the file it wrote, and the name is
  workspace-relative.
- `artifact`: a path outside the workspace is refused; a `.txt` is refused; a
  real PNG is served with `image/png`; a missing file is a 404.
- `conversation`: a screenshot deed yields one `<img>` with the plan's artifact
  URL; an ordinary deed yields none.

## Rehearsing it

```bash
KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch
```

The fixture court has no screenshot in it. Rather than adding one to
`fixtures.rs` — the fake data is deliberately about *plan states*, not tool
output — drive a real plan in the mirror: `browser_navigate` to the app itself,
then `browser_take_screenshot`. Note that the running server currently has no
wasm bundle built (`/pkg/kingdom-ide.wasm` 404s), so `cargo leptos` needs a
full build before any of this is visible in a browser.

## Out of scope

- A lightbox or gallery. A new tab is the whole feature and costs no state.
- Persisting image bytes anywhere. Explicitly rejected above.
- Surfacing screenshots on the map or in the rail. That is the live-updates
  gap in AGENTS.md §4 and a different task.
- Changing what `browser_take_screenshot` returns to the model, or making
  `read_image` unnecessary.
