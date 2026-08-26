# A loading bar that is actually seen moving

The map's loading card has had a real bar since `00220`: bytes against
`content-length` for the fetch, weighted items against the manifest's own totals
for the raise. The King reported it as "an indeterminate progress bar that seems
to go forever", and he was describing what reaches the screen rather than what
the code computes.

Both were true. The bar **was** measured, and it **was** a sweep for most of the
wait -- because it went indeterminate twice in the middle, and because almost
none of the readings it published were ever painted.

## What was measured, before

Against the running dev server on the `kingdom-ide` folder (6 towns, 3,028
holdings), loading the map in a same-origin iframe and sampling the card from
the parent every animation frame -- the map's own thread is far too blocked to
sample itself. Three runs, `SWEEP` = indeterminate, `M57` = measured at 57%:

| Run A | Run B | Run C |
|---|---|---|
| 4017 Surveying — **SWEEP** | 2773 Surveying — **SWEEP** | 4902 Surveying — **SWEEP** |
| 4513 Surveying — M0 | 3060 Surveying — M0 | 5402 Surveying — M0 |
| 6709 Surveying — M74 | 4669 Surveying — M77 | 8258 Surveying — M57 |
| 6792 Raising the cities — **SWEEP** | 4756 Raising the cities — **SWEEP** | 8382 Raising the cities — **SWEEP** |
| 7101 Planting the groves — M79 | *(no raise reading at all)* | 8952 Planting the groves — M83 |
| 9508 The kingdom stands — M100 | 6190 The kingdom stands — M100 | 11378 The kingdom stands — M100 |

Three distinct fractions in a five-second card, two indeterminate stretches
bracketing them, and a multi-second freeze at the end. In run B the raise never
showed a fraction at all.

## The two faults

**It went indeterminate between the phases.** `Survey::fraction` asked each
phase for its own answer and swept whenever the one it asked had none. The
comment said the gap between the two "lasts a frame". Measured, it lasted up to
**1,430 ms** -- the engine was still booting and the `Load` command was sitting
in the bridge queue.

**The readings were published and not painted.** Over a 5.5 s card the 50 ms
poll had ~110 chances and produced three repaints. Even the pure-Leptos half was
starved: the streamed read set `transfer` on every chunk of a 4.3 MB body and
two of those were drawn.

The cause of the second, found by recording frame timestamps inside the map
page: **single frames of 2,694 ms and 1,066 ms**, while `FRAME_BUDGET` was 8 ms.
The budget bounds the wrong half of the work. `spawn_step` queues a `Commands`
spawn and adds a mesh to `Assets`; the expensive part is what the rest of the
frame then does with them -- applying the commands, preparing every new mesh for
the GPU -- and all of that happens *after* the deadline is checked for the last
time. Eight milliseconds of issuing buys seconds of consequence, and nothing
paints inside a frame that is still running.

## What was built

**One scale over the whole wait.** `progress::Wait` composes the two phases:
the fetch fills the first `FETCH_SHARE`, the raise fills the rest, and a phase
that has not begun holds the bar where the last one left it. Monotonic by
construction -- each phase owns its own stretch, so no handover can step it
back. The sweep survives for the one case it was written for, a fetch whose
length the server never declared.

`FETCH_SHARE` is an estimate and the design says so, exactly as `Step::weight`
does. Being wrong makes the bar advance unevenly across the handover and nothing
else.

**The engine boots when the manifest lands, not on mount.** Its wgpu setup and
first pipeline compiles used to land on top of the fetch, which is why a
4.3 MB download off localhost took 3.3 s and painted twice. Nothing can be drawn
before the manifest exists, so waiting costs no pixel. The fetch now takes about
**60 ms**.

**The engine stops drawing what nobody can see.** `raise_world` deactivates the
map camera for the length of the raise and restores it -- to whatever
`Standing` justifies, not blindly to on -- in the frame that reveals the world.
The root was already spawned hidden and the card covers the region, so nothing
visible stops being drawn.

**Frames are held to a length the browser can draw in.** `Job::allowance` is
what a frame may spend, in `Step::weight` units, adapted by `next_allowance`
from what the *whole previous frame* cost -- measured start-of-system to
start-of-system, because the part that hurt was never the part spent inside the
system. Too slow a frame halves it, a comfortably quick one doubles it, and the
rest is left alone so the raise settles instead of oscillating.

