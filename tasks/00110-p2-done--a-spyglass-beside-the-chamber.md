# A spyglass beside the chamber

The spyglass works, and it is in the wrong place. It is a horizontal band wedged
between the chamber header and the log, capped at `max-height: 45%`, showing a
1024x768 page in a short wide slot. The King sees the top third of a page and
the transcript is squeezed for the privilege. The two things he wants to read at
once -- *what the court said* and *what the court's browser is doing* -- are
stacked in the one axis where they compete.

Move it to the right-hand side of the chamber, as a full-height column with a
drag handle, so the transcript and the page sit side by side and neither
shortens the other. Then make the picture legible as *action* rather than as
pixels, by captioning it with the browser deed currently in flight.

## Not in this task, deliberately

- **An app-level dock** -- a third column on `.throne-room` that survives
  navigation to the map. It is the better answer once the King wants to watch
  several plans at once, and it is a bigger change: a dock outside the route
  does not know whose browser it shows, so it needs a
  `watched: RwSignal<Option<PlanId>>` on `KingdomState` and a toggle that
  *selects* a plan rather than toggling a panel. Nothing here forecloses it --
  this task leaves the component and the socket as portable as it found them.
  Recorded so the next person need not rediscover the shape of it.
- **Click markers** -- a ring flashed where a click landed. It needs bounding
  boxes back from `session.rs::click` through `ToolOutcome`, translated into
  canvas space: a domain change for a visual flourish. The deed caption below
  buys most of the same legibility for none of that.
- **Input into the page.** Still no. `screencast.rs` in both crates argues this
  at length and the argument has not changed: two input sources driving one page
  with nothing arbitrating between them is the collision this product exists to
  surface. `pointer-events: none` stays on the canvas.
- **Resizing Chrome's viewport to match the panel.** Tempting, and a trap:
  `browser_resize` is the court's own tool for testing responsive layouts, and
  moving the viewport underneath it would corrupt the thing it is measuring. The
  panel fits itself to the page, never the reverse.

---

## Piece 1 -- The panel moves to the side

`.chamber` is a flex column today: header, spyglass, log, error strip, proposal
card, composer. The spyglass comes out of that column and sits beside the rest
of it.

The shape that costs least: keep the existing column exactly as it is, wrap it,
and put the spyglass next to the wrapper.

```text
.chamber                     (flex row)
  .chamber-column            (flex column -- everything that is there today)
    header / log / error / proposal / composer
  .spyglass-resizer          (only while watching)
  .spyglass                  (flex column, full height, width from a signal)
```

Markup changes are confined to `ConversationBody` in `conversation.rs`: the
`<Show when=watching>` block moves out of the column and to the end, and one
wrapper `<div>` appears. The `watching` signal, the header toggle and the
`BrowserView` component itself are untouched -- including the comment on
`watching` about it being the King's direct control over whether Chrome is
painting for an audience, which stays true because opening the panel is still
what attaches the viewer.

### Width, and the resizer

The rail's drag handle is exactly the behaviour wanted, and it is private to
`sidebar.rs` with "drag right widens" baked in. Generalise it rather than
copying it -- a second hand-rolled resizer is a second place to fix the
text-selection bug, the persist-on-release rule, and the window-level listener
that exists because the element-level one loses the pointer.

Lift it to `components/resizer.rs` taking: the width signal, the direction (the
rail grows rightwards, the spyglass leftwards), the bounds, the default for
double-click, and the storage key. `Sidebar` then calls it with its existing
constants and `kingdom.sidebar_width`, so the rail's behaviour is unchanged and
the move is proved by using it.

The spyglass width is a plain local signal in `ConversationBody`, persisted to
`kingdom.spyglass_width` on release. Local rather than on `KingdomState`
because, like the rail's collapse set, nothing outside this view cares. Bounds:
narrower than ~320px and a 1024-wide page is unreadable; wider than ~60% of the
viewport and the transcript stops being the thing under review. Default around
480px.

Restore it the way `restore_width` does -- inside an `Effect`, so the server
does not emit markup that hydration will disagree with. That comment exists
because the bug is easy.

### Fitting the page: contain

The canvas keeps `object-fit: contain` and gains nothing clever. A 4:3 page in
a tall panel letterboxes with dead space above and below, which is honest: the
whole frame is always visible, and the King is never left wondering whether
something happened below a fold. Centre it in the stage, both axes.

