# A loading bar that reports real work

The map's loading card names what it is doing -- "Surveying the realm", then
"Raising the cities" -- and animates three towers going up. It reports **no
progress at all**: the King is told something is happening and never how far
along it is, for a wait that runs to eight seconds on a real dev folder.

Add a bar, fed by measured work rather than by a timer pretending to be one.

## What the wait is actually made of

Measured against a running server, on the `kingdom-mirror` realm
(`/kingdom/map.json` = **4,351,840 bytes**, `content-length` present, no
`content-encoding` even when gzip is offered):

| Phase | What it is | Measured |
|---|---|---|
| fetch | pulling map.json | 1.9 s -- 4.0 s across runs |
| raise | `spawn_world` building ~20k entities | seconds, in main-thread blocks of 1.3--1.7 s |

The second row is the crux, and it is why this is not a one-file change.

`PerformanceObserver` reports long tasks of 1707 ms, 1544 ms and 1368 ms back to
back after the fetch lands. A `browser_eval` sampling loop started immediately
after navigation did not get its **first** turn until t~=10.9 s -- the main
thread was never free enough to run it, and by then the card was already gone.

> During "Raising the cities" the browser cannot paint. A bar drawn there would
> freeze at whatever fraction it last showed -- which reads as a hung
> application, the exact impression this card exists to remove.

That is also why the card's SCSS already insists every animation on it be
`transform`/`opacity`: those composite off the main thread. A changing *number*
has no such escape. So an honest bar for the second phase needs the build cut up
so the frame comes back between the pieces.

## What was built

**The fetch reports bytes.** `view::read_whole` reads the body a chunk at a time
through a `ReadableStreamDefaultReader` instead of calling `Response::json`, and
counts against `content-length`. gloo's `json()` is `from_str(&text().await?)`,
so the parse cost is unchanged (17 ms for this manifest, measured in-browser).

`progress::Transfer` is the arithmetic -- a fraction and a line, *2.1 MB of
4.2 MB*. It lives in its own module on **both** targets rather than in `view.rs`
for one reason: `view.rs` is hydrate-only and `cargo test` builds this crate with
no features, so anything there is never compiled by the suite, let alone run.
Rounding and unit-crossing are exactly what a test catches and a reader does not.

It declines to answer rather than guessing when the two numbers cannot be
compared: no declared length, or a count that has passed it. The second is not
hypothetical -- compressing this route later would make `content-length` the
compressed size while the reader counts decompressed bytes, and a bar reading
640% is worse than one that admits it does not know.

**The raise reports holdings, and stops blocking the frame.** New
`engine::raise`: `RaisePlan` is a cursor and some arithmetic with no Bevy in it,
`Raise` is the resource holding a job, and `raise_world` consumes slices against
an 8 ms deadline, publishing the fraction to the bridge between them.
`ViewerCommand::Load` no longer builds anything -- it clears, lights the scene,
spawns a hidden root, and hands the manifest over.

`spawn.rs`'s stages take a `Range` and `spawn_step` is the one door both paths go
through, so the sliced build and `spawn_world` cannot raise different
settlements.

**The bar itself.** `ViewerStatus::raising` carries a stage and a fraction;
`Survey` draws one bar for both phases, with the indeterminate sweep the card
already had as the fallback. `scaleX` on a full-width layer, never `width`, for
the compositor reason above -- now with an actual reason to hold to it.

## What is load-bearing

- **The root is hidden until it stands.** The card is a translucent gradient
  rather than an opaque screen, and scenery spawns visible and is only culled
  when `apply_lod` next runs -- so the King would otherwise watch trees appear
  and then vanish.
- **`ActiveLod` and `Activity` are marked changed at completion.** Both systems
  early-return unless their resource moved, and every entity of the new world
  was spawned after the last change.
- **Winit is held at the watching pace while a raise is in flight.** Walking into
  a chamber mid-raise sends `Show(false)`, which drops the engine to four ticks a
  second and would turn three seconds of building into a minute. `winit_for` is
  one definition, so the two systems cannot disagree about what idle means.
- **`built` still means standing**, so the card's dismissal and
  `the_world_standing_up_wakes_the_interface` are untouched.
- **Finishing is a state of the bar, not the absence of one.** `raising` clears
  the moment the world stands and the card then spends 320 ms fading, so without
  a finished case the King's last sight of the bar is it emptying back to a
  sweep -- work being undone at the moment it succeeded.
- **`status_matches` compares the fraction within 0.5%**, for the reason it
  already tolerates sub-pixel camera drift. But starting and finishing are always
  heard: `None` means *nothing is going up*, and rounding that together with a
  build at 0% would leave the bar indeterminate for the whole first slice.

The weights in `Step::weight` are **estimates** and the design assumes so. Being
wrong makes the bar advance unevenly and nothing else -- it cannot stall the
build, and `fraction` reaches exactly 1.0 whatever they say.

## What was deliberately not done

- **Compressing `/kingdom/map.json`.** It would cut 4.35 MB to ~694 KB and help
  the fetch more than any bar does, but it is a server change with its own
  trade-offs. `Transfer`'s clamp is written so that it cannot break this.
- **The pre-hydration wait.** ~1.5 s of wasm download before the app boots, with
  the folder picker on screen and no card to draw into.
- **The folder-scan wait** on `ChooseKingdom` -- a different screen.

## Verified

- `cargo test -p kingdom-citymap` -- 102 pass (97 before), including the new
  `raise` and `progress` tests: every item built exactly once and in order, a
  fraction that only moves forwards and reaches exactly 1.0, an all-but-empty
  world that finishes without dividing by zero, and every stage reachable.
- `cargo test -p kingdom-app --features ssr --no-default-features` -- 225 pass.
- wasm32 builds clean under `--features hydrate`; clippy clean for the new code.
- By hand against the `crowded` realm (40 towns, 1,320 holdings) on a spare
  port, under CPU throttling so the phases are observable:

      Surveying the realm | Reading every city in the kingdom |   0
      Surveying the realm | 3.0 MB of 3.0 MB                  | 100
      Raising the cities  | 40 towns / 1,320 holdings         |  --
      Laying the roads    | 40 towns / 1,320 holdings         |  10
      Laying the roads    | 40 towns / 1,320 holdings         |  35
      Planting the groves | 40 towns / 1,320 holdings         |  66
      Planting the groves | 40 towns / 1,320 holdings         |  77
      The kingdom stands  | 40 towns / 1,320 holdings         | 100

  The ~550 ms long tasks that remain are Bevy's ordinary debug-build frame: an
  idle, fully-built map shows the same, so the slices sit inside them.
