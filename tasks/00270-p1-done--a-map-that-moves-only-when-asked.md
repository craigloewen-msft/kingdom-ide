# A map that moves only when the King asks it to

The map at the foot of the rail swept and re-zoomed on its own while the King
was reading or reviewing — roughly once per round of any agent working in that
project. Reported as "the map view keeps moving around... I think it's moving
anytime an agent anywhere adds or changes a file."

That reading was exactly right, and the cause was not in the map.

## The chamber was shouting the same thing over and over

`conversation.rs` published the open plan's city to the map on every render of
an effect over `plan` — a `Memo<Option<Plan>>` over the *whole* plan, transcript
included. Every watch-socket push makes that memo's value differ, so the effect
re-ran once per deed of every turn:

```rust
Effect::new(move |_| {
    if let Some(p) = plan.get() {
        state.selected.set(Some(p.city));   // the same city, every time
    }
});
```

And `set` notifies whether or not the value moved. `reactive_graph`'s
`Set::set` is a bare `try_update(|n| *n = value)` — there is no equality check
anywhere in it. So `state.selected`, which is the map's `focus_city` prop,
announced a change on every deed.

Both camera effects in `citymap/src/view.rs` tracked it, and both re-sent their
commands: `Focus` pulled out to the whole town, `Inspect` dove back onto the
open file. That pair, once per agent round, is the movement he saw.

The same trap sat on `focus_file`, which `ConversationBody` re-published
whenever it was rebuilt.

The fix at the source is a comparison before the write. `open_plan` in the same
file already records why identity beats value for this signal; this is that
lesson applied to the two signals that drive the camera.

## And then a rule that cannot quietly rot

A guard at a call site is one line away from being deleted by someone who reads
it as an optimisation. So the *rule* moved into `citymap/src/follow.rs`, a pure
function with the two camera effects collapsed into one caller.

| Cause | What the camera does |
|---|---|
| The King opens a file | `Inspect` its building — gliding within a city, cutting on arrival |
| The chamber becomes about a **different** city | `Focus` that town, once |
| The map changes home | the existing re-fit |
| **Anything else** | **`Stay`** |

That last row is the feature. An agent writing a file, a status poll landing
sixteen times a second, a pan re-setting the whole status signal — every one of
them now answers `Stay`, and the map sends nothing.

It is a pure function for `engine/input.rs`'s reason, quoted in its own module
doc: `view.rs` is `hydrate`-only and there is no DOM under `cargo test`, so a
rule left inside an effect is a rule nothing can pin. Here it is arithmetic over
six plain values and thirteen tests.

### Two rulings from the King, on questions the code had answered for itself

- **Closing the file panel no longer pulls back to the town.** It used to, and
  `00230`'s notes defended the change that introduced it. It is motion he did
  not ask for, and the building on screen is still a building of the project he
  is in.
- **Entering a chamber still frames its town once.** The strict reading — never
  move except on a file click — would leave a chamber about project B showing
  project A, a pane confidently displaying the wrong place.

A consequence worth naming: moving between two plans *of one project* now moves
the camera not at all.

## The memory has to hold the path, not just the city

The first draft of `follow.rs` remembered only which *city* the camera had been
pointed into — carried over from the old `inspected_city` signal. Two of its own
tests failed immediately, and they were right to:

```
---- follow::tests::nothing_new_moves_nothing ----
  left: Inspect { glide: true }
 right: Stay
```

Remembering the city alone cannot distinguish "the King opened another file
here" from "something woke this and nothing has changed" — and the second is
precisely the reported bug. `Followed::inspected` is `(city, path)`, and
`already_at` is what returns `Stay`.

This is the one place the plan as approved was wrong, and the tests caught it
before the browser did.

## Rehearsed, not merely compiled

Against `kingdom-mirror` with a turn actually running, measuring the rail pane
between screenshots (RMSE; the canvas itself reads back blank without
`preserveDrawingBuffer`, so an early in-page pixel diff was worthless and was
thrown away):

| Moment | Change in the pane |
|---|---|
| 25 s of an agent working in the open city | **0.19 %** — the activity ring breathing |
| Clicking a file in the tree | **16.8 %** — the camera dives to its building |
| Clicking a second file | **18.8 %** — it hops next door |
| Closing the panel | **0.04 %** — it stays, per the ruling |
| Opening a chamber in another city | frames that town |