The budget is **weighed, not counted**, and that matters: a frame allowed two
thousand items would plant two thousand trees in no time or paint two thousand
folder names in several seconds. The weights the bar already trusts are reused
to spend a frame rather than to fill a bar.

**The reveal gets the end of the wait to itself.** Showing 20k entities to a
camera that has been off is the most expensive frame there is -- 2,835 ms -- and
whatever the card shows when it begins is what the King reads until it ends. So
the last slice publishes a full bar, the card holds it for `REVEAL_PAUSE`, and
the reveal then takes its frame. One yielded frame was not enough: the engine
runs continuously during a raise, so its next frame can start before the poll
has read the full bar, and measured that was a coin flip between a bar that
finished and one that stopped at 98%.

**Two more captions, because there were two more waits being misreported.**
"Summoning the masons" while the engine is still coming up -- `ViewerStatus`
now carries `awake`, set by the engine's `Startup`, so that is a fact rather
than an inference from the absence of a `raising`. And "Opening the gates" for
the reveal, because a full bar under "Painting the names" claims the masons are
still at work while they are standing back.

**The poll runs at 16 ms rather than 50.** A world goes up in frames of about
`TARGET_FRAME`, so a poll of the same order aliases against them and throws away
most of what the engine published. The bridge's revision counter is what makes
asking this often cheap.

## What is load-bearing

- **The `Load` command is queued before `engine::run` is called.** `run` hands
  control to the browser's animation loop and may never return, so anything
  after it may never happen. The bridge queue is what holds the manifest until
  the engine's first update drains it.
- **The engine is booted on the failure path too**, so a kingdom whose manifest
  could not be read still shows empty space and stars behind the error rather
  than a black rectangle.
- **`take_worth` always returns at least one item.** A budget of zero that
  returned an empty range would be a raise that never ends and a card that never
  comes down -- the worst failure this module has, and one slip away.
- **The camera is restored from `Standing`**, so a world raised while the King
  is in a chamber finishes stood down, exactly as `Show` left it.
- **`awake` is compared in `status_matches`.** A field left out of that
  comparison is a field the interface never hears about.

## What was deliberately not done

- **The reveal frame itself.** ~2.8 s to draw a 20k-entity world for the first
  time in a debug wasm build. The bar now completes before it rather than during
  it, but the frame is the renderer's, not the card's.
- **The wait before the card exists.** The SSR document for `/` is 29.7 MB and
  `kingdom-ide.wasm` is 113 MB on a dev build -- about 2 s of the 4 s before the
  card appears. No bar can cover that; it is separate work.
- **Compressing `/kingdom/map.json`**, still. `Transfer`'s clamp is still
  written so that doing it later cannot make the bar lie.

## Verified

- `cargo test -p kingdom-citymap` -- 163 pass (158 before, 97 before `00220`),
  including the new `Wait` composition tests (the handover holds, the bar never
  moves backwards, an unmeasurable fetch still sweeps, a standing world pins it
  full) and the new `raise` tests (a frame takes only what its budget affords,
  an expensive step costs more of it, a spent frame cannot stall the raise, the
  controller follows the frames and stays between its bounds).
- `cargo test -p kingdom-app --features ssr --no-default-features` -- 259 pass,
  1 pre-existing failure in `llm::catalogue` that fails identically with these
  changes stashed.
- wasm32 builds clean under `--features hydrate`; clippy reports nothing new.
- By hand, same harness as the table above, on a spare port:

      853  Surveying the realm  SWEEP   (19 ms, before the headers land)
      872  Surveying the realm  M24
      915  Summoning the masons M45
      1777 Laying the ground    M45
      …    Laying the roads     M48 M50 M53 M55 M57 M59 M61 M63 M66 M68 M72 M74
      …    Raising the holdings M76 M81 M83 M85
      …    Planting the groves  M89 M92 M96
      …    Painting the names   M98
      2373 Opening the gates    M100
      5208 The kingdom stands   M100, gone

  25 painted readings against three, and one sweep frame of 19 ms against two
  stretches of up to 1.4 s.
