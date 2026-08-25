# An effort the King does not have to repeat

Kingdom already remembers which model drafts the next plan *and* how hard it was
asked to think: `kingdom.model` and `kingdom.effort` in the browser, written by
`store_choice` and read back by `restore_choice`. Pick `high`, reload the page,
and the chip still says `high`.

It does not survive a change of model. Picking a model that declares no
`reasoning_effort` — the offline mock, a Flash, any of the 4o entries — erases
the level from storage, and going back to a model that *does* declare it comes
back at the model's own default. One click on the mock costs the King a standing
preference he set weeks ago, silently, and the only sign is that his next plan
thinks less hard than he expected.

That is squarely the product's stance: the King's scarce resource is attention
and judgement. Making him re-state a judgement he already made, because of a
model he merely looked at, spends it for nothing.

---

## Where it goes wrong

`components/prompt_bar.rs`, in the picker's `pick_model`:

```rust
let pick_model = move |option: &ModelOption| {
    let keep = chosen
        .get_untracked()
        .and_then(|c| c.effort)
        .filter(|e| option.efforts.contains(e));   // <- the wish dies here
    state.choose_model(ModelChoice::new(option.id.clone(), keep));
};
```

`keep` is `None` whenever the newly picked model does not declare that level, and
`choose_model` writes straight through to storage, where `store_choice`'s `None`
arm does `remove_item(EFFORT_KEY)`.

The erasure is right for the *other* caller and wrong for this one, and the code
cannot tell them apart:

| The King clicks | What `effort: None` means | Storage should |
|---|---|---|
| `default` on the effort row | "use the model's own default" — a real choice | forget the level |
| a model with no effort row | nothing at all — he chose a *model* | keep the level |

## The fix

The filtering is already done, correctly, in the one place that owns it.
`ModelCatalogue::resolve` drops an effort the resolved model does not declare,
and it runs on **both** paths that matter: the `choice` memo that feeds the chip,
and `api::begin_plan` server-side before the plan is opened. So nothing
undeclared can reach the wire whatever the picker stores — which means
`pick_model`'s own filter buys no safety and costs the preference.

Carry the wish through unfiltered and let `resolve` keep deciding what is
sendable. The stored effort becomes a *standing wish*, forgotten only when the
King says so.

### 1. `kingdom-core/src/model.rs` — name the operation

```rust
impl ModelChoice {
    /// The same standing wish, aimed at another model.
    ///
    /// The effort is carried across **unfiltered**, on purpose. Whether a level
    /// can actually be sent is [`ModelCatalogue::resolve`]'s decision, and it
    /// makes it on every path that reaches a provider -- the chip's own memo and
    /// `api::begin_plan`. Filtering a second time here would not make the wire
    /// any safer; it would only mean that passing through a model with no effort
    /// control destroys a preference the user set deliberately.
    pub fn with_model(&self, model: impl Into<String>) -> ModelChoice
}
```

A method rather than an inline change, so the reasoning has somewhere to live and
the invariant has something to test. It is also the seam a second picker would
reuse if the chamber ever gets one.

### 2. `components/prompt_bar.rs` — use it

`pick_model` reads the standing wish from **`state.choice`**, not from the local
`chosen` memo, and aims it at the newly picked model with `with_model`. It falls
back to `ModelChoice::new(id, None)` when nothing is chosen yet. The
`option.efforts.contains` filter goes.

The `state.choice` / `chosen` distinction is load-bearing, and was found in the
browser rather than by the type system — both are `Option<ModelChoice>`, so
either compiles. `chosen` is the **resolved** view, already stripped of any level
the *currently selected* model does not declare, so sourcing the wish from it
only moves the erasure one click later: storage survives the trip *into* an
effortless model and loses the level on the way back *out*.

`pick_effort` is untouched: it is the caller for which `None` genuinely means
"the model's own default", and it should keep clearing the key. It reads its
model from `chosen`, which is right — that is the model actually selected.

### 3. `app.rs` — say what `None` now means

`store_choice`'s comment currently explains why `None` is remembered as an
absence. That stays true, but the reason is now sharper and worth writing down:
`None` reaches storage only from an explicit press on `default`, never from a
change of model. Without that line the next reader will re-add the filter.

---

## Test

One, in `kingdom-core`, pinning the thing a user would notice breaking:

**A standing effort survives a model that cannot honour it.** `with_model` onto a
model declaring no efforts keeps `High`; `resolve` against that catalogue reports
the choice *without* it; `with_model` back onto a model that declares it, and
`resolve` reports `High` again. That is the whole round trip the bug breaks, and
it pins the division of labour — the picker remembers, `resolve` decides — that
re-adding the filter would violate.

No test for the picker rendering or for the storage keys: the first restates the
implementation and the second needs a browser to say anything the `Effect` does
not already say plainly.

---

## Verified

The unit test pins the domain half. The UI half was checked end to end against a
live Copilot catalogue (21 models) on a proving ground, since that is where the
bug actually bit:

1. Pick `high` on `claude-opus-5` — chip reads `claude-opus-5 · high`,
   `kingdom.effort` is `high`.
2. Switch to `gpt-4o`, which declares no levels — chip drops to `gpt-4o`, the
   effort row hides, **and `kingdom.effort` is still `high`**.
3. Switch back to `claude-opus-5` — chip reads `claude-opus-5 · high` again, with
   `high` marked chosen. *(This step is what caught the `chosen` vs
   `state.choice` mistake: it read `default` before the fix was corrected.)*
4. Reload the page — still `claude-opus-5 · high`.
5. Press `default` — the level clears, as it should. The King forgets it only
   when he says so.

No console errors; the 23 warnings present are pre-existing Leptos resource
warnings on untouched lines. `cargo fmt --check` reports the same 111 diffs
before and after this change, so it introduces no formatting drift.

## Out of scope

- **A `KINGDOM_EFFORT` environment default**, as a sibling to `KINGDOM_MODEL`.
  Cheap, and genuinely the same shape — but `KINGDOM_MODEL` earns its place by
  making a fresh clone draft with no setup, and an effort on a model nobody has
  chosen yet has no such job. Worth adding when someone wants it, not before.
- **Nearest-declared-level fallback** — honouring a wish of `max` as `high` on a
  model that stops at `high`. `ModelEffort::ALL` is ordered weakest-first, so it
  is buildable, but the type's own doc refuses to treat the levels as a scale
  with meaningful neighbours, and quietly thinking *less* hard than asked is a
  worse failure than visibly falling back to the default.
- **A picker in the chamber.** The choice is frozen when a plan opens, and
  `draft_plan` says why: a transcript whose model changed halfway is a record of
  nothing in particular. Unrelated to remembering a default for the *next* plan.

## Files

- `crates/kingdom-core/src/model.rs` — `ModelChoice::with_model`, and the test.
- `crates/kingdom-app/src/components/prompt_bar.rs` — `pick_model` uses it.
- `crates/kingdom-app/src/app.rs` — the comment on `store_choice`.

No change to the storage keys, the wire, or any plan already on disk.