(The alternative -- scale to width, top-align, scroll the stage -- reads more
like a real browser and was considered. It costs a scroll position that fights
the live frame stream, and it hides part of the page by default, which is the
opposite of what a *watching* panel is for.)

### Below the panel's minimum

A chamber narrower than the panel's minimum plus a readable transcript has no
good side-by-side answer. Under a breakpoint, fall back to the stacked layout
that exists today rather than inventing a third arrangement -- the current CSS
becomes the small-screen branch rather than deleted code.

## Piece 2 -- The deed caption

A moving picture with no words still leaves the King inferring. The panel gains
a caption naming the browser deed in flight:

```text
  browser_click   .submit-btn        working...
```

and, when nothing is in flight, the last browser deed that completed, gone
quiet. That is the difference between "the pixels changed" and "it clicked the
button".

All of it is client-side and there is no new server work:

- `live: Memo<Option<Plan>>` is already in `ConversationBody` and already
  updates over the chamber's watch socket.
- A memo over `plan.transcript` finds the last `Entry::Tool` whose `tool` starts
  with `browser_`, preferring one where `in_flight()` is true.
- `telling_argument` already promotes `selector` and `url` onto a deed line --
  reuse it rather than teaching the panel about each browser tool's schema. It
  is the same judgement and it should stay in one place.

Render it with the deed vocabulary that already exists (`.deed-tool`,
`.deed-gist`, `.deed-running`) so an in-flight call looks the same here as in
the log. Pass the memo into `BrowserView` as a prop rather than reaching for
context: the component stays a thing that renders a browser and a caption for
whatever it is given, which is what keeps it liftable to an app-level dock
later.

Where it goes: beneath the stage, not in the top bar. The top bar says *where*
the page is; this says *what is being done to it*. Two different questions, and
the URL is the one that should keep the position the King already knows.

---

## Suggested order of work

1. Lift `Resizer` into its own component with a direction and a storage key;
   `Sidebar` adopts it and behaves identically. Nothing visible changes.
2. Re-lay the chamber: wrapper column, spyglass beside it, resizer between,
   width signal persisted. Stacked fallback under the breakpoint.
3. The deed caption: the memo, the prop, the markup, the styling.

## Done when

- The King opens the spyglass mid-flow and reads the transcript and the page at
  the same time, with neither one shortened.
- Dragging the divider resizes the panel; double-click returns it to its
  default; the width survives a reload.
- The rail resizes exactly as it did before, through the shared component.
- While the court is clicking, the panel names the deed doing it, and says
  "working..." only while it really is in flight.
- Closing the panel still stops the screencast, and opening one on a plan with
  no browser still says so and launches nothing.
- The canvas still cannot be clicked into.
- `cargo test -p kingdom-core`, and
  `cargo test -p kingdom-app --features ssr --no-default-features`, both pass;
  the hydrate bundle still builds.

## Tests

This is layout and a derived label, and most of it is only true in a browser.
One test earns its place: the caption's selection -- that it picks the in-flight
`browser_*` deed over an earlier settled one, and falls back to the last
completed browser deed when nothing is running. That is the one piece of
judgement here that can be wrong without looking wrong. Write it against a plain
function over `&[Entry]` so it is testable without a DOM. The rest is checked by
looking at it.

## Risks worth naming

- **The lifted `Resizer` is the only change that can break something already
  working.** The rail's resizer carries three non-obvious details: the
  window-level mousemove listener (the element-level one loses the pointer once
  it crosses the map, whose own handler then steals the drag), `body.resizing`
  suppressing text selection mid-drag, and persisting once on release rather
  than every frame. All three must survive the lift, and the direction parameter
  must not quietly invert one. Do this step alone and confirm the rail before
  touching the chamber.
- **A panel that is comfortable to leave open is a screencast that is always
  running.** The side dock is nicer than the band, which makes it likelier to be
  left open on a plan nobody is watching -- a permanent paint-per-frame tax on
  that plan's Chrome. The `Arc`/`Weak` lifecycle still ends it the moment the
  panel closes, so nothing leaks; it is a habit risk, not a correctness one.
  Worth noticing after living with it, not worth pre-empting with machinery.
- **The caption re-reads the transcript on every plan update.** Scan from the
  back and stop at the first match; a long transcript walked from the front on
  every event of a busy turn is wasted work.
